use super::*;
use crate::persist::workspace_transfer::{TransferPane, WorkspaceTransferBundle};
use std::collections::{BTreeMap, HashMap};

fn settled_fixture() -> ReceiveRecord {
    serde_json::from_value(serde_json::json!({
        "source_session": "source",
        "source_workspace": "w1",
        "digest": "settled-fixture",
        "phase": "Owned",
        "info": {
            "transfer_id": "26000000-0000-4000-8000-000000000001",
            "state": "committed",
            "destination_session": "target",
            "workspace_id": "w2"
        },
        "route": {
            "transfer_id": "26000000-0000-4000-8000-000000000001",
            "destination_session": "target",
            "origin_workspace": "w1",
            "destination_workspace": "w2",
            "identities": {}, "names": {}, "ready": false
        },
        "settled": true
    }))
    .unwrap()
}

#[test]
fn workspace_transfer_finished_outcome_retries_journal_failure_and_lost_ack() {
    for outcome in [
        SourceOutcome::Aborted("confirmed receiver rejection".into()),
        SourceOutcome::Released,
        SourceOutcome::Settled(Box::new(settled_fixture())),
    ] {
        let (tx, receiver) = mpsc::channel();
        let sender = EventSender {
            tx,
            notify: Arc::new(Notify::new()),
        };
        let worker = std::thread::spawn(move || {
            report_finished("retry", outcome, &sender, &AtomicBool::new(false)).is_some()
        });
        for attempt in 0..3 {
            let TransferEvent::Finished {
                transfer_id,
                outcome,
                reply: Some(reply),
            } = receiver.recv_timeout(Duration::from_secs(3)).unwrap()
            else {
                panic!("outcome was not acknowledged");
            };
            assert_eq!(transfer_id, "retry");
            assert!(matches!(
                outcome,
                SourceOutcome::Aborted(_) | SourceOutcome::Released | SourceOutcome::Settled(_)
            ));
            match attempt {
                0 => reply
                    .send(Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "journal blocked",
                    )))
                    .unwrap(),
                1 => drop(reply),
                _ => reply.send(Ok(Vec::new())).unwrap(),
            }
        }
        assert!(worker.join().unwrap());
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn workspace_transfer_rejects_another_runtime_domain_before_descriptors() {
    let (mut client, mut peer) = UnixStream::pair().unwrap();
    let bytes = frame_bytes(&Reply {
        transfer_protocol: super::super::TRANSFER_PROTOCOL_VERSION,
        runtime_domain: Some("another-runtime-domain".into()),
        version: crate::build_info::version(),
        protocol: crate::protocol::PROTOCOL_VERSION,
        session: "target".into(),
        record: None,
        error: None,
    })
    .unwrap();
    write_bytes(&mut peer, &bytes).unwrap();
    assert_eq!(
        read_reply(&mut client, "target").unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn workspace_transfer_frame_roundtrips_numeric_pane_maps() {
    let state = crate::app::AppState::test_with_adversarial_identity_state();
    let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
    let snapshot = crate::persist::capture(
        &state.workspaces,
        &state.terminals,
        &runtimes,
        state.active,
        state.selected,
        state.next_agent_activity_seq,
    );
    let workspace = &state.workspaces[0];
    let panes = workspace
        .tabs
        .iter()
        .flat_map(|tab| tab.panes.iter())
        .map(|(id, pane)| (id.raw(), pane));
    let panes: BTreeMap<_, _> = panes
        .map(|(id, pane)| {
            (
                id,
                TransferPane {
                    terminal: state.terminals[&pane.attached_terminal_id]
                        .workspace_transfer_state()
                        .unwrap(),
                    seen: pane.seen,
                    right_click_passthrough: pane.right_click_passthrough,
                },
            )
        })
        .collect();
    let expected_panes = panes.len();
    let bundle = WorkspaceTransferBundle {
        transfer_protocol: super::super::TRANSFER_PROTOCOL_VERSION,
        transfer_id: "23000000-0000-4000-8000-000000000001".into(),
        source_session: "source".into(),
        destination_session: "target".into(),
        source_version: crate::build_info::version(),
        source_protocol: crate::protocol::PROTOCOL_VERSION,
        source_domain: "test-runtime-domain".into(),
        forwarding_depth: 0,
        snapshot,
        identity_cache: crate::persist::workspace_transfer::WorkspaceIdentityTransferState::capture(
            workspace,
        ),
        panes,
        runtimes: Vec::new(),
        metadata_tokens: Default::default(),
        metadata_token_sequences: HashMap::new(),
        previous_focus: workspace
            .tabs
            .iter()
            .map(|tab| tab.layout.previous_focus().map(crate::layout::PaneId::raw))
            .collect(),
        next_agent_state_change_seq: 0,
        next_agent_activity_seq: 0,
        aliases: BTreeMap::new(),
        plugins: Vec::new(),
        plugin_panes: BTreeMap::new(),
    };
    let bytes = frame_bytes(&Command::Prepare {
        bundle: Box::new(bundle),
    })
    .unwrap();
    let command: Command = serde_json::from_slice(&bytes).unwrap();
    match command {
        Command::Prepare { bundle } => {
            assert_eq!(bundle.panes.len(), expected_panes);
            assert_eq!(
                bundle.snapshot.workspaces[0].public_pane_numbers.len(),
                expected_panes
            );
        }
        _ => panic!("prepare command changed kind"),
    }
}
