use atomicwrites::{AllowOverwrite, AtomicFile};
#[cfg(not(windows))]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::{prelude::*, ListenerOptions, Stream};
use serde_json::Value;
#[cfg(windows)]
use std::process::Command;
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

pub type MessageHandler = Arc<dyn Fn(&str) -> Result<Value, String> + Send + Sync + 'static>;

/// Keeps the per-login-session instance mutex alive for the lifetime of the
/// primary process. Windows releases the named mutex automatically if the
/// process exits unexpectedly, so a crash cannot permanently poison startup.
#[cfg(windows)]
pub struct InstanceGuard(*mut std::ffi::c_void);

#[cfg(not(windows))]
pub struct InstanceGuard(std::fs::File);

#[cfg(windows)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Returns `Some` for the primary process and `None` when another XiaoLi
/// process in the current login session already owns the mutex.
#[cfg(windows)]
pub fn acquire_instance_guard() -> Result<Option<InstanceGuard>, String> {
    let name = format!("Local\\{}.Instance", pipe_name_for_current_user());
    acquire_instance_guard_named(&name)
}

/// Uses a state-root-scoped guard for shadow validation so a read-only shadow
/// process can run beside production while duplicate shadows using the same
/// isolated state root are still rejected.
#[cfg(windows)]
pub fn acquire_shadow_instance_guard(state_root: &Path) -> Result<Option<InstanceGuard>, String> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    state_root
        .to_string_lossy()
        .to_ascii_lowercase()
        .hash(&mut hasher);
    let name = format!(
        "Local\\{}.Shadow.{:016x}",
        pipe_name_for_current_user(),
        hasher.finish()
    );
    acquire_instance_guard_named(&name)
}

#[cfg(not(windows))]
pub fn acquire_shadow_instance_guard(state_root: &Path) -> Result<Option<InstanceGuard>, String> {
    prepare_private_directory(state_root)?;
    acquire_instance_guard_at(&state_root.join("xiaoli.shadow.instance.lock"))
}

#[cfg(not(windows))]
pub fn acquire_instance_guard() -> Result<Option<InstanceGuard>, String> {
    let directory = unix_runtime_directory()?;
    acquire_instance_guard_at(&directory.join("xiaoli.instance.lock"))
}

#[cfg(not(windows))]
fn acquire_instance_guard_at(lock_path: &Path) -> Result<Option<InstanceGuard>, String> {
    use fs2::FileExt;
    use std::fs::OpenOptions;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|error| format!("open {}: {error}", lock_path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(InstanceGuard(file))),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(format!("lock {}: {error}", lock_path.display())),
    }
}

#[cfg(not(windows))]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

#[cfg(windows)]
fn acquire_instance_guard_named(name: &str) -> Result<Option<InstanceGuard>, String> {
    const ERROR_ALREADY_EXISTS: u32 = 183;
    let wide = name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(format!("CreateMutexW failed: {}", unsafe {
            GetLastError()
        }));
    }
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_exists {
        unsafe {
            CloseHandle(handle);
        }
        Ok(None)
    } else {
        Ok(Some(InstanceGuard(handle)))
    }
}

#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    fn CreateMutexW(
        mutex_attributes: *mut std::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn GetLastError() -> u32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
pub fn pipe_name_for_current_user() -> String {
    let sid = Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .ok()
        // `whoami` encodes the localized account name using the console code
        // page, so the full CSV is not guaranteed to be UTF-8. Lossy decoding
        // preserves the ASCII SID that follows it.
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .and_then(|line| {
            line.split(',')
                .nth(1)
                .map(|value| value.trim().trim_matches('"').to_string())
        })
        .filter(|value| {
            value.starts_with("S-1-")
                && value[4..]
                    .chars()
                    .all(|ch| ch.is_ascii_digit() || ch == '-')
        })
        .unwrap_or_else(|| {
            let sanitized = std::env::var("USERNAME")
                .unwrap_or_else(|_| "current-user".to_string())
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
                .collect::<String>();
            if sanitized.is_empty() {
                "current-user".to_owned()
            } else {
                sanitized
            }
        });
    format!("OpenAI.Codex.ModelMonitor.{sid}")
}

#[cfg(not(windows))]
pub fn pipe_name_for_current_user() -> String {
    format!("XiaoLi.{}", effective_user_id())
}

pub fn default_state_root() -> PathBuf {
    if let Some(path) = std::env::var_os("XIAOLI_STATE_DIR") {
        return PathBuf::from(path);
    }
    let base = dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    #[cfg(target_os = "linux")]
    return base.join("xiaoli");
    #[cfg(not(target_os = "linux"))]
    base.join("XiaoLi")
}

pub fn start_hook_listener(state_root: &Path, handler: MessageHandler) -> Result<String, String> {
    prepare_private_directory(state_root)?;
    let endpoint = create_endpoint()?;
    let listener = endpoint.create_listener()?;

    let endpoint_info = endpoint.descriptor();
    write_atomic_json(&state_root.join("ipc-endpoint.json"), &endpoint_info)?;
    #[cfg(windows)]
    write_atomic_json(&state_root.join("pipe-name.json"), &endpoint_info)?;

    thread::Builder::new()
        .name("xiaoli-hook-ipc".to_string())
        .spawn(move || {
            for connection in listener.incoming() {
                let Ok(mut connection) = connection else {
                    continue;
                };
                // Named pipes do not support OS read timeouts in interprocess.
                // Nonblocking polling gives every client a hard deadline, so a
                // same-user peer that never sends a newline cannot freeze hook,
                // MCP and control traffic behind one serial read.
                let result =
                    read_bounded_line(&mut connection, 16 * 1024, Duration::from_millis(500))
                        .map_err(|error| error.to_string())
                        .and_then(|payload| handler(payload.trim_end()));
                let response = match result {
                    Ok(value) => serde_json::to_vec(&value).unwrap_or_else(|_| {
                        b"{\"ok\":false,\"error\":\"serialization_failed\"}".to_vec()
                    }),
                    Err(error) => serde_json::to_vec(&serde_json::json!({
                        "ok": false,
                        "error": error.chars().take(160).collect::<String>()
                    }))
                    .unwrap_or_else(|_| b"{\"ok\":false}".to_vec()),
                };
                let _ = connection.write_all(&response);
                let _ = connection.write_all(b"\n");
                let _ = connection.flush();
            }
        })
        .map_err(|error| error.to_string())?;

    Ok(endpoint.display_name())
}

pub fn send_request(payload: &Value) -> Result<Value, String> {
    send_request_at(&default_state_root(), payload)
}

pub fn send_request_at(state_root: &Path, payload: &Value) -> Result<Value, String> {
    let endpoint = connect_endpoint(state_root)?;
    let mut stream = endpoint
        .connect()
        .map_err(|error| format!("connect IPC endpoint: {error}"))?;
    let mut request = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
    request.push(b'\n');
    stream
        .write_all(&request)
        .map_err(|error| format!("write IPC request: {error}"))?;
    stream
        .flush()
        .map_err(|error| format!("flush IPC request: {error}"))?;
    let response = read_bounded_line(&mut stream, 1024 * 1024, Duration::from_secs(5))
        .map_err(|error| format!("read IPC response: {error}"))?;
    serde_json::from_str(response.trim_end()).map_err(|error| error.to_string())
}

fn read_bounded_line(
    stream: &mut Stream,
    max_bytes: usize,
    timeout: Duration,
) -> io::Result<String> {
    let deadline = Instant::now() + timeout;
    set_nonblocking_for_bounded_read(stream, deadline)?;
    let result = read_bounded_line_nonblocking(stream, max_bytes, deadline);
    let restore_result = stream.set_nonblocking(false);
    match (result, restore_result) {
        (Ok(line), Ok(())) => Ok(line),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn set_nonblocking_for_bounded_read(stream: &Stream, deadline: Instant) -> io::Result<()> {
    loop {
        match stream.set_nonblocking(true) {
            Ok(()) => return Ok(()),
            Err(error) if is_windows_pipe_mode_pending(&error) => {
                // A Windows client can connect to a listening named-pipe
                // instance before the server thread accepts it. During that
                // short interval SetNamedPipeHandleState(PIPE_NOWAIT) returns
                // ERROR_PIPE_BUSY even though the connection is valid.
                wait_for_ipc_data(deadline)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn read_bounded_line_nonblocking(
    stream: &mut Stream,
    max_bytes: usize,
    deadline: Instant,
) -> io::Result<String> {
    let mut bytes = Vec::with_capacity(512.min(max_bytes));
    let mut buffer = [0_u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => {
                #[cfg(windows)]
                {
                    // interprocess maps both an empty PIPE_NOWAIT byte pipe
                    // and a disconnected byte pipe to Ok(0). PeekNamedPipe
                    // distinguishes a connected empty pipe from EOF before
                    // the bounded poll waits again.
                    wait_for_windows_pipe_data(stream, deadline)?;
                    continue;
                }
                #[cfg(not(windows))]
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "IPC connection closed before newline",
                ));
            }
            Ok(read) => {
                let end = buffer[..read]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(read, |index| index + 1);
                if bytes.len().saturating_add(end) > max_bytes {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "IPC line exceeds size limit",
                    ));
                }
                bytes.extend_from_slice(&buffer[..end]);
                if end < read || bytes.last() == Some(&b'\n') {
                    return String::from_utf8(bytes).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "IPC line is not UTF-8")
                    });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || is_windows_pipe_read_empty(&error) =>
            {
                wait_for_ipc_data(deadline)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn is_windows_pipe_mode_pending(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        // ERROR_PIPE_BUSY. A named-pipe client can connect and write before
        // the server thread accepts the instance. Enabling PIPE_NOWAIT during
        // that interval returns 231, so retry within the shared hard deadline.
        error.raw_os_error() == Some(231)
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

fn is_windows_pipe_read_empty(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        // ERROR_NO_DATA. In PIPE_NOWAIT mode this is the documented result for
        // an empty pipe whose peer is still connected; it is not EOF.
        error.raw_os_error() == Some(232)
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

#[cfg(windows)]
fn wait_for_windows_pipe_data(stream: &Stream, deadline: Instant) -> io::Result<()> {
    use std::os::windows::io::{AsHandle, AsRawHandle};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let Stream::NamedPipe(pipe) = stream;
    let result = unsafe {
        PeekNamedPipe(
            pipe.as_handle().as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    wait_for_ipc_data(deadline)
}

fn wait_for_ipc_data(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "IPC line timed out",
        ));
    }
    thread::sleep(Duration::from_millis(2));
    Ok(())
}

fn write_atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| file.write_all(value.to_string().as_bytes()))
        .map_err(|error| error.to_string())
}

enum LocalEndpoint {
    #[cfg(windows)]
    NamedPipe(String),
    #[cfg(not(windows))]
    UnixSocket(PathBuf),
}

impl LocalEndpoint {
    fn display_name(&self) -> String {
        match self {
            #[cfg(windows)]
            Self::NamedPipe(name) => name.clone(),
            #[cfg(not(windows))]
            Self::UnixSocket(path) => path.to_string_lossy().into_owned(),
        }
    }

    fn descriptor(&self) -> Value {
        match self {
            #[cfg(windows)]
            Self::NamedPipe(name) => serde_json::json!({
                "schemaVersion": 1,
                "transport": "windows-named-pipe",
                "pipeName": name
            }),
            #[cfg(not(windows))]
            Self::UnixSocket(path) => serde_json::json!({
                "schemaVersion": 1,
                "transport": "unix-domain-socket",
                "path": path
            }),
        }
    }

    fn create_listener(&self) -> Result<interprocess::local_socket::Listener, String> {
        match self {
            #[cfg(windows)]
            Self::NamedPipe(pipe_name) => {
                let name = pipe_name
                    .clone()
                    .to_ns_name::<GenericNamespaced>()
                    .map_err(|error| error.to_string())?;
                let options = secure_listener_options(ListenerOptions::new().name(name))?;
                options.create_sync().map_err(|error| error.to_string())
            }
            #[cfg(not(windows))]
            Self::UnixSocket(path) => {
                use interprocess::os::unix::local_socket::ListenerOptionsExt;
                use std::os::unix::fs::PermissionsExt;

                if path.exists() {
                    fs::remove_file(path)
                        .map_err(|error| format!("remove stale {}: {error}", path.display()))?;
                }
                let make_name = || {
                    path.clone()
                        .to_fs_name::<GenericFilePath>()
                        .map_err(|error| error.to_string())
                };
                let listener = match ListenerOptions::new()
                    .name(make_name()?)
                    .mode(0o600)
                    .create_sync()
                {
                    Ok(listener) => listener,
                    // Some Unix implementations do not support applying the
                    // mode before bind. The containing 0700 directory still
                    // prevents cross-user access during this fallback.
                    Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                        ListenerOptions::new()
                            .name(make_name()?)
                            .create_sync()
                            .map_err(|error| error.to_string())?
                    }
                    Err(error) => return Err(error.to_string()),
                };
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                    .map_err(|error| format!("chmod {}: {error}", path.display()))?;
                Ok(listener)
            }
        }
    }

    fn connect(&self) -> Result<Stream, String> {
        match self {
            #[cfg(windows)]
            Self::NamedPipe(pipe_name) => {
                let name = pipe_name
                    .clone()
                    .to_ns_name::<GenericNamespaced>()
                    .map_err(|error| error.to_string())?;
                Stream::connect(name).map_err(|error| error.to_string())
            }
            #[cfg(not(windows))]
            Self::UnixSocket(path) => {
                let name = path
                    .clone()
                    .to_fs_name::<GenericFilePath>()
                    .map_err(|error| error.to_string())?;
                Stream::connect(name).map_err(|error| error.to_string())
            }
        }
    }
}

fn create_endpoint() -> Result<LocalEndpoint, String> {
    #[cfg(windows)]
    {
        Ok(LocalEndpoint::NamedPipe(pipe_name_for_current_user()))
    }
    #[cfg(not(windows))]
    {
        Ok(LocalEndpoint::UnixSocket(
            unix_runtime_directory()?.join("xiaoli.sock"),
        ))
    }
}

fn connect_endpoint(state_root: &Path) -> Result<LocalEndpoint, String> {
    #[cfg(windows)]
    {
        let descriptor_path = state_root.join("ipc-endpoint.json");
        if let Ok(bytes) = fs::read(&descriptor_path) {
            if bytes.len() <= 16 * 1024 {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if value.get("transport").and_then(Value::as_str) == Some("windows-named-pipe")
                    {
                        if let Some(name) = value.get("pipeName").and_then(Value::as_str) {
                            let safe = name.starts_with("OpenAI.Codex.ModelMonitor.")
                                && name.chars().all(|character| {
                                    character.is_ascii_alphanumeric()
                                        || character == '.'
                                        || character == '-'
                                        || character == '_'
                                });
                            if safe {
                                return Ok(LocalEndpoint::NamedPipe(name.to_owned()));
                            }
                        }
                    }
                }
            }
        }
        Ok(LocalEndpoint::NamedPipe(pipe_name_for_current_user()))
    }
    #[cfg(not(windows))]
    {
        let descriptor_path = state_root.join("ipc-endpoint.json");
        if let Ok(bytes) = fs::read(&descriptor_path) {
            if bytes.len() <= 16 * 1024 {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if value.get("transport").and_then(Value::as_str) == Some("unix-domain-socket")
                    {
                        if let Some(path) = value.get("path").and_then(Value::as_str) {
                            return Ok(LocalEndpoint::UnixSocket(PathBuf::from(path)));
                        }
                    }
                }
            }
        }
        create_endpoint()
    }
}

#[cfg(not(windows))]
fn effective_user_id() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot mutate process state.
    unsafe { libc::geteuid() }
}

#[cfg(not(windows))]
fn unix_runtime_directory() -> Result<PathBuf, String> {
    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("xiaoli-{}", effective_user_id()));
    prepare_private_directory(&directory)?;
    Ok(directory)
}

fn prepare_private_directory(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("chmod {}: {error}", directory.display()))?;
    }
    Ok(())
}

#[cfg(windows)]
fn secure_listener_options(options: ListenerOptions<'_>) -> Result<ListenerOptions<'_>, String> {
    use interprocess::os::windows::{
        local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
    };
    use widestring::U16CString;

    // Protected DACL. The object owner (the current user), SYSTEM, and local
    // administrators receive full access. Other interactive users receive none.
    let sddl = U16CString::from_str("D:P(A;;GA;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)")
        .map_err(|error| error.to_string())?;
    let descriptor =
        SecurityDescriptor::deserialize(sddl.as_ucstr()).map_err(|error| error.to_string())?;
    Ok(options.security_descriptor(descriptor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    };

    static NEXT_ENDPOINT: AtomicU64 = AtomicU64::new(1);

    fn unique_test_endpoint(label: &str) -> LocalEndpoint {
        let nonce = NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed);
        #[cfg(windows)]
        {
            LocalEndpoint::NamedPipe(format!(
                "OpenAI.Codex.ModelMonitor.Test.{label}.{}.{nonce}",
                std::process::id()
            ))
        }
        #[cfg(not(windows))]
        {
            LocalEndpoint::UnixSocket(std::env::temp_dir().join(format!(
                "xiaoli-ipc-{label}-{}-{nonce}.sock",
                std::process::id()
            )))
        }
    }

    fn cleanup_test_endpoint(endpoint: &LocalEndpoint) {
        #[cfg(not(windows))]
        match endpoint {
            LocalEndpoint::UnixSocket(path) => {
                let _ = fs::remove_file(path);
            }
        }
        #[cfg(windows)]
        let _ = endpoint;
    }

    fn read_test_payload(label: &str, max_bytes: usize, chunks: &[&[u8]]) -> io::Result<String> {
        let endpoint = unique_test_endpoint(label);
        let listener = endpoint.create_listener().expect("test IPC listener");
        let (accepted_tx, accepted_rx) = mpsc::sync_channel(0);
        let server = thread::spawn(move || {
            let mut connection = listener
                .incoming()
                .next()
                .expect("incoming stream")
                .expect("accepted stream");
            accepted_tx.send(()).expect("signal accepted stream");
            read_bounded_line(&mut connection, max_bytes, Duration::from_millis(500))
        });

        let mut client = endpoint.connect().expect("test IPC client");
        accepted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server should accept the test client");
        for chunk in chunks {
            client.write_all(chunk).expect("write test payload chunk");
            client.flush().expect("flush test payload chunk");
        }
        drop(client);

        let result = server.join().expect("server thread");
        cleanup_test_endpoint(&endpoint);
        result
    }

    #[cfg(windows)]
    #[test]
    fn windows_nonblocking_pipe_waits_for_a_delayed_first_byte() {
        let endpoint = unique_test_endpoint("delayed-first-byte");
        let listener = endpoint.create_listener().expect("test pipe listener");
        let (accepted_tx, accepted_rx) = mpsc::sync_channel(0);
        let server = thread::spawn(move || {
            let mut connection = listener
                .incoming()
                .next()
                .expect("incoming stream")
                .expect("accepted stream");
            accepted_tx.send(()).expect("signal accepted stream");
            read_bounded_line(&mut connection, 1024, Duration::from_millis(500))
        });

        let mut client = endpoint.connect().expect("test pipe client");
        accepted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server should accept before the delayed write");
        // The acceptance barrier makes the server ready before this deliberate
        // delay. That gives read_bounded_line a stable empty PIPE_NOWAIT poll
        // before the first request byte arrives.
        thread::sleep(Duration::from_millis(50));
        client
            .write_all(b"{\"method\":\"ping\"}\n")
            .expect("write delayed request");
        client.flush().expect("flush delayed request");

        let received = server
            .join()
            .expect("server thread")
            .expect("server should wait for the delayed request");
        assert_eq!(received, "{\"method\":\"ping\"}\n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_client_waits_for_delayed_response_bytes() {
        let endpoint = unique_test_endpoint("delayed-client-response");
        let listener = endpoint.create_listener().expect("test pipe listener");
        let (allow_accept_tx, allow_accept_rx) = mpsc::sync_channel(1);
        let (request_read_tx, request_read_rx) = mpsc::sync_channel(1);
        let (allow_response_tx, allow_response_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || -> io::Result<()> {
            allow_accept_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("client should enter its response read");
            let mut connection = listener
                .incoming()
                .next()
                .expect("incoming stream")
                .expect("accepted stream");
            let request = read_bounded_line(&mut connection, 1024, Duration::from_millis(500))?;
            assert_eq!(request, "{\"method\":\"summary\"}\n");
            request_read_tx
                .send(())
                .expect("signal that the server read the request");
            allow_response_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("client should allow the delayed response");
            connection.write_all(b"{\"ok\":true}\n")?;
            connection.flush()
        });

        let mut client = endpoint.connect().expect("test pipe client");
        client
            .write_all(b"{\"method\":\"summary\"}\n")
            .expect("write request");
        client.flush().expect("flush request");

        let (reader_started_tx, reader_started_rx) = mpsc::sync_channel(0);
        let (reader_result_tx, reader_result_rx) = mpsc::sync_channel(1);
        let client_reader = thread::spawn(move || {
            reader_started_tx
                .send(())
                .expect("signal before the delayed response read");
            let result = read_bounded_line(&mut client, 1024, Duration::from_secs(1));
            reader_result_tx
                .send(result)
                .expect("return the client read result");
        });
        reader_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("client reader should start");
        // The client must remain inside the bounded read while the server has
        // not accepted the already-connected pipe. The old implementation
        // returned raw OS error 231 immediately in this exact state.
        match reader_result_rx.recv_timeout(Duration::from_millis(150)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("client reader disconnected before server acceptance")
            }
            Ok(Ok(line)) => panic!("client read returned before server acceptance: {line:?}"),
            Ok(Err(error)) => panic!(
                "client read failed before server acceptance: raw_os_error={:?}: {error}",
                error.raw_os_error()
            ),
        }
        allow_accept_tx
            .send(())
            .expect("allow the server to accept and respond");
        request_read_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server should accept and read the request");
        // Once accepted, an empty client-side PIPE_NOWAIT read reports
        // ERROR_NO_DATA (232). That is also temporary until the same deadline.
        match reader_result_rx.recv_timeout(Duration::from_millis(150)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("client reader disconnected before the delayed response")
            }
            Ok(Ok(line)) => panic!("client read returned before the response: {line:?}"),
            Ok(Err(error)) => panic!(
                "client read failed before the response: raw_os_error={:?}: {error}",
                error.raw_os_error()
            ),
        }
        allow_response_tx
            .send(())
            .expect("allow the server to send its response");

        let response = reader_result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("client should finish after the server responds")
            .expect("client should wait for the delayed response");
        client_reader.join().expect("client reader thread");
        server
            .join()
            .expect("server thread")
            .expect("server should write the delayed response");
        assert_eq!(response, "{\"ok\":true}\n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipe_mode_and_empty_read_errors_are_classified_exactly() {
        let pipe_busy = io::Error::from_raw_os_error(231);
        let no_data = io::Error::from_raw_os_error(232);
        let broken_pipe = io::Error::from_raw_os_error(109);

        assert!(is_windows_pipe_mode_pending(&pipe_busy));
        assert!(!is_windows_pipe_mode_pending(&no_data));
        assert!(!is_windows_pipe_mode_pending(&broken_pipe));

        assert!(is_windows_pipe_read_empty(&no_data));
        assert!(!is_windows_pipe_read_empty(&pipe_busy));
        assert!(!is_windows_pipe_read_empty(&broken_pipe));
    }

    #[cfg(windows)]
    #[test]
    fn windows_peer_close_does_not_wait_for_the_read_deadline() {
        let endpoint = unique_test_endpoint("peer-close");
        let listener = endpoint.create_listener().expect("test pipe listener");
        let (accepted_tx, accepted_rx) = mpsc::sync_channel(0);
        let (allow_close_tx, allow_close_rx) = mpsc::sync_channel(0);
        let server = thread::spawn(move || {
            let connection = listener
                .incoming()
                .next()
                .expect("incoming stream")
                .expect("accepted stream");
            accepted_tx.send(()).expect("signal accepted stream");
            allow_close_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("client should allow the peer close");
            drop(connection);
        });

        let mut client = endpoint.connect().expect("test pipe client");
        accepted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server should accept the client");
        let (reader_started_tx, reader_started_rx) = mpsc::sync_channel(0);
        let (reader_result_tx, reader_result_rx) = mpsc::sync_channel(1);
        let client_reader = thread::spawn(move || {
            reader_started_tx.send(()).expect("signal client read");
            let result = read_bounded_line(&mut client, 1024, Duration::from_secs(1));
            reader_result_tx
                .send(result)
                .expect("return client read result");
        });
        reader_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("client reader should start");
        assert!(matches!(
            reader_result_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        allow_close_tx.send(()).expect("close the server peer");
        server.join().expect("server thread");
        let error = reader_result_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("peer close should be detected before the hard deadline")
            .expect_err("peer close cannot produce a complete line");
        client_reader.join().expect("client reader thread");
        assert_ne!(error.kind(), io::ErrorKind::TimedOut);
        assert!(matches!(error.raw_os_error(), Some(109 | 233)));
    }

    #[cfg(windows)]
    #[test]
    fn bounded_read_restores_blocking_mode_before_a_large_response() {
        let endpoint = unique_test_endpoint("blocking-response");
        let listener = endpoint.create_listener().expect("test pipe listener");
        let response = vec![b'x'; 1024 * 1024];
        let expected_len = response.len();
        let (writing_tx, writing_rx) = mpsc::sync_channel(0);
        let server = thread::spawn(move || {
            let mut connection = listener
                .incoming()
                .next()
                .expect("incoming stream")
                .expect("accepted stream");
            let request = read_bounded_line(&mut connection, 1024, Duration::from_millis(500))?;
            assert_eq!(request, "{\"method\":\"large\"}\n");
            writing_tx
                .send(())
                .expect("signal before writing the large response");
            connection.write_all(&response)
        });

        let mut client = endpoint.connect().expect("test pipe client");
        client
            .write_all(b"{\"method\":\"large\"}\n")
            .expect("write request");
        client.flush().expect("flush request");
        writing_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server should reach the large response write");
        // The barrier proves the server is at write_all before the client
        // deliberately waits. A stream left in PIPE_NOWAIT fills its pipe
        // buffer and turns write_all into WriteZero here.
        thread::sleep(Duration::from_millis(100));
        let mut received = Vec::with_capacity(expected_len);
        client
            .read_to_end(&mut received)
            .expect("read complete response");

        server
            .join()
            .expect("server thread")
            .expect("large response should use restored blocking mode");
        assert_eq!(received.len(), expected_len);
        assert!(received.iter().all(|byte| *byte == b'x'));
    }

    #[cfg(windows)]
    #[test]
    fn timed_out_client_does_not_block_the_next_pipe_connection() {
        let endpoint = unique_test_endpoint("timeout-recovery");
        let listener = endpoint.create_listener().expect("test pipe listener");
        let (first_accepted_tx, first_accepted_rx) = mpsc::sync_channel(0);
        let (first_done_tx, first_done_rx) = mpsc::sync_channel(0);
        let server = thread::spawn(move || -> io::Result<String> {
            let mut stalled = listener.incoming().next().expect("first incoming stream")?;
            first_accepted_tx
                .send(())
                .expect("signal first accepted stream");
            let error = read_bounded_line(&mut stalled, 1024, Duration::from_millis(100))
                .expect_err("silent client must time out");
            first_done_tx
                .send(error.kind())
                .expect("signal first read result");
            drop(stalled);

            let mut healthy = listener
                .incoming()
                .next()
                .expect("second incoming stream")?;
            read_bounded_line(&mut healthy, 1024, Duration::from_millis(500))
        });

        let stalled_client = endpoint.connect().expect("stalled test pipe client");
        first_accepted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server should accept the stalled client");
        assert_eq!(
            first_done_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("silent client should hit the hard deadline"),
            io::ErrorKind::TimedOut
        );

        let mut healthy_client = endpoint.connect().expect("healthy test pipe client");
        healthy_client
            .write_all(b"{\"method\":\"ping\"}\n")
            .expect("write healthy request");
        healthy_client.flush().expect("flush healthy request");

        let received = server
            .join()
            .expect("server thread")
            .expect("listener should recover for the next client");
        drop(stalled_client);
        assert_eq!(received, "{\"method\":\"ping\"}\n");
    }

    #[test]
    fn bounded_read_assembles_a_line_across_multiple_reads() {
        let first = vec![b'a'; 1500];
        let result = read_test_payload("partial-line", 4096, &[&first, b"tail\n"])
            .expect("split line should be reassembled");
        assert_eq!(result.len(), first.len() + 5);
        assert!(result.starts_with(&"a".repeat(first.len())));
        assert!(result.ends_with("tail\n"));
    }

    #[test]
    fn bounded_read_rejects_a_line_over_the_size_limit() {
        let error = read_test_payload("oversized-line", 8, &[b"123456789\n"])
            .expect_err("oversized line must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "IPC line exceeds size limit");
    }

    #[test]
    fn bounded_read_rejects_invalid_utf8() {
        let error = read_test_payload("invalid-utf8", 16, &[&[0xff, b'\n']])
            .expect_err("invalid UTF-8 must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "IPC line is not UTF-8");
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_peer_close_before_newline_is_unexpected_eof() {
        let error = read_test_payload("unix-eof", 1024, &[b"partial"])
            .expect_err("Unix peer close before newline must be EOF");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(error.to_string(), "IPC connection closed before newline");
    }

    #[test]
    fn pipe_name_contains_only_safe_namespace_characters() {
        let name = pipe_name_for_current_user();
        #[cfg(windows)]
        {
            assert!(name.starts_with("OpenAI.Codex.ModelMonitor."));
            assert!(name.len() > "OpenAI.Codex.ModelMonitor.".len());
            assert!(
                name.contains("S-1-"),
                "Windows pipe name should use the user SID: {name}"
            );
        }
        #[cfg(not(windows))]
        assert_eq!(name, format!("XiaoLi.{}", effective_user_id()));
        assert!(name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_'));
    }

    #[test]
    fn shadow_instance_guard_is_isolated_by_state_root() {
        let base = std::env::temp_dir().join(format!(
            "xiaoli-shadow-instance-test-{}",
            std::process::id()
        ));
        let first_root = base.join("one");
        let second_root = base.join("two");
        let first = acquire_shadow_instance_guard(&first_root)
            .expect("first shadow guard")
            .expect("first shadow owner");
        assert!(acquire_shadow_instance_guard(&first_root)
            .expect("duplicate shadow guard")
            .is_none());
        let independent = acquire_shadow_instance_guard(&second_root)
            .expect("independent shadow guard")
            .expect("different state root must be independent");
        drop(independent);
        drop(first);
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn early_instance_mutex_allows_only_one_owner_and_recovers_after_drop() {
        let name = format!(
            "Local\\OpenAI.Codex.ModelMonitor.Test.{}.Instance",
            std::process::id()
        );
        let first = acquire_instance_guard_named(&name)
            .expect("first mutex acquisition should succeed")
            .expect("first caller should own the mutex");
        assert!(
            acquire_instance_guard_named(&name)
                .expect("second mutex acquisition should be observable")
                .is_none(),
            "second caller must not become a primary instance"
        );
        drop(first);
        assert!(
            acquire_instance_guard_named(&name)
                .expect("mutex should be reusable after owner exit")
                .is_some(),
            "a crashed or exited owner must not poison future launches"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_file_lock_allows_only_one_owner_and_recovers_after_drop() {
        let directory = std::env::temp_dir().join(format!(
            "xiaoli-instance-test-{}-{}",
            effective_user_id(),
            std::process::id()
        ));
        prepare_private_directory(&directory).expect("private directory");
        let path = directory.join("instance.lock");
        let first = acquire_instance_guard_at(&path)
            .expect("first lock")
            .expect("first owner");
        assert!(acquire_instance_guard_at(&path)
            .expect("second lock result")
            .is_none());
        drop(first);
        assert!(acquire_instance_guard_at(&path)
            .expect("recovered lock")
            .is_some());
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&directory);
    }
}
