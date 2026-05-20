//! Mandate enforcement hook.
//!
//! DESIGN.md lists mandate enforcement as one of the three jobs the Guardian
//! does for the buyer ("mandate enforcement (already), payment verification
//! (new), batch settlement (new)"). In practice no mandate concept exists in
//! OZ Guardian today, and there is no single canonical mandate schema for
//! agent payments. This module defines the **shape** of mandate enforcement
//! (a trait + context object) and ships an `AllowAll` default so the verify
//! path is operational from day one. Real policies plug in via
//! `MIDEN_X402_MANDATE_POLICY` at the binary boundary.
//!
//! A real mandate policy might enforce, for example:
//!
//! - per-account spending caps per `(faucet_id, time_window)`,
//! - merchant allowlists (which `resource_url` an agent may pay),
//! - per-tx ceilings.
//!
//! This crate intentionally does not commit to a schema — keeping the trait
//! narrow lets future work add policies without churning the verify path.
//! See [`docs/mandate.md`] in the repo for guidance on writing a policy.

use std::sync::Arc;

use thiserror::Error;

use miden_x402_types::{AccountIdHex, MidenPaymentRequirements};

/// Returned by [`MandatePolicy::evaluate`] on rejection.
#[derive(Debug, Error, Clone)]
pub enum MandateError {
    /// The policy rejected the payment. `reason` is human-readable; the
    /// verify handler maps this onto `ErrorReason::InvalidPaymentAuth` so
    /// the merchant SDK can surface "your mandate would not permit this"
    /// to the buyer.
    #[error("mandate rejected: {reason}")]
    Rejected { reason: String },

    /// The policy could not be evaluated due to a transient backend error
    /// (e.g. a database read failed). The verify handler returns 503 so the
    /// merchant retries.
    #[error("mandate evaluation failed: {0}")]
    EvaluationFailed(String),
}

/// Inputs available to a [`MandatePolicy`] when deciding whether to approve
/// a payment.
#[derive(Debug, Clone)]
pub struct MandateContext<'a> {
    /// Buyer's account id — the one the policy is enforcing limits against.
    pub buyer: &'a AccountIdHex,
    /// Resource URL the buyer is paying for (`paymentPayload.resource`
    /// when present; falls back to the merchant's offered `resource.url`).
    pub resource_url: Option<&'a str>,
    /// Faucet id (which token).
    pub asset: &'a AccountIdHex,
    /// Atomic-unit amount as a decimal string. Parsed to `u128` if the
    /// policy needs to compare against caps.
    pub amount: &'a str,
    /// Full requirements snapshot — gives the policy access to merchant
    /// pay-to, network, and any `extra` fields.
    pub requirements: &'a MidenPaymentRequirements,
}

/// A policy decides whether a verified-but-unsubmitted payment should be
/// allowed to settle through this facilitator.
pub trait MandatePolicy: Send + Sync + 'static {
    fn evaluate(&self, ctx: &MandateContext<'_>) -> Result<(), MandateError>;
}

/// Default policy — every payment passes. Useful for the reference
/// deployment and tests. Real deployments should configure a concrete policy.
#[derive(Debug, Clone, Default)]
pub struct AllowAll;

impl MandatePolicy for AllowAll {
    fn evaluate(&self, _: &MandateContext<'_>) -> Result<(), MandateError> { Ok(()) }
}

/// Convenience: a boxed policy trait object so handlers can hold one in
/// shared state regardless of the concrete type the binary loaded.
pub type ArcMandatePolicy = Arc<dyn MandatePolicy>;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(req: &'a MidenPaymentRequirements) -> MandateContext<'a> {
        MandateContext {
            buyer: &req.pay_to,
            resource_url: Some("https://api.example.com/weather"),
            asset: &req.asset,
            amount: "1000",
            requirements: req,
        }
    }

    fn sample_requirements() -> MidenPaymentRequirements {
        use miden_x402_types::{
            MidenP2idPrivateExtra, MidenP2idPrivateScheme, miden_testnet,
        };
        MidenPaymentRequirements {
            scheme: MidenP2idPrivateScheme,
            network: miden_testnet(),
            amount: "1000".into(),
            pay_to: "0x103f8a1ad4b983104aec0412ab0b0d".parse().unwrap(),
            max_timeout_seconds: 120,
            asset: "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap(),
            extra: MidenP2idPrivateExtra { note_tag: "t".into(), serial_num: None },
        }
    }

    #[test]
    fn allow_all_passes_everything() {
        let req = sample_requirements();
        let c = ctx(&req);
        assert!(AllowAll.evaluate(&c).is_ok());
    }
}
