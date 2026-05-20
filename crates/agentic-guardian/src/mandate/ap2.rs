//! AP2 mandate evaluator — runs the four NEW_DESIGN bullets against an
//! incoming tx + the agent's rolling counters.
//!
//! Inputs:
//!
//! - `Ap2Mandate` (from storage, looked up by `mandate_id`).
//! - `MandateContext { amount, merchant, now_unix_secs }` from the
//!   incoming `/agentic/submit`.
//! - [`MandateCounterRepo`] — historic spend totals for windowed +
//!   daily checks.
//!
//! Output: `Ok(())` if the tx is in-mandate; otherwise an
//! [`AgenticError::MandateRejected`] carrying the specific
//! [`MandateSchemaError`] variant so the merchant SDK can surface it.
//!
//! On success the evaluator also advances the counter store
//! atomically (NEW_DESIGN §44).

use std::sync::Arc;

use miden_x402_types::{AccountIdHex, Ap2Mandate, MandateSchemaError};

use crate::error::{AgenticError, AgenticResult};
use crate::storage::MandateCounterRepo;

/// Per-tx evaluation context.
#[derive(Debug, Clone)]
pub struct MandateContext<'a> {
    pub agent_account_id: &'a AccountIdHex,
    pub amount: u64,
    pub merchant: &'a AccountIdHex,
    pub now_unix_secs: u64,
}

/// Stateful evaluator. Cheap to clone (holds an `Arc` to the counter
/// repo).
#[derive(Clone)]
pub struct Ap2Policy {
    counters: Arc<dyn MandateCounterRepo>,
}

impl Ap2Policy {
    pub fn new(counters: Arc<dyn MandateCounterRepo>) -> Self { Self { counters } }

    /// Runs all four NEW_DESIGN bullets against `ctx` + `mandate`. On
    /// success, increments the rolling-window counter for the agent by
    /// `ctx.amount` (so the next call sees the cumulative effect).
    pub async fn evaluate(
        &self,
        mandate: &Ap2Mandate,
        ctx: &MandateContext<'_>,
    ) -> AgenticResult<()> {
        // 1. expiry + 2. per-tx cap + 3. merchant allowlist
        mandate.pre_check(ctx.amount, ctx.merchant, ctx.now_unix_secs)?;

        // 4. rolling window — sum spend over `mandate.time_window_secs`
        //    must not exceed `daily_total_cap * (window / 86400)`.
        let window_spent = self
            .counters
            .sum_recent(
                ctx.agent_account_id.as_str(),
                mandate.time_window_secs,
                ctx.now_unix_secs,
            )
            .await
            .map_err(|e| AgenticError::Storage(e.to_string()))?;
        let window_cap = compute_window_cap(
            mandate.daily_total_cap,
            mandate.time_window_secs,
        );
        if window_spent.saturating_add(ctx.amount) > window_cap {
            return Err(MandateSchemaError::TimeWindowExceeded {
                observed: window_spent.saturating_add(ctx.amount),
                cap: window_cap,
                window_secs: mandate.time_window_secs,
            }
            .into());
        }

        // 5. daily total — sum over the last 24h must not exceed
        //    `daily_total_cap`.
        const SECONDS_PER_DAY: u64 = 86_400;
        let day_spent = self
            .counters
            .sum_recent(ctx.agent_account_id.as_str(), SECONDS_PER_DAY, ctx.now_unix_secs)
            .await
            .map_err(|e| AgenticError::Storage(e.to_string()))?;
        if day_spent.saturating_add(ctx.amount) > mandate.daily_total_cap {
            return Err(MandateSchemaError::DailyTotalExceeded {
                observed: day_spent.saturating_add(ctx.amount),
                cap: mandate.daily_total_cap,
            }
            .into());
        }

        // Bucket counter by minute (60-sec windows) so the table stays
        // small even at high throughput.
        let bucket = (ctx.now_unix_secs / 60) * 60;
        self.counters
            .add(ctx.agent_account_id.as_str(), bucket, ctx.amount)
            .await
            .map_err(|e| AgenticError::Storage(e.to_string()))?;
        Ok(())
    }
}

/// Compute the windowed cap as a proportion of the daily cap. Saturates
/// to `daily_cap` for windows ≥ 24h.
fn compute_window_cap(daily_cap: u64, window_secs: u64) -> u64 {
    const SECONDS_PER_DAY: u64 = 86_400;
    if window_secs >= SECONDS_PER_DAY {
        return daily_cap;
    }
    // u128 arithmetic to avoid overflow on (daily_cap * window_secs).
    let cap = (daily_cap as u128).saturating_mul(window_secs as u128) / (SECONDS_PER_DAY as u128);
    cap.min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::memory::MemoryMandateCounterRepo;
    use miden_x402_types::Ap2Mandate;

    fn ac(c: char) -> AccountIdHex {
        format!("0x{}", c.to_string().repeat(30)).parse().unwrap()
    }

    fn mandate() -> Ap2Mandate {
        Ap2Mandate {
            mandate_id: "m1".into(),
            agent_account_id: ac('a'),
            amount_cap_per_tx: 1_000,
            merchant_allowlist: vec![ac('b')],
            // Window = full day so the proportional cap == daily cap;
            // simplifies the "no spend yet" test.
            time_window_secs: 86_400,
            daily_total_cap: 100_000,
            issued_at_unix_secs: 0,
            expires_at_unix_secs: u64::MAX,
        }
    }

    #[tokio::test]
    async fn evaluates_first_in_mandate_tx() {
        let counters: Arc<dyn MandateCounterRepo> = Arc::new(MemoryMandateCounterRepo::default());
        let policy = Ap2Policy::new(counters);
        let m = mandate();
        let agent = ac('a');
        let merchant = ac('b');
        let ctx = MandateContext {
            agent_account_id: &agent,
            amount: 500,
            merchant: &merchant,
            now_unix_secs: 1_700_000_000,
        };
        policy.evaluate(&m, &ctx).await.unwrap();
    }

    #[tokio::test]
    async fn rejects_amount_cap_exceeded() {
        let counters: Arc<dyn MandateCounterRepo> = Arc::new(MemoryMandateCounterRepo::default());
        let policy = Ap2Policy::new(counters);
        let m = mandate();
        let agent = ac('a');
        let merchant = ac('b');
        let ctx = MandateContext {
            agent_account_id: &agent,
            amount: 5_000,
            merchant: &merchant,
            now_unix_secs: 1_700_000_000,
        };
        let err = policy.evaluate(&m, &ctx).await.unwrap_err();
        assert!(matches!(
            err,
            AgenticError::MandateRejected(MandateSchemaError::AmountCapExceeded { .. })
        ));
    }

    #[tokio::test]
    async fn rejects_daily_total_after_accumulation() {
        let counters: Arc<dyn MandateCounterRepo> = Arc::new(MemoryMandateCounterRepo::default());
        let policy = Ap2Policy::new(counters);
        // Tight caps so a handful of txs blow the daily limit.
        let m = Ap2Mandate {
            daily_total_cap: 1_500,
            ..mandate()
        };
        let agent = ac('a');
        let merchant = ac('b');
        let ctx = MandateContext {
            agent_account_id: &agent,
            amount: 600,
            merchant: &merchant,
            now_unix_secs: 1_700_000_000,
        };
        // First two pass; third trips the windowed or daily cap (same
        // value when window == day; we accept either rejection so the
        // test is robust to the check order in `Ap2Policy::evaluate`).
        policy.evaluate(&m, &ctx).await.unwrap();
        policy.evaluate(&m, &ctx).await.unwrap();
        let err = policy.evaluate(&m, &ctx).await.unwrap_err();
        assert!(matches!(
            err,
            AgenticError::MandateRejected(
                MandateSchemaError::DailyTotalExceeded { .. }
                | MandateSchemaError::TimeWindowExceeded { .. }
            )
        ));
    }

    #[test]
    fn window_cap_proportional_to_daily() {
        assert_eq!(compute_window_cap(86_400, 86_400), 86_400);
        assert_eq!(compute_window_cap(86_400, 3_600), 3_600);
        assert_eq!(compute_window_cap(86_400, 0), 0);
    }
}
