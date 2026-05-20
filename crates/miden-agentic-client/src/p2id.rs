//! Helper for building a `TransactionRequest` whose output is the
//! exact P2ID note the merchant's 402 advertises.
//!
//! **Skeleton**: real impl uses `miden-tx`'s
//! `TransactionRequestBuilder` to add an output P2ID note with the
//! merchant's `payTo`, the requested `asset`+`amount`, and the
//! server-issued `serial_num`. Currently a no-op shim that documents
//! the seam.

use miden_x402_types::{AccountIdHex, NoteIdHex};

use crate::{AgenticClientError, AgenticClientResult};

/// All the inputs the M8 wire requires for a P2ID transaction output.
#[derive(Debug, Clone)]
pub struct P2idOutput {
    pub merchant: AccountIdHex,
    pub asset: AccountIdHex,
    pub amount: String,
    pub serial_num: NoteIdHex,
    /// Note tag attached by the merchant; baked into the on-chain
    /// note's metadata so the merchant can demultiplex incoming notes.
    pub note_tag: String,
}

/// Stub for building a `miden_tx::TransactionRequest` from the inputs.
pub fn build_request(_out: &P2idOutput) -> AgenticClientResult<()> {
    Err(AgenticClientError::NotImplemented(
        "build_request: uses miden-tx::TransactionRequestBuilder; \
         wire to the agent runtime in a follow-up",
    ))
}
