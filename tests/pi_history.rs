#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

struct Fixture {
    root: PathBuf,
    history: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "herdr-history-cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let history = root.join("history.jsonl");
        let entries = [
            json!({"type":"session","version":3,"id":"synthetic"}),
            json!({"type":"message","id":"old","parentId":null,"message":{"role":"user","content":"NEEDLE[5] before compaction"}}),
            json!({"type":"compaction","firstKeptEntryId":"recent","summary":"short summary"}),
            json!({"type":"message","id":"inactive","parentId":"old","message":{"role":"assistant","content":[{"type":"text","text":"needle[5] inactive branch"}]}}),
            json!({"type":"context_edit","targetId":"old","replacement":null}),
            json!({"type":"message","id":"recent","parentId":"old","message":{"role":"user","content":"current context"}}),
        ];
        fs::write(
            &history,
            entries
                .iter()
                .map(|entry| format!("{entry}\n"))
                .collect::<String>(),
        )
        .unwrap();
        for session in ["default", "other"] {
            let dir = if session == "default" {
                root.join("config/herdr-dev")
            } else {
                root.join("config/herdr-dev/sessions").join(session)
            };
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("session.json"),
                Self::snapshot(&history).to_string(),
            )
            .unwrap();
        }
        Self { root, history }
    }

    fn snapshot(history: &Path) -> Value {
        json!({"version":3,"active":0,"selected":0,"workspaces":[{
            "id":"w1","custom_name":"Synthetic space","identity_cwd":history.parent(),
            "public_tab_numbers":[3],"public_pane_numbers":{"7":2},"tabs":[{
                "custom_name":"Synthetic tab","layout":{"Pane":7},"zoomed":false,"panes":{"7":{
                    "cwd":history.parent(),"agent_session":{"source":"herdr:pi","agent":"pi","kind":"path","value":history}
                }}
            }]
        }]})
    }

    fn run(&self, request: Value) -> Vec<Value> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(["history", "--stdio"])
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"))
            .env("HERDR_ENV", "1")
            .env("HERDR_SOCKET_PATH", self.root.join("unrelated.sock"))
            .env_remove("HERDR_CLIENT_SOCKET_PATH")
            .env_remove("HERDR_CONFIG_PATH")
            .env_remove("HERDR_SESSION")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_TAB_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect()
    }

    fn request(&self, scope: &str, session: &str, action: Value) -> Value {
        json!({"scope":scope,"session":session,"workspace_id":"w1","query":"needle[5]","action":action})
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn history_stdio_searches_and_previews_stopped_sessions_without_starting_servers() {
    let fixture = Fixture::new();
    let before = fs::read(&fixture.history).unwrap();
    let saved: Vec<_> = [
        "config/herdr-dev/session.json",
        "config/herdr-dev/sessions/other/session.json",
    ]
    .map(|path| {
        (
            fixture.root.join(path),
            fs::read(fixture.root.join(path)).unwrap(),
        )
    })
    .into_iter()
    .collect();
    let events = fixture.run(fixture.request("all", "default", json!({"kind":"search"})));
    assert_eq!(
        events.first().unwrap(),
        &json!({"type":"hello","version":1})
    );
    assert_eq!(
        events.last().unwrap(),
        &json!({"type":"done","truncated":false})
    );
    let results: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "result")
        .collect();
    assert_eq!(results.len(), 1);
    let result = &results[0]["result"];
    assert_eq!(result["matches"].as_array().unwrap().len(), 2);
    assert_eq!(result["source"]["locations"].as_array().unwrap().len(), 2);
    assert!(result["source"]["locations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|location| location["running"] == false));
    let preview = fixture.run(fixture.request(
        "all",
        "default",
        json!({
            "kind":"preview","source_id":result["source"]["id"],"anchor":2
        }),
    ));
    let value = preview
        .iter()
        .find(|event| event["type"] == "preview")
        .unwrap();
    let matched = value["preview"]["matched_entry"].as_u64().unwrap() as usize;
    assert!(value["preview"]["entries"][matched]["text"]
        .as_str()
        .unwrap()
        .contains("before compaction"));
    assert_eq!(fs::read(&fixture.history).unwrap(), before);
    for (path, contents) in saved {
        assert_eq!(fs::read(path).unwrap(), contents);
    }
    for session_dir in ["config/herdr-dev", "config/herdr-dev/sessions/other"] {
        let dir = fixture.root.join(session_dir);
        assert!(!dir.join("herdr.sock").exists());
        assert!(!dir.join("herdr-client.sock").exists());
        assert!(!dir.join("herdr-server.log").exists());
    }
}

#[test]
fn history_stdio_honors_scope_and_rejects_unlisted_preview_handles() {
    let fixture = Fixture::new();
    for scope in ["space", "session"] {
        let events = fixture.run(fixture.request(scope, "other", json!({"kind":"search"})));
        let result = events
            .iter()
            .find(|event| event["type"] == "result")
            .unwrap();
        let locations = result["result"]["source"]["locations"].as_array().unwrap();
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0]["session"], "other");
        assert_eq!(locations[0]["pane_id"], "w1:p2");
        assert_eq!(locations[0]["tab_id"], "w1:t3");
    }
    let events = fixture.run(fixture.request(
        "all",
        "default",
        json!({
            "kind":"preview","source_id":format!("pi:{}", "f".repeat(64)),"anchor":2
        }),
    ));
    assert!(!events.iter().any(|event| event["type"] == "preview"));
    assert!(events.iter().any(|event| event["type"] == "unavailable"));
    assert_eq!(
        events.last().unwrap(),
        &json!({"type":"done","truncated":true})
    );
}
