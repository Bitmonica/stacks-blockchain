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

use net::PeerAddress;
use net::Neighbor;
use net::NeighborKey;
use net::Error as net_error;
use net::db::PeerDB;
use net::asn::ASEntry4;

use net::*;
use net::codec::*;

use net::StacksMessage;
use net::StacksP2P;
use net::GetBlocksInv;
use net::connection::ConnectionP2P;
use net::connection::ReplyHandleP2P;
use net::connection::ConnectionOptions;
use net::chat::ConversationP2P;

use net::neighbors::MAX_NEIGHBOR_BLOCK_DELAY;

use net::db::*;

use net::p2p::PeerNetwork;

use util::db::Error as db_error;
use util::db::DBConn;
use util::secp256k1::Secp256k1PublicKey;
use util::secp256k1::Secp256k1PrivateKey;
use util::get_epoch_time_ms;
use util::get_epoch_time_secs;

use chainstate::burn::BlockHeaderHash;
use chainstate::burn::db::sortdb::{
    SortitionDB, SortitionDBConn, SortitionId, SortitionHandleConn, PoxId, BlockHeaderCache
};
use chainstate::burn::BlockSnapshot;

use chainstate::stacks::db::StacksChainState;

use burnchains::Burnchain;
use burnchains::BurnchainView;

use std::net::SocketAddr;

use std::cmp;

use std::collections::HashMap;
use std::collections::BTreeMap;
use std::collections::HashSet;

use std::io::Read;
use std::io::Write;

use std::convert::TryFrom;

use util::log;
use util::hash::to_hex;

/// This module is responsible for synchronizing block inventories with other peers
#[cfg(not(test))] pub const INV_SYNC_INTERVAL : u64 = 150;
#[cfg(test)] pub const INV_SYNC_INTERVAL : u64 = 10;

#[derive(Debug, PartialEq, Clone)]
pub struct PeerBlocksInv {
    /// Bitmap of which anchored blocks this peer has
    pub block_inv: Vec<u8>,
    /// Bitmap of which microblock streams this peer has 
    pub microblocks_inv: Vec<u8>,
    /// Bitmap of PoX anchor block knowledge this peer has
    pub pox_inv: Vec<u8>,
    /// Number of sortitions we know this peer knows about (after successive
    /// getblocksinv/blocksinv rounds)
    pub num_sortitions: u64,
    /// Number of reward cycles we know this peer knows about
    pub num_reward_cycles: u64,
    /// Time of last update, in seconds
    pub last_updated_at: u64,
    /// Burn block height of first sortition
    pub first_block_height: u64
}

impl PeerBlocksInv {
    pub fn empty(first_block_height: u64) -> PeerBlocksInv {
        PeerBlocksInv {
            block_inv: vec![],
            microblocks_inv: vec![],
            pox_inv: vec![],
            num_sortitions: 0,
            num_reward_cycles: 0,
            last_updated_at: 0,
            first_block_height: first_block_height
        }
    }

    pub fn new(block_inv: Vec<u8>, microblocks_inv: Vec<u8>, pox_inv: Vec<u8>, num_sortitions: u64, num_reward_cycles: u64, first_block_height: u64) -> PeerBlocksInv {
        assert_eq!(block_inv.len(), microblocks_inv.len());
        PeerBlocksInv {
            block_inv: block_inv,
            microblocks_inv: microblocks_inv,
            num_sortitions: num_sortitions,
            num_reward_cycles: num_reward_cycles,
            pox_inv: pox_inv,
            last_updated_at: get_epoch_time_secs(),
            first_block_height: first_block_height
        }
    }

    /// Does this remote neighbor have the ith block (for the ith sortition)?
    /// (note that block_height is the _absolute_ block height)
    pub fn has_ith_block(&self, block_height: u64) -> bool {
        if block_height < self.first_block_height {
            return false;
        }
        
        let sortition_height = block_height - self.first_block_height;
        let idx = (sortition_height / 8) as usize;
        let bit = sortition_height % 8;
        
        if idx >= self.block_inv.len() {
            false
        }
        else {
            (self.block_inv[idx] & (1 << bit)) != 0
        }
    }
    
    /// Does this remote neighbor have the ith microblock stream (for the ith sortition)?
    /// (note that block_height is the _absolute_ block height)
    pub fn has_ith_microblock_stream(&self, block_height: u64) -> bool {
        if block_height < self.first_block_height {
            return false;
        }
        
        let sortition_height = block_height - self.first_block_height;
        let idx = (sortition_height / 8) as usize;
        let bit = sortition_height % 8;
        
        if idx >= self.microblocks_inv.len() {
            false
        }
        else {
            (self.microblocks_inv[idx] & (1 << bit)) != 0
        }
    }

    /// Does this remote neighbor have certainty about the ith PoX anchor block?
    pub fn has_ith_anchor_block(&self, reward_cycle: u64) -> bool {
        if self.num_reward_cycles <= reward_cycle {
            return false;
        }

        let idx = (reward_cycle / 8) as usize;
        let bit = reward_cycle % 8;

        if idx >= self.pox_inv.len() {
            false
        }
        else {
            (self.pox_inv[idx] & (1 << bit)) != 0
        }
    }

    /// Merge a blocksinv into our knowledge of what blocks exist for this neighbor.
    /// block_height corresponds to bitvec[0] & 0x01
    /// bitlen = number of sortitions represented by this inv.
    /// If clear_bits is true, then any 0-bits in the given bitvecs will be set as well as 1-bits.
    /// returns the number of bits set in each bitvec
    pub fn merge_blocks_inv(&mut self, block_height: u64, bitlen: u64, block_bitvec: Vec<u8>, microblocks_bitvec: Vec<u8>, clear_bits: bool) -> (usize, usize) {
        assert!(block_height >= self.first_block_height);
        let sortition_height = block_height - self.first_block_height;

        self.num_sortitions = 
            if self.num_sortitions < sortition_height + (bitlen as u64) {
                sortition_height + (bitlen as u64)
            }
            else {
                self.num_sortitions
            };

        let mut insert_index = sortition_height;
        let mut new_blocks = 0;
        let mut new_microblocks = 0;

        // add each bit in, growing the block inv as needed
        for i in 0..bitlen {
            let idx = (i / 8) as usize;
            let bit = i % 8;
            let block_set = (block_bitvec[idx] & (1 << bit)) != 0;
            let microblock_set = (microblocks_bitvec[idx] & (1 << bit)) != 0;

            let set_idx = (insert_index / 8) as usize;
            let set_bit = insert_index % 8;
            if set_idx >= self.block_inv.len() {
                self.block_inv.resize(set_idx + 1, 0);
            }
            if set_idx >= self.microblocks_inv.len() {
                self.microblocks_inv.resize(set_idx + 1, 0);
            }

            if block_set {
                if self.block_inv[set_idx] & (1 << set_bit) == 0 {
                    // new 
                    new_blocks += 1;
                }

                self.block_inv[set_idx] |= 1 << set_bit;
            }
            else if clear_bits {
                // unset
                self.block_inv[set_idx] &= !(1 << set_bit);
            }

            if microblock_set {
                if self.microblocks_inv[set_idx] & (1 << set_bit) == 0 {
                    // new 
                    new_microblocks += 1;
                }

                self.microblocks_inv[set_idx] |= 1 << set_bit;
            }
            else if clear_bits {
                // unset
                self.microblocks_inv[set_idx] &= !(1 << set_bit);
            }

            insert_index += 1;
        }

        self.last_updated_at = get_epoch_time_secs();

        assert!(insert_index / 8 <= self.block_inv.len() as u64);
        assert!(self.num_sortitions / 8 <= self.block_inv.len() as u64);

        (new_blocks, new_microblocks)
    }

    /// Invalidate block and microblock inventories as a result of learning a new reward cycle's status.
    /// Drop all blocks and microblocks at and after the given reward cycle.
    /// Returns how many bits were dropped.
    fn truncate_block_inventories(&mut self, burnchain: &Burnchain, reward_cycle: u64) -> u64 {
        // invalidate all blocks and microblocks that come after this
        let highest_agreed_block_height = burnchain.reward_cycle_to_block_height(reward_cycle);

        assert!(highest_agreed_block_height >= self.first_block_height, "BUG: highest agreed block height is lower than the first-ever block");

        if self.first_block_height + self.num_sortitions >= highest_agreed_block_height {
            // clear block/microblock inventories
            let num_bits = self.first_block_height + self.num_sortitions - highest_agreed_block_height;
            let mut zeros : Vec<u8> = Vec::with_capacity((num_bits / 8 + 1) as usize);
            for _i in 0..(num_bits / 8 + 1) {
                zeros.push(0x00);
            }

            test_debug!("Clear all blocks after height {} (reward cycle {}; {} bits)", highest_agreed_block_height, reward_cycle, num_bits);
            self.merge_blocks_inv(highest_agreed_block_height, num_bits, zeros.clone(), zeros.clone(), true);
            self.num_sortitions = highest_agreed_block_height - self.first_block_height;
            num_bits
        }
        else {
            0
        }
    }

    /// Invalidate PoX inventories as a result of learning a new reward cycle's status
    /// Returns how many bits were dropped
    fn truncate_pox_inventory(&mut self, burnchain: &Burnchain, reward_cycle: u64) -> u64 {
        let highest_agreed_block_height = burnchain.reward_cycle_to_block_height(reward_cycle);

        assert!(highest_agreed_block_height >= self.first_block_height, "BUG: highest agreed block height is lower than the first-ever block");

        if reward_cycle < self.num_reward_cycles {
            // clear pox inventories
            let num_bits = self.num_reward_cycles - reward_cycle;
            let mut zeros : Vec<u8> = Vec::with_capacity((num_bits / 8 + 1) as usize);
            for _i in 0..(num_bits / 8 + 1) {
                zeros.push(0x00);
            }

            self.merge_pox_inv(burnchain, reward_cycle, num_bits, zeros, true);
            let diff = self.num_reward_cycles - reward_cycle;
            self.num_reward_cycles = reward_cycle;
            diff
        }
        else {
            0
        }
    }
    
    /// Merge a remote peer's PoX bitvector into our view of its PoX bitvector.
    /// If we flip a 0 to a 1, then invalidate the block/microblock bits for that reward cycle _and
    /// all subsequent reward cycles_.
    /// Returns the lowest reward cycle number that changed from a 0 to a 1, if such a flip happens
    pub fn merge_pox_inv(&mut self, burnchain: &Burnchain, reward_cycle: u64, bitlen: u64, pox_bitvec: Vec<u8>, clear_bits: bool) -> Option<u64> {
        self.num_reward_cycles = 
            if self.num_reward_cycles < reward_cycle + bitlen {
                reward_cycle + bitlen
            }
            else {
                self.num_reward_cycles
            };

        let mut insert_index = reward_cycle;
        let mut reward_cycle_flipped = None;

        // add each bit in, growing the pox inv as needed
        for i in 0..bitlen {
            let idx = (i / 8) as usize;
            let bit = i % 8;
            let anchor_block_set = (pox_bitvec[idx] & (1 << bit)) != 0;

            let set_idx = (insert_index / 8) as usize;
            let set_bit = insert_index % 8;
            if set_idx >= self.pox_inv.len() {
                self.pox_inv.resize(set_idx + 1, 0);
            }

            if anchor_block_set {
                if self.pox_inv[set_idx] & (1 << set_bit) == 0 {
                    // we didn't know about this bit
                    if reward_cycle_flipped.is_none() {
                        reward_cycle_flipped = Some(insert_index);
                    }
                }

                self.pox_inv[set_idx] |= 1 << set_bit;
            }
            else if clear_bits {
                // unset
                self.pox_inv[set_idx] &= !(1 << set_bit);
            }

            insert_index += 1;
        }

        if let Some(flipped_reward_cycle) = reward_cycle_flipped.as_ref() {
            self.truncate_block_inventories(burnchain, *flipped_reward_cycle);
        }

        self.last_updated_at = get_epoch_time_secs();

        assert!(insert_index / 8 <= self.pox_inv.len() as u64);
        assert!(self.num_reward_cycles / 8 <= self.pox_inv.len() as u64);

        reward_cycle_flipped
    }

    /// Set a block's bit as available.
    /// Return whether or not the block bit was flipped to 1.
    pub fn set_block_bit(&mut self, block_height: u64) -> bool {
        let (new_blocks, _) = self.merge_blocks_inv(block_height, 1, vec![0x01], vec![0x00], false);
        new_blocks != 0
    }
    
    /// Set a confirmed microblock stream's bit as available.
    /// Return whether or not the bit was flipped to 1.
    pub fn set_microblocks_bit(&mut self, block_height: u64) -> bool {
        let (_, new_mblocks) = self.merge_blocks_inv(block_height, 1, vec![0x00], vec![0x01], false);
        new_mblocks != 0
    }

    /// Clear a block bit
    pub fn clear_block_bit(&mut self, block_height: u64) {
        self.merge_blocks_inv(block_height, 1, vec![0x01], vec![0x00], true);
    }

    /// Clear a microblock bit
    pub fn clear_microblock_bit(&mut self, microblock_height: u64) {
        self.merge_blocks_inv(microblock_height, 1, vec![0x00], vec![0x01], true);
    }

    /// Set a confirmed anchor block detection.
    /// Return whether or not the bit was flipped to 1
    pub fn set_pox_bit(&mut self, burnchain: &Burnchain, reward_cycle: u64) -> bool {
        let bits_set = self.merge_pox_inv(burnchain, reward_cycle, 1, vec![0x01], true);
        bits_set.unwrap_or(0) != 0
    }

    /// Count up the number of blocks represented
    pub fn num_blocks(&self) -> u64 {
        let mut total = 0;
        for i in 0..self.num_sortitions {
            if self.has_ith_block(i) {
                total += 1;
            }
        }
        total
    }
    
    /// Count up the number of microblock streams represented
    pub fn num_microblock_streams(&self) -> u64 {
        let mut total = 0;
        for i in 0..self.num_sortitions {
            if self.has_ith_microblock_stream(i) {
                total += 1;
            }
        }
        total
    }

    /// Count up the number of anchor blocks represented
    pub fn num_pox_anchor_blocks(&self) -> u64 {
        let mut total = 0;
        for i in 0..self.num_reward_cycles {
            if self.has_ith_anchor_block(i) {
                total += 1;
            }
        }
        total
    }
    
    /// Determine the lowest reward cycle that this pox inv disagrees with a given pox id
    /// Returns (disagreed reward cycle, my-inv-bit, poxid-inv-bit)
    /// If one is longer than the other, there will be disagreement
    pub fn pox_inv_cmp(&self, pox_id: &PoxId) -> Option<(u64, bool, bool)> {
        let min = cmp::min(pox_id.len() as u64, self.num_reward_cycles);
        for i in 0..min {
            let my_bit = self.has_ith_anchor_block(i);
            let pox_bit = pox_id.has_ith_anchor_block(i as usize);
            if my_bit != pox_bit {
                return Some((i, my_bit, pox_bit));
            }
        }
        if (pox_id.len() as u64) == self.num_reward_cycles {
            // all agreed
            None
        }
        else if (pox_id.len() as u64) < self.num_reward_cycles {
            // pox inv is longer
            Some((pox_id.len() as u64, self.has_ith_anchor_block(pox_id.len() as u64), false))
        }
        else {
            // our inv is longer
            Some((self.num_reward_cycles, false, pox_id.has_ith_anchor_block(self.num_reward_cycles as usize)))
        }
    }

    /// Compare a Pox ID against our PoX inventory, but if one is a prefix of another, and they
    /// otherwise agree, then return None.
    pub fn pox_inv_prefix_cmp(&self, pox_id: &PoxId) -> Option<(u64, bool, bool)> {
        let (disagreed, my_bit, pox_bit) = match self.pox_inv_cmp(pox_id) {
            Some(x) => x,
            None => {
                return None;
            }
        };

        if disagreed == (pox_id.len() as u64) || disagreed == self.num_reward_cycles {
            // one contains the other as a prefix
            None
        }
        else {
            Some((disagreed, my_bit, pox_bit))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Copy)]
pub enum InvWorkState {
    GetPoxInvBegin,
    GetPoxInvFinish,
    GetBlocksInvBegin,
    GetBlocksInvFinish,
    Done
}

#[derive(Debug, Clone, PartialEq, Copy)]
pub enum NodeStatus {
    Online,
    Broken,
    Diverged,
    Stale,
    Dead
}

#[derive(Debug)]
pub struct NeighborBlockStats {
    /// Who are we talking to?
    pub nk: NeighborKey,
    /// What blocks do we know this peer has?
    pub inv: PeerBlocksInv,
    /// Current scan height for PoX (in reward cycles)
    pub pox_reward_cycle: u64,
    /// Scan height for block invs (in reward cycles)
    pub block_reward_cycle: u64,
    /// Scan state
    pub state: InvWorkState,
    /// Peer status
    pub status: NodeStatus,
    /// Ongoing request
    pub request: Option<ReplyHandleP2P>,
    /// How many blocks do we expect, if this is a BlocksInv we're expecting
    pub num_blocks_expected: u64,
    /// Ongoing request's PoX target reward cycle
    pub target_pox_reward_cycle: u64,
    /// Ongoing request's block target reward cycle
    pub target_block_reward_cycle: u64,
    /// Received PoxInv
    pub pox_inv: Option<PoxInvData>,
    /// Received BlocksInv
    pub blocks_inv: Option<BlocksInvData>,
    /// Last time we did a full scan
    pub last_rescan_timestamp: u64,
    /// Finished synchronizing?
    pub done: bool,
    /// Did we learn anything new?
    pub learned_data: bool,
}

impl NeighborBlockStats {
    pub fn new(nk: NeighborKey, first_block_height: u64) -> NeighborBlockStats {
        NeighborBlockStats {
            nk: nk,
            inv: PeerBlocksInv::empty(first_block_height),
            pox_reward_cycle: 0,
            block_reward_cycle: 0,
            state: InvWorkState::GetPoxInvBegin,
            status: NodeStatus::Online,
            num_blocks_expected: 0,
            target_pox_reward_cycle: 0,
            target_block_reward_cycle: 0,
            request: None,
            pox_inv: None,
            blocks_inv: None,
            last_rescan_timestamp: 0,
            done: false,
            learned_data: false
        }
    }

    pub fn is_peer_online(&self) -> bool {
        self.status == NodeStatus::Online
    }

    pub fn reset_pox_scan(&mut self, pox_reward_cycle: u64) {
        self.pox_reward_cycle = pox_reward_cycle;
        self.request = None;
        self.pox_inv = None;
        self.blocks_inv = None;
        self.state = InvWorkState::GetPoxInvBegin;
    }

    pub fn reset_block_scan(&mut self, block_reward_cycle: u64) {
        self.block_reward_cycle = block_reward_cycle;
        self.request = None;
        self.pox_inv = None;
        self.blocks_inv = None;
        self.state = InvWorkState::GetBlocksInvBegin;
    }

    /// Determine what to do with a NACK response.
    fn diagnose_nack(_nk: &NeighborKey,
                     nack_data: NackData,
                     chain_view: &BurnchainView,
                     preamble_burn_block_height: u64,
                     preamble_burn_stable_block_height: u64,
                     preamble_burn_block_hash: &BurnchainHeaderHash,
                     preamble_burn_stable_block_hash: &BurnchainHeaderHash) -> NodeStatus {

        let mut diverged = false;
        let mut unstable = false;
        let mut broken = false;
        let mut stale = false;

        if nack_data.error_code == NackErrorCodes::Throttled {
            // TODO: do something smarter here, like just back off
            return NodeStatus::Dead;
        }
        else if nack_data.error_code == NackErrorCodes::NoSuchBurnchainBlock {
            // peer nacked us -- it doesn't know about the block(s) we asked about.
            if preamble_burn_block_height < chain_view.burn_block_height {
                // Because it's stale
                test_debug!("Remote neighbor {:?} is still bootstrapping at block {}, whereas we are at block {}", _nk, preamble_burn_block_height, chain_view.burn_block_height);
                stale = true;
            }
            else {
                // Because it's diverged?
                diverged = match chain_view.last_burn_block_hashes.get(&preamble_burn_stable_block_height) {
                    Some(stable_ch) => stable_ch != preamble_burn_stable_block_hash,
                    None => true
                };
                // Because its view of the unstable portion of the burn chain is not the same as
                // ours?
                unstable = 
                    if chain_view.burn_block_height == preamble_burn_block_height {
                        chain_view.burn_block_hash != *preamble_burn_block_hash
                    }
                    else {
                        // peer is ahead of us, so we can't tell
                        true
                    };

                if diverged {
                    debug!("Remote neighbor {:?} NACKed us because it diverged", _nk);
                }
                else if unstable {
                    debug!("Remote neighbor {:?} NACKed us because it's chain tip is different from ours", _nk);
                }
                else {
                    // something else is wrong
                    debug!("Remote neighbor {:?} NACKed us because its block height is not in the chain view", _nk);
                    broken = true;
                }
            }
        }
        else if nack_data.error_code == NackErrorCodes::InvalidPoxFork {
            // peer nacked us -- it's on a different PoX fork
            debug!("Remote neighbor {:?} NACKed us because it is on a different PoX fork than our GetPoxInv/GetBlocksInv query suggested", _nk);
            diverged = true;
        }
        else {
            // some other error
            debug!("Remote neighbor {:?} NACKed us with error code {}", _nk, nack_data.error_code);
            broken = true;
        }

        if broken {
            NodeStatus::Broken
        }
        else if diverged || unstable {
            NodeStatus::Diverged
        }
        else if stale {
            NodeStatus::Stale
        }
        else {
            NodeStatus::Dead
        }
    }
    
    fn handle_nack(&mut self, chain_view: &BurnchainView, preamble: &Preamble, nack_data: NackData) {
        let preamble_burn_block_height = preamble.burn_block_height;
        let preamble_burn_stable_block_height = preamble.burn_stable_block_height;
        let preamble_burn_block_hash = &preamble.burn_block_hash;
        let preamble_burn_stable_block_hash = &preamble.burn_stable_block_hash;

        self.status = NeighborBlockStats::diagnose_nack(&self.nk, nack_data, &chain_view, preamble_burn_block_height, preamble_burn_stable_block_height, preamble_burn_block_hash, preamble_burn_stable_block_hash);
    }

    pub fn getpoxinv_begin(&mut self, request: ReplyHandleP2P, target_pox_reward_cycle: u64) {
        assert!(!self.done);
        assert_eq!(self.state, InvWorkState::GetPoxInvBegin);

        self.request = Some(request);
        self.pox_inv = None;
        self.target_pox_reward_cycle = target_pox_reward_cycle;

        self.state = InvWorkState::GetPoxInvFinish;
    }

    /// Determine whether or not a received PoxInv is less certain than the local PoX
    /// inventory.  Return the lowest reward cycle where the remote node is less certain than us.
    pub fn check_remote_pox_inv_uncertainty(network: &mut PeerNetwork, target_pox_reward_cycle: u64, poxinv_data: &PoxInvData) -> u64 {
        let mut bit = target_pox_reward_cycle;
        while bit < (network.pox_id.len() as u64) && (bit - target_pox_reward_cycle) < poxinv_data.bitlen as u64 && (bit - target_pox_reward_cycle) < u16::max_value() as u64 {
            if network.pox_id.has_ith_anchor_block(bit as usize) && !poxinv_data.has_ith_reward_cycle((bit - target_pox_reward_cycle) as u16) {
                // the given PoxInvData is less certain than us
                break;
            }
            bit += 1;
        }
        return bit;
    }
    
    /// Determine whether or not a received PoxInv is more certain as the local PoX
    /// inventory.  Return the loewst reward cycle where the local nodes is less certain than the
    /// remote node.
    pub fn check_local_pox_inv_uncertainty(network: &mut PeerNetwork, target_pox_reward_cycle: u64, poxinv_data: &PoxInvData) -> u64 {
        let mut bit = target_pox_reward_cycle;
        while bit < (network.pox_id.len() as u64) && (bit - target_pox_reward_cycle) < poxinv_data.bitlen as u64 && (bit - target_pox_reward_cycle) < u16::max_value() as u64 {
            if !network.pox_id.has_ith_anchor_block(bit as usize) && poxinv_data.has_ith_reward_cycle((bit - target_pox_reward_cycle) as u16) {
                // the given PoxInvData is more certain than us
                break;
            }
            bit += 1;
        }
        return bit;
    }

    /// Try to finish getting all PoxInvData requests.
    /// Return true if this method is done -- i.e. all requests have been handled.
    /// Return false if we're not done.
    /// Return Err(..) on irrecoverable error.
    pub fn getpoxinv_try_finish(&mut self, network: &mut PeerNetwork) -> Result<bool, net_error> {
        assert!(!self.done);
        assert_eq!(self.state, InvWorkState::GetPoxInvFinish);
        
        let mut request = self.request.take().expect("BUG: no request set");
        if let Err(e) = network.saturate_p2p_socket(request.get_event_id(), &mut request) {
            self.status = NodeStatus::Dead;
            return Err(e);
        }

        let next_request = match request.try_send_recv() {
            Ok(message) => {
                match message.payload {
                    StacksMessageType::PoxInv(poxinv_data) => {
                        debug!("Got PoxInv response at reward cycle {} from {:?} at ({},{}): {:?}", self.target_pox_reward_cycle, &self.nk, message.preamble.burn_block_height, message.preamble.burn_stable_block_height, &poxinv_data); 
                        self.pox_inv = Some(poxinv_data);
                    },
                    StacksMessageType::Nack(nack_data) => {
                        debug!("Remote neighbor {:?} nack'ed our GetPoxInv at reward cycle {}: NACK code {}", &self.nk, self.target_pox_reward_cycle, nack_data.error_code);
                        self.handle_nack(&network.chain_view, &message.preamble, nack_data);
                    },
                    _ => {
                        // unexpected reply
                        debug!("Remote neighbor {:?} sent an unexpected reply of '{}'", &self.nk, message.get_message_name());
                        self.status = NodeStatus::Broken;
                        return Err(net_error::InvalidMessage);
                    }
                }
                None
            },
            Err(req_res) => match req_res {
                Ok(same_req) => Some(same_req),
                Err(e) => {
                    debug!("Failed to get PoX inventory: {:?}", &e);
                    self.status = NodeStatus::Dead;
                    None
                }
            }
        };

        if let Some(next_request) = next_request {
            // still working
            self.request = Some(next_request);
            Ok(false)
        }
        else {
            // done!
            self.state = InvWorkState::Done;
            Ok(true)
        }
    }

    /// Proceed to get block inventories
    pub fn getblocksinv_begin(&mut self, request: ReplyHandleP2P, target_block_reward_cycle: u64, num_blocks_expected: u16) {
        assert!(!self.done);
        assert_eq!(self.state, InvWorkState::GetBlocksInvBegin);

        self.request = Some(request);
        self.target_block_reward_cycle = target_block_reward_cycle;
        self.num_blocks_expected = num_blocks_expected as u64;

        self.state = InvWorkState::GetBlocksInvFinish;
    }

    /// Try to finish getting all BlocksInvData requests.
    /// Return true if this method is done -- i.e. all requests have been handled.
    /// Return false if we're not done.
    pub fn getblocksinv_try_finish(&mut self, network: &mut PeerNetwork) -> Result<bool, net_error> {
        assert!(!self.done);
        assert_eq!(self.state, InvWorkState::GetBlocksInvFinish);

        let mut request = self.request.take().expect("BUG: request not set");
        if let Err(e) = network.saturate_p2p_socket(request.get_event_id(), &mut request) {
            self.status = NodeStatus::Dead;
            return Err(e);
        }

        let next_request = match request.try_send_recv() {
            Ok(message) => {
                match message.payload {
                    StacksMessageType::BlocksInv(blocks_inv_data) => {
                        // got a BlocksInv!
                        // but, did we get all the bits we asked for?
                        if blocks_inv_data.bitlen as u64 != self.num_blocks_expected {
                            info!("Got invalid BlocksInv response: expected {} bits, got {}", self.num_blocks_expected, blocks_inv_data.bitlen);
                            self.status = NodeStatus::Broken;
                        }
                        else {
                            debug!("Got BlocksInv response from {:?} at reward cycle {} at ({},{}): {:?}", &self.nk, self.target_block_reward_cycle, message.preamble.burn_block_height, message.preamble.burn_stable_block_height, &blocks_inv_data);
                            self.blocks_inv = Some(blocks_inv_data);
                        }
                    },
                    StacksMessageType::Nack(nack_data) => {
                        debug!("Remote neighbor {:?} nack'ed our GetBlocksInv at reward cycle {}: NACK code {}", &self.nk, self.target_block_reward_cycle, nack_data.error_code);
                        self.handle_nack(&network.chain_view, &message.preamble, nack_data);
                    },
                    _ => {
                        // unexpected reply
                        debug!("Remote neighbor {:?} sent an unexpected reply of '{}'", &self.nk, message.get_message_name());
                        self.status = NodeStatus::Broken;
                    }
                }
                None
            },
            Err(req_res) => match req_res {
                Ok(same_req) => Some(same_req),
                Err(e) => {
                    debug!("Failed to send/receive GetBlocksInv/BlocksInv from {:?}: {:?}", &self.nk, &e);
                    self.status = NodeStatus::Dead;
                    None
                }
            }
        };

        if let Some(next_request) = next_request {
            debug!("Still waiting for BlocksInv reply from {:?}", &self.nk);
            self.request = Some(next_request);
            Ok(false)
        }
        else {
            self.state = InvWorkState::Done;
            Ok(true)
        }
    }
}

pub struct InvState {
    /// Peers that we are currently synchronizing with.
    pub sync_peers: HashSet<NeighborKey>,
    
    /// Accumulated knowledge of which peers have which blocks.
    /// Kept separately from p2p conversations so they persist 
    /// beyond connection resets (since they can be expensive 
    /// to build up).
    pub block_stats: HashMap<NeighborKey, NeighborBlockStats>,

    /// How long is a request allowed to take?
    request_timeout: u64,
    /// First burn block height
    first_block_height: u64,

    /// Last time we learned about new blocks
    pub last_change_at: u64,
    /// How often to re-sync
    sync_interval: u64,
    /// Did any neighbor learn any new data?
    pub hint_learned_data: bool,
    /// Should we do a full re-scan?
    hint_do_full_rescan: bool,
    /// last time a full scan was completed
    last_rescanned_at: u64
}

impl InvState {
    pub fn new(first_block_height: u64, request_timeout: u64, sync_interval: u64, initial_peers: HashSet<NeighborKey>) -> InvState {
        InvState {
            sync_peers: initial_peers,

            block_stats: HashMap::new(),

            request_timeout: request_timeout,
            first_block_height: first_block_height,

            last_change_at: 0,
            sync_interval: sync_interval,

            hint_learned_data: false,
            hint_do_full_rescan: true,
            last_rescanned_at: 0,
        }
    }

    pub fn reset_sync_peers(&mut self, peers: HashSet<NeighborKey>) -> () {
        self.sync_peers.clear();
        self.sync_peers = peers;

        for peer in self.sync_peers.iter() {
            if !self.block_stats.contains_key(peer) {
                self.block_stats.remove(peer);
            }
        }

        for (_, stats) in self.block_stats.iter_mut() {
            if stats.status != NodeStatus::Online {
                stats.status = NodeStatus::Online;
            }
            stats.done = false;
            stats.learned_data = false;
        }

        for peer in self.sync_peers.iter() {
            if let Some(stats) = self.block_stats.get_mut(peer) {
                stats.reset_pox_scan(0);
            }
            else {
                self.block_stats.insert(peer.clone(), NeighborBlockStats::new(peer.clone(), self.first_block_height));
            }
        }
    }

    pub fn get_peer_status(&self, nk: &NeighborKey) -> NodeStatus {
        if let Some(stats) = self.block_stats.get(nk) {
            stats.status
        }
        else {
            NodeStatus::Dead
        }
    }

    /// How many sortitions do we know about from this neighbor?
    /// Ignores broken or diverged peers.
    pub fn get_inv_sortitions(&self, nk: &NeighborKey) -> u64 {
        if self.get_peer_status(nk) != NodeStatus::Online {
            return 0;
        }

        match self.block_stats.get(nk) {
            Some(stats) => stats.inv.num_sortitions,
            _ => 0
        }
    }
    
    /// How many blocks do we know about from this neighbor?
    /// Ignores broken or diverged peers
    pub fn get_inv_num_blocks(&self, nk: &NeighborKey) -> u64 {
        if self.get_peer_status(nk) != NodeStatus::Online {
            return 0;
        }

        match self.block_stats.get(nk) {
            Some(stats) => stats.inv.num_blocks(),
            _ => 0
        }
    }

    /// Cull broken peers and purge their stats
    pub fn cull_bad_peers(&mut self) {
        let mut bad_peers = HashSet::new();
        for (nk, stats) in self.block_stats.iter() {
            if stats.status == NodeStatus::Broken || stats.status == NodeStatus::Dead {
                debug!("Peer {:?} has node status {:?}; culling...", nk, &stats.status);
                self.sync_peers.remove(nk);
                bad_peers.insert(nk.clone());
            }
        }

        for bad_peer in bad_peers.into_iter() {
            self.block_stats.remove(&bad_peer);
        }
    }

    /// Get the list of broken peers
    pub fn get_broken_peers(&self) -> Vec<NeighborKey> {
        let mut list = vec![];
        for (nk, stats) in self.block_stats.iter() {
            if stats.status == NodeStatus::Broken {
                list.push(nk.clone());
            }
        }
        list
    }
    
    /// Get the list of diverged peers 
    pub fn get_diverged_peers(&self) -> Vec<NeighborKey> {
        let mut list = vec![];
        for (nk, stats) in self.block_stats.iter() {
            if stats.status == NodeStatus::Diverged {
                list.push(nk.clone());
            }
        }
        list
    }
    
    /// Get the list of dead
    pub fn get_dead_peers(&self) -> Vec<NeighborKey> {
        let mut list = vec![];
        for (nk, stats) in self.block_stats.iter() {
            if stats.status == NodeStatus::Dead {
                list.push(nk.clone());
            }
        }
        list
    }

    pub fn get_stats(&self, nk: &NeighborKey) -> Option<&NeighborBlockStats> {
        self.block_stats.get(nk)
    }

    pub fn add_peer(&mut self, nk: NeighborKey) -> () {
        self.block_stats.insert(nk.clone(), NeighborBlockStats::new(nk.clone(), self.first_block_height));
    }

    pub fn del_peer(&mut self, nk: &NeighborKey) -> () {
        self.block_stats.remove(&nk);
    }

    /// Set a block or confirmed microblock stream as available, given the burn header hash and consensus hash.
    /// Used when processing a BlocksAvailable or MicroblocksAvailable message.
    /// Returns the optional block sortition height at which the block or confirmed microblock stream resides in the blockchain (returns
    /// None if its bit was already set).
    fn set_data_available(&mut self, neighbor_key: &NeighborKey, sortdb: &SortitionDB, consensus_hash: &ConsensusHash, microblocks: bool) -> Result<Option<u64>, net_error> {
        let sn = match SortitionDB::get_block_snapshot_consensus(sortdb.conn(), &consensus_hash)? {
            Some(sn) => {
                if !sn.pox_valid {
                    debug!("Unknown consensus hash {}: not on valid PoX fork", consensus_hash);
                    return Ok(None);
                }
                sn
            },
            None => {
                // we don't know about this block -- the sending node is probably on a different
                // PoX fork (or is too far ahead of us)
                debug!("Unknown consensus hash {}", consensus_hash);
                return Ok(None);
            }
        };

        if !sn.sortition || sn.block_height == 0 {
            // No block is available here anyway, even though the peer agrees with us on the
            // consensus hash.
            // This is bad behavior on the peer's part.
            test_debug!("No sortition for consensus hash {}", consensus_hash);
            return Err(net_error::InvalidMessage);
        }

        match self.block_stats.get_mut(neighbor_key) {
            Some(stats) => {
                // NOTE: block heights are 1-indexed in the burn DB, since the 0th snapshot block is the
                // genesis snapshot and doesn't correspond to anything (the 1st snapshot is block 0)
                let set = 
                    if microblocks {
                        debug!("Neighbor {:?} now has confirmed microblock stream at {} ({})", neighbor_key, sn.block_height - 1, consensus_hash);
                        stats.inv.set_microblocks_bit(sn.block_height - 1)
                    }
                    else {
                        debug!("Neighbor {:?} now has block at {} ({})", neighbor_key, sn.block_height - 1, consensus_hash);
                        stats.inv.set_block_bit(sn.block_height - 1)
                    };

                debug!("Neighbor {:?} stats: {:?}", neighbor_key, stats);
                if set {
                    let block_sortition_height = sn.block_height - 1 - sortdb.first_block_height;
                    Ok(Some(block_sortition_height))
                }
                else {
                    Ok(None)
                }
            },
            None => {
                test_debug!("No inv stats for neighbor {:?}", neighbor_key);
                Ok(None)
            }
        }
    }
    
    pub fn set_block_available(&mut self, neighbor_key: &NeighborKey, sortdb: &SortitionDB, consensus_hash: &ConsensusHash) -> Result<Option<u64>, net_error> {
        self.set_data_available(neighbor_key, sortdb, consensus_hash, false)
    }

    pub fn set_microblocks_available(&mut self, neighbor_key: &NeighborKey, sortdb: &SortitionDB, consensus_hash: &ConsensusHash) -> Result<Option<u64>, net_error> {
        self.set_data_available(neighbor_key, sortdb, consensus_hash, true)
    }

    /// Invalidate all block inventories at and after a given reward cycle
    pub fn invalidate_block_inventories(&mut self, burnchain: &Burnchain, reward_cycle: u64) {
        for (_, stats) in self.block_stats.iter_mut() {
            let pox_dropped = stats.inv.truncate_pox_inventory(burnchain, reward_cycle);
            let blocks_dropped = stats.inv.truncate_block_inventories(burnchain, reward_cycle);

            if pox_dropped > 0 || blocks_dropped > 0 {
                // re-start synchronization at this height
                stats.reset_pox_scan(reward_cycle);
            }
        }
    }
}

impl PeerNetwork { 
    /// Get our current tip snapshot, accounting for PoX invalidation
    fn get_tip_sortition_snapshot(&self, sortdb: &SortitionDB) -> Result<BlockSnapshot, net_error> {
        match SortitionDB::get_block_snapshot(sortdb.conn(), &self.tip_sort_id)? {
            Some(sn) => {
                if !sn.pox_valid {
                    // our sortition ID tip got invalidated
                    return Err(net_error::StaleView);
                }
                Ok(sn)
            },
            None => {
                Err(net_error::StaleView)
            }
        }
    }

    /// Get an ancestor snapshot, accounting for PoX invalidation
    fn get_ancestor_sortition_snapshot(&self, sortdb: &SortitionDB, height: u64) -> Result<BlockSnapshot, net_error> {
        let sn = self.get_tip_sortition_snapshot(sortdb)?;
        let ic = sortdb.index_conn();
        match SortitionDB::get_ancestor_snapshot(&ic, height, &sn.sortition_id)? {
            Some(sn) => {
                if !sn.pox_valid {
                    // our sortition ID tip got invalidated
                    return Err(net_error::StaleView);
                }
                Ok(sn)
            },
            None => {
                Err(net_error::StaleView)
            }
        }
    }

    /// Get a snapshot that a peer claims to have.
    /// Return Ok(None) if we don't have this snapshot
    fn get_peer_sortition_snapshot(&self, sortdb: &SortitionDB, burn_header_hash: &BurnchainHeaderHash) -> Result<Option<BlockSnapshot>, net_error> {
        let ic = sortdb.index_conn();
        let sortdb_reader = SortitionHandleConn::open_reader(&ic, &self.tip_sort_id)?;
        match sortdb_reader.get_block_snapshot(burn_header_hash)? {
            Some(sn) => {
                if !sn.pox_valid {
                    // our sortition ID tip got invalidated
                    return Err(net_error::StaleView);
                }
                Ok(Some(sn))
            },
            None => {
                Ok(None)
            }
        }
    }

    /// Try to make a GetPoxInv request for the target reward cycle for this peer.
    /// The resulting GetPoxInv, if Some(..), will request a segment of the remote peer's PoX
    /// bitvector starting from target_pox_reward_cycle, and fetching up to GETPOXINV_MAX_BITLEN bits.
    /// The target_pox_reward_cycle will be the _lowest_ reward cycle requested.
    fn make_getpoxinv(&self, sortdb: &SortitionDB, nk: &NeighborKey, target_pox_reward_cycle: u64) -> Result<Option<GetPoxInv>, net_error> {
        if target_pox_reward_cycle >= self.pox_id.len() as u64 {
            debug!("{:?}: target reward cycle for neighbor {:?} is {}, which is higher than our PoX bit vector length {}", &self.local_peer, nk, target_pox_reward_cycle, self.pox_id.len());
            return Ok(None);
        }

        let target_block_height = self.burnchain.reward_cycle_to_block_height(target_pox_reward_cycle);

        let max_reward_cycle = match self.get_convo(nk) {
            Some(convo) => {
                // only proceed if the remote neighbor has this reward cycle
                let tip_height = convo.get_burnchain_tip_height();
                let tip_burn_block_hash = convo.get_burnchain_tip_burn_header_hash();

                debug!("{:?}: chain view of {:?} is ({},{})", &self.local_peer, nk, tip_height, &tip_burn_block_hash);
                
                if target_block_height > tip_height {
                    // this peer is behind us
                    debug!("{:?}: remote neighbor {:?}'s burnchain view tip is {}-{:?}, which is lower than our target reward cycle {} (height {})",
                            &self.local_peer, nk, tip_height, &tip_burn_block_hash, target_pox_reward_cycle, target_block_height);
                    return Ok(None);
                }

                let tip_reward_cycle = match self.burnchain.block_height_to_reward_cycle(tip_height) {
                    Some(tip_rc) => tip_rc,
                    None => {
                        // peer is behind the first block, which should never happen if the peer
                        // is behaving correctly
                        debug!("{:?}: remote neighbor {:?} is behind the first-ever block", &self.local_peer, nk);
                        return Ok(None);
                    }
                };

                let max_reward_cycle = cmp::min(self.pox_id.len() as u64, tip_reward_cycle as u64);
                test_debug!("{:?}: request up to reward cycle min({},{}) = {}", &self.local_peer, self.pox_id.len(), tip_reward_cycle, max_reward_cycle);
                max_reward_cycle
            },
            None => {
                debug!("{:?}: no conversation open for {}", &self.local_peer, nk);
                return Ok(None);
            }
        };

        // ask for all PoX bits in-between target_reward_cyle and highest_reward_cycle, inclusive
        let num_reward_cycles = 
            if target_pox_reward_cycle + GETPOXINV_MAX_BITLEN <= max_reward_cycle {
                GETPOXINV_MAX_BITLEN
            }
            else {
                max_reward_cycle - target_pox_reward_cycle + 1
            };

        if num_reward_cycles == 0 {
            debug!("{:?}: will not send GetPoxInv to {:?}, since we are sync'ed up to its highest reward cycle (our target was {}, our max len is {})", &self.local_peer, nk, target_pox_reward_cycle, self.pox_id.len());
            return Ok(None);
        }
        assert!(num_reward_cycles <= GETPOXINV_MAX_BITLEN);

        // make sure the remote node has the same chain tip view as us
        let ancestor_sn = self.get_ancestor_sortition_snapshot(sortdb, target_block_height)?;
        let getpoxinv = GetPoxInv { consensus_hash: ancestor_sn.consensus_hash, num_cycles: num_reward_cycles as u16 };

        debug!("{:?}: Send GetPoxInv to {:?} for {} rewad cycles starting at {} ({})", &self.local_peer, nk, num_reward_cycles, target_pox_reward_cycle, &getpoxinv.consensus_hash);
        Ok(Some(getpoxinv))
    }

    /// Determine how many blocks to ask for in a GetBlocksInv request to a given neighbor.
    /// Possibly ask for no blocks -- for example, the neighbor may not have sync'ed the burnchain,
    /// or may not agree on our PoX view.
    fn get_getblocksinv_num_blocks(&self, sortdb: &SortitionDB, target_block_reward_cycle: u64, nk: &NeighborKey, stats: &NeighborBlockStats, convo: &ConversationP2P) -> Result<u64, net_error> {
        if target_block_reward_cycle >= self.pox_id.len() as u64 {
            test_debug!("{:?}: target reward cycle {} >= our max reward cycle {}", &self.local_peer, target_block_reward_cycle, self.pox_id.len());
            return Ok(0);
        }

        // does the peer agree with our PoX view up to this reward cycle?
        match stats.inv.pox_inv_cmp(&self.pox_id) {
            Some((disagreed, _, _)) => {
                if disagreed <= target_block_reward_cycle {
                    // can't proceed
                    debug!("{:?}: remote neighbor {:?} disagrees with our PoX inventory at reward cycle {} (asked for {})", &self.local_peer, nk, disagreed, target_block_reward_cycle);
                    return Ok(0);
                }
            },
            None => {}
        }
        
        let target_block_height = self.burnchain.reward_cycle_to_block_height(target_block_reward_cycle);
        let tip_height = convo.get_burnchain_tip_height();
        let tip_burn_block_hash = convo.get_burnchain_tip_burn_header_hash();
        let stable_tip_height = convo.get_stable_burnchain_tip_height();
        let stable_tip_burn_block_hash = convo.get_stable_burnchain_tip_burn_header_hash();

        let my_tip = self.get_tip_sortition_snapshot(sortdb)?;
        
        debug!("{:?}: chain view of {:?} is ({},{})", &self.local_peer, nk, tip_height, &tip_burn_block_hash);

        if target_block_height > tip_height {
            debug!("{:?}: target block height {} for reward cycle {} is higher than {:?}'s highest block {}", &self.local_peer, target_block_height, target_block_reward_cycle, nk, tip_height);
            return Ok(0);
        }

        // maximum burn block height we can ask this neighbor about
        let max_burn_block_height = {
            if my_tip.block_height >= tip_height && self.get_peer_sortition_snapshot(sortdb, &tip_burn_block_hash)?.is_none() {
                // we are at least as far along as the remote peer, but we don't know about this remote peer's burnchain tip
                if self.get_peer_sortition_snapshot(sortdb, &stable_tip_burn_block_hash)?.is_none() {
                    // we don't know about this remote peer's stable burnchain tip either
                    debug!("{:?}: remote neighbor {:?}'s burnchain stable view tip is {}-{:?}, which we do not know", &self.local_peer, nk, stable_tip_height, &stable_tip_burn_block_hash);
                    return Ok(0);
                }
               
                // go with the remote peer's stable burnchain view as our maximum allowed query
                // height
                debug!("{:?}: remote neighbor {:?}'s burnchain view tip is {}-{:?}, which we do not know. Falling back to stable tip {}-{:?}", &self.local_peer, nk, tip_height, &tip_burn_block_hash, stable_tip_height, &stable_tip_burn_block_hash);
                self.chain_view.burn_stable_block_height
            }
            else {
                // remote peer is further ahead than us, so max out at our maximum view
                self.chain_view.burn_block_height
            }
        };
        
        // ask for all blocks in-between target_block_height and max_burn_block_height in this
        // reward cycle, inclusive
        let num_blocks = 
            if target_block_height + (self.burnchain.pox_constants.reward_cycle_length as u64) <= max_burn_block_height {
                self.burnchain.pox_constants.reward_cycle_length as u64
            }
            else {
                max_burn_block_height - target_block_height + 1
            };

        if num_blocks == 0 {
            // target_block_height was higher than the highest known height of the remote node
            debug!("{:?}: will not send GetBlocksInv to {:?}, since we are sync'ed up to its highest sortition block (our target reward cycle was {}, height was {})", &self.local_peer, nk, target_block_reward_cycle, target_block_height);
        }

        Ok(num_blocks)
    }

    /// Make a GetBlocksInv request for a given reward cycle.
    /// Returns Ok(None) if we cannot make a request at this reward cycle (either the remote peer
    /// is too far behind the burnchain tip, or their PoX inventory disagrees with us).
    fn make_getblocksinv(&self, sortdb: &SortitionDB, nk: &NeighborKey, stats: &NeighborBlockStats, target_block_reward_cycle: u64) -> Result<Option<GetBlocksInv>, net_error> {
        let target_block_height = self.burnchain.reward_cycle_to_block_height(target_block_reward_cycle);
        if target_block_height > self.chain_view.burn_block_height {
            debug!("{:?}: target block height for neighbor {:?} is {}, which is higher than our chain view height {}", &self.local_peer, nk, target_block_height, self.chain_view.burn_block_height);
            return Ok(None);
        }

        assert!(target_block_reward_cycle == 0 || self.burnchain.is_reward_cycle_start(target_block_height));

        let num_blocks = match self.get_convo(nk) {
            Some(convo) => match self.get_getblocksinv_num_blocks(sortdb, target_block_reward_cycle, nk, stats, convo)? {
                0 => {
                    // cannot ask this peer for any blocks in this reward cycle
                    return Ok(None)
                },
                x => x
            },
            None => {
                debug!("{:?}: no conversation open for {}", &self.local_peer, nk);
                return Ok(None);
            }
        };

        assert!(num_blocks <= self.burnchain.pox_constants.reward_cycle_length as u64);
        let ancestor_sn = self.get_ancestor_sortition_snapshot(sortdb, target_block_height)?;
        
        debug!("{:?}: Send GetBlocksInv to {:?} for {} blocks at sortition block {} ({})", &self.local_peer, nk, num_blocks, target_block_height, &ancestor_sn.consensus_hash);
        Ok(Some(GetBlocksInv { consensus_hash: ancestor_sn.consensus_hash, num_blocks: num_blocks as u16 }))
    }

    /// Is a peer worth talking to?
    fn is_peer_target(&self, nk: &NeighborKey) -> bool {
        // don't talk to inbound peers; only outbound (and only ones we have the key for)
        // (we make this check each time we begin a round of requests, since the set of
        // available peers can change during this time).
        match self.events.get(nk) {
            Some(event_id) => match self.peers.get(event_id) {
                Some(convo) => {
                    if !convo.is_outbound() {
                        debug!("{:?}: skip {:?}: not outbound", &self.local_peer, convo);
                        return false;
                    }
                    if !convo.is_authenticated() {
                        debug!("{:?}: skip {:?}: not authenticated", &self.local_peer, convo);
                        return false;
                    }
                    return true;
                }
                None => {
                    return false;
                }
            },
            None => {
                return false;
            }
        }
    }
    
    /// Make a possible GetPoxInv request for this neighbor.
    /// Returns Some((target-reward-cycle, getpoxinv-request)) if we are to request a PoX
    /// inventory for this node.
    fn make_next_getpoxinv(&self, sortdb: &SortitionDB, nk: &NeighborKey, stats: &NeighborBlockStats) -> Result<Option<(u64, GetPoxInv)>, net_error> {
        if stats.inv.num_reward_cycles < (self.pox_id.len() as u64) {
            // We don't yet know all of the PoX bits for this node
            debug!("{:?}: PoX inventory not sync'ed with {:?} yet (target {} < our tip {}); make GetPoxInv based at {}", &self.local_peer, nk, stats.inv.num_reward_cycles, self.pox_id.len(), stats.inv.num_reward_cycles);
            match self.make_getpoxinv(sortdb, nk, stats.inv.num_reward_cycles)? {
                Some(request) => Ok(Some((stats.inv.num_reward_cycles, request))),
                None => {
                    debug!("{:?}: will not fetch PoX inventory from {:?} even though target reward cycle {} < our tip {}", &self.local_peer, nk, stats.inv.num_reward_cycles, self.pox_id.len());
                    Ok(None)
                }
            }
        }
        else {
            // We do know all of this node's PoX bits, but proceed to rescan anyway
            debug!("{:?}: PoX inventory sync'ed with {:?}, but rescan at reward cycle {}", &self.local_peer, nk, stats.pox_reward_cycle);
            match self.make_getpoxinv(sortdb, nk, stats.pox_reward_cycle)? {
                Some(request) => Ok(Some((stats.pox_reward_cycle, request))),
                None => {
                    debug!("{:?}: will not fetch PoX inventory from {:?} even though rescan reward cycle {} < our tip {}", &self.local_peer, nk, stats.pox_reward_cycle, self.pox_id.len());
                    Ok(None)
                }
            }
        }
    }
    
    /// Make the next GetBlocksInv for a peer
    fn make_next_getblocksinv(&self, sortdb: &SortitionDB, nk: &NeighborKey, stats: &NeighborBlockStats) -> Result<Option<(u64, GetBlocksInv)>, net_error> {
        if stats.block_reward_cycle < stats.inv.num_reward_cycles {
            self.make_getblocksinv(sortdb, nk, stats, stats.block_reward_cycle)
                .and_then(|getblocksinv_opt| Ok(getblocksinv_opt.map(|getblocksinv| (stats.block_reward_cycle, getblocksinv))))
        }
        else {
            Ok(None)
        }
    }
    
    /// Start requesting the next batch of PoX inventories
    fn inv_getpoxinv_begin(&mut self, sortdb: &SortitionDB, nk: &NeighborKey, stats: &mut NeighborBlockStats, request_timeout: u64) -> Result<(), net_error> {
        let (target_pox_reward_cycle, getpoxinv) = match self.make_next_getpoxinv(sortdb, nk, stats)? {
            Some(x) => x,
            None => {
                // proceed to block scan
                debug!("{:?}: cannot make any more GetPoxInv requests for {:?}; proceeding to block inventory scan", &self.local_peer, nk);
                stats.reset_block_scan(0);
                return Ok(())
            }
        };

        let payload = StacksMessageType::GetPoxInv(getpoxinv);
        let message = self.sign_for_peer(nk, payload)?;
        let request = self.send_message(nk, message, request_timeout)
            .map_err(|e| {
                debug!("Failed to send GetPoxInv to {:?}: {:?}", &nk, &e);
                e
            })?;

        stats.getpoxinv_begin(request, target_pox_reward_cycle);
        Ok(())
    }

    /// Finish requesting the next batch of PoX inventories
    fn inv_getpoxinv_try_finish(&mut self, nk: &NeighborKey, stats: &mut NeighborBlockStats) -> Result<bool, net_error> {
        if stats.done {
            return Ok(true);
        }
        if !stats.getpoxinv_try_finish(self)? {
            // try again
            return Ok(false);
        }

        assert_eq!(stats.state, InvWorkState::Done);

        if !stats.is_peer_online() {
            if stats.status == NodeStatus::Diverged {
                // remote node diverged from this node's view of the burnchain.
                // proceed to block download up to the reward cycles up to this one.
                stats.status = NodeStatus::Online;

                debug!("{:?}: Burnchain/PoX view diverged. Truncate inventories down to reward cycle {} for {:?}", &self.local_peer, stats.target_pox_reward_cycle, nk);
                stats.inv.truncate_pox_inventory(&self.burnchain, stats.target_pox_reward_cycle);
                stats.inv.truncate_block_inventories(&self.burnchain, stats.target_pox_reward_cycle);

                // proceed with block scan
                stats.reset_block_scan(0);
            }
            // done
            return Ok(true);
        }

        let pox_inv = stats.pox_inv.take().expect("BUG: finished getpoxinv without an error but got no poxinv");

        debug!("{:?}: got PoxInv at reward cycle {} from {:?}: {:?}", &self.local_peer, stats.target_pox_reward_cycle, nk, &pox_inv);
        let lowest_learned_reward_cycle = stats.inv.merge_pox_inv(&self.burnchain, stats.target_pox_reward_cycle, pox_inv.bitlen as u64, pox_inv.pox_bitvec.clone(), true);

        if let Some(lowest_learned_reward_cycle) = lowest_learned_reward_cycle.as_ref() {
            debug!("{:?}: {:?} has learned about reward cycle {} (total {} reward cycles): {:?}", 
                   &self.local_peer, &nk, lowest_learned_reward_cycle, stats.inv.num_reward_cycles, &stats.inv);

            stats.learned_data = true;
        }
        else {
            debug!("{:?}: have {} total reward cycles for {:?}", &self.local_peer, stats.inv.num_reward_cycles, nk);
        }

        let remote_uncertain = NeighborBlockStats::check_remote_pox_inv_uncertainty(self, stats.target_pox_reward_cycle, &pox_inv);
        let local_uncertain = NeighborBlockStats::check_local_pox_inv_uncertainty(self, stats.target_pox_reward_cycle, &pox_inv);

        test_debug!("{:?}: PoX bitlen = {}, remote-uncertain: {}, local-uncertain: {}", &self.local_peer, pox_inv.bitlen, remote_uncertain, local_uncertain);

        if stats.target_pox_reward_cycle >= (self.pox_id.len() as u64) ||                                   // did full pass?
           remote_uncertain != (pox_inv.bitlen as u64) + stats.target_pox_reward_cycle ||                   // remote node is less certain than we are?
           local_uncertain != (pox_inv.bitlen as u64) + stats.target_pox_reward_cycle {                     // we are less certain than the remote node?

            if remote_uncertain != (pox_inv.bitlen as u64) + stats.target_pox_reward_cycle || local_uncertain != (pox_inv.bitlen as u64) + stats.target_pox_reward_cycle {
                // finished PoX scan -- we have found uncertainty in our neighbor, or we've
                // fully-synced.
                let minimum_certainty = cmp::min(remote_uncertain, local_uncertain);

                debug!("{:?}: Truncate inventories down to reward cycle {} for {:?}", &self.local_peer, minimum_certainty, nk);
                stats.inv.truncate_pox_inventory(&self.burnchain, minimum_certainty);
                stats.inv.truncate_block_inventories(&self.burnchain, minimum_certainty);
            }
            else {
                debug!("{:?}: Sync'ed PoX inventory with {:?}, and it is equally certain up to reward cycle {}", &self.local_peer, nk, self.pox_id.len());
            }

            // proceed to block scan.
            stats.reset_block_scan(0);
        }
        else {
            // continue with PoX scan.
            stats.pox_reward_cycle += pox_inv.bitlen as u64;
            stats.reset_pox_scan(stats.pox_reward_cycle);
        }

        Ok(true)
    }

    /// Start requesting the next batch of block inventories
    fn inv_getblocksinv_begin(&mut self, sortdb: &SortitionDB, nk: &NeighborKey, stats: &mut NeighborBlockStats, request_timeout: u64) -> Result<(), net_error> {
        let (target_block_reward_cycle, getblocksinv) = match self.make_next_getblocksinv(sortdb, nk, stats)? {
            Some(x) => x,
            None => {
                stats.done = true;
                return Ok(())
            }
        };

        let num_blocks_expected = getblocksinv.num_blocks;
        let payload = StacksMessageType::GetBlocksInv(getblocksinv);
        let message = self.sign_for_peer(nk, payload)?;
        let request = self.send_message(nk, message, request_timeout)
            .map_err(|e| {
                debug!("Failed to send GetPoxInv to {:?}: {:?}", &nk, &e);
                e
            })?;

        stats.getblocksinv_begin(request, target_block_reward_cycle, num_blocks_expected);
        Ok(())
    }

    /// Finish receiving the next batch of block inventories
    fn inv_getblocksinv_try_finish(&mut self, nk: &NeighborKey, stats: &mut NeighborBlockStats) -> Result<bool, net_error> {
        if stats.done {
            return Ok(true);
        }
        if !stats.getblocksinv_try_finish(self)? {
            return Ok(false);
        }
        if !stats.is_peer_online() {
            // done
            return Ok(true);
        }

        // if we get a blocksinv, then it means the remote peer still agrees with us on PoX state
        // (otherwise we would have been NACK'ed, and the peer would not be considered online)
        let blocks_inv = stats.blocks_inv.take().expect("BUG: finished getblocksinv without an error but got no blocksinv");
        let target_block_height = self.burnchain.reward_cycle_to_block_height(stats.target_block_reward_cycle);

        debug!("{:?}: got blocksinv at reward cycle {} (block height {}) from {:?}: {:?}", &self.local_peer, stats.target_block_reward_cycle, target_block_height, nk, &blocks_inv);
        let (new_blocks, new_microblocks) = stats.inv.merge_blocks_inv(target_block_height, blocks_inv.bitlen as u64, blocks_inv.block_bitvec, blocks_inv.microblocks_bitvec, true);

        debug!("{:?}: {:?} has {} new blocks and {} new microblocks (total {} blocks, {} microblocks, {} sortitions): {:?}", 
               &self.local_peer, &nk, new_blocks, new_microblocks, stats.inv.num_blocks(), stats.inv.num_microblock_streams(), stats.inv.num_sortitions, &stats.inv);

        if new_blocks > 0 || new_microblocks > 0 {
            stats.learned_data = true;
        }

        assert_eq!(stats.state, InvWorkState::Done);

        if stats.target_block_reward_cycle < (self.pox_id.len() as u64) {
            // ask for more blocks
            stats.block_reward_cycle += 1;
            stats.reset_block_scan(stats.block_reward_cycle);
        }
        else {
            // we're done scanning!  proceed to rescan
            stats.last_rescan_timestamp = get_epoch_time_secs();
            stats.done = true;
        }

        Ok(true)
    }

    /// Run a single state-machine to completion
    fn inv_sync_run(&mut self, sortdb: &SortitionDB, nk: &NeighborKey, stats: &mut NeighborBlockStats, request_timeout: u64) -> Result<bool, net_error> {
        while !stats.done {
            if !stats.is_peer_online() {
                stats.done = true;
                break;
            }

            test_debug!("{:?}: inv state-machine for {:?} is in state {:?}", &self.local_peer, nk, &stats.state);

            let again = match stats.state {
                InvWorkState::GetPoxInvBegin => {
                    self.inv_getpoxinv_begin(sortdb, nk, stats, request_timeout)
                        .and_then(|_| Ok(true))?
                },
                InvWorkState::GetPoxInvFinish => {
                    self.inv_getpoxinv_try_finish(nk, stats)?
                },
                InvWorkState::GetBlocksInvBegin => {
                    self.inv_getblocksinv_begin(sortdb, nk, stats, request_timeout)
                        .and_then(|_| Ok(true))?
                },
                InvWorkState::GetBlocksInvFinish => {
                    self.inv_getblocksinv_try_finish(nk, stats)?
                },
                InvWorkState::Done => {
                    false
                }
            };
            if !again {
                break;
            }
        }
        Ok(stats.done)
    }

    /// Refresh our cached PoX bitvector, and invalidate any PoX state if we have since learned
    /// about a new reward cycle
    pub fn refresh_sortition_view(&mut self, sortdb: &SortitionDB) -> Result<(), net_error> {
        if self.inv_state.is_none() {
            self.init_inv_sync(sortdb);
        }

        let inv_state = self.inv_state.as_mut().expect("Unreachable: inv state not initialized");

        let (new_tip_sort_id, new_pox_id) = {
            let ic = sortdb.index_conn();
            let tip_sort_id = SortitionDB::get_canonical_sortition_tip(sortdb.conn())?;
            let sortdb_reader = SortitionHandleConn::open_reader(&ic, &tip_sort_id)?;
            (tip_sort_id, sortdb_reader.get_pox_id()?)
        };

        // find the lowest reward cycle whose bit has since changed from a 0 to a 1.
        let num_reward_cycles = cmp::min(new_pox_id.len(), self.pox_id.len());
        for i in 0..num_reward_cycles {
            if !self.pox_id.has_ith_anchor_block(i) && new_pox_id.has_ith_anchor_block(i) {
                // we learned of a new anchor block intermittently.  Invalidate all cached state at and after this reward cycle.
                inv_state.invalidate_block_inventories(&self.burnchain, i as u64);
                
                // also clear block header cache (TODO: this is pessimistic -- only invalidated
                // entries need to be cleared)
                debug!("{:?}: invalidating block header cache in response to PoX bit flip", &self.local_peer);
                self.header_cache.clear();
                break;
            }
        }

        // if the PoX bitvector shrinks, then invalidate block inventories that are no longer represented
        if new_pox_id.len() < self.pox_id.len() {
            inv_state.invalidate_block_inventories(&self.burnchain, self.pox_id.len() as u64);
        }

        self.tip_sort_id = new_tip_sort_id;
        self.pox_id = new_pox_id;

        test_debug!("{:?}: PoX bit vector is {:?}", &self.local_peer, &self.pox_id);

        Ok(())
    }

    /// Drive all state machines.
    /// returns (done?, peers-to-disconnect, peers-that-are-dead)
    pub fn sync_inventories(&mut self, sortdb: &SortitionDB) -> Result<(bool, Vec<NeighborKey>, Vec<NeighborKey>), net_error> {
        PeerNetwork::with_inv_state(self, |network, inv_state| {
            let mut all_done = true;

            if !inv_state.hint_do_full_rescan && !inv_state.hint_learned_data && inv_state.last_rescanned_at + inv_state.sync_interval >= get_epoch_time_secs() {
                // we didn't learn anything on the last sync, and it hasn't been enough time
                // since the last sync for us to do it again
                debug!("{:?}: Throttle inv sync until {}s", &network.local_peer, inv_state.last_rescanned_at + inv_state.sync_interval);
                return Ok((true, vec![], vec![]));
            }

            for (nk, stats) in inv_state.block_stats.iter_mut() {
                if !stats.done {
                    let done = match network.inv_sync_run(sortdb, nk, stats, inv_state.request_timeout) {
                        Ok(d) => d,
                        Err(net_error::StaleView) => {
                            // stop work on this state machine -- it needs to be restarted.
                            // we'll need to keep scanning.
                            debug!("{:?}: stale PoX view; will rescan", &network.local_peer);
                            stats.done = true;
                            inv_state.hint_learned_data = true;
                            true
                        }
                        Err(net_error::PeerNotConnected) => {
                            stats.status = NodeStatus::Dead;
                            true
                        },
                        Err(e) => {
                            info!("{:?}: remote neighbor inv_sync_run finished with error {:?}", &network.local_peer, &e);
                            stats.status = NodeStatus::Broken;
                            true
                        }
                    };
                    all_done = done && all_done;

                    if stats.learned_data {
                        // update hints
                        inv_state.hint_learned_data = inv_state.hint_learned_data || stats.learned_data;
                        inv_state.last_change_at = get_epoch_time_secs();
                    }
                }
            }

            if all_done {
                let new_sync_peers = network.get_outbound_sync_peers();
                let broken_peers = inv_state.get_broken_peers();
                let dead_peers = inv_state.get_dead_peers();

                if !inv_state.hint_learned_data && inv_state.block_stats.len() > 0 {
                    // did a full scan without learning anything new
                    inv_state.last_rescanned_at = get_epoch_time_secs();
                    inv_state.hint_do_full_rescan = false;
                
                    debug!("{:?}: inv sync finished, learned nothing new from {:?} neighbor(s)", &network.local_peer, &inv_state.block_stats.len());
                }
                else {
                    // keep learning
                    inv_state.hint_learned_data = false;
                    inv_state.hint_do_full_rescan = true;
                    
                    debug!("{:?}: inv sync finished, learned something new (or have no peers)", &network.local_peer);
                }

                inv_state.cull_bad_peers();
                inv_state.reset_sync_peers(new_sync_peers);

                Ok((true, broken_peers, dead_peers))
            }
            else {
                Ok((false, vec![], vec![]))
            }
        })
    }

    pub fn with_inv_state<F, R>(network: &mut PeerNetwork, handler: F) -> Result<R, net_error> 
    where
        F: FnOnce(&mut PeerNetwork, &mut InvState) -> Result<R, net_error>
    {
        let mut inv_state = network.inv_state.take();
        let res = match inv_state {
            None => {
                test_debug!("{:?}: inv state not connected", &network.local_peer);
                Err(net_error::NotConnected)
            },
            Some(ref mut invs) => handler(network, invs)
        };
        network.inv_state = inv_state;
        res
    }

    /// Get the list of outbound neighbors we can sync with 
    fn get_outbound_sync_peers(&self) -> HashSet<NeighborKey> {
        let mut cur_neighbors = HashSet::new();
        for (nk, event_id) in self.events.iter() {
            // only outbound authenticated peers
            match self.peers.get(event_id) {
                Some(convo) => {
                    if convo.is_outbound() && convo.is_authenticated() {
                        cur_neighbors.insert(nk.clone());
                    }
                },
                None => {}
            }
        }
        cur_neighbors
    }

    /// Set a hint that we learned something new, and need to sync invs again
    pub fn hint_sync_invs(&mut self) {
        match self.inv_state {
            Some(ref mut inv_state) => {
                debug!("Awaken inv sync to re-scan peer block inventories");
                inv_state.hint_learned_data = true;
                inv_state.hint_do_full_rescan = true;
            },
            None => {}
        }
    }
    
    /// Initialize inv state
    pub fn init_inv_sync(&mut self, sortdb: &SortitionDB) -> () {
        // find out who we'll be synchronizing with for the duration of this inv sync
        let cur_neighbors = self.get_outbound_sync_peers();
        
        debug!("{:?}: Initializing peer block inventory state with {} neighbors", &self.local_peer, cur_neighbors.len());
        self.inv_state = Some(InvState::new(sortdb.first_block_height, self.connection_opts.timeout, self.connection_opts.inv_sync_interval, cur_neighbors));
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use net::*;
    use net::test::*;
    use chainstate::stacks::*;
    use burnchains::PoxConstants;
    use std::collections::HashMap;
    use util::test::*;

    #[test]
    fn peerblocksinv_has_ith_block() {
        let peer_inv = PeerBlocksInv::new(vec![0x55, 0x77], vec![0x11, 0x22], vec![0x01], 16, 1, 12345);
        let has_blocks = vec![
            true,
            false,
            true,
            false,

            true,
            false,
            true,
            false,

            true,
            true,
            true,
            false,

            true,
            true,
            true,
            false
        ];
        let has_microblocks = vec![
            true,
            false,
            false,
            false,

            true,
            false,
            false,
            false,

            false,
            true,
            false,
            false,

            false,
            true,
            false,
            false
        ];

        assert!(!peer_inv.has_ith_block(12344));
        assert!(!peer_inv.has_ith_block(12345 + 17));
        
        assert!(!peer_inv.has_ith_microblock_stream(12344));
        assert!(!peer_inv.has_ith_microblock_stream(12345 + 17));

        for i in 0..16 {
            assert_eq!(has_blocks[i], peer_inv.has_ith_block((12345 + i) as u64));
            assert_eq!(has_microblocks[i], peer_inv.has_ith_microblock_stream((12345 + i) as u64));
        }
    }

    #[test]
    fn peerblocksinv_merge() {
        let peer_inv = PeerBlocksInv::new(vec![0x00, 0x00, 0x55, 0x77], vec![0x00, 0x00, 0x55, 0x77], vec![0x01], 32, 1, 12345);
        
        // merge below, aligned
        let mut peer_inv_below = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_below.merge_blocks_inv(12345, 16, vec![0x11, 0x22], vec![0x11, 0x22], false);
        assert_eq!(new_blocks, 4);
        assert_eq!(new_microblocks, 4);
        assert_eq!(peer_inv_below.num_sortitions, 32);
        assert_eq!(peer_inv_below.block_inv, vec![0x11, 0x22, 0x55, 0x77]);
        assert_eq!(peer_inv_below.microblocks_inv, vec![0x11, 0x22, 0x55, 0x77]);

        // merge below, overlapping, aligned
        let mut peer_inv_below_overlap = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_below_overlap.merge_blocks_inv(12345 + 8, 16, vec![0x11, 0x22], vec![0x11, 0x22], false);
        assert_eq!(new_blocks, 4);
        assert_eq!(new_microblocks, 4);
        assert_eq!(peer_inv_below_overlap.num_sortitions, 32);
        assert_eq!(peer_inv_below_overlap.block_inv, vec![0x00, 0x11, 0x22 | 0x55, 0x77]);
        assert_eq!(peer_inv_below_overlap.microblocks_inv, vec![0x00, 0x11, 0x22 | 0x55, 0x77]);

        // merge equal, overlapping, aligned
        let mut peer_inv_equal = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_equal.merge_blocks_inv(12345 + 16, 16, vec![0x11, 0x22], vec![0x11, 0x22], false);
        assert_eq!(new_blocks, 0);
        assert_eq!(new_microblocks, 0);
        assert_eq!(peer_inv_equal.num_sortitions, 32);
        assert_eq!(peer_inv_equal.block_inv, vec![0x00, 0x00, 0x11 | 0x55, 0x22 | 0x77]);
        assert_eq!(peer_inv_equal.microblocks_inv, vec![0x00, 0x00, 0x11 | 0x55, 0x22 | 0x77]);

        // merge above, overlapping, aligned
        let mut peer_inv_above_overlap = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_above_overlap.merge_blocks_inv(12345 + 24, 16, vec![0x11, 0x22], vec![0x11, 0x22], false);
        assert_eq!(new_blocks, 2);
        assert_eq!(new_microblocks, 2);
        assert_eq!(peer_inv_above_overlap.num_sortitions, 40);
        assert_eq!(peer_inv_above_overlap.block_inv, vec![0x00, 0x00, 0x55, 0x77 | 0x11, 0x22]);
        assert_eq!(peer_inv_above_overlap.microblocks_inv, vec![0x00, 0x00, 0x55, 0x77 | 0x11, 0x22]);

        // merge above, non-overlapping, aligned
        let mut peer_inv_above = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_above.merge_blocks_inv(12345 + 32, 16, vec![0x11, 0x22], vec![0x11, 0x22], false);
        assert_eq!(peer_inv_above.num_sortitions, 48);
        assert_eq!(new_blocks, 4);
        assert_eq!(new_microblocks, 4);
        assert_eq!(peer_inv_above.block_inv, vec![0x00, 0x00, 0x55, 0x77, 0x11, 0x22]);
        assert_eq!(peer_inv_above.microblocks_inv, vec![0x00, 0x00, 0x55, 0x77, 0x11, 0x22]);

        // try merging unaligned
        let mut peer_inv = PeerBlocksInv::new(vec![0x00, 0x00, 0x00, 0x00], vec![0x00, 0x00, 0x00, 0x00], vec![0x01], 32, 1, 12345);
        for i in 0..32 {
            let (new_blocks, new_microblocks) = peer_inv.merge_blocks_inv(12345 + i, 1, vec![0x01], vec![0x01], false);
            assert_eq!(new_blocks, 1);
            assert_eq!(new_microblocks, 1);
            assert_eq!(peer_inv.num_sortitions, 32);
            for j in 0..i+1 {
                assert!(peer_inv.has_ith_block(12345 + j));
                assert!(peer_inv.has_ith_microblock_stream(12345 + j));
            }
            for j in i+1..32 {
                assert!(!peer_inv.has_ith_block(12345 + j));
                assert!(!peer_inv.has_ith_microblock_stream(12345 + j));
            }
        }
        
        // try merging unaligned, with multiple blocks
        let mut peer_inv = PeerBlocksInv::new(vec![0x00, 0x00, 0x00, 0x00], vec![0x00, 0x00, 0x00, 0x00], vec![0x01], 32, 1, 12345);
        for i in 0..16 {
            let (new_blocks, new_microblocks) = peer_inv.merge_blocks_inv(12345 + i, 32, vec![0x01, 0x00, 0x01, 0x00], vec![0x01, 0x00, 0x01, 0x00], false);
            assert_eq!(new_blocks, 2);
            assert_eq!(new_microblocks, 2);
            assert_eq!(peer_inv.num_sortitions, 32 + i);
            for j in 0..i+1 {
                assert!(peer_inv.has_ith_block(12345 + j));
                assert!(peer_inv.has_ith_block(12345 + j + 16));
                
                assert!(peer_inv.has_ith_microblock_stream(12345 + j));
                assert!(peer_inv.has_ith_microblock_stream(12345 + j + 16));
            }
            for j in i+1..16 {
                assert!(!peer_inv.has_ith_block(12345 + j));
                assert!(!peer_inv.has_ith_block(12345 + j + 16));
                
                assert!(!peer_inv.has_ith_microblock_stream(12345 + j));
                assert!(!peer_inv.has_ith_microblock_stream(12345 + j + 16));
            }
        }

        // merge 0's grows the bitvec
        let mut peer_inv = PeerBlocksInv::new(vec![0x00, 0x00, 0x00, 0x00], vec![0x00, 0x00, 0x00, 0x00], vec![0x01], 32, 1, 12345);
        let (new_blocks, new_microblocks) = peer_inv.merge_blocks_inv(12345 + 24, 16, vec![0x00, 0x00], vec![0x00, 0x00], false);
        assert_eq!(new_blocks, 0);
        assert_eq!(new_microblocks, 0);
        assert_eq!(peer_inv.num_sortitions, 40);
        assert_eq!(peer_inv.block_inv, vec![0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(peer_inv.microblocks_inv, vec![0x00, 0x00, 0x00, 0x00, 0x00]);
    }
    
    #[test]
    fn peerblocksinv_merge_clear_bits() {
        let peer_inv = PeerBlocksInv::new(vec![0x00, 0x00, 0x55, 0x77], vec![0x00, 0x00, 0x55, 0x77], vec![0x01], 32, 1, 12345);
        
        // merge below, aligned
        let mut peer_inv_below = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_below.merge_blocks_inv(12345, 16, vec![0x11, 0x22], vec![0x11, 0x22], true);
        assert_eq!(new_blocks, 4);
        assert_eq!(new_microblocks, 4);
        assert_eq!(peer_inv_below.num_sortitions, 32);
        assert_eq!(peer_inv_below.block_inv, vec![0x11, 0x22, 0x55, 0x77]);
        assert_eq!(peer_inv_below.microblocks_inv, vec![0x11, 0x22, 0x55, 0x77]);

        // merge below, overlapping, aligned
        let mut peer_inv_below_overlap = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_below_overlap.merge_blocks_inv(12345 + 8, 16, vec![0x11, 0x22], vec![0x11, 0x22], true);
        assert_eq!(new_blocks, 4);
        assert_eq!(new_microblocks, 4);
        assert_eq!(peer_inv_below_overlap.num_sortitions, 32);
        assert_eq!(peer_inv_below_overlap.block_inv, vec![0x00, 0x11, 0x22, 0x77]);
        assert_eq!(peer_inv_below_overlap.microblocks_inv, vec![0x00, 0x11, 0x22, 0x77]);

        // merge equal, overlapping, aligned
        let mut peer_inv_equal = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_equal.merge_blocks_inv(12345 + 16, 16, vec![0x11, 0x22], vec![0x11, 0x22], true);
        assert_eq!(new_blocks, 0);
        assert_eq!(new_microblocks, 0);
        assert_eq!(peer_inv_equal.num_sortitions, 32);
        assert_eq!(peer_inv_equal.block_inv, vec![0x00, 0x00, 0x11, 0x22]);
        assert_eq!(peer_inv_equal.microblocks_inv, vec![0x00, 0x00, 0x11, 0x22]);

        // merge above, overlapping, aligned
        let mut peer_inv_above_overlap = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_above_overlap.merge_blocks_inv(12345 + 24, 16, vec![0x11, 0x22], vec![0x11, 0x22], true);
        assert_eq!(new_blocks, 2);
        assert_eq!(new_microblocks, 2);
        assert_eq!(peer_inv_above_overlap.num_sortitions, 40);
        assert_eq!(peer_inv_above_overlap.block_inv, vec![0x00, 0x00, 0x55, 0x11, 0x22]);
        assert_eq!(peer_inv_above_overlap.microblocks_inv, vec![0x00, 0x00, 0x55, 0x11, 0x22]);

        // merge above, non-overlapping, aligned
        let mut peer_inv_above = peer_inv.clone();
        let (new_blocks, new_microblocks) = peer_inv_above.merge_blocks_inv(12345 + 32, 16, vec![0x11, 0x22], vec![0x11, 0x22], true);
        assert_eq!(peer_inv_above.num_sortitions, 48);
        assert_eq!(new_blocks, 4);
        assert_eq!(new_microblocks, 4);
        assert_eq!(peer_inv_above.block_inv, vec![0x00, 0x00, 0x55, 0x77, 0x11, 0x22]);
        assert_eq!(peer_inv_above.microblocks_inv, vec![0x00, 0x00, 0x55, 0x77, 0x11, 0x22]);

        // try merging unaligned
        let mut peer_inv = PeerBlocksInv::new(vec![0x00, 0x00, 0x00, 0x00], vec![0x00, 0x00, 0x00, 0x00], vec![0x01], 32, 1, 12345);
        for i in 0..32 {
            let (new_blocks, new_microblocks) = peer_inv.merge_blocks_inv(12345 + i, 1, vec![0x01], vec![0x01], true);
            assert_eq!(new_blocks, 1);
            assert_eq!(new_microblocks, 1);
            assert_eq!(peer_inv.num_sortitions, 32);
            for j in 0..i+1 {
                assert!(peer_inv.has_ith_block(12345 + j));
                assert!(peer_inv.has_ith_microblock_stream(12345 + j));
            }
            for j in i+1..32 {
                assert!(!peer_inv.has_ith_block(12345 + j));
                assert!(!peer_inv.has_ith_microblock_stream(12345 + j));
            }
        }
        
        // try merging unaligned, with multiple blocks
        let mut peer_inv = PeerBlocksInv::new(vec![0x00, 0x00, 0x00, 0x00], vec![0x00, 0x00, 0x00, 0x00], vec![0x01], 32, 1, 12345);
        for i in 0..16 {
            let (new_blocks, new_microblocks) = peer_inv.merge_blocks_inv(12345 + i, 32, vec![0x01, 0x00, 0x01, 0x00], vec![0x01, 0x00, 0x01, 0x00], true);
            assert_eq!(new_blocks, 2);
            assert_eq!(new_microblocks, 2);
            assert_eq!(peer_inv.num_sortitions, 32 + i);
            for j in 0..i {
                assert!(peer_inv.has_ith_block(12345 + j));
                assert!(!peer_inv.has_ith_block(12345 + j + 16));
                
                assert!(peer_inv.has_ith_microblock_stream(12345 + j));
                assert!(!peer_inv.has_ith_microblock_stream(12345 + j + 16));
            }

            assert!(peer_inv.has_ith_block(12345 + i));
            assert!(peer_inv.has_ith_block(12345 + i + 16));
            
            assert!(peer_inv.has_ith_microblock_stream(12345 + i));
            assert!(peer_inv.has_ith_microblock_stream(12345 + i + 16));

            for j in i+1..16 {
                assert!(!peer_inv.has_ith_block(12345 + j));
                assert!(!peer_inv.has_ith_block(12345 + j + 16));
                
                assert!(!peer_inv.has_ith_microblock_stream(12345 + j));
                assert!(!peer_inv.has_ith_microblock_stream(12345 + j + 16));
            }
        }

        // merge 0's grows the bitvec
        let mut peer_inv = PeerBlocksInv::new(vec![0x00, 0x00, 0x00, 0x00], vec![0x00, 0x00, 0x00, 0x00], vec![0x01], 32, 1, 12345);
        let (new_blocks, new_microblocks) = peer_inv.merge_blocks_inv(12345 + 24, 16, vec![0x00, 0x00], vec![0x00, 0x00], true);
        assert_eq!(new_blocks, 0);
        assert_eq!(new_microblocks, 0);
        assert_eq!(peer_inv.num_sortitions, 40);
        assert_eq!(peer_inv.block_inv, vec![0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(peer_inv.microblocks_inv, vec![0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_inv_set_block_microblock_bits() {
        let mut peer_inv = PeerBlocksInv::new(vec![0x01], vec![0x01], vec![0x01], 1, 1, 12345);

        assert!(peer_inv.set_block_bit(12345 + 1));
        assert_eq!(peer_inv.block_inv, vec![0x03]);
        assert_eq!(peer_inv.num_sortitions, 2);
        assert!(!peer_inv.set_block_bit(12345 + 1));
        assert_eq!(peer_inv.block_inv, vec![0x03]);
        assert_eq!(peer_inv.num_sortitions, 2);

        assert!(peer_inv.set_microblocks_bit(12345 + 1));
        assert_eq!(peer_inv.microblocks_inv, vec![0x03]);
        assert_eq!(peer_inv.num_sortitions, 2);
        assert!(!peer_inv.set_microblocks_bit(12345 + 1));
        assert_eq!(peer_inv.microblocks_inv, vec![0x03]);
        assert_eq!(peer_inv.num_sortitions, 2);

        assert!(peer_inv.set_block_bit(12345 + 1 + 16));
        assert_eq!(peer_inv.block_inv, vec![0x03, 0x00, 0x02]);
        assert_eq!(peer_inv.microblocks_inv, vec![0x03, 0x00, 0x00]);
        assert_eq!(peer_inv.num_sortitions, 18);
        assert!(!peer_inv.set_block_bit(12345 + 1 + 16));
        assert_eq!(peer_inv.block_inv, vec![0x03, 0x00, 0x02]);
        assert_eq!(peer_inv.microblocks_inv, vec![0x03, 0x00, 0x00]);
        assert_eq!(peer_inv.num_sortitions, 18);
        
        assert!(peer_inv.set_microblocks_bit(12345 + 1 + 32));
        assert_eq!(peer_inv.block_inv, vec![0x03, 0x00, 0x02, 0x00, 0x00]);
        assert_eq!(peer_inv.microblocks_inv, vec![0x03, 0x00, 0x00, 0x00, 0x02]);
        assert_eq!(peer_inv.num_sortitions, 34);
        assert!(!peer_inv.set_microblocks_bit(12345 + 1 + 32));
        assert_eq!(peer_inv.block_inv, vec![0x03, 0x00, 0x02, 0x00, 0x00]);
        assert_eq!(peer_inv.microblocks_inv, vec![0x03, 0x00, 0x00, 0x00, 0x02]);
        assert_eq!(peer_inv.num_sortitions, 34);
    }

    #[test]
    fn test_inv_merge_pox_inv() {
        let mut burnchain = Burnchain::new("unused", "bitcoin", "regtest").unwrap();
        burnchain.pox_constants = PoxConstants::new(5, 3, 3, 25);

        let mut peer_inv = PeerBlocksInv::new(vec![0x01], vec![0x01], vec![0x01], 1, 1, 0);
        for i in 0..32 {
            let bit_flipped = peer_inv.merge_pox_inv(&burnchain, i+1, 1, vec![0x01], false).unwrap();
            assert_eq!(bit_flipped, i+1);
            assert_eq!(peer_inv.num_reward_cycles, i+2);
        }

        assert_eq!(peer_inv.pox_inv, vec![0xff, 0xff, 0xff, 0xff, 0x01]);
        assert_eq!(peer_inv.num_reward_cycles, 33);
    }

    #[test]
    fn test_inv_truncate_pox_inv() {
        let mut burnchain = Burnchain::new("unused", "bitcoin", "regtest").unwrap();
        burnchain.pox_constants = PoxConstants::new(5, 3, 3, 25);
        
        let mut peer_inv = PeerBlocksInv::new(vec![0x01], vec![0x01], vec![0x01], 1, 1, 0);
        for i in 0..5 {
            let bit_flipped_opt = peer_inv.merge_pox_inv(&burnchain, i+1, 1, vec![0x00], false);
            assert!(bit_flipped_opt.is_none());
            assert_eq!(peer_inv.num_reward_cycles, i+2);
        }
        
        assert_eq!(peer_inv.pox_inv, vec![0x01]);  // 0000 0001
        assert_eq!(peer_inv.num_reward_cycles, 6);

        for i in 0..(6 * burnchain.pox_constants.reward_cycle_length) {
            peer_inv.set_block_bit(i as u64);
            peer_inv.set_microblocks_bit(i as u64);
        }

        // 30 bits set, since the reward cycle is 5 blocks long
        assert_eq!(peer_inv.block_inv, vec![0xff, 0xff, 0xff, 0x3f]);
        assert_eq!(peer_inv.microblocks_inv, vec![0xff, 0xff, 0xff, 0x3f]);
        assert_eq!(peer_inv.num_sortitions, (6 * burnchain.pox_constants.reward_cycle_length) as u64);
       
        // PoX bit 3 flipped
        let bit_flipped = peer_inv.merge_pox_inv(&burnchain, 3, 1, vec![0x01], false).unwrap();
        assert_eq!(bit_flipped, 3);

        assert_eq!(peer_inv.pox_inv, vec![0x9]);  // 0000 1001
        assert_eq!(peer_inv.num_reward_cycles, 6);

        // truncate happened -- only reward cycles 0, 1, and 2 remain (3 * 5 = 15 bits)
        // BUT: reward cycles start on the _first_ block, so the first bit doesn't count!
        // The expected bit vector (grouped by reward cycle) is actually 1 11111 11111 11111.
        assert_eq!(peer_inv.block_inv, vec![0xff, 0xff, 0x00, 0x00]);
        assert_eq!(peer_inv.microblocks_inv, vec![0xff, 0xff, 0x00, 0x00]);
        assert_eq!(peer_inv.num_sortitions, (3 * burnchain.pox_constants.reward_cycle_length + 1) as u64);
    }

    #[test]
    fn test_sync_inv_set_blocks_microblocks_available() {
        let mut peer_1_config = TestPeerConfig::new("test_sync_inv_set_blocks_microblocks_available", 31981, 41981);
        let mut peer_2_config = TestPeerConfig::new("test_sync_inv_set_blocks_microblocks_available", 31982, 41982);

        peer_1_config.burnchain.first_block_height = 5;
        peer_2_config.burnchain.first_block_height = 5;

        let mut peer_1 = TestPeer::new(peer_1_config);
        let mut peer_2 = TestPeer::new(peer_2_config);

        let num_blocks = 5;
        let first_stacks_block_height = {
            let sn = SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
            sn.block_height
        };

        for i in 0..num_blocks {
            let (burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

            peer_1.next_burnchain_block(burn_ops.clone());
            peer_2.next_burnchain_block(burn_ops.clone());
            peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
        }

        let (tip, num_burn_blocks) = {
            let sn = SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
            let num_burn_blocks = sn.block_height - peer_1.config.burnchain.first_block_height;
            (sn, num_burn_blocks)
        };

        let nk = peer_1.to_neighbor().addr;

        let sortdb = peer_1.sortdb.take().unwrap();
        peer_1.network.init_inv_sync(&sortdb);
        match peer_1.network.inv_state {
            Some(ref mut inv) => {
                inv.add_peer(nk.clone());
            },
            None => {
                panic!("No inv state");
            }
        };
        peer_1.sortdb = Some(sortdb);
        
        for i in 0..num_blocks {
            let sortdb = peer_1.sortdb.take().unwrap();
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, i + 1 + first_stacks_block_height, &tip.sortition_id).unwrap().unwrap();
                eprintln!("{:?}", &sn);
                sn
            };
            peer_1.sortdb = Some(sortdb);
        }

        for i in 0..num_blocks {
            let sortdb = peer_1.sortdb.take().unwrap();
            match peer_1.network.inv_state {
                Some(ref mut inv) => {
                    assert!(!inv.block_stats.get(&nk).unwrap().inv.has_ith_block(i + first_stacks_block_height));
                    assert!(!inv.block_stats.get(&nk).unwrap().inv.has_ith_microblock_stream(i + first_stacks_block_height));

                    let sn = {
                        let ic = sortdb.index_conn();
                        let sn = SortitionDB::get_ancestor_snapshot(&ic, i + first_stacks_block_height + 1, &tip.sortition_id).unwrap().unwrap();
                        eprintln!("{:?}", &sn);
                        sn
                    };
                   
                    // non-existent consensus hash
                    let sh = inv.set_block_available(&nk, &sortdb, &ConsensusHash([0xfe; 20])).unwrap();
                    assert_eq!(None, sh);
                    assert!(!inv.block_stats.get(&nk).unwrap().inv.has_ith_block(i + first_stacks_block_height));
                    assert!(!inv.block_stats.get(&nk).unwrap().inv.has_ith_microblock_stream(i + first_stacks_block_height));
                    
                    // existing consensus hash
                    let sh = inv.set_block_available(&nk, &sortdb, &sn.consensus_hash).unwrap();

                    assert_eq!(Some(i + first_stacks_block_height - sortdb.first_block_height), sh);
                    assert!(inv.block_stats.get(&nk).unwrap().inv.has_ith_block(i + first_stacks_block_height));
                    
                    // idempotent
                    let sh = inv.set_microblocks_available(&nk, &sortdb, &sn.consensus_hash).unwrap();

                    assert_eq!(Some(i + first_stacks_block_height - sortdb.first_block_height), sh);
                    assert!(inv.block_stats.get(&nk).unwrap().inv.has_ith_microblock_stream(i + first_stacks_block_height));

                    assert!(inv.set_block_available(&nk, &sortdb, &sn.consensus_hash).unwrap().is_none());
                    assert!(inv.set_microblocks_available(&nk, &sortdb, &sn.consensus_hash).unwrap().is_none());
                },
                None => {
                    panic!("No inv state");
                }
            }
            peer_1.sortdb = Some(sortdb);
        }
    }
    
    #[test]
    fn test_sync_inv_make_inv_messages() {
        let peer_1_config = TestPeerConfig::new("test_sync_inv_make_inv_messages", 31985, 41986);
        
        let reward_cycle_length = peer_1_config.burnchain.pox_constants.reward_cycle_length;
        let num_blocks = peer_1_config.burnchain.pox_constants.reward_cycle_length * 2;

        assert_eq!(reward_cycle_length, 5);

        let mut peer_1 = TestPeer::new(peer_1_config);

        let first_stacks_block_height = {
            let sn = SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
            sn.block_height
        };

        for i in 0..num_blocks {
            let (burn_ops, stacks_block, microblocks) = peer_1.make_default_tenure();

            peer_1.next_burnchain_block(burn_ops.clone());
            peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
        }

        let (tip, num_burn_blocks) = {
            let sn = SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
            let num_burn_blocks = sn.block_height - peer_1.config.burnchain.first_block_height;
            (sn, num_burn_blocks)
        };

        peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            network.refresh_local_peer().unwrap();
            network.refresh_burnchain_view(sortdb).unwrap();
            network.refresh_sortition_view(sortdb).unwrap();
            Ok(())
        }).unwrap();

        // simulate a getpoxinv / poxinv for one reward cycle
        let getpoxinv_request = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(1);
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id).unwrap().unwrap();
                sn
            };
            let getpoxinv = GetPoxInv { consensus_hash: sn.consensus_hash, num_cycles: 1 };
            Ok(getpoxinv)
        }).unwrap();

        test_debug!("\n\nSend {:?}\n\n", &getpoxinv_request);

        let reply = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getpoxinv_response(&network.local_peer, &network.burnchain, sortdb, &network.pox_id, &getpoxinv_request)
        }).unwrap();

        test_debug!("\n\nReply {:?}\n\n", &reply);

        match reply {
            StacksMessageType::PoxInv(poxinv) => {
                assert_eq!(poxinv.bitlen, 1);
                assert_eq!(poxinv.pox_bitvec, vec![0x01]);
            },
            x => {
                error!("Did not get PoxInv, but got {:?}", &x);
                assert!(false);
            }
        }

        // simulate a getpoxinv / poxinv for several reward cycles, including more than we have
        // (10, but only have 7)
        let getpoxinv_request = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(1);
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id).unwrap().unwrap();
                sn
            };
            let getpoxinv = GetPoxInv { consensus_hash: sn.consensus_hash, num_cycles: 10 };
            Ok(getpoxinv)
        }).unwrap();

        test_debug!("\n\nSend {:?}\n\n", &getpoxinv_request);

        let reply = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getpoxinv_response(&network.local_peer, &network.burnchain, sortdb, &network.pox_id, &getpoxinv_request)
        }).unwrap();

        test_debug!("\n\nReply {:?}\n\n", &reply);

        match reply {
            StacksMessageType::PoxInv(poxinv) => {
                assert_eq!(poxinv.bitlen, 6);                   // 2 reward cycles we generated, plus 5 reward cycles when booted up (1 reward cycle = 5 blocks).  1st one is free
                assert_eq!(poxinv.pox_bitvec, vec![0x3f]);
            },
            x => {
                error!("Did not get PoxInv, but got {:?}", &x);
                assert!(false);
            }
        }
        
        // ask for a PoX vector off of an unknown consensus hash
        let getpoxinv_request = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let getpoxinv = GetPoxInv { consensus_hash: ConsensusHash([0xaa; 20]), num_cycles: 10 };
            Ok(getpoxinv)
        }).unwrap();

        test_debug!("\n\nSend {:?}\n\n", &getpoxinv_request);

        let reply = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getpoxinv_response(&network.local_peer, &network.burnchain, sortdb, &network.pox_id, &getpoxinv_request)
        }).unwrap();

        test_debug!("\n\nReply {:?}\n\n", &reply);

        match reply {
            StacksMessageType::Nack(nack_data) => {
                assert_eq!(nack_data.error_code, NackErrorCodes::InvalidPoxFork);
            },
            x => {
                error!("Did not get PoxInv, but got {:?}", &x);
                assert!(false);
            }
        }

        // ask for a getblocksinv, aligned on a reward cycle.
        let getblocksinv_request = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(network.burnchain.block_height_to_reward_cycle(first_stacks_block_height).unwrap());
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id).unwrap().unwrap();
                sn
            };
            let getblocksinv = GetBlocksInv { consensus_hash: sn.consensus_hash, num_blocks: reward_cycle_length as u16 };
            Ok(getblocksinv)
        }).unwrap();

        test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

        let reply = peer_1.with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(&network.local_peer, &network.burnchain, sortdb, chainstate, &mut network.header_cache, &getblocksinv_request)
        }).unwrap();

        test_debug!("\n\nReply {:?}\n\n", &reply);

        match reply {
            StacksMessageType::BlocksInv(blocksinv) => {
                assert_eq!(blocksinv.bitlen, reward_cycle_length as u16);
                assert_eq!(blocksinv.block_bitvec, vec![0x1f]);
                assert_eq!(blocksinv.microblocks_bitvec, vec![0x1f]);
            },
            x => {
                error!("Did not get BlocksInv, but got {:?}", &x);
                assert!(false);
            }
        };
        
        // ask for a getblocksinv, right at the first Stacks block height
        let getblocksinv_request = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(network.burnchain.block_height_to_reward_cycle(first_stacks_block_height).unwrap());
            test_debug!("Ask for inv at height {}", height);
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id).unwrap().unwrap();
                sn
            };
            let getblocksinv = GetBlocksInv { consensus_hash: sn.consensus_hash, num_blocks: reward_cycle_length as u16 };
            Ok(getblocksinv)
        }).unwrap();

        test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

        let reply = peer_1.with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(&network.local_peer, &network.burnchain, sortdb, chainstate, &mut network.header_cache, &getblocksinv_request)
        }).unwrap();

        test_debug!("\n\nReply {:?}\n\n", &reply);

        match reply {
            StacksMessageType::BlocksInv(blocksinv) => {
                assert_eq!(blocksinv.bitlen, reward_cycle_length as u16);
                assert_eq!(blocksinv.block_bitvec, vec![0x1f]);
                assert_eq!(blocksinv.microblocks_bitvec, vec![0x1f]);
            },
            x => {
                error!("Did not get Nack, but got {:?}", &x);
                assert!(false);
            }
        };
        
        // ask for a getblocksinv, prior to the first Stacks block height
        let getblocksinv_request = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(network.burnchain.block_height_to_reward_cycle(first_stacks_block_height).unwrap() - 1);
            test_debug!("Ask for inv at height {}", height);
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id).unwrap().unwrap();
                sn
            };
            let getblocksinv = GetBlocksInv { consensus_hash: sn.consensus_hash, num_blocks: reward_cycle_length as u16 };
            Ok(getblocksinv)
        }).unwrap();

        test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

        let reply = peer_1.with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(&network.local_peer, &network.burnchain, sortdb, chainstate, &mut network.header_cache, &getblocksinv_request)
        }).unwrap();

        test_debug!("\n\nReply {:?}\n\n", &reply);

        match reply {
            StacksMessageType::BlocksInv(blocksinv) => {
                assert_eq!(blocksinv.bitlen, reward_cycle_length as u16);
                assert_eq!(blocksinv.block_bitvec, vec![0x0]);
                assert_eq!(blocksinv.microblocks_bitvec, vec![0x0]);
            },
            x => {
                error!("Did not get BlocksInv, but got {:?}", &x);
                assert!(false);
            }
        };
        
        // ask for a getblocksinv, unaligned to a reward cycle
        let getblocksinv_request = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let height = network.burnchain.reward_cycle_to_block_height(network.burnchain.block_height_to_reward_cycle(first_stacks_block_height).unwrap()) + 1;
            let sn = {
                let ic = sortdb.index_conn();
                let sn = SortitionDB::get_ancestor_snapshot(&ic, height, &tip.sortition_id).unwrap().unwrap();
                sn
            };
            let getblocksinv = GetBlocksInv { consensus_hash: sn.consensus_hash, num_blocks: reward_cycle_length as u16 };
            Ok(getblocksinv)
        }).unwrap();

        test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

        let reply = peer_1.with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(&network.local_peer, &network.burnchain, sortdb, chainstate, &mut network.header_cache, &getblocksinv_request)
        }).unwrap();

        test_debug!("\n\nReply {:?}\n\n", &reply);

        match reply {
            StacksMessageType::Nack(nack_data) => {
                assert_eq!(nack_data.error_code, NackErrorCodes::InvalidPoxFork);
            },
            x => {
                error!("Did not get Nack, but got {:?}", &x);
                assert!(false);
            }
        };

        // ask for a getblocksinv, for an unknown consensus hash
        let getblocksinv_request = peer_1.with_network_state(|sortdb, _chainstate, network, _relayer, _mempool| {
            let getblocksinv = GetBlocksInv { consensus_hash: ConsensusHash([0xaa; 20]), num_blocks: reward_cycle_length as u16 };
            Ok(getblocksinv)
        }).unwrap();

        test_debug!("\n\nSend {:?}\n\n", &getblocksinv_request);

        let reply = peer_1.with_network_state(|sortdb, chainstate, network, _relayer, _mempool| {
            ConversationP2P::make_getblocksinv_response(&network.local_peer, &network.burnchain, sortdb, chainstate, &mut network.header_cache, &getblocksinv_request)
        }).unwrap();

        test_debug!("\n\nReply {:?}\n\n", &reply);

        match reply {
            StacksMessageType::Nack(nack_data) => {
                assert_eq!(nack_data.error_code, NackErrorCodes::NoSuchBurnchainBlock);
            },
            x => {
                error!("Did not get Nack, but got {:?}", &x);
                assert!(false);
            }
        };
    }
     
    #[test]
    fn test_sync_inv_diagnose_nack() {
        let peer_config = TestPeerConfig::new("test_sync_inv_diagnose_nack", 31983, 41983);
        let neighbor = peer_config.to_neighbor();
        let neighbor_key = neighbor.addr.clone();
        let nack_no_block = NackData { error_code: NackErrorCodes::NoSuchBurnchainBlock };

        let mut burnchain_view = BurnchainView {
            burn_block_height: 12346,
            burn_block_hash: BurnchainHeaderHash([0x11; 32]),
            burn_stable_block_height: 12340,
            burn_stable_block_hash: BurnchainHeaderHash([0x22; 32]),
            last_burn_block_hashes: HashMap::new()
        };

        burnchain_view.make_test_data();
        let ch_12345 = burnchain_view.last_burn_block_hashes.get(&12345).unwrap().clone();
        let ch_12340 = burnchain_view.last_burn_block_hashes.get(&12340).unwrap().clone();
        let ch_12341 = burnchain_view.last_burn_block_hashes.get(&12341).unwrap().clone();
        let ch_12339 = burnchain_view.last_burn_block_hashes.get(&12339).unwrap().clone();
        let ch_12334 = burnchain_view.last_burn_block_hashes.get(&12334).unwrap().clone();

        // should be stable; but got nacked (so this would be inappropriate)
        assert_eq!(NodeStatus::Broken, NeighborBlockStats::diagnose_nack(&neighbor_key, nack_no_block.clone(), &burnchain_view, 12346, 12340, &BurnchainHeaderHash([0x11; 32]), &BurnchainHeaderHash([0x22; 32])));
        
        // should be stale
        assert_eq!(NodeStatus::Stale, NeighborBlockStats::diagnose_nack(&neighbor_key, nack_no_block.clone(), &burnchain_view, 12345, 12339, &ch_12345.clone(), &ch_12339.clone()));

        // should be diverged -- different stable burn block hash
        assert_eq!(NodeStatus::Diverged, NeighborBlockStats::diagnose_nack(&neighbor_key, nack_no_block.clone(), &burnchain_view, 12346, 12340, &BurnchainHeaderHash([0x12; 32]), &BurnchainHeaderHash([0x23; 32])));
    }

    #[test]
    #[ignore]
    fn test_sync_inv_2_peers_plain() {
        with_timeout(600, || {
            let mut peer_1_config = TestPeerConfig::new("test_sync_inv_2_peers_plain", 31992, 41992);
            let mut peer_2_config = TestPeerConfig::new("test_sync_inv_2_peers_plain", 31993, 41993);

            peer_1_config.add_neighbor(&peer_2_config.to_neighbor());
            peer_2_config.add_neighbor(&peer_1_config.to_neighbor());

            let mut peer_1 = TestPeer::new(peer_1_config);
            let mut peer_2 = TestPeer::new(peer_2_config);

            let num_blocks = (GETPOXINV_MAX_BITLEN * 2) as u64;
            let first_stacks_block_height = {
                let sn = SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                sn.block_height + 1
            };

            for i in 0..num_blocks {
                let (burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

                peer_1.next_burnchain_block(burn_ops.clone());
                peer_2.next_burnchain_block(burn_ops.clone());

                peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
                peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
            }

            let num_burn_blocks = {
                let sn = SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                sn.block_height + 1
            };
            
            let mut round = 0;
            let mut inv_1_count = 0;
            let mut inv_2_count = 0;

            while inv_1_count < num_blocks || inv_2_count < num_blocks {
                let _ = peer_1.step();
                let _ = peer_2.step();

                inv_1_count = match peer_1.network.inv_state {
                    Some(ref inv) => {
                        info!("Peer 1 stats: {:?}", &inv.block_stats);
                        inv.get_inv_num_blocks(&peer_2.to_neighbor().addr)
                    },
                    None => 0
                };

                inv_2_count = match peer_2.network.inv_state {
                    Some(ref inv) => {
                        info!("Peer 2 stats: {:?}", &inv.block_stats);
                        inv.get_inv_num_blocks(&peer_1.to_neighbor().addr)
                    },
                    None => 0
                };

                // nothing should break
                match peer_1.network.inv_state {
                    Some(ref inv) => {
                        assert_eq!(inv.get_broken_peers().len(), 0);
                        assert_eq!(inv.get_dead_peers().len(), 0);
                        assert_eq!(inv.get_diverged_peers().len(), 0);
                    },
                    None => {}
                }

                match peer_2.network.inv_state {
                    Some(ref inv) => {
                        assert_eq!(inv.get_broken_peers().len(), 0);
                        assert_eq!(inv.get_dead_peers().len(), 0);
                        assert_eq!(inv.get_diverged_peers().len(), 0);
                    },
                    None => {}
                }

                round += 1;
            }

            info!("Completed walk round {} step(s)", round);

            peer_1.dump_frontier();
            peer_2.dump_frontier();
            
            info!("Peer 1 stats: {:?}", &peer_1.network.inv_state.as_ref().unwrap().block_stats);
            info!("Peer 2 stats: {:?}", &peer_2.network.inv_state.as_ref().unwrap().block_stats);

            let peer_1_inv = peer_2.network.inv_state.as_ref().unwrap().block_stats.get(&peer_1.to_neighbor().addr).unwrap().inv.clone();
            let peer_2_inv = peer_1.network.inv_state.as_ref().unwrap().block_stats.get(&peer_2.to_neighbor().addr).unwrap().inv.clone();

            info!("Peer 1 inv: {:?}", &peer_1_inv);
            info!("Peer 2 inv: {:?}", &peer_2_inv);

            info!("peer 1's view of peer 2: {:?}", &peer_2_inv);

            assert_eq!(peer_2_inv.num_sortitions, num_burn_blocks);
            
            // peer 1 should have learned that peer 2 has all the blocks
            for i in 0..num_blocks {
                assert!(peer_2_inv.has_ith_block(i + first_stacks_block_height), format!("Missing block {} (+ {})", i, first_stacks_block_height));
            }

            // peer 1 should have learned that peer 2 has all the microblock streams 
            for i in 0..(num_blocks - 1) {
                assert!(peer_2_inv.has_ith_microblock_stream(i + first_stacks_block_height), format!("Missing microblock {} (+ {})", i, first_stacks_block_height));
            }

            let peer_1_inv = peer_2.network.inv_state.as_ref().unwrap().block_stats.get(&peer_1.to_neighbor().addr).unwrap().inv.clone();
            test_debug!("peer 2's view of peer 1: {:?}", &peer_1_inv);

            assert_eq!(peer_1_inv.num_sortitions, num_burn_blocks);
            
            // peer 2 should have learned that peer 1 has all the blocks as well
            for i in 0..num_blocks {
                assert!(peer_1_inv.has_ith_block(i + first_stacks_block_height), format!("Missing block {} (+ {})", i, first_stacks_block_height));
            }
        })
    }
   
    #[test]
    #[ignore]
    fn test_sync_inv_2_peers_stale() {
        with_timeout(600, || {
            let mut peer_1_config = TestPeerConfig::new("test_sync_inv_2_peers_stale", 31994, 41995);
            let mut peer_2_config = TestPeerConfig::new("test_sync_inv_2_peers_stale", 31995, 41996);

            peer_1_config.add_neighbor(&peer_2_config.to_neighbor());
            peer_2_config.add_neighbor(&peer_1_config.to_neighbor());

            let mut peer_1 = TestPeer::new(peer_1_config);
            let mut peer_2 = TestPeer::new(peer_2_config);

            let num_blocks = (GETPOXINV_MAX_BITLEN * 2) as u64;
            let first_stacks_block_height = {
                let sn = SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                sn.block_height + 1
            };

            for i in 0..num_blocks {
                let (burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

                peer_2.next_burnchain_block(burn_ops.clone());
                peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
            }
            
            let mut round = 0;
            let mut inv_1_count = 0;
            let mut inv_2_count = 0;

            let mut peer_1_check = false;
            let mut peer_2_check = false;
            
            while !peer_1_check || !peer_2_check {
                let _ = peer_1.step();
                let _ = peer_2.step();

                inv_1_count = match peer_1.network.inv_state {
                    Some(ref inv) => inv.get_inv_sortitions(&peer_2.to_neighbor().addr),
                    None => 0
                };

                inv_2_count = match peer_2.network.inv_state {
                    Some(ref inv) => inv.get_inv_sortitions(&peer_1.to_neighbor().addr),
                    None => 0
                };

                match peer_1.network.inv_state {
                    Some(ref inv) => {
                        info!("Peer 1 stats: {:?}", &inv.block_stats);
                        assert_eq!(inv.get_broken_peers().len(), 0);
                        assert_eq!(inv.get_dead_peers().len(), 0);
                        assert_eq!(inv.get_diverged_peers().len(), 0);

                        if let Some(ref peer_2_inv) = inv.block_stats.get(&peer_2.to_neighbor().addr) {
                            if peer_2_inv.inv.num_sortitions == first_stacks_block_height - peer_1.config.burnchain.first_block_height {
                                for i in 0..first_stacks_block_height {
                                    assert!(!peer_2_inv.inv.has_ith_block(i));
                                    assert!(!peer_2_inv.inv.has_ith_microblock_stream(i));
                                }
                                peer_2_check = true;
                            }
                        }
                    },
                    None => {}
                }

                match peer_2.network.inv_state {
                    Some(ref inv) => {
                        info!("Peer 2 stats: {:?}", &inv.block_stats);
                        assert_eq!(inv.get_broken_peers().len(), 0);
                        assert_eq!(inv.get_dead_peers().len(), 0);
                        assert_eq!(inv.get_diverged_peers().len(), 0);

                        if let Some(ref peer_1_inv) = inv.block_stats.get(&peer_1.to_neighbor().addr) {
                            if peer_1_inv.inv.num_sortitions == first_stacks_block_height - peer_1.config.burnchain.first_block_height {
                                peer_1_check = true;
                            }
                        }
                    },
                    None => {}
                }

                round += 1;

                test_debug!("\n\npeer_1_check = {}, peer_2_check = {}, inv_1_count = {}, inv_2_count = {}, first_stacks_block_height = {}\n\n", peer_1_check, peer_2_check, inv_1_count, inv_2_count, first_stacks_block_height);
            }

            info!("Completed walk round {} step(s)", round);

            peer_1.dump_frontier();
            peer_2.dump_frontier();
        })
    }
    
    #[test]
    #[ignore]
    fn test_sync_inv_2_peers_unstable() {
        with_timeout(600, || {
            let mut peer_1_config = TestPeerConfig::new("test_sync_inv_2_peers_unstable", 31996, 41997);
            let mut peer_2_config = TestPeerConfig::new("test_sync_inv_2_peers_unstable", 31997, 41998);

            peer_1_config.add_neighbor(&peer_2_config.to_neighbor());
            peer_2_config.add_neighbor(&peer_1_config.to_neighbor());

            let mut peer_1 = TestPeer::new(peer_1_config);
            let mut peer_2 = TestPeer::new(peer_2_config);

            let num_blocks = (GETPOXINV_MAX_BITLEN * 2) as u64;

            let first_stacks_block_height = {
                let sn = SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                sn.block_height + 1
            };

            // only peer 2 makes progress after the point of stability.
            for i in 0..num_blocks {
                let (mut burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

                let (_, burn_header_hash, consensus_hash) = peer_2.next_burnchain_block(burn_ops.clone());
                peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
                
                TestPeer::set_ops_burn_header_hash(&mut burn_ops, &burn_header_hash);

                // NOTE: the nodes only differ by one block -- they agree on the same PoX vector
                if i + 1 < num_blocks {
                    peer_1.next_burnchain_block_raw(burn_ops.clone());
                    peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
                }
                else {
                    // peer 1 diverges
                    test_debug!("Peer 1 diverges");
                    peer_1.next_burnchain_block(vec![]);
                }
            }

            // tips must differ
            {
                let sn1 = SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                let sn2 = SortitionDB::get_canonical_burn_chain_tip(peer_2.sortdb.as_ref().unwrap().conn()).unwrap();
                assert_ne!(sn1.burn_header_hash, sn2.burn_header_hash);
            }

            let num_stable_blocks = num_blocks - 1;

            let num_burn_blocks = {
                let sn = SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                sn.block_height + 1
            };
            
            let mut round = 0;
            let mut inv_1_count = 0;
            let mut inv_2_count = 0;

            let mut peer_1_pox_cycle_start = false;
            let mut peer_1_block_cycle_start = false;
            let mut peer_2_pox_cycle_start = false;
            let mut peer_2_block_cycle_start = false;
            
            let mut peer_1_pox_cycle = false;
            let mut peer_1_block_cycle = false;
            let mut peer_2_pox_cycle = false;
            let mut peer_2_block_cycle = false;
            
            while inv_1_count < num_stable_blocks || inv_2_count < num_stable_blocks {
                let _ = peer_1.step();
                let _ = peer_2.step();

                inv_1_count = match peer_1.network.inv_state {
                    Some(ref inv) => inv.get_inv_num_blocks(&peer_2.to_neighbor().addr),
                    None => 0
                };

                inv_2_count = match peer_2.network.inv_state {
                    Some(ref inv) => inv.get_inv_num_blocks(&peer_1.to_neighbor().addr),
                    None => 0
                };

                match peer_1.network.inv_state {
                    Some(ref inv) => {
                        info!("Peer 1 stats: {:?}", &inv.block_stats);
                        assert_eq!(inv.get_broken_peers().len(), 0);
                        assert_eq!(inv.get_dead_peers().len(), 0);
                        assert_eq!(inv.get_diverged_peers().len(), 0);

                        if let Some(stats) = inv.get_stats(&peer_2.to_neighbor().addr) {
                            if stats.target_pox_reward_cycle > 0 {
                                peer_1_pox_cycle_start = true;
                            }
                            if stats.target_block_reward_cycle > 0 {
                                peer_1_block_cycle_start = true;
                            }
                            if stats.target_pox_reward_cycle == 0 && peer_1_pox_cycle_start {
                                peer_1_pox_cycle = true;
                            }
                            if stats.target_block_reward_cycle == 0 && peer_1_block_cycle_start {
                                peer_1_block_cycle = true;
                            }
                        }
                    },
                    None => {}
                }

                match peer_2.network.inv_state {
                    Some(ref inv) => {
                        info!("Peer 2 stats: {:?}", &inv.block_stats);
                        assert_eq!(inv.get_broken_peers().len(), 0);
                        assert_eq!(inv.get_dead_peers().len(), 0);
                        assert_eq!(inv.get_diverged_peers().len(), 0);
                        
                        if let Some(stats) = inv.get_stats(&peer_1.to_neighbor().addr) {
                            if stats.target_pox_reward_cycle > 0 {
                                peer_2_pox_cycle_start = true;
                            }
                            if stats.target_block_reward_cycle > 0 {
                                peer_2_block_cycle_start = true;
                            }
                            if stats.target_pox_reward_cycle == 0 && peer_2_pox_cycle_start {
                                peer_2_pox_cycle = true;
                            }
                            if stats.target_block_reward_cycle == 0 && peer_2_block_cycle_start {
                                peer_2_block_cycle = true;
                            }
                        }
                    },
                    None => {}
                }

                round += 1;

                test_debug!("\n\ninv_1_count = {}, inv_2_count = {}, num_stable_blocks = {}\n\n", inv_1_count, inv_2_count, num_stable_blocks);
            }

            info!("Completed walk round {} step(s)", round);

            peer_1.dump_frontier();
            peer_2.dump_frontier();

            let peer_2_inv = peer_1.network.inv_state.as_ref().unwrap().block_stats.get(&peer_2.to_neighbor().addr).unwrap().inv.clone();
            test_debug!("peer 1's view of peer 2: {:?}", &peer_2_inv);
            
            let peer_1_inv = peer_2.network.inv_state.as_ref().unwrap().block_stats.get(&peer_1.to_neighbor().addr).unwrap().inv.clone();
            test_debug!("peer 2's view of peer 1: {:?}", &peer_1_inv);
            
            assert_eq!(peer_2_inv.num_sortitions, num_burn_blocks - 1);
            assert_eq!(peer_1_inv.num_sortitions, num_burn_blocks - 1);

            // only 8 reward cycles -- we couldn't agree on the 9th
            assert_eq!(peer_1_inv.pox_inv, vec![255]);
            assert_eq!(peer_2_inv.pox_inv, vec![255]);
            
            // peer 1 should have learned that peer 2 has all the blocks, up to the point of
            // instability
            for i in 0..(num_blocks - 1) {
                assert!(peer_2_inv.has_ith_block(i + first_stacks_block_height));
                assert!(peer_2_inv.has_ith_microblock_stream(i + first_stacks_block_height));
            }
            
            for i in 0..(num_blocks - 1) {
                assert!(peer_1_inv.has_ith_block(i + first_stacks_block_height));
                if i != num_blocks - 2 {
                    // peer 1 doesn't have the final microblock stream, since no anchor block confirmed it
                    assert!(peer_1_inv.has_ith_microblock_stream(i + first_stacks_block_height));
                }
            }

            assert!(!peer_2_inv.has_ith_block(num_blocks - 1));
            assert!(!peer_2_inv.has_ith_microblock_stream(num_blocks - 1));
        })
    }
    
    #[test]
    #[ignore]
    fn test_sync_inv_2_peers_different_pox_vectors() {
        with_timeout(600, || {
            let mut peer_1_config = TestPeerConfig::new("test_sync_inv_2_peers_different_pox_vectors", 31996, 41997);
            let mut peer_2_config = TestPeerConfig::new("test_sync_inv_2_peers_different_pox_vectors", 31997, 41998);

            peer_1_config.add_neighbor(&peer_2_config.to_neighbor());
            peer_2_config.add_neighbor(&peer_1_config.to_neighbor());

            let reward_cycle_length = peer_1_config.burnchain.pox_constants.reward_cycle_length as u64;
            assert_eq!(reward_cycle_length, 5);

            let mut peer_1 = TestPeer::new(peer_1_config);
            let mut peer_2 = TestPeer::new(peer_2_config);

            let num_blocks = (GETPOXINV_MAX_BITLEN * 3) as u64;

            let first_stacks_block_height = {
                let sn = SortitionDB::get_canonical_burn_chain_tip(&peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                sn.block_height + 1
            };

            // only peer 2 makes progress after the point of stability.
            for i in 0..num_blocks {
                let (mut burn_ops, stacks_block, microblocks) = peer_2.make_default_tenure();

                let (_, burn_header_hash, consensus_hash) = peer_2.next_burnchain_block(burn_ops.clone());
                peer_2.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
                
                TestPeer::set_ops_burn_header_hash(&mut burn_ops, &burn_header_hash);

                peer_1.next_burnchain_block_raw(burn_ops.clone());
                if i < num_blocks - reward_cycle_length * 2 {
                    peer_1.process_stacks_epoch_at_tip(&stacks_block, &microblocks);
                }
            }

            let peer_1_pox_id = {
                let tip_sort_id = SortitionDB::get_canonical_sortition_tip(peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                let ic = peer_1.sortdb.as_ref().unwrap().index_conn();
                let sortdb_reader = SortitionHandleConn::open_reader(&ic, &tip_sort_id).unwrap();
                sortdb_reader.get_pox_id().unwrap()
            };

            let peer_2_pox_id = {
                let tip_sort_id = SortitionDB::get_canonical_sortition_tip(peer_2.sortdb.as_ref().unwrap().conn()).unwrap();
                let ic = peer_2.sortdb.as_ref().unwrap().index_conn();
                let sortdb_reader = SortitionHandleConn::open_reader(&ic, &tip_sort_id).unwrap();
                sortdb_reader.get_pox_id().unwrap()
            };

            // peers must have different PoX bit vectors -- peer 1 didn't see the last reward cycle
            assert_eq!(peer_1_pox_id, PoxId::from_bools(vec![true, true, true, true, true, true, true, true, true, false]));
            assert_eq!(peer_2_pox_id, PoxId::from_bools(vec![true, true, true, true, true, true, true, true, true, true]));

            let num_burn_blocks = {
                let sn = SortitionDB::get_canonical_burn_chain_tip(peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                sn.block_height + 1
            };
            
            let mut round = 0;
            let mut inv_1_count = 0;
            let mut inv_2_count = 0;
            let mut peer_1_sorts = 0;
            let mut peer_2_sorts = 0;

            while inv_1_count < reward_cycle_length * 4 || inv_2_count < num_blocks - reward_cycle_length * 2 || peer_1_sorts < reward_cycle_length * 9 + 1 || peer_2_sorts < reward_cycle_length * 9 + 1 {
                let _ = peer_1.step();
                let _ = peer_2.step();

                // peer 1 should see that peer 2 has all blocks for reward cycles 5 through 9
                match peer_1.network.inv_state {
                    Some(ref inv) => {
                        inv_1_count = inv.get_inv_num_blocks(&peer_2.to_neighbor().addr);
                        peer_1_sorts = inv.get_inv_sortitions(&peer_2.to_neighbor().addr);
                    },
                    None => {}
                };

                // peer 2 should see that peer 1 has all blocks up to where we stopped feeding them to
                // it
                match peer_2.network.inv_state {
                    Some(ref inv) => {
                        inv_2_count = inv.get_inv_num_blocks(&peer_1.to_neighbor().addr);
                        peer_2_sorts = inv.get_inv_sortitions(&peer_1.to_neighbor().addr);
                    },
                    None => {}
                };

                match peer_1.network.inv_state {
                    Some(ref inv) => {
                        info!("Peer 1 stats: {:?}", &inv.block_stats);
                        assert_eq!(inv.get_broken_peers().len(), 0);
                        assert_eq!(inv.get_dead_peers().len(), 0);
                        assert_eq!(inv.get_diverged_peers().len(), 0);
                    },
                    None => {}
                }

                match peer_2.network.inv_state {
                    Some(ref inv) => {
                        info!("Peer 2 stats: {:?}", &inv.block_stats);
                        assert_eq!(inv.get_broken_peers().len(), 0);
                        assert_eq!(inv.get_dead_peers().len(), 0);
                        assert_eq!(inv.get_diverged_peers().len(), 0);
                    },
                    None => {}
                }

                round += 1;

                test_debug!("\n\ninv_1_count = {}, inv_2_count = {}, peer_1_sorts = {}, peer_2_sorts = {}", inv_1_count, inv_2_count, peer_1_sorts, peer_2_sorts);
            }

            info!("Completed walk round {} step(s)", round);

            peer_1.dump_frontier();
            peer_2.dump_frontier();
            
            let peer_1_pox_id = {
                let tip_sort_id = SortitionDB::get_canonical_sortition_tip(peer_1.sortdb.as_ref().unwrap().conn()).unwrap();
                let ic = peer_1.sortdb.as_ref().unwrap().index_conn();
                let sortdb_reader = SortitionHandleConn::open_reader(&ic, &tip_sort_id).unwrap();
                sortdb_reader.get_pox_id().unwrap()
            };

            let peer_2_pox_id = {
                let tip_sort_id = SortitionDB::get_canonical_sortition_tip(peer_2.sortdb.as_ref().unwrap().conn()).unwrap();
                let ic = peer_2.sortdb.as_ref().unwrap().index_conn();
                let sortdb_reader = SortitionHandleConn::open_reader(&ic, &tip_sort_id).unwrap();
                sortdb_reader.get_pox_id().unwrap()
            };

            let peer_2_inv = peer_1.network.inv_state.as_ref().unwrap().block_stats.get(&peer_2.to_neighbor().addr).unwrap().inv.clone();
            test_debug!("peer 1's view of peer 2: {:?}", &peer_2_inv);
            test_debug!("peer 1's PoX bit vector is {:?}", &peer_1_pox_id);
            
            let peer_1_inv = peer_2.network.inv_state.as_ref().unwrap().block_stats.get(&peer_1.to_neighbor().addr).unwrap().inv.clone();
            test_debug!("peer 2's view of peer 1: {:?}", &peer_1_inv);
            test_debug!("peer 2's PoX bit vector is {:?}", &peer_2_pox_id);
            
            // nodes only learn about the prefix of their PoX bit vectors that they agree on
            assert_eq!(peer_2_inv.num_sortitions, reward_cycle_length * 9 + 1);
            assert_eq!(peer_1_inv.num_sortitions, reward_cycle_length * 9 + 1);

            // only 9 reward cycles -- we couldn't agree on the 10th
            assert_eq!(peer_1_inv.pox_inv, vec![255, 1]);
            assert_eq!(peer_2_inv.pox_inv, vec![255, 1]);
            
            // peer 1 should have learned that peer 2 has all the blocks, up to the point of
            // PoX instability between the two
            for i in 0..(reward_cycle_length * 4) {
                assert!(peer_2_inv.has_ith_block(i + first_stacks_block_height));
                assert!(peer_2_inv.has_ith_microblock_stream(i + first_stacks_block_height));
            }
           
            // peer 2 should have learned about all of peer 1's blocks
            for i in 0..(num_blocks - 2 * reward_cycle_length) {
                assert!(peer_1_inv.has_ith_block(i + first_stacks_block_height));
                if i != num_blocks - 2 * reward_cycle_length - 1 {
                    // peer 1 doesn't have the final microblock stream, since no anchor block confirmed it
                    assert!(peer_1_inv.has_ith_microblock_stream(i + first_stacks_block_height));
                }
            }
            
            assert!(!peer_1_inv.has_ith_block(reward_cycle_length * 4));
            assert!(!peer_1_inv.has_ith_microblock_stream(reward_cycle_length * 4));

            assert!(!peer_2_inv.has_ith_block(num_blocks - 2 * reward_cycle_length));
            assert!(!peer_2_inv.has_ith_microblock_stream(num_blocks - 2 * reward_cycle_length));
        })
    }
}
