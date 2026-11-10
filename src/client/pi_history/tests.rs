use super::*;
use crate::client::{endpoint, endpoint_commands, shell, ClientLoopEvent, ClientState};
use crate::input::TerminalKey;
use crate::pi_history::types::{HistoryMatch, HistoryResult};
use crate::protocol::ClientMessage;
use crossterm::event::{KeyCode, KeyModifiers};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone)]
pub(super) struct Capture(pub(super) Arc<Mutex<Vec<ClientMessage>>>);

impl endpoint::EndpointTransport for Capture {
    fn send(&mut self, message: &ClientMessage) -> io::Result<()> {
        self.0.lock().unwrap().push(message.clone());
        Ok(())
    }
}

pub(super) struct Harness {
    pub(super) state: ClientState,
    pub(super) endpoints: endpoint::EndpointRegistry,
    pub(super) commands: endpoint_commands::EndpointCommands,
    supervisors: endpoint::EndpointSupervisors,
    pub(super) catalog: endpoint::EndpointCatalog,
    activation: Option<endpoint::PendingEndpointActivation>,
    next_surface_serial: u64,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    pub(super) sent: Arc<Mutex<Vec<ClientMessage>>>,
}

impl Harness {
    pub(super) fn new() -> Self {
        let mut state = ClientState::test_new();
        let shell = state.shell.as_mut().unwrap();
        shell.set_endpoint_snapshot_for_generation(
            &ClientEndpointId::Local,
            41,
            Box::new(shell::test_snapshot()),
        );
        shell.set_endpoint_status(
            &ClientEndpointId::Local,
            endpoint::ClientEndpointStatus::Online,
        );
        let sent = Arc::new(Mutex::new(Vec::new()));
        Self {
            state,
            endpoints: endpoint::EndpointRegistry::new(
                Capture(Arc::clone(&sent)),
                41,
                endpoint::EndpointNegotiation::new(vec!["pane.focus_if_session".into()], vec![]),
            ),
            commands: Default::default(),
            supervisors: endpoint::EndpointSupervisors::new(&[], Instant::now()),
            catalog: Default::default(),
            activation: None,
            next_surface_serial: 1,
            event_tx: tokio::sync::mpsc::channel(4).0,
            sent,
        }
    }

    pub(super) fn context(&mut self) -> SessionSwitchContext<'_> {
        let options = crate::client::session_switch::connect_options(&self.state, false).unwrap();
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

    pub(super) fn key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> shell::ClientShellInput {
        self.state.shell.as_mut().unwrap().handle_raw_events(vec![
            crate::raw_input::RawInputEvent::Key(TerminalKey::new(code, modifiers)),
        ])
    }

    pub(super) fn session(&self) -> String {
        self.state
            .shell
            .as_ref()
            .unwrap()
            .history_navigation_view()
            .unwrap()
            .session
    }

    pub(super) fn open(&mut self) -> (u64, HistoryRequest) {
        let outcome = self.key(KeyCode::Char('f'), KeyModifiers::CONTROL);
        match outcome.actions.into_iter().next().unwrap() {
            shell::ClientShellAction::History {
                request_id,
                request,
                ..
            } => (request_id, *request),
            other => panic!("expected history, got {other:?}"),
        }
    }

    pub(super) fn result(&mut self, request_id: u64, location: HistoryLocation) {
        self.state.shell.as_mut().unwrap().receive_history_event(
            &ClientEndpointId::Local,
            request_id,
            HistoryEvent::Result {
                result: HistoryResult {
                    source: source(location),
                    matches: vec![HistoryMatch {
                        entry_line: 1,
                        role: "user".into(),
                        excerpt: "synthetic needle".into(),
                    }],
                    truncated: false,
                },
            },
        );
    }
}

pub(super) fn coordinator() -> HistoryCoordinator {
    HistoryCoordinator::new(tokio::sync::mpsc::channel(8).0)
}

pub(super) fn location(session: &str, running: bool) -> HistoryLocation {
    HistoryLocation {
        session: session.into(),
        workspace_id: "ws_1".into(),
        workspace_label: "Synthetic".into(),
        tab_id: "tab_1".into(),
        tab_label: "Synthetic".into(),
        pane_id: "pane_1".into(),
        pane_label: None,
        terminal_id: running.then(|| "fresh-terminal".into()),
        running,
        agent_session: crate::api::schema::AgentSessionInfo {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: "/synthetic/pi.jsonl".into(),
        },
    }
}

pub(super) fn source(location: HistoryLocation) -> HistorySource {
    HistorySource {
        id: "synthetic-source".into(),
        name: "Synthetic".into(),
        locations: vec![location],
    }
}

fn worker(key: RequestKey, lane: Lane) -> Worker {
    Worker {
        key,
        lane,
        cancelled: Arc::new(AtomicBool::new(false)),
        discarded: false,
        resolved: None,
        problem: None,
    }
}

#[test]
fn query_changes_cancel_both_workers_but_keep_bounded_slots_until_completion() {
    let mut harness = Harness::new();
    let mut coordinator = coordinator();
    let mut sessions = SessionSwitchCoordinator::default();
    let (search, _) = harness.open();
    harness.result(search, location(&harness.session(), true));
    let preview = harness.key(KeyCode::Enter, KeyModifiers::empty());
    let shell::ClientShellAction::History {
        request_id: preview,
        ..
    } = preview.actions.into_iter().next().unwrap()
    else {
        panic!("preview");
    };
    let search_key = coordinator
        .key(&ClientEndpointId::Local, search, &harness.context())
        .unwrap();
    let preview_key = coordinator
        .key(&ClientEndpointId::Local, preview, &harness.context())
        .unwrap();
    coordinator.workers = vec![
        worker(search_key.clone(), Lane::Search),
        worker(preview_key, Lane::Detail),
    ];
    let old = coordinator
        .workers
        .iter()
        .map(|worker| Arc::clone(&worker.cancelled))
        .collect::<Vec<_>>();
    harness.key(KeyCode::Esc, KeyModifiers::empty());
    let changed = harness.key(KeyCode::Char('x'), KeyModifiers::empty());
    let shell::ClientShellAction::History {
        request_id,
        request,
        ..
    } = changed
        .actions
        .into_iter()
        .find(|action| matches!(action, shell::ClientShellAction::History { .. }))
        .unwrap()
    else {
        unreachable!();
    };
    let key = coordinator
        .key(&ClientEndpointId::Local, request_id, &harness.context())
        .unwrap();
    for _ in 0..20 {
        coordinator.queue(key.clone(), (*request).clone());
    }
    coordinator
        .drive(&mut sessions, &mut harness.context())
        .unwrap();
    assert!(old
        .iter()
        .all(|cancelled| cancelled.load(Ordering::Acquire)));
    assert_eq!(coordinator.workers.len(), 2);
    assert_eq!(coordinator.pending.len(), 1);
    assert!(coordinator.workers.iter().all(|worker| worker.discarded));
    coordinator.pending.clear();
    coordinator
        .receive(
            HistoryWorkerEvent {
                key: search_key,
                payload: WorkerPayload::Finished(Ok(())),
            },
            &mut sessions,
            &mut harness.context(),
        )
        .unwrap();
    assert_eq!(coordinator.workers.len(), 1);
    assert!(harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .history_request_is_current(&ClientEndpointId::Local, request_id));
}

#[test]
fn stale_worker_serials_and_unrelated_cancellation_do_not_take_current_ownership() {
    let mut harness = Harness::new();
    let mut coordinator = coordinator();
    let mut sessions = SessionSwitchCoordinator::default();
    let (request_id, _) = harness.open();
    let key = coordinator
        .key(&ClientEndpointId::Local, request_id, &harness.context())
        .unwrap();
    coordinator.workers.push(worker(key.clone(), Lane::Search));
    coordinator.cancel_worker(request_id + 100);
    let mut stale = key.clone();
    stale.serial += 1;
    coordinator
        .receive(
            HistoryWorkerEvent {
                key: stale,
                payload: WorkerPayload::Event(HistoryEvent::Done { truncated: false }),
            },
            &mut sessions,
            &mut harness.context(),
        )
        .unwrap();
    assert!(!coordinator.workers[0].cancelled.load(Ordering::Acquire));
    assert!(harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .history_request_is_current(&ClientEndpointId::Local, request_id));
    coordinator
        .receive(
            HistoryWorkerEvent {
                key,
                payload: WorkerPayload::Finished(Err("synthetic missing Done".into())),
            },
            &mut sessions,
            &mut harness.context(),
        )
        .unwrap();
    assert!(!harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .history_request_is_current(&ClientEndpointId::Local, request_id));
    assert!(harness.sent.lock().unwrap().is_empty());
}

#[test]
fn machine_fingerprints_preserve_ssh_identity_and_remote_local_root_target() {
    let mut harness = Harness::new();
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Synthetic".into(),
        target: "user@synthetic.invalid".into(),
        session: "configured".into(),
        enabled: true,
    };
    harness.catalog.ssh = vec![profile.clone()];
    let machine = ClientEndpointId::Ssh(profile.id.clone())
        .with_session("another")
        .unwrap();
    let fingerprint = MachineFingerprint::read(
        &machine,
        &harness.catalog,
        harness.state.shell.as_ref().unwrap(),
    )
    .unwrap();
    assert_eq!(
        fingerprint.machine,
        ClientEndpointId::Ssh(profile.id.clone())
    );
    assert_eq!(
        fingerprint.target.as_deref(),
        Some("user@synthetic.invalid")
    );
    assert_eq!(fingerprint.configured_session, "configured");
    let mut changed = profile.clone();
    changed.target = "different.invalid".into();
    assert!(!matches_profiles(&fingerprint, &[changed]));
    let mut renamed = profile;
    renamed.label = "Renamed".into();
    assert!(matches_profiles(&fingerprint, &[renamed]));
    let config = shell::ClientShellConfig::from_config(&crate::config::Config::default())
        .with_endpoint_preferences(
            std::path::Path::new("/synthetic/client.sock"),
            Some("[\"root.invalid\",\"root-session\"]"),
        );
    let shell = shell::ClientShellState::new(config);
    let root =
        MachineFingerprint::read(&ClientEndpointId::Local, &harness.catalog, &shell).unwrap();
    assert_eq!(root.target.as_deref(), Some("root.invalid"));
    assert_eq!(root.configured_session, "root-session");
    assert_eq!(root.machine, ClientEndpointId::Local);
}

#[test]
fn external_modal_replacement_and_shutdown_cancel_only_owned_history_workers() {
    let mut harness = Harness::new();
    let mut coordinator = coordinator();
    let mut sessions = SessionSwitchCoordinator::default();
    let (request_id, _) = harness.open();
    let key = coordinator
        .key(&ClientEndpointId::Local, request_id, &harness.context())
        .unwrap();
    coordinator.workers.push(worker(key, Lane::Search));
    let cancelled = Arc::clone(&coordinator.workers[0].cancelled);
    harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .open_session_picker(&mut shell::ClientShellInput::default());
    coordinator
        .drive(&mut sessions, &mut harness.context())
        .unwrap();
    assert!(cancelled.load(Ordering::Acquire));
    let outcome = harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .history_navigation_view()
        .unwrap();
    drop(coordinator);
    assert!(harness.endpoints.active_surface_available());
    assert_eq!(
        harness
            .state
            .shell
            .as_ref()
            .unwrap()
            .history_navigation_view()
            .unwrap(),
        outcome
    );
    let mut coordinator = self::coordinator();
    let key = RequestKey {
        serial: 1,
        request_id: 50,
        fingerprint: MachineFingerprint::read(
            &ClientEndpointId::Local,
            &harness.catalog,
            harness.state.shell.as_ref().unwrap(),
        )
        .unwrap(),
    };
    coordinator.workers.push(worker(key, Lane::Search));
    let cancelled = Arc::clone(&coordinator.workers[0].cancelled);
    drop(coordinator);
    assert!(cancelled.load(Ordering::Acquire));
    assert!(harness.sent.lock().unwrap().is_empty());
}

#[test]
fn history_session_allocator_does_not_touch_navigation_or_picker_ownership() {
    let mut harness = Harness::new();
    let before = harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .history_navigation_view()
        .unwrap();
    let first = harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .allocate_history_session_request();
    let second = harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .allocate_history_session_request();
    let mut input = shell::ClientShellInput::default();
    harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .open_session_picker(&mut input);
    let shell::ClientShellAction::SessionList { request_id, .. } =
        input.actions.into_iter().next().unwrap()
    else {
        panic!("session list");
    };
    assert!(first < second && second < request_id);
    assert!(!harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .finish_session_switch(second, Ok(())));
    assert_eq!(
        harness
            .state
            .shell
            .as_ref()
            .unwrap()
            .history_navigation_view()
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn saturated_history_and_runtime_channels_are_interleaved_for_cancellation_and_responses() {
    let mut harness = Harness::new();
    let key = RequestKey {
        serial: 1,
        request_id: 1,
        fingerprint: MachineFingerprint::read(
            &ClientEndpointId::Local,
            &harness.catalog,
            harness.state.shell.as_ref().unwrap(),
        )
        .unwrap(),
    };
    let (client_tx, mut client_rx) = tokio::sync::mpsc::channel(4);
    let (history_tx, mut history_rx) = tokio::sync::mpsc::channel(4);
    for _ in 0..4 {
        client_tx.try_send(ClientLoopEvent::Timer).unwrap();
        assert!(history_tx
            .try_send(HistoryWorkerEvent {
                key: key.clone(),
                payload: WorkerPayload::Finished(Ok(()))
            })
            .is_ok());
    }
    let mut prefer_history = false;
    for expected in [false, true, false, true, false, true, false, true] {
        let event = crate::client::events::receive_runtime_event(
            &mut client_rx,
            &mut history_rx,
            &mut prefer_history,
        )
        .await;
        assert_eq!(matches!(event, ClientLoopEvent::PiHistory(_)), expected);
    }
    assert!(client_rx.try_recv().is_err());
    assert!(history_rx.try_recv().is_err());
    assert!(harness.sent.lock().unwrap().is_empty());
    let _ = harness.context();
}
