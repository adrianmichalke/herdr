mod args;
mod attach;
mod host;
mod process;
mod restart_policy;
mod saved;
#[cfg(unix)]
mod ssh_agent;

pub(crate) use args::*;
pub(crate) use attach::*;
pub(crate) use saved::*;

const EXISTING_ONLY_CLIENT_BRIDGE_FLAG: &str = "--existing-only-v1";

pub(crate) fn run_remote_client_bridge(args: &[String]) -> std::io::Result<()> {
    if !existing_only_client_bridge_requested(args) {
        return host::run_remote_client_bridge(args);
    }
    let socket_path = existing_client_bridge_socket_path(
        crate::session::explicit_session_requested(),
        crate::session::active_name().as_deref(),
    )?;
    let stream = crate::ipc::connect_local_stream_cancellable(
        &socket_path,
        &std::sync::atomic::AtomicBool::new(false),
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
    .map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "failed to connect to existing remote Herdr client socket {}: {error}",
                socket_path.display()
            ),
        )
    })?;
    crate::platform::forward_remote_bridge_stdio(stream, false)
}

fn existing_only_client_bridge_requested(args: &[String]) -> bool {
    matches!(args, [flag] if flag == EXISTING_ONLY_CLIENT_BRIDGE_FLAG)
}

fn existing_client_bridge_socket_path(
    explicit_session: bool,
    session_name: Option<&str>,
) -> std::io::Result<std::path::PathBuf> {
    if !explicit_session {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "existing-only remote client bridge requires --session <name>",
        ));
    }
    if let Some(name) = session_name {
        crate::session::validate_name(name)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    }
    let session_name = session_name.filter(|name| *name != crate::session::DEFAULT_SESSION_NAME);
    Ok(crate::session::client_socket_path_for(session_name))
}

pub(crate) fn run_remote_api_bridge(args: &[String]) -> std::io::Result<()> {
    match args {
        [] => {
            let path = crate::api::socket_path();
            let stream = crate::ipc::connect_local_stream(&path).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "failed to connect to remote Herdr API socket {}: {error}",
                        path.display()
                    ),
                )
            })?;
            crate::platform::forward_remote_bridge_stdio(stream, false)
        }
        [flag] if flag == "--check" => {
            println!("herdr-api-bridge-v1");
            Ok(())
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage: herdr remote-api-bridge [--check]",
        )),
    }
}

pub(crate) fn print_saved_ssh_error_hint(err: &std::io::Error, target: &str) {
    if is_remote_host_key_error(err) {
        eprintln!(
            "hint: saved machines use strict host-key checking; add the host key to the configured known_hosts file, then retry."
        );
    } else {
        print_remote_error_hint(err, target);
    }
}

pub(crate) fn print_remote_error_hint(err: &std::io::Error, target: &str) {
    if is_remote_auth_error(err) {
        eprintln!(
            "hint: verify SSH access first with `{}`.",
            ssh_check_command(target)
        );
        eprintln!(
            "hint: if your SSH key has a passphrase, load it into ssh-agent with `ssh-add` before running `herdr --remote`."
        );
    }
}

fn is_remote_host_key_error(err: &std::io::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    message.contains("host key verification failed")
        || message.contains("remote host identification has changed")
}

fn is_remote_auth_error(err: &std::io::Error) -> bool {
    let message = err.to_string();
    message.contains("Permission denied")
        && (message.contains("(publickey")
            || message.contains("(keyboard-interactive")
            || message.contains("(password"))
}

fn ssh_check_command(target: &str) -> String {
    format!("ssh {}", shell_quote(target))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_only_client_bridge_requires_the_exact_single_flag() {
        for (args, expected) in [
            (vec!["--existing-only-v1"], true),
            (vec![], false),
            (vec!["--idle-timeout-v1"], false),
            (vec!["--existing-only"], false),
            (vec!["--existing-only-v1=true"], false),
            (vec!["--existing-only-v1", "--idle-timeout-v1"], false),
            (vec!["--idle-timeout-v1", "--existing-only-v1"], false),
            (vec!["--existing-only-v1", "--existing-only-v1"], false),
            (vec!["--existing-only-v1", "extra"], false),
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(
                existing_only_client_bridge_requested(&args),
                expected,
                "{args:?}"
            );
        }
    }

    #[test]
    fn existing_client_bridge_requires_an_explicit_session() {
        for session in [None, Some("default"), Some("work")] {
            let error = existing_client_bridge_socket_path(false, session).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn existing_client_bridge_rejects_invalid_session_names() {
        for session in [
            "",
            ".",
            "..",
            "../other",
            "work/other",
            "work\\other",
            "two words",
        ] {
            let error = existing_client_bridge_socket_path(true, Some(session)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
        let long_name = "s".repeat(65);
        let error = existing_client_bridge_socket_path(true, Some(&long_name)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn existing_client_bridge_selects_only_the_requested_session_path() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        for session in [None, Some("default")] {
            assert_eq!(
                existing_client_bridge_socket_path(true, session).unwrap(),
                crate::session::client_socket_path_for(None)
            );
        }
        for session in ["focus", "--work", "a.b_c-1"] {
            assert_eq!(
                existing_client_bridge_socket_path(true, Some(session)).unwrap(),
                crate::session::client_socket_path_for(Some(session))
            );
        }
    }

    #[test]
    fn remote_host_key_error_matches_ssh_diagnostics() {
        for message in [
            "Host key verification failed.",
            "REMOTE HOST IDENTIFICATION HAS CHANGED!",
        ] {
            assert!(is_remote_host_key_error(&std::io::Error::other(message)));
        }
        assert!(!is_remote_host_key_error(&std::io::Error::other(
            "server closed connection"
        )));
    }

    #[test]
    fn remote_auth_error_matches_ssh_auth_denied() {
        let err = std::io::Error::other(
            "remote platform detection failed: user@host: Permission denied (publickey).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_matches_keyboard_interactive_denied() {
        let err = std::io::Error::other(
            "remote server status failed: user@host: Permission denied (keyboard-interactive).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_ignores_non_auth_errors() {
        let err = std::io::Error::other("remote platform detection failed: unsupported platform");

        assert!(!is_remote_auth_error(&err));
    }

    #[test]
    fn ssh_check_command_quotes_remote_target() {
        assert_eq!(ssh_check_command("host name"), "ssh 'host name'");
    }
}
