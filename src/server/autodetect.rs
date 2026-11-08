//! Auto-detect launch behavior for the `herdr` command.
//!
//! When the user runs `herdr` with no subcommand:
//! 1. Check if a server is already listening on the client socket
//! 2. If no server → spawn one as a background daemon → wait for socket readiness (up to 15s)
//! 3. Attach as a client to the server

use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracing::info;

use super::socket_paths::client_socket_path;

/// Maximum time to wait for the server's client socket to become ready
/// after spawning the server process.
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Poll interval when waiting for the server socket to appear.
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Timeout for checking the stable JSON API before attaching to the binary protocol socket.
const STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Private daemon-start hint used to seed a fresh headless server from the
/// directory where the user ran `herdr`.
pub(crate) const STARTUP_CWD_ENV_VAR: &str = "HERDR_STARTUP_CWD";

// ---------------------------------------------------------------------------
// Server detection
// ---------------------------------------------------------------------------

/// Checks whether a herdr server is currently listening on the client socket.
///
/// This works by attempting to connect to the client socket. If the connection
/// succeeds, a server is running. If the socket file doesn't exist or the
/// connection is refused, no server is running. Stale sockets (from a crashed
/// server) are detected because connect returns `ConnectionRefused`
/// when nobody is listening.
#[allow(dead_code)] // Public API for external use and testing
pub fn is_server_listening() -> io::Result<bool> {
    is_server_listening_at(&client_socket_path())
}

/// Checks whether a herdr server is listening at a specific socket path.
fn is_server_listening_at(socket_path: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        match crate::platform::probe_local_server(socket_path) {
            Ok(()) => Ok(true),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    #[cfg(not(windows))]
    {
        if !socket_path.exists() {
            return Ok(false);
        }

        Ok(match crate::ipc::connect_local_stream(socket_path) {
            Ok(_) => {
                // Server is listening. Close the test connection immediately.
                // The server's handshake handler will time out on this connection
                // since we don't send a handshake, which is fine.
                true
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
                ) =>
            {
                // Socket file exists but nobody is listening — stale socket.
                false
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // Socket file disappeared between exists() and connect().
                false
            }
            Err(err) => {
                // Other errors (permission denied, etc.) — assume not listening.
                tracing::warn!(err = %err, "unexpected error checking server socket");
                false
            }
        })
    }
}

#[cfg(windows)]
fn client_protocol_accepts_hello(socket_path: &Path, cancelled: &AtomicBool) -> io::Result<bool> {
    if !socket_path.exists() {
        return Ok(false);
    }

    let mut stream = match crate::ipc::connect_local_stream_cancellable(
        socket_path,
        cancelled,
        Instant::now() + STATUS_REQUEST_TIMEOUT,
    ) {
        Ok(stream) => stream,
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::NotFound
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::WouldBlock
            ) =>
        {
            return Ok(false);
        }
        Err(err) => return Err(err),
    };

    let hello = crate::protocol::ClientMessage::TerminalHello {
        version: crate::protocol::PROTOCOL_VERSION,
        cols: 80,
        rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
    };

    match crate::protocol::write_message(&mut stream, &hello) {
        Ok(()) => Ok(true),
        Err(crate::protocol::FramingError::Io(err))
            if matches!(
                err.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::NotFound
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::WouldBlock
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
            ) =>
        {
            Ok(false)
        }
        Err(err) => Err(io::Error::other(err.to_string())),
    }
}

fn validate_running_server_compatibility(saved_federation: bool) -> io::Result<()> {
    validate_running_server_compatibility_at(
        &crate::api::socket_path(),
        saved_federation,
        &crate::session::active_restart_after_update_guidance(),
    )
}

fn validate_running_server_compatibility_at(
    api_socket: &Path,
    saved_federation: bool,
    restart_guidance: &str,
) -> io::Result<()> {
    let Some(status) = crate::api::read_runtime_status_at(api_socket, STATUS_REQUEST_TIMEOUT)?
    else {
        return Err(io::Error::other(format!(
            "a herdr server is listening, but its status API is unavailable.\n\n{}\nIf that fails, stop the old server process manually.",
            restart_guidance
        )));
    };

    let capabilities = status.capabilities.as_ref();
    let endpoint_generation =
        capabilities.and_then(|capabilities| capabilities.endpoint_protocol_generation);
    let surface_interest = capabilities.is_some_and(|capabilities| capabilities.surface_interest);
    if endpoint_generation == Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
        && (!saved_federation || surface_interest)
    {
        return Ok(());
    }

    let requirement = if saved_federation && !surface_interest {
        "saved SSH machines require surface lifecycle support"
    } else {
        "the stable endpoint generation is incompatible"
    };
    Err(io::Error::other(format!(
        "This session needs one final server update before Herdr can attach ({requirement}).\n\nserver: v{} endpoint generation {}\nclient: v{} endpoint generation {}\n\n{}",
        status.version.as_deref().unwrap_or("unknown"),
        endpoint_generation
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unavailable".to_string()),
        crate::build_info::version(),
        crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
        restart_guidance
    )))
}

// ---------------------------------------------------------------------------
// Server spawning
// ---------------------------------------------------------------------------

/// Spawns the herdr server as a background daemon process.
///
/// The server process is fully detached:
/// - Runs in its own session (setsid) so it survives the client exiting
/// - Stdin/stdout/stderr are redirected to /dev/null
/// - Inherits relevant environment variables (`XDG_CONFIG_HOME`, `HERDR_SESSION`,
///   socket overrides, etc.), except inherited socket overrides are cleared when
///   this CLI invocation explicitly selected a session.
///
/// Returns the PID of the spawned server process.
pub fn spawn_server_daemon() -> io::Result<u32> {
    spawn_server_daemon_for_session(None)
}

fn spawn_server_daemon_for_session(session: Option<&str>) -> io::Result<u32> {
    let exe = std::env::current_exe().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("failed to determine herdr executable path: {err}"),
        )
    })?;

    info!(exe = %exe.display(), "spawning server daemon");

    let mut command = match session {
        Some(name) => build_session_server_daemon_command(exe, name),
        None => build_server_daemon_command(exe),
    };

    let pid =
        crate::platform::launch_server_daemon_command(&mut command).map_err(|err: io::Error| {
            io::Error::new(err.kind(), format!("failed to spawn herdr server: {err}"))
        })?;
    info!(pid, "server daemon spawned");

    Ok(pid)
}

fn build_server_daemon_command(exe: PathBuf) -> Command {
    let mut command = Command::new(&exe);
    command
        .arg("server")
        // Redirect stdio to /dev/null
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::platform::detach_server_daemon_command(&mut command);

    match std::env::current_dir() {
        Ok(cwd) => {
            command.env(STARTUP_CWD_ENV_VAR, cwd);
        }
        Err(_) => {
            command.env_remove(STARTUP_CWD_ENV_VAR);
        }
    }

    if crate::session::explicit_session_requested() {
        command
            .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
            .env_remove("HERDR_CLIENT_SOCKET_PATH");
    }

    command
}

fn build_session_server_daemon_command(exe: PathBuf, name: &str) -> Command {
    let mut command = build_server_daemon_command(exe);
    command.arg("--session").arg(name);
    for key in [
        crate::api::SOCKET_PATH_ENV_VAR,
        super::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
        crate::session::SESSION_ENV_VAR,
        crate::integration::HERDR_PANE_ID_ENV_VAR,
        crate::integration::HERDR_TAB_ID_ENV_VAR,
        crate::integration::HERDR_WORKSPACE_ID_ENV_VAR,
        "HERDR_PANE_RUNTIME_ID",
        "HERDR_ACTIVE_PANE_ID",
        "HERDR_ACTIVE_TAB_ID",
        "HERDR_ACTIVE_WORKSPACE_ID",
        crate::remote::REMOTE_PREFERENCES_ENV_VAR,
        crate::remote::REMOTE_KEYBINDINGS_ENV_VAR,
        crate::remote::REATTACH_COMMAND_ENV_VAR,
    ] {
        command.env_remove(key);
    }
    command
}

pub fn ensure_session_server(name: &str, cancelled: &AtomicBool) -> io::Result<PathBuf> {
    let session = crate::session::parse_target_name(name)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    check_startup_cancelled(cancelled)?;
    let socket_path = crate::session::client_socket_path_for(session.as_deref());
    if is_session_server_listening_at(&socket_path, cancelled)? {
        let restart_guidance = crate::session::restart_after_update_guidance(
            &crate::session::stop_command_for(Some(name)),
            Some(&format!("herdr session attach {name}")),
        );
        validate_running_server_compatibility_at(
            &crate::session::api_socket_path_for(session.as_deref()),
            false,
            &restart_guidance,
        )?;
    } else {
        check_startup_cancelled(cancelled)?;
        spawn_server_daemon_for_session(Some(name))?;
        wait_for_server_socket_cancellable(
            &socket_path,
            SERVER_READY_TIMEOUT,
            cancelled,
            &crate::session::data_dir_for(session.as_deref()).join("herdr-server.log"),
        )?;
    }
    check_startup_cancelled(cancelled)?;
    Ok(socket_path)
}

fn check_startup_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "session startup cancelled",
        ))
    } else {
        Ok(())
    }
}

fn is_session_server_listening_at(socket_path: &Path, cancelled: &AtomicBool) -> io::Result<bool> {
    match crate::ipc::connect_local_stream_cancellable(
        socket_path,
        cancelled,
        Instant::now() + STATUS_REQUEST_TIMEOUT,
    ) {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// Socket readiness
// ---------------------------------------------------------------------------

/// Waits for the server's client socket to become ready for connections.
///
/// Polls the socket path at regular intervals until a connection succeeds
/// or the timeout elapses. Returns an error if the server doesn't become
/// ready within the timeout.
pub fn wait_for_server_socket(socket_path: &Path, timeout: Duration) -> io::Result<()> {
    wait_for_server_socket_cancellable(
        socket_path,
        timeout,
        &AtomicBool::new(false),
        &crate::session::data_dir().join("herdr-server.log"),
    )
}

fn wait_for_server_socket_cancellable(
    socket_path: &Path,
    timeout: Duration,
    cancelled: &AtomicBool,
    log_path: &Path,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        check_startup_cancelled(cancelled)?;
        #[cfg(windows)]
        if client_protocol_accepts_hello(socket_path, cancelled)? {
            check_startup_cancelled(cancelled)?;
            info!(path = %socket_path.display(), "server client protocol ready");
            return Ok(());
        }

        #[cfg(not(windows))]
        if is_session_server_listening_at(socket_path, cancelled)? {
            check_startup_cancelled(cancelled)?;
            info!(path = %socket_path.display(), "server socket ready");
            return Ok(());
        }
        std::thread::sleep(SOCKET_POLL_INTERVAL);
    }

    check_startup_cancelled(cancelled)?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "server did not become ready within {}s (socket: {}). The background server may still be starting; try `herdr` again, or check {}",
            timeout.as_secs(),
            socket_path.display(),
            log_path.display()
        ),
    ))
}

// ---------------------------------------------------------------------------
// Auto-detect launch
// ---------------------------------------------------------------------------

/// Performs auto-detect launch: check for server, spawn if needed, then
/// attach as a thin client.
///
/// This is the entry point called from `main.rs` when the user runs `herdr`
/// without a subcommand.
///
/// Flow:
/// 1. Check if a server is listening on the client socket
/// 2. If no server → spawn server daemon → wait for socket readiness
/// 3. Run the thin client (which connects to the server)
pub fn auto_detect_launch(saved_federation: bool) -> io::Result<()> {
    // The client requires terminal geometry before it can attach. Reject an
    // unusable terminal before socket lookup creates directories or starts a daemon.
    crate::platform::terminal_grid_size().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("cannot attach without a usable terminal: {err}; run inside a terminal"),
        )
    })?;
    let socket_path = client_socket_path();
    info!(path = %socket_path.display(), "auto-detect launch starting");

    let startup = is_server_listening().and_then(|listening| {
        if listening {
            info!("server already running, attaching as client");
            if saved_federation {
                Ok(())
            } else {
                validate_running_server_compatibility(false)
            }
        } else {
            info!("no server running, spawning server daemon");
            spawn_server_daemon()
                .and_then(|_| wait_for_server_socket(&socket_path, SERVER_READY_TIMEOUT))
        }
    });
    if let Err(error) = startup {
        if !saved_federation {
            return Err(error);
        }
        tracing::warn!(%error, "Local startup failed; keeping saved machines available");
    }

    // Now attach as a thin client.
    crate::client::run_client()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod session_tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn explicit_session_startup_validates_names_before_io() {
        for name in [
            "",
            ".",
            "..",
            "../other",
            "work session",
            "a/b",
            "a\\b",
            "é",
        ] {
            let error = ensure_session_server(name, &AtomicBool::new(false)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name}");
        }
        assert_eq!(
            ensure_session_server(&"a".repeat(65), &AtomicBool::new(false))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn explicit_session_startup_honors_cancellation_before_io() {
        for name in ["default", "work", "--work", "a.b_c-1"] {
            let error = ensure_session_server(name, &AtomicBool::new(true)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted, "{name}");
        }
    }

    #[test]
    fn explicit_session_daemon_command_clears_inherited_identity_without_mutating_env() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        for name in ["default", "work", "--work"] {
            let keys = [
                crate::api::SOCKET_PATH_ENV_VAR,
                super::super::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
                crate::session::SESSION_ENV_VAR,
                crate::integration::HERDR_PANE_ID_ENV_VAR,
                crate::integration::HERDR_TAB_ID_ENV_VAR,
                crate::integration::HERDR_WORKSPACE_ID_ENV_VAR,
                "HERDR_PANE_RUNTIME_ID",
                "HERDR_ACTIVE_PANE_ID",
                "HERDR_ACTIVE_TAB_ID",
                "HERDR_ACTIVE_WORKSPACE_ID",
                crate::remote::REMOTE_PREFERENCES_ENV_VAR,
                crate::remote::REMOTE_KEYBINDINGS_ENV_VAR,
                crate::remote::REATTACH_COMMAND_ENV_VAR,
            ];
            let inherited: Vec<_> = keys.iter().map(std::env::var_os).collect();
            let command = build_session_server_daemon_command(PathBuf::from("herdr"), name);
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                ["server", "--session", name]
            );
            let envs: Vec<_> = command.get_envs().collect();
            for (key, before) in keys.iter().zip(inherited) {
                assert!(envs
                    .iter()
                    .any(|(entry, value)| *entry == OsStr::new(key) && value.is_none()));
                assert_eq!(std::env::var_os(key), before);
            }
        }
    }

    #[test]
    fn readiness_wait_honors_cancellation_without_starting_or_stopping_a_daemon() {
        let error = wait_for_server_socket_cancellable(
            Path::new("unused-session-client.sock"),
            Duration::from_secs(15),
            &AtomicBool::new(true),
            Path::new("unused-session-server.log"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }

    #[test]
    fn readiness_polling_observes_later_cancellation() {
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let stop = std::sync::Arc::clone(&cancelled);
        let cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            stop.store(true, Ordering::Release);
        });
        let socket = std::env::temp_dir().join(format!(
            "herdr-cancel-readiness-{}-absent.sock",
            std::process::id()
        ));
        let error = wait_for_server_socket_cancellable(
            &socket,
            Duration::from_secs(5),
            &cancelled,
            Path::new("unused-session-server.log"),
        )
        .unwrap_err();
        cancel.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions};
    use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;

    #[test]
    fn reachability_distinguishes_denied_silent_busy_and_absent_pipes() {
        let path =
            std::env::temp_dir().join(format!("herdr-startup-probe-{}.sock", std::process::id()));
        let name = path.to_string_lossy();
        let denied = ListenerOptions::new()
            .name(name.to_ns_name::<GenericNamespaced>().unwrap())
            .security_descriptor(
                SecurityDescriptor::deserialize(
                    &widestring::U16CString::from_str("D:P(A;;GA;;;SY)").unwrap(),
                )
                .unwrap(),
            )
            .create_sync()
            .unwrap();
        assert_eq!(
            is_server_listening_at(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        drop(denied);
        // A real listener that never answers the status API is still present:
        // automatic startup must not replace it just because it is unresponsive.
        let silent = crate::ipc::bind_local_listener(&path).unwrap();
        assert!(is_server_listening_at(&path).unwrap());
        drop(silent);
        let busy = crate::ipc::bind_local_listener(&path).unwrap();
        let occupied = crate::ipc::connect_local_stream(&path).unwrap();
        let start_probe = || {
            let probe_path = path.clone();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = result_tx.send(is_server_listening_at(&probe_path));
            });
            result_rx
        };
        assert_eq!(
            start_probe()
                .recv_timeout(Duration::from_secs(5))
                .expect("an occupied pipe must not block startup")
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let retry = start_probe();
        assert!(matches!(
            retry.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        let accepted = busy.accept().unwrap();
        assert!(retry.recv_timeout(Duration::from_secs(5)).unwrap().unwrap());
        drop(accepted);
        drop(occupied);
        drop(busy);
        assert!(!is_server_listening_at(&path).unwrap());
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::Mutex;

    fn env_lock() -> &'static Mutex<()> {
        crate::config::test_config_env_lock()
    }

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::path::PathBuf::from(format!("/tmp/ha-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn is_server_listening_returns_false_for_nonexistent_path() {
        let dir = unique_test_dir("nonexistent");
        let path = dir.join("s.sock");
        assert!(!is_server_listening_at(&path).unwrap());
    }

    #[test]
    fn server_daemon_command_clears_socket_overrides_for_explicit_session() {
        let _guard = env_lock().lock().unwrap();
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, "/tmp/inherited.sock");
        std::env::set_var("HERDR_CLIENT_SOCKET_PATH", "/tmp/inherited-client.sock");
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        let args = vec![
            "herdr".to_string(),
            "--session".to_string(),
            "work".to_string(),
        ];
        crate::session::configure_from_args(&args).unwrap();

        let command = build_server_daemon_command(PathBuf::from("/tmp/herdr-test"));
        let envs: Vec<_> = command.get_envs().collect();

        assert!(envs.iter().any(|(key, value)| {
            *key == OsStr::new(crate::api::SOCKET_PATH_ENV_VAR) && value.is_none()
        }));
        assert!(envs.iter().any(|(key, value)| {
            *key == OsStr::new("HERDR_CLIENT_SOCKET_PATH") && value.is_none()
        }));
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        std::env::remove_var("HERDR_CLIENT_SOCKET_PATH");
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
    }

    #[test]
    fn server_daemon_command_passes_current_dir_as_startup_cwd() {
        let expected = std::env::current_dir().unwrap();
        let command = build_server_daemon_command(PathBuf::from("/tmp/herdr-test"));
        let envs: Vec<_> = command.get_envs().collect();

        assert!(envs.iter().any(|(key, value)| {
            *key == OsStr::new(STARTUP_CWD_ENV_VAR) && value == &Some(expected.as_os_str())
        }));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn server_daemon_detach_creates_new_session() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            r#"sid=$(ps -o sid= -p $$ | tr -d ' ')
test "$sid" = "$$"
"#,
        );
        crate::platform::detach_server_daemon_command(&mut command);

        let status = command.status().unwrap();
        assert!(
            status.success(),
            "detached server child should be its own session leader"
        );
    }

    #[test]
    fn is_server_listening_returns_true_for_live_socket() {
        let dir = unique_test_dir("live");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        let _listener = UnixListener::bind(&path).unwrap();
        assert!(is_server_listening_at(&path).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn is_server_listening_returns_false_for_stale_socket() {
        let dir = unique_test_dir("stale");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        // Create a socket and immediately drop the listener.
        // This leaves a stale socket file with nobody listening.
        {
            let _listener = UnixListener::bind(&path).unwrap();
        }

        // The socket file exists but nobody is listening.
        assert!(!is_server_listening_at(&path).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn is_server_listening_returns_false_when_listener_dropped() {
        let dir = unique_test_dir("dropped");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        // Bind and immediately drop the listener.
        drop(UnixListener::bind(&path).unwrap());

        // Socket is stale — should return false.
        assert!(!is_server_listening_at(&path).unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wait_for_server_socket_succeeds_immediately() {
        let dir = unique_test_dir("wait-ok");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        let _listener = UnixListener::bind(&path).unwrap();

        // Should succeed immediately (socket is already ready).
        let result = wait_for_server_socket(&path, Duration::from_millis(100));
        assert!(result.is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wait_for_server_socket_times_out() {
        let dir = unique_test_dir("wait-timeout");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        // No listener — should time out.
        let result = wait_for_server_socket(&path, Duration::from_millis(50));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wait_for_server_socket_succeeds_after_delay() {
        let dir = unique_test_dir("wait-delay");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        // Spawn a thread that will create the listener after a short delay.
        let path_clone = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let _listener = UnixListener::bind(&path_clone).unwrap();
            // Keep the listener alive for a bit.
            std::thread::sleep(Duration::from_secs(1));
        });

        // Wait with a generous timeout — should succeed.
        let result = wait_for_server_socket(&path, Duration::from_secs(2));
        assert!(result.is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn read_server_status_at_reads_ping_response() {
        let dir = unique_test_dir("status");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("api.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert!(request.contains("ping"));
            stream
                .write_all(
                    b"{\"id\":\"autodetect:server:status\",\"result\":{\"type\":\"pong\",\"version\":\"0.5.5\",\"protocol\":2}}\n",
                )
                .unwrap();
            stream.flush().unwrap();
        });

        let status = crate::api::read_runtime_status_at(&path, Duration::from_millis(200))
            .unwrap()
            .unwrap();
        let _ = handle.join();
        assert_eq!(status.version.as_deref(), Some("0.5.5"));
        assert_eq!(status.protocol, Some(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_running_server_compatibility_fails_when_status_api_missing() {
        let _guard = env_lock().lock().unwrap();
        let dir = unique_test_dir("missing-api");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("api.sock");
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, &path);

        let err = validate_running_server_compatibility(false).unwrap_err();

        assert!(
            err.to_string().contains("status API is unavailable"),
            "unexpected error: {err}"
        );
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_running_server_compatibility_names_session_commands_for_protocol_mismatch() {
        let _guard = env_lock().lock().unwrap();
        let dir = unique_test_dir("named-protocol");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var(crate::session::SESSION_ENV_VAR, "work");
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        let path = crate::session::api_socket_path_for(Some("work"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert!(request.contains("ping"));
            let body = format!(
                "{{\"id\":\"autodetect:server:status\",\"result\":{{\"type\":\"pong\",\"version\":\"0.5.5\",\"protocol\":{}}}}}\n",
                crate::protocol::PROTOCOL_VERSION + 1
            );
            stream.write_all(body.as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        let err = validate_running_server_compatibility(false).unwrap_err();
        let message = err.to_string();

        let _ = handle.join();
        assert!(
            message.contains("Stop the old server to use the new version"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("Run `herdr session stop work`"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("then run `herdr session attach work` again"),
            "unexpected error: {message}"
        );
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        let _ = std::fs::remove_dir_all(dir);
    }
}
