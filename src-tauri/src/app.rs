#[cfg(test)]
use crate::metrics::assess_behavior;
#[cfg(test)]
use crate::model::CompletedTurnSample;
use crate::{
    collector::RolloutCollector,
    ipc,
    metrics::{
        assess_quality_checkpoint, cache_input_share, eligible_baseline_sample, output_bucket,
        reasoning_active_ms, uncached_input_bucket, QualityGateState,
    },
    model::{
        BehaviorSampleV2, CollectorCache, ConversationSnapshot, FileState, HookObservation,
        ModelReroutedObservation, MonitorSnapshot, StatusLevel, ThreadKind, TokenUsage,
        TurnLifecycle, SNAPSHOT_SCHEMA_VERSION,
    },
    persistence::Persistence,
    runtime::{detect_codex_runtime, CodexRuntime, LaunchOptions},
};
use chrono::{SecondsFormat, Utc};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, RwLock,
    },
    thread,
    time::Duration,
};
use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, Monitor, PhysicalPosition, PhysicalSize, State,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;

const UI_PREFERENCES_VERSION: u32 = 2;
const UI_PREFERENCES_KEY: &str = "uiPreferencesV2";
#[cfg(windows)]
const LEGACY_IMPORT_MARKER_KEY: &str = "legacyImportV1";
const COMPACT_WIDTH: f64 = 304.0;
const COMPACT_HEIGHT: f64 = 72.0;
const COMPACT_MIN_WIDTH: f64 = 280.0;
const COMPACT_MIN_HEIGHT: f64 = 68.0;
const COMPACT_MAX_WIDTH: f64 = 520.0;
const COMPACT_MAX_HEIGHT: f64 = 120.0;
const EXPANDED_WIDTH: f64 = 440.0;
const EXPANDED_HEIGHT: f64 = 500.0;
const EXPANDED_MIN_WIDTH: f64 = 380.0;
const EXPANDED_MIN_HEIGHT: f64 = 300.0;
const EXPANDED_MAX_WIDTH: f64 = 760.0;
const EXPANDED_MAX_HEIGHT: f64 = 800.0;
const EXPANDED_WORK_AREA_FRACTION: f64 = 0.90;
const WINDOW_EDGE_MARGIN_DIP: f64 = 12.0;
const WINDOW_ANCHOR_SNAP_DIP: f64 = 24.0;
const WINDOW_EVENT_DEBOUNCE_MS: u64 = 400;
const WINDOW_INTERACTION_POLL_MS: u64 = 50;
#[cfg(any(windows, test))]
const PHYSICAL_LEFT_BUTTON_VK: i32 = 0x01;
#[cfg(any(windows, test))]
const PHYSICAL_RIGHT_BUTTON_VK: i32 = 0x02;
const TRAY_ICON_SIZE: u32 = 32;
const TRAY_BASE_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tray-base-32.rgba"));

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowBounds {
    width: f64,
    height: f64,
}

impl WindowBounds {
    const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowOffsetDip {
    x: f64,
    y: f64,
}

impl WindowOffsetDip {
    const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowPlacement {
    monitor_id: Option<String>,
    scale_factor: f64,
    anchor: String,
    offset_dip: WindowOffsetDip,
}

impl Default for WindowPlacement {
    fn default() -> Self {
        Self {
            monitor_id: None,
            scale_factor: 1.0,
            anchor: "topRight".to_owned(),
            offset_dip: WindowOffsetDip::new(WINDOW_EDGE_MARGIN_DIP, WINDOW_EDGE_MARGIN_DIP),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UiPreferencesV2 {
    version: u32,
    theme: String,
    topmost: bool,
    expanded: bool,
    compact_bounds: WindowBounds,
    expanded_bounds: WindowBounds,
    window_placement: WindowPlacement,
}

impl Default for UiPreferencesV2 {
    fn default() -> Self {
        Self {
            version: UI_PREFERENCES_VERSION,
            theme: "cute".to_owned(),
            topmost: true,
            // Expanded is intentionally a runtime preference: each launch starts compact.
            expanded: false,
            compact_bounds: WindowBounds::new(COMPACT_WIDTH, COMPACT_HEIGHT),
            expanded_bounds: WindowBounds::new(EXPANDED_WIDTH, EXPANDED_HEIGHT),
            window_placement: WindowPlacement::default(),
        }
    }
}

#[derive(Clone)]
struct TrayPreferenceItems {
    topmost: CheckMenuItem<tauri::Wry>,
    cute: CheckMenuItem<tauri::Wry>,
    minimal: CheckMenuItem<tauri::Wry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshNowResult {
    status: String,
    snapshot: MonitorSnapshot,
}

struct RefreshRequest {
    force_emit: bool,
    mutation: Option<RefreshMutation>,
    response: Option<mpsc::Sender<Result<RefreshNowResult, String>>>,
}

enum RefreshMutation {
    Hook(HookObservation),
    ServerReroute(ModelReroutedObservation),
}

impl RefreshRequest {
    fn background(force_emit: bool) -> Self {
        Self {
            force_emit,
            mutation: None,
            response: None,
        }
    }

    fn waiting(force_emit: bool, response: mpsc::Sender<Result<RefreshNowResult, String>>) -> Self {
        Self {
            force_emit,
            mutation: None,
            response: Some(response),
        }
    }

    fn hook(observation: HookObservation) -> Self {
        Self {
            force_emit: true,
            mutation: Some(RefreshMutation::Hook(observation)),
            response: None,
        }
    }

    fn server_reroute(observation: ModelReroutedObservation) -> Self {
        Self {
            force_emit: true,
            mutation: Some(RefreshMutation::ServerReroute(observation)),
            response: None,
        }
    }
}

struct RefreshCoreOutcome {
    changed: bool,
    snapshot: MonitorSnapshot,
}

pub struct MonitorAppState {
    collector: Mutex<RolloutCollector>,
    snapshot: RwLock<MonitorSnapshot>,
    persistence: Persistence,
    options: LaunchOptions,
    expanded: AtomicBool,
    topmost: AtomicBool,
    theme: Mutex<String>,
    window_preferences: Mutex<UiPreferencesV2>,
    tray_preferences: Mutex<Option<TrayPreferenceItems>>,
    window_event_sender: Mutex<Option<mpsc::Sender<bool>>>,
    shutting_down: AtomicBool,
    refresh_guard: Mutex<()>,
    refresh_sender: Mutex<Option<mpsc::Sender<RefreshRequest>>>,
    last_fingerprint: Mutex<String>,
    recorded_samples: Mutex<HashSet<String>>,
    recorded_samples_v2: Mutex<HashSet<String>>,
    quality_gates: Mutex<HashMap<(String, String), (String, QualityGateState)>>,
    hook_fallback_fingerprint: Mutex<Option<u64>>,
    plugin_install_status: RwLock<Option<Value>>,
    legacy_behavior_import_started: AtomicBool,
}

impl MonitorAppState {
    fn new(options: LaunchOptions, persistence: Persistence) -> Self {
        let mut collector = RolloutCollector::new(
            options.sessions_root.clone(),
            Some(options.session_index_path.clone()),
        );
        if let Ok(Some(cache)) = persistence.load_collector_cache_json() {
            if let Ok(cache) = serde_json::from_str::<CollectorCache>(&cache) {
                let _ = collector.restore_cache(cache);
            }
        }
        // Cold rollout discovery can be expensive on long-lived Codex profiles.
        // Keep it entirely on the refresh worker so the window and tray can
        // become responsive before the first scan finishes.
        let runtime = detect_codex_runtime();
        let mut snapshot = empty_snapshot();
        snapshot.codex_running = runtime.running;
        let stored_preferences = persistence
            .get_setting(UI_PREFERENCES_KEY)
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_str::<UiPreferencesV2>(&value).ok());
        let theme = stored_preferences
            .as_ref()
            .map(|value| value.theme.clone())
            .filter(|value| matches!(value.as_str(), "cute" | "minimal"))
            .or_else(|| {
                persistence
                    .get_setting("theme")
                    .ok()
                    .flatten()
                    .filter(|value| matches!(value.as_str(), "cute" | "minimal"))
            })
            .unwrap_or_else(|| "cute".to_owned());
        let topmost = stored_preferences
            .as_ref()
            .map(|value| value.topmost)
            .unwrap_or_else(|| {
                persistence
                    .get_setting("topmost")
                    .ok()
                    .flatten()
                    .is_none_or(|value| value != "false")
            });
        let mut window_preferences = stored_preferences.unwrap_or_default();
        window_preferences.version = UI_PREFERENCES_VERSION;
        window_preferences.theme = theme.clone();
        window_preferences.topmost = topmost;
        // A user-expanded card never makes the next login unexpectedly large.
        window_preferences.expanded = false;
        sanitize_preferences(&mut window_preferences);
        Self {
            collector: Mutex::new(collector),
            snapshot: RwLock::new(snapshot),
            persistence,
            options,
            expanded: AtomicBool::new(false),
            topmost: AtomicBool::new(topmost),
            theme: Mutex::new(theme),
            window_preferences: Mutex::new(window_preferences),
            tray_preferences: Mutex::new(None),
            window_event_sender: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
            refresh_guard: Mutex::new(()),
            refresh_sender: Mutex::new(None),
            last_fingerprint: Mutex::new(String::new()),
            recorded_samples: Mutex::new(HashSet::new()),
            recorded_samples_v2: Mutex::new(HashSet::new()),
            quality_gates: Mutex::new(HashMap::new()),
            hook_fallback_fingerprint: Mutex::new(None),
            plugin_install_status: RwLock::new(None),
            legacy_behavior_import_started: AtomicBool::new(false),
        }
    }
}

#[tauri::command]
pub fn get_snapshot(state: State<'_, Arc<MonitorAppState>>) -> Result<MonitorSnapshot, String> {
    state
        .snapshot
        .read()
        .map(|value| value.clone())
        .map_err(|_| "snapshot lock poisoned".to_owned())
}

#[tauri::command]
pub fn get_ui_preferences(
    state: State<'_, Arc<MonitorAppState>>,
) -> Result<UiPreferencesV2, String> {
    current_ui_preferences(&state)
}

#[tauri::command]
pub fn get_plugin_install_status(
    state: State<'_, Arc<MonitorAppState>>,
) -> Result<Option<Value>, String> {
    state
        .plugin_install_status
        .read()
        .map(|value| value.clone())
        .map_err(|_| "plugin install status lock poisoned".to_owned())
}

#[tauri::command]
pub fn toggle_expanded(
    app: AppHandle,
    state: State<'_, Arc<MonitorAppState>>,
) -> Result<bool, String> {
    let expanded = !state.expanded.load(Ordering::SeqCst);
    resize_window(&app, &state, expanded)?;
    Ok(expanded)
}

#[tauri::command]
pub fn reset_window_position(
    app: AppHandle,
    state: State<'_, Arc<MonitorAppState>>,
) -> Result<(), String> {
    reset_to_current_monitor_top_right(&app, &state)
}

#[tauri::command]
pub fn set_theme(
    app: AppHandle,
    state: State<'_, Arc<MonitorAppState>>,
    theme: String,
) -> Result<String, String> {
    apply_theme(&app, &state, &theme)
}

#[tauri::command]
pub fn hide_to_tray(app: AppHandle) -> Result<(), String> {
    app.get_webview_window("main")
        .ok_or_else(|| "main window unavailable".to_owned())?
        .hide()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn exit_app(app: AppHandle, state: State<'_, Arc<MonitorAppState>>) {
    begin_shutdown(&state);
    app.exit(0);
}

#[tauri::command]
pub fn set_topmost(
    app: AppHandle,
    state: State<'_, Arc<MonitorAppState>>,
    value: bool,
) -> Result<bool, String> {
    apply_topmost(&app, &state, value)?;
    Ok(value)
}

#[tauri::command]
pub async fn refresh_now(
    state: State<'_, Arc<MonitorAppState>>,
) -> Result<RefreshNowResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || request_refresh_and_wait(&state, true))
        .await
        .map_err(|error| format!("refresh worker join failed: {error}"))?
}

pub fn run(options: LaunchOptions) {
    if options.probe_once {
        run_probe_once(&options);
        return;
    }

    if options.stop {
        let result = ipc::send_request(&json!({
            "schemaVersion": 1,
            "method": "control",
            "params": { "action": "stop" }
        }));
        println!("{}", json!({"ok": result.is_ok(), "action": "stop"}));
        return;
    }

    // Claim the instance before opening SQLite or scanning rollout files. The
    // Tauri single-instance plugin initializes later in startup, which leaves
    // a cold-start race where a second `--show` process could enter the heavy
    // initialization path and wait indefinitely. A login-session mutex closes
    // that gap; the secondary retries the pipe only while the primary finishes
    // bringing IPC online.
    let instance_guard = if options.shadow {
        ipc::acquire_shadow_instance_guard(&options.state_root)
    } else {
        ipc::acquire_instance_guard()
    };
    let _instance_guard = match instance_guard {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            let should_activate = options.show || (!options.hidden && !options.shadow);
            if should_activate {
                let request = json!({
                    "schemaVersion": 1,
                    "method": "control",
                    "params": { "action": "show" }
                });
                for attempt in 0..100 {
                    if ipc::send_request(&request).is_ok() {
                        return;
                    }
                    if attempt < 99 {
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            return;
        }
        Err(error) => {
            eprintln!("XiaoLi instance mutex unavailable: {error}");
            let should_activate = options.show || (!options.hidden && !options.shadow);
            if should_activate {
                let _ = ipc::send_request(&json!({
                    "schemaVersion": 1,
                    "method": "control",
                    "params": { "action": "show" }
                }));
            }
            // Failing to create the OS instance guard is not evidence that no
            // other XiaoLi process exists. Starting without the guard would
            // allow two collectors to mutate the same SQLite/cache state, so
            // this path is deliberately fail-closed.
            return;
        }
    };

    let persistence = match Persistence::open(&options.state_root) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("XiaoLi state initialization failed: {error}");
            return;
        }
    };
    try_import_legacy_preferences(&options, &persistence);
    let state = Arc::new(MonitorAppState::new(options.clone(), persistence));
    let setup_state = state.clone();

    let mut builder = tauri::Builder::default().manage(state.clone());
    if !options.shadow {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = show_main_window(app, true);
        }));
    }
    let builder = builder
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .arg("--hidden")
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_ui_preferences,
            get_plugin_install_status,
            toggle_expanded,
            reset_window_position,
            set_theme,
            hide_to_tray,
            exit_app,
            set_topmost,
            refresh_now
        ])
        .setup(move |app| {
            setup_application(app, setup_state.clone())?;
            Ok(())
        });

    if let Err(error) = builder.run(tauri::generate_context!()) {
        eprintln!("XiaoLi application error: {error}");
    }
}

#[cfg(windows)]
fn should_import_legacy_state(options: &LaunchOptions, persistence: &Persistence) -> bool {
    if options.shadow
        || options.state_root != ipc::default_state_root()
        || persistence
            .get_setting(LEGACY_IMPORT_MARKER_KEY)
            .ok()
            .flatten()
            .is_some()
    {
        return false;
    }
    true
}

#[cfg(windows)]
fn legacy_state_candidates() -> Option<[PathBuf; 2]> {
    let local_data = dirs::data_local_dir()?;
    Some([
        local_data.join("OpenAI/Codex/model-monitor-v3/monitor.db"),
        local_data.join("Mochi Meter/monitor.db"),
    ])
}

#[cfg(windows)]
fn try_import_legacy_preferences(options: &LaunchOptions, persistence: &Persistence) {
    if !should_import_legacy_state(options, persistence) {
        return;
    }
    let Some(candidates) = legacy_state_candidates() else {
        return;
    };
    // Four indexed settings lookups per source are intentionally the only
    // migration work before Tauri creates the window.
    for candidate in candidates.iter().filter(|path| path.is_file()) {
        if let Err(error) = persistence.import_legacy_preferences(candidate) {
            eprintln!("XiaoLi legacy preferences skipped: {error}");
        }
    }
}

#[cfg(not(windows))]
fn try_import_legacy_preferences(_options: &LaunchOptions, _persistence: &Persistence) {}

#[cfg(windows)]
fn start_legacy_behavior_import(state: Arc<MonitorAppState>) {
    if !should_import_legacy_state(&state.options, &state.persistence) {
        return;
    }
    let Some(candidates) = legacy_state_candidates() else {
        return;
    };
    let _ = thread::Builder::new()
        .name("xiaoli-legacy-import".to_owned())
        .spawn(move || {
            let mut imported_sources = 0usize;
            let mut behavior_samples = 0usize;
            let mut behavior_samples_v2 = 0usize;
            for candidate in candidates.iter().filter(|path| path.is_file()) {
                match state.persistence.import_legacy_behavior_state(candidate) {
                    Ok(summary) => {
                        imported_sources += 1;
                        behavior_samples += summary.behavior_samples;
                        behavior_samples_v2 += summary.behavior_samples_v2;
                    }
                    Err(error) => eprintln!("XiaoLi legacy state import skipped: {error}"),
                }
            }
            if imported_sources > 0 {
                let marker = json!({
                    "version": 1,
                    "importedSources": imported_sources,
                    "behaviorSamples": behavior_samples,
                    "behaviorSamplesV2": behavior_samples_v2
                });
                let _ = state
                    .persistence
                    .set_setting(LEGACY_IMPORT_MARKER_KEY, &marker.to_string());
            }
        });
}

#[cfg(not(windows))]
fn start_legacy_behavior_import(_state: Arc<MonitorAppState>) {}

fn run_probe_once(options: &LaunchOptions) {
    let runtime = detect_codex_runtime();
    let mut collector = RolloutCollector::new(
        options.sessions_root.clone(),
        Some(options.session_index_path.clone()),
    );
    let snapshot = collector.scan_with_runtime(runtime.running, runtime.earliest_start_time);
    match serde_json::to_string_pretty(&snapshot) {
        Ok(json) => println!("{json}"),
        Err(error) => println!(
            "{}",
            json!({
                "schemaVersion": SNAPSHOT_SCHEMA_VERSION,
                "collectorHealth": {"level": "red", "lastError": error.to_string()}
            })
        ),
    }
}

fn setup_application(
    app: &mut tauri::App,
    state: Arc<MonitorAppState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    let window = app
        .get_webview_window("main")
        .ok_or("main window unavailable")?;
    window.set_skip_taskbar(true)?;
    window.set_resizable(true)?;
    window.set_always_on_top(state.topmost.load(Ordering::SeqCst))?;
    initialize_window_geometry(&window, &state)?;

    let initial = state
        .snapshot
        .read()
        .map(|value| value.clone())
        .unwrap_or_else(|_| empty_snapshot());
    if !state.options.shadow {
        create_tray(app, &state, &initial)?;
    }

    #[cfg(not(debug_assertions))]
    if !state.options.shadow
        && state
            .persistence
            .get_setting("autostart")
            .ok()
            .flatten()
            .is_none_or(|value| value != "false")
    {
        // Refresh the registration to the currently installed executable.
        // This also repairs a stale path left by a portable/shadow validation
        // build without creating a service or machine-wide task.
        if app.autolaunch().is_enabled().unwrap_or(false) {
            let _ = app.autolaunch().disable();
        }
        let _ = app.autolaunch().enable();
    }

    start_window_state_worker(handle.clone(), state.clone())?;
    let event_state = state.clone();
    let event_handle = handle.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { .. } => {
            begin_shutdown(&event_state);
            event_handle.exit(0);
        }
        tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
            queue_window_state_save(&event_state, false);
        }
        tauri::WindowEvent::ScaleFactorChanged { .. } => {
            queue_window_state_save(&event_state, true);
        }
        _ => {}
    });

    start_refresh_worker(handle.clone(), state.clone())?;
    if !state.options.shadow {
        start_ipc_listener(handle.clone(), state.clone())?;
    }
    start_plugin_install(handle.clone(), state.clone());
    start_refresh_loop(handle.clone(), state.clone());
    queue_refresh(&state, RefreshRequest::background(true)).map_err(std::io::Error::other)?;

    if state.options.show
        || (!state.options.hidden && !state.options.shadow && initial.codex_running)
    {
        let _ = show_main_window(&handle, state.options.show);
    }
    Ok(())
}

fn start_plugin_install(app: AppHandle, state: Arc<MonitorAppState>) {
    if state.options.shadow {
        return;
    }
    let failure_app = app.clone();
    let failure_state = state.clone();
    if let Err(error) = thread::Builder::new()
        .name("xiaoli-plugin-install".to_owned())
        .spawn(move || {
            let status = match crate::install_plugin() {
                Ok(detail) => {
                    let changed = detail
                        .get("changed")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    json!({
                        "ok": true,
                        "changed": changed,
                        "message": if changed {
                            "Codex 插件配置已写入；请在 Codex /hooks 中审阅并信任，已运行的 Codex 请新建任务或重启后加载"
                        } else {
                            "Codex 插件路径已检查"
                        },
                        "detail": detail
                    })
                }
                Err(error) => json!({
                    "ok": false,
                    "changed": false,
                    "message": "Codex 插件自动安装失败，可在菜单或 CLI 中重试",
                    "error": error.chars().take(240).collect::<String>()
                }),
            };
            if let Ok(mut slot) = state.plugin_install_status.write() {
                *slot = Some(status.clone());
            }
            let _ = app.emit("monitor://plugin-install", status);
        })
    {
        let status = json!({
            "ok": false,
            "changed": false,
            "message": "Codex 插件自动安装线程未能启动，可使用 xiaoli --install-plugin 重试",
            "error": error.to_string().chars().take(240).collect::<String>()
        });
        if let Ok(mut slot) = failure_state.plugin_install_status.write() {
            *slot = Some(status.clone());
        }
        let _ = failure_app.emit("monitor://plugin-install", status);
    }
}

fn create_tray(
    app: &tauri::App,
    state: &Arc<MonitorAppState>,
    snapshot: &MonitorSnapshot,
) -> tauri::Result<()> {
    let show_hide = MenuItem::with_id(app, "show_hide", "显示 / 隐藏", true, None::<&str>)?;
    let topmost = CheckMenuItem::with_id(
        app,
        "topmost",
        "保持置顶",
        true,
        state.topmost.load(Ordering::SeqCst),
        None::<&str>,
    )?;
    let refresh = MenuItem::with_id(app, "refresh", "立即刷新", true, None::<&str>)?;
    let reset_position = MenuItem::with_id(
        app,
        "reset_position",
        "重置到当前屏幕右上角",
        true,
        None::<&str>,
    )?;
    let theme = state
        .theme
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| "cute".to_owned());
    let cute = CheckMenuItem::with_id(
        app,
        "theme_cute",
        "手绘二次元主题",
        true,
        theme == "cute",
        None::<&str>,
    )?;
    let minimal = CheckMenuItem::with_id(
        app,
        "theme_minimal",
        "极简主题",
        true,
        theme == "minimal",
        None::<&str>,
    )?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "开机启动",
        true,
        app.autolaunch().is_enabled().unwrap_or(false),
        None::<&str>,
    )?;
    let exit = MenuItem::with_id(app, "exit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show_hide,
            &topmost,
            &refresh,
            &reset_position,
            &cute,
            &minimal,
            &autostart,
            &exit,
        ],
    )?;

    let tray_preferences = TrayPreferenceItems {
        topmost: topmost.clone(),
        cute: cute.clone(),
        minimal: minimal.clone(),
    };
    TrayIconBuilder::with_id("main")
        .icon(status_icon(snapshot))
        .tooltip("小狸 · XiaoLi · Codex 模型监视器")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                let _ = toggle_window_visibility(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| {
            let Some(state) = app.try_state::<Arc<MonitorAppState>>() else {
                return;
            };
            match event.id().as_ref() {
                "show_hide" => {
                    let _ = toggle_window_visibility(app);
                }
                "topmost" => {
                    let value = !state.topmost.load(Ordering::SeqCst);
                    let _ = apply_topmost(app, &state, value);
                }
                "refresh" => {
                    let _ = queue_refresh(&state, RefreshRequest::background(true));
                }
                "reset_position" => {
                    let _ = reset_to_current_monitor_top_right(app, &state);
                }
                "theme_cute" => {
                    let _ = apply_theme(app, &state, "cute");
                }
                "theme_minimal" => {
                    let _ = apply_theme(app, &state, "minimal");
                }
                "autostart" => {
                    let enabled = app.autolaunch().is_enabled().unwrap_or(false);
                    let result = if enabled {
                        app.autolaunch().disable()
                    } else {
                        app.autolaunch().enable()
                    };
                    if result.is_ok() {
                        let _ = state
                            .persistence
                            .set_setting("autostart", if enabled { "false" } else { "true" });
                    }
                }
                "exit" => {
                    begin_shutdown(&state);
                    app.exit(0);
                }
                _ => {}
            }
        })
        .build(app)?;
    if let Ok(mut handles) = state.tray_preferences.lock() {
        *handles = Some(tray_preferences);
    }
    Ok(())
}

fn start_ipc_listener(
    app: AppHandle,
    state: Arc<MonitorAppState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let state_root = state.options.state_root.clone();
    ipc::start_hook_listener(
        &state_root,
        Arc::new(move |payload| handle_ipc_message(&app, &state, payload)),
    )
    .map(|_| ())
    .map_err(|error| std::io::Error::other(error).into())
}

fn handle_ipc_message(
    app: &AppHandle,
    state: &Arc<MonitorAppState>,
    payload: &str,
) -> Result<Value, String> {
    let value: Value = serde_json::from_str(payload).map_err(|_| "invalid_json".to_owned())?;
    if let Some(method) = value.get("method").and_then(Value::as_str) {
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        return match method {
            "get_monitor_summary" => serde_json::to_value(
                state
                    .snapshot
                    .read()
                    .map_err(|_| "snapshot_lock_poisoned".to_owned())?
                    .clone(),
            )
            .map_err(|error| error.to_string()),
            "render_monitor_card" => {
                let snapshot = state
                    .snapshot
                    .read()
                    .map_err(|_| "snapshot_lock_poisoned".to_owned())?;
                project_monitor_card_snapshot(&snapshot, &params)
            }
            "get_session_detail" => {
                let snapshot = state
                    .snapshot
                    .read()
                    .map_err(|_| "snapshot_lock_poisoned".to_owned())?;
                project_session_detail(&snapshot, &params)
            }
            "control" => {
                match params.get("action").and_then(Value::as_str) {
                    Some("show") => show_main_window(app, true)?,
                    Some("hide") => {
                        if let Some(window) = app.get_webview_window("main") {
                            window.hide().map_err(|error| error.to_string())?;
                        }
                    }
                    Some("refresh") => {
                        queue_refresh(state, RefreshRequest::background(true))?;
                    }
                    Some("stop") => {
                        begin_shutdown(state);
                        let handle = app.clone();
                        thread::spawn(move || {
                            thread::sleep(Duration::from_millis(75));
                            handle.exit(0);
                        });
                    }
                    _ => return Err("unknown_control_action".to_owned()),
                }
                Ok(json!({"ok": true}))
            }
            "model/rerouted" => {
                let observation: ModelReroutedObservation = serde_json::from_value(params)
                    .map_err(|_| "invalid_reroute_event".to_owned())?;
                queue_refresh(state, RefreshRequest::server_reroute(observation))?;
                Ok(json!({"ok": true}))
            }
            _ => Err("unknown_method".to_owned()),
        };
    }

    let event = value
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let session = value
        .get("session")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if session.is_empty() {
        return Err("session_required".to_owned());
    }
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(now_iso);
    if event.eq_ignore_ascii_case("UserPromptSubmit")
        || event.eq_ignore_ascii_case("SessionStart")
        || event.eq_ignore_ascii_case("SubagentStart")
    {
        let observation = HookObservation {
            thread_id: session.to_owned(),
            turn_id: value.get("turn").and_then(Value::as_str).map(str::to_owned),
            model: value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
            observed_at: timestamp,
        };
        queue_refresh(state, RefreshRequest::hook(observation))?;
    } else {
        queue_refresh(state, RefreshRequest::background(true))?;
    }
    Ok(json!({"ok": true}))
}

/// Select one active conversation and every descendant without ever falling
/// back to a similarly named or newer task. `turn_id` is an optional freshness
/// guard for callers that must not accidentally inspect a later turn on the
/// same thread.
fn select_conversation_tree(
    snapshot: &MonitorSnapshot,
    thread_id: &str,
    turn_id: Option<&str>,
) -> Result<(ConversationSnapshot, Vec<ConversationSnapshot>), String> {
    let target = snapshot
        .conversations
        .iter()
        .find(|item| item.thread_id == thread_id && turn_id.is_none_or(|turn| item.turn_id == turn))
        .cloned()
        .ok_or_else(|| "active_conversation_not_found".to_owned())?;

    let mut selected_ids = HashSet::from([target.thread_id.clone()]);
    loop {
        let mut changed = false;
        for item in &snapshot.conversations {
            if item
                .parent_thread_id
                .as_ref()
                .is_some_and(|parent| selected_ids.contains(parent))
                && selected_ids.insert(item.thread_id.clone())
            {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let descendants = snapshot
        .conversations
        .iter()
        .filter(|item| item.thread_id != target.thread_id && selected_ids.contains(&item.thread_id))
        .cloned()
        .collect();
    Ok((target, descendants))
}

fn nonempty_param<'a>(params: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.as_str())),
        _ => Err(format!("invalid_{key}")),
    }
}

fn project_session_detail(snapshot: &MonitorSnapshot, params: &Value) -> Result<Value, String> {
    let thread_id =
        nonempty_param(params, "threadId")?.ok_or_else(|| "thread_id_required".to_owned())?;
    let turn_id = nonempty_param(params, "turnId")?;
    let (conversation, children) = select_conversation_tree(snapshot, thread_id, turn_id)?;
    Ok(json!({
        "schemaVersion": SNAPSHOT_SCHEMA_VERSION,
        "checkedAt": snapshot.checked_at,
        "conversation": conversation,
        "children": children
    }))
}

fn project_monitor_card_snapshot(
    snapshot: &MonitorSnapshot,
    params: &Value,
) -> Result<Value, String> {
    let theme = nonempty_param(params, "theme")?.unwrap_or("cute");
    if !matches!(theme, "cute" | "minimal") {
        return Err("invalid_theme".to_owned());
    }
    let thread_id = nonempty_param(params, "threadId")?;
    let conversations = if let Some(thread_id) = thread_id {
        let (conversation, mut descendants) = select_conversation_tree(snapshot, thread_id, None)?;
        let mut selected = Vec::with_capacity(descendants.len() + 1);
        selected.push(conversation);
        selected.append(&mut descendants);
        selected
    } else {
        snapshot.conversations.clone()
    };
    let mut projection = serde_json::to_value(MonitorSnapshot {
        conversations,
        ..snapshot.clone()
    })
    .map_err(|error| error.to_string())?;
    if let Some(object) = projection.as_object_mut() {
        object.insert("theme".to_owned(), Value::String(theme.to_owned()));
        if let Some(thread_id) = thread_id {
            object.insert(
                "projectionThreadId".to_owned(),
                Value::String(thread_id.to_owned()),
            );
        }
    }
    Ok(projection)
}

fn start_refresh_loop(app: AppHandle, state: Arc<MonitorAppState>) {
    thread::Builder::new()
        .name("mochi-rollout-watcher".to_owned())
        .spawn(move || {
            let (event_tx, event_rx) = mpsc::sync_channel::<()>(1);
            let callback_tx = event_tx.clone();
            let mut watcher =
                notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                    if event.is_ok() {
                        let _ = callback_tx.try_send(());
                    }
                })
                .ok();
            if let Some(watcher) = watcher.as_mut() {
                let watch_path = if state.options.sessions_root.exists() {
                    state.options.sessions_root.clone()
                } else {
                    state
                        .options
                        .sessions_root
                        .parent()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| state.options.sessions_root.clone())
                };
                let _ = watcher.watch(&watch_path, RecursiveMode::Recursive);
            }

            let mut was_running = state
                .snapshot
                .read()
                .map(|value| value.codex_running)
                .unwrap_or(false);
            let mut fallback_ticks = 0_u8;
            while !state.shutting_down.load(Ordering::SeqCst) {
                let changed_event = event_rx.recv_timeout(Duration::from_secs(5)).is_ok();
                if state.shutting_down.load(Ordering::SeqCst) {
                    break;
                }
                let runtime = detect_codex_runtime();
                let runtime_running = runtime.running;
                fallback_ticks = fallback_ticks.saturating_add(1);
                let periodic_recovery = fallback_ticks >= 6;
                // Filesystem events are the fast path. Every five seconds we
                // only check process state; a full rediscovery every 30 seconds
                // repairs missed watcher events without rereading log bodies.
                if changed_event || runtime_running != was_running || periodic_recovery {
                    let _ = queue_refresh(&state, RefreshRequest::background(false));
                    fallback_ticks = 0;
                }
                if periodic_recovery {
                    let recovery_app = app.clone();
                    let recovery_state = state.clone();
                    let _ = app.run_on_main_thread(move || {
                        if let Some(window) = recovery_app.get_webview_window("main") {
                            if window_geometry_needs_recovery(
                                &window,
                                recovery_state.expanded.load(Ordering::SeqCst),
                            ) {
                                let _ = recover_window_visibility(
                                    &window,
                                    recovery_state.expanded.load(Ordering::SeqCst),
                                );
                            }
                        }
                    });
                }
                // Process discovery is already current here; do not wait for
                // the asynchronously exchanged snapshot before showing the
                // compact window on a Codex start transition.
                if runtime_running && !was_running && !state.options.shadow {
                    let _ = show_main_window(&app, false);
                }
                was_running = runtime_running;
            }
        })
        .ok();
}

fn start_refresh_worker(app: AppHandle, state: Arc<MonitorAppState>) -> Result<(), String> {
    let (sender, receiver) = mpsc::channel::<RefreshRequest>();
    let worker_state = state.clone();
    thread::Builder::new()
        .name("xiaoli-refresh-worker".to_owned())
        .spawn(move || {
            run_refresh_scheduler(receiver, |force_emit, mutations| {
                let outcome =
                    refresh_once_with_runtime(&worker_state, detect_codex_runtime(), mutations)?;
                // Tauri's event and tray APIs may synchronously marshal work to
                // the UI thread. The refresh guard is intentionally gone before
                // this call, preventing the former UI <-> watcher lock cycle.
                publish_refresh(&app, &outcome.snapshot, outcome.changed || force_emit);
                // The first successfully exchanged snapshot is the startup
                // readiness boundary. Only now may a potentially large legacy
                // behavior import compete for SQLite; doing this from setup()
                // could lock the database before the user's first refresh.
                if !worker_state
                    .legacy_behavior_import_started
                    .swap(true, Ordering::SeqCst)
                {
                    start_legacy_behavior_import(worker_state.clone());
                }
                Ok(outcome.snapshot)
            });
        })
        .map_err(|error| error.to_string())?;
    *state
        .refresh_sender
        .lock()
        .map_err(|_| "refresh_sender_lock_poisoned".to_owned())? = Some(sender);
    Ok(())
}

fn queue_refresh(state: &Arc<MonitorAppState>, request: RefreshRequest) -> Result<(), String> {
    if state.shutting_down.load(Ordering::SeqCst) {
        return Err("refresh_worker_stopping".to_owned());
    }
    let sender = state
        .refresh_sender
        .lock()
        .map_err(|_| "refresh_sender_lock_poisoned".to_owned())?
        .clone()
        .ok_or_else(|| "refresh_worker_unavailable".to_owned())?;
    sender
        .send(request)
        .map_err(|_| "refresh_worker_stopped".to_owned())
}

fn request_refresh_and_wait(
    state: &Arc<MonitorAppState>,
    force_emit: bool,
) -> Result<RefreshNowResult, String> {
    let (response_tx, response_rx) = mpsc::channel();
    queue_refresh(state, RefreshRequest::waiting(force_emit, response_tx))?;
    response_rx
        .recv_timeout(Duration::from_secs(15))
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => "refresh_timeout".to_owned(),
            mpsc::RecvTimeoutError::Disconnected => "refresh_worker_stopped".to_owned(),
        })?
}

fn run_refresh_scheduler<F>(receiver: mpsc::Receiver<RefreshRequest>, mut execute: F)
where
    F: FnMut(bool, Vec<RefreshMutation>) -> Result<MonitorSnapshot, String>,
{
    while let Ok(first) = receiver.recv() {
        let mut batch = vec![first];
        while let Ok(request) = receiver.try_recv() {
            batch.push(request);
        }

        let force_emit = batch.iter().any(|request| request.force_emit);
        let mutations = batch
            .iter_mut()
            .filter_map(|request| request.mutation.take())
            .collect();
        let result = execute(force_emit, mutations);
        for (index, request) in batch.into_iter().enumerate() {
            let Some(response) = request.response else {
                continue;
            };
            let response_value = result.as_ref().map(|snapshot| RefreshNowResult {
                status: if index == 0 {
                    "completed".to_owned()
                } else {
                    "coalesced".to_owned()
                },
                snapshot: snapshot.clone(),
            });
            let _ = response.send(response_value.map_err(Clone::clone));
        }
    }
}

fn refresh_once_with_runtime(
    state: &Arc<MonitorAppState>,
    runtime: CodexRuntime,
    mutations: Vec<RefreshMutation>,
) -> Result<RefreshCoreOutcome, String> {
    let _refresh = state
        .refresh_guard
        .lock()
        .map_err(|_| "refresh_lock_poisoned".to_owned())?;
    let (mut snapshot, cache, samples, samples_v2, active_turn_evidence) = {
        let mut collector = state
            .collector
            .lock()
            .map_err(|_| "collector_lock_poisoned".to_owned())?;
        if let Some(observation) = read_hook_fallback(state) {
            collector.observe_hook(observation);
        }
        for mutation in mutations {
            match mutation {
                RefreshMutation::Hook(observation) => collector.observe_hook(observation),
                RefreshMutation::ServerReroute(observation) => {
                    collector.observe_server_reroute(observation)
                }
            }
        }
        let snapshot = collector.scan_with_runtime(runtime.running, runtime.earliest_start_time);
        let file_states = collector.export_file_states();
        let cache = collector.export_cache();
        let samples = collector
            .completed_turn_samples()
            .cloned()
            .collect::<Vec<_>>();
        let samples_v2 = collector
            .completed_behavior_samples_v2()
            .cloned()
            .collect::<Vec<_>>();
        let active_turn_evidence = selected_active_turn_evidence(&file_states);
        (snapshot, cache, samples, samples_v2, active_turn_evidence)
    };
    record_completed_samples_v2(state, &samples_v2);
    apply_quality_assessments(state, &mut snapshot, &active_turn_evidence);

    let fingerprint = stable_fingerprint(&snapshot)?;
    let changed = {
        let mut previous = state
            .last_fingerprint
            .lock()
            .map_err(|_| "fingerprint_lock_poisoned".to_owned())?;
        let changed = *previous != fingerprint;
        if changed {
            *previous = fingerprint;
        }
        changed
    };

    if changed {
        state
            .persistence
            .save_snapshot(&snapshot, &snapshot.checked_at)?;
        state
            .persistence
            .save_collector_cache(&cache, &snapshot.checked_at)?;
        record_completed_samples(state, &samples);
        state
            .persistence
            .append_monitor_log(&legacy_log_record(&snapshot))?;
    }
    *state
        .snapshot
        .write()
        .map_err(|_| "snapshot_lock_poisoned".to_owned())? = snapshot.clone();

    Ok(RefreshCoreOutcome { changed, snapshot })
}

/// The hook is fail-open and can outlive the GUI IPC endpoint. Consume its
/// atomic metadata-only fallback on the next refresh, de-duplicated by content
/// fingerprint. The file is deliberately retained so a crash between parsing
/// and snapshot persistence cannot lose the latest request observation.
fn read_hook_fallback(state: &Arc<MonitorAppState>) -> Option<HookObservation> {
    let bytes = fs::read(state.options.state_root.join("hook-latest.json")).ok()?;
    if bytes.len() > 16 * 1024 {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let fingerprint = hasher.finish();
    let mut seen = state.hook_fallback_fingerprint.lock().ok()?;
    if *seen == Some(fingerprint) {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    *seen = Some(fingerprint);
    if value.get("event").and_then(Value::as_str) != Some("UserPromptSubmit") {
        return None;
    }
    let clean = |key: &str, limit: usize| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| text.chars().take(limit).collect::<String>())
    };
    Some(HookObservation {
        thread_id: clean("session", 256)?,
        turn_id: clean("turn", 256),
        model: clean("model", 128),
        observed_at: clean("timestamp", 64)?,
    })
}

fn publish_refresh(app: &AppHandle, snapshot: &MonitorSnapshot, should_publish: bool) {
    if !should_publish {
        return;
    }
    let _ = app.emit("monitor://snapshot", snapshot);
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_icon(Some(status_icon(snapshot)));
        let _ = tray.set_tooltip(Some(tray_tooltip(snapshot)));
    }
}

#[derive(Clone, Debug)]
struct ActiveTurnEvidence {
    usage: TokenUsage,
    reasoning_active_ms: Option<u64>,
    clean: bool,
}

fn apply_quality_assessments(
    state: &Arc<MonitorAppState>,
    snapshot: &mut MonitorSnapshot,
    active_turn_evidence: &HashMap<(String, String), ActiveTurnEvidence>,
) {
    let checked_at_ms = Utc::now().timestamp_millis().max(0) as u64;
    let mut active_keys = HashSet::new();
    let Ok(mut gates) = state.quality_gates.lock() else {
        return;
    };

    for conversation in &mut snapshot.conversations {
        let key = (conversation.thread_id.clone(), conversation.turn_id.clone());
        active_keys.insert(key.clone());
        let Some(evidence) = active_turn_evidence.get(&key) else {
            continue;
        };
        let Some(sample) = active_quality_sample(conversation, &snapshot.checked_at, evidence)
        else {
            continue;
        };
        let baseline_key = sample.baseline_key();
        let history = state
            .persistence
            .load_behavior_samples_v2::<BehaviorSampleV2>(&baseline_key)
            .unwrap_or_default();
        let mut comparator_template = sample.clone();
        comparator_template.model = "gpt-5.5".to_owned();
        let comparator_history = state
            .persistence
            .load_behavior_samples_v2::<BehaviorSampleV2>(&comparator_template.baseline_key())
            .unwrap_or_default();
        let previous = gates
            .get(&key)
            .filter(|(stored_key, _)| stored_key == &baseline_key)
            .map(|(_, gate)| gate.clone())
            .unwrap_or_default();
        let evaluation = assess_quality_checkpoint(
            &history,
            &comparator_history,
            &sample,
            &previous,
            checked_at_ms,
        );
        gates.insert(key, (baseline_key, evaluation.gate));
        conversation.quality_assessment = evaluation.assessment;

        if conversation.quality_assessment.state != "suspectedDegradation" {
            continue;
        }
        let labels = conversation
            .quality_assessment
            .factors
            .iter()
            .map(|factor| match factor.code.as_str() {
                "ttftHigh" => "TTFT 偏高",
                "modelPhaseOutputRateLow" => "模型阶段速率偏低",
                "reasoningOutputShareLow" => "推理输出占比偏低",
                "reasoningPhaseShareLow" => "推理阶段占比偏低",
                _ => "行为指标偏离",
            })
            .collect::<Vec<_>>();
        conversation.anomalies.push(format!(
            "疑似降质：{}（{} 个同桶健康样本，连续命中 {}）；仅为本机统计提醒，不能证明实际模型或 effort",
            labels.join("、"),
            conversation.quality_assessment.baseline_sample_count,
            conversation.quality_assessment.consecutive_hits
        ));
        if conversation.status.level == StatusLevel::Green {
            conversation.status.level = StatusLevel::Yellow;
            conversation.status.code = "suspected_degradation".to_owned();
            conversation.status.explanation =
                "至少两个独立行为信号连续偏离本机同配置历史；点击查看统计原因和证据限制".to_owned();
        }
    }
    gates.retain(|key, _| active_keys.contains(key));
}

fn active_quality_sample(
    conversation: &crate::model::ConversationSnapshot,
    checked_at: &str,
    evidence: &ActiveTurnEvidence,
) -> Option<BehaviorSampleV2> {
    let usage = &evidence.usage;
    if usage.is_empty() {
        return None;
    }
    let model_active_ms = conversation.timing.model_active_ms;
    let reasoning_output_share = (usage.output_tokens > 0)
        .then_some(usage.reasoning_output_tokens as f64 / usage.output_tokens as f64);
    let reasoning_phase_share = match (evidence.reasoning_active_ms, model_active_ms) {
        (Some(reasoning_ms), Some(model_ms)) if model_ms > 0 => {
            Some(reasoning_ms as f64 / model_ms as f64)
        }
        _ => None,
    };
    Some(BehaviorSampleV2 {
        thread_id: conversation.thread_id.clone(),
        turn_id: conversation.turn_id.clone(),
        model: conversation
            .active_request
            .model
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
        effort: conversation
            .active_request
            .effort
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
        uncached_input_bucket: uncached_input_bucket(usage.input_tokens, usage.cached_input_tokens)
            .to_owned(),
        output_bucket: output_bucket(usage.output_tokens).to_owned(),
        tool_activity: conversation.tool_activity,
        output_tokens: usage.output_tokens,
        // The lower edge is deliberately conservative: only a lower bound
        // that is already anomalously high may vote while the turn is active.
        ttft_ms: conversation
            .timing
            .ttft_ms
            .or(conversation.timing.ttft_evidence.lower_ms),
        model_phase_output_rate: conversation.timing.model_phase_output_rate,
        reasoning_output_share,
        reasoning_phase_share,
        cache_input_share: cache_input_share(usage),
        clean: evidence.clean,
        explicit_reroute: conversation
            .server_route
            .evidence
            .eq_ignore_ascii_case("explicitReroute"),
        observed_at: checked_at.to_owned(),
    })
}

fn selected_active_turn_evidence(
    files: &[FileState],
) -> HashMap<(String, String), ActiveTurnEvidence> {
    let mut selected: HashMap<&str, &FileState> = HashMap::new();
    for file in files {
        let Some(thread_id) = file.thread_id.as_deref() else {
            continue;
        };
        let replace = selected.get(thread_id).is_none_or(|current| {
            file.segment_start_ordinal > current.segment_start_ordinal
                || (file.segment_start_ordinal == current.segment_start_ordinal
                    && file.last_write_ms > current.last_write_ms)
        });
        if replace {
            selected.insert(thread_id, file);
        }
    }
    selected
        .into_iter()
        .filter_map(|(thread_id, file)| {
            if !file.identity_known || file.identity_rejected {
                return None;
            }
            let turn = file.current_turn.as_ref()?;
            if turn.lifecycle != TurnLifecycle::Active {
                return None;
            }
            Some((
                (thread_id.to_owned(), turn.turn_id.clone()),
                ActiveTurnEvidence {
                    usage: turn.usage_turn.clone(),
                    reasoning_active_ms: reasoning_active_ms(&turn.model_intervals),
                    clean: file.parse_warnings == turn.parse_warnings_at_start,
                },
            ))
        })
        .collect()
}

/// Mirrors the collector's paginated-rollout selection without changing the
/// public snapshot contract. Only the selected active segment may contribute a
/// turn-local delta to behavior assessment.
#[cfg(test)]
fn selected_active_turn_usage(files: &[FileState]) -> HashMap<(String, String), TokenUsage> {
    let mut selected: HashMap<&str, &FileState> = HashMap::new();
    for file in files {
        let Some(thread_id) = file.thread_id.as_deref() else {
            continue;
        };
        let replace = selected.get(thread_id).is_none_or(|current| {
            file.segment_start_ordinal > current.segment_start_ordinal
                || (file.segment_start_ordinal == current.segment_start_ordinal
                    && file.last_write_ms > current.last_write_ms)
        });
        if replace {
            selected.insert(thread_id, file);
        }
    }

    selected
        .into_iter()
        .filter_map(|(thread_id, file)| {
            if !file.identity_known || file.identity_rejected {
                return None;
            }
            let turn = file.current_turn.as_ref()?;
            if turn.lifecycle != TurnLifecycle::Active {
                return None;
            }
            Some((
                (thread_id.to_owned(), turn.turn_id.clone()),
                turn.usage_turn.clone(),
            ))
        })
        .collect()
}

#[cfg(test)]
fn active_behavior_sample(
    conversation: &crate::model::ConversationSnapshot,
    checked_at: &str,
    turn_usage: &TokenUsage,
) -> Option<CompletedTurnSample> {
    if turn_usage.is_empty() {
        return None;
    }
    Some(CompletedTurnSample {
        thread_id: conversation.thread_id.clone(),
        turn_id: conversation.turn_id.clone(),
        kind: conversation.kind,
        model: conversation.active_request.model.clone(),
        effort: conversation.active_request.effort.clone(),
        input_bucket: behavior_input_bucket(turn_usage.input_tokens).to_owned(),
        tool_activity: conversation.tool_activity,
        ttft_ms: conversation.timing.ttft_ms,
        duration_ms: conversation
            .timing
            .duration_ms
            .or(conversation.timing.elapsed_ms),
        input_tokens: turn_usage.input_tokens,
        output_tokens: turn_usage.output_tokens,
        reasoning_output_tokens: turn_usage.reasoning_output_tokens,
        cache_input_share: cache_input_share(turn_usage),
        completed_at: checked_at.to_owned(),
    })
}

#[cfg(test)]
fn behavior_input_bucket(input_tokens: u64) -> &'static str {
    match input_tokens {
        0..=8_191 => "0-8k",
        8_192..=32_767 => "8k-32k",
        32_768..=131_071 => "32k-128k",
        _ => "128k+",
    }
}

fn record_completed_samples_v2(state: &Arc<MonitorAppState>, samples: &[BehaviorSampleV2]) {
    let Ok(mut seen) = state.recorded_samples_v2.lock() else {
        return;
    };
    for sample in samples
        .iter()
        .filter(|sample| eligible_baseline_sample(sample))
    {
        let key = format!(
            "{}:{}:{}",
            sample.thread_id, sample.turn_id, sample.observed_at
        );
        if seen.contains(&key) {
            continue;
        }
        let bucket = sample.baseline_key();
        if state
            .persistence
            .append_behavior_sample_v2(&bucket, &sample.observed_at, sample)
            .is_ok()
        {
            seen.insert(key);
        }
    }
}

fn record_completed_samples(
    state: &Arc<MonitorAppState>,
    samples: &[crate::model::CompletedTurnSample],
) {
    let Ok(mut seen) = state.recorded_samples.lock() else {
        return;
    };
    for sample in samples {
        let key = format!(
            "{}:{}:{}",
            sample.thread_id, sample.turn_id, sample.completed_at
        );
        if seen.contains(&key) {
            continue;
        }
        let bucket = format!(
            "{}|{}|{}|{}",
            sample
                .model
                .as_deref()
                .unwrap_or("unknown")
                .to_ascii_lowercase(),
            sample
                .effort
                .as_deref()
                .unwrap_or("unknown")
                .to_ascii_lowercase(),
            sample.input_bucket,
            if sample.tool_activity {
                "tools"
            } else {
                "no-tools"
            }
        );
        if state
            .persistence
            .append_behavior_sample(&bucket, &sample.completed_at, sample)
            .is_ok()
        {
            seen.insert(key);
        }
    }
}

fn stable_fingerprint(snapshot: &MonitorSnapshot) -> Result<String, String> {
    let mut value = serde_json::to_value(snapshot).map_err(|error| error.to_string())?;
    if let Some(root) = value.as_object_mut() {
        root.remove("checkedAt");
        if let Some(conversations) = root.get_mut("conversations").and_then(Value::as_array_mut) {
            for conversation in conversations {
                if let Some(timing) = conversation
                    .get_mut("timing")
                    .and_then(Value::as_object_mut)
                {
                    timing.remove("elapsedMs");
                }
            }
        }
    }
    serde_json::to_string(&value).map_err(|error| error.to_string())
}

fn legacy_log_record(snapshot: &MonitorSnapshot) -> Value {
    let mut models = snapshot
        .conversations
        .iter()
        .filter_map(|item| {
            item.server_route
                .model
                .as_ref()
                .or(item.active_request.model.as_ref())
        })
        .map(|value| value.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut efforts = snapshot
        .conversations
        .iter()
        .filter_map(|item| item.active_request.effort.as_ref())
        .map(|value| value.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let model = if models.len() == 1 {
        models
            .drain()
            .next()
            .unwrap_or_else(|| "unknown".to_owned())
    } else if models.is_empty() {
        "unknown".to_owned()
    } else {
        "mixed".to_owned()
    };
    let effort = if efforts.len() == 1 {
        efforts
            .drain()
            .next()
            .unwrap_or_else(|| "unknown".to_owned())
    } else if efforts.is_empty() {
        "unknown".to_owned()
    } else {
        "mixed".to_owned()
    };
    let level = overall_level(snapshot);
    let conversations = snapshot
        .conversations
        .iter()
        .map(|item| {
            json!({
                "threadId": item.thread_id,
                "turnId": item.turn_id,
                "parentThreadId": item.parent_thread_id,
                "kind": item.kind,
                "title": item.title,
                "requestedModel": item.active_request.model,
                "requestedEffort": item.active_request.effort,
                "routedModel": item.server_route.model,
                "routeEvidence": item.server_route.evidence,
                "usage": item.usage,
                "timing": item.timing,
                "qualityAssessment": item.quality_assessment,
                "status": item.status,
                "anomalies": item.anomalies
            })
        })
        .collect::<Vec<_>>();
    json!({
        "timestamp": snapshot.checked_at,
        "event": "snapshot_changed",
        "state": format!("{:?}", level).to_ascii_lowercase(),
        "model": model,
        "effort": effort,
        "sessions": snapshot.conversations.len(),
        "dir": "",
        "schemaVersion": SNAPSHOT_SCHEMA_VERSION,
        "health": snapshot.collector_health,
        "conversations": conversations
    })
}

fn overall_level(snapshot: &MonitorSnapshot) -> StatusLevel {
    if snapshot.collector_health.level == StatusLevel::Red
        || snapshot
            .conversations
            .iter()
            .any(|item| item.status.level == StatusLevel::Red)
    {
        StatusLevel::Red
    } else if snapshot.collector_health.level == StatusLevel::Yellow
        || snapshot
            .conversations
            .iter()
            .any(|item| item.status.level == StatusLevel::Yellow)
    {
        StatusLevel::Yellow
    } else if snapshot
        .conversations
        .iter()
        .any(|item| item.status.level == StatusLevel::Green)
    {
        StatusLevel::Green
    } else {
        StatusLevel::Gray
    }
}

fn status_icon(snapshot: &MonitorSnapshot) -> Image<'static> {
    Image::new_owned(
        render_tray_status_rgba(overall_level(snapshot)),
        TRAY_ICON_SIZE,
        TRAY_ICON_SIZE,
    )
}

fn render_tray_status_rgba(level: StatusLevel) -> Vec<u8> {
    debug_assert_eq!(
        TRAY_BASE_RGBA.len(),
        (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize
    );
    let mut rgba = vec![0_u8; (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize];
    let status_color = match level {
        StatusLevel::Green => [47, 156, 106, 255],
        StatusLevel::Yellow => [184, 120, 18, 255],
        StatusLevel::Red => [216, 72, 97, 255],
        StatusLevel::Gray => [129, 120, 130, 255],
    };

    // The colored ring remains one physical pixel after Windows scales 32px
    // down to a 16px taskbar. A warm outer highlight and dark separator keep
    // the silhouette readable against both light and dark taskbars.
    paint_annulus(&mut rgba, [255, 248, 243, 238], 15.05, 15.85);
    paint_annulus(&mut rgba, [69, 60, 72, 250], 14.20, 15.15);
    paint_annulus(&mut rgba, status_color, 12.55, 14.35);
    composite_rgba(&mut rgba, TRAY_BASE_RGBA);
    // Reassert the status edge over the mascot's outer antialiasing so the
    // state never disappears when the curl approaches the circular border.
    paint_annulus(&mut rgba, status_color, 13.10, 14.30);
    paint_annulus(&mut rgba, [69, 60, 72, 245], 14.35, 15.10);
    paint_annulus(&mut rgba, [255, 248, 243, 225], 15.10, 15.80);
    rgba
}

fn paint_annulus(rgba: &mut [u8], color: [u8; 4], inner_radius: f64, outer_radius: f64) {
    let center = (TRAY_ICON_SIZE as f64 - 1.0) / 2.0;
    for y in 0..TRAY_ICON_SIZE {
        for x in 0..TRAY_ICON_SIZE {
            let dx = x as f64 - center;
            let dy = y as f64 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let outer_coverage = (outer_radius + 0.5 - distance).clamp(0.0, 1.0);
            let inner_coverage = (distance - inner_radius + 0.5).clamp(0.0, 1.0);
            let coverage = outer_coverage.min(inner_coverage);
            if coverage <= 0.0 {
                continue;
            }
            let mut source = color;
            source[3] = (color[3] as f64 * coverage).round() as u8;
            let index = ((y * TRAY_ICON_SIZE + x) * 4) as usize;
            composite_pixel(&mut rgba[index..index + 4], source);
        }
    }
}

fn composite_rgba(destination: &mut [u8], source: &[u8]) {
    if destination.len() != source.len() {
        return;
    }
    for (destination, source) in destination.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
        composite_pixel(destination, [source[0], source[1], source[2], source[3]]);
    }
}

fn composite_pixel(destination: &mut [u8], source: [u8; 4]) {
    let source_alpha = source[3] as f64 / 255.0;
    if source_alpha <= 0.0 {
        return;
    }
    let destination_alpha = destination[3] as f64 / 255.0;
    let output_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
    for channel in 0..3 {
        let value = (source[channel] as f64 * source_alpha
            + destination[channel] as f64 * destination_alpha * (1.0 - source_alpha))
            / output_alpha;
        destination[channel] = value.round().clamp(0.0, 255.0) as u8;
    }
    destination[3] = (output_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn tray_tooltip(snapshot: &MonitorSnapshot) -> String {
    if !snapshot.codex_running {
        return "小狸 · Codex 未运行".to_owned();
    }
    let roots = snapshot
        .conversations
        .iter()
        .filter(|item| item.kind == ThreadKind::Root)
        .collect::<Vec<_>>();
    let subtask_count = snapshot
        .conversations
        .iter()
        .filter(|item| item.kind == ThreadKind::Subagent)
        .count();
    match (roots.len(), subtask_count) {
        (0, 0) => "小狸 · 等待任务".to_owned(),
        (0, subtasks) => {
            format!("小狸 · 0 个活动对话 · {subtasks} 个子任务（父会话未找到）")
        }
        (1, 0) => {
            let item = roots[0];
            format!(
                "小狸 · {} · {}（请求）",
                item.active_request.model.as_deref().unwrap_or("模型未知"),
                item.active_request
                    .effort
                    .as_deref()
                    .unwrap_or("effort 未知")
            )
        }
        (1, subtasks) => {
            let item = roots[0];
            format!(
                "小狸 · 1 个活动对话 · {subtasks} 个子任务 · {} · {}（请求）",
                item.active_request.model.as_deref().unwrap_or("模型未知"),
                item.active_request
                    .effort
                    .as_deref()
                    .unwrap_or("effort 未知")
            )
        }
        (root_count, 0) => format!("小狸 · {root_count} 个活动对话"),
        (root_count, subtasks) => {
            format!("小狸 · {root_count} 个活动对话 · {subtasks} 个子任务")
        }
    }
}

fn resize_window(
    app: &AppHandle,
    state: &Arc<MonitorAppState>,
    expanded: bool,
) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window unavailable".to_owned())?;
    let old_expanded = state.expanded.load(Ordering::SeqCst);
    if old_expanded == expanded {
        return Ok(());
    }

    let monitor = best_monitor_for_window(&window)
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor is available".to_owned())?;
    let old_position = window.outer_position().map_err(|error| error.to_string())?;
    let old_size = window.outer_size().map_err(|error| error.to_string())?;
    let placement = placement_from_geometry(&monitor, old_position, old_size);

    update_mode_bounds(
        state,
        old_expanded,
        logical_bounds(old_size, monitor.scale_factor()),
    )?;
    state.expanded.store(expanded, Ordering::SeqCst);
    {
        let mut preferences = state
            .window_preferences
            .lock()
            .map_err(|_| "window preferences lock poisoned".to_owned())?;
        preferences.expanded = expanded;
        preferences.window_placement = placement.clone();
    }

    apply_mode_constraints(&window, expanded, &monitor)?;
    let requested = saved_bounds_for_mode(state, expanded)?;
    let target = clamp_bounds_for_mode(requested, expanded, &monitor);
    let target_physical = physical_size(target, monitor.scale_factor());
    let target_position = position_from_placement(&monitor, &placement, target_physical);
    window
        .set_size(LogicalSize::new(target.width, target.height))
        .map_err(|error| error.to_string())?;
    window
        .set_position(target_position)
        .map_err(|error| error.to_string())?;
    recover_window_visibility(&window, expanded)?;
    capture_window_state(app, state)?;
    Ok(())
}

fn show_main_window(app: &AppHandle, focus: bool) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window unavailable".to_owned())?;
    window.show().map_err(|error| error.to_string())?;
    window
        .set_skip_taskbar(true)
        .map_err(|error| error.to_string())?;
    recover_window_visibility(&window, app_state_expanded(app))?;
    if focus {
        window.set_focus().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn toggle_window_visibility(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window unavailable".to_owned())?;
    if window.is_visible().map_err(|error| error.to_string())? {
        window.hide().map_err(|error| error.to_string())
    } else {
        show_main_window(app, true)
    }
}

fn apply_theme(
    app: &AppHandle,
    state: &Arc<MonitorAppState>,
    theme: &str,
) -> Result<String, String> {
    if !matches!(theme, "cute" | "minimal") {
        return Err("theme must be cute or minimal".to_owned());
    }
    *state
        .theme
        .lock()
        .map_err(|_| "theme_lock_poisoned".to_owned())? = theme.to_owned();
    state
        .window_preferences
        .lock()
        .map_err(|_| "window preferences lock poisoned".to_owned())?
        .theme = theme.to_owned();
    state.persistence.set_setting("theme", theme)?;
    app.emit("monitor://theme", theme)
        .map_err(|error| error.to_string())?;
    persist_and_emit_preferences(app, state)?;
    Ok(theme.to_owned())
}

fn apply_topmost(app: &AppHandle, state: &Arc<MonitorAppState>, value: bool) -> Result<(), String> {
    app.get_webview_window("main")
        .ok_or_else(|| "main window unavailable".to_owned())?
        .set_always_on_top(value)
        .map_err(|error| error.to_string())?;
    state.topmost.store(value, Ordering::SeqCst);
    state
        .window_preferences
        .lock()
        .map_err(|_| "window preferences lock poisoned".to_owned())?
        .topmost = value;
    state
        .persistence
        .set_setting("topmost", if value { "true" } else { "false" })?;
    persist_and_emit_preferences(app, state)
}

fn app_state_expanded(app: &AppHandle) -> bool {
    app.try_state::<Arc<MonitorAppState>>()
        .is_some_and(|state| state.expanded.load(Ordering::SeqCst))
}

fn initialize_window_geometry(
    window: &tauri::WebviewWindow,
    state: &Arc<MonitorAppState>,
) -> Result<(), String> {
    state.expanded.store(false, Ordering::SeqCst);
    let preferences = current_ui_preferences(state)?;
    let monitors = window
        .available_monitors()
        .map_err(|error| error.to_string())?;
    let monitor = preferences
        .window_placement
        .monitor_id
        .as_deref()
        .and_then(|identifier| find_saved_monitor(&monitors, identifier))
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten())
        .or_else(|| monitors.into_iter().next())
        .ok_or_else(|| "no monitor is available".to_owned())?;

    apply_mode_constraints(window, false, &monitor)?;
    let bounds = clamp_bounds_for_mode(preferences.compact_bounds, false, &monitor);
    let size = physical_size(bounds, monitor.scale_factor());
    let position = position_from_placement(&monitor, &preferences.window_placement, size);
    window
        .set_size(LogicalSize::new(bounds.width, bounds.height))
        .map_err(|error| error.to_string())?;
    window
        .set_position(position)
        .map_err(|error| error.to_string())?;
    recover_window_visibility(window, false)?;
    capture_window_state(window.app_handle(), state)
}

fn find_saved_monitor(monitors: &[Monitor], identifier: &str) -> Option<Monitor> {
    monitors
        .iter()
        .find(|monitor| monitor_identifier(monitor) == identifier)
        .cloned()
        .or_else(|| {
            // Resolution changes alter the geometry suffix; retain the same named
            // display when possible before falling back to the primary monitor.
            let saved_name = identifier.rsplit_once('@')?.0;
            monitors
                .iter()
                .find(|monitor| monitor.name().is_some_and(|name| name == saved_name))
                .cloned()
        })
}

fn reset_to_current_monitor_top_right(
    app: &AppHandle,
    state: &Arc<MonitorAppState>,
) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window unavailable".to_owned())?;
    let monitor = best_monitor_for_window(&window)
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor is available".to_owned())?;
    let placement = WindowPlacement {
        monitor_id: Some(monitor_identifier(&monitor)),
        scale_factor: monitor.scale_factor(),
        anchor: "topRight".to_owned(),
        offset_dip: WindowOffsetDip::new(WINDOW_EDGE_MARGIN_DIP, WINDOW_EDGE_MARGIN_DIP),
    };
    let size = window.outer_size().map_err(|error| error.to_string())?;
    window
        .set_position(position_from_placement(&monitor, &placement, size))
        .map_err(|error| error.to_string())?;
    {
        let mut preferences = state
            .window_preferences
            .lock()
            .map_err(|_| "window preferences lock poisoned".to_owned())?;
        preferences.window_placement = placement;
    }
    capture_window_state(app, state)
}

fn start_window_state_worker(app: AppHandle, state: Arc<MonitorAppState>) -> Result<(), String> {
    let (sender, receiver) = mpsc::channel::<bool>();
    *state
        .window_event_sender
        .lock()
        .map_err(|_| "window event sender lock poisoned".to_owned())? = Some(sender);
    thread::Builder::new()
        .name("mochi-window-state".to_owned())
        .spawn(move || {
            while let Ok(mut recover_for_display_change) = receiver.recv() {
                loop {
                    match receiver.recv_timeout(Duration::from_millis(WINDOW_EVENT_DEBOUNCE_MS)) {
                        Ok(recover) => recover_for_display_change |= recover,
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                if state.shutting_down.load(Ordering::SeqCst) {
                    return;
                }
                if !wait_for_native_interaction_end(
                    &receiver,
                    &mut recover_for_display_change,
                    &state.shutting_down,
                    Duration::from_millis(WINDOW_INTERACTION_POLL_MS),
                    native_primary_button_down,
                ) {
                    return;
                }
                let callback_app = app.clone();
                let callback_state = state.clone();
                let _ = app.run_on_main_thread(move || {
                    // A new native gesture may start between the worker's release
                    // check and this main-thread callback. Requeue instead of
                    // clamping or saving geometry under the user's cursor.
                    if native_primary_button_down() {
                        queue_window_state_save(&callback_state, recover_for_display_change);
                        return;
                    }
                    if let Some(window) = callback_app.get_webview_window("main") {
                        // Delaying this until the native move/resize gesture is over prevents
                        // the old "sticky" cursor and oscillating off-screen clamp behavior.
                        if recover_for_display_change || window.is_visible().unwrap_or(false) {
                            let _ = recover_window_visibility(
                                &window,
                                callback_state.expanded.load(Ordering::SeqCst),
                            );
                        }
                    }
                    let _ = capture_window_state(&callback_app, &callback_state);
                });
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn wait_for_native_interaction_end<F>(
    receiver: &mpsc::Receiver<bool>,
    recover_for_display_change: &mut bool,
    shutting_down: &AtomicBool,
    poll_interval: Duration,
    mut primary_button_down: F,
) -> bool
where
    F: FnMut() -> bool,
{
    loop {
        if shutting_down.load(Ordering::SeqCst) {
            return false;
        }
        if !primary_button_down() {
            return true;
        }
        match receiver.recv_timeout(poll_interval) {
            Ok(recover) => *recover_for_display_change |= recover,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
}

#[cfg(windows)]
fn native_primary_button_down() -> bool {
    use windows_sys::Win32::UI::{
        Input::KeyboardAndMouse::GetAsyncKeyState,
        WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON},
    };

    // The high bit reports the current physical state. The low bit only means
    // the key transitioned since the previous query and is intentionally ignored.
    // GetAsyncKeyState itself follows physical buttons, so first map Windows'
    // logical primary button to the correct physical virtual key.
    let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) != 0 };
    let primary_key = primary_button_virtual_key(swapped);
    unsafe { (GetAsyncKeyState(primary_key) as u16 & 0x8000) != 0 }
}

#[cfg(not(windows))]
fn native_primary_button_down() -> bool {
    false
}

#[cfg(any(windows, test))]
fn primary_button_virtual_key(swapped: bool) -> i32 {
    if swapped {
        PHYSICAL_RIGHT_BUTTON_VK
    } else {
        PHYSICAL_LEFT_BUTTON_VK
    }
}

fn begin_shutdown(state: &Arc<MonitorAppState>) {
    state.shutting_down.store(true, Ordering::SeqCst);
    if let Ok(mut sender) = state.refresh_sender.lock() {
        sender.take();
    }
    if let Ok(mut sender) = state.window_event_sender.lock() {
        // Disconnect the dedicated debounce worker instead of leaving an
        // Arc/channel cycle alive until Windows tears down the process.
        sender.take();
    }
}

fn queue_window_state_save(state: &Arc<MonitorAppState>, recover_for_display_change: bool) {
    let sender = state
        .window_event_sender
        .lock()
        .ok()
        .and_then(|sender| sender.clone());
    if let Some(sender) = sender {
        let _ = sender.send(recover_for_display_change);
    }
}

fn capture_window_state(app: &AppHandle, state: &Arc<MonitorAppState>) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window unavailable".to_owned())?;
    let monitor = best_monitor_for_window(&window)
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor is available".to_owned())?;
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let size = window.outer_size().map_err(|error| error.to_string())?;
    let expanded = state.expanded.load(Ordering::SeqCst);
    update_mode_bounds(
        state,
        expanded,
        logical_bounds(size, monitor.scale_factor()),
    )?;
    {
        let mut preferences = state
            .window_preferences
            .lock()
            .map_err(|_| "window preferences lock poisoned".to_owned())?;
        preferences.window_placement = placement_from_geometry(&monitor, position, size);
        preferences.expanded = expanded;
    }
    persist_and_emit_preferences(app, state)
}

fn current_ui_preferences(state: &Arc<MonitorAppState>) -> Result<UiPreferencesV2, String> {
    let mut preferences = state
        .window_preferences
        .lock()
        .map_err(|_| "window preferences lock poisoned".to_owned())?
        .clone();
    preferences.version = UI_PREFERENCES_VERSION;
    preferences.expanded = state.expanded.load(Ordering::SeqCst);
    preferences.topmost = state.topmost.load(Ordering::SeqCst);
    preferences.theme = state
        .theme
        .lock()
        .map_err(|_| "theme lock poisoned".to_owned())?
        .clone();
    sanitize_preferences(&mut preferences);
    Ok(preferences)
}

fn persist_and_emit_preferences(
    app: &AppHandle,
    state: &Arc<MonitorAppState>,
) -> Result<(), String> {
    let preferences = current_ui_preferences(state)?;
    {
        let mut stored = state
            .window_preferences
            .lock()
            .map_err(|_| "window preferences lock poisoned".to_owned())?;
        *stored = preferences.clone();
    }
    state.persistence.set_setting(
        UI_PREFERENCES_KEY,
        &serde_json::to_string(&preferences).map_err(|error| error.to_string())?,
    )?;
    sync_tray_preferences(state, &preferences);
    app.emit("monitor://preferences", &preferences)
        .map_err(|error| error.to_string())
}

fn sync_tray_preferences(state: &Arc<MonitorAppState>, preferences: &UiPreferencesV2) {
    let handles = state
        .tray_preferences
        .lock()
        .ok()
        .and_then(|handles| handles.clone());
    if let Some(handles) = handles {
        let _ = handles.topmost.set_checked(preferences.topmost);
        let _ = handles.cute.set_checked(preferences.theme == "cute");
        let _ = handles.minimal.set_checked(preferences.theme == "minimal");
    }
}

fn saved_bounds_for_mode(
    state: &Arc<MonitorAppState>,
    expanded: bool,
) -> Result<WindowBounds, String> {
    let preferences = state
        .window_preferences
        .lock()
        .map_err(|_| "window preferences lock poisoned".to_owned())?;
    Ok(if expanded {
        preferences.expanded_bounds
    } else {
        preferences.compact_bounds
    })
}

fn update_mode_bounds(
    state: &Arc<MonitorAppState>,
    expanded: bool,
    bounds: WindowBounds,
) -> Result<(), String> {
    let mut preferences = state
        .window_preferences
        .lock()
        .map_err(|_| "window preferences lock poisoned".to_owned())?;
    if expanded {
        preferences.expanded_bounds = bounds;
    } else {
        preferences.compact_bounds = bounds;
    }
    Ok(())
}

fn apply_mode_constraints(
    window: &tauri::WebviewWindow,
    expanded: bool,
    monitor: &Monitor,
) -> Result<(), String> {
    let (minimum, maximum) = mode_limits(expanded, monitor);
    // Clear the previous mode first: otherwise switching from compact max-height
    // 120 to expanded min-height 300 briefly creates impossible constraints.
    window
        .set_min_size::<LogicalSize<f64>>(None)
        .map_err(|error| error.to_string())?;
    window
        .set_max_size::<LogicalSize<f64>>(None)
        .map_err(|error| error.to_string())?;
    window
        .set_min_size(Some(LogicalSize::new(minimum.width, minimum.height)))
        .map_err(|error| error.to_string())?;
    window
        .set_max_size(Some(LogicalSize::new(maximum.width, maximum.height)))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn mode_limits(expanded: bool, monitor: &Monitor) -> (WindowBounds, WindowBounds) {
    if !expanded {
        return (
            WindowBounds::new(COMPACT_MIN_WIDTH, COMPACT_MIN_HEIGHT),
            WindowBounds::new(COMPACT_MAX_WIDTH, COMPACT_MAX_HEIGHT),
        );
    }
    let scale = valid_scale(monitor.scale_factor());
    let area = monitor.work_area();
    let work_width = area.size.width as f64 / scale;
    let work_height = area.size.height as f64 / scale;
    let maximum = WindowBounds::new(
        EXPANDED_MAX_WIDTH
            .min(work_width * EXPANDED_WORK_AREA_FRACTION)
            .max(EXPANDED_MIN_WIDTH),
        EXPANDED_MAX_HEIGHT
            .min(work_height * EXPANDED_WORK_AREA_FRACTION)
            .max(EXPANDED_MIN_HEIGHT),
    );
    (
        WindowBounds::new(EXPANDED_MIN_WIDTH, EXPANDED_MIN_HEIGHT),
        maximum,
    )
}

fn clamp_bounds_for_mode(bounds: WindowBounds, expanded: bool, monitor: &Monitor) -> WindowBounds {
    let (minimum, maximum) = mode_limits(expanded, monitor);
    WindowBounds::new(
        finite_or(
            bounds.width,
            if expanded {
                EXPANDED_WIDTH
            } else {
                COMPACT_WIDTH
            },
        )
        .clamp(minimum.width, maximum.width),
        finite_or(
            bounds.height,
            if expanded {
                EXPANDED_HEIGHT
            } else {
                COMPACT_HEIGHT
            },
        )
        .clamp(minimum.height, maximum.height),
    )
}

fn recover_window_visibility(window: &tauri::WebviewWindow, expanded: bool) -> Result<(), String> {
    let monitor = best_monitor_for_window(window)
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor is available".to_owned())?;
    apply_mode_constraints(window, expanded, &monitor)?;
    let current_size = window.outer_size().map_err(|error| error.to_string())?;
    let bounds = clamp_bounds_for_mode(
        logical_bounds(current_size, monitor.scale_factor()),
        expanded,
        &monitor,
    );
    let size = physical_size(bounds, monitor.scale_factor());
    if size != current_size {
        window
            .set_size(LogicalSize::new(bounds.width, bounds.height))
            .map_err(|error| error.to_string())?;
    }
    let current_position = window.outer_position().map_err(|error| error.to_string())?;
    let position = clamp_position_to_monitor(&monitor, current_position, size);
    if position != current_position {
        window
            .set_position(position)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn window_geometry_needs_recovery(window: &tauri::WebviewWindow, expanded: bool) -> bool {
    let Some(monitor) = best_monitor_for_window(window) else {
        return true;
    };
    let Ok(size) = window.outer_size() else {
        return true;
    };
    let Ok(position) = window.outer_position() else {
        return true;
    };
    let clamped_bounds = clamp_bounds_for_mode(
        logical_bounds(size, monitor.scale_factor()),
        expanded,
        &monitor,
    );
    let clamped_size = physical_size(clamped_bounds, monitor.scale_factor());
    clamped_size != size || clamp_position_to_monitor(&monitor, position, clamped_size) != position
}

fn best_monitor_for_window(window: &tauri::WebviewWindow) -> Option<Monitor> {
    let monitors = window.available_monitors().ok()?;
    let position = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    let mut best: Option<(u64, Monitor)> = None;
    for monitor in monitors {
        let area = monitor.work_area();
        let overlap = intersection_area(
            position,
            size,
            area.position,
            PhysicalSize::new(area.size.width, area.size.height),
        );
        if best.as_ref().is_none_or(|(largest, _)| overlap > *largest) {
            best = Some((overlap, monitor));
        }
    }
    best.and_then(|(overlap, monitor)| (overlap > 0).then_some(monitor))
}

fn intersection_area(
    first_position: PhysicalPosition<i32>,
    first_size: PhysicalSize<u32>,
    second_position: PhysicalPosition<i32>,
    second_size: PhysicalSize<u32>,
) -> u64 {
    let left = first_position.x.max(second_position.x) as i64;
    let top = first_position.y.max(second_position.y) as i64;
    let right = (first_position.x as i64 + first_size.width as i64)
        .min(second_position.x as i64 + second_size.width as i64);
    let bottom = (first_position.y as i64 + first_size.height as i64)
        .min(second_position.y as i64 + second_size.height as i64);
    right.saturating_sub(left).max(0) as u64 * bottom.saturating_sub(top).max(0) as u64
}

fn monitor_identifier(monitor: &Monitor) -> String {
    format!(
        "{}@{},{}:{}x{}",
        monitor.name().map(String::as_str).unwrap_or("monitor"),
        monitor.position().x,
        monitor.position().y,
        monitor.size().width,
        monitor.size().height
    )
}

fn placement_from_geometry(
    monitor: &Monitor,
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
) -> WindowPlacement {
    let area = monitor.work_area();
    placement_from_rect(
        monitor_identifier(monitor),
        monitor.scale_factor(),
        area.position,
        PhysicalSize::new(area.size.width, area.size.height),
        position,
        size,
    )
}

fn placement_from_rect(
    monitor_id: String,
    scale_factor: f64,
    area_position: PhysicalPosition<i32>,
    area_size: PhysicalSize<u32>,
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
) -> WindowPlacement {
    let scale = valid_scale(scale_factor);
    let left = (position.x - area_position.x) as f64 / scale;
    let top = (position.y - area_position.y) as f64 / scale;
    let right = (area_position.x as i64 + area_size.width as i64
        - position.x as i64
        - size.width as i64) as f64
        / scale;
    let bottom = (area_position.y as i64 + area_size.height as i64
        - position.y as i64
        - size.height as i64) as f64
        / scale;
    let center_x = (position.x as f64 + size.width as f64 / 2.0
        - (area_position.x as f64 + area_size.width as f64 / 2.0))
        / scale;
    let center_y = (position.y as f64 + size.height as f64 / 2.0
        - (area_position.y as f64 + area_size.height as f64 / 2.0))
        / scale;

    let horizontal = if left.abs().min(right.abs()) <= WINDOW_ANCHOR_SNAP_DIP {
        if left.abs() <= right.abs() {
            Some("left")
        } else {
            Some("right")
        }
    } else {
        None
    };
    let vertical = if top.abs().min(bottom.abs()) <= WINDOW_ANCHOR_SNAP_DIP {
        if top.abs() <= bottom.abs() {
            Some("top")
        } else {
            Some("bottom")
        }
    } else {
        None
    };
    let (anchor, offset_dip) = match (horizontal, vertical) {
        (Some("left"), Some("top")) => ("topLeft", WindowOffsetDip::new(left, top)),
        (Some("right"), Some("top")) => ("topRight", WindowOffsetDip::new(right, top)),
        (Some("left"), Some("bottom")) => ("bottomLeft", WindowOffsetDip::new(left, bottom)),
        (Some("right"), Some("bottom")) => ("bottomRight", WindowOffsetDip::new(right, bottom)),
        (Some("left"), None) => ("left", WindowOffsetDip::new(left, center_y)),
        (Some("right"), None) => ("right", WindowOffsetDip::new(right, center_y)),
        (None, Some("top")) => ("top", WindowOffsetDip::new(center_x, top)),
        (None, Some("bottom")) => ("bottom", WindowOffsetDip::new(center_x, bottom)),
        _ => ("center", WindowOffsetDip::new(center_x, center_y)),
    };
    WindowPlacement {
        monitor_id: Some(monitor_id),
        scale_factor: scale,
        anchor: anchor.to_owned(),
        offset_dip,
    }
}

fn position_from_placement(
    monitor: &Monitor,
    placement: &WindowPlacement,
    size: PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    let area = monitor.work_area();
    let position = position_from_anchor(
        monitor.scale_factor(),
        area.position,
        PhysicalSize::new(area.size.width, area.size.height),
        placement,
        size,
    );
    clamp_position_to_monitor(monitor, position, size)
}

fn position_from_anchor(
    scale_factor: f64,
    area_position: PhysicalPosition<i32>,
    area_size: PhysicalSize<u32>,
    placement: &WindowPlacement,
    size: PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    let scale = valid_scale(scale_factor);
    let offset_x = finite_or(placement.offset_dip.x, WINDOW_EDGE_MARGIN_DIP) * scale;
    let offset_y = finite_or(placement.offset_dip.y, WINDOW_EDGE_MARGIN_DIP) * scale;
    let left = area_position.x as f64;
    let top = area_position.y as f64;
    let right = left + area_size.width as f64;
    let bottom = top + area_size.height as f64;
    let center_x = left + area_size.width as f64 / 2.0;
    let center_y = top + area_size.height as f64 / 2.0;
    let width = size.width as f64;
    let height = size.height as f64;
    let (x, y) = match placement.anchor.as_str() {
        "topLeft" => (left + offset_x, top + offset_y),
        "topRight" => (right - width - offset_x, top + offset_y),
        "bottomLeft" => (left + offset_x, bottom - height - offset_y),
        "bottomRight" => (right - width - offset_x, bottom - height - offset_y),
        "left" => (left + offset_x, center_y + offset_y - height / 2.0),
        "right" => (right - width - offset_x, center_y + offset_y - height / 2.0),
        "top" => (center_x + offset_x - width / 2.0, top + offset_y),
        "bottom" => (
            center_x + offset_x - width / 2.0,
            bottom - height - offset_y,
        ),
        _ => (
            center_x + offset_x - width / 2.0,
            center_y + offset_y - height / 2.0,
        ),
    };
    PhysicalPosition::new(x.round() as i32, y.round() as i32)
}

fn clamp_position_to_monitor(
    monitor: &Monitor,
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    let area = monitor.work_area();
    let min_x = area.position.x;
    let min_y = area.position.y;
    let max_x = area.position.x + area.size.width as i32 - size.width as i32;
    let max_y = area.position.y + area.size.height as i32 - size.height as i32;
    PhysicalPosition::new(
        position.x.clamp(min_x, max_x.max(min_x)),
        position.y.clamp(min_y, max_y.max(min_y)),
    )
}

fn logical_bounds(size: PhysicalSize<u32>, scale_factor: f64) -> WindowBounds {
    let scale = valid_scale(scale_factor);
    WindowBounds::new(size.width as f64 / scale, size.height as f64 / scale)
}

fn physical_size(bounds: WindowBounds, scale_factor: f64) -> PhysicalSize<u32> {
    let scale = valid_scale(scale_factor);
    PhysicalSize::new(
        (bounds.width * scale).round().max(1.0) as u32,
        (bounds.height * scale).round().max(1.0) as u32,
    )
}

fn sanitize_preferences(preferences: &mut UiPreferencesV2) {
    preferences.version = UI_PREFERENCES_VERSION;
    if !matches!(preferences.theme.as_str(), "cute" | "minimal") {
        preferences.theme = "cute".to_owned();
    }
    preferences.compact_bounds = WindowBounds::new(
        finite_or(preferences.compact_bounds.width, COMPACT_WIDTH)
            .clamp(COMPACT_MIN_WIDTH, COMPACT_MAX_WIDTH),
        finite_or(preferences.compact_bounds.height, COMPACT_HEIGHT)
            .clamp(COMPACT_MIN_HEIGHT, COMPACT_MAX_HEIGHT),
    );
    preferences.expanded_bounds = WindowBounds::new(
        finite_or(preferences.expanded_bounds.width, EXPANDED_WIDTH)
            .clamp(EXPANDED_MIN_WIDTH, EXPANDED_MAX_WIDTH),
        finite_or(preferences.expanded_bounds.height, EXPANDED_HEIGHT)
            .clamp(EXPANDED_MIN_HEIGHT, EXPANDED_MAX_HEIGHT),
    );
    preferences.window_placement.scale_factor =
        valid_scale(preferences.window_placement.scale_factor);
    if !matches!(
        preferences.window_placement.anchor.as_str(),
        "topLeft"
            | "topRight"
            | "bottomLeft"
            | "bottomRight"
            | "left"
            | "right"
            | "top"
            | "bottom"
            | "center"
    ) {
        preferences.window_placement.anchor = "topRight".to_owned();
    }
    preferences.window_placement.offset_dip.x = finite_or(
        preferences.window_placement.offset_dip.x,
        WINDOW_EDGE_MARGIN_DIP,
    );
    preferences.window_placement.offset_dip.y = finite_or(
        preferences.window_placement.offset_dip.y,
        WINDOW_EDGE_MARGIN_DIP,
    );
}

fn valid_scale(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        1.0
    }
}

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

fn empty_snapshot() -> MonitorSnapshot {
    MonitorSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        checked_at: now_iso(),
        codex_running: false,
        collector_health: crate::model::CollectorHealth::default(),
        conversations: Vec::new(),
    }
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CollectorHealth, ConversationSnapshot, RequestSnapshot, ServerRouteSnapshot,
        StatusSnapshot, TimingSnapshot, UsageSnapshot,
    };
    use std::{
        fs,
        sync::atomic::AtomicUsize,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn refresh_scheduler_is_single_flight_and_coalesces_one_trailing_scan() {
        let (request_tx, request_rx) = mpsc::channel();
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let scans = Arc::new(AtomicUsize::new(0));
        let worker_scans = scans.clone();
        let mutation_counts = Arc::new(Mutex::new(Vec::new()));
        let worker_mutation_counts = mutation_counts.clone();
        let worker = thread::spawn(move || {
            run_refresh_scheduler(request_rx, |_, mutations| {
                worker_mutation_counts.lock().unwrap().push(mutations.len());
                let scan = worker_scans.fetch_add(1, Ordering::SeqCst) + 1;
                if scan == 1 {
                    first_entered_tx.send(()).unwrap();
                    release_first_rx.recv().unwrap();
                }
                Ok(empty_snapshot())
            });
        });

        let (first_tx, first_rx) = mpsc::channel();
        request_tx
            .send(RefreshRequest::waiting(true, first_tx))
            .unwrap();
        first_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first refresh did not start");

        // These requests all arrive while the first scan is running. They must
        // collapse into exactly one trailing scan, even when manual and watcher
        // requests are interleaved.
        let (second_tx, second_rx) = mpsc::channel();
        let (third_tx, third_rx) = mpsc::channel();
        request_tx
            .send(RefreshRequest::waiting(true, second_tx))
            .unwrap();
        request_tx
            .send(RefreshRequest::hook(HookObservation {
                thread_id: "thread-fixture".to_owned(),
                turn_id: Some("turn-fixture".to_owned()),
                model: Some("gpt-5.6-sol".to_owned()),
                observed_at: "2026-08-25T00:00:00.000Z".to_owned(),
            }))
            .unwrap();
        for _ in 0..20 {
            request_tx.send(RefreshRequest::background(false)).unwrap();
        }
        request_tx
            .send(RefreshRequest::waiting(true, third_tx))
            .unwrap();
        release_first_tx.send(()).unwrap();

        assert_eq!(
            first_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .status,
            "completed"
        );
        assert_eq!(
            second_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .status,
            "completed"
        );
        assert_eq!(
            third_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .status,
            "coalesced"
        );
        drop(request_tx);
        worker.join().unwrap();
        assert_eq!(scans.load(Ordering::SeqCst), 2);
        assert_eq!(*mutation_counts.lock().unwrap(), vec![0, 1]);
    }

    #[test]
    fn refresh_core_releases_guard_before_presentation_phase() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "xiaoli-refresh-lock-{}-{unique}",
            std::process::id()
        ));
        let sessions_root = root.join("sessions");
        fs::create_dir_all(&sessions_root).unwrap();
        let options = LaunchOptions {
            probe_once: false,
            stop: false,
            show: false,
            hidden: true,
            shadow: true,
            sessions_root,
            session_index_path: root.join("session_index.jsonl"),
            state_root: root.join("state"),
        };
        let persistence = Persistence::open(&options.state_root).unwrap();
        let state = Arc::new(MonitorAppState::new(options, persistence));

        let outcome =
            refresh_once_with_runtime(&state, CodexRuntime::default(), Vec::new()).unwrap();
        assert_eq!(outcome.snapshot.schema_version, SNAPSHOT_SCHEMA_VERSION);
        assert!(
            state.refresh_guard.try_lock().is_ok(),
            "presentation would recreate the UI/refresh lock inversion"
        );

        drop(state);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn hook_fallback_is_consumed_once_and_only_as_sanitized_metadata() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "xiaoli-hook-fallback-{}-{unique}",
            std::process::id()
        ));
        let options = LaunchOptions {
            probe_once: false,
            stop: false,
            show: false,
            hidden: true,
            shadow: true,
            sessions_root: root.join("sessions"),
            session_index_path: root.join("session_index.jsonl"),
            state_root: root.join("state"),
        };
        let persistence = Persistence::open(&options.state_root).unwrap();
        let state = Arc::new(MonitorAppState::new(options, persistence));
        let fallback = state.options.state_root.join("hook-latest.json");
        fs::write(
            &fallback,
            serde_json::to_vec(&json!({
                "event":"UserPromptSubmit",
                "session":"thread-fallback",
                "turn":"turn-fallback",
                "model":"gpt-5.6-sol",
                "timestamp":"2026-08-25T00:00:00.000Z",
                "prompt":"PRIVATE_BODY_MUST_NOT_ESCAPE"
            }))
            .unwrap(),
        )
        .unwrap();

        let observation = read_hook_fallback(&state).expect("new fallback");
        assert_eq!(observation.thread_id, "thread-fallback");
        assert_eq!(observation.turn_id.as_deref(), Some("turn-fallback"));
        assert_eq!(observation.model.as_deref(), Some("gpt-5.6-sol"));
        assert!(read_hook_fallback(&state).is_none());
        assert!(fallback.exists(), "fallback remains crash-recoverable");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unrelated_file_warnings_do_not_mark_a_clean_active_turn_dirty() {
        let mut unrelated = FileState::new(PathBuf::from("unrelated.jsonl"));
        unrelated.parse_warnings = 9;
        unrelated.last_error = Some("old_fixture_warning".to_owned());

        let mut selected = FileState::new(PathBuf::from("selected.jsonl"));
        selected.identity_known = true;
        selected.thread_id = Some("thread-clean-active".to_owned());
        selected.current_turn = Some(crate::model::TurnState {
            turn_id: "turn-clean-active".to_owned(),
            lifecycle: TurnLifecycle::Active,
            parse_warnings_at_start: 0,
            usage_turn: TokenUsage {
                output_tokens: 128,
                ..TokenUsage::default()
            },
            ..crate::model::TurnState::default()
        });

        let evidence = selected_active_turn_evidence(&[unrelated, selected]);
        let current = evidence
            .get(&(
                "thread-clean-active".to_owned(),
                "turn-clean-active".to_owned(),
            ))
            .expect("selected active evidence");
        assert!(current.clean);
    }

    #[test]
    fn cold_state_construction_defers_rollout_scanning_to_worker() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("xiaoli-cold-start-{}-{unique}", std::process::id()));
        let sessions_root = root.join("sessions/2030/01/01");
        fs::create_dir_all(&sessions_root).unwrap();
        fs::write(
            sessions_root.join("rollout-large.jsonl"),
            vec![b' '; 2 * 1024 * 1024],
        )
        .unwrap();
        let options = LaunchOptions {
            probe_once: false,
            stop: false,
            show: false,
            hidden: true,
            shadow: true,
            sessions_root: root.join("sessions"),
            session_index_path: root.join("session_index.jsonl"),
            state_root: root.join("state"),
        };
        let persistence = Persistence::open(&options.state_root).unwrap();
        let state = MonitorAppState::new(options, persistence);
        assert_eq!(state.collector.lock().unwrap().file_states().count(), 0);
        drop(state);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fingerprint_ignores_poll_time_and_live_elapsed_age() {
        let mut first = empty_snapshot();
        first.codex_running = true;
        let mut second = first.clone();
        second.checked_at = "2026-08-24T12:00:05.000Z".to_owned();
        assert_eq!(
            stable_fingerprint(&first).unwrap(),
            stable_fingerprint(&second).unwrap()
        );
    }

    #[test]
    fn collector_red_dominates_tray_level() {
        let snapshot = MonitorSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            checked_at: now_iso(),
            codex_running: true,
            collector_health: CollectorHealth {
                level: StatusLevel::Red,
                parse_warnings: 0,
                last_error: Some("fixture".to_owned()),
            },
            conversations: Vec::new(),
        };
        assert_eq!(overall_level(&snapshot), StatusLevel::Red);
        let _unused = StatusSnapshot::default();
    }

    #[test]
    fn subagent_red_bubbles_to_tray_level() {
        let mut snapshot = empty_snapshot();
        snapshot.codex_running = true;
        snapshot.conversations.push(ConversationSnapshot {
            thread_id: "child".to_owned(),
            turn_id: "turn".to_owned(),
            parent_thread_id: Some("root".to_owned()),
            kind: ThreadKind::Subagent,
            title: "子智能体".to_owned(),
            source_timestamp: None,
            active_request: RequestSnapshot::default(),
            pending_next_turn: None,
            server_route: ServerRouteSnapshot::default(),
            usage: UsageSnapshot::default(),
            timing: TimingSnapshot::default(),
            quality_assessment: Default::default(),
            tool_activity: false,
            status: StatusSnapshot {
                level: StatusLevel::Red,
                code: "fixture".to_owned(),
                explanation: "fixture".to_owned(),
            },
            anomalies: Vec::new(),
        });
        assert_eq!(overall_level(&snapshot), StatusLevel::Red);
    }

    #[test]
    fn tray_tooltip_counts_roots_and_subtasks_separately() {
        let mut snapshot = empty_snapshot();
        snapshot.codex_running = true;
        let mut root = fixture_conversation("root", ThreadKind::Root, None);
        root.active_request = RequestSnapshot::new(
            Some("gpt-5.6-sol".to_owned()),
            Some("ultra".to_owned()),
            "turnContext",
        );
        snapshot.conversations.push(root);
        for index in 0..3 {
            snapshot.conversations.push(fixture_conversation(
                &format!("child-{index}"),
                ThreadKind::Subagent,
                Some("root"),
            ));
        }
        assert_eq!(
            tray_tooltip(&snapshot),
            "小狸 · 1 个活动对话 · 3 个子任务 · gpt-5.6-sol · ultra（请求）"
        );
    }

    #[test]
    fn mcp_session_detail_honors_turn_guard_and_card_projection() {
        let mut snapshot = empty_snapshot();
        snapshot.codex_running = true;
        let mut root = fixture_conversation("root", ThreadKind::Root, None);
        root.turn_id = "turn-current".to_owned();
        root.active_request = RequestSnapshot::new(
            Some("gpt-5.6-sol".to_owned()),
            Some("ultra".to_owned()),
            "turnContext",
        );
        snapshot.conversations.push(root);
        snapshot.conversations.push(fixture_conversation(
            "child",
            ThreadKind::Subagent,
            Some("root"),
        ));
        snapshot
            .conversations
            .push(fixture_conversation("unrelated", ThreadKind::Root, None));

        let detail = project_session_detail(
            &snapshot,
            &json!({"threadId":"root", "turnId":"turn-current"}),
        )
        .expect("matching active turn");
        assert_eq!(
            detail
                .pointer("/conversation/turnId")
                .and_then(Value::as_str),
            Some("turn-current")
        );
        assert_eq!(
            detail
                .pointer("/children/0/threadId")
                .and_then(Value::as_str),
            Some("child")
        );
        assert_eq!(
            project_session_detail(
                &snapshot,
                &json!({"threadId":"root", "turnId":"stale-turn"})
            )
            .unwrap_err(),
            "active_conversation_not_found"
        );

        let card = project_monitor_card_snapshot(
            &snapshot,
            &json!({"threadId":"root", "theme":"minimal"}),
        )
        .expect("thread card projection");
        assert_eq!(card.get("theme").and_then(Value::as_str), Some("minimal"));
        assert_eq!(
            card.get("projectionThreadId").and_then(Value::as_str),
            Some("root")
        );
        let conversations = card
            .get("conversations")
            .and_then(Value::as_array)
            .expect("projected conversations");
        assert_eq!(conversations.len(), 2);
        assert!(conversations
            .iter()
            .all(|item| { item.get("threadId").and_then(Value::as_str) != Some("unrelated") }));
    }

    #[test]
    fn orphan_subtask_never_masquerades_as_root_conversation() {
        let mut snapshot = empty_snapshot();
        snapshot.codex_running = true;
        snapshot.conversations.push(fixture_conversation(
            "orphan",
            ThreadKind::Subagent,
            Some("missing-root"),
        ));
        let tooltip = tray_tooltip(&snapshot);
        assert_eq!(tooltip, "小狸 · 0 个活动对话 · 1 个子任务（父会话未找到）");
        assert!(!tooltip.contains("1 个活动对话"));
    }

    fn fixture_conversation(
        thread_id: &str,
        kind: ThreadKind,
        parent_thread_id: Option<&str>,
    ) -> ConversationSnapshot {
        ConversationSnapshot {
            thread_id: thread_id.to_owned(),
            turn_id: format!("turn-{thread_id}"),
            parent_thread_id: parent_thread_id.map(str::to_owned),
            kind,
            title: thread_id.to_owned(),
            source_timestamp: None,
            active_request: RequestSnapshot::default(),
            pending_next_turn: None,
            server_route: ServerRouteSnapshot::default(),
            usage: UsageSnapshot::default(),
            timing: TimingSnapshot::default(),
            quality_assessment: Default::default(),
            tool_activity: false,
            status: StatusSnapshot::default(),
            anomalies: Vec::new(),
        }
    }

    #[test]
    fn multi_turn_behavior_uses_turn_delta_and_does_not_raise_false_yellow() {
        let mut conversation = fixture_conversation("root", ThreadKind::Root, None);
        conversation.active_request = RequestSnapshot::new(
            Some("gpt-5.6-sol".to_owned()),
            Some("ultra".to_owned()),
            "turnContext",
        );
        conversation.timing = TimingSnapshot {
            elapsed_ms: Some(2_060),
            ttft_ms: Some(930),
            duration_ms: None,
            observed_output_rate: None,
            ..TimingSnapshot::default()
        };
        // This is the public thread/session total after multiple turns. It must
        // remain cumulative for the UI, but must never be paired with this
        // turn's 2.06-second elapsed time for behavior assessment.
        conversation.usage.cumulative = TokenUsage {
            input_tokens: 80_000,
            cached_input_tokens: 8_000,
            output_tokens: 9_000,
            reasoning_output_tokens: 8_000,
            total_tokens: 89_000,
            ..TokenUsage::default()
        };
        conversation.usage.cache_input_share = Some(0.10);

        let current_turn_usage = TokenUsage {
            input_tokens: 40_000,
            cached_input_tokens: 28_600,
            output_tokens: 103,
            reasoning_output_tokens: 23,
            total_tokens: 40_103,
            ..TokenUsage::default()
        };
        let mut selected_file = FileState::new(PathBuf::from("new-rollout.jsonl"));
        selected_file.identity_known = true;
        selected_file.thread_id = Some(conversation.thread_id.clone());
        selected_file.segment_start_ordinal = 20;
        selected_file.last_write_ms = 2;
        selected_file.current_turn = Some(crate::model::TurnState {
            turn_id: conversation.turn_id.clone(),
            usage_turn: current_turn_usage.clone(),
            usage_cumulative: conversation.usage.cumulative.clone(),
            ..crate::model::TurnState::default()
        });
        let active_usage = selected_active_turn_usage(&[selected_file]);
        let exact_turn_usage = active_usage
            .get(&(conversation.thread_id.clone(), conversation.turn_id.clone()))
            .expect("selected active turn has a reliable local delta");
        let current =
            active_behavior_sample(&conversation, "2026-08-24T08:00:00.000Z", exact_turn_usage)
                .expect("turn-local sample");

        assert_eq!(current.input_tokens, 40_000);
        assert_eq!(current.output_tokens, 103);
        assert_eq!(current.reasoning_output_tokens, 23);
        assert_eq!(current.cache_input_share, Some(0.715));
        assert_eq!(current.input_bucket, "32k-128k");
        assert_eq!(conversation.usage.cumulative.output_tokens, 9_000);

        let history = (0_u64..30)
            .map(|index| {
                let phase = index % 7;
                let output_tokens = 100 + phase;
                CompletedTurnSample {
                    thread_id: format!("history-{index}"),
                    turn_id: format!("turn-{index}"),
                    kind: ThreadKind::Root,
                    model: Some("gpt-5.6-sol".to_owned()),
                    effort: Some("ultra".to_owned()),
                    input_bucket: "32k-128k".to_owned(),
                    tool_activity: false,
                    ttft_ms: Some(900 + phase * 10),
                    duration_ms: Some(2_000 + phase * 20),
                    input_tokens: 40_000,
                    output_tokens,
                    reasoning_output_tokens: 20 + index % 5,
                    cache_input_share: Some(0.70 + phase as f64 * 0.005),
                    completed_at: "2026-08-24T07:00:00.000Z".to_owned(),
                }
            })
            .collect::<Vec<_>>();
        let assessment = assess_behavior(&history, &current);
        assert!(assessment.eligible);
        assert!(!assessment.yellow_anomaly);

        // Demonstrate the exact old failure: the same public cumulative total,
        // combined with current-turn timing, crosses multiple independent MAD
        // checks and would have produced a false yellow warning.
        let old_mixed_sample = active_behavior_sample(
            &conversation,
            "2026-08-24T08:00:00.000Z",
            &conversation.usage.cumulative,
        )
        .expect("legacy mixed sample");
        let old_assessment = assess_behavior(&history, &old_mixed_sample);
        assert!(old_assessment.yellow_anomaly);
        assert!(old_assessment.deviations.len() >= 2);

        assert!(active_behavior_sample(
            &conversation,
            "2026-08-24T08:00:00.000Z",
            &TokenUsage::default(),
        )
        .is_none());
    }

    #[test]
    fn embedded_tray_master_is_tight_transparent_rgba() {
        assert_eq!(
            TRAY_BASE_RGBA.len(),
            (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize
        );
        let visible = TRAY_BASE_RGBA
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 16)
            .count();
        assert!(visible > 220, "mascot lost too much detail: {visible}");
        assert!(visible < 850, "mascot no longer has transparent framing");
        for &(x, y) in &[(0_u32, 0_u32), (31, 0), (0, 31), (31, 31)] {
            let index = ((y * TRAY_ICON_SIZE + x) * 4) as usize;
            assert_eq!(TRAY_BASE_RGBA[index + 3], 0);
        }
    }

    #[test]
    fn tray_states_keep_mascot_and_have_distinct_accessible_rings() {
        let levels = [
            (StatusLevel::Green, [47, 156, 106]),
            (StatusLevel::Yellow, [184, 120, 18]),
            (StatusLevel::Red, [216, 72, 97]),
            (StatusLevel::Gray, [129, 120, 130]),
        ];
        let icons = levels
            .iter()
            .map(|(level, _)| render_tray_status_rgba(*level))
            .collect::<Vec<_>>();
        for ((_, expected), icon) in levels.iter().zip(&icons) {
            let matching_ring_pixels = icon
                .chunks_exact(4)
                .filter(|pixel| pixel[3] > 180 && color_distance(pixel, expected) < 24)
                .count();
            assert!(
                matching_ring_pixels >= 50,
                "status ring is too weak: {matching_ring_pixels} pixels"
            );
        }
        assert_ne!(icons[0], icons[1]);
        assert_ne!(icons[1], icons[2]);

        // State only changes the perimeter; the mask itself remains identical.
        for y in 7..25 {
            for x in 7..25 {
                let dx = x as f64 - 15.5;
                let dy = y as f64 - 15.5;
                if (dx * dx + dy * dy).sqrt() >= 11.5 {
                    continue;
                }
                let index = (y * TRAY_ICON_SIZE as usize + x) * 4;
                assert_eq!(&icons[0][index..index + 4], &icons[2][index..index + 4]);
            }
        }
    }

    #[test]
    fn tray_icon_stays_legible_at_windows_taskbar_sizes() {
        let source = render_tray_status_rgba(StatusLevel::Green);
        for size in [16_usize, 20, 24, 32] {
            let icon = resize_rgba_for_test(&source, TRAY_ICON_SIZE as usize, size);
            let opaque = icon.chunks_exact(4).filter(|pixel| pixel[3] > 96).count();
            assert!(
                opaque > size * size / 3,
                "{size}px silhouette is too sparse"
            );
            let status_pixels = icon
                .chunks_exact(4)
                .filter(|pixel| pixel[3] > 128 && color_distance(pixel, &[47, 156, 106]) < 72)
                .count();
            assert!(status_pixels >= size, "{size}px status ring is not visible");
            let luminances = icon
                .chunks_exact(4)
                .filter(|pixel| pixel[3] > 180)
                .map(relative_luminance)
                .collect::<Vec<_>>();
            let darkest = luminances.iter().copied().fold(1.0_f64, f64::min);
            let lightest = luminances.iter().copied().fold(0.0_f64, f64::max);
            assert!(
                lightest - darkest > 0.45,
                "{size}px icon lacks light/dark taskbar contrast"
            );
        }
    }

    #[test]
    fn export_runtime_tray_assets_when_explicitly_requested() {
        if std::env::var("MOCHI_EXPORT_TRAY_ASSETS").as_deref() != Ok("1") {
            return;
        }
        export_runtime_tray_assets().expect("runtime tray asset export failed");
    }

    fn export_runtime_tray_assets() -> Result<(), String> {
        use image::{ColorType, ImageFormat};
        use std::{fs, path::PathBuf};

        let output_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("icons")
            .join("tray");
        fs::create_dir_all(&output_root).map_err(|error| error.to_string())?;
        let states = [
            ("green", StatusLevel::Green, "#2F9C6A"),
            ("yellow", StatusLevel::Yellow, "#B87812"),
            ("red", StatusLevel::Red, "#D84861"),
            ("gray", StatusLevel::Gray, "#817882"),
        ];
        let sizes = [16_usize, 20, 24, 32];
        let mut assets = Vec::new();
        for (state_name, level, status_color) in states {
            let runtime_rgba = render_tray_status_rgba(level);
            for size in sizes {
                let rgba = resize_rgba_for_test(&runtime_rgba, TRAY_ICON_SIZE as usize, size);
                let file_name = format!("{state_name}-{size}.png");
                let path = output_root.join(&file_name);
                image::save_buffer_with_format(
                    &path,
                    &rgba,
                    size as u32,
                    size as u32,
                    ColorType::Rgba8,
                    ImageFormat::Png,
                )
                .map_err(|error| error.to_string())?;
                let png = fs::read(&path).map_err(|error| error.to_string())?;
                let (visible, opaque, translucent, alpha_bounds) = alpha_statistics(&rgba, size);
                assets.push(json!({
                    "state": state_name,
                    "size": size,
                    "file": file_name,
                    "statusColor": status_color,
                    "hasAlpha": translucent > 0 || visible < size * size,
                    "visiblePixels": visible,
                    "opaquePixels": opaque,
                    "translucentPixels": translucent,
                    "alphaBounds": alpha_bounds,
                    "rgbaFnv1a64": format!("{:016x}", fnv1a64(&rgba)),
                    "pngFnv1a64": format!("{:016x}", fnv1a64(&png)),
                }));
            }
        }
        let manifest = json!({
            "schemaVersion": 1,
            "source": "render_tray_status_rgba",
            "master": "../tray-master.png",
            "resizeAlgorithm": "premultiplied-area",
            "hashAlgorithm": "FNV-1a-64",
            "sizes": sizes,
            "states": ["green", "yellow", "red", "gray"],
            "assets": assets,
            "checklist": {
                "runtimeRendererReused": true,
                "transparentRgba": true,
                "lightAndDarkTaskbarContrast": "covered by tray_icon_stays_legible_at_windows_taskbar_sizes",
                "subagentStatusBubbles": "covered by subagent_red_bubbles_to_tray_level",
                "normalBuildWritesSourceTree": false,
                "regeneratePowerShell": "$env:MOCHI_EXPORT_TRAY_ASSETS='1'; cargo test app::tests::export_runtime_tray_assets_when_explicitly_requested --lib -- --exact"
            }
        });
        fs::write(
            output_root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }

    fn alpha_statistics(rgba: &[u8], size: usize) -> (usize, usize, usize, serde_json::Value) {
        let mut visible = 0_usize;
        let mut opaque = 0_usize;
        let mut translucent = 0_usize;
        let mut min_x = size;
        let mut min_y = size;
        let mut max_x = 0_usize;
        let mut max_y = 0_usize;
        for (index, pixel) in rgba.chunks_exact(4).enumerate() {
            let alpha = pixel[3];
            if alpha == 0 {
                continue;
            }
            visible += 1;
            if alpha == 255 {
                opaque += 1;
            } else {
                translucent += 1;
            }
            let x = index % size;
            let y = index / size;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        let bounds = if visible == 0 {
            Value::Null
        } else {
            json!({
                "x": min_x,
                "y": min_y,
                "width": max_x - min_x + 1,
                "height": max_y - min_y + 1,
            })
        };
        (visible, opaque, translucent, bounds)
    }

    fn fnv1a64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    fn color_distance(pixel: &[u8], expected: &[u8; 3]) -> i32 {
        (pixel[0] as i32 - expected[0] as i32).abs()
            + (pixel[1] as i32 - expected[1] as i32).abs()
            + (pixel[2] as i32 - expected[2] as i32).abs()
    }

    fn relative_luminance(pixel: &[u8]) -> f64 {
        (0.2126 * pixel[0] as f64 + 0.7152 * pixel[1] as f64 + 0.0722 * pixel[2] as f64) / 255.0
    }

    fn resize_rgba_for_test(source: &[u8], source_size: usize, output_size: usize) -> Vec<u8> {
        if source_size == output_size {
            return source.to_vec();
        }
        let mut output = vec![0_u8; output_size * output_size * 4];
        for output_y in 0..output_size {
            let source_y0 = output_y * source_size / output_size;
            let source_y1 = ((output_y + 1) * source_size).div_ceil(output_size);
            for output_x in 0..output_size {
                let source_x0 = output_x * source_size / output_size;
                let source_x1 = ((output_x + 1) * source_size).div_ceil(output_size);
                let mut alpha_sum = 0_u64;
                let mut colors = [0_u64; 3];
                let mut samples = 0_u64;
                for source_y in source_y0..source_y1 {
                    for source_x in source_x0..source_x1 {
                        let index = (source_y * source_size + source_x) * 4;
                        let alpha = source[index + 3] as u64;
                        alpha_sum += alpha;
                        for channel in 0..3 {
                            colors[channel] += source[index + channel] as u64 * alpha;
                        }
                        samples += 1;
                    }
                }
                let target = (output_y * output_size + output_x) * 4;
                if alpha_sum > 0 {
                    for channel in 0..3 {
                        output[target + channel] = (colors[channel] / alpha_sum) as u8;
                    }
                }
                output[target + 3] = (alpha_sum / samples) as u8;
            }
        }
        output
    }

    #[test]
    fn ui_preferences_v2_uses_stable_camel_case_contract() {
        let preferences = UiPreferencesV2::default();
        let value = serde_json::to_value(&preferences).unwrap();
        assert_eq!(value["version"], UI_PREFERENCES_VERSION);
        assert_eq!(value["expanded"], false);
        assert_eq!(value["compactBounds"]["width"], COMPACT_WIDTH);
        assert_eq!(value["expandedBounds"]["height"], EXPANDED_HEIGHT);
        assert_eq!(value["windowPlacement"]["anchor"], "topRight");
    }

    #[test]
    fn corrupt_window_preferences_are_safely_sanitized() {
        let mut preferences = UiPreferencesV2 {
            version: 999,
            theme: "unknown".to_owned(),
            topmost: false,
            expanded: true,
            compact_bounds: WindowBounds::new(f64::NAN, 9_999.0),
            expanded_bounds: WindowBounds::new(1.0, f64::INFINITY),
            window_placement: WindowPlacement {
                monitor_id: Some("missing".to_owned()),
                scale_factor: 0.0,
                anchor: "invalid".to_owned(),
                offset_dip: WindowOffsetDip::new(f64::NAN, f64::INFINITY),
            },
        };
        sanitize_preferences(&mut preferences);
        assert_eq!(preferences.version, UI_PREFERENCES_VERSION);
        assert_eq!(preferences.theme, "cute");
        assert_eq!(
            preferences.compact_bounds,
            WindowBounds::new(COMPACT_WIDTH, COMPACT_MAX_HEIGHT)
        );
        assert_eq!(
            preferences.expanded_bounds,
            WindowBounds::new(EXPANDED_MIN_WIDTH, EXPANDED_HEIGHT)
        );
        assert_eq!(preferences.window_placement.scale_factor, 1.0);
        assert_eq!(preferences.window_placement.anchor, "topRight");
        assert_eq!(
            preferences.window_placement.offset_dip,
            WindowOffsetDip::new(WINDOW_EDGE_MARGIN_DIP, WINDOW_EDGE_MARGIN_DIP)
        );
    }

    #[test]
    fn primary_button_mapping_and_gate_cover_swapped_windows_buttons() {
        assert_eq!(primary_button_virtual_key(false), PHYSICAL_LEFT_BUTTON_VK);
        assert_eq!(primary_button_virtual_key(true), PHYSICAL_RIGHT_BUTTON_VK);

        for swapped in [false, true] {
            let (sender, receiver) = mpsc::channel();
            sender.send(true).expect("queue display recovery");
            let shutting_down = AtomicBool::new(false);
            let mut recover_for_display_change = false;
            let primary_key = primary_button_virtual_key(swapped);
            let physically_pressed_keys = [Some(primary_key), Some(primary_key), None];
            let mut checks = 0_usize;

            let ready = wait_for_native_interaction_end(
                &receiver,
                &mut recover_for_display_change,
                &shutting_down,
                Duration::ZERO,
                || {
                    let pressed_key = physically_pressed_keys[checks];
                    checks += 1;
                    pressed_key == Some(primary_button_virtual_key(swapped))
                },
            );

            assert!(ready, "release should allow geometry processing");
            assert_eq!(checks, 3, "pressed states must defer all processing");
            assert!(
                recover_for_display_change,
                "queued DPI/display recovery must survive the interaction gate"
            );
        }
    }

    #[test]
    fn window_state_gate_stops_without_polling_input_during_shutdown() {
        let (_sender, receiver) = mpsc::channel::<bool>();
        let shutting_down = AtomicBool::new(true);
        let mut recover_for_display_change = false;
        let mut input_polled = false;

        let ready = wait_for_native_interaction_end(
            &receiver,
            &mut recover_for_display_change,
            &shutting_down,
            Duration::ZERO,
            || {
                input_polled = true;
                true
            },
        );

        assert!(!ready);
        assert!(!input_polled);
    }

    #[test]
    fn top_right_anchor_survives_mode_resize_on_negative_monitor() {
        let area_position = PhysicalPosition::new(-2_560, 0);
        let area_size = PhysicalSize::new(2_560, 1_400);
        let compact_size = PhysicalSize::new(456, 108);
        let compact_position = PhysicalPosition::new(-474, 18);
        let placement = placement_from_rect(
            "secondary".to_owned(),
            1.5,
            area_position,
            area_size,
            compact_position,
            compact_size,
        );
        assert_eq!(placement.anchor, "topRight");
        assert_eq!(placement.offset_dip, WindowOffsetDip::new(12.0, 12.0));

        let expanded_size = PhysicalSize::new(660, 750);
        let expanded_position =
            position_from_anchor(1.5, area_position, area_size, &placement, expanded_size);
        assert_eq!(expanded_position, PhysicalPosition::new(-678, 18));
    }

    #[test]
    fn floating_window_preserves_center_during_mode_resize() {
        let area_position = PhysicalPosition::new(0, 0);
        let area_size = PhysicalSize::new(1_920, 1_040);
        let compact_size = PhysicalSize::new(304, 72);
        let compact_position = PhysicalPosition::new(500, 300);
        let placement = placement_from_rect(
            "primary".to_owned(),
            1.0,
            area_position,
            area_size,
            compact_position,
            compact_size,
        );
        assert_eq!(placement.anchor, "center");

        let expanded_size = PhysicalSize::new(440, 500);
        let expanded_position =
            position_from_anchor(1.0, area_position, area_size, &placement, expanded_size);
        assert_eq!(
            compact_position.x + compact_size.width as i32 / 2,
            expanded_position.x + expanded_size.width as i32 / 2
        );
        assert_eq!(
            compact_position.y + compact_size.height as i32 / 2,
            expanded_position.y + expanded_size.height as i32 / 2
        );
    }

    #[test]
    fn monitor_selection_intersection_handles_negative_coordinates() {
        assert_eq!(
            intersection_area(
                PhysicalPosition::new(-300, 100),
                PhysicalSize::new(500, 200),
                PhysicalPosition::new(-1_920, 0),
                PhysicalSize::new(1_920, 1_080),
            ),
            60_000
        );
        assert_eq!(
            intersection_area(
                PhysicalPosition::new(2_000, 100),
                PhysicalSize::new(300, 200),
                PhysicalPosition::new(0, 0),
                PhysicalSize::new(1_920, 1_080),
            ),
            0
        );
    }
}
