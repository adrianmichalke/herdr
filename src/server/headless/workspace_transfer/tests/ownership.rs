use super::*;
use std::path::PathBuf;

struct DiskJournal(PathBuf);

impl DiskJournal {
    fn attach(server: &mut HeadlessServer) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "herdr-transfer-outcome-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        let path = directory.join("journal.json");
        let journal = server.transfer_registry.snapshot().unwrap();
        let registry = TransferRegistry::open(path.clone()).unwrap();
        registry
            .update(|state| {
                *state = journal;
                Ok(())
            })
            .unwrap();
        server.transfer_registry = registry;
        Self(path)
    }

    fn block(&self) {
        std::fs::create_dir(self.0.with_extension("json.tmp")).unwrap();
    }

    fn unblock(&self) {
        std::fs::remove_dir(self.0.with_extension("json.tmp")).unwrap();
    }

    fn reopen(&self) -> TransferRegistry {
        TransferRegistry::open(self.0.clone()).unwrap()
    }
}

impl Drop for DiskJournal {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[tokio::test]
async fn workspace_transfer_release_settlement_and_commit_writes_fail_closed_and_retry() {
    let (mut source, mut target, prepared) = fixture();
    let id = prepared.info.transfer_id.clone();
    let origin = prepared.source_workspace.clone();
    source.decide_source_commit(&id, prepared.clone()).unwrap();
    target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Commit)
        .unwrap();
    let owned = finish_checkpoint(&mut target, &id);
    assert_eq!(owned.phase, ReceivePhase::Owned);
    let source_disk = DiskJournal::attach(&mut source);
    let target_disk = DiskJournal::attach(&mut target);
    let destination = owned.info.workspace_id.clone().unwrap();
    assert!(source.finish_source_transfer(&id, owned.clone()).is_err());
    for runtime in source.release_source_transfer(&id, owned.clone()).unwrap() {
        runtime.preserve_for_handoff();
    }
    source_disk.block();
    assert!(source.record_source_release(&id).is_err());
    assert!(!source.transfer_registry.snapshot().unwrap().sources[&id].runtime_released);
    assert!(!source_disk.reopen().snapshot().unwrap().sources[&id].runtime_released);
    assert!(source.app.state.workspace_transfer_fences.contains(&origin));
    assert!(source.workspace_transfers_preclude_handoff());
    assert!(target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Settle)
        .is_err());
    source_disk.unblock();
    source.record_source_release(&id).unwrap();
    assert!(source_disk.reopen().snapshot().unwrap().sources[&id].runtime_released);
    assert!(source
        .fail_source_transfer(&id, "stale abort".into())
        .is_err());
    target_disk.block();
    assert!(target
        .record_receiver_settlement(&id, owned.clone())
        .is_err());
    assert!(!target.transfer_registry.snapshot().unwrap().receivers[&id].settled);
    assert!(!target_disk.reopen().snapshot().unwrap().receivers[&id].settled);
    assert!(target
        .app
        .state
        .workspace_transfer_fences
        .contains(&destination));
    assert!(source.finish_source_transfer(&id, owned.clone()).is_err());
    target_disk.unblock();
    let settled = target.record_receiver_settlement(&id, owned).unwrap();
    assert!(target_disk.reopen().snapshot().unwrap().receivers[&id].settled);
    assert!(!target
        .app
        .state
        .workspace_transfer_fences
        .contains(&destination));
    let repeated = target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Settle)
        .unwrap();
    assert!(repeated.settled);
    source_disk.block();
    assert!(source.finish_source_transfer(&id, settled.clone()).is_err());
    let pending = source_disk.reopen().snapshot().unwrap();
    assert_eq!(
        pending.sources[&id].info.state,
        WorkspaceTransferState::Pending
    );
    assert!(!pending.routes[&id].ready);
    assert!(source.app.state.workspace_transfer_fences.contains(&origin));
    source_disk.unblock();
    source.finish_source_transfer(&id, settled.clone()).unwrap();
    source.finish_source_transfer(&id, settled).unwrap();
    let committed = source_disk.reopen().snapshot().unwrap();
    assert_eq!(committed.sources[&id].phase, SourcePhase::Committed);
    assert_eq!(
        committed.sources[&id].info.state,
        WorkspaceTransferState::Committed
    );
    assert!(committed.routes[&id].ready);
    assert!(!source.app.state.workspace_transfer_fences.contains(&origin));
    source.app.state.assert_invariants_for_test();
    target.app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn workspace_transfer_aborted_write_failure_preserves_fence_and_deferred_exit_until_ack() {
    let (mut source, _, prepared) = fixture();
    let id = prepared.info.transfer_id;
    let origin = prepared.source_workspace;
    let pane_id = source.app.state.workspaces[0].tabs[0].root_pane;
    source.handle_internal_event_with_forwarding(AppEvent::PaneDied {
        pane_id,
        exit_reason: crate::platform::ChildExitReason::Exited,
    });
    let disk = DiskJournal::attach(&mut source);
    disk.block();
    for attempt in 0..2 {
        if attempt == 1 {
            disk.unblock();
        }
        let (reply, receiver) = std::sync::mpsc::channel();
        source
            .workspace_transfers
            .sender
            .send(TransferEvent::Finished {
                transfer_id: id.clone(),
                outcome: SourceOutcome::Aborted("known precommit rejection".into()),
                reply: Some(reply),
            })
            .unwrap();
        source.pump_workspace_transfers();
        let result = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let journal = disk.reopen().snapshot().unwrap();
        if attempt == 0 {
            assert!(result.is_err());
            assert_eq!(journal.sources[&id].phase, SourcePhase::Preparing);
            assert!(source.app.state.workspace_transfer_fences.contains(&origin));
            assert_eq!(source.workspace_transfers.deferred_exits.len(), 1);
            assert!(source.app.find_pane(pane_id).is_some());
        } else {
            assert!(result.is_ok());
            assert_eq!(journal.sources[&id].phase, SourcePhase::Failed);
            assert!(!source.app.state.workspace_transfer_fences.contains(&origin));
            assert!(source.workspace_transfers.deferred_exits.is_empty());
            assert!(source.app.find_pane(pane_id).is_none());
        }
    }
    source
        .app
        .state
        .workspace_transfer_fences
        .insert(origin.clone());
    source
        .fail_source_transfer(&id, "duplicate after lost ack".into())
        .unwrap();
    assert!(source.app.state.workspace_transfer_fences.contains(&origin));
    source.app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn workspace_transfer_receiver_settlement_replays_deferred_exits_only_after_durable_write() {
    let (_, mut target, prepared) = fixture();
    let id = prepared.info.transfer_id.clone();
    target
        .control_received_workspace(&id, &prepared.source_session, ControlAction::Commit)
        .unwrap();
    let owned = finish_checkpoint(&mut target, &id);
    let pane_id = target.app.state.workspaces[0].tabs[0].root_pane;
    target.handle_internal_event_with_forwarding(AppEvent::PaneDied {
        pane_id,
        exit_reason: crate::platform::ChildExitReason::Exited,
    });
    assert_eq!(target.workspace_transfers.deferred_exits.len(), 1);
    let disk = DiskJournal::attach(&mut target);
    disk.block();
    assert!(target
        .record_receiver_settlement(&id, owned.clone())
        .is_err());
    assert_eq!(target.workspace_transfers.deferred_exits.len(), 1);
    assert!(target.app.find_pane(pane_id).is_some());
    disk.unblock();
    let settled = target.record_receiver_settlement(&id, owned).unwrap();
    assert!(target.workspace_transfers.deferred_exits.is_empty());
    assert!(target.app.find_pane(pane_id).is_none());
    target.record_receiver_settlement(&id, settled).unwrap();
    assert!(target.workspace_transfers.deferred_exits.is_empty());
    target.app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn workspace_transfer_lost_owned_ack_releases_detached_runtime_before_retry() {
    use crate::pty::actor::{PtyIoActor, PtyIoActorConfig, PtyReadResult};
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    let (mut source, _, mut owned) = fixture();
    let id = owned.info.transfer_id.clone();
    source.decide_source_commit(&id, owned.clone()).unwrap();
    owned.phase = ReceivePhase::Owned;
    owned.info = transport::committed_info(&owned);
    let terminal = source.app.state.terminals.keys().next().unwrap().clone();
    let (socket, mut peer) = UnixStream::pair().unwrap();
    let actor = PtyIoActor::spawn(PtyIoActorConfig {
        pane_id: source.app.state.workspaces[0].tabs[0].root_pane.raw(),
        master_fd: socket.into(),
        initially_quiesced: true,
        on_read: Box::new(|_| PtyReadResult::empty()),
        on_reader_exit: None,
    })
    .unwrap();
    let mut runtime = crate::terminal::TerminalRuntime::test_with_actor(actor);
    runtime.set_transfer_drop_protection(true);
    source.app.terminal_runtimes.insert(terminal, runtime);
    let (reply, receiver) = std::sync::mpsc::channel();
    drop(receiver);
    source
        .workspace_transfers
        .sender
        .send(TransferEvent::Finished {
            transfer_id: id.clone(),
            outcome: SourceOutcome::Owned(Box::new(owned.clone())),
            reply: Some(reply),
        })
        .unwrap();
    source.pump_workspace_transfers();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
    assert!(!source.transfer_registry.snapshot().unwrap().sources[&id].runtime_released);
    assert!(source.workspace_transfers_preclude_handoff());
    assert!(source
        .release_source_transfer(&id, owned)
        .unwrap()
        .is_empty());
    source.record_source_release(&id).unwrap();
    assert!(source.transfer_registry.snapshot().unwrap().sources[&id].runtime_released);
    source.app.state.assert_invariants_for_test();
}
