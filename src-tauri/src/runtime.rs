use serde::Serialize;
use std::path::PathBuf;
#[cfg(not(windows))]
use std::sync::{Mutex, OnceLock};
#[cfg(not(windows))]
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Clone, Debug)]
pub struct LaunchOptions {
    pub probe_once: bool,
    pub stop: bool,
    pub show: bool,
    pub hidden: bool,
    pub shadow: bool,
    pub sessions_root: PathBuf,
    pub session_index_path: PathBuf,
    pub state_root: PathBuf,
}

impl LaunchOptions {
    pub fn from_env() -> Self {
        Self::from_args(std::env::args_os().skip(1))
    }

    pub fn from_args<I>(args: I) -> Self
    where
        I: IntoIterator<Item = std::ffi::OsString>,
    {
        let profile = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let local_app_data = dirs::data_local_dir().unwrap_or_else(|| {
            #[cfg(windows)]
            {
                profile.join("AppData/Local")
            }
            #[cfg(not(windows))]
            {
                profile.join(".local/share")
            }
        });
        #[cfg(target_os = "linux")]
        let state_directory = "xiaoli";
        #[cfg(not(target_os = "linux"))]
        let state_directory = "XiaoLi";
        let mut result = Self {
            probe_once: false,
            stop: false,
            show: false,
            hidden: false,
            shadow: false,
            sessions_root: profile.join(".codex/sessions"),
            session_index_path: profile.join(".codex/session_index.jsonl"),
            state_root: local_app_data.join(state_directory),
        };

        let args: Vec<_> = args.into_iter().collect();
        let mut state_root_explicit = false;
        let mut index = 0;
        while index < args.len() {
            let value = args[index].to_string_lossy();
            match value.as_ref() {
                "--probe-once" | "-ProbeOnce" => result.probe_once = true,
                "--stop" | "-Stop" => result.stop = true,
                "--show" => result.show = true,
                "--hidden" => result.hidden = true,
                "--shadow" => result.shadow = true,
                "--sessions-root" | "-SessionsRoot" => {
                    if let Some(next) = args.get(index + 1) {
                        result.sessions_root = PathBuf::from(next);
                        index += 1;
                    }
                }
                "--session-index" | "-SessionIndexPath" => {
                    if let Some(next) = args.get(index + 1) {
                        result.session_index_path = PathBuf::from(next);
                        index += 1;
                    }
                }
                "--state-root" | "-StateRoot" => {
                    if let Some(next) = args.get(index + 1) {
                        result.state_root = PathBuf::from(next);
                        state_root_explicit = true;
                        index += 1;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        // Shadow mode is intentionally allowed to coexist with production.
        // Never let an omitted CLI flag make both processes write the same
        // SQLite/cache/log directory.
        if result.shadow && !state_root_explicit {
            result.state_root = result.state_root.join("shadow");
        }
        result
    }
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRuntime {
    pub running: bool,
    pub process_count: usize,
    pub earliest_start_time: Option<u64>,
}

#[cfg(windows)]
pub fn detect_codex_runtime() -> CodexRuntime {
    windows_runtime::detect()
}

#[cfg(not(windows))]
pub fn detect_codex_runtime() -> CodexRuntime {
    static PROCESS_SYSTEM: OnceLock<Mutex<System>> = OnceLock::new();
    let system = PROCESS_SYSTEM.get_or_init(|| Mutex::new(System::new()));
    let Ok(mut system) = system.lock() else {
        return CodexRuntime::default();
    };
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );

    let mut result = CodexRuntime::default();
    for process in system.processes().values() {
        let name = process.name().to_string_lossy();
        if !is_codex_process_name(&name) {
            continue;
        }
        // The packaged App Server can deny command-line reads. The exact
        // executable name is the stable, low-cost signal; this also treats a
        // standalone Codex CLI as Codex running, which is correct for rollout
        // collection and avoids a costly command-line query every five seconds.
        result.running = true;
        result.process_count += 1;
        let started = process.start_time();
        result.earliest_start_time = Some(
            result
                .earliest_start_time
                .map_or(started, |current| current.min(started)),
        );
    }
    result
}

#[cfg(any(not(windows), test))]
fn is_codex_process_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("codex.exe") || name.eq_ignore_ascii_case("codex")
}

#[cfg(windows)]
mod windows_runtime {
    use super::CodexRuntime;
    use std::{
        ffi::c_void,
        mem::size_of,
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;
    const WAIT_FAILED: u32 = 0xffff_ffff;
    const MAX_PATH: usize = 260;
    const FULL_REDISCOVERY_INTERVAL: Duration = Duration::from_secs(30);
    const WINDOWS_TO_UNIX_EPOCH_100NS: u64 = 116_444_736_000_000_000;
    type Handle = *mut c_void;
    const INVALID_HANDLE_VALUE: Handle = -1_isize as Handle;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessEntry32W {
        dwSize: u32,
        cntUsage: u32,
        th32ProcessID: u32,
        th32DefaultHeapID: usize,
        th32ModuleID: u32,
        cntThreads: u32,
        th32ParentProcessID: u32,
        pcPriClassBase: i32,
        dwFlags: u32,
        szExeFile: [u16; MAX_PATH],
    }

    impl Default for ProcessEntry32W {
        fn default() -> Self {
            Self {
                dwSize: size_of::<Self>() as u32,
                cntUsage: 0,
                th32ProcessID: 0,
                th32DefaultHeapID: 0,
                th32ModuleID: 0,
                cntThreads: 0,
                th32ParentProcessID: 0,
                pcPriClassBase: 0,
                dwFlags: 0,
                szExeFile: [0; MAX_PATH],
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    #[allow(non_snake_case)]
    struct FileTime {
        dwLowDateTime: u32,
        dwHighDateTime: u32,
    }

    #[link(name = "Kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
        fn GetProcessTimes(
            process: Handle,
            creation_time: *mut FileTime,
            exit_time: *mut FileTime,
            kernel_time: *mut FileTime,
            user_time: *mut FileTime,
        ) -> i32;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    struct OwnedHandle(Handle);

    impl OwnedHandle {
        fn new(handle: Handle) -> Option<Self> {
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                None
            } else {
                Some(Self(handle))
            }
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: `OwnedHandle` is only constructed for a valid Win32
            // handle and owns it exclusively.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    #[derive(Clone)]
    struct TrackedProcess {
        process_id: u32,
        start_time: Option<u64>,
    }

    #[derive(Default)]
    struct RuntimeDetector {
        tracked: Vec<TrackedProcess>,
        last_full_refresh: Option<Instant>,
    }

    pub(super) fn detect() -> CodexRuntime {
        static DETECTOR: OnceLock<Mutex<RuntimeDetector>> = OnceLock::new();
        let detector = DETECTOR.get_or_init(|| Mutex::new(RuntimeDetector::default()));
        let Ok(mut detector) = detector.lock() else {
            return CodexRuntime::default();
        };
        detector.detect()
    }

    impl RuntimeDetector {
        fn detect(&mut self) -> CodexRuntime {
            let full_refresh_due = self.tracked.is_empty()
                || self
                    .last_full_refresh
                    .is_none_or(|last| last.elapsed() >= FULL_REDISCOVERY_INTERVAL);
            if full_refresh_due {
                return self.refresh_all();
            }

            let mut liveness_unknown = false;
            self.tracked
                .retain(|process| match process_is_alive(process.process_id) {
                    Some(alive) => alive,
                    None => {
                        liveness_unknown = true;
                        false
                    }
                });
            // If every known Codex stopped, rediscover in this same five-second
            // tick so a replacement process never produces a false idle gap.
            if liveness_unknown || self.tracked.is_empty() {
                return self.refresh_all();
            }
            runtime_from_tracked(&self.tracked)
        }

        fn refresh_all(&mut self) -> CodexRuntime {
            let tracked = enumerate_codex_processes();
            let runtime = runtime_from_tracked(&tracked);
            self.tracked = tracked;
            self.last_full_refresh = Some(Instant::now());
            runtime
        }
    }

    fn enumerate_codex_processes() -> Vec<TrackedProcess> {
        // SAFETY: Toolhelp is called with the documented process snapshot flag
        // and the returned handle is immediately wrapped for deterministic close.
        let Some(snapshot) =
            OwnedHandle::new(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })
        else {
            return Vec::new();
        };
        let mut entry = ProcessEntry32W::default();
        // SAFETY: `entry.dwSize` is initialized to the exact C structure size.
        if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
            return Vec::new();
        }

        let mut tracked = Vec::new();
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(entry.szExeFile.len());
            if is_codex_process_name_wide(&entry.szExeFile[..end]) {
                tracked.push(TrackedProcess {
                    process_id: entry.th32ProcessID,
                    start_time: process_start_time(entry.th32ProcessID),
                });
            }

            entry.dwSize = size_of::<ProcessEntry32W>() as u32;
            // SAFETY: the snapshot and output structure remain valid for the
            // duration of the enumeration.
            if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
                break;
            }
        }
        tracked
    }

    fn runtime_from_tracked(processes: &[TrackedProcess]) -> CodexRuntime {
        CodexRuntime {
            running: !processes.is_empty(),
            process_count: processes.len(),
            earliest_start_time: processes
                .iter()
                .filter_map(|process| process.start_time)
                .min(),
        }
    }

    fn is_codex_process_name_wide(name: &[u16]) -> bool {
        const CODEX: [u16; 5] = [
            b'c' as u16,
            b'o' as u16,
            b'd' as u16,
            b'e' as u16,
            b'x' as u16,
        ];
        const CODEX_EXE: [u16; 9] = [
            b'c' as u16,
            b'o' as u16,
            b'd' as u16,
            b'e' as u16,
            b'x' as u16,
            b'.' as u16,
            b'e' as u16,
            b'x' as u16,
            b'e' as u16,
        ];
        let expected: &[u16] = match name.len() {
            5 => &CODEX,
            9 => &CODEX_EXE,
            _ => return false,
        };
        name.iter().zip(expected).all(|(actual, expected)| {
            let folded = if (*actual >= b'A' as u16) && (*actual <= b'Z' as u16) {
                *actual + u16::from(b'a' - b'A')
            } else {
                *actual
            };
            folded == *expected
        })
    }

    fn process_start_time(process_id: u32) -> Option<u64> {
        // SAFETY: PID comes directly from PROCESSENTRY32W. Query-only access
        // cannot mutate the target process.
        let process = OwnedHandle::new(unsafe {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id)
        })?;
        let mut creation = FileTime::default();
        let mut exit = FileTime::default();
        let mut kernel = FileTime::default();
        let mut user = FileTime::default();
        // SAFETY: all FILETIME output pointers are valid for writes.
        if unsafe { GetProcessTimes(process.0, &mut creation, &mut exit, &mut kernel, &mut user) }
            == 0
        {
            return None;
        }
        let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        ticks
            .checked_sub(WINDOWS_TO_UNIX_EPOCH_100NS)
            .map(|unix_ticks| unix_ticks / 10_000_000)
    }

    fn process_is_alive(process_id: u32) -> Option<bool> {
        // SAFETY: PID comes from the last Toolhelp snapshot. SYNCHRONIZE grants
        // no mutation access and is enough for a zero-time liveness query.
        let process = OwnedHandle::new(unsafe { OpenProcess(SYNCHRONIZE, 0, process_id) })?;
        // SAFETY: process is a valid owned handle; timeout zero cannot block.
        match unsafe { WaitForSingleObject(process.0, 0) } {
            WAIT_TIMEOUT => Some(true),
            WAIT_FAILED => None,
            _ => Some(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_testable_roots_and_flags() {
        let options = LaunchOptions::from_args([
            "--probe-once".into(),
            "--sessions-root".into(),
            "C:\\fixtures\\sessions".into(),
            "--session-index".into(),
            "C:\\fixtures\\index.jsonl".into(),
            "--state-root".into(),
            "C:\\fixtures\\state".into(),
            "--shadow".into(),
        ]);
        assert!(options.probe_once);
        assert!(options.shadow);
        assert_eq!(
            options.sessions_root,
            PathBuf::from("C:\\fixtures\\sessions")
        );
        assert_eq!(
            options.session_index_path,
            PathBuf::from("C:\\fixtures\\index.jsonl")
        );
        assert_eq!(options.state_root, PathBuf::from("C:\\fixtures\\state"));
    }

    #[test]
    fn shadow_without_explicit_state_root_is_always_isolated() {
        let production = LaunchOptions::from_args(Vec::<std::ffi::OsString>::new());
        let shadow = LaunchOptions::from_args(["--shadow".into()]);
        assert!(shadow.shadow);
        assert_ne!(shadow.state_root, production.state_root);
        assert_eq!(shadow.state_root, production.state_root.join("shadow"));
    }

    #[test]
    fn codex_process_name_matching_is_exact_and_case_insensitive() {
        assert!(is_codex_process_name("Codex.exe"));
        assert!(is_codex_process_name("codex"));
        assert!(!is_codex_process_name("codex-helper.exe"));
        assert!(!is_codex_process_name("my-codex.exe"));
    }
}
