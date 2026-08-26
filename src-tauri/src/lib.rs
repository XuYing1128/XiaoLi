pub mod app;
pub mod audit_manager;
pub mod collector;
pub mod community_baseline;
pub mod connection;
pub mod credentials;
pub mod history;
pub mod ipc;
pub mod metrics;
pub mod model;
pub mod persistence;
pub mod private_probe_pack;
pub mod relay_audit;
pub mod relay_baseline;
pub mod relay_transport;
pub mod runtime;
pub mod selective_service;

use atomicwrites::{AllowOverwrite, AtomicFile};
use chrono::Utc;
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

const PLUGIN_NAME: &str = "xiaoli-model-monitor";
const PLUGIN_VERSION: &str = "0.2.0-beta.1";
// Hook payloads are metadata envelopes, not a transport for prompt or response
// bodies. Keep the cap 64x below the previous 16 MiB limit so a hostile or
// accidental body cannot consume the entire fail-open budget.
const MAX_HOOK_BYTES: usize = 256 * 1024;
const MAX_MCP_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
// One monotonic deadline covers stdin, parsing, local IPC and the optional
// atomic fallback. The Codex host timeout remains a secondary safety net.
const HOOK_FAIL_OPEN_BUDGET: Duration = Duration::from_millis(150);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if run_utility_mode(&args) {
        return;
    }
    app::run(runtime::LaunchOptions::from_args(args));
}

fn run_utility_mode(args: &[std::ffi::OsString]) -> bool {
    let mode = args.first().map(|value| value.to_string_lossy());
    match mode.as_deref() {
        Some("--hook-capture") => {
            hook_capture(args.get(1).map(PathBuf::from).as_deref());
            true
        }
        Some("--mcp-server") => {
            run_mcp_server();
            true
        }
        Some("--install-plugin") => {
            print_utility_result(install_plugin());
            true
        }
        Some("--uninstall-plugin") => {
            print_utility_result(uninstall_plugin());
            true
        }
        _ => false,
    }
}

fn print_utility_result(result: Result<Value, String>) {
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            println!("{}", json!({"ok": false, "error": error}));
            std::process::exit(1);
        }
    }
}

fn hook_capture(fallback_dir: Option<&Path>) {
    let fallback_dir = fallback_dir.map(Path::to_path_buf);
    let _ = run_hook_with_budget(HOOK_FAIL_OPEN_BUDGET, move |deadline| {
        process_hook_input(std::io::stdin(), fallback_dir, deadline)
    });
    println!("{}", json!({"continue": true, "suppressOutput": true}));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookWorkerOutcome {
    Completed,
    DeadlineReached,
    SpawnFailed,
}

fn run_hook_with_budget<F>(budget: Duration, work: F) -> HookWorkerOutcome
where
    F: FnOnce(Instant) -> Result<(), String> + Send + 'static,
{
    let deadline = Instant::now()
        .checked_add(budget)
        .unwrap_or_else(Instant::now);
    let (sender, receiver) = mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("xiaoli-hook-capture".to_owned())
        .spawn(move || {
            let _ = sender.send(work(deadline));
        })
        .is_err()
    {
        return HookWorkerOutcome::SpawnFailed;
    }
    let Some(remaining) = remaining_hook_budget(deadline) else {
        return HookWorkerOutcome::DeadlineReached;
    };
    match receiver.recv_timeout(remaining) {
        Ok(_) => HookWorkerOutcome::Completed,
        Err(_) => HookWorkerOutcome::DeadlineReached,
    }
}

fn process_hook_input<R>(
    mut reader: R,
    fallback_dir: Option<PathBuf>,
    deadline: Instant,
) -> Result<(), String>
where
    R: Read,
{
    let bytes = read_hook_input(&mut reader, deadline)?;
    ensure_hook_budget(deadline)?;
    let input: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_json".to_owned())?;
    ensure_hook_budget(deadline)?;
    let Some(event) = sanitize_hook_input(&input) else {
        return Ok(());
    };
    ensure_hook_budget(deadline)?;
    if send_hook_request_with_deadline(event.clone(), deadline).is_err() {
        persist_hook_fallback_with_deadline(event, fallback_dir, deadline)?;
    }
    Ok(())
}

fn read_hook_input<R: Read>(reader: &mut R, deadline: Instant) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(8 * 1024);
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        ensure_hook_budget(deadline)?;
        let remaining_capacity = MAX_HOOK_BYTES.saturating_add(1).saturating_sub(bytes.len());
        if remaining_capacity == 0 {
            return Err("hook_input_too_large".to_owned());
        }
        let read_capacity = chunk.len().min(remaining_capacity);
        let read = reader
            .read(&mut chunk[..read_capacity])
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HOOK_BYTES {
            return Err("hook_input_too_large".to_owned());
        }
    }
    Ok(bytes)
}

fn ensure_hook_budget(deadline: Instant) -> Result<(), String> {
    remaining_hook_budget(deadline)
        .map(|_| ())
        .ok_or_else(|| "hook_deadline_reached".to_owned())
}

fn remaining_hook_budget(deadline: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    (!remaining.is_zero()).then_some(remaining)
}

fn run_hook_task_with_deadline<T, F>(
    deadline: Instant,
    thread_name: &'static str,
    timeout_error: &'static str,
    task: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    ensure_hook_budget(deadline)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            let _ = sender.send(task());
        })
        .map_err(|error| error.to_string())?;
    let remaining =
        remaining_hook_budget(deadline).ok_or_else(|| "hook_deadline_reached".to_owned())?;
    receiver
        .recv_timeout(remaining)
        .map_err(|_| timeout_error.to_owned())?
}

fn send_hook_request_with_deadline(event: Value, deadline: Instant) -> Result<Value, String> {
    send_hook_request_with_deadline_using(event, deadline, |event| ipc::send_request(&event))
}

fn send_hook_request_with_deadline_using<F>(
    event: Value,
    deadline: Instant,
    send: F,
) -> Result<Value, String>
where
    F: FnOnce(Value) -> Result<Value, String> + Send + 'static,
{
    run_hook_task_with_deadline(
        deadline,
        "xiaoli-hook-send",
        "hook_delivery_timeout",
        move || send(event),
    )
}

fn sanitize_hook_input(input: &Value) -> Option<Value> {
    fn clean(value: Option<&Value>, limit: usize) -> Option<String> {
        let value = value?.as_str()?.trim();
        (!value.is_empty()).then(|| value.chars().take(limit).collect())
    }
    fn first<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a Value> {
        keys.iter().find_map(|key| input.get(*key))
    }

    let event = clean(
        first(input, &["hook_event_name", "hookEventName", "event"]),
        64,
    )?;
    let session = clean(first(input, &["session_id", "sessionId", "session"]), 256)?;
    let (endpoint_class, endpoint_host_hash) = hook_endpoint_evidence();
    Some(json!({
        "event": event,
        "session": session,
        "turn": clean(first(input, &["turn_id", "turnId", "turn"]), 256),
        "model": clean(input.get("model"), 128),
        "endpointClass": endpoint_class,
        "endpointHostHash": endpoint_host_hash,
        "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }))
}

fn hook_endpoint_evidence() -> (Option<connection::EndpointClass>, Option<String>) {
    let mut observations = Vec::new();
    for key in [
        "OPENAI_BASE_URL",
        "OPENAI_API_BASE",
        "ANTHROPIC_BASE_URL",
        "AZURE_OPENAI_ENDPOINT",
    ] {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value.len() > 2_048 {
            continue;
        }
        let class = connection::classify_endpoint(value);
        let Some(scope) = connection::normalize_endpoint_scope(value) else {
            continue;
        };
        observations.push((class, scope));
    }
    if observations.is_empty() {
        return (None, None);
    }
    observations.sort_by(|left, right| left.1.cmp(&right.1));
    observations.dedup();
    let first_class = observations[0].0;
    let class = observations
        .iter()
        .all(|(value, _)| *value == first_class)
        .then_some(first_class)
        .or(Some(connection::EndpointClass::Unknown));
    let scopes = observations
        .iter()
        .map(|(_, scope)| scope.clone())
        .collect::<Vec<_>>();
    (class, connection::combined_endpoint_scope_hash(&scopes))
}

fn persist_hook_fallback(event: &Value, fallback_dir: Option<&Path>) -> Result<(), String> {
    let preferred = ipc::default_state_root();
    let mut candidates = vec![preferred.as_path()];
    if let Some(fallback) = fallback_dir {
        if fallback != preferred {
            candidates.push(fallback);
        }
    }
    let bytes = serde_json::to_vec(event).map_err(|error| error.to_string())?;
    for directory in candidates {
        if fs::create_dir_all(directory).is_err() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o700));
        }
        if AtomicFile::new(directory.join("hook-latest.json"), AllowOverwrite)
            .write(|file| file.write_all(&bytes))
            .is_ok()
        {
            return Ok(());
        }
    }
    Err("hook_fallback_unavailable".to_owned())
}

fn persist_hook_fallback_with_deadline(
    event: Value,
    fallback_dir: Option<PathBuf>,
    deadline: Instant,
) -> Result<(), String> {
    persist_hook_fallback_with_deadline_using(event, fallback_dir, deadline, |event, directory| {
        persist_hook_fallback(&event, directory.as_deref())
    })
}

fn persist_hook_fallback_with_deadline_using<F>(
    event: Value,
    fallback_dir: Option<PathBuf>,
    deadline: Instant,
    persist: F,
) -> Result<(), String>
where
    F: FnOnce(Value, Option<PathBuf>) -> Result<(), String> + Send + 'static,
{
    run_hook_task_with_deadline(
        deadline,
        "xiaoli-hook-fallback",
        "hook_fallback_timeout",
        move || persist(event, fallback_dir),
    )
}

fn run_mcp_server() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.len() > MAX_MCP_MESSAGE_BYTES {
            let _ = writeln!(stdout, "{}", mcp_error(Value::Null, -32700, "Parse error"));
            let _ = stdout.flush();
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_mcp_request(&message),
            Err(_) => Some(mcp_error(Value::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            let _ = writeln!(stdout, "{response}");
            let _ = stdout.flush();
        }
    }
}

fn handle_mcp_request(message: &Value) -> Option<Value> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(mcp_error(id, -32600, "Invalid Request"));
    };
    match method {
        "initialize" => Some(mcp_success(
            id,
            json!({
                "protocolVersion": message.pointer("/params/protocolVersion")
                    .and_then(Value::as_str).unwrap_or("2024-11-05"),
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": PLUGIN_NAME, "version": PLUGIN_VERSION},
                "instructions": "Read-only XiaoLi telemetry. Requested model and effort are not physical-model proof; only explicit server reroute evidence confirms a reroute notification."
            }),
        )),
        "ping" => Some(mcp_success(id, json!({}))),
        "tools/list" => Some(mcp_success(id, json!({"tools": mcp_tool_definitions()}))),
        "tools/call" => {
            let name = message
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let args = message
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Some(mcp_success(id, call_mcp_tool(name, &args)))
        }
        value if value.starts_with("notifications/") => None,
        _ => Some(mcp_error(id, -32601, "Method not found")),
    }
}

fn mcp_tool_definitions() -> Value {
    json!([
        {
            "name": "get_monitor_summary",
            "description": "Read the current sanitized XiaoLi overview with request and server-route evidence kept separate.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}}
        },
        {
            "name": "get_session_detail",
            "description": "Read sanitized model, effort, route evidence, token, cache, timing, and quality detail for one active task.",
            "inputSchema": {
                "type": "object", "additionalProperties": false, "required": ["threadId"],
                "properties": {
                    "threadId": {"type": "string", "minLength": 1, "maxLength": 256},
                    "turnId": {"type": "string", "minLength": 1, "maxLength": 256}
                }
            }
        },
        {
            "name": "render_monitor_card",
            "description": "Render a compact read-only XiaoLi status card.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "threadId": {"type": "string", "minLength": 1, "maxLength": 256},
                    "theme": {"type": "string", "enum": ["cute", "minimal"], "default": "cute"}
                }
            }
        },
        {
            "name": "get_connection_origin",
            "description": "Read configured connection-origin evidence for one active task. This does not identify the physical server model.",
            "inputSchema": {
                "type": "object", "additionalProperties": false, "required": ["threadId"],
                "properties": {
                    "threadId": {"type": "string", "minLength": 1, "maxLength": 256},
                    "turnId": {"type": "string", "minLength": 1, "maxLength": 256}
                }
            }
        },
        {
            "name": "list_relay_audits",
            "description": "List sanitized XiaoLi relay-audit summaries and current read-only progress. This tool cannot start an audit or spend quota.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 20}}
            }
        },
        {
            "name": "get_relay_audit",
            "description": "Read one sanitized relay-audit report or current progress by audit id. This tool cannot start or modify an audit.",
            "inputSchema": {
                "type": "object", "additionalProperties": false, "required": ["auditId"],
                "properties": {"auditId": {"type": "string", "minLength": 1, "maxLength": 256}}
            }
        }
    ])
}

fn call_mcp_tool(name: &str, args: &Value) -> Value {
    let result = match name {
        "get_monitor_summary" => query_monitor("get_monitor_summary", json!({})),
        "get_session_detail" => {
            let params = match session_detail_params(args) {
                Ok(params) => params,
                Err(error) => return mcp_tool_error(&error),
            };
            query_monitor("get_session_detail", params)
        }
        "render_monitor_card" => {
            let params = match monitor_card_params(args) {
                Ok(params) => params,
                Err(error) => return mcp_tool_error(&error),
            };
            query_monitor("render_monitor_card", params.clone())
                .and_then(|snapshot| render_text_card(&snapshot, &params))
        }
        "get_connection_origin" => {
            let params = match session_detail_params(args) {
                Ok(params) => params,
                Err(error) => return mcp_tool_error(&error),
            };
            query_monitor("get_connection_origin", params)
        }
        "list_relay_audits" => {
            let params = match relay_audit_list_params(args) {
                Ok(params) => params,
                Err(error) => return mcp_tool_error(&error),
            };
            query_monitor("list_relay_audits", params)
        }
        "get_relay_audit" => {
            let params = match relay_audit_detail_params(args) {
                Ok(params) => params,
                Err(error) => return mcp_tool_error(&error),
            };
            query_monitor("get_relay_audit", params)
        }
        _ => return mcp_tool_error("unknown tool"),
    };
    match result {
        Ok(value) => {
            let text = if name == "render_monitor_card" {
                value
                    .get("card")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "小狸状态卡暂不可用".to_owned())
            } else {
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_owned())
            };
            json!({
                "content": [{"type": "text", "text": text}],
                "structuredContent": value
            })
        }
        Err(error) => mcp_tool_error(&error),
    }
}

fn optional_nonempty_string<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.as_str())),
        _ => Err(format!("{key} must be a non-empty string")),
    }
}

fn session_detail_params(args: &Value) -> Result<Value, String> {
    let thread_id = optional_nonempty_string(args, "threadId")?
        .ok_or_else(|| "threadId is required".to_owned())?;
    if thread_id.chars().count() > 256 {
        return Err("threadId must not exceed 256 characters".to_owned());
    }
    let mut params = json!({"threadId": thread_id});
    if let Some(turn_id) = optional_nonempty_string(args, "turnId")? {
        if turn_id.chars().count() > 256 {
            return Err("turnId must not exceed 256 characters".to_owned());
        }
        params
            .as_object_mut()
            .expect("object literal")
            .insert("turnId".to_owned(), Value::String(turn_id.to_owned()));
    }
    Ok(params)
}

fn monitor_card_params(args: &Value) -> Result<Value, String> {
    let thread_id = optional_nonempty_string(args, "threadId")?;
    let theme = optional_nonempty_string(args, "theme")?.unwrap_or("cute");
    if !matches!(theme, "cute" | "minimal") {
        return Err("theme must be cute or minimal".to_owned());
    }
    let mut params = json!({"theme": theme});
    if let Some(thread_id) = thread_id {
        params
            .as_object_mut()
            .expect("object literal")
            .insert("threadId".to_owned(), Value::String(thread_id.to_owned()));
    }
    Ok(params)
}

fn relay_audit_list_params(args: &Value) -> Result<Value, String> {
    let limit = match args.get("limit") {
        None | Some(Value::Null) => 20,
        Some(Value::Number(value)) => value
            .as_u64()
            .filter(|value| (1..=200).contains(value))
            .ok_or_else(|| "limit must be an integer between 1 and 200".to_owned())?,
        _ => return Err("limit must be an integer between 1 and 200".to_owned()),
    };
    Ok(json!({"limit": limit}))
}

fn relay_audit_detail_params(args: &Value) -> Result<Value, String> {
    let audit_id = optional_nonempty_string(args, "auditId")?
        .ok_or_else(|| "auditId is required".to_owned())?;
    if audit_id.chars().count() > 256 {
        return Err("auditId must not exceed 256 characters".to_owned());
    }
    Ok(json!({"auditId": audit_id}))
}

fn query_monitor(method: &str, params: Value) -> Result<Value, String> {
    query_monitor_with(method, params, ipc::send_request)
}

fn query_monitor_with<F>(method: &str, params: Value, send: F) -> Result<Value, String>
where
    F: FnOnce(&Value) -> Result<Value, String>,
{
    let request = json!({"schemaVersion": 1, "method": method, "params": params});
    let mut response = send(&request).map_err(|_| {
        "XiaoLi monitor is offline. Cached snapshots are intentionally not returned as current data; start XiaoLi and retry."
            .to_owned()
    })?;
    let object = response
        .as_object_mut()
        .ok_or_else(|| "XiaoLi live monitor returned an invalid response".to_owned())?;
    if object.get("ok").and_then(Value::as_bool) == Some(false) {
        let reason = object
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request_rejected")
            .chars()
            .take(240)
            .collect::<String>();
        return Err(format!(
            "XiaoLi live monitor rejected the request: {reason}"
        ));
    }
    object.insert(
        "snapshotSource".to_owned(),
        Value::String("liveMonitorIpc".to_owned()),
    );
    Ok(response)
}

fn project_card_conversations(
    snapshot: &Value,
    thread_id: Option<&str>,
) -> Result<Vec<Value>, String> {
    let conversations = snapshot
        .get("conversations")
        .and_then(Value::as_array)
        .ok_or_else(|| "XiaoLi live monitor response has no conversations".to_owned())?;
    let Some(thread_id) = thread_id else {
        return Ok(conversations.clone());
    };
    let target = conversations
        .iter()
        .find(|conversation| {
            conversation.get("threadId").and_then(Value::as_str) == Some(thread_id)
        })
        .cloned()
        .ok_or_else(|| "requested active conversation was not found".to_owned())?;
    let mut selected_ids = HashSet::from([thread_id.to_owned()]);
    loop {
        let mut changed = false;
        for conversation in conversations {
            let Some(id) = conversation.get("threadId").and_then(Value::as_str) else {
                continue;
            };
            if conversation
                .get("parentThreadId")
                .and_then(Value::as_str)
                .is_some_and(|parent| selected_ids.contains(parent))
                && selected_ids.insert(id.to_owned())
            {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut projected = Vec::with_capacity(selected_ids.len());
    projected.push(target);
    projected.extend(conversations.iter().filter_map(|conversation| {
        let id = conversation.get("threadId").and_then(Value::as_str)?;
        (id != thread_id && selected_ids.contains(id)).then(|| conversation.clone())
    }));
    Ok(projected)
}

fn render_text_card(snapshot: &Value, params: &Value) -> Result<Value, String> {
    let theme = optional_nonempty_string(params, "theme")?.unwrap_or("cute");
    let thread_id = optional_nonempty_string(params, "threadId")?;
    let projected_thread_id = snapshot.get("projectionThreadId").and_then(Value::as_str);
    let conversations = project_card_conversations(snapshot, thread_id)?;
    let count = conversations.len();
    let primary = if thread_id.is_some() || count == 1 {
        conversations.first()
    } else {
        None
    };
    let line = if let Some(conversation) = primary {
        let model = conversation
            .pointer("/activeRequest/model")
            .and_then(Value::as_str)
            .unwrap_or("模型未知");
        let effort = conversation
            .pointer("/activeRequest/effort")
            .and_then(Value::as_str)
            .unwrap_or("effort 未知");
        let route = conversation
            .pointer("/serverRoute/model")
            .and_then(Value::as_str)
            .filter(|_| {
                conversation
                    .pointer("/serverRoute/evidence")
                    .and_then(Value::as_str)
                    == Some("explicitReroute")
            });
        let evidence = route.map_or_else(
            || format!("{model}（请求）"),
            |routed| format!("{model}（请求） → {routed}（服务器已重路由）"),
        );
        if theme == "minimal" {
            format!("小狸 | {evidence} | {effort}（请求）")
        } else {
            format!("小狸 · {evidence} · {effort}（请求）")
        }
    } else if count == 0 {
        if theme == "minimal" {
            "小狸 | 当前没有活动任务".to_owned()
        } else {
            "小狸 · 当前没有活动任务".to_owned()
        }
    } else if theme == "minimal" {
        format!("小狸 | {count} 个活动任务")
    } else {
        format!("小狸 · {count} 个活动任务")
    };
    let mut projected_snapshot = snapshot.clone();
    projected_snapshot
        .as_object_mut()
        .ok_or_else(|| "XiaoLi live monitor returned an invalid snapshot".to_owned())?
        .insert(
            "conversations".to_owned(),
            Value::Array(conversations.clone()),
        );
    Ok(json!({
        "schemaVersion": snapshot.get("schemaVersion").cloned().unwrap_or(json!(5)),
        "checkedAt": snapshot.get("checkedAt").cloned().unwrap_or(Value::Null),
        "snapshotSource": snapshot.get("snapshotSource").cloned().unwrap_or(Value::Null),
        "theme": theme,
        "threadId": projected_thread_id,
        "card": line,
        "conversation": primary.cloned(),
        "children": if primary.is_some() { conversations.iter().skip(1).cloned().collect::<Vec<_>>() } else { Vec::new() },
        "snapshot": projected_snapshot
    }))
}

fn mcp_success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn mcp_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn mcp_tool_error(message: &str) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PluginHost {
    executable: PathBuf,
    appimage_extract_and_run: bool,
}

impl PluginHost {
    fn direct(executable: PathBuf) -> Self {
        Self {
            executable,
            appimage_extract_and_run: false,
        }
    }
}

pub(crate) fn install_plugin() -> Result<Value, String> {
    let host = plugin_host()?;
    let home = dirs::home_dir().ok_or_else(|| "home_directory_unavailable".to_owned())?;
    install_plugin_host_at(&home, &host)
}

/// Resolve the stable executable that Codex should invoke after this process
/// exits. Linux AppImage payloads run from an ephemeral `/tmp/.mount_*` path;
/// the launcher exposes the persistent archive through `$APPIMAGE`.
fn plugin_host() -> Result<PluginHost, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    Ok(resolve_plugin_host(
        current,
        appimage,
        cfg!(target_os = "linux"),
    ))
}

fn resolve_plugin_host(
    current: PathBuf,
    appimage: Option<PathBuf>,
    prefer_appimage: bool,
) -> PluginHost {
    if prefer_appimage {
        if let Some(stable) = appimage
            .filter(|path| path.is_absolute())
            .and_then(|path| fs::canonicalize(path).ok())
        {
            return PluginHost {
                executable: stable,
                appimage_extract_and_run: true,
            };
        }
    }
    PluginHost::direct(current)
}

#[cfg(test)]
fn install_plugin_at(home: &Path, executable: &Path) -> Result<Value, String> {
    install_plugin_host_at(home, &PluginHost::direct(executable.to_path_buf()))
}

fn install_plugin_host_at(home: &Path, host: &PluginHost) -> Result<Value, String> {
    if !host.executable.is_absolute() {
        return Err("executable_path_must_be_absolute".to_owned());
    }
    with_plugin_install_lock(home, || install_plugin_at_locked(home, host))
}

fn install_plugin_at_locked(home: &Path, host: &PluginHost) -> Result<Value, String> {
    let plugin_root = home.join("plugins").join(PLUGIN_NAME);
    let was_current = plugin_installation_is_current(home, host);
    for directory in [
        plugin_root.join(".codex-plugin"),
        plugin_root.join("hooks"),
        plugin_root.join("skills/model-monitor"),
        plugin_root.join("assets"),
    ] {
        fs::create_dir_all(&directory)
            .map_err(|error| format!("create {}: {error}", directory.display()))?;
    }

    write_json(
        &plugin_root.join(".codex-plugin/plugin.json"),
        &plugin_manifest(),
    )?;
    write_json(&plugin_root.join(".mcp.json"), &mcp_manifest(host))?;
    write_json(&plugin_root.join("hooks/hooks.json"), &hooks_manifest(host))?;
    write_text(
        &plugin_root.join("skills/model-monitor/SKILL.md"),
        PLUGIN_SKILL,
    )?;
    write_text(&plugin_root.join("README.md"), PLUGIN_README)?;
    write_text(&plugin_root.join("assets/icon.svg"), PLUGIN_ICON)?;
    update_personal_marketplace(home, true)?;

    if !plugin_installation_is_current(home, host) {
        return Err("plugin_installation_verification_failed".to_owned());
    }

    Ok(json!({
        "ok": true, "action": "installed", "plugin": PLUGIN_NAME,
        "path": plugin_root, "executable": host.executable, "changed": !was_current
    }))
}

fn uninstall_plugin() -> Result<Value, String> {
    let home = dirs::home_dir().ok_or_else(|| "home_directory_unavailable".to_owned())?;
    with_plugin_install_lock(&home, || uninstall_plugin_at_locked(&home))
}

fn uninstall_plugin_at_locked(home: &Path) -> Result<Value, String> {
    let plugin_root = home.join("plugins").join(PLUGIN_NAME);
    let expected_parent = home.join("plugins");
    if plugin_root.parent() != Some(expected_parent.as_path()) {
        return Err("refusing_unexpected_plugin_path".to_owned());
    }
    // Parse, validate, mutate, and serialize the marketplace before touching
    // the plugin tree. The old order could permanently delete a working plugin
    // and only then discover that marketplace.json was corrupt.
    let (marketplace_path, marketplace_bytes) = prepare_personal_marketplace(home, false)?;
    uninstall_plugin_transactionally(
        &plugin_root,
        &marketplace_path,
        &marketplace_bytes,
        write_prepared_json,
    )?;
    Ok(json!({"ok": true, "action": "uninstalled", "plugin": PLUGIN_NAME}))
}

fn uninstall_plugin_transactionally<F>(
    plugin_root: &Path,
    marketplace_path: &Path,
    marketplace_bytes: &[u8],
    write_marketplace: F,
) -> Result<(), String>
where
    F: FnOnce(&Path, &[u8]) -> Result<(), String>,
{
    let staged = if plugin_root.exists() {
        let parent = plugin_root
            .parent()
            .ok_or_else(|| "refusing_unexpected_plugin_path".to_owned())?;
        let staged = (0_u16..=u16::MAX)
            .map(|suffix| {
                parent.join(format!(
                    ".{PLUGIN_NAME}.uninstall-{}-{suffix}",
                    std::process::id()
                ))
            })
            .find(|candidate| !candidate.exists())
            .ok_or_else(|| "plugin_uninstall_staging_unavailable".to_owned())?;
        fs::rename(plugin_root, &staged).map_err(|error| {
            format!(
                "stage plugin {} -> {}: {error}",
                plugin_root.display(),
                staged.display()
            )
        })?;
        Some(staged)
    } else {
        None
    };

    if let Err(error) = write_marketplace(marketplace_path, marketplace_bytes) {
        if let Some(staged) = staged.as_ref() {
            if let Err(restore) = fs::rename(staged, plugin_root) {
                return Err(format!(
                    "write marketplace failed: {error}; restore plugin failed: {restore}; recover from {}",
                    staged.display()
                ));
            }
        }
        return Err(error);
    }

    if let Some(staged) = staged {
        let metadata = fs::symlink_metadata(&staged)
            .map_err(|error| format!("inspect staged plugin {}: {error}", staged.display()))?;
        let result = if metadata.is_dir() {
            fs::remove_dir_all(&staged)
        } else {
            fs::remove_file(&staged)
        };
        result.map_err(|error| {
            format!(
                "marketplace updated but staged plugin cleanup failed at {}: {error}",
                staged.display()
            )
        })?;
    }
    Ok(())
}

fn with_plugin_install_lock<T>(
    home: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let lock_directory = home.join(".agents/plugins");
    fs::create_dir_all(&lock_directory)
        .map_err(|error| format!("create plugin lock directory: {error}"))?;
    let lock_path = lock_directory.join(".xiaoli-install.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("open plugin install lock: {error}"))?;
    lock.lock_exclusive()
        .map_err(|error| format!("lock plugin installation: {error}"))?;
    let result = operation();
    let _ = lock.unlock();
    result
}

fn plugin_manifest() -> Value {
    json!({
        "name": PLUGIN_NAME, "version": PLUGIN_VERSION,
        "description": "小狸提供只读的 Codex 请求模型、effort、路由证据、token、缓存、时序与质量评估。",
        "author": {"name": "XuYing1128", "url": "https://github.com/XuYing1128"},
        "homepage": "https://github.com/XuYing1128/XiaoLi",
        "repository": "https://github.com/XuYing1128/XiaoLi",
        "license": "PolyForm-Noncommercial-1.0.0",
        "keywords": ["codex", "model", "telemetry", "tokens", "xiaoli"],
        "skills": "./skills/", "mcpServers": "./.mcp.json",
        "interface": {
            "displayName": "小狸 · XiaoLi",
            "shortDescription": "查看 Codex 请求模型、路由证据与 token 指标。",
            "longDescription": "本地只读监视器，严格区分请求模型/effort、显式服务器重路由和行为统计异常。",
            "developerName": "XuYing1128", "category": "Productivity", "capabilities": ["Read"],
            "websiteURL": "https://github.com/XuYing1128/XiaoLi",
            "defaultPrompt": ["显示当前小狸监视摘要。", "显示当前任务的模型证据和 token。", "渲染一张小狸监视卡。"],
            "brandColor": "#8B7AA8", "composerIcon": "./assets/icon.svg", "logo": "./assets/icon.svg"
        }
    })
}

fn mcp_manifest(host: &PluginHost) -> Value {
    let args = if host.appimage_extract_and_run {
        vec!["--appimage-extract-and-run", "--mcp-server"]
    } else {
        vec!["--mcp-server"]
    };
    json!({"mcpServers": {PLUGIN_NAME: {
        "type": "stdio", "command": host.executable, "args": args,
        "startup_timeout_sec": 5, "tool_timeout_sec": 3
    }}})
}

fn hooks_manifest(host: &PluginHost) -> Value {
    let runtime_argument = if host.appimage_extract_and_run {
        " --appimage-extract-and-run"
    } else {
        ""
    };
    let command = format!(
        "{}{} --hook-capture \"${{PLUGIN_DATA}}\"",
        shell_command_path(&host.executable),
        runtime_argument
    );
    let hook = || {
        json!({"hooks": [{
            "type": "command", "command": command, "commandWindows": command,
            "timeout": 2, "async": true
        }]})
    };
    json!({
        "description": "Fail-open metadata-only XiaoLi hooks; prompts, replies, cwd, tool content, and transcript paths are discarded.",
        "hooks": {
            "SessionStart": [hook()], "UserPromptSubmit": [hook()],
            "SubagentStart": [hook()], "SubagentStop": [hook()], "Stop": [hook()]
        }
    })
}

fn shell_command_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        format!("\"{}\"", path.to_string_lossy().replace('"', ""))
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }
}

fn plugin_installation_is_current(home: &Path, host: &PluginHost) -> bool {
    let plugin_root = home.join("plugins").join(PLUGIN_NAME);
    let json_matches = |path: &Path, expected: &Value| {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .as_ref()
            == Some(expected)
    };
    if !json_matches(
        &plugin_root.join(".codex-plugin/plugin.json"),
        &plugin_manifest(),
    ) || !json_matches(&plugin_root.join(".mcp.json"), &mcp_manifest(host))
        || !json_matches(&plugin_root.join("hooks/hooks.json"), &hooks_manifest(host))
        || fs::read(plugin_root.join("skills/model-monitor/SKILL.md"))
            .ok()
            .as_deref()
            != Some(PLUGIN_SKILL.as_bytes())
        || fs::read(plugin_root.join("README.md")).ok().as_deref() != Some(PLUGIN_README.as_bytes())
        || fs::read(plugin_root.join("assets/icon.svg"))
            .ok()
            .as_deref()
            != Some(PLUGIN_ICON.as_bytes())
    {
        return false;
    }
    let marketplace_path = home.join(".agents/plugins/marketplace.json");
    let Some(marketplace) = fs::read(&marketplace_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return false;
    };
    marketplace
        .get("plugins")
        .and_then(Value::as_array)
        .is_some_and(|plugins| {
            plugins.iter().any(|entry| {
                entry.get("name").and_then(Value::as_str) == Some(PLUGIN_NAME)
                    && entry.pointer("/source/source").and_then(Value::as_str) == Some("local")
                    && entry.pointer("/source/path").and_then(Value::as_str)
                        == Some("./plugins/xiaoli-model-monitor")
                    && entry
                        .pointer("/policy/installation")
                        .and_then(Value::as_str)
                        == Some("INSTALLED_BY_DEFAULT")
            })
        })
}

fn update_personal_marketplace(home: &Path, install: bool) -> Result<(), String> {
    let (path, bytes) = prepare_personal_marketplace(home, install)?;
    write_prepared_json(&path, &bytes)
}

fn prepare_personal_marketplace(home: &Path, install: bool) -> Result<(PathBuf, Vec<u8>), String> {
    let marketplace_path = home.join(".agents/plugins/marketplace.json");
    if let Some(parent) = marketplace_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut marketplace = match fs::read(&marketplace_path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| format!("invalid personal marketplace JSON: {error}"))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            json!({"name": "personal", "interface": {"displayName": "Personal"}, "plugins": []})
        }
        Err(error) => return Err(format!("read personal marketplace: {error}")),
    };
    let plugins = marketplace
        .get_mut("plugins")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "invalid_personal_marketplace_plugins".to_owned())?;
    plugins.retain(|entry| {
        let name = entry.get("name").and_then(Value::as_str);
        name != Some(PLUGIN_NAME) && (!install || !is_owned_legacy_model_monitor_entry(home, entry))
    });
    if install {
        plugins.push(json!({
            "name": PLUGIN_NAME,
            "source": {"source": "local", "path": format!("./plugins/{PLUGIN_NAME}")},
            "policy": {"installation": "INSTALLED_BY_DEFAULT", "authentication": "ON_INSTALL"},
            "category": "Productivity"
        }));
    }
    let bytes = serde_json::to_vec_pretty(&marketplace).map_err(|error| error.to_string())?;
    Ok((marketplace_path, bytes))
}

fn write_prepared_json(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| file.write_all(bytes))
        .map_err(|error| error.to_string())
}

/// Only migrate the exact v3 predecessor that XiaoLi itself installed. A
/// marketplace name is not ownership: another local plugin may legitimately
/// be called `model-monitor`, so path and immutable manifest fingerprints must
/// all match before its registration is replaced.
fn is_owned_legacy_model_monitor_entry(home: &Path, entry: &Value) -> bool {
    if entry.get("name").and_then(Value::as_str) != Some("model-monitor")
        || entry.pointer("/source/source").and_then(Value::as_str) != Some("local")
        || entry.pointer("/source/path").and_then(Value::as_str) != Some("./plugins/model-monitor")
    {
        return false;
    }
    let manifest = home.join("plugins/model-monitor/.codex-plugin/plugin.json");
    let Some(manifest) = fs::read(manifest)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return false;
    };
    manifest.get("name").and_then(Value::as_str) == Some("model-monitor")
        && manifest.get("version").and_then(Value::as_str) == Some("0.1.0")
        && manifest.get("description").and_then(Value::as_str)
            == Some(
                "Read-only Codex model, effort, and token telemetry with explicit evidence labels.",
            )
        && manifest.pointer("/author/name").and_then(Value::as_str)
            == Some("Model Monitor contributors")
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    if fs::read(path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| file.write_all(&bytes))
        .map_err(|error| error.to_string())
}

fn write_text(path: &Path, value: &str) -> Result<(), String> {
    if fs::read(path).ok().as_deref() == Some(value.as_bytes()) {
        return Ok(());
    }
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| file.write_all(value.as_bytes()))
        .map_err(|error| error.to_string())
}

const PLUGIN_SKILL: &str = r#"---
name: model-monitor
description: Inspect current Codex task request models, explicit server reroute evidence, configured connection origin, token usage, timing, quality assessments, and completed relay audits through read-only MCP tools.
---

# 小狸模型监视

先调用 `get_monitor_summary`。需要单个任务证据时调用 `get_session_detail`；需要简洁卡片时调用 `render_monitor_card`。需要连接来源时调用 `get_connection_origin`；只读查看中转审计时使用 `list_relay_audits` 和 `get_relay_audit`。

- 请求模型和请求 effort 不是物理后端或实测思考强度证明。
- 连接来源只是端点与认证配置证据，不是服务器物理模型身份证明。
- 只有明确的 `model/rerouted` 证据才能称为服务器已重路由。
- “未见服务器重路由”只代表小狸未捕获显式事件，不证明物理模型没有变化。
- token、缓存、耗时和行为偏离只能作为遥测或黄色疑似降质提示，不能伪装为服务器路由证据。
- 只把带有 `snapshotSource: liveMonitorIpc` 的工具结果作为当前状态；小狸离线时明确说明无法查询，不用磁盘旧快照代替。
- 不输出 prompt、回复正文、cwd、工具内容、transcript 或 rollout 路径。
- MCP 没有启动、取消或排程审计的工具，不会因为对话内调用而消耗 API 额度。
"#;

const PLUGIN_README: &str = r#"# 小狸 Codex 插件

该插件调用同一份 `xiaoli` 可执行文件提供 fail-open hook 与只读 MCP 服务，不需要 Node.js。

安装、升级或移动程序后，请在 Codex `/hooks` 中审阅并信任本地 hook 命令；写入配置不会绕过 Codex 的信任确认。已运行的 Codex 请新建任务或重启后加载。

请求模型/effort 是请求配置；只有显式 `model/rerouted` 才是服务器重路由通知；行为统计只用于黄色疑似降质。MCP 只接受正在运行的小狸 IPC 快照并标记 `snapshotSource: liveMonitorIpc`，离线时不会把磁盘旧快照冒充为当前状态。
"#;

const PLUGIN_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="18" fill="#FFF8F3"/><path d="M14 38c4-15 32-15 36 0-5 12-31 12-36 0Z" fill="#9A8BA8"/><path d="M22 29 18 18l11 7m13 4 4-11-11 7" fill="#F4E6D2" stroke="#675B70" stroke-width="2"/><circle cx="26" cy="37" r="2" fill="#453C48"/><circle cx="38" cy="37" r="2" fill="#453C48"/><path d="M28 44q4 3 8 0" fill="none" stroke="#453C48" stroke-width="2"/></svg>"##;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Cursor,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
    };

    struct SlowReader {
        payload: Option<Vec<u8>>,
        delay: Duration,
    }

    impl Read for SlowReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(payload) = self.payload.take() else {
                return Ok(0);
            };
            thread::sleep(self.delay);
            let length = payload.len().min(buffer.len());
            buffer[..length].copy_from_slice(&payload[..length]);
            Ok(length)
        }
    }

    #[test]
    fn hook_sanitizer_keeps_only_metadata() {
        let value = sanitize_hook_input(&json!({
            "hook_event_name": "UserPromptSubmit", "session_id": "thread-1",
            "turn_id": "turn-1", "model": "gpt-5.6-sol",
            "cwd": "PRIVATE", "prompt": "PRIVATE", "last_assistant_message": "PRIVATE"
        }))
        .expect("valid hook");
        let text = value.to_string();
        assert!(!text.contains("PRIVATE"));
        assert_eq!(
            value.get("session").and_then(Value::as_str),
            Some("thread-1")
        );
        assert_eq!(value.as_object().map(|value| value.len()), Some(7));
        assert!(value.get("endpointClass").is_some());
        assert!(value.get("endpointHostHash").is_some());
    }

    #[test]
    fn hook_fail_open_deadline_bounds_a_stalled_stdin_read() {
        let started = Instant::now();
        let outcome = run_hook_with_budget(Duration::from_millis(30), |deadline| {
            process_hook_input(
                SlowReader {
                    payload: Some(br#"{}"#.to_vec()),
                    delay: Duration::from_millis(250),
                },
                None,
                deadline,
            )
        });
        assert_eq!(outcome, HookWorkerOutcome::DeadlineReached);
        assert!(
            started.elapsed() < Duration::from_millis(120),
            "fail-open supervisor waited {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn hook_rejects_oversized_metadata_before_parsing_or_delivery() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let error = read_hook_input(&mut Cursor::new(vec![b'x'; MAX_HOOK_BYTES + 1]), deadline)
            .expect_err("oversized hook input must fail open");
        assert_eq!(error, "hook_input_too_large");
    }

    #[test]
    fn hook_ipc_and_fallback_share_the_same_remaining_deadline() {
        let deadline = Instant::now() + Duration::from_millis(30);
        let delivery =
            send_hook_request_with_deadline_using(json!({"event":"Stop"}), deadline, |_| {
                thread::sleep(Duration::from_millis(200));
                Ok(json!({"ok": true}))
            });
        assert_eq!(delivery.unwrap_err(), "hook_delivery_timeout");

        let fallback_started = Arc::new(AtomicBool::new(false));
        let observed = fallback_started.clone();
        let fallback = persist_hook_fallback_with_deadline_using(
            json!({"event":"Stop"}),
            None,
            deadline,
            move |_, _| {
                observed.store(true, Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(fallback.unwrap_err(), "hook_deadline_reached");
        assert!(!fallback_started.load(Ordering::SeqCst));
    }

    #[test]
    fn hook_deadline_bounds_a_slow_atomic_fallback() {
        let deadline = Instant::now() + Duration::from_millis(25);
        let started = Instant::now();
        let result = persist_hook_fallback_with_deadline_using(
            json!({"event":"Stop"}),
            None,
            deadline,
            |_, _| {
                thread::sleep(Duration::from_millis(200));
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err(), "hook_fallback_timeout");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "fallback deadline waited {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn hook_fast_path_p95_stays_inside_the_fail_open_target() {
        let mut samples = Vec::new();
        for _ in 0..40 {
            let started = Instant::now();
            let outcome = run_hook_with_budget(HOOK_FAIL_OPEN_BUDGET, |deadline| {
                process_hook_input(Cursor::new(br#"{}"#.to_vec()), None, deadline)
            });
            assert_eq!(outcome, HookWorkerOutcome::Completed);
            samples.push(started.elapsed());
        }
        samples.sort_unstable();
        let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
        assert!(
            p95 < HOOK_FAIL_OPEN_BUDGET,
            "hook fast-path P95 {:?} exceeded {:?}",
            p95,
            HOOK_FAIL_OPEN_BUDGET
        );
    }

    #[test]
    fn mcp_server_lists_six_read_only_tools_and_never_exposes_audit_start() {
        let response =
            handle_mcp_request(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                .expect("response");
        let tools = response
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .expect("tools");
        assert_eq!(tools.len(), 6);
        assert_eq!(
            tools[0].get("name").and_then(Value::as_str),
            Some("get_monitor_summary")
        );
        let names = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(names.contains(&"get_connection_origin"));
        assert!(names.contains(&"list_relay_audits"));
        assert!(names.contains(&"get_relay_audit"));
        assert!(!names.iter().any(|name| name.contains("start")));
        assert!(!names.iter().any(|name| name.contains("cancel")));
    }

    #[test]
    fn mcp_marks_live_ipc_results_and_refuses_offline_cache_as_current() {
        let live = query_monitor_with("get_monitor_summary", json!({}), |_| {
            Ok(json!({"schemaVersion": 5, "checkedAt": "2026-08-25T00:00:00Z", "conversations": []}))
        })
        .expect("live IPC response");
        assert_eq!(
            live.get("snapshotSource").and_then(Value::as_str),
            Some("liveMonitorIpc")
        );

        let offline = query_monitor_with("get_monitor_summary", json!({}), |_| {
            Err("connection refused".to_owned())
        })
        .expect_err("offline monitor must not return a stale snapshot");
        assert!(offline.contains("offline"));
        assert!(offline.contains("Cached snapshots are intentionally not returned"));
        assert!(!offline.contains("connection refused"));

        let rejected = query_monitor_with("get_session_detail", json!({}), |_| {
            Ok(json!({"ok": false, "error": "active_conversation_not_found"}))
        })
        .expect_err("a live rejection must not be mistaken for telemetry");
        assert!(rejected.contains("live monitor rejected"));
        assert!(!rejected.contains("offline"));
    }

    #[test]
    fn mcp_detail_and_card_honor_turn_thread_and_theme_projection() {
        assert_eq!(
            session_detail_params(&json!({"threadId":"root"})).unwrap(),
            json!({"threadId":"root"})
        );
        assert_eq!(
            session_detail_params(&json!({"threadId":"root", "turnId":"turn-7"})).unwrap(),
            json!({"threadId":"root", "turnId":"turn-7"})
        );

        let live = json!({
            "schemaVersion": 5,
            "checkedAt": "2026-08-25T00:00:00Z",
            "snapshotSource": "liveMonitorIpc",
            "conversations": [
                {
                    "threadId":"root", "turnId":"turn-root",
                    "activeRequest":{"model":"gpt-5.6-sol","effort":"ultra"},
                    "serverRoute":{"evidence":"notObserved"}
                },
                {
                    "threadId":"child", "turnId":"turn-child", "parentThreadId":"root",
                    "activeRequest":{"model":"gpt-5.6-sol","effort":"high"},
                    "serverRoute":{"evidence":"notObserved"}
                },
                {
                    "threadId":"unrelated", "turnId":"turn-other",
                    "activeRequest":{"model":"gpt-5.5","effort":"medium"},
                    "serverRoute":{"evidence":"notObserved"}
                }
            ]
        });
        let card = render_text_card(
            &live,
            &monitor_card_params(&json!({"threadId":"root", "theme":"minimal"})).unwrap(),
        )
        .expect("projected card");
        assert_eq!(card.get("theme").and_then(Value::as_str), Some("minimal"));
        assert_eq!(
            card.get("snapshotSource").and_then(Value::as_str),
            Some("liveMonitorIpc")
        );
        assert_eq!(
            card.pointer("/conversation/threadId")
                .and_then(Value::as_str),
            Some("root")
        );
        assert_eq!(
            card.pointer("/children/0/threadId").and_then(Value::as_str),
            Some("child")
        );
        assert_eq!(
            card.pointer("/snapshot/conversations")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        assert!(card
            .get("card")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains(" | ")));
    }

    #[test]
    fn mcp_relay_audit_arguments_are_bounded_and_read_only() {
        assert_eq!(
            relay_audit_list_params(&json!({})).unwrap(),
            json!({"limit": 20})
        );
        assert_eq!(
            relay_audit_list_params(&json!({"limit": 200})).unwrap(),
            json!({"limit": 200})
        );
        assert!(relay_audit_list_params(&json!({"limit": 0})).is_err());
        assert!(relay_audit_list_params(&json!({"limit": 201})).is_err());
        assert!(relay_audit_list_params(&json!({"limit": 1.5})).is_err());
        assert_eq!(
            relay_audit_detail_params(&json!({"auditId": "audit-fixture"})).unwrap(),
            json!({"auditId": "audit-fixture"})
        );
        assert!(relay_audit_detail_params(&json!({})).is_err());
        assert!(relay_audit_detail_params(&json!({"auditId": "x".repeat(257)})).is_err());

        let blocked = call_mcp_tool("start_relay_audit", &json!({}));
        assert_eq!(blocked.get("isError").and_then(Value::as_bool), Some(true));
        assert!(blocked.to_string().contains("unknown tool"));
    }

    #[test]
    fn appimage_plugin_path_uses_the_persistent_archive_not_the_mount() {
        let temporary =
            std::env::temp_dir().join(format!("xiaoli-appimage-path-test-{}", std::process::id()));
        let mounted = temporary.join(".mount_XiaoLi/usr/bin/xiaoli");
        let archive = temporary.join("XiaoLi.AppImage");
        let _ = fs::remove_dir_all(&temporary);
        fs::create_dir_all(mounted.parent().expect("mount parent")).expect("mount tree");
        fs::write(&mounted, b"ephemeral").expect("mounted executable");
        fs::write(&archive, b"persistent").expect("AppImage archive");

        let resolved = resolve_plugin_host(mounted.clone(), Some(archive.clone()), true);
        assert_eq!(
            resolved.executable,
            fs::canonicalize(&archive).expect("canonical archive")
        );
        assert!(resolved.appimage_extract_and_run);
        assert!(!resolved.executable.to_string_lossy().contains(".mount_"));

        let fallback = resolve_plugin_host(
            mounted.clone(),
            Some(temporary.join("missing.AppImage")),
            true,
        );
        assert_eq!(fallback, PluginHost::direct(mounted));
        let _ = fs::remove_dir_all(&temporary);
    }

    #[test]
    fn generated_appimage_plugin_persists_extract_and_run_for_mcp_and_hooks() {
        let temporary = std::env::temp_dir().join(format!(
            "xiaoli-appimage-plugin-test-{}",
            std::process::id()
        ));
        let executable = temporary.join("portable/XiaoLi renamed archive");
        let host = PluginHost {
            executable: executable.clone(),
            appimage_extract_and_run: true,
        };
        let _ = fs::remove_dir_all(&temporary);
        fs::create_dir_all(executable.parent().expect("archive parent")).expect("archive tree");
        fs::write(&executable, b"appimage fixture").expect("archive fixture");

        install_plugin_host_at(&temporary, &host).expect("install AppImage plugin");
        let plugin = temporary.join("plugins").join(PLUGIN_NAME);
        let mcp: Value = serde_json::from_slice(
            &fs::read(plugin.join(".mcp.json")).expect("generated MCP manifest"),
        )
        .expect("valid MCP manifest");
        assert_eq!(
            mcp.pointer("/mcpServers/xiaoli-model-monitor/args"),
            Some(&json!(["--appimage-extract-and-run", "--mcp-server"]))
        );
        let hooks =
            fs::read_to_string(plugin.join("hooks/hooks.json")).expect("generated hooks manifest");
        assert!(hooks.contains("--appimage-extract-and-run --hook-capture"));
        assert!(plugin_installation_is_current(&temporary, &host));

        let _ = fs::remove_dir_all(&temporary);
    }

    #[test]
    fn generated_plugin_uses_absolute_executable_and_no_node() {
        let temporary =
            std::env::temp_dir().join(format!("xiaoli-plugin-test-{}", std::process::id()));
        let executable = if cfg!(windows) {
            PathBuf::from(r"C:\Portable XiaoLi\XiaoLi.exe")
        } else {
            PathBuf::from("/opt/xiaoli/xiaoli")
        };
        let _ = fs::remove_dir_all(&temporary);
        let first = install_plugin_at(&temporary, &executable).expect("install plugin");
        assert_eq!(first.get("changed").and_then(Value::as_bool), Some(true));
        let plugin = temporary.join("plugins").join(PLUGIN_NAME);
        let mcp = fs::read_to_string(plugin.join(".mcp.json")).expect("mcp");
        let hooks = fs::read_to_string(plugin.join("hooks/hooks.json")).expect("hooks");
        let mcp_json: Value = serde_json::from_str(&mcp).expect("valid mcp json");
        assert_eq!(
            mcp_json
                .pointer("/mcpServers/xiaoli-model-monitor/command")
                .and_then(Value::as_str),
            Some(executable.to_string_lossy().as_ref())
        );
        assert_eq!(
            mcp_json.pointer("/mcpServers/xiaoli-model-monitor/args"),
            Some(&json!(["--mcp-server"]))
        );
        assert!(hooks.contains("--hook-capture"));
        assert!(!hooks.contains("--appimage-extract-and-run"));
        assert!(!mcp.to_ascii_lowercase().contains("node"));
        assert!(!hooks.to_ascii_lowercase().contains("node"));
        let unchanged = install_plugin_at(&temporary, &executable).expect("verify plugin");
        assert_eq!(
            unchanged.get("changed").and_then(Value::as_bool),
            Some(false)
        );
        fs::remove_file(plugin.join("hooks/hooks.json")).expect("remove generated hook");
        let repaired = install_plugin_at(&temporary, &executable).expect("repair plugin");
        assert_eq!(repaired.get("changed").and_then(Value::as_bool), Some(true));
        let _ = fs::remove_dir_all(&temporary);
    }

    #[test]
    fn plugin_install_never_overwrites_a_corrupt_marketplace() {
        let temporary = std::env::temp_dir().join(format!(
            "xiaoli-plugin-corrupt-marketplace-{}",
            std::process::id()
        ));
        let executable = if cfg!(windows) {
            PathBuf::from(r"C:\Portable XiaoLi\XiaoLi.exe")
        } else {
            PathBuf::from("/opt/xiaoli/xiaoli")
        };
        let _ = fs::remove_dir_all(&temporary);
        let marketplace = temporary.join(".agents/plugins/marketplace.json");
        fs::create_dir_all(marketplace.parent().unwrap()).unwrap();
        fs::write(&marketplace, b"{corrupt but precious").unwrap();
        let error = install_plugin_at(&temporary, &executable).unwrap_err();
        assert!(error.contains("invalid personal marketplace JSON"));
        assert_eq!(fs::read(&marketplace).unwrap(), b"{corrupt but precious");
        let _ = fs::remove_dir_all(&temporary);
    }

    #[test]
    fn plugin_uninstall_never_deletes_files_before_marketplace_validation() {
        let temporary = std::env::temp_dir().join(format!(
            "xiaoli-plugin-uninstall-corrupt-marketplace-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temporary);
        let plugin = temporary.join("plugins").join(PLUGIN_NAME);
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("keep-me.txt"), b"recoverable plugin").unwrap();
        let marketplace = temporary.join(".agents/plugins/marketplace.json");
        fs::create_dir_all(marketplace.parent().unwrap()).unwrap();
        fs::write(&marketplace, b"{corrupt but precious").unwrap();

        let error = uninstall_plugin_at_locked(&temporary).unwrap_err();
        assert!(error.contains("invalid personal marketplace JSON"));
        assert_eq!(
            fs::read(plugin.join("keep-me.txt")).unwrap(),
            b"recoverable plugin"
        );
        assert_eq!(fs::read(&marketplace).unwrap(), b"{corrupt but precious");
        let _ = fs::remove_dir_all(&temporary);
    }

    #[test]
    fn plugin_uninstall_restores_staged_tree_when_marketplace_write_fails() {
        let temporary = std::env::temp_dir().join(format!(
            "xiaoli-plugin-uninstall-rollback-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temporary);
        let plugin = temporary.join("plugins").join(PLUGIN_NAME);
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("keep-me.txt"), b"recoverable plugin").unwrap();
        let marketplace = temporary.join(".agents/plugins/marketplace.json");
        fs::create_dir_all(marketplace.parent().unwrap()).unwrap();
        write_json(
            &marketplace,
            &json!({"name":"personal","plugins":[{"name":PLUGIN_NAME}]}),
        )
        .unwrap();
        let original_marketplace = fs::read(&marketplace).unwrap();
        let (path, bytes) = prepare_personal_marketplace(&temporary, false).unwrap();

        let error = uninstall_plugin_transactionally(&plugin, &path, &bytes, |_, _| {
            Err("injected marketplace write failure".to_owned())
        })
        .unwrap_err();
        assert_eq!(error, "injected marketplace write failure");
        assert_eq!(
            fs::read(plugin.join("keep-me.txt")).unwrap(),
            b"recoverable plugin"
        );
        assert_eq!(fs::read(&marketplace).unwrap(), original_marketplace);
        let _ = fs::remove_dir_all(&temporary);
    }

    #[test]
    fn plugin_install_preserves_an_unrelated_model_monitor_registration() {
        let temporary = std::env::temp_dir().join(format!(
            "xiaoli-plugin-unrelated-predecessor-{}",
            std::process::id()
        ));
        let executable = if cfg!(windows) {
            PathBuf::from(r"C:\Portable XiaoLi\XiaoLi.exe")
        } else {
            PathBuf::from("/opt/xiaoli/xiaoli")
        };
        let _ = fs::remove_dir_all(&temporary);
        let marketplace = temporary.join(".agents/plugins/marketplace.json");
        fs::create_dir_all(marketplace.parent().unwrap()).unwrap();
        write_json(
            &marketplace,
            &json!({"name":"personal","plugins":[{
                "name":"model-monitor",
                "source":{"source":"local","path":"./plugins/someone-elses-monitor"}
            }]}),
        )
        .unwrap();

        install_plugin_at(&temporary, &executable).expect("install beside unrelated plugin");
        let updated: Value =
            serde_json::from_slice(&fs::read(&marketplace).unwrap()).expect("marketplace");
        let names = updated["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(names.contains(&"model-monitor"));
        assert!(names.contains(&PLUGIN_NAME));
        let _ = fs::remove_dir_all(&temporary);
    }

    #[test]
    fn plugin_install_migrates_only_the_fingerprinted_v3_predecessor() {
        let temporary = std::env::temp_dir().join(format!(
            "xiaoli-plugin-owned-predecessor-{}",
            std::process::id()
        ));
        let executable = if cfg!(windows) {
            PathBuf::from(r"C:\Portable XiaoLi\XiaoLi.exe")
        } else {
            PathBuf::from("/opt/xiaoli/xiaoli")
        };
        let _ = fs::remove_dir_all(&temporary);
        let legacy_manifest = temporary.join("plugins/model-monitor/.codex-plugin/plugin.json");
        fs::create_dir_all(legacy_manifest.parent().unwrap()).unwrap();
        write_json(
            &legacy_manifest,
            &json!({
                "name":"model-monitor",
                "version":"0.1.0",
                "description":"Read-only Codex model, effort, and token telemetry with explicit evidence labels.",
                "author":{"name":"Model Monitor contributors"}
            }),
        )
        .unwrap();
        let marketplace = temporary.join(".agents/plugins/marketplace.json");
        fs::create_dir_all(marketplace.parent().unwrap()).unwrap();
        write_json(
            &marketplace,
            &json!({"name":"personal","plugins":[{
                "name":"model-monitor",
                "source":{"source":"local","path":"./plugins/model-monitor"}
            }]}),
        )
        .unwrap();

        install_plugin_at(&temporary, &executable).expect("migrate owned predecessor");
        let updated: Value =
            serde_json::from_slice(&fs::read(&marketplace).unwrap()).expect("marketplace");
        let names = updated["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(!names.contains(&"model-monitor"));
        assert!(names.contains(&PLUGIN_NAME));
        let _ = fs::remove_dir_all(&temporary);
    }
}
