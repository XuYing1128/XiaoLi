use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::connection::{ConnectionOriginSnapshot, EndpointClass};

pub const SNAPSHOT_SCHEMA_VERSION: u32 = 5;
// Increment when parser semantics change in a way that can leave a previously
// rejected physical rollout parked at EOF. Version 7 re-evaluates paginated
// history bases that may legitimately point at an ancestor thread.
// Version 8 replays rollout bytes so item_completed timing metadata that v7
// intentionally ignored can be recovered without trusting a stale EOF cursor.
// Version 9 replaces serialized runtime FileState values with an
// absolute-path-free, content-free state whose cursor stops at the last
// complete line.
// Version 10 replays rollout headers so session_meta.model_provider can be
// recovered even when a previous parser left the file parked at EOF.
pub const COLLECTOR_CACHE_FORMAT_VERSION: u32 = 10;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub cache_write_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn is_empty(&self) -> bool {
        self.input_tokens == 0
            && self.cached_input_tokens == 0
            && self.cache_write_input_tokens == 0
            && self.output_tokens == 0
            && self.reasoning_output_tokens == 0
            && self.total_tokens == 0
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub source: String,
}

impl RequestSnapshot {
    pub fn new(model: Option<String>, effort: Option<String>, source: impl Into<String>) -> Self {
        Self {
            model: clean_optional(model),
            effort: clean_optional(effort),
            source: source.into(),
        }
    }

    pub fn differs_from(&self, other: &Self) -> bool {
        normalize_optional(&self.model) != normalize_optional(&other.model)
            || normalize_optional(&self.effort) != normalize_optional(&other.effort)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteHop {
    pub from_model: String,
    pub to_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub timestamp: String,
    pub association: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRouteSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub chain: Vec<RouteHop>,
}

impl Default for ServerRouteSnapshot {
    fn default() -> Self {
        Self {
            model: None,
            evidence: "notObserved".to_owned(),
            observed_at: None,
            chain: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    pub last: TokenUsage,
    pub cumulative: TokenUsage,
    /// Internal turn-local delta used when writing per-turn history. It is
    /// deliberately excluded from the public snapshot and all persisted JSON.
    #[serde(skip)]
    pub turn: TokenUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cache_input_share: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_input_share: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_input_share: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub ttft_evidence: TtftEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_active_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_to_end_output_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_phase_output_rate: Option<f64>,
    /// V3 compatibility alias. This is the end-to-end observed rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_output_rate: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TtftEvidence {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lower_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upper_ms: Option<u64>,
}

impl Default for TtftEvidence {
    fn default() -> Self {
        Self {
            kind: "pending".to_owned(),
            lower_ms: None,
            upper_ms: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityFactor {
    pub code: String,
    pub direction: String,
    pub observed: f64,
    pub baseline_median: f64,
    pub mad: f64,
    pub robust_deviation: f64,
    pub unit: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityComparator {
    pub requested_model: String,
    pub compared_model: String,
    pub sample_count: usize,
    pub relative_distance: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityAssessment {
    pub state: String,
    pub baseline_key: String,
    pub baseline_sample_count: usize,
    pub consecutive_hits: u32,
    #[serde(default)]
    pub factors: Vec<QualityFactor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparator: Option<QualityComparator>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

impl Default for QualityAssessment {
    fn default() -> Self {
        Self {
            state: "learning".to_owned(),
            baseline_key: String::new(),
            baseline_sample_count: 0,
            consecutive_hits: 0,
            factors: Vec::new(),
            comparator: None,
            limitations: vec![
                "Behavioral telemetry cannot verify the physical server model or effort."
                    .to_owned(),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusLevel {
    Green,
    Yellow,
    Red,
    Gray,
}

impl Default for StatusLevel {
    fn default() -> Self {
        Self::Gray
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusSnapshot {
    pub level: StatusLevel,
    pub code: String,
    pub explanation: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadKind {
    Root,
    Subagent,
}

impl Default for ThreadKind {
    fn default() -> Self {
        Self::Root
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSnapshot {
    pub thread_id: String,
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    pub kind: ThreadKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timestamp: Option<String>,
    pub active_request: RequestSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_next_turn: Option<RequestSnapshot>,
    pub server_route: ServerRouteSnapshot,
    pub usage: UsageSnapshot,
    pub timing: TimingSnapshot,
    #[serde(default)]
    pub quality_assessment: QualityAssessment,
    /// Configured connection-origin evidence. This is deliberately separate
    /// from request settings, explicit server reroutes and behavior telemetry.
    #[serde(default)]
    pub connection_origin: ConnectionOriginSnapshot,
    #[serde(default)]
    pub tool_activity: bool,
    pub status: StatusSnapshot,
    #[serde(default)]
    pub anomalies: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorHealth {
    pub level: StatusLevel,
    #[serde(default)]
    pub parse_warnings: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorSnapshot {
    pub schema_version: u32,
    pub checked_at: String,
    pub codex_running: bool,
    pub collector_health: CollectorHealth,
    #[serde(default)]
    pub conversations: Vec<ConversationSnapshot>,
}

impl MonitorSnapshot {
    /// Returns the metadata-only snapshot allowed to cross the persistence
    /// boundary. Prompt-derived session titles remain available to the live UI
    /// but are replaced with a time and short-thread label on disk.
    pub fn redacted_for_persistence(&self) -> Self {
        let mut redacted = self.clone();
        for conversation in &mut redacted.conversations {
            conversation.title =
                persistence_display_label(&self.checked_at, &conversation.thread_id);
        }
        redacted
    }
}

pub fn persistence_display_label(checked_at: &str, thread_id: &str) -> String {
    let time = checked_at.chars().take(16).collect::<String>();
    let short_thread = thread_id.chars().take(8).collect::<String>();
    format!("{time} · {short_thread}")
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnLifecycle {
    Active,
    Completed,
    Aborted,
}

impl Default for TurnLifecycle {
    fn default() -> Self {
        Self::Active
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnState {
    pub turn_id: String,
    pub lifecycle: TurnLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timestamp: Option<String>,
    /// Timestamp of the latest request boundary (`turn_context`) inside this
    /// Codex turn. A turn can contain multiple request continuations, so a
    /// thread/turn-bound live reroute observed before this boundary must not be
    /// reused as evidence for the newer request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_observed_at: Option<String>,
    /// File warning counter captured at the current request boundary. Quality
    /// eligibility compares against this baseline so an unrelated historical
    /// bad line cannot poison every later clean request.
    #[serde(default)]
    pub parse_warnings_at_start: u64,
    pub active_request: RequestSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_next_turn: Option<RequestSnapshot>,
    pub server_route: ServerRouteSnapshot,
    pub usage_baseline: TokenUsage,
    pub usage_last: TokenUsage,
    /// Exact raw `total_token_usage` reported for the thread/session.
    pub usage_cumulative: TokenUsage,
    /// Internal turn-local delta used for completed samples and turn timing.
    /// It is not exposed as the conversation's cumulative token total.
    #[serde(default)]
    pub usage_turn: TokenUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default)]
    pub tool_activity: bool,
    /// Sanitized model-phase intervals from item_completed. No item body is
    /// retained in collector state or cache.
    #[serde(default)]
    pub model_intervals: Vec<ModelItemInterval>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelItemInterval {
    pub item_id: String,
    pub item_type: String,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
}

impl ThreadSettings {
    pub fn as_request(&self, source: impl Into<String>) -> RequestSnapshot {
        RequestSnapshot::new(self.model.clone(), self.effort.clone(), source)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileState {
    pub path: PathBuf,
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub observed_length: u64,
    #[serde(default)]
    pub last_write_ms: u64,
    #[serde(default)]
    pub anchor_offset: u64,
    #[serde(default)]
    pub anchor_length: usize,
    #[serde(default)]
    pub anchor_hash: u64,
    /// Last byte immediately after a complete JSONL newline. Unlike `offset`,
    /// this never advances across an in-memory partial line and is therefore
    /// safe to use as the restart cursor.
    #[serde(default)]
    pub durable_offset: u64,
    #[serde(default)]
    pub durable_anchor_offset: u64,
    #[serde(default)]
    pub durable_anchor_length: usize,
    #[serde(default)]
    pub durable_anchor_hash: u64,
    #[serde(default)]
    pub carry_bytes: Vec<u8>,
    #[serde(default)]
    pub discard_oversize: bool,
    #[serde(default)]
    pub identity_known: bool,
    #[serde(default)]
    pub identity_rejected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub kind: ThreadKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(default = "default_negative_ordinal")]
    pub segment_start_ordinal: i64,
    #[serde(default)]
    pub own_start_ordinal: i64,
    #[serde(default)]
    pub thread_settings: ThreadSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn: Option<TurnState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_turn: Option<CompletedTurnSample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_behavior_sample_v2: Option<BehaviorSampleV2>,
    /// Sanitized recent completed samples discovered while replaying this
    /// rollout. Capped so a cold cache rebuild can repopulate V2 baselines
    /// without retaining message bodies or unbounded session history.
    #[serde(default)]
    pub completed_behavior_samples_v2: Vec<BehaviorSampleV2>,
    #[serde(default)]
    pub last_total_usage: TokenUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_timestamp: Option<String>,
    #[serde(default)]
    pub parse_warnings: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl FileState {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            observed_length: 0,
            last_write_ms: 0,
            anchor_offset: 0,
            anchor_length: 0,
            anchor_hash: 0,
            durable_offset: 0,
            durable_anchor_offset: 0,
            durable_anchor_length: 0,
            durable_anchor_hash: 0,
            carry_bytes: Vec::new(),
            discard_oversize: false,
            identity_known: false,
            identity_rejected: false,
            thread_id: None,
            model_provider: None,
            parent_thread_id: None,
            kind: ThreadKind::Root,
            agent_path: None,
            agent_nickname: None,
            segment_start_ordinal: -1,
            own_start_ordinal: 0,
            thread_settings: ThreadSettings::default(),
            current_turn: None,
            last_completed_turn: None,
            last_completed_behavior_sample_v2: None,
            completed_behavior_samples_v2: Vec::new(),
            last_total_usage: TokenUsage::default(),
            last_event_timestamp: None,
            parse_warnings: 0,
            last_error: None,
        }
    }

    pub fn reset_preserving_path(&mut self) {
        let path = self.path.clone();
        *self = Self::new(path);
    }

    pub fn register_warning(&mut self, code: impl Into<String>) {
        self.parse_warnings = self.parse_warnings.saturating_add(1);
        self.last_error = Some(code.into());
    }
}

/// Cursor persisted across process restarts. It deliberately contains only
/// byte positions, timestamps and one-way content fingerprints. Runtime
/// absolute paths and pending JSONL bytes never cross this boundary.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedFileCursor {
    pub offset: u64,
    pub observed_length: u64,
    pub last_write_ms: u64,
    pub anchor_offset: u64,
    pub anchor_length: usize,
    pub anchor_hash: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedThreadSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
}

/// Sanitized logical state saved in SQLite. The type intentionally has no
/// absolute path, cwd, agent_path, raw JSON or partial-line field. Its only
/// location is a traversal-checked path relative to SessionsRoot.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedFileState {
    /// Path relative to SessionsRoot, normalized to forward-slash components.
    /// Absolute paths and traversal components are rejected by the collector.
    pub relative_path: String,
    pub cursor: PersistedFileCursor,
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub kind: ThreadKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(default = "default_negative_ordinal")]
    pub segment_start_ordinal: i64,
    #[serde(default)]
    pub own_start_ordinal: i64,
    #[serde(default)]
    pub thread_settings: PersistedThreadSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_turn: Option<TurnState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_turn: Option<CompletedTurnSample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_behavior_sample_v2: Option<BehaviorSampleV2>,
    #[serde(default)]
    pub completed_behavior_samples_v2: Vec<BehaviorSampleV2>,
    #[serde(default)]
    pub last_total_usage: TokenUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_timestamp: Option<String>,
    #[serde(default)]
    pub parse_warnings: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl PersistedFileState {
    pub fn from_runtime(state: &FileState, relative_path: String) -> Option<Self> {
        let thread_id = state.thread_id.clone()?;
        if !state.identity_known
            || state.identity_rejected
            || state.durable_offset == 0
            || state.durable_anchor_length == 0
        {
            return None;
        }
        let mut current_turn = state.current_turn.clone();
        if let Some(turn) = current_turn.as_mut() {
            // Reroute reasons are useful live diagnostics but are not required
            // to reconstruct route identity. Do not persist free-form server
            // text in the derived cursor cache.
            for hop in &mut turn.server_route.chain {
                hop.reason = None;
            }
        }
        Some(Self {
            relative_path,
            cursor: PersistedFileCursor {
                offset: state.durable_offset,
                observed_length: state.observed_length,
                last_write_ms: state.last_write_ms,
                anchor_offset: state.durable_anchor_offset,
                anchor_length: state.durable_anchor_length,
                anchor_hash: state.durable_anchor_hash,
            },
            thread_id,
            model_provider: state.model_provider.clone(),
            parent_thread_id: state.parent_thread_id.clone(),
            kind: state.kind,
            agent_nickname: state.agent_nickname.clone(),
            segment_start_ordinal: state.segment_start_ordinal,
            own_start_ordinal: state.own_start_ordinal,
            thread_settings: PersistedThreadSettings {
                model: state.thread_settings.model.clone(),
                effort: state.thread_settings.effort.clone(),
                observed_at: state.thread_settings.observed_at.clone(),
            },
            current_turn,
            last_completed_turn: state.last_completed_turn.clone(),
            last_completed_behavior_sample_v2: state.last_completed_behavior_sample_v2.clone(),
            completed_behavior_samples_v2: state.completed_behavior_samples_v2.clone(),
            last_total_usage: state.last_total_usage.clone(),
            last_event_timestamp: state.last_event_timestamp.clone(),
            parse_warnings: state.parse_warnings,
            last_error: state.last_error.clone(),
        })
    }

    pub fn into_runtime(self, path: PathBuf) -> FileState {
        FileState {
            path,
            offset: self.cursor.offset,
            observed_length: self.cursor.observed_length,
            last_write_ms: self.cursor.last_write_ms,
            anchor_offset: self.cursor.anchor_offset,
            anchor_length: self.cursor.anchor_length,
            anchor_hash: self.cursor.anchor_hash,
            durable_offset: self.cursor.offset,
            durable_anchor_offset: self.cursor.anchor_offset,
            durable_anchor_length: self.cursor.anchor_length,
            durable_anchor_hash: self.cursor.anchor_hash,
            carry_bytes: Vec::new(),
            discard_oversize: false,
            identity_known: true,
            identity_rejected: false,
            thread_id: Some(self.thread_id),
            model_provider: self.model_provider,
            parent_thread_id: self.parent_thread_id,
            kind: self.kind,
            agent_path: None,
            agent_nickname: self.agent_nickname,
            segment_start_ordinal: self.segment_start_ordinal,
            own_start_ordinal: self.own_start_ordinal,
            thread_settings: ThreadSettings {
                model: self.thread_settings.model,
                effort: self.thread_settings.effort,
                cwd: None,
                observed_at: self.thread_settings.observed_at,
            },
            current_turn: self.current_turn,
            last_completed_turn: self.last_completed_turn,
            last_completed_behavior_sample_v2: self.last_completed_behavior_sample_v2,
            completed_behavior_samples_v2: self.completed_behavior_samples_v2,
            last_total_usage: self.last_total_usage,
            last_event_timestamp: self.last_event_timestamp,
            parse_warnings: self.parse_warnings,
            last_error: self.last_error,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookObservation {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_class: Option<EndpointClass>,
    /// Short one-way endpoint-scope digest covering scheme, host, effective
    /// port, and normalized base path. It is never copied into public snapshots
    /// or conversation history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_host_hash: Option<String>,
    pub observed_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletedTurnSample {
    pub thread_id: String,
    pub turn_id: String,
    pub kind: ThreadKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub input_bucket: String,
    /// The rollout token/timing stream does not prove tool activity. It stays
    /// false until a separate structured tool lifecycle collector supplies it.
    pub tool_activity: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_input_share: Option<f64>,
    pub completed_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorSampleV2 {
    pub thread_id: String,
    pub turn_id: String,
    pub model: String,
    pub effort: String,
    pub uncached_input_bucket: String,
    pub output_bucket: String,
    pub tool_activity: bool,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_phase_output_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_share: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_phase_share: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_input_share: Option<f64>,
    pub clean: bool,
    pub explicit_reroute: bool,
    pub observed_at: String,
}

impl BehaviorSampleV2 {
    pub fn baseline_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.model.trim().to_ascii_lowercase(),
            self.effort.trim().to_ascii_lowercase(),
            self.uncached_input_bucket,
            self.output_bucket,
            if self.tool_activity {
                "tools"
            } else {
                "no-tools"
            }
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReroutedObservation {
    pub thread_id: String,
    pub turn_id: String,
    pub from_model: String,
    pub to_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub observed_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorCache {
    pub schema_version: u32,
    pub cache_format_version: u32,
    #[serde(default)]
    pub files: Vec<PersistedFileState>,
}

fn default_negative_ordinal() -> i64 {
    -1
}

pub fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

pub fn normalize_optional(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|text| text.trim().to_ascii_lowercase())
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_snapshot_is_camel_case_json() {
        let snapshot = MonitorSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            checked_at: "2026-08-24T08:00:00.000Z".to_owned(),
            codex_running: false,
            collector_health: CollectorHealth::default(),
            conversations: Vec::new(),
        };
        let value = serde_json::to_value(snapshot).expect("serialize snapshot");
        assert_eq!(value["schemaVersion"], 5);
        assert_eq!(value["codexRunning"], false);
        assert!(value.get("schema_version").is_none());
    }
}
