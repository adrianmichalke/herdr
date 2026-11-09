use super::*;

const COLS: u16 = 106;
const ROWS: u16 = 30;

fn wrapped_snapshot(revision: u64, tab_count: usize) -> ClientShellSnapshot {
    let mut projected = snapshot();
    projected.revision = revision;
    let template = projected.tabs[0].clone();
    projected.tabs = (1..=tab_count)
        .map(|number| ClientShellTab {
            tab_id: format!("tab_{number}"),
            number,
            label: number.to_string(),
            focused: number == 1,
            ..template.clone()
        })
        .collect();
    projected
}

fn sized_surface(
    size: ClientSurfaceSize,
    projection_revision: u64,
    surface_revision: u64,
    text: &str,
) -> PaneSurfaceFrame {
    let mut pane_surface = surface();
    let area = Rect::new(0, 0, size.cols, size.rows);
    let mut buffer = Buffer::empty(area);
    buffer.set_string(0, 0, text, Style::default());
    pane_surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
    pane_surface.projection_revision = projection_revision;
    pane_surface.surface_revision = surface_revision;
    let pane = &mut pane_surface.panes[0];
    pane.rect = area.into();
    pane.inner_rect = pane.rect;
    pane.mouse_reporting = true;
    pane_surface
}

fn surface_patch(surface: &PaneSurfaceFrame) -> crate::protocol::PaneSurfacePatch {
    let mut pane = surface.panes[0].clone();
    pane.content_revision += 1;
    let mut cell = surface.frame.cells[0].clone();
    cell.symbol = "P".into();
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

fn assert_presented(state: &mut ClientShellState) {
    let frame = state.compose(COLS, ROWS).expect("composed pane surface");
    let area = state.layout(COLS, ROWS).pane_surface;
    assert_eq!(state.last_composed_pane_surface, Some(area));
    assert_eq!(state.hits.panes.len(), 1);
    assert_eq!(state.hits.panes[0].inner_rect, area);
    let surface = state.pane_surface.as_ref().expect("installed pane surface");
    assert_eq!(
        (surface.frame.width, surface.frame.height),
        (area.width, area.height)
    );
    let first_cell = usize::from(area.y) * usize::from(COLS) + usize::from(area.x);
    assert_eq!(frame.cells[first_cell], surface.frame.cells[0]);
}

fn wrapped_shell(tab_count: usize) -> ClientShellState {
    let mut config = Config::default();
    config.ui.tab_bar_wrap = true;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(wrapped_snapshot(1, tab_count)));
    let size = state.surface_size(COLS, ROWS);
    state.expect_pane_surface_size(size);
    state.set_pane_surface(sized_surface(size, 1, 1, "CURRENT"));
    assert_presented(&mut state);
    state
}

#[test]
fn wrapped_snapshot_waits_for_the_expected_resized_surface() {
    let mut state = wrapped_shell(1);
    let mut old_surface = state.pane_surface.as_ref().unwrap().clone();
    assert_eq!(
        (old_surface.frame.width, old_surface.frame.height),
        (80, 29)
    );

    state.set_snapshot(Box::new(wrapped_snapshot(2, 20)));
    let size = state.surface_size(COLS, ROWS);
    assert_eq!(size, ClientSurfaceSize { cols: 80, rows: 27 });
    state.expect_pane_surface_size(size);
    assert_eq!(state.expected_pane_surface_size, Some(size));
    assert!(state.pane_surface.is_none());
    assert!(state.last_composed_pane_surface.is_none());
    assert!(state.hits.panes.is_empty());
    assert!(state.hits.tabs.is_empty());

    old_surface.projection_revision = 2;
    old_surface.surface_revision = 50;
    state.set_pane_surface(old_surface.clone());
    state.compose(COLS, ROWS).expect("waiting shell chrome");
    assert!(!state.has_presented_surface());
    assert!(state.hits.panes.is_empty());
    assert!(state.last_composed_pane_surface.is_none());
    assert!(matches!(
        state.apply_pane_surface_patch(surface_patch(&old_surface)),
        ClientPaneSurfacePatchOutcome::Rejected
    ));
    let area = state.layout(COLS, ROWS).pane_surface;
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 1,
        row: area.y + 1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(click.requests.is_empty());
    assert!(click.actions.is_empty());

    let resized = sized_surface(size, 2, 2, "RESIZED");
    state.set_pane_surface(resized.clone());
    assert_eq!(state.pane_surface.as_ref(), Some(&resized));
    assert_presented(&mut state);
}

#[test]
fn future_wrong_sized_surface_cannot_become_authoritative_with_its_snapshot() {
    for revision in [2, 4] {
        let mut state = wrapped_shell(20);
        let current = state.pane_surface.as_ref().unwrap().clone();
        let size = state.surface_size(COLS, ROWS);
        let wrong_size = ClientSurfaceSize {
            rows: size.rows + 2,
            ..size
        };
        let future = sized_surface(wrong_size, revision, 50, "OLD SIZE");
        state.set_pane_surface(future.clone());
        assert_eq!(state.pane_surface.as_ref(), Some(&current));

        state.set_snapshot(Box::new(wrapped_snapshot(revision, 20)));
        assert_eq!(state.pane_surface.as_ref(), Some(&current));
        assert!(state.hits.panes.is_empty());
        assert!(state.compose(COLS, ROWS).is_none());
        assert!(matches!(
            state.apply_pane_surface_patch(surface_patch(&future)),
            ClientPaneSurfacePatchOutcome::Rejected
        ));

        let matching = sized_surface(size, revision, 2, "MATCHING");
        state.set_pane_surface(matching.clone());
        assert_eq!(state.pane_surface.as_ref(), Some(&matching));
        assert_presented(&mut state);
    }
}

#[test]
fn late_wrong_sized_high_revision_cannot_replace_the_matching_frame_or_patch_base() {
    let mut state = wrapped_shell(20);
    let current = state.pane_surface.as_ref().unwrap().clone();
    let size = state.surface_size(COLS, ROWS);
    for wrong_size in [
        ClientSurfaceSize {
            cols: size.cols + 1,
            ..size
        },
        ClientSurfaceSize {
            rows: size.rows + 2,
            ..size
        },
    ] {
        let late = sized_surface(wrong_size, 1, 100, "LATE");
        state.set_pane_surface(late.clone());
        assert_eq!(state.pane_surface.as_ref(), Some(&current));
        assert_eq!(
            state.hits.panes[0].inner_rect,
            state.layout(COLS, ROWS).pane_surface
        );
        assert!(matches!(
            state.apply_pane_surface_patch(surface_patch(&late)),
            ClientPaneSurfacePatchOutcome::Rejected
        ));
    }
    assert!(matches!(
        state.apply_pane_surface_patch(surface_patch(&current)),
        ClientPaneSurfacePatchOutcome::Applied(Some(_))
    ));
}

#[test]
fn invalidation_retains_the_size_fence_but_endpoint_projection_reset_clears_it() {
    let mut state = wrapped_shell(20);
    let size = state.surface_size(COLS, ROWS);
    state.invalidate_pane_surface();
    assert_eq!(state.expected_pane_surface_size, Some(size));
    assert!(state.last_composed_pane_surface.is_none());
    assert!(state.hits.panes.is_empty());

    let other_size = ClientSurfaceSize {
        rows: size.rows + 2,
        ..size
    };
    let delayed = sized_surface(other_size, 1, 50, "OTHER ENDPOINT");
    state.set_pane_surface(delayed.clone());
    assert!(!state.has_presented_surface());
    assert!(state.hits.panes.is_empty());
    assert!(matches!(
        state.apply_pane_surface_patch(surface_patch(&delayed)),
        ClientPaneSurfacePatchOutcome::Rejected
    ));

    let target = ClientEndpointId::Local.with_session("other").unwrap();
    assert!(state.stage_session_endpoint(&ClientEndpointId::Local, target.clone(), "other".into()));
    state.set_endpoint_status(&target, ClientEndpointStatus::Online);
    state.cache_endpoint_snapshot_inactive_for_generation(
        &target,
        2,
        Box::new(wrapped_snapshot(1, 1)),
    );
    assert!(state.activate_endpoint_projection(&target));
    assert!(state.expected_pane_surface_size.is_none());
    state.set_pane_surface(delayed.clone());
    assert_eq!(state.pane_surface.as_ref(), Some(&delayed));
    assert_presented(&mut state);
}
