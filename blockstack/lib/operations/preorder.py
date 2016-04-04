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
    make_pay_to_address_script, b58check_encode, b58check_decode, BlockchainInfoClient, \
    hex_hash160, bin_hash160, BitcoinPrivateKey, BitcoinPublicKey, script_hex_to_address, get_unspents, \
    make_op_return_outputs


from pybitcoin.transactions.outputs import calculate_change_amount
from utilitybelt import is_hex
from binascii import hexlify, unhexlify

from ..b40 import b40_to_hex, is_b40
from ..config import *
from ..scripts import *
from ..hashing import hash_name
from ..nameset import state_preorder

from register import FIELDS as register_FIELDS

# consensus hash fields (ORDER MATTERS!)
FIELDS = [
     'preorder_hash',       # hash(name,sender,register_addr) 
     'consensus_hash',      # consensus hash at time of send
     'sender',              # scriptPubKey hex that identifies the principal that issued the preorder
     'sender_pubkey',       # if sender is a pubkeyhash script, then this is the public key
     'address',             # address from the sender's scriptPubKey
     'block_number',        # block number at which this name was preordered for the first time

     'op',                  # blockstack bytestring describing the operation
     'txid',                # transaction ID
     'vtxindex',            # the index in the block where the tx occurs
     'op_fee',              # blockstack fee (sent to burn address)
]

# fields this operation changes
MUTATE_FIELDS = FIELDS[:]

# fields to back up when processing this operation 
BACKUP_FIELDS = [
    "__all__"
]


def build(name, script_pubkey, register_addr, consensus_hash, name_hash=None, testset=False):
    """
    Takes a name, including the namespace ID (but not the id: scheme), a script_publickey to prove ownership
    of the subsequent NAME_REGISTER operation, and the current consensus hash for this block (to prove that the 
    caller is not on a shorter fork).
    
    Returns a NAME_PREORDER script.
    
    Record format:
    
    0     2  3                                              23             39
    |-----|--|----------------------------------------------|--------------|
    magic op  hash(name.ns_id,script_pubkey,register_addr)   consensus hash
    
    """
    
    if name_hash is None:

        # expect inputs to the hash
        if not is_b40( name ) or "+" in name or name.count(".") > 1:
           raise Exception("Name '%s' has non-base-38 characters" % name)
        
        # name itself cannot exceed LENGTHS['blockchain_id_name']
        if len(NAME_SCHEME) + len(name) > LENGTHS['blockchain_id_name']:
           raise Exception("Name '%s' is too long; exceeds %s bytes" % (name, LENGTHS['blockchain_id_name'] - len(NAME_SCHEME)))
    
        name_hash = hash_name(name, script_pubkey, register_addr=register_addr)

    script = 'NAME_PREORDER 0x%s 0x%s' % (name_hash, consensus_hash)
    hex_script = blockstack_script_to_hex(script)
    packaged_script = add_magic_bytes(hex_script, testset=testset)
    
    return packaged_script
    

@state_preorder("check_preorder_collision")
def check( state_engine, nameop, block_id, checked_ops ):
    """
    Verify that a preorder of a name at a particular block number is well-formed

    NOTE: these *can't* be incorporated into namespace-imports,
    since we have no way of knowning which namespace the
    nameop belongs to (it is blinded until registration).
    But that's okay--we don't need to preorder names during
    a namespace import, because we will only accept names
    sent from the importer until the NAMESPACE_REVEAL operation
    is sent.

    Return True if accepted
    Return False if not.
    """

    from .register import get_num_names_owned

    preorder_name_hash = nameop['preorder_hash']
    consensus_hash = nameop['consensus_hash']
    sender = nameop['sender']

    # must be unique in this block
    # NOTE: now checked externally in the @state_preorder decorator
    """
    for pending_preorders in checked_nameops[ NAME_PREORDER ]:
        if pending_preorders['preorder_name_hash'] == preorder_name_hash:
            log.debug("Name hash '%s' is already preordered" % preorder_name_hash)
            return False
    """

    # must be unique across all pending preorders
    if not state_engine.is_new_preorder( preorder_name_hash ):
        log.debug("Name hash '%s' is already preordered" % preorder_name_hash )
        return False

    # must have a valid consensus hash
    if not state_engine.is_consensus_hash_valid( block_id, consensus_hash ):
        log.debug("Invalid consensus hash '%s'" % consensus_hash )
        return False

    # sender must be beneath quota
    num_names = get_num_names_owned( state_engine, checked_ops, sender ) 
    if num_names >= MAX_NAMES_PER_SENDER:
        log.debug("Sender '%s' exceeded name quota of %s" % (sender, MAX_NAMES_PER_SENDER ))
        return False 

    # burn fee must be present
    if not 'op_fee' in nameop:
        log.debug("Missing preorder fee")
        return False

    return True


def tx_extract( payload, senders, inputs, outputs, block_id, vtxindex, txid ):
    """
    Extract and return a dict of fields from the underlying blockchain transaction data
    that are useful to this operation.

    Required (+ parse):
    sender:  the script_pubkey (as a hex string) of the principal that sent the name preorder transaction
    address:  the address from the sender script
    sender_pubkey_hex: the public key of the sender
    """
  
    sender_script = None 
    sender_address = None 
    sender_pubkey_hex = None

    try:

       # by construction, the first input comes from the principal
       # who sent the registration transaction...
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
       raise Exception("Failed to extract")

    parsed_payload = parse( payload )
    assert parsed_payload is not None 

    ret = {
       "sender": sender_script,
       "address": sender_address,
       "block_number": block_id,
       "vtxindex": vtxindex,
       "txid": txid,
       "op": NAME_PREORDER
    }

    ret.update( parsed_payload )

    if sender_pubkey_hex is not None:
        ret['sender_pubkey'] = sender_pubkey_hex

    return ret


def make_outputs( data, inputs, sender_addr, op_fee, format='bin' ):
    """
    Make outputs for a name preorder:
    [0] OP_RETURN with the name 
    [1] address with the NAME_PREORDER sender's address
    [2] pay-to-address with the *burn address* with the fee
    """
    
    outputs = [
        # main output
        {"script_hex": make_op_return_script(data, format=format),
         "value": 0},
        
        # change address (can be subsidy key)
        {"script_hex": make_pay_to_address_script(sender_addr),
         "value": calculate_change_amount(inputs, 0, 0)},
        
        # burn address
        {"script_hex": make_pay_to_address_script(BLOCKSTACK_BURN_ADDRESS),
         "value": op_fee}
    ]

    dust_fee = tx_dust_fee_from_inputs_and_outputs( inputs, outputs )
    outputs[1]['value'] = calculate_change_amount( inputs, op_fee, dust_fee )
    return outputs


def broadcast(name, private_key, register_addr, consensus_hash, blockchain_client, fee, blockchain_broadcaster=None, subsidy_public_key=None, tx_only=False, testset=False):
    """
    Builds and broadcasts a preorder transaction.

    @subsidy_public_key: if given, the public part of the subsidy key 
    """

    if subsidy_public_key is not None:
        # subsidizing, and only want the tx 
        tx_only = True
    
    # sanity check 
    if subsidy_public_key is None and private_key is None:
        raise Exception("Missing both client public and private key")
    
    if blockchain_broadcaster is None:
        blockchain_broadcaster = blockchain_client 

    from_address = None     # change address
    inputs = None
    private_key_obj = None
    script_pubkey = None    # to be mixed into preorder hash
    
    if subsidy_public_key is not None:
        # subsidizing
        pubk = BitcoinPublicKey( subsidy_public_key )
        
        from_address = BitcoinPublicKey( subsidy_public_key ).address()

        inputs = get_unspents( from_address, blockchain_client )
        script_pubkey = make_p2pkh_script( subsidy_public_key )

    else:
        # ordering directly
        pubk = BitcoinPrivateKey( private_key ).public_key()
        public_key = pubk.to_hex()
        script_pubkey = make_p2pkh_script( public_key )
        
        # get inputs and from address using private key
        private_key_obj, from_address, inputs = analyze_private_key(private_key, blockchain_client)
        
    nulldata = build( name, script_pubkey, register_addr, consensus_hash, testset=testset)
    outputs = make_outputs(nulldata, inputs, from_address, fee, format='hex')
    
    if tx_only:

        unsigned_tx = serialize_transaction( inputs, outputs )
        return {"unsigned_tx": unsigned_tx}
    
    else:
        # serialize, sign, and broadcast the tx
        response = serialize_sign_and_broadcast(inputs, outputs, private_key_obj, blockchain_broadcaster)
        response.update({'data': nulldata})
        return response


def parse(bin_payload):
    """
    Parse a name preorder.
    NOTE: bin_payload *excludes* the leading 3 bytes (magic + op) returned by build.
    """
    
    if len(bin_payload) != LENGTHS['preorder_name_hash'] + LENGTHS['consensus_hash']:
        return None 

    name_hash = hexlify( bin_payload[0:LENGTHS['preorder_name_hash']] )
    consensus_hash = hexlify( bin_payload[LENGTHS['preorder_name_hash']:] )
    
    return {
        'opcode': 'NAME_PREORDER',
        'preorder_hash': name_hash,
        'consensus_hash': consensus_hash
    }


def get_fees( inputs, outputs ):
    """
    Given a transaction's outputs, look up its fees:
    * the first output must be an OP_RETURN, and it must have a fee of 0.
    # the second must be the change address
    * the third must be a burn fee to the burn address.
    
    Return (dust fees, operation fees) on success 
    Return (None, None) on invalid output listing
    """
    if len(outputs) != 3:
        log.debug("Expected 3 outputs; got %s" % len(outputs))
        return (None, None)
    
    # 0: op_return
    if not tx_output_is_op_return( outputs[0] ):
        log.debug("outputs[0] is not an OP_RETURN")
        return (None, None) 
    
    if outputs[0]["value"] != 0:
        log.debug("outputs[0] has value %s'" % outputs[0]["value"])
        return (None, None) 
    
    # 1: change address 
    if script_hex_to_address( outputs[1]["script_hex"] ) is None:
        log.error("outputs[1] has no decipherable change address")
        return (None, None)
    
    # 2: burn address 
    addr_hash = script_hex_to_address( outputs[2]["script_hex"] )
    if addr_hash is None:
        log.error("outputs[2] has no decipherable burn address")
        return (None, None) 
    
    if addr_hash != BLOCKSTACK_BURN_ADDRESS:
        log.error("outputs[2] is not the burn address")
        return (None, None)
    
    dust_fee = (len(inputs) + 2) * DEFAULT_DUST_FEE + DEFAULT_OP_RETURN_FEE
    op_fee = outputs[2]["value"]
    
    return (dust_fee, op_fee)


def restore_delta( name_rec, block_number, history_index, untrusted_db, testset=False ):
    """
    Find the fields in a name record that were changed by an instance of this operation, at the 
    given (block_number, history_index) point in time in the past.  The history_index is the
    index into the list of changes for this name record in the given block.

    Return the fields that were modified on success.
    Return None on error.
    """

    # reconstruct the previous fields of the preorder op...
    name_rec_script = build( None, None, None, str(name_rec['consensus_hash']), \
            name_hash=str(name_rec['preorder_hash']), testset=testset )

    name_rec_payload = unhexlify( name_rec_script )[3:]
    ret_delta = parse( name_rec_payload )
    return ret_delta


def snv_consensus_extras( name_rec, block_id, blockchain_name_data, db ):
    """
    Calculate any derived missing data that goes into the check() operation,
    given the block number, the name record at the block number, and the db.
    """
    return {}
