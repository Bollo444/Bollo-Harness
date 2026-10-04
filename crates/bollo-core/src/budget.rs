//! Run budgets: tool-call, wall-clock, token and spend ceilings.
//!
//! Rules: reserve the worst-case next request before streaming; unknown pricing
//! blocks new requests under a finite spend cap (`pricing_unknown`); unknown
//! cost is `None`, never zero.

use std::time::{Duration, Instant};

use bollo_policy::config::LimitsConfig;
use bollo_protocol::errors::{BolloError, ErrorCode};

pub const MICRO_USD_PER_CENT: u64 = 10_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_microusd: Option<u64>,
    pub cost_known: bool,
}

pub struct Budget {
    limits: LimitsConfig,
    pricing_known: bool,
    input_microusd_per_token: Option<u64>,
    output_microusd_per_token: Option<u64>,
    started: Instant,
    tool_calls: u32,
    spent_microusd: u64,
    usage: UsageTotals,
}

impl Budget {
    pub fn new(
        limits: LimitsConfig,
        pricing_known: bool,
        input_microusd_per_token: Option<u64>,
        output_microusd_per_token: Option<u64>,
    ) -> Self {
        Self {
            limits,
            pricing_known,
            input_microusd_per_token,
            output_microusd_per_token,
            started: Instant::now(),
            tool_calls: 0,
            spent_microusd: 0,
            usage: UsageTotals::default(),
        }
    }

    pub fn limits(&self) -> &LimitsConfig {
        &self.limits
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn tool_calls(&self) -> u32 {
        self.tool_calls
    }

    pub fn usage(&self) -> &UsageTotals {
        &self.usage
    }

    /// Consume one tool-call slot; exceeding the ceiling is `run_limit_exceeded`.
    pub fn next_tool_call(&mut self) -> Result<(), BolloError> {
        if self.tool_calls + 1 > self.limits.max_tool_calls {
            return Err(BolloError::new(
                ErrorCode::RunLimitExceeded,
                format!(
                    "tool-call ceiling {} reached",
                    self.limits.max_tool_calls
                ),
            ));
        }
        self.tool_calls += 1;
        Ok(())
    }

    /// Check the wall clock, the pricing precondition and the spend reserve
    /// before issuing the next model request.
    pub fn before_request(
        &self,
        estimated_input_tokens: u64,
        max_output_tokens: u64,
    ) -> Result<(), BolloError> {
        if self.elapsed() >= Duration::from_secs(self.limits.max_run_seconds) {
            return Err(BolloError::new(
                ErrorCode::RunLimitExceeded,
                format!(
                    "wall-clock ceiling {}s reached",
                    self.limits.max_run_seconds
                ),
            ));
        }
        let Some(cap_cents) = self.limits.max_spend_cents else {
            return Ok(());
        };
        if !self.pricing_known {
            return Err(BolloError::new(
                ErrorCode::PricingUnknown,
                "a finite spend cap is configured but no trusted price table is available; \
                 configure prices or set max_spend_cents to null",
            ));
        }
        let cap_micro = cap_cents.saturating_mul(MICRO_USD_PER_CENT);
        let reserve = estimated_input_tokens.saturating_mul(
            self.input_microusd_per_token.unwrap_or(0),
        ) + max_output_tokens.saturating_mul(self.output_microusd_per_token.unwrap_or(0));
        if self.spent_microusd.saturating_add(reserve) > cap_micro {
            return Err(BolloError::new(
                ErrorCode::BudgetExceeded,
                format!(
                    "spend cap {} cents would be exceeded (reserved {} µ$)",
                    cap_cents, reserve
                ),
            ));
        }
        Ok(())
    }

    /// Reconcile observed usage after a response; unreported usage stays 0 for
    /// tokens but cost remains `None` unless pricing is known.
    pub fn record_usage(&mut self, input_tokens: Option<u64>, output_tokens: Option<u64>) {
        if let Some(input) = input_tokens {
            self.usage.input_tokens = self.usage.input_tokens.saturating_add(input);
            if self.pricing_known {
                self.spent_microusd = self.spent_microusd.saturating_add(
                    input.saturating_mul(self.input_microusd_per_token.unwrap_or(0)),
                );
            }
        }
        if let Some(output) = output_tokens {
            self.usage.output_tokens = self.usage.output_tokens.saturating_add(output);
            if self.pricing_known {
                self.spent_microusd = self.spent_microusd.saturating_add(
                    output.saturating_mul(self.output_microusd_per_token.unwrap_or(0)),
                );
            }
        }
        self.usage.cost_known = self.pricing_known;
        self.usage.cost_microusd = if self.pricing_known {
            Some(self.spent_microusd)
        } else {
            None
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> LimitsConfig {
        LimitsConfig {
            max_tool_calls: 2,
            max_run_seconds: 600,
            max_output_tokens: 1024,
            max_spend_cents: Some(1),
            tool_output_bytes: 65_536,
        }
    }

    #[test]
    fn tool_ceiling_is_enforced() {
        let mut budget = Budget::new(limits(), false, None, None);
        assert!(budget.next_tool_call().is_ok());
        assert!(budget.next_tool_call().is_ok());
        let err = budget.next_tool_call().unwrap_err();
        assert_eq!(err.code, ErrorCode::RunLimitExceeded);
    }

    #[test]
    fn unknown_pricing_blocks_finite_spend_cap() {
        let budget = Budget::new(limits(), false, None, None);
        let err = budget.before_request(100, 100).unwrap_err();
        assert_eq!(err.code, ErrorCode::PricingUnknown);
    }

    #[test]
    fn known_pricing_reserves_and_reconciles() {
        let mut budget = Budget::new(limits(), true, Some(3), Some(15));
        // 100*3 + 1024*15 = 15660 µ$ > 1 cent (10000 µ$) → refuse.
        assert_eq!(
            budget
                .before_request(100, 1024)
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        budget.record_usage(Some(50), Some(50));
        assert_eq!(budget.usage().input_tokens, 50);
        assert_eq!(budget.usage().cost_microusd, Some(50 * 3 + 50 * 15));
        assert!(budget.usage().cost_known);
    }

    #[test]
    fn unknown_cost_stays_null_not_zero() {
        let budget = Budget::new(limits(), false, None, None);
        let mut budget = budget;
        budget.record_usage(None, None);
        assert_eq!(budget.usage().cost_microusd, None);
        assert!(!budget.usage().cost_known);
    }

    #[test]
    fn null_spend_cap_disables_only_money_checks() {
        let mut config = limits();
        config.max_spend_cents = None;
        let budget = Budget::new(config, false, None, None);
        assert!(budget.before_request(10_000_000, 100_000).is_ok());
    }
}
