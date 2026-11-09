use super::*;
use std::sync::Mutex;

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<ClientMessage>>>);

impl endpoint::EndpointTransport for Capture {
    fn send(&mut self, message: &ClientMessage) -> io::Result<()> {
        self.0.lock().unwrap().push(message.clone());
        Ok(())
    }
}

fn negotiation() -> endpoint::EndpointNegotiation {
    endpoint::EndpointNegotiation::new(
        shell::WORKSPACE_TRANSFER_METHODS
            .into_iter()
            .chain([
                "client_shell.surface.set",
                "pane.focus",
                "tab.focus",
                "workspace.focus",
            ])
            .map(str::to_owned)
            .collect(),
        vec![
            crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
            crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
        ],
    )
}

struct Harness {
    state: ClientState,
    endpoints: endpoint::EndpointRegistry,
    commands: endpoint_commands::EndpointCommands,
    supervisors: endpoint::EndpointSupervisors,
    catalog: endpoint::EndpointCatalog,
    activation: Option<endpoint::PendingEndpointActivation>,
    next_surface_serial: u64,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    sent: Arc<Mutex<Vec<ClientMessage>>>,
    source: shell::WorkspaceTransferSource,
    request_id: u64,
    destination: String,
}

impl Harness {
    fn new(background: bool) -> Self {
        Self::with_machine(background, None, false)
    }

    fn with_machine(
        background: bool,
        profile: Option<SavedSshEndpoint>,
        standalone_remote: bool,
    ) -> Self {
        let mut state = ClientState::test_new();
        if standalone_remote {
            state.shell = Some(shell::ClientShellState::new(
                shell::ClientShellConfig::from_config(&crate::config::Config::default())
                    .with_endpoint_preferences(
                        std::path::Path::new("source.sock"),
                        Some(r#"["dev@example.com", "named"]"#),
                    ),
            ));
        }
        let mut catalog = endpoint::EndpointCatalog::default();
        let endpoint_id = profile.as_ref().map_or(ClientEndpointId::Local, |profile| {
            ClientEndpointId::Ssh(profile.id.clone())
        });
        catalog.ssh.extend(profile);
        let shell = state.shell.as_mut().unwrap();
        shell.set_endpoint_catalog(&catalog.ssh);
        let mut snapshot = shell::test_snapshot();
        if background {
            let mut workspace = snapshot.workspaces[0].clone();
            workspace.workspace_id = "ws_background".into();
            workspace.focused = false;
            snapshot.workspaces.push(workspace);
        }
        shell.set_endpoint_snapshot_for_generation(&endpoint_id, 41, Box::new(snapshot));
        shell.set_endpoint_methods_for(&endpoint_id, Some(negotiation().methods()));
        shell.set_endpoint_status(&endpoint_id, endpoint::ClientEndpointStatus::Online);
        assert!(shell.activate_endpoint_projection(&endpoint_id));
        let source = shell
            .workspace_transfer_source(if background { "ws_background" } else { "ws_1" })
            .unwrap();
        let destination = if source.session == "destination" {
            "other-destination"
        } else {
            "destination"
        }
        .to_owned();
        let mut outcome = shell::ClientShellInput::default();
        shell.open_workspace_transfer_picker(source.clone(), &mut outcome);
        let [shell::ClientShellAction::SessionList {
            machine,
            request_id,
        }] = outcome.actions.as_slice()
        else {
            panic!()
        };
        assert!(shell.receive_session_list(
            machine,
            *request_id,
            Ok(vec![crate::session::SessionInfo {
                name: destination.clone(),
                default: false,
                running: false,
                connection_error: None,
                socket_path: format!("/tmp/herdr-ui-test/{destination}/client.sock"),
                session_dir: format!("/tmp/herdr-ui-test/{destination}"),
            }])
        ));
        let outcome = shell.handle_raw_events(vec![crate::raw_input::RawInputEvent::Key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            ),
        )]);
        let [shell::ClientShellAction::WorkspaceTransfer { request_id, .. }] =
            outcome.actions.as_slice()
        else {
            panic!()
        };
        let request_id = *request_id;
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut endpoints =
            endpoint::EndpointRegistry::new(Capture(sent.clone()), 41, negotiation());
        if endpoint_id != ClientEndpointId::Local {
            endpoints.insert(
                endpoint_id.clone(),
                Capture(sent.clone()),
                41,
                negotiation(),
                true,
            );
            assert!(endpoints.set_active(&endpoint_id));
        }
        Self {
            state,
            endpoints,
            commands: Default::default(),
            supervisors: endpoint::EndpointSupervisors::new(&[], Instant::now()),
            catalog,
            activation: None,
            next_surface_serial: 1,
            event_tx: tokio::sync::mpsc::channel(4).0,
            sent,
            source,
            request_id,
            destination,
        }
    }

    fn context(&mut self) -> SessionSwitchContext<'_> {
        let options = connect_options(&self.state, false).unwrap();
        SessionSwitchContext {
            state: &mut self.state,
            endpoints: &mut self.endpoints,
            commands: &mut self.commands,
            supervisors: &mut self.supervisors,
            catalog: &mut self.catalog,
            activation: &mut self.activation,
            next_surface_serial: &mut self.next_surface_serial,
            event_tx: &self.event_tx,
            options,
        }
    }

    fn submit(&mut self, coordinator: &mut SessionSwitchCoordinator) {
        coordinator
            .actions(
                vec![SessionAction::Transfer {
                    source: self.source.clone(),
                    request_id: self.request_id,
                    destination_session: self.destination.clone(),
                }],
                &mut self.context(),
            )
            .unwrap();
    }

    fn last_request(&self) -> Request {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|message| {
                let ClientMessage::ClientShellEndpointRequest { request, .. } = message else {
                    return None;
                };
                Some(serde_json::from_str(request).unwrap())
            })
            .unwrap()
    }

    fn info(
        &self,
        coordinator: &SessionSwitchCoordinator,
        state: WorkspaceTransferState,
    ) -> WorkspaceTransferInfo {
        WorkspaceTransferInfo {
            transfer_id: coordinator.transfer.as_ref().unwrap().transfer_id.clone(),
            state,
            destination_session: self.destination.clone(),
            workspace_id: (state == WorkspaceTransferState::Committed)
                .then(|| "ws_destination".into()),
            focused_tab_id: (state == WorkspaceTransferState::Committed)
                .then(|| "tab_destination".into()),
            focused_pane_id: (state == WorkspaceTransferState::Committed)
                .then(|| "pane_destination".into()),
            error: (state == WorkspaceTransferState::Failed)
                .then(|| "destination unavailable; source retained".into()),
        }
    }

    fn reply(&mut self, coordinator: &mut SessionSwitchCoordinator, result: ResponseResult) {
        let request = self.last_request();
        let data = serde_json::to_vec(&crate::api::schema::SuccessResponse {
            id: request.id.clone(),
            result,
        })
        .unwrap();
        let completed = self
            .commands
            .receive_chunk(
                &self.source.endpoint_id,
                self.source.generation,
                &self.source.boot_id,
                &request.id,
                true,
                data,
            )
            .unwrap()
            .unwrap();
        coordinator.receive_transfer(completed, &mut self.context());
    }

    fn reply_state(
        &mut self,
        coordinator: &mut SessionSwitchCoordinator,
        state: WorkspaceTransferState,
    ) {
        let transfer = self.info(coordinator, state);
        self.reply(coordinator, ResponseResult::WorkspaceTransfer { transfer });
    }

    fn source_usable(&mut self) {
        assert_eq!(self.endpoints.active_id(), &self.source.endpoint_id);
        assert!(self
            .endpoints
            .accepts(&self.source.endpoint_id, self.source.generation));
        assert!(self.endpoints.active_surface_available());
        assert!(!self.state.presentation_frozen);
        assert_eq!(
            self.endpoints
                .send(&ClientMessage::ClientShellFocus { focused: true }),
            endpoint::EndpointSendOutcome::Sent
        );
    }

    fn stage_follow(&mut self, coordinator: &mut SessionSwitchCoordinator) -> ClientEndpointId {
        let queued = coordinator.queued.take().unwrap();
        let endpoint_id = queued.key.fingerprint.endpoint_id(&queued.session).unwrap();
        assert!(self.state.shell.as_mut().unwrap().stage_session_endpoint(
            &self.source.endpoint_id.machine_id(),
            endpoint_id.clone(),
            queued.session.clone()
        ));
        self.endpoints.insert(
            endpoint_id.clone(),
            Capture(self.sent.clone()),
            97,
            negotiation(),
            false,
        );
        let mut snapshot = shell::test_snapshot();
        snapshot.boot_id = "destination-boot".into();
        snapshot.focused_workspace_id = Some("ws_destination".into());
        snapshot.focused_tab_id = Some("tab_destination".into());
        snapshot.focused_pane_id = Some("pane_destination".into());
        snapshot.workspaces[0].workspace_id = "ws_destination".into();
        snapshot.workspaces[0].active_tab_id = "tab_destination".into();
        snapshot.tabs[0].workspace_id = "ws_destination".into();
        snapshot.tabs[0].tab_id = "tab_destination".into();
        snapshot.panes[0].workspace_id = "ws_destination".into();
        snapshot.panes[0].tab_id = "tab_destination".into();
        snapshot.panes[0].pane_id = "pane_destination".into();
        self.state
            .shell
            .as_mut()
            .unwrap()
            .set_endpoint_snapshot_for_generation(&endpoint_id, 97, Box::new(snapshot));
        coordinator.preparation = Some(Preparation {
            key: queued.key,
            endpoint_id: endpoint_id.clone(),
            generation: 97,
            target: EndpointSessionTarget::Local(queued.session),
            staged: true,
            owns_connection: true,
            cancelled: Arc::new(AtomicBool::new(false)),
            phase: PreparationPhase::Metadata,
            deadline: Instant::now() + METADATA_TIMEOUT,
            follow: queued.follow,
            activation_serial: None,
        });
        endpoint_id
    }
}

#[test]
fn submission_is_async_uuid_keyed_and_status_is_polled_on_the_source() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    let request = harness.last_request();
    let Method::WorkspaceTransfer(params) = request.method else {
        panic!()
    };
    assert_eq!(params.workspace_id, harness.source.workspace_id);
    assert_eq!(params.destination_session, harness.destination);
    let parts = params
        .transfer_id
        .split('-')
        .map(str::len)
        .collect::<Vec<_>>();
    assert_eq!(parts, vec![8, 4, 4, 4, 12]);
    assert_eq!(&params.transfer_id[14..15], "4");
    assert!(matches!(
        params.transfer_id.as_bytes()[19],
        b'8' | b'9' | b'a' | b'b'
    ));
    assert!(coordinator.queued.is_none());
    assert!(coordinator.preparation.is_none());
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Pending);
    coordinator.transfer.as_mut().unwrap().next_poll = Instant::now();
    coordinator.drive_transfer(&mut harness.context());
    let request = harness.last_request();
    let Method::WorkspaceTransferStatus(status) = request.method else {
        panic!()
    };
    assert_eq!(status.transfer_id, params.transfer_id);
    let count = harness.sent.lock().unwrap().len();
    coordinator.drive_transfer(&mut harness.context());
    assert_eq!(harness.sent.lock().unwrap().len(), count);
    harness.source_usable();
}

#[test]
fn unsupported_submission_is_rejected_without_disconnection_or_destination_startup() {
    let mut harness = Harness::new(false);
    harness.endpoints =
        endpoint::EndpointRegistry::new(Capture(harness.sent.clone()), 41, Default::default());
    harness.sent.lock().unwrap().clear();
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    assert!(coordinator.transfer.is_none());
    assert!(coordinator.preparation.is_none());
    assert!(harness.sent.lock().unwrap().is_empty());
    let frame = harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .compose(100, 30)
        .unwrap();
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("does not support workspace moves"));
    harness.source_usable();
}

#[test]
fn missing_advertised_method_is_a_local_error_and_never_disconnects() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    let request = harness.last_request();
    let data = serde_json::to_vec(&crate::api::schema::ErrorResponse {
        id: request.id.clone(),
        error: crate::api::schema::ErrorBody {
            code: "unsupported_endpoint_command".into(),
            message: "workspace.transfer is not supported".into(),
        },
    })
    .unwrap();
    let completed = harness
        .commands
        .receive_chunk(
            &harness.source.endpoint_id,
            41,
            &harness.source.boot_id,
            &request.id,
            true,
            data,
        )
        .unwrap()
        .unwrap();
    coordinator.receive_transfer(completed, &mut harness.context());
    assert!(coordinator.transfer.is_none());
    assert!(coordinator.queued.is_none());
    harness.source_usable();
    let frame = harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .compose(100, 30)
        .unwrap();
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("Workspace move rejected"));
}

#[test]
fn backend_failed_transfer_preserves_source_and_does_not_activate_destination() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Failed);
    assert!(coordinator.transfer.is_none());
    assert!(coordinator.queued.is_none());
    assert!(coordinator.preparation.is_none());
    harness.source_usable();
    let frame = harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .compose(100, 30)
        .unwrap();
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("destination unavailable; source retained"));
}

#[test]
fn committed_active_move_follows_canonical_pane_tab_or_workspace_but_background_move_stays() {
    for focus in 0..3 {
        let mut harness = Harness::new(false);
        let mut coordinator = SessionSwitchCoordinator::default();
        harness.submit(&mut coordinator);
        let mut info = harness.info(&coordinator, WorkspaceTransferState::Committed);
        if focus > 0 {
            info.focused_pane_id = None;
        }
        if focus > 1 {
            info.focused_tab_id = None;
        }
        harness.reply(
            &mut coordinator,
            ResponseResult::WorkspaceTransfer { transfer: info },
        );
        assert!(coordinator.transfer.is_none());
        let queued = coordinator.queued.as_ref().unwrap();
        assert!(!queued.create);
        assert_eq!(queued.session, harness.destination);
        assert_eq!(
            queued.follow.as_ref().unwrap().target,
            match focus {
                0 => shell::ClientEndpointFocusTarget::Pane("pane_destination".into()),
                1 => shell::ClientEndpointFocusTarget::Tab("tab_destination".into()),
                _ => shell::ClientEndpointFocusTarget::Workspace("ws_destination".into()),
            }
        );
        harness.source_usable();
    }
    let mut harness = Harness::new(true);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Committed);
    assert!(coordinator.queued.is_none());
    assert!(!harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .workspace_transfer_picker_matches(harness.request_id, &harness.source));
    harness.source_usable();
}

#[test]
fn stale_source_cancels_only_the_unsent_transfer_and_keeps_the_existing_command_lane() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.commands.enqueue(
        harness.source.endpoint_id.clone(),
        harness.source.generation,
        harness.source.boot_id.clone(),
        Box::new(Request {
            id: "before-move".into(),
            method: Method::WorkspaceList(Default::default()),
        }),
    );
    harness
        .commands
        .send_next(&harness.source.endpoint_id, &mut harness.endpoints);
    harness.submit(&mut coordinator);
    let transfer_request = coordinator
        .transfer
        .as_ref()
        .unwrap()
        .outstanding
        .as_ref()
        .unwrap()
        .request_id
        .clone();
    coordinator
        .transfer
        .as_mut()
        .unwrap()
        .key
        .fingerprint
        .target = Some("changed@example.com".into());
    coordinator.drive_transfer(&mut harness.context());
    assert!(coordinator.transfer.is_none());
    assert!(!harness.commands.contains_request(
        &harness.source.endpoint_id,
        harness.source.generation,
        &harness.source.boot_id,
        &transfer_request
    ));
    assert!(harness.commands.contains_request(
        &harness.source.endpoint_id,
        harness.source.generation,
        &harness.source.boot_id,
        "before-move"
    ));
    assert_eq!(harness.last_request().id, "before-move");
    harness.source_usable();
}

#[test]
fn source_focus_reconciliation_after_removal_does_not_suppress_active_follow() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    let mut snapshot = shell::test_snapshot();
    snapshot.revision += 1;
    snapshot.workspaces.clear();
    snapshot.tabs.clear();
    snapshot.panes.clear();
    snapshot.focused_workspace_id = None;
    snapshot.focused_tab_id = None;
    snapshot.focused_pane_id = None;
    harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .set_endpoint_snapshot_for_generation(&harness.source.endpoint_id, 41, Box::new(snapshot));
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Committed);
    assert!(coordinator.queued.as_ref().unwrap().follow.is_some());
    harness.source_usable();
}

#[test]
fn navigation_generation_boot_and_profile_changes_never_hijack_the_current_view() {
    for change in 0..4 {
        let mut harness = Harness::new(false);
        let mut coordinator = SessionSwitchCoordinator::default();
        harness.submit(&mut coordinator);
        match change {
            0 => harness
                .state
                .shell
                .as_mut()
                .unwrap()
                .note_session_navigation(),
            1 => {
                harness.endpoints = endpoint::EndpointRegistry::new(
                    Capture(harness.sent.clone()),
                    42,
                    negotiation(),
                )
            }
            2 => {
                let mut snapshot = shell::test_snapshot();
                snapshot.boot_id = "new-boot".into();
                harness
                    .state
                    .shell
                    .as_mut()
                    .unwrap()
                    .set_endpoint_snapshot_for_generation(
                        &harness.source.endpoint_id,
                        41,
                        Box::new(snapshot),
                    );
            }
            3 => coordinator.latest_serial += 1,
            _ => unreachable!(),
        }
        harness.reply_state(&mut coordinator, WorkspaceTransferState::Committed);
        assert!(coordinator.queued.is_none());
        assert!(coordinator.preparation.is_none());
        assert_eq!(harness.endpoints.active_id(), &harness.source.endpoint_id);
    }
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    coordinator
        .transfer
        .as_mut()
        .unwrap()
        .key
        .fingerprint
        .target = Some("changed@example.com".into());
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Committed);
    assert!(coordinator.queued.is_none());
    harness.source_usable();
}

#[test]
fn duplicate_late_or_other_client_results_cannot_follow_or_finish_a_newer_request() {
    let mut first = Harness::new(false);
    let mut second = Harness::new(false);
    let mut first_coordinator = SessionSwitchCoordinator::default();
    let mut second_coordinator = SessionSwitchCoordinator::default();
    first.submit(&mut first_coordinator);
    second.submit(&mut second_coordinator);
    let first_request = first.last_request();
    let info = first.info(&first_coordinator, WorkspaceTransferState::Committed);
    second_coordinator.receive_transfer(
        endpoint_commands::EndpointCommandResult {
            endpoint_id: first.source.endpoint_id.clone(),
            generation: first.source.generation,
            boot_id: first.source.boot_id.clone(),
            request_id: first_request.id.clone(),
            result: Ok(ResponseResult::WorkspaceTransfer {
                transfer: info.clone(),
            }),
        },
        &mut second.context(),
    );
    assert!(second_coordinator.transfer.is_some());
    assert!(second_coordinator.queued.is_none());
    first.reply(
        &mut first_coordinator,
        ResponseResult::WorkspaceTransfer {
            transfer: info.clone(),
        },
    );
    assert!(first_coordinator.queued.is_some());
    first_coordinator.queued = None;
    first_coordinator.receive_transfer(
        endpoint_commands::EndpointCommandResult {
            endpoint_id: first.source.endpoint_id.clone(),
            generation: first.source.generation,
            boot_id: first.source.boot_id.clone(),
            request_id: first_request.id,
            result: Ok(ResponseResult::WorkspaceTransfer { transfer: info }),
        },
        &mut first.context(),
    );
    assert!(first_coordinator.queued.is_none());
    second.source_usable();
}

#[test]
fn timeout_or_retired_request_recovers_by_polling_the_same_transaction_not_resubmitting() {
    for retired in [false, true] {
        let mut harness = Harness::new(false);
        let mut coordinator = SessionSwitchCoordinator::default();
        harness.submit(&mut coordinator);
        let id = coordinator.transfer.as_ref().unwrap().transfer_id.clone();
        if retired {
            harness.commands.retire_lane(&harness.source.endpoint_id);
        } else {
            let expired = harness
                .commands
                .expire(Instant::now() + Duration::from_secs(61));
            assert_eq!(expired.len(), 1);
            coordinator
                .receive_transfer(expired.into_iter().next().unwrap(), &mut harness.context());
        }
        coordinator.transfer.as_mut().unwrap().next_poll = Instant::now();
        coordinator.drive_transfer(&mut harness.context());
        let Method::WorkspaceTransferStatus(params) = harness.last_request().method else {
            panic!()
        };
        assert_eq!(params.transfer_id, id);
        assert!(coordinator.queued.is_none());
        harness.source_usable();
    }
}

#[test]
fn committed_follow_uses_existing_activation_and_the_returned_focus_target() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Committed);
    let endpoint_id = harness.stage_follow(&mut coordinator);
    coordinator.drive(&mut harness.context()).unwrap();
    let source_off = harness.last_request();
    assert!(
        matches!(source_off.method, Method::ClientShellSurfaceSet(ref params) if !params.active)
    );
    let data = serde_json::to_vec(&crate::api::schema::SuccessResponse {
        id: source_off.id.clone(),
        result: ResponseResult::ClientShellSurfaceSet {
            active: false,
            projection_revision: 2,
        },
    })
    .unwrap();
    let progress = harness.activation.as_mut().unwrap().receive_response(
        &harness.source.endpoint_id,
        41,
        &source_off.id,
        &data,
        &mut harness.endpoints,
    );
    assert!(matches!(
        progress,
        endpoint::SurfaceActivationProgress::Pending
    ));
    assert_eq!(harness.activation.as_ref().unwrap().target(), &endpoint_id);
    assert!(
        matches!(harness.last_request().method, Method::PaneFocus(ref params) if params.pane_id == "pane_destination")
    );
}

#[test]
fn committed_follow_is_fenced_again_after_preparation_and_old_activation_completion() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Committed);
    let endpoint_id = harness.stage_follow(&mut coordinator);
    harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .note_session_navigation();
    coordinator.drive(&mut harness.context()).unwrap();
    assert!(coordinator.preparation.is_none());
    assert!(harness.activation.is_none());
    harness.source_usable();

    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Committed);
    let current_endpoint = harness.stage_follow(&mut coordinator);
    assert_eq!(current_endpoint, endpoint_id);
    coordinator.drive(&mut harness.context()).unwrap();
    let serial = coordinator
        .preparation
        .as_ref()
        .unwrap()
        .activation_serial
        .unwrap();
    harness.state.activation_completion = Some((current_endpoint, 97, serial + 1, Ok(())));
    coordinator.drive(&mut harness.context()).unwrap();
    assert!(coordinator.preparation.is_some());
    assert_eq!(harness.endpoints.active_id(), &harness.source.endpoint_id);
}

#[test]
fn transfer_discovery_and_follow_stay_on_the_source_machine_for_local_ssh_and_standalone_remote() {
    let profile = SavedSshEndpoint::new("Build", "dev@example.com", "initial").unwrap();
    for mode in 0..3 {
        let mut harness =
            Harness::with_machine(false, (mode == 1).then(|| profile.clone()), mode == 2);
        let mut coordinator = SessionSwitchCoordinator::default();
        harness.submit(&mut coordinator);
        let key = &coordinator.transfer.as_ref().unwrap().key;
        assert_eq!(
            key.fingerprint.machine,
            harness.source.endpoint_id.machine_id()
        );
        if mode == 0 {
            assert_eq!(key.fingerprint.target, None);
        } else {
            assert_eq!(key.fingerprint.target.as_deref(), Some("dev@example.com"));
            let EndpointSessionTarget::Ssh(target) = key
                .fingerprint
                .connection_target(&harness.destination, &coordinator.root_profile)
            else {
                panic!("remote transfer attached to the physical Local machine")
            };
            assert_eq!(target.target, "dev@example.com");
            assert_eq!(target.session, harness.destination);
        }
        harness.reply_state(&mut coordinator, WorkspaceTransferState::Committed);
        let follow = coordinator.queued.as_ref().unwrap();
        assert_eq!(
            follow.key.fingerprint.machine,
            harness.source.endpoint_id.machine_id()
        );
        assert_eq!(follow.session, harness.destination);
        harness.source_usable();
    }
}

#[test]
fn deferred_ssh_profile_edits_fence_transfer_polling_and_late_commit_without_touching_source() {
    for change in ["target", "session", "disable", "remove"] {
        let profile = SavedSshEndpoint::new("Build", "dev@example.com", "initial").unwrap();
        let mut harness = Harness::with_machine(false, Some(profile.clone()), false);
        let mut coordinator = SessionSwitchCoordinator::default();
        harness.submit(&mut coordinator);
        let info = harness.info(&coordinator, WorkspaceTransferState::Committed);
        let mut profiles = vec![profile];
        match change {
            "target" => profiles[0].target = "other@example.com".into(),
            "session" => profiles[0].session = "different".into(),
            "disable" => profiles[0].enabled = false,
            "remove" => profiles.clear(),
            _ => unreachable!(),
        }
        coordinator.catalog_changed(&profiles, &mut harness.context());
        coordinator.drive_transfer(&mut harness.context());
        assert!(coordinator.transfer.is_none());
        harness.reply(
            &mut coordinator,
            ResponseResult::WorkspaceTransfer { transfer: info },
        );
        assert!(coordinator.queued.is_none());
        assert!(coordinator.preparation.is_none());
        harness.source_usable();
    }
}

#[test]
fn source_disconnect_after_committed_activation_starts_does_not_cancel_the_healthy_destination() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Committed);
    let destination = harness.stage_follow(&mut coordinator);
    coordinator.drive(&mut harness.context()).unwrap();
    let rollback = harness.activation.as_mut().unwrap().endpoint_disconnected(
        &mut harness.endpoints,
        &harness.source.endpoint_id,
        "source disconnected".into(),
    );
    assert!(matches!(rollback, endpoint::ActivationRollback::Pending));
    harness.endpoints.disconnect(&harness.source.endpoint_id);
    harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .mark_endpoint_disconnected(&harness.source.endpoint_id);
    let before = harness.sent.lock().unwrap().len();
    coordinator.drive(&mut harness.context()).unwrap();
    assert!(coordinator.preparation.is_some());
    assert_eq!(harness.activation.as_ref().unwrap().target(), &destination);
    assert!(!harness.activation.as_ref().unwrap().restoring_source());
    assert_eq!(harness.sent.lock().unwrap().len(), before);
}

#[test]
fn retiring_a_queued_never_sent_submission_does_not_poll_a_nonexistent_transfer() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.commands.enqueue(
        harness.source.endpoint_id.clone(),
        harness.source.generation,
        harness.source.boot_id.clone(),
        Box::new(Request {
            id: "before-move".into(),
            method: Method::WorkspaceList(Default::default()),
        }),
    );
    harness
        .commands
        .send_next(&harness.source.endpoint_id, &mut harness.endpoints);
    harness.submit(&mut coordinator);
    assert!(!coordinator.transfer.as_ref().unwrap().submission_started);
    harness.commands.retire_lane(&harness.source.endpoint_id);
    coordinator.drive_transfer(&mut harness.context());
    assert!(coordinator.transfer.is_none());
    assert!(coordinator.queued.is_none());
    assert_eq!(harness.last_request().id, "before-move");
    harness.source_usable();
}

#[test]
fn a_retired_sent_submission_is_not_misreported_as_source_unmodified() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    coordinator.transfer.as_mut().unwrap().submission_started = false;
    harness.commands.retire_lane(&harness.source.endpoint_id);
    coordinator.drive_transfer(&mut harness.context());
    assert!(coordinator.transfer.as_ref().unwrap().submission_started);
    assert!(coordinator.queued.is_none());
    harness.source_usable();
}

#[test]
fn missing_status_record_finishes_with_an_unknown_outcome_instead_of_trapping_the_picker() {
    let mut harness = Harness::new(false);
    let mut coordinator = SessionSwitchCoordinator::default();
    harness.submit(&mut coordinator);
    harness.reply_state(&mut coordinator, WorkspaceTransferState::Pending);
    coordinator.transfer.as_mut().unwrap().next_poll = Instant::now();
    coordinator.drive_transfer(&mut harness.context());
    let request = harness.last_request();
    let data = serde_json::to_vec(&crate::api::schema::ErrorResponse {
        id: request.id.clone(),
        error: crate::api::schema::ErrorBody {
            code: "not_found".into(),
            message: "workspace transfer was not found on this source endpoint".into(),
        },
    })
    .unwrap();
    let completed = harness
        .commands
        .receive_chunk(
            &harness.source.endpoint_id,
            41,
            &harness.source.boot_id,
            &request.id,
            true,
            data,
        )
        .unwrap()
        .unwrap();
    coordinator.receive_transfer(completed, &mut harness.context());
    assert!(coordinator.transfer.is_none());
    assert!(coordinator.queued.is_none());
    let outcome = harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .handle_raw_events(vec![crate::raw_input::RawInputEvent::Key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::empty(),
            ),
        )]);
    assert!(outcome.actions.is_empty());
    assert!(!harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .workspace_transfer_picker_matches(harness.request_id, &harness.source));
    harness.source_usable();
}
