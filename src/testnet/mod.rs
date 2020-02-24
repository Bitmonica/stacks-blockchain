pub mod run_loop; 
pub mod mem_pool;
pub mod keychain;
pub mod burnchain;
pub mod node;
pub mod tenure;

pub use self::run_loop::{RunLoop};
pub use self::mem_pool::{MemPool, MemPoolFS};
pub use self::keychain::{Keychain};
pub use self::node::{Node, SortitionedBlock};
pub use self::burnchain::{BurnchainSimulator, BurnchainState};
pub use self::tenure::{LeaderTenure};

use vm::types::PrincipalData;

#[derive(Clone)]
pub struct Config {
    pub testnet_name: String,
    pub chain: String,
    pub burnchain_path: String,
    pub burnchain_block_time: u64,
    pub node_config: Vec<NodeConfig>,
    pub genesis_config: GenesisConfig,
}

#[derive(Clone)]
pub struct GenesisConfig {
    pub initial_balances: Vec<(PrincipalData, u64)>,
}

#[derive(Clone)]
pub struct NodeConfig {
    pub name: String,
    pub path: String,
    pub mem_pool_path: String,
}

#[cfg(test)]
pub mod tests;
