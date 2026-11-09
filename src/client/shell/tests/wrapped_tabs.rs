use super::*;

fn projected_tabs(count: usize) -> ClientShellSnapshot {
    let mut projected = snapshot();
    projected.tabs = (1..=count)
        .map(|number| ClientShellTab {
            tab_id: format!("tab_{number}"),
            workspace_id: "ws_1".into(),
            number,
            label: number.to_string(),
            custom_label: false,
            zoomed: false,
            focused: number == 1,
            agent_status: AgentStatus::Idle,
        })
        .collect();
    projected
}

fn wrapped_state(count: usize) -> ClientShellState {
    let mut config = Config::default();
    config.ui.tab_bar_wrap = true;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(projected_tabs(count)));
    state.set_pane_surface(surface());
    state
}

fn mouse_at(state: &mut ClientShellState, rect: Rect, kind: MouseEventKind) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind,
        column: rect.x + rect.width / 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

#[test]
fn wrapped_tabs_use_only_needed_rows_and_keep_legacy_default() {
    for (count, height) in [(1, 1), (5, 1), (6, 2), (11, 3), (30, 3)] {
        let mut state = wrapped_state(count);
        assert_eq!(state.layout(80, 20).tab_bar.height, height, "{count} tabs");
        state.compose(80, 20).unwrap();
        assert!(state.hits.tabs.iter().all(|(rect, _)| rect.y < height));
        assert_eq!(state.hits.new_tab.y, 0);
        assert_eq!(state.hits.tab_max_scroll > 0, count > 15);
    }
    let mut state = wrapped_state(30);
    state.config.tab_bar_wrap = false;
    assert_eq!(state.layout(80, 20).tab_bar.height, 1);
    state.compose(80, 20).unwrap();
    assert!(state.hits.tabs.iter().all(|(rect, _)| rect.y == 0));
    assert!(state.hits.tab_body.is_empty());
}

#[test]
fn wrapped_tabs_scroll_rows_without_switching_tabs_and_reveal_focus() {
    let mut state = wrapped_state(30);
    state.compose(80, 20).unwrap();
    let body = state.hits.tab_body;
    let focused = state.snapshot.as_ref().unwrap().focused_tab_id.clone();
    let outcome = mouse_at(&mut state, body, MouseEventKind::ScrollDown);
    assert!(outcome.repaint);
    assert!(outcome.requests.is_empty());
    assert!(outcome.actions.is_empty());
    assert_eq!(state.tab_scroll, 1);
    state.compose(80, 20).unwrap();
    assert_eq!(state.snapshot.as_ref().unwrap().focused_tab_id, focused);
    assert!(!state.hits.tabs.iter().any(|(_, id)| id == "tab_1"));
    let mut projected = state.snapshot.as_ref().unwrap().as_ref().clone();
    projected.focused_tab_id = Some("tab_30".into());
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "tab_30";
    }
    state.set_snapshot(Box::new(projected));
    state.compose(80, 20).unwrap();
    assert!(state.hits.tabs.iter().any(|(_, id)| id == "tab_30"));
    assert_eq!(state.tab_scroll, state.hits.tab_max_scroll);
    let down = state.hits.tab_scroll_right;
    mouse_at(&mut state, down, MouseEventKind::Down(MouseButton::Left));
    assert_eq!(state.tab_scroll, state.hits.tab_max_scroll);
    let up = state.hits.tab_scroll_left;
    mouse_at(&mut state, up, MouseEventKind::Down(MouseButton::Left));
    assert_eq!(state.tab_scroll + 1, state.hits.tab_max_scroll);
    state.config.tab_bar_wrap = false;
    state.compose(80, 20).unwrap();
    assert!(state.hits.tabs.iter().any(|(_, id)| id == "tab_30"));
}

#[test]
fn wrapped_tabs_resize_reflows_and_preserves_useful_pane_area() {
    let mut state = wrapped_state(30);
    state.config.tab_bar_max_rows = u16::MAX;
    for rows in 0..10 {
        let layout = state.layout(80, rows);
        assert!(layout.tab_bar.height <= rows);
        if rows >= 3 {
            assert!(layout.pane_surface.height >= crate::pane::MIN_PANE_ROWS);
        }
        if rows <= 1 {
            assert!(layout.tab_bar.is_empty());
        }
        if rows == 2 {
            assert_eq!(layout.tab_bar.height, 1);
        }
    }
    state.config.tab_bar_max_rows = 3;
    state.compose(80, 20).unwrap();
    state.tab_scroll = usize::MAX;
    state.compose(80, 20).unwrap();
    assert_eq!(state.tab_scroll, state.hits.tab_max_scroll);
    state.compose(320, 20).unwrap();
    assert_eq!(state.tab_scroll, 0);
    assert_eq!(state.layout(320, 20).tab_bar.height, 1);
    state.config.hide_tab_bar_when_single_tab = true;
    state.set_snapshot(Box::new(projected_tabs(1)));
    assert!(state.layout(80, 20).tab_bar.is_empty());
    state.config.hide_tab_bar_when_single_tab = false;
    assert!(!state.layout(40, 20).mobile_header.is_empty());
    assert!(state.layout(40, 20).tab_bar.is_empty());
}

#[test]
fn wrapped_tabs_status_and_compact_selector_do_not_overlap_tabs() {
    let mut state = wrapped_state(16);
    let mut projected = state.snapshot.as_ref().unwrap().as_ref().clone();
    projected.tab_bar_right = vec![crate::protocol::ClientShellTabStatusSegment {
        text: "HOST".into(),
        accent: false,
    }];
    state.set_snapshot(Box::new(projected));
    state.sidebar_collapsed = true;
    state.config.sidebar_collapsed_mode = crate::config::SidebarCollapsedModeConfig::Hidden;
    let frame = state.compose(106, 20).unwrap();
    let selector = state.hits.session_selector;
    assert!(!selector.is_empty());
    assert!(state
        .hits
        .tabs
        .iter()
        .all(|(rect, _)| !rect.intersects(selector) && rect.x >= selector.right()));
    assert!(frame_rows(&frame)[0].contains("HOST"));
    assert!(state.hits.tabs.iter().all(|(rect, _)| rect.right() < 102));
    assert!(state
        .hits
        .tabs
        .iter()
        .all(|(rect, _)| rect.y < state.layout(106, 20).tab_bar.bottom()));
}

#[test]
fn wrapped_tabs_bottom_mode_bar_only_removes_occluded_hits() {
    let mut state = wrapped_state(15);
    state.config.tab_bar_position = crate::config::TabBarPositionConfig::Bottom;
    state.mode = ClientShellMode::Prefix;
    state.compose(80, 20).unwrap();
    let area = state.layout(80, 20).tab_bar;
    assert_eq!(area.height, 3);
    assert!(!state.hits.tabs.is_empty());
    assert!(state
        .hits
        .tabs
        .iter()
        .all(|(rect, _)| rect.y >= area.y && rect.y < 19));
    assert_eq!(state.hits.new_tab.y, area.y);
    assert_eq!(state.hits.tab_body.bottom(), 19);
}

#[test]
fn wrapped_tabs_click_context_and_close_use_second_row_ids() {
    let mut state = wrapped_state(12);
    state.config.confirm_close = false;
    state.compose(80, 20).unwrap();
    let (rect, id) = state
        .hits
        .tabs
        .iter()
        .find(|(rect, _)| rect.y == 1)
        .unwrap()
        .clone();
    mouse_at(&mut state, rect, MouseEventKind::Down(MouseButton::Left));
    let outcome = mouse_at(&mut state, rect, MouseEventKind::Up(MouseButton::Left));
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint { request, .. } if matches!(&request.method, crate::api::schema::Method::TabFocus(params) if params.tab_id == id))));
    mouse_at(&mut state, rect, MouseEventKind::Down(MouseButton::Right));
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        panic!("tab context menu");
    };
    let close = menu
        .items()
        .iter()
        .position(|item| matches!(item.action, ClientContextMenuAction::Close))
        .unwrap();
    state.compose(80, 20).unwrap();
    let close = state
        .hits
        .context_menu_rows
        .iter()
        .find(|(_, index)| *index == close)
        .unwrap()
        .0;
    let outcome = mouse_at(&mut state, close, MouseEventKind::Down(MouseButton::Left));
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint { request, .. } if matches!(&request.method, crate::api::schema::Method::TabClose(params) if params.tab_id == id))));
}

#[test]
fn wrapped_tabs_drag_targets_and_indicator_follow_pointer_row() {
    let mut state = wrapped_state(12);
    state.compose(80, 20).unwrap();
    let source = state.hits.tabs[0].0;
    let target = state
        .hits
        .tabs
        .iter()
        .find(|(rect, _)| rect.y == 1)
        .unwrap()
        .0;
    mouse_at(&mut state, source, MouseEventKind::Down(MouseButton::Left));
    mouse_at(&mut state, target, MouseEventKind::Drag(MouseButton::Left));
    assert!(
        matches!(state.chrome_drag, Some(ClientChromeDrag::Tab { insert_index: Some(index), .. }) if index >= 5)
    );
    let frame = state.compose(80, 20).unwrap();
    assert!(frame_rows(&frame)[1].contains('│'));
    let outcome = mouse_at(&mut state, target, MouseEventKind::Up(MouseButton::Left));
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint { request, .. } if matches!(&request.method, crate::api::schema::Method::TabMove(params) if params.tab_id == "tab_1" && params.insert_index >= 5))));
    state.compose(80, 20).unwrap();
    mouse_at(&mut state, source, MouseEventKind::Down(MouseButton::Left));
    mouse_at(&mut state, target, MouseEventKind::Drag(MouseButton::Left));
    mouse_at(
        &mut state,
        Rect::new(target.x, 10, target.width, 1),
        MouseEventKind::Drag(MouseButton::Left),
    );
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Tab {
            insert_index: None,
            ..
        })
    ));
}

#[test]
fn wrapped_tabs_clip_only_overwide_titles_and_keep_unicode_names() {
    let mut state = wrapped_state(3);
    let mut projected = state.snapshot.as_ref().unwrap().as_ref().clone();
    projected.tabs[0].label = "界🙂 file".into();
    projected.tabs[1].label = "x".repeat(300);
    projected.tabs[2].zoomed = true;
    state.set_snapshot(Box::new(projected));
    let frame = state.compose(80, 20).unwrap();
    let title = &frame_rows(&frame)[0];
    assert!(
        title.contains('界') && title.contains('🙂') && title.contains("file"),
        "{title:?}"
    );
    let wide = state
        .hits
        .tabs
        .iter()
        .find(|(_, id)| id == "tab_2")
        .unwrap()
        .0;
    assert_eq!(wide.y, 1);
    assert_eq!(wide.width, state.hits.tab_body.width);
    assert_eq!(
        state
            .hits
            .tabs
            .iter()
            .find(|(_, id)| id == "tab_3")
            .unwrap()
            .0
            .y,
        2
    );
}

#[test]
fn wrapped_tabs_geometry_change_blocks_patches_until_successful_composition() {
    let mut state = wrapped_state(30);
    state.compose(80, 20).unwrap();
    let previous = state.last_composed_pane_surface;
    state.config.tab_bar_max_rows = 1;
    state.pending_pane_surface = Some(surface());
    assert!(state.compose(80, 20).is_none());
    assert_eq!(state.last_composed_pane_surface, previous);
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: Vec::new(),
        panes: Vec::new(),
        cursor: Some(crate::protocol::CursorState {
            x: 1,
            y: 1,
            visible: true,
            shape: 2,
        }),
    };
    assert!(matches!(
        state.apply_pane_surface_patch(patch),
        ClientPaneSurfacePatchOutcome::Applied(None)
    ));
    assert_eq!(state.last_composed_pane_surface, previous);
}

#[test]
fn wrapped_tabs_without_mouse_chrome_still_scroll_rows() {
    let mut state = wrapped_state(40);
    state.config.mouse_capture = false;
    state.compose(80, 20).unwrap();
    assert!(state.hits.new_tab.is_empty());
    assert!(state.hits.tab_scroll_left.is_empty());
    assert!(state.hits.tab_scroll_right.is_empty());
    let body = state.hits.tab_body;
    let outcome = mouse_at(&mut state, body, MouseEventKind::ScrollDown);
    assert!(outcome.actions.is_empty());
    assert_eq!(state.tab_scroll, 1);
}

#[test]
#[ignore = "fixed-geometry wrapped tab composition scaling profile"]
fn wrapped_tab_bar_render_scale_profile() {
    for count in [1, 15] {
        let mut state = wrapped_state(40);
        let mut projected = state.snapshot.as_ref().unwrap().as_ref().clone();
        let pane_template = projected.panes[0].clone();
        projected.panes = (1..=count)
            .map(|index| {
                let mut pane = pane_template.clone();
                pane.pane_id = format!("pane_{index}");
                pane.focused = index == 1;
                pane
            })
            .collect();
        state.set_snapshot(Box::new(projected));
        let mut next = surface();
        next.frame =
            FrameData::from_ratatui_buffer(&Buffer::with_lines(vec!["x".repeat(94); 37]), None);
        let pane = next.panes[0].clone();
        next.panes = (0..count)
            .map(|index| {
                let mut pane = pane.clone();
                pane.pane_id = format!("pane_{}", index + 1);
                pane.focused = index == 0;
                pane.rect = SurfaceRect {
                    x: if count == 1 {
                        0
                    } else {
                        (index % 5) as u16 * 18
                    },
                    y: if count == 1 {
                        0
                    } else {
                        (index / 5) as u16 * 12
                    },
                    width: if count == 1 { 94 } else { 18 },
                    height: if count == 1 { 37 } else { 12 },
                };
                pane.inner_rect = pane.rect;
                pane
            })
            .collect();
        state.set_pane_surface(next);
        for wrap in [false, true] {
            state.config.tab_bar_wrap = wrap;
            for _ in 0..20 {
                std::hint::black_box(state.compose(120, 40).unwrap());
            }
            let mut samples = Vec::new();
            for _ in 0..200 {
                let started = std::time::Instant::now();
                std::hint::black_box(state.compose(120, 40).unwrap());
                samples.push(started.elapsed().as_micros());
            }
            samples.sort_unstable();
            eprintln!(
                "wrapped-tabs panes={count} wrap={wrap} median_us={} p95_us={}",
                samples[100], samples[190]
            );
            assert!(state.pending_requests.is_empty());
        }
    }
}

#[test]
fn wrapped_tabs_bottom_modes_reveal_the_last_tab_in_unoccluded_rows() {
    let mut state = wrapped_state(30);
    state.config.tab_bar_position = crate::config::TabBarPositionConfig::Bottom;
    let mut projected = state.snapshot.as_ref().unwrap().as_ref().clone();
    projected.focused_tab_id = Some("tab_30".into());
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "tab_30";
    }
    state.set_snapshot(Box::new(projected));
    for mode in [
        ClientShellMode::Prefix,
        ClientShellMode::Copy,
        ClientShellMode::Resize,
        ClientShellMode::Terminal,
    ] {
        state.mode = mode;
        state.compose(80, 20).unwrap();
        let rect = state
            .hits
            .tabs
            .iter()
            .find(|(_, id)| id == "tab_30")
            .expect("focused tab visible above mode bar")
            .0;
        assert!(
            rect.y
                < if mode == ClientShellMode::Terminal {
                    20
                } else {
                    19
                }
        );
        assert_eq!(state.layout(80, 20).pane_surface.height, 17);
    }
}

#[test]
fn wrapped_tabs_drag_wheel_uses_the_same_target_for_indicator_and_release() {
    let mut state = wrapped_state(30);
    state.compose(80, 20).unwrap();
    let source = state.hits.tabs[0].0;
    let target = state
        .hits
        .tabs
        .iter()
        .find(|(rect, _)| rect.y == 1)
        .unwrap()
        .0;
    mouse_at(&mut state, source, MouseEventKind::Down(MouseButton::Left));
    mouse_at(&mut state, target, MouseEventKind::Drag(MouseButton::Left));
    let Some(ClientChromeDrag::Tab {
        insert_index: Some(before),
        ..
    }) = state.chrome_drag
    else {
        panic!("drag target");
    };
    mouse_at(&mut state, target, MouseEventKind::ScrollDown);
    let frame = state.compose(80, 20).unwrap();
    let Some(ClientChromeDrag::Tab {
        insert_index: Some(after),
        ..
    }) = state.chrome_drag
    else {
        panic!("scrolled drag target");
    };
    assert!(after > before);
    assert!(frame_rows(&frame)[1].contains('│'));
    let outcome = mouse_at(&mut state, target, MouseEventKind::Up(MouseButton::Left));
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
        if matches!(&request.method, crate::api::schema::Method::TabMove(params) if params.insert_index == after)
    )));
}

#[test]
fn wrapped_tabs_offscreen_edge_drops_show_visible_feedback() {
    let mut state = wrapped_state(30);
    state.compose(80, 20).unwrap();
    state.tab_scroll = 1;
    state.compose(80, 20).unwrap();
    for end in [false, true] {
        let source = state.hits.tabs[0].0;
        let target = if end {
            state.hits.tab_scroll_right
        } else {
            state.hits.tab_scroll_left
        };
        mouse_at(&mut state, source, MouseEventKind::Down(MouseButton::Left));
        mouse_at(&mut state, target, MouseEventKind::Drag(MouseButton::Left));
        let frame = state.compose(80, 20).unwrap();
        let body = state.hits.tab_body;
        let (x, y) = if end {
            (body.right() - 1, body.bottom() - 1)
        } else {
            (body.x, body.y)
        };
        assert_eq!(
            frame.cells[usize::from(y) * usize::from(frame.width) + usize::from(x)].symbol,
            "│"
        );
        let outcome = mouse_at(&mut state, target, MouseEventKind::Up(MouseButton::Left));
        assert!(outcome.actions.iter().any(|action| matches!(action,
            ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabMove(params) if params.insert_index == if end { 30 } else { 0 })
        )));
        state.compose(80, 20).unwrap();
    }
}
