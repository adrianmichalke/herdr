use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

use super::types::{HistoryLocation, HistoryRequest, HistoryScope};

pub(super) struct FixtureDir(pub PathBuf);

impl FixtureDir {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "herdr-pi-history-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    pub fn write(&self, name: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn request() -> HistoryRequest {
    HistoryRequest {
        scope: HistoryScope::Session,
        session: "one".into(),
        workspace_id: None,
        query: "needle".into(),
        action: super::types::HistoryAction::Search,
    }
}

pub(super) fn location(path: &Path, session: &str, workspace_id: &str) -> HistoryLocation {
    HistoryLocation {
        session: session.into(),
        workspace_id: workspace_id.into(),
        workspace_label: "Synthetic workspace".into(),
        tab_id: crate::workspace::public_tab_id_for_number(workspace_id, 3),
        tab_label: "Synthetic tab".into(),
        pane_id: crate::workspace::public_pane_id_for_number(workspace_id, 2),
        pane_label: None,
        terminal_id: None,
        running: false,
        agent_session: crate::api::schema::AgentSessionInfo {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: path.to_str().unwrap().into(),
        },
    }
}

pub(super) fn saved_snapshot(path: &Path, workspace_id: &str) -> Value {
    json!({"version":3, "active":0, "selected":0, "workspaces":[{
        "id": workspace_id, "custom_name":"Synthetic workspace", "identity_cwd":"/synthetic-project",
        "public_tab_numbers":[3], "public_pane_numbers":{"7":2}, "tabs":[{
            "custom_name":"Synthetic tab", "layout":{"Pane":7}, "zoomed":false, "panes":{"7":{
                "cwd":"/synthetic-project", "agent_session": location(path, "one", workspace_id).agent_session
            }}
        }]
    }]})
}

pub(super) fn live_snapshot(
    path: &Path,
    workspace_id: &str,
) -> crate::api::schema::SessionSnapshot {
    let location = location(path, "one", workspace_id);
    serde_json::from_value(json!({
        "version":"synthetic", "protocol":1, "workspaces":[], "tabs":[], "layouts":[], "agents":[], "panes":[{
            "pane_id":location.pane_id, "terminal_id":"term_synthetic", "workspace_id":workspace_id, "tab_id":location.tab_id,
            "focused":false, "agent_status":"idle", "revision":0, "agent_session":location.agent_session
        }]
    })).unwrap()
}
