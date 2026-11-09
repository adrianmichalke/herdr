use super::*;
use crate::persist::workspace_transfer::ImportedWorkspace;
use crate::server::workspace_transfer::journal::Route;
use std::collections::BTreeMap;

#[path = "tests/ownership.rs"]
mod ownership;

fn imported_fixture() -> ImportedWorkspace {
    let mut state = crate::app::AppState::test_with_adversarial_identity_state();
    let workspace = state.workspaces.remove(0);
    let mut runtimes = HashMap::new();
    let mut receivers = Vec::new();
    for terminal in state.terminals.keys() {
        let (mut runtime, receiver) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.set_transfer_drop_protection(true);
        runtimes.insert(terminal.clone(), runtime);
        receivers.push(receiver);
    }
    std::thread::spawn(move || {
        while receivers.iter().any(|receiver| !receiver.is_closed()) {
            for receiver in &mut receivers {
                while receiver.try_recv().is_ok() {}
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    ImportedWorkspace {
        workspace,
        terminals: state.terminals,
        runtimes,
        plugins: Vec::new(),
        plugin_panes: HashMap::new(),
        next_agent_state_change_seq: 47,
        next_agent_activity_seq: 91,
        checkpoint_cwds: HashMap::new(),
    }
}

fn fixture() -> (HeadlessServer, HeadlessServer, ReceiveRecord) {
    let mut source = crate::server::headless::tests::test_headless_server();
    source.app.state = crate::app::AppState::test_with_adversarial_identity_state();
    let mut target = crate::server::headless::tests::test_headless_server();
    let imported = imported_fixture();
    let origin = source.app.state.workspaces[0].id.clone();
    let destination = imported.workspace.id.clone();
    let id = "22000000-0000-4000-8000-000000000001".to_owned();
    let params = WorkspaceTransferParams {
        workspace_id: origin.clone(),
        destination_session: "target".into(),
        transfer_id: id.clone(),
    };
    let pending = WorkspaceTransferInfo {
        transfer_id: id.clone(),
        state: WorkspaceTransferState::Pending,
        destination_session: "target".into(),
        workspace_id: None,
        focused_tab_id: None,
        focused_pane_id: None,
        error: None,
    };
    let mut info = pending.clone();
    info.workspace_id = Some(destination.clone());
    let route = Route {
        transfer_id: id.clone(),
        destination_session: "target".into(),
        origin_workspace: origin.clone(),
        destination_workspace: destination.clone(),
        identities: BTreeMap::from([(origin.clone(), destination)]),
        canonical_inverse: BTreeMap::new(),
        terminals: BTreeMap::new(),
        names: BTreeMap::new(),
        pending_names: Default::default(),
        ready: false,
    };
    let record = ReceiveRecord {
        source_session: transfer::session_name(),
        source_workspace: origin.clone(),
        digest: "fixture".into(),
        phase: ReceivePhase::Prepared,
        info,
        route,
        owner_boot_id: None,
        forwarding_depth: 1,
        settled: false,
    };
    source
        .transfer_registry
        .update(|journal| {
            journal.sources.insert(
                id.clone(),
                SourceRecord {
                    params,
                    info: pending,
                    phase: SourcePhase::Preparing,
                    route: None,
                    source_identities: Default::default(),
                    source_names: Default::default(),
                    runtime_released: false,
                },
            );
            journal.reserved_workspaces.insert(origin.clone());
            Ok(())
        })
        .unwrap();
    source.app.state.workspace_transfer_fences.insert(origin);
    target
        .transfer_registry
        .update(|journal| {
            journal.receivers.insert(id.clone(), record.clone());
            Ok(())
        })
        .unwrap();
    target.workspace_transfers.staged.insert(id, imported);
    (source, target, record)
}

fn finish_checkpoint(target: &mut HeadlessServer, id: &str) -> ReceiveRecord {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        target.pump_workspace_transfers();
        let record = target.transfer_registry.snapshot().unwrap().receivers[id].clone();
        if record.phase != ReceivePhase::Prepared {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "receiver checkpoint never completed"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn workspace_transfer_reprobes_agents_after_staged_runtimes_become_live() {
    let (_, mut target, prepared) = fixture();
    let id = prepared.info.transfer_id.clone();
    let notify = target.workspace_transfers.staged[&id]
        .runtimes
        .values()
        .next()
        .unwrap()
        .agent_detection_reset_notify_for_test();
    assert!(tokio::time::timeout(Duration::ZERO, notify.notified())
        .await
        .is_err());
    target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Commit)
        .unwrap();
    let owned = finish_checkpoint(&mut target, &id);
    assert_eq!(owned.phase, ReceivePhase::Owned);
    tokio::time::timeout(Duration::from_secs(1), notify.notified())
        .await
        .expect("ownership activation must refresh detection events observed before insertion");
}

#[tokio::test]
async fn workspace_transfer_source_restart_keeps_preparation_pending_and_suppresses_cold_restore() {
    let (source, _, prepared) = fixture();
    let id = prepared.info.transfer_id;
    let moving = source.app.state.workspaces[0].id.clone();
    let terminal = source
        .app
        .state
        .terminals
        .keys()
        .next()
        .unwrap()
        .to_string();
    source
        .transfer_registry
        .update(|journal| {
            let record = journal.sources.get_mut(&id).unwrap();
            record.source_identities.insert(moving.clone());
            record
                .source_names
                .insert("reserved-origin-name".into(), terminal);
            Ok(())
        })
        .unwrap();
    let mut snapshot = crate::persist::capture(
        &source.app.state.workspaces,
        &source.app.state.terminals,
        &source.app.terminal_runtimes,
        source.app.state.active,
        source.app.state.selected,
        source.app.state.next_agent_activity_seq,
    );
    source.transfer_registry.recover().unwrap();
    source
        .transfer_registry
        .suppress_exported_workspaces(&mut snapshot)
        .unwrap();
    assert!(snapshot.workspaces.is_empty());
    let journal = source.transfer_registry.snapshot().unwrap();
    assert_eq!(journal.sources[&id].phase, SourcePhase::Preparing);
    assert_eq!(
        journal.sources[&id].info.state,
        WorkspaceTransferState::Pending
    );
    assert!(source
        .transfer_registry
        .reserve_local_name("reserved-origin-name", "another-terminal".into())
        .is_err());
    let request: api::schema::Request = serde_json::from_value(serde_json::json!({"id":"wait","method":"agent.wait","params":{"target":"reserved-origin-name","until":["idle"]}})).unwrap();
    assert_eq!(
        transfer::routing::resolve(
            &source.transfer_registry,
            &request,
            &serde_json::to_string(&request).unwrap()
        )
        .err()
        .unwrap()
        .kind(),
        io::ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn workspace_transfer_checkpoint_failure_aborts_without_claiming_ownership() {
    let (mut source, mut target, prepared) = fixture();
    let id = prepared.info.transfer_id.clone();
    source.decide_source_commit(&id, prepared.clone()).unwrap();
    target.workspace_transfers.checkpoint_in_flight = Some(id.clone());
    target.app.workspace_transfer_checkpoint_pending = true;
    target
        .finish_receiver_checkpoint(
            &id,
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "checkpoint denied",
            )),
        )
        .unwrap();
    let status = target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Status)
        .unwrap();
    assert_eq!(status.phase, ReceivePhase::Aborted);
    assert!(target.app.state.workspaces.is_empty());
    assert!(target.workspace_transfers.staged.is_empty());
    source
        .fail_source_transfer(&id, "receiver aborted".into())
        .unwrap();
    assert!(!source.app.state.workspaces.is_empty());
    source.app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn workspace_transfer_orphaned_preparation_expires_but_owned_receipt_does_not() {
    let (_, mut target, prepared) = fixture();
    let id = prepared.info.transfer_id.clone();
    let now = Instant::now();
    target
        .workspace_transfers
        .receive_deadlines
        .insert(id.clone(), now);
    target.workspace_transfers.next_receive_deadline = Some(now);
    assert!(target.expire_prepared_workspace_transfers(now));
    assert!(target.workspace_transfers.staged.is_empty());
    assert_eq!(
        target.transfer_registry.snapshot().unwrap().receivers[&id].phase,
        ReceivePhase::Aborted
    );
    target
        .transfer_registry
        .update(|journal| {
            let record = journal.receivers.get_mut(&id).unwrap();
            record.phase = ReceivePhase::Owned;
            record.info.state = WorkspaceTransferState::Committed;
            record.owner_boot_id = Some(target.client_shell_boot_id.clone());
            Ok(())
        })
        .unwrap();
    target
        .workspace_transfers
        .receive_deadlines
        .insert(id.clone(), now);
    target.workspace_transfers.next_receive_deadline = Some(now);
    target.expire_prepared_workspace_transfers(now);
    assert_eq!(
        target
            .control_received_workspace(&id, &prepared.source_session, ControlAction::Status)
            .unwrap()
            .phase,
        ReceivePhase::Owned
    );
}

#[tokio::test]
async fn workspace_transfer_defers_source_exit_cleanup_until_rollback() {
    let (mut source, _, prepared) = fixture();
    let pane_id = source.app.state.workspaces[0].tabs[0].root_pane;
    assert!(
        !source.handle_internal_event_with_forwarding(AppEvent::PaneDied {
            pane_id,
            exit_reason: crate::platform::ChildExitReason::Exited
        })
    );
    assert!(source.app.find_pane(pane_id).is_some());
    assert_eq!(source.workspace_transfers.deferred_exits.len(), 1);
    source
        .fail_source_transfer(&prepared.info.transfer_id, "before commit rejection".into())
        .unwrap();
    assert!(source.workspace_transfers.deferred_exits.is_empty());
    assert!(source.app.find_pane(pane_id).is_none());
    source.app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn workspace_transfer_failed_reader_activation_stays_pending_and_cannot_abort_to_source() {
    let (mut source, mut target, prepared) = fixture();
    let id = prepared.info.transfer_id.clone();
    let imported = target.workspace_transfers.staged.get_mut(&id).unwrap();
    let terminal = imported.terminals.keys().next().unwrap().clone();
    let (mut closed, receiver) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
    closed.set_transfer_drop_protection(true);
    drop(receiver);
    imported.runtimes.insert(terminal, closed);
    source.decide_source_commit(&id, prepared.clone()).unwrap();
    target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Commit)
        .unwrap();
    let pending = finish_checkpoint(&mut target, &id);
    assert_eq!(pending.phase, ReceivePhase::Activating);
    assert_eq!(pending.info.state, WorkspaceTransferState::Pending);
    assert_eq!(
        target
            .control_received_workspace(&id, &prepared.source_session, ControlAction::Abort)
            .unwrap()
            .phase,
        ReceivePhase::Activating
    );
    assert_eq!(
        source.transfer_registry.snapshot().unwrap().sources[&id]
            .info
            .state,
        WorkspaceTransferState::Pending
    );
    assert!(!source.app.state.workspace_transfer_fences.is_empty());
    assert!(target
        .app
        .state
        .workspace_transfer_fences
        .contains(pending.info.workspace_id.as_ref().unwrap()));
    target.expire_prepared_workspace_transfers(Instant::now() + Duration::from_secs(300));
    assert_eq!(
        target.transfer_registry.snapshot().unwrap().receivers[&id].phase,
        ReceivePhase::Activating
    );
}

#[tokio::test]
async fn workspace_transfer_lost_ack_stays_pending_and_commit_is_idempotent() {
    let (mut source, mut target, prepared) = fixture();
    let id = prepared.info.transfer_id.clone();
    source.decide_source_commit(&id, prepared.clone()).unwrap();
    let before = source.transfer_registry.snapshot().unwrap();
    assert_eq!(
        before.sources[&id].info.state,
        WorkspaceTransferState::Pending
    );
    assert!(!before.routes[&id].ready);
    let first = target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Commit)
        .unwrap();
    assert_eq!(first.phase, ReceivePhase::Prepared);
    assert!(target.app.state.workspaces.is_empty());
    let owned = finish_checkpoint(&mut target, &id);
    assert_eq!(owned.phase, ReceivePhase::Owned);
    assert_eq!(
        source.transfer_registry.snapshot().unwrap().sources[&id]
            .info
            .state,
        WorkspaceTransferState::Pending
    );
    let repeated = target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Commit)
        .unwrap();
    assert_eq!(repeated.info, owned.info);
    assert_eq!(target.app.state.workspaces.len(), 1);
    assert!(source.finish_source_transfer(&id, owned.clone()).is_err());
    assert!(source.record_source_release(&id).is_err());
    for runtime in source.release_source_transfer(&id, owned.clone()).unwrap() {
        runtime.preserve_for_handoff();
    }
    assert!(source.workspace_transfers_preclude_handoff());
    assert!(target.workspace_transfers_preclude_handoff());
    assert_eq!(
        source.transfer_registry.snapshot().unwrap().sources[&id]
            .info
            .state,
        WorkspaceTransferState::Pending
    );
    source.record_source_release(&id).unwrap();
    assert!(source.finish_source_transfer(&id, owned.clone()).is_err());
    let settled = target.record_receiver_settlement(&id, owned).unwrap();
    source.finish_source_transfer(&id, settled.clone()).unwrap();
    source.finish_source_transfer(&id, settled).unwrap();
    assert!(!source.workspace_transfers_preclude_handoff());
    assert!(!target.workspace_transfers_preclude_handoff());
    let after = source.transfer_registry.snapshot().unwrap();
    assert_eq!(
        after.sources[&id].info.state,
        WorkspaceTransferState::Committed
    );
    assert!(after.routes[&id].ready);
    assert!(source
        .fail_source_transfer(&id, "stale failure".into())
        .is_err());
    assert_eq!(
        source.transfer_registry.snapshot().unwrap().sources[&id]
            .info
            .state,
        WorkspaceTransferState::Committed
    );
    source.app.state.assert_invariants_for_test();
    target.app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn workspace_transfer_abort_during_checkpoint_cannot_activate_receiver() {
    let (mut source, mut target, prepared) = fixture();
    let id = prepared.info.transfer_id.clone();
    source.decide_source_commit(&id, prepared.clone()).unwrap();
    target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Commit)
        .unwrap();
    let aborted = target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Abort)
        .unwrap();
    assert_eq!(aborted.phase, ReceivePhase::Aborted);
    assert!(target.workspace_transfers.staged.is_empty());
    let deadline = Instant::now() + Duration::from_secs(3);
    while target.app.workspace_transfer_checkpoint_pending {
        target.pump_workspace_transfers();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(target.app.state.workspaces.is_empty());
    source
        .fail_source_transfer(&id, "confirmed abort".into())
        .unwrap();
    assert!(!source.app.state.workspaces.is_empty());
    assert!(source.app.state.workspace_transfer_fences.is_empty());
    assert!(source
        .transfer_registry
        .snapshot()
        .unwrap()
        .routes
        .is_empty());
    source.app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn workspace_transfer_stale_receiver_generation_never_confirms_ownership() {
    let (_, mut target, mut record) = fixture();
    let id = record.info.transfer_id.clone();
    target.workspace_transfers.staged.clear();
    record.phase = ReceivePhase::Owned;
    record.info.state = WorkspaceTransferState::Committed;
    record.owner_boot_id = Some("previous-boot".into());
    target
        .transfer_registry
        .update(|journal| {
            journal.receivers.insert(id.clone(), record.clone());
            Ok(())
        })
        .unwrap();
    for action in [
        ControlAction::Status,
        ControlAction::Commit,
        ControlAction::Abort,
        ControlAction::Settle,
    ] {
        let error = target
            .control_received_workspace(&id, &record.source_session, action)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }
    assert!(target.app.state.workspaces.is_empty());
    assert_eq!(
        target.transfer_registry.snapshot().unwrap().receivers[&id].phase,
        ReceivePhase::Owned
    );
}

#[tokio::test]
async fn workspace_transfer_terminal_and_plugin_identity_collisions_do_not_overwrite() {
    let (_, mut target, _) = fixture();
    target.workspace_transfers.staged.clear();
    let mut imported = imported_fixture();
    let terminal = imported.terminals.values().next().unwrap().id.clone();
    target.app.state.terminals.insert(
        terminal.clone(),
        crate::terminal::TerminalState::new(terminal.clone(), "/".into()),
    );
    let error = target
        .validate_received_collisions(&imported, None)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        target.app.state.terminals[&terminal].cwd,
        std::path::PathBuf::from("/")
    );
    target.app.state.terminals.clear();
    let plugin: api::schema::InstalledPluginInfo = serde_json::from_value(serde_json::json!({"plugin_id":"plugin", "name":"original", "version":"1", "manifest_path":"/plugin/manifest", "plugin_root":"/plugin", "enabled":true})).unwrap();
    target
        .app
        .state
        .installed_plugins
        .insert(plugin.plugin_id.clone(), plugin.clone());
    let mut conflicting = plugin.clone();
    conflicting.version = "2".into();
    imported.plugins.push(conflicting);
    assert_eq!(
        target
            .validate_received_collisions(&imported, None)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(target.app.state.installed_plugins["plugin"], plugin);
}

#[tokio::test]
async fn workspace_transfer_fence_rejects_selected_mutation_without_blocking_other_workspace() {
    let (mut source, _, _) = fixture();
    let moving_pane = source
        .app
        .public_pane_id(0, source.app.state.workspaces[0].tabs[0].root_pane)
        .unwrap();
    source
        .app
        .state
        .workspaces
        .push(crate::workspace::Workspace::test_new("unrelated"));
    source.app.state.ensure_test_terminals();
    let other_pane = source
        .app
        .public_pane_id(1, source.app.state.workspaces[1].tabs[0].root_pane)
        .unwrap();
    for (pane, allowed) in [(moving_pane, false), (other_pane, true)] {
        let request: api::schema::Request = serde_json::from_value(serde_json::json!({"id":"fence", "method":"pane.rename", "params":{"pane_id":pane,"label":"changed"}})).unwrap();
        let response: serde_json::Value =
            serde_json::from_str(&source.app.handle_api_request(request)).unwrap();
        assert_eq!(response.get("error").is_none(), allowed, "{response}");
        if !allowed {
            assert_eq!(response["error"]["code"], "workspace_busy");
        }
    }
    source.app.state.assert_invariants_for_test();
}
