pub mod app;
pub mod collector;
pub mod ipc;
pub mod metrics;
pub mod model;
pub mod persistence;
pub mod runtime;

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
    time::Duration,
};

const PLUGIN_NAME: &str = "xiaoli-model-monitor";
const PLUGIN_VERSION: &str = "0.1.0-beta.3";
const MAX_HOOK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MCP_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
// Reserve most of the 150 ms fail-open budget for cold process startup and
// the atomic fallback write. A healthy local pipe normally answers in a few
// milliseconds; a busy collector must never hold up prompt submission.
const HOOK_DELIVERY_DEADLINE_MS: u64 = 40;

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
    let result = (|| -> Result<(), String> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAX_HOOK_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_HOOK_BYTES {
            return Err("hook_input_too_large".to_owned());
        }
        let input: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_json".to_owned())?;
        let Some(event) = sanitize_hook_input(&input) else {
            return Ok(());
        };
        if send_hook_request_with_deadline(event.clone()).is_err() {
            persist_hook_fallback(&event, fallback_dir)?;
        }
        Ok(())
    })();
    let _ = result;
    println!("{}", json!({"continue": true, "suppressOutput": true}));
}

fn send_hook_request_with_deadline(event: Value) -> Result<Value, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("xiaoli-hook-send".to_owned())
        .spawn(move || {
            let _ = sender.send(ipc::send_request(&event));
        })
        .map_err(|error| error.to_string())?;
    receiver
        .recv_timeout(Duration::from_millis(HOOK_DELIVERY_DEADLINE_MS))
        .map_err(|_| "hook_delivery_timeout".to_owned())?
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
    Some(json!({
        "event": event,
        "session": session,
        "turn": clean(first(input, &["turn_id", "turnId", "turn"]), 256),
        "model": clean(input.get("model"), 128),
        "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }))
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
    let mut params = json!({"threadId": thread_id});
    if let Some(turn_id) = optional_nonempty_string(args, "turnId")? {
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
        "schemaVersion": snapshot.get("schemaVersion").cloned().unwrap_or(json!(4)),
        "checkedAt": snapshot.get("checkedAt").cloned().unwrap_or(Value::Null),
        "snapshotSource": snapshot.get("snapshotSource").cloned().unwrap_or(Value::Null),
        "theme": theme,
        "threadId": thread_id,
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

pub(crate) fn install_plugin() -> Result<Value, String> {
    let executable = plugin_host_executable()?;
    let home = dirs::home_dir().ok_or_else(|| "home_directory_unavailable".to_owned())?;
    install_plugin_at(&home, &executable)
}

/// Resolve the stable executable that Codex should invoke after this process
/// exits. Linux AppImage payloads run from an ephemeral `/tmp/.mount_*` path;
/// the launcher exposes the persistent archive through `$APPIMAGE`.
fn plugin_host_executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    Ok(resolve_plugin_host_executable(
        current,
        appimage,
        cfg!(target_os = "linux"),
    ))
}

fn resolve_plugin_host_executable(
    current: PathBuf,
    appimage: Option<PathBuf>,
    prefer_appimage: bool,
) -> PathBuf {
    if prefer_appimage {
        if let Some(stable) = appimage
            .filter(|path| path.is_absolute())
            .and_then(|path| fs::canonicalize(path).ok())
        {
            return stable;
        }
    }
    current
}

fn install_plugin_at(home: &Path, executable: &Path) -> Result<Value, String> {
    if !executable.is_absolute() {
        return Err("executable_path_must_be_absolute".to_owned());
    }
    with_plugin_install_lock(home, || install_plugin_at_locked(home, executable))
}

fn install_plugin_at_locked(home: &Path, executable: &Path) -> Result<Value, String> {
    let plugin_root = home.join("plugins").join(PLUGIN_NAME);
    let was_current = plugin_installation_is_current(home, executable);
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
    write_json(&plugin_root.join(".mcp.json"), &mcp_manifest(executable))?;
    write_json(
        &plugin_root.join("hooks/hooks.json"),
        &hooks_manifest(executable),
    )?;
    write_text(
        &plugin_root.join("skills/model-monitor/SKILL.md"),
        PLUGIN_SKILL,
    )?;
    write_text(&plugin_root.join("README.md"), PLUGIN_README)?;
    write_text(&plugin_root.join("assets/icon.svg"), PLUGIN_ICON)?;
    update_personal_marketplace(home, true)?;

    if !plugin_installation_is_current(home, executable) {
        return Err("plugin_installation_verification_failed".to_owned());
    }

    Ok(json!({
        "ok": true, "action": "installed", "plugin": PLUGIN_NAME,
        "path": plugin_root, "executable": executable, "changed": !was_current
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

fn mcp_manifest(executable: &Path) -> Value {
    json!({"mcpServers": {PLUGIN_NAME: {
        "type": "stdio", "command": executable, "args": ["--mcp-server"],
        "startup_timeout_sec": 5, "tool_timeout_sec": 3
    }}})
}

fn hooks_manifest(executable: &Path) -> Value {
    let command = format!(
        "{} --hook-capture \"${{PLUGIN_DATA}}\"",
        shell_command_path(executable)
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

fn plugin_installation_is_current(home: &Path, executable: &Path) -> bool {
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
    ) || !json_matches(&plugin_root.join(".mcp.json"), &mcp_manifest(executable))
        || !json_matches(
            &plugin_root.join("hooks/hooks.json"),
            &hooks_manifest(executable),
        )
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
description: Inspect current Codex task request models, explicit server reroute evidence, requested effort, token usage, cache share, timing, and XiaoLi quality assessments through read-only MCP tools.
---

# 小狸模型监视

先调用 `get_monitor_summary`。需要单个任务证据时调用 `get_session_detail`；需要简洁卡片时调用 `render_monitor_card`。

- 请求模型和请求 effort 不是物理后端或实测思考强度证明。
- 只有明确的 `model/rerouted` 证据才能称为服务器已重路由。
- “未见服务器重路由”只代表小狸未捕获显式事件，不证明物理模型没有变化。
- token、缓存、耗时和行为偏离只能作为遥测或黄色疑似降质提示，不能伪装为服务器路由证据。
- 只把带有 `snapshotSource: liveMonitorIpc` 的工具结果作为当前状态；小狸离线时明确说明无法查询，不用磁盘旧快照代替。
- 不输出 prompt、回复正文、cwd、工具内容、transcript 或 rollout 路径。
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
        assert_eq!(value.as_object().map(|value| value.len()), Some(5));
    }

    #[test]
    fn mcp_server_lists_three_read_only_tools() {
        let response =
            handle_mcp_request(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
                .expect("response");
        let tools = response
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .expect("tools");
        assert_eq!(tools.len(), 3);
        assert_eq!(
            tools[0].get("name").and_then(Value::as_str),
            Some("get_monitor_summary")
        );
    }

    #[test]
    fn mcp_marks_live_ipc_results_and_refuses_offline_cache_as_current() {
        let live = query_monitor_with("get_monitor_summary", json!({}), |_| {
            Ok(json!({"schemaVersion": 4, "checkedAt": "2026-08-25T00:00:00Z", "conversations": []}))
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
            "schemaVersion": 4,
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
    fn appimage_plugin_path_uses_the_persistent_archive_not_the_mount() {
        let temporary =
            std::env::temp_dir().join(format!("xiaoli-appimage-path-test-{}", std::process::id()));
        let mounted = temporary.join(".mount_XiaoLi/usr/bin/xiaoli");
        let archive = temporary.join("XiaoLi.AppImage");
        let _ = fs::remove_dir_all(&temporary);
        fs::create_dir_all(mounted.parent().expect("mount parent")).expect("mount tree");
        fs::write(&mounted, b"ephemeral").expect("mounted executable");
        fs::write(&archive, b"persistent").expect("AppImage archive");

        let resolved = resolve_plugin_host_executable(mounted.clone(), Some(archive.clone()), true);
        assert_eq!(
            resolved,
            fs::canonicalize(&archive).expect("canonical archive")
        );
        assert!(!resolved.to_string_lossy().contains(".mount_"));

        let fallback = resolve_plugin_host_executable(
            mounted.clone(),
            Some(temporary.join("missing.AppImage")),
            true,
        );
        assert_eq!(fallback, mounted);
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
        assert!(hooks.contains("--hook-capture"));
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
