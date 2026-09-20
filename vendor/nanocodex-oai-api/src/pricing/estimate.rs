use super::UsdAmount;
use crate::{Model, Usage};
use serde::{Deserialize, Serialize};

// OpenAI publishes rates per one million tokens. All supported rates convert
// exactly to nano-USD per token, avoiding floating point and division.
const SOL_STANDARD: TokenRates = TokenRates {
    input: 5_000,
    cached_input: 500,
    cache_write_input: 6_250,
    output: 30_000,
};
const SOL_PRIORITY: TokenRates = TokenRates {
    input: 10_000,
    cached_input: 1_000,
    cache_write_input: 12_500,
    output: 60_000,
};
const TERRA_STANDARD: TokenRates = TokenRates {
    input: 2_000,
    cached_input: 200,
    cache_write_input: 2_500,
    output: 12_000,
};
const TERRA_PRIORITY: TokenRates = TokenRates {
    input: 4_000,
    cached_input: 400,
    cache_write_input: 5_000,
    output: 24_000,
};
const LUNA_STANDARD: TokenRates = TokenRates {
    input: 200,
    cached_input: 20,
    cache_write_input: 250,
    output: 1_200,
};
const LUNA_PRIORITY: TokenRates = TokenRates {
    input: 400,
    cached_input: 40,
    cache_write_input: 500,
    output: 2_400,
};

#[derive(Clone, Copy)]
struct TokenRates {
    input: u64,
    cached_input: u64,
    cache_write_input: u64,
    output: u64,
}

impl TokenRates {
    const fn for_model(model: Model, service_tier: ServiceTier) -> Self {
        match (model, service_tier) {
            (Model::Sol, ServiceTier::Standard) => SOL_STANDARD,
            (Model::Sol, ServiceTier::Priority) => SOL_PRIORITY,
            (Model::Terra, ServiceTier::Standard) => TERRA_STANDARD,
            (Model::Terra, ServiceTier::Priority) => TERRA_PRIORITY,
            (Model::Luna, ServiceTier::Standard) => LUNA_STANDARD,
            (Model::Luna, ServiceTier::Priority) => LUNA_PRIORITY,
        }
    }
}

/// OpenAI service tiers supported by Nanocodex.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    /// Standard processing and token rates.
    #[default]
    Standard,
    /// Priority processing selected by `fast_mode`.
    Priority,
}

impl ServiceTier {
    /// Returns the OpenAI service-tier name used in events and traces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Priority => "priority",
        }
    }
}

/// Exact estimated USD cost for provider-reported token usage.
///
/// Nanocodex calculates this automatically using the selected model's built-in
/// standard or priority rates. This is a local estimate, not a charge reported
/// by the Responses API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EstimatedUsdCost {
    #[serde(rename = "usd")]
    amount: UsdAmount,
    #[serde(rename = "input_usd")]
    input: UsdAmount,
    #[serde(rename = "cached_input_usd")]
    cached_input: UsdAmount,
    #[serde(rename = "cache_write_input_usd")]
    cache_write_input: UsdAmount,
    #[serde(rename = "output_usd")]
    output: UsdAmount,
    #[serde(default)]
    service_tier: ServiceTier,
}

impl EstimatedUsdCost {
    /// Returns the exact aggregate estimate.
    #[must_use]
    pub const fn amount(&self) -> UsdAmount {
        self.amount
    }

    /// Returns the ordinary-input component.
    #[must_use]
    pub const fn input(&self) -> UsdAmount {
        self.input
    }

    /// Returns the cache-read component.
    #[must_use]
    pub const fn cached_input(&self) -> UsdAmount {
        self.cached_input
    }

    /// Returns the cache-write component.
    #[must_use]
    pub const fn cache_write_input(&self) -> UsdAmount {
        self.cache_write_input
    }

    /// Returns the output component, including reasoning output.
    #[must_use]
    pub const fn output(&self) -> UsdAmount {
        self.output
    }

    /// Returns the service tier whose built-in rates were applied.
    #[must_use]
    pub const fn service_tier(&self) -> ServiceTier {
        self.service_tier
    }
}

/// Estimates one provider operation from its authoritative usage record.
///
/// Cached and cache-write tokens are subsets of `input_tokens`; this function
/// subtracts both before pricing ordinary input. The returned value is a local
/// estimate, not a charge reported by the Responses API.
///
/// ```
/// use nanocodex_oai_api::{
///     pricing::{ServiceTier, estimate},
///     responses::{InputTokenDetails, Usage},
/// };
///
/// let usage = Usage {
///     input_tokens: 1_000,
///     input_tokens_details: Some(InputTokenDetails {
///         cached_tokens: 800,
///         cache_write_tokens: 100,
///     }),
///     output_tokens: 50,
///     total_tokens: 1_050,
///     ..Usage::default()
/// };
/// let cost = estimate(&usage, ServiceTier::Standard);
///
/// assert_eq!(cost.amount().decimal(), "0.003025");
/// ```
#[must_use]
pub fn estimate(usage: &Usage, service_tier: ServiceTier) -> EstimatedUsdCost {
    estimate_for_model(usage, Model::Sol, service_tier)
}

/// Estimates one provider operation using the selected model's built-in rates.
///
/// This is the model-aware form of [`estimate`]. Managed sessions use it so
/// every supported GPT-5.6 model receives an estimate from reported usage.
#[must_use]
pub fn estimate_for_model(
    usage: &Usage,
    model: Model,
    service_tier: ServiceTier,
) -> EstimatedUsdCost {
    let cached_input_tokens = usage
        .input_tokens_details
        .as_ref()
        .map_or(0, |details| details.cached_tokens);
    let cache_write_input_tokens = usage
        .input_tokens_details
        .as_ref()
        .map_or(0, |details| details.cache_write_tokens);
    estimate_tokens(
        usage.input_tokens,
        cached_input_tokens,
        cache_write_input_tokens,
        usage.output_tokens,
        model,
        service_tier,
    )
}

pub(crate) fn estimate_tokens(
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    model: Model,
    service_tier: ServiceTier,
) -> EstimatedUsdCost {
    let rates = TokenRates::for_model(model, service_tier);
    let cached_input_tokens = cached_input_tokens.min(input_tokens);
    let remaining_input = input_tokens.saturating_sub(cached_input_tokens);
    let cache_write_input_tokens = cache_write_input_tokens.min(remaining_input);
    let ordinary_input_tokens = remaining_input.saturating_sub(cache_write_input_tokens);

    let input = UsdAmount::saturating_mul(ordinary_input_tokens, rates.input);
    let cached_input = UsdAmount::saturating_mul(cached_input_tokens, rates.cached_input);
    let cache_write_input =
        UsdAmount::saturating_mul(cache_write_input_tokens, rates.cache_write_input);
    let output = UsdAmount::saturating_mul(output_tokens, rates.output);
    let amount = input
        .saturating_add(cached_input)
        .saturating_add(cache_write_input)
        .saturating_add(output);

    EstimatedUsdCost {
        amount,
        input,
        cached_input,
        cache_write_input,
        output,
        service_tier,
    }
}

#[cfg(test)]
mod tests {
    use super::{ServiceTier, estimate, estimate_for_model, estimate_tokens};
    use crate::{
        Model,
        responses::{InputTokenDetails, OutputTokenDetails, Usage},
    };

    #[test]
    fn standard_rates_price_each_input_class_once() {
        let estimate = estimate(
            &Usage {
                input_tokens: 1_000_000,
                input_tokens_details: Some(InputTokenDetails {
                    cached_tokens: 250_000,
                    cache_write_tokens: 100_000,
                }),
                output_tokens: 200_000,
                output_tokens_details: Some(OutputTokenDetails {
                    reasoning_tokens: 150_000,
                }),
                total_tokens: 1_200_000,
            },
            ServiceTier::Standard,
        );

        assert_eq!(estimate.input().decimal(), "3.25");
        assert_eq!(estimate.cached_input().decimal(), "0.125");
        assert_eq!(estimate.cache_write_input().decimal(), "0.625");
        assert_eq!(estimate.output().decimal(), "6");
        assert_eq!(estimate.amount().decimal(), "10");
    }

    #[test]
    fn priority_rates_are_selected_by_fast_mode() {
        let standard = estimate_tokens(
            1_000_000,
            0,
            0,
            1_000_000,
            Model::Sol,
            ServiceTier::Standard,
        );
        let priority = estimate_tokens(
            1_000_000,
            0,
            0,
            1_000_000,
            Model::Sol,
            ServiceTier::Priority,
        );

        assert_eq!(standard.amount().decimal(), "35");
        assert_eq!(priority.amount().decimal(), "70");
        assert_eq!(priority.service_tier(), ServiceTier::Priority);
        assert_eq!(priority.service_tier().as_str(), "priority");
    }

    #[test]
    fn luna_rates_cover_standard_and_priority_usage() {
        let usage = Usage {
            input_tokens: 1_000_000,
            input_tokens_details: Some(InputTokenDetails {
                cached_tokens: 200_000,
                cache_write_tokens: 100_000,
            }),
            output_tokens: 1_000_000,
            ..Usage::default()
        };

        let standard = estimate_for_model(&usage, Model::Luna, ServiceTier::Standard);
        let priority = estimate_for_model(&usage, Model::Luna, ServiceTier::Priority);

        assert_eq!(standard.input().decimal(), "0.14");
        assert_eq!(standard.cached_input().decimal(), "0.004");
        assert_eq!(standard.cache_write_input().decimal(), "0.025");
        assert_eq!(standard.output().decimal(), "1.2");
        assert_eq!(standard.amount().decimal(), "1.369");
        assert_eq!(priority.amount().decimal(), "2.738");
    }

    #[test]
    fn terra_rates_cover_standard_and_priority_usage() {
        let usage = Usage {
            input_tokens: 1_000_000,
            input_tokens_details: Some(InputTokenDetails {
                cached_tokens: 200_000,
                cache_write_tokens: 100_000,
            }),
            output_tokens: 1_000_000,
            ..Usage::default()
        };

        let standard = estimate_for_model(&usage, Model::Terra, ServiceTier::Standard);
        let priority = estimate_for_model(&usage, Model::Terra, ServiceTier::Priority);

        assert_eq!(standard.input().decimal(), "1.4");
        assert_eq!(standard.cached_input().decimal(), "0.04");
        assert_eq!(standard.cache_write_input().decimal(), "0.25");
        assert_eq!(standard.output().decimal(), "12");
        assert_eq!(standard.amount().decimal(), "13.69");
        assert_eq!(priority.amount().decimal(), "27.38");
    }

    #[test]
    fn malformed_detail_counts_do_not_double_charge_input() {
        let estimate = estimate_tokens(10, 8, 8, 0, Model::Sol, ServiceTier::Standard);

        assert_eq!(estimate.input().nano_usd(), 0);
        assert_eq!(estimate.cached_input().nano_usd(), 4_000);
        assert_eq!(estimate.cache_write_input().nano_usd(), 12_500);
    }
}
