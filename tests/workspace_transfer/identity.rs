use super::*;

#[test]
fn layout_template_id_of_transferred_pane_keeps_implicit_target_local() {
    let mut sessions = Sessions::new();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    let moving = sessions.create("source", "template origin");
    let original_workspace = moving["workspace"]["workspace_id"].as_str().unwrap();
    let template_id = moving["root_pane"]["pane_id"].as_str().unwrap();
    let local = sessions.create("source", "implicit layout destination");
    let local_workspace = local["workspace"]["workspace_id"].as_str().unwrap();
    let existing = sessions.create("target", "target remains unchanged");
    let status = transfer(
        &source,
        original_workspace,
        "22000000-0000-4000-8000-000000000001",
    );
    assert_eq!(status["state"], "committed", "{status}");
    let target_workspaces = [
        &existing["workspace"]["workspace_id"],
        &status["workspace_id"],
    ];
    let before: Vec<_> = target_workspaces
        .iter()
        .map(|workspace| success(&target, "tab.list", json!({"workspace_id":workspace})))
        .collect();
    let local_before = success(&source, "tab.list", json!({"workspace_id":local_workspace}));
    let applied = success(
        &source,
        "layout.apply",
        json!({"tab_label":"local template", "root":{"type":"pane", "pane_id":template_id}}),
    );
    assert_eq!(applied["layout"]["workspace_id"], local_workspace);
    let local_after = success(&source, "tab.list", json!({"workspace_id":local_workspace}));
    assert_eq!(
        local_after["tabs"].as_array().unwrap().len(),
        local_before["tabs"].as_array().unwrap().len() + 1
    );
    for (workspace, before) in target_workspaces.iter().zip(before) {
        assert_eq!(
            success(&target, "tab.list", json!({"workspace_id":workspace})),
            before
        );
    }
}

#[test]
fn inherited_agent_routes_survive_target_live_handoff_but_reject_cold_replacement() {
    let mut sessions = Sessions::new();
    sessions.start("source");
    sessions.start("target");
    let source = sessions.path("source");
    let target = sessions.path("target");
    sessions.create("target", "preexisting target workspace");
    let moving = sessions.create("source", "inherited agent");
    let workspace = moving["workspace"]["workspace_id"].as_str().unwrap();
    let pane = moving["root_pane"]["pane_id"].as_str().unwrap();
    let terminal = moving["root_pane"]["terminal_id"].clone();
    let shell_pid = pid(&source, pane);
    success(
        &source,
        "pane.report_agent",
        json!({"pane_id":pane,"source":"transfer-test","agent":"codex","state":"idle","seq":1}),
    );
    success(
        &source,
        "agent.rename",
        json!({"target":pane,"name":"inherited"}),
    );
    let status = transfer(&source, workspace, "22000000-0000-4000-8000-000000000002");
    assert_eq!(status["state"], "committed", "{status}");
    let canonical = status["focused_pane_id"].as_str().unwrap();
    for agent_target in [pane, "inherited"] {
        assert_eq!(
            success(&source, "agent.get", json!({"target":agent_target}))["agent"]["terminal_id"],
            terminal
        );
    }
    for handoff in 0..2 {
        success(&target, "server.live_handoff", json!({}));
        success(&target, "ping", json!({}));
        assert_eq!(pid(&target, canonical), shell_pid);
        assert_eq!(
            success(&target, "pane.get", json!({"pane_id":canonical}))["pane"]["terminal_id"],
            terminal
        );
        for agent_target in [pane, "inherited"] {
            let agent = success(&source, "agent.get", json!({"target":agent_target}));
            assert_eq!(agent["agent"]["pane_id"], pane);
            assert_eq!(agent["agent"]["terminal_id"], terminal);
        }
        let marker = format!("INHERITED-HANDOFF-ROUTE-{handoff}");
        send(
            &source,
            pane,
            &format!("printf 'INHERITED-HANDOFF-%s\\n' 'ROUTE-{handoff}'"),
        );
        wait_output(&target, canonical, &marker);
        let read = success(
            &source,
            "agent.read",
            json!({"target":"inherited","source":"recent","lines":20,"format":"text"}),
        );
        assert!(read["read"]["text"].as_str().unwrap().contains(&marker));
    }
    success(&target, "server.stop", json!({}));
    let deadline = Instant::now() + Duration::from_secs(10);
    while target.exists() {
        assert!(Instant::now() < deadline, "target did not stop");
        std::thread::sleep(Duration::from_millis(25));
    }
    sessions.start("target");
    let replacement = success(&target, "pane.get", json!({"pane_id":canonical}));
    assert_ne!(replacement["pane"]["terminal_id"], terminal);
    for agent_target in [pane, "inherited"] {
        let rejected = raw(&source, "agent.get", json!({"target":agent_target}));
        assert!(
            rejected["error"]["message"]
                .as_str()
                .unwrap()
                .contains("pane occupant changed"),
            "{rejected}"
        );
    }
}
