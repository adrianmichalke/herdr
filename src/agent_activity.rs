use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentActivity {
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAgentActivity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<AgentActivity>,
    pub state: crate::detect::AgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl AgentActivity {
    pub(crate) fn now(sequence: u64) -> Self {
        Self {
            sequence,
            changed_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| duration.as_millis().try_into().ok()),
        }
    }
}
