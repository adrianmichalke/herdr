use std::io;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use super::attach::{find_installed_remote_herdr, RemoteSsh, SshStdioBridge};

pub(crate) struct SavedSshBridge {
    _bridge: SshStdioBridge,
}

pub(crate) struct SavedSshStream {
    pub(crate) stream: crate::ipc::LocalStream,
    pub(crate) bridge: SavedSshBridge,
}

pub(crate) fn connect_saved_ssh(
    profile_id: &str,
    target: &str,
    session: &str,
    cancelled: Arc<AtomicBool>,
) -> io::Result<SavedSshStream> {
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    let mut ssh = validated_saved_ssh(profile_id, target, session)?;
    ssh.cancelled = Arc::clone(&cancelled);
    ssh.prepare_shared_transport()?;
    let remote_herdr = find_installed_remote_herdr(&ssh)?;
    let metadata = remote_herdr.machine_metadata();
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    let path = saved_bridge_path(profile_id, "client");
    let bridge = SshStdioBridge::start(
        target.to_owned(),
        remote_herdr,
        path.clone(),
        session.to_owned(),
        ssh.options(),
        true,
        Arc::clone(&cancelled),
    )?;
    let stream = crate::ipc::connect_local_stream_cancellable(
        &path,
        &cancelled,
        Instant::now() + Duration::from_secs(5),
    )?;
    if let Some(metadata) = metadata {
        crate::client::endpoint::SshMetadataCache::new(profile_id, target, session)?
            .store(&metadata);
    }
    Ok(SavedSshStream {
        stream,
        bridge: SavedSshBridge { _bridge: bridge },
    })
}

pub(crate) fn connect_saved_existing_ssh(
    profile_id: &str,
    target: &str,
    session: &str,
    cancelled: Arc<AtomicBool>,
) -> io::Result<SavedSshStream> {
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    let mut ssh = validated_saved_ssh(profile_id, target, session)?;
    ssh.cancelled = Arc::clone(&cancelled);
    let remote_herdr = find_installed_remote_herdr(&ssh)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    let command = super::attach::remote_existing_client_bridge_command(&remote_herdr, session);
    let path = saved_bridge_path(profile_id, "existing-client");
    let bridge = SshStdioBridge::start_command(
        target.to_owned(),
        command,
        path.clone(),
        ssh.options(),
        true,
        Arc::clone(&cancelled),
    )?;
    let stream = crate::ipc::connect_local_stream_cancellable(
        &path,
        &cancelled,
        Instant::now() + Duration::from_secs(5),
    )?;
    Ok(SavedSshStream {
        stream,
        bridge: SavedSshBridge { _bridge: bridge },
    })
}

pub(crate) fn connect_saved_history(
    profile_id: &str,
    target: &str,
    session: &str,
    cancelled: Arc<AtomicBool>,
) -> io::Result<SavedSshStream> {
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    let mut ssh = validated_saved_ssh(profile_id, target, session)?;
    ssh.cancelled = Arc::clone(&cancelled);
    let remote_herdr = find_installed_remote_herdr(&ssh)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    let command = super::attach::remote_history_command(&remote_herdr, session);
    let path = saved_bridge_path(profile_id, "history");
    let bridge = SshStdioBridge::start_command(
        target.to_owned(),
        command,
        path.clone(),
        ssh.options(),
        true,
        Arc::clone(&cancelled),
    )?;
    let stream = crate::ipc::connect_local_stream_cancellable(
        &path,
        &cancelled,
        Instant::now() + Duration::from_secs(5),
    )?;
    Ok(SavedSshStream {
        stream,
        bridge: SavedSshBridge { _bridge: bridge },
    })
}

pub(crate) fn list_saved_sessions(
    profile_id: &str,
    target: &str,
    session: &str,
    cancelled: Arc<AtomicBool>,
) -> io::Result<Vec<crate::session::SessionInfo>> {
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    let mut ssh = validated_saved_ssh(profile_id, target, session)?;
    ssh.cancelled = Arc::clone(&cancelled);
    ssh.prepare_shared_transport()?;
    let result = super::attach::list_remote_sessions(&ssh);
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    result
}

pub(crate) struct SavedSshApiBridge {
    path: PathBuf,
    bridge: SshStdioBridge,
    metadata_cache: crate::client::endpoint::SshMetadataCache,
    ssh: RemoteSsh,
    pub(crate) can_retry: bool,
    pub(crate) used_cached_metadata: bool,
}

impl SavedSshApiBridge {
    pub(crate) fn start(
        profile_id: &str,
        target: &str,
        session: &str,
        use_cached_metadata: bool,
        allow_retry: bool,
    ) -> io::Result<Self> {
        let ssh = validated_saved_ssh(profile_id, target, session)?;
        let mut can_retry = allow_retry;
        let metadata_cache =
            crate::client::endpoint::SshMetadataCache::new(profile_id, target, session)?;
        let cached = use_cached_metadata.then(|| metadata_cache.load()).flatten();
        let used_cached_metadata = cached.is_some();
        let metadata = match cached {
            Some(metadata) => metadata,
            None => {
                let discover = || super::attach::discover_remote_api_metadata(&ssh, session);
                let metadata = match discover() {
                    Err(_) if can_retry && ssh.prepare_shared_transport().unwrap_or(false) => {
                        can_retry = false;
                        discover()?
                    }
                    result => result?,
                };
                metadata_cache.store(&metadata);
                metadata
            }
        };
        let command = super::attach::cached_remote_api_command(&metadata, session);
        let path = saved_bridge_path(profile_id, "api");
        let bridge = SshStdioBridge::start_command(
            target.to_owned(),
            command,
            path.clone(),
            ssh.options(),
            true,
            Arc::new(AtomicBool::new(false)),
        )?;
        Ok(Self {
            path,
            bridge,
            metadata_cache,
            ssh,
            can_retry,
            used_cached_metadata,
        })
    }

    pub(crate) fn socket_path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) fn reported_failure(&self) -> Option<io::Error> {
        self.bridge.reported_failure()
    }

    pub(crate) fn invalidate_metadata(&self) {
        self.metadata_cache.invalidate();
    }

    pub(crate) fn recover_transport(&self) -> bool {
        self.can_retry && self.ssh.prepare_shared_transport().unwrap_or(false)
    }

    pub(crate) fn stale_metadata_failure(error: &io::Error) -> bool {
        error
            .to_string()
            .contains(super::attach::STALE_API_METADATA)
    }
}

pub(crate) fn saved_ssh_bootstrap_command(target: &str, session: &str) -> String {
    format!(
        "herdr --remote {} --session {}",
        super::shell_quote(target),
        super::shell_quote(session)
    )
}

pub(crate) fn saved_ssh_failure_needs_attention(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput
            | io::ErrorKind::InvalidData
            | io::ErrorKind::NotFound
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::Unsupported
    ) {
        return true;
    }
    let message = error.to_string().to_ascii_lowercase();
    [
        "permission denied",
        "host key verification failed",
        "remote host identification has changed",
        "no matching host key",
        "unsupported remote platform",
        "not ready",
        "install or update",
        "protocol",
        "handshake",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn saved_bridge_path(profile_id: &str, kind: &str) -> PathBuf {
    static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(0);
    let instance = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let readable = format!("herdr-{kind}-ssh-{pid}-{instance:x}-{profile_id}.sock");
    let short = format!("herdr-{kind}-{pid}-{instance:x}-{}.sock", &profile_id[..16]);
    crate::platform::remote_bridge_endpoint_path(&readable, &short)
}

fn validated_saved_ssh(profile_id: &str, target: &str, session: &str) -> io::Result<RemoteSsh> {
    validate_profile_path_id(profile_id)?;
    super::validate_remote_target(target)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    crate::session::validate_name(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(RemoteSsh::new_noninteractive(target.to_owned()))
}

fn validate_profile_path_id(profile_id: &str) -> io::Result<()> {
    if profile_id.len() == 32
        && profile_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSH endpoint profile id",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_paths_use_profile_identity_not_target_or_session() {
        let first = saved_bridge_path("0123456789abcdef0123456789abcdef", "client");
        let second = saved_bridge_path("fedcba9876543210fedcba9876543210", "client");
        assert_ne!(first, second);
        assert!(!first.to_string_lossy().contains("example.com"));
        assert!(!first.to_string_lossy().contains("default"));
    }

    #[test]
    fn bridge_paths_are_unique_for_concurrent_attachments_to_the_same_profile() {
        let profile = "0123456789abcdef0123456789abcdef";
        let paths = (0..32)
            .flat_map(|_| {
                [
                    saved_bridge_path(profile, "client"),
                    saved_bridge_path(profile, "api"),
                    saved_bridge_path(profile, "history"),
                    saved_bridge_path(profile, "existing-client"),
                ]
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(paths.len(), 128);
        assert!(paths.iter().all(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(&profile[..16])
        }));
    }

    #[test]
    fn saved_session_operations_reject_cancelled_requests_before_ssh_setup() {
        let cancelled = Arc::new(AtomicBool::new(true));
        let profile = "0123456789abcdef0123456789abcdef";
        let error =
            list_saved_sessions(profile, "unused.invalid", "default", Arc::clone(&cancelled))
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        let error = connect_saved_ssh(profile, "unused.invalid", "work", cancelled)
            .err()
            .expect("cancelled connection");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }

    #[test]
    fn saved_existing_ssh_rejects_cancellation_before_validation_or_ssh_setup() {
        for (profile, target, session) in [
            (
                "0123456789abcdef0123456789abcdef",
                "unused.invalid",
                "default",
            ),
            ("invalid", "-option", "../other"),
        ] {
            let error = connect_saved_existing_ssh(
                profile,
                target,
                session,
                Arc::new(AtomicBool::new(true)),
            )
            .err()
            .expect("cancelled existing SSH connection");
            assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        }
    }

    #[test]
    fn saved_history_rejects_cancellation_before_validation_or_ssh_setup() {
        for (profile, target, session) in [
            (
                "0123456789abcdef0123456789abcdef",
                "unused.invalid",
                "default",
            ),
            ("invalid", "-option", "../other"),
        ] {
            let error =
                connect_saved_history(profile, target, session, Arc::new(AtomicBool::new(true)))
                    .err()
                    .expect("cancelled history connection");
            assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        }
    }

    #[test]
    fn saved_session_operations_validate_names_targets_and_profile_ids() {
        for (profile, target, session) in [
            ("short", "unused.invalid", "default"),
            ("0123456789abcdef0123456789abcdef", "", "default"),
            ("0123456789abcdef0123456789abcdef", "-option", "work"),
            (
                "0123456789abcdef0123456789abcdef",
                "unused.invalid",
                "../other",
            ),
        ] {
            let error =
                list_saved_sessions(profile, target, session, Arc::new(AtomicBool::new(false)))
                    .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            let error =
                connect_saved_ssh(profile, target, session, Arc::new(AtomicBool::new(false)))
                    .err()
                    .expect("invalid saved connection target");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            let error = connect_saved_existing_ssh(
                profile,
                target,
                session,
                Arc::new(AtomicBool::new(false)),
            )
            .err()
            .expect("invalid existing SSH connection target");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            let error =
                connect_saved_history(profile, target, session, Arc::new(AtomicBool::new(false)))
                    .err()
                    .expect("invalid history connection target");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn bootstrap_command_explicitly_selects_default() {
        assert_eq!(
            saved_ssh_bootstrap_command("build-host", "default"),
            "herdr --remote build-host --session default"
        );
    }

    #[test]
    fn bootstrap_command_preserves_the_explicit_remote_session() {
        assert_eq!(
            saved_ssh_bootstrap_command("build host", "agent work"),
            "herdr --remote 'build host' --session 'agent work'"
        );
    }

    #[test]
    fn prompt_and_compatibility_failures_require_attention() {
        for message in [
            "Permission denied (publickey)",
            "Host key verification failed",
            "matching Herdr is not ready; install or update",
            "handshake rejected",
        ] {
            assert!(saved_ssh_failure_needs_attention(&io::Error::other(
                message
            )));
        }
        assert!(!saved_ssh_failure_needs_attention(&io::Error::new(
            io::ErrorKind::TimedOut,
            "network timed out"
        )));
    }
}
