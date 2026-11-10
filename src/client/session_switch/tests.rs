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
}

impl Harness {
    fn new() -> Self {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut supervisors = endpoint::EndpointSupervisors::new(&[], Instant::now());
        supervisors.add_local("source.sock".into(), Some(41), Instant::now());
        Self {
            state: ClientState::test_new(),
            endpoints: endpoint::EndpointRegistry::new(
                Capture(sent.clone()),
                41,
                Default::default(),
            ),
            commands: Default::default(),
            supervisors,
            catalog: Default::default(),
            activation: None,
            next_surface_serial: 1,
            event_tx: tokio::sync::mpsc::channel(4).0,
            sent,
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

    fn source_is_usable(&mut self) {
        assert_eq!(self.endpoints.active_id(), &ClientEndpointId::Local);
        assert!(self.endpoints.accepts(&ClientEndpointId::Local, 41));
        assert!(self.endpoints.active_surface_available());
        assert!(!self.state.presentation_frozen);
        assert_eq!(
            self.endpoints
                .send(&ClientMessage::ClientShellFocus { focused: true }),
            endpoint::EndpointSendOutcome::Sent
        );
        assert!(!self.sent.lock().unwrap().is_empty());
    }
}

fn stage(
    harness: &mut Harness,
    coordinator: &mut SessionSwitchCoordinator,
    request_id: u64,
) -> SessionRequestKey {
    let fingerprint = MachineFingerprint::read(
        &ClientEndpointId::Local,
        &harness.catalog,
        harness.state.shell.as_ref().unwrap(),
    )
    .unwrap();
    let session = if fingerprint.configured_session == "candidate" {
        "other"
    } else {
        "candidate"
    };
    let endpoint_id = fingerprint.endpoint_id(session).unwrap();
    assert!(harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .stage_session_endpoint(
            &ClientEndpointId::Local,
            endpoint_id.clone(),
            session.to_owned()
        ));
    let key = SessionRequestKey {
        request_id,
        fingerprint,
    };
    coordinator.preparation = Some(Preparation {
        key: key.clone(),
        endpoint_id,
        generation: 97,
        target: EndpointSessionTarget::Local(session.to_owned()),
        staged: true,
        owns_connection: true,
        allow_start: true,
        cancelled: Arc::new(AtomicBool::new(false)),
        phase: PreparationPhase::Preparing,
        deadline: Instant::now() + PREPARATION_TIMEOUT,
        follow: None,
        activation_serial: None,
    });
    coordinator.latest_serial = request_id;
    key
}

#[test]
fn failed_or_cancelled_session_preparation_preserves_the_live_source() {
    for cancelled in [false, true] {
        let mut harness = Harness::new();
        let mut coordinator = SessionSwitchCoordinator::default();
        let key = stage(&mut harness, &mut coordinator, 7);
        let cancellation = coordinator.preparation.as_ref().unwrap().cancelled.clone();
        if cancelled {
            coordinator
                .actions(
                    vec![SessionAction::Cancel { request_id: 7 }],
                    &mut harness.context(),
                )
                .unwrap();
        } else {
            coordinator
                .receive(
                    SessionSwitchEvent::Prepared {
                        key,
                        generation: 97,
                        result: Err("target unavailable".into()),
                    },
                    &mut harness.context(),
                )
                .unwrap();
        }
        assert!(!coordinator.busy());
        assert!(cancellation.load(Ordering::Acquire));
        assert!(!harness
            .state
            .shell
            .as_ref()
            .unwrap()
            .multi_endpoint_active());
        harness.source_is_usable();
    }
}

#[test]
fn stale_preparation_results_cannot_retire_a_newer_candidate() {
    let mut harness = Harness::new();
    let mut coordinator = SessionSwitchCoordinator::default();
    let key = stage(&mut harness, &mut coordinator, 9);
    let mut stale_key = key.clone();
    stale_key.request_id = 8;
    for (key, generation) in [(stale_key, 97), (key.clone(), 96)] {
        coordinator
            .receive(
                SessionSwitchEvent::Prepared {
                    key,
                    generation,
                    result: Err("late failure".into()),
                },
                &mut harness.context(),
            )
            .unwrap();
        assert_eq!(coordinator.preparation.as_ref().unwrap().key.request_id, 9);
        assert!(!coordinator
            .preparation
            .as_ref()
            .unwrap()
            .cancelled
            .load(Ordering::Acquire));
        harness.source_is_usable();
    }
    coordinator.cancel(9, &mut harness.context());
    coordinator
        .receive(
            SessionSwitchEvent::Prepared {
                key,
                generation: 97,
                result: Err("late cancelled completion".into()),
            },
            &mut harness.context(),
        )
        .unwrap();
    assert!(!coordinator.busy());
    harness.source_is_usable();
}

#[test]
fn current_live_session_is_a_noop_and_inactive_current_binding_reuses_its_transport() {
    let mut harness = Harness::new();
    let mut coordinator = SessionSwitchCoordinator::default();
    let fingerprint = MachineFingerprint::read(
        &ClientEndpointId::Local,
        &harness.catalog,
        harness.state.shell.as_ref().unwrap(),
    )
    .unwrap();
    coordinator.start(
        QueuedOpen {
            key: SessionRequestKey {
                request_id: 1,
                fingerprint: fingerprint.clone(),
            },
            session: fingerprint.configured_session,
            create: false,
            follow: None,
        },
        &mut harness.context(),
    );
    assert!(!coordinator.busy());
    harness.source_is_usable();

    let profile = SavedSshEndpoint::new("Build", "dev@example.com", "initial").unwrap();
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    harness.catalog.ssh.push(profile.clone());
    harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .set_endpoint_catalog(&harness.catalog.ssh);
    harness.endpoints.insert(
        remote.clone(),
        Capture(Arc::new(Mutex::new(Vec::new()))),
        73,
        Default::default(),
        false,
    );
    let fingerprint = MachineFingerprint::read(
        &remote,
        &harness.catalog,
        harness.state.shell.as_ref().unwrap(),
    )
    .unwrap();
    coordinator.start(
        QueuedOpen {
            key: SessionRequestKey {
                request_id: 2,
                fingerprint,
            },
            session: "initial".into(),
            create: false,
            follow: None,
        },
        &mut harness.context(),
    );
    let preparation = coordinator.preparation.as_ref().unwrap();
    assert_eq!(preparation.endpoint_id, remote);
    assert_eq!(preparation.generation, 73);
    assert!(!preparation.staged);
    assert!(!preparation.owns_connection);
    coordinator.cancel(2, &mut harness.context());
    assert!(harness.endpoints.accepts(&remote, 73));
    assert_eq!(
        harness
            .state
            .shell
            .as_ref()
            .unwrap()
            .visible_endpoint_for_machine(&remote)
            .unwrap()
            .endpoint_id,
        remote
    );
    harness.source_is_usable();
}

#[test]
fn invalid_session_preflight_does_not_stage_a_candidate_or_touch_the_source() {
    let mut harness = Harness::new();
    let mut coordinator = SessionSwitchCoordinator::default();
    let fingerprint = MachineFingerprint::read(
        &ClientEndpointId::Local,
        &harness.catalog,
        harness.state.shell.as_ref().unwrap(),
    )
    .unwrap();
    coordinator.start(
        QueuedOpen {
            key: SessionRequestKey {
                request_id: 1,
                fingerprint,
            },
            session: "../invalid".into(),
            create: true,
            follow: None,
        },
        &mut harness.context(),
    );
    assert!(!coordinator.busy());
    assert!(!harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .multi_endpoint_active());
    harness.source_is_usable();
}

#[test]
fn initial_session_ids_are_reused_instead_of_creating_cache_aliases() {
    let fingerprint = MachineFingerprint {
        machine: ClientEndpointId::Local,
        target: None,
        configured_session: "initial".into(),
    };
    assert_eq!(
        fingerprint.endpoint_id("initial").unwrap(),
        ClientEndpointId::Local
    );
    assert_eq!(
        fingerprint.endpoint_id("default").unwrap(),
        ClientEndpointId::Local.with_session("default").unwrap()
    );
    assert!(fingerprint.endpoint_id("../bad").is_err());
}

#[test]
fn root_remote_identity_uses_remote_discovery_and_transport_without_environment_mutation() {
    let config = shell::ClientShellConfig::from_config(&crate::config::Config::default())
        .with_endpoint_preferences(
            std::path::Path::new("unrelated.sock"),
            Some(r#"["dev@example.com", "named"]"#),
        );
    let shell = shell::ClientShellState::new(config);
    let fingerprint =
        MachineFingerprint::read(&ClientEndpointId::Local, &Default::default(), &shell).unwrap();
    assert_eq!(fingerprint.target.as_deref(), Some("dev@example.com"));
    assert_eq!(fingerprint.configured_session, "named");
    assert_eq!(
        fingerprint.endpoint_id("named").unwrap(),
        ClientEndpointId::Local
    );
    let root = ProfileId::generate();
    let EndpointSessionTarget::Ssh(target) = fingerprint.connection_target("default", &root) else {
        panic!("standalone remote switched to a physical Local server");
    };
    assert_eq!(target.id, root);
    assert_eq!(target.target, "dev@example.com");
    assert_eq!(target.session, "default");
}

#[test]
fn deferred_catalog_changes_fence_new_requests_until_the_destination_is_applied() {
    for change in ["target", "session", "disable", "remove"] {
        let mut harness = Harness::new();
        let profile = SavedSshEndpoint::new("Build", "dev@example.com", "initial").unwrap();
        harness.catalog.ssh.push(profile.clone());
        let machine = ClientEndpointId::Ssh(profile.id.clone());
        let mut coordinator = SessionSwitchCoordinator::default();
        stage(&mut harness, &mut coordinator, 1);
        let mut updated = profile;
        match change {
            "target" => updated.target = "other@example.com".into(),
            "session" => updated.session = "other".into(),
            "disable" => updated.enabled = false,
            "remove" => {}
            _ => unreachable!(),
        }
        let profiles = if change == "remove" {
            Vec::new()
        } else {
            vec![updated]
        };
        coordinator.catalog_changed(&profiles, &mut harness.context());
        assert!(coordinator
            .fingerprint(&machine, &harness.context())
            .is_err());
        coordinator
            .actions(
                vec![
                    SessionAction::List {
                        machine: machine.clone(),
                        request_id: 2,
                    },
                    SessionAction::Open {
                        machine: machine.clone(),
                        request_id: 3,
                        session: "new-work".into(),
                        create: true,
                    },
                ],
                &mut harness.context(),
            )
            .unwrap();
        assert!(coordinator.discovery.is_none());
        assert!(coordinator.queued.is_none());
        assert_eq!(coordinator.preparation.as_ref().unwrap().key.request_id, 1);
        harness.source_is_usable();
        harness.catalog.ssh = profiles;
        if matches!(change, "target" | "session") {
            assert!(coordinator
                .fingerprint(&machine, &harness.context())
                .is_ok());
        }
        coordinator
            .actions(
                vec![SessionAction::Cancel { request_id: 1 }],
                &mut harness.context(),
            )
            .unwrap();
        harness.source_is_usable();
    }
}

#[test]
fn catalog_label_changes_preserve_discovery_but_destination_edits_cancel_it() {
    let mut harness = Harness::new();
    let mut profile = SavedSshEndpoint::new("Build", "dev@example.com", "initial").unwrap();
    harness.catalog.ssh.push(profile.clone());
    let machine = ClientEndpointId::Ssh(profile.id.clone());
    let key = SessionRequestKey {
        request_id: 1,
        fingerprint: MachineFingerprint::read(
            &machine,
            &harness.catalog,
            harness.state.shell.as_ref().unwrap(),
        )
        .unwrap(),
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut coordinator = SessionSwitchCoordinator::default();
    coordinator.discovery = Some(Discovery {
        key,
        cancelled: cancelled.clone(),
    });
    profile.label = "Renamed".into();
    coordinator.catalog_changed(&[profile.clone()], &mut harness.context());
    assert!(!cancelled.load(Ordering::Acquire));
    assert!(coordinator.discovery.is_some());
    profile.target = "other@example.com".into();
    coordinator.catalog_changed(&[profile], &mut harness.context());
    assert!(cancelled.load(Ordering::Acquire));
    assert!(coordinator.discovery.is_none());
    harness.source_is_usable();
}

#[test]
fn existing_only_preparation_failure_never_schedules_a_start_capable_retry() {
    for allow_start in [false, true] {
        let mut harness = Harness::new();
        let mut coordinator = SessionSwitchCoordinator::default();
        stage(&mut harness, &mut coordinator, 1);
        let mut preparation = coordinator.preparation.take().unwrap();
        preparation.staged = false;
        preparation.allow_start = allow_start;
        let endpoint_id = preparation.endpoint_id.clone();
        harness.context().discard(preparation);
        assert_eq!(
            harness.supervisors.retry_now(&endpoint_id, Instant::now()),
            allow_start
        );
        harness.source_is_usable();
    }
}

#[test]
fn existing_only_connections_remain_busy_and_unadopted_until_successful_history_focus() {
    for outcome in ["success", "failure", "cancel", "disconnect"] {
        let mut harness = Harness::new();
        let shell = harness.state.shell.as_mut().unwrap();
        shell.set_endpoint_snapshot_for_generation(
            &ClientEndpointId::Local,
            41,
            Box::new(shell::test_snapshot()),
        );
        shell.set_endpoint_status(
            &ClientEndpointId::Local,
            endpoint::ClientEndpointStatus::Online,
        );
        let session = shell.initial_session_name().to_owned();
        let input = shell.handle_raw_events(vec![crate::raw_input::RawInputEvent::Key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Char('f'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        )]);
        let shell::ClientShellAction::History {
            request_id: history_request,
            ..
        } = input.actions.into_iter().next().unwrap()
        else {
            panic!("history ownership");
        };
        let mut coordinator = SessionSwitchCoordinator::default();
        coordinator
            .open_history(
                history_request,
                &ClientEndpointId::Local,
                &session,
                false,
                &mut harness.context(),
            )
            .unwrap();
        let session_request = coordinator.queued.take().unwrap().key.request_id;
        harness.supervisors.remove(&ClientEndpointId::Local);
        coordinator.retain_history_connection(
            ClientEndpointId::Local,
            EndpointSessionTarget::Local(session),
            None,
            41,
        );
        coordinator.finish(&mut harness.context(), session_request, Ok(()));
        assert!(coordinator.busy());
        assert_eq!(
            coordinator.take_history_result(),
            Some((history_request, Ok(())))
        );
        assert!(!harness
            .supervisors
            .disconnected(&ClientEndpointId::Local, 41, Instant::now()));
        match outcome {
            "cancel" => coordinator.cancel_history(history_request, &mut harness.context()),
            "disconnect" => {
                harness.endpoints.disconnect(&ClientEndpointId::Local);
                coordinator.finish_history(history_request, true, &mut harness.context());
            }
            "success" => coordinator.finish_history(history_request, true, &mut harness.context()),
            "failure" => coordinator.finish_history(history_request, false, &mut harness.context()),
            _ => unreachable!(),
        }
        assert!(!coordinator.busy());
        assert_eq!(
            harness
                .supervisors
                .disconnected(&ClientEndpointId::Local, 41, Instant::now()),
            outcome == "success"
        );
    }
}
