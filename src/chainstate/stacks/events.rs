use crate::types::chainstate::StacksAddress;
use crate::{codec::StacksMessageCodec, types::chainstate::StacksMicroblockHeader};
use burnchains::Txid;
use chainstate::stacks::StacksTransaction;
use vm::analysis::ContractAnalysis;
use vm::costs::ExecutionCost;
use vm::events::StacksTransactionEvent;
use vm::types::{
    AssetIdentifier, PrincipalData, QualifiedContractIdentifier, StandardPrincipalData, Value,
};

#[derive(Debug, Clone, PartialEq)]
pub enum TransactionOrigin {
    Stacks(StacksTransaction),
    Burn(Txid),
}

impl From<StacksTransaction> for TransactionOrigin {
    fn from(o: StacksTransaction) -> TransactionOrigin {
        TransactionOrigin::Stacks(o)
    }
}

impl TransactionOrigin {
    pub fn txid(&self) -> Txid {
        match self {
            TransactionOrigin::Burn(txid) => txid.clone(),
            TransactionOrigin::Stacks(tx) => tx.txid(),
        }
    }
    pub fn serialize_to_vec(&self) -> Vec<u8> {
        match self {
            TransactionOrigin::Burn(txid) => txid.as_bytes().to_vec(),
            TransactionOrigin::Stacks(tx) => tx.txid().as_bytes().to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StacksTransactionReceipt {
    pub transaction: TransactionOrigin,
    pub events: Vec<StacksTransactionEvent>,
    pub post_condition_aborted: bool,
    pub result: Value,
    pub stx_burned: u128,
    pub contract_analysis: Option<ContractAnalysis>,
    pub execution_cost: ExecutionCost,
    pub microblock_header: Option<StacksMicroblockHeader>,
}
