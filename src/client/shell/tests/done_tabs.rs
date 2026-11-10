use super::*;

fn tab_snapshot() -> ClientShellSnapshot {
    let mut view = snapshot();
    view.tabs[0].label = "reviewed tab".into();
    view.tabs.push(ClientShellTab {
        tab_id: "tab_2".into(),
        number: 2,
        label: "active tab".into(),
        focused: false,
        ..view.tabs[0].clone()
    });
    for number in 1..=3 {
        let tab_id = if number < 3 { "tab_1" } else { "tab_2" };
        if number > 1 {
            view.panes.push(ClientShellPane {
                pane_id: format!("pane_{number}"),
                tab_id: tab_id.into(),
                focused: false,
                ..view.panes[0].clone()
            });
        }
        view.agents.push(ClientShellAgent {
            pane_id: format!("pane_{number}"),
            workspace_id: "ws_1".into(),
            tab_id: tab_id.into(),
            name: Some(format!("agent-{number}")),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Working,
            state_change_seq: number,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: number == 1,
        });
    }
    view
}

fn shell(config: ClientShellConfig) -> ClientShellState {
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(tab_snapshot()));
    state.set_pane_surface(surface());
    state
}

fn assert_presentation_only(outcome: &ClientShellInput) {
    assert!(outcome.actions.is_empty());
    assert!(outcome.requests.is_empty());
    assert!(!outcome.detach);
    assert!(!outcome.resize);
}

fn activate(state: &mut ClientShellState, tab: &str, label: &str) {
    state.open_tab_context_menu(tab.into(), 25, 1);
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        panic!("tab menu");
    };
    let index = menu
        .items()
        .iter()
        .position(|item| item.label == label)
        .expect(label);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert_presentation_only(&outcome);
    assert!(outcome.repaint);
}

fn visible(state: &ClientShellState) -> Vec<String> {
    aggregate_navigation::aggregate_agent_rows(
        &state.endpoints,
        &state.active_endpoint_id,
        state.config.agent_panel_sort,
    )
    .into_iter()
    .map(|row| row.agent.pane_id.clone())
    .collect()
}

#[test]
fn done_tabs_context_menu_hides_and_restores_without_changing_runtime_or_focus() {
    for status in [
        AgentStatus::Idle,
        AgentStatus::Working,
        AgentStatus::Blocked,
        AgentStatus::Done,
        AgentStatus::Unknown,
    ] {
        let mut state = shell(ClientShellConfig::from_config(&Config::default()));
        let mut original = tab_snapshot();
        original.focused_tab_id = Some("tab_2".into());
        original.focused_pane_id = Some("pane_3".into());
        original.workspaces[0].active_tab_id = "tab_2".into();
        for tab in &mut original.tabs {
            tab.focused = tab.tab_id == "tab_2";
        }
        for pane in &mut original.panes {
            pane.focused = pane.pane_id == "pane_3";
        }
        for agent in &mut original.agents {
            agent.agent_status = status;
            agent.focused = agent.pane_id == "pane_3";
        }
        state.set_snapshot(Box::new(original));
        let mut pane_surface = surface();
        pane_surface.panes[0].pane_id = "pane_3".into();
        state.set_pane_surface(pane_surface);
        let original = state.snapshot.as_deref().unwrap().clone();
        let recency = state.endpoints[0].agent_recency.clone();
        state.compose(120, 35).expect("initial frame");
        let tab = state
            .hits
            .tabs
            .iter()
            .find(|(_, id)| id == "tab_1")
            .unwrap()
            .0;
        let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: tab.x,
            row: tab.y,
            modifiers: KeyModifiers::empty(),
        })]);
        assert_presentation_only(&outcome);
        let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
            panic!("right-click menu")
        };
        let index = menu
            .items()
            .iter()
            .position(|item| item.label == "Mark done")
            .unwrap();
        let mut outcome = ClientShellInput::default();
        state.activate_context_menu_item(index, &mut outcome);
        assert_presentation_only(&outcome);
        assert_eq!(visible(&state), ["pane_3"]);
        assert_eq!(state.snapshot.as_deref(), Some(&original));
        assert_eq!(state.endpoints[0].snapshot.as_deref(), Some(&original));
        assert_eq!(state.endpoints[0].agent_recency, recency);
        assert!(state.pending_notifications.is_empty());
        let frame = state.compose(120, 35).expect("done frame");
        assert!(frame_rows(&frame).join("\n").contains("reviewed tab ✓"));
        assert_eq!(
            state
                .hits
                .agents
                .iter()
                .map(|(_, id)| id.as_str())
                .collect::<Vec<_>>(),
            ["pane_3"]
        );
        activate(&mut state, "tab_1", "Mark active");
        assert_eq!(visible(&state).len(), 3);
        assert_eq!(state.snapshot.as_deref(), Some(&original));
    }
}

#[test]
fn done_tabs_remain_hidden_through_agent_changes_and_local_navigation_skips_them() {
    let mut state = shell(ClientShellConfig::from_config(&Config::default()));
    activate(&mut state, "tab_1", "Mark done");
    let mut changed = tab_snapshot();
    changed.revision += 1;
    changed.agents[0].agent_status = AgentStatus::Blocked;
    changed.agents[0].state_change_seq += 10;
    state.set_snapshot(Box::new(changed));
    let mut pane_surface = surface();
    pane_surface.projection_revision = 2;
    state.set_pane_surface(pane_surface);
    assert_eq!(visible(&state), ["pane_3"]);
    for action in [
        crate::input::KeybindAction::FocusAgent(0),
        crate::input::KeybindAction::NextAgent,
        crate::input::KeybindAction::PreviousAgent,
    ] {
        assert!(
            matches!(state.endpoint_method_for_action(action), Some(crate::api::schema::Method::PaneFocus(target)) if target.pane_id == "pane_3")
        );
    }
    assert!(state
        .endpoint_method_for_action(crate::input::KeybindAction::FocusAgent(1))
        .is_none());
    state.sidebar_collapsed = true;
    state.compose(120, 35).unwrap();
    assert_eq!(
        state
            .hits
            .agents
            .iter()
            .map(|(_, id)| id.as_str())
            .collect::<Vec<_>>(),
        ["pane_3"]
    );
    state.mode = ClientShellMode::Navigate;
    state.compose(44, 45).unwrap();
    let targets = state
        .hits
        .mobile_targets
        .iter()
        .filter_map(|(_, target)| match target {
            ClientMobileTarget::Agent { pane_id, .. } => Some(pane_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(targets, ["pane_3"]);
}

#[test]
fn done_tabs_apply_to_legacy_and_modern_agent_views_without_changing_them() {
    let mut state = shell(ClientShellConfig::from_config(&Config::default()));
    activate(&mut state, "tab_1", "Mark done");
    let mut view = tab_snapshot();
    view.agent_view_label = Some("explicit".into());
    view.agent_order = vec!["pane_2".into(), "pane_3".into(), "pane_1".into()];
    state.set_snapshot(Box::new(view));
    assert_eq!(visible(&state), ["pane_3"]);
    let order = agent_sidebar::ordered_agent_pane_ids(
        state.snapshot.as_deref().unwrap(),
        state.config.agent_panel_sort,
        None,
        state.active_done_tabs(),
    );
    assert_eq!(order, ["pane_3"]);
    state.endpoints[0].agent_view_projection_supported = true;
    let mut view = tab_snapshot();
    view.agent_view_label = None;
    state.set_snapshot(Box::new(view));
    assert_eq!(visible(&state), ["pane_3"]);
    state.set_test_endpoint_agent_view(
        &ClientEndpointId::Local,
        Some(crate::api::schema::AgentViewSetParams {
            source: "review-test".into(),
            label: Some("all agents".into()),
            filter: None,
            sort: vec![crate::api::schema::AgentViewSort {
                field: crate::api::schema::AgentViewSortField::Builtin(
                    crate::api::schema::AgentViewBuiltinSortField::StateChangeSeq,
                ),
                order: crate::api::schema::AgentViewSortOrder::Asc,
            }],
        }),
    );
    assert_eq!(visible(&state), ["pane_3"]);
}

#[test]
fn done_tabs_are_scoped_to_the_endpoint_when_public_ids_overlap() {
    let mut state = shell(ClientShellConfig::from_config(&Config::default()));
    let remote = SavedSshEndpoint::new("Build", "dev@build.example", "agents").unwrap();
    let remote_id = ClientEndpointId::Ssh(remote.id.clone());
    state.set_endpoint_catalog(std::slice::from_ref(&remote));
    state.set_endpoint_status(&remote_id, ClientEndpointStatus::Online);
    let mut remote_view = tab_snapshot();
    remote_view.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote_id, Box::new(remote_view));
    activate(&mut state, "tab_1", "Mark done");
    let rows = aggregate_navigation::aggregate_agent_rows(
        &state.endpoints,
        &state.active_endpoint_id,
        state.config.agent_panel_sort,
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.endpoint.endpoint_id == &ClientEndpointId::Local)
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.endpoint.endpoint_id == &remote_id)
            .count(),
        3
    );
    state.set_endpoint_catalog(&[remote]);
    assert!(state.endpoints[0].done_tabs.contains("tab_1"));
    state.compose(120, 40).unwrap();
    assert_eq!(state.hits.endpoint_agents.len(), 4);
}

#[test]
fn done_tabs_context_action_revalidates_boot_session_and_tab() {
    for invalid in 0..4 {
        let mut state = shell(ClientShellConfig::from_config(&Config::default()));
        state.open_tab_context_menu("tab_1".into(), 25, 1);
        let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
            panic!()
        };
        let index = menu
            .items()
            .iter()
            .position(|item| item.label == "Mark done")
            .unwrap();
        match invalid {
            0 => state.endpoints[0].session_name = "different".into(),
            1 => state
                .set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting),
            2 => {
                let mut view = tab_snapshot();
                view.boot_id = "new-boot".into();
                state.set_snapshot(Box::new(view));
            }
            _ => {
                let mut view = tab_snapshot();
                view.tabs.retain(|tab| tab.tab_id != "tab_1");
                state.set_snapshot(Box::new(view));
            }
        }
        let mut outcome = ClientShellInput::default();
        state.activate_context_menu_item(index, &mut outcome);
        assert_presentation_only(&outcome);
        assert!(state.endpoints[0].done_tabs.is_empty());
    }
}

fn temporary_directory() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "herdr-done-tabs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn persistent_config(
    directory: &std::path::Path,
    launch: &str,
    session: &str,
) -> ClientShellConfig {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.initial_session_name = session.into();
    let name = crate::session::parse_target_name(session).unwrap();
    config
        .with_local_endpoint(&crate::session::client_socket_path_for(name.as_deref()))
        .with_preferences_path(directory.join(format!("{launch}.json")))
}

#[test]
fn done_tabs_persist_across_launch_sessions_and_unrelated_chrome_saves() {
    let directory = temporary_directory();
    let mut first = shell(persistent_config(&directory, "launch-a", "default"));
    activate(&mut first, "tab_1", "Mark done");
    let mut second = shell(persistent_config(&directory, "launch-b", "review"));
    activate(&mut second, "tab_2", "Mark done");
    let mut outcome = ClientShellInput::default();
    first.sidebar_width = 31;
    first.persist_chrome_preferences(&mut outcome);
    let reopened = shell(persistent_config(&directory, "launch-b", "default"));
    assert_eq!(visible(&reopened), ["pane_3"]);
    let mut reopened = shell(persistent_config(&directory, "launch-a", "review"));
    assert_eq!(visible(&reopened), ["pane_1", "pane_2"]);
    activate(&mut reopened, "tab_2", "Mark active");
    let reopened = shell(persistent_config(&directory, "launch-b", "review"));
    assert_eq!(visible(&reopened).len(), 3);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn done_tabs_closed_tabs_are_pruned_without_changing_other_marks() {
    let directory = temporary_directory();
    let mut state = shell(persistent_config(&directory, "launch", "default"));
    activate(&mut state, "tab_1", "Mark done");
    activate(&mut state, "tab_2", "Mark done");
    let mut view = tab_snapshot();
    view.tabs.retain(|tab| tab.tab_id == "tab_2");
    state.set_snapshot(Box::new(view));
    assert_eq!(
        state.endpoints[0].done_tabs,
        HashSet::from(["tab_2".into()])
    );
    let reopened = shell(persistent_config(&directory, "launch", "default"));
    assert_eq!(visible(&reopened), ["pane_1", "pane_2"]);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn done_tabs_session_candidates_load_their_own_preferences() {
    let directory = temporary_directory();
    let mut reviewed = shell(persistent_config(&directory, "review-launch", "review"));
    activate(&mut reviewed, "tab_1", "Mark done");
    let mut state = shell(persistent_config(&directory, "default-launch", "default"));
    let candidate = ClientEndpointId::Local.with_session("review").unwrap();
    assert!(state.stage_session_endpoint(
        &ClientEndpointId::Local,
        candidate.clone(),
        "review".into()
    ));
    let endpoint = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == candidate)
        .unwrap();
    assert!(endpoint.done_tabs.contains("tab_1"));
    assert!(state.endpoints[0].done_tabs.is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn done_tabs_remote_root_and_local_session_preferences_do_not_alias() {
    let directory = temporary_directory();
    let mut config = persistent_config(&directory, "remote", "default");
    config.root_remote_target = Some("dev@build.example".into());
    let mut remote = shell(config);
    activate(&mut remote, "tab_1", "Mark done");
    let local = shell(persistent_config(&directory, "local", "default"));
    assert_eq!(visible(&local).len(), 3);
    let mut config = persistent_config(&directory, "remote-other-launch", "default");
    config.root_remote_target = Some("dev@build.example".into());
    let remote = shell(config);
    assert_eq!(visible(&remote), ["pane_3"]);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn done_tabs_custom_local_sockets_do_not_share_marks() {
    let directory = temporary_directory();
    let config = |socket, launch: &str| {
        ClientShellConfig::from_config(&Config::default())
            .with_local_endpoint(std::path::Path::new(socket))
            .with_preferences_path(directory.join(format!("{launch}.json")))
    };
    let mut first = shell(config("/tmp/done-tabs-first.sock", "first"));
    activate(&mut first, "tab_1", "Mark done");
    let second = shell(config("/tmp/done-tabs-second.sock", "second"));
    assert_eq!(visible(&second).len(), 3);
    let reopened = shell(config("/tmp/done-tabs-first.sock", "other-launch"));
    assert_eq!(visible(&reopened), ["pane_3"]);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn done_tabs_stale_snapshots_and_filtered_agent_lists_do_not_prune_marks() {
    let mut state = shell(ClientShellConfig::from_config(&Config::default()));
    activate(&mut state, "tab_1", "Mark done");
    let mut stale = tab_snapshot();
    stale.revision = 0;
    stale.tabs.clear();
    state.set_snapshot(Box::new(stale));
    assert!(state.endpoints[0].done_tabs.contains("tab_1"));
    let mut filtered = tab_snapshot();
    filtered.revision += 1;
    filtered.agents.clear();
    state.set_snapshot(Box::new(filtered));
    assert!(state.endpoints[0].done_tabs.contains("tab_1"));
}

#[test]
fn done_tabs_write_errors_do_not_send_requests_or_change_focus() {
    let directory = temporary_directory();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("done-tabs"), "not a directory").unwrap();
    let mut state = shell(persistent_config(&directory, "launch", "default"));
    let focus = state.snapshot.as_ref().unwrap().focused_pane_id.clone();
    activate(&mut state, "tab_1", "Mark done");
    assert!(state.endpoint_error.is_some());
    assert_eq!(visible(&state), ["pane_3"]);
    assert_eq!(state.snapshot.as_ref().unwrap().focused_pane_id, focus);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn done_tabs_moving_to_a_new_tab_identity_reopens_its_agents() {
    let mut state = shell(ClientShellConfig::from_config(&Config::default()));
    activate(&mut state, "tab_1", "Mark done");
    let mut moved = tab_snapshot();
    moved.tabs[0].tab_id = "moved-tab".into();
    for pane in &mut moved.panes {
        if pane.tab_id == "tab_1" {
            pane.tab_id = "moved-tab".into();
        }
    }
    for agent in &mut moved.agents {
        if agent.tab_id == "tab_1" {
            agent.tab_id = "moved-tab".into();
        }
    }
    moved.focused_tab_id = Some("moved-tab".into());
    moved.workspaces[0].active_tab_id = "moved-tab".into();
    state.set_snapshot(Box::new(moved));
    assert!(state.endpoints[0].done_tabs.is_empty());
    assert_eq!(visible(&state).len(), 3);
}

#[test]
fn done_tabs_indicator_handles_zoom_unicode_and_wrapping() {
    let mut config = Config::default();
    config.ui.tab_bar_wrap = true;
    let mut state = shell(ClientShellConfig::from_config(&config));
    let mut view = tab_snapshot();
    view.tabs[0].label = "日本語 reviewed".into();
    view.tabs[0].zoomed = true;
    state.set_snapshot(Box::new(view));
    activate(&mut state, "tab_1", "Mark done");
    for width in [90, 120] {
        let frame = state.compose(width, 35).unwrap();
        let text = frame_rows(&frame).join("\n");
        assert!(
            text.contains("reviewed Z ✓")
                && ['日', '本', '語']
                    .iter()
                    .all(|character| text.contains(*character)),
            "{text:?}"
        );
        assert!(state.hits.tabs.iter().any(|(_, tab)| tab == "tab_1"));
    }
}

#[test]
fn done_tabs_wrapping_transition_requests_resize_and_fences_the_old_surface() {
    let mut config = Config::default();
    config.ui.tab_bar_wrap = true;
    let mut state = shell(ClientShellConfig::from_config(&config));
    let (cols, rows) = (80, 20);
    let available = state.layout(cols, rows).tab_bar.width - NEW_TAB_WIDTH;
    let mut view = tab_snapshot();
    view.tabs[0].label = "a".repeat(18);
    view.tabs[1].label = "b".repeat(usize::from(available - 27));
    state.set_snapshot(Box::new(view));
    let make_surface = |size: ClientSurfaceSize, revision| {
        let mut frame = surface();
        frame.surface_revision = revision;
        frame.frame = FrameData::from_ratatui_buffer(
            &ratatui::buffer::Buffer::empty(Rect::new(0, 0, size.cols, size.rows)),
            None,
        );
        frame.panes[0].rect = SurfaceRect {
            x: 0,
            y: 0,
            width: size.cols,
            height: size.rows,
        };
        frame.panes[0].inner_rect = frame.panes[0].rect;
        frame
    };
    let size = state.surface_size(cols, rows);
    state.expect_pane_surface_size(size);
    state.set_pane_surface(make_surface(size, 1));
    state.compose(cols, rows).unwrap();
    assert_eq!(state.layout(cols, rows).tab_bar.height, 1);
    for (label, expected_height, revision) in [("Mark done", 2, 2), ("Mark active", 1, 3)] {
        state.open_tab_context_menu("tab_1".into(), 25, 1);
        let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
            panic!()
        };
        let index = menu
            .items()
            .iter()
            .position(|item| item.label == label)
            .unwrap();
        let mut outcome = ClientShellInput::default();
        state.activate_context_menu_item(index, &mut outcome);
        assert!(outcome.resize && outcome.repaint);
        assert!(outcome.actions.is_empty() && outcome.requests.is_empty());
        assert_eq!(state.layout(cols, rows).tab_bar.height, expected_height);
        assert!(state.pane_surface.is_none());
        assert!(state.last_composed_pane_surface.is_none());
        let size = state.surface_size(cols, rows);
        state.expect_pane_surface_size(size);
        state.set_pane_surface(make_surface(size, revision));
        assert!(state.compose(cols, rows).is_some());
        assert_eq!(state.pane_surface.as_ref().unwrap().frame.height, size.rows);
    }
}

#[test]
#[ignore]
fn done_tabs_render_scale_profile() {
    fn sample(count: usize, marked: bool) -> std::time::Duration {
        let mut state = shell(ClientShellConfig::from_config(&Config::default()));
        let mut view = tab_snapshot();
        view.agents.truncate(1);
        view.panes.truncate(1);
        for number in 2..=count {
            let mut agent = view.agents[0].clone();
            agent.pane_id = format!("pane_{number}");
            view.agents.push(agent);
            let mut pane = view.panes[0].clone();
            pane.pane_id = format!("pane_{number}");
            view.panes.push(pane);
        }
        state.set_snapshot(Box::new(view.clone()));
        let area = state.layout(120, 40).pane_surface;
        let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, area.width, area.height));
        let mut frame = surface();
        let template = frame.panes[0].clone();
        frame.panes.clear();
        let columns = if count == 1 { 1 } else { 3 };
        let rows = count.div_ceil(columns);
        let width = area.width / columns as u16;
        let height = area.height / rows as u16;
        for number in 0..count {
            let x = (number % columns) as u16 * width;
            let y = (number / columns) as u16 * height;
            buffer.set_string(
                x + 1,
                y + 1,
                format!("Agent pane {}", number + 1),
                Style::default(),
            );
            frame.panes.push(PaneSurfacePane {
                pane_id: format!("pane_{}", number + 1),
                rect: SurfaceRect {
                    x,
                    y,
                    width,
                    height,
                },
                inner_rect: SurfaceRect {
                    x: x + 1,
                    y: y + 1,
                    width: width.saturating_sub(2),
                    height: height.saturating_sub(2),
                },
                ..template.clone()
            });
        }
        frame.frame = FrameData::from_ratatui_buffer(&buffer, None);
        state.set_pane_surface(frame.clone());
        if marked {
            activate(&mut state, "tab_2", "Mark done");
        }
        for _ in 0..10 {
            std::hint::black_box(state.compose(120, 40).unwrap());
        }
        let started = std::time::Instant::now();
        for _ in 0..200 {
            view.revision += 1;
            frame.projection_revision = view.revision;
            frame.surface_revision += 1;
            state.set_snapshot(Box::new(view.clone()));
            state.set_pane_surface(frame.clone());
            std::hint::black_box(state.compose(120, 40).unwrap());
        }
        started.elapsed()
    }
    for count in [1, 15] {
        let mut plain = Vec::new();
        let mut marked = Vec::new();
        for _ in 0..5 {
            plain.push(sample(count, false));
            marked.push(sample(count, true));
        }
        plain.sort();
        marked.sort();
        println!("120x40, {count} agent panes, 200 metadata+compose iterations: plain={:?}, marked={:?}, delta={:.2}%", plain[2], marked[2], 100.0 * (marked[2].as_secs_f64() / plain[2].as_secs_f64() - 1.0));
    }
}
