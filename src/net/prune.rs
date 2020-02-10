/*
 copyright: (c) 2013-2019 by Blockstack PBC, a public benefit corporation.

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

/// This module contains the logic for pruning client and neighbor connections

use net::*;
use net::p2p::*;

use net::Error as net_error;

use net::connection::ConnectionOptions;

use net::chat::Conversation;
use net::chat::NeighborStats;

use net::poll::NetworkState;
use net::poll::NetworkPollState;

use net::db::LocalPeer;
use net::db::PeerDB;

use net::neighbors::*;

use util::db::Error as db_error;
use util::db::DBConn;

use std::net::SocketAddr;
use std::net::Shutdown;

use std::collections::VecDeque;
use std::collections::HashMap;
use std::collections::HashSet;
use std::cmp::Ordering;

use util::log;
use util::get_epoch_time_secs;

use rand::prelude::*;
use rand::thread_rng;

impl PeerNetwork {
    /// Find out which organizations have which of our outbound neighbors.
    /// Gives back a map from the organization ID to the list of (neighbor, neighbor-stats) tuples
    fn org_neighbor_distribution(&self, peer_dbconn: &DBConn, preserve: &HashSet<usize>) -> Result<HashMap<u32, Vec<(NeighborKey, NeighborStats)>>, net_error> {
        // find out which organizations have which neighbors
        let mut org_neighbor : HashMap<u32, Vec<(NeighborKey, NeighborStats)>> = HashMap::new();
        for (_, event_id) in self.events.iter() {
            if preserve.contains(event_id) {
                continue;
            }

            match self.peers.get(event_id) {
                None => {
                    continue;
                },
                Some(ref convo) => {
                    if !convo.stats.outbound {
                        continue;
                    }

                    let nk = convo.to_neighbor_key();
                    let peer_opt = PeerDB::get_peer(peer_dbconn, nk.network_id, &nk.addrbytes, nk.port)
                        .map_err(|_e| net_error::DBError)?;

                    match peer_opt {
                        None => {
                            continue;
                        },
                        Some(peer) => {
                            let stats = convo.stats.clone();
                            let org = peer.org;
                            if org_neighbor.contains_key(&org) {
                                org_neighbor.get_mut(&org).unwrap().push((nk, stats));
                            }
                            else {
                                org_neighbor.insert(org, vec![(nk, stats)]);
                            }
                        }
                    };
                }
            };
        }

        test_debug!("==== ORG NEIGHBOR DISTRIBUTION OF {:?} ===", &self.local_peer);
        for (ref _org, ref neighbor_infos) in org_neighbor.iter() {
            let _neighbors : Vec<NeighborKey> = neighbor_infos.iter().map(|ni| ni.0.clone()).collect();
            test_debug!("Org {}: {} neighbors: {:?}", _org, _neighbors.len(), &_neighbors);
        }
        test_debug!("===============================================================");

        Ok(org_neighbor)
    }

    /// Sort function for a neighbor list in order to compare by by uptime and health.
    /// Bucket uptime geometrically by powers of 2 -- a node that's been up for X seconds is
    /// likely to be up for X more seconds, so we only really want to distinguish between nodes that
    /// have wildly different uptimes.
    /// Within uptime buckets, sort by health.
    fn compare_neighbor_uptime_health(stats1: &NeighborStats, stats2: &NeighborStats) -> Ordering {
        let now = get_epoch_time_secs();
        let uptime_1 = (now - stats1.first_contact_time) as f64;
        let uptime_2 = (now - stats2.first_contact_time) as f64;

        let uptime_bucket_1 = fmax!(0.0, uptime_1.log2().round()) as u64;
        let uptime_bucket_2 = fmax!(0.0, uptime_2.log2().round()) as u64;

        if uptime_bucket_1 < uptime_bucket_2 {
            return Ordering::Less;
        }
        if uptime_bucket_1 > uptime_bucket_1 {
            return Ordering::Greater;
        }

        // same bucket; sort by health 
        let health_1 = stats1.get_health_score();
        let health_2 = stats2.get_health_score();
        
        if health_1 < health_2 {
            return Ordering::Less;
        }
        if health_1 > health_2 {
            return Ordering::Greater;
        }

        // flip a coin
        let mut rng = thread_rng();
        if rng.next_u32() % 2 == 0 {
            return Ordering::Less;
        }
        else {
            return Ordering::Greater;
        }
        // return Ordering::Equal;
    }

    /// Sample an org based on its weight
    fn sample_org_by_neighbor_count(org_weights: &HashMap<u32, usize>) -> u32 {
        let mut rng = thread_rng();
        let mut total = 0;
        for (_, count) in org_weights.iter() {
            total += count;
        }

        let sample = rng.gen_range(0, total);
        let mut offset = 0;
        for (org, count) in org_weights.iter() {
            if *count == 0 {
                continue;
            }

            if offset <= sample && sample < offset + *count {
                return *org;
            }
            offset += *count;
        }
        unreachable!();
    }

    /// If we have an overabundance of outbound connections, then remove ones from overrepresented
    /// organizations that are unhealthy or very-recently discovered.
    /// Returns the list of neighbor keys to remove.
    fn prune_frontier_outbound_orgs(&mut self, preserve: &HashSet<usize>) -> Result<Vec<NeighborKey>, net_error> {
        let num_outbound = PeerNetwork::count_outbound_conversations(&self.peers);
        if num_outbound <= self.connection_opts.soft_num_neighbors {
            return Ok(vec![]);
        }

        let mut org_neighbors = self.org_neighbor_distribution(self.peerdb.conn(), preserve)?;
        let mut ret = vec![];
        let orgs : Vec<u32> = org_neighbors.keys().map(|o| {let r = *o; r }).collect();

        for org in orgs.iter() {
            // sort each neighbor list by uptime and health.
            // bucket uptime geometrically by powers of 2 -- a node that's been up for X seconds is
            // likely to be up for X more seconds, so we only really want to distinguish between nodes that
            // have wildly different uptimes.
            // Within uptime buckets, sort by health.
            match org_neighbors.get_mut(&org) {
                None => {},
                Some(ref mut neighbor_infos) => {
                    neighbor_infos.sort_by(|&(ref _nk1, ref stats1), &(ref _nk2, ref stats2)| PeerNetwork::compare_neighbor_uptime_health(stats1, stats2));
                }
            }
        }

        // don't let a single organization have more than
        // soft_max_neighbors_per_org neighbors.
        for org in orgs.iter() {
            match org_neighbors.get_mut(&org) {
                None => {},
                Some(ref mut neighbor_infos) => {
                    if neighbor_infos.len() as u64 > self.connection_opts.soft_max_neighbors_per_org {
                        test_debug!("Org {} has {} neighbors (more than {} soft limit)", org, neighbor_infos.len(), self.connection_opts.soft_max_neighbors_per_org);
                        for i in 0..((neighbor_infos.len() as u64) - self.connection_opts.soft_max_neighbors_per_org) {
                            let (neighbor_key, _) = neighbor_infos[i as usize].clone();

                            test_debug!("{:?}: Prune {:?} because its org ({}) dominates our peer table", &self.local_peer, &neighbor_key, org);
                            
                            ret.push(neighbor_key);
                            
                            // don't prune too many
                            if num_outbound - (ret.len() as u64) <= self.connection_opts.soft_num_neighbors {
                                break;
                            }
                        }
                        for _ in 0..ret.len() {
                            neighbor_infos.remove(0);
                        }
                    }
                }
            }
        }

        if num_outbound - (ret.len() as u64) <= self.connection_opts.soft_num_neighbors {
            // pruned enough 
            debug!("{:?}: removed {} outbound peers out of {}", &self.local_peer, ret.len(), num_outbound);
            return Ok(ret);
        }

        // select an org at random proportional to its popularity, and remove a neighbor 
        // at random proportional to how unhealthy and short-lived it is.
        test_debug!("{:?}: Prune outbound neighbor set of {} down to {}", &self.local_peer, num_outbound, self.connection_opts.soft_num_neighbors);
        while num_outbound - (ret.len() as u64) > self.connection_opts.soft_num_neighbors {
            let mut weighted_sample : HashMap<u32, usize> = HashMap::new();
            for (org, neighbor_info) in org_neighbors.iter() {
                if neighbor_info.len() > 0 {
                    weighted_sample.insert(*org, neighbor_info.len());
                }
            }
            if weighted_sample.len() == 0 {
                // nothing to do 
                break;
            }

            let prune_org = PeerNetwork::sample_org_by_neighbor_count(&weighted_sample);

            match org_neighbors.get_mut(&prune_org) {
                None => {
                    unreachable!();
                },
                Some(ref mut neighbor_info) => {
                    let (neighbor_key, _) = neighbor_info[0].clone();
                    
                    test_debug!("Prune {:?} because its org ({}) has too many members", &neighbor_key, prune_org);

                    neighbor_info.remove(0);
                    ret.push(neighbor_key);
                }
            }
        }

        debug!("{:?}: removed {} outbound peers out of {}", &self.local_peer, ret.len(), num_outbound);
        Ok(ret)
    }

    /// Prune inbound peers by IP address -- can't have too many from the same IP.
    /// Returns the list of IPs to remove.
    /// Removes them in reverse order they are added
    fn prune_frontier_inbound_ip(&mut self, preserve: &HashSet<usize>) -> Vec<NeighborKey> {
        let num_inbound = (self.num_peers() as u64) - PeerNetwork::count_outbound_conversations(&self.peers);
        if num_inbound <= self.connection_opts.soft_num_clients {
            return vec![];
        }

        let mut ip_neighbor : HashMap<PeerAddress, Vec<(usize, NeighborKey, NeighborStats)>> = HashMap::new();
        for (nk, event_id) in self.events.iter() {
            if preserve.contains(event_id) {
                continue;
            }
            match self.peers.get(&event_id) {
                Some(ref convo) => {
                    if !convo.stats.outbound {
                        let stats = convo.stats.clone();
                        if !ip_neighbor.contains_key(&nk.addrbytes) {
                            ip_neighbor.insert(nk.addrbytes, vec![(*event_id, nk.clone(), stats)]);
                        }
                        else {
                            ip_neighbor.get_mut(&nk.addrbytes).unwrap().push((*event_id, nk.clone(), stats));
                        }
                    }
                },
                None => {}
            }
        }

        // sort in order by first-contact time (oldest first)
        for (_, stats_list) in ip_neighbor.iter_mut() {
            stats_list.sort_by(|&(ref _e1, ref _nk1, ref stats1), &(ref _e2, ref _nk2, ref stats2)| {
                if stats1.first_contact_time < stats2.first_contact_time {
                    Ordering::Less
                }
                else if stats1.first_contact_time > stats2.first_contact_time {
                    Ordering::Greater
                }
                else {
                    Ordering::Equal
                }
            });
        }

        let mut to_remove = vec![];
        for (addrbytes, neighbor_info) in ip_neighbor.iter_mut() {
            if (neighbor_info.len() as u64) > self.connection_opts.soft_max_clients_per_host {
                debug!("{:?}: Starting to have too many inbound connections from {:?}; will close the last {:?}", &self.local_peer, &addrbytes, (neighbor_info.len() as u64) - self.connection_opts.soft_max_clients_per_host);
                for i in (self.connection_opts.soft_max_clients_per_host as usize)..neighbor_info.len() {
                    to_remove.push(neighbor_info[i].1.clone());
                }
            }
        }

        debug!("{:?}: removed {} inbound peers out of {}", &self.local_peer, to_remove.len(), ip_neighbor.len());
        to_remove
    }

    /// Dump our peer table
    #[cfg(test)]
    pub fn dump_peer_table(&mut self) -> (Vec<String>, Vec<String>) {
        let mut inbound: Vec<String> = vec![];
        let mut outbound: Vec<String> = vec![];

        for (nk, event_id) in self.events.iter() {
            match self.peers.get(event_id) {
                Some(convo) => {
                    if convo.stats.outbound {
                        outbound.push(format!("{:?}", &nk));
                    }
                    else {
                        inbound.push(format!("{:?}", &nk));
                    }
                },
                None => {}
            }
        }
        (inbound, outbound)
    }

    /// Prune our frontier.  Ignore connections in the preserve set.
    pub fn prune_frontier(&mut self, preserve: &HashSet<usize>) -> () {
        let pruned_by_ip = self.prune_frontier_inbound_ip(preserve);

        if pruned_by_ip.len() > 0 {
            test_debug!("{:?}: remove {} inbound peers by shared IP", &self.local_peer, pruned_by_ip.len());
        }

        for prune in pruned_by_ip.iter() {
            test_debug!("{:?}: prune by IP: {:?}", &self.local_peer, prune);
            self.deregister_neighbor(&prune);
            
            if !self.prune_inbound_counts.contains_key(prune) {
                self.prune_inbound_counts.insert(prune.clone(), 1);
            }
            else {
                let c = self.prune_inbound_counts.get(prune).unwrap().to_owned();
                self.prune_inbound_counts.insert(prune.clone(), c + 1);
            }
        }
       
        let pruned_by_org = self.prune_frontier_outbound_orgs(preserve).unwrap_or(vec![]);

        if pruned_by_org.len() > 0 {
            test_debug!("{:?}: remove {} outbound peers by shared Org", &self.local_peer, pruned_by_org.len());
        }

        for prune in pruned_by_org.iter() {
            test_debug!("{:?}: prune by Org: {:?}", &self.local_peer, prune);
            self.deregister_neighbor(&prune);

            if !self.prune_outbound_counts.contains_key(prune) {
                self.prune_outbound_counts.insert(prune.clone(), 1);
            }
            else {
                let c = self.prune_outbound_counts.get(prune).unwrap().to_owned();
                self.prune_outbound_counts.insert(prune.clone(), c + 1);
            }
        }

        #[cfg(test)]
        {
            if pruned_by_ip.len() > 0 || pruned_by_org.len() > 0 {
                let (mut inbound, mut outbound) = self.dump_peer_table();

                inbound.sort();
                outbound.sort();

                debug!("{:?}: Peers outbound ({}): {}", &self.local_peer, outbound.len(), outbound.join(", "));
                debug!("{:?}: Peers inbound ({}):  {}", &self.local_peer, inbound.len(), inbound.join(", "));

                match PeerDB::get_frontier_size(self.peerdb.conn()) {
                    Ok(count) => {
                        debug!("{:?}: Frontier size: {}", &self.local_peer, count);
                    },
                    Err(_) => {}
                };
            }
        }
    }
}
