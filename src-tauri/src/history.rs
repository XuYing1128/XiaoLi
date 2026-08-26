use crate::{
    connection::ConnectionOriginSnapshot,
    metrics::cache_input_share,
    model::{ConversationSnapshot, StatusLevel, ThreadKind, TimingSnapshot, TokenUsage},
};
use serde::{Deserialize, Serialize};

/// A content-free historical row. Task titles, prompts, responses and cwd are
/// deliberately excluded so the workbench can show usage history without
/// becoming a second transcript store.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationHistoryRecord {
    pub thread_id: String,
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    pub kind: ThreadKind,
    pub display_label: String,
    /// Optional user-authored local label. It is stored separately from the
    /// rollout-derived history JSON and overlaid only when history is queried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_alias: Option<String>,
    /// Internal, non-secret binding to a saved relay profile. The profile is
    /// attached only when turn-bound endpoint-scope evidence matches exactly;
    /// no endpoint URL or private scope digest is persisted in this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_effort: Option<String>,
    pub origin_kind: String,
    #[serde(default)]
    pub connection_origin: ConnectionOriginSnapshot,
    pub route_evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_model: Option<String>,
    pub usage: HistoryUsage,
    pub timing: TimingSnapshot,
    pub status_level: StatusLevel,
    pub status_code: String,
    pub started_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub active: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_input_share: Option<f64>,
}

impl HistoryUsage {
    fn from_token_usage(value: &TokenUsage, cache_input_share: Option<f64>) -> Self {
        Self {
            input_tokens: value.input_tokens,
            cached_input_tokens: value.cached_input_tokens,
            cache_write_input_tokens: value.cache_write_input_tokens,
            output_tokens: value.output_tokens,
            reasoning_output_tokens: value.reasoning_output_tokens,
            total_tokens: value.total_tokens,
            cache_input_share,
        }
    }
}

impl ConversationHistoryRecord {
    pub fn from_live(value: &ConversationSnapshot, checked_at: &str) -> Self {
        let started_at = value
            .source_timestamp
            .clone()
            .unwrap_or_else(|| checked_at.to_owned());
        Self {
            thread_id: value.thread_id.clone(),
            turn_id: value.turn_id.clone(),
            parent_thread_id: value.parent_thread_id.clone(),
            kind: value.kind,
            display_label: format!(
                "{} · {}",
                &checked_at.get(..16).unwrap_or(checked_at),
                short_id(&value.thread_id)
            ),
            local_alias: None,
            relay_profile_id: None,
            requested_model: value.active_request.model.clone(),
            requested_effort: value.active_request.effort.clone(),
            origin_kind: value.connection_origin.kind.as_wire().to_owned(),
            connection_origin: value.connection_origin.clone(),
            route_evidence: value.server_route.evidence.clone(),
            routed_model: value.server_route.model.clone(),
            usage: HistoryUsage::from_token_usage(
                &value.usage.turn,
                cache_input_share(&value.usage.turn),
            ),
            timing: value.timing.clone(),
            status_level: value.status.level,
            status_code: value.status.code.clone(),
            started_at,
            updated_at: checked_at.to_owned(),
            ended_at: None,
            active: true,
        }
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.thread_id, self.turn_id)
    }

    pub fn apply_local_alias(&mut self, alias: Option<String>) {
        self.local_alias = alias.filter(|value| !value.is_empty());
        if let Some(alias) = self.local_alias.as_ref() {
            self.display_label = alias.clone();
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationHistoryFilter {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub origin_kind: String,
    #[serde(default)]
    pub status_level: String,
    #[serde(default)]
    pub date_from: String,
    #[serde(default)]
    pub date_to: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

impl ConversationHistoryFilter {
    pub fn bounded_limit(&self) -> usize {
        self.limit.clamp(1, 200)
    }
}

fn default_limit() -> usize {
    50
}

fn short_id(value: &str) -> String {
    value.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        QualityAssessment, RequestSnapshot, ServerRouteSnapshot, StatusSnapshot, UsageSnapshot,
    };

    #[test]
    fn history_record_does_not_persist_live_title_or_content() {
        const PRIVATE_TITLE: &str = "PRIVATE_TITLE_MUST_NOT_PERSIST";
        const PRIVATE_PROMPT: &str = "PRIVATE_PROMPT_MUST_NOT_PERSIST";
        const PRIVATE_RESPONSE: &str = "PRIVATE_RAW_RESPONSE_MUST_NOT_PERSIST";
        const PRIVATE_CWD: &str = "C:\\PRIVATE_CWD_MUST_NOT_PERSIST\\repo";
        const PRIVATE_CREDENTIAL: &str = "sk-PRIVATE_CREDENTIAL_MUST_NOT_PERSIST";
        let conversation = ConversationSnapshot {
            thread_id: "thread-private".to_owned(),
            turn_id: "turn-1".to_owned(),
            parent_thread_id: None,
            kind: ThreadKind::Root,
            // The title is the only live free-form display field accepted by
            // this conversion. Packing every sensitive marker into it proves
            // the history DTO does not accidentally become a transcript.
            title: format!(
                "{PRIVATE_TITLE} {PRIVATE_PROMPT} {PRIVATE_RESPONSE} {PRIVATE_CWD} {PRIVATE_CREDENTIAL}"
            ),
            source_timestamp: Some("2026-08-27T01:00:00Z".to_owned()),
            active_request: RequestSnapshot::new(
                Some("gpt-5.6-sol".to_owned()),
                Some("ultra".to_owned()),
                "turnContext",
            ),
            pending_next_turn: None,
            server_route: ServerRouteSnapshot::default(),
            usage: UsageSnapshot::default(),
            timing: TimingSnapshot::default(),
            quality_assessment: QualityAssessment::default(),
            connection_origin: ConnectionOriginSnapshot::unknown(),
            tool_activity: false,
            status: StatusSnapshot {
                level: StatusLevel::Green,
                code: "ok".to_owned(),
                explanation: "ok".to_owned(),
            },
            anomalies: Vec::new(),
        };

        let history =
            ConversationHistoryRecord::from_live(&conversation, "2026-08-27T01:02:03.000Z");
        let json = serde_json::to_string(&history).unwrap();
        for forbidden in [
            PRIVATE_TITLE,
            PRIVATE_PROMPT,
            PRIVATE_RESPONSE,
            PRIVATE_CWD,
            PRIVATE_CREDENTIAL,
        ] {
            assert!(!json.contains(forbidden), "history leaked {forbidden}");
        }
        let value = serde_json::to_value(&history).unwrap();
        for forbidden_key in [
            "title",
            "prompt",
            "response",
            "rawResponse",
            "cwd",
            "credential",
            "apiKey",
        ] {
            assert!(
                value.get(forbidden_key).is_none(),
                "history contract exposed {forbidden_key}"
            );
        }
        assert_eq!(history.display_label, "2026-08-27T01:02 · thread-p");
    }

    #[test]
    fn history_uses_turn_local_usage_without_recounting_prior_turns() {
        let mut conversation = ConversationSnapshot {
            thread_id: "thread-shared".to_owned(),
            turn_id: "turn-1".to_owned(),
            parent_thread_id: None,
            kind: ThreadKind::Root,
            title: "live only".to_owned(),
            source_timestamp: Some("2026-08-27T01:00:00Z".to_owned()),
            active_request: RequestSnapshot::new(
                Some("gpt-5.6-sol".to_owned()),
                Some("ultra".to_owned()),
                "turnContext",
            ),
            pending_next_turn: None,
            server_route: ServerRouteSnapshot::default(),
            usage: UsageSnapshot {
                cumulative: TokenUsage {
                    input_tokens: 90_000,
                    cached_input_tokens: 80_000,
                    output_tokens: 10_000,
                    total_tokens: 100_000,
                    ..TokenUsage::default()
                },
                turn: TokenUsage {
                    input_tokens: 90_000,
                    cached_input_tokens: 80_000,
                    output_tokens: 10_000,
                    total_tokens: 100_000,
                    ..TokenUsage::default()
                },
                ..UsageSnapshot::default()
            },
            timing: TimingSnapshot::default(),
            quality_assessment: QualityAssessment::default(),
            connection_origin: ConnectionOriginSnapshot::unknown(),
            tool_activity: false,
            status: StatusSnapshot {
                level: StatusLevel::Green,
                code: "ok".to_owned(),
                explanation: "ok".to_owned(),
            },
            anomalies: Vec::new(),
        };
        let first = ConversationHistoryRecord::from_live(&conversation, "2026-08-27T01:02:00Z");

        conversation.turn_id = "turn-2".to_owned();
        conversation.usage.cumulative = TokenUsage {
            input_tokens: 160_000,
            cached_input_tokens: 145_000,
            output_tokens: 20_000,
            total_tokens: 180_000,
            ..TokenUsage::default()
        };
        conversation.usage.turn = TokenUsage {
            input_tokens: 70_000,
            cached_input_tokens: 65_000,
            output_tokens: 10_000,
            total_tokens: 80_000,
            ..TokenUsage::default()
        };
        let second = ConversationHistoryRecord::from_live(&conversation, "2026-08-27T01:04:00Z");

        assert_eq!(first.usage.total_tokens, 100_000);
        assert_eq!(second.usage.total_tokens, 80_000);
        assert_eq!(
            first.usage.total_tokens + second.usage.total_tokens,
            180_000,
            "cross-turn totals must equal the thread total without recounting turn 1"
        );
        assert_eq!(second.usage.cache_input_share, Some(65_000.0 / 70_000.0));
    }

    #[test]
    fn history_limit_is_bounded() {
        let mut filter = ConversationHistoryFilter {
            limit: 10_000,
            ..ConversationHistoryFilter::default()
        };
        assert_eq!(filter.bounded_limit(), 200);
        filter.limit = 0;
        assert_eq!(filter.bounded_limit(), 1);
    }
}
