use super::super::sessions::SessionPickerOverlay;
use super::*;
use crate::input::{TerminalKey, TextCommit};

fn shell() -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.initial_session_name = "source".to_owned();
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

fn session(name: &str, running: bool) -> crate::session::SessionInfo {
    crate::session::SessionInfo {
        name: name.to_owned(),
        default: name == "default",
        running,
        connection_error: None,
        socket_path: format!("/tmp/herdr-ui-test/{name}/client.sock"),
        session_dir: format!("/tmp/herdr-ui-test/{name}"),
    }
}

fn picker(state: &ClientShellState) -> &SessionPickerOverlay {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::SessionPicker(picker)) => picker,
        other => panic!("expected session picker, got {other:?}"),
    }
}

fn open(state: &mut ClientShellState) -> u64 {
    let mut outcome = ClientShellInput::default();
    state.open_session_picker(&mut outcome);
    assert!(outcome.requests.is_empty());
    let [ClientShellAction::SessionList {
        machine,
        request_id,
    }] = outcome.actions.as_slice()
    else {
        panic!("expected session list");
    };
    assert_eq!(machine, &state.active_endpoint_id.machine_id());
    *request_id
}

fn load(state: &mut ClientShellState) -> u64 {
    let request_id = open(state);
    let machine = state.active_endpoint_id.machine_id();
    assert!(state.receive_session_list(
        &machine,
        request_id,
        Ok(vec![
            session("stopped", false),
            session("source", true),
            session("default", true)
        ]),
    ));
    request_id
}

fn press(state: &mut ClientShellState, code: KeyCode) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        code,
        KeyModifiers::empty(),
    ))])
}

fn text(state: &mut ClientShellState, value: &str) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new(value.to_owned()))])
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn clear_name(state: &mut ClientShellState) {
    state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))]);
}

#[test]
fn session_selector_is_above_spaces_for_a_single_machine_without_changing_pane_geometry() {
    let mut state = shell();
    state.mode = ClientShellMode::Navigate;
    let layout = state.layout(106, 30);
    let active = state.active_endpoint_id.clone();
    let focus = state.snapshot.as_ref().unwrap().focused_pane_id.clone();
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame).join("\n").contains("Session: source ▾"));
    assert_eq!(state.hits.session_selector.y, layout.sidebar.y);
    assert!(!state.hits.session_selector.is_empty());
    assert!(state
        .hits
        .workspaces
        .iter()
        .all(|hit| hit.rect.y > state.hits.session_selector.y));
    assert!(state.hits.machines.is_empty());
    let selector = state.hits.session_selector;
    let outcome = click(&mut state, selector);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::SessionList {
            machine: ClientEndpointId::Local,
            ..
        }]
    ));
    assert!(outcome.requests.is_empty());
    assert_eq!(state.active_endpoint_id, active);
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(state.snapshot.as_ref().unwrap().focused_pane_id, focus);
    assert_eq!(state.layout(106, 30).pane_surface, layout.pane_surface);
}

#[test]
fn session_picker_rows_show_current_running_stopped_and_connection_errors() {
    let mut state = shell();
    let request_id = open(&mut state);
    let mut unavailable = session("broken", false);
    unavailable.connection_error = Some("permission denied".to_owned());
    assert!(state.receive_session_list(
        &ClientEndpointId::Local,
        request_id,
        Ok(vec![
            session("stopped", false),
            session("source", true),
            session("default", true),
            unavailable,
        ])
    ));
    let current = picker(&state);
    assert_eq!(current.entries[0].name, "default");
    assert_eq!(current.entries[current.selected].name, "source");
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    assert!(rows
        .iter()
        .any(|row| row.contains("source") && row.contains("current · running")));
    assert!(rows
        .iter()
        .any(|row| row.contains("stopped") && !row.contains("source")));
    assert!(rows
        .iter()
        .any(|row| row.contains("broken") && row.contains("unavailable")));
    let stopped_index = picker(&state)
        .entries
        .iter()
        .position(|entry| entry.name == "stopped")
        .unwrap();
    let stopped = state
        .hits
        .worktree_rows
        .iter()
        .find(|(_, index)| *index == stopped_index)
        .unwrap()
        .0;
    let outcome = click(&mut state, stopped);
    assert!(
        matches!(outcome.actions.as_slice(), [ClientShellAction::SessionOpen { machine: ClientEndpointId::Local, session, create: false, request_id: opened }] if session == "stopped" && *opened != request_id)
    );
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
    assert_eq!(
        state.snapshot.as_ref().unwrap().focused_pane_id.as_deref(),
        Some("pane_1")
    );
    assert!(outcome.requests.is_empty());
}

#[test]
fn current_session_selection_closes_without_opening_or_focusing() {
    let mut state = shell();
    load(&mut state);
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(state.overlay.is_none());
    assert!(outcome
        .actions
        .iter()
        .all(|action| matches!(action, ClientShellAction::SessionCancel { .. })));
    assert!(outcome.requests.is_empty());
    assert_eq!(state.endpoints[0].session_name, "source");
}

#[test]
fn new_session_form_validates_names_and_duplicates_before_creating() {
    let mut state = shell();
    let list_id = load(&mut state);
    press(&mut state, KeyCode::End);
    press(&mut state, KeyCode::Enter);
    assert!(picker(&state).new_name.is_some());
    for name in [
        "",
        "../bad",
        "two words",
        "日本語",
        "default",
        "source",
        "stopped",
        &"a".repeat(65),
    ] {
        clear_name(&mut state);
        text(&mut state, name);
        let outcome = press(&mut state, KeyCode::Enter);
        assert!(outcome.actions.is_empty(), "invalid name {name}");
        assert!(picker(&state).error.is_some(), "invalid name {name}");
    }
    clear_name(&mut state);
    text(&mut state, "feature-01");
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(
        matches!(outcome.actions.as_slice(), [ClientShellAction::SessionOpen { machine: ClientEndpointId::Local, session, create: true, request_id }] if session == "feature-01" && *request_id != list_id)
    );
    assert!(picker(&state).switching);
    assert_eq!(state.endpoints[0].session_name, "source");
    assert!(outcome.requests.is_empty());
}

#[test]
fn name_form_escape_returns_to_picker_then_picker_escape_cancels() {
    let mut state = shell();
    let request_id = load(&mut state);
    press(&mut state, KeyCode::Char('n'));
    text(&mut state, "unused");
    let back = press(&mut state, KeyCode::Esc);
    assert!(back.actions.is_empty());
    assert!(picker(&state).new_name.is_none());
    assert_eq!(picker(&state).request_id, request_id);
    let cancel = press(&mut state, KeyCode::Esc);
    assert!(
        matches!(cancel.actions.as_slice(), [ClientShellAction::SessionCancel { request_id: cancelled }] if *cancelled == request_id)
    );
    assert!(state.overlay.is_none());
    assert_eq!(state.endpoints[0].session_name, "source");
    assert!(!state.receive_session_list(&ClientEndpointId::Local, request_id, Ok(Vec::new())));
}

#[test]
fn list_failure_can_retry_but_cannot_create_and_old_results_are_fenced() {
    let mut state = shell();
    let first = open(&mut state);
    assert!(!state.receive_session_list(&ClientEndpointId::Local, first + 1, Ok(Vec::new())));
    assert!(state.receive_session_list(
        &ClientEndpointId::Local,
        first,
        Err("SSH unavailable".to_owned())
    ));
    press(&mut state, KeyCode::Char('n'));
    assert!(picker(&state).new_name.is_none());
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame).join("\n").contains("SSH unavailable"));
    let outcome = press(&mut state, KeyCode::Enter);
    let [ClientShellAction::SessionList {
        request_id: second, ..
    }] = outcome.actions.as_slice()
    else {
        panic!("expected retry list");
    };
    assert_ne!(*second, first);
    assert!(!state.receive_session_list(
        &ClientEndpointId::Local,
        first,
        Ok(vec![session("old", true)])
    ));
    assert!(state.receive_session_list(
        &ClientEndpointId::Local,
        *second,
        Ok(vec![session("source", true)])
    ));
    assert!(!state.receive_session_list(
        &ClientEndpointId::Local,
        *second,
        Err("duplicate completion".to_owned())
    ));
    assert_eq!(picker(&state).entries[0].name, "source");
}

#[test]
fn failed_create_preserves_source_and_retry_cancel_fences_late_completion() {
    let mut state = shell();
    load(&mut state);
    press(&mut state, KeyCode::Char('n'));
    text(&mut state, "new-one");
    press(&mut state, KeyCode::Enter);
    let failed_id = picker(&state).request_id;
    assert!(!state.finish_session_switch(failed_id + 1, Ok(())));
    assert!(state.finish_session_switch(failed_id, Err("start failed".to_owned())));
    assert!(!state.finish_session_switch(failed_id, Ok(())));
    assert_eq!(
        picker(&state).new_name.as_ref().unwrap().as_str(),
        "new-one"
    );
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame).join("\n").contains("start failed"));
    assert!(frame.cursor.is_some());
    assert_eq!(state.endpoints[0].session_name, "source");
    press(&mut state, KeyCode::Enter);
    let pending_id = picker(&state).request_id;
    assert_ne!(pending_id, failed_id);
    assert!(!state.finish_session_switch(failed_id, Ok(())));
    let cancel = press(&mut state, KeyCode::Esc);
    assert!(
        matches!(cancel.actions.as_slice(), [ClientShellAction::SessionCancel { request_id }] if *request_id == pending_id)
    );
    assert!(state.overlay.is_none());
    assert!(!state.finish_session_switch(pending_id, Ok(())));
    assert!(cancel.requests.is_empty());
}

#[test]
fn successful_switch_closes_only_its_own_pending_picker() {
    let mut state = shell();
    let list_id = load(&mut state);
    assert!(!state.finish_session_switch(list_id, Ok(())));
    press(&mut state, KeyCode::Down);
    press(&mut state, KeyCode::Enter);
    let switched_id = picker(&state).request_id;
    assert!(state.finish_session_switch(switched_id, Ok(())));
    assert!(state.overlay.is_none());
    let reopened_id = open(&mut state);
    assert_ne!(reopened_id, switched_id);
    assert!(!state.finish_session_switch(switched_id, Err("late error".to_owned())));
    assert_eq!(picker(&state).request_id, reopened_id);
    assert!(picker(&state).error.is_none());
}

#[test]
fn session_mouse_form_buttons_and_outside_click_do_not_navigate_panes() {
    let mut state = shell();
    load(&mut state);
    state.compose(106, 30).unwrap();
    let new_session = state.hits.overlay_primary;
    click(&mut state, new_session);
    state.compose(106, 30).unwrap();
    let input = state.hits.worktree_search;
    assert!(click(&mut state, input).actions.is_empty());
    assert!(picker(&state).new_name.is_some());
    assert!(click(&mut state, Rect::new(0, 0, 1, 1)).actions.is_empty());
    assert!(picker(&state).new_name.is_none());
    state.compose(106, 30).unwrap();
    let cancel = click(&mut state, Rect::new(0, 0, 1, 1));
    assert!(matches!(
        cancel.actions.as_slice(),
        [ClientShellAction::SessionCancel { .. }]
    ));
    assert!(state.overlay.is_none());
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
    assert!(cancel.requests.is_empty());
}

#[test]
fn global_menu_sessions_is_keyboard_accessible_without_a_wire_keybind_action() {
    let mut state = shell();
    state.mode = ClientShellMode::Prefix;
    let menu = press(&mut state, KeyCode::Char('m'));
    assert!(menu.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::GlobalMenu(_))
    ));
    let index = global_menu::global_menu_items(state.snapshot.as_deref())
        .iter()
        .position(|(_, action)| *action == global_menu::ClientGlobalMenuAction::Sessions)
        .unwrap();
    for _ in 0..index {
        press(&mut state, KeyCode::Down);
    }
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::SessionList { .. }]
    ));
    assert!(outcome.requests.is_empty());
    press(&mut state, KeyCode::Esc);
    press(&mut state, KeyCode::Menu);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::GlobalMenu(_))
    ));
}

#[test]
fn unavailable_selected_machine_still_exposes_its_session_and_picker() {
    let mut state = shell();
    let profile = SavedSshEndpoint {
        id: crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".to_owned(),
        target: "build".to_owned(),
        session: "remote-work".to_owned(),
        enabled: true,
    };
    let machine = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.active_endpoint_id = machine.clone();
    state.snapshot = None;
    state.pane_surface = None;
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame)
        .join("\n")
        .contains("Session: remote-work ▾"));
    let selector = state.hits.session_selector;
    let outcome = click(&mut state, selector);
    assert!(
        matches!(outcome.actions.as_slice(), [ClientShellAction::SessionList { machine: selected, .. }] if selected == &machine)
    );
    let request_id = picker(&state).request_id;
    assert!(!state.receive_session_list(&ClientEndpointId::Local, request_id, Ok(Vec::new())));
    assert!(state.receive_session_list(
        &machine,
        request_id,
        Ok(vec![
            session("remote-work", true),
            session("default", false)
        ])
    ));
    press(&mut state, KeyCode::Char('n'));
    let frame = state.compose(44, 12).unwrap();
    assert!(frame_rows(&frame).join("\n").contains("New session"));
    assert!(frame.cursor.is_some());
    assert_eq!(state.active_endpoint_id, machine);
}

#[test]
fn scoped_session_actions_use_canonical_machine_and_current_endpoint_label() {
    let mut state = shell();
    let scoped = ClientEndpointId::Local.with_session("scoped").unwrap();
    state.endpoints[0].endpoint_id = scoped.clone();
    state.endpoints[0].session_name = "scoped".to_owned();
    state.active_endpoint_id = scoped.clone();
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame).join("\n").contains("Session: scoped ▾"));
    assert!(state
        .hits
        .workspaces
        .iter()
        .all(|hit| hit.endpoint_id == scoped));
    let request_id = open(&mut state);
    assert_eq!(picker(&state).machine, ClientEndpointId::Local);
    assert!(state.receive_session_list(
        &ClientEndpointId::Local,
        request_id,
        Ok(vec![session("default", false)])
    ));
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(
        matches!(outcome.actions.as_slice(), [ClientShellAction::SessionOpen { machine: ClientEndpointId::Local, session, .. }] if session == "default")
    );
}

#[test]
fn hidden_session_candidates_are_not_machines_workspaces_or_agents() {
    let mut state = shell();
    let mut hidden = state.endpoints[0].clone();
    hidden.endpoint_id = ClientEndpointId::Local.with_session("candidate").unwrap();
    hidden.session_name = "candidate".to_owned();
    hidden.session_hidden = true;
    hidden.label = "Hidden candidate".to_owned();
    let snapshot = hidden.snapshot.as_mut().unwrap();
    snapshot.workspaces[0].label = "Hidden workspace".to_owned();
    snapshot.agents.push(ClientShellAgent {
        pane_id: "hidden-pane".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("hidden-agent".into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Blocked,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    });
    let hidden_id = hidden.endpoint_id.clone();
    state.endpoints.push(hidden);
    for (cols, collapsed) in [(106, false), (106, true), (44, false)] {
        state.sidebar_collapsed = collapsed;
        state.mode = ClientShellMode::Navigate;
        let frame = state.compose(cols, 30).unwrap();
        let text = frame_rows(&frame).join("\n");
        assert!(!text.contains("Hidden"));
        assert!(!text.contains("hidden-agent"));
        assert!(state.hits.machines.is_empty());
        assert!(state
            .hits
            .workspaces
            .iter()
            .all(|hit| hit.endpoint_id != hidden_id));
        assert!(state
            .hits
            .endpoint_agents
            .iter()
            .all(|(_, id, _)| id != &hidden_id));
        assert!(state.hits.mobile_targets.iter().all(|(_, target)| !matches!(target, ClientMobileTarget::Machine(id) | ClientMobileTarget::Workspace { endpoint_id: id, .. } | ClientMobileTarget::Agent { endpoint_id: id, .. } if id == &hidden_id)));
    }
}

#[test]
fn multiple_machine_sidebars_skip_hidden_session_candidates_in_both_modes() {
    let mut state = shell();
    let mut hidden = state.endpoints[0].clone();
    hidden.endpoint_id = ClientEndpointId::Local.with_session("candidate").unwrap();
    hidden.session_hidden = true;
    hidden.label = "Hidden candidate".to_owned();
    hidden.snapshot.as_mut().unwrap().workspaces[0].label = "Hidden workspace".to_owned();
    let hidden_id = hidden.endpoint_id.clone();
    state.endpoints.push(hidden);
    let mut remote = state.endpoints[0].clone();
    remote.endpoint_id = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    );
    remote.label = "Build".to_owned();
    remote.session_name = "remote".to_owned();
    state.endpoints.push(remote);
    for collapsed in [false, true] {
        state.sidebar_collapsed = collapsed;
        let frame = state.compose(106, 30).unwrap();
        assert!(!frame_rows(&frame).join("\n").contains("Hidden"));
        assert!(state
            .hits
            .machines
            .iter()
            .all(|hit| hit.endpoint_id != hidden_id));
        assert!(state
            .hits
            .workspaces
            .iter()
            .all(|hit| hit.endpoint_id != hidden_id));
        assert!(!state.hits.session_selector.is_empty());
    }
}

#[test]
fn session_picker_and_global_menu_own_keys_and_mouse_above_a_terminal_popup() {
    let mut state = shell();
    state.popup_terminal_id = Some("popup-source".into());
    let opened = press(&mut state, KeyCode::Menu);
    assert!(opened.requests.is_empty());
    state.compose(106, 30).unwrap();
    let index = global_menu::global_menu_items(state.snapshot.as_deref())
        .iter()
        .position(|(_, action)| *action == global_menu::ClientGlobalMenuAction::Sessions)
        .unwrap();
    for _ in 0..index {
        assert!(press(&mut state, KeyCode::Down).requests.is_empty());
    }
    let listed = press(&mut state, KeyCode::Enter);
    assert!(listed.requests.is_empty());
    assert!(matches!(
        listed.actions.as_slice(),
        [ClientShellAction::SessionList { .. }]
    ));
    assert!(press(&mut state, KeyCode::Esc).requests.is_empty());
    press(&mut state, KeyCode::Menu);
    state.compose(106, 30).unwrap();
    let sessions = state
        .hits
        .global_menu_rows
        .iter()
        .find(|(_, item)| *item == index)
        .unwrap()
        .0;
    let clicked = click(&mut state, sessions);
    assert!(clicked.requests.is_empty());
    assert!(matches!(
        clicked.actions.as_slice(),
        [ClientShellAction::SessionList { .. }]
    ));
    assert_eq!(state.popup_terminal_id.as_deref(), Some("popup-source"));
}

#[test]
fn new_session_form_owns_clipboard_text_and_blocks_images_above_popup() {
    let mut state = shell();
    load(&mut state);
    state.popup_terminal_id = Some("popup-source".into());
    state.popup_pending = true;
    press(&mut state, KeyCode::Char('n'));
    assert!(state.modal_paste_target_active());
    assert!(state.clipboard_image_target().is_none());
    let mut outcome = ClientShellInput::default();
    assert!(state.handle_modal_paste_shortcut_with(
        &TerminalKey::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        &mut outcome,
        || Some("clipboard".into()),
    ));
    assert!(outcome.requests.is_empty());
    assert_eq!(
        picker(&state).new_name.as_ref().unwrap().as_str(),
        "clipboard"
    );
    let pasted = state.handle_raw_events(vec![RawInputEvent::Paste("-paste".into())]);
    assert!(pasted.requests.is_empty());
    assert!(text(&mut state, "-text").requests.is_empty());
    assert_eq!(
        picker(&state).new_name.as_ref().unwrap().as_str(),
        "clipboard-paste-text"
    );
    let opened = press(&mut state, KeyCode::Enter);
    assert!(opened.requests.is_empty());
    assert!(matches!(
        opened.actions.as_slice(),
        [ClientShellAction::SessionOpen { create: true, .. }]
    ));
    assert!(!state.modal_paste_target_active());
    assert!(state.clipboard_image_target().is_none());
}

#[test]
fn short_global_menu_keeps_sessions_visible_when_keyboard_or_wheel_selects_it() {
    let mut state = shell();
    press(&mut state, KeyCode::Menu);
    let index = global_menu::global_menu_items(state.snapshot.as_deref())
        .iter()
        .position(|(_, action)| *action == global_menu::ClientGlobalMenuAction::Sessions)
        .unwrap();
    for _ in 0..index {
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        })]);
    }
    let frame = state.compose(106, 6).unwrap();
    assert!(frame_rows(&frame).join("\n").contains("sessions"));
    let sessions = state
        .hits
        .global_menu_rows
        .iter()
        .find(|(_, item)| *item == index)
        .unwrap()
        .0;
    let outcome = click(&mut state, sessions);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::SessionList { .. }]
    ));
    assert!(outcome.requests.is_empty());
}

#[test]
fn narrow_picker_preserves_recognizable_session_names_next_to_compact_statuses() {
    let mut state = shell();
    load(&mut state);
    let frame = state.compose(17, 20).unwrap();
    let rows = frame_rows(&frame);
    assert!(rows
        .iter()
        .any(|row| row.contains("source") && row.contains("run")));
    assert!(rows
        .iter()
        .any(|row| row.contains("default") && row.contains("run")));
}

#[test]
fn cancelled_running_session_switch_keeps_source_and_rejects_late_success() {
    let mut state = shell();
    load(&mut state);
    press(&mut state, KeyCode::Home);
    let open = press(&mut state, KeyCode::Enter);
    assert!(
        matches!(open.actions.as_slice(), [ClientShellAction::SessionOpen { session, create: false, .. }] if session == "default")
    );
    let request_id = picker(&state).request_id;
    state.compose(106, 30).unwrap();
    let cancelled = click(&mut state, Rect::new(0, 0, 1, 1));
    assert!(
        matches!(cancelled.actions.as_slice(), [ClientShellAction::SessionCancel { request_id: id }] if *id == request_id)
    );
    assert!(state.overlay.is_none());
    assert!(!state.finish_session_switch(request_id, Ok(())));
    assert_eq!(state.endpoints[0].session_name, "source");
    assert!(open.requests.is_empty() && cancelled.requests.is_empty());
}

#[test]
fn switching_to_the_name_form_invalidates_old_list_and_primary_hits() {
    let mut state = shell();
    load(&mut state);
    state.compose(106, 30).unwrap();
    let old_row = state.hits.worktree_rows[0];
    let old_primary = state.hits.overlay_primary;
    let outcome = state.handle_raw_events(vec![
        RawInputEvent::Key(TerminalKey::new(KeyCode::Char('n'), KeyModifiers::empty())),
        RawInputEvent::Text(TextCommit::new("new-valid")),
        RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: old_row.0.x,
            row: old_row.0.y,
            modifiers: KeyModifiers::empty(),
        }),
    ]);
    assert!(outcome.actions.is_empty());
    assert!(state.hits.worktree_rows.is_empty());
    assert!(state.hits.overlay_primary.is_empty());
    assert!(!picker(&state).switching);
    state.hits.worktree_rows.push(old_row);
    assert!(click(&mut state, old_row.0).actions.is_empty());
    assert!(click(&mut state, old_primary).actions.is_empty());
    assert_eq!(
        picker(&state).new_name.as_ref().unwrap().as_str(),
        "new-valid"
    );
    assert!(!picker(&state).switching);
}

#[test]
fn unavailable_global_menu_destinations_stay_disabled_in_new_contexts() {
    for no_snapshot in [false, true] {
        let mut state = shell();
        if no_snapshot {
            state.snapshot = None;
            state.pane_surface = None;
        } else {
            state.popup_terminal_id = Some("popup-source".into());
        }
        press(&mut state, KeyCode::Menu);
        for index in 0..2 {
            if let Some(ClientShellOverlay::GlobalMenu(menu)) = state.overlay.as_mut() {
                menu.highlighted = index;
            }
            let outcome = press(&mut state, KeyCode::Enter);
            assert!(outcome.actions.is_empty() && outcome.requests.is_empty());
            assert!(matches!(
                state.overlay,
                Some(ClientShellOverlay::GlobalMenu(_))
            ));
        }
        let frame = state.compose(106, 20).unwrap();
        let row = state.hits.global_menu_rows[0].0;
        let buffer = frame.to_ratatui_buffer().unwrap();
        assert!(buffer[(row.x, row.y)].modifier.contains(Modifier::DIM));
        let index = global_menu::global_menu_items(state.snapshot.as_deref())
            .iter()
            .position(|(_, action)| *action == global_menu::ClientGlobalMenuAction::Sessions)
            .unwrap();
        if let Some(ClientShellOverlay::GlobalMenu(menu)) = state.overlay.as_mut() {
            menu.highlighted = index;
        }
        let outcome = press(&mut state, KeyCode::Enter);
        assert!(matches!(
            outcome.actions.as_slice(),
            [ClientShellAction::SessionList { .. }]
        ));
        assert!(outcome.requests.is_empty());
    }
}

#[test]
fn opening_session_controls_does_not_lose_an_existing_source_mouse_release() {
    for use_picker in [false, true] {
        let mut state = shell();
        let mut projected = surface();
        projected.panes[0].mouse_reporting = true;
        state.set_pane_surface(projected);
        state.compose(106, 30).unwrap();
        let pane = state.hits.panes[0].inner_rect;
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.x + 1,
            row: pane.y + 1,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(state.pane_mouse_gesture.is_some());
        if use_picker {
            open(&mut state);
        } else {
            press(&mut state, KeyCode::Menu);
        }
        let released = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(state.pane_mouse_gesture.is_none());
        assert!(released.actions.is_empty());
        assert!(
            matches!(released.requests.as_slice(), [ClientMessage::ClientShellPaneInput { pane_id, events }] if pane_id == "pane_1" && matches!(events.as_slice(), [ClientPaneInputEvent::Mouse { kind: crate::protocol::ClientMouseKind::Up(_), .. }]))
        );
        assert!(state.overlay.is_some());
    }
}

#[test]
fn session_scoped_ssh_sidebar_reads_canonical_worktree_group_state() {
    let mut state = shell();
    let machine = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    );
    let scoped = machine.with_session("selected").unwrap();
    state.endpoints[0].endpoint_id = scoped.clone();
    state.active_endpoint_id = scoped.clone();
    state
        .endpoints
        .push(super::super::endpoints::local_endpoint());
    let mut projected = snapshot();
    projected.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    let mut child = projected.workspaces[0].clone();
    child.workspace_id = "child-workspace".into();
    child.label = "child".into();
    child.number = 2;
    child.focused = false;
    child.worktree.as_mut().unwrap().is_linked_worktree = true;
    projected.workspaces.push(child);
    state.set_snapshot(Box::new(projected));
    state.toggle_collapsed_group(&scoped, "repo".into());
    state.compose(106, 30).unwrap();
    assert!(!state
        .hits
        .workspaces
        .iter()
        .any(|hit| hit.workspace_id == "child-workspace"));
    state.toggle_collapsed_group(&scoped, "repo".into());
    state.compose(106, 30).unwrap();
    assert!(state
        .hits
        .workspaces
        .iter()
        .any(|hit| hit.workspace_id == "child-workspace"));
}

#[test]
fn compact_floating_selector_detects_only_intersecting_retained_rows_and_cursors() {
    let mut state = shell();
    state.sidebar_collapsed = true;
    state.config.sidebar_collapsed_mode = SidebarCollapsedModeConfig::Hidden;
    state.config.hide_tab_bar_when_single_tab = true;
    state.compose(106, 30).unwrap();
    let current = state.pane_surface.as_ref().unwrap();
    let mut patch = crate::protocol::PaneSurfacePatch {
        boot_id: current.boot_id.clone(),
        projection_revision: current.projection_revision,
        base_surface_revision: current.surface_revision,
        surface_revision: current.surface_revision + 1,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 0,
            y: 0,
            cells: current.frame.cells[..1].to_vec(),
        }],
        panes: current.panes.clone(),
        cursor: None,
    };
    assert!(state.session_selector_blocks_patch(&patch));
    patch.rows[0].y = 1;
    assert!(!state.session_selector_blocks_patch(&patch));
    patch.cursor = Some(crate::protocol::CursorState {
        x: 0,
        y: 0,
        visible: true,
        shape: 2,
    });
    assert!(state.session_selector_blocks_patch(&patch));
    patch.cursor.as_mut().unwrap().y = 1;
    assert!(!state.session_selector_blocks_patch(&patch));
    state.config.hide_tab_bar_when_single_tab = false;
    state.compose(106, 30).unwrap();
    patch.rows[0].y = 0;
    patch.cursor.as_mut().unwrap().y = 0;
    assert!(!state.session_selector_blocks_patch(&patch));
}

#[test]
fn provisional_session_activation_keeps_the_committed_selector_and_picker() {
    let mut state = shell();
    let target = ClientEndpointId::Local.with_session("candidate").unwrap();
    assert!(state.stage_session_endpoint(
        &ClientEndpointId::Local,
        target.clone(),
        "candidate".into()
    ));
    state.active_endpoint_id = target;
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame).join("\n").contains("Session: source ▾"));
    open(&mut state);
    assert_eq!(picker(&state).current, "source");
    assert_eq!(picker(&state).machine, ClientEndpointId::Local);
}

#[test]
fn stopped_or_unavailable_current_session_is_opened_instead_of_dismissed() {
    for (running, status) in [
        (false, ClientEndpointStatus::Online),
        (true, ClientEndpointStatus::Attention),
    ] {
        let mut state = shell();
        state.endpoints[0].status = status;
        let request_id = open(&mut state);
        assert!(state.receive_session_list(
            &ClientEndpointId::Local,
            request_id,
            Ok(vec![session("source", running)]),
        ));
        let outcome = press(&mut state, KeyCode::Enter);
        assert!(
            matches!(outcome.actions.as_slice(), [ClientShellAction::SessionOpen {
            machine: ClientEndpointId::Local, session, create: false, ..
        }] if session == "source")
        );
        assert!(picker(&state).switching);
        assert!(outcome.requests.is_empty());
    }
}

#[test]
fn source_popup_transitions_preserve_the_picker_and_its_request() {
    let mut state = shell();
    let request_id = open(&mut state);
    state.set_pane_surface(surface_with_popup());
    assert_eq!(picker(&state).request_id, request_id);
    state.set_pane_surface(surface());
    assert_eq!(picker(&state).request_id, request_id);
    assert!(state.receive_session_list(&ClientEndpointId::Local, request_id, Ok(Vec::new())));
}

#[test]
fn hidden_session_candidates_do_not_change_navigator_or_workspace_preview() {
    let mut state = shell();
    let mut hidden = state.endpoints[0].clone();
    hidden.endpoint_id = ClientEndpointId::Local.with_session("candidate").unwrap();
    hidden.session_hidden = true;
    hidden.snapshot.as_mut().unwrap().workspaces[0].label = "Hidden workspace".into();
    state.endpoints.push(hidden);
    let navigator = ClientNavigatorOverlay {
        query: TextEditor::default(),
        search_focused: false,
        selected: None,
        scroll: 0,
        filter: None,
    };
    let rows = super::super::aggregate_navigation::navigator_rows(
        &state.endpoints,
        &state.active_endpoint_id,
        &navigator,
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].depth, 0);
    assert!(rows.iter().all(|row| !row.label.contains("Hidden")));
    state.move_navigate_workspace(1);
    assert_eq!(
        state.navigate_workspace_id.as_ref().unwrap().endpoint_id,
        ClientEndpointId::Local
    );
}

#[test]
fn floating_session_selector_survives_retained_surface_updates() {
    let mut state = shell();
    state.sidebar_collapsed = true;
    state.config.sidebar_collapsed_mode = SidebarCollapsedModeConfig::Hidden;
    state.config.hide_tab_bar_when_single_tab = true;
    state.compose(106, 30).unwrap();
    let current = state.pane_surface.as_ref().unwrap();
    let mut patch = crate::protocol::PaneSurfacePatch {
        boot_id: current.boot_id.clone(),
        projection_revision: current.projection_revision,
        base_surface_revision: current.surface_revision,
        surface_revision: current.surface_revision + 1,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 0,
            y: 0,
            cells: current.frame.cells[..1].to_vec(),
        }],
        panes: current.panes.clone(),
        cursor: None,
    };
    patch.rows[0].cells[0].symbol = "X".into();
    assert!(matches!(
        state.apply_pane_surface_patch(patch.clone()),
        super::super::surface_patch::ClientPaneSurfacePatchOutcome::Applied(None),
    ));
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame).join("\n").contains("Session: source ▾"));
    patch.base_surface_revision += 1;
    patch.surface_revision += 1;
    patch.rows[0].y = 1;
    assert!(matches!(
        state.apply_pane_surface_patch(patch),
        super::super::surface_patch::ClientPaneSurfacePatchOutcome::Applied(Some(_)),
    ));
}

#[test]
fn offline_machine_context_opens_its_picker_without_changing_the_source() {
    for (cols, available) in [(106, true), (106, false), (44, true), (44, false)] {
        let mut state = shell();
        let profile = SavedSshEndpoint {
            id: crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef")
                .unwrap(),
            label: "Build".into(),
            target: "build".into(),
            session: "remote-work".into(),
            enabled: true,
        };
        let machine = ClientEndpointId::Ssh(profile.id.clone());
        state.set_endpoint_catalog(&[profile]);
        if !available {
            state.snapshot = None;
            state.pane_surface = None;
        }
        if cols == 44 {
            state.mode = ClientShellMode::Navigate;
        }
        state.compose(cols, 30).unwrap();
        let rect = state
            .hits
            .machines
            .iter()
            .find(|hit| hit.endpoint_id == machine)
            .map(|hit| hit.rect)
            .or_else(|| {
                state.hits.mobile_targets.iter().find_map(|(rect, target)| {
                    matches!(target, ClientMobileTarget::Machine(id) if id == &machine)
                        .then_some(*rect)
                })
            })
            .expect("offline machine header");
        let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ContextMenu(_))
        ));
        assert!(outcome.actions.is_empty());
        let frame = state.compose(cols, 30).unwrap();
        assert!(frame_rows(&frame).join("\n").contains("Sessions..."));
        let item = state.hits.context_menu_rows[0].0;
        let outcome = click(&mut state, item);
        assert!(
            matches!(outcome.actions.as_slice(), [ClientShellAction::SessionList { machine: id, .. }] if id == &machine)
        );
        assert_eq!(picker(&state).current, "remote-work");
        assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
        if available {
            assert_eq!(
                state.snapshot.as_ref().unwrap().focused_pane_id.as_deref(),
                Some("pane_1")
            );
        }
    }
}

#[test]
fn session_scoped_machine_headers_keep_canonical_collapse_preferences() {
    let mut state = shell();
    let scoped = ClientEndpointId::Local.with_session("scoped").unwrap();
    state.endpoints[0].endpoint_id = scoped.clone();
    state.endpoints[0].session_name = "scoped".into();
    state.active_endpoint_id = scoped.clone();
    let mut remote = state.endpoints[0].clone();
    remote.endpoint_id = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    );
    state.endpoints.push(remote);
    state.collapsed_endpoints.insert(ClientEndpointId::Local);
    for collapsed in [false, true] {
        state.sidebar_collapsed = collapsed;
        state.compose(106, 30).unwrap();
        assert!(state
            .hits
            .workspaces
            .iter()
            .all(|hit| hit.endpoint_id != scoped));
    }
    let rect = state
        .hits
        .machines
        .iter()
        .find(|hit| hit.endpoint_id == scoped)
        .unwrap()
        .collapse_toggle;
    click(&mut state, rect);
    assert!(!state.collapsed_endpoints.contains(&ClientEndpointId::Local));
}

#[test]
fn selector_clears_old_chrome_text_inside_its_hit_area() {
    let mut buffer = Buffer::with_lines(["XXXXXXXXXXXXXXXXXXXXXXXX"]);
    let mut hits = ShellHitMap::default();
    super::super::sessions::render_selector(
        &mut buffer,
        Rect::new(0, 0, 24, 1),
        "source",
        &ClientShellConfig::from_config(&Config::default()).palette,
        &mut hits,
    );
    assert_eq!(buffer[(23, 0)].symbol(), " ");
    assert_eq!(hits.session_selector, Rect::new(0, 0, 24, 1));
}

#[test]
fn session_controls_and_forms_clip_to_narrow_and_tiny_areas() {
    for (cols, rows) in [(1, 1), (2, 2), (4, 3), (9, 5), (17, 8), (44, 20), (106, 3)] {
        for unavailable in [false, true] {
            let mut state = shell();
            if unavailable {
                state.snapshot = None;
                state.pane_surface = None;
            }
            let bounds = Rect::new(0, 0, cols, rows);
            state.compose(cols, rows).unwrap();
            assert!(
                !state.hits.session_selector.is_empty(),
                "selector at {bounds:?}, unavailable {unavailable}"
            );
            assert_eq!(
                state.hits.session_selector.intersection(bounds),
                state.hits.session_selector
            );
            load(&mut state);
            for form in [false, true] {
                if form {
                    press(&mut state, KeyCode::Char('n'));
                }
                let frame = state.compose(cols, rows).unwrap();
                for rect in [
                    state.hits.navigator_popup,
                    state.hits.overlay_primary,
                    state.hits.overlay_clear,
                    state.hits.overlay_cancel,
                    state.hits.worktree_search,
                ] {
                    if !rect.is_empty() {
                        assert_eq!(rect.intersection(bounds), rect, "hit at {bounds:?}");
                    }
                }
                if let Some(cursor) = frame.cursor.as_ref() {
                    assert!(cursor.x < cols && cursor.y < rows);
                }
            }
            press(&mut state, KeyCode::Esc);
            assert!(picker(&state).new_name.is_none());
            press(&mut state, KeyCode::Esc);
            assert!(state.overlay.is_none());
        }
    }
}

#[test]
fn compact_hidden_sidebar_and_mobile_keep_the_selector_without_resizing_the_pane() {
    for hide_tabs in [false, true] {
        let mut state = shell();
        state.sidebar_collapsed = true;
        state.config.sidebar_collapsed_mode = SidebarCollapsedModeConfig::Hidden;
        state.config.hide_tab_bar_when_single_tab = hide_tabs;
        for (cols, rows) in [(106, 30), (44, 20)] {
            let before = state.surface_size(cols, rows);
            let frame = state.compose(cols, rows).unwrap();
            assert!(!state.hits.session_selector.is_empty());
            assert!(frame_rows(&frame).join("\n").contains("Session: source ▾"));
            assert_eq!(state.surface_size(cols, rows), before);
            let selector = state.hits.session_selector;
            assert!(matches!(
                click(&mut state, selector).actions.as_slice(),
                [ClientShellAction::SessionList { .. }]
            ));
            press(&mut state, KeyCode::Esc);
        }
    }
}
