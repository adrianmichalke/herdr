use super::*;

fn supported_shell() -> ClientShellState {
    let mut state = shell();
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 41, Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(Some(WORKSPACE_TRANSFER_METHODS.map(str::to_owned).into()));
    state
}

fn open_transfer(state: &mut ClientShellState, workspace_id: &str) -> u64 {
    let source = state.workspace_transfer_source(workspace_id).unwrap();
    let mut outcome = ClientShellInput::default();
    state.open_workspace_transfer_picker(source, &mut outcome);
    let [ClientShellAction::SessionList {
        request_id,
        machine,
    }] = outcome.actions.as_slice()
    else {
        panic!("transfer must reuse session discovery");
    };
    let request_id = *request_id;
    assert!(state.receive_session_list(
        machine,
        request_id,
        Ok(vec![session("source", true), session("destination", false)]),
    ));
    request_id
}

#[test]
fn transfer_picker_excludes_source_and_normal_picker_still_selects_current() {
    let mut state = supported_shell();
    open_transfer(&mut state, "ws_1");
    assert!(picker(&state).is_transfer());
    assert_eq!(picker(&state).entries.len(), 1);
    assert_eq!(picker(&state).entries[0].name, "destination");
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame).join("\n").contains("Move to session"));
    let cancel = press(&mut state, KeyCode::Esc);
    assert!(matches!(
        cancel.actions.as_slice(),
        [ClientShellAction::SessionCancel { .. }]
    ));
    assert!(state.overlay.is_none());
    assert_eq!(
        state
            .snapshot
            .as_ref()
            .unwrap()
            .focused_workspace_id
            .as_deref(),
        Some("ws_1")
    );
    load(&mut state);
    assert!(!picker(&state).is_transfer());
    assert_eq!(
        picker(&state).entries[picker(&state).selected].name,
        "source"
    );
    assert!(press(&mut state, KeyCode::Enter)
        .actions
        .iter()
        .all(|action| matches!(action, ClientShellAction::SessionCancel { .. })));
    assert!(state.overlay.is_none());
}

#[test]
fn transfer_new_session_validates_and_submits_without_opening_the_destination() {
    let mut state = supported_shell();
    open_transfer(&mut state, "ws_1");
    press(&mut state, KeyCode::Char('n'));
    for name in ["", "source", "destination", "../bad", "two words"] {
        clear_name(&mut state);
        text(&mut state, name);
        assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
        assert!(picker(&state).error.is_some());
    }
    clear_name(&mut state);
    text(&mut state, "brand-new");
    let outcome = press(&mut state, KeyCode::Enter);
    let [ClientShellAction::WorkspaceTransfer {
        source,
        destination_session,
        ..
    }] = outcome.actions.as_slice()
    else {
        panic!("new destination must be created by the transfer backend, not SessionOpen");
    };
    assert_eq!(source.workspace_id, "ws_1");
    assert_eq!(source.generation, 41);
    assert_eq!(source.boot_id, "boot-1");
    assert_eq!(destination_session, "brand-new");
    assert!(picker(&state).noncancellable());
    assert_eq!(state.endpoints[0].session_name, "source");
    assert_eq!(state.endpoints.len(), 1);
}

#[test]
fn transfer_cancels_before_submission_but_escape_outside_and_replacement_cannot_cancel_a_move() {
    let mut state = supported_shell();
    open_transfer(&mut state, "ws_1");
    press(&mut state, KeyCode::Char('n'));
    assert!(press(&mut state, KeyCode::Esc).actions.is_empty());
    assert!(picker(&state).new_name.is_none());
    assert!(matches!(
        press(&mut state, KeyCode::Esc).actions.as_slice(),
        [ClientShellAction::SessionCancel { .. }]
    ));
    open_transfer(&mut state, "ws_1");
    press(&mut state, KeyCode::Enter);
    let request_id = picker(&state).request_id;
    let frame = state.compose(106, 30).unwrap();
    let rendered = frame_rows(&frame).join("\n");
    assert!(rendered.contains("cannot cancel"));
    assert!(!rendered.contains("esc cancel"));
    assert!(state.hits.overlay_cancel.is_empty());
    assert!(press(&mut state, KeyCode::Esc).actions.is_empty());
    assert!(click(&mut state, Rect::new(0, 0, 1, 1)).actions.is_empty());
    let mut outcome = ClientShellInput::default();
    state.open_session_picker(&mut outcome);
    assert!(outcome.actions.is_empty());
    assert_eq!(picker(&state).request_id, request_id);
    assert!(picker(&state).noncancellable());
    assert_eq!(
        state.snapshot.as_ref().unwrap().focused_pane_id.as_deref(),
        Some("pane_1")
    );
}

#[test]
fn transfer_rejection_can_retry_but_committed_attachment_failure_cannot_resubmit() {
    let mut state = supported_shell();
    open_transfer(&mut state, "ws_1");
    press(&mut state, KeyCode::Enter);
    let first = picker(&state).request_id;
    assert!(state.finish_session_switch(first, Err("destination could not start".into())));
    assert!(!picker(&state).switching);
    assert!(!picker(&state).finished());
    assert_eq!(
        picker(&state).error.as_deref(),
        Some("destination could not start")
    );
    press(&mut state, KeyCode::Enter);
    let second = picker(&state).request_id;
    assert_ne!(first, second);
    state.workspace_transfer_following(second);
    assert!(state.finish_session_switch(second, Err("SSH disconnected".into())));
    assert!(picker(&state).finished());
    assert!(picker(&state)
        .error
        .as_ref()
        .unwrap()
        .contains("Workspace moved"));
    for code in [KeyCode::Enter, KeyCode::Char('r'), KeyCode::Char('n')] {
        assert!(press(&mut state, code).actions.is_empty());
    }
    assert!(!state.finish_session_switch(first, Ok(())));
    assert!(press(&mut state, KeyCode::Esc).actions.is_empty());
    assert!(state.overlay.is_none());
}

#[test]
fn transfer_is_offered_only_with_positive_support_for_both_methods_and_a_live_generation() {
    for methods in [
        None,
        Some(Vec::new()),
        Some(vec![WORKSPACE_TRANSFER_METHODS[0].into()]),
        Some(vec![WORKSPACE_TRANSFER_METHODS[1].into()]),
    ] {
        let mut state = supported_shell();
        state.set_endpoint_methods(methods);
        state.open_workspace_context_menu("ws_1".into(), 0, 0);
        let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
            panic!()
        };
        assert!(menu
            .items()
            .iter()
            .all(|item| item.action != ClientContextMenuAction::MoveToSession));
    }
    let mut state = supported_shell();
    state.set_snapshot(Box::new(snapshot()));
    assert!(state.workspace_transfer_source("ws_1").is_none());
}

#[test]
fn every_workspace_menu_variant_offers_transfer_without_focusing_a_background_workspace() {
    for variant in 0..4 {
        let mut state = supported_shell();
        let mut view = snapshot();
        let mut background = view.workspaces[0].clone();
        background.workspace_id = "ws_2".into();
        background.focused = false;
        background.number = 2;
        match variant {
            0 => background.branch = None,
            1 => {}
            2 | 3 => {
                background.worktree = Some(crate::protocol::ClientShellWorktree {
                    key: "repo".into(),
                    label: "repo".into(),
                    is_linked_worktree: variant == 2,
                })
            }
            _ => unreachable!(),
        }
        view.workspaces.push(background);
        if variant == 3 {
            let mut child = view.workspaces[1].clone();
            child.workspace_id = "ws_3".into();
            child.worktree.as_mut().unwrap().is_linked_worktree = true;
            view.workspaces.push(child);
        }
        state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 41, Box::new(view));
        state.compose(106, 30).unwrap();
        let rect = state
            .hits
            .workspaces
            .iter()
            .find(|hit| hit.workspace_id == "ws_2")
            .unwrap()
            .rect;
        let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(outcome.actions.is_empty());
        let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
            panic!()
        };
        let index = menu
            .items()
            .iter()
            .position(|item| item.label == "Move to session...")
            .unwrap();
        let mut outcome = ClientShellInput::default();
        state.activate_context_menu_item(index, &mut outcome);
        assert!(matches!(
            outcome.actions.as_slice(),
            [ClientShellAction::SessionList { .. }]
        ));
        assert_eq!(
            state
                .snapshot
                .as_ref()
                .unwrap()
                .focused_workspace_id
                .as_deref(),
            Some("ws_1")
        );
        let super::super::super::sessions::SessionPickerPurpose::WorkspaceTransfer {
            source, ..
        } = &picker(&state).purpose
        else {
            panic!()
        };
        assert_eq!(source.workspace_id, "ws_2");
        assert!(!source.was_active());
    }
}

#[test]
fn transfer_selection_is_fenced_against_source_boot_generation_and_focus_changes() {
    for change in 0..3 {
        let mut state = supported_shell();
        open_transfer(&mut state, "ws_1");
        let mut view = snapshot();
        let generation = if change == 0 { 42 } else { 41 };
        if change == 1 {
            view.boot_id = "boot-2".into();
        }
        if change == 2 {
            view.focused_pane_id = None;
        }
        state.set_endpoint_snapshot_for_generation(
            &ClientEndpointId::Local,
            generation,
            Box::new(view),
        );
        assert!(press(&mut state, KeyCode::Enter).actions.is_empty());
        assert!(!picker(&state).switching);
        assert!(picker(&state).error.as_ref().unwrap().contains("changed"));
    }
}
