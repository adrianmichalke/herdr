use super::*;
use crate::client::{
    endpoint_commands::EndpointCommands,
    shell_runtime::{
        begin_endpoint_activation, complete_endpoint_activation, finish_client_shell_input,
        install_client_shell_snapshot, rollback_endpoint_activation,
    },
    ClientState,
};
use crate::protocol::{
    ClientMessage, ClientShellPane, ClientShellSnapshot, ClientShellTab, ClientShellWorkspace,
    ClientSurfaceSize, FrameData, PaneSurfaceFrame,
};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

const COLS: u16 = 106;
const ROWS: u16 = 30;
const INITIAL_SIZE: ClientSurfaceSize = ClientSurfaceSize { cols: 80, rows: 29 };
const WRAPPED_SIZE: ClientSurfaceSize = ClientSurfaceSize { cols: 80, rows: 27 };

fn wrapped_snapshot(boot_id: &str, revision: u64, tab_count: usize) -> ClientShellSnapshot {
    let mut snapshot = test_snapshot(boot_id, revision);
    snapshot.focused_workspace_id = Some("ws_1".into());
    snapshot.focused_tab_id = Some("tab_1".into());
    snapshot.focused_pane_id = Some("pane_1".into());
    snapshot.workspaces = vec![ClientShellWorkspace {
        workspace_id: "ws_1".into(),
        active_tab_id: "tab_1".into(),
        new_workspace_cwd: "/repo".into(),
        number: 1,
        label: "client-shell".into(),
        custom_label: false,
        branch: None,
        git_ahead_behind: None,
        tokens: Vec::new(),
        worktree: None,
        focused: true,
        agent_status: crate::api::schema::AgentStatus::Idle,
    }];
    snapshot.tabs = (1..=tab_count)
        .map(|number| ClientShellTab {
            tab_id: format!("tab_{number}"),
            workspace_id: "ws_1".into(),
            number,
            label: number.to_string(),
            custom_label: false,
            zoomed: false,
            focused: number == 1,
            agent_status: crate::api::schema::AgentStatus::Idle,
        })
        .collect();
    snapshot.panes = vec![ClientShellPane {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: None,
        cwd: Some("/repo".into()),
        foreground_cwd: Some("/repo".into()),
        focused: true,
        right_click_passthrough: false,
    }];
    snapshot
}

fn sized_surface(
    boot_id: &str,
    revision: u64,
    size: ClientSurfaceSize,
    text: &str,
) -> PaneSurfaceFrame {
    let mut surface = surface(boot_id, revision, "pane_1");
    let area = Rect::new(0, 0, size.cols, size.rows);
    let mut buffer = Buffer::empty(area);
    buffer.set_string(0, 0, text, Style::default());
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
    surface.panes[0].rect = area.into();
    surface.panes[0].inner_rect = area.into();
    surface
}

#[test]
fn runtime_reconciles_wrapped_geometry_before_target_second_sync_effects_complete() {
    assert_second_sync_reconciles_wrapped_geometry(false);
}

#[test]
fn runtime_reconciles_wrapped_geometry_before_source_rollback_second_sync_effects_complete() {
    assert_second_sync_reconciles_wrapped_geometry(true);
}

fn assert_second_sync_reconciles_wrapped_geometry(restore_source: bool) {
    let mut config = crate::config::Config::default();
    config.ui.tab_bar_wrap = true;
    let (mut shell, mut endpoints, source_sent, target_sent) =
        shell_and_registry_with_config(false, &config);
    let source = ClientEndpointId::Local;
    let target = endpoint();
    shell.set_endpoint_snapshot_for_generation(
        &source,
        1,
        Box::new(wrapped_snapshot("local-boot", 1, 1)),
    );
    shell.cache_endpoint_snapshot_inactive_for_generation(
        &target,
        7,
        Box::new(wrapped_snapshot("remote-boot", 1, 1)),
    );
    assert_eq!(shell.surface_size(COLS, ROWS), INITIAL_SIZE);
    shell.expect_pane_surface_size(INITIAL_SIZE);
    shell.set_pane_surface(sized_surface("local-boot", 1, INITIAL_SIZE, "SOURCE"));

    let mut state = ClientState::test_new();
    state.reported_size = (COLS, ROWS);
    state.shell = Some(shell);
    let mut pending = None;
    let mut commands = EndpointCommands::default();
    let mut serial = 51;
    let mut scheduled = None;
    let mut prefix_input_source = crate::platform::RealPrefixInputSource::default();
    begin_endpoint_activation(
        &mut state,
        &mut endpoints,
        &mut commands,
        &mut pending,
        &mut serial,
        target.clone(),
        None,
        false,
        Instant::now(),
        &mut scheduled,
    )
    .unwrap();
    assert!(state.presentation_frozen);
    assert!(!endpoints.active_surface_available());
    let source_off = "client-shell-surface:51:off";
    assert_eq!(
        pending.as_mut().unwrap().receive_response(
            &source,
            1,
            source_off,
            &surface_success(source_off, false, 1),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );

    let (owner, generation, boot_id, sent, first_request) = if restore_source {
        rollback_endpoint_activation(
            &mut state,
            &mut endpoints,
            &mut pending,
            "cancelled".into(),
            false,
        );
        let target_off = "client-shell-surface:51:rollback-target-off";
        assert_eq!(
            pending.as_mut().unwrap().receive_response(
                &target,
                7,
                target_off,
                &surface_success(target_off, false, 1),
                &mut endpoints,
            ),
            SurfaceActivationProgress::Pending
        );
        (
            source,
            1,
            "local-boot",
            source_sent,
            "client-shell-surface:51:rollback-source-on",
        )
    } else {
        (
            target,
            7,
            "remote-boot",
            target_sent,
            "client-shell-surface:51:on",
        )
    };
    assert_eq!(
        pending.as_mut().unwrap().receive_response(
            &owner,
            generation,
            first_request,
            &surface_success(first_request, true, 2),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    let snapshot = wrapped_snapshot(boot_id, 2, 1);
    assert_eq!(
        pending
            .as_mut()
            .unwrap()
            .receive_snapshot(&owner, generation, &snapshot),
        SurfaceActivationProgress::Pending
    );
    install_client_shell_snapshot(
        &mut state,
        &owner,
        Box::new(snapshot),
        true,
        &mut endpoints,
        &mut prefix_input_source,
    )
    .unwrap();
    assert_eq!(
        pending.as_mut().unwrap().receive_surface(
            &owner,
            generation,
            sized_surface(boot_id, 2, INITIAL_SIZE, "COHERENT"),
        ),
        SurfaceActivationProgress::Ready
    );
    assert!(
        complete_endpoint_activation(&mut state, &mut endpoints, &mut pending, &mut commands)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        &pending.as_ref().unwrap().phase,
        ActivationPhase::SynchronizingPresentation { .. }
    ));
    assert!(!endpoints.active_surface_available());
    let coherent_frame = state.shell.as_mut().unwrap().compose(COLS, ROWS).unwrap();
    assert!(state.blit_encoder.is_current(&coherent_frame.frame));
    let input = state.shell.as_mut().unwrap().handle_input_bytes(b"x");
    assert!(matches!(
        input.requests.as_slice(),
        [ClientMessage::ClientShellPaneInput { .. }]
    ));
    commands.enqueue(
        owner.clone(),
        generation,
        boot_id.into(),
        Box::new(crate::api::schema::Request {
            id: "after-effects".into(),
            method: crate::api::schema::Method::WorkspaceFocus(
                crate::api::schema::WorkspaceTarget {
                    workspace_id: "ws_1".into(),
                },
            ),
        }),
    );

    let sync_request = "client-shell-surface:51:presentation-sync";
    assert_eq!(
        pending.as_mut().unwrap().receive_response(
            &owner,
            generation,
            sync_request,
            &surface_success(sync_request, true, 3),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    let snapshot = wrapped_snapshot(boot_id, 3, 20);
    assert_eq!(
        pending
            .as_mut()
            .unwrap()
            .receive_snapshot(&owner, generation, &snapshot),
        SurfaceActivationProgress::Pending
    );
    install_client_shell_snapshot(
        &mut state,
        &owner,
        Box::new(snapshot),
        true,
        &mut endpoints,
        &mut prefix_input_source,
    )
    .unwrap();
    assert_eq!(
        state.shell.as_ref().unwrap().surface_size(COLS, ROWS),
        INITIAL_SIZE
    );
    assert_eq!(
        sent.lock()
            .unwrap()
            .iter()
            .filter_map(resize_geometry)
            .collect::<Vec<_>>(),
        vec![INITIAL_SIZE]
    );
    assert_eq!(
        pending.as_mut().unwrap().receive_surface(
            &owner,
            generation,
            sized_surface(boot_id, 3, INITIAL_SIZE, "WRONG-SIZED"),
        ),
        SurfaceActivationProgress::Ready
    );
    let before_completion = sent.lock().unwrap().len();
    assert!(
        complete_endpoint_activation(&mut state, &mut endpoints, &mut pending, &mut commands)
            .unwrap()
            .is_none()
    );
    assert_eq!(pending.as_ref().unwrap().geometry(), WRAPPED_SIZE);
    assert_eq!(
        state.shell.as_ref().unwrap().surface_size(COLS, ROWS),
        WRAPPED_SIZE
    );
    assert!(!state.shell.as_ref().unwrap().has_presented_surface());
    assert!(!endpoints.active_surface_available());
    assert!(state.blit_encoder.is_current(&coherent_frame.frame));
    match &pending.as_ref().unwrap().phase {
        ActivationPhase::SynchronizingPresentation {
            lease,
            acknowledged_revision,
            evidence,
            completion,
            ..
        } => {
            assert_eq!(lease.endpoint_id, owner);
            assert!(acknowledged_revision.is_none());
            assert!(evidence.surface.is_none());
            assert_eq!(
                matches!(
                    completion.as_ref(),
                    ActivationCompletion::RestoredSource { .. }
                ),
                restore_source
            );
        }
        phase => panic!("geometry change must restart presentation sync: {phase:?}"),
    }
    let old_token = {
        let sent = sent.lock().unwrap();
        let emitted = &sent[before_completion..];
        assert_eq!(emitted.len(), 3);
        assert_eq!(resize_geometry(&emitted[1]), Some(WRAPPED_SIZE));
        assert_eq!(
            emitted[2],
            surface_interest_request(boot_id, sync_request.into(), true).unwrap()
        );
        match &emitted[0] {
            ClientMessage::EndpointControl { kind, data } => {
                assert_eq!(
                    kind,
                    crate::protocol::endpoint::PRESENTATION_EFFECTS_SYNC_KIND
                );
                data.clone()
            }
            message => panic!("expected the superseded effects fence, got {message:?}"),
        }
    };
    assert_eq!(
        pending
            .as_mut()
            .unwrap()
            .receive_presentation_effects_ready(&owner, generation, &old_token),
        SurfaceActivationProgress::Stale
    );
    let before_input = sent.lock().unwrap().len();
    assert!(!finish_client_shell_input(
        &mut state,
        input,
        None,
        &mut endpoints,
        &mut pending,
        &mut commands,
        &mut prefix_input_source,
        &mut scheduled,
    )
    .unwrap());
    assert_eq!(sent.lock().unwrap().len(), before_input);
    assert!(!commands.request_may_have_been_sent(&owner, generation, boot_id, "after-effects"));

    assert_eq!(
        pending.as_mut().unwrap().receive_response(
            &owner,
            generation,
            sync_request,
            &surface_success(sync_request, true, 4),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    let snapshot = wrapped_snapshot(boot_id, 4, 20);
    assert_eq!(
        pending
            .as_mut()
            .unwrap()
            .receive_snapshot(&owner, generation, &snapshot),
        SurfaceActivationProgress::Pending
    );
    install_client_shell_snapshot(
        &mut state,
        &owner,
        Box::new(snapshot),
        true,
        &mut endpoints,
        &mut prefix_input_source,
    )
    .unwrap();
    let waiting_frame = state.shell.as_mut().unwrap().compose(COLS, ROWS).unwrap();
    assert!(state.blit_encoder.is_current(&waiting_frame.frame));
    assert_eq!(
        pending.as_mut().unwrap().receive_surface(
            &owner,
            generation,
            sized_surface(boot_id, 4, INITIAL_SIZE, "LATE-WRONG-SIZED"),
        ),
        SurfaceActivationProgress::Pending
    );
    assert!(!state.shell.as_ref().unwrap().has_presented_surface());
    assert!(!endpoints.active_surface_available());
    assert_eq!(
        pending.as_mut().unwrap().receive_surface(
            &owner,
            generation,
            sized_surface(boot_id, 4, WRAPPED_SIZE, "RESIZED"),
        ),
        SurfaceActivationProgress::Ready
    );
    assert!(
        complete_endpoint_activation(&mut state, &mut endpoints, &mut pending, &mut commands)
            .unwrap()
            .is_none()
    );
    let token = match &pending.as_ref().unwrap().phase {
        ActivationPhase::AwaitingPresentationEffects { token, ready, .. } => {
            assert!(!ready);
            token.clone()
        }
        phase => panic!("matching geometry must still wait for effects: {phase:?}"),
    };
    assert!(state.shell.as_ref().unwrap().has_presented_surface());
    assert!(!endpoints.active_surface_available());
    assert!(state.blit_encoder.is_current(&waiting_frame.frame));
    assert!(!commands.request_may_have_been_sent(&owner, generation, boot_id, "after-effects"));
    assert_eq!(
        pending
            .as_mut()
            .unwrap()
            .receive_presentation_effects_ready(&owner, generation, &token),
        SurfaceActivationProgress::Ready
    );
    assert!(
        complete_endpoint_activation(&mut state, &mut endpoints, &mut pending, &mut commands)
            .unwrap()
            .is_none()
    );
    assert!(pending.is_none());
    assert!(endpoints.active_surface_available());
    assert_eq!(endpoints.active_id(), &owner);
    assert!(!state.presentation_frozen);
    assert_eq!(
        state.shell.as_ref().unwrap().surface_size(COLS, ROWS),
        WRAPPED_SIZE
    );
    assert!(commands.request_may_have_been_sent(&owner, generation, boot_id, "after-effects"));
    let final_frame = state.shell.as_mut().unwrap().compose(COLS, ROWS).unwrap();
    assert!(state.blit_encoder.is_current(&final_frame.frame));
    if !restore_source {
        let text: String = final_frame
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect();
        assert!(text.contains("RESIZED"));
        assert!(!text.contains("WRONG-SIZED"));
    }
}
