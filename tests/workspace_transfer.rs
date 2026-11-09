#![cfg(unix)]

pub mod support;

#[path = "workspace_transfer/ownership.rs"]
mod ownership;

#[path = "workspace_transfer/history.rs"]
mod history;

#[path = "workspace_transfer/identity.rs"]
mod identity;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};

struct Server {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for Server {
    fn drop(&mut self) {
        support::stop_spawned_herdr(&mut *self.child);
        let _ = self.child.wait();
        support::unregister_spawned_herdr_pid(self.child.process_id());
    }
}

struct Sessions {
    base: PathBuf,
    servers: Vec<Server>,
}

impl Sessions {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = PathBuf::from(format!(
            "/var/tmp/hwt-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(base.join("config/herdr-dev")).unwrap();
        fs::create_dir_all(base.join("home")).unwrap();
        fs::write(
            base.join("config/herdr-dev/config.toml"),
            "onboarding = false\n[experimental]\nallow_nested = true\n",
        )
        .unwrap();
        support::register_runtime_dir(&base.join("runtime"));
        Self {
            base,
            servers: Vec::new(),
        }
    }

    fn without_graphics() -> Self {
        let sessions = Self::new();
        fs::write(sessions.base.join("config/herdr-dev/config.toml"), "onboarding = false\n[terminal]\nkitty_graphics = false\n[experimental]\nallow_nested = true\n").unwrap();
        sessions
    }

    fn path(&self, name: &str) -> PathBuf {
        self.base
            .join("config/herdr-dev/sessions")
            .join(name)
            .join("herdr.sock")
    }

    fn start(&mut self, name: &str) {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
        support::isolate_herdr_test_process(&mut command);
        command.args(["--session", name, "server"]);
        command.env("XDG_CONFIG_HOME", self.base.join("config"));
        command.env("XDG_RUNTIME_DIR", self.base.join("runtime"));
        command.env("XDG_STATE_HOME", self.base.join("state"));
        command.env("HOME", self.base.join("home"));
        command.env("SHELL", "/bin/sh");
        for key in [
            "HERDR_SOCKET_PATH",
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_CONFIG_PATH",
            "HERDR_WORKSPACE_ID",
            "HERDR_TAB_ID",
            "HERDR_PANE_ID",
        ] {
            command.env_remove(key);
        }
        let child = pair.slave.spawn_command(command).unwrap();
        support::register_spawned_herdr_pid(child.process_id());
        self.servers.push(Server {
            _master: pair.master,
            child,
        });
        support::wait_for_socket(&self.path(name), Duration::from_secs(15));
        success(&self.path(name), "ping", json!({}));
    }

    fn create(&self, name: &str, label: &str) -> Value {
        success(
            &self.path(name),
            "workspace.create",
            json!({"cwd": self.base, "label":label, "focus":true}),
        )
    }

    fn stop_first(&mut self) {
        success(&self.path("source"), "server.stop", json!({}));
        let mut server = self.servers.remove(0);
        let _ = server.child.wait();
        drop(server);
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        if std::thread::panicking() {
            for name in ["source", "target"] {
                let path = self
                    .base
                    .join("config/herdr-dev/sessions")
                    .join(name)
                    .join("herdr-server.log");
                if let Ok(log) = fs::read_to_string(&path) {
                    let lines: Vec<_> = log.lines().rev().take(18).collect();
                    eprintln!(
                        "{}:\n{}",
                        path.display(),
                        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
                    );
                }
            }
        }
        self.servers.clear();
        support::cleanup_test_base(&self.base);
    }
}

fn raw(path: &Path, method: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(40)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        json!({"id":"transfer-test", "method":method, "params":params})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or_else(|error| panic!("{method}: {error}: {line}"))
}

fn success(path: &Path, method: &str, params: Value) -> Value {
    let response = raw(path, method, params);
    assert!(response.get("error").is_none(), "{method}: {response}");
    response["result"].clone()
}

fn transfer(path: &Path, workspace: &str, id: &str) -> Value {
    transfer_to(path, workspace, id, "target")
}

fn transfer_to(path: &Path, workspace: &str, id: &str, destination: &str) -> Value {
    let params =
        json!({"workspace_id":workspace,"destination_session":destination,"transfer_id":id});
    let initial = success(path, "workspace.transfer", params.clone());
    assert_eq!(initial["transfer"]["state"], "pending");
    let retry = success(path, "workspace.transfer", params);
    assert_eq!(retry["transfer"]["transfer_id"], id);
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        let status = success(path, "workspace.transfer.status", json!({"transfer_id":id}));
        if status["transfer"]["state"] != "pending" {
            return status["transfer"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "transfer stayed pending: {status}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn send(path: &Path, pane: &str, text: &str) {
    success(
        path,
        "pane.send_input",
        json!({"pane_id":pane,"text":text,"keys":["Enter"]}),
    );
}

fn wait_output(path: &Path, pane: &str, value: &str) {
    success(
        path,
        "pane.wait_for_output",
        json!({"pane_id":pane,"source":"recent","lines":2000,"match":{"type":"substring","value":value},"timeout_ms":10000}),
    );
}

fn pid(path: &Path, pane: &str) -> Value {
    success(path, "pane.process_info", json!({"pane_id":pane}))["process_info"]["shell_pid"].clone()
}

#[test]
fn workspace_transfer_delivers_an_admitted_hook_even_when_its_caller_disconnects_during_the_move() {
    let mut sessions = Sessions::new();
    sessions.start("source");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let created = sessions.create("source", "deferred hook");
    let workspace = created["workspace"]["workspace_id"].as_str().unwrap();
    let pane = created["root_pane"]["pane_id"].as_str().unwrap();
    send(&source, pane, "sleep 60");
    let id = "23000000-0000-4000-8000-000000000001";
    let initial = success(
        &source,
        "workspace.transfer",
        json!({
            "workspace_id":workspace, "destination_session":"target", "transfer_id":id
        }),
    );
    assert_eq!(initial["transfer"]["state"], "pending");
    let mut report = UnixStream::connect(&source).unwrap();
    writeln!(report, "{}", json!({
        "id":"one-shot-hook", "method":"pane.report_agent",
        "params":{"pane_id":pane, "source":"custom:deferred-hook", "agent":"deferred-reporter", "state":"working", "seq":101}
    })).unwrap();
    drop(report);
    let deadline = Instant::now() + Duration::from_secs(35);
    let destination_pane = loop {
        let status = success(
            &source,
            "workspace.transfer.status",
            json!({"transfer_id":id}),
        );
        let transfer = &status["transfer"];
        assert_ne!(transfer["state"], "failed", "{transfer}");
        if transfer["state"] == "committed" {
            break transfer["focused_pane_id"].as_str().unwrap().to_owned();
        }
        assert!(Instant::now() < deadline, "{transfer}");
        std::thread::sleep(Duration::from_millis(25));
    };
    loop {
        let current = success(&target, "pane.get", json!({"pane_id":destination_pane}));
        if current["pane"]["agent"] == "deferred-reporter" {
            assert_eq!(current["pane"]["agent_status"], "working");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "deferred hook was lost: {current}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    success(&target, "server.stop", json!({}));
}

#[test]
fn workspace_transfer_preserves_processes_all_tabs_alternate_history_and_raw_hook_routing() {
    let mut sessions = Sessions::without_graphics();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let moved = sessions.create("source", "moving");
    let workspace = moved["workspace"]["workspace_id"].as_str().unwrap();
    let pane = moved["root_pane"]["pane_id"].as_str().unwrap();
    let terminal_id = moved["root_pane"]["terminal_id"].clone();
    let shell_pid = pid(&source, pane);
    let tab = success(
        &source,
        "tab.create",
        json!({"workspace_id":workspace,"label":"second tab","focus":false}),
    );
    let tab_pane = tab["root_pane"]["pane_id"].as_str().unwrap();
    let tab_pid = pid(&source, tab_pane);
    let split = success(
        &source,
        "pane.split",
        json!({"target_pane_id":tab_pane,"direction":"down","focus":false}),
    );
    let third_pane = split["pane"]["pane_id"].as_str().unwrap();
    let third_pid = pid(&source, third_pane);
    success(
        &source,
        "pane.zoom",
        json!({"pane_id":tab_pane,"mode":"on"}),
    );
    success(
        &source,
        "pane.report_agent",
        json!({"pane_id":pane,"source":"transfer-test","agent":"codex","state":"idle","seq":1}),
    );
    success(
        &source,
        "agent.rename",
        json!({"target":pane,"name":"traveller"}),
    );
    success(
        &source,
        "pane.report_metadata",
        json!({"pane_id":pane,"source":"transfer-test","seq":7,"tokens":{"task":"w1:p1 is literal payload"}}),
    );
    success(
        &source,
        "workspace.report_metadata",
        json!({"workspace_id":workspace,"source":"transfer-test","seq":5,"tokens":{"project":"retained"}}),
    );
    let closed_tab = success(
        &source,
        "tab.create",
        json!({"workspace_id":workspace,"label":"closed tab","focus":false}),
    );
    success(
        &source,
        "tab.close",
        json!({"tab_id":closed_tab["tab"]["tab_id"]}),
    );
    let unrelated = sessions.create("source", "unrelated");
    let unrelated_pane = unrelated["root_pane"]["pane_id"].as_str().unwrap();
    let unrelated_pid = pid(&source, unrelated_pane);
    let destination_existing = sessions.create("target", "destination original");
    let destination_workspace = destination_existing["workspace"]["workspace_id"]
        .as_str()
        .unwrap();
    let destination_pane = destination_existing["root_pane"]["pane_id"]
        .as_str()
        .unwrap();
    let destination_pid = pid(&target, destination_pane);
    send(&source, pane, "i=0; while [ \"$i\" -lt 800 ]; do printf 'transfer-primary-%04d xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' \"$i\"; i=$((i+1)); done; printf '\\033[?1049h\\nALT-TRANSFER-READY\\n'");
    wait_output(&source, pane, "ALT-TRANSFER-READY");
    let status = transfer(&source, workspace, "21000000-0000-4000-8000-000000000001");
    assert_eq!(status["state"], "committed", "{status}");
    let canonical = status["workspace_id"].as_str().unwrap();
    assert_ne!(canonical, destination_workspace);
    let panes = success(&target, "pane.list", json!({"workspace_id":canonical}));
    let root = panes["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pane| pane["terminal_id"] == terminal_id)
        .unwrap();
    let destination_root = root["pane_id"].as_str().unwrap();
    assert_eq!(pid(&target, destination_root), shell_pid);
    assert_eq!(pid(&source, pane), shell_pid);
    assert_eq!(pid(&source, tab_pane), tab_pid);
    assert_eq!(pid(&source, third_pane), third_pid);
    assert_eq!(pid(&source, unrelated_pane), unrelated_pid);
    assert_eq!(pid(&target, destination_pane), destination_pid);
    let tabs = success(&target, "tab.list", json!({"workspace_id":canonical}));
    assert_eq!(tabs["tabs"].as_array().unwrap().len(), 2);
    let imported_workspace = success(&target, "workspace.get", json!({"workspace_id":canonical}));
    assert_eq!(
        imported_workspace["workspace"]["tokens"]["project"],
        "retained"
    );
    assert_eq!(
        success(&source, "pane.layout", json!({"pane_id":tab_pane}))["layout"]["zoomed"],
        true
    );
    let next_tab = success(
        &target,
        "tab.create",
        json!({"workspace_id":canonical,"label":"after transfer","focus":false}),
    );
    assert_eq!(
        next_tab["tab"]["number"].as_u64().unwrap(),
        closed_tab["tab"]["number"].as_u64().unwrap() + 1
    );
    assert!(next_tab["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .ends_with(":p5"));
    let forwarded = success(&source, "pane.get", json!({"pane_id":pane}));
    assert_eq!(forwarded["pane"]["workspace_id"], workspace);
    assert_eq!(forwarded["pane"]["pane_id"], pane);
    assert_eq!(
        forwarded["pane"]["tokens"]["task"],
        "w1:p1 is literal payload"
    );
    wait_output(&source, pane, "ALT-TRANSFER-READY");
    success(
        &source,
        "pane.report_metadata",
        json!({"pane_id":pane,"source":"transfer-test","seq":8,"tokens":{"origin":pane}}),
    );
    let root_after = success(&target, "pane.get", json!({"pane_id":destination_root}));
    assert_eq!(root_after["pane"]["tokens"]["origin"], pane);
    let mut subscription = UnixStream::connect(&source).unwrap();
    subscription
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    writeln!(subscription, "{}", json!({"id":"targeted-stream","method":"events.subscribe","params":{"subscriptions":[{"type":"pane.agent_status_changed","pane_id":pane}]}})).unwrap();
    let mut subscription = BufReader::new(subscription);
    let mut line = String::new();
    subscription.read_line(&mut line).unwrap();
    let started: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(started["result"]["type"], "subscription_started");
    success(
        &source,
        "pane.report_agent",
        json!({"pane_id":pane,"source":"transfer-test","agent":"codex","state":"working","seq":2}),
    );
    line.clear();
    subscription.read_line(&mut line).unwrap();
    let event: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(event["data"]["pane_id"], pane);
    drop(subscription);
    let named = success(&source, "agent.get", json!({"target":"traveller"}));
    assert_eq!(named["agent"]["pane_id"], pane);
    success(
        &source,
        "agent.rename",
        json!({"target":"traveller","name":"renamed-traveller"}),
    );
    let renamed = success(&source, "agent.get", json!({"target":"renamed-traveller"}));
    assert_eq!(renamed["agent"]["pane_id"], pane);
    send(&source, pane, "printf '\\033[?1049l\\nPRIMARY-RESTORED\\n'");
    wait_output(&target, destination_root, "PRIMARY-RESTORED");
    let primary = success(
        &target,
        "pane.read",
        json!({"pane_id":destination_root,"source":"recent","lines":2000,"format":"text"}),
    );
    assert!(primary["read"]["text"]
        .as_str()
        .unwrap()
        .contains("transfer-primary-0000"));
    assert!(primary["read"]["text"]
        .as_str()
        .unwrap()
        .contains("transfer-primary-0799"));
    send(&source, unrelated_pane, "printf 'UNRELATED-STILL-ALIVE\\n'");
    wait_output(&source, unrelated_pane, "UNRELATED-STILL-ALIVE");
    let relocated = success(
        &target,
        "pane.move",
        json!({"pane_id":destination_root,"destination":{"type":"new_workspace"},"focus":false}),
    );
    let relocated_pane = relocated["move_result"]["pane"]["pane_id"]
        .as_str()
        .unwrap();
    let relocated_agent = success(&source, "agent.get", json!({"target":"renamed-traveller"}));
    assert_eq!(relocated_agent["agent"]["terminal_id"], terminal_id);
    assert_eq!(pid(&source, pane), shell_pid);
    assert_eq!(
        success(&source, "agent.get", json!({"target":pane}))["agent"]["terminal_id"],
        terminal_id
    );
    success(&source, "server.stop", json!({}));
    send(&target, relocated_pane, "printf 'TARGET-OWNS-PROCESS\\n'");
    wait_output(&target, relocated_pane, "TARGET-OWNS-PROCESS");
    assert_eq!(pid(&target, relocated_pane), shell_pid);
}

#[test]
fn workspace_transfer_starts_destination_and_preserves_forwarding_after_empty_source_restart() {
    let mut sessions = Sessions::without_graphics();
    sessions.start("source");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let moved = sessions.create("source", "only workspace");
    let workspace = moved["workspace"]["workspace_id"].as_str().unwrap();
    let pane = moved["root_pane"]["pane_id"].as_str().unwrap();
    let shell_pid = pid(&source, pane);
    let status = transfer(&source, workspace, "21000000-0000-4000-8000-000000000002");
    assert_eq!(status["state"], "committed", "{status}");
    let canonical = status["focused_pane_id"].as_str().unwrap();
    assert_eq!(pid(&target, canonical), shell_pid);
    let local = success(&source, "workspace.list", json!({}));
    assert_eq!(local["workspaces"].as_array().unwrap().len(), 0);
    sessions.stop_first();
    sessions.start("source");
    assert_eq!(pid(&source, pane), shell_pid);
    let new_workspace = sessions.create("source", "new local");
    assert_ne!(new_workspace["workspace"]["workspace_id"], workspace);
    send(&source, pane, "printf 'FORWARDED-AFTER-RESTART\\n'");
    wait_output(&target, canonical, "FORWARDED-AFTER-RESTART");
    let retry = success(
        &source,
        "workspace.transfer",
        json!({"workspace_id":workspace,"destination_session":"target","transfer_id":"21000000-0000-4000-8000-000000000002"}),
    );
    assert_eq!(retry["transfer"]["state"], "committed");
}

#[test]
fn workspace_transfer_rejects_name_collision_and_rolls_back_original_reader() {
    let mut sessions = Sessions::without_graphics();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let moved = sessions.create("source", "moving");
    let existing = sessions.create("target", "not overwritten");
    let workspace = moved["workspace"]["workspace_id"].as_str().unwrap();
    let pane = moved["root_pane"]["pane_id"].as_str().unwrap();
    let target_pane = existing["root_pane"]["pane_id"].as_str().unwrap();
    let shell_pid = pid(&source, pane);
    let other_pid = pid(&target, target_pane);
    success(
        &source,
        "pane.report_agent",
        json!({"pane_id":pane,"source":"transfer-test","agent":"codex","state":"idle","seq":1}),
    );
    success(
        &target,
        "pane.report_agent",
        json!({"pane_id":target_pane,"source":"transfer-test","agent":"codex","state":"idle","seq":1}),
    );
    success(
        &source,
        "agent.rename",
        json!({"target":pane,"name":"collision"}),
    );
    success(
        &target,
        "agent.rename",
        json!({"target":target_pane,"name":"collision"}),
    );
    let status = transfer(&source, workspace, "21000000-0000-4000-8000-000000000003");
    assert_eq!(status["state"], "failed", "{status}");
    assert!(status["error"].as_str().unwrap().contains("collision"));
    assert_eq!(pid(&source, pane), shell_pid);
    assert_eq!(pid(&target, target_pane), other_pid);
    send(&source, pane, "printf 'ROLLBACK-READER-RESUMED\\n'");
    wait_output(&source, pane, "ROLLBACK-READER-RESUMED");
    let conflict = raw(
        &source,
        "workspace.transfer",
        json!({"workspace_id":workspace,"destination_session":"another","transfer_id":"21000000-0000-4000-8000-000000000003"}),
    );
    assert_eq!(conflict["error"]["code"], "transfer_conflict");
}

#[test]
fn workspace_transfer_can_return_to_an_origin_session_without_alias_cycles() {
    let mut sessions = Sessions::without_graphics();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let moving = sessions.create("source", "round trip");
    let original_workspace = moving["workspace"]["workspace_id"].as_str().unwrap();
    let original_pane = moving["root_pane"]["pane_id"].as_str().unwrap();
    let original_pid = pid(&source, original_pane);
    success(
        &source,
        "pane.report_agent",
        json!({"pane_id":original_pane,"source":"transfer-test","agent":"codex","state":"idle","seq":1}),
    );
    success(
        &source,
        "agent.rename",
        json!({"target":original_pane,"name":"returning"}),
    );
    let first = transfer(
        &source,
        original_workspace,
        "21000000-0000-4000-8000-000000000005",
    );
    assert_eq!(first["state"], "committed", "{first}");
    let second = transfer_to(
        &target,
        first["workspace_id"].as_str().unwrap(),
        "21000000-0000-4000-8000-000000000006",
        "source",
    );
    assert_eq!(second["state"], "committed", "{second}");
    assert_ne!(second["workspace_id"], original_workspace);
    assert_eq!(pid(&source, original_pane), original_pid);
    assert_eq!(
        success(&source, "agent.get", json!({"target":"returning"}))["agent"]["terminal_id"],
        moving["root_pane"]["terminal_id"]
    );
    success(
        &source,
        "agent.rename",
        json!({"target":"returning","name":"returning-new"}),
    );
    assert_eq!(
        success(&source, "agent.get", json!({"target":"returning"}))["agent"]["name"],
        "returning-new"
    );
    let third = transfer(
        &source,
        second["workspace_id"].as_str().unwrap(),
        "21000000-0000-4000-8000-000000000007",
    );
    assert_eq!(third["state"], "committed", "{third}");
    assert_eq!(pid(&source, original_pane), original_pid);
    assert_eq!(
        success(&source, "agent.get", json!({"target":"returning"}))["agent"]["name"],
        "returning-new"
    );
    send(
        &source,
        original_pane,
        "printf 'ORIGIN-ROUTING-SURVIVED\\n'",
    );
    wait_output(
        &target,
        third["focused_pane_id"].as_str().unwrap(),
        "ORIGIN-ROUTING-SURVIVED",
    );
}

#[test]
fn tab_move_aliases_stay_with_the_live_pane_when_its_old_space_moves_to_another_session() {
    let mut sessions = Sessions::without_graphics();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let moving = sessions.create("source", "tab origin");
    let retained = sessions.create("source", "retained space");
    let untouched = sessions.create("target", "existing destination");
    let original_workspace = moving["workspace"]["workspace_id"].as_str().unwrap();
    let original_pane = moving["root_pane"]["pane_id"].as_str().unwrap();
    let tab_id = moving["tab"]["tab_id"].as_str().unwrap();
    let retained_workspace = retained["workspace"]["workspace_id"].as_str().unwrap();
    let untouched_pane = untouched["root_pane"]["pane_id"].as_str().unwrap();
    let original_pid = pid(&source, original_pane);
    let retained_pid = pid(&source, retained["root_pane"]["pane_id"].as_str().unwrap());
    let untouched_pid = pid(&target, untouched_pane);
    let moved = success(
        &source,
        "tab.move_to_workspace",
        json!({"tab_id":tab_id,"workspace_id":retained_workspace}),
    );
    assert_eq!(moved["tab"]["workspace_id"], retained_workspace);
    assert_eq!(pid(&source, original_pane), original_pid);
    success(
        &source,
        "pane.report_agent",
        json!({"pane_id":original_pane,"source":"tab-transfer-test","agent":"codex","state":"idle","seq":1}),
    );
    let exported = transfer(
        &source,
        original_workspace,
        "21000000-0000-4000-8000-000000000008",
    );
    assert_eq!(exported["state"], "committed", "{exported}");
    assert_eq!(pid(&source, original_pane), original_pid);
    assert_eq!(
        pid(&source, retained["root_pane"]["pane_id"].as_str().unwrap()),
        retained_pid
    );
    success(
        &source,
        "pane.report_agent",
        json!({"pane_id":original_pane,"source":"tab-transfer-test","agent":"codex","state":"working","seq":2}),
    );
    assert_eq!(
        success(&source, "agent.get", json!({"target":original_pane}))["agent"]["terminal_id"],
        moving["root_pane"]["terminal_id"],
    );
    send(&source, original_pane, "printf 'RETAINED-TAB-ALIAS\\n'");
    wait_output(&source, original_pane, "RETAINED-TAB-ALIAS");
    let owner_moved = transfer(
        &source,
        retained_workspace,
        "21000000-0000-4000-8000-000000000009",
    );
    assert_eq!(owner_moved["state"], "committed", "{owner_moved}");
    assert_eq!(pid(&source, original_pane), original_pid);
    success(
        &source,
        "pane.report_agent",
        json!({"pane_id":original_pane,"source":"tab-transfer-test","agent":"codex","state":"idle","seq":3}),
    );
    send(&source, original_pane, "printf 'EXPORTED-TAB-ALIAS\\n'");
    wait_output(&source, original_pane, "EXPORTED-TAB-ALIAS");
    assert_eq!(pid(&target, untouched_pane), untouched_pid);
}

#[test]
fn workspace_transfer_default_empty_graphics_terminal_is_supported() {
    let mut sessions = Sessions::new();
    sessions.start("source");
    let source = sessions.path("source");
    let moved = sessions.create("source", "default terminal");
    let workspace = moved["workspace"]["workspace_id"].as_str().unwrap();
    let pane = moved["root_pane"]["pane_id"].as_str().unwrap();
    let original_pid = pid(&source, pane);
    let status = transfer(&source, workspace, "21000000-0000-4000-8000-000000000004");
    assert_eq!(status["state"], "committed", "{status}");
    assert_eq!(pid(&source, pane), original_pid);
}
