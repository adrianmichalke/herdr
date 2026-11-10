use super::*;
use crate::client::pi_history::tests::{coordinator, location, source, Capture, Harness};
use crate::client::{endpoint, shell};
use crate::protocol::ClientMessage;
use crossterm::event::{KeyCode, KeyModifiers};

fn install(
    harness: &mut Harness,
    coordinator: &mut HistoryCoordinator,
    target: HistoryLocation,
    open: bool,
) -> u64 {
    let (search, _) = harness.open();
    harness.result(search, target.clone());
    let preview = harness.key(KeyCode::Enter, KeyModifiers::empty());
    let shell::ClientShellAction::History {
        request_id: preview,
        ..
    } = preview.actions.into_iter().next().unwrap()
    else {
        panic!("preview request");
    };
    let shell = harness.state.shell.as_mut().unwrap();
    shell.receive_history_event(
        &ClientEndpointId::Local,
        preview,
        HistoryEvent::Preview {
            preview: crate::pi_history::types::HistoryPreview {
                source: source(target),
                entries: vec![crate::pi_history::types::HistoryEntry {
                    entry_line: 1,
                    role: "user".into(),
                    text: "synthetic needle".into(),
                    truncated: false,
                }],
                matched_entry: 0,
                previous: None,
                next: None,
                truncated: false,
            },
        },
    );
    shell.receive_history_event(
        &ClientEndpointId::Local,
        preview,
        HistoryEvent::Done { truncated: false },
    );
    let outcome = harness.key(
        KeyCode::Char(if open { 'o' } else { 'g' }),
        KeyModifiers::CONTROL,
    );
    let shell::ClientShellAction::HistoryNavigate {
        machine,
        request_id,
        request,
        location,
        open_session,
    } = outcome.actions.into_iter().next().unwrap()
    else {
        panic!("explicit navigation");
    };
    let key = coordinator
        .key(&machine, request_id, &harness.context())
        .unwrap();
    let view = harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .history_navigation_view()
        .unwrap();
    coordinator.navigation =
        Some(Navigation::new(key, *request, location, open_session, view).unwrap());
    request_id
}

fn captured_focus(harness: &Harness) -> Request {
    let messages = harness.sent.lock().unwrap();
    let [ClientMessage::ClientShellEndpointRequest { request, .. }] = messages.as_slice() else {
        panic!("only one conditional JSON focus is allowed");
    };
    serde_json::from_str(request).unwrap()
}

#[test]
fn current_session_uses_refreshed_terminal_and_conditional_json_focus_without_a_switch() {
    let mut harness = Harness::new();
    let mut coordinator = coordinator();
    let mut sessions = SessionSwitchCoordinator::default();
    let mut original = location(&harness.session(), true);
    original.terminal_id = Some("old-terminal".into());
    let request_id = install(&mut harness, &mut coordinator, original.clone(), false);
    let mut fresh = original;
    fresh.terminal_id = Some("fresh-terminal".into());
    let before = harness.endpoints.active_id().clone();
    coordinator.resolved(
        Some(source(fresh.clone())),
        &mut sessions,
        &mut harness.context(),
    );
    assert!(!sessions.busy());
    assert_eq!(harness.endpoints.active_id(), &before);
    let request = captured_focus(&harness);
    let Method::PaneFocusIfSession(params) = &request.method else {
        panic!("conditional focus only");
    };
    assert_eq!(params.terminal_id, "fresh-terminal");
    assert_eq!(params.agent_session, fresh.agent_session);
    assert!(harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .history_request_is_current(&ClientEndpointId::Local, request_id));
    let pane = serde_json::from_value(serde_json::json!({
        "pane_id": fresh.pane_id, "terminal_id": "fresh-terminal", "workspace_id": fresh.workspace_id,
        "tab_id": fresh.tab_id, "focused": true, "agent_status": "idle", "revision": 1, "agent_session": fresh.agent_session,
    })).unwrap();
    let response = crate::api::schema::SuccessResponse {
        id: request.id.clone(),
        result: ResponseResult::PaneInfo { pane },
    };
    let completed = harness
        .commands
        .receive_chunk(
            &before,
            41,
            "boot-1",
            &request.id,
            true,
            serde_json::to_vec(&response).unwrap(),
        )
        .unwrap()
        .unwrap();
    coordinator
        .receive_focus_result(completed, &mut sessions, &mut harness.context())
        .unwrap();
    assert!(coordinator.navigation.is_none());
    assert!(!harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .history_request_is_current(&ClientEndpointId::Local, request_id));
    assert_eq!(
        harness
            .state
            .shell
            .as_mut()
            .unwrap()
            .allocate_history_session_request(),
        1
    );
}

#[test]
fn changed_source_association_or_stopped_focus_never_sends_or_opens() {
    for change in ["source", "association", "stopped", "missing"] {
        let mut harness = Harness::new();
        let mut coordinator = coordinator();
        let mut sessions = SessionSwitchCoordinator::default();
        let target = location(&harness.session(), true);
        let request_id = install(&mut harness, &mut coordinator, target.clone(), false);
        let mut resolved = source(target);
        match change {
            "source" => resolved.id = "another-source".into(),
            "association" => {
                resolved.locations[0].agent_session.value = "/synthetic/changed.jsonl".into()
            }
            "stopped" => {
                resolved.locations[0].running = false;
                resolved.locations[0].terminal_id = None;
            }
            "missing" => resolved.locations.clear(),
            _ => unreachable!(),
        }
        coordinator.resolved(Some(resolved), &mut sessions, &mut harness.context());
        assert!(coordinator.navigation.is_none());
        assert!(!sessions.busy());
        assert!(harness.sent.lock().unwrap().is_empty());
        assert!(harness.endpoints.active_surface_available());
        assert!(!harness
            .state
            .shell
            .as_ref()
            .unwrap()
            .history_request_is_current(&ClientEndpointId::Local, request_id));
    }
}

#[test]
fn optional_focus_absence_and_busy_normal_lane_fail_locally_without_leaving_a_queued_focus() {
    for busy in [false, true] {
        let mut harness = Harness::new();
        let mut coordinator = coordinator();
        let mut sessions = SessionSwitchCoordinator::default();
        let target = location(&harness.session(), true);
        install(&mut harness, &mut coordinator, target.clone(), false);
        if busy {
            harness.commands.enqueue(
                ClientEndpointId::Local,
                41,
                "boot-1".into(),
                Box::new(Request {
                    id: "normal-action".into(),
                    method: Method::PaneFocus(crate::api::schema::PaneTarget {
                        pane_id: "pane_1".into(),
                    }),
                }),
            );
            harness
                .commands
                .send_next(&ClientEndpointId::Local, &mut harness.endpoints);
        } else {
            harness.endpoints = endpoint::EndpointRegistry::new(
                Capture(Arc::clone(&harness.sent)),
                41,
                Default::default(),
            );
        }
        let sent_before = harness.sent.lock().unwrap().len();
        coordinator.resolved(Some(source(target)), &mut sessions, &mut harness.context());
        assert!(coordinator.navigation.is_none());
        assert!(!sessions.busy());
        assert_eq!(harness.sent.lock().unwrap().len(), sent_before);
        if busy {
            assert!(harness.commands.contains_request(
                &ClientEndpointId::Local,
                41,
                "boot-1",
                "normal-action"
            ));
        }
        assert!(harness.endpoints.active_surface_available());
    }
}

#[test]
fn explicit_cross_session_open_re_resolves_before_focusing_the_new_terminal() {
    let mut harness = Harness::new();
    let mut coordinator = coordinator();
    let mut sessions = SessionSwitchCoordinator::default();
    let target = location("stopped-synthetic", false);
    let request_id = install(&mut harness, &mut coordinator, target.clone(), true);
    coordinator.resolved(
        Some(source(target.clone())),
        &mut sessions,
        &mut harness.context(),
    );
    assert!(sessions.busy());
    assert_eq!(sessions.history_allow_start(1), Some(true));
    assert_ne!(request_id, 1);
    assert!(harness.sent.lock().unwrap().is_empty());
    sessions.cancel_history(request_id, &mut harness.context());
    let endpoint_id = ClientEndpointId::Local
        .with_session(&target.session)
        .unwrap();
    let shell = harness.state.shell.as_mut().unwrap();
    assert!(shell.stage_session_endpoint(
        &ClientEndpointId::Local,
        endpoint_id.clone(),
        target.session.clone()
    ));
    shell.set_endpoint_snapshot_for_generation(&endpoint_id, 42, Box::new(shell::test_snapshot()));
    shell.set_endpoint_status(&endpoint_id, endpoint::ClientEndpointStatus::Online);
    assert!(shell.activate_endpoint_projection(&endpoint_id));
    shell.commit_session_endpoint(&endpoint_id);
    harness.endpoints.insert(
        endpoint_id.clone(),
        Capture(Arc::clone(&harness.sent)),
        42,
        endpoint::EndpointNegotiation::new(vec!["pane.focus_if_session".into()], vec![]),
        true,
    );
    assert!(harness.endpoints.set_active(&endpoint_id));
    let first_serial = coordinator.navigation.as_ref().unwrap().key.serial;
    coordinator.resolve_after_open(&mut sessions, &mut harness.context());
    let navigation = coordinator.navigation.as_ref().unwrap();
    assert!(navigation.key.serial > first_serial);
    assert!(matches!(
        navigation.phase,
        Phase::Resolving { after_open: true }
    ));
    assert_eq!(coordinator.pending.len(), 1);
    assert!(matches!(
        coordinator.pending[0].request.action,
        RequestAction::Resolve { .. }
    ));
    assert!(harness.sent.lock().unwrap().is_empty());
    coordinator.pending.clear();
    let mut fresh = target;
    fresh.running = true;
    fresh.terminal_id = Some("new-terminal-after-open".into());
    coordinator.resolved(Some(source(fresh)), &mut sessions, &mut harness.context());
    let Method::PaneFocusIfSession(params) = captured_focus(&harness).method else {
        panic!("conditional focus");
    };
    assert_eq!(params.terminal_id, "new-terminal-after-open");
}

#[test]
fn running_cross_session_focus_is_existing_only_and_unrelated_picker_cancellation_is_preserved() {
    let mut harness = Harness::new();
    let mut coordinator = coordinator();
    let mut sessions = SessionSwitchCoordinator::default();
    let target = location("other-running", true);
    let request_id = install(&mut harness, &mut coordinator, target.clone(), false);
    coordinator.resolved(Some(source(target)), &mut sessions, &mut harness.context());
    assert!(sessions.busy());
    assert_eq!(sessions.history_allow_start(1), Some(false));
    assert_ne!(request_id, 1);
    assert!(harness.sent.lock().unwrap().is_empty());
    coordinator.cancel_worker(request_id + 100);
    sessions.cancel_history(request_id + 100, &mut harness.context());
    assert!(sessions.busy());
    harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .open_session_picker(&mut shell::ClientShellInput::default());
    coordinator
        .drive(&mut sessions, &mut harness.context())
        .unwrap();
    assert!(!sessions.busy());
    assert_eq!(harness.endpoints.active_id(), &ClientEndpointId::Local);
    assert!(harness.endpoints.active_surface_available());
    assert_eq!(
        harness
            .state
            .shell
            .as_mut()
            .unwrap()
            .allocate_history_session_request(),
        3
    );
}

#[test]
fn unrelated_navigation_cancels_history_without_changing_the_committed_session() {
    let mut harness = Harness::new();
    let mut coordinator = coordinator();
    let mut sessions = SessionSwitchCoordinator::default();
    let target = location(&harness.session(), true);
    install(&mut harness, &mut coordinator, target, false);
    harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .note_session_navigation();
    let view = harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .history_navigation_view()
        .unwrap();
    coordinator
        .drive(&mut sessions, &mut harness.context())
        .unwrap();
    assert!(coordinator.navigation.is_none());
    assert_eq!(
        harness
            .state
            .shell
            .as_ref()
            .unwrap()
            .history_navigation_view()
            .unwrap(),
        view
    );
    assert!(harness.sent.lock().unwrap().is_empty());
    assert!(harness.endpoints.active_surface_available());
}

#[test]
fn unrelated_catalog_unavailability_does_not_block_a_valid_resolved_association() {
    let mut harness = Harness::new();
    let mut coordinator = coordinator();
    let mut sessions = SessionSwitchCoordinator::default();
    let target = location(&harness.session(), true);
    install(&mut harness, &mut coordinator, target.clone(), false);
    let key = coordinator.navigation.as_ref().unwrap().key.clone();
    coordinator.workers.push(Worker {
        key: key.clone(),
        lane: Lane::Detail,
        cancelled: Arc::new(AtomicBool::new(false)),
        discarded: false,
        resolved: None,
        problem: None,
    });
    for event in [
        HistoryEvent::Unavailable {
            locations: vec![location("unrelated", false)],
            message: "Unrelated synthetic source unavailable".into(),
        },
        HistoryEvent::Resolved {
            source: source(target),
        },
        HistoryEvent::Done { truncated: true },
    ] {
        coordinator
            .receive(
                HistoryWorkerEvent {
                    key: key.clone(),
                    payload: WorkerPayload::Event(event),
                },
                &mut sessions,
                &mut harness.context(),
            )
            .unwrap();
    }
    coordinator
        .receive(
            HistoryWorkerEvent {
                key,
                payload: WorkerPayload::Finished(Ok(())),
            },
            &mut sessions,
            &mut harness.context(),
        )
        .unwrap();
    assert!(matches!(
        captured_focus(&harness).method,
        Method::PaneFocusIfSession(_)
    ));
    assert!(!sessions.busy());
}
