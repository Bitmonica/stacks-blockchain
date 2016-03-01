#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
    Blockstack
    ~~~~~
    copyright: (c) 2014-2015 by Halfmoon Labs, Inc.
    copyright: (c) 2016 by Blockstack.org

    This file is part of Blockstack

    Blockstack is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.

    Blockstack is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU General Public License for more details.
    You should have received a copy of the GNU General Public License
    along with Blockstack. If not, see <http://www.gnu.org/licenses/>.
"""

from pybitcoin import embed_data_in_blockchain, serialize_transaction, \
    analyze_private_key, serialize_sign_and_broadcast, make_op_return_script, \
    make_pay_to_address_script, b58check_encode, b58check_decode, BlockchainInfoClient, hex_hash160

from pybitcoin.transactions.outputs import calculate_change_amount

from utilitybelt import is_hex
from binascii import hexlify, unhexlify
import types
import json

from ..b40 import b40_to_hex, bin_to_b40, is_b40
from ..config import *
from ..scripts import *
from ..nameset import *
from ..hashing import hash_name
   
from namespacepreorder import FIELDS as namespacepreorder_FIELDS
import virtualchain

if not globals().has_key('log'):
    log = virtualchain.session.log

# consensus hash fields (ORDER MATTERS!)
FIELDS = [
    'namespace_id',         # human-readable namespace ID
    'preorder_hash',        # hash(namespace_id,sender,reveal_addr) from the preorder (binds this namespace to its preorder)
    'version',              # namespace rules version

    'sender',               # the scriptPubKey hex script that identifies the preorderer
    'sender_pubkey',        # if sender is a p2pkh script, this is the public key
    'address',              # address of the sender, from the scriptPubKey
    'recipient',            # the scriptPubKey hex script that identifies the revealer.
    'recipient_address',    # the address of the revealer
    'block_number',         # block number at which this namespace was preordered
    'reveal_block',         # block number at which this namespace was revealed

    'op',                   # byte code identifying this operation to Blockstack
    'txid',                 # transaction ID at which this namespace was revealed
    'vtxindex',             # the index in the block where the tx occurs

    'lifetime',             # how long names last in this namespace (in number of blocks)
    'coeff',                # constant multiplicative coefficient on a name's price
    'base',                 # exponential base of a name's price
    'buckets',              # array that maps name length to the exponent to which to raise 'base' to
    'nonalpha_discount',    # multiplicative coefficient that drops a name's price if it has non-alpha characters 
    'no_vowel_discount',    # multiplicative coefficient that drops a name's price if it has no vowels
]

# fields this operation changes
# everything but the block number
MUTATE_FIELDS = filter( lambda f: f not in ["block_number"], FIELDS )

# fields that must be backed up when applying this operation (all of them)
BACKUP_FIELDS = ["__all__"]

def serialize_int( int_field, numbytes ):
   """
   Serialize an integer to a hex string that is padlen characters long.
   Raise an exception on overflow.
   """
   
   if int_field >= 2**(numbytes*8) or int_field < -(2**(numbytes*8)):
      raise Exception("Integer overflow (%s bytes)" % (numbytes) )
   
   format_str = "%%0.%sx" % (numbytes*2) 
   hex_str = format_str % int_field 
   
   if len(hex_str) % 2 != 0:
      # sometimes python cuts off the leading zero 
      hex_str = '0' + hex_str
   
   return hex_str
   

def serialize_buckets( bucket_exponents ):
    """
    Serialize the list of bucket exponents.
    There should be 16 buckets, and each one should have an integer between 0 and 15.
    """
    ret = ""
    for i in xrange(0, len(bucket_exponents)):
        ret += "%X" % bucket_exponents[i]
    
    return ret


def serialize_discounts( nonalpha_discount, no_vowel_discount ):
    """
    Serialize the non-alpha and no-vowel discounts.
    They must be between 0 and 15
    """
    return "%X%X" % (nonalpha_discount, no_vowel_discount)


def namespacereveal_sanity_check( namespace_id, version, lifetime, coeff, base, bucket_exponents, nonalpha_discount, no_vowel_discount ):
   """
   Verify the validity of a namespace reveal.
   Return True if valid
   Raise an Exception if not valid.
   """
   # sanity check 
   if not is_b40( namespace_id ) or "+" in namespace_id or namespace_id.count(".") > 0:
      raise Exception("Namespace ID '%s' has non-base-38 characters" % namespace_id)
   
   if len(namespace_id) > LENGTHS['blockchain_id_namespace_id']:
      raise Exception("Invalid namespace ID length for '%s' (expected length between 1 and %s)" % (namespace_id, LENGTHS['blockchain_id_namespace_id']))
   
   if lifetime < 0 or lifetime > (2**32 - 1):
      lifetime = NAMESPACE_LIFE_INFINITE 

   if coeff < 0 or coeff > 255:
      raise Exception("Invalid cost multiplier %s: must be in range [0, 256)" % coeff)
  
   if base < 0 or base > 255:
      raise Exception("Invalid base price %s: must be in range [0, 256)" % base)
 
   if type(bucket_exponents) != list:
        raise Exception("Bucket exponents must be a list")

   if len(bucket_exponents) != 16:
        raise Exception("Exactly 16 buckets required")

   for i in xrange(0, len(bucket_exponents)):
       if bucket_exponents[i] < 0 or bucket_exponents[i] > 15:
          raise Exception("Invalid bucket exponent %s (must be in range [0, 16)" % bucket_exponents[i])
   
   if nonalpha_discount <= 0 or nonalpha_discount > 15:
        raise Exception("Invalid non-alpha discount %s: must be in range [0, 16)" % nonalpha_discount)
    
   if no_vowel_discount <= 0 or no_vowel_discount > 15:
        raise Exception("Invalid no-vowel discount %s: must be in range [0, 16)" % no_vowel_discount)

   return True


# version: 2 bytes
# namespace ID: up to 19 bytes
def build( namespace_id, version, reveal_addr, lifetime, coeff, base, bucket_exponents, nonalpha_discount, no_vowel_discount, testset=False ):
   """
   Record to mark the beginning of a namespace import in the blockchain.
   This reveals the namespace ID, and encodes the preorder's namespace rules.
   
   The rules for a namespace are as follows:
   * a name can fall into one of 16 buckets, measured by length.  Bucket 16 incorporates all names at least 16 characters long.
   * the pricing structure applies a multiplicative penalty for having numeric characters, or punctuation characters.
   * the price of a name in a bucket is ((coeff) * (base) ^ (bucket exponent)) / ((numeric discount multiplier) * (punctuation discount multiplier))
   
   Example:
   base = 10
   coeff = 2
   nonalpha discount: 10
   no-vowel discount: 10
   buckets 1, 2: 9
   buckets 3, 4, 5, 6: 8
   buckets 7, 8, 9, 10, 11, 12, 13, 14: 7
   buckets 15, 16+:
   
   The price of "john" would be 2 * 10^8, since "john" falls into bucket 4 and has no punctuation or numerics.
   The price of "john1" would be 2 * 10^6, since "john1" falls into bucket 5 but has a number (and thus receives a 10x discount)
   The price of "john_1" would be 2 * 10^6, since "john_1" falls into bucket 6 but has a number and puncuation (and thus receives a 10x discount)
   The price of "j0hn_1" would be 2 * 10^5, since "j0hn_1" falls into bucket 6 but has a number and punctuation and lacks vowels (and thus receives a 100x discount)
   Namespace ID must be base38.
   
   Format:
   
   0     2   3        7     8     9    10   11   12   13   14    15    16    17       18        20                        39
   |-----|---|--------|-----|-----|----|----|----|----|----|-----|-----|-----|--------|----------|-------------------------|
   magic  op  life    coeff. base 1-2  3-4  5-6  7-8  9-10 11-12 13-14 15-16  nonalpha  version   namespace ID
                                                     bucket exponents         no-vowel
                                                                              discounts
   
   """
   
   rc = namespacereveal_sanity_check( namespace_id, version, lifetime, coeff, base, bucket_exponents, nonalpha_discount, no_vowel_discount )
   if not rc:
       raise Exception("Invalid namespace parameters")
    
   # good to go!
   life_hex = serialize_int( lifetime, 4 )
   coeff_hex = serialize_int( coeff, 1 )
   base_hex = serialize_int( base, 1 )
   bucket_hex = serialize_buckets( bucket_exponents )
   discount_hex = serialize_discounts( nonalpha_discount, no_vowel_discount )
   version_hex = serialize_int( version, 2 )
   namespace_id_hex = hexlify( namespace_id )
   
   readable_script = "NAMESPACE_REVEAL 0x%s 0x%s 0x%s 0x%s 0x%s 0x%s 0x%s" % (life_hex, coeff_hex, base_hex, bucket_hex, discount_hex, version_hex, namespace_id_hex)
   hex_script = blockstack_script_to_hex(readable_script)
   packaged_script = add_magic_bytes(hex_script, testset=testset)
   
   return packaged_script


@state_create( "namespace_id", "namespaces", "check_namespace_collision" )
def check( state_engine, nameop, block_id, checked_ops ):
    """
    Check a NAMESPACE_REVEAL operation to the name database.
    It is only valid if it is the first such operation
    for this namespace, and if it was sent by the same
    sender who sent the NAMESPACE_PREORDER.

    Return True if accepted
    Return False if not
    """

    namespace_id = nameop['namespace_id']
    namespace_id_hash = nameop['preorder_hash']
    sender = nameop['sender']
    namespace_preorder = None

    if not nameop.has_key('sender_pubkey'):
       log.debug("Namespace reveal requires a sender_pubkey (i.e. a p2pkh transaction)")
       return False

    if not nameop.has_key('recipient'):
       log.debug("No recipient script for namespace '%s'" % namespace_id)
       return False

    if not nameop.has_key('recipient_address'):
       log.debug("No recipient address for namespace '%s'" % namespace_id)
       return False

    # well-formed?
    if not is_b40( namespace_id ) or "+" in namespace_id or namespace_id.count(".") > 0:
       log.debug("Malformed namespace ID '%s': non-base-38 characters")
       return False

    # can't be revealed already
    if state_engine.is_namespace_revealed( namespace_id ):
       # this namespace was already revealed
       log.debug("Namespace '%s' is already revealed" % namespace_id )
       return False

    # can't be ready already
    if state_engine.is_namespace_ready( namespace_id ):
       # this namespace already exists (i.e. was already begun)
       log.debug("Namespace '%s' is already registered" % namespace_id )
       return False

    # must currently be preordered
    namespace_preorder = state_engine.get_namespace_preorder( namespace_id_hash )
    if namespace_preorder is None:
       # not preordered
       log.debug("Namespace '%s' is not preordered (no preorder %s)" % (namespace_id, namespace_id_hash) )
       return False

    # must be sent by the same principal who preordered it
    if namespace_preorder['sender'] != sender:
       # not sent by the preorderer
       log.debug("Namespace '%s' is not preordered by '%s'" % (namespace_id, sender))

    # must be a version we support
    if int(nameop['version']) != BLOCKSTACK_VERSION:
       log.debug("Namespace '%s' requires version %s, but this blockstack is version %s" % (namespace_id, nameop['version'], BLOCKSTACK_VERSION))
       return False

    # check fee...
    if not 'op_fee' in namespace_preorder:
       log.debug("Namespace '%s' preorder did not pay the fee" % (namespace_id))
       return False

    namespace_fee = namespace_preorder['op_fee']

    # must have paid enough
    if namespace_fee < price_namespace( namespace_id ):
       # not enough money
       log.debug("Namespace '%s' costs %s, but sender paid %s" % (namespace_id, price_namespace(namespace_id), namespace_fee ))
       return False

    # record preorder
    nameop['block_number'] = namespace_preorder['block_number']
    nameop['reveal_block'] = nameop['block_number']     # start counting down from the preorder, not the reveal
    state_create_put_preorder( nameop, namespace_preorder )
    state_create_put_prior_history( nameop, None )

    # NOTE: not fed into the consensus hash, but necessary for database constraints:
    nameop['ready_block'] = 0
    nameop['op_fee'] = namespace_preorder['op_fee']

    # can begin import
    return True


def get_reveal_recipient_from_outputs( outputs ):
    """
    There are between three outputs:
    * the OP_RETURN
    * the pay-to-address with the "reveal_addr", not the sender's address
    * the change address (i.e. from the namespace preorderer)
    
    Given the outputs from a namespace_reveal operation,
    find the revealer's address's script hex.
    
    By construction, it will be the first non-OP_RETURN 
    output (i.e. the second output).
    """
    
    ret = None
    if len(outputs) != 3:
        # invalid
        raise Exception("Outputs are not from a namespace reveal")

    reveal_output = outputs[1]
   
    output_script = reveal_output['scriptPubKey']
    output_asm = output_script.get('asm')
    output_hex = output_script.get('hex')
    output_addresses = output_script.get('addresses')
    
    if output_asm[0:9] != 'OP_RETURN' and output_hex is not None:
        
        # recipient's script hex
        ret = output_hex

    else:
       raise Exception("No namespace reveal script found")

    return ret


def tx_extract( payload, senders, inputs, outputs, block_id, vtxindex, txid ):
    """
    Extract and return a dict of fields from the underlying blockchain transaction data
    that are useful to this operation.
    """
  
    sender_script = None 
    sender_address = None 
    sender_pubkey_hex = None

    recipient_script = None 
    recipient_address = None 

    try:
       recipient_script = get_reveal_recipient_from_outputs( outputs )
       recipient_address = pybitcoin.script_hex_to_address( recipient_script )

       assert recipient_script is not None 
       assert recipient_address is not None

       # by construction, the first input comes from the principal
       # who sent the reveal transaction...
       assert len(senders) > 0
       assert 'script_pubkey' in senders[0].keys()
       assert 'addresses' in senders[0].keys()

       sender_script = str(senders[0]['script_pubkey'])
       sender_address = str(senders[0]['addresses'][0])

       assert sender_script is not None 
       assert sender_address is not None

       if str(senders[0]['script_type']) == 'pubkeyhash':
          sender_pubkey_hex = get_public_key_hex_from_tx( inputs, sender_address )

    except Exception, e:
       log.exception(e)
       raise Exception("No reveal address")

    parsed_payload = parse( payload, sender_script, recipient_address )
    assert parsed_payload is not None 

    ret = {
       "sender": sender_script,
       "address": sender_address,
       "recipient": recipient_script,
       "recipient_address": recipient_address,
       "reveal_block": block_id,
       "vtxindex": vtxindex,
       "txid": txid,
       "op": NAMESPACE_REVEAL
    }

    ret.update( parsed_payload )

    if sender_pubkey_hex is not None:
        ret['sender_pubkey'] = sender_pubkey_hex

    return ret


def make_outputs( data, inputs, reveal_addr, change_addr, format='bin', testset=False ):
    """
    Make outputs for a namespace reveal:
    [0] OP_RETURN with the name 
    [1] pay-to-address with the *reveal_addr*, not the sender's address.
    [2] change address with the NAMESPACE_PREORDER sender's address
    """
    
    outputs = [
        # main output
        {"script_hex": make_op_return_script(data, format=format),
         "value": 0},
    
        # reveal address
        {"script_hex": make_pay_to_address_script(reveal_addr),
         "value": DEFAULT_DUST_FEE},
        
        # change address
        {"script_hex": make_pay_to_address_script(change_addr),
         "value": calculate_change_amount(inputs, 0, 0)},
    ]

    dust_fee = tx_dust_fee_from_inputs_and_outputs( inputs, outputs )
    outputs[-1]['value'] = calculate_change_amount( inputs, DEFAULT_DUST_FEE, dust_fee )
    return outputs
    
    

def broadcast( namespace_id, reveal_addr, lifetime, coeff, base_cost, bucket_exponents, nonalpha_discount, no_vowel_discount, private_key, blockchain_client, tx_only=False, blockchain_broadcaster=None, testset=False ):
   """
   Propagate a namespace.
   
   Arguments:
   namespace_id         human-readable (i.e. base-40) name of the namespace
   reveal_addr          address to own this namespace until it is ready
   lifetime:            the number of blocks for which names will be valid (pass a negative value for "infinite")
   coeff:               cost multipler
   base_cost:           the base cost (i.e. cost of a 1-character name), in satoshis 
   bucket_exponents:    bucket cost exponents to which to raise the base cost 
   nonalpha_discount:   discount multipler for non-alpha-character names 
   no_vowel_discount:   discount multipler for no-vowel names
   """
   
   if blockchain_broadcaster is None:
       blockchain_broadcaster = blockchain_client 
    
   nulldata = build( namespace_id, BLOCKSTACK_VERSION, reveal_addr, lifetime, coeff, base_cost, bucket_exponents, nonalpha_discount, no_vowel_discount, testset=testset )
   
   # get inputs and from address
   private_key_obj, from_address, inputs = analyze_private_key(private_key, blockchain_client)
    
   # build custom outputs here
   outputs = make_outputs(nulldata, inputs, reveal_addr, from_address, format='hex')
    
   if tx_only:
        unsigned_tx = serialize_transaction( inputs, outputs )
        return {"unsigned_tx": unsigned_tx}
   
   else:
        # serialize, sign, and broadcast the tx
        response = serialize_sign_and_broadcast(inputs, outputs, private_key_obj, blockchain_broadcaster)
            
        # response = {'success': True }
        response.update({'data': nulldata})
            
        return response
   

def parse( bin_payload, sender_script, recipient_address ):
   """
   NOTE: the first three bytes will be missing
   """ 
   
   if len(bin_payload) < MIN_OP_LENGTHS['namespace_reveal']:
       raise AssertionError("Payload is too short to be a namespace reveal")

   off = 0
   life = None 
   coeff = None 
   base = None 
   bucket_hex = None
   buckets = []
   discount_hex = None
   nonalpha_discount = None 
   no_vowel_discount = None
   version = None
   namespace_id = None 
   namespace_id_hash = None
   
   life = int( hexlify(bin_payload[off:off+LENGTHS['blockchain_id_namespace_life']]), 16 )
   
   off += LENGTHS['blockchain_id_namespace_life']
   
   coeff = int( hexlify(bin_payload[off:off+LENGTHS['blockchain_id_namespace_coeff']]), 16 )
   
   off += LENGTHS['blockchain_id_namespace_coeff']
   
   base = int( hexlify(bin_payload[off:off+LENGTHS['blockchain_id_namespace_base']]), 16 )
   
   off += LENGTHS['blockchain_id_namespace_base']
   
   bucket_hex = hexlify(bin_payload[off:off+LENGTHS['blockchain_id_namespace_buckets']])
   
   off += LENGTHS['blockchain_id_namespace_buckets']
   
   discount_hex = hexlify(bin_payload[off:off+LENGTHS['blockchain_id_namespace_discounts']])
   
   off += LENGTHS['blockchain_id_namespace_discounts']
   
   version = int( hexlify(bin_payload[off:off+LENGTHS['blockchain_id_namespace_version']]), 16)
   
   off += LENGTHS['blockchain_id_namespace_version']
   
   namespace_id = bin_payload[off:]
   namespace_id_hash = None
   try:
       namespace_id_hash = hash_name( namespace_id, sender_script, register_addr=recipient_address )
   except:
       log.error("Invalid namespace ID and/or sender script")
       return None
   
   # extract buckets 
   buckets = [int(x, 16) for x in list(bucket_hex)]
   
   # extract discounts
   nonalpha_discount = int( list(discount_hex)[0], 16 )
   no_vowel_discount = int( list(discount_hex)[1], 16 )
  
   try:
       rc = namespacereveal_sanity_check( namespace_id, version, life, coeff, base, buckets, nonalpha_discount, no_vowel_discount )
       if not rc:
           raise Exception("Invalid namespace parameters")

   except Exception, e:
       log.error("Invalid namespace parameters")
       return None 

   return {
      'opcode': 'NAMESPACE_REVEAL',
      'lifetime': life,
      'coeff': coeff,
      'base': base,
      'buckets': buckets,
      'version': version,
      'nonalpha_discount': nonalpha_discount,
      'no_vowel_discount': no_vowel_discount,
      'namespace_id': namespace_id,
      'preorder_hash': namespace_id_hash
   }


def get_fees( inputs, outputs ):
    """
    Blockstack currently does not allow 
    the subsidization of namespaces.
    """
    return (None, None)


def restore_delta( name_rec, block_number, history_index, untrusted_db, testset=False ):
    """
    Find the fields in a name record that were changed by an instance of this operation, at the 
    given (block_number, history_index) point in time in the past.  The history_index is the
    index into the list of changes for this name record in the given block.

    Return the fields that were modified on success.
    Return None on error.
    """

    buckets = name_rec['buckets']

    if type(buckets) in [str, unicode]:
        # serialized bucket list.
        # unserialize 
        reg = "[" + "[ ]*[0-9]+[ ]*," * 15 + "[ ]*[0-9]+[ ]*]"
        match = re.match( reg, buckets )
        if match is None:
            log.error("FATAL: bucket list '%s' is not parsable" % (buckets))
            sys.exit(1)

        try:
            buckets = [int(b) for b in buckets.strip("[]").split(", ")]
        except Exception, e:
            log.exception(e)
            log.error("FATAL: failed to parse '%s' into a 16-elemenet list" % (buckets))
            sys.exit(1)

    name_rec_script = build( str(name_rec['namespace_id']), name_rec['version'], str(name_rec['recipient_address']), \
                             name_rec['lifetime'], name_rec['coeff'], name_rec['base'], buckets, 
                             name_rec['nonalpha_discount'], name_rec['no_vowel_discount'], testset=testset )

    name_rec_payload = unhexlify( name_rec_script )[3:]
    ret_op = parse( name_rec_payload, str(name_rec['sender']), str(name_rec['recipient_address']) )

    return ret_op


def snv_consensus_extras( name_rec, block_id, commit, db ):
    """
    Calculate any derived missing data that goes into the check() operation,
    given the block number, the name record at the block number, and the db.
    """
    return {}
