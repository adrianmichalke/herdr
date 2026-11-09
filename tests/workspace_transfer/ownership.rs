use super::*;
use std::io::Read;
use std::os::unix::net::UnixListener;

fn journal(path: &Path) -> Value {
    serde_json::from_slice(
        &fs::read(path.parent().unwrap().join("workspace-transfers.json")).unwrap(),
    )
    .unwrap()
}

fn wait_until(description: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(Instant::now() < deadline, "{description}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn workspace_transfer_settled_receiver_survives_deleted_source_session_and_restart() {
    let mut sessions = Sessions::without_graphics();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let created = sessions.create("source", "settled destination");
    let workspace = created["workspace"]["workspace_id"].as_str().unwrap();
    let id = "25000000-0000-4000-8000-000000000001";
    let status = transfer(&source, workspace, id);
    assert_eq!(status["state"], "committed", "{status}");
    assert_eq!(journal(&target)["receivers"][id]["settled"], true);
    assert_eq!(journal(&source)["sources"][id]["runtime_released"], true);
    let destination = status["workspace_id"].as_str().unwrap();
    let pane = status["focused_pane_id"].as_str().unwrap();
    sessions.stop_first();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_herdr"));
    command.args(["session", "delete", "source", "--json"]);
    command.env("XDG_CONFIG_HOME", sessions.base.join("config"));
    command.env("XDG_RUNTIME_DIR", sessions.base.join("runtime"));
    command.env("XDG_STATE_HOME", sessions.base.join("state"));
    command.env("HOME", sessions.base.join("home"));
    for key in [
        "HERDR_SESSION",
        "HERDR_SOCKET_PATH",
        "HERDR_CLIENT_SOCKET_PATH",
        "HERDR_CONFIG_PATH",
        "HERDR_WORKSPACE_ID",
        "HERDR_TAB_ID",
        "HERDR_PANE_ID",
        "HERDR_STARTUP_CWD",
    ] {
        command.env_remove(key);
    }
    let deleted = command.output().unwrap();
    assert!(
        deleted.status.success(),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    assert!(!source.parent().unwrap().exists());
    success(&target, "server.stop", json!({}));
    let mut server = sessions.servers.remove(0);
    server.child.wait().unwrap();
    drop(server);
    sessions.start("target");
    let restored = success(
        &target,
        "workspace.get",
        json!({"workspace_id":destination}),
    );
    assert_eq!(restored["workspace"]["label"], "settled destination");
    send(&target, pane, "printf 'SETTLED-AFTER-SOURCE-DELETION\\n'");
    wait_output(&target, pane, "SETTLED-AFTER-SOURCE-DELETION");
    assert_eq!(journal(&target)["receivers"][id]["settled"], true);
}

#[test]
fn workspace_transfer_released_source_recovers_settlement_after_receiver_restart() {
    let mut sessions = Sessions::without_graphics();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let created = sessions.create("source", "release before settlement");
    let workspace = created["workspace"]["workspace_id"].as_str().unwrap();
    let id = "25000000-0000-4000-8000-000000000003";
    let status = transfer(&source, workspace, id);
    assert_eq!(status["state"], "committed", "{status}");
    let destination = status["workspace_id"].as_str().unwrap();
    let pane = status["focused_pane_id"].as_str().unwrap();
    sessions.stop_first();
    success(&target, "server.stop", json!({}));
    let mut server = sessions.servers.remove(0);
    server.child.wait().unwrap();
    drop(server);
    let mut source_journal = journal(&source);
    source_journal["sources"][id]["phase"] = json!("CommitDecided");
    source_journal["sources"][id]["info"]["state"] = json!("pending");
    source_journal["sources"][id]["route"]["ready"] = json!(false);
    source_journal["routes"][id]["ready"] = json!(false);
    fs::write(
        source.parent().unwrap().join("workspace-transfers.json"),
        serde_json::to_vec(&source_journal).unwrap(),
    )
    .unwrap();
    let mut receiver_journal = journal(&target);
    receiver_journal["receivers"][id]["settled"] = json!(false);
    fs::write(
        target.parent().unwrap().join("workspace-transfers.json"),
        serde_json::to_vec(&receiver_journal).unwrap(),
    )
    .unwrap();
    sessions.start("target");
    let restored = success(&target, "workspace.list", json!({}));
    assert!(restored["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .any(|workspace| { workspace["workspace_id"] == destination }));
    let fenced = raw(
        &target,
        "pane.rename",
        json!({"pane_id":pane,"label":"still settling"}),
    );
    assert_eq!(fenced["error"]["code"], "workspace_busy");
    sessions.start("source");
    wait_until(
        "released source did not recover durable receiver settlement",
        || {
            success(
                &source,
                "workspace.transfer.status",
                json!({"transfer_id":id}),
            )["transfer"]["state"]
                == "committed"
        },
    );
    assert_eq!(journal(&target)["receivers"][id]["settled"], true);
    success(
        &target,
        "pane.rename",
        json!({"pane_id":pane,"label":"settlement recovered"}),
    );
    send(&target, pane, "printf 'RECOVERED-RECEIVER-SETTLEMENT\\n'");
    wait_output(&target, pane, "RECOVERED-RECEIVER-SETTLEMENT");
}

struct HelloGate {
    path: PathBuf,
    original: PathBuf,
    listener: UnixListener,
    restored: bool,
}

impl HelloGate {
    fn new(api_path: &Path) -> Self {
        let directory = api_path.parent().unwrap();
        let path = directory.join("herdr-transfer.sock");
        let original = directory.join("transfer-gated.sock");
        fs::rename(&path, &original).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self {
            path,
            original,
            listener,
            restored: false,
        }
    }

    fn receive(&self) -> (UnixStream, Vec<u8>) {
        let mut incoming = None;
        wait_until(
            "source never connected to transfer handshake gate",
            || match self.listener.accept() {
                Ok((stream, _)) => {
                    incoming = Some(stream);
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(error) => panic!("transfer handshake failed: {error}"),
            },
        );
        let mut incoming = incoming.unwrap();
        incoming
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let request = read_frame(&mut incoming);
        assert_eq!(
            serde_json::from_slice::<Value>(&request).unwrap(),
            json!("hello")
        );
        let mut original = UnixStream::connect(&self.original).unwrap();
        original
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write_frame(&mut original, &request);
        let reply = read_frame(&mut original);
        (incoming, reply)
    }

    fn restore(&mut self) {
        fs::remove_file(&self.path).unwrap();
        fs::rename(&self.original, &self.path).unwrap();
        self.restored = true;
    }
}

impl Drop for HelloGate {
    fn drop(&mut self) {
        if !self.restored {
            let _ = fs::remove_file(&self.path);
            let _ = fs::rename(&self.original, &self.path);
        }
    }
}

fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut length = [0; 8];
    stream.read_exact(&mut length).unwrap();
    let length = usize::try_from(u64::from_be_bytes(length)).unwrap();
    assert!(length < 1024 * 1024);
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}

fn write_frame(stream: &mut UnixStream, bytes: &[u8]) {
    stream
        .write_all(&(bytes.len() as u64).to_be_bytes())
        .unwrap();
    stream.write_all(bytes).unwrap();
    stream.flush().unwrap();
}

#[test]
fn workspace_transfer_known_abort_retries_blocked_journal_and_unfences_same_live_source() {
    let mut sessions = Sessions::without_graphics();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let created = sessions.create("source", "rollback source");
    let existing = sessions.create("target", "collision owner");
    let workspace = created["workspace"]["workspace_id"].as_str().unwrap();
    let pane = created["root_pane"]["pane_id"].as_str().unwrap();
    let other_pane = existing["root_pane"]["pane_id"].as_str().unwrap();
    let shell_pid = pid(&source, pane);
    let other_pid = pid(&target, other_pane);
    for (path, pane) in [(&source, pane), (&target, other_pane)] {
        success(
            path,
            "pane.report_agent",
            json!({"pane_id":pane,"source":"transfer-test","agent":"codex","state":"idle","seq":1}),
        );
        success(
            path,
            "agent.rename",
            json!({"target":pane,"name":"blocked-journal-collision"}),
        );
    }
    let mut gate = HelloGate::new(&target);
    let id = "25000000-0000-4000-8000-000000000002";
    let params = json!({"workspace_id":workspace,"destination_session":"target","transfer_id":id});
    let submitted = success(&source, "workspace.transfer", params.clone());
    assert_eq!(submitted["transfer"]["state"], "pending");
    let (mut incoming, reply) = gate.receive();
    let blocked = source
        .parent()
        .unwrap()
        .join("workspace-transfers.json.tmp");
    fs::create_dir(&blocked).unwrap();
    gate.restore();
    write_frame(&mut incoming, &reply);
    drop(incoming);
    let log_path = source.parent().unwrap().join("herdr-server.log");
    wait_until(
        "known abort was not retried while journal was blocked",
        || {
            fs::read_to_string(&log_path)
                .unwrap_or_default()
                .matches("workspace transfer outcome remains pending")
                .count()
                >= 2
        },
    );
    let pending = success(
        &source,
        "workspace.transfer.status",
        json!({"transfer_id":id}),
    );
    assert_eq!(pending["transfer"]["state"], "pending");
    let fenced = raw(
        &source,
        "pane.rename",
        json!({"pane_id":pane,"label":"not yet"}),
    );
    assert_eq!(fenced["error"]["code"], "workspace_busy");
    assert_eq!(
        success(&source, "workspace.transfer", params)["transfer"]["state"],
        "pending"
    );
    fs::remove_dir(&blocked).unwrap();
    wait_until(
        "known abort never finalized after journal became writable",
        || {
            success(
                &source,
                "workspace.transfer.status",
                json!({"transfer_id":id}),
            )["transfer"]["state"]
                == "failed"
        },
    );
    let failed = success(
        &source,
        "workspace.transfer.status",
        json!({"transfer_id":id}),
    );
    assert!(failed["transfer"]["error"]
        .as_str()
        .unwrap()
        .contains("collision"));
    assert_eq!(pid(&source, pane), shell_pid);
    assert_eq!(pid(&target, other_pane), other_pid);
    assert_eq!(journal(&source)["sources"][id]["phase"], "Failed");
    success(
        &source,
        "pane.rename",
        json!({"pane_id":pane,"label":"rollback complete"}),
    );
    send(&source, pane, "printf 'ABORT-RETRY-READER-RESUMED\\n'");
    wait_output(&source, pane, "ABORT-RETRY-READER-RESUMED");
}
