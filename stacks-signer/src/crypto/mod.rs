/// FROST (Fast Reliable Optimistic Secure Threshold) signature generation module
pub mod frost;

use frost::Error as FrostError;
use frost_signer::net::Message;
use wsts::{bip340::SchnorrProof, common::Signature, Point};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("An error occurred in the FROST module: {0}")]
    FrostError(#[from] FrostError),
}

// Result of a DKG or sign operation
#[allow(dead_code)]
pub enum OperationResult {
    /// The DKG result
    Dkg(Point),
    /// The sign result
    Sign(Signature, SchnorrProof),
}

/// Coordinatable trait for handling the coordination of DKG and sign messages
pub trait Coordinatable {
    /// Process inbound messages
    fn process_inbound_messages(
        &mut self,
        messages: Vec<Message>,
    ) -> Result<(Vec<Message>, Vec<OperationResult>), Error>;
    /// Retrieve the aggregate public key
    fn get_aggregate_public_key(&self) -> Point;
    /// Trigger a DKG round
    fn start_distributed_key_generation(&mut self) -> Result<(), Error>;
    // Trigger a signing round
    fn start_signing_message(&mut self, _message: &[u8]) -> Result<(), Error>;
}