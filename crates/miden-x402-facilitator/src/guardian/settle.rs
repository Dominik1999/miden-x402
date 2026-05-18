//! Synchronous prove-and-submit for the Guardian flow.
//!
//! Driven by a [`VerifiedGuardianTx`] handle from [`super::verify`]. The
//! settle path:
//!
//! 1. Forwards `tx_inputs` to the configured `RemoteTransactionProver`.
//! 2. Submits the resulting `ProvenTransaction` to the Miden node.
//! 3. On success, promotes the reserved nullifiers to "consumed" and
//!    returns a `SettleResponse::Success` with the post-prove
//!    `ProvenTransaction.id()` as the on-chain transaction id.
//! 4. On any failure (prover error, node submit error), releases the
//!    reservations so the input notes become available again.
//!
//! Phase B does **synchronous** prove-and-submit (one tx at a time). The
//! batch-accumulator variant from GUARDIAN.md is deferred to a follow-on
//! iteration; the seam to add it is here — replace this function with one
//! that pushes the verified tx onto a queue and returns once the batched
//! prover finishes.

use miden_remote_prover_client::RemoteTransactionProver;
use miden_x402_types::SettleResponse;

use crate::error::FacilitatorError;
use crate::guardian::reservation::ReservedNullifierSet;
use crate::guardian::verify::VerifiedGuardianTx;
use crate::node::MidenNode;

/// Synchronously proves `verified.tx_inputs` via the configured remote
/// prover and submits the resulting `ProvenTransaction` to `node`. Always
/// drains the reservation set on the way out — either promoting to
/// consumed (success) or releasing (failure).
pub async fn settle_and_submit(
    verified: VerifiedGuardianTx,
    prover: &RemoteTransactionProver,
    node: &dyn MidenNode,
    reservations: &ReservedNullifierSet,
    network: &str,
) -> Result<SettleResponse, FacilitatorError> {
    let VerifiedGuardianTx {
        tx_inputs,
        signature: _,
        reserved_nullifiers,
        payer,
        claimed_transaction_id: _,
        challenge: _,
    } = verified;

    // The wire `signature` is already present in
    // `tx_inputs.tx_args.advice_inputs.map` because the buyer's SDK
    // (`Client::execute_transaction`) inserted it during local execution.
    // We re-receive it as a separate wire field only so the Guardian can
    // do offline verification; it does not need to be re-injected here
    // before forwarding to the prover.
    let proven = match prover.prove(&tx_inputs).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "guardian: remote prover failed; releasing reservations");
            reservations.release(&reserved_nullifiers);
            return Err(FacilitatorError::RemoteProverError(e.to_string()));
        }
    };

    let proven_id_hex = proven.id().to_hex();

    match node.submit_proven_transaction(proven, tx_inputs).await {
        Ok(_block_num) => {
            reservations.promote_to_consumed(&reserved_nullifiers);
            Ok(SettleResponse::Success {
                payer: payer.into_inner(),
                transaction: proven_id_hex,
                network: network.to_owned(),
            })
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "guardian: node submit failed; releasing reservations",
            );
            reservations.release(&reserved_nullifiers);
            Err(FacilitatorError::NodeRpc(e.to_string()))
        }
    }
}
