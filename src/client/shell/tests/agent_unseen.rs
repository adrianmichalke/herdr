use super::*;

fn idle_snapshot(status: AgentStatus) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.agents = vec![ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("review-agent".into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: 7,
        state_labels: Vec::new(),
        tokens: vec![("summary".into(), "ready to review".into())],
        focused: false,
    }];
    snapshot
}

fn right_click_agent(
    state: &mut ClientShellState,
    endpoint_id: &ClientEndpointId,
) -> ClientShellInput {
    state.compose(106, 30).expect("agent panel");
    let rect = state
        .hits
        .endpoint_agents
        .iter()
        .find(|(_, endpoint, pane)| endpoint == endpoint_id && pane == "pane_1")
        .map(|(rect, _, _)| *rect)
        .or_else(|| {
            (endpoint_id == &state.active_endpoint_id)
                .then(|| {
                    state
                        .hits
                        .agents
                        .iter()
                        .find(|(_, pane)| pane == "pane_1")
                        .map(|(rect, _)| *rect)
                })
                .flatten()
        })
        .expect("agent hit");
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn mark_unseen_item(state: &ClientShellState) -> Option<usize> {
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        return None;
    };
    menu.items()
        .iter()
        .position(|item| item.label == "Mark unseen")
}

fn activate_mark_unseen(state: &mut ClientShellState) -> ClientShellInput {
    let index = mark_unseen_item(state).expect("Mark unseen action");
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    outcome
}

fn assert_client_only(outcome: &ClientShellInput) {
    assert!(outcome.actions.is_empty());
    assert!(outcome.requests.is_empty());
    assert!(!outcome.detach);
    assert!(!outcome.resize);
}

#[test]
fn mark_unseen_context_action_uses_done_style_without_focus_activity_or_notifications() {
    for style in [
        crate::config::StatusIndicatorStyle::Dots,
        crate::config::StatusIndicatorStyle::Symbols,
    ] {
        let mut config = Config::default();
        config.ui.status_indicators = style;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        let original = idle_snapshot(AgentStatus::Idle);
        state.set_snapshot(Box::new(original.clone()));
        let ranks = state.endpoints[0].agent_recency.clone();
        let focus = state.snapshot.as_ref().unwrap().focused_pane_id.clone();
        assert_client_only(&right_click_agent(&mut state, &ClientEndpointId::Local));
        let outcome = activate_mark_unseen(&mut state);
        assert_client_only(&outcome);
        assert!(outcome.repaint);
        let projected = state.snapshot.as_ref().unwrap();
        assert_eq!(projected.agents[0].agent_status, AgentStatus::Done);
        assert_eq!(projected.agents[0].state_change_seq, 7);
        assert_eq!(projected.agents[0].tokens, original.agents[0].tokens);
        assert_eq!(projected.focused_pane_id, focus);
        assert_eq!(projected.tabs[0].agent_status, AgentStatus::Done);
        assert_eq!(projected.workspaces[0].agent_status, AgentStatus::Done);
        assert_eq!(state.endpoints[0].agent_recency, ranks);
        assert!(state.pending_notifications.is_empty());
        assert!(state.queued_notifications.is_empty());
        assert!(state.visible_notification.is_none());
        let frame = state.compose(106, 30).expect("unseen frame");
        let text = frame_rows(&frame).join("\n");
        assert!(
            text.contains(status_icon(AgentStatus::Done, style)),
            "{text}"
        );
        assert_eq!(original.agents[0].agent_status, AgentStatus::Idle);
        right_click_agent(&mut state, &ClientEndpointId::Local);
        assert_client_only(&activate_mark_unseen(&mut state));
        assert_eq!(
            state.snapshot.as_ref().unwrap().agents[0].agent_status,
            AgentStatus::Done
        );
    }
}

#[test]
fn mark_unseen_context_action_is_unavailable_for_non_idle_agents() {
    for status in [
        AgentStatus::Working,
        AgentStatus::Blocked,
        AgentStatus::Unknown,
    ] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(idle_snapshot(status)));
        assert_client_only(&right_click_agent(&mut state, &ClientEndpointId::Local));
        assert!(mark_unseen_item(&state).is_none(), "{status:?}");
        assert_eq!(
            state.snapshot.as_ref().unwrap().agents[0].agent_status,
            status
        );
    }
}

#[test]
fn mark_unseen_context_action_targets_an_inactive_endpoint_without_navigation() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(idle_snapshot(AgentStatus::Idle)));
    let remote = SavedSshEndpoint::new("Build", "dev@build.example", "agents").unwrap();
    let remote_id = ClientEndpointId::Ssh(remote.id.clone());
    state.set_endpoint_catalog(&[remote]);
    state.set_endpoint_status(&remote_id, ClientEndpointStatus::Online);
    let mut snapshot = idle_snapshot(AgentStatus::Idle);
    snapshot.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote_id, Box::new(snapshot));
    assert_client_only(&right_click_agent(&mut state, &remote_id));
    assert_client_only(&activate_mark_unseen(&mut state));
    assert_eq!(state.active_endpoint_id, ClientEndpointId::Local);
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Idle
    );
    let remote = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == remote_id)
        .unwrap();
    assert_eq!(
        remote.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Done
    );
}

#[test]
fn mark_unseen_context_action_cancellation_and_normal_viewing_use_existing_semantics() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(idle_snapshot(AgentStatus::Idle)));
    right_click_agent(&mut state, &ClientEndpointId::Local);
    let cancelled = state.handle_raw_events(vec![RawInputEvent::Key(
        crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()).into(),
    )]);
    assert_client_only(&cancelled);
    assert!(state.overlay.is_none());
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Idle
    );
    right_click_agent(&mut state, &ClientEndpointId::Local);
    activate_mark_unseen(&mut state);
    state.set_pane_surface(surface());
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Idle
    );
    assert_eq!(
        state.endpoints[0].snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Idle
    );
}

#[test]
fn mark_unseen_context_action_reuses_attention_priority_without_updating_recency() {
    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    let mut snapshot = idle_snapshot(AgentStatus::Idle);
    let mut busy = snapshot.agents[0].clone();
    busy.pane_id = "pane_2".into();
    busy.name = Some("busy-agent".into());
    busy.agent_status = AgentStatus::Working;
    snapshot.agents.push(busy);
    state.set_snapshot(Box::new(snapshot));
    let ranks = state.endpoints[0].agent_recency.clone();
    let first = aggregate_navigation::aggregate_agent_rows(
        &state.endpoints,
        &state.active_endpoint_id,
        crate::config::AgentPanelSortConfig::Priority,
    )[0]
    .agent
    .pane_id
    .clone();
    assert_eq!(first, "pane_2");
    right_click_agent(&mut state, &ClientEndpointId::Local);
    assert_client_only(&activate_mark_unseen(&mut state));
    let rows = aggregate_navigation::aggregate_agent_rows(
        &state.endpoints,
        &state.active_endpoint_id,
        crate::config::AgentPanelSortConfig::Priority,
    );
    assert_eq!(rows[0].agent.pane_id, "pane_1");
    assert_eq!(rows[1].agent.agent_status, AgentStatus::Working);
    assert_eq!(state.endpoints[0].agent_recency, ranks);
}

#[test]
fn mark_unseen_context_action_does_not_apply_after_endpoint_disconnect() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(idle_snapshot(AgentStatus::Idle)));
    right_click_agent(&mut state, &ClientEndpointId::Local);
    let index = mark_unseen_item(&state).unwrap();
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Reconnecting);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert_client_only(&outcome);
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Idle
    );
    right_click_agent(&mut state, &ClientEndpointId::Local);
    assert!(mark_unseen_item(&state).is_none());
}

#[test]
fn mark_unseen_does_not_revive_finished_notifications() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delivery = crate::config::ToastDelivery::Herdr;
    config.toast_delay_seconds = 5;
    let mut state = ClientShellState::new(config);
    let mut working = idle_snapshot(AgentStatus::Working);
    working.agents[0].state_change_seq = 6;
    state.set_snapshot(Box::new(working));
    let mut finished = idle_snapshot(AgentStatus::Idle);
    finished.revision = 2;
    state.set_snapshot(Box::new(finished));
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Done
    );
    let now = std::time::Instant::now();
    let event = SemanticNotification {
        kind: SemanticNotificationKind::Finished,
        title: "finished".into(),
        body: None,
        sound: Some(SemanticNotificationSound::Done),
        agent: Some("pi".into()),
        workspace_id: Some("ws_1".into()),
        tab_id: Some("tab_1".into()),
        pane_id: Some("pane_1".into()),
        position: None,
    };
    assert!(state
        .receive_notification(&ClientEndpointId::Local, event.clone(), now)
        .0
        .is_empty());
    let mut viewed = surface();
    viewed.projection_revision = 2;
    state.set_pane_surface(viewed);
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Idle
    );
    state.pane_surface = None;
    let mut background = idle_snapshot(AgentStatus::Idle);
    background.revision = 3;
    background.focused_tab_id = Some("other-tab".into());
    state.set_snapshot(Box::new(background));
    right_click_agent(&mut state, &ClientEndpointId::Local);
    assert_client_only(&activate_mark_unseen(&mut state));
    let (effects, repaint) = state.tick_notifications(now + std::time::Duration::from_secs(6));
    assert!(
        effects.is_empty(),
        "manual unread must not revive a delayed notification"
    );
    assert!(!repaint);
    assert!(state.visible_notification.is_none());
    assert!(state.pending_notifications.is_empty());
    state.config.toast_delay_seconds = 0;
    assert!(state
        .receive_notification(&ClientEndpointId::Local, event, now)
        .0
        .is_empty());
    assert!(state.visible_notification.is_none());
}

fn patch(surface: &PaneSurfaceFrame) -> crate::protocol::PaneSurfacePatch {
    let mut pane = surface.panes[0].clone();
    pane.content_revision += 1;
    let mut cell = surface.frame.cells[0].clone();
    cell.symbol = "N".into();
    crate::protocol::PaneSurfacePatch {
        boot_id: surface.boot_id.clone(),
        projection_revision: surface.projection_revision,
        base_surface_revision: surface.surface_revision,
        surface_revision: surface.surface_revision + 1,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 0,
            y: 0,
            cells: vec![cell],
        }],
        panes: vec![pane],
        cursor: None,
    }
}

#[test]
fn mark_unseen_surface_patch_acknowledges_visible_agents_and_preserves_the_fast_path() {
    for focused in [true, false] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(idle_snapshot(AgentStatus::Idle)));
        state.set_pane_surface(surface());
        state.compose(106, 30).unwrap();
        right_click_agent(&mut state, &ClientEndpointId::Local);
        activate_mark_unseen(&mut state);
        state.outer_focused = Some(focused);
        state.compose(106, 30).unwrap();
        let current = state.pane_surface.as_ref().unwrap().clone();
        let outcome = state.apply_pane_surface_patch(patch(&current));
        if focused {
            assert!(matches!(
                outcome,
                ClientPaneSurfacePatchOutcome::Applied(None)
            ));
            assert_eq!(
                state.snapshot.as_ref().unwrap().agents[0].agent_status,
                AgentStatus::Idle
            );
        } else {
            assert!(matches!(
                outcome,
                ClientPaneSurfacePatchOutcome::Applied(Some(_))
            ));
            assert_eq!(
                state.snapshot.as_ref().unwrap().agents[0].agent_status,
                AgentStatus::Done
            );
        }
        state.compose(106, 30).unwrap();
        let current = state.pane_surface.as_ref().unwrap().clone();
        assert!(matches!(
            state.apply_pane_surface_patch(patch(&current)),
            ClientPaneSurfacePatchOutcome::Applied(Some(_))
        ));
    }
}

#[test]
fn mark_unseen_background_agent_does_not_disable_visible_pane_patches() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let mut projected = idle_snapshot(AgentStatus::Idle);
    let mut visible = projected.panes[0].clone();
    visible.pane_id = "pane_2".into();
    projected.panes.push(visible);
    projected.focused_pane_id = Some("pane_2".into());
    state.set_snapshot(Box::new(projected));
    let mut pane_surface = surface();
    pane_surface.panes[0].pane_id = "pane_2".into();
    state.set_pane_surface(pane_surface);
    right_click_agent(&mut state, &ClientEndpointId::Local);
    activate_mark_unseen(&mut state);
    state.compose(106, 30).unwrap();
    let current = state.pane_surface.as_ref().unwrap().clone();
    assert!(matches!(
        state.apply_pane_surface_patch(patch(&current)),
        ClientPaneSurfacePatchOutcome::Applied(Some(_))
    ));
    assert_eq!(
        state.snapshot.as_ref().unwrap().agents[0].agent_status,
        AgentStatus::Done
    );
}

#[test]
#[ignore]
fn mark_unseen_render_scale_profile() {
    fn sample(count: usize, marked: bool) -> std::time::Duration {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        let mut snapshot = idle_snapshot(AgentStatus::Idle);
        for number in 2..=count {
            let mut agent = snapshot.agents[0].clone();
            agent.pane_id = format!("pane_{number}");
            snapshot.agents.push(agent);
            let mut pane = snapshot.panes[0].clone();
            pane.pane_id = format!("pane_{number}");
            snapshot.panes.push(pane);
        }
        state.set_snapshot(Box::new(snapshot.clone()));
        if marked {
            state.open_agent_context_menu(ClientEndpointId::Local, "pane_1".into(), 0, 0);
            activate_mark_unseen(&mut state);
        }
        for _ in 0..10 {
            std::hint::black_box(state.compose(120, 40).unwrap());
        }
        let started = std::time::Instant::now();
        for _ in 0..200 {
            snapshot.revision += 1;
            state.set_snapshot(Box::new(snapshot.clone()));
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
        println!(
            "120x40, {count} agent panes, 200 metadata+compose iterations: plain={:?}, marked={:?}, delta={:.2}%",
            plain[2],
            marked[2],
            100.0 * (marked[2].as_secs_f64() / plain[2].as_secs_f64() - 1.0),
        );
    }
}

#[test]
fn mark_unseen_context_action_rechecks_agent_boot_and_lifecycle() {
    for (status, boot, sequence) in [
        (AgentStatus::Working, "boot-1", 8),
        (AgentStatus::Blocked, "boot-1", 8),
        (AgentStatus::Idle, "new-boot", 7),
        (AgentStatus::Idle, "boot-1", 8),
    ] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(idle_snapshot(AgentStatus::Idle)));
        right_click_agent(&mut state, &ClientEndpointId::Local);
        let index = mark_unseen_item(&state).expect("Mark unseen action");
        let mut changed = idle_snapshot(status);
        changed.boot_id = boot.into();
        changed.revision += 1;
        changed.agents[0].state_change_seq = sequence;
        state.set_snapshot(Box::new(changed));
        let mut outcome = ClientShellInput::default();
        state.activate_context_menu_item(index, &mut outcome);
        assert_client_only(&outcome);
        assert_eq!(
            state.snapshot.as_ref().unwrap().agents[0].agent_status,
            status
        );
    }
}
