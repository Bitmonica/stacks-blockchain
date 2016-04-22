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

import os
import sys
from ConfigParser import SafeConfigParser
import pybitcoin
import blockstack_utxo
from blockstack_utxo import *
from ..version import __version__

import virtualchain

if not globals().has_key('log'):
    log = virtualchain.session.log

try:
    import blockstack_client
except:
    blockstack_client = None

DEBUG = True
TESTNET = False
VERSION = __version__

# namespace version
BLOCKSTACK_VERSION = 1

""" constants
"""

AVERAGE_MINUTES_PER_BLOCK = 10
DAYS_PER_YEAR = 365.2424
HOURS_PER_DAY = 24
MINUTES_PER_HOUR = 60
SECONDS_PER_MINUTE = 60
MINUTES_PER_YEAR = DAYS_PER_YEAR*HOURS_PER_DAY*MINUTES_PER_HOUR
SECONDS_PER_YEAR = int(round(MINUTES_PER_YEAR*SECONDS_PER_MINUTE))
BLOCKS_PER_YEAR = int(round(MINUTES_PER_YEAR/AVERAGE_MINUTES_PER_BLOCK))
BLOCKS_PER_DAY = int(round(float(MINUTES_PER_HOUR * HOURS_PER_DAY)/AVERAGE_MINUTES_PER_BLOCK))
EXPIRATION_PERIOD = BLOCKS_PER_YEAR*1
NAME_PREORDER_EXPIRE = BLOCKS_PER_DAY
# EXPIRATION_PERIOD = 10
AVERAGE_BLOCKS_PER_HOUR = MINUTES_PER_HOUR/AVERAGE_MINUTES_PER_BLOCK

""" blockstack configs
"""
MAX_NAMES_PER_SENDER = 25                # a sender can own exactly one name

""" RPC server configs
"""
RPC_SERVER_PORT = 6264

""" Bitcoin configs
"""
DEFAULT_BITCOIND_SERVER = 'btcd.onename.com'
DEFAULT_BITCOIND_PORT = 8332
DEFAULT_BITCOIND_PORT_TESTNET = 18332
DEFAULT_BITCOIND_USERNAME = 'openname'
DEFAULT_BITCOIND_PASSWD = 'opennamesystem'

""" block indexing configs
"""
REINDEX_FREQUENCY = 300 # seconds

FIRST_BLOCK_MAINNET = 373601
FIRST_BLOCK_MAINNET_TESTSET = 380960
# FIRST_BLOCK_TESTNET = 343883
FIRST_BLOCK_TESTNET = 529008
FIRST_BLOCK_TESTNET_TESTSET = FIRST_BLOCK_TESTNET

GENESIS_SNAPSHOT = {
    str(FIRST_BLOCK_MAINNET-4): "17ac43c1d8549c3181b200f1bf97eb7d",
    str(FIRST_BLOCK_MAINNET-3): "17ac43c1d8549c3181b200f1bf97eb7d",
    str(FIRST_BLOCK_MAINNET-2): "17ac43c1d8549c3181b200f1bf97eb7d",
    str(FIRST_BLOCK_MAINNET-1): "17ac43c1d8549c3181b200f1bf97eb7d",
}

GENESIS_SNAPSHOT_TESTSET = {
    str(FIRST_BLOCK_MAINNET_TESTSET-2): "9e938749294b8019f9857cda93e7e73f",
    str(FIRST_BLOCK_MAINNET_TESTSET-1): "9e938749294b8019f9857cda93e7e73f",
}

""" magic bytes configs
"""

MAGIC_BYTES_TESTSET = 'eg'
MAGIC_BYTES_MAINSET = 'id'

""" name operation data configs
"""

# Opcodes
NAME_PREORDER = '?'
NAME_REGISTRATION = ':'
NAME_UPDATE = '+'
NAME_TRANSFER = '>'
NAME_RENEWAL = NAME_REGISTRATION
NAME_REVOKE = '~'
NAME_IMPORT = ';'

NAME_OPCODES = [
    NAME_PREORDER,
    NAME_REGISTRATION,
    NAME_UPDATE,
    NAME_TRANSFER,
    NAME_RENEWAL,
    NAME_REVOKE,
    NAME_IMPORT
]

NAME_SCHEME = MAGIC_BYTES_MAINSET + NAME_REGISTRATION

NAMESPACE_PREORDER = '*'
NAMESPACE_REVEAL = '&'
NAMESPACE_READY = '!'

NAMESPACE_OPCODES = [
    NAMESPACE_PREORDER,
    NAMESPACE_REVEAL,
    NAMESPACE_READY
]

ANNOUNCE = '#'

# extra bytes affecting a transfer
TRANSFER_KEEP_DATA = '>'
TRANSFER_REMOVE_DATA = '~'

# list of opcodes we support
# ORDER MATTERS--it determines processing order, and determines collision priority
# (i.e. earlier operations in this list are preferred over later operations)
OPCODES = [
   NAME_PREORDER,
   NAME_REVOKE,
   NAME_REGISTRATION,
   NAME_UPDATE,
   NAME_TRANSFER,
   NAME_IMPORT,
   NAMESPACE_PREORDER,
   NAMESPACE_REVEAL,
   NAMESPACE_READY,
   ANNOUNCE
]

OPCODE_NAMES = {
    NAME_PREORDER: "NAME_PREORDER",
    NAME_REGISTRATION: "NAME_REGISTRATION",
    NAME_UPDATE: "NAME_UPDATE",
    NAME_TRANSFER: "NAME_TRANSFER",
    NAME_RENEWAL: "NAME_REGISTRATION",
    NAME_REVOKE: "NAME_REVOKE",
    NAME_IMPORT: "NAME_IMPORT",
    NAMESPACE_PREORDER: "NAMESPACE_PREORDER",
    NAMESPACE_REVEAL: "NAMESPACE_REVEAL",
    NAMESPACE_READY: "NAMESPACE_READY",
    ANNOUNCE: "ANNOUNCE"
}

NAME_OPCODES = {
    "NAME_PREORDER": NAME_PREORDER,
    "NAME_REGISTRATION": NAME_REGISTRATION,
    "NAME_UPDATE": NAME_UPDATE,
    "NAME_TRANSFER": NAME_TRANSFER,
    "NAME_RENEWAL": NAME_REGISTRATION,
    "NAME_IMPORT": NAME_IMPORT,
    "NAME_REVOKE": NAME_REVOKE,
    "NAMESPACE_PREORDER": NAMESPACE_PREORDER,
    "NAMESPACE_REVEAL": NAMESPACE_REVEAL,
    "NAMESPACE_READY": NAMESPACE_READY,
    "ANNOUNCE": ANNOUNCE
}

NAMESPACE_LIFE_INFINITE = 0xffffffff

# op-return formats
LENGTHS = {
    'magic_bytes': 2,
    'opcode': 1,
    'preorder_name_hash': 20,
    'consensus_hash': 16,
    'namelen': 1,
    'name_min': 1,
    'name_max': 34,
    'name_hash': 16,
    'update_hash': 20,
    'data_hash': 20,
    'blockchain_id_name': 37,
    'blockchain_id_namespace_life': 4,
    'blockchain_id_namespace_coeff': 1,
    'blockchain_id_namespace_base': 1,
    'blockchain_id_namespace_buckets': 8,
    'blockchain_id_namespace_discounts': 1,
    'blockchain_id_namespace_version': 2,
    'blockchain_id_namespace_id': 19,
    'announce': 20,
    'max_op_length': 40
}

MIN_OP_LENGTHS = {
    'preorder': LENGTHS['preorder_name_hash'] + LENGTHS['consensus_hash'],
    'registration': LENGTHS['name_min'],
    'update': LENGTHS['name_hash'] + LENGTHS['update_hash'],
    'transfer': LENGTHS['name_hash'] + LENGTHS['consensus_hash'],
    'revoke': LENGTHS['name_min'],
    'name_import': LENGTHS['name_min'],
    'namespace_preorder': LENGTHS['preorder_name_hash'] + LENGTHS['consensus_hash'],
    'namespace_reveal': LENGTHS['blockchain_id_namespace_life'] + LENGTHS['blockchain_id_namespace_coeff'] + \
                        LENGTHS['blockchain_id_namespace_base'] + LENGTHS['blockchain_id_namespace_buckets'] + \
                        LENGTHS['blockchain_id_namespace_discounts'] + LENGTHS['blockchain_id_namespace_version'] + \
                        LENGTHS['name_min'],
    'namespace_ready': 1 + LENGTHS['name_min'],
    'announce': LENGTHS['announce']
}

OP_RETURN_MAX_SIZE = 40

""" transaction fee configs
"""

DEFAULT_OP_RETURN_FEE = 10000
DEFAULT_DUST_FEE = 5500
DEFAULT_OP_RETURN_VALUE = 0
DEFAULT_FEE_PER_KB = 10000

""" name price configs
"""

SATOSHIS_PER_BTC = 10**8
PRICE_FOR_1LETTER_NAMES = 10*SATOSHIS_PER_BTC
PRICE_DROP_PER_LETTER = 10
PRICE_DROP_FOR_NON_ALPHABETIC = 10
ALPHABETIC_PRICE_FLOOR = 10**4

NAME_COST_UNIT = 100    # 100 satoshis

NAMESPACE_1_CHAR_COST = 400 * SATOSHIS_PER_BTC        # ~$96,000
NAMESPACE_23_CHAR_COST = 40 * SATOSHIS_PER_BTC        # ~$9,600
NAMESPACE_4567_CHAR_COST = 4 * SATOSHIS_PER_BTC       # ~$960
NAMESPACE_8UP_CHAR_COST = 0.4 * SATOSHIS_PER_BTC      # ~$96

TESTSET_NAMESPACE_1_CHAR_COST = 10000
TESTSET_NAMESPACE_23_CHAR_COST = 10000
TESTSET_NAMESPACE_4567_CHAR_COST = 10000
TESTSET_NAMESPACE_8UP_CHAR_COST = 10000

NAMESPACE_PREORDER_EXPIRE = BLOCKS_PER_DAY      # namespace preorders expire after 1 day, if not revealed
NAMESPACE_REVEAL_EXPIRE = BLOCKS_PER_YEAR       # namespace reveals expire after 1 year, if not readied.

NAME_IMPORT_KEYRING_SIZE = 300                  # number of keys to derive from the import key

NUM_CONFIRMATIONS = 6                         # number of blocks to wait for before accepting names

# burn address for fees (the address of public key 0x0000000000000000000000000000000000000000)
BLOCKSTORE_BURN_PUBKEY_HASH = "0000000000000000000000000000000000000000"
BLOCKSTORE_BURN_ADDRESS = "1111111111111111111114oLvT2"

# default namespace record (i.e. for names with no namespace ID)
NAMESPACE_DEFAULT = {
   'opcode': 'NAMESPACE_REVEAL',
   'lifetime': EXPIRATION_PERIOD,
   'coeff': 15,
   'base': 15,
   'buckets': [15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15],
   'version': BLOCKSTACK_VERSION,
   'nonalpha_discount': 1.0,
   'no_vowel_discount': 1.0,
   'namespace_id': None,
   'namespace_id_hash': None,
   'sender': "",
   'recipient': "",
   'address': "",
   'recipient_address': "",
   'sender_pubkey': None,
   'history': {},
   'block_number': 0
}


""" UTXOs
"""

SUPPORTED_UTXO_PROMPT_MESSAGES = {
    "chain_com": "Please enter your chain.com API key and secret.",
    "blockcypher": "Please enter your Blockcypher API token.",
    "blockchain_info": "Please enter your blockchain.info API token.",
    "bitcoind_utxo": "Please enter your fully-indexed bitcoind node information.",
    "blockstack_utxo": "Please enter your Blockstack server info.",
    "mock_utxo": "Mock UTXO provider.  Do not use in production."
}


"""
Which announcements has this blockstack node seen so far?
Announcements encode CVEs, bugs, and new features.  This list will be
updated in Blockstack releases to describe which of them have been
incorporated into the codebase.
"""
ANNOUNCEMENTS = []


blockstack_client_session = None
blockstack_client_session_opts = None

def get_testset_filename( working_dir=None ):
   """
   Get the path to the file to determine whether or not we're in testset.
   """

   if working_dir is None:
       working_dir = virtualchain.get_working_dir()

   testset_filepath = os.path.join( working_dir, virtualchain.get_implementation().get_virtual_chain_name() ) + ".testset"
   return testset_filepath


def get_announce_filename( working_dir=None ):
   """
   Get the path to the file that stores all of the announcements.
   """

   if working_dir is None:
       working_dir = virtualchain.get_working_dir()

   announce_filepath = os.path.join( working_dir, virtualchain.get_implementation().get_virtual_chain_name() ) + ".announce"
   return announce_filepath


def get_blockstack_client_session( new_blockstack_client_session_opts=None ):
    """
    Get or instantiate our storage API session.
    """
    global blockstack_client_session
    global blockstack_client_session_opts

    # do we have storage?
    if blockstack_client is None:
        return None

    opts = None
    if new_blockstack_client_session_opts is not None:
        opts = new_blockstack_client_session_opts
    else:
        opts = blockstack_client.config.get_config()

    if opts is None:
        return None

    blockstack_client_session = blockstack_client.session( conf=opts )
    if blockstack_client_session is not None:

        if new_blockstack_client_session_opts is not None:
            blockstack_client_session_opts = new_blockstack_client_session_opts

    return blockstack_client_session


def store_announcement( announcement_hash, announcement_text, working_dir=None, force=False ):
   """
   Store a new announcement locally, atomically.
   """

   if working_dir is None:
       working_dir = virtualchain.get_working_dir()

   if not force:
       # don't store unless we haven't seen it before
       if announcement_hash in ANNOUNCEMENTS:
           return

   announce_filename = get_announce_filename( working_dir )
   announce_filename_tmp = announce_filename + ".tmp"
   announce_text = ""
   announce_cleanup_list = []

   # did we try (and fail) to store a previous announcement?  If so, merge them all
   if os.path.exists( announce_filename_tmp ):

       log.debug("Merge announcement list %s" % announce_filename_tmp )

       with open(announce_filename, "r") as f:
           announce_text += f.read()

       i = 1
       failed_path = announce_filename_tmp + (".%s" % i)
       while os.path.exists( failed_path ):

           log.debug("Merge announcement list %s" % failed_paht )
           with open(failed_path, "r") as f:
               announce_text += f.read()

           announce_cleanup_list.append( failed_path )

           i += 1
           failed_path = announce_filename_tmp + (".%s" % i)

       announce_filename_tmp = failed_path

   if os.path.exists( announce_filename ):
       with open(announce_filename, "r" ) as f:
           announce_text += f.read()

   announce_text += ("\n%s\n" % announcement_hash)

   # filter
   if not force:
       announcement_list = announce_text.split("\n")
       unseen_announcements = filter( lambda a: a not in ANNOUNCEMENTS, announcement_list )
       announce_text = "\n".join( unseen_announcements ).strip() + "\n"

   log.debug("Store announcement hash to %s" % announce_filename )

   with open(announce_filename_tmp, "w" ) as f:
       f.write( announce_text )
       f.flush()

   # NOTE: rename doesn't remove the old file on Windows
   if sys.platform == 'win32' and os.path.exists( announcement_filename_tmp ):
       try:
           os.unlink( announcement_filename_tmp )
       except:
           pass

   try:
       os.rename( announce_filename_tmp, announce_filename )
   except:
       log.error("Failed to save announcement %s to %s" % (announcement_hash, announce_filename ))
       raise

   # clean up
   for tmp_path in announce_cleanup_list:
       try:
           os.unlink( tmp_path )
       except:
           pass

   # put the announcement text
   announcement_text_dir = os.path.join( working_dir, "announcements" )
   if not os.path.exists( announcement_text_dir ):
       try:
           os.makedirs( announcement_text_dir )
       except:
           log.error("Failed to make directory %s" % announcement_text_dir )
           raise

   announcement_text_path = os.path.join( announcement_text_dir, "%s.txt" % announcement_hash )

   try:
       with open( announcement_text_path, "w" ) as f:
           f.write( announcement_text )

   except:
       log.error("Failed to save announcement text to %s" % announcement_text_path )
       raise

   log.debug("Stored announcement to %s" % (announcement_text_path))


def get_announcement( announcement_hash ):
    """
    Go get an announcement's text, given its hash.
    Use the blockstack client library, so we can get at
    the storage drivers for the storage systems the sender used
    to host it.

    Return the data on success
    """

    session = get_blockstack_client_session()   # has the side-effect of initializing all storage drivers, if they're not already.
    data = blockstack_client.storage.get_immutable_data( announcement_hash )
    if data is None:
        log.error("Failed to get announcement '%s'" % (announcement_hash))
        return None

    return data


def put_announcement( announcement_text, txid ):
    """
    Go put an announcement into back-end storage.
    Use the blockstack client library, so we can get at
    the storage drivers for the storage systems this host
    is configured to use.

    Return the data's hash
    """

    session = get_blockstack_client_session()   # has the side-effect of initializing all storage drivers, if they're not already
    data_hash = blockstack_client.storage.put_immutable_data( announcement_text, txid )
    if data_hash is None:
        log.error("Failed to put announcement '%s'" % (pybitcoin.hex_hash160(announcement_text)))
        return None

    return data_hash


def default_blockstack_opts( config_file=None, testset=False ):
   """
   Get our default blockstack opts from a config file
   or from sane defaults.
   """

   if config_file is None:
      config_file = virtualchain.get_config_filename()

   testset_path = get_testset_filename( virtualchain.get_working_dir() )
   announce_path = get_announce_filename( virtualchain.get_working_dir() )

   parser = SafeConfigParser()
   parser.read( config_file )

   blockstack_opts = {}
   tx_broadcaster = None
   utxo_provider = None
   testset_first_block = None
   max_subsidy = 0
   contact_email = None
   announcers = "judecn.id,muneeb.id,shea256.id"
   announcements = None
   rpc_port = RPC_SERVER_PORT 
   blockchain_proxy = False

   if parser.has_section('blockstack'):

      if parser.has_option('blockstack', 'tx_broadcaster'):
         tx_broadcaster = parser.get('blockstack', 'tx_broadcaster')

      if parser.has_option('blockstack', 'utxo_provider'):
         utxo_provider = parser.get('blockstack', 'utxo_provider')

      if parser.has_option('blockstack', 'testset_first_block'):
         testset_first_block = int( parser.get('blockstack', 'testset_first_block') )

      if parser.has_option('blockstack', 'max_subsidy'):
         max_subsidy = int( parser.get('blockstack', 'max_subsidy'))

      if parser.has_option('blockstack', 'email'):
         contact_email = parser.get('blockstack', 'email')

      if parser.has_option('blockstack', 'rpc_port'):
         rpc_port = int(parser.get('blockstack', 'rpc_port'))

      if parser.has_option('blockstack', 'blockchain_proxy'):
         blockchain_proxy = parser.get('blockstack', 'blockchain_proxy')
         if blockchain_proxy.lower() in ['1', 'yes', 'true']:
             blockchain_proxy = True
         else:
             blockchain_proxy = False

      if parser.has_option('blockstack', 'announcers'):
         # must be a CSV of blockchain IDs
         announcer_list_str = parser.get('blockstack', 'announcers')
         announcer_list = announcer_list_str.split(",")

         import scripts

         # validate each one
         valid = True
         for bid in announcer_list:
             if not scripts.is_name_valid( bid ):
                 log.error("Invalid blockchain ID '%s'" % bid)
                 valid = False

         if valid:
             announcers = ",".join(announcer_list)

   if os.path.exists( testset_path ):
       # testset file flag set
       testset = True

   if os.path.exists( announce_path ):
       # load announcement list
       with open( announce_path, "r" ) as f:
           announce_text = f.readlines()

       all_announcements = [ a.strip() for a in announce_text ]
       unseen_announcements = []

       # find announcements we haven't seen yet
       for a in all_announcements:
           if a not in ANNOUNCEMENTS:
               unseen_announcements.append( a )

       announcements = ",".join( unseen_announcements )

   blockstack_opts = {
       'rpc_port': rpc_port,
       'tx_broadcaster': tx_broadcaster,
       'utxo_provider': utxo_provider,
       'testset': testset,
       'testset_first_block': testset_first_block,
       'max_subsidy': max_subsidy,
       'email': contact_email,
       'announcers': announcers,
       'announcements': announcements,
       'blockchain_proxy': blockchain_proxy
   }

   # strip Nones
   for (k, v) in blockstack_opts.items():
      if v is None:
         del blockstack_opts[k]

   return blockstack_opts


def default_bitcoind_opts( config_file=None ):
   """
   Get our default bitcoind options, such as from a config file,
   or from sane defaults
   """

   default_bitcoin_opts = virtualchain.get_bitcoind_config( config_file=config_file )
   
   # strip None's
   for (k, v) in default_bitcoin_opts.items():
      if v is None:
         del default_bitcoin_opts[k]

   return default_bitcoin_opts


def opt_strip( prefix, opts ):
   """
   Given a dict of opts that start with prefix,
   remove the prefix from each of them.
   """

   ret = {}
   for (opt_name, opt_value) in opts.items():

      # remove prefix
      if opt_name.startswith(prefix):
         opt_name = opt_name[len(prefix):]

      ret[ opt_name ] = opt_value

   return ret


def opt_restore( prefix, opts ):
   """
   Given a dict of opts, add the given prefix to each key
   """

   ret = {}

   for (opt_name, opt_value) in opts.items():

      ret[ prefix + opt_name ] = opt_value

   return ret


def interactive_prompt( message, parameters, default_opts, strip_prefix="" ):
   """
   Prompt the user for a series of parameters
   Return a dict mapping the parameter name to the
   user-given value.
   """

   # pretty-print the message
   lines = message.split("\n")
   max_line_len = max( [len(l) for l in lines] )

   print '-' * max_line_len
   print message
   print '-' * max_line_len

   ret = {}

   for param in parameters:

      formatted_param = param
      if param.startswith( strip_prefix ):
          formatted_param = param[len(strip_prefix):]

      prompt_str = "%s: "  % formatted_param
      if param in default_opts.keys():
          prompt_str = "%s (default: '%s'): " % (formatted_param, default_opts[param])

      value = raw_input(prompt_str)

      if len(value) > 0:
         ret[param] = value
      elif param in default_opts.keys():
         ret[param] = default_opts[param]
      else:
         ret[param] = None


   return ret


def find_missing( message, all_params, given_opts, default_opts, prompt_missing=True, strip_prefix="" ):
   """
   Find and interactively prompt the user for missing parameters,
   given the list of all valid parameters and a dict of known options.

   Return the (updated dict of known options, missing, num_prompted), with the user's input.
   """

   # are we missing anything?
   missing_params = []
   for missing_param in all_params:
      if missing_param not in given_opts.keys():
         missing_params.append( missing_param )

   num_prompted = 0
   if len(missing_params) > 0:

      if prompt_missing:
         missing_values = interactive_prompt( message, missing_params, default_opts, strip_prefix=strip_prefix )
         given_opts.update( missing_values )
         num_prompted = len(missing_values)

      else:
         # count the number missing, and go with defaults
         for default_key in default_opts.keys():
            if default_key not in given_opts:
                num_prompted += 1

         given_opts.update( default_opts )


   return given_opts, missing_params, num_prompted


def configure( config_file=None, force=False, interactive=True, testset=False ):
   """
   Configure blockstack:  find and store configuration parameters to the config file.

   Optionally prompt for missing data interactively (with interactive=True).  Or, raise an exception
   if there are any fields missing.

   Optionally force a re-prompting for all configuration details (with force=True)

   Return (bitcoind_opts, utxo_opts)
   """

   global SUPPORTED_UTXO_PROVIDERS, SUPPORTED_UTXO_PARAMS, SUPPORTED_UTXO_PROMPT_MESSAGES

   if config_file is None:
      try:
         # get input for everything
         config_file = virtualchain.get_config_filename()
      except:
         raise

   if not os.path.exists( config_file ):
       # definitely ask for everything
       force = True

   # get blockstack opts
   blockstack_opts = {}
   blockstack_opts_defaults = default_blockstack_opts( config_file=config_file, testset=testset )
   blockstack_params = blockstack_opts_defaults.keys()

   if not force:

       # default blockstack options
       blockstack_opts = default_blockstack_opts( config_file=config_file, testset=testset )

   blockstack_msg = "ADVANCED USERS ONLY.\nPlease enter blockstack configuration hints."

   # NOTE: disabled
   blockstack_opts, missing_blockstack_opts, num_blockstack_opts_prompted = find_missing( blockstack_msg, blockstack_params, blockstack_opts, blockstack_opts_defaults, prompt_missing=False )

   utxo_provider = None
   if 'utxo_provider' in blockstack_opts:
       utxo_provider = blockstack_opts['utxo_provider']
   else:
       utxo_provider = default_utxo_provider( config_file=config_file )

   bitcoind_message  = "Blockstack does not have enough information to connect\n"
   bitcoind_message += "to bitcoind.  Please supply the following parameters, or\n"
   bitcoind_message += "press [ENTER] to select the default value."

   bitcoind_opts = {}
   bitcoind_opts_defaults = default_bitcoind_opts( config_file=config_file )
   bitcoind_params = bitcoind_opts_defaults.keys()

   if not force:

      # get default set of bitcoind opts
      bitcoind_opts = default_bitcoind_opts( config_file=config_file )


   # get any missing bitcoind fields
   bitcoind_opts, missing_bitcoin_opts, num_bitcoind_prompted = find_missing( bitcoind_message, bitcoind_params, bitcoind_opts, bitcoind_opts_defaults, prompt_missing=interactive, strip_prefix="bitcoind_" )

   # find the current utxo provider
   while utxo_provider is None or utxo_provider not in SUPPORTED_UTXO_PROVIDERS:

       # prompt for it?
       if interactive or force:

           utxo_message  = 'NOTE: Blockstack currently requires an external API\n'
           utxo_message += 'for querying unspent transaction outputs.  The set of\n'
           utxo_message += 'supported providers are:\n'
           utxo_message += "\t\n".join( SUPPORTED_UTXO_PROVIDERS ) + "\n"
           utxo_message += "Please get the requisite API tokens and enter them here."

           utxo_provider_dict = interactive_prompt( utxo_message, ['utxo_provider'], {} )
           utxo_provider = utxo_provider_dict['utxo_provider']

       else:
           raise Exception("No UTXO provider given")

   utxo_opts = {}
   utxo_opts_defaults = default_utxo_provider_opts( utxo_provider, config_file=config_file )
   utxo_params = SUPPORTED_UTXO_PARAMS[ utxo_provider ]

   if not force:

      # get current set of utxo opts
      utxo_opts = default_utxo_provider_opts( utxo_provider, config_file=config_file )

   utxo_opts, missing_utxo_opts, num_utxo_opts_prompted = find_missing( SUPPORTED_UTXO_PROMPT_MESSAGES[utxo_provider], utxo_params, utxo_opts, utxo_opts_defaults, prompt_missing=interactive )
   utxo_opts['utxo_provider'] = utxo_provider

   if not interactive and (len(missing_bitcoin_opts) > 0 or len(missing_utxo_opts) > 0 or len(missing_blockstack_opts) > 0):

       # cannot continue
       raise Exception("Missing configuration fields: %s" % (",".join( missing_bitcoin_opts + missing_utxo_opts )) )

   # ask for contact info, so we can send out notifications for bugfixes and upgrades
   if blockstack_opts.get('email', None) is None:
       email_msg = "Would you like to receive notifications\n"
       email_msg+= "from the developers when there are critical\n"
       email_msg+= "updates available to install?\n\n"
       email_msg+= "If so, please enter your email address here.\n"
       email_msg+= "If not, leave this field blank.\n\n"
       email_msg+= "Your email address will be used solely\n"
       email_msg+= "for this purpose.\n"
       email_opts, _, email_prompted = find_missing( email_msg, ['email'], {}, {'email': ''}, prompt_missing=interactive )

       # merge with blockstack section
       num_blockstack_opts_prompted += 1
       blockstack_opts['email'] = email_opts['email']

   # if we prompted, then save
   if num_bitcoind_prompted > 0 or num_utxo_opts_prompted > 0 or num_blockstack_opts_prompted > 0:
       print >> sys.stderr, "Saving configuration to %s" % config_file
       write_config_file( bitcoind_opts=bitcoind_opts, utxo_opts=utxo_opts, blockstack_opts=blockstack_opts, config_file=config_file )

   return (blockstack_opts, bitcoind_opts, utxo_opts)


def write_config_file( blockstack_opts=None, bitcoind_opts=None, utxo_opts=None, config_file=None ):
   """
   Update a configuration file, given the bitcoind options and chain.com options.
   Return True on success
   Return False on failure
   """

   if config_file is None:
      try:
         config_file = virtualchain.get_config_filename()
      except:
         return False

   if config_file is None:
      return False

   parser = SafeConfigParser()
   parser.read(config_file)

   if bitcoind_opts is not None and len(bitcoind_opts) > 0:

      tmp_bitcoind_opts = opt_strip( "bitcoind_", bitcoind_opts )

      if parser.has_section('bitcoind'):
          parser.remove_section('bitcoind')

      parser.add_section( 'bitcoind' )
      for opt_name, opt_value in tmp_bitcoind_opts.items():
         if opt_value is None:
             raise Exception("%s is not defined" % opt_name)
         parser.set( 'bitcoind', opt_name, "%s" % opt_value )

   if utxo_opts is not None and len(utxo_opts) > 0:

      if parser.has_section( utxo_opts['utxo_provider'] ):
          parser.remove_section( utxo_opts['utxo_provider'] )

      parser.add_section( utxo_opts['utxo_provider'] )
      for opt_name, opt_value in utxo_opts.items():

         # don't log this meta-field
         if opt_name == 'utxo_provider':
             continue

         if opt_value is None:
             raise Exception("%s is not defined" % opt_name)

         parser.set( utxo_opts['utxo_provider'], opt_name, "%s" % opt_value )

   if blockstack_opts is not None and len(blockstack_opts) > 0:

      if parser.has_section("blockstack"):
          parser.remove_section("blockstack")

      parser.add_section( "blockstack" )
      for opt_name, opt_value in blockstack_opts.items():

          if opt_value is None:
              raise Exception("%s is not defined" % opt_name )

          parser.set( "blockstack", opt_name, "%s" % opt_value )


   with open(config_file, "w") as fout:
      os.fchmod( fout.fileno(), 0600 )
      parser.write( fout )

   return True


