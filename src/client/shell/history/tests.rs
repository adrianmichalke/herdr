use super::*;
use crate::client::shell::tests::{snapshot, surface, surface_with_popup};
use crate::input::{TerminalKey, TextCommit};
use crate::pi_history::types::{HistoryEntry, HistoryMatch};
use crossterm::event::{KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn shell() -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.initial_session_name = "source".into();
    let mut shell = ClientShellState::new(config);
    shell.set_snapshot(Box::new(snapshot()));
    shell.set_pane_surface(surface());
    shell
}

fn history(shell: &ClientShellState) -> &PiHistoryOverlay {
    match shell.overlay.as_ref() {
        Some(ClientShellOverlay::PiHistory(history)) => history,
        other => panic!("expected history, got {other:?}"),
    }
}

fn key(shell: &mut ClientShellState, code: KeyCode, modifiers: KeyModifiers) -> ClientShellInput {
    shell.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(code, modifiers))])
}

fn press(shell: &mut ClientShellState, code: KeyCode) -> ClientShellInput {
    key(shell, code, KeyModifiers::empty())
}

fn open(shell: &mut ClientShellState) -> u64 {
    let outcome = key(shell, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(outcome.requests.is_empty());
    let [ClientShellAction::History {
        request_id,
        request,
        ..
    }] = outcome.actions.as_slice()
    else {
        panic!("expected search action");
    };
    assert_eq!(request.scope, HistoryScope::Space);
    assert_eq!(request.action, HistoryAction::Search);
    *request_id
}

fn location(running: bool) -> HistoryLocation {
    HistoryLocation {
        session: if running {
            "source".into()
        } else {
            "stopped".into()
        },
        workspace_id: "ws_native".into(),
        workspace_label: "Native project".into(),
        tab_id: "tab_native".into(),
        tab_label: "Agent tab".into(),
        pane_id: "pane_native".into(),
        pane_label: Some("Research".into()),
        terminal_id: Some("terminal_native".into()),
        running,
        agent_session: crate::api::schema::AgentSessionInfo {
            source: "pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: "/synthetic/pi.jsonl".into(),
        },
    }
}

fn result() -> HistoryResult {
    HistoryResult {
        source: HistorySource {
            id: "source-one".into(),
            name: "Synthetic conversation".into(),
            locations: vec![location(true), location(false)],
        },
        matches: vec![
            HistoryMatch {
                entry_line: 4,
                role: "user".into(),
                excerpt: "First synthetic needle".into(),
            },
            HistoryMatch {
                entry_line: 14,
                role: "assistant".into(),
                excerpt: "Second synthetic needle".into(),
            },
        ],
        truncated: false,
    }
}

fn preview() -> HistoryPreview {
    HistoryPreview {
        source: result().source,
        entries: vec![
            HistoryEntry {
                entry_line: 3,
                role: "user".into(),
                text: "Before the match\n".repeat(8),
                truncated: false,
            },
            HistoryEntry {
                entry_line: 4,
                role: "assistant".into(),
                text: "Matched 界 e\u{301} 🙂\nRead-only target\n".into(),
                truncated: false,
            },
            HistoryEntry {
                entry_line: 5,
                role: "tool".into(),
                text: "After the match\n".repeat(20),
                truncated: true,
            },
        ],
        matched_entry: 1,
        previous: Some(1),
        next: Some(10),
        truncated: true,
    }
}

fn load(shell: &mut ClientShellState) -> u64 {
    let request_id = open(shell);
    let machine = history(shell).machine.clone();
    assert!(shell.receive_history_event(
        &machine,
        request_id,
        HistoryEvent::Result { result: result() }
    ));
    request_id
}

fn start_preview(shell: &mut ClientShellState) -> u64 {
    let outcome = press(shell, KeyCode::Enter);
    assert!(outcome.requests.is_empty());
    let [ClientShellAction::History {
        request_id,
        request,
        ..
    }] = outcome.actions.as_slice()
    else {
        panic!("expected preview only");
    };
    assert_eq!(
        request.action,
        HistoryAction::Preview {
            source_id: "source-one".into(),
            anchor: 4
        }
    );
    *request_id
}

fn show_preview(shell: &mut ClientShellState) -> u64 {
    load(shell);
    let request_id = start_preview(shell);
    let machine = history(shell).machine.clone();
    assert!(shell.receive_history_event(
        &machine,
        request_id,
        HistoryEvent::Preview { preview: preview() }
    ));
    assert!(shell.receive_history_event(
        &machine,
        request_id,
        HistoryEvent::Done { truncated: false }
    ));
    request_id
}

fn frame_text(frame: &FrameData) -> String {
    frame
        .cells
        .chunks(usize::from(frame.width).max(1))
        .map(|row| {
            let mut text = String::new();
            let mut column = 0;
            while let Some(cell) = row.get(column) {
                text.push_str(&cell.symbol);
                column += unicode_width::UnicodeWidthStr::width(cell.symbol.as_str()).max(1);
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn mouse(
    shell: &mut ClientShellState,
    kind: MouseEventKind,
    point: (u16, u16),
) -> ClientShellInput {
    shell.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind,
        column: point.0,
        row: point.1,
        modifiers: KeyModifiers::empty(),
    })])
}

#[test]
fn main_view_history_entry_respects_modes_and_consumed_key_leases() {
    for mode in [
        ClientShellMode::Terminal,
        ClientShellMode::Navigate,
        ClientShellMode::Resize,
    ] {
        let mut shell = shell();
        shell.mode = mode;
        shell.set_host_reports_key_releases(true);
        let id = open(&mut shell);
        assert_eq!(history(&shell).session, "source");
        assert_eq!(history(&shell).workspace_id.as_deref(), Some("ws_1"));
        assert!(shell.history_request_is_current(&ClientEndpointId::Local, id));
        let repeated = shell.handle_raw_events(vec![RawInputEvent::Key(
            TerminalKey::new(KeyCode::Char('f'), KeyModifiers::CONTROL)
                .with_kind(KeyEventKind::Repeat),
        )]);
        assert!(repeated.requests.is_empty());
        assert!(repeated.actions.is_empty());
        press(&mut shell, KeyCode::Esc);
        for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
            let outcome = shell.handle_raw_events(vec![RawInputEvent::Key(
                TerminalKey::new(KeyCode::Char('f'), KeyModifiers::CONTROL).with_kind(kind),
            )]);
            assert!(outcome.requests.is_empty());
            assert!(outcome.actions.is_empty());
            assert!(shell.overlay.is_none());
        }
    }
}

#[test]
fn history_keeps_popup_dialog_and_copy_mode_shortcut_ownership() {
    let mut shell = shell();
    shell.set_pane_surface(surface_with_popup());
    let outcome = key(&mut shell, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(outcome.actions.is_empty());
    assert!(outcome
        .requests
        .iter()
        .any(|request| matches!(request, ClientMessage::ClientShellPopupInput { .. })));
    assert!(shell.overlay.is_none());
    shell.set_pane_surface(surface());
    shell.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
        title: "rename",
        input: TextEditor::new("界🙂", false),
        target: ClientRenameTarget::Pane {
            pane_id: "pane_1".into(),
        },
    }));
    let outcome = key(&mut shell, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(outcome.actions.is_empty());
    assert!(matches!(shell.overlay, Some(ClientShellOverlay::Rename(_))));
    shell.overlay = None;
    shell.compose(106, 30).unwrap();
    shell.mode = ClientShellMode::Copy;
    shell.copy_mode = Some(ClientCopyModeState {
        pane_id: "pane_1".into(),
        content_revision: 0,
        geometry: (4, 2),
        alternate_screen_active: false,
        cursor: crate::api::schema::PaneTextPoint { row: 30, col: 0 },
        offset_from_bottom: 30,
        max_offset_from_bottom: 100,
        entry_offset_from_bottom: 0,
        selection: None,
        search_prompt: None,
        search_query: String::new(),
        search_direction: None,
        search_matches: Vec::new(),
        search_total: 0,
        search_current: None,
        search_current_global: None,
        search_generation: 0,
        copy_after_search: false,
    });
    let outcome = key(&mut shell, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(shell.overlay.is_none());
    assert!(!outcome
        .actions
        .iter()
        .any(|action| matches!(action, ClientShellAction::History { .. })));
    assert!(shell.copy_mode.as_ref().unwrap().offset_from_bottom < 30);
}

#[test]
fn scopes_capture_selected_remote_committed_session_and_navigation_space() {
    let mut shell = shell();
    let machine = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    );
    let mut remote = shell.endpoints[0].clone();
    remote.endpoint_id = machine.with_session("committed").unwrap();
    remote.session_name = "committed".into();
    remote.label = "Synthetic remote".into();
    remote.snapshot.as_mut().unwrap().workspaces[0].workspace_id = "remote_space".into();
    remote.snapshot.as_mut().unwrap().focused_workspace_id = Some("remote_space".into());
    let remote_id = remote.endpoint_id.clone();
    shell.endpoints.push(remote.clone());
    remote.endpoint_id = machine.with_session("hidden-candidate").unwrap();
    remote.session_name = "hidden-candidate".into();
    remote.session_hidden = true;
    shell.endpoints.push(remote);
    shell.navigate_workspace_id = shell.navigation_target(&remote_id, "remote_space");
    shell.mode = ClientShellMode::Navigate;
    let first = open(&mut shell);
    assert_eq!(history(&shell).machine, machine);
    assert_eq!(history(&shell).session, "committed");
    assert_eq!(
        history(&shell).workspace_id.as_deref(),
        Some("remote_space")
    );
    for (digit, scope) in [
        ('2', HistoryScope::Session),
        ('3', HistoryScope::All),
        ('1', HistoryScope::Space),
    ] {
        let outcome = key(&mut shell, KeyCode::Char(digit), KeyModifiers::CONTROL);
        assert!(outcome.requests.is_empty());
        let ClientShellAction::History {
            machine: request_machine,
            request,
            ..
        } = outcome.actions.last().unwrap()
        else {
            panic!("expected search");
        };
        assert_eq!(request_machine, &machine);
        assert_eq!(request.scope, scope);
        assert_eq!(request.session, "committed");
        assert_eq!(request.workspace_id.as_deref(), Some("remote_space"));
    }
    assert!(!shell.history_request_is_current(&machine, first));
    assert_eq!(shell.active_endpoint_id, ClientEndpointId::Local);
}

#[test]
fn history_text_ime_paste_and_clipboard_own_input_above_popup_without_ascii_forcing() {
    let mut shell = shell();
    shell.config.switch_ascii_input_source_in_prefix = true;
    shell.mode = ClientShellMode::Navigate;
    shell.reconcile_input_source();
    assert_eq!(shell.take_input_source_changes(), vec![true]);
    open(&mut shell);
    assert_eq!(shell.take_input_source_changes(), vec![false]);
    shell.set_pane_surface(surface_with_popup());
    for event in [
        RawInputEvent::Text(TextCommit::new("界e\u{301}🙂")),
        RawInputEvent::Paste("\r\nß\t".into()),
    ] {
        let outcome = shell.handle_raw_events(vec![event]);
        assert!(outcome.requests.is_empty());
        assert!(outcome
            .actions
            .iter()
            .any(|action| matches!(action, ClientShellAction::History { .. })));
    }
    assert_eq!(history(&shell).query.as_str(), "界e\u{301}🙂 ß ");
    assert!(shell.clipboard_image_target().is_none());
    assert!(shell.modal_paste_target_active());
    let mut outcome = ClientShellInput::default();
    assert!(shell.handle_modal_paste_shortcut_with(
        &TerminalKey::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        &mut outcome,
        || Some("\nclipboard".into())
    ));
    assert!(outcome.requests.is_empty());
    assert_eq!(history(&shell).query.as_str(), "界e\u{301}🙂 ß  clipboard");
    key(&mut shell, KeyCode::Char('a'), KeyModifiers::CONTROL);
    press(&mut shell, KeyCode::Right);
    press(&mut shell, KeyCode::Delete);
    assert_eq!(history(&shell).query.as_str(), "界🙂 ß  clipboard");
    assert_eq!(shell.endpoints[0].status, ClientEndpointStatus::Online);
}

#[test]
fn search_and_preview_are_independently_fenced_and_cancelled() {
    let mut shell = shell();
    let search = load(&mut shell);
    let first_preview = start_preview(&mut shell);
    assert!(shell.history_request_is_current(&ClientEndpointId::Local, search));
    assert!(shell.history_request_is_current(&ClientEndpointId::Local, first_preview));
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        search,
        HistoryEvent::Progress {
            scanned: 1,
            total: 2
        }
    ));
    assert!(!shell.receive_history_event(
        &ClientEndpointId::Local,
        search,
        HistoryEvent::Preview { preview: preview() }
    ));
    assert!(!shell.receive_history_event(
        &ClientEndpointId::Local,
        first_preview,
        HistoryEvent::Result { result: result() }
    ));
    let next = press(&mut shell, KeyCode::Char('n'));
    assert!(next.actions.iter().any(|action| matches!(action, ClientShellAction::HistoryCancel { request_id } if *request_id == first_preview)));
    let second_preview = history(&shell).preview_request.as_ref().unwrap().id;
    assert_ne!(first_preview, second_preview);
    assert!(!shell.receive_history_event(
        &ClientEndpointId::Local,
        first_preview,
        HistoryEvent::Preview { preview: preview() }
    ));
    let mut wrong = preview();
    wrong.source.id = "different-source".into();
    assert!(!shell.receive_history_event(
        &ClientEndpointId::Local,
        second_preview,
        HistoryEvent::Preview { preview: wrong }
    ));
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        search,
        HistoryEvent::Done { truncated: false }
    ));
    assert!(!shell.history_request_is_current(&ClientEndpointId::Local, search));
    assert!(shell.history_request_is_current(&ClientEndpointId::Local, second_preview));
    let changed = key(&mut shell, KeyCode::Char('2'), KeyModifiers::CONTROL);
    assert!(changed.actions.iter().any(|action| matches!(action, ClientShellAction::HistoryCancel { request_id } if *request_id == second_preview)));
    assert!(!history(&shell).preview_open);
    assert!(!shell.receive_history_event(
        &ClientEndpointId::Local,
        second_preview,
        HistoryEvent::Preview { preview: preview() }
    ));
    let new_search = history(&shell).search_request.unwrap();
    assert!(!shell.receive_history_event(
        &ClientEndpointId::Local.with_session("other").unwrap(),
        new_search,
        HistoryEvent::Result { result: result() }
    ));
    let closed = press(&mut shell, KeyCode::Esc);
    assert!(closed.actions.iter().any(|action| matches!(action, ClientShellAction::HistoryCancel { request_id } if *request_id == new_search)));
    assert!(!shell.receive_history_event(
        &ClientEndpointId::Local,
        new_search,
        HistoryEvent::Result { result: result() }
    ));
    open(&mut shell);
    assert!(!shell.receive_history_event(
        &ClientEndpointId::Local,
        new_search,
        HistoryEvent::Error {
            message: "stale".into()
        }
    ));
}

#[test]
fn preview_selection_is_read_only_and_only_explicit_resolve_can_navigate() {
    let mut shell = shell();
    let original = shell.snapshot.as_ref().unwrap().focused_pane_id.clone();
    show_preview(&mut shell);
    let frame = shell.compose(106, 30).unwrap();
    let text = frame_text(&frame);
    assert!(text.contains("Matched 界 e\u{301} 🙂"), "{text}");
    assert!(text.contains("Focus running"));
    assert_eq!(shell.snapshot.as_ref().unwrap().focused_pane_id, original);
    for event in [
        RawInputEvent::Paste("do not execute".into()),
        RawInputEvent::Text(TextCommit::new("do not type")),
    ] {
        let outcome = shell.handle_raw_events(vec![event]);
        assert!(outcome.requests.is_empty());
        assert!(outcome.actions.is_empty());
    }
    assert!(shell.clipboard_image_target().is_none());
    assert!(!shell.modal_paste_target_active());
    assert!(press(&mut shell, KeyCode::Enter).actions.is_empty());
    let outcome = key(&mut shell, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(outcome.requests.is_empty());
    let [ClientShellAction::HistoryNavigate {
        request_id,
        request,
        location: target,
        open_session,
        ..
    }] = outcome.actions.as_slice()
    else {
        panic!("expected resolve-only navigation");
    };
    assert!(!open_session);
    assert_eq!(target, &location(true));
    assert_eq!(
        request.action,
        HistoryAction::Resolve {
            source_id: "source-one".into()
        }
    );
    let request_id = *request_id;
    assert!(!shell.finish_history_navigation(request_id + 1, Ok(())));
    assert!(shell.finish_history_navigation(
        request_id,
        Err("Target changed; nothing was focused".into())
    ));
    let outcome = shell.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("typing"))]);
    assert!(outcome.requests.is_empty());
    assert_eq!(
        history(&shell).navigation_error.as_deref(),
        Some("Target changed; nothing was focused")
    );
    assert_eq!(shell.endpoints[0].status, ClientEndpointStatus::Online);
    press(&mut shell, KeyCode::Tab);
    let outcome = key(&mut shell, KeyCode::Char('o'), KeyModifiers::CONTROL);
    let [ClientShellAction::HistoryNavigate {
        request_id,
        location: target,
        open_session,
        ..
    }] = outcome.actions.as_slice()
    else {
        panic!("expected explicit open-session action");
    };
    assert!(*open_session);
    assert_eq!(target, &location(false));
    let request_id = *request_id;
    shell.reset_endpoint_projection();
    assert!(shell.history_request_is_current(&ClientEndpointId::Local, request_id));
    assert!(shell.finish_history_navigation(request_id, Ok(())));
    assert!(shell.overlay.is_none());
    assert_eq!(shell.snapshot.as_ref().unwrap().focused_pane_id, original);
}

#[test]
fn resolved_locations_preserve_the_explicit_target_by_identity_not_list_position() {
    let mut shell = shell();
    show_preview(&mut shell);
    press(&mut shell, KeyCode::Tab);
    let outcome = key(&mut shell, KeyCode::Char('o'), KeyModifiers::CONTROL);
    let [ClientShellAction::HistoryNavigate { request_id, .. }] = outcome.actions.as_slice() else {
        panic!("expected navigation");
    };
    let request_id = *request_id;
    let mut source = result().source;
    source.locations.reverse();
    source.locations[0].pane_label = Some("Refreshed target".into());
    let target = source.locations[0].clone();
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        request_id,
        HistoryEvent::Resolved { source }
    ));
    assert_eq!(history(&shell).selected_location(), Some(&target));
    assert!(shell.finish_history_navigation(request_id, Err("Explicit target changed".into())));
    press(&mut shell, KeyCode::Home);
    assert!(frame_text(&shell.compose(106, 42).unwrap()).contains("Refreshed target"));
    assert!(history(&shell).navigation_error.is_some());
}

#[test]
fn preview_pages_hits_back_and_query_changes_cancel_owned_requests() {
    let mut shell = shell();
    show_preview(&mut shell);
    let outcome = press(&mut shell, KeyCode::Right);
    let [ClientShellAction::History {
        request,
        request_id,
        ..
    }] = outcome.actions.as_slice()
    else {
        panic!("expected next page");
    };
    assert_eq!(
        request.action,
        HistoryAction::Preview {
            source_id: "source-one".into(),
            anchor: 10
        }
    );
    let page = *request_id;
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        page,
        HistoryEvent::Error {
            message: "Preview unavailable".into()
        }
    ));
    let retry = press(&mut shell, KeyCode::F(5));
    let [ClientShellAction::History {
        request,
        request_id,
        ..
    }] = retry.actions.as_slice()
    else {
        panic!("expected preview retry");
    };
    assert_eq!(
        request.action,
        HistoryAction::Preview {
            source_id: "source-one".into(),
            anchor: 10
        }
    );
    let page = *request_id;
    let outcome = press(&mut shell, KeyCode::Esc);
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::HistoryCancel { request_id } if *request_id == page)));
    assert!(!history(&shell).preview_open);
    press(&mut shell, KeyCode::Down);
    let outcome = press(&mut shell, KeyCode::Enter);
    let ClientShellAction::History { request, .. } = outcome.actions.last().unwrap() else {
        panic!("expected selected hit preview");
    };
    assert_eq!(
        request.action,
        HistoryAction::Preview {
            source_id: "source-one".into(),
            anchor: 14
        }
    );
    let outcome = key(&mut shell, KeyCode::Char('3'), KeyModifiers::CONTROL);
    assert_eq!(
        outcome
            .actions
            .iter()
            .filter(|action| matches!(action, ClientShellAction::HistoryCancel { .. }))
            .count(),
        2
    );
    let id = history(&shell).search_request.unwrap();
    let outcome = shell.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("new query"))]);
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::HistoryCancel { request_id } if *request_id == id)));
    assert_eq!(history(&shell).query.as_str(), "new query");
    shell.reset_endpoint_projection();
    assert!(shell.overlay.is_none());
    assert!(!shell.history_request_is_current(&ClientEndpointId::Local, id));
}

#[test]
fn unrelated_machine_activation_retires_pending_history_navigation() {
    let mut shell = shell();
    show_preview(&mut shell);
    let outcome = key(&mut shell, KeyCode::Char('g'), KeyModifiers::CONTROL);
    let [ClientShellAction::HistoryNavigate { request_id, .. }] = outcome.actions.as_slice() else {
        panic!("expected navigation");
    };
    let request_id = *request_id;
    let mut other = shell.endpoints[0].clone();
    other.endpoint_id = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    );
    let other_id = other.endpoint_id.clone();
    shell.endpoints.push(other);
    assert!(shell.activate_endpoint_projection(&other_id));
    assert!(shell.overlay.is_none());
    assert!(!shell.history_request_is_current(&ClientEndpointId::Local, request_id));
    assert!(!shell.finish_history_navigation(request_id, Ok(())));
}

#[test]
fn grouped_results_include_all_provenance_and_persistent_partial_coverage_feedback() {
    let mut shell = shell();
    let id = load(&mut shell);
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        id,
        HistoryEvent::Progress {
            scanned: 2,
            total: 5
        }
    ));
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        id,
        HistoryEvent::Unavailable {
            locations: vec![location(false)],
            message: "Saved source unreadable".into()
        }
    ));
    let frame = shell.compose(106, 42).unwrap();
    let text = frame_text(&frame);
    for expected in [
        "Synthetic conversation",
        "source / Native project",
        "stopped / Native project",
        "ws_native",
        "tab_native",
        "pane_native",
        "terminal_native",
        "First synthetic needle",
        "2/5 sources scanned",
        "unavailable sources",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        id,
        HistoryEvent::Done { truncated: true }
    ));
    assert!(frame_text(&shell.compose(106, 42).unwrap()).contains("truncated"));
    let id = key(&mut shell, KeyCode::Char('2'), KeyModifiers::CONTROL);
    assert!(!id.actions.is_empty());
    let id = history(&shell).search_request.unwrap();
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        id,
        HistoryEvent::Error {
            message: "History helper unavailable on this machine".into()
        }
    ));
    press(&mut shell, KeyCode::Down);
    assert_eq!(
        history(&shell).error.as_deref(),
        Some("History helper unavailable on this machine")
    );
    assert!(shell.endpoint_error.is_none());
    assert_eq!(shell.endpoints[0].status, ClientEndpointStatus::Online);
}

#[test]
fn history_mouse_owns_new_gestures_but_releases_the_existing_pane_owner() {
    let mut shell = shell();
    let mut pane = surface();
    pane.panes[0].mouse_reporting = true;
    shell.set_pane_surface(pane);
    shell.compose(106, 30).unwrap();
    let point = (
        shell.hits.panes[0].inner_rect.x,
        shell.hits.panes[0].inner_rect.y,
    );
    let pressed = mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), point);
    assert!(!pressed.requests.is_empty());
    assert!(shell.pane_mouse_gesture.is_some());
    let search = load(&mut shell);
    let released = mouse(&mut shell, MouseEventKind::Up(MouseButton::Left), point);
    assert!(released.requests.iter().any(|request| matches!(request, ClientMessage::ClientShellPaneInput { events, .. } if events.iter().any(|event| matches!(event, ClientPaneInputEvent::Mouse { kind: crate::protocol::ClientMouseKind::Up(_), .. })))));
    assert!(shell.pane_mouse_gesture.is_none());
    let outside = mouse(&mut shell, MouseEventKind::Down(MouseButton::Left), (0, 0));
    assert!(outside.requests.is_empty());
    assert!(outside.actions.is_empty());
    assert!(shell.history_request_is_current(&ClientEndpointId::Local, search));
    shell.compose(106, 30).unwrap();
    let hit = shell.hits.history.rows[0].0;
    let selected = mouse(
        &mut shell,
        MouseEventKind::Down(MouseButton::Left),
        (hit.x, hit.y),
    );
    assert!(selected.requests.is_empty());
    assert!(selected.actions.iter().any(|action| matches!(action, ClientShellAction::History { request, .. } if matches!(request.action, HistoryAction::Preview { .. }))));
}

#[test]
fn coalesced_scroll_and_mouse_scrollbar_use_current_wrapped_preview_geometry() {
    let mut shell = shell();
    show_preview(&mut shell);
    shell.compose(60, 20).unwrap();
    let initial = history(&shell).preview.as_ref().unwrap().document.scroll;
    press(&mut shell, KeyCode::Down);
    assert_eq!(
        history(&shell).preview.as_ref().unwrap().document.scroll,
        initial + 1
    );
    let body = shell.hits.history.body;
    let outcome = mouse(&mut shell, MouseEventKind::ScrollDown, (body.x, body.y));
    assert!(outcome.requests.is_empty());
    assert_eq!(
        history(&shell).preview.as_ref().unwrap().document.scroll,
        initial + 4
    );
    shell.compose(60, 20).unwrap();
    let track = shell.hits.history.scrollbar;
    assert!(!track.is_empty());
    mouse(
        &mut shell,
        MouseEventKind::Down(MouseButton::Left),
        (track.x, track.bottom() - 1),
    );
    let preview = history(&shell).preview.as_ref().unwrap();
    assert_eq!(
        preview.document.scroll,
        preview.document.metrics(body).max_offset_from_bottom
    );
    let before = preview.document.scroll;
    mouse(
        &mut shell,
        MouseEventKind::Down(MouseButton::Left),
        (track.x, track.bottom() - 1),
    );
    mouse(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (track.x, track.y),
    );
    assert!(history(&shell).preview.as_ref().unwrap().document.scroll < before);
}

#[test]
fn coalesced_preview_reply_scrolls_before_the_first_preview_frame() {
    for use_mouse in [false, true] {
        let mut shell = shell();
        shell.compose(60, 20).unwrap();
        show_preview(&mut shell);
        let body = overlay::history_geometry(Rect::new(0, 0, 60, 20)).body;
        let mut expected = HistoryDocument::from_preview(&preview());
        expected.prepare(body);
        let start = expected.scroll;
        let outcome = if use_mouse {
            mouse(&mut shell, MouseEventKind::ScrollDown, (body.x, body.y))
        } else {
            press(&mut shell, KeyCode::Down)
        };
        assert!(outcome.requests.is_empty());
        assert_eq!(
            history(&shell).preview.as_ref().unwrap().document.scroll,
            start + if use_mouse { 3 } else { 1 }
        );
    }
}

#[test]
fn shifted_kitty_preview_shortcuts_select_previous_hit_and_location() {
    let mut shell = shell();
    show_preview(&mut shell);
    press(&mut shell, KeyCode::Tab);
    assert_eq!(history(&shell).preview.as_ref().unwrap().location, Some(1));
    key(&mut shell, KeyCode::Tab, KeyModifiers::SHIFT);
    assert_eq!(history(&shell).preview.as_ref().unwrap().location, Some(0));
    press(&mut shell, KeyCode::Char('n'));
    assert_eq!(history(&shell).selected.hit, 1);
    let outcome = shell.handle_raw_events(vec![RawInputEvent::Key(
        TerminalKey::new(KeyCode::Char('n'), KeyModifiers::SHIFT)
            .with_generated_text(Some("N".into())),
    )]);
    assert_eq!(history(&shell).selected.hit, 0);
    assert!(outcome.requests.is_empty());
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::History { request, .. } if matches!(request.action, HistoryAction::Preview { anchor: 4, .. }))));
}

#[test]
fn compact_preview_exposes_navigation_errors_and_current_location_without_dismissing_them() {
    let mut shell = shell();
    show_preview(&mut shell);
    let outcome = key(&mut shell, KeyCode::Char('g'), KeyModifiers::CONTROL);
    let [ClientShellAction::HistoryNavigate { request_id, .. }] = outcome.actions.as_slice() else {
        panic!("expected navigation");
    };
    assert!(shell.finish_history_navigation(*request_id, Err("Target unavailable".into())));
    let text = frame_text(&shell.compose(106, 12).unwrap());
    assert!(text.contains("Target unavailable"));
    assert!(text.contains("source / Native project"));
    shell.handle_raw_events(vec![RawInputEvent::Paste("normal typing".into())]);
    assert!(frame_text(&shell.compose(106, 12).unwrap()).contains("Target unavailable"));
}

#[test]
fn forwarded_key_release_keeps_its_original_pane_while_history_is_open() {
    let mut shell = shell();
    shell.set_host_reports_key_releases(true);
    let held = TerminalKey::new(KeyCode::Char('z'), KeyModifiers::empty());
    let pressed = shell.handle_raw_events(vec![RawInputEvent::Key(held.clone())]);
    assert!(!pressed.requests.is_empty());
    open(&mut shell);
    let released = shell.handle_raw_events(vec![RawInputEvent::Key(
        held.with_kind(KeyEventKind::Release),
    )]);
    assert!(released.requests.iter().any(|request| matches!(request, ClientMessage::ClientShellPaneInput { pane_id, events } if pane_id == "pane_1" && events.iter().any(|event| matches!(event, ClientPaneInputEvent::Key { kind: crate::protocol::ClientKeyKind::Release, .. })))));
    assert!(matches!(
        shell.overlay,
        Some(ClientShellOverlay::PiHistory(_))
    ));
}

#[test]
fn partial_preview_diagnostics_survive_the_transcript_and_done_events() {
    let mut shell = shell();
    let search = load(&mut shell);
    let preview_id = start_preview(&mut shell);
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        preview_id,
        HistoryEvent::Unavailable {
            locations: Vec::new(),
            message: "Partial preview: unreadable records".into(),
        }
    ));
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        preview_id,
        HistoryEvent::Preview { preview: preview() }
    ));
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        preview_id,
        HistoryEvent::Done { truncated: true }
    ));
    assert!(shell.history_request_is_current(&ClientEndpointId::Local, search));
    assert!(!shell.history_request_is_current(&ClientEndpointId::Local, preview_id));
    for event in [
        RawInputEvent::Text(TextCommit::new("ordinary typing")),
        RawInputEvent::Paste("ordinary paste".into()),
    ] {
        let outcome = shell.handle_raw_events(vec![event]);
        assert!(outcome.requests.is_empty() && outcome.actions.is_empty());
        assert!(frame_text(&shell.compose(106, 30).unwrap())
            .contains("Partial preview: unreadable records"));
    }
}

#[test]
fn resolved_locations_never_silently_retarget_a_disappeared_explicit_location() {
    let mut shell = shell();
    show_preview(&mut shell);
    press(&mut shell, KeyCode::Tab);
    let outcome = key(&mut shell, KeyCode::Char('o'), KeyModifiers::CONTROL);
    let [ClientShellAction::HistoryNavigate {
        request_id,
        location: target,
        ..
    }] = outcome.actions.as_slice()
    else {
        panic!("expected navigation");
    };
    assert_eq!(target, &location(false));
    let request_id = *request_id;
    let mut source = result().source;
    source.locations.retain(|location| location.running);
    assert!(shell.receive_history_event(
        &ClientEndpointId::Local,
        request_id,
        HistoryEvent::Resolved { source }
    ));
    assert!(history(&shell).selected_location().is_none());
    assert!(shell.finish_history_navigation(request_id, Err("Selected target disappeared".into())));
    for character in ['g', 'o'] {
        assert!(
            key(&mut shell, KeyCode::Char(character), KeyModifiers::CONTROL)
                .actions
                .is_empty()
        );
    }
    press(&mut shell, KeyCode::Tab);
    let outcome = key(&mut shell, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(matches!(
        outcome.actions.as_slice(),
        [ClientShellAction::HistoryNavigate { location: target, open_session: false, .. }] if target == &location(true)
    ));
}

#[test]
fn tiny_history_layout_keeps_close_and_explicit_actions_visible_and_clickable() {
    for (width, height) in [(1, 1), (2, 3), (4, 4), (8, 7), (20, 8), (32, 10)] {
        let mut shell = shell();
        show_preview(&mut shell);
        let frame = shell.compose(width, height).unwrap();
        let close = shell
            .hits
            .history
            .controls
            .iter()
            .find(|(_, control)| *control == HistoryControl::Close)
            .unwrap()
            .0;
        let symbol = &frame.cells
            [usize::from(close.y) * usize::from(frame.width) + usize::from(close.x)]
        .symbol;
        assert!(
            !symbol.trim().is_empty(),
            "invisible close at {width}x{height}"
        );
        let outcome = mouse(
            &mut shell,
            MouseEventKind::Down(MouseButton::Left),
            (close.x, close.y),
        );
        assert!(outcome.requests.is_empty());
        assert!(shell.overlay.is_none());
    }
    for (width, height) in [(4, 4), (8, 7), (20, 8), (32, 10), (60, 20)] {
        for stopped in [false, true] {
            let mut shell = shell();
            show_preview(&mut shell);
            if stopped {
                press(&mut shell, KeyCode::Tab);
            }
            shell.compose(width, height).unwrap();
            let control = if stopped {
                HistoryControl::OpenSession
            } else {
                HistoryControl::FocusRunning
            };
            let action = shell
                .hits
                .history
                .controls
                .iter()
                .find(|(_, candidate)| *candidate == control)
                .unwrap_or_else(|| panic!("missing explicit action at {width}x{height}"))
                .0;
            let outcome = mouse(
                &mut shell,
                MouseEventKind::Down(MouseButton::Left),
                (action.x, action.y),
            );
            assert!(outcome.requests.is_empty());
            assert!(
                matches!(outcome.actions.as_slice(), [ClientShellAction::HistoryNavigate { location: target, open_session, .. }] if *open_session == stopped && target == &location(!stopped))
            );
        }
    }
}

#[test]
fn compact_query_and_scope_controls_keep_their_input_and_cancel_ids() {
    for (width, height) in [(8, 7), (20, 8), (32, 10)] {
        let mut shell = shell();
        let search = load(&mut shell);
        let outcome = shell.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("界🙂"))]);
        assert!(outcome.requests.is_empty());
        assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::HistoryCancel { request_id } if *request_id == search)));
        let frame = shell.compose(width, height).unwrap();
        assert!(frame.cursor.is_some());
        assert!(frame_text(&frame).contains('🙂'));
        let search = history(&shell).search_request.unwrap();
        let scope = shell
            .hits
            .history
            .controls
            .iter()
            .find(|(_, control)| *control == HistoryControl::Scope(HistoryScope::All))
            .unwrap()
            .0;
        let outcome = mouse(
            &mut shell,
            MouseEventKind::Down(MouseButton::Left),
            (scope.x, scope.y),
        );
        assert!(outcome.requests.is_empty());
        assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::HistoryCancel { request_id } if *request_id == search)));
        assert!(
            matches!(outcome.actions.last(), Some(ClientShellAction::History { request, .. }) if request.scope == HistoryScope::All && request.query == "界🙂")
        );
    }
}

#[test]
fn history_retires_shell_drags_and_autoscroll_but_keeps_pane_release_leases() {
    let mut shell = shell();
    shell.compose(106, 30).unwrap();
    shell.chrome_drag = Some(ClientChromeDrag::SidebarWidth);
    shell.selection_autoscroll = Some(ClientSelectionAutoscroll {
        pane_id: "pane_1".into(),
        direction: ClientSelectionAutoscrollDirection::Down,
        last_mouse_column: 20,
        last_mouse_row: 20,
        inner_rect: shell.hits.panes[0].inner_rect,
        offset_from_bottom: 20,
        max_offset_from_bottom: 100,
    });
    shell.selection_autoscroll_deadline = Some(std::time::Instant::now());
    open(&mut shell);
    assert!(shell.chrome_drag.is_none());
    assert!(shell.selection_autoscroll.is_none());
    assert!(shell.selection_autoscroll_deadline.is_none());
    assert!(shell
        .tick_selection_autoscroll(std::time::Instant::now())
        .requests
        .is_empty());
}

#[test]
fn history_scrollbar_grab_is_released_when_host_focus_is_lost() {
    let mut shell = shell();
    show_preview(&mut shell);
    shell.compose(60, 20).unwrap();
    let track = shell.hits.history.scrollbar;
    for _ in 0..2 {
        mouse(
            &mut shell,
            MouseEventKind::Down(MouseButton::Left),
            (track.x, track.bottom() - 1),
        );
    }
    assert!(history(&shell).scrollbar_grab.is_some());
    shell.handle_raw_events(vec![RawInputEvent::OuterFocusLost]);
    assert!(history(&shell).scrollbar_grab.is_none());
    let before = history(&shell).preview.as_ref().unwrap().document.scroll;
    let outcome = mouse(
        &mut shell,
        MouseEventKind::Drag(MouseButton::Left),
        (track.x, track.y),
    );
    assert!(outcome.requests.is_empty());
    assert_eq!(
        history(&shell).preview.as_ref().unwrap().document.scroll,
        before
    );
}

#[test]
fn unicode_preview_virtualizes_beyond_u16_rows_and_survives_tiny_sizes() {
    let mut value = preview();
    value.entries[0].text = "x".repeat(280_000);
    value.entries[1].text = "MATCH\n界e\u{301}🙂".into();
    let mut document = HistoryDocument::from_preview(&value);
    let body = Rect::new(0, 0, 4, 25);
    document.prepare(body);
    assert!(document.scroll > usize::from(u16::MAX));
    let mut buffer = Buffer::empty(body);
    document.render(&mut buffer, body, None, &Palette::catppuccin());
    let text = buffer
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("MAT"));
    for grapheme in ["界", "e\u{301}", "🙂"] {
        assert!(buffer.content.iter().any(|cell| cell.symbol() == grapheme));
    }
    assert!(document.metrics(body).max_offset_from_bottom > usize::from(u16::MAX));
    let mut shell = shell();
    show_preview(&mut shell);
    for width in [1, 2, 3, 4, 8, 20, 44, 106] {
        for height in [1, 2, 3, 4, 7, 12, 30] {
            let frame = shell.compose(width, height).unwrap();
            assert_eq!((frame.width, frame.height), (width, height));
            let area = shell.hits.history.area;
            assert!(area.right() <= width && area.bottom() <= height);
            press(&mut shell, KeyCode::End);
            press(&mut shell, KeyCode::Home);
        }
    }
    let _ = shell.compose(0, 0);
}
