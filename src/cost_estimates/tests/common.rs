use crate::chainstate::burn::ConsensusHash;
use crate::chainstate::stacks::db::{StacksEpochReceipt, StacksHeaderInfo};
use crate::chainstate::stacks::events::StacksTransactionReceipt;
use clarity::vm::costs::ExecutionCost;
use stacks_common::types::chainstate::TrieHash;
use stacks_common::types::chainstate::{BlockHeaderHash, BurnchainHeaderHash, StacksWorkScore};
use stacks_common::util::hash::{to_hex, Hash160, MerkleTree, Sha512Trunc256Sum};
use stacks_common::util::vrf::VRFProof;

use crate::chainstate::stacks::db::blocks::MessageSignatureList;
use crate::chainstate::stacks::{
    CoinbasePayload, StacksBlockHeader, StacksTransaction, TokenTransferMemo, TransactionAuth,
    TransactionContractCall, TransactionPayload, TransactionSpendingCondition, TransactionVersion,
};
use crate::core::StacksEpochId;

/// Make a block receipt from `tx_receipts` with some dummy values filled for test.
#[cfg(test)]
pub fn make_block_receipt(tx_receipts: Vec<StacksTransactionReceipt>) -> StacksEpochReceipt {
    StacksEpochReceipt {
        header: StacksHeaderInfo {
            anchored_header: StacksBlockHeader {
                version: 1,
                total_work: StacksWorkScore { burn: 1, work: 1 },
                proof: VRFProof::empty(),
                parent_block: BlockHeaderHash([0; 32]),
                parent_microblock: BlockHeaderHash([0; 32]),
                parent_microblock_sequence: 0,
                tx_merkle_root: Sha512Trunc256Sum([0; 32]),
                state_index_root: TrieHash([0; 32]),
                withdrawal_merkle_root: Sha512Trunc256Sum([0; 32]),
                microblock_pubkey_hash: Hash160([0; 20]),
                miner_signatures: MessageSignatureList::empty(),
            },
            microblock_tail: None,
            stacks_block_height: 1,
            index_root: TrieHash([0; 32]),
            consensus_hash: ConsensusHash([2; 20]),
            burn_header_hash: BurnchainHeaderHash([1; 32]),
            burn_header_height: 2,
            burn_header_timestamp: 2,
            anchored_block_size: 1,
            withdrawal_tree: MerkleTree::empty(),
        },
        tx_receipts,
        matured_rewards: vec![],
        matured_rewards_info: None,
        parent_microblocks_cost: ExecutionCost::zero(),
        anchored_block_cost: ExecutionCost::zero(),
        parent_burn_block_hash: BurnchainHeaderHash([0; 32]),
        parent_burn_block_height: 1,
        parent_burn_block_timestamp: 1,
        evaluated_epoch: StacksEpochId::Epoch20,
    }
}
