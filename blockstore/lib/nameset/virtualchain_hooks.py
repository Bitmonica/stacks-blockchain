#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
    Blockstore
    ~~~~~
    copyright: (c) 2014 by Halfmoon Labs, Inc.
    copyright: (c) 2015 by Blockstack.org
    
    This file is part of Blockstore
    
    Blockstore is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.
    
    Blockstore is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU General Public License for more details.
    You should have received a copy of the GNU General Public License
    along with Blockstore.  If not, see <http://www.gnu.org/licenses/>.
"""

# Hooks to the virtual chain's state engine that bind our namedb to the virtualchain package.

import os
from binascii import hexlify, unhexlify
import time

import pybitcoin 
import traceback
import json

from .namedb import BlockstoreDB

from ..config import *
from ..operations import parse_preorder, parse_registration, parse_update, parse_transfer, parse_revoke, \
    parse_name_import, parse_namespace_preorder, parse_namespace_reveal, parse_namespace_ready, \
    get_transfer_recipient_from_outputs, get_import_update_hash_from_outputs, get_registration_recipient_from_outputs, \
    SERIALIZE_FIELDS

import virtualchain

log = virtualchain.session.log
blockstore_db = None
last_load_time = 0

def get_burn_fee_from_outputs( outputs ):
    """
    Given the set of outputs, find the fee sent 
    to our burn address.
    
    Return the fee on success
    Return None if not found
    """
    
    ret = None
    for output in outputs:
       
        output_script = output['scriptPubKey']
        output_asm = output_script.get('asm')
        output_hex = output_script.get('hex')
        output_addresses = output_script.get('addresses')
        
        if output_asm[0:9] != 'OP_RETURN' and BLOCKSTORE_BURN_ADDRESS == output_addresses[0]:
            
            # recipient's script_pubkey and address
            ret = int(output['value']*(10**8))
            break
    
    return ret 
    

def get_public_key_hex_from_tx( inputs, address ):
    """
    Given a list of inputs and outputs and the address of one of the inputs,
    find the public key.
    """
    
    ret = None 
    
    for inp in inputs:
        
        input_scriptsig = inp.get('scriptSig', None )
        if input_scriptsig is None:
            continue 
        
        input_asm = input_scriptsig.get("asm")
        
        if len(input_asm.split(" ")) >= 2:
            
            # public key is the second hex string.  verify it matches the address
            pubkey_hex = input_asm.split(" ")[1]
            pubkey = None 
            
            try:
                pubkey = pybitcoin.BitcoinPublicKey( str(pubkey_hex) ) 
            except Exception, e: 
                traceback.print_exc()
                log.warning("Invalid public key '%s'" % pubkey_hex)
                continue 
            
            if address != pubkey.address():
                continue 
            
            ret = pubkey_hex
            break
        
    return ret 


def parse_blockstore_op_data( opcode, payload, sender, recipient=None, recipient_address=None, import_update_hash=None ):
    """
    Parse a string of binary data (nulldata from a blockchain transaction) into a blockstore operation.
    
    full OP_RETURN data format (once unhex'ed):
    
    0           2      3                                   40
    |-----------|------|-----------------------------------|
    magic bytes opcode  payload
    (consumed)  (arg)   (arg)
    
    We are given opcode and payload as arguments.
    
    Returns a parsed operation on success
    Returns None if no operation could be parsed.
    """

    op = None 
    data = hexlify(payload)
    
    if opcode == NAME_PREORDER and len(payload) >= MIN_OP_LENGTHS['preorder']:
        log.debug( "Parse NAME_PREORDER: %s" % data )
        op = parse_preorder(payload)
        
    elif (opcode == NAME_REGISTRATION and len(payload) >= MIN_OP_LENGTHS['registration']):
        log.debug( "Parse NAME_REGISTRATION: %s" % data )
        op = parse_registration(payload)
        
    elif opcode == NAME_UPDATE and len(payload) >= MIN_OP_LENGTHS['update']:
        log.debug( "Parse NAME_UPDATE: %s" % data )
        op = parse_update(payload)
        
    elif (opcode == NAME_TRANSFER and len(payload) >= MIN_OP_LENGTHS['transfer']):
        log.debug( "Parse NAME_TRANSFER: %s" % data )
        op = parse_transfer(payload, recipient )
    
    elif (opcode == NAME_REVOKE and len(payload) >= MIN_OP_LENGTHS['revoke']):
        log.debug( "Parse NAME_REVOKE: %s" % data )
        op = parse_revoke(payload)
        
    elif opcode == NAME_IMPORT and len(payload) >= MIN_OP_LENGTHS['name_import']:
        log.debug( "Parse NAME_IMPORT: %s" % data )
        op = parse_name_import( payload, recipient, import_update_hash )
        
    elif opcode == NAMESPACE_PREORDER and len(payload) >= MIN_OP_LENGTHS['namespace_preorder']:
        log.debug( "Parse NAMESPACE_PREORDER: %s" % data)
        op = parse_namespace_preorder( payload )
        
    elif opcode == NAMESPACE_REVEAL and len(payload) >= MIN_OP_LENGTHS['namespace_reveal']:
        log.debug( "Parse NAMESPACE_REVEAL: %s" % data )
        op = parse_namespace_reveal( payload, sender, recipient_address )
         
    elif opcode == NAMESPACE_READY and len(payload) >= MIN_OP_LENGTHS['namespace_ready']:
        log.debug( "Parse NAMESPACE_READY: %s" % data )
        op = parse_namespace_ready( payload )
    
    else:
        log.warning("Unrecognized op: code='%s', data=%s, len=%s" % (opcode, data, len(payload)))
        
    return op


def get_virtual_chain_name(testset=False):
   """
   (required by virtualchain state engine)
   
   Get the name of the virtual chain we're building.
   """
   
   if testset:
       return "blockstore-test"
   
   else:
       return "blockstore"


def get_virtual_chain_version():
   """
   (required by virtualchain state engine)
   
   Get the version string for this virtual chain.
   """
   return VERSION


def get_opcodes():
   """
   (required by virtualchain state engine)
   
   Get the list of opcodes we're looking for.
   """
   return OPCODES 


def get_op_processing_order():
   """
   (required by virtualchain state engine)
   
   Give a hint as to the order in which we process operations 
   """
   return None 


def get_magic_bytes():
   """
   (required by virtualchain state engine)
   
   Get the magic byte sequence for our OP_RETURNs
   """
   blockstore_opts = default_blockstore_opts( virtualchain.get_config_filename() )
   if blockstore_opts['testset']:
       return MAGIC_BYTES_TESTSET
   
   else:
       return MAGIC_BYTES_MAINSET


def get_first_block_id():
   """
   (required by virtualchain state engine)
   
   Get the id of the first block to start indexing.
   """ 
   blockstore_opts = default_blockstore_opts( virtualchain.get_config_filename() )
   start_block = None
   
   if TESTNET:
       if blockstore_opts['testset']:
           start_block = FIRST_BLOCK_TESTNET_TESTSET
       else:
           start_block = FIRST_BLOCK_TESTNET
   else:
       if blockstore_opts['testset']:
           start_block = FIRST_BLOCK_MAINNET_TESTSET
       else:
           start_block = FIRST_BLOCK_MAINNET

   return start_block


def get_db_state():
   """
   (required by virtualchain state engine)
   
   Callback to the virtual chain state engine.
   
   Get a handle to our state engine implementation
   (i.e. our name database)
   """
   
   global blockstore_db
   global last_load_time
   
   now = time.time()
   
   # force invalidation
   if now - last_load_time > REINDEX_FREQUENCY:
       blockstore_db = None
       
   if blockstore_db is not None:
      return blockstore_db 
   
   db_filename = virtualchain.get_db_filename()
   
   log.info("(Re)Loading blockstore state from '%s'" % db_filename )
   blockstore_db = BlockstoreDB( db_filename )
   
   last_load_time = time.time()
   
   return blockstore_db


def db_parse( block_id, opcode, data, senders, inputs, outputs, fee, db_state=None ):
   """
   (required by virtualchain state engine)
   
   Parse a blockstore operation from a transaction's nulldata (data) and a list of outputs, as well as 
   optionally the list of transaction's senders and the total fee paid.
   
   Return a parsed operation, and will also optionally have:
   * "sender": the first (primary) sender's script_pubkey.
   * "address": the sender's bitcoin address
   * "fee": the total fee paid for this record.
   * "recipient": the first non-OP_RETURN output's script_pubkey.
   * "sender_pubkey": the sender's public key (hex string)
   
   NOTE: the transactions that our tools put have a single sender, and a single output address.
   This is assumed by this code.  An exception will be raised if these criteria are not met.
   """

   sender = None 
   recipient = None
   import_update_hash = None
   address = None
   sender_pubkey_hex = None
   
   if len(senders) == 0:
      raise Exception("No senders for (%s, %s)" % (opcode, hexlify(data)))
   
   if 'script_pubkey' not in senders[0].keys():
      raise Exception("No script_pubkey in sender of (%s, %s)" % (opcode, hexlify(data)))
   
   if 'addresses' not in senders[0].keys():
      raise Exception("No addresses in sender of (%s, %s)" % (opcode, hexlify(data)))
   
   if len(senders[0]['addresses']) != 1:
      raise Exception("Multiple addresses are unsupported for (%s, %s)" % (opcode, hexlify(data)))
   
   sender = str(senders[0]['script_pubkey'])
   address = str(senders[0]['addresses'][0])
   sender_pubkey_hex = get_public_key_hex_from_tx( inputs, address )
   
   if sender_pubkey_hex is None:
      raise Exception("Could not determine public key for '%s'" % address)
   
   recipient = None 
   recipient_address = None 
   
   op_fee = get_burn_fee_from_outputs( outputs )
   
   if opcode in [NAME_REGISTRATION, NAMESPACE_REVEAL]:
      # these operations have a separate change address from the sender 
      try:
         recipient = get_registration_recipient_from_outputs( outputs )
         recipient_address = pybitcoin.script_hex_to_address( recipient )
      except Exception, e:
         log.exception(e)
         raise Exception("No registration address for (%s, %s)" % (opcode, hexlify(data)))
     
   
   if opcode in [NAME_IMPORT, NAME_TRANSFER]:
      # these operations have a designated recipient
      try:
         recipient = get_transfer_recipient_from_outputs( outputs )
         recipient_address = pybitcoin.script_hex_to_address( recipient )
      except Exception, e:
         log.exception(e)
         raise Exception("No recipient for (%s, %s)" % (opcode, hexlify(data)))
      
      
   if opcode in [NAME_IMPORT]:
      # this operation has an update hash embedded as a phony recipient 
      try:
         import_update_hash = get_import_update_hash_from_outputs( outputs, recipient )
      except Exception, e:
         log.exception(e)
         raise Exception("No update hash for (%s, %s)" % (opcode, hexlify(data)))
     
         
   op = parse_blockstore_op_data(opcode, data, sender, recipient=recipient, recipient_address=recipient_address, import_update_hash=import_update_hash )
   
   if op is not None:
      
      # store the above ancillary data with the opcode, so our namedb can look it up later 
      if fee is not None:
         op['fee'] = fee 
         
      if op_fee is not None:
         op['op_fee'] = op_fee 
      
      # sender script_pubkey, change address, and sender's public key
      op['sender'] = sender 
      op['address'] = address 
      
      if recipient is not None:
         op['recipient'] = recipient
      
      if recipient_address is not None:
         op['recipient_address'] = recipient_address
      
      if sender_pubkey_hex is not None:
         op['sender_pubkey'] = sender_pubkey_hex
         
   return op


def db_check( block_id, checked_ops, opcode, op, db_state=None ):
   """
   (required by virtualchain state engine)
   
   Given the block ID and a parsed operation, check to see if this is a *valid* operation.
   Is this operation consistent with blockstore's rules?
   
   checked_ops is a dict that maps opcodes to operations already checked by
   this method for this block.
   
   Return True if it's valid; False if not.
   """
   
   if db_state is not None:
         
      db = db_state
      rc = False
      
      if opcode not in OPCODES:
         log.error("Unrecognized opcode '%s'" % (opcode))
         return False 
      
      if opcode == NAME_PREORDER:
         rc = db.log_preorder( checked_ops, op, block_id )
      
      elif opcode == NAME_REGISTRATION:
         rc = db.log_registration( checked_ops, op, block_id )
      
      elif opcode == NAME_UPDATE:
         rc = db.log_update( checked_ops, op, block_id )
      
      elif opcode == NAME_TRANSFER:
         rc = db.log_transfer( checked_ops, op, block_id )
      
      elif opcode == NAME_REVOKE:
         rc = db.log_revoke( checked_ops, op, block_id )
      
      elif opcode == NAME_IMPORT:
         rc = db.log_name_import( checked_ops, op, block_id )
         
      elif opcode == NAMESPACE_PREORDER:
         rc = db.log_namespace_preorder( checked_ops, op, block_id )
      
      elif opcode == NAMESPACE_REVEAL:
         rc = db.log_namespace_reveal( checked_ops, op, block_id )
      
      elif opcode == NAMESPACE_READY:
         rc = db.log_namespace_ready( checked_ops, op, block_id )
      
      if rc:
         log.debug("ACCEPT op '%s' (%s)" % (opcode, op))
      else:
         log.debug("REJECT op '%s' (%s)" % (opcode, op))
         
      return rc
   
   else:
      log.error("No state engine defined")
      return False
   
   
def db_commit( block_id, opcode, op, txid, db_state=None ):
   """
   (required by virtualchain state engine)
   
   Given a block ID and checked opcode, record it as 
   part of the database.  This does *not* need to write 
   the data to persistent storage, since save() will be 
   called once per block processed.
  
   Returns a new name record on success, which will 
   be fed into db_serialize to translate into a string
   to be used to generate this block's consensus hash.
   """
   
   new_namerec = None 

   if db_state is not None:
      
      db = db_state
      
      if op is not None:

        # committing an operation
        # pass along txid 
        op['txid'] = txid
        log.debug("COMMIT op '%s' (%s)" % (opcode, op))
            
        if opcode == NAME_PREORDER:
            new_namerec = db.commit_preorder( op, block_id )

        elif opcode == NAME_REGISTRATION:
            new_namerec = db.commit_registration( op, block_id )

        elif opcode == NAME_UPDATE:
            new_namerec = db.commit_update( op, block_id )

        elif opcode == NAME_TRANSFER:
            new_namerec = db.commit_transfer( op, block_id )

        elif opcode == NAME_REVOKE:
            new_namerec = db.commit_revoke( op, block_id )
            
        elif opcode == NAME_IMPORT:
            new_namerec = db.commit_name_import( op, block_id )
            
        elif opcode == NAMESPACE_PREORDER:
            new_namerec = db.commit_namespace_preorder( op, block_id )
            
        elif opcode == NAMESPACE_REVEAL:
            new_namerec = db.commit_namespace_reveal( op, block_id )

        elif opcode == NAMESPACE_READY:
            new_namerec = db.commit_namespace_ready( op, block_id )
      
      else:

        # final commit before save
        # do expirations
        log.debug("Clear all expired names at %s" % block_id )
        expired_names = db.commit_name_expire_all( block_id )
        
        log.debug("Clear all expired preorders at %s" % block_id )
        expired_name_hashes = db.commit_preorder_expire_all( block_id )
        
        log.debug("Clear all expired namespace preorders at %s" % block_id )
        expired_namespace_hashes = db.commit_namespace_preorder_expire_all( block_id )
        
        log.debug("Clear all expired partial namespace imports at %s" % block_id )
        expired_namespaces = db.commit_namespace_reveal_expire_all( block_id )

        # merge expired namespaces...
        expired_namespace_ids = expired_namespaces.keys()[:]
        for nsid in expired_namespace_ids:
            expired_names += expired_namespaces[ nsid ]

        expired_names.sort()
        expired_name_hashes.sort()
        expired_namespace_hashes.sort()
        expired_namespace_ids.sort()

        # create a virtual nameop that encompasses the above expirations
        new_namerec = {
            "opcode": "VIRTUAL_EXPIRE",
            "expired_names": str(len(expired_names)) + ":" + ",".join( expired_names ),
            "expired_name_hashes": str(len(expired_name_hashes)) + ":" + ",".join( expired_name_hashes ),
            "expired_namespace_hashes": str(len(expired_namespace_hashes)) + ":" + ",".join( expired_namespace_hashes ),
            "expired_namespace_ids": str(len(expired_namespace_ids)) + ":" + ",".join( expired_namespace_ids )
        }
        
   else:
      log.error("No state engine defined")
      return None
  
   return new_namerec


def db_serialize( op, nameop, db_state=None ):
   """
   (required by virtualchain state engine)
   
   Serialize a given name operation
   """
   
   if db_state is not None:

      fields = None

      # special case: final nameop
      if nameop.has_key('opcode') and nameop['opcode'] == 'VIRTUAL_EXPIRE':
          fields = ['expired_names', 'expired_name_hashes', 'expired_namespace_hashes', 'expired_namespace_ids']
      
      else:
          opcode_name = OPCODE_NAMES.get( op, None )
          if opcode_name is None:
              log.error("No such opcode '%s'" % op)
              return None 

          fields = SERIALIZE_FIELDS.get( opcode_name, None )
          if fields is None:
              log.error("BUG: unrecongnized opcode '%s'" % opcode_name )
              return None 

      fields = sorted( fields )
      field_values = ",".join( str(nameop[field]) for field in fields ] )

      return op + ":" + field_values
      
   else:
      log.error("No state engine defined")
      return None


def db_save( block_id, consensus_hash, pending_ops, filename, db_state=None ):
   """
   (required by virtualchain state engine)
   
   Save all persistent state to stable storage.
   Clear out expired names in the process.
   Called once per block.
   
   Return True on success
   Return False on failure.
   """
   
   db = db_state 
   
   # remove expired names before saving
   if db is not None:
      
      # see if anything actually changed 
      if len(pending_ops) > 0:
          
          # state has changed 
          return db.save_db( filename )
      
      else:
          
          # all good 
          return True
   
   else:
      log.error("No state engine defined")
      return False 

