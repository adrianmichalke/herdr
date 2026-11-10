use crate::api::schema::{PaneInfo, ResponseResult, SessionSnapshot};
use crate::app::App;

use super::responses::encode_success;

impl App {
    pub(crate) fn handle_session_history_snapshot(&self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::SessionSnapshot {
                snapshot: Box::new(self.session_metadata_snapshot()),
            },
        )
    }

    pub(super) fn handle_session_snapshot(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::SessionSnapshot {
                snapshot: Box::new(self.session_snapshot()),
            },
        )
    }

    pub(crate) fn session_snapshot(&self) -> SessionSnapshot {
        self.session_snapshot_with_pane_info(Self::pane_info)
    }

    pub(crate) fn session_metadata_snapshot(&self) -> SessionSnapshot {
        self.session_snapshot_with_pane_info(Self::pane_metadata)
    }

    fn session_snapshot_with_pane_info(
        &self,
        pane_info: fn(&Self, usize, crate::layout::PaneId) -> Option<PaneInfo>,
    ) -> SessionSnapshot {
        let focused_workspace_id = self
            .state
            .active
            .map(|ws_idx| self.public_workspace_id(ws_idx));
        let focused_tab_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_tab_id(ws_idx, ws.active_tab)
        });
        let focused_pane_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_pane_id(ws_idx, ws.focused_pane_id()?)
        });

        let mut workspaces = Vec::new();
        let mut tabs = Vec::new();
        let mut layouts = Vec::new();
        let mut panes = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            workspaces.push(self.workspace_info(ws_idx));
            for tab_idx in 0..ws.tabs.len() {
                if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                    tabs.push(tab);
                }
                if let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) {
                    layouts.push(layout);
                }
                panes.extend(
                    ws.tabs[tab_idx]
                        .layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(|pane_id| pane_info(self, ws_idx, pane_id)),
                );
            }
        }

        SessionSnapshot {
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            focused_workspace_id,
            focused_tab_id,
            focused_pane_id,
            workspaces,
            tabs,
            panes,
            layouts,
            agents: self.collect_agent_infos(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{EmptyParams, Method, ResponseResult, SuccessResponse};
    use crate::{config::Config, workspace::Workspace};

    fn app_with_two_tabs() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("snapshot");
        workspace.test_add_tab(None);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app
    }

    #[test]
    fn history_snapshot_does_not_drain_pending_events_or_change_state() {
        let mut app = app_with_two_tabs();
        let before = serde_json::to_value(app.session_metadata_snapshot()).unwrap();
        let runtimes_before = app.terminal_runtimes.len();
        app.event_tx
            .try_send(crate::events::AppEvent::UpdateReady {
                version: "synthetic-pending".into(),
                install_command: "unused".into(),
            })
            .unwrap();
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "history-read".into(),
            method: Method::SessionHistorySnapshot(EmptyParams::default()),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::SessionSnapshot { snapshot } = success.result else {
            panic!("expected history metadata");
        };
        assert_eq!(serde_json::to_value(snapshot).unwrap(), before);
        assert_eq!(
            serde_json::to_value(app.session_metadata_snapshot()).unwrap(),
            before
        );
        assert_eq!(app.terminal_runtimes.len(), runtimes_before);
        assert_eq!(app.event_rx.len(), 1);
        assert!(app.state.update_available.is_none());
    }

    #[test]
    fn history_snapshot_of_empty_session_never_creates_a_workspace() {
        let mut app = app_with_two_tabs();
        app.state.workspaces.clear();
        app.state.active = None;
        let runtime_count = app.terminal_runtimes.len();
        for after_drained in [false, true] {
            let request = crate::api::schema::Request {
                id: "empty-history".into(),
                method: Method::SessionHistorySnapshot(EmptyParams::default()),
            };
            let response = if after_drained {
                app.handle_api_request_after_internal_events_drained(request)
            } else {
                app.handle_api_request(request)
            };
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::SessionSnapshot { snapshot } = success.result else {
                panic!("expected history metadata");
            };
            assert!(snapshot.workspaces.is_empty());
            assert!(app.state.workspaces.is_empty());
            assert_eq!(app.terminal_runtimes.len(), runtime_count);
        }
    }

    #[test]
    fn conditional_focus_rejects_reused_terminal_or_changed_native_reference() {
        let mut app = app_with_two_tabs();
        let pane_id = app.state.workspaces[0].tabs[1].layout.pane_ids()[0];
        let terminal_id = app.state.workspaces[0].tabs[1].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:pi".into(),
                agent: "pi".into(),
                session_ref: crate::agent_resume::AgentSessionRef {
                    kind: crate::agent_resume::AgentSessionRefKind::Path,
                    value: "/synthetic/pi.jsonl".into(),
                },
            });
        let target = app.pane_metadata(0, pane_id).unwrap();
        let params = crate::api::schema::PaneFocusIfSessionParams {
            pane_id: target.pane_id.clone(),
            terminal_id: target.terminal_id.clone(),
            agent_session: target.agent_session.clone().unwrap(),
        };
        let before = serde_json::to_value(app.session_metadata_snapshot()).unwrap();
        let mut stale_terminal = params.clone();
        stale_terminal.terminal_id.push_str("-reused");
        let mut stale_ref = params.clone();
        stale_ref.agent_session.value.push_str("-other");
        let mut stale_kind = params.clone();
        stale_kind.agent_session.kind = crate::agent_resume::AgentSessionRefKind::Id;
        for params in [stale_terminal, stale_ref, stale_kind] {
            let response = app.handle_api_request(crate::api::schema::Request {
                id: "conditional-focus".into(),
                method: Method::PaneFocusIfSession(params),
            });
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&response).unwrap()["error"]["code"],
                "stale_agent_session"
            );
            assert_eq!(
                serde_json::to_value(app.session_metadata_snapshot()).unwrap(),
                before
            );
        }
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "conditional-focus".into(),
            method: Method::PaneFocusIfSession(params),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected focused pane");
        };
        assert_eq!(pane.pane_id, target.pane_id);
        assert!(pane.focused);
        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_id));
    }

    #[test]
    fn session_snapshot_bootstraps_runtime_resources() {
        let mut app = app_with_two_tabs();
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_snapshot".into(),
            method: Method::SessionSnapshot(EmptyParams::default()),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::SessionSnapshot { snapshot } = success.result else {
            panic!("expected session snapshot response");
        };
        assert_eq!(success.id, "req_snapshot");
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.panes.len(), 2);
        assert_eq!(snapshot.layouts.len(), 2);
        assert_eq!(
            snapshot.focused_workspace_id.as_deref(),
            Some(snapshot.workspaces[0].workspace_id.as_str())
        );
        assert_eq!(
            snapshot.focused_tab_id.as_deref(),
            Some(snapshot.tabs[0].tab_id.as_str())
        );
        assert_eq!(
            snapshot.focused_pane_id.as_deref(),
            Some(snapshot.panes[0].pane_id.as_str())
        );
    }
}
