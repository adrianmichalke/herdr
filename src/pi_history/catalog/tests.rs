use super::*;
use crate::pi_history::test_support::{
    live_snapshot, location, request, saved_snapshot, FixtureDir,
};

fn session(dir: &FixtureDir, name: &str, running: bool) -> SessionInfo {
    SessionInfo {
        name: name.into(),
        default: false,
        running,
        connection_error: None,
        socket_path: dir
            .0
            .join(name)
            .join("never-start.sock")
            .display()
            .to_string(),
        session_dir: dir.0.join(name).display().to_string(),
    }
}

#[test]
fn session_discovery_is_bounded_before_any_socket_probes() {
    let dir = FixtureDir::new();
    let missing = dir.0.join("not-created");
    assert_eq!(
        session_names(&missing).unwrap(),
        (vec!["default".to_owned()], false)
    );
    for index in 0..MAX_SESSIONS + 2 {
        std::fs::create_dir(dir.0.join(format!("synthetic-{index:04}"))).unwrap();
    }
    let (names, limited) = session_names(&dir.0).unwrap();
    assert!(limited);
    assert_eq!(names.len(), MAX_SESSIONS);
    assert_eq!(names[0], "default");
    assert!(names[1..].windows(2).all(|names| names[0] < names[1]));
}

#[test]
fn stopped_catalog_reads_only_explicit_saved_metadata_without_restore() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "{\"type\":\"session\",\"version\":3}\n");
    let saved = serde_json::to_vec(&saved_snapshot(&path, "w1")).unwrap();
    let snapshot_path = dir.write("one/session.json", &saved);
    let mut events = Vec::new();
    let catalog = Catalog::from_sessions(
        &request(),
        vec![session(&dir, "one", false)],
        |_| panic!("stopped metadata must not contact a server"),
        &mut |event| {
            events.push(event);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(catalog.sources.len(), 1);
    assert!(!catalog.truncated);
    assert!(events.is_empty());
    let source = &catalog.sources.values().next().unwrap().source;
    assert_eq!(source.locations, vec![location(&path, "one", "w1")]);
    assert_eq!(std::fs::read(snapshot_path).unwrap(), saved);
    assert!(!Path::new(&session(&dir, "one", false).socket_path).exists());
    assert_eq!(std::fs::read_dir(dir.0.join("one")).unwrap().count(), 1);
}

#[test]
fn explicit_saved_metadata_reader_is_bounded_and_does_not_modify_files() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "");
    let bytes = serde_json::to_vec(&saved_snapshot(&path, "w1")).unwrap();
    let saved = dir.write("one/session.json", &bytes);
    let original = std::fs::metadata(&saved).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    std::fs::set_permissions(&saved, readonly).unwrap();
    assert_eq!(
        crate::persist::load_from_path(&saved)
            .unwrap()
            .workspaces
            .len(),
        1
    );
    assert_eq!(std::fs::read(&saved).unwrap(), bytes);
    assert!(std::fs::metadata(&saved).unwrap().permissions().readonly());
    std::fs::set_permissions(&saved, original).unwrap();
    let oversized = dir.write("oversized.json", "");
    std::fs::File::options()
        .write(true)
        .open(&oversized)
        .unwrap()
        .set_len(MAX_METADATA_BYTES + 1)
        .unwrap();
    assert_eq!(
        crate::persist::load_from_path(&oversized)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        crate::persist::load_from_path(&dir.0).err().unwrap().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        std::fs::metadata(&oversized).unwrap().len(),
        MAX_METADATA_BYTES + 1
    );
}

#[test]
fn scopes_filter_recorded_workspaces_and_sessions_not_labels_or_cwds() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "");
    let mut req = request();
    req.scope = HistoryScope::Space;
    req.workspace_id = Some("w2".into());
    let mut queried = Vec::new();
    let catalog = Catalog::from_sessions(
        &req,
        vec![session(&dir, "one", true), session(&dir, "two", true)],
        |session| {
            queried.push(session.name.clone());
            Ok(live_snapshot(&path, "w1"))
        },
        &mut |_| Ok(()),
    )
    .unwrap();
    assert_eq!(queried, vec!["one"]);
    assert!(catalog.sources.is_empty());
    req.scope = HistoryScope::All;
    let catalog = Catalog::from_sessions(
        &req,
        vec![session(&dir, "one", true), session(&dir, "two", true)],
        |_| Ok(live_snapshot(&path, "w1")),
        &mut |_| Ok(()),
    )
    .unwrap();
    assert_eq!(catalog.sources.len(), 1);
    assert_eq!(
        catalog
            .sources
            .values()
            .next()
            .unwrap()
            .source
            .locations
            .len(),
        2
    );
}

#[test]
fn canonical_aliases_dedupe_without_losing_provenance() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "");
    std::fs::create_dir(dir.0.join("nested")).unwrap();
    let alias = dir.0.join("nested/../raw.jsonl");
    let mut catalog = Catalog::default();
    catalog
        .add_location(location(&path, "one", "w1"), &mut |_| Ok(()))
        .unwrap();
    catalog
        .add_location(location(&alias, "two", "w2"), &mut |_| Ok(()))
        .unwrap();
    assert_eq!(catalog.sources.len(), 1);
    assert_eq!(
        catalog
            .sources
            .values()
            .next()
            .unwrap()
            .source
            .locations
            .len(),
        2
    );
    assert_eq!(
        catalog.sources.values().next().unwrap().source.id,
        source_id(&std::fs::canonicalize(path).unwrap())
    );
}

#[test]
fn id_only_missing_and_unavailable_live_refs_never_guess_or_fall_back() {
    let dir = FixtureDir::new();
    let missing = dir.0.join("missing.jsonl");
    let mut events = Vec::new();
    let mut emit = |event| {
        events.push(event);
        Ok(())
    };
    let mut catalog = Catalog::default();
    let mut id = location(&missing, "one", "w1");
    id.agent_session.kind = crate::agent_resume::AgentSessionRefKind::Id;
    id.agent_session.value = "only-a-native-id".into();
    catalog.add_location(id, &mut emit).unwrap();
    catalog
        .add_location(location(&missing, "one", "w1"), &mut emit)
        .unwrap();
    assert!(catalog.sources.is_empty());
    assert!(catalog.truncated);
    let saved = serde_json::to_vec(&saved_snapshot(&missing, "w1")).unwrap();
    dir.write("one/session.json", saved);
    let live = Catalog::from_sessions(
        &request(),
        vec![session(&dir, "one", true)],
        |_| Err(io::Error::other("unsupported method")),
        &mut emit,
    )
    .unwrap();
    assert!(live.sources.is_empty());
    assert!(live.truncated);
    assert_eq!(events.len(), 3);
    assert!(
        matches!(&events[2], HistoryEvent::Unavailable { message, .. } if message.contains("session.history_snapshot"))
    );
}

#[test]
fn missing_native_refs_are_scoped_and_count_toward_the_association_limit() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "");
    let mut snapshot = live_snapshot(&path, "other-space");
    let pane = &mut snapshot.panes[0];
    pane.agent_session = None;
    pane.agent = Some("pi".into());
    let mut req = request();
    req.scope = HistoryScope::Space;
    req.workspace_id = Some("w1".into());
    let mut catalog = Catalog::default();
    catalog
        .add_live(&req, "one", snapshot.clone(), &mut |_| {
            panic!("out-of-scope metadata")
        })
        .unwrap();
    assert!(!catalog.truncated);
    assert_eq!(catalog.locations, 0);
    let pane = snapshot.panes.pop().unwrap();
    snapshot.panes = (0..MAX_LOCATIONS + 10)
        .map(|index| {
            let mut pane = pane.clone();
            pane.pane_id = format!("other-space:p{index}");
            pane
        })
        .collect();
    let mut events = Vec::new();
    catalog
        .add_live(&request(), "one", snapshot, &mut |event| {
            events.push(event);
            Ok(())
        })
        .unwrap();
    assert!(catalog.truncated);
    assert_eq!(catalog.locations, MAX_LOCATIONS);
    assert_eq!(events.len(), MAX_LOCATIONS + 1);
    assert_eq!(events.iter().filter(|event| matches!(event, HistoryEvent::Unavailable { message, .. } if message.contains("association limit"))).count(), 1);
}

#[test]
fn saved_identity_reconstruction_uses_recorded_numbers_and_rejects_ambiguity() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "");
    let mut saved = saved_snapshot(&path, "w1");
    saved["workspaces"][0]["public_tab_numbers"] = serde_json::json!([33]);
    saved["workspaces"][0]["public_pane_numbers"] = serde_json::json!({"7":64});
    let mut catalog = Catalog::default();
    catalog
        .add_saved(
            &request(),
            "one",
            serde_json::from_value(saved.clone()).unwrap(),
            &mut |_| Ok(()),
        )
        .unwrap();
    let location = &catalog.sources.values().next().unwrap().source.locations[0];
    assert_eq!(location.tab_id, "w1:t11");
    assert_eq!(location.pane_id, "w1:p10");
    assert_eq!(location.terminal_id, None);
    assert!(!catalog.truncated);

    let mut invalid = Vec::new();
    let mut snapshot = saved.clone();
    snapshot["workspaces"][0]["public_tab_numbers"] = serde_json::json!([]);
    invalid.push(snapshot);
    let mut snapshot = saved.clone();
    snapshot["workspaces"][0]["public_pane_numbers"] = serde_json::json!({});
    invalid.push(snapshot);
    let mut snapshot = saved.clone();
    snapshot["workspaces"][0]["tabs"][0]["layout"] = serde_json::json!({"Pane":8});
    invalid.push(snapshot);
    let mut snapshot = saved.clone();
    let pane = snapshot["workspaces"][0]["tabs"][0]["panes"]["7"].clone();
    snapshot["workspaces"][0]["tabs"][0]["panes"]["8"] = pane;
    snapshot["workspaces"][0]["tabs"][0]["layout"] = serde_json::json!({"Split":{
        "direction":"Horizontal", "ratio":0.5, "first":{"Pane":7}, "second":{"Pane":8}
    }});
    snapshot["workspaces"][0]["public_pane_numbers"]["8"] = serde_json::json!(64);
    invalid.push(snapshot);
    let mut snapshot = saved.clone();
    let workspace = snapshot["workspaces"][0].clone();
    snapshot["workspaces"]
        .as_array_mut()
        .unwrap()
        .push(workspace);
    invalid.push(snapshot);
    for saved in invalid {
        let mut catalog = Catalog::default();
        let mut events = Vec::new();
        catalog
            .add_saved(
                &request(),
                "one",
                serde_json::from_value(saved).unwrap(),
                &mut |event| {
                    events.push(event);
                    Ok(())
                },
            )
            .unwrap();
        assert!(catalog.sources.is_empty());
        assert!(catalog.truncated);
        assert!(!events.is_empty());
    }
}

#[test]
fn normal_captured_snapshots_retain_real_navigation_identity_without_restore() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "");
    let mut state = crate::app::AppState::test_new();
    state.workspaces = vec![crate::workspace::Workspace::test_new("history-capture")];
    state.workspaces[0].test_add_tab(None);
    state.ensure_test_terminals();
    for terminal in state.terminals.values_mut() {
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef {
                kind: crate::agent_resume::AgentSessionRefKind::Path,
                value: path.to_str().unwrap().into(),
            },
        });
    }
    let snapshot = crate::persist::capture(
        &state.workspaces,
        &state.terminals,
        &Default::default(),
        Some(0),
        0,
        0,
    );
    let saved = serde_json::to_vec(&snapshot).unwrap();
    dir.write("one/session.json", &saved);
    let catalog = Catalog::from_sessions(
        &request(),
        vec![session(&dir, "one", false)],
        |_| panic!("no startup"),
        &mut |_| Ok(()),
    )
    .unwrap();
    assert!(!catalog.truncated);
    let locations = &catalog.sources.values().next().unwrap().source.locations;
    let workspace = &state.workspaces[0];
    assert_eq!(locations.len(), workspace.tabs.len());
    for tab in &workspace.tabs {
        let pane = tab.layout.pane_ids()[0];
        assert!(locations
            .iter()
            .any(|location| location.workspace_id == workspace.id
                && location.tab_id
                    == crate::workspace::public_tab_id_for_number(&workspace.id, tab.number)
                && location.pane_id
                    == crate::workspace::public_pane_id_for_number(
                        &workspace.id,
                        workspace.public_pane_numbers[&pane]
                    )
                && location.terminal_id.is_none()));
    }
    assert_eq!(
        std::fs::read(dir.0.join("one/session.json")).unwrap(),
        saved
    );
}

#[test]
fn repeated_provenance_is_bounded_without_splitting_the_canonical_source() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "");
    let mut catalog = Catalog::default();
    let mut limited = false;
    for index in 0..256 {
        let mut location = location(&path, "one", "w1");
        location.pane_id = format!("w1:p{}", crate::workspace::encode_public_number(index + 1));
        location.workspace_label = "🦀".repeat(256);
        location.tab_label = "\t".repeat(256);
        catalog
            .add_location(location, &mut |event| {
                if let HistoryEvent::Unavailable { message, .. } = event {
                    limited |= message.contains("provenance limit");
                }
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(catalog.sources.len(), 1);
    let source = &catalog.sources.values().next().unwrap().source;
    assert!(serde_json::to_vec(source).unwrap().len() <= MAX_SOURCE_BYTES);
    assert!(limited);
    assert!(catalog.truncated);
}

#[test]
fn live_metadata_uses_the_exact_catalog_socket_and_only_the_history_method() {
    use interprocess::local_socket::traits::Listener as _;
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "");
    let socket = dir.0.join("metadata.sock");
    let listener = crate::ipc::bind_local_listener(&socket).unwrap();
    let expected = live_snapshot(&path, "w1");
    let reply = crate::api::schema::SuccessResponse {
        id: "history:metadata".into(),
        result: ResponseResult::SessionSnapshot {
            snapshot: Box::new(expected.clone()),
        },
    };
    let server = std::thread::spawn(move || {
        let stream = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(request.method, Method::SessionHistorySnapshot(_)));
        let mut bytes = serde_json::to_vec(&reply).unwrap();
        bytes.push(b'\n');
        reader.get_mut().write_all(&bytes).unwrap();
    });
    let mut info = session(&dir, "different-name", true);
    info.socket_path = socket.display().to_string();
    assert_eq!(read_live_snapshot(&info).unwrap(), expected);
    server.join().unwrap();
}

#[test]
fn malformed_future_and_legacy_identity_snapshots_are_explicitly_unavailable() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", "");
    for bytes in [
        b"not json".to_vec(),
        serde_json::to_vec(
            &serde_json::json!({"version":999, "workspaces":[], "active":null, "selected":0}),
        )
        .unwrap(),
        {
            let mut snapshot = saved_snapshot(&path, "w1");
            snapshot["workspaces"][0]["id"] = serde_json::Value::Null;
            serde_json::to_vec(&snapshot).unwrap()
        },
    ] {
        dir.write("one/session.json", bytes);
        let mut unavailable = 0;
        let catalog = Catalog::from_sessions(
            &request(),
            vec![session(&dir, "one", false)],
            |_| panic!("no startup"),
            &mut |event| {
                if matches!(event, HistoryEvent::Unavailable { .. }) {
                    unavailable += 1;
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(catalog.truncated);
        assert!(catalog.sources.is_empty());
        assert_eq!(unavailable, 1);
    }
}
