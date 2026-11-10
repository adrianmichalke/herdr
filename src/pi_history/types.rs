use serde::{Deserialize, Serialize};

pub(crate) const PROTOCOL_VERSION: u32 = 1;
pub(crate) const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HistoryScope {
    #[default]
    Space,
    Session,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryRequest {
    pub scope: HistoryScope,
    pub session: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub query: String,
    pub action: HistoryAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum HistoryAction {
    Search,
    Preview { source_id: String, anchor: u64 },
    Resolve { source_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryLocation {
    pub session: String,
    pub workspace_id: String,
    pub workspace_label: String,
    pub tab_id: String,
    pub tab_label: String,
    pub pane_id: String,
    #[serde(default)]
    pub pane_label: Option<String>,
    #[serde(default)]
    pub terminal_id: Option<String>,
    pub running: bool,
    pub agent_session: crate::api::schema::AgentSessionInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistorySource {
    pub id: String,
    pub name: String,
    pub locations: Vec<HistoryLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryMatch {
    pub entry_line: u64,
    pub role: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryResult {
    pub source: HistorySource,
    pub matches: Vec<HistoryMatch>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryEntry {
    pub entry_line: u64,
    pub role: String,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HistoryPreview {
    pub source: HistorySource,
    pub entries: Vec<HistoryEntry>,
    pub matched_entry: usize,
    pub previous: Option<u64>,
    pub next: Option<u64>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum HistoryEvent {
    Hello {
        version: u32,
    },
    Result {
        result: HistoryResult,
    },
    Preview {
        preview: HistoryPreview,
    },
    Resolved {
        source: HistorySource,
    },
    Unavailable {
        locations: Vec<HistoryLocation>,
        message: String,
    },
    Progress {
        scanned: usize,
        total: usize,
    },
    Done {
        truncated: bool,
    },
    Error {
        message: String,
    },
}
