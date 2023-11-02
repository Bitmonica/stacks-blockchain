// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2023 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::ops::DerefMut;

use clarity::vm::ast::ASTRules;
use clarity::vm::costs::ExecutionCost;
use clarity::vm::database::BurnStateDB;
use clarity::vm::events::StacksTransactionEvent;
use clarity::vm::types::StacksAddressExtensions;
use lazy_static::{__Deref, lazy_static};
use rusqlite::types::{FromSql, FromSqlError};
use rusqlite::{params, Connection, OptionalExtension, ToSql, NO_PARAMS};
use sha2::{Digest as Sha2Digest, Sha512_256};
use stacks_common::codec::{
    read_next, write_next, Error as CodecError, StacksMessageCodec, MAX_MESSAGE_LEN,
};
use stacks_common::consts::{
    FIRST_BURNCHAIN_CONSENSUS_HASH, FIRST_STACKS_BLOCK_HASH, MINER_REWARD_MATURITY,
};
use stacks_common::types::chainstate::{
    BlockHeaderHash, BurnchainHeaderHash, ConsensusHash, StacksBlockId, TrieHash,
};
use stacks_common::types::StacksEpochId;
use stacks_common::util::get_epoch_time_secs;
use stacks_common::util::hash::{Hash160, MerkleHashFunc, MerkleTree, Sha512Trunc256Sum};
use stacks_common::util::retry::BoundReader;
use stacks_common::util::secp256k1::{MessageSignature};
use stacks_common::types::chainstate::StacksPrivateKey;
use stacks_common::types::chainstate::StacksPublicKey;
use stacks_common::types::PrivateKey;

use super::burn::db::sortdb::{SortitionHandleConn, SortitionHandleTx};
use super::burn::operations::{DelegateStxOp, StackStxOp, TransferStxOp};
use super::stacks::db::accounts::MinerReward;
use super::stacks::db::blocks::StagingUserBurnSupport;
use super::stacks::db::{
    ChainstateTx, ClarityTx, MinerPaymentSchedule, MinerPaymentTxFees, MinerRewardInfo,
    StacksBlockHeaderTypes, StacksDBTx, StacksEpochReceipt, StacksHeaderInfo,
};
use super::stacks::events::StacksTransactionReceipt;
use super::stacks::{
    Error as ChainstateError, StacksBlock, StacksBlockHeader, StacksMicroblock, StacksTransaction,
    TenureChangeError, TenureChangePayload, TransactionPayload,
};
use crate::burnchains::PoxConstants;
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::stacks::db::StacksChainState;
use crate::chainstate::stacks::{MINER_BLOCK_CONSENSUS_HASH, MINER_BLOCK_HEADER_HASH};
use crate::clarity_vm::clarity::{ClarityInstance, PreCommitClarityBlock};
use crate::clarity_vm::database::SortitionDBRef;
use crate::monitoring;
use crate::util_lib::db::{
    query_row_panic, query_rows, u64_to_sql, DBConn, Error as DBError, FromRow,
};

use crate::core::BOOT_BLOCK_HASH;

use crate::chainstate::coordinator::BlockEventDispatcher;
use crate::chainstate::coordinator::Error;

use crate::net::Error as net_error;

pub mod coordinator;
pub mod miner;

#[cfg(test)]
pub mod tests;

pub const NAKAMOTO_BLOCK_VERSION: u8 = 0;

define_named_enum!(HeaderTypeNames {
    Nakamoto("nakamoto"),
    Epoch2("epoch2"),
});

impl ToSql for HeaderTypeNames {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.get_name_str().to_sql()
    }
}

impl FromSql for HeaderTypeNames {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        Self::lookup_by_name(value.as_str()?).ok_or_else(|| FromSqlError::InvalidType)
    }
}

lazy_static! {
    pub static ref FIRST_STACKS_BLOCK_ID: StacksBlockId = StacksBlockId::new(&FIRST_BURNCHAIN_CONSENSUS_HASH, &FIRST_STACKS_BLOCK_HASH);

    pub static ref NAKAMOTO_CHAINSTATE_SCHEMA_1: Vec<String> = vec![
    r#"
      -- Table for staging nakamoto blocks
      -- TODO: this goes into its own DB at some point
      CREATE TABLE nakamoto_staging_blocks (
                     -- SHA512/256 hash of this block
                     block_hash TEXT NOT NULL,
                     -- the consensus hash of the burnchain block that selected this block's **tenure**
                     consensus_hash TEXT NOT NULL,
                     -- the parent index_block_hash
                     parent_block_id TEXT NOT NULL,

                     -- has the burnchain block with this block's `consensus_hash` been processed?
                     burn_attachable INT NOT NULL,
                     -- has the parent Stacks block been processed?
                     stacks_attachable INT NOT NULL,
                     -- set to 1 if this block can never be attached
                     orphaned INT NOT NULL,
                     -- has this block been processed?
                     processed INT NOT NULL,

                     height INT NOT NULL,

                     -- used internally -- this is the StacksBlockId of this block's consensus hash and block hash
                     index_block_hash TEXT NOT NULL,
                     -- how long the block was in-flight
                     download_time INT NOT NULL,
                     -- when this block was stored
                     arrival_time INT NOT NULL,
                     -- when this block was processed
                     processed_time INT NOT NULL,

                     -- block data
                     data BLOB NOT NULL,
                    
                     PRIMARY KEY(block_hash,consensus_hash)
    );"#.into(),
    r#"
      -- Table for Nakamoto block headers
      CREATE TABLE nakamoto_block_headers (
          -- The following fields all correspond to entries in the StacksHeaderInfo struct
                     block_height INTEGER NOT NULL,
                     -- root hash of the internal, not-consensus-critical MARF that allows us to track chainstate/fork metadata
                     index_root TEXT NOT NULL,
                     -- burn header hash corresponding to the consensus hash (NOT guaranteed to be unique, since we can 
                     --    have 2+ blocks per burn block if there's a PoX fork)
                     burn_header_hash TEXT NOT NULL,
                     -- height of the burnchain block header that generated this consensus hash
                     burn_header_height INT NOT NULL,
                     -- timestamp from burnchain block header that generated this consensus hash
                     burn_header_timestamp INT NOT NULL,
                     -- size of this block, in bytes.
                     -- encoded as TEXT for compatibility
                     block_size TEXT NOT NULL,
          -- The following fields all correspond to entries in the NakamotoBlockHeader struct
                     version INTEGER NOT NULL,
                     -- this field is the total number of blocks in the chain history (including this block)
                     chain_length INTEGER NOT NULL,
                     -- this field is the total amount of BTC spent in the chain history (including this block)
                     burn_spent INTEGER NOT NULL,
                     -- the consensus hash of the burnchain block that selected this block's tenure
                     consensus_hash TEXT NOT NULL,
                     -- the parent StacksBlockId
                     parent_block_id TEXT NOT NULL,
                     -- Merkle root of a Merkle tree constructed out of all the block's transactions
                     tx_merkle_root TEXT NOT NULL,
                     -- root hash of the Stacks chainstate MARF
                     state_index_root TEXT NOT NULL,
                     -- miner's signature over the block
                     miner_signature TEXT NOT NULL,
                     -- stackers' signature over the block
                     stacker_signature TEXT NOT NULL,
          -- The following fields are not part of either the StacksHeaderInfo struct
          --   or its contained NakamotoBlockHeader struct, but are used for querying
                     -- what kind of header this is (nakamoto or stacks 2.x)
                     header_type TEXT NOT NULL,
                     -- hash of the block
                     block_hash TEXT NOT NULL,
                     -- index_block_hash is the hash of the block hash and consensus hash of the burn block that selected it, 
                     -- and is guaranteed to be globally unique (across all Stacks forks and across all PoX forks).
                     -- index_block_hash is the block hash fed into the MARF index.
                     index_block_hash TEXT NOT NULL,
                     -- the ExecutionCost of the block
                     cost TEXT NOT NULL,
                     -- the total cost up to and including this block in the current tenure
                     total_tenure_cost TEXT NOT NULL,
                     -- this field is the total number of *tenures* in the chain history (including this tenure),
                     -- as of the _end_ of this block.  A block can contain multiple TenureChanges; if so, then this
                     -- is the height of the _last_ TenureChange.
                     tenure_height INTEGER NOT NULL,
                     -- this field is true if this is the first block of a new tenure
                     tenure_changed INTEGER NOT NULL,
                     -- this field tracks the total tx fees so far in this tenure. it is a text-serialized u128
                     tenure_tx_fees TEXT NOT NULL,
              PRIMARY KEY(consensus_hash,block_hash)
          );
    "#.into(),
        format!(
            r#"ALTER TABLE payments
               ADD COLUMN schedule_type TEXT NOT NULL DEFAULT "{}";
            "#,
            HeaderTypeNames::Epoch2.get_name_str()),
        r#"
        UPDATE db_config SET version = "4";
        "#.into(),
    ];
}

/// Result of preparing to produce or validate a block
pub struct SetupBlockResult<'a, 'b> {
    /// Handle to the ClarityVM
    pub clarity_tx: ClarityTx<'a, 'b>,
    /// Transaction receipts from any Stacks-on-Bitcoin transactions and epoch transition events
    pub tx_receipts: Vec<StacksTransactionReceipt>,
    /// Miner rewards that can be paid now: (this-miner-reward, parent-miner-reward, miner-info)
    pub matured_miner_rewards_opt: Option<(MinerReward, MinerReward, MinerRewardInfo)>,
    /// Epoch in which this block was set up
    pub evaluated_epoch: StacksEpochId,
    /// Whether or not we applied an epoch transition in this block
    pub applied_epoch_transition: bool,
    /// stack-stx Stacks-on-Bitcoin txs
    pub burn_stack_stx_ops: Vec<StackStxOp>,
    /// transfer-stx Stacks-on-Bitcoin txs
    pub burn_transfer_stx_ops: Vec<TransferStxOp>,
    /// delegate-stx Stacks-on-Bitcoin txs
    pub burn_delegate_stx_ops: Vec<DelegateStxOp>,
    /// STX auto-unlock events from PoX
    pub auto_unlock_events: Vec<StacksTransactionEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NakamotoBlockHeader {
    pub version: u8,
    /// The total number of StacksBlock and NakamotoBlocks preceding
    /// this block in this block's history.
    pub chain_length: u64,
    /// Total amount of BTC spent producing the sortition that
    /// selected this block's miner.
    pub burn_spent: u64,
    /// The consensus hash of the burnchain block that selected this tenure.  The consensus hash
    /// uniquely identifies this tenure, including across all Bitcoin forks.
    pub consensus_hash: ConsensusHash,
    /// The index block hash of the immediate parent of this block.
    /// This is the hash of the parent block's hash and consensus hash.
    pub parent_block_id: StacksBlockId,
    /// The root of a SHA512/256 merkle tree over all this block's
    /// contained transactions
    pub tx_merkle_root: Sha512Trunc256Sum,
    /// The MARF trie root hash after this block has been processed
    pub state_index_root: TrieHash,
    /// Recoverable ECDSA signature from the tenure's miner.
    pub miner_signature: MessageSignature,
    /// Recoverable ECDSA signature from the stacker set active during the tenure.
    /// TODO: This is a placeholder
    pub stacker_signature: MessageSignature,
}

#[derive(Debug, Clone)]
pub struct NakamotoBlock {
    pub header: NakamotoBlockHeader,
    pub txs: Vec<StacksTransaction>,
}

pub struct NakamotoChainState;

impl FromRow<NakamotoBlockHeader> for NakamotoBlockHeader {
    fn from_row(row: &rusqlite::Row) -> Result<NakamotoBlockHeader, DBError> {
        let version = row.get("version")?;
        let chain_length_i64: i64 = row.get("chain_length")?;
        let chain_length = chain_length_i64
            .try_into()
            .map_err(|_| DBError::ParseError)?;
        let burn_spent_i64: i64 = row.get("burn_spent")?;
        let burn_spent = burn_spent_i64.try_into().map_err(|_| DBError::ParseError)?;
        let consensus_hash = row.get("consensus_hash")?;
        let parent_block_id = row.get("parent_block_id")?;
        let tx_merkle_root = row.get("tx_merkle_root")?;
        let state_index_root = row.get("state_index_root")?;
        let stacker_signature = row.get("stacker_signature")?;
        let miner_signature = row.get("miner_signature")?;

        Ok(NakamotoBlockHeader {
            version,
            chain_length,
            burn_spent,
            consensus_hash,
            parent_block_id,
            tx_merkle_root,
            state_index_root,
            stacker_signature,
            miner_signature,
        })
    }
}

impl StacksMessageCodec for NakamotoBlockHeader {
    fn consensus_serialize<W: std::io::Write>(&self, fd: &mut W) -> Result<(), CodecError> {
        write_next(fd, &self.version)?;
        write_next(fd, &self.chain_length)?;
        write_next(fd, &self.burn_spent)?;
        write_next(fd, &self.consensus_hash)?;
        write_next(fd, &self.parent_block_id)?;
        write_next(fd, &self.tx_merkle_root)?;
        write_next(fd, &self.state_index_root)?;
        write_next(fd, &self.miner_signature)?;
        write_next(fd, &self.stacker_signature)?;

        Ok(())
    }

    fn consensus_deserialize<R: std::io::Read>(fd: &mut R) -> Result<Self, CodecError> {
        Ok(NakamotoBlockHeader {
            version: read_next(fd)?,
            chain_length: read_next(fd)?,
            burn_spent: read_next(fd)?,
            consensus_hash: read_next(fd)?,
            parent_block_id: read_next(fd)?,
            tx_merkle_root: read_next(fd)?,
            state_index_root: read_next(fd)?,
            miner_signature: read_next(fd)?,
            stacker_signature: read_next(fd)?,
        })
    }
}

impl NakamotoBlockHeader {
    /// Calculate the message digest to sign.
    /// This includes all fields _except_ the signatures.
    pub fn signature_hash(&self) -> Result<Sha512Trunc256Sum, CodecError> {
        let mut hasher = Sha512_256::new();
        let fd = &mut hasher;
        write_next(fd, &self.version)?;
        write_next(fd, &self.chain_length)?;
        write_next(fd, &self.burn_spent)?;
        write_next(fd, &self.consensus_hash)?;
        write_next(fd, &self.parent_block_id)?;
        write_next(fd, &self.tx_merkle_root)?;
        write_next(fd, &self.state_index_root)?;
        Ok(Sha512Trunc256Sum::from_hasher(hasher))
    }

    pub fn recover_miner_pk(&self) -> Option<StacksPublicKey> {
        let signed_hash = self.signature_hash().ok()?;
        let recovered_pk =
            StacksPublicKey::recover_to_pubkey(signed_hash.bits(), &self.miner_signature)
                .ok()?;

        Some(recovered_pk)
    }

    pub fn block_hash(&self) -> BlockHeaderHash {
        BlockHeaderHash::from_serializer(self)
            .expect("BUG: failed to serialize block header hash struct")
    }

    pub fn block_id(&self) -> StacksBlockId {
        StacksBlockId::new(&self.consensus_hash, &self.block_hash())
    }

    pub fn is_first_mined(&self) -> bool {
        StacksBlockHeader::is_first_index_block_hash(&self.parent_block_id)
    }

    /// Sign the block header by the miner
    pub fn sign_miner(&mut self, privk: &StacksPrivateKey) -> Result<(), ChainstateError> {
        let sighash = self.signature_hash()?.0;
        let sig = privk
            .sign(&sighash)
            .map_err(|se| net_error::SigningError(se.to_string()))?;
        self.miner_signature = sig;
        Ok(())
    }

    /// Make an "empty" header whose block data needs to be filled in.
    /// This is used by the miner code.
    pub fn from_parent_empty(
        chain_length: u64,
        burn_spent: u64,
        consensus_hash: ConsensusHash,
        parent_block_id: StacksBlockId,
    ) -> NakamotoBlockHeader {
        NakamotoBlockHeader {
            version: NAKAMOTO_BLOCK_VERSION,
            chain_length,
            burn_spent,
            consensus_hash,
            parent_block_id,
            tx_merkle_root: Sha512Trunc256Sum([0u8; 32]),
            state_index_root: TrieHash([0u8; 32]),
            miner_signature: MessageSignature::empty(),
            stacker_signature: MessageSignature::empty()
        }
    }

    /// Make a completely empty header
    pub fn empty() -> NakamotoBlockHeader {
        NakamotoBlockHeader {
            version: 0,
            chain_length: 0,
            burn_spent: 0,
            consensus_hash: ConsensusHash([0u8; 20]),
            parent_block_id: StacksBlockId([0u8; 32]),
            tx_merkle_root: Sha512Trunc256Sum([0u8; 32]),
            state_index_root: TrieHash([0u8; 32]),
            miner_signature: MessageSignature::empty(),
            stacker_signature: MessageSignature::empty()
        }
    }
    
    /// Make a genesis header (testing only)
    pub fn genesis() -> NakamotoBlockHeader {
        NakamotoBlockHeader {
            version: 0,
            chain_length: 0,
            burn_spent: 0,
            consensus_hash: FIRST_BURNCHAIN_CONSENSUS_HASH.clone(),
            parent_block_id: StacksBlockId(BOOT_BLOCK_HASH.0.clone()),
            tx_merkle_root: Sha512Trunc256Sum([0u8; 32]),
            state_index_root: TrieHash([0u8; 32]),
            miner_signature: MessageSignature::empty(),
            stacker_signature: MessageSignature::empty()
        }
    }
}

impl NakamotoBlock {
    /// Did the stacks tenure change on this nakamoto block? i.e., does this block
    ///  include a TenureChange transaction?
    pub fn tenure_changed(&self, parent: &StacksBlockId) -> bool {
        // Find all txs that have TenureChange payload
        let tenure_changes = self
            .txs
            .iter()
            .filter_map(|tx| match &tx.payload {
                TransactionPayload::TenureChange(payload) => Some(payload),
                _ => None,
            })
            .collect::<Vec<_>>();

        if tenure_changes.len() > 1 {
            warn!(
                "Block contains multiple TenureChange transactions";
                "tenure_change_txs" => tenure_changes.len(),
                "parent_block_id" => %self.header.parent_block_id,
                "consensus_hash" => %self.header.consensus_hash,
            );
        }

        let validate = |tc: &TenureChangePayload| -> Result<(), TenureChangeError> {
            if tc.previous_tenure_end != *parent {
                return Err(TenureChangeError::PreviousTenureInvalid);
            }

            tc.validate()
        };

        // Return true if there is a valid TenureChange
        tenure_changes
            .iter()
            .find(|tc| validate(tc).is_ok())
            .is_some()
    }

    pub fn is_first_mined(&self) -> bool {
        self.header.is_first_mined()
    }

    /// Get the coinbase transaction in Nakamoto.
    /// It's the first non-TenureChange transaction
    /// (and, all preceding transactions _must_ be TenureChanges)
    pub fn get_coinbase_tx(&self) -> Option<&StacksTransaction> {
        let mut tx_ref = None;
        for tx in self.txs.iter() {
            if let TransactionPayload::TenureChange(..) = &tx.payload {
                if tx_ref.is_none() {
                    continue;
                }
                // non-TenureChange tx precedes a coinbase, so there's no valid coinbase.
                // (a coinbase in any other position is invalid anyway).
                return None;
            }
            else if let TransactionPayload::Coinbase(..) = &tx.payload {
                if tx_ref.is_none() {
                    // contender
                    tx_ref = Some(tx);
                }
                else {
                    // multiple coinbases, so none of them are valid.
                    return None;
                }
            }
            else if tx_ref.is_none() {
                // non-Coinbase and non-TenureChange tx, so there's no valid coinbase.
                // (a coinbase in any other position is invalid anyway)
                return None;
            }
        }
        tx_ref
    }

    pub fn block_id(&self) -> StacksBlockId {
        self.header.block_id()
    }
}

impl NakamotoChainState {
    /// Notify the staging database that a given stacks block has been processed.
    /// This will update the attachable status for children blocks, as well as marking the stacks
    ///  block itself as processed.
    pub fn set_block_processed(
        staging_db_tx: &rusqlite::Transaction,
        block: &StacksBlockId,
    ) -> Result<(), ChainstateError> {
        let update_dependents = "UPDATE nakamoto_staging_blocks SET stacks_attachable = 1
                                 WHERE parent_block_id = ?";
        staging_db_tx.execute(&update_dependents, &[&block])?;

        let clear_staged_block =
            "UPDATE nakamoto_staging_blocks SET processed = 1, processed_time = ?2
                                  WHERE index_block_hash = ?1";
        staging_db_tx.execute(
            &clear_staged_block,
            params![&block, &u64_to_sql(get_epoch_time_secs())?],
        )?;

        Ok(())
    }

    /// Modify the staging database that a given stacks block can never be processed.
    /// This will update the attachable status for children blocks, as well as marking the stacks
    ///  block itself as orphaned.
    pub fn set_block_orphaned(
        staging_db_tx: &rusqlite::Transaction,
        block: &StacksBlockId,
    ) -> Result<(), ChainstateError> {
        let update_dependents =
            "UPDATE nakamoto_staging_blocks SET stacks_attachable = 0, orphaned = 1
                                 WHERE parent_block_id = ?";
        staging_db_tx.execute(&update_dependents, &[&block])?;

        let clear_staged_block =
            "UPDATE nakamoto_staging_blocks SET processed = 1, processed_time = ?2, orphaned = 1
                                  WHERE index_block_hash = ?1";
        staging_db_tx.execute(
            &clear_staged_block,
            params![&block, &u64_to_sql(get_epoch_time_secs())?],
        )?;

        Ok(())
    }

    /// Notify the staging database that a given burn block has been processed.
    /// This is required for staged blocks to be eligible for processing.
    pub fn set_burn_block_processed(
        staging_db_tx: &rusqlite::Transaction,
        consensus_hash: &ConsensusHash,
    ) -> Result<(), ChainstateError> {
        let update_dependents = "UPDATE nakamoto_staging_blocks SET burn_attachable = 1
                                 WHERE consensus_hash = ?";
        staging_db_tx.execute(&update_dependents, &[consensus_hash])?;

        Ok(())
    }

    /// Find the next ready-to-process Nakamoto block, given a connection to the staging blocks DB.
    /// Returns (the block, the size of the block)
    pub fn next_ready_nakamoto_block(
        staging_db_conn: &Connection,
    ) -> Result<Option<(NakamotoBlock, u64)>, ChainstateError> {
        let query = "SELECT data FROM nakamoto_staging_blocks
                     WHERE burn_attachable = 1
                       AND stacks_attachable = 1
                       AND orphaned = 0
                       AND processed = 0
                     ORDER BY height ASC";
        staging_db_conn
            .query_row_and_then(query, NO_PARAMS, |row| {
                let data: Vec<u8> = row.get("data")?;
                let block = NakamotoBlock::consensus_deserialize(&mut data.as_slice())?;
                Ok(Some((block, data.len() as u64)))
            })
            .or_else(|e| {
                if let ChainstateError::DBError(DBError::SqliteError(
                    rusqlite::Error::QueryReturnedNoRows,
                )) = e
                {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
    }

    /// Extract and parse a nakamoto block from the DB, and verify its integrity.
    fn load_nakamoto_block(
        staging_db_conn: &Connection,
        consensus_hash: &ConsensusHash,
        block_hash: &BlockHeaderHash,
    ) -> Result<Option<NakamotoBlock>, ChainstateError> {
        let query = "SELECT data FROM nakamoto_staging_blocks WHERE consensus_hash = ?1 AND block_hash = ?2";
        staging_db_conn
            .query_row_and_then(
                query,
                rusqlite::params![consensus_hash, block_hash],
                |row| {
                    let data: Vec<u8> = row.get("data")?;
                    let block = NakamotoBlock::consensus_deserialize(&mut data.as_slice())?;
                    if &block.header.block_hash() != block_hash {
                        panic!(
                            "Staging DB corruption: expected {}, got {}",
                            &block_hash,
                            &block.header.block_hash()
                        );
                    }
                    Ok(Some(block))
                },
            )
            .or_else(|e| {
                if let ChainstateError::DBError(DBError::SqliteError(
                    rusqlite::Error::QueryReturnedNoRows,
                )) = e
                {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
    }

    /// Process the next ready block.
    /// If there exists a ready Nakamoto block, then this method returns Ok(Some(..)) with the
    /// receipt.  Otherwise, it returns Ok(None).
    ///
    /// It returns Err(..) on DB error, or if the child block does not connect to the parent.
    /// The caller should keep calling this until it gets Ok(None)
    pub fn process_next_nakamoto_block<'a, T: BlockEventDispatcher>(
        stacks_chain_state: &mut StacksChainState,
        sort_tx: &mut SortitionHandleTx,
        dispatcher_opt: Option<&'a T>,
    ) -> Result<Option<StacksEpochReceipt>, ChainstateError> {
        let (mut chainstate_tx, clarity_instance) = stacks_chain_state.chainstate_tx_begin()?;
        let Some((next_ready_block, block_size)) =
            Self::next_ready_nakamoto_block(&chainstate_tx.tx)?
        else {
            // no more blocks
            return Ok(None);
        };

        let block_id = next_ready_block.block_id();

        // find corresponding snapshot
        let next_ready_block_snapshot = SortitionDB::get_block_snapshot_consensus(
            sort_tx,
            &next_ready_block.header.consensus_hash,
        )?
        .expect(&format!(
            "CORRUPTION: staging Nakamoto block {}/{} does not correspond to a burn block",
            &next_ready_block.header.consensus_hash,
            &next_ready_block.header.block_hash()
        ));

        debug!("Process staging Nakamoto block";
               "consensus_hash" => %next_ready_block.header.consensus_hash,
               "block_hash" => %next_ready_block.header.block_hash(),
               "burn_block_hash" => %next_ready_block_snapshot.burn_header_hash
        );

        // find parent header
        let Some(parent_header_info) =
            Self::get_block_header(&chainstate_tx.tx, &next_ready_block.header.parent_block_id)?
        else {
            // no parent; cannot process yet
            debug!("Cannot process Nakamoto block: missing parent header";
                   "consensus_hash" => %next_ready_block.header.consensus_hash,
                   "block_hash" => %next_ready_block.header.block_hash(),
                   "parent_block_id" => %next_ready_block.header.parent_block_id
            );
            return Ok(None);
        };

        // sanity check -- must attach to parent
        let parent_block_id = StacksBlockId::new(
            &parent_header_info.consensus_hash,
            &parent_header_info.anchored_header.block_hash(),
        );
        if parent_block_id != next_ready_block.header.parent_block_id {
            let msg = "Discontinuous Nakamoto Stacks block";
            warn!("{}", &msg;
                  "child parent_block_id" => %next_ready_block.header.parent_block_id,
                  "expected parent_block_id" => %parent_block_id
            );
            let _ = Self::set_block_orphaned(&chainstate_tx.tx, &block_id);
            chainstate_tx.commit()?;
            return Err(ChainstateError::InvalidStacksBlock(msg.into()));
        }

        // find commit and sortition burns if this is a tenure-start block
        // TODO: store each *tenure*
        let (commit_burn, sortition_burn) = if next_ready_block.tenure_changed(&parent_block_id) {
            // find block-commit to get commit-burn
            let block_commit = sort_tx
                .get_block_commit(
                    &next_ready_block_snapshot.winning_block_txid,
                    &next_ready_block_snapshot.sortition_id,
                )?
                .expect("FATAL: no block-commit for tenure-start block");

            let sort_burn = SortitionDB::get_block_burn_amount(
                sort_tx.deref().deref(),
                &next_ready_block_snapshot,
            )?;
            (block_commit.burn_fee, sort_burn)
        } else {
            (0, 0)
        };

        // attach the block to the chain state and calculate the next chain tip.
        let pox_constants = sort_tx.context.pox_constants.clone();
        let (receipt, clarity_commit) = match NakamotoChainState::append_block(
            &mut chainstate_tx,
            clarity_instance,
            sort_tx,
            &pox_constants,
            &parent_header_info,
            &next_ready_block_snapshot.burn_header_hash,
            next_ready_block_snapshot
                .block_height
                .try_into()
                .expect("Failed to downcast u64 to u32"),
            next_ready_block_snapshot.burn_header_timestamp,
            &next_ready_block,
            block_size,
            commit_burn,
            sortition_burn,
        ) {
            Ok(next_chain_tip_info) => next_chain_tip_info,
            Err(e) => {
                test_debug!(
                    "Failed to append {}/{}: {:?}",
                    &next_ready_block.header.consensus_hash,
                    &next_ready_block.header.block_hash(),
                    &e
                );
                let _ = Self::set_block_orphaned(&chainstate_tx.tx, &block_id);
                chainstate_tx.commit()?;
                return Err(e);
            }
        };

        assert_eq!(
            receipt.header.anchored_header.block_hash(),
            next_ready_block.header.block_hash()
        );
        assert_eq!(
            receipt.header.consensus_hash,
            next_ready_block.header.consensus_hash
        );

        NakamotoChainState::set_block_processed(&chainstate_tx.tx, &block_id)?;

        // set stacks block accepted
        sort_tx.set_stacks_block_accepted(
            &next_ready_block.header.consensus_hash,
            &next_ready_block.header.block_hash(),
            next_ready_block.header.chain_length,
        )?;

        // announce the block, if we're connected to an event dispatcher
        if let Some(dispatcher) = dispatcher_opt {
            dispatcher.announce_block(
                (
                    next_ready_block,
                    parent_header_info.anchored_header.block_hash(),
                )
                    .into(),
                &receipt.header.clone(),
                &receipt.tx_receipts,
                &parent_block_id,
                next_ready_block_snapshot.winning_block_txid,
                &receipt.matured_rewards,
                receipt.matured_rewards_info.as_ref(),
                receipt.parent_burn_block_hash,
                receipt.parent_burn_block_height,
                receipt.parent_burn_block_timestamp,
                &receipt.anchored_block_cost,
                &receipt.parent_microblocks_cost,
                &pox_constants,
            );
        }

        // this will panic if the Clarity commit fails.
        clarity_commit.commit();
        chainstate_tx.commit()
            .unwrap_or_else(|e| {
                error!("Failed to commit chainstate transaction after committing Clarity block. The chainstate database is now corrupted.";
                       "error" => ?e);
                panic!()
            });

        Ok(Some(receipt))
    }

    /// Accept a Nakamoto block into the staging blocks DB.
    /// Fails if:
    /// * the public key cannot be recovered from the miner's signature
    /// * the stackers during the tenure didn't sign it
    /// * a DB error occurs
    /// Does nothing if:
    /// * we already have the block
    /// Returns true if we stored the block; false if not.
    pub fn accept_block(
        block: NakamotoBlock,
        sortdb: &SortitionHandleConn,
        staging_db_tx: &rusqlite::Transaction,
    ) -> Result<bool, ChainstateError> {
        // do nothing if we already have this block
        if let Some(_) = Self::get_block_header(&staging_db_tx, &block.header.block_id())? {
            debug!("Already have block {}", &block.header.block_id());
            return Ok(false)
        }

        // identify the winning block-commit
        let sortition = SortitionDB::get_block_snapshot_consensus(sortdb, &block.header.consensus_hash)?
            .ok_or(ChainstateError::NoSuchBlockError)
            .map_err(|e| {
                warn!("No block snapshot for {}", &block.header.consensus_hash);
                e
            })?;

        let block_commit = SortitionDB::get_block_commit(sortdb, &sortition.winning_block_txid, &sortition.sortition_id)?
            .ok_or(ChainstateError::NoSuchBlockError)
            .map_err(|e| {
                warn!("No block commit {} off of sortition tip {}", &sortition.winning_block_txid, &sortition.sortition_id);
                e
            })?;

        // identify the leader key for this block-commit
        let leader_key = SortitionDB::get_leader_key_at(sortdb, u64::from(block_commit.key_block_ptr), u32::from(block_commit.key_vtxindex), &sortition.sortition_id)?
            .ok_or(ChainstateError::NoSuchBlockError)
            .map_err(|e| {
                warn!("No leader key at {},{} for block-commit {} off of sortition tip {}", block_commit.key_block_ptr, block_commit.key_vtxindex, &block_commit.txid, &sortition.sortition_id);
                e
            })?;

        let miner_pubkey_hash160 = leader_key.interpret_nakamoto_signing_key()
            .ok_or(ChainstateError::NoSuchBlockError)
            .map_err(|e| {
                warn!(
                    "Leader key did not contain a hash160 of the miner signing public key";
                    "leader_key" => format!("{:?}", &leader_key),
                );
                e
            })?;

        let recovered_miner_pubk = block.header.recover_miner_pk().ok_or_else(|| {
            warn!(
                "Nakamoto Stacks block downloaded with unrecoverable miner public key";
                "block_hash" => %block.header.block_hash(),
                "block_id" => %block.header.block_id(),
            );
            return ChainstateError::InvalidStacksBlock("Unrecoverable miner public key".into());
        })?;

        let recovered_miner_hash160 = Hash160::from_node_public_key(&recovered_miner_pubk);
        if recovered_miner_hash160 != miner_pubkey_hash160 {
            warn!(
                "Nakamoto Stacks block signature from {recovered_miner_pubk:?} mismatch: {recovered_miner_hash160} != {miner_pubkey_hash160} from leader-key";
                "block_hash" => %block.header.block_hash(),
                "block_id" => %block.header.block_id(),
                "leader_key" => format!("{:?}", &leader_key),
                "block_commit" => format!("{:?}", &block_commit)
            );
            return Err(ChainstateError::InvalidStacksBlock("Invalid miner signature".into()));
        }

        if !sortdb.expects_stacker_signature(
            &block.header.consensus_hash,
            &block.header.stacker_signature,
        )? {
            let msg = format!("Received block, signed by {recovered_miner_pubk:?}, but the stacker signature does not match the active stacking cycle");
            warn!("{}", msg);
            return Err(ChainstateError::InvalidStacksBlock(msg));
        }

        // if the burnchain block of this Stacks block's tenure has been processed, then it
        // is ready to be processed from the perspective of the burnchain
        let burn_attachable = sortdb.processed_block(&block.header.consensus_hash)?;

        // check if the parent Stacks Block ID has been processed. if so, then this block is stacks_attachable
        let stacks_attachable =
            // block is the first-ever mined (test only)
            block.is_first_mined()
            // block attaches to a processed nakamoto block
            || staging_db_tx.query_row(
                "SELECT 1 FROM nakamoto_staging_blocks WHERE index_block_hash = ? AND processed = 1 AND orphaned = 0",
                rusqlite::params![&block.header.parent_block_id],
                |_row| Ok(())
            ).optional()?.is_some()
            // block attaches to a Stacks epoch 2.x block, and there are no nakamoto blocks at all
            || (
                staging_db_tx.query_row(
                    "SELECT 1 FROM block_headers WHERE index_block_hash = ?",
                    rusqlite::params![&block.header.parent_block_id],
                    |_row| Ok(())
                ).optional()?.is_some()
                && staging_db_tx.query_row(
                    "SELECT 1 FROM nakamoto_block_headers LIMIT 1",
                    rusqlite::NO_PARAMS,
                    |_row| Ok(())
                ).optional()?.is_none()
               );

        let block_id = block.block_id();
        staging_db_tx.execute(
            "INSERT INTO nakamoto_staging_blocks (
                     block_hash,
                     consensus_hash,
                     parent_block_id,
                     burn_attachable,
                     stacks_attachable,
                     orphaned,
                     processed,

                     height,
                     index_block_hash,
                     download_time,
                     arrival_time,
                     processed_time,
                     data
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &block.header.block_hash(),
                &block.header.consensus_hash,
                &block.header.parent_block_id,
                if burn_attachable { 1 } else { 0 },
                if stacks_attachable { 1 } else { 0 },
                0,
                0,
                u64_to_sql(block.header.chain_length)?,
                &block_id,
                0,
                0,
                0,
                block.serialize_to_vec(),
            ],
        )?;

        Ok(true)
    }

    /// Create the block reward for a NakamotoBlock
    /// `coinbase_reward_ustx` is the total coinbase reward for this block, including any
    ///    accumulated rewards from missed sortitions or initial mining rewards.
    pub fn make_scheduled_miner_reward(
        mainnet: bool,
        epoch_id: StacksEpochId,
        parent_block_hash: &BlockHeaderHash,
        parent_consensus_hash: &ConsensusHash,
        block_hash: &BlockHeaderHash,
        block_consensus_hash: &ConsensusHash,
        block_height: u64,
        coinbase_tx: &StacksTransaction,
        parent_fees: u128,
        burnchain_commit_burn: u64,
        burnchain_sortition_burn: u64,
        coinbase_reward_ustx: u128,
    ) -> Result<MinerPaymentSchedule, ChainstateError> {
        let miner_auth = coinbase_tx.get_origin();
        let miner_addr = miner_auth.get_address(mainnet);

        let recipient = if epoch_id >= StacksEpochId::Epoch21 {
            // pay to tx-designated recipient, or if there is none, pay to the origin
            match coinbase_tx.try_as_coinbase() {
                Some((_, recipient_opt, _)) => recipient_opt
                    .cloned()
                    .unwrap_or(miner_addr.to_account_principal()),
                None => miner_addr.to_account_principal(),
            }
        } else {
            // pre-2.1, always pay to the origin
            miner_addr.to_account_principal()
        };

        // N.B. a `MinerPaymentSchedule` that pays to a contract can never be created before 2.1,
        // per the above check (and moreover, a Stacks block with a pay-to-alt-recipient coinbase would
        // not become valid until after 2.1 activates).
        let miner_reward = MinerPaymentSchedule {
            address: miner_addr,
            recipient,
            block_hash: block_hash.clone(),
            consensus_hash: block_consensus_hash.clone(),
            parent_block_hash: parent_block_hash.clone(),
            parent_consensus_hash: parent_consensus_hash.clone(),
            coinbase: coinbase_reward_ustx,
            tx_fees: MinerPaymentTxFees::Nakamoto { parent_fees },
            burnchain_commit_burn,
            burnchain_sortition_burn,
            miner: true,
            stacks_block_height: block_height,
            vtxindex: 0,
        };

        Ok(miner_reward)
    }

    /// Return the total ExecutionCost consumed during the tenure up to and including
    ///  `block`
    pub fn get_total_tenure_cost_at(
        conn: &Connection,
        block: &StacksBlockId,
    ) -> Result<Option<ExecutionCost>, ChainstateError> {
        let qry = "SELECT total_tenure_cost FROM nakamoto_block_headers WHERE index_block_hash = ?";
        conn.query_row(qry, &[block], |row| row.get(0))
            .optional()
            .map_err(ChainstateError::from)
    }

    /// Return the total transactions fees during the tenure up to and including
    ///  `block`
    pub fn get_total_tenure_tx_fees_at(
        conn: &Connection,
        block: &StacksBlockId,
    ) -> Result<Option<u128>, ChainstateError> {
        let qry = "SELECT tenure_tx_fees FROM nakamoto_block_headers WHERE index_block_hash = ?";
        let tx_fees_str: Option<String> =
            conn.query_row(qry, &[block], |row| row.get(0)).optional()?;
        tx_fees_str
            .map(|x| x.parse())
            .transpose()
            .map_err(|_| ChainstateError::DBError(DBError::ParseError))
    }

    /// Return a Nakamoto StacksHeaderInfo at a given tenure height in the fork identified by `tip_index_hash`.
    /// * For Stacks 2.x, this is the Stacks block's header
    /// * For Stacks 3.x (Nakamoto), this is the first block in the miner's tenure.
    pub fn get_header_by_tenure_height(
        tx: &mut StacksDBTx,
        tip_index_hash: &StacksBlockId,
        tenure_height: u64,
    ) -> Result<Option<StacksHeaderInfo>, ChainstateError> {
        // query for block header info at the tenure-height, then check if in fork
        let qry =
            "SELECT * FROM nakamoto_block_headers WHERE tenure_changed = 1 AND tenure_height = ?";
        let candidate_headers: Vec<StacksHeaderInfo> =
            query_rows(tx.tx(), qry, &[u64_to_sql(tenure_height)?])?;

        if candidate_headers.len() == 0 {
            // no nakamoto_block_headers at that tenure height, check if there's a stack block header where
            //   block_height = tenure_height
            let Some(ancestor_at_height) = tx
                .get_ancestor_block_hash(tenure_height, tip_index_hash)?
                .map(|ancestor| Self::get_block_header(tx.tx(), &ancestor))
                .transpose()?
                .flatten()
            else {
                return Ok(None);
            };
            // only return if it is an epoch-2 block, because that's
            // the only case where block_height can be interpreted as
            // tenure height.
            if ancestor_at_height.is_epoch_2_block() {
                return Ok(Some(ancestor_at_height));
            } else {
                return Ok(None);
            }
        }

        for candidate in candidate_headers.into_iter() {
            let Ok(Some(ancestor_at_height)) =
                tx.get_ancestor_block_hash(tenure_height, tip_index_hash)
            else {
                // if there's an error or no result, this candidate doesn't match, so try next candidate
                continue;
            };
            if ancestor_at_height == candidate.index_block_hash() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// Return the tenure height of `block` if it was a nakamoto block, or the
    ///  Stacks block height of `block` if it was an epoch-2 block
    ///
    /// In Stacks 2.x, the tenure height and block height are the
    /// same. A miner's tenure in Stacks 2.x is entirely encompassed
    /// in the single Bitcoin-anchored Stacks block they produce, as
    /// well as the microblock stream they append to it.
    pub fn get_tenure_height(
        conn: &Connection,
        block: &StacksBlockId,
    ) -> Result<Option<u64>, ChainstateError> {
        let nak_qry = "SELECT tenure_height FROM nakamoto_block_headers WHERE index_block_hash = ?";
        let opt_height: Option<i64> = conn
            .query_row(nak_qry, &[block], |row| row.get(0))
            .optional()?;
        if let Some(height) = opt_height {
            return Ok(Some(
                u64::try_from(height).map_err(|_| DBError::ParseError)?,
            ));
        }

        let epoch_2_qry = "SELECT block_height FROM block_headers WHERE index_block_hash = ?";
        let opt_height: Option<i64> = conn
            .query_row(epoch_2_qry, &[block], |row| row.get(0))
            .optional()?;
        opt_height
            .map(u64::try_from)
            .transpose()
            .map_err(|_| ChainstateError::DBError(DBError::ParseError))
    }

    /// Load block header (either Epoch-2 rules or Nakamoto) by `index_block_hash`
    pub fn get_block_header(
        conn: &Connection,
        index_block_hash: &StacksBlockId,
    ) -> Result<Option<StacksHeaderInfo>, ChainstateError> {
        let sql = "SELECT * FROM nakamoto_block_headers WHERE index_block_hash = ?1";
        let result = query_row_panic(conn, sql, &[&index_block_hash], || {
            "FATAL: multiple rows for the same block hash".to_string()
        })?;
        if result.is_some() {
            return Ok(result);
        }

        let sql = "SELECT * FROM block_headers WHERE index_block_hash = ?1";
        let result = query_row_panic(conn, sql, &[&index_block_hash], || {
            "FATAL: multiple rows for the same block hash".to_string()
        })?;

        Ok(result)
    }

    /// Load the canonical Stacks block header (either epoch-2 rules or Nakamoto)
    pub fn get_canonical_block_header(
        conn: &Connection,
        sortdb: &SortitionDB,
    ) -> Result<Option<StacksHeaderInfo>, ChainstateError> {
        let (consensus_hash, block_bhh) =
            SortitionDB::get_canonical_stacks_chain_tip_hash(sortdb.conn())?;
        let index_block_hash = StacksBlockId::new(&consensus_hash, &block_bhh);
        Self::get_block_header(conn, &index_block_hash)
    }

    /// Get the first block header in a Nakamoto tenure
    pub fn get_nakamoto_tenure_start_block_header(
        conn: &Connection,
        consensus_hash: &ConsensusHash,
    ) -> Result<Option<StacksHeaderInfo>, ChainstateError> {
        let sql = "SELECT * FROM nakamoto_block_headers WHERE consensus_hash = ?1 ORDER BY block_height ASC LIMIT 1";
        query_row_panic(conn, sql, &[&consensus_hash], || {
            "FATAL: multiple rows for the same consensus hash".to_string()
        })
        .map_err(ChainstateError::DBError)
    }

    /// Get the last block header in a Nakamoto tenure
    pub fn get_nakamoto_tenure_finish_block_header(
        conn: &Connection,
        consensus_hash: &ConsensusHash,
    ) -> Result<Option<StacksHeaderInfo>, ChainstateError> {
        let sql = "SELECT * FROM nakamoto_block_headers WHERE consensus_hash = ?1 ORDER BY block_height DESC LIMIT 1";
        query_row_panic(conn, sql, &[&consensus_hash], || {
            "FATAL: multiple rows for the same consensus hash".to_string()
        })
        .map_err(ChainstateError::DBError)
    }

    /// Get the status of a Nakamoto block.
    /// Returns Some(accepted?, orphaned?) on success
    /// Returns None if there's no such block
    /// Returns Err on DBError
    pub fn get_nakamoto_block_status(
        conn: &Connection,
        consensus_hash: &ConsensusHash,
        block_hash: &BlockHeaderHash
    ) -> Result<Option<(bool, bool)>, ChainstateError> {
        let sql = "SELECT (processed, orphaned) FROM nakamoto_block_headers WHERE consensus_hash = ?1 AND block_hash = ?2";
        let args: &[&dyn ToSql] = &[consensus_hash, block_hash];
        Ok(query_row_panic(conn, sql, args, || {
            "FATAL: multiple rows for the same consensus hash and block hash".to_string()
        })
        .map_err(ChainstateError::DBError)?
        .map(|(processed, orphaned): (u32, u32)| (processed != 0, orphaned != 0)))
    }

    /// Insert a nakamoto block header that is paired with an
    /// already-existing block commit and snapshot
    ///
    /// `header` should be a pointer to the header in `tip_info`.
    pub fn insert_stacks_block_header(
        tx: &Connection,
        tip_info: &StacksHeaderInfo,
        header: &NakamotoBlockHeader,
        block_cost: &ExecutionCost,
        total_tenure_cost: &ExecutionCost,
        tenure_height: u64,
        tenure_changed: bool,
        tenure_tx_fees: u128,
    ) -> Result<(), ChainstateError> {
        assert_eq!(tip_info.stacks_block_height, header.chain_length,);
        assert!(tip_info.burn_header_timestamp < u64::try_from(i64::MAX).unwrap());

        let StacksHeaderInfo {
            index_root,
            consensus_hash,
            burn_header_hash,
            stacks_block_height,
            burn_header_height,
            burn_header_timestamp,
            ..
        } = tip_info;

        let block_size_str = format!("{}", tip_info.anchored_block_size);

        let block_hash = header.block_hash();

        let index_block_hash =
            StacksBlockHeader::make_index_block_hash(&consensus_hash, &block_hash);

        assert!(*stacks_block_height < u64::try_from(i64::MAX).unwrap());

        let args: &[&dyn ToSql] = &[
            &u64_to_sql(*stacks_block_height)?,
            &index_root,
            &consensus_hash,
            &burn_header_hash,
            &burn_header_height,
            &u64_to_sql(*burn_header_timestamp)?,
            &block_size_str,
            &HeaderTypeNames::Nakamoto,
            &header.version,
            &u64_to_sql(header.chain_length)?,
            &u64_to_sql(header.burn_spent)?,
            &header.miner_signature,
            &header.stacker_signature,
            &header.tx_merkle_root,
            &header.state_index_root,
            &block_hash,
            &index_block_hash,
            block_cost,
            total_tenure_cost,
            &tenure_tx_fees.to_string(),
            &header.parent_block_id,
            &u64_to_sql(tenure_height)?,
            if tenure_changed { &1i64 } else { &0 },
        ];

        tx.execute(
            "INSERT INTO nakamoto_block_headers
                    (block_height,  index_root, consensus_hash,
                     burn_header_hash, burn_header_height,
                     burn_header_timestamp, block_size,

                     header_type,
                     version, chain_length, burn_spent,
                     miner_signature, stacker_signature, tx_merkle_root, state_index_root,

                     block_hash,
                     index_block_hash,
                     cost,
                     total_tenure_cost,
                     tenure_tx_fees,
                     parent_block_id,
                     tenure_height,
                     tenure_changed)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
            args
        )?;

        Ok(())
    }

    /// Append a Stacks block to an existing Stacks block, and grant the miner the block reward.
    /// Return the new Stacks header info.
    pub fn advance_tip(
        headers_tx: &mut StacksDBTx,
        parent_tip: &StacksBlockHeaderTypes,
        parent_consensus_hash: &ConsensusHash,
        new_tip: &NakamotoBlockHeader,
        new_burn_header_hash: &BurnchainHeaderHash,
        new_burnchain_height: u32,
        new_burnchain_timestamp: u64,
        block_reward: Option<&MinerPaymentSchedule>,
        mature_miner_payouts: Option<(MinerReward, MinerReward, MinerRewardInfo)>, // (miner, parent, matured rewards)
        anchor_block_cost: &ExecutionCost,
        total_tenure_cost: &ExecutionCost,
        block_size: u64,
        applied_epoch_transition: bool,
        burn_stack_stx_ops: Vec<StackStxOp>,
        burn_transfer_stx_ops: Vec<TransferStxOp>,
        burn_delegate_stx_ops: Vec<DelegateStxOp>,
        tenure_height: u64,
        tenure_changed: bool,
        block_fees: u128,
    ) -> Result<StacksHeaderInfo, ChainstateError> {
        if new_tip.parent_block_id
            != StacksBlockHeader::make_index_block_hash(
                &FIRST_BURNCHAIN_CONSENSUS_HASH,
                &FIRST_STACKS_BLOCK_HASH,
            )
        {
            // not the first-ever block, so linkage must occur
            match parent_tip {
                StacksBlockHeaderTypes::Epoch2(stacks_header) => {
                    // this is the first nakamoto block
                    assert_eq!(parent_tip.block_hash(), stacks_header.block_hash());
                    assert_eq!(
                        new_tip.parent_block_id,
                        StacksBlockHeader::make_index_block_hash(
                            &parent_consensus_hash,
                            &parent_tip.block_hash()
                        )
                    );
                }
                StacksBlockHeaderTypes::Nakamoto(nakamoto_header) => {
                    // nakamoto blocks link to their parent via index block hashes
                    assert_eq!(
                        new_tip.parent_block_id,
                        StacksBlockHeader::make_index_block_hash(
                            &nakamoto_header.consensus_hash,
                            &parent_tip.block_hash()
                        )
                    );
                }
            }
        }

        assert_eq!(
            parent_tip
                .height()
                .checked_add(1)
                .expect("Block height overflow"),
            new_tip.chain_length
        );

        let parent_hash = StacksBlockId::new(parent_consensus_hash, &parent_tip.block_hash());
        assert_eq!(
            parent_hash,
            new_tip.parent_block_id,
            "FATAL: parent_consensus_hash/parent_block_hash ({}/{}) {} != {}",
            parent_consensus_hash,
            &parent_tip.block_hash(),
            &parent_hash,
            &new_tip.parent_block_id
        );

        let new_block_hash = new_tip.block_hash();
        let index_block_hash = StacksBlockId::new(&new_tip.consensus_hash, &new_block_hash);

        // store each indexed field
        test_debug!("Headers index_put_begin {parent_hash}-{index_block_hash}");
        let root_hash =
            headers_tx.put_indexed_all(&parent_hash, &index_block_hash, &vec![], &vec![])?;
        test_debug!("Headers index_indexed_all finished {parent_hash}-{index_block_hash}");

        let new_tip_info = StacksHeaderInfo {
            anchored_header: new_tip.clone().into(),
            microblock_tail: None,
            index_root: root_hash,
            stacks_block_height: new_tip.chain_length,
            consensus_hash: new_tip.consensus_hash.clone(),
            burn_header_hash: new_burn_header_hash.clone(),
            burn_header_height: new_burnchain_height,
            burn_header_timestamp: new_burnchain_timestamp,
            anchored_block_size: block_size,
        };

        let tenure_fees = block_fees
            + if tenure_changed {
                0
            } else {
                Self::get_total_tenure_tx_fees_at(&headers_tx, &parent_hash)?.ok_or_else(|| {
                    warn!(
                        "Failed to fetch parent block's total tx fees";
                        "parent_block_id" => %parent_hash,
                        "block_id" => %index_block_hash,
                    );
                    ChainstateError::NoSuchBlockError
                })?
            };

        Self::insert_stacks_block_header(
            headers_tx.deref_mut(),
            &new_tip_info,
            &new_tip,
            anchor_block_cost,
            total_tenure_cost,
            tenure_height,
            tenure_changed,
            tenure_fees,
        )?;
        if let Some(block_reward) = block_reward {
            StacksChainState::insert_miner_payment_schedule(
                headers_tx.deref_mut(),
                block_reward,
                &[],
            )?;
        }
        StacksChainState::store_burnchain_txids(
            headers_tx.deref(),
            &index_block_hash,
            burn_stack_stx_ops,
            burn_transfer_stx_ops,
            burn_delegate_stx_ops,
        )?;

        if let Some((miner_payout, parent_payout, reward_info)) = mature_miner_payouts {
            let rewarded_miner_block_id = StacksBlockId::new(
                &reward_info.from_block_consensus_hash,
                &reward_info.from_stacks_block_hash,
            );
            let rewarded_parent_miner_block_id = StacksBlockId::new(
                &reward_info.from_parent_block_consensus_hash,
                &reward_info.from_parent_stacks_block_hash,
            );

            StacksChainState::insert_matured_child_miner_reward(
                headers_tx.deref_mut(),
                &rewarded_parent_miner_block_id,
                &rewarded_miner_block_id,
                &miner_payout,
            )?;
            StacksChainState::insert_matured_parent_miner_reward(
                headers_tx.deref_mut(),
                &rewarded_parent_miner_block_id,
                &rewarded_miner_block_id,
                &parent_payout,
            )?;
        }

        if applied_epoch_transition {
            debug!("Block {} applied an epoch transition", &index_block_hash);
            let sql = "INSERT INTO epoch_transitions (block_id) VALUES (?)";
            let args: &[&dyn ToSql] = &[&index_block_hash];
            headers_tx.deref_mut().execute(sql, args)?;
        }

        debug!(
            "Advanced to new tip! {}/{}",
            &new_tip.consensus_hash, new_block_hash,
        );
        Ok(new_tip_info)
    }

    /// This function is called in both `append_block` in blocks.rs (follower) and
    /// `mine_anchored_block` in miner.rs.
    /// Processes matured miner rewards, alters liquid supply of ustx, processes
    /// stx lock events, and marks the microblock public key as used
    /// Returns stx lockup events.
    pub fn finish_block(
        clarity_tx: &mut ClarityTx,
        miner_payouts: Option<&(MinerReward, MinerReward, MinerRewardInfo)>,
    ) -> Result<Vec<StacksTransactionEvent>, ChainstateError> {
        // add miner payments
        if let Some((ref miner_reward, ref parent_reward, _)) = miner_payouts {
            // grant in order by miner, then users
            let matured_ustx = StacksChainState::process_matured_miner_rewards(
                clarity_tx,
                miner_reward,
                &[],
                parent_reward,
            )?;

            clarity_tx.increment_ustx_liquid_supply(matured_ustx);
        }

        // process unlocks
        let (new_unlocked_ustx, lockup_events) = StacksChainState::process_stx_unlocks(clarity_tx)?;

        clarity_tx.increment_ustx_liquid_supply(new_unlocked_ustx);

        Ok(lockup_events)
    }

    /// Called in both follower and miner block assembly paths.
    ///
    /// Returns clarity_tx, list of receipts, microblock execution cost,
    /// microblock fees, microblock burns, list of microblock tx receipts,
    /// miner rewards tuples, the stacks epoch id, and a boolean that
    /// represents whether the epoch transition has been applied.
    pub fn setup_block<'a, 'b>(
        // Transaction against the chainstate
        chainstate_tx: &'b mut ChainstateTx,
        // Clarity connection to the chainstate
        clarity_instance: &'a mut ClarityInstance,
        // Reference to the sortition DB
        sortition_dbconn: &'b dyn SortitionDBRef,
        // PoX constants for the system
        pox_constants: &PoxConstants,
        // Stacks chain tip
        chain_tip: &StacksHeaderInfo,
        // Burnchain block hash and height of the tenure for this Stacks block
        burn_header_hash: BurnchainHeaderHash,
        burn_header_height: u32,
        // Parent Stacks block's tenure's burnchain block hash and its consensus hash
        parent_consensus_hash: ConsensusHash,
        parent_header_hash: BlockHeaderHash,
        // are we in mainnet or testnet?
        mainnet: bool,
        // is this the start of a new tenure?
        tenure_changed: bool,
        // What tenure height are we in?
        tenure_height: u64,
    ) -> Result<SetupBlockResult<'a, 'b>, ChainstateError> {
        let parent_index_hash = StacksBlockId::new(&parent_consensus_hash, &parent_header_hash);
        let parent_sortition_id = sortition_dbconn
            .get_sortition_id_from_consensus_hash(&parent_consensus_hash)
            .expect("Failed to get parent SortitionID from ConsensusHash");
        let tip_index_hash = chain_tip.index_block_hash();

        // find matured miner rewards, so we can grant them within the Clarity DB tx.
        let matured_rewards_pair = if !tenure_changed {
            // only grant matured rewards at a tenure changing block
            None
        } else {
            if tenure_height < MINER_REWARD_MATURITY {
                Some((vec![], MinerPaymentSchedule::genesis(mainnet)))
            } else {
                let matured_tenure_height = tenure_height - MINER_REWARD_MATURITY;
                // for finding matured rewards at a tenure height, we identify the tenure
                //  by the consensus hash associated with that tenure's sortition.
                let matured_tenure_block_header = Self::get_header_by_tenure_height(
                    chainstate_tx,
                    &tip_index_hash,
                    matured_tenure_height,
                )?
                .ok_or_else(|| {
                    warn!("Matured tenure data not found");
                    ChainstateError::NoSuchBlockError
                })?;

                if matured_tenure_block_header.is_epoch_2_block() {
                    // is matured_tenure_height a epoch-2 rules block? if so, use StacksChainState block rewards methods
                    let latest_miners = StacksChainState::get_scheduled_block_rewards_at_block(
                        chainstate_tx.deref_mut(),
                        &matured_tenure_block_header.index_block_hash(),
                    )?;
                    let parent_miner = StacksChainState::get_parent_matured_miner(
                        chainstate_tx.deref_mut(),
                        mainnet,
                        &latest_miners,
                    )?;
                    Some((latest_miners, parent_miner))
                } else {
                    // otherwise, apply nakamoto rules for getting block rewards: fetch by the consensus hash
                    //   associated with the tenure, parent_miner is None.
                    let latest_miners = StacksChainState::get_scheduled_block_rewards_at_block(
                        chainstate_tx.deref_mut(),
                        &matured_tenure_block_header.index_block_hash(),
                    )?;
                    // find the parent of this tenure
                    let parent_miner = StacksChainState::get_parent_matured_miner(
                        chainstate_tx.deref_mut(),
                        mainnet,
                        &latest_miners,
                    )?;
                    Some((latest_miners, parent_miner))
                }
            }
        };

        let (stacking_burn_ops, transfer_burn_ops, delegate_burn_ops) =
            StacksChainState::get_stacking_and_transfer_and_delegate_burn_ops(
                chainstate_tx,
                &parent_index_hash,
                sortition_dbconn.sqlite_conn(),
                &burn_header_hash,
                burn_header_height.into(),
            )?;

        let mut clarity_tx = StacksChainState::chainstate_block_begin(
            chainstate_tx,
            clarity_instance,
            sortition_dbconn.as_burn_state_db(),
            &parent_consensus_hash,
            &parent_header_hash,
            &MINER_BLOCK_CONSENSUS_HASH,
            &MINER_BLOCK_HEADER_HASH,
        );

        let matured_miner_rewards_result =
            matured_rewards_pair.map(|(latest_matured_miners, matured_miner_parent)| {
                StacksChainState::find_mature_miner_rewards(
                    &mut clarity_tx,
                    sortition_dbconn.sqlite_conn(),
                    &chain_tip,
                    latest_matured_miners,
                    matured_miner_parent,
                )
            });
        let matured_miner_rewards_opt = match matured_miner_rewards_result {
            Some(Ok(Some((miner, _user_burns, parent, reward_info)))) => {
                Some((miner, parent, reward_info))
            }
            Some(Ok(None)) => None,
            Some(Err(e)) => {
                let msg = format!("Failed to load miner rewards: {:?}", &e);
                warn!("{}", &msg);

                clarity_tx.rollback_block();
                return Err(ChainstateError::InvalidStacksBlock(msg));
            }
            None => None,
        };

        // Nakamoto must load block cost from parent if this block isn't a tenure change
        let initial_cost = if tenure_changed {
            ExecutionCost::zero()
        } else {
            let parent_cost_total =
                Self::get_total_tenure_cost_at(&chainstate_tx.deref().deref(), &parent_index_hash)?
                    .ok_or_else(|| {
                        ChainstateError::InvalidStacksBlock(format!(
                    "Failed to load total tenure cost from parent. parent_stacks_block_id = {}",
                    &parent_index_hash
                ))
                    })?;
            parent_cost_total
        };

        clarity_tx.reset_cost(initial_cost);

        // is this stacks block the first of a new epoch?
        let (applied_epoch_transition, mut tx_receipts) =
            StacksChainState::process_epoch_transition(&mut clarity_tx, burn_header_height)?;

        debug!(
            "Setup block: Processed epoch transition at {}/{}",
            &chain_tip.consensus_hash,
            &chain_tip.anchored_header.block_hash()
        );

        let evaluated_epoch = clarity_tx.get_epoch();

        let auto_unlock_events = if evaluated_epoch >= StacksEpochId::Epoch21 {
            let unlock_events = StacksChainState::check_and_handle_reward_start(
                burn_header_height.into(),
                sortition_dbconn.as_burn_state_db(),
                sortition_dbconn,
                &mut clarity_tx,
                chain_tip,
                &parent_sortition_id,
            )?;
            debug!(
                "Setup block: Processed unlock events at {}/{}",
                &chain_tip.consensus_hash,
                &chain_tip.anchored_header.block_hash()
            );
            unlock_events
        } else {
            vec![]
        };

        let active_pox_contract = pox_constants.active_pox_contract(burn_header_height.into());

        // process stacking & transfer operations from burnchain ops
        tx_receipts.extend(StacksChainState::process_stacking_ops(
            &mut clarity_tx,
            stacking_burn_ops.clone(),
            active_pox_contract,
        ));
        debug!(
            "Setup block: Processed burnchain stacking ops for {}/{}",
            &chain_tip.consensus_hash,
            &chain_tip.anchored_header.block_hash()
        );
        tx_receipts.extend(StacksChainState::process_transfer_ops(
            &mut clarity_tx,
            transfer_burn_ops.clone(),
        ));
        debug!(
            "Setup block: Processed burnchain transfer ops for {}/{}",
            &chain_tip.consensus_hash,
            &chain_tip.anchored_header.block_hash()
        );
        // DelegateStx ops are allowed from epoch 2.1 onward.
        // The query for the delegate ops only returns anything in and after Epoch 2.1,
        // but we do a second check here just to be safe.
        if evaluated_epoch >= StacksEpochId::Epoch21 {
            tx_receipts.extend(StacksChainState::process_delegate_ops(
                &mut clarity_tx,
                delegate_burn_ops.clone(),
                active_pox_contract,
            ));
            debug!(
                "Setup block: Processed burnchain delegate ops for {}/{}",
                &chain_tip.consensus_hash,
                &chain_tip.anchored_header.block_hash()
            );
        }

        debug!(
            "Setup block: ready to go for {}/{}",
            &chain_tip.consensus_hash,
            &chain_tip.anchored_header.block_hash()
        );
        Ok(SetupBlockResult {
            clarity_tx,
            tx_receipts,
            matured_miner_rewards_opt,
            evaluated_epoch,
            applied_epoch_transition,
            burn_stack_stx_ops: stacking_burn_ops,
            burn_transfer_stx_ops: transfer_burn_ops,
            auto_unlock_events,
            burn_delegate_stx_ops: delegate_burn_ops,
        })
    }

    /// Append a Nakamoto Stacks block to the Stacks chain state.
    fn append_block<'a>(
        chainstate_tx: &mut ChainstateTx,
        clarity_instance: &'a mut ClarityInstance,
        burn_dbconn: &mut SortitionHandleTx,
        pox_constants: &PoxConstants,
        parent_chain_tip: &StacksHeaderInfo,
        chain_tip_burn_header_hash: &BurnchainHeaderHash,
        chain_tip_burn_header_height: u32,
        chain_tip_burn_header_timestamp: u64,
        block: &NakamotoBlock,
        block_size: u64,
        burnchain_commit_burn: u64,
        burnchain_sortition_burn: u64,
    ) -> Result<(StacksEpochReceipt, PreCommitClarityBlock<'a>), ChainstateError> {
        debug!(
            "Process block {:?} with {} transactions",
            &block.header.block_hash().to_hex(),
            block.txs.len()
        );

        let ast_rules = ASTRules::PrecheckSize;
        let mainnet = chainstate_tx.get_config().mainnet;
        let next_block_height = block.header.chain_length;

        let (parent_ch, parent_block_hash) = if block.is_first_mined() {
            (
                FIRST_BURNCHAIN_CONSENSUS_HASH.clone(),
                FIRST_STACKS_BLOCK_HASH.clone(),
            )
        } else {
            (
                parent_chain_tip.consensus_hash.clone(),
                parent_chain_tip.anchored_header.block_hash(),
            )
        };

        let parent_block_id = StacksChainState::get_index_hash(&parent_ch, &parent_block_hash);
        if parent_block_id != block.header.parent_block_id {
            warn!("Error processing nakamoto block: Parent consensus hash does not match db view";
                  "db.parent_block_id" => %parent_block_id,
                  "header.parent_block_id" => %block.header.parent_block_id);
            return Err(ChainstateError::InvalidStacksBlock(
                "Parent block does not match".into(),
            ));
        }

        // check that the burnchain block that this block is associated with has been processed.
        // N.B. we must first get its hash, and then verify that it's in the same Bitcoin fork as
        // our `burn_dbconn` indicates.
        let burn_header_hash = SortitionDB::get_burnchain_header_hash_by_consensus(
            burn_dbconn,
            &block.header.consensus_hash,
        )?
        .ok_or_else(|| {
            warn!(
                "Unrecognized consensus hash";
                "block_hash" => %block.header.block_hash(),
                "consensus_hash" => %block.header.consensus_hash,
            );
            ChainstateError::NoSuchBlockError
        })?;

        let sortition_tip = burn_dbconn.context.chain_tip.clone();
        let burn_header_height = burn_dbconn
            .get_block_snapshot(&burn_header_hash, &sortition_tip)?
            .ok_or_else(|| {
                warn!(
                    "Tried to process Nakamoto block before its burn view was processed";
                    "block_hash" => block.header.block_hash(),
                    "burn_header_hash" => %burn_header_hash,
                );
                ChainstateError::NoSuchBlockError
            })?
            .block_height;

        let block_hash = block.header.block_hash();

        let tenure_changed = block.tenure_changed(&parent_block_id);
        if !tenure_changed && (block.is_first_mined() || parent_ch != block.header.consensus_hash) {
            return Err(ChainstateError::ExpectedTenureChange);
        }

        let parent_tenure_height = if block.is_first_mined() {
            0
        } else {
            Self::get_tenure_height(chainstate_tx.deref(), &parent_block_id)?.ok_or_else(|| {
                warn!(
                    "Parent of Nakamoto block in block headers DB yet";
                    "block_hash" => %block.header.block_hash(),
                    "parent_block_hash" => %parent_block_hash,
                    "parent_block_id" => %parent_block_id
                );
                ChainstateError::NoSuchBlockError
            })?
        };

        let tenure_height = if tenure_changed {
            parent_tenure_height + 1
        } else {
            parent_tenure_height
        };

        let SetupBlockResult {
            mut clarity_tx,
            mut tx_receipts,
            matured_miner_rewards_opt,
            evaluated_epoch,
            applied_epoch_transition,
            burn_stack_stx_ops,
            burn_transfer_stx_ops,
            mut auto_unlock_events,
            burn_delegate_stx_ops,
        } = Self::setup_block(
            chainstate_tx,
            clarity_instance,
            burn_dbconn,
            pox_constants,
            &parent_chain_tip,
            burn_header_hash,
            burn_header_height.try_into().map_err(|_| {
                ChainstateError::InvalidStacksBlock("Burn block height exceeded u32".into())
            })?,
            parent_ch,
            parent_block_hash,
            mainnet,
            tenure_changed,
            tenure_height,
        )?;

        let starting_cost = clarity_tx.cost_so_far();

        debug!(
            "Append nakamoto block";
            "block" => format!("{}/{block_hash}", block.header.consensus_hash),
            "parent_block" => %block.header.parent_block_id,
            "stacks_height" => next_block_height,
            "total_burns" => block.header.burn_spent,
            "evaluated_epoch" => %evaluated_epoch
        );

        // process anchored block
        let (block_fees, txs_receipts) = match StacksChainState::process_block_transactions(
            &mut clarity_tx,
            &block.txs,
            0,
            ast_rules,
        ) {
            Err(e) => {
                let msg = format!("Invalid Stacks block {}: {:?}", &block_hash, &e);
                warn!("{}", &msg);

                clarity_tx.rollback_block();
                return Err(ChainstateError::InvalidStacksBlock(msg));
            }
            Ok((block_fees, _block_burns, txs_receipts)) => (block_fees, txs_receipts),
        };

        tx_receipts.extend(txs_receipts.into_iter());

        let total_tenure_cost = clarity_tx.cost_so_far();
        let mut block_execution_cost = total_tenure_cost.clone();
        block_execution_cost.sub(&starting_cost).map_err(|_e| {
            ChainstateError::InvalidStacksBlock("Block execution cost was negative".into())
        })?;

        // obtain reward info for receipt -- consolidate miner, user, and parent rewards into a
        // single list, but keep the miner/user/parent/info tuple for advancing the chain tip
        // TODO: drop user burn support
        let (matured_rewards, miner_payouts_opt) =
            if let Some(matured_miner_rewards) = matured_miner_rewards_opt {
                let (miner_reward, parent_reward, reward_ptr) = matured_miner_rewards;

                let mut ret = vec![];
                ret.push(miner_reward.clone());
                ret.push(parent_reward.clone());
                (ret, Some((miner_reward, parent_reward, reward_ptr)))
            } else {
                (vec![], None)
            };

        let mut lockup_events =
            match Self::finish_block(&mut clarity_tx, miner_payouts_opt.as_ref()) {
                Err(ChainstateError::InvalidStacksBlock(e)) => {
                    clarity_tx.rollback_block();
                    return Err(ChainstateError::InvalidStacksBlock(e));
                }
                Err(e) => return Err(e),
                Ok(lockup_events) => lockup_events,
            };

        // if any, append lockups events to the coinbase receipt
        if lockup_events.len() > 0 {
            // Receipts are appended in order, so the first receipt should be
            // the one of the coinbase transaction
            if let Some(receipt) = tx_receipts.get_mut(0) {
                if receipt.is_coinbase_tx() {
                    receipt.events.append(&mut lockup_events);
                }
            } else {
                warn!("Unable to attach lockups events, block's first transaction is not a coinbase transaction")
            }
        }
        // if any, append auto unlock events to the coinbase receipt
        if auto_unlock_events.len() > 0 {
            // Receipts are appended in order, so the first receipt should be
            // the one of the coinbase transaction
            if let Some(receipt) = tx_receipts.get_mut(0) {
                if receipt.is_coinbase_tx() {
                    receipt.events.append(&mut auto_unlock_events);
                }
            } else {
                warn!("Unable to attach auto unlock events, block's first transaction is not a coinbase transaction")
            }
        }

        let root_hash = clarity_tx.seal();
        if root_hash != block.header.state_index_root {
            let msg = format!(
                "Block {} state root mismatch: expected {}, got {}",
                &block_hash, block.header.state_index_root, root_hash,
            );
            warn!("{}", &msg);

            clarity_tx.rollback_block();
            return Err(ChainstateError::InvalidStacksBlock(msg));
        }

        debug!("Reached state root {}", root_hash;
               "block_cost" => %block_execution_cost);

        // good to go!
        let block_limit = clarity_tx
            .block_limit()
            .ok_or_else(|| ChainstateError::InvalidChainstateDB)?;
        let clarity_commit =
            clarity_tx.precommit_to_block(&block.header.consensus_hash, &block_hash);

        // figure out if there any accumulated rewards by
        //   getting the snapshot that elected this block.
        let accumulated_rewards = SortitionDB::get_block_snapshot_consensus(
            burn_dbconn.tx(),
            &block.header.consensus_hash,
        )?
        .expect("CORRUPTION: failed to load snapshot that elected processed block")
        .accumulated_coinbase_ustx;

        let coinbase_at_block = StacksChainState::get_coinbase_reward(
            u64::from(chain_tip_burn_header_height),
            burn_dbconn.context.first_block_height,
        );

        let total_coinbase = coinbase_at_block.saturating_add(accumulated_rewards);

        let scheduled_miner_reward = if tenure_changed {
            let parent_tenure_header: StacksHeaderInfo = Self::get_header_by_tenure_height(
                chainstate_tx,
                &parent_block_id,
                parent_tenure_height,
            )?
            .ok_or_else(|| {
                warn!("While processing tenure change, failed to look up parent tenure";
                      "parent_tenure_height" => parent_tenure_height,
                      "block_hash" => %block_hash,
                      "block_consensus_hash" => %block.header.consensus_hash);
                ChainstateError::NoSuchBlockError
            })?;
            // fetch the parent tenure fees by reading the total tx fees from this block's
            // *parent* (not parent_tenure_header), because `parent_block_id` is the last
            // block of that tenure, so contains a total fee accumulation for the whole tenure
            let parent_tenure_fees = if parent_tenure_header.is_nakamoto_block() {
                Self::get_total_tenure_tx_fees_at(
                    chainstate_tx,
                    &parent_block_id
                )?.ok_or_else(|| {
                    warn!("While processing tenure change, failed to look up parent block's total tx fees";
                          "parent_block_id" => %parent_block_id,
                          "block_hash" => %block_hash,
                          "block_consensus_hash" => %block.header.consensus_hash);
                    ChainstateError::NoSuchBlockError
                })?
            } else {
                // if the parent tenure is an epoch-2 block, don't pay
                // any fees to them in this schedule: nakamoto blocks
                // cannot confirm microblock transactions, and
                // anchored transactions are scheduled
                // by the parent in epoch-2.
                0
            };

            Some(
                Self::make_scheduled_miner_reward(
                    mainnet,
                    evaluated_epoch,
                    &parent_tenure_header.anchored_header.block_hash(),
                    &parent_tenure_header.consensus_hash,
                    &block_hash,
                    &block.header.consensus_hash,
                    next_block_height,
                    block
                        .get_coinbase_tx()
                        .ok_or(ChainstateError::InvalidStacksBlock(
                            "No coinbase transaction in tenure changing block".into(),
                        ))?,
                    parent_tenure_fees,
                    burnchain_commit_burn,
                    burnchain_sortition_burn,
                    total_coinbase,
                )
                .expect("FATAL: parsed and processed a block without a coinbase"),
            )
        } else {
            None
        };

        let matured_rewards_info = miner_payouts_opt.as_ref().map(|(_, _, info)| info.clone());

        let new_tip = Self::advance_tip(
            &mut chainstate_tx.tx,
            &parent_chain_tip.anchored_header,
            &parent_chain_tip.consensus_hash,
            &block.header,
            chain_tip_burn_header_hash,
            chain_tip_burn_header_height,
            chain_tip_burn_header_timestamp,
            scheduled_miner_reward.as_ref(),
            miner_payouts_opt,
            &block_execution_cost,
            &total_tenure_cost,
            block_size,
            applied_epoch_transition,
            burn_stack_stx_ops,
            burn_transfer_stx_ops,
            burn_delegate_stx_ops,
            tenure_height,
            tenure_changed,
            block_fees,
        )
        .expect("FATAL: failed to advance chain tip");

        let new_block_id = new_tip.index_block_hash();
        chainstate_tx.log_transactions_processed(&new_block_id, &tx_receipts);

        monitoring::set_last_block_transaction_count(u64::try_from(block.txs.len()).unwrap());
        monitoring::set_last_execution_cost_observed(&block_execution_cost, &block_limit);

        // get previous burn block stats
        let (parent_burn_block_hash, parent_burn_block_height, parent_burn_block_timestamp) =
            if block.is_first_mined() {
                (BurnchainHeaderHash([0; 32]), 0, 0)
            } else {
                match SortitionDB::get_block_snapshot_consensus(burn_dbconn, &parent_ch)? {
                    Some(sn) => (
                        sn.burn_header_hash,
                        u32::try_from(sn.block_height).map_err(|_| {
                            ChainstateError::InvalidStacksBlock(
                                "Burn block height exceeds u32".into(),
                            )
                        })?,
                        sn.burn_header_timestamp,
                    ),
                    None => {
                        // shouldn't happen
                        warn!(
                            "CORRUPTION: block {}/{} does not correspond to a burn block",
                            &parent_ch, &parent_block_hash
                        );
                        (BurnchainHeaderHash([0; 32]), 0, 0)
                    }
                }
            };

        let epoch_receipt = StacksEpochReceipt {
            header: new_tip,
            tx_receipts,
            matured_rewards,
            matured_rewards_info,
            parent_microblocks_cost: ExecutionCost::zero(),
            anchored_block_cost: block_execution_cost,
            parent_burn_block_hash,
            parent_burn_block_height,
            parent_burn_block_timestamp,
            evaluated_epoch,
            epoch_transition: applied_epoch_transition,
        };

        NakamotoChainState::set_block_processed(&chainstate_tx, &new_block_id)?;

        Ok((epoch_receipt, clarity_commit))
    }
}

impl StacksMessageCodec for NakamotoBlock {
    fn consensus_serialize<W: std::io::Write>(&self, fd: &mut W) -> Result<(), CodecError> {
        write_next(fd, &self.header)?;
        write_next(fd, &self.txs)
    }

    fn consensus_deserialize<R: std::io::Read>(fd: &mut R) -> Result<Self, CodecError> {
        let (header, txs) = {
            let mut bound_read = BoundReader::from_reader(fd, u64::from(MAX_MESSAGE_LEN));
            let header: NakamotoBlockHeader = read_next(&mut bound_read)?;
            let txs: Vec<_> = read_next(&mut bound_read)?;
            (header, txs)
        };

        // all transactions are unique
        if !StacksBlock::validate_transactions_unique(&txs) {
            warn!("Invalid block: Found duplicate transaction"; "block_hash" => header.block_hash());
            return Err(CodecError::DeserializeError(
                "Invalid block: found duplicate transaction".to_string(),
            ));
        }

        // header and transactions must be consistent
        let txid_vecs = txs.iter().map(|tx| tx.txid().as_bytes().to_vec()).collect();

        let merkle_tree = MerkleTree::new(&txid_vecs);
        let tx_merkle_root: Sha512Trunc256Sum = merkle_tree.root();

        if tx_merkle_root != header.tx_merkle_root {
            warn!("Invalid block: Tx Merkle root mismatch"; "block_hash" => header.block_hash());
            return Err(CodecError::DeserializeError(
                "Invalid block: tx Merkle root mismatch".to_string(),
            ));
        }

        Ok(NakamotoBlock { header, txs })
    }
}
