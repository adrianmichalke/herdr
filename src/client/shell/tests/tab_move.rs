use super::*;
use crate::api::schema::{Method, Request, ResponseResult, TabInfo, TabTarget};
use crate::client::shell::tab_move::{TabMovePhase, TabMovePicker, TAB_MOVE_METHOD};
use crate::input::TerminalKey;

const SOURCE: &str = "ws_1:t1";
const BACKGROUND: &str = "ws_1:t2";
const MOVED: &str = "ws_2:t7";

fn view() -> ClientShellSnapshot {
    let mut view = snapshot();
    view.tabs[0].tab_id = SOURCE.into();
    view.workspaces[0].active_tab_id = SOURCE.into();
    view.focused_tab_id = Some(SOURCE.into());
    view.panes[0].tab_id = SOURCE.into();
    let mut tab = view.tabs[0].clone();
    tab.tab_id = BACKGROUND.into();
    tab.number = 2;
    tab.focused = false;
    view.tabs.push(tab);
    add_space(&mut view, "ws_2", 2);
    view
}

fn add_space(view: &mut ClientShellSnapshot, workspace_id: &str, number: usize) {
    let mut workspace = view.workspaces[0].clone();
    workspace.workspace_id = workspace_id.into();
    workspace.active_tab_id = format!("{workspace_id}:t1");
    workspace.number = number;
    workspace.label = "duplicate label".into();
    workspace.focused = false;
    let mut tab = view.tabs[0].clone();
    tab.tab_id = workspace.active_tab_id.clone();
    tab.workspace_id = workspace_id.into();
    tab.number = 1;
    tab.focused = false;
    view.workspaces.push(workspace);
    view.tabs.push(tab);
}

fn shell() -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.initial_session_name = "source".into();
    let mut state = ClientShellState::new(config);
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 41, Box::new(view()));
    state.set_endpoint_methods(Some(vec![TAB_MOVE_METHOD.into(), "tab.focus".into()]));
    refresh_surface(&mut state);
    state
}

fn refresh_surface(state: &mut ClientShellState) {
    let mut surface = surface();
    let snapshot = state.snapshot.as_ref().unwrap();
    surface.boot_id = snapshot.boot_id.clone();
    surface.projection_revision = snapshot.revision;
    state.set_pane_surface(surface);
}

fn update(state: &mut ClientShellState, mut view: ClientShellSnapshot) {
    view.revision = state.snapshot.as_ref().unwrap().revision + 1;
    let endpoint = state.active_endpoint_id.clone();
    state.set_endpoint_snapshot_for_generation(&endpoint, 41, Box::new(view));
}

fn picker(state: &ClientShellState) -> &TabMovePicker {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::TabMove(picker)) => picker,
        other => panic!("expected tab move picker, got {other:?}"),
    }
}

fn press(state: &mut ClientShellState, code: KeyCode) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        code,
        KeyModifiers::NONE,
    ))])
}

fn click(state: &mut ClientShellState, column: u16, row: u16) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })])
}

fn open(state: &mut ClientShellState, tab_id: &str) {
    state.open_tab_context_menu(tab_id.into(), 30, 1);
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        panic!();
    };
    assert_eq!(
        menu.items()
            .iter()
            .take(3)
            .map(|item| item.label)
            .collect::<Vec<_>>(),
        ["New tab", "Rename", "Close"]
    );
    let index = menu
        .items()
        .iter()
        .position(|item| item.action == ClientContextMenuAction::MoveToSpace)
        .unwrap();
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(
        outcome.actions.is_empty(),
        "opening a picker must never pre-focus its tab"
    );
    assert!(outcome.requests.is_empty());
    assert_eq!(picker(state).phase, TabMovePhase::Selecting);
}

fn request(outcome: &ClientShellInput) -> &Request {
    let [ClientShellAction::Endpoint { request, .. }] = outcome.actions.as_slice() else {
        panic!("expected one endpoint request");
    };
    assert!(outcome.requests.is_empty());
    request
}

fn submit(state: &mut ClientShellState, tab_id: &str) -> String {
    open(state, tab_id);
    let outcome = press(state, KeyCode::Enter);
    assert!(
        matches!(&request(&outcome).method, Method::TabMoveToWorkspace(params)
        if params.tab_id == tab_id && params.workspace_id == "ws_2")
    );
    assert_eq!(picker(state).phase, TabMovePhase::Moving);
    request(&outcome).id.clone()
}

fn moved_result() -> ResponseResult {
    ResponseResult::TabInfo {
        tab: TabInfo {
            tab_id: MOVED.into(),
            workspace_id: "ws_2".into(),
            number: 7,
            label: "live tab".into(),
            focused: false,
            pane_count: 1,
            agent_status: AgentStatus::Idle,
        },
    }
}

fn moved_view(state: &ClientShellState, source: &str) -> ClientShellSnapshot {
    let mut view = *state.snapshot.clone().unwrap();
    let tab = view
        .tabs
        .iter_mut()
        .find(|tab| tab.tab_id == source)
        .unwrap();
    tab.tab_id = MOVED.into();
    tab.workspace_id = "ws_2".into();
    tab.number = 7;
    tab.focused = false;
    if view.focused_tab_id.as_deref() == Some(source) {
        view.focused_tab_id = Some(BACKGROUND.into());
        view.focused_pane_id = Some("replacement-pane".into());
        view.workspaces[0].active_tab_id = BACKGROUND.into();
    }
    view
}

fn remote(state: &mut ClientShellState) -> ClientEndpointId {
    let profile = SavedSshEndpoint {
        id: crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "remote".into(),
        enabled: true,
    };
    let id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&id, ClientEndpointStatus::Online);
    let mut remote_view = view();
    remote_view.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot_for_generation(&id, 51, Box::new(remote_view));
    state.set_endpoint_methods_for(&id, Some(vec![TAB_MOVE_METHOD.into(), "tab.focus".into()]));
    id
}

#[test]
fn move_requires_positive_advertisement_and_a_live_generation() {
    for methods in [None, Some(vec![]), Some(vec!["tab.move".into()])] {
        let mut state = shell();
        state.set_endpoint_methods(methods);
        state.open_tab_context_menu(SOURCE.into(), 30, 1);
        let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
            panic!()
        };
        assert_eq!(
            menu.items()
                .iter()
                .map(|item| item.label)
                .collect::<Vec<_>>(),
            ["New tab", "Rename", "Close"]
        );
        assert!(state.tab_move_source(SOURCE).is_none());
    }
    for invalid in 0..4 {
        let mut state = shell();
        match invalid {
            0 => state.set_snapshot(Box::new(view())),
            1 => state
                .set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting),
            2 => state.endpoints[0].session_hidden = true,
            3 => state.active_snapshot_generation = Some(42),
            _ => unreachable!(),
        }
        assert!(state.tab_move_source(SOURCE).is_none());
    }
    let mut state = shell();
    state.set_endpoint_methods(Some(vec![TAB_MOVE_METHOD.into()]));
    assert!(state.tab_move_source(SOURCE).is_some());
}

#[test]
fn capability_and_source_are_rechecked_after_opening_context_menu() {
    for moved in [false, true] {
        let mut state = shell();
        state.open_tab_context_menu(SOURCE.into(), 30, 1);
        if moved {
            let view = moved_view(&state, SOURCE);
            update(&mut state, view);
        } else {
            state.set_endpoint_methods(Some(vec![]));
        }
        let mut outcome = ClientShellInput::default();
        state.activate_context_menu_item(3, &mut outcome);
        assert!(outcome.actions.is_empty());
        assert!(!matches!(
            state.overlay,
            Some(ClientShellOverlay::TabMove(_))
        ));
        assert!(state.visible_endpoint_notice.is_some());
    }
}

#[test]
fn picker_excludes_source_and_other_endpoint_or_session_spaces() {
    let mut state = shell();
    let remote_id = remote(&mut state);
    let mut remote_view = view();
    add_space(&mut remote_view, "remote-only", 3);
    state.set_endpoint_snapshot_for_generation(&remote_id, 51, Box::new(remote_view));
    let other = ClientEndpointId::Session {
        machine: Box::new(ClientEndpointId::Local),
        name: "other".into(),
    };
    assert!(state.stage_session_endpoint(&ClientEndpointId::Local, other.clone(), "other".into()));
    let mut other_view = view();
    add_space(&mut other_view, "other-session-only", 3);
    state.set_endpoint_status(&other, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot_for_generation(&other, 61, Box::new(other_view));
    open(&mut state, SOURCE);
    assert_eq!(
        picker(&state)
            .entries
            .iter()
            .map(|entry| entry.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["ws_2"]
    );
    assert_eq!(picker(&state).source.session, "source");
    assert_eq!(picker(&state).source.endpoint_id, ClientEndpointId::Local);
}

#[test]
fn duplicate_labels_and_reordered_snapshots_keep_selection_by_workspace_id() {
    let mut state = shell();
    let mut next = view();
    add_space(&mut next, "ws_3", 3);
    update(&mut state, next);
    open(&mut state, SOURCE);
    press(&mut state, KeyCode::Down);
    assert_eq!(picker(&state).selected.as_deref(), Some("ws_3"));
    let mut next = *state.snapshot.clone().unwrap();
    next.workspaces.swap(1, 2);
    update(&mut state, next);
    assert_eq!(picker(&state).selected.as_deref(), Some("ws_3"));
    refresh_surface(&mut state);
    let frame = state.compose(106, 30).unwrap();
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("ws_2") && text.contains("ws_3"));
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(
        matches!(&request(&outcome).method, Method::TabMoveToWorkspace(params) if params.workspace_id == "ws_3")
    );
}

#[test]
fn escape_and_outside_or_cancel_click_never_submit_or_change_focus() {
    for cancel in 0..3 {
        let mut state = shell();
        let before = state.snapshot.clone();
        open(&mut state, BACKGROUND);
        state.compose(106, 30).unwrap();
        let outcome = match cancel {
            0 => press(&mut state, KeyCode::Esc),
            1 => click(&mut state, 0, 0),
            _ => {
                let rect = state.hits.overlay_cancel;
                click(&mut state, rect.x, rect.y)
            }
        };
        assert!(outcome.actions.is_empty());
        assert!(outcome.requests.is_empty());
        assert!(state.overlay.is_none());
        assert_eq!(state.snapshot, before);
        assert!(state.pending_requests.is_empty());
    }
}

#[test]
fn no_destinations_is_explained_and_enter_is_a_noop() {
    let mut state = shell();
    let mut next = view();
    next.workspaces
        .retain(|workspace| workspace.workspace_id == "ws_1");
    next.tabs.retain(|tab| tab.workspace_id == "ws_1");
    update(&mut state, next);
    refresh_surface(&mut state);
    open(&mut state, SOURCE);
    assert!(picker(&state).entries.is_empty());
    assert!(picker(&state).selected.is_none());
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame)
        .join("\n")
        .contains("No other spaces in this session"));
    assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
    assert!(state.pending_requests.is_empty());
    assert!(press(&mut state, KeyCode::Esc).actions.is_empty());
    assert!(state.overlay.is_none());
}

#[test]
fn removed_destination_is_not_replaced_by_another_row_on_enter() {
    let mut state = shell();
    let mut next = view();
    add_space(&mut next, "ws_3", 3);
    update(&mut state, next);
    open(&mut state, SOURCE);
    let mut next = *state.snapshot.clone().unwrap();
    next.workspaces
        .retain(|workspace| workspace.workspace_id != "ws_2");
    update(&mut state, next);
    assert_eq!(picker(&state).selected.as_deref(), Some("ws_2"));
    assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
    assert!(state.pending_requests.is_empty());
    press(&mut state, KeyCode::Down);
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(
        matches!(&request(&outcome).method, Method::TabMoveToWorkspace(params) if params.workspace_id == "ws_3")
    );
}

#[test]
fn background_context_tab_is_never_prefocused_or_followed_after_index_shift() {
    let mut state = shell();
    let mut next = view();
    next.focused_tab_id = Some(BACKGROUND.into());
    next.workspaces[0].active_tab_id = BACKGROUND.into();
    update(&mut state, next);
    refresh_surface(&mut state);
    state.compose(106, 30).unwrap();
    let tab = state
        .hits
        .tabs
        .iter()
        .find(|(_, tab_id)| tab_id == SOURCE)
        .unwrap()
        .0;
    let opened = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: tab.x,
        row: tab.y,
        modifiers: KeyModifiers::NONE,
    })]);
    assert!(opened.actions.is_empty());
    let mut picked = ClientShellInput::default();
    state.activate_context_menu_item(3, &mut picked);
    assert!(picked.actions.is_empty());
    assert!(!picker(&state).source.was_active());
    let outcome = press(&mut state, KeyCode::Enter);
    let id = request(&outcome).id.clone();
    assert!(matches!(
        request(&outcome).method,
        Method::TabMoveToWorkspace(_)
    ));
    let next = moved_view(&state, SOURCE);
    update(&mut state, next);
    let (_, actions) = state.handle_endpoint_result("boot-1", &id, Ok(moved_result()));
    assert!(actions.is_empty());
    assert!(state.overlay.is_none());
    assert_eq!(
        state.snapshot.as_ref().unwrap().focused_tab_id.as_deref(),
        Some(BACKGROUND)
    );
    assert_eq!(
        state
            .snapshot
            .as_ref()
            .unwrap()
            .focused_workspace_id
            .as_deref(),
        Some("ws_1")
    );
}

#[test]
fn active_move_follows_only_returned_canonical_tab_and_retires_duplicate_results() {
    let mut state = shell();
    let id = submit(&mut state, SOURCE);
    let (repaint, actions) = state.handle_endpoint_result("boot-1", &id, Ok(moved_result()));
    assert!(repaint);
    let [ClientShellAction::Endpoint {
        endpoint_id,
        boot_id,
        request,
    }] = actions.as_slice()
    else {
        panic!()
    };
    assert_eq!(endpoint_id, &ClientEndpointId::Local);
    assert_eq!(boot_id, "boot-1");
    assert!(matches!(&request.method, Method::TabFocus(target) if target.tab_id == MOVED));
    assert_eq!(picker(&state).phase, TabMovePhase::Following);
    assert!(state
        .handle_endpoint_result("boot-1", &id, Ok(moved_result()))
        .1
        .is_empty());
    let follow_id = request.id.clone();
    assert!(state
        .handle_endpoint_result("boot-1", &follow_id, Ok(moved_result()))
        .1
        .is_empty());
    assert!(state.overlay.is_none());
    assert!(state
        .handle_endpoint_result("boot-1", &follow_id, Ok(moved_result()))
        .1
        .is_empty());
}

#[test]
fn active_follow_survives_source_tab_reconciliation_while_its_space_remains() {
    let mut state = shell();
    let id = submit(&mut state, SOURCE);
    let next = moved_view(&state, SOURCE);
    update(&mut state, next);
    assert!(!state
        .snapshot
        .as_ref()
        .unwrap()
        .tabs
        .iter()
        .any(|tab| tab.tab_id == SOURCE));
    assert!(state
        .snapshot
        .as_ref()
        .unwrap()
        .workspaces
        .iter()
        .any(|workspace| workspace.workspace_id == "ws_1"));
    let (_, actions) = state.handle_endpoint_result("boot-1", &id, Ok(moved_result()));
    assert!(
        matches!(actions.as_slice(), [ClientShellAction::Endpoint { request, .. }] if matches!(&request.method, Method::TabFocus(target) if target.tab_id == MOVED))
    );
}

#[test]
fn active_only_tab_follows_after_a_new_source_replacement_becomes_focused() {
    let mut state = shell();
    let mut next = view();
    next.tabs.retain(|tab| tab.tab_id != BACKGROUND);
    update(&mut state, next);
    let id = submit(&mut state, SOURCE);
    let mut replacement = state.snapshot.as_ref().unwrap().tabs[0].clone();
    replacement.tab_id = "ws_1:t3".into();
    replacement.number = 3;
    let mut next = moved_view(&state, SOURCE);
    next.workspaces[0].active_tab_id = replacement.tab_id.clone();
    next.focused_tab_id = Some(replacement.tab_id.clone());
    next.focused_pane_id = Some("new-shell-pane".into());
    next.tabs.push(replacement);
    update(&mut state, next);
    let (_, actions) = state.handle_endpoint_result("boot-1", &id, Ok(moved_result()));
    assert!(
        matches!(actions.as_slice(), [ClientShellAction::Endpoint { request, .. }]
        if matches!(&request.method, Method::TabFocus(target) if target.tab_id == MOVED))
    );
}

#[test]
fn user_navigation_away_and_back_suppresses_active_follow() {
    let mut state = shell();
    let id = submit(&mut state, SOURCE);
    for tab_id in [BACKGROUND, SOURCE] {
        state.push_endpoint_method(
            Method::TabFocus(TabTarget {
                tab_id: tab_id.into(),
            }),
            &mut ClientShellInput::default(),
        );
    }
    let (_, actions) = state.handle_endpoint_result("boot-1", &id, Ok(moved_result()));
    assert!(actions.is_empty());
    assert_eq!(picker(&state).phase, TabMovePhase::Finished);
    assert!(picker(&state)
        .error
        .as_ref()
        .unwrap()
        .contains("current view was kept"));
    assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
}

#[test]
fn source_moved_or_focus_changed_before_submission_never_sends_a_move() {
    for moved in [false, true] {
        let mut state = shell();
        open(&mut state, SOURCE);
        let mut next = if moved {
            moved_view(&state, SOURCE)
        } else {
            view()
        };
        next.focused_tab_id = Some(BACKGROUND.into());
        update(&mut state, next);
        assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
        assert_eq!(picker(&state).phase, TabMovePhase::Finished);
        assert!(state.pending_requests.is_empty());
    }
}

#[test]
fn source_focus_change_or_a_second_move_after_submission_suppresses_follow() {
    for moved_again in [false, true] {
        let mut state = shell();
        let id = submit(&mut state, SOURCE);
        let mut next = if moved_again {
            moved_view(&state, SOURCE)
        } else {
            view()
        };
        next.focused_tab_id = Some(BACKGROUND.into());
        if moved_again {
            next.tabs.retain(|tab| tab.tab_id != MOVED);
        }
        update(&mut state, next);
        assert!(state
            .handle_endpoint_result("boot-1", &id, Ok(moved_result()))
            .1
            .is_empty());
        assert_eq!(picker(&state).phase, TabMovePhase::Finished);
    }
}

#[test]
fn destination_removed_while_move_is_pending_does_not_follow() {
    let mut state = shell();
    let id = submit(&mut state, SOURCE);
    let mut next = view();
    next.workspaces
        .retain(|workspace| workspace.workspace_id != "ws_2");
    update(&mut state, next);
    assert!(state
        .handle_endpoint_result("boot-1", &id, Ok(moved_result()))
        .1
        .is_empty());
    assert!(picker(&state)
        .error
        .as_ref()
        .unwrap()
        .contains("destination space is no longer available"));
}

#[test]
fn stale_boot_generation_session_or_response_never_follows() {
    for change in 0..4 {
        let mut state = shell();
        let id = submit(&mut state, SOURCE);
        let response_boot = if change == 3 { "old-boot" } else { "boot-1" };
        match change {
            0 => state.set_endpoint_snapshot_for_generation(
                &ClientEndpointId::Local,
                42,
                Box::new(view()),
            ),
            1 => {
                let mut next = view();
                next.boot_id = "new-boot".into();
                update(&mut state, next);
            }
            2 => state.endpoints[0].session_name = "other".into(),
            _ => {}
        }
        assert!(state
            .handle_endpoint_result(response_boot, &id, Ok(moved_result()))
            .1
            .is_empty());
        assert_eq!(picker(&state).phase, TabMovePhase::Finished);
        assert!(picker(&state).error.as_ref().unwrap().contains("changed"));
        assert!(state
            .handle_endpoint_result("boot-1", &id, Ok(moved_result()))
            .1
            .is_empty());
    }
}

#[test]
fn disconnect_and_reconnect_retire_move_without_retry_and_leave_an_ambiguous_notice() {
    let mut state = shell();
    let id = submit(&mut state, SOURCE);
    state.mark_endpoint_disconnected(&ClientEndpointId::Local);
    assert!(state.pending_requests.is_empty());
    assert_eq!(picker(&state).phase, TabMovePhase::Finished);
    assert!(picker(&state)
        .error
        .as_ref()
        .unwrap()
        .contains("may have completed"));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 42, Box::new(view()));
    assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
    assert!(state
        .handle_endpoint_result("boot-1", &id, Ok(moved_result()))
        .1
        .is_empty());
    assert!(picker(&state)
        .error
        .as_ref()
        .unwrap()
        .contains("may have completed"));
}

#[test]
fn endpoint_switch_away_and_back_rejects_late_success_without_touching_other_endpoint() {
    let mut state = shell();
    let other = remote(&mut state);
    let before = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == other)
        .unwrap()
        .snapshot
        .clone();
    let id = submit(&mut state, SOURCE);
    assert!(state.activate_endpoint_projection(&other));
    assert!(state.activate_endpoint_projection(&ClientEndpointId::Local));
    assert!(state
        .handle_endpoint_result("boot-1", &id, Ok(moved_result()))
        .1
        .is_empty());
    assert!(state.overlay.is_none());
    assert_eq!(
        state
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == other)
            .unwrap()
            .snapshot,
        before
    );
}

#[test]
fn rejected_cancelled_and_timed_out_moves_are_visible_and_never_automatically_retried() {
    for code in [
        "stale_target",
        "tab_not_found",
        "workspace_not_found",
        "unknown_method",
        "endpoint_cancelled",
        "endpoint_timeout",
        "server_unavailable",
    ] {
        let mut state = shell();
        let id = submit(&mut state, SOURCE);
        let error = ClientShellEndpointError {
            code: Some(code.into()),
            message: format!("rejected {code}"),
        };
        let (repaint, actions) = state.handle_endpoint_result("boot-1", &id, Err(error));
        assert!(repaint);
        assert!(actions.is_empty());
        let message = picker(&state).error.clone().unwrap();
        if matches!(
            code,
            "endpoint_cancelled" | "endpoint_timeout" | "server_unavailable"
        ) {
            assert!(
                message.contains("may have completed") && message.contains("Check both spaces")
            );
        } else {
            assert!(message.contains(code));
        }
        assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
        assert!(press(&mut state, KeyCode::Char('x')).actions.is_empty());
        assert_eq!(picker(&state).error.as_ref(), Some(&message));
        assert!(state
            .handle_endpoint_result("boot-1", &id, Ok(moved_result()))
            .1
            .is_empty());
        assert_eq!(picker(&state).phase, TabMovePhase::Finished);
        press(&mut state, KeyCode::Esc);
        assert!(state.overlay.is_none());
    }
}

#[test]
fn unreadable_or_invalid_response_errors_leave_the_move_outcome_unknown() {
    for code in [None, Some("invalid_response".into())] {
        let mut state = shell();
        let id = submit(&mut state, SOURCE);
        let (_, actions) = state.handle_endpoint_result(
            "boot-1",
            &id,
            Err(ClientShellEndpointError {
                code,
                message: "response could not be decoded".into(),
            }),
        );
        assert!(actions.is_empty());
        let message = picker(&state).error.as_ref().unwrap();
        assert!(message.contains("outcome unknown") && message.contains("may have completed"));
        assert!(!message.contains("rejected"));
        assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
        assert!(state
            .handle_endpoint_result("boot-1", &id, Ok(moved_result()))
            .1
            .is_empty());
    }
}

#[test]
fn invalid_typed_success_never_follows_or_claims_move_failed() {
    for invalid in 0..7 {
        let mut state = shell();
        let id = submit(&mut state, SOURCE);
        let ResponseResult::TabInfo { mut tab } = moved_result() else {
            panic!()
        };
        match invalid {
            0 => tab.workspace_id = "ws_3".into(),
            1 => tab.tab_id.clear(),
            2 => tab.number = 0,
            3 => tab.pane_count = 0,
            4 => tab.tab_id = SOURCE.into(),
            5 => tab.number = 8,
            _ => {}
        }
        let result = if invalid == 6 {
            ResponseResult::Ok {}
        } else {
            ResponseResult::TabInfo { tab }
        };
        assert!(state
            .handle_endpoint_result("boot-1", &id, Ok(result))
            .1
            .is_empty());
        assert!(picker(&state)
            .error
            .as_ref()
            .unwrap()
            .contains("invalid move result"));
        assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
    }
}

#[test]
fn follow_failure_is_distinguished_from_move_failure() {
    for invalid_success in [false, true] {
        let mut state = shell();
        let id = submit(&mut state, SOURCE);
        let (_, actions) = state.handle_endpoint_result("boot-1", &id, Ok(moved_result()));
        let [ClientShellAction::Endpoint { request, .. }] = actions.as_slice() else {
            panic!()
        };
        let result = if invalid_success {
            Ok(ResponseResult::Ok {})
        } else {
            Err(ClientShellEndpointError {
                code: Some("endpoint_timeout".into()),
                message: "focus timed out".into(),
            })
        };
        assert!(state
            .handle_endpoint_result("boot-1", &request.id, result)
            .1
            .is_empty());
        assert!(picker(&state)
            .error
            .as_ref()
            .unwrap()
            .starts_with("Tab moved, but"));
        assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
    }
}

#[test]
fn reconnect_during_follow_keeps_the_confirmed_move_outcome() {
    let mut state = shell();
    let id = submit(&mut state, SOURCE);
    let (_, actions) = state.handle_endpoint_result("boot-1", &id, Ok(moved_result()));
    let [ClientShellAction::Endpoint { request, .. }] = actions.as_slice() else {
        panic!()
    };
    state.mark_endpoint_disconnected(&ClientEndpointId::Local);
    assert_eq!(picker(&state).phase, TabMovePhase::Finished);
    let message = picker(&state).error.as_ref().unwrap();
    assert!(message.starts_with("Tab moved, but"));
    assert!(!message.contains("may have completed"));
    assert!(state
        .handle_endpoint_result("boot-1", &request.id, Ok(moved_result()))
        .1
        .is_empty());
}

#[test]
fn submitted_move_is_not_cancellable_and_enter_cannot_submit_it_twice() {
    let mut state = shell();
    let id = submit(&mut state, SOURCE);
    state.compose(106, 30).unwrap();
    for code in [KeyCode::Enter, KeyCode::Esc, KeyCode::Down] {
        assert!(press(&mut state, code).actions.is_empty());
        assert_eq!(picker(&state).phase, TabMovePhase::Moving);
    }
    assert!(click(&mut state, 0, 0).actions.is_empty());
    assert_eq!(picker(&state).request_id.as_deref(), Some(id.as_str()));
    assert_eq!(state.pending_requests.len(), 1);
}

#[test]
fn remote_move_follows_on_its_own_endpoint_without_switching_sessions() {
    let mut state = shell();
    let remote = remote(&mut state);
    let local = state.endpoints[0].snapshot.clone();
    assert!(state.activate_endpoint_projection(&remote));
    refresh_surface(&mut state);
    let id = submit(&mut state, SOURCE);
    let (_, actions) = state.handle_endpoint_result("remote-boot", &id, Ok(moved_result()));
    assert!(
        matches!(actions.as_slice(), [ClientShellAction::Endpoint { endpoint_id, request, .. }] if endpoint_id == &remote && matches!(&request.method, Method::TabFocus(target) if target.tab_id == MOVED))
    );
    assert_eq!(state.active_endpoint_id, remote);
    assert_eq!(state.endpoints[0].snapshot, local);
    assert_eq!(picker(&state).source.session, "remote");
}

#[test]
fn compact_picker_keeps_confirmed_and_uncertain_outcomes_visible() {
    for case in ["timeout", "rejected", "follow", "not-submitted"] {
        let mut state = shell();
        let expected = if case == "not-submitted" {
            open(&mut state, SOURCE);
            state.note_session_navigation();
            assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
            "notsubmitted"
        } else {
            let id = submit(&mut state, SOURCE);
            if case == "follow" {
                let (_, actions) = state.handle_endpoint_result("boot-1", &id, Ok(moved_result()));
                let follow_id = request(&ClientShellInput {
                    actions,
                    ..ClientShellInput::default()
                })
                .id
                .clone();
                state.handle_endpoint_result(
                    "boot-1",
                    &follow_id,
                    Err(ClientShellEndpointError {
                        code: Some("endpoint_timeout".into()),
                        message: "focus timed out".into(),
                    }),
                );
                "tabmoved"
            } else {
                state.handle_endpoint_result(
                    "boot-1",
                    &id,
                    Err(ClientShellEndpointError {
                        code: Some(
                            if case == "timeout" {
                                "endpoint_timeout"
                            } else {
                                "workspace_busy"
                            }
                            .into(),
                        ),
                        message: "test rejection".into(),
                    }),
                );
                if case == "timeout" {
                    "outcomeunknown"
                } else {
                    "moverejected"
                }
            }
        };
        assert_eq!(picker(&state).phase, TabMovePhase::Finished);
        for (cols, rows) in [(16, 6), (80, 6)] {
            let frame = state.compose(cols, rows).unwrap();
            let buffer = frame.to_ratatui_buffer().unwrap();
            let area = state.hits.navigator_popup;
            let mut visible = String::new();
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    visible.extend(
                        buffer[(x, y)]
                            .symbol()
                            .chars()
                            .filter(char::is_ascii_alphabetic),
                    );
                }
            }
            assert!(
                visible.to_ascii_lowercase().contains(expected),
                "{case} {cols}x{rows}: {visible}"
            );
        }
    }
}

#[test]
fn picker_scroll_and_mouse_use_ids_and_render_at_small_geometries() {
    let mut state = shell();
    let mut next = view();
    for number in 3..=20 {
        add_space(&mut next, &format!("ws_{number}"), number);
    }
    update(&mut state, next);
    refresh_surface(&mut state);
    open(&mut state, SOURCE);
    for (cols, rows) in [(1, 1), (3, 2), (16, 6), (40, 10), (40, 14)] {
        assert!(state.compose(cols, rows).is_some());
        assert!(state.hits.tab_move_rows.iter().any(|(_, id)| id == "ws_2"));
    }
    press(&mut state, KeyCode::End);
    state.compose(40, 14).unwrap();
    assert_eq!(picker(&state).selected.as_deref(), Some("ws_20"));
    let rect = state
        .hits
        .tab_move_rows
        .iter()
        .find(|(_, id)| id == "ws_20")
        .unwrap()
        .0;
    let outcome = click(&mut state, rect.x, rect.y);
    assert!(
        matches!(&request(&outcome).method, Method::TabMoveToWorkspace(params) if params.workspace_id == "ws_20")
    );
}
