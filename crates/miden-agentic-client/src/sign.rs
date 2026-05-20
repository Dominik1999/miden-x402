//! Sign-without-prove primitive.
//!
//! Wraps `MultisigClient::propose_multisig_transaction` from
//! [`inicio-labs/MultiSig`](https://github.com/inicio-labs/MultiSig/blob/hubcycle-ui-flow-md/crates/miden-multisig-client/src/lib.rs)
//! (in agent context: single-signer, threshold=1 multisig with `{hot,
//! cold, guardian}` cosigners).
//!
//! **Skeleton**: returns `NotImplemented` until the
//! `miden-multisig-client` git dep is wired and the runtime thread can
//! drive a `TransactionRequest` through `propose_multisig_transaction`.
//! The module shape is locked so callers can integrate against it.

use miden_x402_types::{AccountIdHex, AgenticPayload, NoteIdHex};

use crate::{AgenticClientError, AgenticClientResult};

/// Intent the agent expresses: "pay `amount` of `asset` to `merchant`,
/// with this `serial_num` baked into the output P2ID note."
#[derive(Debug, Clone)]
pub struct PaymentIntent {
    pub buyer_account_id: AccountIdHex,
    pub merchant: AccountIdHex,
    pub asset: AccountIdHex,
    pub amount: String,
    pub serial_num: NoteIdHex,
    pub mandate_id: String,
    /// The agent's view of pending state — must equal
    /// `agentic-guardian`'s tracked value.
    pub pending_state_commitment: NoteIdHex,
}

/// Produces a signed-but-unproven [`AgenticPayload`] ready to POST to
/// `/agentic/submit`.
///
/// **Skeleton**: the real impl spawns a single-threaded tokio
/// `LocalSet`, drives `MultisigClient::propose_multisig_transaction`
/// against a local `miden-client` instance, then signs the resulting
/// `TransactionSummary.to_commitment()` with the agent's hot Falcon
/// key. Currently returns `NotImplemented`.
pub async fn sign_unproven_payment(_intent: &PaymentIntent) -> AgenticClientResult<AgenticPayload> {
    Err(AgenticClientError::NotImplemented(
        "sign-without-prove requires the inicio-labs miden-multisig-client git dep \
         and an embedded miden-client runtime — see sign.rs and \
         crates/agentic-guardian/src/runtime/mod.rs for the LocalSet pattern",
    ))
}
