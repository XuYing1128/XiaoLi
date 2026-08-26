//! Conservative comparison between a successful active relay audit and
//! content-free metrics from real Codex turns bound to the same relay profile.
//!
//! This is deliberately a separate warning signal. It never changes the four
//! audit axes, names a physical model, or treats a synthetic audit as proof
//! that normal traffic received the same service.

use crate::{
    connection::{classify_endpoint, EndpointClass},
    history::ConversationHistoryRecord,
    relay_audit::RelayProfile,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const SELECTIVE_SERVICE_MIN_SESSIONS: usize = 10;
pub const SELECTIVE_SERVICE_MIN_SUSPICIOUS: usize = 5;
pub const SELECTIVE_SERVICE_WINDOW_DAYS: u32 = 30;

type TurnKey = (String, String);

/// Matches private, turn-bound endpoint-scope evidence to exactly one saved
/// relay profile. The digest covers scheme, host, effective port, and normalized
/// base path. It is never returned or persisted; only the local profile id is
/// safe to persist with content-free history metrics.
pub(crate) fn match_relay_profile_bindings(
    profiles: &[RelayProfile],
    endpoint_evidence: &HashMap<TurnKey, (EndpointClass, String)>,
) -> HashMap<TurnKey, String> {
    let mut candidates = HashMap::<(&'static str, String), Vec<String>>::new();
    for profile in profiles {
        let endpoint_class = classify_endpoint(&profile.normalized_base_url);
        let Some(endpoint_class_key) = bindable_endpoint_key(endpoint_class) else {
            continue;
        };
        let Some(endpoint_scope_hash) =
            crate::connection::endpoint_scope_hash(&profile.normalized_base_url)
        else {
            continue;
        };
        candidates
            .entry((endpoint_class_key, endpoint_scope_hash))
            .or_default()
            .push(profile.id.clone());
    }

    endpoint_evidence
        .iter()
        .filter_map(|(turn, (endpoint_class, host_hash))| {
            if host_hash.len() != 16 || !host_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            let key = (
                bindable_endpoint_key(*endpoint_class)?,
                host_hash.to_ascii_lowercase(),
            );
            let matching = candidates.get(&key)?;
            (matching.len() == 1).then(|| (turn.clone(), matching[0].clone()))
        })
        .collect()
}

fn bindable_endpoint_key(value: EndpointClass) -> Option<&'static str> {
    match value {
        EndpointClass::ManagedProvider => Some("managed"),
        EndpointClass::CustomEndpoint => Some("custom"),
        EndpointClass::LocalEndpoint => Some("local"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectiveServiceState {
    NotApplicable,
    InsufficientEvidence,
    NoMismatchObserved,
    SuspectedSelectiveService,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectiveServiceAssessment {
    pub state: SelectiveServiceState,
    pub sample_count: usize,
    pub suspicious_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspicious_share: Option<f64>,
    pub window_days: u32,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

impl SelectiveServiceAssessment {
    pub fn not_applicable() -> Self {
        Self {
            state: SelectiveServiceState::NotApplicable,
            sample_count: 0,
            suspicious_count: 0,
            suspicious_share: None,
            window_days: SELECTIVE_SERVICE_WINDOW_DAYS,
            reasons: vec![
                "active audit did not produce a fully consistent matched-reference result"
                    .to_owned(),
            ],
            limitations: standard_limitations(),
        }
    }
}

/// Compares only already-bound, completed, content-free history rows. The
/// caller is responsible for selecting the same relay profile and the fixed
/// lookback window; this function intentionally has no URL or credential input.
pub fn assess_selective_service(
    active_audit_consistent: bool,
    records: &[ConversationHistoryRecord],
) -> SelectiveServiceAssessment {
    if !active_audit_consistent {
        return SelectiveServiceAssessment::not_applicable();
    }

    let completed = records
        .iter()
        .filter(|record| !record.active && record.ended_at.is_some())
        .collect::<Vec<_>>();
    let sample_count = completed.len();
    let suspicious_count = completed
        .iter()
        .filter(|record| record.status_code == "suspected_degradation")
        .count();
    let suspicious_share =
        (sample_count > 0).then_some(suspicious_count as f64 / sample_count as f64);

    if sample_count < SELECTIVE_SERVICE_MIN_SESSIONS {
        return SelectiveServiceAssessment {
            state: SelectiveServiceState::InsufficientEvidence,
            sample_count,
            suspicious_count,
            suspicious_share,
            window_days: SELECTIVE_SERVICE_WINDOW_DAYS,
            reasons: vec![format!(
                "only {sample_count} bound completed Codex turns are available; at least {SELECTIVE_SERVICE_MIN_SESSIONS} are required"
            )],
            limitations: standard_limitations(),
        };
    }

    let suspected = suspicious_count >= SELECTIVE_SERVICE_MIN_SUSPICIOUS
        && suspicious_count.saturating_mul(2) >= sample_count;
    SelectiveServiceAssessment {
        state: if suspected {
            SelectiveServiceState::SuspectedSelectiveService
        } else {
            SelectiveServiceState::NoMismatchObserved
        },
        sample_count,
        suspicious_count,
        suspicious_share,
        window_days: SELECTIVE_SERVICE_WINDOW_DAYS,
        reasons: vec![if suspected {
            format!(
                "the active audit was consistent while {suspicious_count} of {sample_count} bound real Codex turns retained a conservative degradation warning"
            )
        } else {
            format!(
                "the active audit and {sample_count} bound real Codex turns did not meet the selective-service warning threshold"
            )
        }],
        limitations: standard_limitations(),
    }
}

fn standard_limitations() -> Vec<String> {
    vec![
        "binding requires matching turn-bound endpoint-scope evidence and never exposes the private digest"
            .to_owned(),
        "real-session behavior warnings are statistical and can be affected by workload, tools, cache, load, or model updates"
            .to_owned(),
        "this signal cannot prove selective routing or identify a physical model".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        connection::ConnectionOriginSnapshot,
        history::HistoryUsage,
        model::{StatusLevel, ThreadKind, TimingSnapshot},
        relay_audit::RelayProtocol,
    };

    fn profile(id: &str, endpoint: &str) -> RelayProfile {
        RelayProfile {
            id: id.to_owned(),
            label: "local test profile".to_owned(),
            normalized_base_url: endpoint.to_owned(),
            protocol: RelayProtocol::OpenAiResponses,
            default_model: "gpt-test".to_owned(),
            credential_ref: None,
            private_probe_pack: None,
            created_at: "2026-08-27T00:00:00Z".to_owned(),
            updated_at: "2026-08-27T00:00:00Z".to_owned(),
        }
    }

    fn record(index: usize, suspicious: bool) -> ConversationHistoryRecord {
        ConversationHistoryRecord {
            thread_id: format!("thread-{index}"),
            turn_id: format!("turn-{index}"),
            parent_thread_id: None,
            kind: ThreadKind::Root,
            display_label: format!("session-{index}"),
            local_alias: None,
            relay_profile_id: Some("relay-profile".to_owned()),
            requested_model: Some("gpt-5.6-sol".to_owned()),
            requested_effort: Some("ultra".to_owned()),
            origin_kind: "customEndpoint".to_owned(),
            connection_origin: ConnectionOriginSnapshot::unknown(),
            route_evidence: "notObserved".to_owned(),
            routed_model: None,
            usage: HistoryUsage::default(),
            timing: TimingSnapshot::default(),
            status_level: if suspicious {
                StatusLevel::Yellow
            } else {
                StatusLevel::Green
            },
            status_code: if suspicious {
                "suspected_degradation".to_owned()
            } else {
                "ok".to_owned()
            },
            started_at: "2026-08-27T00:00:00Z".to_owned(),
            updated_at: "2026-08-27T00:01:00Z".to_owned(),
            ended_at: Some("2026-08-27T00:01:00Z".to_owned()),
            active: false,
        }
    }

    #[test]
    fn needs_ten_bound_completed_turns() {
        let records = (0..9).map(|index| record(index, true)).collect::<Vec<_>>();
        let assessment = assess_selective_service(true, &records);
        assert_eq!(
            assessment.state,
            SelectiveServiceState::InsufficientEvidence
        );
        assert_eq!(assessment.sample_count, 9);
    }

    #[test]
    fn requires_five_and_at_least_half_suspicious() {
        let records = (0..10)
            .map(|index| record(index, index < 5))
            .collect::<Vec<_>>();
        let assessment = assess_selective_service(true, &records);
        assert_eq!(
            assessment.state,
            SelectiveServiceState::SuspectedSelectiveService
        );
        assert_eq!(assessment.suspicious_count, 5);

        let healthy = (0..10)
            .map(|index| record(index, index < 4))
            .collect::<Vec<_>>();
        assert_eq!(
            assess_selective_service(true, &healthy).state,
            SelectiveServiceState::NoMismatchObserved
        );
    }

    #[test]
    fn non_consistent_audit_never_creates_a_selective_service_claim() {
        let records = (0..20).map(|index| record(index, true)).collect::<Vec<_>>();
        assert_eq!(
            assess_selective_service(false, &records).state,
            SelectiveServiceState::NotApplicable
        );
    }

    #[test]
    fn binds_only_one_exact_non_official_endpoint() {
        let turn = ("thread-a".to_owned(), "turn-a".to_owned());
        let hash = crate::connection::endpoint_scope_hash("https://relay.example/v1").unwrap();
        let evidence =
            HashMap::from([(turn.clone(), (EndpointClass::CustomEndpoint, hash.clone()))]);
        let profiles = vec![profile("relay-a", "https://relay.example/v1")];
        assert_eq!(
            match_relay_profile_bindings(&profiles, &evidence).get(&turn),
            Some(&"relay-a".to_owned())
        );

        let official = vec![profile("official", "https://api.openai.com/v1")];
        let official_evidence = HashMap::from([(
            turn.clone(),
            (
                EndpointClass::OfficialOpenAi,
                crate::connection::endpoint_scope_hash("https://api.openai.com/v1").unwrap(),
            ),
        )]);
        assert!(match_relay_profile_bindings(&official, &official_evidence).is_empty());

        let wrong_class = HashMap::from([(turn, (EndpointClass::LocalEndpoint, hash))]);
        assert!(match_relay_profile_bindings(&profiles, &wrong_class).is_empty());
    }

    #[test]
    fn same_endpoint_scope_with_multiple_profiles_is_ambiguous_and_never_binds() {
        let turn = (
            "thread-shared-host".to_owned(),
            "turn-shared-host".to_owned(),
        );
        let evidence = HashMap::from([(
            turn.clone(),
            (
                EndpointClass::CustomEndpoint,
                crate::connection::endpoint_scope_hash("https://relay.example/v1").unwrap(),
            ),
        )]);
        let profiles = vec![
            profile("relay-a", "https://relay.example/v1"),
            profile("relay-b", "https://relay.example:443/v1/"),
        ];

        let bindings = match_relay_profile_bindings(&profiles, &evidence);
        assert!(!bindings.contains_key(&turn));
        assert!(bindings.is_empty());
    }

    #[test]
    fn same_host_with_a_different_port_or_base_path_never_binds() {
        let turn = ("thread-scope".to_owned(), "turn-scope".to_owned());
        let profiles = vec![profile("relay-a", "https://relay.example:8443/v1")];
        for observed in [
            "https://relay.example/v1",
            "https://relay.example:8443/compatible/v1",
            "http://relay.example:8443/v1",
        ] {
            let evidence = HashMap::from([(
                turn.clone(),
                (
                    EndpointClass::CustomEndpoint,
                    crate::connection::endpoint_scope_hash(observed).unwrap(),
                ),
            )]);
            assert!(match_relay_profile_bindings(&profiles, &evidence).is_empty());
        }
    }
}
