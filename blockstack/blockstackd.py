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

import argparse
import logging
import os
import sys
import subprocess
import signal
import json
import datetime
import traceback
import httplib
import time
import socket
import math
import random
import shutil
import tempfile
import binascii
import copy
import threading
import atexit
import errno

import virtualchain

try:
    import blockstack_client
except:
    # storage API won't work
    blockstack_client = None

log = virtualchain.get_logger("blockstack-server")
from ConfigParser import SafeConfigParser

import pybitcoin
from txjsonrpc.netstring import jsonrpc

from lib import nameset as blockstack_state_engine
from lib.config import REINDEX_FREQUENCY, DEFAULT_DUST_FEE
from lib import *

import lib.nameset.virtualchain_hooks as virtualchain_hooks
import lib.config as config

# global variables, for use with the RPC server and the twisted callback
blockstack_opts = None
bitcoind = None
bitcoin_opts = None
utxo_opts = None
blockchain_client = None
blockchain_broadcaster = None
blockstackd_api_server = None

def get_bitcoind( new_bitcoind_opts=None, reset=False, new=False ):
   """
   Get or instantiate our bitcoind client.
   Optionally re-set the bitcoind options.
   """
   global bitcoind
   global bitcoin_opts

   if reset:
       bitcoind = None

   elif not new and bitcoind is not None:
      return bitcoind

   if new or bitcoind is None:
      if new_bitcoind_opts is not None:
         bitcoin_opts = new_bitcoind_opts

      new_bitcoind = None
      try:
         if bitcoin_opts.has_key('bitcoind_mock') and bitcoin_opts['bitcoind_mock']:
            # make a mock connection
            import tests.mock_bitcoind
            new_bitcoind = tests.mock_bitcoind.connect_mock_bitcoind( bitcoin_opts, reset=reset )

         else:
            new_bitcoind = virtualchain.connect_bitcoind( bitcoin_opts )

         if new:
             return new_bitcoind

         else:
             # save for subsequent reuse
             bitcoind = new_bitcoind
             return bitcoind

      except Exception, e:
         log.exception( e )
         return None


def get_bitcoin_opts():
   """
   Get the bitcoind connection arguments.
   """

   global bitcoin_opts
   return bitcoin_opts


def get_utxo_opts():
   """
   Get UTXO provider options.
   """
   global utxo_opts
   return utxo_opts


def get_blockstack_opts():
   """
   Get blockstack configuration options.
   """
   global blockstack_opts
   return blockstack_opts


def set_bitcoin_opts( new_bitcoin_opts ):
   """
   Set new global bitcoind operations
   """
   global bitcoin_opts
   bitcoin_opts = new_bitcoin_opts


def set_utxo_opts( new_utxo_opts ):
   """
   Set new global chian.com options
   """
   global utxo_opts
   utxo_opts = new_utxo_opts


def get_pidfile_path():
   """
   Get the PID file path.
   """
   working_dir = virtualchain.get_working_dir()
   pid_filename = blockstack_state_engine.get_virtual_chain_name() + ".pid"
   return os.path.join( working_dir, pid_filename )


def get_pid_from_pidfile( pidfile_path ):
    """
    Get the PID from a pidfile
    """
    with open( pidfile_path, "r" ) as f:
        txt = f.read()

    try:
        pid = int( txt.strip() )
    except:
        raise Exception("Invalid PID '%s'" % pid)

    return pid


def put_pidfile( pidfile_path, pid ):
    """
    Put a PID into a pidfile
    """
    with open( pidfile_path, "w" ) as f:
        f.write("%s" % pid)

    return 


def get_tacfile_path( testset=False ):
   """
   Get the TAC file path for our service endpoint.
   Should be in the same directory as this module.
   """
   working_dir = os.path.abspath(os.path.dirname(__file__))
   tac_filename = ""

   if testset:
      tac_filename = blockstack_state_engine.get_virtual_chain_name() + "-testset.tac"
   else:
      tac_filename = blockstack_state_engine.get_virtual_chain_name() + ".tac"

   return os.path.join( working_dir, tac_filename )


def get_logfile_path():
   """
   Get the logfile path for our service endpoint.
   """
   working_dir = virtualchain.get_working_dir()
   logfile_filename = blockstack_state_engine.get_virtual_chain_name() + ".log"
   return os.path.join( working_dir, logfile_filename )


def get_lastblock():
    """
    Get the last block processed.
    """
    lastblock_filename = virtualchain.get_lastblock_filename()
    if not os.path.exists( lastblock_filename ):
        return None

    try:
        with open(lastblock_filename, "r") as f:
           lastblock_txt = f.read()

        lastblock = int(lastblock_txt.strip())
        return lastblock
    except:
        return None


def json_traceback():
    exception_data = traceback.format_exc().splitlines()
    return {
        "error": exception_data[-1],
        "traceback": exception_data
    }


def get_utxo_provider_client():
   """
   Get or instantiate our blockchain UTXO provider's client.
   Return None if we were unable to connect
   """

   # acquire configuration (which we should already have)
   blockstack_opts, bitcoin_opts, utxo_opts, dht_opts = configure( interactive=False )

   try:
       utxo_provider = connect_utxo_provider( utxo_opts )
       return utxo_provider
   except Exception, e:
       log.exception(e)
       return None


def get_tx_broadcaster():
   """
   Get or instantiate our blockchain UTXO provider's transaction broadcaster.
   fall back to the utxo provider client, if one is not designated
   """

   # acquire configuration (which we should already have)
   blockstack_opts, blockchain_opts, utxo_opts, dht_opts = configure( interactive=False )

   # is there a particular blockchain client we want for importing?
   if 'tx_broadcaster' not in blockstack_opts:
       return get_utxo_provider_client()

   broadcaster_opts = default_utxo_provider_opts( blockstack_opts['tx_broadcaster'] )

   try:
       blockchain_broadcaster = connect_utxo_provider( broadcaster_opts )
       return blockchain_broadcaster
   except:
       log.exception(e)
       return None


def get_name_cost( name ):
    """
    Get the cost of a name, given the fully-qualified name.
    Do so by finding the namespace it belongs to (even if the namespace is being imported).
    Return None if the namespace has not been declared
    """
    db = get_db_state()

    namespace_id = get_namespace_from_name( name )
    if namespace_id is None or len(namespace_id) == 0:
        return None

    namespace = db.get_namespace( namespace_id )
    if namespace is None:
        # maybe importing?
        namespace = db.get_namespace_reveal( namespace_id )

    if namespace is None:
        # no such namespace
        return None

    name_fee = price_name( get_name_from_fq_name( name ), namespace )
    return name_fee


def get_max_subsidy( testset=False ):
    """
    Get the maximum subsidy we offer, and get a key with a suitable balance
    to pay the subsidy.

    Return (subsidy, key)
    """

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )
    if blockstack_opts.get("max_subsidy") is None:
        return (None, None)

    return blockstack_opts["max_subsidy"]


def make_subsidized_tx( unsigned_tx, fee_cb, max_subsidy, subsidy_key, blockchain_client_inst ):
    """
    Create a subsidized transaction
    transaction and a callback that determines the fee structure.
    """

    # subsidize the transaction
    subsidized_tx = tx_make_subsidizable( unsigned_tx, fee_cb, max_subsidy, subsidy_key, blockchain_client_inst )
    if subsidized_tx is None:
        return {"error": "Order exceeds maximum subsidy"}

    else:
        resp = {
            "subsidized_tx": subsidized_tx
        }
        return resp


def broadcast_subsidized_tx( subsidized_tx ):
    """
    Broadcast a subsidized tx to the blockchain.
    """
    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    # broadcast
    response = pybitcoin.broadcast_transaction( subsidized_tx, broadcaster_client_inst, format='hex' )
    return response


def blockstack_name_preorder( name, privatekey, register_addr, tx_only=False, subsidy_key=None, testset=False, consensus_hash=None ):
    """
    Preorder a name.

    @name: the name to preorder
    @register_addr: the address that will own the name upon registration
    @privatekey: the private key that will pay for the preorder. Can be None if we're subsidizing (in which case subsidy_key is required)
    @tx_only: if True, then return only the unsigned serialized transaction.  Do not broadcast it.
    @pay_fee: if False, then return a subsidized serialized transaction, where we have signed our
    inputs/outputs with SIGHASH_ANYONECANPAY.  The caller will need to sign their input and then
    broadcast it.
    @subsidy_key: if given, then this transaction will be subsidized with this key and returned (but not broadcasted)
    This forcibly sets tx_only=True and pay_fee=False.

    Return a JSON object on success.
    Return a JSON object with 'error' set on error.
    """

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    db = get_db_state()

    if consensus_hash is None:
        consensus_hash = db.get_current_consensus()

    if consensus_hash is None:
        # consensus hash must exist
        return {"error": "Nameset snapshot not found."}

    if db.is_name_registered( name ):
        # name can't be registered
        return {"error": "Name already registered"}

    namespace_id = get_namespace_from_name( name )

    if not db.is_namespace_ready( namespace_id ):
        # namespace must be ready; otherwise this is a waste
        return {"error": "Namespace is not ready"}

    name_fee = get_name_cost( name )
    
    log.debug("The price of '%s' is %s satoshis" % (name, name_fee))

    if privatekey is not None:
        privatekey = str(privatekey)

    public_key = None
    if subsidy_key is not None:
        subsidy_key = str(subsidy_key)
        tx_only = True

        # the sender will be the subsidizer (otherwise it will be the given private key's owner)
        public_key = BitcoinPrivateKey( subsidy_key ).public_key().to_hex()

    try:
        resp = preorder_name(str(name), privatekey, str(register_addr), str(consensus_hash), blockchain_client_inst, \
            name_fee, testset=blockstack_opts['testset'], subsidy_public_key=public_key, tx_only=tx_only, blockchain_broadcaster=broadcaster_client_inst )
    except:
        return json_traceback()

    if subsidy_key is not None:
        # sign each input
        inputs, outputs, _, _ = tx_deserialize( resp['unsigned_tx'] )
        tx_signed = tx_serialize_and_sign( inputs, outputs, subsidy_key )

        resp = {
            'subsidized_tx': tx_signed
        }


    log.debug('preorder <name, consensus_hash>: <%s, %s>' % (name, consensus_hash))

    return resp


def blockstack_name_preorder_multi( name_list, privatekey, register_addr_list, tx_only=False, subsidy_key=None, testset=False, consensus_hash=None ):
    """
    Preorder a list of names.  They must each be registered with the same private key

    @name_list: the names to preorder
    @register_addr_list: the addresses that will own the names upon registration.  register_addr_list[i] will own name_list[i].
    @privatekey: the private key that will pay for the preorder. Can be None if we're subsidizing (in which case subsidy_key is required)
    @tx_only: if True, then return only the unsigned serialized transaction.  Do not broadcast it.
    @pay_fee: if False, then return a subsidized serialized transaction, where we have signed our
    inputs/outputs with SIGHASH_ANYONECANPAY.  The caller will need to sign their input and then
    broadcast it.
    @subsidy_key: if given, then this transaction will be subsidized with this key and returned (but not broadcasted)
    This forcibly sets tx_only=True and pay_fee=False.

    Return a JSON object on success.
    Return a JSON object with 'error' set on error.
    """

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    if len(name_list) != len(register_addr_list):
        return {"error": "Need equal numbers of names and registration addresses"}

    if len(name_list) != len(set(name_list)):
        return {"error": "Duplicate names"}

    db = get_db_state()

    if consensus_hash is None:
        consensus_hash = db.get_current_consensus()

    if consensus_hash is None:
        # consensus hash must exist
        return {"error": "Nameset snapshot not found."}

    name_fee_total = 0

    for i in xrange( 0, len(name_list) ):

        name_list[i] = str(name_list[i])
        register_addr_list[i] = str(register_addr_list[i])

        name = name_list[i]
        if db.is_name_registered( name ):
            # name can't be registered
            return {"error": "At least one name already registered"}

        namespace_id = get_namespace_from_name( name )

        if not db.is_namespace_ready( namespace_id ):
            # namespace must be ready; otherwise this is a waste
            return {"error": "Namespace is not ready"}

        name_fee = get_name_cost( name )
        name_fee_total += name_fee

        log.debug("The price of %s is %s satoshis" % (name, name_fee))


    if privatekey is not None:
        privatekey = str(privatekey)

    public_key = None
    if subsidy_key is not None:
        subsidy_key = str(subsidy_key)
        tx_only = True

        # the sender will be the subsidizer (otherwise it will be the given private key's owner)
        public_key = BitcoinPrivateKey( subsidy_key ).public_key().to_hex()

    try:
        resp = preorder_name_multi(name_list, privatekey, register_addr_list, str(consensus_hash), blockchain_client_inst, \
            name_fee_total, testset=blockstack_opts['testset'], subsidy_public_key=public_key, tx_only=tx_only )
    except:
        return json_traceback()

    if subsidy_key is not None:
        # sign each input
        inputs, outputs, _, _ = tx_deserialize( resp['unsigned_tx'] )
        tx_signed = tx_serialize_and_sign( inputs, outputs, subsidy_key )

        resp = {
            'subsidized_tx': tx_signed
        }


    log.debug('preorder_multi <names, consensus_hash>: <%s, %s>' % (", ".join(name_list), consensus_hash))

    return resp


def blockstack_name_register( name, privatekey, register_addr, renewal_fee=None, tx_only=False, subsidy_key=None, user_public_key=None, testset=False, consensus_hash=None ):
    """
    Register or renew a name

    @name: the name to register
    @register_addr: the address that will own the name (must be the same as the address
    given on preorder)
    @privatekey: if registering, this is the key that will pay for the registration (must
    be the same key as the key used to preorder).  If renewing, this is the private key of the
    name owner's address.
    @renewal_fee: if given, this is the fee to renew the name (must be at least the
    cost of the name itself)
    @tx_only: if True, then return only the unsigned serialized transaction. Do not broadcast it.
    @pay_fee: if False, then do not pay any associated dust or operational fees.  This should be used
    to generate a signed serialized transaction that another key will later subsidize

    Return a JSON object on success
    Return a JSON object with 'error' set on error.
    """

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    db = get_db_state()

    if db.is_name_registered( name ) and renewal_fee is None:
        # *must* be given, so we don't accidentally charge
        return {"error": "Name already registered"}

    public_key = None
    if subsidy_key is not None:
        subsidy_key = str(subsidy_key)
        tx_only = True

        # the sender will be the subsidizer (otherwise it will be the given private key's owner)
        public_key = BitcoinPrivateKey( subsidy_key ).public_key().to_hex()

    try:
        resp = register_name(str(name), privatekey, str(register_addr), blockchain_client_inst, renewal_fee=renewal_fee, \
            tx_only=tx_only, blockchain_broadcaster=broadcaster_client_inst, testset=blockstack_opts['testset'], \
            subsidy_public_key=public_key, user_public_key=user_public_key )
    except:
        return json_traceback()

    if subsidy_key is not None and renewal_fee is not None:
        resp = make_subsidized_tx( resp['unsigned_tx'], registration_fees, blockstack_opts['max_subsidy'], subsidy_key, blockchain_client_inst )

    elif subsidy_key is not None:
        # sign each input
        inputs, outputs, _, _ = tx_deserialize( resp['unsigned_tx'] )
        tx_signed = tx_serialize_and_sign( inputs, outputs, subsidy_key )

        resp = {
            'subsidized_tx': tx_signed
        }


    log.debug("name register/renew: %s" % name)
    return resp


def blockstack_name_update( name, data_hash, privatekey, tx_only=False, user_public_key=None, subsidy_key=None, testset=False, consensus_hash=None ):
    """
    Update a name with new data.

    @name: the name to update
    @data_hash: the hash of the new name record
    @privatekey: the private key of the owning address.
    @tx_only: if True, then return only the unsigned serialized transaction.  Do not broadcast it.
    @pay_fee: if False, then do not pay any associated dust or operational fees.  This should be
    used to generate a signed serialized transaction that another key will later subsidize.

    Return a JSON object on success
    Return a JSON object with 'error' set on error.
    """
    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}


    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    db = get_db_state()

    if consensus_hash is None:
        consensus_hash = db.get_current_consensus()

    if consensus_hash is None:
        return {"error": "Nameset snapshot not found."}

    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    try:
        resp = update_name(str(name), str(data_hash), str(consensus_hash), privatekey, blockchain_client_inst, \
            tx_only=tx_only, blockchain_broadcaster=broadcaster_client_inst, user_public_key=user_public_key, testset=blockstack_opts['testset'])
    except:
        return json_traceback()

    if subsidy_key is not None:
        # subsidize the transaction
        resp = make_subsidized_tx( resp['unsigned_tx'], update_fees, blockstack_opts['max_subsidy'], subsidy_key, blockchain_client_inst )

    log.debug('name update <name, data_hash, consensus_hash>: <%s, %s, %s>' % (name, data_hash, consensus_hash))
    return resp


def blockstack_name_transfer( name, address, keepdata, privatekey, user_public_key=None, subsidy_key=None, tx_only=False, testset=False, consensus_hash=None ):
    """
    Transfer a name to a new address.

    @name: the name to transfer
    @address:  the new address to own the name
    @keepdata: if True, then keep the name record tied to the name.  Otherwise, discard it.
    @privatekey: the private key of the owning address.
    @tx_only: if True, then return only the unsigned serialized transaction.  Do not broadcast it.
    @pay_fee: if False, then do not pay any associated dust or operational fees.  This should be
    used to generate a signed serialized transaction that another key will later subsidize.

    Return a JSON object on success
    Return a JSON object with 'error' set on error.
    """
    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    db = get_db_state()

    if consensus_hash is None:
        consensus_hash = db.get_current_consensus()

    if consensus_hash is None:
        return {"error": "Nameset snapshot not found."}

    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    if type(keepdata) != bool:
        if str(keepdata) == "True":
            keepdata = True
        else:
            keepdata = False

    try:
        resp = transfer_name(str(name), str(address), keepdata, str(consensus_hash), privatekey, blockchain_client_inst, \
            tx_only=tx_only, blockchain_broadcaster=broadcaster_client_inst, user_public_key=user_public_key, testset=blockstack_opts['testset'])
    except:
        return json_traceback()

    if subsidy_key is not None:
        # subsidize the transaction
        resp = make_subsidized_tx( resp['unsigned_tx'], transfer_fees, blockstack_opts['max_subsidy'], subsidy_key, blockchain_client_inst )

    log.debug('name transfer <name, address, keepdata>: <%s, %s, %s>' % (name, address, keepdata))

    return resp


def blockstack_name_renew( name, privatekey, register_addr=None, tx_only=False, subsidy_key=None, user_public_key=None, testset=False, consensus_hash=None ):
    """
    Renew a name

    @name: the name to renew
    @privatekey: the private key of the name owner
    @tx_only: if True, then return only the unsigned serialized transaction.  Do not broadcast it.
    @pay_fee: if False, then do not pay any associated dust or operational fees.  This should be
    used to generate a signed serialized transaction that another key will later subsidize.

    Return a JSON object on success
    Return a JSON object with 'error' set on error.
    """

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    # renew the name for the caller
    db = get_db_state()
    name_rec = db.get_name( name )
    if name_rec is None:
        return {"error": "Name is not registered"}

    # renew to the caller (should be the same as the sender)
    if register_addr is None:
        register_addr = name_rec['address']

    if str(register_addr) != str(pybitcoin.BitcoinPrivateKey( privatekey ).public_key().address()):
        return {"error": "Only the name's owner can send a renew request"}

    renewal_fee = get_name_cost( name )

    return blockstack_name_register( name, privatekey, register_addr, renewal_fee=renewal_fee, tx_only=tx_only, subsidy_key=subsidy_key, user_public_key=user_public_key, testset=testset )


def blockstack_name_revoke( name, privatekey, tx_only=False, subsidy_key=None, user_public_key=None, testset=False, consensus_hash=None ):
    """
    Revoke a name and all its data.

    @name: the name to renew
    @privatekey: the private key of the name owner
    @tx_only: if True, then return only the unsigned serialized transaction.  Do not broadcast it.
    @pay_fee: if False, then do not pay any associated dust or operational fees.  This should be
    used to generate a signed serialized transaction that another key will later subsidize.

    Return a JSON object on success
    Return a JSON object with 'error' set on error.
    """

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    try:
        resp = revoke_name(str(name), privatekey, blockchain_client_inst, \
            tx_only=tx_only, blockchain_broadcaster=broadcaster_client_inst, \
            user_public_key=user_public_key, testset=blockstack_opts['testset'])
    except:
        return json_traceback()

    if subsidy_key is not None:
        # subsidize the transaction
        resp = make_subsidized_tx( resp['unsigned_tx'], revoke_fees, blockstack_opts['max_subsidy'], subsidy_key, blockchain_client_inst )

    log.debug("name revoke <%s>" % name )

    return resp


def blockstack_name_import( name, recipient_address, update_hash, privatekey, tx_only=False, testset=False, consensus_hash=None ):
    """
    Import a name into a namespace.
    """

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    db = get_db_state()

    try:
        resp = name_import( str(name), str(recipient_address), str(update_hash), str(privatekey), blockchain_client_inst, \
            tx_only=tx_only, blockchain_broadcaster=broadcaster_client_inst, testset=blockstack_opts['testset'] )
    except:
        return json_traceback()

    log.debug("import <%s>" % name )

    return resp


def blockstack_namespace_preorder( namespace_id, register_addr, privatekey, tx_only=False, testset=False, consensus_hash=None ):
    """
    Define the properties of a namespace.
    Between the namespace definition and the "namespace begin" operation, only the
    user who created the namespace can create names in it.
    """

    if is_namespace_id_blacklisted( namespace_id ):
        return {"error": "Invalid namespace ID"}

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    db = get_db_state()

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    if consensus_hash is None:
        consensus_hash = db.get_current_consensus()

    if consensus_hash is None:
        return {"error": "Nameset snapshot not found."}

    namespace_fee = price_namespace( namespace_id )

    log.debug("Namespace '%s' will cost %s satoshis" % (namespace_id, namespace_fee))

    try:
        resp = namespace_preorder( str(namespace_id), str(register_addr), str(consensus_hash), str(privatekey), blockchain_client_inst, namespace_fee, \
            tx_only=tx_only, blockchain_broadcaster=broadcaster_client_inst, testset=blockstack_opts['testset'] )

    except:
        return json_traceback()

    log.debug("namespace_preorder <%s>" % (namespace_id))
    return resp


def blockstack_namespace_reveal( namespace_id, register_addr, lifetime, coeff, base, bucket_exponents, nonalpha_discount, no_vowel_discount, privatekey, tx_only=False, testset=False, consensus_hash=None ):
    """
    Reveal and define the properties of a namespace.
    Between the namespace definition and the "namespace begin" operation, only the
    user who created the namespace can create names in it.
    """
    
    if is_namespace_id_blacklisted( namespace_id ):
        return {"error": "Invalid namespace ID"}

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    try:
        resp = namespace_reveal( str(namespace_id), str(register_addr), int(lifetime), \
                                int(coeff), int(base), list(bucket_exponents), \
                                int(nonalpha_discount), int(no_vowel_discount), \
                                str(privatekey), blockchain_client_inst, \
                                blockchain_broadcaster=broadcaster_client_inst, testset=blockstack_opts['testset'], tx_only=tx_only )
    except:
        return json_traceback()

    log.debug("namespace_reveal <%s, %s, %s, %s, %s, %s, %s>" % (namespace_id, lifetime, coeff, base, bucket_exponents, nonalpha_discount, no_vowel_discount))
    return resp


def blockstack_namespace_ready( namespace_id, privatekey, tx_only=False, testset=False, consensus_hash=None ):
    """
    Declare that a namespace is open to accepting new names.
    """

    if is_namespace_id_blacklisted( namespace_id ):
        return {"error": "Invalid namespace ID"}

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    try:
        resp = namespace_ready( str(namespace_id), str(privatekey), blockchain_client_inst, \
            tx_only=tx_only, blockchain_broadcaster=broadcaster_client_inst, testset=blockstack_opts['testset'] )
    except:
        return json_traceback()

    log.debug("namespace_ready %s" % namespace_id )
    return resp


def blockstack_announce( message, privatekey, tx_only=False, subsidy_key=None, user_public_key=None, testset=False ):
    """
    Send an announcement via the blockchain.
    If we're sending the tx out, then also replicate the message text to storage providers, via the blockstack_client library
    """

    blockstack_opts = default_blockstack_opts( virtualchain.get_config_filename(), testset=testset )

    # are we doing our initial indexing?
    if is_indexing():
        return {"error": "Indexing blockchain"}

    blockchain_client_inst = get_utxo_provider_client()
    if blockchain_client_inst is None:
        return {"error": "Failed to connect to blockchain UTXO provider"}

    broadcaster_client_inst = get_tx_broadcaster()
    if broadcaster_client_inst is None:
        return {"error": "Failed to connect to blockchain transaction broadcaster"}

    message_hash = pybitcoin.hex_hash160( message )

    try:
        resp = send_announce( message_hash, privatekey, blockchain_client_inst, \
            tx_only=tx_only, blockchain_broadcaster=broadcaster_client_inst, \
            user_public_key=user_public_key, testset=blockstack_opts['testset'])

    except:
        return json_traceback()

    if subsidy_key is not None:
        # subsidize the transaction
        resp = make_subsidized_tx( resp['unsigned_tx'], announce_fees, blockstack_opts['max_subsidy'], subsidy_key, blockchain_client_inst )

    elif not tx_only:
        # propagate the data to back-end storage
        data_hash = put_announcement( message, resp['transaction_hash'] )
        if data_hash is None:
            resp = {
                'error': 'failed to storage message text',
                'transaction_hash': resp['transaction_hash']
            }

        else:
            resp['data_hash'] = data_hash

    log.debug("announce <%s>" % message_hash )

    return resp


class BlockstackdRPC(jsonrpc.JSONRPC, object):
    """
    Blockstackd not-quite-JSON-RPC server.

    We say "not quite" because the implementation serves data
    via Netstrings, not HTTP, and does not pay attention to
    the 'id' or 'version' fields in the JSONRPC spec.

    This endpoint does *not* talk to a storage provider, but only
    serves back information from the blockstack virtual chain.

    The client is responsible for resolving this information
    to data, via an ancillary storage provider.
    """

    def __init__(self, testset=False):
        self.testset = testset
        super(BlockstackdRPC, self).__init__()

    def jsonrpc_ping(self):
        reply = {}
        reply['status'] = "alive"
        return reply

    def jsonrpc_get_name_blockchain_record(self, name):
        """
        Lookup the blockchain-derived profile for a name.
        """
        db = get_db_state()

        try:
            name = str(name)
        except Exception as e:
            return {"error": str(e)}

        name_record = db.get_name(str(name))

        if name_record is None:
            if is_indexing():
                return {"error": "Indexing blockchain"}
            else:
                return {"error": "Not found."}

        else:
            return name_record


    def jsonrpc_get_name_blockchain_history( self, name, start_block, end_block ):
        """
        Get the sequence of name operations processed for a given name.
        """
        db = get_db_state()
        name_history = db.get_name_history( name, start_block, end_block )

        if name_history is None:
            if is_indexing():
                return {"error": "Indexing blockchain"}
            else:
                return {"error": "Not found."}

        else:
            return name_history


    def jsonrpc_get_records_at( self, block_id ):
        """
        Get the sequence of name and namespace records at the given block.
        Returns the list of name operations to be fed into virtualchain.
        Used by SNV clients.
        """
        db = get_db_state()

        prior_records = db.get_all_records_at( block_id )
        ret = []
        for rec in prior_records:
            restored_rec = rec_restore_snv_consensus_fields( rec, block_id )
            ret.append( restored_rec )

        return ret


    def jsonrpc_get_nameops_at( self, block_id ):
        """
        Old name for jsonrpc_get_records_at
        """
        return self.jsonrpc_get_records_at( block_id )


    def jsonrpc_get_records_hash_at( self, block_id ):
        """
        Get the hash over the sequence of names and namespaces altered at the given block.
        Used by SNV clients.
        """
        db = get_db_state()

        prior_recs = db.get_all_records_at( block_id, include_history=True )
        if prior_recs is None:
            prior_recs = []

        restored_recs = []
        for rec in prior_recs:
            restored_rec = rec_restore_snv_consensus_fields( rec, block_id )
            restored_recs.append( restored_rec )

        # NOTE: extracts only the operation-given fields, and ignores ancilliary record fields
        serialized_ops = [ virtualchain.StateEngine.serialize_op( str(op['op'][0]), op, BlockstackDB.make_opfields(), verbose=False ) for op in restored_recs ]

        for serialized_op in serialized_ops:
            log.debug("SERIALIZED (%s): %s" % (block_id, serialized_op))

        ops_hash = virtualchain.StateEngine.make_ops_snapshot( serialized_ops )
        log.debug("Serialized hash at (%s): %s" % (block_id, ops_hash))

        return ops_hash


    def jsonrpc_get_nameops_hash_at( self, block_id ):
        """
        Old name for jsonrpc_get_records_hash_at
        """
        return self.jsonrpc_get_records_hash_at( block_id )


    def jsonrpc_getinfo(self):
        """
        Get the number of blocks the
        """
        bitcoind_opts = default_bitcoind_opts( virtualchain.get_config_filename() )
        bitcoind = get_bitcoind( new_bitcoind_opts=bitcoind_opts, new=True )

        info = bitcoind.getinfo()
        reply = {}
        reply['bitcoind_blocks'] = info['blocks']

        db = get_db_state()
        reply['consensus'] = db.get_current_consensus()
        reply['last_block'] = db.get_current_block()
        reply['blockstack_version'] = "%s.%s" % (VERSION, BLOCKSTACK_VERSION)
        reply['testset'] = str(self.testset)
        return reply


    def jsonrpc_get_names_owned_by_address(self, address):
        """
        Get the list of names owned by an address.
        Valid only for names with p2pkh sender scripts.
        """
        db = get_db_state()
        names = db.get_names_owned_by_address( address )
        if names is None:
            names = []
        return names


    def jsonrpc_preorder( self, name, privatekey, register_addr ):
        """
        Preorder a name:
        @name is the name to preorder
        @register_addr is the address of the key pair that will own the name
        @privatekey is the private key that will send the preorder transaction
        (it must be *different* from the register_addr keypair)

        Returns a JSON object with the transaction ID on success.
        Returns a JSON object with 'error' on error.
        """
        return blockstack_name_preorder( str(name), str(privatekey), str(register_addr), testset=self.testset )


    def jsonrpc_preorder_tx( self, name, privatekey, register_addr ):
        """
        Generate a transaction that preorders a name:
        @name is the name to preorder
        @register_addr is the address of the key pair that will own the name
        @privatekey is the private key that will send the preorder transaction
        (it must be *different* from the register_addr keypair)

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_preorder( str(name), str(privatekey), str(register_addr), tx_only=True, testset=self.testset )


    def jsonrpc_preorder_tx_subsidized( self, name, register_addr, subsidy_key ):
        """
        Generate a transaction that preorders a name, but without paying fees.
        @name is the name to preorder
        @register_addr is the address of the key pair that will own the name
        @public_key is the client's public key that will sign the preorder transaction
        (it must be *different* from the register_addr keypair)

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_preorder( str(name), None, str(register_addr), tx_only=True, subsidy_key=str(subsidy_key), testset=self.testset )


    def jsonrpc_register( self, name, privatekey, register_addr ):
        """
        Register a name:
        @name is the name to register
        @register_addr is the address of the key pair that will own the name
        (given earlier in the preorder)
        @privatekey is the private key that sent the preorder transaction.

        Returns a JSON object with the transaction ID on success.
        Returns a JSON object with 'error' on error.
        """
        return blockstack_name_register( str(name), str(privatekey), str(register_addr), testset=self.testset )


    def jsonrpc_register_tx( self, name, privatekey, register_addr ):
        """
        Generate a transaction that will register a name:
        @name is the name to register
        @register_addr is the address of the key pair that will own the name
        (given earlier in the preorder)
        @privatekey is the private key that sent the preorder transaction.

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_register( str(name), str(privatekey), str(register_addr), tx_only=True, testset=self.testset )


    def jsonrpc_register_tx_subsidized( self, name, user_public_key, register_addr, subsidy_key ):
        """
        Generate a subsidizable transaction that will register a name
        @name is the name to register
        @register_addr is the address of the key pair that will own the name
        (given earlier in the preorder)
        public_key is the public key whose private counterpart sent the preorder transaction.

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_register( str(name), None, str(register_addr), tx_only=True, user_public_key=str(user_public_key), subsidy_key=str(subsidy_key), testset=self.testset )


    def jsonrpc_update( self, name, data_hash, privatekey ):
        """
        Update a name's record:
        @name is the name to update
        @data_hash is the hash of the new name record
        @privatekey is the private key that owns the name

        Returns a JSON object with the transaction ID on success.
        Returns a JSON object with 'error' on error.
        """
        return blockstack_name_update( str(name), str(data_hash), str(privatekey), testset=self.testset )


    def jsonrpc_update_tx( self, name, data_hash, privatekey ):
        """
        Generate a transaction that will update a name's name record hash.
        @name is the name to update
        @data_hash is the hash of the new name record
        @privatekey is the private key that owns the name

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_update( str(name), str(data_hash), str(privatekey), tx_only=True, testset=self.testset )


    def jsonrpc_update_tx_subsidized( self, name, data_hash, user_public_key, subsidy_key ):
        """
        Generate a subsidizable transaction that will update a name's name record hash.
        @name is the name to update
        @data_hash is the hash of the new name record
        @privatekey is the private key that owns the name

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_update( str(name), str(data_hash), None, user_public_key=str(user_public_key), subsidy_key=str(subsidy_key), tx_only=True, testset=self.testset )


    def jsonrpc_transfer( self, name, address, keepdata, privatekey ):
        """
        Transfer a name's record to a new address
        @name is the name to transfer
        @address is the new address that will own the name
        @keepdata determines whether or not the name record will
        remain associated with the name on transfer.
        @privatekey is the private key that owns the name

        Returns a JSON object with the transaction ID on success.
        Returns a JSON object with 'error' on error.
        """

        # coerce boolean
        if type(keepdata) != bool:
            if str(keepdata) == "True":
                keepdata = True
            else:
                keepdata = False

        return blockstack_name_transfer( str(name), str(address), keepdata, str(privatekey), testset=self.testset )


    def jsonrpc_transfer_tx( self, name, address, keepdata, privatekey ):
        """
        Generate a transaction that will transfer a name to a new address
        @name is the name to transfer
        @address is the new address that will own the name
        @keepdata determines whether or not the name record will
        remain associated with the name on transfer.
        @privatekey is the private key that owns the name

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """

        # coerce boolean
        if type(keepdata) != bool:
            if str(keepdata) == "True":
                keepdata = True
            else:
                keepdata = False

        return blockstack_name_transfer( str(name), str(address), keepdata, str(privatekey), tx_only=True, testset=self.testset )


    def jsonrpc_transfer_tx_subsidized( self, name, address, keepdata, user_public_key, subsidy_key ):
        """
        Generate a subsidizable transaction that will transfer a name to a new address
        @name is the name to transfer
        @address is the new address that will own the name
        @keepdata determines whether or not the name record will
        remain associated with the name on transfer.
        @privatekey is the private key that owns the name

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """

        # coerce boolean
        if type(keepdata) != bool:
            if str(keepdata) == "True":
                keepdata = True
            else:
                keepdata = False

        return blockstack_name_transfer( str(name), str(address), keepdata, None, user_public_key=str(user_public_key), subsidy_key=str(subsidy_key), tx_only=True, testset=self.testset )


    def jsonrpc_renew( self, name, privatekey ):
        """
        Renew a name:
        @name is the name to renew
        @privatekey is the private key that owns the name

        Returns a JSON object with the transaction ID on success.
        Returns a JSON object with 'error' on error.
        """
        return blockstack_name_renew( str(name), str(privatekey), testset=self.testset )


    def jsonrpc_renew_tx( self, name, privatekey ):
        """
        Generate a transaction that will register a name:
        @name is the name to renew
        @privatekey is the private key that owns the name

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_renew( str(name), str(privatekey), tx_only=True, testset=self.testset )


    def jsonrpc_renew_tx_subsidized( self, name, user_public_key, subsidy_key ):
        """
        Generate a subsidizable transaction that will register a name
        @name is the name to renew
        @privatekey is the private key that owns the name

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_renew( name, None, user_public_key=str(user_public_key), subsidy_key=str(subsidy_key), tx_only=True, testset=self.testset )


    def jsonrpc_revoke( self, name, privatekey ):
        """
        revoke a name:
        @name is the name to revoke
        @privatekey is the private key that owns the name

        Returns a JSON object with the transaction ID on success.
        Returns a JSON object with 'error' on error.
        """
        return blockstack_name_revoke( str(name), str(privatekey), testset=self.testset )


    def jsonrpc_revoke_tx( self, name, privatekey ):
        """
        Generate a transaction that will revoke a name:
        @name is the name to revoke
        @privatekey is the private key that owns the name

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_revoke( str(name), str(privatekey), tx_only=True, testset=self.testset )


    def jsonrpc_revoke_tx_subsidized( self, name, public_key, subsidy_key ):
        """
        Generate a subsidizable transaction that will revoke a name
        @name is the name to revoke
        @privatekey is the private key that owns the name

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_name_revoke( str(name), None, user_public_key=str(public_key), subsidy_key=str(subsidy_key), tx_only=True, testset=self.testset )


    def jsonrpc_name_import( self, name, recipient_address, update_hash, privatekey ):
        """
        Import a name into a namespace.
        """
        return blockstack_name_import( name, recipient_address, update_hash, privatekey, testset=self.testset )


    def jsonrpc_name_import_tx( self, name, recipient_address, update_hash, privatekey ):
        """
        Generate a tx that will import a name
        """
        return blockstack_name_import( name, recipient_address, update_hash, privatekey, tx_only=True, testset=self.testset )


    def jsonrpc_namespace_preorder( self, namespace_id, reveal_addr, privatekey ):
        """
        Define the properties of a namespace.
        Between the namespace definition and the "namespace begin" operation, only the
        user who created the namespace can create names in it.
        """
        return blockstack_namespace_preorder( namespace_id, reveal_addr, privatekey, testset=self.testset )


    def jsonrpc_namespace_preorder_tx( self, namespace_id, reveal_addr, privatekey ):
        """
        Create a signed transaction that will define the properties of a namespace.
        Between the namespace definition and the "namespace begin" operation, only the
        user who created the namespace can create names in it.
        """
        return blockstack_namespace_preorder( namespace_id, reveal_addr, privatekey, tx_only=True, testset=self.testset )


    def jsonrpc_namespace_reveal( self, namespace_id, reveal_addr, lifetime, coeff, base, bucket_exponents, nonalpha_discount, no_vowel_discount, privatekey ):
        """
        Reveal and define the properties of a namespace.
        Between the namespace definition and the "namespace begin" operation, only the
        user who created the namespace can create names in it.
        """
        return blockstack_namespace_reveal( namespace_id, reveal_addr, lifetime, coeff, base, bucket_exponents, nonalpha_discount, no_vowel_discount, privatekey, testset=self.testset )


    def jsonrpc_namespace_reveal_tx( self, namespace_id, reveal_addr, lifetime, coeff, base, bucket_exponents, nonalpha_discount, no_vowel_discount, privatekey ):
        """
        Generate a signed transaction that will reveal and define the properties of a namespace.
        Between the namespace definition and the "namespace begin" operation, only the
        user who created the namespace can create names in it.
        """
        return blockstack_namespace_reveal( namespace_id, reveal_addr, lifetime, coeff, base, bucket_exponents, nonalpha_discount, no_vowel_discount, \
                privatekey, tx_only=True, testset=self.testset )


    def jsonrpc_namespace_ready( self, namespace_id, privatekey ):
        """
        Declare that a namespace is open to accepting new names.
        """
        return blockstack_namespace_ready( namespace_id, privatekey, testset=self.testset )


    def jsonrpc_namespace_ready_tx( self, namespace_id, privatekey ):
        """
        Create a signed transaction that will declare that a namespace is open to accepting new names.
        """
        return blockstack_namespace_ready( namespace_id, privatekey, tx_only=True, testset=self.testset )


    def jsonrpc_announce( self, message, privatekey ):
        """
        announce a message to all blockstack nodes on the blockchain
        @message is the message to send
        @privatekey is the private key that will sign the announcement

        Returns a JSON object with the transaction ID on success.
        Returns a JSON object with 'error' on error.
        """
        return blockstack_announce( str(message), str(privatekey), testset=self.testset )


    def jsonrpc_announce_tx( self, message, privatekey ):
        """
        Generate a transaction that will make an announcement:
        @message is the message text to send
        @privatekey is the private key that signs the message

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_announce( str(message), str(privatekey), tx_only=True, testset=self.testset )


    def jsonrpc_announce_tx_subsidized( self, message, public_key, subsidy_key ):
        """
        Generate a subsidizable transaction that will make an announcement
        @message is hte message text to send
        @privatekey is the private key that signs the message

        Return a JSON object with the signed serialized transaction on success.  It will not be broadcast.
        Return a JSON object with 'error' on error.
        """
        return blockstack_announce( str(message), None, user_public_key=str(user_public_key), subsidy_key=str(subsidy_key), tx_only=True, testset=self.testset )


    def jsonrpc_get_name_cost( self, name ):
        """
        Return the cost of a given name, including fees
        Return value is in satoshis
        """

        # are we doing our initial indexing?

        if len(name) > LENGTHS['blockchain_id_name']:
            return {"error": "Name too long"}

        ret = get_name_cost( name )
        if ret is None:
            if is_indexing():
               return {"error": "Indexing blockchain"}

            else:
               return {"error": "Unknown/invalid namespace"}

        return {"satoshis": int(math.ceil(ret))}


    def jsonrpc_get_namespace_cost( self, namespace_id ):
        """
        Return the cost of a given namespace, including fees.
        Return value is in satoshis
        """

        if len(namespace_id) > LENGTHS['blockchain_id_namespace_id']:
            return {"error": "Namespace ID too long"}

        ret = price_namespace(namespace_id)
        return {"satoshis": int(math.ceil(ret))}


    def jsonrpc_get_namespace_blockchain_record( self, namespace_id ):
        """
        Return the readied namespace with the given namespace_id
        """

        db = get_db_state()
        ns = db.get_namespace( namespace_id )
        if ns is None:
            if is_indexing():
                return {"error": "Indexing blockchain"}
            else:
                return {"error": "No such ready namespace"}
        else:
            return ns
    

    def jsonrpc_get_namespace_reveal_blockchain_record( self, namespace_id ):
        """
        Return the revealed namespace with the given namespace_id
        """
        
        db = get_db_state()
        ns = db.get_namespace_reveal( namespace_id )
        if ns is None:
            if is_indexing():
                return {"error": "Indexing blockchain"}
            else:
                return {"error": "No such revealed namespace"}
        else:
            return ns
   
    
    def jsonrpc_get_all_names( self, offset, count ):
        """
        Return all names
        """
        # are we doing our initial indexing?
        if is_indexing():
            return {"error": "Indexing blockchain"}

        db = get_db_state()
        return db.get_all_names( offset=offset, count=count )


    def jsonrpc_get_names_in_namespace( self, namespace_id, offset, count ):
        """
        Return all names in a namespace
        """
        # are we doing our initial indexing?
        if is_indexing():
            return {"error": "Indexing blockchain"}

        db = get_db_state()
        return db.get_names_in_namespace( namespace_id, offset=offset, count=count )


    def jsonrpc_get_consensus_at( self, block_id ):
        """
        Return the consensus hash at a block number
        """
        db = get_db_state()
        return db.get_consensus_at( block_id )


    def jsonrpc_get_consensus_range( self, block_id_start, block_id_end ):
        """
        Get a range of consensus hashes.  The range is inclusive.
        """
        db = get_db_state()
        ret = []
        for b in xrange(block_id_start, block_id_end+1):
            ch = db.get_consensus_at( b )
            if ch is None:
                break

            ret.append(ch)

        return ret


    def jsonrpc_get_block_from_consensus( self, consensus_hash ):
        """
        Given the consensus hash, find the block number
        """
        db = get_db_state()
        return db.get_block_from_consensus( consensus_hash )


    def jsonrpc_get_mutable_data( self, blockchain_id, data_name ):
        """
        Get a mutable data record written by a given user.
        """
        client = get_blockstack_client_session()
        return client.get_mutable( str(blockchain_id), str(data_name) )


    def jsonrpc_get_immutable_data( self, blockchain_id, data_hash ):
        """
        Get immutable data record written by a given user.
        """
        client = get_blockstack_client_session()
        return client.get_immutable( str(blockchain_id), str(data_hash) )


def stop_server( clean=False, kill=False ):
    """
    Stop the blockstackd server.
    """

    # kill the main supervisor
    pid_file = get_pidfile_path()
    try:
        fin = open(pid_file, "r")
    except Exception, e:
        pass

    else:
        pid_data = fin.read().strip()
        fin.close()

        pid = int(pid_data)

        try:
           os.kill(pid, signal.SIGINT)
        except OSError, oe:
           if oe.errno == errno.ESRCH:
              # already dead 
              log.info("Process %s is not running" % pid)
              try:
                  os.unlink(pid_file)
              except:
                  pass

              return

        except Exception, e:
            log.exception(e)
            sys.exit(1)

        if kill:
            time.sleep(3.0)
            try:
                os.kill(pid, signal.SIGKILL)
            except Exception, e:
                pass
   
    if clean:
        # always blow away the pid file 
        try:
            os.unlink(pid_file)
        except:
            pass

    log.debug("Blockstack server stopped")


def get_indexing_lockfile():
    """
    Return path to the indexing lockfile
    """
    return os.path.join( virtualchain.get_working_dir(), "blockstack.indexing" )


def is_indexing():
    """
    Is the blockstack daemon synchronizing with the blockchain?
    """
    indexing_path = get_indexing_lockfile()
    if os.path.exists( indexing_path ):
        return True
    else:
        return False


def set_indexing( flag ):
    """
    Set a flag in the filesystem as to whether or not we're indexing.
    Return True if we succeed in carrying out the operation.
    Return False if not.
    """
    indexing_path = get_indexing_lockfile()
    if flag:
        try:
            fd = os.open( indexing_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_TRUNC )
            os.close( fd )
            return True
        except:
            return False

    else:
        try:
            os.unlink( indexing_path )
            return True
        except:
            return False


def get_index_range():
    """
    Get the bitcoin block index range.
    Mask connection failures with timeouts.
    Always try to reconnect.

    The last block will be the last block to search for names.
    This will be NUM_CONFIRMATIONS behind the actual last-block the
    cryptocurrency node knows about.
    """

    bitcoind_conn = get_bitcoind( new=True )

    first_block = None
    last_block = None
    delay = 1.0
    while last_block is None:

        first_block, last_block = virtualchain.get_index_range( bitcoind_conn )

        if last_block is None:

            # try to reconnnect
            log.error("Reconnect to bitcoind in %s seconds" % delay)
            time.sleep(delay)
            bitcoind_conn = get_bitcoind( new=True )

            delay = (delay * 2) + (random.random() * delay)
            if delay > 300:
                delay = 300
            continue

        else:
            return first_block, last_block - NUM_CONFIRMATIONS

    return (None, None)


def index_blockchain( expected_snapshots={} ):
    """
    Index the blockchain:
    * find the range of blocks
    * synchronize our state engine up to them
    """

    bt_opts = get_bitcoin_opts() 
    start_block, current_block = get_index_range()

    if start_block is None and current_block is None:
        log.error("Failed to find block range")
        return

    # bring us up to speed
    log.debug("Begin indexing (up to %s)" % current_block)
    set_indexing( True )
    virtualchain_hooks.sync_blockchain( bt_opts, current_block, expected_snapshots=expected_snapshots )
    set_indexing( False )
    log.debug("End indexing (up to %s)" % current_block)


def api_server_subprocess( foreground=False, testset=False ):
    """
    Start up the API server in a subprocess.
    Returns a Subprocess connected to the API server on success.
    Returns None on error
    """

    tac_file = get_tacfile_path( testset=testset )
    access_log_file = get_logfile_path() + ".access"
    api_server_command = None 

    if not foreground:
        api_server_command = ('twistd --pidfile= --logfile=%s -n -o -y' % (access_log_file)).split()
        api_server_command.append(tac_file)

    else:
        api_server_command = ('twistd --pidfile= -n -o -y').split()
        api_server_command.append(tac_file)

    api_server = subprocess.Popen( api_server_command, shell=False, close_fds=True, stdin=None, stdout=sys.stderr, stderr=sys.stderr)
    return api_server


def blockstack_exit():
    """
    Shut down the server on exit(3)
    """

    global blockstackd_api_server

    # stop API server
    if blockstackd_api_server is not None:
        blockstackd_api_server.kill()
        blockstackd_api_server.wait()

    pid_file = get_pidfile_path()
    try:
        fin = open(pid_file, "r")
        os.unlink(pid_file)
    except Exception, e:
        pass

    else:
        pid_data = fin.read().strip()
        fin.close()

        pid = int(pid_data)
        if pid != os.getpid(): 

            # kill the supervisor
            try:
               os.kill(pid, signal.SIGTERM)
            except Exception, e:
               pass


def run_server( testset=False, foreground=False, expected_snapshots={} ):
    """
    Run the blockstackd RPC server, optionally in the foreground.
    """

    global blockstackd_api_server

    bt_opts = get_bitcoin_opts()

    tac_file = get_tacfile_path( testset=testset )
    access_log_file = get_logfile_path() + ".access"
    indexer_log_file = get_logfile_path() + ".indexer"
    pid_file = get_pidfile_path()
    working_dir = virtualchain.get_working_dir()

    logfile = None
    if not foreground:
        try:
            if os.path.exists( indexer_log_file ):
                logfile = open( indexer_log_file, "a" )
            else:
                logfile = open( indexer_log_file, "a+" )
        except OSError, oe:
            log.error("Failed to open '%s': %s" % (indexer_log_file, oe.strerror))
            sys.exit(1)

        # become a daemon
        child_pid = os.fork()
        if child_pid == 0:

            # child! detach, setsid, and make a new child to be adopted by init
            sys.stdin.close()
            os.dup2( logfile.fileno(), sys.stdout.fileno() )
            os.dup2( logfile.fileno(), sys.stderr.fileno() )
            os.setsid()

            daemon_pid = os.fork()
            if daemon_pid == 0:

                # daemon!
                os.chdir("/")

            elif daemon_pid > 0:

                # parent (intermediate child)
                sys.exit(0)

            else:

                # error
                sys.exit(1)

        elif child_pid > 0:

            # grand-parent
            # wait for intermediate child
            pid, status = os.waitpid( child_pid, 0 )
            sys.exit(status)
    
    # put supervisor pid file
    put_pidfile( pid_file, os.getpid() )

    # start API server
    atexit.register( blockstack_exit )
    blockstackd_api_server = api_server_subprocess( foreground=True, testset=testset )

    # clear any stale indexing state
    set_indexing( False )

    log.debug("Begin Indexing")
    running = True

    while running:

	try:
           index_blockchain( expected_snapshots=expected_snapshots )
        except Exception, e:
           log.exception(e)
           log.error("FATAL: caught exception while indexing")
           sys.exit(1)
        
        # wait for the next block
        deadline = time.time() + REINDEX_FREQUENCY
        while time.time() < deadline:
            try:
                time.sleep(1.0)
            except:
                # interrupt
                running = False
                break

    # close logfile
    if logfile is not None:
        logfile.flush()
        logfile.close()

    return 0


def setup( working_dir=None, testset=False, return_parser=False ):
   """
   Do one-time initialization.

   If return_parser is True, return a partially-
   setup argument parser to be populated with
   subparsers (i.e. as part of main())

   Otherwise return None.
   """

   global blockstack_opts
   global blockchain_client
   global blockchain_broadcaster
   global bitcoin_opts
   global utxo_opts
   global dht_opts

   # set up our implementation
   if working_dir is not None:
       if not os.path.exists( working_dir ):
           os.makedirs( working_dir, 0700 )

       blockstack_state_engine.working_dir = working_dir

   virtualchain.setup_virtualchain( impl=blockstack_state_engine, testset=testset )

   testset_path = get_testset_filename( working_dir )
   if testset:
       # flag testset so our subprocesses see it
       if not os.path.exists( testset_path ):
           with open( testset_path, "w+" ) as f:
              pass

   else:
       # flag not set
       if os.path.exists( testset_path ):
           os.unlink( testset_path )

   # acquire configuration, and store it globally
   blockstack_opts, bitcoin_opts, utxo_opts, dht_opts = configure( interactive=True, testset=testset )

   # do we need to enable testset?
   if blockstack_opts['testset']:
       virtualchain.setup_virtualchain( impl=blockstack_state_engine, testset=True )
       testset = True

   # if we're using the mock UTXO provider, then switch to the mock bitcoind node as well
   if utxo_opts['utxo_provider'] == 'mock_utxo':
       import tests.mock_bitcoind
       mock_bitcoind_save_path = os.path.join( virtualchain.get_working_dir(), "mock_blockchain.dat" )
       worker_env = {
            # use mock_bitcoind to connect to bitcoind (but it has to import it in order to use it)
            "VIRTUALCHAIN_MOD_CONNECT_BLOCKCHAIN": tests.mock_bitcoind.__file__,
            "MOCK_BITCOIND_SAVE_PATH": mock_bitcoind_save_path,
            "BLOCKSTACK_TEST": "1"
       }

       if os.environ.get("PYTHONPATH", None) is not None:
           worker_env["PYTHONPATH"] = os.environ["PYTHONPATH"]

       virtualchain.setup_virtualchain( impl=blockstack_state_engine, testset=testset, bitcoind_connection_factory=tests.mock_bitcoind.connect_mock_bitcoind, index_worker_env=worker_env )

   # merge in command-line bitcoind options
   config_file = virtualchain.get_config_filename()

   arg_bitcoin_opts = None
   argparser = None

   if return_parser:
      arg_bitcoin_opts, argparser = virtualchain.parse_bitcoind_args( return_parser=return_parser )

   else:
      arg_bitcoin_opts = virtualchain.parse_bitcoind_args( return_parser=return_parser )

   # command-line overrides config file
   for (k, v) in arg_bitcoin_opts.items():
      bitcoin_opts[k] = v

   # store options
   set_bitcoin_opts( bitcoin_opts )
   set_utxo_opts( utxo_opts )

   if return_parser:
      return argparser
   else:
      return None


def reconfigure( testset=False ):
   """
   Reconfigure blockstackd.
   """
   configure( force=True, testset=testset )
   print "Blockstack successfully reconfigured."
   sys.exit(0)


def clean( testset=False, confirm=True ):
    """
    Remove blockstack's db, lastblock, and snapshot files.
    Prompt for confirmation
    """

    delete = False
    exit_status = 0

    if confirm:
        warning = "WARNING: THIS WILL DELETE YOUR BLOCKSTACK DATABASE!\n"
        warning+= "Database: '%s'\n" % blockstack_state_engine.working_dir
        warning+= "Are you sure you want to proceed?\n"
        warning+= "Type 'YES' if so: "
        value = raw_input( warning )

        if value != "YES":
            sys.exit(exit_status)

        else:
            delete = True

    else:
        delete = True


    if delete:
        print "Deleting..."

        db_filename = virtualchain.get_db_filename()
        lastblock_filename = virtualchain.get_lastblock_filename()
        snapshots_filename = virtualchain.get_snapshots_filename()

        for path in [db_filename, lastblock_filename, snapshots_filename]:
            try:
                os.unlink( path )
            except:
                log.warning("Unable to delete '%s'" % path)
                exit_status = 1

    sys.exit(exit_status)


def rec_to_virtualchain_op( name_rec, block_number, history_index, untrusted_db, testset=False ):
    """
    Given a record from the blockstack database,
    convert it into the virtualchain operation that
    was used to create/alter it at the given point
    in the past (i.e. (block_number, history_index)).
    
    @history_index is the index into the name_rec's 
    history that encodes the prior state of the 
    desired virtualchain operation.

    @untrusted_db is the database at 
    the state of the block_number.
    """

    # apply opcodes so we can consume them with virtualchain
    opcode_name = op_get_opcode_name( name_rec['op'] )
    assert opcode_name is not None, "Unrecognized opcode '%s'" % name_rec['op'] 

    ret_op = {}

    if name_rec.has_key('expired') and name_rec['expired']:
        # don't care--wasn't sent at this time
        return None

    ret_op = op_make_restore_diff( opcode_name, name_rec, block_number, history_index, untrusted_db, testset=testset ) 
    if ret_op is None:
        raise Exception("Failed to restore %s at (%s, %s)" % (opcode_name, block_number, history_index))

    # restore virtualchain fields
    ret_op = virtualchain.virtualchain_set_opfields( ret_op, \
                                                     virtualchain_opcode=getattr( config, opcode_name ), \
                                                     virtualchain_txid=str(name_rec['txid']), \
                                                     virtualchain_txindex=int(name_rec['vtxindex']) )

    ret_op['opcode'] = opcode_name

    # apply the operation.
    # don't worry about ancilliary fields from the name_rec--they'll be ignored.
    merged_ret_op = copy.deepcopy( name_rec )
    merged_ret_op.update( ret_op )
    return merged_ret_op


def rec_restore_snv_consensus_fields( name_rec, block_id ):
    """
    Given a name record at a given point in time, ensure
    that all of its consensus fields are present.
    Because they can be reconstructed directly from the record,
    but they are not always stored in the db, we have to do so here.
    """

    opcode_name = op_get_opcode_name( name_rec['op'] )
    assert opcode_name is not None, "Unrecognized opcode '%s'" % name_rec['op']

    ret_op = {}
    db = get_db_state()

    ret_op = op_snv_consensus_extra( opcode_name, name_rec, block_id, db )
    if ret_op is None:
        raise Exception("Failed to derive extra consensus fields for '%s'" % opcode_name)
    
    ret_op = virtualchain.virtualchain_set_opfields( ret_op, \
                                                     virtualchain_opcode=getattr( config, opcode_name ), \
                                                     virtualchain_txid=str(name_rec['txid']), \
                                                     virtualchain_txindex=int(name_rec['vtxindex']) )
    ret_op['opcode'] = opcode_name

    merged_op = copy.deepcopy( name_rec )
    merged_op.update( ret_op )

    return merged_op


def block_to_virtualchain_ops( block_id, db ):
    """
    convert a block's name ops to virtualchain ops.
    This is needed in order to recreate the virtualchain
    transactions that generated the block's name operations,
    such as for re-building the db or serving SNV clients.

    Returns the list of virtualchain ops.
    """

    # all records altered at this block, in tx order, as they were
    prior_recs = db.get_all_records_at( block_id )
    log.debug("Records at %s: %s" % (block_id, len(prior_recs)))
    virtualchain_ops = []

    # process records in order by vtxindex
    prior_recs = sorted( prior_recs, key=lambda op: op['vtxindex'] )

    # each name record has its own history, and their interleaving in tx order
    # is what makes up prior_recs.  However, when restoring a name record to
    # a previous state, we need to know the *relative* order of operations
    # that changed it during this block.  This is called the history index,
    # and it maps names to a dict, which maps the the virtual tx index (vtxindex)
    # to integer h such that prior_recs[name][vtxindex] is the hth update to the name
    # record.

    history_index = {}
    for i in xrange(0, len(prior_recs)):
        rec = prior_recs[i]

        if 'name' not in rec.keys():
            continue

        name = str(rec['name'])
        if name not in history_index.keys():
            history_index[name] = { i: 0 }

        else:
            history_index[name][i] = max( history_index[name].values() ) + 1


    for i in xrange(0, len(prior_recs)):

        # only trusted fields
        opcode_name = op_get_opcode_name( prior_recs[i]['op'] )
        assert opcode_name is not None, "Unrecognized opcode '%s'" % prior_recs[i]['op']

        consensus_fields = SERIALIZE_FIELDS.get( opcode_name, None )
        if consensus_fields is None:
            raise Exception("BUG: no consensus fields defined for '%s'" % opcode_name )

        # coerce string, not unicode
        for k in prior_recs[i].keys():
            if type(prior_recs[i][k]) == unicode:
                prior_recs[i][k] = str(prior_recs[i][k])

        # remove virtualchain-specific fields--they won't be trusted
        prior_recs[i] = db.sanitize_op( prior_recs[i] )

        for field in prior_recs[i].keys():

            # remove untrusted fields, except for indirect consensus fields
            if field not in consensus_fields and field not in NAMEREC_INDIRECT_CONSENSUS_FIELDS:
                log.debug("OP '%s': Removing untrusted field '%s'" % (opcode_name, field))
                del prior_recs[i][field]

        try:
            # recover virtualchain op from name record
            h = 0
            if 'name' in prior_recs[i]:
                if prior_recs[i]['name'] in history_index:
                    h = history_index[ prior_recs[i]['name'] ][i]

            log.debug("Recover %s" % op_get_opcode_name( prior_recs[i]['op'] ))
            virtualchain_op = rec_to_virtualchain_op( prior_recs[i], block_id, h, db )
        except:
            print json.dumps( prior_recs[i], indent=4, sort_keys=True )
            raise

        if virtualchain_op is not None:
            virtualchain_ops.append( virtualchain_op )

    return virtualchain_ops


def rebuild_database( target_block_id, untrusted_db_path, working_db_path=None, resume_dir=None, start_block=None, testset=False ):
    """
    Given a target block ID and a path to an (untrusted) db, reconstruct it in a temporary directory by
    replaying all the nameops it contains.

    Return the consensus hash calculated at the target block.
    """

    # reconfigure the virtualchain to use a temporary directory,
    # so we don't interfere with this instance's primary database
    working_dir = None
    if resume_dir is None:
        working_dir = tempfile.mkdtemp( prefix='blockstack-verify-database-' )
    else:
        working_dir = resume_dir

    blockstack_state_engine.working_dir = working_dir

    virtualchain.setup_virtualchain( impl=blockstack_state_engine, testset=testset )

    if resume_dir is None:
        # not resuming
        start_block = virtualchain.get_first_block_id()
    else:
        # resuming
        old_start_block = start_block
        start_block = get_lastblock()
        if start_block is None:
            start_block = old_start_block

    log.debug( "Rebuilding database from %s to %s" % (start_block, target_block_id) )

    # feed in operations, block by block, from the untrusted database
    untrusted_db = BlockstackDB( untrusted_db_path, DISPOSITION_RO )

    # working db, to build up the operations in the untrusted db block-by-block
    working_db = None
    if working_db_path is None:
        working_db_path = virtualchain.get_db_filename()

    working_db = BlockstackDB( working_db_path, DISPOSITION_RW )

    # map block ID to consensus hashes
    consensus_hashes = {}

    for block_id in xrange( start_block, target_block_id+1 ):

        untrusted_db.lastblock = block_id
        virtualchain_ops = block_to_virtualchain_ops( block_id, untrusted_db )

        # feed ops to virtualchain to reconstruct the db at this block
        consensus_hash = working_db.process_block( block_id, virtualchain_ops )
        log.debug("VERIFY CONSENSUS(%s): %s" % (block_id, consensus_hash))

        consensus_hashes[block_id] = consensus_hash

    # final consensus hash
    return consensus_hashes[ target_block_id ]


def verify_database( trusted_consensus_hash, consensus_block_id, untrusted_db_path, working_db_path=None, start_block=None, testset=False ):
    """
    Verify that a database is consistent with a
    known-good consensus hash.

    This algorithm works by creating a new database,
    parsing the untrusted database, and feeding the untrusted
    operations into the new database block-by-block.  If we
    derive the same consensus hash, then we can trust the
    database.
    """

    final_consensus_hash = rebuild_database( consensus_block_id, untrusted_db_path, working_db_path=working_db_path, start_block=start_block, testset=testset )

    # did we reach the consensus hash we expected?
    if final_consensus_hash == trusted_consensus_hash:
        return True

    else:
        log.error("Unverifiable database state stored in '%s'" % blockstack_state_engine.working_dir )
        return False


def restore( working_dir, block_number ):
    """
    Restore the database from a backup in the backups/ directory.
    If block_number is None, then use the latest backup.
    Raise an exception if no such backup exists
    """

    if block_number is None:
        all_blocks = BlockstackDB.get_backup_blocks( virtualchain_hooks )
        if len(all_blocks) == 0:
            log.error("No backups available")
            return False

        block_number = max(all_blocks)

    found = True
    backup_paths = BlockstackDB.get_backup_paths( block_number, virtualchain_hooks )
    for p in backup_paths:
        if not os.path.exists(p):
            log.error("Missing backup file: '%s'" % p)
            found = False

    if not found:
        return False 

    rc = BlockstackDB.backup_restore( block_number, virtualchain_hooks )
    if not rc:
        log.error("Failed to restore backup")

    return rc


def check_testset_enabled():
    """
    Check sys.argv to see if testset is enabled.
    Must be done before we initialize the virtual chain.
    """
    for arg in sys.argv:
        if arg == "--testset":
            return True

    return False


def check_alternate_working_dir():
    """
    Check sys.argv to see if there is an alternative
    working directory selected.  We need to know this
    before setting up the virtual chain.
    """

    path = None
    for i in xrange(0, len(sys.argv)):
        arg = sys.argv[i]
        if arg.startswith('--working-dir'):
            if '=' in arg:
                argparts = arg.split("=")
                arg = argparts[0]
                parts = argparts[1:]
                path = "=".join(parts)
            elif i + 1 < len(sys.argv):
                path = sys.argv[i+1]
            else:
                print >> sys.stderr, "--working-dir requires an argument"
                return None

    return path


def run_blockstackd():
   """
   run blockstackd
   """

   testset = check_testset_enabled()
   if testset:
       os.environ['BLOCKSTACK_TESTSET'] = "1"
   else:
       os.environ['BLOCKSTACK_TESTSET'] = "0"

   working_dir = check_alternate_working_dir()
   argparser = setup( testset=testset, working_dir=working_dir, return_parser=True )

   # get RPC server options
   subparsers = argparser.add_subparsers(
      dest='action', help='the action to be taken')

   parser = subparsers.add_parser(
      'start',
      help='start the blockstackd server')
   parser.add_argument(
      '--foreground', action='store_true',
      help='start the blockstackd server in foreground')
   parser.add_argument(
      '--testset', action='store_true',
      help='run with the set of name operations used for testing, instead of the main set')
   parser.add_argument(
      '--working-dir', action='store',
      help='use an alternative working directory')
   parser.add_argument(
      '--check-snapshots', action='store',
      help='verify that consensus hashes calculated match those in this given .snapshots file')

   parser = subparsers.add_parser(
      'stop',
      help='stop the blockstackd server')
   parser.add_argument(
      '--testset', action='store_true',
      help='required if the daemon is using the testing set of name operations')
   parser.add_argument(
      '--working-dir', action='store',
      help='use an alternative working directory')
   parser.add_argument(
      '--clean', action='store_true',
      help='clear out old runtime state from a failed blockstackd process')
   parser.add_argument(
      '--kill', action='store_true',
      help='send SIGKILL to the blockstackd process')

   parser = subparsers.add_parser(
      'reconfigure',
      help='reconfigure the blockstackd server')
   parser.add_argument(
      '--testset', action='store_true',
      help='required if the daemon is using the testing set of name operations')
   parser.add_argument(
      '--working-dir', action='store',
      help='use an alternative working directory')

   parser = subparsers.add_parser(
      'clean',
      help='remove all blockstack database information')
   parser.add_argument(
      '--force', action='store_true',
      help='Do not confirm the request to delete.')
   parser.add_argument(
      '--testset', action='store_true',
      help='required if the daemon is using the testing set of name operations')
   parser.add_argument(
      '--working-dir', action='store',
      help='use an alternative working directory')

   parser = subparsers.add_parser(
      'restore',
      help="Restore the database from a backup")
   parser.add_argument(
      'block_number', nargs='?',
      help="The block number to restore from (if not given, the last backup will be used)")

   parser = subparsers.add_parser(
      'rebuilddb',
      help='Reconstruct the current database from particular block number by replaying all prior name operations')
   parser.add_argument(
      'db_path',
      help='the path to the database')
   parser.add_argument(
      'start_block_id',
      help='the block ID from which to start rebuilding')
   parser.add_argument(
      'end_block_id',
      help='the block ID at which to stop rebuilding')
   parser.add_argument(
      '--resume-dir', nargs='?',
      help='the temporary directory to store the database state as it is being rebuilt.  Blockstackd will resume working from this directory if it is interrupted.')
   parser.add_argument(
      '--working-dir', action='store',
      help='use an alternative working directory')

   parser = subparsers.add_parser(
      'verifydb',
      help='verify an untrusted database against a known-good consensus hash')
   parser.add_argument(
      'block_id',
      help='the block ID of the known-good consensus hash')
   parser.add_argument(
      'consensus_hash',
      help='the known-good consensus hash')
   parser.add_argument(
      'db_path',
      help='the path to the database')
   parser.add_argument(
      '--working-dir', action='store',
      help='use an alternative working directory')

   parser = subparsers.add_parser(
      'importdb',
      help='import an existing trusted database')
   parser.add_argument(
      'db_path',
      help='the path to the database')
   parser.add_argument(
      '--working-dir', action='store',
      help='use an alternative working directory')

   parser = subparsers.add_parser(
      'version',
      help='Print version and exit')

   args, _ = argparser.parse_known_args()

   log.debug("bitcoin options: (%s, %s, %s)" % (bitcoin_opts['bitcoind_server'],
                                                bitcoin_opts['bitcoind_port'],
                                                bitcoin_opts['bitcoind_user']))

   if args.action == 'version':
      print "Blockstack version: %s.%s" % (VERSION, BLOCKSTACK_VERSION)
      print "Testset: %s" % testset
      sys.exit(0)

   if args.action == 'start':

      if os.path.exists( get_pidfile_path() ):
          log.error("Blockstackd appears to be running already.  If not, please run '%s stop --clean'" % (sys.argv[0]))
          sys.exit(1)

   
      # use snapshots?
      expected_snapshots = {"snapshots":{}}
      if args.check_snapshots is not None:
          snapshots_path = args.check_snapshots
          try:
              with open(snapshots_path, "r") as f:
                  snapshots_data = f.read()

              expected_snapshots = json.loads(snapshots_data)
              assert 'snapshots' in expected_snapshots.keys(), "Not a valid snapshots file"
          except Exception, e:
              log.exception(e)
              log.error("Failed to read expected snapshots from '%s'" % snapshots_path)
              sys.exit(1)

      log.info('Starting blockstack server (testset = %s, working dir = \'%s\', %s expected snapshots) ...' % (testset, working_dir, len(expected_snapshots['snapshots'])))

      if args.foreground:

         exit_status = run_server( foreground=True, testset=testset, expected_snapshots=expected_snapshots )
         log.info("Service endpoint exited with status code %s" % exit_status )

      else:
         run_server( testset=testset, expected_snapshots=expected_snapshots )

   elif args.action == 'stop':
      stop_server( clean=args.clean, kill=args.kill )

   elif args.action == 'reconfigure':
      reconfigure( testset=testset )

   elif args.action == 'restore':
      restore( working_dir, args.block_number )

   elif args.action == 'clean':
      clean( confirm=(not args.force), testset=args.testset )

   elif args.action == 'rebuilddb':

      resume_dir = None
      if hasattr(args, 'resume_dir') and args.resume_dir is not None:
          resume_dir = args.resume_dir

      final_consensus_hash = rebuild_database( int(args.end_block_id), args.db_path, start_block=int(args.start_block_id), resume_dir=resume_dir )
      print "Rebuilt database in '%s'" % blockstack_state_engine.working_dir
      print "The final consensus hash is '%s'" % final_consensus_hash

   elif args.action == 'repair':

      resume_dir = None
      if hasattr(args, 'resume_dir') and args.resume_dir is not None:
          resume_dir = args.resume_dir

      restart_block_id = int(args.restart_block_id)

      # roll the db back in time
      # TODO

   elif args.action == 'verifydb':
      rc = verify_database( args.consensus_hash, int(args.block_id), args.db_path )
      if rc:
          # success!
          print "Database is consistent with %s" % args.consensus_hash
          print "Verified files are in '%s'" % blockstack_state_engine.working_dir

      else:
          # failure!
          print "Database is NOT CONSISTENT"

   elif args.action == 'importdb':
      old_working_dir = blockstack_state_engine.working_dir
      blockstack_state_engine.working_dir = None
      virtualchain.setup_virtualchain( impl=blockstack_state_engine, testset=testset )

      db_path = virtualchain.get_db_filename()
      old_snapshots_path = os.path.join( old_working_dir, os.path.basename( virtualchain.get_snapshots_filename() ) )
      old_lastblock_path = os.path.join( old_working_dir, os.path.basename( virtualchain.get_lastblock_filename() ) )

      if os.path.exists( db_path ):
          print "Backing up existing database to %s.bak" % db_path
          shutil.move( db_path, db_path + ".bak" )

      print "Importing database from %s to %s" % (args.db_path, db_path)
      shutil.copy( args.db_path, db_path )

      print "Importing snapshots from %s to %s" % (old_snapshots_path, virtualchain.get_snapshots_filename() )
      shutil.copy( old_snapshots_path, virtualchain.get_snapshots_filename() )

      print "Importing lastblock from %s to %s" % (old_lastblock_path, virtualchain.get_lastblock_filename() )
      shutil.copy( old_lastblock_path, virtualchain.get_lastblock_filename() )

      # clean up
      shutil.rmtree( old_working_dir )
      if os.path.exists( old_working_dir ):
          os.rmdir( old_working_dir )

if __name__ == '__main__':

   run_blockstackd()
