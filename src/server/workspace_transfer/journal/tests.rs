use super::tests::TestDirectory;
use super::*;

fn ownership_fixture() -> (SourceRecord, ReceiveRecord, crate::persist::SessionSnapshot) {
    let state = crate::app::AppState::test_with_adversarial_identity_state();
    let snapshot = crate::persist::capture(
        &state.workspaces,
        &state.terminals,
        &crate::terminal::TerminalRuntimeRegistry::new(),
        state.active,
        state.selected,
        state.next_agent_activity_seq,
    );
    let destination = state.workspaces[0].id.clone();
    let id = "24000000-0000-4000-8000-000000000001".to_owned();
    let info = WorkspaceTransferInfo {
        transfer_id: id.clone(),
        state: WorkspaceTransferState::Pending,
        destination_session: "target".into(),
        workspace_id: Some(destination.clone()),
        focused_tab_id: None,
        focused_pane_id: None,
        error: None,
    };
    let route = Route {
        transfer_id: id.clone(),
        destination_session: "target".into(),
        origin_workspace: "w1".into(),
        destination_workspace: destination,
        identities: BTreeMap::new(),
        canonical_inverse: BTreeMap::new(),
        terminals: BTreeMap::new(),
        names: BTreeMap::new(),
        pending_names: Default::default(),
        ready: false,
    };
    let source = SourceRecord {
        params: WorkspaceTransferParams {
            transfer_id: id,
            workspace_id: "w1".into(),
            destination_session: "target".into(),
        },
        info: info.clone(),
        phase: SourcePhase::CommitDecided,
        route: Some(route.clone()),
        source_identities: Default::default(),
        source_names: Default::default(),
        runtime_released: false,
    };
    let receiver = ReceiveRecord {
        source_session: "missing-transfer-source".into(),
        source_workspace: "w1".into(),
        digest: "ownership-fixture".into(),
        phase: ReceivePhase::Owned,
        info,
        route,
        owner_boot_id: Some("previous-boot".into()),
        forwarding_depth: 1,
        settled: false,
    };
    (source, receiver, snapshot)
}

#[test]
fn workspace_transfer_owned_receiver_requires_explicit_matching_runtime_release() {
    let directory = TestDirectory::new();
    let source_path = directory.path().join("source.json");
    let (mut source, receiver, _) = ownership_fixture();
    let id = receiver.info.transfer_id.clone();
    let persist = |source: &SourceRecord| {
        let journal = Journal {
            sources: BTreeMap::from([(id.clone(), source.clone())]),
            ..Journal::default()
        };
        save(&source_path, &journal).unwrap();
        source_released_workspace_from_path(&receiver, &source_path)
    };
    assert!(!persist(&source));
    source.phase = SourcePhase::Committed;
    assert!(!persist(&source));
    source.phase = SourcePhase::CommitDecided;
    source.runtime_released = true;
    assert!(persist(&source));
    source
        .route
        .as_mut()
        .unwrap()
        .destination_workspace
        .push('x');
    assert!(!persist(&source));
    source.route = Some(receiver.route.clone());
    source.params.destination_session = "other".into();
    assert!(!persist(&source));
    source.params.destination_session = receiver.info.destination_session.clone();
    source.phase = SourcePhase::Failed;
    assert!(!persist(&source));
    std::fs::remove_file(&source_path).unwrap();
    assert!(!source_released_workspace_from_path(
        &receiver,
        &source_path
    ));
}

#[test]
fn workspace_transfer_durable_settlement_restores_without_any_source_journal() {
    let directory = TestDirectory::new();
    let path = directory.path().join("receiver.json");
    let (_, mut receiver, snapshot) = ownership_fixture();
    let registry = TransferRegistry::open(path.clone()).unwrap();
    registry
        .update(|journal| {
            journal
                .receivers
                .insert(receiver.info.transfer_id.clone(), receiver.clone());
            Ok(())
        })
        .unwrap();
    let mut suppressed = serde_json::from_value(serde_json::to_value(&snapshot).unwrap()).unwrap();
    registry
        .suppress_exported_workspaces(&mut suppressed)
        .unwrap();
    assert!(suppressed.workspaces.is_empty());
    receiver.settled = true;
    registry
        .update(|journal| {
            journal
                .receivers
                .insert(receiver.info.transfer_id.clone(), receiver);
            Ok(())
        })
        .unwrap();
    let restarted = TransferRegistry::open(path).unwrap();
    let mut restored = serde_json::from_value(serde_json::to_value(&snapshot).unwrap()).unwrap();
    restarted
        .suppress_exported_workspaces(&mut restored)
        .unwrap();
    assert_eq!(restored.workspaces.len(), snapshot.workspaces.len());
    assert_eq!(restored.active, snapshot.active);
    assert_eq!(restored.selected, snapshot.selected);
}

#[test]
fn workspace_transfer_missing_settlement_fields_are_unconfirmed() {
    let (source, receiver, _) = ownership_fixture();
    let mut source = serde_json::to_value(source).unwrap();
    source.as_object_mut().unwrap().remove("runtime_released");
    assert!(
        !serde_json::from_value::<SourceRecord>(source)
            .unwrap()
            .runtime_released
    );
    let mut receiver = serde_json::to_value(receiver).unwrap();
    receiver.as_object_mut().unwrap().remove("settled");
    assert!(
        !serde_json::from_value::<ReceiveRecord>(receiver)
            .unwrap()
            .settled
    );
}
