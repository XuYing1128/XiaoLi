use crate::metrics::{
    cache_input_share, observed_output_rate, output_bucket, reasoning_active_ms, timing_snapshot,
    uncached_input_bucket, union_interval_ms, usage_since_baseline, usage_snapshot,
};
use crate::model::{
    clean_optional, normalize_optional, BehaviorSampleV2, CollectorCache, CollectorHealth,
    CompletedTurnSample, ConversationSnapshot, FileState, HookObservation, ModelItemInterval,
    ModelReroutedObservation, MonitorSnapshot, PersistedFileState, QualityAssessment,
    RequestSnapshot, RouteHop, ServerRouteSnapshot, StatusLevel, StatusSnapshot, ThreadKind,
    TokenUsage, TurnLifecycle, TurnState, COLLECTOR_CACHE_FORMAT_VERSION, SNAPSHOT_SCHEMA_VERSION,
};
use chrono::DateTime;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const READ_CHUNK_BYTES: usize = 64 * 1024;
const MAX_PENDING_LINE_BYTES: usize = 16 * 1024 * 1024;
const ANCHOR_BYTES: usize = 4 * 1024;
const MAX_LIVE_OBSERVATION_KEYS: usize = 2_048;
const MAX_ROUTE_HOPS_PER_TURN: usize = 16;
const MAX_BEHAVIOR_SAMPLES_PER_BASELINE: usize = 100;

pub struct RolloutCollector {
    sessions_root: PathBuf,
    session_index_path: Option<PathBuf>,
    files: HashMap<PathBuf, FileState>,
    restored_files: HashMap<PathBuf, PersistedFileState>,
    titles: HashMap<String, String>,
    title_index_signature: Option<(u64, u64)>,
    hook_observations: HashMap<(String, String), HookObservation>,
    live_reroutes: HashMap<(String, String), Vec<ModelReroutedObservation>>,
    collector_error: Option<String>,
    scan_parse_warnings: u64,
}

impl RolloutCollector {
    pub fn new(sessions_root: impl Into<PathBuf>, session_index_path: Option<PathBuf>) -> Self {
        Self {
            sessions_root: sessions_root.into(),
            session_index_path,
            files: HashMap::new(),
            restored_files: HashMap::new(),
            titles: HashMap::new(),
            title_index_signature: None,
            hook_observations: HashMap::new(),
            live_reroutes: HashMap::new(),
            collector_error: None,
            scan_parse_warnings: 0,
        }
    }

    /// Records the model exposed by the trusted UserPromptSubmit hook. Hook
    /// evidence is still a client/request value, never a server-route receipt.
    pub fn observe_hook(&mut self, observation: HookObservation) {
        // SessionStart legitimately has no turn/model. It remains useful to the
        // launcher, but is not request evidence and is ignored by this parser.
        let Some(turn_id) = clean_optional(observation.turn_id.clone()) else {
            return;
        };
        let Some(model) = clean_optional(observation.model.clone()) else {
            return;
        };
        let key = (observation.thread_id.clone(), turn_id.clone());
        let observation = HookObservation {
            turn_id: Some(turn_id),
            model: Some(model),
            ..observation
        };
        self.hook_observations.insert(key.clone(), observation);
        self.trim_observations(&key);
    }

    /// Accepts only a structured, explicitly thread/turn-bound reroute event.
    /// Callers must not synthesize this from timing or token characteristics.
    pub fn observe_server_reroute(&mut self, observation: ModelReroutedObservation) {
        let key = (observation.thread_id.clone(), observation.turn_id.clone());
        let chain = self.live_reroutes.entry(key.clone()).or_default();
        if chain.last() != Some(&observation) {
            chain.push(observation);
            if chain.len() > MAX_ROUTE_HOPS_PER_TURN {
                let excess = chain.len() - MAX_ROUTE_HOPS_PER_TURN;
                chain.drain(0..excess);
            }
        }
        self.trim_observations(&key);
    }

    pub fn scan(&mut self, codex_running: bool) -> MonitorSnapshot {
        self.scan_at_with_runtime(codex_running, now_unix_ms(), None)
    }

    pub fn scan_at(&mut self, codex_running: bool, checked_at_ms: u64) -> MonitorSnapshot {
        self.scan_at_with_runtime(codex_running, checked_at_ms, None)
    }

    pub fn scan_with_runtime(
        &mut self,
        codex_running: bool,
        earliest_process_start_seconds: Option<u64>,
    ) -> MonitorSnapshot {
        self.scan_at_with_runtime(codex_running, now_unix_ms(), earliest_process_start_seconds)
    }

    pub fn scan_at_with_runtime(
        &mut self,
        codex_running: bool,
        checked_at_ms: u64,
        earliest_process_start_seconds: Option<u64>,
    ) -> MonitorSnapshot {
        let warnings_before_scan = self
            .files
            .values()
            .map(|state| state.parse_warnings)
            .sum::<u64>();
        match self.discover_and_scan() {
            Ok(()) => self.collector_error = None,
            Err(error) => {
                self.collector_error = Some(if error.kind() == io::ErrorKind::NotFound {
                    "sessions_root_missing".to_owned()
                } else {
                    "session_discovery_failed".to_owned()
                });
            }
        }
        let warnings_after_scan = self
            .files
            .values()
            .map(|state| state.parse_warnings)
            .sum::<u64>();
        self.scan_parse_warnings = warnings_after_scan.saturating_sub(warnings_before_scan);
        self.apply_live_reroutes_to_states();
        prune_behavior_samples_per_baseline(&mut self.files);
        self.refresh_titles_if_changed();
        self.build_snapshot(
            codex_running,
            checked_at_ms,
            earliest_process_start_seconds.map(|value| value.saturating_mul(1_000)),
        )
    }

    pub fn file_states(&self) -> impl Iterator<Item = &FileState> {
        self.files.values()
    }

    pub fn export_file_states(&self) -> Vec<FileState> {
        self.files.values().cloned().collect()
    }

    pub fn export_cache(&self) -> CollectorCache {
        CollectorCache {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            cache_format_version: COLLECTOR_CACHE_FORMAT_VERSION,
            files: self
                .files
                .values()
                .filter_map(|state| {
                    let relative_path = normalized_relative_path(&self.sessions_root, &state.path)?;
                    PersistedFileState::from_runtime(state, relative_path)
                })
                .collect(),
        }
    }

    pub fn restore_cache(&mut self, cache: CollectorCache) -> bool {
        if cache.schema_version != SNAPSHOT_SCHEMA_VERSION
            || cache.cache_format_version != COLLECTOR_CACHE_FORMAT_VERSION
        {
            return false;
        }
        self.files.clear();
        self.restored_files.clear();
        let mut duplicates = HashSet::new();
        for state in cache.files {
            let Some(path) = safe_cached_path(&self.sessions_root, &state.relative_path) else {
                continue;
            };
            if duplicates.contains(&path) {
                continue;
            }
            if self.restored_files.insert(path.clone(), state).is_some() {
                self.restored_files.remove(&path);
                duplicates.insert(path);
            }
        }
        true
    }

    pub fn completed_turn_samples(&self) -> impl Iterator<Item = &CompletedTurnSample> {
        self.files
            .values()
            .filter_map(|state| state.last_completed_turn.as_ref())
    }

    pub fn completed_behavior_samples_v2(&self) -> impl Iterator<Item = &BehaviorSampleV2> {
        self.files
            .values()
            .flat_map(|state| state.completed_behavior_samples_v2.iter())
    }

    fn trim_observations(&mut self, newest_key: &(String, String)) {
        // Never throw away evidence for a turn that is still active. The key
        // that triggered this trim is also protected because a hook/reroute
        // can arrive just before the rollout file exposes the active turn.
        let mut protected = self
            .files
            .values()
            .filter_map(|state| {
                let thread_id = state.thread_id.as_ref()?;
                let turn = state.current_turn.as_ref()?;
                (turn.lifecycle == TurnLifecycle::Active)
                    .then(|| (thread_id.clone(), turn.turn_id.clone()))
            })
            .collect::<HashSet<_>>();
        protected.insert(newest_key.clone());

        trim_observation_map(&mut self.hook_observations, &protected, |observation| {
            observation.observed_at.clone()
        });
        trim_observation_map(&mut self.live_reroutes, &protected, |chain| {
            chain
                .last()
                .map(|observation| observation.observed_at.clone())
                .unwrap_or_default()
        });
    }

    fn apply_live_reroutes_to_states(&mut self) {
        for state in self.files.values_mut() {
            let Some(thread_id) = state.thread_id.as_ref() else {
                continue;
            };
            let Some(turn) = state.current_turn.as_mut() else {
                continue;
            };
            let key = (thread_id.clone(), turn.turn_id.clone());
            let matching_reroutes = self
                .live_reroutes
                .get(&key)
                .map(|chain| {
                    chain
                        .iter()
                        .filter(|reroute| live_reroute_matches_current_request(reroute, turn))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let Some(reroute) = matching_reroutes.last() else {
                continue;
            };
            turn.server_route.model = clean_optional(Some(reroute.to_model.clone()));
            turn.server_route.evidence = "explicitReroute".to_owned();
            turn.server_route.observed_at = Some(reroute.observed_at.clone());
            for reroute in &matching_reroutes {
                push_bounded_route_hop(
                    &mut turn.server_route.chain,
                    RouteHop {
                        from_model: reroute.from_model.clone(),
                        to_model: reroute.to_model.clone(),
                        reason: reroute.reason.clone(),
                        timestamp: reroute.observed_at.clone(),
                        association: "explicitThreadTurnLive".to_owned(),
                    },
                );
            }

            // A live reroute can arrive immediately before the terminal record
            // is discovered. Preserve that explicit evidence on the sanitized
            // completed sample so it can never contaminate the healthy local
            // behavior baseline.
            if turn.lifecycle != TurnLifecycle::Active {
                if let Some(sample) = state
                    .last_completed_behavior_sample_v2
                    .as_mut()
                    .filter(|sample| sample.turn_id == turn.turn_id)
                {
                    sample.explicit_reroute = true;
                }
                for sample in state
                    .completed_behavior_samples_v2
                    .iter_mut()
                    .filter(|sample| sample.turn_id == turn.turn_id)
                {
                    sample.explicit_reroute = true;
                }
            }
        }
    }

    fn discover_and_scan(&mut self) -> io::Result<()> {
        if !self.sessions_root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "sessions root is missing",
            ));
        }

        let mut paths = Vec::new();
        discover_rollouts(&self.sessions_root, &mut paths)?;
        let discovered: HashSet<PathBuf> = paths.iter().cloned().collect();
        self.files.retain(|path, _| discovered.contains(path));

        for path in paths {
            if !self.files.contains_key(&path) {
                let restored = take_restored_state(&mut self.restored_files, &path);
                self.files.insert(
                    path.clone(),
                    restored.unwrap_or_else(|| FileState::new(path.clone())),
                );
            }
            let state = self.files.get_mut(&path).expect("rollout state inserted");
            if let Err(error) = read_file_delta(state) {
                let code = if error.kind() == io::ErrorKind::NotFound {
                    "rollout_removed"
                } else {
                    "read_failed"
                };
                state.register_warning(code);
            }
        }
        // A saved cursor is useful only for the exact physical file whose
        // durable anchor matched during this discovery pass. Never attach an
        // unmatched cursor to a future file.
        self.restored_files.clear();
        Ok(())
    }

    fn refresh_titles_if_changed(&mut self) {
        let Some(path) = self.session_index_path.as_ref() else {
            return;
        };
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };
        let signature = (metadata.len(), modified_ms(&metadata));
        if self.title_index_signature == Some(signature) {
            return;
        }
        let Ok(contents) = fs::read_to_string(path) else {
            return;
        };

        let mut titles = HashMap::new();
        for line in contents.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(id) = get_string(&value, &["id", "thread_id", "threadId"]) else {
                continue;
            };
            let Some(title) = get_string(&value, &["thread_name", "title", "threadName"]) else {
                continue;
            };
            titles.insert(id, title);
        }
        self.titles = titles;
        self.title_index_signature = Some(signature);
    }

    fn build_snapshot(
        &self,
        codex_running: bool,
        checked_at_ms: u64,
        earliest_process_start_ms: Option<u64>,
    ) -> MonitorSnapshot {
        let active_parse_warnings = self
            .files
            .values()
            .filter_map(|state| {
                let turn = state.current_turn.as_ref()?;
                (turn.lifecycle == TurnLifecycle::Active).then_some(
                    state
                        .parse_warnings
                        .saturating_sub(turn.parse_warnings_at_start),
                )
            })
            .sum::<u64>();
        let parse_warnings = active_parse_warnings.max(self.scan_parse_warnings);
        let last_parse_error = self
            .files
            .values()
            .filter(|state| {
                state.current_turn.as_ref().is_some_and(|turn| {
                    turn.lifecycle == TurnLifecycle::Active
                        && state.parse_warnings > turn.parse_warnings_at_start
                }) || self.scan_parse_warnings > 0
            })
            .filter_map(|state| state.last_error.clone())
            .last();

        let health = if let Some(error) = self.collector_error.clone() {
            CollectorHealth {
                level: StatusLevel::Red,
                parse_warnings,
                last_error: Some(error),
            }
        } else if parse_warnings > 0 {
            CollectorHealth {
                level: StatusLevel::Yellow,
                parse_warnings,
                last_error: last_parse_error,
            }
        } else if codex_running {
            CollectorHealth {
                level: StatusLevel::Green,
                parse_warnings: 0,
                last_error: None,
            }
        } else {
            CollectorHealth {
                level: StatusLevel::Gray,
                parse_warnings: 0,
                last_error: None,
            }
        };

        let mut conversations = Vec::new();
        if codex_running && self.collector_error.is_none() {
            let mut selected: HashMap<String, &FileState> = HashMap::new();
            for state in self.files.values() {
                let Some(thread_id) = state.thread_id.as_ref() else {
                    continue;
                };
                let replace = selected.get(thread_id).is_none_or(|current| {
                    state.segment_start_ordinal > current.segment_start_ordinal
                        || (state.segment_start_ordinal == current.segment_start_ordinal
                            && state.last_write_ms > current.last_write_ms)
                });
                if replace {
                    selected.insert(thread_id.clone(), state);
                }
            }

            for state in selected.values() {
                if !state.identity_known || state.identity_rejected {
                    continue;
                }
                let Some(turn) = state.current_turn.as_ref() else {
                    continue;
                };
                if turn.lifecycle != TurnLifecycle::Active {
                    continue;
                }
                if earliest_process_start_ms.is_some_and(|minimum| {
                    turn_latest_event_ms(turn).is_some_and(|observed| observed < minimum)
                }) {
                    continue;
                }
                if let Some(conversation) = self.conversation_from_state(state, turn, checked_at_ms)
                {
                    conversations.push(conversation);
                }
            }
        }

        conversations.sort_by(|left, right| {
            let left_kind = if left.kind == ThreadKind::Root { 0 } else { 1 };
            let right_kind = if right.kind == ThreadKind::Root { 0 } else { 1 };
            left_kind
                .cmp(&right_kind)
                .then_with(|| left.parent_thread_id.cmp(&right.parent_thread_id))
                .then_with(|| left.title.cmp(&right.title))
                .then_with(|| left.thread_id.cmp(&right.thread_id))
        });

        MonitorSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            checked_at: unix_ms_to_iso8601(checked_at_ms),
            codex_running,
            collector_health: health,
            conversations,
        }
    }

    fn conversation_from_state(
        &self,
        state: &FileState,
        turn: &TurnState,
        checked_at_ms: u64,
    ) -> Option<ConversationSnapshot> {
        let thread_id = state.thread_id.clone()?;
        let key = (thread_id.clone(), turn.turn_id.clone());
        let hook = self.hook_observations.get(&key);

        let mut active_request = turn.active_request.clone();
        let mut hook_conflict = false;
        if let Some(hook) = hook {
            let hook_model = clean_optional(hook.model.clone());
            match (
                normalize_optional(&active_request.model),
                normalize_optional(&hook_model),
            ) {
                (None, Some(_)) => {
                    active_request.model = hook_model;
                    active_request.source = "userPromptSubmitHook".to_owned();
                }
                (Some(context_model), Some(hook_model)) if context_model == hook_model => {
                    active_request.source = "hook+turnContext".to_owned();
                }
                (Some(_), Some(_)) => hook_conflict = true,
                _ => {}
            }
        }

        let mut server_route = turn.server_route.clone();
        let matching_reroutes = self
            .live_reroutes
            .get(&key)
            .map(|chain| {
                chain
                    .iter()
                    .filter(|reroute| live_reroute_matches_current_request(reroute, turn))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(reroute) = matching_reroutes.last() {
            server_route.model = clean_optional(Some(reroute.to_model.clone()));
            server_route.evidence = "explicitReroute".to_owned();
            server_route.observed_at = Some(reroute.observed_at.clone());
            for reroute in matching_reroutes {
                push_bounded_route_hop(
                    &mut server_route.chain,
                    RouteHop {
                        from_model: reroute.from_model.clone(),
                        to_model: reroute.to_model.clone(),
                        reason: reroute.reason.clone(),
                        timestamp: reroute.observed_at.clone(),
                        association: "explicitThreadTurnLive".to_owned(),
                    },
                );
            }
        }

        let route_conflict = match (
            normalize_optional(&server_route.model),
            normalize_optional(&active_request.model),
        ) {
            (Some(route), Some(requested)) => route != requested,
            _ => false,
        };

        let status = if hook_conflict {
            StatusSnapshot {
                level: StatusLevel::Red,
                code: "request_evidence_conflict".to_owned(),
                explanation: "同一回合的 hook 模型与 turn_context 明确冲突".to_owned(),
            }
        } else if route_conflict {
            StatusSnapshot {
                level: StatusLevel::Red,
                code: "server_reroute_conflict".to_owned(),
                explanation: "显式服务器重路由目标与本回合请求模型不同".to_owned(),
            }
        } else if active_request.model.is_none()
            || active_request
                .effort
                .as_deref()
                .is_none_or(|effort| !is_known_effort(effort))
        {
            StatusSnapshot {
                level: StatusLevel::Yellow,
                code: "request_evidence_incomplete".to_owned(),
                explanation: "本回合请求模型缺失，或请求 effort 缺失/未知".to_owned(),
            }
        } else if turn.pending_next_turn.is_some() {
            StatusSnapshot {
                level: StatusLevel::Yellow,
                code: "next_turn_pending".to_owned(),
                explanation: "模型或 effort 已修改，将在下一回合生效".to_owned(),
            }
        } else if turn.usage_cumulative.is_empty() {
            StatusSnapshot {
                level: StatusLevel::Yellow,
                code: "token_data_pending".to_owned(),
                explanation: "尚未收到本回合 token_count".to_owned(),
            }
        } else if state.parse_warnings > turn.parse_warnings_at_start {
            StatusSnapshot {
                level: StatusLevel::Yellow,
                code: "collector_parse_warning".to_owned(),
                explanation: "该任务存在结构化事件解析警告".to_owned(),
            }
        } else {
            StatusSnapshot {
                level: StatusLevel::Green,
                code: "request_configuration_consistent".to_owned(),
                explanation: "本回合请求配置一致，采集器健康".to_owned(),
            }
        };

        let elapsed_ms = turn
            .started_at_ms
            .map(|started| checked_at_ms.saturating_sub(started));
        let usage = usage_snapshot(
            turn.usage_last.clone(),
            turn.usage_cumulative.clone(),
            turn.context_window,
        );
        let timing = timing_snapshot(
            elapsed_ms,
            turn.ttft_ms,
            turn.duration_ms,
            turn.usage_turn.output_tokens,
            turn.started_at_ms,
            &turn.model_intervals,
        );

        Some(ConversationSnapshot {
            thread_id: thread_id.clone(),
            turn_id: turn.turn_id.clone(),
            parent_thread_id: state.parent_thread_id.clone(),
            kind: state.kind,
            title: self.title_for(state, &thread_id),
            source_timestamp: turn.source_timestamp.clone(),
            active_request,
            pending_next_turn: turn.pending_next_turn.clone(),
            server_route,
            usage,
            timing,
            quality_assessment: QualityAssessment::default(),
            tool_activity: turn.tool_activity,
            status,
            // Heuristic anomaly detection is intentionally a separate future
            // input. This collector never derives model identity from metrics.
            anomalies: Vec::new(),
        })
    }

    fn title_for(&self, state: &FileState, thread_id: &str) -> String {
        if state.kind == ThreadKind::Subagent {
            if let Some(name) = state.agent_nickname.as_ref() {
                if !name.trim().is_empty() {
                    return format!("{}（子任务）", name.trim());
                }
            }
        }
        if let Some(title) = self.titles.get(thread_id) {
            return title.clone();
        }
        if let Some(cwd) = state.thread_settings.cwd.as_ref() {
            if let Some(name) = cwd
                .rsplit(['\\', '/'])
                .find(|component| !component.trim().is_empty())
            {
                return name.to_owned();
            }
        }
        thread_id.chars().take(8).collect()
    }
}

fn trim_observation_map<V>(
    observations: &mut HashMap<(String, String), V>,
    protected: &HashSet<(String, String)>,
    observed_at: impl Fn(&V) -> String,
) {
    let excess = observations.len().saturating_sub(MAX_LIVE_OBSERVATION_KEYS);
    if excess == 0 {
        return;
    }
    let mut removable = observations
        .iter()
        .filter(|(key, _)| !protected.contains(*key))
        .map(|(key, value)| (observed_at(value), key.clone()))
        .collect::<Vec<_>>();
    removable.sort();
    for (_, key) in removable.into_iter().take(excess) {
        observations.remove(&key);
    }
}

fn push_bounded_route_hop(chain: &mut Vec<RouteHop>, hop: RouteHop) {
    // A snapshot rebuild may replay the same live side-channel chain. Exact
    // route identity (including timestamp, but not transport association) is
    // sufficient to distinguish a genuine later A -> B transition from the
    // same event observed both live and in a persisted rollout.
    if chain.iter().any(|existing| {
        existing.from_model == hop.from_model
            && existing.to_model == hop.to_model
            && existing.timestamp == hop.timestamp
    }) {
        return;
    }
    chain.push(hop);
    if chain.len() > MAX_ROUTE_HOPS_PER_TURN {
        let excess = chain.len() - MAX_ROUTE_HOPS_PER_TURN;
        chain.drain(0..excess);
    }
}

fn prune_file_behavior_samples_per_baseline(state: &mut FileState) {
    let mut grouped = HashMap::<String, Vec<usize>>::new();
    for (index, sample) in state.completed_behavior_samples_v2.iter().enumerate() {
        grouped
            .entry(sample.baseline_key())
            .or_default()
            .push(index);
    }
    let mut keep = HashSet::new();
    for indices in grouped.values_mut() {
        indices.sort_by(|left, right| {
            compare_behavior_samples_newest_first(
                &state.completed_behavior_samples_v2[*left],
                &state.completed_behavior_samples_v2[*right],
            )
        });
        let mut identities = HashSet::new();
        for index in indices.iter().copied() {
            let sample = &state.completed_behavior_samples_v2[index];
            if identities.insert(behavior_sample_identity(sample))
                && identities.len() <= MAX_BEHAVIOR_SAMPLES_PER_BASELINE
            {
                keep.insert(index);
            }
        }
    }
    let mut index = 0_usize;
    state.completed_behavior_samples_v2.retain(|_| {
        let retain = keep.contains(&index);
        index += 1;
        retain
    });
}

fn prune_behavior_samples_per_baseline(files: &mut HashMap<PathBuf, FileState>) {
    let mut grouped = HashMap::<String, Vec<(PathBuf, usize)>>::new();
    for (path, state) in files.iter() {
        for (index, sample) in state.completed_behavior_samples_v2.iter().enumerate() {
            grouped
                .entry(sample.baseline_key())
                .or_default()
                .push((path.clone(), index));
        }
    }

    let mut keep = HashSet::<(PathBuf, usize)>::new();
    for positions in grouped.values_mut() {
        positions.sort_by(|(left_path, left_index), (right_path, right_index)| {
            let left = &files[left_path].completed_behavior_samples_v2[*left_index];
            let right = &files[right_path].completed_behavior_samples_v2[*right_index];
            compare_behavior_samples_newest_first(left, right)
                .then_with(|| left_path.cmp(right_path))
                .then_with(|| left_index.cmp(right_index))
        });
        let mut identities = HashSet::new();
        for (path, index) in positions.iter() {
            let sample = &files[path].completed_behavior_samples_v2[*index];
            if identities.insert(behavior_sample_identity(sample))
                && identities.len() <= MAX_BEHAVIOR_SAMPLES_PER_BASELINE
            {
                keep.insert((path.clone(), *index));
            }
        }
    }

    for (path, state) in files.iter_mut() {
        let mut index = 0_usize;
        state.completed_behavior_samples_v2.retain(|_| {
            let retain = keep.contains(&(path.clone(), index));
            index += 1;
            retain
        });
    }
}

fn compare_behavior_samples_newest_first(
    left: &BehaviorSampleV2,
    right: &BehaviorSampleV2,
) -> std::cmp::Ordering {
    behavior_sample_timestamp(right)
        .cmp(&behavior_sample_timestamp(left))
        .then_with(|| right.observed_at.cmp(&left.observed_at))
        // For duplicate terminal records, retain the conservative form so a
        // rerouted or dirty copy can never be replaced by an eligible copy.
        .then_with(|| right.explicit_reroute.cmp(&left.explicit_reroute))
        .then_with(|| left.clean.cmp(&right.clean))
        .then_with(|| left.thread_id.cmp(&right.thread_id))
        .then_with(|| left.turn_id.cmp(&right.turn_id))
}

fn behavior_sample_identity(sample: &BehaviorSampleV2) -> (String, String, String) {
    (
        sample.thread_id.clone(),
        sample.turn_id.clone(),
        sample.observed_at.clone(),
    )
}

fn behavior_sample_timestamp(sample: &BehaviorSampleV2) -> Option<i64> {
    DateTime::parse_from_rfc3339(&sample.observed_at)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

fn discover_rollouts(directory: &Path, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            discover_rollouts(&path, paths)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("rollout-") && name.ends_with(".jsonl") {
            paths.push(path);
        }
    }
    Ok(())
}

fn normalized_relative_path(sessions_root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(sessions_root).ok()?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (!components.is_empty()).then(|| components.join("/"))
}

fn safe_cached_path(sessions_root: &Path, relative_path: &str) -> Option<PathBuf> {
    if relative_path.is_empty() || relative_path.contains('\\') {
        return None;
    }
    let relative = Path::new(relative_path);
    if relative.is_absolute() {
        return None;
    }
    let mut path = sessions_root.to_path_buf();
    let mut count = 0usize;
    for component in relative.components() {
        let Component::Normal(value) = component else {
            return None;
        };
        path.push(value);
        count += 1;
    }
    (count > 0).then_some(path)
}

fn take_restored_state(
    restored: &mut HashMap<PathBuf, PersistedFileState>,
    path: &Path,
) -> Option<FileState> {
    let state = restored.remove(path)?;
    let mut file = open_rollout(path).ok()?;
    let target_length = file.metadata().ok()?.len();
    if !persisted_anchor_matches(&state, &mut file, target_length).ok()? {
        return None;
    }
    Some(state.into_runtime(path.to_path_buf()))
}

fn persisted_anchor_matches(
    state: &PersistedFileState,
    file: &mut File,
    target_length: u64,
) -> io::Result<bool> {
    let cursor = &state.cursor;
    if cursor.offset == 0
        || cursor.offset > target_length
        || cursor.observed_length < cursor.offset
        || cursor.anchor_length == 0
        || cursor.anchor_length > ANCHOR_BYTES
        || cursor
            .anchor_offset
            .saturating_add(cursor.anchor_length as u64)
            != cursor.offset
    {
        return Ok(false);
    }
    let saved = file.stream_position()?;
    file.seek(SeekFrom::Start(cursor.anchor_offset))?;
    let mut bytes = vec![0_u8; cursor.anchor_length];
    let result = file.read_exact(&mut bytes);
    file.seek(SeekFrom::Start(saved))?;
    result?;
    Ok(stable_hash(&bytes) == cursor.anchor_hash)
}

fn read_file_delta(state: &mut FileState) -> io::Result<()> {
    let mut file = open_rollout(&state.path)?;
    let metadata = file.metadata()?;
    let target_length = metadata.len();
    let write_ms = modified_ms(&metadata);

    let same_length_rewrite = state.offset > 0
        && state.offset == target_length
        && state.last_write_ms > 0
        && write_ms > 0
        && state.last_write_ms != write_ms;
    let anchor_changed = state.offset > 0 && !anchor_matches(state, &mut file)?;
    if state.offset > target_length || same_length_rewrite || anchor_changed {
        state.reset_preserving_path();
    }

    file.seek(SeekFrom::Start(state.offset))?;
    while file.stream_position()? < target_length {
        let remaining = target_length - file.stream_position()?;
        let mut buffer = vec![0_u8; remaining.min(READ_CHUNK_BYTES as u64) as usize];
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        buffer.truncate(read);
        process_bytes(state, &buffer);
        state.offset = file.stream_position()?;
        if !state.discard_oversize {
            state.durable_offset = state.offset.saturating_sub(state.carry_bytes.len() as u64);
        }
    }

    state.observed_length = target_length;
    state.last_write_ms = write_ms;
    set_anchor(state, &mut file)?;
    set_durable_anchor(state, &mut file)?;
    if matches!(
        state.last_error.as_deref(),
        Some("read_failed" | "metadata_failed")
    ) {
        state.last_error = None;
    }
    Ok(())
}

fn open_rollout(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x00000001;
        const FILE_SHARE_WRITE: u32 = 0x00000002;
        const FILE_SHARE_DELETE: u32 = 0x00000004;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    options.open(path)
}

fn anchor_matches(state: &FileState, file: &mut File) -> io::Result<bool> {
    if state.anchor_length == 0 {
        return Ok(true);
    }
    if state
        .anchor_offset
        .saturating_add(state.anchor_length as u64)
        > file.metadata()?.len()
    {
        return Ok(false);
    }
    let saved = file.stream_position()?;
    file.seek(SeekFrom::Start(state.anchor_offset))?;
    let mut bytes = vec![0_u8; state.anchor_length];
    let result = file.read_exact(&mut bytes);
    file.seek(SeekFrom::Start(saved))?;
    result?;
    Ok(stable_hash(&bytes) == state.anchor_hash)
}

fn set_anchor(state: &mut FileState, file: &mut File) -> io::Result<()> {
    let length = (state.offset as usize).min(ANCHOR_BYTES);
    if length == 0 {
        state.anchor_offset = 0;
        state.anchor_length = 0;
        state.anchor_hash = 0;
        return Ok(());
    }
    let offset = state.offset - length as u64;
    let saved = file.stream_position()?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)?;
    file.seek(SeekFrom::Start(saved))?;
    state.anchor_offset = offset;
    state.anchor_length = length;
    state.anchor_hash = stable_hash(&bytes);
    Ok(())
}

fn set_durable_anchor(state: &mut FileState, file: &mut File) -> io::Result<()> {
    let length = (state.durable_offset as usize).min(ANCHOR_BYTES);
    if length == 0 {
        state.durable_anchor_offset = 0;
        state.durable_anchor_length = 0;
        state.durable_anchor_hash = 0;
        return Ok(());
    }
    if state.durable_anchor_length == length
        && state
            .durable_anchor_offset
            .saturating_add(state.durable_anchor_length as u64)
            == state.durable_offset
    {
        return Ok(());
    }
    let offset = state.durable_offset - length as u64;
    let saved = file.stream_position()?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)?;
    file.seek(SeekFrom::Start(saved))?;
    state.durable_anchor_offset = offset;
    state.durable_anchor_length = length;
    state.durable_anchor_hash = stable_hash(&bytes);
    Ok(())
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn process_bytes(state: &mut FileState, chunk: &[u8]) {
    let mut incoming = chunk;
    if state.discard_oversize {
        let Some(position) = incoming.iter().position(|byte| *byte == b'\n') else {
            return;
        };
        state.discard_oversize = false;
        incoming = &incoming[position + 1..];
    }

    let mut combined = std::mem::take(&mut state.carry_bytes);
    combined.extend_from_slice(incoming);
    let mut line_start = 0;
    for index in 0..combined.len() {
        if combined[index] != b'\n' {
            continue;
        }
        let mut line_end = index;
        if line_end > line_start && combined[line_end - 1] == b'\r' {
            line_end -= 1;
        }
        let line_bytes = &combined[line_start..line_end];
        if line_bytes.len() > MAX_PENDING_LINE_BYTES {
            state.register_warning("line_exceeds_limit");
        } else {
            match std::str::from_utf8(line_bytes) {
                Ok(line) => process_line(state, line.trim_start_matches('\u{feff}')),
                Err(_) => state.register_warning("invalid_utf8_line"),
            }
        }
        line_start = index + 1;
    }

    let pending = &combined[line_start..];
    if pending.len() > MAX_PENDING_LINE_BYTES {
        state.register_warning("line_exceeds_limit");
        state.discard_oversize = true;
    } else {
        state.carry_bytes.extend_from_slice(pending);
    }
}

fn process_line(state: &mut FileState, line: &str) {
    if line.trim().is_empty() {
        return;
    }

    if !state.identity_known {
        if state.identity_rejected {
            return;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            state.register_warning("invalid_session_meta");
            state.identity_rejected = true;
            return;
        };
        let ordinal = record.get("ordinal").and_then(Value::as_i64);
        if record.get("type").and_then(Value::as_str) != Some("session_meta")
            || ordinal.is_none_or(|value| value < 0)
        {
            state.register_warning("first_record_not_session_meta");
            state.identity_rejected = true;
            return;
        }
        process_session_meta(state, &record, ordinal.unwrap_or_default());
        return;
    }

    if !is_potentially_relevant(line) {
        return;
    }
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        state.register_warning("invalid_relevant_json");
        return;
    };
    if record.get("type").and_then(Value::as_str) == Some("session_meta") {
        return;
    }

    let ordinal = record.get("ordinal").and_then(Value::as_i64);
    if state.own_start_ordinal > 0 && ordinal.is_none() {
        state.register_warning("subagent_record_missing_ordinal");
        return;
    }
    if ordinal.is_some_and(|value| value < state.own_start_ordinal) {
        return;
    }

    let record_type = record
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = record.get("payload").unwrap_or(&Value::Null);
    let timestamp = get_string(&record, &["timestamp"]);

    if record_type == "turn_context" {
        process_turn_context(state, payload, timestamp);
        return;
    }

    if record_type == "event_msg" {
        let event_type = get_string(payload, &["type"]).unwrap_or_default();
        process_event(state, &event_type, payload, timestamp);
        return;
    }

    if record_type == "response_item" {
        let item_type = get_string(payload, &["type"]).unwrap_or_default();
        if matches!(
            item_type.as_str(),
            "function_call"
                | "custom_tool_call"
                | "local_shell_call"
                | "web_search_call"
                | "mcp_tool_call"
                | "computer_call"
        ) {
            if let Some(turn) = state.current_turn.as_mut() {
                if turn.lifecycle == TurnLifecycle::Active {
                    turn.tool_activity = true;
                }
            }
            set_event_timestamp(state, timestamp);
        }
        return;
    }

    let normalized = normalize_event_type(record_type);
    if matches!(
        normalized.as_str(),
        "model_rerouted" | "turn_completed" | "turn_aborted"
    ) {
        process_event(state, record_type, payload, timestamp);
    }
}

fn process_session_meta(state: &mut FileState, record: &Value, ordinal: i64) {
    let payload = record.get("payload").unwrap_or(&Value::Null);
    let Some(thread_id) = get_string(payload, &["id", "thread_id", "threadId"]) else {
        state.register_warning("session_meta_missing_id");
        state.identity_rejected = true;
        return;
    };

    // Preserve identity and segment position even when pagination is invalid.
    // A malformed newer segment must suppress stale state from an older file.
    state.thread_id = Some(thread_id.clone());
    state.segment_start_ordinal = ordinal;
    if ordinal > 0 {
        let history_mode = get_string(payload, &["history_mode", "historyMode"]);
        let history_base = payload
            .get("history_base")
            .or_else(|| payload.get("historyBase"))
            .unwrap_or(&Value::Null);
        let base_thread_id = get_string(history_base, &["thread_id", "threadId"]);
        let end_ordinal = history_base
            .get("end_ordinal_exclusive")
            .or_else(|| history_base.get("endOrdinalExclusive"))
            .and_then(Value::as_i64);
        // `history_base` names the rollout whose prefix this physical segment
        // inherits. In a fork or a second pagination boundary it can refer to
        // an ancestor rather than to this segment's logical thread. The
        // session_meta payload remains the identity of the current segment;
        // validate the pagination boundary without conflating it with lineage.
        let valid = history_mode.as_deref() == Some("paginated")
            && base_thread_id.is_some()
            && end_ordinal == Some(ordinal);
        if !valid {
            state.register_warning("invalid_session_continuation");
            state.identity_rejected = true;
            return;
        }
    }

    state.parent_thread_id = get_string(payload, &["parent_thread_id", "parentThreadId"]);
    let thread_source = get_string(payload, &["thread_source", "threadSource"]);
    state.kind = if thread_source.as_deref() == Some("subagent") {
        ThreadKind::Subagent
    } else {
        ThreadKind::Root
    };
    state.own_start_ordinal = payload
        .get("subagent_history_start_ordinal")
        .or_else(|| payload.get("subagentHistoryStartOrdinal"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    if let Some(cwd) = get_string(payload, &["cwd"]) {
        state.thread_settings.cwd = Some(cwd);
    }

    let spawn = payload
        .pointer("/source/subagent/thread_spawn")
        .or_else(|| payload.pointer("/source/subagent/threadSpawn"))
        .unwrap_or(&Value::Null);
    if state.parent_thread_id.is_none() {
        state.parent_thread_id = get_string(spawn, &["parent_thread_id", "parentThreadId"]);
    }
    state.agent_path = get_string(payload, &["agent_path", "agentPath"])
        .or_else(|| get_string(spawn, &["agent_path", "agentPath"]));
    state.agent_nickname = get_string(payload, &["agent_nickname", "agentNickname"])
        .or_else(|| get_string(spawn, &["agent_nickname", "agentNickname"]));
    state.identity_known = true;
    state.identity_rejected = false;
}

fn process_turn_context(state: &mut FileState, payload: &Value, timestamp: Option<String>) {
    let Some(turn_id) = get_string(payload, &["turn_id", "turnId"]) else {
        state.register_warning("turn_context_missing_turn_id");
        return;
    };
    let warning_baseline = state.parse_warnings;
    let Some(turn) = state.current_turn.as_mut() else {
        state.register_warning("orphan_turn_context");
        return;
    };
    if turn.lifecycle != TurnLifecycle::Active || turn.turn_id != turn_id {
        state.register_warning("orphan_turn_context");
        return;
    }

    // A continuation is a new model request within the Codex turn. A reroute
    // from the previous request is no longer evidence for this request.
    turn.server_route.model = None;
    turn.server_route.evidence = "notObserved".to_owned();
    turn.server_route.observed_at = None;
    turn.server_route.chain.clear();
    turn.request_observed_at = timestamp.clone();
    turn.parse_warnings_at_start = warning_baseline;
    turn.active_request = RequestSnapshot::new(
        get_string(payload, &["model"]),
        get_string(payload, &["effort", "reasoning_effort", "reasoningEffort"]),
        "turnContext",
    );
    if let Some(cwd) = get_string(payload, &["cwd"]) {
        state.thread_settings.cwd = Some(cwd);
    }
    update_pending(turn, &state.thread_settings);
    set_event_timestamp(state, timestamp);
}

fn process_event(
    state: &mut FileState,
    raw_event_type: &str,
    payload: &Value,
    timestamp: Option<String>,
) {
    match normalize_event_type(raw_event_type).as_str() {
        "thread_settings_applied" => {
            let settings = payload
                .get("thread_settings")
                .or_else(|| payload.get("threadSettings"))
                .unwrap_or(payload);
            if let Some(model) = get_string(settings, &["model"]) {
                state.thread_settings.model = Some(model);
            }
            if let Some(effort) =
                get_string(settings, &["reasoning_effort", "reasoningEffort", "effort"])
            {
                state.thread_settings.effort = Some(effort);
            }
            if let Some(cwd) = get_string(settings, &["cwd"]) {
                state.thread_settings.cwd = Some(cwd);
            }
            state.thread_settings.observed_at = timestamp.clone();
            if let Some(turn) = state.current_turn.as_mut() {
                if turn.lifecycle == TurnLifecycle::Active {
                    update_pending(turn, &state.thread_settings);
                }
            }
            set_event_timestamp(state, timestamp);
        }
        "task_started" | "turn_started" => {
            let Some(turn_id) = get_string(payload, &["turn_id", "turnId"]) else {
                state.register_warning("task_started_missing_turn_id");
                return;
            };
            if state.current_turn.as_ref().is_some_and(|turn| {
                turn.turn_id == turn_id && turn.lifecycle != TurnLifecycle::Active
            }) {
                return;
            }
            if state
                .current_turn
                .as_ref()
                .is_none_or(|turn| turn.turn_id != turn_id)
            {
                state.current_turn = Some(TurnState {
                    turn_id,
                    lifecycle: TurnLifecycle::Active,
                    started_at_ms: numeric_u64(payload, &["started_at", "startedAt"])
                        .map(seconds_or_milliseconds)
                        .or_else(|| timestamp.as_deref().and_then(timestamp_to_unix_ms)),
                    started_at: timestamp.clone(),
                    source_timestamp: timestamp.clone(),
                    request_observed_at: timestamp.clone(),
                    parse_warnings_at_start: state.parse_warnings,
                    active_request: state.thread_settings.as_request("threadSettings"),
                    pending_next_turn: None,
                    server_route: ServerRouteSnapshot::default(),
                    usage_baseline: state.last_total_usage.clone(),
                    usage_last: TokenUsage::default(),
                    usage_cumulative: TokenUsage::default(),
                    usage_turn: TokenUsage::default(),
                    context_window: numeric_u64(
                        payload,
                        &["model_context_window", "modelContextWindow"],
                    ),
                    ttft_ms: None,
                    duration_ms: None,
                    terminal_reason: None,
                    tool_activity: false,
                    model_intervals: Vec::new(),
                });
            }
            set_event_timestamp(state, timestamp);
        }
        "model_reroute" | "model_rerouted" => {
            process_reroute(state, raw_event_type, payload, timestamp);
        }
        "token_count" => process_token_count(state, payload, timestamp),
        "item_completed" => process_item_completed(state, payload, timestamp),
        "task_complete" | "turn_completed" => {
            process_terminal(state, payload, TurnLifecycle::Completed, timestamp)
        }
        "turn_aborted" => process_terminal(state, payload, TurnLifecycle::Aborted, timestamp),
        _ => {}
    }
}

fn process_reroute(
    state: &mut FileState,
    raw_event_type: &str,
    payload: &Value,
    timestamp: Option<String>,
) {
    let Some(turn) = state.current_turn.as_mut() else {
        return;
    };
    if turn.lifecycle != TurnLifecycle::Active {
        return;
    }
    let event_thread_id = get_string(payload, &["thread_id", "threadId"]);
    let event_turn_id = get_string(payload, &["turn_id", "turnId"]);
    if event_thread_id
        .as_ref()
        .zip(state.thread_id.as_ref())
        .is_some_and(|(event, current)| event != current)
        || event_turn_id
            .as_ref()
            .is_some_and(|event| event != &turn.turn_id)
    {
        return;
    }
    let Some(from_model) = get_string(payload, &["from_model", "fromModel"]) else {
        state.register_warning("invalid_model_reroute");
        return;
    };
    let Some(to_model) = get_string(payload, &["to_model", "toModel"]) else {
        state.register_warning("invalid_model_reroute");
        return;
    };
    let Some(timestamp) = timestamp else {
        state.register_warning("invalid_model_reroute");
        return;
    };

    let normalized = normalize_event_type(raw_event_type);
    let association = if normalized == "model_reroute" {
        "persistedRolloutOrder"
    } else {
        if event_thread_id.as_deref() != state.thread_id.as_deref()
            || event_turn_id.as_deref() != Some(turn.turn_id.as_str())
        {
            state.register_warning("unbound_model_rerouted_notification");
            return;
        }
        "explicitThreadTurn"
    };
    let hop = RouteHop {
        from_model,
        to_model: to_model.clone(),
        reason: get_string(payload, &["reason"]),
        timestamp: timestamp.clone(),
        association: association.to_owned(),
    };
    turn.server_route.model = Some(to_model);
    turn.server_route.evidence = "explicitReroute".to_owned();
    turn.server_route.observed_at = Some(timestamp.clone());
    push_bounded_route_hop(&mut turn.server_route.chain, hop);
    set_event_timestamp(state, Some(timestamp));
}

fn process_item_completed(state: &mut FileState, payload: &Value, timestamp: Option<String>) {
    let Some(turn) = state.current_turn.as_mut() else {
        return;
    };
    if turn.lifecycle != TurnLifecycle::Active {
        return;
    }
    if get_string(payload, &["thread_id", "threadId"])
        .as_ref()
        .zip(state.thread_id.as_ref())
        .is_some_and(|(event, current)| event != current)
        || get_string(payload, &["turn_id", "turnId"])
            .as_deref()
            .is_some_and(|event| event != turn.turn_id)
    {
        return;
    }

    // Deliberately access only the sanitized envelope. The item content,
    // reasoning summary, message text, command and tool payloads are never
    // copied into monitor state, cache, logs or snapshots.
    let item = payload.get("item").unwrap_or(&Value::Null);
    let Some(item_type) = get_string(item, &["type"]) else {
        return;
    };
    if !matches!(item_type.as_str(), "Reasoning" | "AgentMessage") {
        return;
    }
    let Some(item_id) = get_string(item, &["id"]) else {
        state.register_warning("model_item_missing_id");
        return;
    };
    let Some(started_at_ms) = numeric_u64(payload, &["started_at_ms", "startedAtMs"])
        .or_else(|| numeric_u64(item, &["started_at_ms", "startedAtMs"]))
    else {
        state.register_warning("model_item_missing_timing");
        return;
    };
    let Some(completed_at_ms) = numeric_u64(payload, &["completed_at_ms", "completedAtMs"])
        .or_else(|| numeric_u64(item, &["completed_at_ms", "completedAtMs"]))
    else {
        state.register_warning("model_item_missing_timing");
        return;
    };
    if completed_at_ms < started_at_ms {
        state.register_warning("model_item_invalid_timing");
        return;
    }
    if !turn
        .model_intervals
        .iter()
        .any(|interval| interval.item_id == item_id)
    {
        turn.model_intervals.push(ModelItemInterval {
            item_id,
            item_type,
            started_at_ms,
            completed_at_ms,
        });
    }
    set_event_timestamp(state, timestamp);
}

fn process_token_count(state: &mut FileState, payload: &Value, timestamp: Option<String>) {
    let info = payload.get("info").unwrap_or(&Value::Null);
    let total = info
        .get("total_token_usage")
        .or_else(|| info.get("totalTokenUsage"))
        .map(parse_token_usage);
    let reported_last = info
        .get("last_token_usage")
        .or_else(|| info.get("lastTokenUsage"))
        .map(parse_token_usage);
    let context_window = numeric_u64(info, &["model_context_window", "modelContextWindow"]);
    let previous_total = state.last_total_usage.clone();

    if let Some(turn) = state.current_turn.as_mut() {
        if turn.lifecycle == TurnLifecycle::Active {
            if let Some(total) = total.as_ref() {
                turn.usage_cumulative = total.clone();
                turn.usage_turn = usage_since_baseline(total, &turn.usage_baseline);
                turn.usage_last = reported_last
                    .clone()
                    .unwrap_or_else(|| usage_since_baseline(total, &previous_total));
            } else if let Some(last) = reported_last.as_ref() {
                turn.usage_last = last.clone();
            }
            if context_window.is_some() {
                turn.context_window = context_window;
            }
            set_event_timestamp(state, timestamp.clone());
        }
    }
    if let Some(total) = total {
        state.last_total_usage = total;
    }
}

fn process_terminal(
    state: &mut FileState,
    payload: &Value,
    lifecycle: TurnLifecycle,
    timestamp: Option<String>,
) {
    let Some(turn_id) = get_string(payload, &["turn_id", "turnId"]) else {
        state.register_warning("terminal_missing_turn_id");
        return;
    };
    let current_parse_warnings = state.parse_warnings;
    let (sample, behavior_sample_v2) = {
        let Some(turn) = state.current_turn.as_mut() else {
            return;
        };
        if turn.turn_id != turn_id {
            return;
        }
        turn.lifecycle = lifecycle;
        turn.terminal_reason = Some(if lifecycle == TurnLifecycle::Aborted {
            "aborted".to_owned()
        } else {
            "completed".to_owned()
        });
        turn.ttft_ms = numeric_u64(payload, &["time_to_first_token_ms", "timeToFirstTokenMs"]);
        turn.duration_ms = numeric_u64(payload, &["duration_ms", "durationMs"]);

        if lifecycle == TurnLifecycle::Completed {
            let usage = &turn.usage_turn;
            let completed_at = timestamp
                .clone()
                .or_else(|| turn.source_timestamp.clone())
                .unwrap_or_default();
            let sample = CompletedTurnSample {
                thread_id: state.thread_id.clone().unwrap_or_default(),
                turn_id: turn.turn_id.clone(),
                kind: state.kind,
                model: turn.active_request.model.clone(),
                effort: turn.active_request.effort.clone(),
                input_bucket: input_bucket(usage.input_tokens).to_owned(),
                tool_activity: turn.tool_activity,
                ttft_ms: turn.ttft_ms,
                duration_ms: turn.duration_ms,
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                reasoning_output_tokens: usage.reasoning_output_tokens,
                cache_input_share: cache_input_share(usage),
                completed_at: completed_at.clone(),
            };
            let model_active_ms = union_interval_ms(
                turn.model_intervals
                    .iter()
                    .map(|interval| (interval.started_at_ms, interval.completed_at_ms)),
            );
            let reasoning_ms = reasoning_active_ms(&turn.model_intervals);
            let reasoning_output_share = (usage.output_tokens > 0).then(|| {
                usage.reasoning_output_tokens.min(usage.output_tokens) as f64
                    / usage.output_tokens as f64
            });
            let reasoning_phase_share = model_active_ms
                .filter(|value| *value > 0)
                .map(|model_ms| reasoning_ms.unwrap_or_default() as f64 / model_ms as f64);
            let behavior = BehaviorSampleV2 {
                thread_id: sample.thread_id.clone(),
                turn_id: sample.turn_id.clone(),
                model: sample.model.clone().unwrap_or_else(|| "unknown".to_owned()),
                effort: sample
                    .effort
                    .clone()
                    .unwrap_or_else(|| "unknown".to_owned()),
                uncached_input_bucket: uncached_input_bucket(
                    usage.input_tokens,
                    usage.cached_input_tokens,
                )
                .to_owned(),
                output_bucket: output_bucket(usage.output_tokens).to_owned(),
                tool_activity: turn.tool_activity,
                output_tokens: usage.output_tokens,
                ttft_ms: turn.ttft_ms,
                model_phase_output_rate: observed_output_rate(usage.output_tokens, model_active_ms),
                reasoning_output_share,
                reasoning_phase_share,
                cache_input_share: cache_input_share(usage),
                clean: current_parse_warnings == turn.parse_warnings_at_start,
                explicit_reroute: !turn.server_route.chain.is_empty(),
                observed_at: completed_at,
            };
            (Some(sample), Some(behavior))
        } else {
            (None, None)
        }
    };
    if let Some(sample) = sample {
        state.last_completed_turn = Some(sample);
    }
    if let Some(sample) = behavior_sample_v2 {
        state.last_completed_behavior_sample_v2 = Some(sample.clone());
        if !state.completed_behavior_samples_v2.iter().any(|existing| {
            existing.thread_id == sample.thread_id && existing.turn_id == sample.turn_id
        }) {
            state.completed_behavior_samples_v2.push(sample);
            prune_file_behavior_samples_per_baseline(state);
        }
    }
    set_event_timestamp(state, timestamp);
}

fn input_bucket(input_tokens: u64) -> &'static str {
    match input_tokens {
        0..=8_191 => "0-8k",
        8_192..=32_767 => "8k-32k",
        32_768..=131_071 => "32k-128k",
        _ => "128k+",
    }
}

fn is_known_effort(effort: &str) -> bool {
    matches!(
        effort.trim().to_ascii_lowercase().as_str(),
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
    )
}

fn update_pending(turn: &mut TurnState, settings: &crate::model::ThreadSettings) {
    let configured = settings.as_request("threadSettings");
    let has_value = configured.model.is_some() || configured.effort.is_some();
    turn.pending_next_turn = if has_value && configured.differs_from(&turn.active_request) {
        Some(configured)
    } else {
        None
    };
}

fn set_event_timestamp(state: &mut FileState, timestamp: Option<String>) {
    let Some(timestamp) = timestamp else {
        return;
    };
    state.last_event_timestamp = Some(timestamp.clone());
    if let Some(turn) = state.current_turn.as_mut() {
        turn.source_timestamp = Some(timestamp);
    }
}

fn parse_token_usage(value: &Value) -> TokenUsage {
    TokenUsage {
        input_tokens: numeric_u64(value, &["input_tokens", "inputTokens"]).unwrap_or_default(),
        cached_input_tokens: numeric_u64(value, &["cached_input_tokens", "cachedInputTokens"])
            .unwrap_or_default(),
        cache_write_input_tokens: numeric_u64(
            value,
            &["cache_write_input_tokens", "cacheWriteInputTokens"],
        )
        .unwrap_or_default(),
        output_tokens: numeric_u64(value, &["output_tokens", "outputTokens"]).unwrap_or_default(),
        reasoning_output_tokens: numeric_u64(
            value,
            &["reasoning_output_tokens", "reasoningOutputTokens"],
        )
        .unwrap_or_default(),
        total_tokens: numeric_u64(value, &["total_tokens", "totalTokens"]).unwrap_or_default(),
    }
}

fn numeric_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|field| {
            field
                .as_u64()
                .or_else(|| field.as_i64().and_then(|number| u64::try_from(number).ok()))
                .or_else(|| field.as_str().and_then(|text| text.parse::<u64>().ok()))
        })
    })
}

fn get_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

fn normalize_event_type(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('/', "_")
}

fn is_potentially_relevant(line: &str) -> bool {
    const NAMES: [&str; 14] = [
        "turn_context",
        "thread_settings_applied",
        "task_started",
        "turn_started",
        "turn/started",
        "model_reroute",
        "model_rerouted",
        "model/rerouted",
        "token_count",
        "item_completed",
        "task_complete",
        "turn_completed",
        "turn/completed",
        "turn_aborted",
    ];
    NAMES.iter().any(|name| line.contains(name))
        || (line.contains("response_item") && line.contains("_call"))
}

fn seconds_or_milliseconds(value: u64) -> u64 {
    if value < 10_000_000_000 {
        value.saturating_mul(1_000)
    } else {
        value
    }
}

fn modified_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn turn_latest_event_ms(turn: &TurnState) -> Option<u64> {
    turn.source_timestamp
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .and_then(|value| u64::try_from(value.timestamp_millis()).ok())
        .or(turn.started_at_ms)
}

/// A `turn_id` can span multiple request continuations. Live reroute
/// notifications are bound to the request that was active when the server
/// emitted them, not forever to the containing turn. Timestamp comparison is
/// deliberately conservative: malformed evidence, or evidence older than the
/// latest `turn_context`, is not presented as the current server route.
fn live_reroute_matches_current_request(
    reroute: &ModelReroutedObservation,
    turn: &TurnState,
) -> bool {
    let Some(boundary) = turn.request_observed_at.as_deref() else {
        return false;
    };
    let Some(boundary_ms) = timestamp_to_unix_ms(boundary) else {
        return false;
    };
    let Some(reroute_ms) = timestamp_to_unix_ms(&reroute.observed_at) else {
        return false;
    };
    reroute_ms >= boundary_ms
}

fn timestamp_to_unix_ms(value: &str) -> Option<u64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|timestamp| u64::try_from(timestamp.timestamp_millis()).ok())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn unix_ms_to_iso8601(milliseconds: u64) -> String {
    let total_seconds = milliseconds / 1_000;
    let millis = milliseconds % 1_000;
    let days = (total_seconds / 86_400) as i64;
    let seconds_of_day = total_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

// Howard Hinnant's civil-from-days transform, with day zero at 1970-01-01.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::Persistence;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let suffix = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mochi-meter-{label}-{}-{suffix}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create fixture directory");
            Self(path)
        }

        fn rollout(&self, name: &str) -> PathBuf {
            let nested = self.0.join("2026").join("08").join("24");
            fs::create_dir_all(&nested).expect("create rollout hierarchy");
            nested.join(format!("rollout-{name}.jsonl"))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn append(path: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open fixture");
        file.write_all(bytes).expect("append fixture");
        file.flush().expect("flush fixture");
    }

    fn line(value: Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&value).expect("serialize fixture");
        bytes.push(b'\n');
        bytes
    }

    fn meta(thread_id: &str, ordinal: i64) -> Value {
        serde_json::json!({
            "timestamp": "2026-08-24T08:00:00.000Z",
            "ordinal": ordinal,
            "type": "session_meta",
            "payload": { "id": thread_id, "cwd": "C:\\fixture\\root", "thread_source": "user" }
        })
    }

    fn event(ordinal: i64, event_type: &str, extra: Value) -> Value {
        let mut payload = serde_json::Map::new();
        payload.insert("type".to_owned(), Value::String(event_type.to_owned()));
        if let Value::Object(extra) = extra {
            payload.extend(extra);
        }
        serde_json::json!({
            "timestamp": format!("2026-08-24T08:00:{:02}.000Z", ordinal.min(59)),
            "ordinal": ordinal,
            "type": "event_msg",
            "payload": payload
        })
    }

    fn context(ordinal: i64, turn_id: &str, model: &str, effort: &str) -> Value {
        serde_json::json!({
            "timestamp": "2026-08-24T08:00:05.000Z",
            "ordinal": ordinal,
            "type": "turn_context",
            "payload": { "turn_id": turn_id, "model": model, "effort": effort }
        })
    }

    fn active_fixture(thread_id: &str, turn_id: &str, model: &str) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend(line(meta(thread_id, 0)));
        data.extend(line(event(
            1,
            "thread_settings_applied",
            serde_json::json!({"thread_settings":{"model":model,"reasoning_effort":"ultra"}}),
        )));
        data.extend(line(event(
            2,
            "task_started",
            serde_json::json!({"turn_id":turn_id,"started_at":1787558400_u64,"model_context_window":258400}),
        )));
        data.extend(line(context(3, turn_id, model, "ultra")));
        data
    }

    fn behavior_fixture(index: usize, output_bucket: &str) -> BehaviorSampleV2 {
        BehaviorSampleV2 {
            thread_id: format!("thread-behavior-{index}"),
            turn_id: format!("turn-behavior-{index}"),
            model: "gpt-5.6-sol".to_owned(),
            effort: "ultra".to_owned(),
            uncached_input_bucket: "8k-32k".to_owned(),
            output_bucket: output_bucket.to_owned(),
            tool_activity: false,
            output_tokens: 512,
            ttft_ms: Some(800),
            model_phase_output_rate: Some(40.0),
            reasoning_output_share: Some(0.5),
            reasoning_phase_share: Some(0.5),
            cache_input_share: Some(0.8),
            clean: true,
            explicit_reroute: false,
            observed_at: unix_ms_to_iso8601(1_787_558_400_000 + index as u64),
        }
    }

    #[test]
    fn incrementally_reads_half_lines_and_complete_token_usage() {
        let fixture = TestDirectory::new("incremental");
        let path = fixture.rollout("incremental");
        append(&path, &active_fixture("thread-a", "turn-a", "gpt-5.6-sol"));
        let token = line(event(
            4,
            "token_count",
            serde_json::json!({"info":{
                "total_token_usage":{"input_tokens":100,"cached_input_tokens":80,"cache_write_input_tokens":3,"output_tokens":20,"reasoning_output_tokens":7,"total_tokens":120},
                "last_token_usage":{"input_tokens":100,"cached_input_tokens":80,"cache_write_input_tokens":3,"output_tokens":20,"reasoning_output_tokens":7,"total_tokens":120},
                "model_context_window":258400
            }}),
        ));
        let split = token.len() / 2;
        append(&path, &token[..split]);

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let first = collector.scan_at(true, 1_787_558_410_000);
        assert_eq!(first.conversations.len(), 1);
        assert_eq!(first.conversations[0].status.code, "token_data_pending");

        append(&path, &token[split..]);
        let second = collector.scan_at(true, 1_787_558_411_000);
        let conversation = &second.conversations[0];
        assert_eq!(conversation.usage.cumulative.total_tokens, 120);
        assert_eq!(conversation.usage.cumulative.reasoning_output_tokens, 7);
        assert_eq!(conversation.usage.cache_input_share, Some(0.8));
        assert_eq!(conversation.status.level, StatusLevel::Green);
    }

    #[test]
    fn public_cumulative_is_exact_raw_total_while_completed_sample_is_turn_delta() {
        let fixture = TestDirectory::new("raw-total");
        let path = fixture.rollout("raw-total");
        append(&path, &line(meta("thread-raw-total", 0)));
        append(
            &path,
            &line(event(
                1,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{
                    "input_tokens":100,"cached_input_tokens":80,"output_tokens":20,
                    "reasoning_output_tokens":8,"total_tokens":120
                }}}),
            )),
        );
        append(
            &path,
            &line(event(
                2,
                "thread_settings_applied",
                serde_json::json!({"thread_settings":{"model":"gpt-5.6-sol","reasoning_effort":"ultra"}}),
            )),
        );
        append(
            &path,
            &line(event(
                3,
                "task_started",
                serde_json::json!({"turn_id":"turn-raw-total"}),
            )),
        );
        append(
            &path,
            &line(context(4, "turn-raw-total", "gpt-5.6-sol", "ultra")),
        );
        append(
            &path,
            &line(event(
                5,
                "token_count",
                serde_json::json!({"info":{
                    "total_token_usage":{
                        "input_tokens":150,"cached_input_tokens":120,"output_tokens":35,
                        "reasoning_output_tokens":14,"total_tokens":185
                    },
                    "last_token_usage":{
                        "input_tokens":50,"cached_input_tokens":40,"output_tokens":15,
                        "reasoning_output_tokens":6,"total_tokens":65
                    }
                }}),
            )),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_560_000);
        let usage = &snapshot.conversations[0].usage;
        assert_eq!(usage.cumulative.input_tokens, 150);
        assert_eq!(usage.cumulative.cached_input_tokens, 120);
        assert_eq!(usage.cumulative.output_tokens, 35);
        assert_eq!(usage.cumulative.reasoning_output_tokens, 14);
        assert_eq!(usage.cumulative.total_tokens, 185);
        assert_eq!(usage.last.total_tokens, 65);

        append(
            &path,
            &line(event(
                6,
                "task_complete",
                serde_json::json!({"turn_id":"turn-raw-total","duration_ms":1000,"time_to_first_token_ms":100}),
            )),
        );
        collector.scan_at(true, 1_787_558_561_000);
        let sample = collector
            .completed_turn_samples()
            .next()
            .expect("completed turn delta");
        assert_eq!(sample.input_tokens, 50);
        assert_eq!(sample.output_tokens, 15);
        assert_eq!(sample.reasoning_output_tokens, 6);
        assert_eq!(sample.cache_input_share, Some(0.8));
    }

    #[test]
    fn item_completed_keeps_only_sanitized_timing_and_estimates_live_ttft() {
        let fixture = TestDirectory::new("item-timing");
        let path = fixture.rollout("item-timing");
        append(
            &path,
            &active_fixture("thread-timing", "turn-timing", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(event(
                4,
                "item_completed",
                serde_json::json!({
                    "thread_id":"thread-timing",
                    "turn_id":"turn-timing",
                    "item":{"type":"Reasoning","id":"reasoning-1","summary_text":["PRIVATE_BODY_MUST_NOT_PERSIST"]},
                    "started_at_ms":1787558401000_u64,
                    "completed_at_ms":1787558402000_u64
                }),
            )),
        );
        append(
            &path,
            &line(event(
                5,
                "item_completed",
                serde_json::json!({
                    "thread_id":"thread-timing",
                    "turn_id":"turn-timing",
                    "item":{"type":"AgentMessage","id":"message-1","content":[{"text":"PRIVATE_BODY_MUST_NOT_PERSIST"}]},
                    "started_at_ms":1787558401500_u64,
                    "completed_at_ms":1787558402500_u64
                }),
            )),
        );
        append(
            &path,
            &line(event(
                6,
                "token_count",
                serde_json::json!({"info":{
                    "total_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":150,"reasoning_output_tokens":60,"total_tokens":1150},
                    "last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":150,"reasoning_output_tokens":60,"total_tokens":1150}
                }}),
            )),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_405_000);
        let timing = &snapshot.conversations[0].timing;
        assert_eq!(timing.ttft_evidence.kind, "estimatedWindow");
        assert_eq!(timing.ttft_evidence.lower_ms, Some(1_000));
        assert_eq!(timing.ttft_evidence.upper_ms, Some(2_000));
        assert_eq!(timing.model_active_ms, Some(1_500));
        assert_eq!(timing.end_to_end_output_rate, Some(30.0));
        assert_eq!(timing.model_phase_output_rate, Some(100.0));
        let cache = serde_json::to_string(&collector.export_cache()).expect("cache json");
        assert!(!cache.contains("PRIVATE_BODY_MUST_NOT_PERSIST"));
        assert!(cache.contains("reasoning-1"));
    }

    #[test]
    fn detects_pending_model_switch_then_applies_it_on_next_turn() {
        let fixture = TestDirectory::new("switch");
        let path = fixture.rollout("switch");
        append(
            &path,
            &active_fixture("thread-switch", "turn-terra", "gpt-5.6-terra"),
        );
        append(
            &path,
            &line(event(
                4,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":20,"output_tokens":5,"total_tokens":25},"last_token_usage":{"input_tokens":20,"output_tokens":5,"total_tokens":25}}}),
            )),
        );
        append(
            &path,
            &line(serde_json::json!({
                "timestamp":"2026-08-24T08:00:04.500Z",
                "type":"response_item",
                "ordinal":5,
                "payload":{"type":"function_call","name":"fixture_tool","arguments":"{}"}
            })),
        );
        append(
            &path,
            &line(event(
                6,
                "thread_settings_applied",
                serde_json::json!({"thread_settings":{"model":"gpt-5.6-sol","reasoning_effort":"ultra"}}),
            )),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let pending = collector.scan_at(true, 1_787_558_420_000);
        let conversation = &pending.conversations[0];
        assert_eq!(
            conversation.active_request.model.as_deref(),
            Some("gpt-5.6-terra")
        );
        assert_eq!(
            conversation
                .pending_next_turn
                .as_ref()
                .and_then(|value| value.model.as_deref()),
            Some("gpt-5.6-sol")
        );
        assert_eq!(conversation.status.code, "next_turn_pending");

        append(
            &path,
            &line(event(
                6,
                "task_complete",
                serde_json::json!({"turn_id":"turn-terra","duration_ms":2000,"time_to_first_token_ms":400}),
            )),
        );
        append(
            &path,
            &line(event(
                7,
                "task_started",
                serde_json::json!({"turn_id":"turn-sol","started_at":1787558421_u64}),
            )),
        );
        append(&path, &line(context(8, "turn-sol", "gpt-5.6-sol", "ultra")));
        append(
            &path,
            &line(event(
                9,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":40,"output_tokens":10,"total_tokens":50},"last_token_usage":{"input_tokens":20,"output_tokens":5,"total_tokens":25}}}),
            )),
        );
        let applied = collector.scan_at(true, 1_787_558_422_000);
        assert_eq!(applied.conversations[0].turn_id, "turn-sol");
        assert_eq!(
            applied.conversations[0].active_request.model.as_deref(),
            Some("gpt-5.6-sol")
        );
        assert!(applied.conversations[0].pending_next_turn.is_none());
    }

    #[test]
    fn only_explicit_reroute_is_server_route_evidence() {
        let fixture = TestDirectory::new("reroute");
        let path = fixture.rollout("reroute");
        append(
            &path,
            &active_fixture("thread-route", "turn-route", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(event(
                4,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":20,"output_tokens":4,"reasoning_output_tokens":3,"total_tokens":24},"last_token_usage":{"input_tokens":20,"output_tokens":4,"reasoning_output_tokens":3,"total_tokens":24}}}),
            )),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        let unknown = collector.scan_at(true, 1_787_558_430_000);
        assert_eq!(
            unknown.conversations[0].server_route.evidence,
            "notObserved"
        );
        assert_eq!(unknown.conversations[0].server_route.model, None);

        append(
            &path,
            &line(event(
                5,
                "model_reroute",
                serde_json::json!({"from_model":"gpt-5.6-sol","to_model":"gpt-5.6-terra","reason":"fixture"}),
            )),
        );
        let routed = collector.scan_at(true, 1_787_558_431_000);
        assert_eq!(
            routed.conversations[0].server_route.model.as_deref(),
            Some("gpt-5.6-terra")
        );
        assert_eq!(
            routed.conversations[0].server_route.evidence,
            "explicitReroute"
        );
        assert_eq!(routed.conversations[0].status.level, StatusLevel::Red);
    }

    #[test]
    fn accepts_valid_paginated_meta_and_isolates_subagent_parent_history() {
        let fixture = TestDirectory::new("pagination");
        let path = fixture.rollout("segment");
        let thread_id = "thread-subagent";
        append(
            &path,
            &line(serde_json::json!({
                "timestamp":"2026-08-24T08:00:00.000Z",
                "ordinal":50,
                "type":"session_meta",
                "payload":{
                    "id":thread_id,
                    "thread_source":"subagent",
                    "parent_thread_id":"thread-parent",
                    "subagent_history_start_ordinal":54,
                    "history_mode":"paginated",
                    "history_base":{"thread_id":thread_id,"end_ordinal_exclusive":50},
                    "agent_nickname":"Mochi"
                }
            })),
        );
        append(
            &path,
            &line(event(
                51,
                "task_started",
                serde_json::json!({"turn_id":"copied-parent"}),
            )),
        );
        append(
            &path,
            &line(context(52, "copied-parent", "wrong-model", "low")),
        );
        append(
            &path,
            &line(event(
                54,
                "thread_settings_applied",
                serde_json::json!({"thread_settings":{"model":"gpt-5.6-sol","reasoning_effort":"ultra"}}),
            )),
        );
        append(
            &path,
            &line(event(
                55,
                "task_started",
                serde_json::json!({"turn_id":"own-turn"}),
            )),
        );
        append(
            &path,
            &line(context(56, "own-turn", "gpt-5.6-sol", "ultra")),
        );
        append(
            &path,
            &line(event(
                57,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":8,"output_tokens":2,"total_tokens":10},"last_token_usage":{"input_tokens":8,"output_tokens":2,"total_tokens":10}}}),
            )),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_440_000);
        assert_eq!(snapshot.conversations.len(), 1);
        assert_eq!(snapshot.conversations[0].turn_id, "own-turn");
        assert_eq!(snapshot.conversations[0].kind, ThreadKind::Subagent);
        assert_eq!(
            snapshot.conversations[0].parent_thread_id.as_deref(),
            Some("thread-parent")
        );
    }

    #[test]
    fn accepts_paginated_continuation_from_another_history_thread() {
        let fixture = TestDirectory::new("cross-thread-pagination");
        let path = fixture.rollout("cross-thread-root");
        append(
            &path,
            &line(serde_json::json!({
                "timestamp":"2026-08-24T08:00:00.000Z",
                "ordinal":9001,
                "type":"session_meta",
                "payload":{
                    "id":"current-root-thread",
                    "thread_source":"user",
                    "history_mode":"paginated",
                    "history_base":{"thread_id":"ancestor-history-thread","end_ordinal_exclusive":9001}
                }
            })),
        );
        append(
            &path,
            &line(event(
                9002,
                "thread_settings_applied",
                serde_json::json!({"thread_settings":{"model":"gpt-5.6-sol","reasoning_effort":"ultra"}}),
            )),
        );
        append(
            &path,
            &line(event(
                9003,
                "task_started",
                serde_json::json!({"turn_id":"current-root-turn"}),
            )),
        );
        append(
            &path,
            &line(context(9004, "current-root-turn", "gpt-5.6-sol", "ultra")),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_450_000);
        assert_eq!(snapshot.collector_health.level, StatusLevel::Green);
        assert_eq!(snapshot.conversations.len(), 1);
        assert_eq!(snapshot.conversations[0].thread_id, "current-root-thread");
        assert_eq!(snapshot.conversations[0].turn_id, "current-root-turn");
        assert_eq!(snapshot.conversations[0].kind, ThreadKind::Root);
    }

    #[test]
    fn malformed_newer_segment_suppresses_stale_older_active_state() {
        let fixture = TestDirectory::new("bad-pagination");
        let older = fixture.rollout("old-segment");
        append(
            &older,
            &active_fixture("same-thread", "old-turn", "gpt-5.6-sol"),
        );
        append(
            &older,
            &line(event(
                4,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
            )),
        );
        let newer = fixture.rollout("new-segment");
        append(
            &newer,
            &line(serde_json::json!({
                "timestamp":"2026-08-24T08:00:00.000Z",
                "ordinal":100,
                "type":"session_meta",
                "payload":{
                    "id":"same-thread",
                    "history_mode":"paginated",
                    "history_base":{"thread_id":"different-thread","end_ordinal_exclusive":99}
                }
            })),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_450_000);
        assert!(snapshot.conversations.is_empty());
        assert_eq!(snapshot.collector_health.level, StatusLevel::Yellow);
    }

    #[test]
    fn mismatched_terminal_does_not_end_the_active_turn() {
        let fixture = TestDirectory::new("terminal");
        let path = fixture.rollout("terminal");
        append(
            &path,
            &active_fixture("thread-terminal", "active-turn", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(event(
                4,
                "task_complete",
                serde_json::json!({"turn_id":"different-turn","duration_ms":10}),
            )),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        assert_eq!(
            collector
                .scan_at(true, 1_787_558_460_000)
                .conversations
                .len(),
            1
        );

        append(
            &path,
            &line(event(
                5,
                "turn_aborted",
                serde_json::json!({"turn_id":"active-turn"}),
            )),
        );
        assert!(collector
            .scan_at(true, 1_787_558_461_000)
            .conversations
            .is_empty());
    }

    #[test]
    fn exposes_a_sanitized_completed_turn_sample_for_behavior_storage() {
        let fixture = TestDirectory::new("completed-sample");
        let path = fixture.rollout("completed-sample");
        append(
            &path,
            &active_fixture("thread-sample", "turn-sample", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(event(
                4,
                "token_count",
                serde_json::json!({"info":{
                    "total_token_usage":{"input_tokens":40000,"cached_input_tokens":30000,"output_tokens":100,"reasoning_output_tokens":40,"total_tokens":40100},
                    "last_token_usage":{"input_tokens":40000,"cached_input_tokens":30000,"output_tokens":100,"reasoning_output_tokens":40,"total_tokens":40100}
                }}),
            )),
        );
        append(
            &path,
            &line(serde_json::json!({
                "timestamp":"2026-08-24T08:00:04.500Z",
                "type":"response_item",
                "ordinal":5,
                "payload":{"type":"function_call","name":"fixture_tool","arguments":"{}"}
            })),
        );
        append(
            &path,
            &line(event(
                6,
                "task_complete",
                serde_json::json!({"turn_id":"turn-sample","duration_ms":5000,"time_to_first_token_ms":600}),
            )),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        collector.scan_at(true, 1_787_558_465_000);
        let sample = collector
            .completed_turn_samples()
            .next()
            .expect("completed sample");
        assert_eq!(sample.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(sample.effort.as_deref(), Some("ultra"));
        assert_eq!(sample.input_bucket, "32k-128k");
        assert!(sample.tool_activity);
        assert_eq!(sample.ttft_ms, Some(600));
        assert_eq!(sample.duration_ms, Some(5000));
        assert_eq!(sample.output_tokens, 100);
        assert_eq!(sample.reasoning_output_tokens, 40);
        assert_eq!(sample.cache_input_share, Some(0.75));
        let behavior = collector
            .completed_behavior_samples_v2()
            .next()
            .expect("v2 behavior sample");
        assert_eq!(behavior.uncached_input_bucket, "8k-32k");
        assert_eq!(behavior.output_bucket, "0-256");
        assert_eq!(behavior.ttft_ms, Some(600));
        assert!(behavior.clean);
        assert!(!behavior.explicit_reroute);
    }

    #[test]
    fn hook_context_conflict_is_red_but_never_overwrites_the_context_model() {
        let fixture = TestDirectory::new("hook-conflict");
        let path = fixture.rollout("hook-conflict");
        append(
            &path,
            &active_fixture("thread-hook", "turn-hook", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(event(
                4,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}),
            )),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        collector.observe_hook(HookObservation {
            thread_id: "thread-hook".to_owned(),
            turn_id: Some("turn-hook".to_owned()),
            model: Some("gpt-5.6-terra".to_owned()),
            observed_at: "2026-08-24T08:00:04.000Z".to_owned(),
        });
        let snapshot = collector.scan_at(true, 1_787_558_470_000);
        assert_eq!(
            snapshot.conversations[0].active_request.model.as_deref(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            snapshot.conversations[0].status.code,
            "request_evidence_conflict"
        );
    }

    #[test]
    fn preserves_utf8_when_a_multibyte_character_crosses_scans() {
        let fixture = TestDirectory::new("utf8-split");
        let path = fixture.rollout("utf8-split");
        let meta_line = line(serde_json::json!({
            "timestamp":"2026-08-24T08:00:00.000Z",
            "ordinal":0,
            "type":"session_meta",
            "payload":{"id":"thread-utf8","cwd":"C:\\fixture\\糯米","thread_source":"user"}
        }));
        let first_non_ascii = meta_line
            .iter()
            .position(|byte| *byte >= 0x80)
            .expect("fixture contains UTF-8");
        append(&path, &meta_line[..first_non_ascii + 1]);

        let mut collector = RolloutCollector::new(&fixture.0, None);
        assert!(collector
            .scan_at(true, 1_787_558_490_000)
            .conversations
            .is_empty());

        append(&path, &meta_line[first_non_ascii + 1..]);
        append(
            &path,
            &line(event(
                1,
                "thread_settings_applied",
                serde_json::json!({"thread_settings":{"model":"gpt-5.6-sol","reasoning_effort":"ultra"}}),
            )),
        );
        append(
            &path,
            &line(event(
                2,
                "task_started",
                serde_json::json!({"turn_id":"turn-utf8"}),
            )),
        );
        append(
            &path,
            &line(context(3, "turn-utf8", "gpt-5.6-sol", "ultra")),
        );
        append(
            &path,
            &line(event(
                4,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":3,"output_tokens":1,"total_tokens":4}}}),
            )),
        );
        let snapshot = collector.scan_at(true, 1_787_558_491_000);
        assert_eq!(snapshot.conversations.len(), 1);
        assert_eq!(snapshot.conversations[0].title, "糯米");
        assert_eq!(snapshot.collector_health.parse_warnings, 0);
    }

    #[test]
    fn scans_long_rollout_without_tail_window_guessing() {
        let fixture = TestDirectory::new("long-rollout");
        let path = fixture.rollout("long-rollout");
        append(
            &path,
            &active_fixture("thread-long", "turn-long", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(serde_json::json!({
                "timestamp":"2026-08-24T08:00:04.000Z",
                "ordinal":4,
                "type":"response_item",
                "payload":{"type":"message","content":"x".repeat(2_200_000)}
            })),
        );
        append(
            &path,
            &line(event(
                5,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}),
            )),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_500_000);
        assert!(fs::metadata(path).unwrap().len() > 2_000_000);
        assert_eq!(
            snapshot.conversations[0].active_request.model.as_deref(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(snapshot.conversations[0].usage.cumulative.total_tokens, 12);
    }

    #[test]
    fn bad_relevant_json_warns_but_does_not_block_later_events() {
        let fixture = TestDirectory::new("bad-json");
        let path = fixture.rollout("bad-json");
        append(
            &path,
            &active_fixture("thread-bad-json", "turn-bad-json", "gpt-5.6-sol"),
        );
        append(
            &path,
            b"{\"ordinal\":4,\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\"\n",
        );
        append(
            &path,
            &line(event(
                5,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":9,"output_tokens":2,"total_tokens":11}}}),
            )),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_510_000);
        assert_eq!(snapshot.conversations[0].usage.cumulative.total_tokens, 11);
        assert_eq!(
            snapshot.conversations[0].status.code,
            "collector_parse_warning"
        );
        assert_eq!(snapshot.collector_health.parse_warnings, 1);
    }

    #[test]
    fn truncation_or_replacement_resets_identity_and_deletion_removes_state() {
        let fixture = TestDirectory::new("replace-delete");
        let path = fixture.rollout("replace-delete");
        let mut old = active_fixture(
            "thread-old-long-name",
            "turn-old-long-name",
            "gpt-5.6-terra",
        );
        old.extend(line(event(
            4,
            "token_count",
            serde_json::json!({"info":{"total_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}),
        )));
        append(&path, &old);
        let mut collector = RolloutCollector::new(&fixture.0, None);
        assert_eq!(
            collector.scan_at(true, 1_787_558_520_000).conversations[0].thread_id,
            "thread-old-long-name"
        );

        let mut replacement = active_fixture("new", "turn-new", "gpt-5.6-sol");
        replacement.extend(line(event(
            4,
            "token_count",
            serde_json::json!({"info":{"total_token_usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}),
        )));
        fs::write(&path, replacement).expect("replace fixture rollout");
        let replaced = collector.scan_at(true, 1_787_558_521_000);
        assert_eq!(replaced.conversations.len(), 1);
        assert_eq!(replaced.conversations[0].thread_id, "new");
        assert_eq!(
            replaced.conversations[0].active_request.model.as_deref(),
            Some("gpt-5.6-sol")
        );

        fs::remove_file(&path).expect("delete fixture rollout");
        assert!(collector
            .scan_at(true, 1_787_558_522_000)
            .conversations
            .is_empty());
    }

    #[test]
    fn unknown_effort_is_yellow_not_green() {
        let fixture = TestDirectory::new("unknown-effort");
        let path = fixture.rollout("unknown-effort");
        append(
            &path,
            &active_fixture("thread-effort", "turn-effort", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(context(4, "turn-effort", "gpt-5.6-sol", "mystery")),
        );
        append(
            &path,
            &line(event(
                5,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}),
            )),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_530_000);
        assert_eq!(snapshot.conversations[0].status.level, StatusLevel::Yellow);
        assert_eq!(
            snapshot.conversations[0].status.code,
            "request_evidence_incomplete"
        );
    }

    #[test]
    fn continuation_clears_prior_request_route_and_chain() {
        let fixture = TestDirectory::new("continuation-route");
        let path = fixture.rollout("continuation-route");
        append(
            &path,
            &active_fixture("thread-continuation", "turn-continuation", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(event(
                4,
                "model_reroute",
                serde_json::json!({"from_model":"gpt-5.6-sol","to_model":"gpt-5.6-terra"}),
            )),
        );
        append(
            &path,
            &line(context(5, "turn-continuation", "gpt-5.6-sol", "ultra")),
        );
        append(
            &path,
            &line(event(
                6,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}),
            )),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_540_000);
        assert_eq!(snapshot.conversations[0].server_route.model, None);
        assert_eq!(
            snapshot.conversations[0].server_route.evidence,
            "notObserved"
        );
        assert!(snapshot.conversations[0].server_route.chain.is_empty());
    }

    #[test]
    fn continuation_never_reuses_a_live_reroute_from_the_previous_request() {
        let fixture = TestDirectory::new("continuation-live-route");
        let path = fixture.rollout("continuation-live-route");
        append(
            &path,
            &active_fixture(
                "thread-live-continuation",
                "turn-live-continuation",
                "gpt-5.6-sol",
            ),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let _ = collector.scan_at(true, 1_787_558_406_000);
        collector.observe_server_reroute(ModelReroutedObservation {
            thread_id: "thread-live-continuation".to_owned(),
            turn_id: "turn-live-continuation".to_owned(),
            from_model: "gpt-5.6-sol".to_owned(),
            to_model: "gpt-5.6-terra".to_owned(),
            reason: Some("fixture".to_owned()),
            observed_at: "2026-08-24T08:00:06.000Z".to_owned(),
        });
        let routed = collector.scan_at(true, 1_787_558_406_000);
        assert_eq!(
            routed.conversations[0].server_route.model.as_deref(),
            Some("gpt-5.6-terra")
        );
        assert_eq!(
            routed.conversations[0].server_route.evidence,
            "explicitReroute"
        );

        append(
            &path,
            &line(serde_json::json!({
                "timestamp": "2026-08-24T08:00:07.000Z",
                "ordinal": 4,
                "type": "turn_context",
                "payload": {
                    "turn_id": "turn-live-continuation",
                    "model": "gpt-5.6-sol",
                    "effort": "ultra"
                }
            })),
        );
        let continued = collector.scan_at(true, 1_787_558_407_000);
        assert_eq!(continued.conversations[0].server_route.model, None);
        assert_eq!(
            continued.conversations[0].server_route.evidence,
            "notObserved"
        );

        collector.observe_server_reroute(ModelReroutedObservation {
            thread_id: "thread-live-continuation".to_owned(),
            turn_id: "turn-live-continuation".to_owned(),
            from_model: "gpt-5.6-sol".to_owned(),
            to_model: "gpt-5.5".to_owned(),
            reason: Some("fixture-new-request".to_owned()),
            observed_at: "2026-08-24T08:00:08.000Z".to_owned(),
        });
        let rerouted_again = collector.scan_at(true, 1_787_558_408_000);
        assert_eq!(
            rerouted_again.conversations[0]
                .server_route
                .model
                .as_deref(),
            Some("gpt-5.5")
        );
    }

    #[test]
    fn live_observation_capacity_preserves_active_turns_without_clearing_the_table() {
        let fixture = TestDirectory::new("live-observation-capacity");
        let path = fixture.rollout("live-observation-capacity");
        let active_key = (
            "thread-live-capacity".to_owned(),
            "turn-live-capacity".to_owned(),
        );
        append(
            &path,
            &active_fixture(&active_key.0, &active_key.1, "gpt-5.6-sol"),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        let _ = collector.scan_at(true, 1_787_558_406_000);
        collector.observe_hook(HookObservation {
            thread_id: active_key.0.clone(),
            turn_id: Some(active_key.1.clone()),
            model: Some("gpt-5.6-sol".to_owned()),
            observed_at: "2026-08-24T08:00:06.000Z".to_owned(),
        });
        collector.observe_server_reroute(ModelReroutedObservation {
            thread_id: active_key.0.clone(),
            turn_id: active_key.1.clone(),
            from_model: "gpt-5.6-sol".to_owned(),
            to_model: "gpt-5.6-terra".to_owned(),
            reason: None,
            observed_at: "2026-08-24T08:00:06.000Z".to_owned(),
        });

        for index in 0..(MAX_LIVE_OBSERVATION_KEYS + 32) {
            let observed_at = unix_ms_to_iso8601(1_787_558_500_000 + index as u64);
            collector.observe_hook(HookObservation {
                thread_id: format!("inactive-hook-{index}"),
                turn_id: Some(format!("turn-{index}")),
                model: Some("gpt-5.6-sol".to_owned()),
                observed_at: observed_at.clone(),
            });
            collector.observe_server_reroute(ModelReroutedObservation {
                thread_id: format!("inactive-route-{index}"),
                turn_id: format!("turn-{index}"),
                from_model: "gpt-5.6-sol".to_owned(),
                to_model: "gpt-5.5".to_owned(),
                reason: None,
                observed_at,
            });
        }

        assert_eq!(collector.hook_observations.len(), MAX_LIVE_OBSERVATION_KEYS);
        assert_eq!(collector.live_reroutes.len(), MAX_LIVE_OBSERVATION_KEYS);
        assert!(collector.hook_observations.contains_key(&active_key));
        assert!(collector.live_reroutes.contains_key(&active_key));
        assert!(collector.hook_observations.len() > 1);
        assert!(collector.live_reroutes.len() > 1);
    }

    #[test]
    fn live_reroute_history_is_bounded_and_replayed_without_duplicates() {
        let fixture = TestDirectory::new("live-reroute-chain");
        let path = fixture.rollout("live-reroute-chain");
        append(
            &path,
            &active_fixture("thread-live-chain", "turn-live-chain", "gpt-5.6-sol"),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        let _ = collector.scan_at(true, 1_787_558_406_000);

        for index in 0..(MAX_ROUTE_HOPS_PER_TURN + 4) {
            collector.observe_server_reroute(ModelReroutedObservation {
                thread_id: "thread-live-chain".to_owned(),
                turn_id: "turn-live-chain".to_owned(),
                from_model: if index == 0 {
                    "gpt-5.6-sol".to_owned()
                } else {
                    format!("route-{}", index - 1)
                },
                to_model: format!("route-{index}"),
                reason: Some(format!("hop-{index}")),
                observed_at: unix_ms_to_iso8601(1_787_558_406_000 + index as u64),
            });
        }

        let first = collector.scan_at(true, 1_787_558_407_000);
        let route = &first.conversations[0].server_route;
        assert_eq!(route.chain.len(), MAX_ROUTE_HOPS_PER_TURN);
        assert_eq!(route.chain[0].to_model, "route-4");
        assert_eq!(
            route.chain.last().map(|hop| hop.to_model.as_str()),
            Some("route-19")
        );
        assert_eq!(route.model.as_deref(), Some("route-19"));

        let replayed = collector.scan_at(true, 1_787_558_408_000);
        assert_eq!(
            replayed.conversations[0].server_route.chain,
            first.conversations[0].server_route.chain
        );
    }

    #[test]
    fn live_reroute_marks_a_terminal_sample_ineligible_for_quality_baselines() {
        let fixture = TestDirectory::new("terminal-live-route");
        let path = fixture.rollout("terminal-live-route");
        append(
            &path,
            &active_fixture("thread-live-terminal", "turn-live-terminal", "gpt-5.6-sol"),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        let _ = collector.scan_at(true, 1_787_558_406_000);
        collector.observe_server_reroute(ModelReroutedObservation {
            thread_id: "thread-live-terminal".to_owned(),
            turn_id: "turn-live-terminal".to_owned(),
            from_model: "gpt-5.6-sol".to_owned(),
            to_model: "gpt-5.5".to_owned(),
            reason: Some("fixture".to_owned()),
            observed_at: "2026-08-24T08:00:06.000Z".to_owned(),
        });
        append(
            &path,
            &line(event(
                7,
                "item_completed",
                serde_json::json!({
                    "item":{"type":"Reasoning","id":"reasoning-live-route"},
                    "started_at_ms":1787558405000_u64,
                    "completed_at_ms":1787558407000_u64
                }),
            )),
        );
        append(
            &path,
            &line(event(
                8,
                "token_count",
                serde_json::json!({"info":{
                    "total_token_usage":{"input_tokens":1000,"cached_input_tokens":500,"output_tokens":200,"reasoning_output_tokens":100,"total_tokens":1200},
                    "last_token_usage":{"input_tokens":1000,"cached_input_tokens":500,"output_tokens":200,"reasoning_output_tokens":100,"total_tokens":1200}
                }}),
            )),
        );
        append(
            &path,
            &line(event(
                9,
                "task_complete",
                serde_json::json!({
                    "turn_id":"turn-live-terminal",
                    "time_to_first_token_ms":900,
                    "duration_ms":4000
                }),
            )),
        );

        let _ = collector.scan_at(true, 1_787_558_410_000);
        let sample = collector
            .completed_behavior_samples_v2()
            .find(|sample| sample.turn_id == "turn-live-terminal")
            .expect("completed live-rerouted sample");
        assert!(sample.clean);
        assert!(sample.explicit_reroute);
        assert!(!crate::metrics::eligible_baseline_sample(sample));
        let mut without_route = sample.clone();
        without_route.explicit_reroute = false;
        assert!(crate::metrics::eligible_baseline_sample(&without_route));
    }

    #[test]
    fn historical_parse_warning_does_not_poison_a_later_clean_turn_sample() {
        let fixture = TestDirectory::new("turn-local-warning");
        let path = fixture.rollout("turn-local-warning");
        append(&path, &line(meta("thread-turn-local-warning", 0)));
        append(
            &path,
            b"{\"type\":\"turn_context\",\"payload\":PRIVATE_BROKEN_JSON\n",
        );
        append(
            &path,
            &line(event(
                1,
                "thread_settings_applied",
                serde_json::json!({"thread_settings":{"model":"gpt-5.6-sol","reasoning_effort":"ultra"}}),
            )),
        );
        append(
            &path,
            &line(event(
                2,
                "task_started",
                serde_json::json!({"turn_id":"turn-clean-after-warning","started_at":1787558400_u64}),
            )),
        );
        append(
            &path,
            &line(context(
                3,
                "turn-clean-after-warning",
                "gpt-5.6-sol",
                "ultra",
            )),
        );
        append(
            &path,
            &line(event(
                6,
                "item_completed",
                serde_json::json!({
                    "item":{"type":"Reasoning","id":"reasoning-clean"},
                    "started_at_ms":1787558405000_u64,
                    "completed_at_ms":1787558407000_u64
                }),
            )),
        );
        append(
            &path,
            &line(event(
                7,
                "token_count",
                serde_json::json!({"info":{
                    "total_token_usage":{"input_tokens":1000,"cached_input_tokens":500,"output_tokens":200,"reasoning_output_tokens":100,"total_tokens":1200},
                    "last_token_usage":{"input_tokens":1000,"cached_input_tokens":500,"output_tokens":200,"reasoning_output_tokens":100,"total_tokens":1200}
                }}),
            )),
        );
        append(
            &path,
            &line(event(
                8,
                "task_complete",
                serde_json::json!({
                    "turn_id":"turn-clean-after-warning",
                    "time_to_first_token_ms":900,
                    "duration_ms":4000
                }),
            )),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        let snapshot = collector.scan_at(true, 1_787_558_410_000);
        assert_eq!(snapshot.collector_health.parse_warnings, 1);
        let sample = collector
            .completed_behavior_samples_v2()
            .find(|sample| sample.turn_id == "turn-clean-after-warning")
            .expect("clean later sample");
        assert!(sample.clean);
        assert!(crate::metrics::eligible_baseline_sample(sample));
        let settled = collector.scan_at(true, 1_787_558_411_000);
        assert_eq!(settled.collector_health.parse_warnings, 0);
        assert_eq!(settled.collector_health.level, StatusLevel::Green);
    }

    #[test]
    fn process_start_filter_removes_crash_leftover_active_turns() {
        let fixture = TestDirectory::new("stale-active");
        let path = fixture.rollout("stale-active");
        append(
            &path,
            &active_fixture("thread-stale", "turn-stale", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(event(
                4,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}),
            )),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        let unfiltered = collector.scan_at(true, 1_787_558_500_000);
        assert_eq!(unfiltered.conversations.len(), 1);

        let filtered = collector.scan_at_with_runtime(true, 1_787_558_500_000, Some(1_787_558_460));
        assert!(filtered.conversations.is_empty());
    }

    #[test]
    fn collector_cache_is_versioned_and_resumes_incrementally() {
        let fixture = TestDirectory::new("cache");
        let path = fixture.rollout("cache");
        append(
            &path,
            &active_fixture("thread-cache", "turn-cache", "gpt-5.6-sol"),
        );
        append(
            &path,
            &line(event(
                4,
                "token_count",
                serde_json::json!({"info":{"total_token_usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}),
            )),
        );
        let mut first = RolloutCollector::new(&fixture.0, None);
        first.scan_at(true, 1_787_558_550_000);
        let cache = first.export_cache();

        let mut resumed = RolloutCollector::new(&fixture.0, None);
        assert!(resumed.restore_cache(cache));
        let snapshot = resumed.scan_at(true, 1_787_558_551_000);
        assert_eq!(snapshot.conversations.len(), 1);
        assert_eq!(snapshot.conversations[0].usage.cumulative.total_tokens, 3);

        let invalid = CollectorCache {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            cache_format_version: COLLECTOR_CACHE_FORMAT_VERSION + 1,
            files: Vec::new(),
        };
        assert!(!resumed.restore_cache(invalid));
    }

    #[test]
    fn persisted_relative_paths_are_normalized_and_cannot_escape_sessions_root() {
        let fixture = TestDirectory::new("relative-cache-path");
        let rollout = fixture.rollout("relative-cache-path");
        let relative = normalized_relative_path(&fixture.0, &rollout).expect("relative path");
        assert_eq!(relative, "2026/08/24/rollout-relative-cache-path.jsonl");
        assert_eq!(
            safe_cached_path(&fixture.0, &relative).as_deref(),
            Some(rollout.as_path())
        );
        assert!(safe_cached_path(&fixture.0, "../outside/rollout-private.jsonl").is_none());
        assert!(safe_cached_path(&fixture.0, "/absolute/rollout-private.jsonl").is_none());
        assert!(safe_cached_path(&fixture.0, "nested\\..\\rollout-private.jsonl").is_none());
        assert!(
            normalized_relative_path(&fixture.0, &fixture.0.join("..").join("outside")).is_none()
        );
    }

    #[test]
    fn persisted_cache_never_writes_paths_cwd_agent_path_or_partial_json() {
        const PRIVATE_PATH: &str = "PRIVATE_ROLLOUT_PATH_MUST_NOT_PERSIST";
        const PRIVATE_CWD: &str = "PRIVATE_CWD_MUST_NOT_PERSIST";
        const PRIVATE_AGENT_PATH: &str = "PRIVATE_AGENT_PATH_MUST_NOT_PERSIST";
        const PRIVATE_PARTIAL: &str = "PRIVATE_PROMPT_REPLY_MUST_NOT_PERSIST";
        const PRIVATE_REASON: &str = "PRIVATE_REROUTE_REASON_MUST_NOT_PERSIST";

        let fixture = TestDirectory::new(PRIVATE_PATH);
        let path = fixture.rollout("privacy");
        let mut complete = Vec::new();
        complete.extend(line(serde_json::json!({
            "timestamp": "2026-08-24T08:00:00.000Z",
            "ordinal": 0,
            "type": "session_meta",
            "payload": {
                "id": "thread-cache-privacy",
                "cwd": format!("C:\\{PRIVATE_CWD}\\repo"),
                "agent_path": format!("C:\\{PRIVATE_AGENT_PATH}\\agent.toml"),
                "thread_source": "subagent"
            }
        })));
        complete.extend(line(event(
            1,
            "thread_settings_applied",
            serde_json::json!({"thread_settings":{
                "model":"gpt-5.6-sol",
                "reasoning_effort":"ultra",
                "cwd":format!("C:\\{PRIVATE_CWD}\\repo")
            }}),
        )));
        complete.extend(line(event(
            2,
            "task_started",
            serde_json::json!({"turn_id":"turn-cache-privacy"}),
        )));
        complete.extend(line(context(
            3,
            "turn-cache-privacy",
            "gpt-5.6-sol",
            "ultra",
        )));
        complete.extend(line(event(
            4,
            "model_reroute",
            serde_json::json!({
                "from_model":"gpt-5.6-sol",
                "to_model":"gpt-5.5",
                "reason":PRIVATE_REASON
            }),
        )));
        append(&path, &complete);
        let durable_length = complete.len() as u64;
        append(
            &path,
            format!(
                "{{\"timestamp\":\"2026-08-24T08:00:05.000Z\",\"ordinal\":5,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"content\":\"{PRIVATE_PARTIAL}"
            )
            .as_bytes(),
        );

        let mut collector = RolloutCollector::new(&fixture.0, None);
        collector.scan_at(true, 1_787_558_555_000);
        let runtime = collector.file_states().next().expect("runtime file state");
        assert!(runtime
            .carry_bytes
            .windows(PRIVATE_PARTIAL.len())
            .any(|window| window == PRIVATE_PARTIAL.as_bytes()));
        assert_eq!(runtime.durable_offset, durable_length);
        assert!(runtime.offset > runtime.durable_offset);

        let cache = collector.export_cache();
        assert_eq!(cache.files.len(), 1);
        assert_eq!(cache.files[0].cursor.offset, durable_length);
        let json = serde_json::to_string(&cache).expect("serialize sanitized cache");
        for forbidden in [
            PRIVATE_PATH,
            PRIVATE_CWD,
            PRIVATE_AGENT_PATH,
            PRIVATE_PARTIAL,
            PRIVATE_REASON,
            "\"path\"",
            "\"cwd\"",
            "agentPath",
            "carryBytes",
        ] {
            assert!(!json.contains(forbidden), "cache leaked {forbidden}");
        }

        let state_root = fixture.0.join("derived-state");
        let persistence = Persistence::open(&state_root).expect("open derived state");
        persistence
            .save_collector_cache(&cache, "2026-08-24T08:00:05.000Z")
            .expect("persist sanitized cache");
        let stored = persistence
            .load_collector_cache_json()
            .expect("read persisted cache")
            .expect("collector cache row");
        assert_eq!(stored, json);
        for entry in fs::read_dir(&state_root).expect("read state directory") {
            let entry = entry.expect("state entry");
            if !entry.file_type().expect("state entry type").is_file() {
                continue;
            }
            let bytes = fs::read(entry.path()).expect("read state file");
            for forbidden in [
                PRIVATE_PATH,
                PRIVATE_CWD,
                PRIVATE_AGENT_PATH,
                PRIVATE_PARTIAL,
                PRIVATE_REASON,
            ] {
                assert!(
                    !bytes
                        .windows(forbidden.len())
                        .any(|window| window == forbidden.as_bytes()),
                    "on-disk cache leaked {forbidden}"
                );
            }
        }
    }

    #[test]
    fn restart_rewinds_to_complete_newline_and_replays_a_partial_token_event() {
        let fixture = TestDirectory::new("cache-partial-restart");
        let path = fixture.rollout("cache-partial-restart");
        let complete = active_fixture("thread-cache-partial", "turn-cache-partial", "gpt-5.6-sol");
        append(&path, &complete);
        let token = line(event(
            4,
            "token_count",
            serde_json::json!({"info":{
                "total_token_usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":42,"reasoning_output_tokens":12,"total_tokens":142},
                "last_token_usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":42,"reasoning_output_tokens":12,"total_tokens":142}
            }}),
        ));
        let split = token.len() / 2;
        append(&path, &token[..split]);

        let mut first = RolloutCollector::new(&fixture.0, None);
        let pending = first.scan_at(true, 1_787_558_556_000);
        assert_eq!(pending.conversations[0].status.code, "token_data_pending");
        let runtime = first.file_states().next().expect("runtime file state");
        assert_eq!(runtime.offset, complete.len() as u64 + split as u64);
        assert_eq!(runtime.durable_offset, complete.len() as u64);
        let cache = first.export_cache();
        assert_eq!(cache.files[0].cursor.offset, complete.len() as u64);

        let mut resumed = RolloutCollector::new(&fixture.0, None);
        assert!(resumed.restore_cache(cache));
        append(&path, &token[split..]);
        let completed = resumed.scan_at(true, 1_787_558_557_000);
        let conversation = &completed.conversations[0];
        assert_eq!(conversation.usage.cumulative.output_tokens, 42);
        assert_eq!(conversation.usage.cumulative.reasoning_output_tokens, 12);
        assert_eq!(conversation.usage.cumulative.total_tokens, 142);
        assert_eq!(conversation.status.level, StatusLevel::Green);
        assert!(resumed
            .file_states()
            .next()
            .expect("resumed file state")
            .carry_bytes
            .is_empty());
    }

    #[test]
    fn session_start_hook_without_turn_or_model_is_not_request_evidence() {
        let fixture = TestDirectory::new("session-start-hook");
        let path = fixture.rollout("session-start-hook");
        append(
            &path,
            &active_fixture("thread-session-start", "turn-real", "gpt-5.6-sol"),
        );
        let mut collector = RolloutCollector::new(&fixture.0, None);
        collector.observe_hook(HookObservation {
            thread_id: "thread-session-start".to_owned(),
            turn_id: None,
            model: None,
            observed_at: "2026-08-24T08:00:00.000Z".to_owned(),
        });
        let snapshot = collector.scan_at(true, 1_787_558_480_000);
        assert_eq!(
            snapshot.conversations[0].active_request.model.as_deref(),
            Some("gpt-5.6-sol")
        );
        assert_ne!(
            snapshot.conversations[0].status.code,
            "request_evidence_conflict"
        );
    }

    #[test]
    fn timestamp_formatter_is_stable_and_utc() {
        assert_eq!(unix_ms_to_iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            unix_ms_to_iso8601(1_787_558_400_123),
            "2026-08-24T08:00:00.123Z"
        );
    }

    #[test]
    fn rebuilt_behavior_history_keeps_the_latest_hundred_per_baseline_globally() {
        let fixture = TestDirectory::new("behavior-per-baseline");
        let mut first = FileState::new(fixture.0.join("first.jsonl"));
        let mut second = FileState::new(fixture.0.join("second.jsonl"));

        // The old per-file/whole-vector cap would either keep 160 samples for
        // bucket A globally or evict bucket B merely because A was newer.
        first
            .completed_behavior_samples_v2
            .extend((0..80).map(|index| behavior_fixture(index, "257-1024")));
        first
            .completed_behavior_samples_v2
            .extend((1_000..1_080).map(|index| behavior_fixture(index, "1025-4096")));
        second
            .completed_behavior_samples_v2
            .extend((80..160).map(|index| behavior_fixture(index, "257-1024")));

        prune_file_behavior_samples_per_baseline(&mut first);
        assert_eq!(first.completed_behavior_samples_v2.len(), 160);

        let mut files = HashMap::from([(first.path.clone(), first), (second.path.clone(), second)]);
        prune_behavior_samples_per_baseline(&mut files);

        let all = files
            .values()
            .flat_map(|state| state.completed_behavior_samples_v2.iter())
            .collect::<Vec<_>>();
        let bucket_a = all
            .iter()
            .filter(|sample| sample.output_bucket == "257-1024")
            .copied()
            .collect::<Vec<_>>();
        let bucket_b = all
            .iter()
            .filter(|sample| sample.output_bucket == "1025-4096")
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(bucket_a.len(), MAX_BEHAVIOR_SAMPLES_PER_BASELINE);
        assert_eq!(bucket_b.len(), 80);
        assert!(bucket_a.iter().all(|sample| {
            sample
                .thread_id
                .trim_start_matches("thread-behavior-")
                .parse::<usize>()
                .is_ok_and(|index| index >= 60)
        }));
    }
}
