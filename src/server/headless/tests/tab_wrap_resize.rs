use super::*;
use crate::terminal_theme::{HostAppearance, RgbColor};

fn with_resize_clients(second_focus: Option<bool>, test: impl FnOnce(&mut HeadlessServer)) {
    with_terminal_session_test_server(|server, _, _, _| {
        let second_tab = server.app.state.workspaces[0].test_add_tab(Some("background"));
        server.app.state.ensure_test_terminals();
        server.app.state.mode = crate::app::Mode::Terminal;
        let second_tab_id = server.app.public_tab_id(0, second_tab).unwrap();
        let (_first_control, _first_render) = connect_test_shell(server, 1, 100, 30);
        let (_second_control, _second_render) = connect_test_shell(server, 2, 70, 20);
        assert!(server.focus_shell_client_on_tab(2, &second_tab_id));
        assert!(server.claim_shell_tab_geometry(2, false));
        assert_ne!(
            server.shell_tab_id_for_client(1),
            server.shell_tab_id_for_client(2)
        );

        let first = server.clients.get_mut(&1).unwrap();
        first.outer_terminal_focus = Some(true);
        first.host_terminal_theme.background = Some(RgbColor {
            r: 17,
            g: 18,
            b: 19,
        });
        first.host_terminal_appearance = Some(HostAppearance::Dark);
        first.host_terminal_appearance_explicit = true;
        let second = server.clients.get_mut(&2).unwrap();
        second.outer_terminal_focus = second_focus;
        second.host_terminal_theme.background = Some(RgbColor {
            r: 240,
            g: 241,
            b: 242,
        });
        second.host_terminal_appearance = Some(HostAppearance::Light);
        second.host_terminal_appearance_explicit = false;
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        test(server);
    });
}

fn resize_second_client(server: &mut HeadlessServer) {
    assert!(server.handle_server_event(ServerEvent::ClientShellResize {
        client_id: 2,
        surface_cols: 70,
        surface_rows: 18,
        cell_width_px: 9,
        cell_height_px: 18,
        pixel_mouse: true,
    }));
}

#[test]
fn background_wrapped_tab_resize_updates_its_viewport_without_stealing_foreground() {
    with_resize_clients(Some(false), |server| {
        let first_location = server.clients[&1].shell_location.clone();
        let second_location = server.clients[&2].shell_location.clone();
        let first_theme = server.app.state.host_terminal_theme;
        let second_activity = server.clients[&2].last_activity;
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));

        resize_second_client(server);

        assert_eq!(server.clients[&2].terminal_size, (70, 18));
        assert_eq!(
            server.clients[&2].cell_size,
            crate::kitty_graphics::HostCellSize {
                width_px: 9,
                height_px: 18,
            }
        );
        assert!(server.clients[&2].pixel_mouse);
        assert_eq!(server.clients[&2].outer_terminal_focus, Some(false));
        assert_eq!(server.clients[&2].last_activity, second_activity);
        assert_eq!(server.clients[&1].terminal_size, (100, 30));
        assert_eq!(server.clients[&1].shell_location, first_location);
        assert_eq!(server.clients[&2].shell_location, second_location);
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(server.effective_size, (100, 30));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
        assert_eq!(server.app.state.host_terminal_theme, first_theme);
        assert_eq!(
            server.app.state.host_terminal_appearance,
            Some(HostAppearance::Dark)
        );
        assert!(server.app.state.host_terminal_appearance_explicit);
    });
}

#[test]
fn wrapped_tab_resize_preserves_focused_and_unknown_focus_promotion() {
    for focus in [Some(true), None] {
        with_resize_clients(focus, |server| {
            resize_second_client(server);

            assert_eq!(server.clients[&2].terminal_size, (70, 18));
            assert_eq!(server.foreground_client_id, Some(2), "focus: {focus:?}");
            assert_eq!(server.effective_size, (70, 18));
            assert_eq!(server.app.state.outer_terminal_focus, focus);
            assert_eq!(
                server.app.state.host_terminal_theme,
                server.clients[&2].host_terminal_theme
            );
            assert_eq!(
                server.app.state.host_terminal_appearance,
                Some(HostAppearance::Light)
            );
            assert!(!server.app.state.host_terminal_appearance_explicit);
        });
    }
}
