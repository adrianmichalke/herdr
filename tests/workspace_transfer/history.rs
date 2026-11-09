use super::*;

fn history_file(sessions: &Sessions, name: &str, file: &str) -> PathBuf {
    sessions.path(name).with_file_name(file)
}

fn wait_for_saved_history(sessions: &Sessions, marker: &str) {
    let path = history_file(sessions, "target", "session-history.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if fs::read_to_string(&path).is_ok_and(|history| history.contains(marker)) {
            return;
        }
        assert!(Instant::now() < deadline, "history was not saved: {path:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn crash_target(sessions: &mut Sessions) -> Instant {
    let server = sessions.servers.remove(1);
    let pid = server.child.process_id().unwrap();
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
    Instant::now()
}

fn assert_history(path: &Path, workspace: &str, marker: &str) {
    let panes = success(path, "pane.list", json!({"workspace_id":workspace}));
    let pane = panes["panes"][0]["pane_id"].as_str().unwrap();
    let read = success(
        path,
        "pane.read",
        json!({"pane_id":pane,"source":"recent","lines":2000,"format":"text"}),
    );
    assert!(
        read["read"]["text"].as_str().unwrap().contains(marker),
        "missing restored history {marker}: {read}"
    );
}

#[test]
fn committed_receiver_checkpoint_restores_resident_and_incoming_history_before_autosave() {
    let mut sessions = Sessions::without_graphics();
    fs::write(
        sessions.base.join("config/herdr-dev/config.toml"),
        "onboarding = false\n[terminal]\nkitty_graphics = false\n[experimental]\nallow_nested = true\npane_history = true\n",
    )
    .unwrap();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let resident = sessions.create("target", "resident");
    let resident_workspace = resident["workspace"]["workspace_id"].as_str().unwrap();
    let resident_pane = resident["root_pane"]["pane_id"].as_str().unwrap();
    let resident_marker = "RESIDENT-CHECKPOINT-HISTORY";
    send(
        &target,
        resident_pane,
        "printf 'RESIDENT-%s\\n' 'CHECKPOINT-HISTORY'; i=0; while [ \"$i\" -lt 80 ]; do printf 'resident-line-%s\\n' \"$i\"; i=$((i+1)); done; printf 'RESIDENT-%s\\n' 'OUTPUT-READY'",
    );
    wait_output(&target, resident_pane, "RESIDENT-OUTPUT-READY");
    wait_for_saved_history(&sessions, resident_marker);

    let incoming = sessions.create("source", "incoming");
    let workspace = incoming["workspace"]["workspace_id"].as_str().unwrap();
    let pane = incoming["root_pane"]["pane_id"].as_str().unwrap();
    let incoming_marker = "INCOMING-CHECKPOINT-HISTORY";
    send(
        &source,
        pane,
        "printf 'INCOMING-%s\\n' 'CHECKPOINT-HISTORY'; i=0; while [ \"$i\" -lt 80 ]; do printf 'incoming-line-%s\\n' \"$i\"; i=$((i+1)); done; printf 'INCOMING-%s\\n' 'OUTPUT-READY'",
    );
    wait_output(&source, pane, "INCOMING-OUTPUT-READY");
    let started = Instant::now();
    let status = transfer(&source, workspace, "23000000-0000-4000-8000-000000000001");
    assert_eq!(status["state"], "committed", "{status}");
    let crashed = crash_target(&mut sessions);
    assert!(
        crashed.duration_since(started) < Duration::from_secs(5),
        "target must crash before the five-second post-transfer autosave"
    );
    let checkpoint = fs::read_to_string(history_file(&sessions, "target", "session.json")).unwrap();
    let checkpoint: Value = serde_json::from_str(&checkpoint).unwrap();
    assert!(checkpoint["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .any(|workspace| workspace["id"] == status["workspace_id"]));
    let history =
        fs::read_to_string(history_file(&sessions, "target", "session-history.json")).unwrap();
    assert!(history.contains(resident_marker));
    assert!(history.contains(incoming_marker));

    sessions.start("target");
    assert_history(&target, resident_workspace, resident_marker);
    assert_history(
        &target,
        status["workspace_id"].as_str().unwrap(),
        incoming_marker,
    );
}
