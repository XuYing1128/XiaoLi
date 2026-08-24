use crate::model::{
    BehaviorSampleV2, CompletedTurnSample, ModelItemInterval, QualityAssessment, QualityComparator,
    QualityFactor, TimingSnapshot, TokenUsage, TtftEvidence, UsageSnapshot,
};
use serde::{Deserialize, Serialize};

/// Computes a turn-local cumulative usage from Codex's thread-cumulative total.
/// A field that moves backwards is treated as a server/process counter reset and
/// starts from the new value. Cached input and reasoning output remain subsets;
/// they are never added to `total_tokens` here.
pub fn usage_since_baseline(total: &TokenUsage, baseline: &TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: counter_delta(total.input_tokens, baseline.input_tokens),
        cached_input_tokens: counter_delta(total.cached_input_tokens, baseline.cached_input_tokens),
        cache_write_input_tokens: counter_delta(
            total.cache_write_input_tokens,
            baseline.cache_write_input_tokens,
        ),
        output_tokens: counter_delta(total.output_tokens, baseline.output_tokens),
        reasoning_output_tokens: counter_delta(
            total.reasoning_output_tokens,
            baseline.reasoning_output_tokens,
        ),
        total_tokens: counter_delta(total.total_tokens, baseline.total_tokens),
    }
}

pub fn cache_input_share(usage: &TokenUsage) -> Option<f64> {
    if usage.input_tokens == 0 {
        return None;
    }
    Some((usage.cached_input_tokens.min(usage.input_tokens) as f64) / usage.input_tokens as f64)
}

pub fn context_input_share(usage: &TokenUsage, context_window: Option<u64>) -> Option<f64> {
    let window = context_window.filter(|value| *value > 0)?;
    Some((usage.input_tokens as f64 / window as f64).max(0.0))
}

/// This is an end-to-end observed rate for the complete turn. It may include
/// tool time and must not be labelled as the server's pure generation TPS.
pub fn observed_output_rate(output_tokens: u64, duration_ms: Option<u64>) -> Option<f64> {
    let duration_ms = duration_ms.filter(|value| *value > 0)?;
    Some(output_tokens as f64 / (duration_ms as f64 / 1_000.0))
}

pub fn usage_snapshot(
    last: TokenUsage,
    cumulative: TokenUsage,
    context_window: Option<u64>,
) -> UsageSnapshot {
    UsageSnapshot {
        last_cache_input_share: cache_input_share(&last),
        cache_input_share: cache_input_share(&cumulative),
        context_input_share: context_input_share(&last, context_window),
        last,
        cumulative,
        context_window,
    }
}

pub fn timing_snapshot(
    elapsed_ms: Option<u64>,
    ttft_ms: Option<u64>,
    duration_ms: Option<u64>,
    output_tokens: u64,
    turn_started_at_ms: Option<u64>,
    model_intervals: &[ModelItemInterval],
) -> TimingSnapshot {
    let model_active_ms = union_interval_ms(
        model_intervals
            .iter()
            .map(|interval| (interval.started_at_ms, interval.completed_at_ms)),
    );
    let effective_duration = duration_ms.or(elapsed_ms);
    let end_to_end_output_rate = observed_output_rate(output_tokens, effective_duration);
    let model_phase_output_rate = observed_output_rate(output_tokens, model_active_ms);
    let ttft_evidence = if let Some(exact) = ttft_ms {
        TtftEvidence {
            kind: "exactTerminal".to_owned(),
            lower_ms: Some(exact),
            upper_ms: Some(exact),
        }
    } else if let (Some(started), Some(first)) = (
        turn_started_at_ms,
        model_intervals
            .iter()
            .min_by_key(|interval| interval.started_at_ms),
    ) {
        TtftEvidence {
            kind: "estimatedWindow".to_owned(),
            lower_ms: Some(first.started_at_ms.saturating_sub(started)),
            upper_ms: Some(first.completed_at_ms.saturating_sub(started)),
        }
    } else {
        TtftEvidence::default()
    };
    TimingSnapshot {
        elapsed_ms,
        ttft_ms,
        duration_ms,
        ttft_evidence,
        model_active_ms,
        end_to_end_output_rate,
        model_phase_output_rate,
        observed_output_rate: end_to_end_output_rate,
    }
}

pub fn union_interval_ms(intervals: impl Iterator<Item = (u64, u64)>) -> Option<u64> {
    let mut ranges = intervals
        .filter(|(start, end)| end >= start)
        .collect::<Vec<_>>();
    if ranges.is_empty() {
        return None;
    }
    ranges.sort_unstable_by_key(|range| (range.0, range.1));
    let mut total = 0_u64;
    let (mut current_start, mut current_end) = ranges[0];
    for (start, end) in ranges.into_iter().skip(1) {
        if start <= current_end {
            current_end = current_end.max(end);
        } else {
            total = total.saturating_add(current_end.saturating_sub(current_start));
            current_start = start;
            current_end = end;
        }
    }
    Some(total.saturating_add(current_end.saturating_sub(current_start)))
}

pub fn reasoning_active_ms(intervals: &[ModelItemInterval]) -> Option<u64> {
    union_interval_ms(
        intervals
            .iter()
            .filter(|interval| interval.item_type.eq_ignore_ascii_case("Reasoning"))
            .map(|interval| (interval.started_at_ms, interval.completed_at_ms)),
    )
}

fn counter_delta(total: u64, baseline: u64) -> u64 {
    if total >= baseline {
        total - baseline
    } else {
        total
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorAssessment {
    pub eligible: bool,
    pub sample_count: usize,
    pub yellow_anomaly: bool,
    #[serde(default)]
    pub deviations: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityGateState {
    pub consecutive_hits: u32,
    pub consecutive_healthy: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_hit_checked_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_hit_output_tokens: Option<u64>,
    pub suspected: bool,
}

pub struct QualityEvaluation {
    pub assessment: QualityAssessment,
    pub gate: QualityGateState,
}

/// Evaluates a live checkpoint against clean local history. This function only
/// emits a yellow consistency assessment; it cannot create server-route
/// evidence or identify a physical model.
pub fn assess_quality_checkpoint(
    history: &[BehaviorSampleV2],
    comparator_history: &[BehaviorSampleV2],
    current: &BehaviorSampleV2,
    previous: &QualityGateState,
    checked_at_ms: u64,
) -> QualityEvaluation {
    let baseline_key = current.baseline_key();
    let matching = history
        .iter()
        .filter(|sample| eligible_baseline_sample(sample) && sample.baseline_key() == baseline_key)
        .collect::<Vec<_>>();
    let limitations = vec![
        "Behavioral telemetry is a local statistical signal, not server route evidence.".to_owned(),
        "Requested effort cannot be independently measured from token timing.".to_owned(),
    ];
    if !current.clean || current.explicit_reroute {
        let mut limitations = limitations;
        limitations.push(if current.explicit_reroute {
            "This checkpoint is excluded because an explicit server reroute was observed."
                .to_owned()
        } else {
            "This checkpoint is excluded because collector evidence is incomplete or damaged."
                .to_owned()
        });
        return QualityEvaluation {
            assessment: QualityAssessment {
                state: "learning".to_owned(),
                baseline_key,
                baseline_sample_count: matching.len(),
                consecutive_hits: 0,
                factors: Vec::new(),
                comparator: None,
                limitations,
            },
            gate: QualityGateState::default(),
        };
    }
    if matching.len() < 30 {
        return QualityEvaluation {
            assessment: QualityAssessment {
                state: "learning".to_owned(),
                baseline_key,
                baseline_sample_count: matching.len(),
                consecutive_hits: 0,
                factors: Vec::new(),
                comparator: None,
                limitations,
            },
            gate: QualityGateState::default(),
        };
    }

    let mut factors = Vec::new();
    assess_one_sided_factor(
        "ttftHigh",
        "higher",
        "ms",
        current.ttft_ms.map(|value| value as f64),
        matching
            .iter()
            .filter_map(|sample| sample.ttft_ms.map(|value| value as f64)),
        true,
        &mut factors,
    );
    assess_one_sided_factor(
        "modelPhaseOutputRateLow",
        "lower",
        "tok/s",
        current.model_phase_output_rate,
        matching
            .iter()
            .filter_map(|sample| sample.model_phase_output_rate),
        false,
        &mut factors,
    );
    assess_one_sided_factor(
        "reasoningOutputShareLow",
        "lower",
        "ratio",
        current.reasoning_output_share,
        matching
            .iter()
            .filter_map(|sample| sample.reasoning_output_share),
        false,
        &mut factors,
    );
    assess_one_sided_factor(
        "reasoningPhaseShareLow",
        "lower",
        "ratio",
        current.reasoning_phase_share,
        matching
            .iter()
            .filter_map(|sample| sample.reasoning_phase_share),
        false,
        &mut factors,
    );

    let families = factors
        .iter()
        .map(|factor| factor_family(&factor.code))
        .collect::<std::collections::HashSet<_>>();
    let raw_hit = current.output_tokens >= 128 && families.len() >= 2;
    let qualifies_as_next_hit = raw_hit
        && previous.last_hit_checked_at_ms.is_none_or(|last| {
            checked_at_ms.saturating_sub(last) >= 2_000
                && current
                    .output_tokens
                    .saturating_sub(previous.last_hit_output_tokens.unwrap_or_default())
                    >= 64
        });

    let mut gate = previous.clone();
    if qualifies_as_next_hit {
        gate.consecutive_hits = gate.consecutive_hits.saturating_add(1);
        gate.consecutive_healthy = 0;
        gate.last_hit_checked_at_ms = Some(checked_at_ms);
        gate.last_hit_output_tokens = Some(current.output_tokens);
        if gate.consecutive_hits >= 2 {
            gate.suspected = true;
        }
    } else if !raw_hit {
        gate.consecutive_healthy = gate.consecutive_healthy.saturating_add(1);
        gate.consecutive_hits = 0;
        gate.last_hit_checked_at_ms = None;
        gate.last_hit_output_tokens = None;
        if gate.consecutive_healthy >= 2 {
            gate.suspected = false;
        }
    }

    let comparator = if gate.suspected {
        assess_comparator(current, &matching, comparator_history)
    } else {
        None
    };
    QualityEvaluation {
        assessment: QualityAssessment {
            state: if gate.suspected {
                "suspectedDegradation".to_owned()
            } else {
                "consistent".to_owned()
            },
            baseline_key,
            baseline_sample_count: matching.len(),
            consecutive_hits: gate.consecutive_hits,
            factors,
            comparator,
            limitations,
        },
        gate,
    }
}

fn assess_one_sided_factor(
    code: &str,
    direction: &str,
    unit: &str,
    current: Option<f64>,
    baseline: impl Iterator<Item = f64>,
    high_is_bad: bool,
    factors: &mut Vec<QualityFactor>,
) {
    let Some(current) = current.filter(|value| value.is_finite()) else {
        return;
    };
    let mut values = baseline
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if values.len() < 30 {
        return;
    }
    let center = median(&mut values);
    let mut deviations = values
        .iter()
        .map(|value| (value - center).abs())
        .collect::<Vec<_>>();
    let mad = median(&mut deviations);
    if mad <= f64::EPSILON {
        return;
    }
    let signed = (current - center) / mad;
    let is_bad = if high_is_bad {
        signed > 4.0
    } else {
        signed < -4.0
    };
    if is_bad {
        factors.push(QualityFactor {
            code: code.to_owned(),
            direction: direction.to_owned(),
            observed: current,
            baseline_median: center,
            mad,
            robust_deviation: signed.abs(),
            unit: unit.to_owned(),
        });
    }
}

fn factor_family(code: &str) -> &'static str {
    match code {
        "ttftHigh" => "latency",
        "modelPhaseOutputRateLow" => "rate",
        "reasoningOutputShareLow" | "reasoningPhaseShareLow" => "reasoning",
        _ => "other",
    }
}

fn assess_comparator(
    current: &BehaviorSampleV2,
    requested: &[&BehaviorSampleV2],
    comparators: &[BehaviorSampleV2],
) -> Option<QualityComparator> {
    let compared = comparators
        .iter()
        .filter(|sample| {
            eligible_baseline_sample(sample)
                && sample.model.eq_ignore_ascii_case("gpt-5.5")
                && sample.effort.eq_ignore_ascii_case(&current.effort)
                && sample.uncached_input_bucket == current.uncached_input_bucket
                && sample.output_bucket == current.output_bucket
                && sample.tool_activity == current.tool_activity
        })
        .collect::<Vec<_>>();
    if compared.len() < 30 {
        return None;
    }
    let mut requested_distance = 0.0;
    let mut compared_distance = 0.0;
    let mut common = 0_usize;
    for metric in 0..4 {
        let observed = metric_value(current, metric);
        let requested_values = requested
            .iter()
            .filter_map(|sample| metric_value(sample, metric))
            .collect::<Vec<_>>();
        let compared_values = compared
            .iter()
            .filter_map(|sample| metric_value(sample, metric))
            .collect::<Vec<_>>();
        let (Some(observed), Some(requested_robust), Some(compared_robust)) = (
            observed,
            robust_center_scale(&requested_values),
            robust_center_scale(&compared_values),
        ) else {
            continue;
        };
        requested_distance += ((observed - requested_robust.0) / requested_robust.1).abs();
        compared_distance += ((observed - compared_robust.0) / compared_robust.1).abs();
        common += 1;
    }
    if common < 3 || requested_distance <= f64::EPSILON {
        return None;
    }
    let relative_distance = 1.0 - compared_distance / requested_distance;
    (relative_distance >= 0.30).then(|| QualityComparator {
        requested_model: current.model.clone(),
        compared_model: "gpt-5.5".to_owned(),
        sample_count: compared.len(),
        relative_distance,
    })
}

pub fn eligible_baseline_sample(sample: &BehaviorSampleV2) -> bool {
    sample.clean
        && !sample.explicit_reroute
        && sample.output_tokens > 0
        && sample.ttft_ms.is_some()
        && sample.model_phase_output_rate.is_some()
        && sample.reasoning_output_share.is_some()
        && sample.reasoning_phase_share.is_some()
        && !sample.model.trim().is_empty()
        && !sample.model.eq_ignore_ascii_case("unknown")
        && !sample.effort.trim().is_empty()
        && !sample.effort.eq_ignore_ascii_case("unknown")
}

fn metric_value(sample: &BehaviorSampleV2, index: usize) -> Option<f64> {
    match index {
        0 => sample.ttft_ms.map(|value| value as f64),
        1 => sample.model_phase_output_rate,
        2 => sample.reasoning_output_share,
        3 => sample.reasoning_phase_share,
        _ => None,
    }
}

fn robust_center_scale(values: &[f64]) -> Option<(f64, f64)> {
    if values.len() < 30 {
        return None;
    }
    let mut values = values.to_vec();
    let center = median(&mut values);
    let mut deviations = values
        .iter()
        .map(|value| (value - center).abs())
        .collect::<Vec<_>>();
    let mad = median(&mut deviations);
    (mad > f64::EPSILON).then_some((center, mad))
}

pub fn uncached_input_bucket(input_tokens: u64, cached_input_tokens: u64) -> &'static str {
    input_bucket(input_tokens.saturating_sub(cached_input_tokens))
}

pub fn output_bucket(output_tokens: u64) -> &'static str {
    match output_tokens {
        0..=256 => "0-256",
        257..=1_024 => "257-1024",
        1_025..=4_096 => "1025-4096",
        _ => "4097+",
    }
}

fn input_bucket(input_tokens: u64) -> &'static str {
    match input_tokens {
        0..=8_191 => "0-8k",
        8_192..=32_767 => "8k-32k",
        32_768..=131_071 => "32k-128k",
        _ => "128k+",
    }
}

/// Conservative local consistency check. It deliberately returns only a
/// yellow anomaly signal and never a model/effort identity guess.
pub fn assess_behavior(
    history: &[CompletedTurnSample],
    current: &CompletedTurnSample,
) -> BehaviorAssessment {
    let matching: Vec<&CompletedTurnSample> = history
        .iter()
        .filter(|sample| behavior_bucket_matches(sample, current))
        .collect();
    if matching.len() < 30 {
        return BehaviorAssessment {
            eligible: false,
            sample_count: matching.len(),
            yellow_anomaly: false,
            deviations: Vec::new(),
        };
    }

    let mut deviations = Vec::new();
    assess_metric(
        "ttft",
        current.ttft_ms.map(|value| value as f64),
        matching
            .iter()
            .filter_map(|sample| sample.ttft_ms.map(|value| value as f64)),
        true,
        &mut deviations,
    );
    assess_metric(
        "observedOutputRate",
        sample_output_rate(current),
        matching
            .iter()
            .filter_map(|sample| sample_output_rate(sample)),
        false,
        &mut deviations,
    );
    assess_metric(
        "cacheInputShare",
        current.cache_input_share,
        matching
            .iter()
            .filter_map(|sample| sample.cache_input_share),
        false,
        &mut deviations,
    );
    assess_metric(
        "reasoningOutputShare",
        sample_reasoning_share(current),
        matching
            .iter()
            .filter_map(|sample| sample_reasoning_share(sample)),
        false,
        &mut deviations,
    );

    BehaviorAssessment {
        eligible: true,
        sample_count: matching.len(),
        yellow_anomaly: deviations.len() >= 2,
        deviations,
    }
}

fn behavior_bucket_matches(left: &CompletedTurnSample, right: &CompletedTurnSample) -> bool {
    left.model
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        == right
            .model
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
        && left
            .effort
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            == right
                .effort
                .as_deref()
                .map(str::trim)
                .map(str::to_ascii_lowercase)
        && left.input_bucket == right.input_bucket
        && left.tool_activity == right.tool_activity
}

fn assess_metric(
    name: &str,
    current: Option<f64>,
    baseline: impl Iterator<Item = f64>,
    high_is_bad: bool,
    deviations: &mut Vec<String>,
) {
    let Some(current) = current.filter(|value| value.is_finite()) else {
        return;
    };
    let mut values: Vec<f64> = baseline.filter(|value| value.is_finite()).collect();
    if values.len() < 30 {
        return;
    }
    let center = median(&mut values);
    let mut absolute_deviations: Vec<f64> =
        values.iter().map(|value| (value - center).abs()).collect();
    let mad = median(&mut absolute_deviations);
    // A zero-MAD baseline has no measured spread. Avoid treating microscopic
    // differences as certainty; wait for a genuinely variable baseline.
    if mad <= f64::EPSILON {
        return;
    }
    // Legacy V3 compatibility only. V4 callers must use
    // assess_quality_checkpoint, whose votes are strictly one-sided and never
    // include cache share.
    let _ = high_is_bad;
    if (current - center).abs() > 4.0 * mad {
        deviations.push(name.to_owned());
    }
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn sample_output_rate(sample: &CompletedTurnSample) -> Option<f64> {
    observed_output_rate(sample.output_tokens, sample.duration_ms)
}

fn sample_reasoning_share(sample: &CompletedTurnSample) -> Option<f64> {
    if sample.output_tokens == 0 {
        return None;
    }
    Some(
        sample.reasoning_output_tokens.min(sample.output_tokens) as f64
            / sample.output_tokens as f64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, cached: u64, output: u64, reasoning: u64, total: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            cached_input_tokens: cached,
            output_tokens: output,
            reasoning_output_tokens: reasoning,
            total_tokens: total,
            ..TokenUsage::default()
        }
    }

    #[test]
    fn total_delta_does_not_double_count_cached_or_reasoning_tokens() {
        let baseline = usage(100, 80, 20, 8, 120);
        let total = usage(150, 120, 35, 14, 185);

        let delta = usage_since_baseline(&total, &baseline);

        assert_eq!(delta.input_tokens, 50);
        assert_eq!(delta.cached_input_tokens, 40);
        assert_eq!(delta.output_tokens, 15);
        assert_eq!(delta.reasoning_output_tokens, 6);
        assert_eq!(delta.total_tokens, 65);
    }

    #[test]
    fn counter_reset_starts_a_fresh_baseline_without_model_inference() {
        let baseline = usage(1_000, 900, 200, 80, 1_200);
        let reset_total = usage(50, 40, 10, 4, 60);

        assert_eq!(usage_since_baseline(&reset_total, &baseline), reset_total);
    }

    #[test]
    fn ratios_and_observed_rate_have_precise_denominators() {
        let value = usage(200, 150, 40, 10, 240);
        assert_eq!(cache_input_share(&value), Some(0.75));
        assert_eq!(context_input_share(&value, Some(1_000)), Some(0.2));
        assert_eq!(observed_output_rate(40, Some(2_000)), Some(20.0));
        assert_eq!(observed_output_rate(40, Some(0)), None);
    }

    #[test]
    fn timing_uses_active_elapsed_and_unions_overlapping_model_intervals() {
        let intervals = vec![
            ModelItemInterval {
                item_id: "reasoning".to_owned(),
                item_type: "Reasoning".to_owned(),
                started_at_ms: 1_000,
                completed_at_ms: 2_000,
            },
            ModelItemInterval {
                item_id: "message".to_owned(),
                item_type: "AgentMessage".to_owned(),
                started_at_ms: 1_500,
                completed_at_ms: 2_500,
            },
        ];
        let timing = timing_snapshot(Some(3_000), None, None, 150, Some(500), &intervals);
        assert_eq!(timing.model_active_ms, Some(1_500));
        assert_eq!(timing.end_to_end_output_rate, Some(50.0));
        assert_eq!(timing.model_phase_output_rate, Some(100.0));
        assert_eq!(timing.observed_output_rate, timing.end_to_end_output_rate);
        assert_eq!(timing.ttft_evidence.kind, "estimatedWindow");
        assert_eq!(timing.ttft_evidence.lower_ms, Some(500));
        assert_eq!(timing.ttft_evidence.upper_ms, Some(1_500));
        assert_eq!(reasoning_active_ms(&intervals), Some(1_000));

        let terminal = timing_snapshot(
            Some(3_000),
            Some(640),
            Some(2_500),
            150,
            Some(500),
            &intervals,
        );
        assert_eq!(terminal.ttft_evidence.kind, "exactTerminal");
        assert_eq!(terminal.ttft_evidence.lower_ms, Some(640));
        assert_eq!(terminal.ttft_evidence.upper_ms, Some(640));
        assert_eq!(terminal.end_to_end_output_rate, Some(60.0));
    }

    fn quality_sample(index: u64) -> BehaviorSampleV2 {
        let phase = (index % 7) as f64;
        BehaviorSampleV2 {
            thread_id: format!("quality-{index}"),
            turn_id: format!("turn-{index}"),
            model: "gpt-5.6-sol".to_owned(),
            effort: "ultra".to_owned(),
            uncached_input_bucket: "8k-32k".to_owned(),
            output_bucket: "0-256".to_owned(),
            tool_activity: false,
            output_tokens: 200,
            ttft_ms: Some(1_000 + (index % 7) * 10),
            model_phase_output_rate: Some(50.0 + phase * 0.2),
            reasoning_output_share: Some(0.40 + phase * 0.002),
            reasoning_phase_share: Some(0.50 + phase * 0.002),
            cache_input_share: Some(0.8),
            clean: true,
            explicit_reroute: false,
            observed_at: "2026-08-25T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn quality_requires_two_independent_families_and_two_spaced_checkpoints() {
        let history = (0..30).map(quality_sample).collect::<Vec<_>>();
        let mut current = quality_sample(31);
        current.output_tokens = 128;
        current.ttft_ms = Some(10_000);
        current.model_phase_output_rate = Some(1.0);

        let first = assess_quality_checkpoint(
            &history,
            &[],
            &current,
            &QualityGateState::default(),
            10_000,
        );
        assert_eq!(first.assessment.state, "consistent");
        assert_eq!(first.gate.consecutive_hits, 1);
        assert!(!first.gate.suspected);

        current.output_tokens = 192;
        let second = assess_quality_checkpoint(&history, &[], &current, &first.gate, 12_000);
        assert_eq!(second.assessment.state, "suspectedDegradation");
        assert!(second.gate.suspected);
        assert!(second.assessment.factors.len() >= 2);
        assert!(second.assessment.comparator.is_none());

        let mut healthy = quality_sample(32);
        healthy.output_tokens = 220;
        let recovery_one = assess_quality_checkpoint(&history, &[], &healthy, &second.gate, 14_000);
        assert!(recovery_one.gate.suspected);
        let recovery_two =
            assess_quality_checkpoint(&history, &[], &healthy, &recovery_one.gate, 16_000);
        assert!(!recovery_two.gate.suspected);
        assert_eq!(recovery_two.assessment.state, "consistent");
    }

    #[test]
    fn cache_change_alone_never_votes_for_degradation() {
        let history = (0..30).map(quality_sample).collect::<Vec<_>>();
        let mut current = quality_sample(40);
        current.output_tokens = 200;
        current.cache_input_share = Some(0.0);
        let result = assess_quality_checkpoint(
            &history,
            &[],
            &current,
            &QualityGateState::default(),
            10_000,
        );
        assert_eq!(result.assessment.state, "consistent");
        assert!(result.assessment.factors.is_empty());
    }

    #[test]
    fn dirty_or_explicitly_rerouted_checkpoints_never_vote_for_degradation() {
        let history = (0..30).map(quality_sample).collect::<Vec<_>>();
        let mut current = quality_sample(41);
        current.output_tokens = 192;
        current.ttft_ms = Some(10_000);
        current.model_phase_output_rate = Some(1.0);
        let previous = QualityGateState {
            consecutive_hits: 1,
            last_hit_checked_at_ms: Some(10_000),
            last_hit_output_tokens: Some(128),
            ..QualityGateState::default()
        };

        current.clean = false;
        let dirty = assess_quality_checkpoint(&history, &[], &current, &previous, 12_000);
        assert_eq!(dirty.assessment.state, "learning");
        assert!(!dirty.gate.suspected);
        assert_eq!(dirty.gate.consecutive_hits, 0);

        current.clean = true;
        current.explicit_reroute = true;
        let rerouted = assess_quality_checkpoint(&history, &[], &current, &previous, 12_000);
        assert_eq!(rerouted.assessment.state, "learning");
        assert!(!rerouted.gate.suspected);
        assert_eq!(rerouted.gate.consecutive_hits, 0);
    }

    fn behavior_sample(index: u64) -> CompletedTurnSample {
        CompletedTurnSample {
            thread_id: format!("thread-{index}"),
            turn_id: format!("turn-{index}"),
            kind: crate::model::ThreadKind::Root,
            model: Some("gpt-5.6-sol".to_owned()),
            effort: Some("ultra".to_owned()),
            input_bucket: "32k-128k".to_owned(),
            tool_activity: false,
            ttft_ms: Some(900 + (index % 7) * 10),
            duration_ms: Some(1_900 + (index % 7) * 20),
            input_tokens: 40_000,
            output_tokens: 90 + index % 7,
            reasoning_output_tokens: 18 + index % 5,
            cache_input_share: Some(0.70 + (index % 7) as f64 * 0.005),
            completed_at: "2026-08-24T08:00:00.000Z".to_owned(),
        }
    }

    #[test]
    fn behavior_check_requires_thirty_matching_samples_and_two_deviations() {
        let history: Vec<_> = (0..30).map(behavior_sample).collect();
        let mut current = behavior_sample(31);
        current.ttft_ms = Some(10_000);
        current.duration_ms = Some(10_000);
        current.output_tokens = 100;
        current.reasoning_output_tokens = 9;

        let assessment = assess_behavior(&history, &current);
        assert!(assessment.eligible);
        assert!(assessment.yellow_anomaly);
        assert!(assessment.deviations.len() >= 2);

        let insufficient = assess_behavior(&history[..29], &current);
        assert!(!insufficient.eligible);
        assert!(!insufficient.yellow_anomaly);
    }
}
