#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
    Blockstack
    ~~~~~
    copyright: (c) 2014-2015 by Halfmoon Labs, Inc.
    copyright: (c) 2016-2017 by Blockstack.org

    This file is part of Blockstack.

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

# This module lets the blockstack client use theh Sia network as a storage provider.

import os
import sys
import traceback
import requests
import urllib
import urlparse
import logging
from common import get_logger, DEBUG
from ConfigParser import SafeConfigParser

log = get_logger("blockstack-storage-driver-sia")
log.setLevel(logging.DEBUG if DEBUG else logging.INFO)

SIAD_HOST = None
SIAD_PORT = None
SIAD_PASSWD = None
USER_AGENT = "Sia-Agent/blockstack-driver-sia"


def storage_init(config):
    """
    This method initializes the storage driver.
    It may be called multiple times, so if you need idempotency,
    you'll need to implement it yourself.
 
    Return True on successful initialization
    Return False on error.
    """

    global SIAD_HOST, SIAD_PORT, SIAD_PASSWD

    # path to the CLI's configuration file (where you can stash driver-specific configuration)
    config_path = config['path']
    if os.path.exists(config_path):
        parser = SafeConfigParser()

        try:
            parser.read(config_path)
        except Exception, e:
            log.exception(e)
            return False

        if parser.has_section('sia'):
            if parser.has_option('sia', 'host'):
                SIAD_HOST = parser.get('sia', 'host')

            if parser.has_option('sia', 'port'):
                SIAD_PORT = parser.get('sia', 'port')

            if parser.has_option('sia', 'passwd'):
                SIAD_PASSWD = parser.get('sia', 'passwd')

    if SIAD_HOST is None or SIAD_PORT is None or SIAD_PASSWD is None:
        log.error(
            "Config file '%s': section 'sia' is missing 'host', 'port', and/or 'passwd'" % config_path)
        return False

    return True


def handles_url(uri):
    """
    Does this storage driver handle this kind of URL?

    It is okay if other drivers say that they can handle it.
    This is used by the storage system to quickly filter out
    drivers that don't handle this type of URL.

    A common strategy is simply to check if the scheme
    matches what your driver does.  Another common strategy
    is to check if the URL matches a particular regex.
    """

    parts = urlparse.urlparse(uri)
    return parts.netlock.endswith("sia.tech")


def make_mutable_url(data_id):
    """
    This method creates a URL, given an (opaque) data ID string.
    The data ID string will be printable, but it is not guaranteed to 
    be globally unqiue.  It is opaque--do not assume anything about its
    structure.
 
    The URL does not need to contain the data ID or even be specific to it.
    It just needs to contain enough information that it can be used by
    get_mutable_handler() below.
 
    This method may be called more than once per data_id, and may be called
    independently of get_mutable_handler() below (which consumes this URL).
 
    Returns a string
    """

    data_id = urllib.quote(data_id.replace('/', '-2f'))  # Hex code for forward slash.

    return "http://sia.tech/%s" % data_id


def get_immutable_handler(data_hash, **kw):
    """
    Given a cryptographic hash of some data, go and fetch it.
    This is used by the immutable data API, whereby users can 
    add and remove data hashes in their zone file (hence the term
    "immutable").  The method that puts data for this method
    to fetch is put_immutable_handler(), described below.
 
    Drivers are encouraged but not required to implement this method.
    A common strategy is to treat the data_hash like the data_id in
    make_mutable_url().
 
    **kw contains hints from Blockstack about the nature of the request.
    TODO: document them here.
 
    Returns the data on success.  It must hash to data_hash (sha256)
    Returns None on error.  Does not raise an exception.
    """

    return download("immutable-%s" % data_hash)


def get_mutable_handler(uri, **kw):
    """
    Given the URL to some data generated by an earlier call to
    make_mutable_url().
 
    **kw contains hints from Blockstack about the nature of the request.
    TODO: document them here.
 
    Drivers are encouraged but not required to implement this method.
 
    Returns the data on success.  The driver is not expected to e.g. verify
    its authenticity (Blockstack will take care of this).
    Return None on error.  Does not raise an exception.
    """

    key = urlparse.urlparse(uri).path[1:]
    return download(key)


def upload(key, data, txid):
    global SIAD_HOST, SIAD_PORT, USER_AGENT, SIAD_PASSWD

    key = urllib.quote(key.replace("/", r"-2f"))

    sia_upload = "http://%s:%s/renter/upload/%s" % (SIAD_HOST, SIAD_PORT, key)

    import tempfile
    with tempfile.NamedTemporaryFile() as temp:
        log.debug("[%s] Preparing upload to siad @ %s..." % (txid, sia_upload))

        temp.write(data)
        temp.flush()

        ok = False

        try:
            r = requests.post(sia_upload, params={
                'source': temp.name
            }, headers={
                'user-agent': USER_AGENT
            }, auth=('', SIAD_PASSWD))

            log.debug("[%s] Uploading to siad @ %s..." % (txid, r.url))

            ok = r.status_code == requests.codes.no_content

            if not ok:
                log.debug("failed to upload file to siad. Status: %s - Response: %s", r.status_code, r.json())
        except Exception as e:
            log.exception(e)

        return ok


def download(key):
    global SIAD_HOST, SIAD_PORT, USER_AGENT, SIAD_PASSWD

    key = urllib.quote(key.replace("/", r"-2f"))

    sia_download = "http://%s:%s/renter/download/%s" % (SIAD_HOST, SIAD_PORT, key)

    log.debug("Preparing download from siad @ %s..." % sia_download)

    import tempfile
    with tempfile.NamedTemporaryFile() as temp:
        try:
            r = requests.get(sia_download, params={
                'destination': temp.name
            }, headers={
                'user-agent': USER_AGENT
            }, auth=('', SIAD_PASSWD))

            log.debug("Downloaded %s from siad @ %s..." % (temp.name, r.url))

            ok = r.status_code == requests.codes.no_content

            if not ok:
                log.debug("failed to download file from siad. Status: %s - Response: %s", r.status_code, r.json())
                return None

            temp.seek(0)
            return temp.read()
        except Exception as e:
            log.exception(e)
            return None


def delete(key, txid):
    global SIAD_HOST, SIAD_PORT, USER_AGENT, SIAD_PASSWD

    key = urllib.quote(key.replace("/", r"-2f"))

    sia_delete = "http://%s:%s/renter/delete/%s" % (SIAD_HOST, SIAD_PORT, key)

    log.debug("[%s] Preparing to delete from siad @ %s..." % (txid, sia_delete))

    ok = False

    try:
        r = requests.post(sia_delete, headers={
            'user-agent': USER_AGENT
        }, auth=('', SIAD_PASSWD))

        log.debug("Delete attempt from siad @ %s..." % r.url)

        ok = r.status_code == requests.codes.no_content

        if not ok:
            log.debug("failed to delete file from siad. Status: %s - Response: %s", r.status_code, r.json())
    except Exception as e:
        log.exception(e)

    return ok


def put_immutable_handler(key, data, txid, **kw):
    """
    Store data that was written by the immutable data API.
    That is, the user updated their zone file and added a data
    hash to it.  This method is given the data's hash (sha256),
    the data itself (as a string), and the transaction ID in the underlying
    blockchain (i.e. as "proof-of-payment").
 
    The driver should store the data in such a way that a
    subsequent call to get_immutable_handler() with the same
    data hash returns the given data here.
 
    **kw contains hints from Blockstack about the nature of the request.
    TODO: document these.
 
    Drivers are encouraged but not required to implement this method.
    Read-only data sources like HTTP servers would not implement this
    method, for example.
 
    Returns True on successful storage
    Returns False on failure.  Does not raise an exception
    """

    return upload("immutable-%s" % key, data, txid)


def put_mutable_handler(data_id, data, **kw):
    """
    Store (signed) data to this storage provider.  The only requirement
    is that a call to get_mutable_url(data_id) must generate a URL that
    can be fed into get_mutable_handler() to get the data back.  That is,
    the overall flow will be:
 
    # store data 
    rc = put_mutable_handler( data_id, data_txt, **kw )
    if not rc:
       # error path...
 
    # ... some time later ...
    # get the data back
    data_url = get_mutable_url( data_id )
    assert data_url 
 
    data_txt_2 = get_mutable_handler( data_url, **kw )
    if data_txt_2 is None:
       # error path...
 
    assert data_txt == data_txt_2
 
    The data_txt argument is the data itself (as a string).
    **kw contains hints from the Blockstack implementation.
    TODO: document these.
 
    Returns True on successful store
    Returns False on error.  Does not raise an exception
    """

    return upload(data_id, data, None)


def delete_immutable_handler(key, txid, tombstone, **kw):
    """
    Delete immutable data.  Called when the user removed a datum's hash
    from their zone file, and the driver must now go and remove the data
    from the storage provider.
 
    The driver is given the hash of the data (data_hash) and the underlying
    blockchain transaction ID (txid).
 
    The tombstone argument is used to prove to the driver that
    the request to delete data corresponds to an earlier request to store data.
    sig_data_txid is the signature over the string
    "delete:{}{}".format(data_hash, txid).  The user's data private key is
    used to generate the signature.  Most driver implementations
    can ignore this, but some storage systems with weak consistency 
    guarantees may find it useful in order to NACK outstanding
    writes.
 
    **kw are hints from Blockstack to the driver.
    TODO: document these
 
    Returns True on successful deletion
    Returns False on failure.  Does not raise an exception.
    """

    return delete("immutable-%s" % key, txid)


def delete_mutable_handler(data_id, tombstone, **kw):
    """
    Delete mutable data.  Called when user requested that some data
    stored earlier with put_mutable_handler() be deleted.
 
    The tombstone argument is used to prove to the driver and
    underlying storage system that the
    request to delete the data corresponds to an earlier request
    to store it.  It is the signature over the string 
    "delete:{}".format(data_id).  Most driver implementations can
    ignore this; it's meant for use with storage systems with
    weak consistency guarantees.
 
    **kw are hints from Blockstack to the driver.
    TODO: document these
 
    Returns True on successful deletion
    Returns False on failure.  Does not raise an exception.
    """

    return delete(data_id, None)


if __name__ == "__main__":
    """
    Unit tests.
    """

    import keylib
    import json
    from virtualchain.lib.hashing import hex_hash160

    # hack around absolute paths
    current_dir = os.path.abspath(os.path.dirname(__file__))
    sys.path.insert(0, current_dir)

    current_dir = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    sys.path.insert(0, current_dir)

    from blockstack_client.storage import parse_mutable_data, serialize_mutable_data
    from blockstack_client.config import log, get_config

    CONFIG_PATH = os.environ.get('BLOCKSTACK_CONFIG_PATH', None)
    assert CONFIG_PATH, "Missing BLOCKSTACK_CONFIG_PATH from environment"

    conf = get_config(CONFIG_PATH)
    print json.dumps(conf, indent=4, sort_keys=True)

    pk = keylib.ECPrivateKey()
    data_privkey = pk.to_hex()
    data_pubkey = pk.public_key().to_hex()

    test_data = [
        ["my_first_datum", "hello world", 1, "unused", None],
        ["/my/second/datum", "hello world 2", 2, "unused", None],
        ["user\"_profile", '{"name":{"formatted":"judecn"},"v":"2"}', 3, "unused", None],
        ["empty_string", "", 4, "unused", None],
    ]


    def hash_data(d):
        return hex_hash160(d)


    rc = storage_init(conf)
    if not rc:
        raise Exception("Failed to initialize")

    if len(sys.argv) > 1:
        # try to get these profiles
        for name in sys.argv[1:]:
            prof = get_mutable_handler(make_mutable_url(name))
            if prof is None:
                raise Exception("Failed to get %s" % name)

            print json.dumps(prof, indent=4, sort_keys=True)

        sys.exit(0)

    # put_immutable_handler
    print "put_immutable_handler"
    for i in xrange(0, len(test_data)):

        d_id, d, n, s, url = test_data[i]

        print "store {} ({})".format(d_id, hash_data(d))

        rc = put_immutable_handler(hash_data(d), d, "unused")
        if not rc:
            raise Exception("put_immutable_handler('%s') failed" % d)

    # put_mutable_handler
    print "put_mutable_handler"
    for i in xrange(0, len(test_data)):

        d_id, d, n, s, url = test_data[i]

        data_url = make_mutable_url(d_id)

        print 'store {} with {}'.format(d_id, data_privkey)
        data_json = serialize_mutable_data(json.dumps({"id": d_id, "nonce": n, "data": d}), data_privkey)

        rc = put_mutable_handler(d_id, data_json)
        if not rc:
            raise Exception("put_mutable_handler('%s', '%s') failed" % (d_id, d))

        test_data[i][4] = data_url

    # get_immutable_handler
    print "get_immutable_handler"
    for i in xrange(0, len(test_data)):

        d_id, d, n, s, url = test_data[i]

        print "get {}".format(hash_data(d))
        rd = get_immutable_handler(hash_data(d))
        if rd != d:
            raise Exception("get_mutable_handler('%s'): '%s' != '%s'" % (hash_data(d), d, rd))

    # get_mutable_handler
    print "get_mutable_handler"
    for i in xrange(0, len(test_data)):

        d_id, d, n, s, url = test_data[i]

        print "get {}".format(d_id)
        rd_json = get_mutable_handler(url)
        if rd_json is None:
            raise Exception("Failed to get data {}".format(d_id))

        rd = parse_mutable_data(rd_json, data_pubkey)
        if rd is None:
            raise Exception("Failed to parse mutable data '%s'" % rd_json)

        rd = json.loads(rd)
        if rd['id'] != d_id:
            raise Exception("Data ID mismatch: '%s' != '%s'" % (rd['id'], d_id))

        if rd['nonce'] != n:
            raise Exception("Nonce mismatch: '%s' != '%s'" % (rd['nonce'], n))

        if rd['data'] != d:
            raise Exception("Data mismatch: '%s' != '%s'" % (rd['data'], d))

    # delete_immutable_handler
    print "delete_immutable_handler"
    for i in xrange(0, len(test_data)):

        d_id, d, n, s, url = test_data[i]

        print "delete {}".format(hash_data(d))
        rc = delete_immutable_handler(hash_data(d), "unused", "unused")
        if not rc:
            raise Exception("delete_immutable_handler('%s' (%s)) failed" % (hash_data(d), d))

    # delete_mutable_handler
    print "delete_mutable_handler"
    for i in xrange(0, len(test_data)):

        d_id, d, n, s, url = test_data[i]

        print "delete {}".format(d_id)
        rc = delete_mutable_handler(d_id, "unused")
        if not rc:
            raise Exception("delete_mutable_handler('%s') failed" % d_id)
