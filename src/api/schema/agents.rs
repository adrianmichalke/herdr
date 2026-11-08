use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::{AgentStatus, ReadFormat, ReadSource};
use crate::agent_activity::AgentActivity;
use crate::cli::agent::bindings;
use crate::cli::projection::{cli_params, Field};

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentReadParams => AgentReadProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        pub source: ReadSource => Field::new(ReadSource::Recent).input(bindings::source()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub lines: Option<u32> => Field::new(None).input(bindings::lines()),
        #[serde(default)]
        pub format: ReadFormat => Field::new(ReadFormat::Text)
            .input(bindings::format()).input(bindings::ansi()),
        #[serde(default = "super::common::default_true")]
        pub strip_ansi: bool => Field::new(true),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentSendKeysParams => AgentSendKeysProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        pub keys: Vec<String> => Field::new(Vec::new()).input(bindings::keys()),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentWaitParams => AgentWaitProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub until: Vec<AgentStatus> => Field::new(Vec::new()).input(bindings::until()
            .help("State to match; repeat for more than one state")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64> => Field::new(None).input(bindings::timeout()
            .help("Fail after this many milliseconds")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentPromptWaitOptions => AgentPromptWaitProjection {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub until: Vec<AgentStatus> => Field::new(Vec::new()).input(bindings::until()
            .help("State to match after --wait; repeat for more than one state")),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64> => Field::new(None).input(bindings::timeout()
            .help("Fail after this many milliseconds")),
        #[serde(skip)]
        #[schemars(skip)]
        pub(crate) submission_deadline: Option<std::time::Instant> => Field::new(None::<std::time::Instant>),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentRenameParams => AgentRenameProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String> => Field::new(None)
            .input(bindings::rename_name()).input(bindings::clear_name()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentViewSetParams {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<AgentViewFilter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<AgentViewSort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentViewClearParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentViewFilter {
    All {
        filters: Vec<AgentViewFilter>,
    },
    Any {
        filters: Vec<AgentViewFilter>,
    },
    Not {
        filter: Box<AgentViewFilter>,
    },
    Eq {
        field: AgentViewField,
        value: AgentViewValue,
    },
    In {
        field: AgentViewField,
        values: Vec<AgentViewValue>,
    },
    Exists {
        field: AgentViewField,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AgentViewField {
    Builtin(AgentViewBuiltinField),
    Token { token: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewBuiltinField {
    Status,
    WorkspaceId,
    TabId,
    PaneId,
    Agent,
    Seen,
    StateChangeSeq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AgentViewValue {
    String(String),
    Bool(bool),
    Number(u64),
    Context { context: AgentViewContext },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewContext {
    CurrentWorkspaceId,
    CurrentTabId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentViewSort {
    pub field: AgentViewSortField,
    #[serde(default)]
    pub order: AgentViewSortOrder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AgentViewSortField {
    Builtin(AgentViewBuiltinSortField),
    Token { token: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewBuiltinSortField {
    WorkspaceOrder,
    TabOrder,
    PaneOrder,
    Attention,
    Status,
    Agent,
    Seen,
    StateChangeSeq,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewSortOrder {
    #[default]
    Asc,
    Desc,
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentStartParams => AgentStartProjection {
        pub name: String => Field::new(String::new()).input(bindings::text_position("name", "NAME", 0)),
        pub kind: String => Field::new(String::new()).input(bindings::kind()),
        pub pane_id: String => Field::new(String::new()).input(bindings::pane()),
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub args: Vec<String> => Field::new(Vec::new()).input(bindings::agent_args()),
        /// Startup timeout in milliseconds. Values must be greater than 3000 and at most 300000.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64> => Field::new(None).input(bindings::timeout()
            .help("Wait for interactive readiness (default: 30000; max: 300000)")),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct AgentPromptParams => AgentPromptProjection {
        pub target: String => Field::new(String::new()).input(bindings::target()),
        pub text: String => Field::new(String::new()).input(bindings::text_position("text", "TEXT", 1).verbatim()),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub wait: Option<AgentPromptWaitOptions> => Field::new(None).input(bindings::wait_presence()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentInfo {
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title_stripped: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_agent: Option<String>,
    pub agent_status: AgentStatus,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub screen_detection_skipped: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[schemars(schema_with = "super::common::metadata_token_values_schema")]
    pub tokens: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSessionInfo>,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub launch_pending: bool,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub interactive_ready: bool,
    #[serde(default)]
    pub state_change_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<AgentActivity>,
    /// The current idle transition completed work, independently of who has viewed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_cwd: Option<String>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_agent_info() -> serde_json::Value {
        serde_json::json!({
            "terminal_id": "terminal",
            "agent_status": "idle",
            "workspace_id": "workspace",
            "tab_id": "tab",
            "pane_id": "pane",
            "focused": false,
            "revision": 7
        })
    }

    #[test]
    fn agent_info_activity_is_optional_for_legacy_json() {
        let mut value = legacy_agent_info();
        let agent: AgentInfo = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(agent.activity, None);
        assert!(serde_json::to_value(&agent)
            .unwrap()
            .get("activity")
            .is_none());

        value["activity"] = serde_json::Value::Null;
        assert_eq!(serde_json::from_value::<AgentInfo>(value).unwrap(), agent);
    }

    #[test]
    fn agent_info_activity_round_trips_with_or_without_a_timestamp() {
        for changed_at_ms in [Some(1_700_000_000_000), None] {
            let activity = AgentActivity {
                sequence: 11,
                changed_at_ms,
            };
            let mut value = legacy_agent_info();
            value["activity"] = serde_json::to_value(&activity).unwrap();
            let agent: AgentInfo = serde_json::from_value(value).unwrap();
            assert_eq!(agent.activity, Some(activity));
            assert_eq!(
                serde_json::from_value::<AgentInfo>(serde_json::to_value(&agent).unwrap()).unwrap(),
                agent
            );
        }
    }

    #[test]
    fn agent_info_schema_keeps_activity_optional() {
        let schema = serde_json::to_value(schemars::schema_for!(AgentInfo)).unwrap();
        assert!(schema["properties"].get("activity").is_some());
        assert!(!schema["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("activity")));
    }
}
