use super::*;
use crate::api::schema::{Method, ResponseResult, SuccessResponse, TabMoveToWorkspaceParams};

fn server() -> HeadlessServer {
    let mut server = test_headless_server();
    let mut source = crate::workspace::Workspace::test_new("source");
    source.test_add_tab(Some("middle"));
    source.test_add_tab(Some("active"));
    source.switch_tab(2);
    server.app.state.workspaces =
        vec![source, crate::workspace::Workspace::test_new("destination")];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    server
}

#[tokio::test]
async fn tab_transfer_reconciles_removed_source_without_redirecting_unaffected_clients() {
    for client_endpoint in [false, true] {
        let mut server = server();
        let moved_id = server.app.public_tab_id(0, 0).unwrap();
        let source_active_id = server.app.public_tab_id(0, 2).unwrap();
        let destination_active_id = server.app.public_tab_id(1, 0).unwrap();
        let destination_id = server.app.public_workspace_id(1);
        let moved_pane = server.app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = server.app.state.workspaces[0].tabs[0]
            .terminal_id(moved_pane)
            .unwrap()
            .clone();
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(
            80,
            24,
            b"retained transfer output\r\n",
        );
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);
        let (_first_control, _first_render) = connect_test_shell(&mut server, 9, 80, 23);
        let (_second_control, _second_render) = connect_test_shell(&mut server, 10, 100, 30);
        let (_third_control, _third_render) = connect_test_shell(&mut server, 11, 120, 40);
        assert!(server.focus_shell_client_on_tab(9, &source_active_id));
        assert!(server.focus_shell_client_on_tab(10, &destination_active_id));
        assert!(server.focus_shell_client_on_tab(11, &moved_id));
        let locations = [
            server.clients[&9].shell_location.clone(),
            server.clients[&10].shell_location.clone(),
        ];
        let revisions = [
            server.clients[&9].shell_projection_revision,
            server.clients[&10].shell_projection_revision,
        ];
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let message = crate::api::ApiRequestMessage {
            request: crate::api::schema::Request {
                id: "move-tab".into(),
                method: Method::TabMoveToWorkspace(TabMoveToWorkspaceParams {
                    tab_id: moved_id,
                    workspace_id: destination_id,
                }),
            },
            respond_to,
            response_write_complete: None,
        };
        if client_endpoint {
            server.handle_client_shell_api_request(9, message);
        } else {
            server.handle_api_request_with_shutdown_check(message);
        }
        let response = response_rx.recv().unwrap();
        let SuccessResponse {
            result: ResponseResult::TabInfo { tab },
            ..
        } = serde_json::from_str(&response).expect(&response)
        else {
            panic!("expected transferred tab");
        };
        assert_eq!(server.app.parse_tab_id(&tab.tab_id), Some((1, 1)));
        assert!(!tab.focused);
        for (index, client) in [9, 10].into_iter().enumerate() {
            assert_eq!(server.clients[&client].shell_location, locations[index]);
            assert_eq!(
                server.clients[&client].shell_projection_revision,
                revisions[index]
            );
        }
        assert_eq!(
            server.shell_tab_id_for_client(9),
            Some(source_active_id.clone())
        );
        assert_eq!(
            server.shell_tab_id_for_client(10),
            Some(destination_active_id)
        );
        assert_eq!(
            server.shell_tab_id_for_client(11),
            Some(source_active_id.clone())
        );
        assert_eq!(
            server
                .app
                .public_tab_id(0, server.app.state.workspaces[0].active_tab),
            Some(source_active_id)
        );
        assert_eq!(server.app.state.active, Some(0));
        assert!(server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .recent_unwrapped_text(30)
            .contains("retained transfer output"));
        server.app.state.assert_invariants_for_test();
        shutdown_test_runtimes(&mut server);
    }
}

#[tokio::test]
async fn active_tab_transfer_with_unchanged_numeric_target_does_not_redirect_other_clients() {
    let mut server = server();
    server.app.state.workspaces[0].switch_tab(0);
    let source = server.app.public_tab_id(0, 0).unwrap();
    let remaining = server.app.public_tab_id(0, 1).unwrap();
    let destination = server.app.public_workspace_id(1);
    let destination_tab = server.app.public_tab_id(1, 0).unwrap();
    let (_source_control, _source_render) = connect_test_shell(&mut server, 9, 80, 23);
    let (_other_control, _other_render) = connect_test_shell(&mut server, 10, 100, 30);
    assert!(server.focus_shell_client_on_tab(10, &destination_tab));
    let mut other_location = server.clients[&10].shell_location.clone();
    let source_workspace = server.app.public_workspace_id(0);
    let other_revision = server.clients[&10].shell_projection_revision;
    let target_before = server.default_shell_target();
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: crate::api::schema::Request {
            id: "move-active-tab".into(),
            method: Method::TabMoveToWorkspace(TabMoveToWorkspaceParams {
                tab_id: source,
                workspace_id: destination,
            }),
        },
        respond_to,
        response_write_complete: None,
    });
    let response = response_rx.recv().unwrap();
    let _: SuccessResponse = serde_json::from_str(&response).expect(&response);
    assert_eq!(server.default_shell_target(), target_before);
    assert_eq!(server.shell_tab_id_for_client(9), Some(remaining.clone()));
    assert_eq!(server.shell_tab_id_for_client(10), Some(destination_tab));
    other_location
        .as_mut()
        .unwrap()
        .active_tab_ids
        .insert(source_workspace, remaining);
    assert_eq!(server.clients[&10].shell_location, other_location);
    assert_eq!(
        server.clients[&10].shell_projection_revision,
        other_revision
    );
    assert_eq!(server.app.state.active, Some(0));
    server.app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn tab_transfer_sends_focus_loss_to_the_moved_terminal_when_its_last_viewer_stays_behind() {
    let mut server = server();
    let moving = server.app.public_tab_id(0, 0).unwrap();
    let remaining = server.app.public_tab_id(0, 2).unwrap();
    let destination = server.app.public_workspace_id(1);
    let pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let terminal = server.app.state.workspaces[0].tabs[0]
        .terminal_id(pane)
        .unwrap()
        .clone();
    let (runtime, mut input) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            0,
            b"\x1b[?1004h",
            4,
        );
    server.app.terminal_runtimes.insert(terminal, runtime);
    let (_first_control, _first_render) = connect_test_shell(&mut server, 9, 80, 23);
    let (_second_control, _second_render) = connect_test_shell(&mut server, 10, 100, 30);
    server.clients.get_mut(&9).unwrap().outer_terminal_focus = Some(true);
    server.clients.get_mut(&10).unwrap().outer_terminal_focus = Some(true);
    assert!(server.focus_shell_client_on_tab(9, &remaining));
    assert!(server.focus_shell_client_on_tab(10, &moving));
    assert!(input.try_recv().is_err());
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_client_shell_api_request(
        9,
        crate::api::ApiRequestMessage {
            request: crate::api::schema::Request {
                id: "move-focused-terminal".into(),
                method: Method::TabMoveToWorkspace(TabMoveToWorkspaceParams {
                    tab_id: moving,
                    workspace_id: destination,
                }),
            },
            respond_to,
            response_write_complete: None,
        },
    );
    let response = response_rx.recv().unwrap();
    let _: SuccessResponse = serde_json::from_str(&response).expect(&response);
    assert_eq!(
        input
            .try_recv()
            .expect("moved terminal receives focus loss"),
        Bytes::from_static(b"\x1b[O")
    );
    assert!(input.try_recv().is_err());
    assert_eq!(server.shell_tab_id_for_client(9), Some(remaining.clone()));
    assert_eq!(server.shell_tab_id_for_client(10), Some(remaining));
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn tab_transfer_client_fences_cover_source_and_destination() {
    for fenced_index in [0, 1] {
        let mut server = server();
        let tab_id = server.app.public_tab_id(0, 0).unwrap();
        let workspace_id = server.app.public_workspace_id(1);
        let (_control, _render) = connect_test_shell(&mut server, 9, 80, 23);
        let request = crate::api::schema::Request {
            id: "move-tab".into(),
            method: Method::TabMoveToWorkspace(TabMoveToWorkspaceParams {
                tab_id,
                workspace_id,
            }),
        };
        let fenced = server.app.public_workspace_id(fenced_index);
        server.app.state.workspace_transfer_fences.insert(fenced);
        assert!(server.client_request_is_transfer_fenced(9, &request));
        let location = server.clients[&9].shell_location.clone();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        server.handle_client_shell_api_request(
            9,
            crate::api::ApiRequestMessage {
                request,
                respond_to,
                response_write_complete: None,
            },
        );
        let response: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response_rx.recv().unwrap()).unwrap();
        assert_eq!(response.error.code, "workspace_busy");
        assert_eq!(server.clients[&9].shell_location, location);
        assert_eq!(server.app.state.workspaces[0].tabs.len(), 3);
        assert_eq!(server.app.state.workspaces[1].tabs.len(), 1);
        server.app.state.assert_invariants_for_test();
    }
}
