use super::*;

fn history_request(
    server: &mut HeadlessServer,
    client_local: bool,
) -> (bool, api::schema::SessionSnapshot) {
    let (respond_to, response) = std::sync::mpsc::channel();
    let msg = api::ApiRequestMessage {
        request: api::schema::Request {
            id: "history-read".into(),
            method: api::schema::Method::SessionHistorySnapshot(api::schema::EmptyParams::default()),
        },
        respond_to,
        response_write_complete: None,
    };
    let changed = if client_local {
        server.handle_client_shell_api_request(1, msg)
    } else {
        server.handle_api_request_with_shutdown_check(msg)
    };
    let success: api::schema::SuccessResponse =
        serde_json::from_str(&response.recv().unwrap()).unwrap();
    let api::schema::ResponseResult::SessionSnapshot { snapshot } = success.result else {
        panic!("expected metadata snapshot");
    };
    (changed, *snapshot)
}

#[test]
fn history_snapshot_with_shell_client_skips_default_workspace_and_pending_events() {
    let mut server = test_headless_server();
    server.app.state.workspaces.clear();
    server.app.state.active = None;
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            Default::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    server.foreground_client_id = Some(1);
    let runtime_count = server.app.terminal_runtimes.len();
    server
        .app
        .event_tx
        .try_send(crate::events::AppEvent::UpdateReady {
            version: "synthetic-pending".into(),
            install_command: "unused".into(),
        })
        .unwrap();
    for client_local in [false, true] {
        let (changed, snapshot) = history_request(&mut server, client_local);
        assert!(!changed);
        assert!(snapshot.workspaces.is_empty());
        assert!(snapshot.panes.is_empty());
    }
    assert!(server.app.state.workspaces.is_empty());
    assert_eq!(server.app.terminal_runtimes.len(), runtime_count);
    assert_eq!(server.app.event_rx.len(), 1);
    assert!(server.app.state.update_available.is_none());
    assert!(server.clients[&1].shell_location.is_none());
    assert!(server.tab_geometry_controllers.is_empty());
    assert!(
        !crate::server::client_commands::supports_client_shell_method_name(
            "session.history_snapshot"
        )
    );
}

#[test]
fn history_snapshot_preserves_focus_seen_and_terminal_metadata_with_pending_title() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("synthetic-history");
    workspace.test_add_tab(None);
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    let pane_id = server.app.state.workspaces[0].tabs[1].layout.pane_ids()[0];
    server.app.render_dirty.request_terminal_title(pane_id);
    let before = serde_json::to_value(server.app.session_metadata_snapshot()).unwrap();
    let runtime_count = server.app.terminal_runtimes.len();
    let (changed, snapshot) = history_request(&mut server, false);
    assert!(!changed);
    assert_eq!(serde_json::to_value(snapshot).unwrap(), before);
    assert_eq!(
        serde_json::to_value(server.app.session_metadata_snapshot()).unwrap(),
        before
    );
    assert_eq!(server.app.terminal_runtimes.len(), runtime_count);
}

fn server_with_conditional_focus_target() -> (
    HeadlessServer,
    crate::layout::PaneId,
    api::schema::PaneFocusIfSessionParams,
) {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("conditional-focus");
    workspace.test_add_tab(None);
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    let pane_id = server.app.state.workspaces[0].tabs[1].layout.pane_ids()[0];
    let terminal_id = server.app.state.workspaces[0]
        .pane_state(pane_id)
        .unwrap()
        .attached_terminal_id
        .clone();
    let terminal = server.app.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.detected_agent = Some(crate::detect::Agent::Pi);
    terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
        source: "herdr:pi".into(),
        agent: "pi".into(),
        session_ref: crate::agent_resume::AgentSessionRef {
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: "/synthetic/old.jsonl".into(),
        },
    });
    for (id, size) in [(1, (80, 24)), (2, (140, 40))] {
        let mut client = ClientConnection::new(
            size,
            Default::default(),
            id,
            RenderEncoding::SemanticFrame,
            None,
        );
        client.outer_terminal_focus = Some(true);
        server.clients.insert(id, client);
    }
    server.foreground_client_id = Some(1);
    server.reconcile_client_shell_locations();
    let tab_id = server.app.public_tab_id(0, 1).unwrap();
    server.focus_shell_client_on_tab(2, &tab_id);
    let pane = server
        .app
        .session_metadata_snapshot()
        .panes
        .into_iter()
        .find(|pane| pane.terminal_id == terminal_id.as_str())
        .unwrap();
    let params = api::schema::PaneFocusIfSessionParams {
        pane_id: pane.pane_id,
        terminal_id: pane.terminal_id,
        agent_session: pane.agent_session.unwrap(),
    };
    (server, pane_id, params)
}

fn changed_native_reference(pane_id: crate::layout::PaneId) -> crate::events::AppEvent {
    crate::events::AppEvent::AgentSessionReported {
        pane_id,
        source: "herdr:pi".into(),
        agent_label: "pi".into(),
        seq: Some(1),
        session_ref: Some(crate::agent_resume::AgentSessionRef {
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: "/synthetic/new.jsonl".into(),
        }),
        session_start_source: Some("new".into()),
    }
}

#[test]
fn conditional_focus_checks_queued_reference_changes_before_projection_and_promotion() {
    for lane in 0..3 {
        let (mut server, pane_id, params) = server_with_conditional_focus_target();
        let active_pane = server.app.state.workspaces[0].tabs[0].layout.pane_ids()[0];
        server.app.state.workspaces[0]
            .pane_state_mut(active_pane)
            .unwrap()
            .seen = false;
        let location = server.clients[&2].shell_location.clone();
        let size = server.effective_size;
        let stamp = server.clients[&2].last_activity;
        server
            .app
            .event_tx
            .try_send(changed_native_reference(pane_id))
            .unwrap();
        let target_id = params.pane_id.clone();
        let request = api::schema::Request {
            id: "queued-reference".into(),
            method: api::schema::Method::PaneFocusIfSession(params),
        };
        if lane == 2 {
            server.handle_client_shell_endpoint_request(
                2,
                server.client_shell_boot_id.clone(),
                Box::new(request),
            );
            assert!(!server.clients[&2].shell_endpoint_command_in_flight);
        } else {
            let (respond_to, response) = std::sync::mpsc::channel();
            let msg = api::ApiRequestMessage {
                request,
                respond_to,
                response_write_complete: None,
            };
            if lane == 1 {
                server.handle_client_shell_api_request(2, msg);
            } else {
                server.handle_api_request_with_shutdown_check(msg);
            }
            let response: serde_json::Value =
                serde_json::from_str(&response.recv().unwrap()).unwrap();
            assert_eq!(response["error"]["code"], "stale_agent_session");
        }
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(server.clients[&2].last_activity, stamp);
        assert_eq!(server.clients[&2].shell_location, location);
        assert_eq!(server.effective_size, size);
        assert_eq!(server.app.state.workspaces[0].active_tab, 0);
        assert!(
            !server.app.state.workspaces[0]
                .pane_state(active_pane)
                .unwrap()
                .seen
        );
        assert!(server.app.event_rx.is_empty());
        assert_eq!(
            server
                .app
                .session_metadata_snapshot()
                .panes
                .into_iter()
                .find(|pane| pane.pane_id == target_id)
                .unwrap()
                .agent_session
                .unwrap()
                .value,
            "/synthetic/new.jsonl"
        );
    }
}

#[test]
fn conditional_focus_shutdown_rejection_never_promotes_an_endpoint() {
    let (mut server, _, params) = server_with_conditional_focus_target();
    server.shutting_down = true;
    let stamp = server.clients[&2].last_activity;
    let size = server.effective_size;
    assert!(!server.handle_client_shell_endpoint_request(
        2,
        server.client_shell_boot_id.clone(),
        Box::new(api::schema::Request {
            id: "shutdown-focus".into(),
            method: api::schema::Method::PaneFocusIfSession(params),
        })
    ));
    assert_eq!(server.foreground_client_id, Some(1));
    assert_eq!(server.clients[&2].last_activity, stamp);
    assert!(!server.clients[&2].shell_endpoint_command_in_flight);
    assert_eq!(server.effective_size, size);
    assert_eq!(server.app.state.workspaces[0].active_tab, 0);
}

#[test]
fn conditional_focus_critical_section_does_not_drain_new_reference_reports() {
    let (mut server, pane_id, params) = server_with_conditional_focus_target();
    assert!(server.app.pane_focus_session_is_current(&params));
    server
        .app
        .event_tx
        .try_send(changed_native_reference(pane_id))
        .unwrap();
    let (respond_to, response) = std::sync::mpsc::channel();
    server.handle_client_shell_api_request_after_internal_events_drained(
        2,
        api::ApiRequestMessage {
            request: api::schema::Request {
                id: "atomic-focus".into(),
                method: api::schema::Method::PaneFocusIfSession(params.clone()),
            },
            respond_to,
            response_write_complete: None,
        },
    );
    let success: api::schema::SuccessResponse =
        serde_json::from_str(&response.recv().unwrap()).unwrap();
    assert!(
        matches!(success.result, api::schema::ResponseResult::PaneInfo { pane } if pane.pane_id == params.pane_id && pane.focused)
    );
    assert_eq!(server.app.event_rx.len(), 1);
    assert_eq!(server.app.state.workspaces[0].active_tab, 1);
    assert!(server.app.pane_focus_session_is_current(&params));
}

#[test]
fn conditional_focus_failure_does_not_project_shell_navigation_or_seen_state() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("synthetic-focus");
    workspace.test_add_tab(None);
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            Default::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    server.reconcile_client_shell_locations();
    let pane = server.app.session_metadata_snapshot().panes.pop().unwrap();
    let before = serde_json::to_value(server.app.session_metadata_snapshot()).unwrap();
    let location = server.clients[&1].shell_location.clone();
    let params = api::schema::PaneFocusIfSessionParams {
        pane_id: pane.pane_id,
        terminal_id: pane.terminal_id,
        agent_session: api::schema::AgentSessionInfo {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: "/synthetic/no-longer-current.jsonl".into(),
        },
    };
    let foreground_before = server.foreground_client_id;
    assert!(!server.handle_client_shell_endpoint_request(
        1,
        server.client_shell_boot_id.clone(),
        Box::new(api::schema::Request {
            id: "conditional-endpoint".into(),
            method: api::schema::Method::PaneFocusIfSession(params.clone()),
        })
    ));
    assert_eq!(server.foreground_client_id, foreground_before);
    assert!(!server.clients[&1].shell_endpoint_command_in_flight);
    assert_eq!(
        serde_json::to_value(server.app.session_metadata_snapshot()).unwrap(),
        before
    );
    for client_local in [false, true] {
        let (respond_to, response) = std::sync::mpsc::channel();
        let msg = api::ApiRequestMessage {
            request: api::schema::Request {
                id: "conditional-focus".into(),
                method: api::schema::Method::PaneFocusIfSession(params.clone()),
            },
            respond_to,
            response_write_complete: None,
        };
        let changed = if client_local {
            server.handle_client_shell_api_request(1, msg)
        } else {
            server.handle_api_request_with_shutdown_check(msg)
        };
        assert!(!changed);
        let response: serde_json::Value = serde_json::from_str(&response.recv().unwrap()).unwrap();
        assert_eq!(response["error"]["code"], "stale_agent_session");
        assert_eq!(
            serde_json::to_value(server.app.session_metadata_snapshot()).unwrap(),
            before
        );
        assert_eq!(server.clients[&1].shell_location, location);
    }
}
