use super::{Config, Node, BurnchainSimulator, SortitionedBlock};

use std::sync::mpsc::Sender;
use std::time;
use std::thread;

use chainstate::burn::{ConsensusHash, SortitionHash};
use net::StacksMessageType;

pub struct RunLoop {
    config: Config,
    nodes: Vec<Node>,
    nodes_txs: Vec<Sender<StacksMessageType>>
}

impl RunLoop {

    pub fn new(config: Config) -> Self {

        // Build a vec of nodes based on the config
        let mut nodes = vec![]; 
        let mut nodes_txs = vec![]; 
        let mut confs = config.node_config.clone();
        for conf in confs.drain(..) {
            let node = Node::new(conf, config.burnchain_block_time);
            nodes_txs.push(node.tx.clone());
            nodes.push(node);
        }

        Self {
            config,
            nodes,
            nodes_txs
        }
    }

    pub fn start(&mut self) {

        // Initialize and start the burnchain
        let mut burnchain = BurnchainSimulator::new();
        let (burnchain_block_rx, burnchain_op_tx) = burnchain.start(&self.config);

        // Tear-up each leader with the op_tx (mpsc::Sender<ops>) 
        // returned by the burnchain, so that each leader can commit
        // its ops independently.
        for node in self.nodes.iter_mut() {
            node.tear_up(burnchain_op_tx.clone(), ConsensusHash::empty());
        }

        let (genesis_block, ops, _) = burnchain_block_rx.recv().unwrap();

        println!("=======================================================");
        println!("GENESIS EPOCH");
        println!("BURNCHAIN: {:?} {:?} {:?}", genesis_block.block_height, genesis_block.burn_header_hash, genesis_block.parent_burn_header_hash);
        println!("=======================================================");

        for node in self.nodes.iter_mut() {
            node.process_burnchain_block(&genesis_block, &ops);
        }

        let (burnchain_block_1, ops, _) = burnchain_block_rx.recv().unwrap();

        println!("=======================================================");
        println!("EPOCH #1 - Targeting Genesis");
        println!("BURNCHAIN: {:?} {:?} {:?}", burnchain_block_1.block_height, burnchain_block_1.burn_header_hash, burnchain_block_1.parent_burn_header_hash);
        println!("=======================================================");

        for node in self.nodes.iter_mut() {
            let (sortitioned_block, won_sortition) = node.process_burnchain_block(&burnchain_block_1, &ops);
        }

        let mut tenure_1 = self.nodes[0].initiate_genesis_tenure(&genesis_block).unwrap();

        let artefacts_from_tenure_1 = tenure_1.run();

        let (anchored_block_1, microblocks, parent_block_1) = artefacts_from_tenure_1.clone();
        println!("ANCHORED_BLOCK: {:?}", anchored_block_1);
        println!("PARENT_BLOCK: {:?}", parent_block_1);
        self.nodes[0].receive_tenure_artefacts(anchored_block_1.unwrap(), parent_block_1.clone());

        let (burnchain_block, ops, burn_db) = burnchain_block_rx.recv().unwrap();
        let mut burnchain_block = burnchain_block;
        let mut ops = ops;
        let mut burn_db = burn_db;
        let mut leader_tenure = None;
        let mut last_sortitioned_block = None;

        for node in self.nodes.iter_mut() {
            let (sortitioned_block, won_sortition) = node.process_burnchain_block(&burnchain_block, &ops);
        
            last_sortitioned_block = sortitioned_block.clone();

            let (anchored_block, microblocks, parent_sortitioned_block) = artefacts_from_tenure_1.clone();

            node.process_tenure(anchored_block.unwrap(), last_sortitioned_block.clone().unwrap(), microblocks, burn_db);

            if won_sortition {
                // This node is in charge of the new tenure
                let parent_block = last_sortitioned_block.clone().unwrap();
                // match sortitioned_block {
                //     Some(parent_block) => parent_block,
                //     None => unreachable!()
                // };
                leader_tenure = node.initiate_new_tenure(&parent_block);
            } 
            break;
        }


        // let artefacts_from_tenure = match leader_tenure {
        //     Some(mut tenure) => Some(tenure.run()),
        //     None => None
        // };
    
        // if artefacts_from_tenure.is_some() {

        //     for node in self.nodes.iter_mut() {
        //         let (anchored_block, _) = artefacts_from_tenure.clone().unwrap();
    
        //         node.receive_tenure_artefacts(anchored_block.unwrap());

        //         break; // todo(ludo): get rid of this.
        //     }
        // } else {
        //     println!("NO SORTITION");
        // }

        // let (new_block, new_ops, new_db) = burnchain_block_rx.recv().unwrap();
        // burnchain_block = new_block;
        // ops = new_ops;
        // burn_db = new_db;

        // leader_tenure = None;

        // for node in self.nodes.iter_mut() {

        //     let (sortitioned_block, won_sortition) = node.process_burnchain_block(&burnchain_block, &ops);
    
        //     if won_sortition {
        //         // This node is in charge of the new tenure
        //         let parent_block = match sortitioned_block {
        //             Some(parent_block) => parent_block,
        //             None => unreachable!()
        //         };
        //         let tenure = node.initiate_new_tenure(&parent_block);
        //         leader_tenure = Some(tenure);
        //     } 
        // }

        // if artefacts_from_tenure.is_some() {

        //     for node in self.nodes.iter_mut() {
        //         let (anchored_block, microblocks) = artefacts_from_tenure.clone().unwrap();
    
        //         node.process_tenure(anchored_block.unwrap(), microblocks, burn_db);

        //         break; // todo(ludo): get rid of this.
        //     }
        // } else {
        //     println!("NO SORTITION");
        // }


        loop {
            println!("=======================================================");
            println!("NEW EPOCH");
            println!("BURNCHAIN: {:?} {:?} {:?}", burnchain_block.block_height, burnchain_block.burn_header_hash, burnchain_block.parent_burn_header_hash);
            // if leader_tenure.is_some() {
            //     println!("{}", leader_tenure.unwrap());
            // }
            println!("=======================================================");
    
            // Wait for incoming block from the burnchain

            // for each leader:
                // process the block:
                    // does the block include a sortition?
                        // did i won sortition?
                    // does the block include a registered key that I've submitted earlier?

                // if sortition
                    // get the blocks and latest microblocks from the previous leader (if was not me)
                    // submit block_commit_op
                    // if winner 
                        // start tenure
                        // dispatch artefacts to other nodes at T/2
                        // keep building microblocks until block from burnchain arrives.

            let artefacts_from_tenure = match leader_tenure {
                Some(mut tenure) => Some(tenure.run()),
                None => None
            };

            if artefacts_from_tenure.is_some() {

                for node in self.nodes.iter_mut() {
                    let (anchored_block, _, parent_block) = artefacts_from_tenure.clone().unwrap();
        
                    node.receive_tenure_artefacts(anchored_block.unwrap(), parent_block);

                    break; // todo(ludo): get rid of this.
                }
            } else {
                println!("NO SORTITION");
            }

            let (new_block, new_ops, new_db) = burnchain_block_rx.recv().unwrap();
            burnchain_block = new_block;
            ops = new_ops;
            burn_db = new_db;
    
            leader_tenure = None;

            // for node in self.nodes.iter_mut() {

            //     let (sortitioned_block, won_sortition) = node.process_burnchain_block(&burnchain_block, &ops);
        
            //     if won_sortition {
            //         // This node is in charge of the new tenure
            //         let parent_block = match sortitioned_block {
            //             Some(parent_block) => parent_block,
            //             None => unreachable!()
            //         };
            //         last_sortitioned_block = Some(parent_block.clone());
            //         leader_tenure = node.initiate_new_tenure(&parent_block);
            //     } 
            // }

            // if artefacts_from_tenure.is_some() {

            //     for node in self.nodes.iter_mut() {
            //         let (anchored_block, microblocks, parent_block) = artefacts_from_tenure.clone().unwrap();
        
            //         node.process_tenure(anchored_block.unwrap(), last_sortitioned_block.clone().unwrap(), microblocks, burn_db);

            //         break; // todo(ludo): get rid of this.
            //     }
            // } else {
            //     println!("NO SORTITION");
            // }





            for node in self.nodes.iter_mut() {

                let (sortitioned_block, won_sortition) = node.process_burnchain_block(&burnchain_block, &ops);

                if artefacts_from_tenure.is_none() {
                    continue;
                }

                last_sortitioned_block = sortitioned_block;

                let (anchored_block, microblocks, parent_block) = artefacts_from_tenure.clone().unwrap();
        
                node.process_tenure(anchored_block.unwrap(), last_sortitioned_block.clone().unwrap(), microblocks, burn_db);

                if won_sortition {
                    // This node is in charge of the new tenure
                    let parent_block = last_sortitioned_block.clone().unwrap();
                    // match sortitioned_block {
                    //     Some(parent_block) => parent_block,
                    //     None => unreachable!()
                    // };
                    leader_tenure = node.initiate_new_tenure(&parent_block);
                } 

                break; // todo(ludo): get rid of this.
            }


            // let tenure_artefacts = {
                
            //     let mut leader_tenure = None;

            //     // Dispatch incoming block to the nodes            
            //     for node in self.nodes.iter_mut() {
    
            //         let won_sortition = node.process_burnchain_block(&burnchain_block, &ops);
    
            //         // todo(ludo): refactor (at least naming)
            //         let parent_block = match (won_sortition, bootstrap_chain) {
            //             (true, _) => node.last_sortitioned_block.clone().unwrap(),
            //             (false, true) => {
            //                 bootstrap_chain = false;
            //                 SortitionedBlock::genesis()
            //             },
            //             (false, false) => { continue; },
            //         };


            //         println!("About to initiate new tenure with {:?}", parent_block);
            //         // Initiate and detach a new tenure targeting the initial sortition hash
            //         leader_tenure = Some(node.initiate_new_tenure(parent_block));
            //     }
    
            //     if leader_tenure.is_none() {
            //         continue;
            //     }
    
            //     // run tenure
            //     // get blocks + micro-blocks
            //     leader_tenure.unwrap().run()
            // };

            // // Dispatch tenure artefacts (anchored_block + microblocks) to the other nodes            
            // for node in self.nodes.iter_mut() {
            //     let (anchored_block, microblocks) = tenure_artefacts.clone();

            //     node.process_tenure(anchored_block, microblocks);

            //     node.maintain_leadership_eligibility();
            // }
        }
    }

    pub fn bootstrap(&mut self) {

    }

    pub fn tear_down(&self) {
        // todo(ludo): Clean dirs
    }
}
