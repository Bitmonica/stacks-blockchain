/*
 copyright: (c) 2013-2020 by Blockstack PBC, a public benefit corporation.

 This file is part of Blockstack.

 Blockstack is free software. You may redistribute or modify
 it under the terms of the GNU General Public License as published by
 the Free Software Foundation, either version 3 of the License or
 (at your option) any later version.

 Blockstack is distributed in the hope that it will be useful,
 but WITHOUT ANY WARRANTY, including without the implied warranty of
 MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 GNU General Public License for more details.

 You should have received a copy of the GNU General Public License
 along with Blockstack. If not, see <http://www.gnu.org/licenses/>.
*/

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::collections::BTreeMap;

use core::mempool::MemPoolDB;

use net::Error as net_error;
use net::*;
use net::connection::*;
use net::db::*;
use net::chat::*;
use net::poll::*;
use net::rpc::*;
use net::http::*;
use net::p2p::*;

use chainstate::burn::db::burndb::BurnDB;
use chainstate::stacks::db::{StacksChainState, StacksHeaderInfo};
use chainstate::stacks::StacksBlockHeader;
use chainstate::stacks::events::StacksTransactionReceipt;

use core::mempool::*;

use chainstate::burn::db::burndb::BurnDBTx;

use burnchains::Burnchain;
use burnchains::BurnchainView;

use util::hash::Sha512Trunc256Sum;
use util::get_epoch_time_secs;

use rand::prelude::*;
use rand::Rng;
use rand::thread_rng;

pub type BlocksAvailableMap = HashMap<BurnchainHeaderHash, (u64, ConsensusHash)>;

pub const MAX_RELAYER_STATS : usize = 4096;
pub const MAX_RECENT_MESSAGES : usize = 256;
pub const MAX_RECENT_MESSAGE_AGE : usize = 600;     // seconds; equal to the expected epoch length
pub const RELAY_DUPLICATE_INFERENCE_WARMUP : usize = 128;

pub struct Relayer {
    /// Connection to the p2p thread
    p2p: NetworkHandle,
}

#[derive(Debug)]
pub struct RelayerStats {
    /// Relayer statistics for the p2p network's ongoing conversations.
    /// Note that we key on (addr, port), not the full NeighborAddress.
    /// (TODO: Nothing is done with this yet, but one day we'll use it to probe for network
    /// choke-points).
    relay_stats: HashMap<NeighborAddress, RelayStats>,
    relay_updates: BTreeMap<u64, NeighborAddress>,

    /// Messages sent from each neighbor recently (includes duplicates)
    recent_messages: HashMap<NeighborKey, VecDeque<(u64, Sha512Trunc256Sum)>>,
    recent_updates: BTreeMap<u64, NeighborKey>,

    next_priority: u64
}

/// Private trait for keeping track of messages that can be relayed, so we can identify the peers
/// who frequently send us duplicates.
pub trait RelayPayload {
    /// Get a representative digest of this message.
    /// m1.get_digest() == m2.get_digest() --> m1 == m2
    fn get_digest(&self) -> Sha512Trunc256Sum;
}

impl RelayPayload for BlocksAvailableData {
    fn get_digest(&self) -> Sha512Trunc256Sum {
        let mut bytes = vec![];
        self.consensus_serialize(&mut bytes).expect("BUG: failed to serialize");
        let h = Sha512Trunc256Sum::from_data(&bytes);
        h
    }
}

impl RelayPayload for StacksBlock {
    fn get_digest(&self) -> Sha512Trunc256Sum {
        let h = self.block_hash();
        Sha512Trunc256Sum(h.0)
    }
}

impl RelayPayload for StacksMicroblock {
    fn get_digest(&self) -> Sha512Trunc256Sum {
        let h = self.block_hash();
        Sha512Trunc256Sum(h.0)
    }
}

impl RelayPayload for StacksTransaction {
    fn get_digest(&self) -> Sha512Trunc256Sum {
        let h = self.txid();
        Sha512Trunc256Sum(h.0)
    }
}

impl RelayerStats {
    pub fn new() -> RelayerStats {
        RelayerStats {
           relay_stats: HashMap::new(),
           relay_updates: BTreeMap::new(),
           recent_messages: HashMap::new(),
           recent_updates: BTreeMap::new(),
           next_priority: 0
        }
    }

    /// Add in new stats gleaned from the PeerNetwork's network result
    pub fn merge_relay_stats(&mut self, mut stats: HashMap<NeighborAddress, RelayStats>) -> () {
        for (mut addr, new_stats) in stats.drain() {
            addr.clear_public_key();
            let inserted = 
                if let Some(stats) = self.relay_stats.get_mut(&addr) {
                    stats.merge(new_stats);
                    false
                }
                else {
                    // remove oldest relay memories if we have too many
                    if self.relay_stats.len() > MAX_RELAYER_STATS - 1 {
                        let mut to_remove = vec![];
                        for (ts, old_addr) in self.relay_updates.iter() {
                            self.relay_stats.remove(old_addr);
                            if self.relay_stats.len() <= MAX_RELAYER_STATS - 1 {
                                break;
                            }
                            to_remove.push(*ts);
                        }
                        for ts in to_remove.drain(..) {
                            self.relay_updates.remove(&ts);
                        }
                    }
                    self.relay_stats.insert(addr.clone(), new_stats);
                    true
                };

            if inserted {
                self.relay_updates.insert(self.next_priority, addr);
                self.next_priority += 1;
            }
        }
    }

    /// Record that we've seen a relayed message from one of our neighbors.
    pub fn add_relayed_message<R: RelayPayload>(&mut self, nk: NeighborKey, msg: &R) -> () {
        let h = msg.get_digest();
        let now = get_epoch_time_secs();
        let inserted = 
            if let Some(relayed) = self.recent_messages.get_mut(&nk) {
                relayed.push_back((now, h));

                // prune if too many
                while relayed.len() > MAX_RECENT_MESSAGES {
                    relayed.pop_front();
                }
                
                // prune stale
                while relayed.len() > 0 {
                    let head_ts = match relayed.front() {
                        Some((ts, _)) => *ts,
                        None => {
                            break;
                        }
                    };
                    if head_ts + (MAX_RECENT_MESSAGE_AGE as u64) < now {
                        relayed.pop_front();
                    }
                    else {
                        break;
                    }
                }
                false
            }
            else {
                let mut relayed = VecDeque::new();
                relayed.push_back((now, h));

                // remove oldest neighbor memories if we have too many
                if self.recent_messages.len() > MAX_RELAYER_STATS {
                    let mut to_remove = vec![];
                    for (ts, old_nk) in self.recent_updates.iter() {
                        self.recent_messages.remove(old_nk);
                        if self.recent_messages.len() <= (MAX_RELAYER_STATS as usize) - 1 {
                            break;
                        }
                        to_remove.push(*ts);
                    }
                    for ts in to_remove {
                        self.recent_updates.remove(&ts);
                    }
                }

                self.recent_messages.insert(nk.clone(), relayed);
                true
            };

        if inserted {
            self.recent_updates.insert(self.next_priority, nk);
            self.next_priority += 1;
        }
    }

    /// Process a neighbor ban -- remove any state for this neighbor
    pub fn process_neighbor_ban(&mut self, nk: &NeighborKey) -> () {
        let addr = NeighborAddress::from_neighbor_key((*nk).clone(), Hash160([0u8; 20]));
        self.recent_messages.remove(nk);
        self.relay_stats.remove(&addr);

        // old state in self.recent_updates and self.relay_updates will eventually be removed by
        // add_relayed_message() and merge_relay_stats()
    }

    /// See if anyone has sent this message to us already, and if so, return the set of neighbors
    /// that did so already (and how many times)
    pub fn count_relay_dups<R: RelayPayload>(&self, msg: &R) -> HashMap<NeighborKey, usize> {
        let h = msg.get_digest();
        let now = get_epoch_time_secs();
        let mut ret = HashMap::new();

        for (nk, relayed) in self.recent_messages.iter() {
            for (ts, msg_hash) in relayed.iter() {
                if ts + (MAX_RECENT_MESSAGE_AGE as u64) < now {
                    // skip old
                    continue;
                }
                if *msg_hash == h {
                    if let Some(count) = ret.get_mut(nk) {
                        *count += 1;
                    }
                    else {
                        ret.insert((*nk).clone(), 1);
                    }
                }
            }
        }

        ret
    }

    /// Map neighbors to the frequency of their AS numbers in the given neighbors list
    fn count_ASNs(conn: &DBConn, neighbors: &Vec<NeighborKey>) -> Result<HashMap<NeighborKey, usize>, net_error> {
        // look up ASNs
        let mut asns = HashMap::new();
        for nk in neighbors.iter() {
            if asns.get(nk).is_none() {
                match PeerDB::asn_lookup(conn, &nk.addrbytes)? {
                    Some(asn) => asns.insert((*nk).clone(), asn),
                    None => asns.insert((*nk).clone(), 0)
                };
            }
        }

        let mut asn_dist = HashMap::new();

        // calculate ASN distribution
        for nk in neighbors.iter() {
            let asn = asns.get(nk).unwrap_or(&0);
            if let Some(asn_count) = asn_dist.get_mut(asn) {
                *asn_count += 1;
            }
            else {
                asn_dist.insert(*asn, 1);
            }
        }

        let mut ret = HashMap::new();

        // map neighbors to ASN counts
        for nk in neighbors.iter() {
            let asn = asns.get(nk).unwrap_or(&0);
            let count = *(asn_dist.get(asn).unwrap_or(&0));
            ret.insert((*nk).clone(), count);
        }

        Ok(ret)
    }

    /// Get the (non-normalized) probability distribution to use to sample inbound neighbors to
    /// relay messages to.  The probability of being selected is proportional to how rarely the
    /// neighbor sends us messages we've already seen before.  The intuition is that if an inbound
    /// neighbor (e.g. a client) sends us data that we've already seen, then it must be connected
    /// to some other peer that's already forwarding it data.  Thus, we don't need to do so.
    pub fn get_inbound_relay_rankings<R: RelayPayload>(&self, neighbors: &Vec<NeighborKey>, msg: &R, warmup_threshold: usize) -> HashMap<NeighborKey, usize> {
        let mut dup_counts = self.count_relay_dups(msg);
        let mut dup_total = dup_counts.values().fold(0, |t, s| { t + s });

        if dup_total < warmup_threshold {
            // don't make inferences on small samples for total duplicates.
            // just assume uniform distribution.
            dup_total = warmup_threshold;
            dup_counts.clear();
        }

        let mut ret = HashMap::new();

        for nk in neighbors.iter() {
            let dup_count = *(dup_counts.get(nk).unwrap_or(&0));

            assert!(dup_total >= dup_count);

            // every peer should have a non-zero chance, hence the + 1
            ret.insert((*nk).clone(), dup_total - dup_count + 1);
        }

        ret
    }
    
    /// Get the (non-normalized) probability distribution to use to sample outbound neighbors to
    /// relay messages to.  The probability of being selected is proportional to how rare the
    /// neighbor's AS number is in our neighbor set.  The intution is that we should try to
    /// disseminate our data to as many different _networks_ as quickly as possible, so nodes in
    /// those networks can take care of forwarding them to their inbound peers.
    pub fn get_outbound_relay_rankings(&self, peerdb: &PeerDB, neighbors: &Vec<NeighborKey>) -> Result<HashMap<NeighborKey, usize>, net_error> {
        let asn_counts = RelayerStats::count_ASNs(peerdb.conn(), neighbors)?;
        let asn_total = asn_counts.values().fold(0, |t, s| { t + s });

        let mut ret = HashMap::new();

        for nk in neighbors.iter() {
            let asn_count = *(asn_counts.get(nk).unwrap_or(&0));

            assert!(asn_total >= asn_count);

            // every peer should have a non-zero chance, hence the + 1
            ret.insert((*nk).clone(), asn_total - asn_count + 1);
        }

        Ok(ret)
    }
    
    /// Sample a set of neighbors according to our relay data.
    /// Sampling is done *without* replacement, so the resulting neighbors list will have length
    /// min(count, rankings.len())
    pub fn sample_neighbors(rankings: HashMap<NeighborKey, usize>, count: usize) -> Vec<NeighborKey> {
        let mut ret = vec![];
        let mut rng = thread_rng();

        let mut norm = rankings.values().fold(0, |t, s| { t + s });
        let mut rankings_vec : Vec<(NeighborKey, usize)> = rankings.into_iter().collect();

        for l in 0..count {
            if norm <= 1 {
                break;
            }

            let target : usize = rng.gen::<usize>() % norm;      // slightly biased, but it doesn't really matter
            let mut w = 0;

            for i in 0..rankings_vec.len() {
                if rankings_vec[i].1 == 0 {
                    continue;
                }

                w += rankings_vec[i].1;
                if w >= target {
                    ret.push(rankings_vec[i].0.clone());

                    // sample without replacement
                    rankings_vec[i].1 -= 1;
                    norm -= 1;
                    break;
                }
            }

            assert_eq!(l + 1, ret.len());
        }

        ret
    }
}

impl Relayer {
    pub fn new(handle: NetworkHandle) -> Relayer {
        Relayer {
           p2p: handle,
        }
    }

    pub fn from_p2p(network: &mut PeerNetwork) -> Relayer {
        let handle = network.new_handle(1024, 1024);
        Relayer::new(handle)
    }

    /// Given blocks pushed to us, verify that they correspond to expected block data.
    pub fn validate_blocks_push(conn: &DBConn, blocks_data: &BlocksData) -> Result<(), net_error> {
        for (burn_header_hash, block) in blocks_data.blocks.iter() { 
            let block_hash = block.block_hash();

            // is this the right Stacks block for this sortition?
            let sn = match BurnDB::get_block_snapshot(conn, burn_header_hash)? {
                Some(sn) => sn,
                None => {
                    // we don't know about this burn block (yet)
                    continue;
                }
            };

            if !sn.sortition || sn.winning_stacks_block_hash != block_hash {
                info!("No such sortition in block {}", burn_header_hash);

                // TODO: once PoX is implemented, this can be permitted if we're missing the reward
                // window's anchor block for the reward window in which this block lives.  Until
                // then, it's never okay -- this peer shall be considered broken.
                return Err(net_error::InvalidMessage);
            }
        }
        Ok(())
    }

    /// Insert a staging block
    fn process_new_anchored_block<'a>(burn_tx: &mut BurnDBTx<'a>, chainstate: &mut StacksChainState, burn_header_hash: &BurnchainHeaderHash, block: &StacksBlock) -> Result<bool, chainstate_error> {
        let sn = match BurnDB::get_block_snapshot(&burn_tx, burn_header_hash)? {
            Some(sn) => sn,
            None => {
                debug!("Received unknown block {}/{}", burn_header_hash, block.block_hash());
                return Ok(false);
            }
        };

        chainstate.preprocess_anchored_block(burn_tx, burn_header_hash, sn.burn_header_timestamp, block, &sn.parent_burn_header_hash)
    }

    /// Coalesce a set of microblocks into relayer hints and MicroblocksData messages, as calculated by
    /// process_new_blocks().  Make sure the messages don't get too big.
    fn make_microblocksdata_messages(mut new_microblocks: HashMap<BlockHeaderHash, (Vec<RelayData>, HashMap<BlockHeaderHash, StacksMicroblock>)>) -> Vec<(Vec<RelayData>, MicroblocksData)> {
        let mut mblocks_data : HashMap<BlockHeaderHash, Vec<(Vec<RelayData>, MicroblocksData)>> = HashMap::new();
        let mut mblocks_sizes : HashMap<BlockHeaderHash, usize> = HashMap::new();

        for (anchored_block_hash, (relayers, mut mblocks_map)) in new_microblocks.drain() {
            for (_, mblock) in mblocks_map.drain() {
                if mblocks_data.get(&anchored_block_hash).is_none() {
                    mblocks_data.insert(anchored_block_hash.clone(), vec![]);
                }

                if let Some(mblocks_msgs) = mblocks_data.get_mut(&anchored_block_hash) {
                    // should always succeed, due to the above insert
                    let mblock_len = {
                        let mut mblocks_buf = vec![];
                        mblock.consensus_serialize(&mut mblocks_buf).expect("BUG: failed to serialize microblock we received");
                        mblocks_buf.len()
                    };

                    assert!(mblock_len <= MAX_PAYLOAD_LEN as usize);     // this should always be true, since otherwise we wouldn't have been able to parse it.

                    let sz = *(mblocks_sizes.get(&anchored_block_hash).unwrap_or(&0));
                    if sz + mblock_len < (MAX_PAYLOAD_LEN as usize) {
                        // enough space to include this block in this messaege
                        if let Some((_, mblock_msg)) = mblocks_msgs.last_mut() {
                            // append to last mblocks message
                            mblock_msg.microblocks.push(mblock);
                        }
                        else {
                            // allocate the first microblocks message, and add this mblock to it
                            let mblocks_msg = MicroblocksData {
                                index_anchor_block: anchored_block_hash.clone(),
                                microblocks: vec![mblock]
                            };
                            mblocks_msgs.push((relayers.clone(), mblocks_msg));
                        }

                        // update size counter with this mblock's length
                        if let Some(sz) = mblocks_sizes.get_mut(&anchored_block_hash) {
                            *sz += mblock_len
                        }
                        else {
                            mblocks_sizes.insert(anchored_block_hash.clone(), mblock_len);
                        }
                    }
                    else {
                        // start a new microblocks message
                        let mblocks_msg = MicroblocksData {
                            index_anchor_block: anchored_block_hash.clone(),
                            microblocks: vec![mblock]
                        };
                        mblocks_msgs.push((relayers.clone(), mblocks_msg));

                        // reset size counter
                        mblocks_sizes.insert(anchored_block_hash.clone(), mblock_len);
                    }
                }
                else {
                    // shouldn't happen because we inserted into mblocks_data earlier
                    unreachable!();
                }
            }
        }

        let mut ret = vec![];
        for (_, mut v) in mblocks_data.drain() {
            ret.append(&mut v);
        }
        ret
    }

    /// Preprocess all our downloaded blocks.
    /// Return burn block hashes for the blocks that we got.
    /// Does not fail on invalid blocks; just logs a warning.
    fn preprocess_downloaded_blocks<'a>(burn_tx: &mut BurnDBTx<'a>, network_result: &mut NetworkResult, chainstate: &mut StacksChainState) -> HashSet<BurnchainHeaderHash> {
        let mut new_blocks = HashSet::new();

        for (burn_header_hash, block) in network_result.blocks.iter() {
            match Relayer::process_new_anchored_block(burn_tx, chainstate, burn_header_hash, block) {
                Ok(accepted) => {
                    if accepted {
                        new_blocks.insert((*burn_header_hash).clone());
                    }
                },
                Err(chainstate_error::InvalidStacksBlock(msg)) => {
                    warn!("Downloaded invalid Stacks block: {}", msg);
                    // NOTE: we can't punish the neighbor for this, since we could have been
                    // MITM'ed in our download.
                    continue;
                }
                Err(e) => {
                    warn!("Could not process downloaded Stacks block {}/{}: {:?}", burn_header_hash, block.block_hash(), &e);
                }
            };
        }

        new_blocks
    }

    /// Preprocess all pushed blocks
    /// Return burn block hashes for blocks we got, as well as the list of peers that served us
    /// invalid data.
    /// Does not fail; just logs warnings.
    fn preprocess_pushed_blocks<'a>(burn_tx: &mut BurnDBTx<'a>, network_result: &mut NetworkResult, chainstate: &mut StacksChainState) -> (HashSet<BurnchainHeaderHash>, Vec<NeighborKey>) {
        let mut new_blocks = HashSet::new();
        let mut bad_neighbors = vec![];

        // process blocks pushed to us.
        // If a neighbor sends us an invalid block, ban them.
        for (neighbor_key, blocks_datas) in network_result.pushed_blocks.iter() {
            for blocks_data in blocks_datas.iter() {
                match Relayer::validate_blocks_push(burn_tx, blocks_data) {
                    Ok(_) => {},
                    Err(_) => {
                        // punish this peer 
                        bad_neighbors.push((*neighbor_key).clone());
                        break;
                    }
                }

                for (burn_header_hash, block) in blocks_data.blocks.iter() {
                    debug!("Received pushed block {}/{} from {}", burn_header_hash, block.block_hash(), neighbor_key);
                    match Relayer::process_new_anchored_block(burn_tx, chainstate, burn_header_hash, block) {
                        Ok(accepted) => {
                            if accepted {
                                new_blocks.insert((*burn_header_hash).clone());
                            }
                        },
                        Err(chainstate_error::InvalidStacksBlock(msg)) => {
                            warn!("Invalid pushed Stacks block {}/{}: {}", burn_header_hash, block.block_hash(), msg);
                            bad_neighbors.push((*neighbor_key).clone());
                        },
                        Err(e) => {
                            warn!("Could not process pushed Stacks block {}/{}: {:?}", burn_header_hash, block.block_hash(), &e);
                        }
                    }
                }
            }
        }

        (new_blocks, bad_neighbors)
    }

    /// Prerocess all downloaded, confirmed microblock streams.
    /// Does not fail on invalid blocks; just logs a warning.
    /// Returns the burnchain header hashes for the stacks anchored blocks that produced these streams.
    fn preprocess_downloaded_microblocks(network_result: &mut NetworkResult, chainstate: &mut StacksChainState) -> HashSet<BurnchainHeaderHash> {
        let mut ret = HashSet::new();
        for (burn_header_hash, microblock_stream) in network_result.confirmed_microblocks.iter() {
            if microblock_stream.len() == 0 {
                continue;
            }
            let anchored_block_hash = microblock_stream[0].header.prev_block.clone();

            for mblock in microblock_stream.iter() {
                match chainstate.preprocess_streamed_microblock(burn_header_hash, &anchored_block_hash, mblock) {
                    Ok(_) => {},
                    Err(e) => {
                        warn!("Invalid downloaded microblock {}/{}-{}: {:?}", burn_header_hash, &anchored_block_hash, mblock.block_hash(), &e);
                    }
                }
            }

            ret.insert((*burn_header_hash).clone());
        }
        ret
    }

    /// Preprocess all unconfirmed microblocks pushed to us.
    /// Return the list of MicroblockData messages we need to broadcast to our neighbors, as well
    /// as the list of neighbors we need to ban because they sent us invalid microblocks.
    fn preprocess_pushed_microblocks(network_result: &mut NetworkResult, chainstate: &mut StacksChainState) -> Result<(Vec<(Vec<RelayData>, MicroblocksData)>, Vec<NeighborKey>), net_error> {
        let mut new_microblocks : HashMap<BlockHeaderHash, (Vec<RelayData>, HashMap<BlockHeaderHash, StacksMicroblock>)> = HashMap::new();
        let mut bad_neighbors = vec![];

        // process unconfirmed microblocks pushed to us.
        // If a neighbor sends us bad microblocks, ban them.
        // Remember which ones we _don't_ have, and remember the prior relay hints.
        for (neighbor_key, mblock_datas) in network_result.pushed_microblocks.iter() {
            for (mblock_relayers, mblock_data) in mblock_datas.iter() {
                let (burn_header_hash, anchored_block_hash) = match chainstate.get_block_header_hashes(&mblock_data.index_anchor_block)? {
                    Some((bhh, bh)) => (bhh, bh),
                    None => {
                        warn!("Missing anchored block whose index hash is {}", &mblock_data.index_anchor_block);
                        continue;
                    }
                };
                for mblock in mblock_data.microblocks.iter() {
                    let need_relay = !StacksChainState::has_staging_microblock(&chainstate.blocks_db, &burn_header_hash, &anchored_block_hash, &mblock.block_hash())? &&
                                     !StacksChainState::has_confirmed_microblock(&chainstate.blocks_db, &burn_header_hash, &anchored_block_hash, &mblock.block_hash())?;

                    match chainstate.preprocess_streamed_microblock(&burn_header_hash, &anchored_block_hash, mblock) {
                        Ok(_) => {
                            if need_relay {
                                // we didn't have this block before, so relay it.
                                // Group by anchored block hash, so we can convert them into
                                // MicroblocksData messages later.  Group microblocks by block
                                // hash, so we don't send dups.
                                if let Some((_, mblocks_map)) = new_microblocks.get_mut(&anchored_block_hash) {
                                    mblocks_map.insert(mblock.block_hash(), (*mblock).clone());
                                }
                                else {
                                    let mut mblocks_map = HashMap::new();
                                    mblocks_map.insert(mblock.block_hash(), (*mblock).clone());
                                    new_microblocks.insert(anchored_block_hash.clone(), ((*mblock_relayers).clone(), mblocks_map));
                                }
                            }
                        }
                        Err(chainstate_error::InvalidStacksMicroblock(msg, hash)) => {
                            warn!("Invalid pushed microblock {}/{}-{}: {:?}", &burn_header_hash, &anchored_block_hash, hash, msg);
                            bad_neighbors.push((*neighbor_key).clone());
                            continue;
                        }
                        Err(e) => {
                            warn!("Could not process pushed microblock {}/{}-{}: {:?}", &burn_header_hash, &anchored_block_hash, &mblock.block_hash(), &e);
                            continue;
                        }
                    }
                }
            }
        }
        
        let mblock_datas = Relayer::make_microblocksdata_messages(new_microblocks);
        Ok((mblock_datas, bad_neighbors))
    }

    /// Process blocks and microblocks that we recieved, both downloaded (confirmed) and streamed
    /// (unconfirmed). Returns:
    /// * list of burn header hashes for newly-discovered blocks, so we can turn them into BlocksAvailable messages
    /// * list of confirmed microblock burn header hashes for newly-discovered microblock streams, so we can turn them into MicroblocksAvailable messages
    /// * list of unconfirmed microblocks that got pushed to us, as well as their relayers (so we can forward them)
    /// * list of neighbors that served us invalid data (so we can ban them)
    pub fn process_new_blocks(network_result: &mut NetworkResult, burndb: &mut BurnDB, chainstate: &mut StacksChainState)
                              -> Result<(Vec<BurnchainHeaderHash>,
                                         Vec<BurnchainHeaderHash>, 
                                         Vec<(Vec<RelayData>, MicroblocksData)>,
                                         Vec<NeighborKey>,
                                         Vec<(StacksHeaderInfo, Vec<StacksTransactionReceipt>)>), net_error> {
        let mut new_blocks = HashSet::new();
        let mut new_confirmed_microblocks = HashSet::new();
        let mut bad_neighbors = vec![];
        {
            let mut burn_tx = burndb.tx_begin()?;

            // process blocks we downloaded
            let mut new_dled_blocks = Relayer::preprocess_downloaded_blocks(&mut burn_tx, network_result, chainstate);
            for new_dled_block in new_dled_blocks.drain() {
                new_blocks.insert(new_dled_block);
            }

            // process blocks pushed to us
            let (mut new_pushed_blocks, mut new_bad_neighbors) = Relayer::preprocess_pushed_blocks(&mut burn_tx, network_result, chainstate);
            for new_pushed_block in new_pushed_blocks.drain() {
                new_blocks.insert(new_pushed_block);
            }
            bad_neighbors.append(&mut new_bad_neighbors);

            // no need to commit the burn_tx, since it's only required for reading the MARF
        }

        let mut new_dled_mblocks = Relayer::preprocess_downloaded_microblocks(network_result, chainstate);
        for new_dled_mblock in new_dled_mblocks.drain() {
            new_confirmed_microblocks.insert(new_dled_mblock);
        }
        
        let (new_microblocks, mut new_bad_neighbors) = Relayer::preprocess_pushed_microblocks(network_result, chainstate)?;
        bad_neighbors.append(&mut new_bad_neighbors);
        
        // process as many epochs as we can.
        // Try to process at least one epoch.
        let receipts: Vec<_> = chainstate.process_blocks(new_blocks.len() + 1)?.into_iter()
            .filter_map(|block_result| block_result.0).collect();

        Ok((new_blocks.into_iter().collect(), new_confirmed_microblocks.into_iter().collect(), new_microblocks, bad_neighbors, receipts))
    }
    
    /// Produce blocks-available messages from blocks we just got.
    pub fn load_blocks_available_data(burndb: &mut BurnDB, mut burn_header_hashes: Vec<BurnchainHeaderHash>) -> Result<BlocksAvailableMap, net_error> {
        let mut ret = BlocksAvailableMap::new();
        for bhh in burn_header_hashes.drain(..) {
            let sn = match BurnDB::get_block_snapshot(&burndb.conn(), &bhh)? {
                Some(sn) => sn,
                None => {
                    continue;
                }
            };

            ret.insert(bhh, (sn.block_height, sn.consensus_hash));
        }
        Ok(ret)
    }

    /// Store a single transaction
    /// Return true if stored; false if it was a dup
    fn store_transaction(mempool: &mut MemPoolDB, burn_header_hash: &BurnchainHeaderHash, block_hash: &BlockHeaderHash, tx: StacksTransaction) -> bool {
        let txid = tx.txid();
        if mempool.has_tx(&txid) {
            return true;
        }
        
        if let Err(e) = mempool.submit(burn_header_hash, block_hash, tx) {
            info!("Reject transaction {}: {:?}", txid, &e);
            return false;
        }

        debug!("Stored tx {}", txid);
        return true;
    }

    /// Store all new transactions we received, and return the list of transactions that we need to
    /// forward (as well as their relay hints).  Also, garbage-collect the mempool.
    fn process_transactions(network_result: &mut NetworkResult, chainstate: &StacksChainState, mempool: &mut MemPoolDB) -> Result<Vec<(Vec<RelayData>, StacksTransaction)>, net_error> {
        let (burn_header_hash, block_hash, chain_height) = match chainstate.get_stacks_chain_tip()? {
            Some(tip) => (tip.burn_header_hash, tip.anchored_block_hash, tip.height),
            None => {
                info!("No Stacks chain tip; dropping {} transaction(s)", network_result.pushed_transactions.len());
                return Ok(vec![]);
            }
        };

        let mut ret = vec![];

        // messages pushed via the p2p network
        for (_nk, tx_data) in network_result.pushed_transactions.iter() {
            for (relayers, tx) in tx_data.iter() {
                if Relayer::store_transaction(mempool, &burn_header_hash, &block_hash, tx.clone()) {
                    ret.push((relayers.clone(), tx.clone()));
                }
            }
        }

        // uploaded via HTTP, but already stored to the mempool.  If we get them here, it means we
        // have to forward them.
        for tx in network_result.uploaded_transactions.iter() {
            ret.push((vec![], tx.clone()));
        }

        // garbage-collect 
        if chain_height > MEMPOOL_MAX_TRANSACTION_AGE {
            let min_height = chain_height - MEMPOOL_MAX_TRANSACTION_AGE;
            let mut mempool_tx = mempool.tx_begin()?;

            debug!("Remove all transactions beneath block height {}", min_height);
            MemPoolDB::garbage_collect(&mut mempool_tx, min_height)?;
            mempool_tx.commit()?;
        }

        Ok(ret)
    }

    pub fn advertize_blocks(&mut self, available: BlocksAvailableMap) -> Result<(), net_error> {
        self.p2p.advertize_blocks(available)
    }

    /// Given a network result, consume and store all data.
    /// * Add all blocks and microblocks to staging.
    /// * Forward BlocksAvailable messages to neighbors for newly-discovered anchored blocks
    /// * Forward MicroblocksAvailable messages to neighbors for newly-discovered confirmed microblock streams
    /// * Forward along unconfirmed microblocks that we didn't already have
    /// * Add all transactions to the mempool.
    /// * Forward transactions we didn't already have.
    /// Mask errors from invalid data -- all errors due to invalid blocks and invalid data should be captured, and
    /// turned into peer bans.
    pub fn process_network_result(&mut self, _local_peer: &LocalPeer, network_result: &mut NetworkResult, burndb: &mut BurnDB, chainstate: &mut StacksChainState, mempool: &mut MemPoolDB)
                                  -> Result<Vec<(StacksHeaderInfo, Vec<StacksTransactionReceipt>)>, net_error> {
        let receipts = match Relayer::process_new_blocks(network_result, burndb, chainstate) {
            Ok((new_blocks, new_confirmed_microblocks, mut new_microblocks, bad_block_neighbors, receipts)) => {
                // attempt to relay messages (note that this is all best-effort).
                // punish bad peers
                test_debug!("{:?}: Ban {} peers", &_local_peer, bad_block_neighbors.len());
                if let Err(e) = self.p2p.ban_peers(bad_block_neighbors) {
                    warn!("Failed to ban bad-block peers: {:?}", &e);
                }

                // have the p2p thread tell our neighbors about newly-discovered blocks
                let available = Relayer::load_blocks_available_data(burndb, new_blocks)?;
                test_debug!("{:?}: Blocks available: {}", &_local_peer, available.len());
                if let Err(e) = self.p2p.advertize_blocks(available) {
                    warn!("Failed to advertize new blocks: {:?}", &e);
                }
                
                // have the p2p thread tell our neighbors about newly-discovered confirmed microblock streams
                let mblocks_available = Relayer::load_blocks_available_data(burndb, new_confirmed_microblocks)?;
                test_debug!("{:?}: Confirmed microblock streams available: {}", &_local_peer, mblocks_available.len());
                if let Err(e) = self.p2p.advertize_microblocks(mblocks_available) {
                    warn!("Failed to advertize new confirmed microblocks: {:?}", &e);
                }

                // have the p2p thread forward all new unconfirmed microblocks
                for (relayers, mblocks_msg) in new_microblocks.drain(..) {
                    test_debug!("{:?}: Send {} microblocks for {}", &_local_peer, mblocks_msg.microblocks.len(), &mblocks_msg.index_anchor_block);
                    let msg = StacksMessageType::Microblocks(mblocks_msg);
                    if let Err(e) = self.p2p.broadcast_message(relayers, msg) {
                        warn!("Failed to broadcast microblock: {:?}", &e);
                    }
                }

                // drain and log errors from the p2p thread's attempt to handle our request
                while let Some(relay_result) = self.p2p.next_result() {
                    if let Err(relay_error) = relay_result {
                        warn!("Message relay error: {:?}", &relay_error);
                    }
                }

                receipts
            },
            Err(e) => {
                warn!("Failed to process new blocks: {:?}", &e);

                Vec::new()
            }
        };

        // store all transactions, and forward the novel ones to neighbors
        test_debug!("{:?}: Process {} transaction(s)", &_local_peer, network_result.pushed_transactions.len());
        let mut new_txs = Relayer::process_transactions(network_result, chainstate, mempool)?;

        test_debug!("{:?}: Send {} transactions to neighbors", &_local_peer, new_txs.len());
        for (relayers, tx) in new_txs.drain(..) {
            let msg = StacksMessageType::Transaction(tx);
            if let Err(e) = self.p2p.broadcast_message(relayers, msg) {
                warn!("Failed to broadcast transaction: {:?}", &e);
            }
        }

        Ok(receipts)
    }
}

impl PeerNetwork {
    /// Find out which neighbors need at least one (micro)block from the availability set.
    /// For outbound neighbors (i.e. ones we have inv data for), only send (Micro)BlocksAvailable messages
    /// for (micro)blocks we have that they don't have.  For inbound neighbors (i.e. ones we don't have
    /// inv data for), pick a random set and send them the full (Micro)BlocksAvailable message.
    fn find_block_recipients(&mut self, available: &BlocksAvailableMap) -> Result<(Vec<NeighborKey>, Vec<NeighborKey>), net_error> {
        let outbound_recipients_set = PeerNetwork::with_inv_state(self, |ref mut _network, ref mut inv_state| {
            let mut recipients = HashSet::new();
            for (neighbor, stats) in inv_state.block_stats.iter() {
                for (_, (block_height, _)) in available.iter() {
                    if !stats.inv.has_ith_block(*block_height) {
                        recipients.insert((*neighbor).clone());
                    }
                }
            }
            Ok(recipients)
        })?;

        // make a normalized random sample of inbound recipients, but don't send to an inbound peer
        // if it's already represented in the outbound set, or its reciprocal conversation is
        // represented in the outbound set.
        let mut inbound_recipients_set = HashSet::new();
        for (event_id, convo) in self.peers.iter() {
            if !convo.is_authenticated() {
                continue;
            }
            if convo.is_outbound() {
                continue;
            }
            let nk = convo.to_neighbor_key();
            if outbound_recipients_set.contains(&nk) {
                continue;
            }

            if let Some(out_nk) = self.find_outbound_neighbor(*event_id) {
                if outbound_recipients_set.contains(&out_nk) {
                    continue;
                }
            }

            inbound_recipients_set.insert(nk);
        }

        let outbound_recipients : Vec<NeighborKey> = outbound_recipients_set.into_iter().collect();
        let mut inbound_recipients_unshuffled : Vec<NeighborKey> = inbound_recipients_set.into_iter().collect();

        let inbound_recipients =
            if inbound_recipients_unshuffled.len() > MAX_BROADCAST_INBOUND_RECEIVERS {
                &mut inbound_recipients_unshuffled[..].shuffle(&mut thread_rng());
                inbound_recipients_unshuffled[0..MAX_BROADCAST_INBOUND_RECEIVERS].to_vec()
            }
            else {
                inbound_recipients_unshuffled
            };

        Ok((outbound_recipients, inbound_recipients))
    }

    /// Announce the availability of a set of blocks or microblocks to a peer.
    /// Break the availability into (Micro)BlocksAvailable messages and queue them for transmission.
    fn advertize_to_peer<S>(&mut self, recipient: &NeighborKey, wanted: &Vec<(ConsensusHash, BurnchainHeaderHash)>, mut msg_builder: S) -> () 
    where 
        S: FnMut(BlocksAvailableData) -> StacksMessageType
    {
        for i in (0..wanted.len()).step_by(BLOCKS_AVAILABLE_MAX_LEN as usize) {
            let to_send = 
                if i + (BLOCKS_AVAILABLE_MAX_LEN as usize) < wanted.len() {
                    wanted[i..(i + (BLOCKS_AVAILABLE_MAX_LEN as usize))].to_vec()
                }
                else {
                    wanted[i..].to_vec()
                };

            let num_blocks = to_send.len();
            let payload = BlocksAvailableData { available: to_send };
            let message = match self.sign_for_peer(recipient, msg_builder(payload)) {
                Ok(m) => m,
                Err(e) => {
                    warn!("{:?}: Failed to sign for {:?}: {:?}", &self.local_peer, recipient, &e);
                    continue;
                }
            };

            // absorb errors
            let _ = self.relay_signed_message(recipient, message)
                .map_err(|e| {
                    warn!("{:?}: Failed to announce {} entries to {:?}: {:?}", &self.local_peer, num_blocks, recipient, &e);
                    e
                });
        }
    }

    /// Announce blocks that we have to an outbound peer that doesn't have them.
    /// Only advertize blocks and microblocks we have that the outbound peer doesn't.
    fn advertize_to_outbound_peer(&mut self, recipient: &NeighborKey, available: &BlocksAvailableMap, microblocks: bool) -> Result<(), net_error> {
        let wanted = PeerNetwork::with_inv_state(self, |ref mut _network, ref mut inv_state| {
            let mut wanted : Vec<(ConsensusHash, BurnchainHeaderHash)> = vec![];
            if let Some(stats) = inv_state.block_stats.get(recipient) {
                for (bhh, (block_height, ch)) in available.iter() {
                    let has_data = 
                        if microblocks {
                            stats.inv.has_ith_microblock_stream(*block_height)
                        }
                        else {
                            stats.inv.has_ith_block(*block_height)
                        };

                    if !has_data {
                        test_debug!("{:?}: Outbound neighbor {:?} wants {} data for {}", 
                                    &_network.local_peer, recipient, if microblocks { "microblock" } else { "block" }, bhh);

                        wanted.push(((*ch).clone(), (*bhh).clone()));
                    }
                }
            }
            Ok(wanted)
        })?;

        if microblocks {
            self.advertize_to_peer(recipient, &wanted, |payload| StacksMessageType::MicroblocksAvailable(payload));
        }
        else {
            self.advertize_to_peer(recipient, &wanted, |payload| StacksMessageType::BlocksAvailable(payload));
        }
        
        Ok(())
    }

    /// Announce blocks that we have to an inbound peer that might not have them.
    /// Send all available blocks and microblocks, since we don't know what the inbound peer has
    /// already.
    fn advertize_to_inbound_peer<S>(&mut self, recipient: &NeighborKey, available: &BlocksAvailableMap, msg_builder: S) -> Result<(), net_error>
    where
        S: FnMut(BlocksAvailableData) -> StacksMessageType
    {
        let mut wanted : Vec<(ConsensusHash, BurnchainHeaderHash)> = vec![];
        for (burn_header_hash, (_, consensus_hash)) in available.iter() {
            wanted.push(((*consensus_hash).clone(), (*burn_header_hash).clone()));
        }
        
        self.advertize_to_peer(recipient, &wanted, msg_builder);
        Ok(())
    }

    /// Announce blocks that we have to a subset of inbound and outbound peers.
    /// * Outbound peers recieve announcements for blocks that we know they don't have, based on
    /// the inv state we synchronized from them.
    /// * Inbound peers are chosen uniformly at random to receive a full announcement, since we
    /// don't track their inventory state.
    pub fn advertize_blocks(&mut self, availability_data: BlocksAvailableMap) -> Result<(), net_error> {
        let (mut outbound_recipients, mut inbound_recipients) = self.find_block_recipients(&availability_data)?;
        for recipient in outbound_recipients.drain(..) {
            test_debug!("{:?}: Advertize {} blocks to outbound peer {}", &self.local_peer, availability_data.len(), &recipient);
            self.advertize_to_outbound_peer(&recipient, &availability_data, false)?;
        }
        for recipient in inbound_recipients.drain(..) {
            test_debug!("{:?}: Advertize {} blocks to inbound peer {}", &self.local_peer, availability_data.len(), &recipient);
            self.advertize_to_inbound_peer(&recipient, &availability_data, |payload| StacksMessageType::BlocksAvailable(payload))?;
        }
        Ok(())
    }
    
    /// Announce confirmed microblocks that we have to a subset of inbound and outbound peers.
    /// * Outbound peers recieve announcements for confirmed microblocks that we know they don't have, based on
    /// the inv state we synchronized from them.
    /// * Inbound peers are chosen uniformly at random to receive a full announcement, since we
    /// don't track their inventory state.
    pub fn advertize_microblocks(&mut self, availability_data: BlocksAvailableMap) -> Result<(), net_error> {
        let (mut outbound_recipients, mut inbound_recipients) = self.find_block_recipients(&availability_data)?;
        for recipient in outbound_recipients.drain(..) {
            test_debug!("{:?}: Advertize {} confirmed microblock streams to outbound peer {}", &self.local_peer, availability_data.len(), &recipient);
            self.advertize_to_outbound_peer(&recipient, &availability_data, true)?;
        }
        for recipient in inbound_recipients.drain(..) {
            test_debug!("{:?}: Advertize {} confirmed microblock streams to inbound peer {}", &self.local_peer, availability_data.len(), &recipient);
            self.advertize_to_inbound_peer(&recipient, &availability_data, |payload| StacksMessageType::MicroblocksAvailable(payload))?;
        }
        Ok(())
    }

    /// Update accounting information for relayed messages from a network result.
    /// This influences selecting next-hop neighbors to get data from us.
    pub fn update_relayer_stats(&mut self, network_result: &NetworkResult) -> () {
        // synchronize
        for (_, convo) in self.peers.iter_mut() {
            let stats = convo.get_stats_mut().take_relayers();
            self.relayer_stats.merge_relay_stats(stats);
        }

        for (nk, blocks_data) in network_result.pushed_blocks.iter() {
            for block_msg in blocks_data.iter() {
                for (_, block) in block_msg.blocks.iter() {
                    self.relayer_stats.add_relayed_message((*nk).clone(), block);
                }
            }
        }

        for (nk, microblocks_data) in network_result.pushed_microblocks.iter() {
            for (_, microblock_msg) in microblocks_data.iter() {
                for mblock in microblock_msg.microblocks.iter() {
                    self.relayer_stats.add_relayed_message((*nk).clone(), mblock);
                }
            }
        }

        for (nk, txs) in network_result.pushed_transactions.iter() {
            for (_, tx) in txs.iter() {
                self.relayer_stats.add_relayed_message((*nk).clone(), tx);
            }
        }
    }
}


#[cfg(test)]
mod test {
    use super::*;
    use net::*;
    use net::asn::*;
    use net::codec::*;
    use net::chat::*;
    use net::inv::*;
    use net::test::*;
    use net::download::*;
    use net::http::*;
    use net::download::test::run_get_blocks_and_microblocks;
    use chainstate::stacks::*;
    use chainstate::stacks::db::blocks::MINIMUM_TX_FEE;
    use chainstate::stacks::db::blocks::MINIMUM_TX_FEE_RATE_PER_BYTE;

    use std::collections::HashMap;
    use std::cell::RefCell;
    
    use chainstate::stacks::*;
    use chainstate::stacks::test::*;

    use vm::costs::LimitedCostTracker;
    use vm::database::ClarityDatabase;
    use vm::clarity::ClarityConnection;

    use util::sleep_ms;

    #[test]
    fn test_relayer_stats_add_relyed_messages() {
        let mut relay_stats = RelayerStats::new();
        
        let all_transactions = codec_all_transactions(&TransactionVersion::Testnet, 0x80000000, &TransactionAnchorMode::Any, &TransactionPostConditionMode::Allow);
        assert!(all_transactions.len() > MAX_RECENT_MESSAGES);

        eprintln!("Test with {} transactions", all_transactions.len());

        let nk = NeighborKey {
            peer_version: 12345,
            network_id: 0x80000000,
            addrbytes: PeerAddress([0,0,0,0,0,0,0,0,0,0,0xff,0xff,127,0,0,1]),
            port: 54321
        };

        // never overflow recent messages for a neighbor
        for (i, tx) in all_transactions.iter().enumerate() {
            relay_stats.add_relayed_message(nk.clone(), tx);

            assert_eq!(relay_stats.recent_messages.len(), 1);
            assert!(relay_stats.recent_messages.get(&nk).unwrap().len() <= MAX_RECENT_MESSAGES);

            assert_eq!(relay_stats.recent_updates.len(), 1);
        }

        assert_eq!(relay_stats.recent_messages.get(&nk).unwrap().len(), MAX_RECENT_MESSAGES);

        for i in (all_transactions.len() - MAX_RECENT_MESSAGES)..MAX_RECENT_MESSAGES {
            let digest = all_transactions[i].get_digest();
            let mut found = false;
            for (_, hash) in relay_stats.recent_messages.get(&nk).unwrap().iter() {
                found = found || (*hash == digest);
            }
            if !found {
                assert!(false);
            }
        }

        // never overflow number of neighbors tracked
        for i in 0..(MAX_RELAYER_STATS + 1) {
            let mut new_nk = nk.clone();
            new_nk.peer_version += i as u32;

            relay_stats.add_relayed_message(new_nk, &all_transactions[0]);
            
            assert!(relay_stats.recent_updates.len() <= i + 1);
            assert!(relay_stats.recent_updates.len() <= MAX_RELAYER_STATS);
        }
    }

    #[test]
    fn test_relayer_merge_stats() {
        let mut relayer_stats = RelayerStats::new();
        
        let na = NeighborAddress {
            addrbytes: PeerAddress([0,0,0,0,0,0,0,0,0,0,0xff,0xff,127,0,0,1]),
            port: 54321,
            public_key_hash: Hash160([0u8; 20])
        };

        let relay_stats = RelayStats {
            num_messages: 1,
            num_bytes: 1,
            last_seen: 1
        };

        let mut rs = HashMap::new();
        rs.insert(na.clone(), relay_stats.clone());

        relayer_stats.merge_relay_stats(rs);
        assert_eq!(relayer_stats.relay_stats.len(), 1);
        assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().num_messages, 1);
        assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().num_bytes, 1);
        assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().last_seen, 1);
        assert_eq!(relayer_stats.relay_updates.len(), 1);

        let now = get_epoch_time_secs() + 60;

        let relay_stats_2 = RelayStats {
            num_messages: 2,
            num_bytes: 2,
            last_seen: now,
        };

        let mut rs = HashMap::new();
        rs.insert(na.clone(), relay_stats_2.clone());

        relayer_stats.merge_relay_stats(rs);
        assert_eq!(relayer_stats.relay_stats.len(), 1);
        assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().num_messages, 3);
        assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().num_bytes, 3);
        assert!(relayer_stats.relay_stats.get(&na).unwrap().last_seen < now && relayer_stats.relay_stats.get(&na).unwrap().last_seen >= get_epoch_time_secs());
        assert_eq!(relayer_stats.relay_updates.len(), 1);
        
        let relay_stats_3 = RelayStats {
            num_messages: 3,
            num_bytes: 3,
            last_seen: 0,
        };
        
        let mut rs = HashMap::new();
        rs.insert(na.clone(), relay_stats_3.clone());

        relayer_stats.merge_relay_stats(rs);
        assert_eq!(relayer_stats.relay_stats.len(), 1);
        assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().num_messages, 3);
        assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().num_bytes, 3);
        assert!(relayer_stats.relay_stats.get(&na).unwrap().last_seen < now && relayer_stats.relay_stats.get(&na).unwrap().last_seen >= get_epoch_time_secs());
        assert_eq!(relayer_stats.relay_updates.len(), 1);

        for i in 0..(MAX_RELAYER_STATS + 1) {
            let na = NeighborAddress {
                addrbytes: PeerAddress([0,0,0,0,0,0,0,0,0,0,0xff,0xff,127,0,0,1]),
                port: 14321 + (i as u16),
                public_key_hash: Hash160([0u8; 20])
            };

            let now = get_epoch_time_secs() + (i as u64) + 1;

            let relay_stats = RelayStats {
                num_messages: 1,
                num_bytes: 1,
                last_seen: now,
            };

            let mut rs = HashMap::new();
            rs.insert(na.clone(), relay_stats.clone());

            relayer_stats.merge_relay_stats(rs);
            assert!(relayer_stats.relay_stats.len() <= MAX_RELAYER_STATS);
            assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().num_messages, 1);
            assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().num_bytes, 1);
            assert_eq!(relayer_stats.relay_stats.get(&na).unwrap().last_seen, now);
        }
    }

    #[test]
    fn test_relay_inbound_peer_rankings() {
        let mut relay_stats = RelayerStats::new();
        
        let all_transactions = codec_all_transactions(&TransactionVersion::Testnet, 0x80000000, &TransactionAnchorMode::Any, &TransactionPostConditionMode::Allow);
        assert!(all_transactions.len() > MAX_RECENT_MESSAGES);

        let nk_1 = NeighborKey {
            peer_version: 12345,
            network_id: 0x80000000,
            addrbytes: PeerAddress([0,0,0,0,0,0,0,0,0,0,0xff,0xff,127,0,0,1]),
            port: 54321
        };
        
        let nk_2 = NeighborKey {
            peer_version: 12345,
            network_id: 0x80000000,
            addrbytes: PeerAddress([0,0,0,0,0,0,0,0,0,0,0xff,0xff,127,0,0,1]),
            port: 54322
        };
        
        let nk_3 = NeighborKey {
            peer_version: 12345,
            network_id: 0x80000000,
            addrbytes: PeerAddress([0,0,0,0,0,0,0,0,0,0,0xff,0xff,127,0,0,1]),
            port: 54323
        };

        let dups = relay_stats.count_relay_dups(&all_transactions[0]);
        assert_eq!(dups.len(), 0);

        relay_stats.add_relayed_message(nk_1.clone(), &all_transactions[0]);
        relay_stats.add_relayed_message(nk_1.clone(), &all_transactions[0]);
        relay_stats.add_relayed_message(nk_1.clone(), &all_transactions[0]);

        let dups = relay_stats.count_relay_dups(&all_transactions[0]);
        assert_eq!(dups.len(), 1);
        assert_eq!(*dups.get(&nk_1).unwrap(), 3);

        relay_stats.add_relayed_message(nk_2.clone(), &all_transactions[0]);
        relay_stats.add_relayed_message(nk_2.clone(), &all_transactions[0]);
        relay_stats.add_relayed_message(nk_2.clone(), &all_transactions[0]);
        relay_stats.add_relayed_message(nk_2.clone(), &all_transactions[0]);

        let dups = relay_stats.count_relay_dups(&all_transactions[0]);
        assert_eq!(dups.len(), 2);
        assert_eq!(*dups.get(&nk_1).unwrap(), 3);
        assert_eq!(*dups.get(&nk_2).unwrap(), 4);

        // total dups == 7
        let dist = relay_stats.get_inbound_relay_rankings(&vec![nk_1.clone(), nk_2.clone(), nk_3.clone()], &all_transactions[0], 0);
        assert_eq!(*dist.get(&nk_1).unwrap(), 7 - 3 + 1);
        assert_eq!(*dist.get(&nk_2).unwrap(), 7 - 4 + 1);
        assert_eq!(*dist.get(&nk_3).unwrap(), 7 + 1);
       
        // high warmup period
        let dist = relay_stats.get_inbound_relay_rankings(&vec![nk_1.clone(), nk_2.clone(), nk_3.clone()], &all_transactions[0], 100);
        assert_eq!(*dist.get(&nk_1).unwrap(), 100 + 1);
        assert_eq!(*dist.get(&nk_2).unwrap(), 100 + 1);
        assert_eq!(*dist.get(&nk_3).unwrap(), 100 + 1);
    }

    #[test]
    fn test_relay_outbound_peer_rankings() {
        let relay_stats = RelayerStats::new();

        let asn1 = ASEntry4 {
            prefix: 0x10000000,
            mask: 8,
            asn: 1,
            org: 1
        };

        let asn2 = ASEntry4 {
            prefix: 0x20000000,
            mask: 8,
            asn: 2,
            org: 2
        };

        let nk_1 = NeighborKey {
            peer_version: 12345,
            network_id: 0x80000000,
            addrbytes: PeerAddress([0,0,0,0,0,0,0,0,0,0,0xff,0xff,0x10,0x11,0x12,0x13]),
            port: 54321
        };
        
        let nk_2 = NeighborKey {
            peer_version: 12345,
            network_id: 0x80000000,
            addrbytes: PeerAddress([0,0,0,0,0,0,0,0,0,0,0xff,0xff,0x20,0x21,0x22,0x23]),
            port: 54322
        };
        
        let nk_3 = NeighborKey {
            peer_version: 12345,
            network_id: 0x80000000,
            addrbytes: PeerAddress([0,0,0,0,0,0,0,0,0,0,0xff,0xff,0x20,0x21,0x22,0x24]),
            port: 54323
        };

        let n1 = Neighbor {
            addr: nk_1.clone(),
            public_key: Secp256k1PublicKey::from_hex("0260569384baa726f877d47045931e5310383f18d0b243a9b6c095cee6ef19abd6").unwrap(),
            expire_block: 4302,
            last_contact_time: 0,
            whitelisted: 0,
            blacklisted: 0,
            asn: 1,
            org: 1,
            in_degree: 0,
            out_degree: 0
        };
        
        let n2 = Neighbor {
            addr: nk_2.clone(),
            public_key: Secp256k1PublicKey::from_hex("02465f9ff58dfa8e844fec86fa5fc3fd59c75ea807e20d469b0a9f885d2891fbd4").unwrap(),
            expire_block: 4302,
            last_contact_time: 0,
            whitelisted: 0,
            blacklisted: 0,
            asn: 2,
            org: 2,
            in_degree: 0,
            out_degree: 0
        };
        
        let n3 = Neighbor {
            addr: nk_3.clone(),
            public_key: Secp256k1PublicKey::from_hex("032d8a1ea2282c1514fdc1a6f21019561569d02a225cf7c14b4f803b0393cef031").unwrap(),
            expire_block: 4302,
            last_contact_time: 0,
            whitelisted: 0,
            blacklisted: 0,
            asn: 2,
            org: 2,
            in_degree: 0,
            out_degree: 0
        };

        let peerdb = PeerDB::connect_memory(0x80000000, 0, 4032, UrlString::try_from("http://foo.com").unwrap(), &vec![asn1, asn2], &vec![n1.clone(), n2.clone(), n3.clone()]).unwrap();

        let asn_count = RelayerStats::count_ASNs(peerdb.conn(), &vec![nk_1.clone(), nk_2.clone(), nk_3.clone()]).unwrap();
        assert_eq!(asn_count.len(), 3);
        assert_eq!(*asn_count.get(&nk_1).unwrap(), 1);
        assert_eq!(*asn_count.get(&nk_2).unwrap(), 2);
        assert_eq!(*asn_count.get(&nk_3).unwrap(), 2);

        let ranking = relay_stats.get_outbound_relay_rankings(&peerdb, &vec![nk_1.clone(), nk_2.clone(), nk_3.clone()]).unwrap();
        assert_eq!(ranking.len(), 3);
        assert_eq!(*ranking.get(&nk_1).unwrap(), 5 - 1 + 1);
        assert_eq!(*ranking.get(&nk_2).unwrap(), 5 - 2 + 1);
        assert_eq!(*ranking.get(&nk_3).unwrap(), 5 - 2 + 1);
        
        let ranking = relay_stats.get_outbound_relay_rankings(&peerdb, &vec![nk_2.clone(), nk_3.clone()]).unwrap();
        assert_eq!(ranking.len(), 2);
        assert_eq!(*ranking.get(&nk_2).unwrap(), 4 - 2 + 1);
        assert_eq!(*ranking.get(&nk_3).unwrap(), 4 - 2 + 1);
    }

    #[test]
    #[ignore]
    fn test_get_blocks_and_microblocks_3_peers_push_available() {
        run_get_blocks_and_microblocks("test_get_blocks_and_microblocks_3_peers_push_available", 4200, 3,
                                       |ref mut peer_configs| {
                                           // build initial network topology.
                                           assert_eq!(peer_configs.len(), 3);

                                           // peer 0 produces the blocks
                                           peer_configs[0].connection_opts.disable_chat_neighbors = true;

                                           // peer 1 downloads the blocks from peer 0, and sends
                                           // BlocksAvailable and MicroblocksAvailable messages to
                                           // peer 2.
                                           peer_configs[1].connection_opts.disable_chat_neighbors = true;

                                           // peer 2 learns about the blocks and microblocks from peer 1's
                                           // BlocksAvaiable and MicroblocksAvailable messages, but
                                           // not from inv syncs.
                                           peer_configs[2].connection_opts.disable_chat_neighbors = true;
                                           peer_configs[2].connection_opts.disable_inv_sync = true;

                                           // generous timeouts
                                           peer_configs[0].connection_opts.timeout = 180;
                                           peer_configs[1].connection_opts.timeout = 180;
                                           peer_configs[2].connection_opts.timeout = 180;

                                           let peer_0 = peer_configs[0].to_neighbor();
                                           let peer_1 = peer_configs[1].to_neighbor();
                                           let peer_2 = peer_configs[2].to_neighbor();

                                           peer_configs[0].add_neighbor(&peer_1);
                                           peer_configs[1].add_neighbor(&peer_0);
                                           
                                           peer_configs[1].add_neighbor(&peer_2);
                                           peer_configs[2].add_neighbor(&peer_1);
                                       },
                                       |num_blocks, ref mut peers| {
                                           // build up block data to replicate
                                           let mut block_data = vec![];
                                           for _ in 0..num_blocks {
                                               let (burn_ops, stacks_block, microblocks) = peers[0].make_default_tenure();
                                               peers[0].next_burnchain_block(burn_ops.clone());
                                               peers[1].next_burnchain_block(burn_ops.clone());
                                               peers[2].next_burnchain_block(burn_ops.clone());

                                               peers[0].process_stacks_epoch_at_tip(&stacks_block, &microblocks);

                                               let sn = BurnDB::get_canonical_burn_chain_tip(&peers[0].burndb.as_ref().unwrap().conn()).unwrap();
                                               block_data.push((sn.burn_header_hash.clone(), Some(stacks_block), Some(microblocks)));
                                           }
                                           block_data
                                       },
                                       |ref mut peers| {
                                           // make sure peer 2's inv has an entry for peer 1, even
                                           // though it's not doing an inv sync
                                           let peer_1_nk = peers[1].to_neighbor().addr;
                                           match peers[2].network.inv_state {
                                               Some(ref mut inv_state) => {
                                                   if inv_state.get_stats(&peer_1_nk).is_none() {
                                                       test_debug!("initialize inv statistics for peer 1 in peer 2");
                                                       inv_state.add_peer(peer_1_nk);
                                                   }
                                                   else {
                                                       test_debug!("peer 2 has inv state for peer 1");
                                                   }
                                               },
                                               None => {
                                                   test_debug!("No inv state for peer 2");
                                               }
                                           }

                                           // peer 2 should never see a BlocksInv
                                           // message.  That would imply it asked for an inv
                                           for (_, convo) in peers[2].network.peers.iter() {
                                               assert_eq!(convo.stats.get_message_recv_count(StacksMessageID::BlocksInv), 0);
                                           }
                                       },
                                       |ref peer| {
                                           // check peer health
                                           // TODO
                                           true
                                       },
                                       |_| true);
    }
        
    fn is_peer_connected(peer: &TestPeer, dest: &NeighborKey) -> bool {
        let event_id = match peer.network.events.get(dest) {
            Some(evid) => *evid,
            None => {
                return false;
            }
        };

        match peer.network.peers.get(&event_id) {
            Some(convo) => {
                return convo.is_authenticated();
            },
            None => {
                return false;
            }
        }
    }
    
    fn push_message(peer: &mut TestPeer, dest: &NeighborKey, relay_hints: Vec<RelayData>, msg: StacksMessageType) -> bool {
        let event_id = match peer.network.events.get(dest) {
            Some(evid) => *evid,
            None => {
                panic!("Unreachable peer: {:?}", dest);
            }
        };

        let relay_msg = match peer.network.peers.get_mut(&event_id) {
            Some(convo) => {
                convo.sign_relay_message(&peer.network.local_peer, &peer.network.chain_view, relay_hints, msg).unwrap()
            },
            None => {
                panic!("No such event ID {} from neighbor {}", event_id, dest);
            }
        };

        match peer.network.relay_signed_message(dest, relay_msg.clone()) {
            Ok(_) => {
                return true;
            },
            Err(net_error::OutboxOverflow) => {
                test_debug!("{:?} outbox overflow; try again later", &peer.to_neighbor().addr);
                return false;
            },
            Err(e) => {
                test_debug!("{:?} encountered fatal error when forwarding: {:?}", &peer.to_neighbor().addr, &e);
                assert!(false);
                unreachable!();
            }
        }
    }

    fn push_block(peer: &mut TestPeer, dest: &NeighborKey, relay_hints: Vec<RelayData>, burn_header_hash: BurnchainHeaderHash, block: StacksBlock) -> bool {
        test_debug!("{:?}: Push block {}/{} to {:?}", peer.to_neighbor().addr, &burn_header_hash, block.block_hash(), dest);
        let msg = StacksMessageType::Blocks(BlocksData { blocks: vec![(burn_header_hash, block)] });
        push_message(peer, dest, relay_hints, msg)
    }
    
    fn push_microblocks(peer: &mut TestPeer, dest: &NeighborKey, relay_hints: Vec<RelayData>, burn_header_hash: BurnchainHeaderHash, block_hash: BlockHeaderHash, microblocks: Vec<StacksMicroblock>) -> bool {
        test_debug!("{:?}: Push {} microblocksblock {}/{} to {:?}", peer.to_neighbor().addr, microblocks.len(), &burn_header_hash, &block_hash, dest);
        let msg = StacksMessageType::Microblocks(MicroblocksData { index_anchor_block: StacksBlockHeader::make_index_block_hash(&burn_header_hash, &block_hash), microblocks: microblocks });
        push_message(peer, dest, relay_hints, msg)
    }

    fn push_transaction(peer: &mut TestPeer, dest: &NeighborKey, relay_hints: Vec<RelayData>, tx: StacksTransaction) -> bool {
        test_debug!("{:?}: Push tx {} to {:?}", peer.to_neighbor().addr, tx.txid(), dest);
        let msg = StacksMessageType::Transaction(tx);
        push_message(peer, dest, relay_hints, msg)
    }

    #[test]
    #[ignore]
    fn test_get_blocks_and_microblocks_2_peers_push_blocks_and_microblocks() {
        let original_blocks_and_microblocks = RefCell::new(vec![]);
        let blocks_and_microblocks = RefCell::new(vec![]);
        let idx = RefCell::new(0);
        let sent_blocks = RefCell::new(false);
        let sent_microblocks = RefCell::new(false);

        run_get_blocks_and_microblocks("test_get_blocks_and_microblocks_2_peers_push_blocks_and_microblocks", 4210, 2,
                                       |ref mut peer_configs| {
                                           // build initial network topology.
                                           assert_eq!(peer_configs.len(), 2);

                                           // peer 0 produces the blocks and pushes them to peer 1
                                           // peer 1 receives the blocks and microblocks.  It
                                           // doesn't download them, nor does it try to get invs
                                           peer_configs[0].connection_opts.disable_block_advertisement = true;

                                           peer_configs[1].connection_opts.disable_inv_sync = true;
                                           peer_configs[1].connection_opts.disable_block_download = true;
                                           peer_configs[1].connection_opts.disable_block_advertisement = true;

                                           let peer_0 = peer_configs[0].to_neighbor();
                                           let peer_1 = peer_configs[1].to_neighbor();

                                           peer_configs[0].add_neighbor(&peer_1);
                                           peer_configs[1].add_neighbor(&peer_0);
                                       },
                                       |num_blocks, ref mut peers| {
                                           // build up block data to replicate
                                           let mut block_data = vec![];
                                           for _ in 0..num_blocks {
                                               let (burn_ops, stacks_block, microblocks) = peers[0].make_default_tenure();
                                               peers[0].next_burnchain_block(burn_ops.clone());
                                               peers[1].next_burnchain_block(burn_ops.clone());

                                               peers[0].process_stacks_epoch_at_tip(&stacks_block, &microblocks);

                                               let sn = BurnDB::get_canonical_burn_chain_tip(&peers[0].burndb.as_ref().unwrap().conn()).unwrap();
                                               block_data.push((sn.burn_header_hash.clone(), Some(stacks_block), Some(microblocks)));
                                           }
                                           let saved_copy : Vec<(BurnchainHeaderHash, StacksBlock, Vec<StacksMicroblock>)> = block_data.clone().drain(..).map(|(bhh, blk_opt, mblocks_opt)| (bhh, blk_opt.unwrap(), mblocks_opt.unwrap())).collect();
                                           *blocks_and_microblocks.borrow_mut() = saved_copy.clone();
                                           *original_blocks_and_microblocks.borrow_mut() = saved_copy;
                                           block_data
                                       },
                                       |ref mut peers| {
                                           // make sure peer 2's inv has an entry for peer 1, even
                                           // though it's not doing an inv sync
                                           let peer_0_nk = peers[0].to_neighbor().addr;
                                           let peer_1_nk = peers[1].to_neighbor().addr;
                                           match peers[1].network.inv_state {
                                               Some(ref mut inv_state) => {
                                                   if inv_state.get_stats(&peer_0_nk).is_none() {
                                                       test_debug!("initialize inv statistics for peer 0 in peer 1");
                                                       inv_state.add_peer(peer_0_nk);
                                                   }
                                                   else {
                                                       test_debug!("peer 1 has inv state for peer 0");
                                                   }
                                               },
                                               None => {
                                                   test_debug!("No inv state for peer 1");
                                               }
                                           }

                                           if is_peer_connected(&peers[0], &peer_1_nk) {
                                               // randomly push a block and/or microblocks to peer 1.
                                               let mut block_data = blocks_and_microblocks.borrow_mut();
                                               let original_block_data = original_blocks_and_microblocks.borrow();
                                               let mut next_idx = idx.borrow_mut();
                                               let data_to_push = {
                                                    if block_data.len() > 0 {
                                                        let (burn_header_hash, block, microblocks) = block_data[*next_idx].clone();
                                                        Some((burn_header_hash, block, microblocks))
                                                    }
                                                    else {
                                                        // start over (can happen if a message gets
                                                        // dropped due to a timeout)
                                                        test_debug!("Reset block transmission (possible timeout)");
                                                        *block_data = (*original_block_data).clone();
                                                        *next_idx = thread_rng().gen::<usize>() % block_data.len();
                                                        let (burn_header_hash, block, microblocks) = block_data[*next_idx].clone();
                                                        Some((burn_header_hash, block, microblocks))
                                                    }
                                               };

                                               if let Some((burn_header_hash, block, microblocks)) = data_to_push {
                                                   test_debug!("Push block {}/{} and microblocks", &burn_header_hash, block.block_hash());
                                                   
                                                   let block_hash = block.block_hash();
                                                   let mut sent_blocks = sent_blocks.borrow_mut();
                                                   let mut sent_microblocks = sent_microblocks.borrow_mut();

                                                   let pushed_block = 
                                                       if !*sent_blocks {
                                                           push_block(&mut peers[0], &peer_1_nk, vec![], burn_header_hash.clone(), block)
                                                       }
                                                       else {
                                                           true
                                                       };

                                                   *sent_blocks = pushed_block;

                                                   if pushed_block {
                                                      let pushed_microblock = 
                                                          if !*sent_microblocks {
                                                              push_microblocks(&mut peers[0], &peer_1_nk, vec![], burn_header_hash, block_hash, microblocks)
                                                          } 
                                                          else {
                                                              true
                                                          };

                                                      *sent_microblocks = pushed_microblock;

                                                      if pushed_block && pushed_microblock {
                                                         block_data.remove(*next_idx);
                                                         if block_data.len() > 0 {
                                                             *next_idx = thread_rng().gen::<usize>() % block_data.len();
                                                         }
                                                         *sent_blocks = false;
                                                         *sent_microblocks = false;
                                                      }
                                                   }
                                                   test_debug!("{} blocks/microblocks remaining", block_data.len());
                                               }
                                           }

                                           // peer 0 should never see a GetBlocksInv message.
                                           // peer 1 should never see a BlocksInv message
                                           for (_, convo) in peers[0].network.peers.iter() {
                                               assert_eq!(convo.stats.get_message_recv_count(StacksMessageID::GetBlocksInv), 0);
                                           }
                                           for (_, convo) in peers[1].network.peers.iter() {
                                               assert_eq!(convo.stats.get_message_recv_count(StacksMessageID::BlocksInv), 0);
                                           }
                                       },
                                       |ref peer| {
                                           // check peer health
                                           // nothing should break
                                           // TODO
                                           true
                                       },
                                       |_| true);
    }

    fn make_test_smart_contract_transaction(peer: &mut TestPeer, name: &str, burn_header_hash: &BurnchainHeaderHash, block_hash: &BlockHeaderHash) -> StacksTransaction {
        // make a smart contract
        let contract = "
        (define-data-var bar int 0)
        (define-public (get-bar) (ok (var-get bar)))
        (define-public (set-bar (x int) (y int))
          (begin (var-set bar (/ x y)) (ok (var-get bar))))";
      
        let cost_limits = peer.config.connection_opts.read_only_call_limit.clone();

        let tx_contract = peer.with_mining_state(|ref mut burndb, ref mut miner, ref mut spending_account, ref mut stacks_node| {
            let mut tx_contract = StacksTransaction::new(TransactionVersion::Testnet,
                                                         spending_account.as_transaction_auth().unwrap().into(),
                                                         TransactionPayload::new_smart_contract(&name.to_string(), &contract.to_string()).unwrap());

            let cur_nonce = stacks_node.chainstate.with_read_only_clarity_tx(burn_header_hash, block_hash, |clarity_tx| {
                clarity_tx.with_clarity_db_readonly(|clarity_db| {
                    clarity_db.get_account_nonce(&spending_account.origin_address().unwrap().into())
                })
            });

            spending_account.set_nonce(cur_nonce + 1);

            tx_contract.chain_id = 0x80000000;
            tx_contract.auth.set_origin_nonce(cur_nonce);
            tx_contract.set_fee_rate(MINIMUM_TX_FEE_RATE_PER_BYTE * 500);
            
            let mut tx_signer = StacksTransactionSigner::new(&tx_contract);
            spending_account.sign_as_origin(&mut tx_signer);
        
            let tx_contract_signed = tx_signer.get_tx().unwrap();

            Ok(tx_contract_signed)
        }).unwrap();

        tx_contract
    }

    #[test]
    #[ignore]
    fn test_get_blocks_and_microblocks_2_peers_push_transactions() {
        let blocks_and_microblocks = RefCell::new(vec![]);
        let blocks_idx = RefCell::new(0);
        let sent_txs = RefCell::new(vec![]);
        let done = RefCell::new(false);

        let peers = run_get_blocks_and_microblocks("test_get_blocks_and_microblocks_2_peers_push_transactions", 4220, 2,
                                       |ref mut peer_configs| {
                                           // build initial network topology.
                                           assert_eq!(peer_configs.len(), 2);

                                           // peer 0 generates blocks and microblocks, and pushes
                                           // them to peer 1.  Peer 0 also generates transactions
                                           // and pushes them to peer 1.
                                           peer_configs[0].connection_opts.disable_block_advertisement = true;

                                           // let peer 0 drive this test, as before, by controlling
                                           // when peer 1 sees blocks.
                                           peer_configs[1].connection_opts.disable_inv_sync = true;
                                           peer_configs[1].connection_opts.disable_block_download = true;
                                           peer_configs[1].connection_opts.disable_block_advertisement = true;

                                           peer_configs[0].connection_opts.outbox_maxlen = 100;
                                           peer_configs[1].connection_opts.inbox_maxlen = 100;

                                           let initial_balances = vec![
                                               (PrincipalData::from(peer_configs[0].spending_account.origin_address().unwrap()), 1000000),
                                               (PrincipalData::from(peer_configs[1].spending_account.origin_address().unwrap()), 1000000)
                                           ];

                                           peer_configs[0].initial_balances = initial_balances.clone();
                                           peer_configs[1].initial_balances = initial_balances.clone();

                                           let peer_0 = peer_configs[0].to_neighbor();
                                           let peer_1 = peer_configs[1].to_neighbor();

                                           peer_configs[0].add_neighbor(&peer_1);
                                           peer_configs[1].add_neighbor(&peer_0);
                                       },
                                       |num_blocks, ref mut peers| {
                                           // build up block data to replicate
                                           let mut block_data = vec![];
                                           for _ in 0..num_blocks {
                                               let (burn_ops, stacks_block, microblocks) = peers[0].make_default_tenure();
                                               peers[0].next_burnchain_block(burn_ops.clone());
                                               peers[1].next_burnchain_block(burn_ops.clone());

                                               peers[0].process_stacks_epoch_at_tip(&stacks_block, &microblocks);

                                               let sn = BurnDB::get_canonical_burn_chain_tip(&peers[0].burndb.as_ref().unwrap().conn()).unwrap();
                                               block_data.push((sn.burn_header_hash.clone(), Some(stacks_block), Some(microblocks)));
                                           }
                                           *blocks_and_microblocks.borrow_mut() = block_data.clone().drain(..).map(|(bhh, blk_opt, mblocks_opt)| (bhh, blk_opt.unwrap(), mblocks_opt.unwrap())).collect();
                                           block_data
                                       },
                                       |ref mut peers| {
                                           // make sure peer 2's inv has an entry for peer 1, even
                                           // though it's not doing an inv sync
                                           let peer_0_nk = peers[0].to_neighbor().addr;
                                           let peer_1_nk = peers[1].to_neighbor().addr;
                                           match peers[1].network.inv_state {
                                               Some(ref mut inv_state) => {
                                                   if inv_state.get_stats(&peer_0_nk).is_none() {
                                                       test_debug!("initialize inv statistics for peer 0 in peer 1");
                                                       inv_state.add_peer(peer_0_nk);
                                                   }
                                                   else {
                                                       test_debug!("peer 1 has inv state for peer 0");
                                                   }
                                               },
                                               None => {
                                                   test_debug!("No inv state for peer 1");
                                               }
                                           }

                                           let done_flag = *done.borrow();
                                           if is_peer_connected(&peers[0], &peer_1_nk) {
                                               // push blocks and microblocks in order, and push a
                                               // transaction that can only be validated once the
                                               // block and microblocks are processed.
                                               let ((burn_header_hash, block, microblocks), idx) = {
                                                    let block_data = blocks_and_microblocks.borrow();
                                                    let mut idx = blocks_idx.borrow_mut();
                                                    
                                                    let ret = block_data[*idx].clone();
                                                    *idx += 1;
                                                    if *idx >= block_data.len() {
                                                        *idx = 0;
                                                    }
                                                    (ret, *idx)
                                               };

                                               if !done_flag {
                                                   test_debug!("Push block {}/{} and microblocks (idx = {})", &burn_header_hash, block.block_hash(), idx);
                                                   
                                                   let block_hash = block.block_hash();
                                                   push_block(&mut peers[0], &peer_1_nk, vec![], burn_header_hash.clone(), block);
                                                   push_microblocks(&mut peers[0], &peer_1_nk, vec![], burn_header_hash, block_hash, microblocks);

                                                   // create a transaction against the resulting
                                                   // (anchored) chain tip
                                                   let tx = make_test_smart_contract_transaction(&mut peers[0], &format!("test-contract-{}", &block_hash.to_hex()[0..10]), &burn_header_hash, &block_hash);

                                                   // push or post 
                                                   push_transaction(&mut peers[0], &peer_1_nk, vec![], tx.clone());

                                                   let mut expected_txs = sent_txs.borrow_mut();
                                                   expected_txs.push(tx);
                                               }
                                               else {
                                                   test_debug!("Done pushing data");
                                               }
                                           }

                                           // peer 0 should never see a GetBlocksInv message.
                                           // peer 1 should never see a BlocksInv message
                                           for (_, convo) in peers[0].network.peers.iter() {
                                               assert_eq!(convo.stats.get_message_recv_count(StacksMessageID::GetBlocksInv), 0);
                                           }
                                           for (_, convo) in peers[1].network.peers.iter() {
                                               assert_eq!(convo.stats.get_message_recv_count(StacksMessageID::BlocksInv), 0);
                                           }
                                       },
                                       |ref peer| {
                                           // check peer health
                                           // nothing should break
                                           // TODO
                                           true
                                       },
                                       |ref mut peers| {
                                           // all blocks downloaded.  only stop if peer 1 has
                                           // all the transactions
                                           let mut done_flag = done.borrow_mut();
                                           *done_flag = true;

                                           let blocks_and_microblocks_list = blocks_and_microblocks.borrow();
                                           let (last_burn_header, last_stacks_block, _) = blocks_and_microblocks_list.last().unwrap();

                                           let txs = MemPoolDB::get_all_txs(peers[1].mempool.as_ref().unwrap().conn()).unwrap();
                                           txs.len() == 10
                                       });

        // peer 1 should have all the transactions
        let blocks_and_microblocks = blocks_and_microblocks.into_inner();
        
        let txs = MemPoolDB::get_all_txs(peers[1].mempool.as_ref().unwrap().conn()).unwrap();
        let expected_txs = sent_txs.into_inner();
        for tx in txs.iter() {
            let mut found = false;
            for expected_tx in expected_txs.iter() {
                if tx.tx.txid() == expected_tx.txid() {
                    found = true;
                    break;
                }
            }
            if !found {
                panic!("Transaction not found: {:?}", &tx.tx);
            }
        }

        // peer 1 should have 1 tx per chain tip
        for ((burn_header, block, _), sent_tx) in blocks_and_microblocks.iter().zip(expected_txs.iter()) {
            let block_hash = block.block_hash();
            let tx_infos = MemPoolDB::get_txs_after(peers[1].mempool.as_ref().unwrap().conn(), burn_header, &block_hash, 0, 1000).unwrap();
            assert_eq!(tx_infos.len(), 1);
            assert_eq!(tx_infos[0].tx.txid(), sent_tx.txid());
        }
    }

// TODO: process bans
// TODO: test sending invalid blocks-available and microblocks-available (should result in a ban)
// TODO: test sending invalid transactions (should result in a ban)
// TODO: test bandwidth limits (sending too much should result in a nack, and then a ban)
}

