//! Active-model token counts and receipt-backed comparisons for context representations.
//! Missing, tied, or non-comparable observations always select native text.

use crate::{
    Digest,
    inference::{Model, UsdCost},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentationKind {
    Native,
    Bitmap,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairedInputTokens {
    pub native: u64,
    pub bitmap: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepresentationObservation {
    pub version: u32,
    pub model: Model,
    pub view_revision: u64,
    pub source_history: Digest,
    pub controls: Digest,
    pub selected: BTreeMap<Digest, RepresentationKind>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub paired_input_tokens: BTreeMap<Digest, PairedInputTokens>,
    pub source_bytes: u64,
    pub bitmap_pages: u64,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cost: Option<UsdCost>,
}

impl RepresentationObservation {
    fn receipt(&self) -> Option<Receipt> {
        let input = self.input_tokens?;
        let cached = self.cached_input_tokens?;
        let output = self.output_tokens?;
        let reasoning = self.reasoning_tokens?;
        if cached > input || reasoning > output {
            return None;
        }
        Some(Receipt {
            input,
            cached,
            output,
            reasoning,
            cost: self.cost?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Receipt {
    input: u64,
    cached: u64,
    output: u64,
    reasoning: u64,
    cost: UsdCost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RepresentationEstimate {
    pub selected: RepresentationKind,
    pub native_input_tokens: u64,
    pub bitmap_input_tokens: u64,
    pub native_cost: UsdCost,
    pub bitmap_cost: UsdCost,
    pub estimated_savings: UsdCost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Recommendation {
    Native,
    MeasureBitmap,
    Bitmap,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepresentationProfile {
    observations: Vec<RepresentationObservation>,
}

impl RepresentationProfile {
    pub fn observe(&mut self, observation: RepresentationObservation) {
        self.observations.push(observation);
    }

    /// Compare calls only when source history, controls, all other segment
    /// choices, output tax, and reasoning tax match. This prevents unrelated
    /// prompt growth or model behavior from being labeled representation savings.
    pub fn estimate(&self, model: Model, segment: Digest) -> Option<RepresentationEstimate> {
        let mut pairs = Vec::new();
        for native in self.observations.iter().filter(|observation| {
            observation.model == model
                && observation.selected.get(&segment) == Some(&RepresentationKind::Native)
        }) {
            let Some(native_receipt) = native.receipt() else {
                continue;
            };
            for bitmap in self.observations.iter().filter(|observation| {
                observation.model == model
                    && observation.source_history == native.source_history
                    && observation.controls == native.controls
                    && observation.selected.get(&segment) == Some(&RepresentationKind::Bitmap)
                    && same_other_segments(&native.selected, &observation.selected, segment)
            }) {
                let Some(bitmap_receipt) = bitmap.receipt() else {
                    continue;
                };
                if native_receipt.output != bitmap_receipt.output
                    || native_receipt.reasoning != bitmap_receipt.reasoning
                {
                    continue;
                }
                pairs.push((native_receipt, bitmap_receipt));
            }
        }
        let (native, bitmap) = *pairs.first()?;
        if pairs.iter().any(|pair| *pair != (native, bitmap)) {
            return None;
        }
        let (selected, estimated_savings) = if bitmap.cost < native.cost {
            (
                RepresentationKind::Bitmap,
                native.cost.checked_sub(bitmap.cost)?,
            )
        } else {
            (
                RepresentationKind::Native,
                bitmap
                    .cost
                    .checked_sub(native.cost)
                    .unwrap_or(UsdCost::ZERO),
            )
        };
        Some(RepresentationEstimate {
            selected,
            native_input_tokens: native.input,
            bitmap_input_tokens: bitmap.input,
            native_cost: native.cost,
            bitmap_cost: bitmap.cost,
            estimated_savings,
        })
    }

    pub fn recommendation(&self, model: Model, segment: Digest) -> Recommendation {
        if let Some(pair) = self
            .observations
            .iter()
            .rev()
            .filter(|observation| observation.model == model)
            .find_map(|observation| observation.paired_input_tokens.get(&segment))
        {
            return if pair.bitmap < pair.native {
                Recommendation::Bitmap
            } else {
                Recommendation::Native
            };
        }
        if let Some(estimate) = self.estimate(model, segment) {
            return match estimate.selected {
                RepresentationKind::Native => Recommendation::Native,
                RepresentationKind::Bitmap => Recommendation::Bitmap,
            };
        }
        let native = self.observations.iter().any(|observation| {
            observation.model == model
                && observation.selected.get(&segment) == Some(&RepresentationKind::Native)
                && observation.receipt().is_some()
        });
        let bitmap_seen = self.observations.iter().any(|observation| {
            observation.model == model
                && observation.selected.get(&segment) == Some(&RepresentationKind::Bitmap)
        });
        if native && !bitmap_seen {
            Recommendation::MeasureBitmap
        } else {
            Recommendation::Native
        }
    }

    pub fn select(&self, model: Model, segment: Digest) -> RepresentationKind {
        self.estimate(model, segment)
            .map_or(RepresentationKind::Native, |estimate| estimate.selected)
    }
}

fn same_other_segments(
    left: &BTreeMap<Digest, RepresentationKind>,
    right: &BTreeMap<Digest, RepresentationKind>,
    target: Digest,
) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.remove(&target);
    right.remove(&target);
    left == right
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn observation(kind: RepresentationKind, cost: &str, input: u64) -> RepresentationObservation {
        RepresentationObservation {
            version: 1,
            model: Model::Sol,
            view_revision: 7,
            source_history: Digest::of(b"history"),
            controls: Digest::of(b"controls"),
            selected: BTreeMap::from([(Digest::of(b"segment"), kind)]),
            paired_input_tokens: BTreeMap::new(),
            source_bytes: 4096,
            bitmap_pages: u64::from(kind == RepresentationKind::Bitmap),
            input_tokens: Some(input),
            cached_input_tokens: Some(10),
            output_tokens: Some(20),
            reasoning_tokens: Some(5),
            cost: Some(UsdCost::from_str(cost).unwrap()),
        }
    }

    #[test]
    fn selects_only_from_consistent_comparable_receipts() {
        let segment = Digest::of(b"segment");
        let mut profile = RepresentationProfile::default();
        assert_eq!(
            profile.select(Model::Sol, segment),
            RepresentationKind::Native
        );
        profile.observe(observation(RepresentationKind::Native, "0.10", 1000));
        assert_eq!(
            profile.select(Model::Sol, segment),
            RepresentationKind::Native
        );
        profile.observe(observation(RepresentationKind::Bitmap, "0.08", 600));
        let estimate = profile.estimate(Model::Sol, segment).unwrap();
        assert_eq!(estimate.selected, RepresentationKind::Bitmap);
        assert_eq!(estimate.estimated_savings.to_string(), "$0.02");

        profile.observe(observation(RepresentationKind::Bitmap, "0.09", 650));
        assert!(profile.estimate(Model::Sol, segment).is_none());
        assert_eq!(
            profile.select(Model::Sol, segment),
            RepresentationKind::Native
        );
    }

    #[test]
    fn missing_receipts_and_changed_output_tax_stay_native() {
        let segment = Digest::of(b"segment");
        let mut native = observation(RepresentationKind::Native, "0.10", 1000);
        native.cost = None;
        let mut bitmap = observation(RepresentationKind::Bitmap, "0.08", 600);
        let mut profile = RepresentationProfile::default();
        profile.observe(native);
        profile.observe(bitmap.clone());
        assert_eq!(
            profile.select(Model::Sol, segment),
            RepresentationKind::Native
        );

        let mut profile = RepresentationProfile::default();
        let native = observation(RepresentationKind::Native, "0.10", 1000);
        bitmap.output_tokens = Some(21);
        profile.observe(native);
        profile.observe(bitmap);
        assert_eq!(
            profile.select(Model::Sol, segment),
            RepresentationKind::Native
        );
    }

    #[test]
    fn exact_count_pair_selects_bitmap_without_matching_generation_receipts() {
        let segment = Digest::of(b"segment");
        let mut measured = observation(RepresentationKind::Native, "0.10", 500);
        measured.paired_input_tokens.insert(
            segment,
            PairedInputTokens {
                native: 500,
                bitmap: 120,
            },
        );
        let mut profile = RepresentationProfile::default();
        profile.observe(measured);

        assert_eq!(
            profile.recommendation(Model::Sol, segment),
            Recommendation::Bitmap
        );
    }

    #[test]
    fn exact_count_pair_keeps_native_on_ties() {
        let segment = Digest::of(b"segment");
        let mut measured = observation(RepresentationKind::Bitmap, "0.10", 500);
        measured.paired_input_tokens.insert(
            segment,
            PairedInputTokens {
                native: 120,
                bitmap: 120,
            },
        );
        let mut profile = RepresentationProfile::default();
        profile.observe(measured);

        assert_eq!(
            profile.recommendation(Model::Sol, segment),
            Recommendation::Native
        );
    }
}
