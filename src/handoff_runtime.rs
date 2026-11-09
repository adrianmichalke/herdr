#[cfg(unix)]
use serde::{Deserialize, Serialize};

/// Long-lived pane runtime transferred during server replacement.
///
/// Handoff preserves server-owned session state such as PTYs, processes, agent
/// identity, and durable plugin/session metadata. It intentionally does not
/// preserve transient coordination such as in-flight requests, waits,
/// subscriptions, client sockets, or pane-to-pane messages; clients reconnect
/// and retry those operations after replacement.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffRuntimeState {
    pub pane_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<crate::terminal::TerminalId>,
    pub child_pid: u32,
    pub rows: u16,
    pub cols: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    #[serde(default)]
    pub keyboard_protocol_flags: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_protocol_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_state: Option<crate::pane::InputState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_history_ansi: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "terminal_snapshot_serde"
    )]
    pub terminal_snapshot: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_snapshot_metadata: Option<TerminalSnapshotMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<crate::terminal::state::HandoffAgentState>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TerminalSnapshotMetadata {
    #[serde(default)]
    pub kitty_graphics_enabled: bool,
    #[serde(default)]
    pub legacy_query: Option<crate::pane::C1XtgettcapTransferState>,
    pub cursor_shape_overridden: bool,
    pub child_default_foreground_changed: bool,
    pub child_default_background_changed: bool,
    pub initial_default_foreground: Option<[u8; 3]>,
    pub initial_default_background: Option<[u8; 3]>,
    pub transient_default_color_owner_pgid: Option<u32>,
    pub agent_osc_title: String,
    pub agent_osc_progress: String,
}

#[cfg(unix)]
mod terminal_snapshot_serde {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        snapshot: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        snapshot
            .as_deref()
            .map(|bytes| STANDARD.encode(bytes))
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|encoded| STANDARD.decode(encoded).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(unix)]
impl HandoffRuntimeState {
    pub fn with_pane_id(mut self, pane_id: crate::layout::PaneId) -> Self {
        self.pane_id = pane_id.raw();
        self
    }
}

#[derive(Debug)]
pub(crate) struct ImportedHandoffRuntime {
    #[cfg(unix)]
    pub master_fd: std::os::fd::RawFd,
    #[cfg(unix)]
    pub state: HandoffRuntimeState,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    fn state_json() -> serde_json::Value {
        json!({
            "pane_id": 7, "child_pid": 0, "rows": 4, "cols": 40,
            "cell_width_px": 9, "cell_height_px": 18
        })
    }

    #[test]
    fn handoff_terminal_identity_is_optional_and_round_trips_without_changing_pane_id() {
        let mut state: HandoffRuntimeState = serde_json::from_value(state_json()).unwrap();
        assert!(state.terminal_id.is_none());
        assert!(serde_json::to_value(&state)
            .unwrap()
            .get("terminal_id")
            .is_none());
        let terminal_id = crate::terminal::TerminalId::alloc();
        state.terminal_id = Some(terminal_id.clone());
        let state: HandoffRuntimeState =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        let state = state.with_pane_id(crate::layout::PaneId::from_raw(29));
        assert_eq!(state.pane_id, 29);
        assert_eq!(state.terminal_id, Some(terminal_id));
    }

    #[test]
    fn handoff_terminal_snapshot_is_optional_base64_json() {
        let mut state: HandoffRuntimeState = serde_json::from_value(state_json()).unwrap();
        assert!(state.terminal_snapshot.is_none());
        assert!(serde_json::to_value(&state)
            .unwrap()
            .get("terminal_snapshot")
            .is_none());
        let snapshot: Vec<u8> = (0..20_000).map(|i| (i % 256) as u8).collect();
        state.terminal_snapshot = Some(snapshot.clone());
        let encoded = serde_json::to_value(&state).unwrap();
        let encoded_snapshot = encoded["terminal_snapshot"].as_str().unwrap();
        assert_eq!(encoded_snapshot.len(), snapshot.len().div_ceil(3) * 4);
        let decoded: HandoffRuntimeState = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.terminal_snapshot, Some(snapshot));
    }

    #[test]
    fn handoff_terminal_snapshot_rejects_invalid_base64_and_byte_arrays() {
        for invalid in [json!("not base64!"), json!([1, 2, 3])] {
            let mut value = state_json();
            value["terminal_snapshot"] = invalid;
            assert!(serde_json::from_value::<HandoffRuntimeState>(value).is_err());
        }
    }
}
