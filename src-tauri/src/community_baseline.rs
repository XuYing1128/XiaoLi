//! Release-pinned, low-confidence community fingerprint references.
//!
//! These measurements were collected in an agent-harness battery, not through
//! XiaoLi's cold API request protocol.  They are therefore useful only for an
//! experimental relative ranking.  They must never produce a physical-model
//! identity claim or a green/red contract verdict by themselves.

use crate::relay_audit::{
    compare_cell_fingerprints, CellFingerprint, ProbeCellKey, ProbeFamily, ProbeLanguage,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityBaselineComparison {
    pub baseline_id: String,
    pub model: String,
    pub source_repository: String,
    pub source_commit: String,
    pub collected_at: String,
    pub reference_protocol: String,
    pub source_data_license: String,
    pub source_sample_note: String,
    pub protocol_matched: bool,
    pub eligible_cells: usize,
    pub reference_samples: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_js_divergence: Option<f64>,
    pub confidence: String,
    pub relative_rank_only: bool,
    #[serde(default)]
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityBaselineAssessment {
    /// `insufficientEvidence` or `experimentalRelativeRanking`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closest_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_up_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_distance_improvement: Option<f64>,
    #[serde(default)]
    pub comparisons: Vec<CommunityBaselineComparison>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

/// Safe, response-free metadata shown on the workbench reference page. These
/// records are compiled into the release and never share the SQLite namespace
/// used by user-imported metadata.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityBaselineDescriptor {
    pub id: String,
    pub label: String,
    pub model: String,
    pub source: String,
    pub version: String,
    pub sample_count: usize,
    pub created_at: String,
    pub signed: bool,
    pub built_in: bool,
    pub reference_protocol: String,
    pub scoring_mode: String,
    #[serde(default)]
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug)]
struct CommunityReference {
    id: &'static str,
    model: &'static str,
    source_repository: &'static str,
    source_commit: &'static str,
    collected_at: &'static str,
    reference_protocol: &'static str,
    source_data_license: &'static str,
    source_sample_note: &'static str,
    cells: BTreeMap<ProbeCellKey, Vec<String>>,
}

pub fn release_community_baseline_descriptors() -> Vec<CommunityBaselineDescriptor> {
    built_in_references()
        .into_iter()
        .map(|reference| CommunityBaselineDescriptor {
            id: reference.id.to_owned(),
            label: format!("{} 公开参考", reference.model),
            model: reference.model.to_owned(),
            source: "community".to_owned(),
            version: reference.source_commit.to_owned(),
            sample_count: reference.cells.values().map(Vec::len).sum(),
            created_at: reference.collected_at.to_owned(),
            signed: false,
            built_in: true,
            reference_protocol: reference.reference_protocol.to_owned(),
            scoring_mode: "experimentalRelativeRanking".to_owned(),
            limitations: vec![
                "releasePinnedPublicDistribution".to_owned(),
                "crossProtocolLowConfidenceOnly".to_owned(),
                "cannotChangeOverallVerdict".to_owned(),
                "cannotProvePhysicalModel".to_owned(),
            ],
        })
        .collect()
}

/// Compare already-normalized, content-bounded probe samples with every
/// release-pinned reference.  No prompt or response body is retained here.
pub fn compare_release_community_baselines(
    observed: &BTreeMap<ProbeCellKey, Vec<String>>,
) -> CommunityBaselineAssessment {
    let mut comparisons = built_in_references()
        .into_iter()
        .map(|reference| compare_reference(observed, reference))
        .collect::<Vec<_>>();
    comparisons.sort_by(
        |left, right| match (left.mean_js_divergence, right.mean_js_divergence) {
            (Some(left), Some(right)) => left.total_cmp(&right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.model.cmp(&right.model),
        },
    );
    let ranked = comparisons
        .iter()
        .filter(|item| item.eligible_cells >= 4 && item.mean_js_divergence.is_some())
        .take(2)
        .collect::<Vec<_>>();
    let (closest_model, runner_up_model, relative_distance_improvement) = if ranked.len() == 2 {
        let closest = ranked[0].mean_js_divergence.unwrap_or(1.0);
        let runner_up = ranked[1].mean_js_divergence.unwrap_or(1.0);
        let improvement = if runner_up > 0.0 {
            ((runner_up - closest) / runner_up).max(0.0)
        } else {
            0.0
        };
        (
            Some(ranked[0].model.clone()),
            Some(ranked[1].model.clone()),
            Some(improvement),
        )
    } else {
        (None, None, None)
    };
    let state = if relative_distance_improvement.is_some_and(|value| value >= 0.30) {
        "experimentalRelativeRanking"
    } else {
        "insufficientEvidence"
    };
    CommunityBaselineAssessment {
        state: state.to_owned(),
        closest_model,
        runner_up_model,
        relative_distance_improvement,
        comparisons,
        limitations: vec![
            "community references are release-pinned public measurements, not a live matched official endpoint"
                .to_owned(),
            "the compared request protocols differ, so the result is relative ranking only"
                .to_owned(),
            "community ranking cannot set the overall audit verdict or identify a physical model"
                .to_owned(),
        ],
    }
}

fn compare_reference(
    observed: &BTreeMap<ProbeCellKey, Vec<String>>,
    reference: CommunityReference,
) -> CommunityBaselineComparison {
    let mut divergences = Vec::new();
    let mut reference_samples = 0usize;
    for (cell, reference_values) in &reference.cells {
        let Some(observed_values) = observed.get(cell) else {
            continue;
        };
        let observed_fingerprint =
            CellFingerprint::from_responses(*cell, observed_values.iter().map(String::as_str));
        let reference_fingerprint =
            CellFingerprint::from_responses(*cell, reference_values.iter().map(String::as_str));
        if let Some(comparison) =
            compare_cell_fingerprints(&observed_fingerprint, &reference_fingerprint)
        {
            divergences.push(comparison.js_divergence);
            reference_samples += reference_fingerprint.valid_count;
        }
    }
    let mean_js_divergence = (!divergences.is_empty())
        .then(|| divergences.iter().sum::<f64>() / divergences.len() as f64);
    CommunityBaselineComparison {
        baseline_id: reference.id.to_owned(),
        model: reference.model.to_owned(),
        source_repository: reference.source_repository.to_owned(),
        source_commit: reference.source_commit.to_owned(),
        collected_at: reference.collected_at.to_owned(),
        reference_protocol: reference.reference_protocol.to_owned(),
        source_data_license: reference.source_data_license.to_owned(),
        source_sample_note: reference.source_sample_note.to_owned(),
        protocol_matched: false,
        eligible_cells: divergences.len(),
        reference_samples,
        mean_js_divergence,
        confidence: "low".to_owned(),
        relative_rank_only: true,
        limitations: vec![
            format!(
                "the community reference used {}, while XiaoLi sends randomized independent API probes",
                reference.reference_protocol
            ),
            "cross-protocol distance is experimental ranking evidence and cannot issue PASS/FAIL or prove a physical model"
                .to_owned(),
            reference.source_sample_note.to_owned(),
            "a relay that recognizes audit traffic may selectively route around the test".to_owned(),
        ],
    }
}

fn built_in_references() -> [CommunityReference; 3] {
    const FPVERIFY_REPOSITORY: &str = "https://github.com/Mohamed7415/fpverify";
    const FPVERIFY_COMMIT: &str = "bcd60d955c92efdc6419a628f10de07a6d123ee5";
    const LLM_FP_REPOSITORY: &str = "https://github.com/dreamor/llm-fingerprint";
    const LLM_FP_COMMIT: &str = "133d40c117980b5c52d0873b8e25d5cc7616e043";
    [
        CommunityReference {
            id: "fpverify-2026-07-gpt56-sol",
            model: "gpt-5.6-sol",
            source_repository: FPVERIFY_REPOSITORY,
            source_commit: FPVERIFY_COMMIT,
            collected_at: "2026-07",
            reference_protocol: "cursor-harness/harness-battery",
            source_data_license: "MIT repository data",
            source_sample_note:
                "the public July 2026 sample has only 11 observations per compatible cell",
            cells: cells([
                (
                    ProbeFamily::Number,
                    ProbeLanguage::English,
                    &[("73", 10), ("47", 1)],
                ),
                (
                    ProbeFamily::Letter,
                    ProbeLanguage::English,
                    &[("k", 8), ("q", 3)],
                ),
                (
                    ProbeFamily::Color,
                    ProbeLanguage::English,
                    &[("orange", 11)],
                ),
                (
                    ProbeFamily::Animal,
                    ProbeLanguage::English,
                    &[("otter", 11)],
                ),
                (ProbeFamily::City, ProbeLanguage::English, &[("lisbon", 11)]),
                (
                    ProbeFamily::Number,
                    ProbeLanguage::Chinese,
                    &[("38", 3), ("63", 1), ("26", 4), ("36", 2), ("28", 1)],
                ),
            ]),
        },
        CommunityReference {
            id: "fpverify-2026-07-gpt56-terra",
            model: "gpt-5.6-terra",
            source_repository: FPVERIFY_REPOSITORY,
            source_commit: FPVERIFY_COMMIT,
            collected_at: "2026-07",
            reference_protocol: "cursor-harness/harness-battery",
            source_data_license: "MIT repository data",
            source_sample_note:
                "the public July 2026 sample has only 11 observations per compatible cell",
            cells: cells([
                (
                    ProbeFamily::Number,
                    ProbeLanguage::English,
                    &[("73", 3), ("37", 3), ("47", 4), ("42", 1)],
                ),
                (
                    ProbeFamily::Letter,
                    ProbeLanguage::English,
                    &[("k", 7), ("m", 3), ("q", 1)],
                ),
                (
                    ProbeFamily::Color,
                    ProbeLanguage::English,
                    &[("teal", 10), ("orange", 1)],
                ),
                (
                    ProbeFamily::Animal,
                    ProbeLanguage::English,
                    &[("otter", 11)],
                ),
                (
                    ProbeFamily::City,
                    ProbeLanguage::English,
                    &[("lisbon", 7), ("kyoto", 4)],
                ),
                (
                    ProbeFamily::Number,
                    ProbeLanguage::Chinese,
                    &[("28", 3), ("82", 5), ("81", 1), ("63", 1), ("64", 1)],
                ),
            ]),
        },
        CommunityReference {
            id: "llm-fingerprint-2026-07-gpt55",
            model: "gpt-5.5",
            source_repository: LLM_FP_REPOSITORY,
            source_commit: LLM_FP_COMMIT,
            collected_at: "2026-07-21",
            reference_protocol: "openrouter/cold-single-question/prompts-v1",
            source_data_license: "CC-BY research data in an MIT repository",
            source_sample_note:
                "the public July 2026 sample has 30 observations per compatible cell",
            cells: cells([
                (
                    ProbeFamily::Number,
                    ProbeLanguage::English,
                    &[("37", 2), ("42", 6), ("47", 19), ("57", 3)],
                ),
                (
                    ProbeFamily::Number,
                    ProbeLanguage::Chinese,
                    &[("37", 5), ("42", 1), ("47", 24)],
                ),
                (
                    ProbeFamily::Letter,
                    ProbeLanguage::English,
                    &[("q", 27), ("k", 3)],
                ),
                (
                    ProbeFamily::Color,
                    ProbeLanguage::English,
                    &[("purple", 15), ("blue", 15)],
                ),
                (
                    ProbeFamily::Animal,
                    ProbeLanguage::English,
                    &[
                        ("giraffe", 12),
                        ("pangolin", 5),
                        ("capybara", 4),
                        ("platypus", 2),
                        ("tapir", 2),
                        ("otter", 2),
                        ("koala", 1),
                        ("ocelot", 1),
                        ("elephant", 1),
                    ],
                ),
                (
                    ProbeFamily::City,
                    ProbeLanguage::English,
                    &[
                        ("valencia", 9),
                        ("lisbon", 8),
                        ("kyoto", 6),
                        ("tokyo", 2),
                        ("copenhagen", 2),
                        ("quito", 1),
                        ("reykjavik", 1),
                        ("barcelona", 1),
                    ],
                ),
            ]),
        },
    ]
}

type ResponseCounts = &'static [(&'static str, usize)];
type CommunityCellSpec = (ProbeFamily, ProbeLanguage, ResponseCounts);

fn cells<const N: usize>(values: [CommunityCellSpec; N]) -> BTreeMap<ProbeCellKey, Vec<String>> {
    values
        .into_iter()
        .map(|(family, language, counts)| {
            let samples = counts
                .iter()
                .flat_map(|(value, count)| std::iter::repeat_n((*value).to_owned(), *count))
                .collect::<Vec<_>>();
            (ProbeCellKey { family, language }, samples)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_sol_fixture_ranks_sol_ahead_of_terra_but_stays_cross_protocol() {
        let references = built_in_references();
        let observed = references[0].cells.clone();
        let assessment = compare_release_community_baselines(&observed);
        assert_eq!(assessment.comparisons.len(), 3);
        let sol = assessment
            .comparisons
            .iter()
            .find(|item| item.model == "gpt-5.6-sol")
            .unwrap();
        let terra = assessment
            .comparisons
            .iter()
            .find(|item| item.model == "gpt-5.6-terra")
            .unwrap();
        assert_eq!(sol.mean_js_divergence, Some(0.0));
        assert!(terra.mean_js_divergence.unwrap() > sol.mean_js_divergence.unwrap());
        assert!(!sol.protocol_matched);
        assert!(sol.relative_rank_only);
        assert_eq!(sol.confidence, "low");
        assert_eq!(assessment.closest_model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(assessment.state, "experimentalRelativeRanking");
    }

    #[test]
    fn missing_cells_produce_insufficient_quantitative_evidence() {
        let assessment = compare_release_community_baselines(&BTreeMap::new());
        assert!(assessment
            .comparisons
            .iter()
            .all(|item| item.eligible_cells == 0 && item.mean_js_divergence.is_none()));
        assert_eq!(assessment.state, "insufficientEvidence");
        assert!(assessment.closest_model.is_none());
    }

    #[test]
    fn descriptors_are_release_pinned_and_never_claim_signed_or_matched_data() {
        let descriptors = release_community_baseline_descriptors();
        assert_eq!(descriptors.len(), 3);
        assert!(descriptors.iter().all(|item| {
            item.built_in
                && !item.signed
                && item.source == "community"
                && item.scoring_mode == "experimentalRelativeRanking"
                && item.version.len() == 40
                && !item.reference_protocol.is_empty()
        }));
    }
}
