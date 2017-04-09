#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
    Blockstack Core
    ~~~~~

    copyright: (c) 2014-2017 by Blockstack Inc.
    copyright: (c) 2017 by Blockstack.org

This file is part of Blockstack Core.

    Blockstack Core is free software: you can redistribute it and/or modify
    it under the terms of the GNU General Public License as published by
    the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.

    Blockstack Core is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU General Public License for more details.

    You should have received a copy of the GNU General Public License
    along with Blockstack Core. If not, see <http://www.gnu.org/licenses/>.
"""

import sys
import os
import requests
import json

from flask import Flask, jsonify, request
from flask import render_template, send_from_directory

from flask_https import RequireHTTPS
from flask_crossdomain import crossdomain

from .parameters import parameters_required
from .utils import get_api_calls
from .config import PUBLIC_NODE, PUBLIC_NODE_URL, BASE_API_URL
from .config import SEARCH_NODE_URL

# hack around absolute paths
current_dir = os.path.abspath(os.path.dirname(__file__))
parent_dir = os.path.abspath(current_dir + "/../")

sys.path.insert(0, parent_dir)

import blockstack_client.config as blockstack_config
import blockstack_client.config as blockstack_constants

from blockstack_client.rpc import local_api_connect, local_api_start, local_api_action
from blockstack_client.wallet import make_wallet
from blockstack_client.proxy import getinfo

log = blockstack_config.get_logger()

"""
# starting internal API logic should go somewhere else
#local_api_start(password='temptemptemp')

#Check first if API daemon is running
status = local_api_action('status')

if(status):
    log.debug("API daemon is running")
else:
    log.debug("Start API daemon first")
    exit(0)
"""

# Import app
from . import app

@app.route('/v1/names/<name>', methods=['GET'])
@crossdomain(origin='*')
def api_names(name):

    API_URL = BASE_API_URL + '/v1/names/' + name

    resp = requests.get(API_URL)

    return jsonify(resp.json()), 200


@app.route('/v1/search', methods=['GET'])
@parameters_required(parameters=['query'])
@crossdomain(origin='*')
def search_people():

    search_url = SEARCH_URL + '/search'

    name = request.values['query']

    try:
        resp = requests.get(url=search_url, params={'query': name})
    except (RequestsConnectionError, RequestsTimeout) as e:
        raise InternalProcessingError()

    data = resp.json()
    if not ('results' in data and isinstance(data['results'], list)):
        data = {'results': []}

    return jsonify(data), 200

@app.route('/<path:path>', methods=['GET'])
def catch_all_get(path):

    API_URL = BASE_API_URL + '/' + path

    resp = requests.get(API_URL)

    return jsonify(resp.json()), 200


@app.route('/<path:path>', methods=['POST'])
def catch_all_post(path):

    if PUBLIC_NODE:
        return render_template('403.html'), 403

    API_URL = BASE_API_URL + '/' + path

    resp = requests.post(API_URL, data=requests.data)

    return jsonify(resp.json()), 200


@app.route('/')
def index():
    current_dir = os.path.abspath(os.path.dirname(__file__))
    api_calls = get_api_calls(current_dir + '/api_v1.md')
    server_info = getinfo()

    return render_template('index.html', api_calls=api_calls,
                                         server_info=server_info,
                                         server_url=PUBLIC_NODE_URL)


@app.route('/favicon.ico')
def favicon():
    return send_from_directory(app.static_folder, 'favicon.ico')


"""
@app.errorhandler(500)
def internal_error(error):
    return make_response(jsonify({'error': error.description}), 500)


@app.errorhandler(404)
def not_found(error):
    return make_response(jsonify({'error': 'Not found'}), 404)
"""