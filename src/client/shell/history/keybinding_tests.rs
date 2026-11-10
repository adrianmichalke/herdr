use super::*;
use crate::input::{KeybindAction, KeybindMatch, TerminalKey};
use crossterm::event::KeyModifiers;

fn config(text: &str) -> Config {
    toml::from_str(text).unwrap()
}

fn command() -> crate::protocol::ClientShellCommand {
    crate::protocol::ClientShellCommand {
        command_id: "cmd_history_collision".into(),
        binding_label: "ctrl+f".into(),
        binding_labels: vec!["ctrl+f".into()],
        action: crate::protocol::ClientShellCommandAction::Shell,
        description: Some("Existing endpoint binding".into()),
    }
}

#[test]
fn remapped_and_disabled_history_bindings_survive_local_snapshot_and_config_reload() {
    for source in [
        ClientShellKeybindingSource::Local,
        ClientShellKeybindingSource::RemoteLocal,
    ] {
        let local = config("[keys]\nhistory_search = \"alt+f\"\n");
        let mut shell = ClientShellState::new(
            ClientShellConfig::from_config(&local).with_keybinding_source(source),
        );
        let mut projection = crate::client::shell::tests::snapshot();
        projection.server_keybindings_toml = Some("[keys]\nhistory_search = \"ctrl+f\"\n".into());
        shell.set_snapshot(Box::new(projection.clone()));
        assert_eq!(
            shell
                .config
                .keybinds
                .keybinds
                .history_search
                .label()
                .as_deref(),
            Some("alt+f")
        );
        let outcome = shell.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ))]);
        assert!(outcome.actions.is_empty());
        assert!(outcome
            .requests
            .iter()
            .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. })));
        let outcome = shell.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
            KeyCode::Char('f'),
            KeyModifiers::ALT,
        ))]);
        assert!(outcome.requests.is_empty());
        assert!(matches!(
            shell.overlay,
            Some(ClientShellOverlay::PiHistory(_))
        ));
        let mut close = ClientShellInput::default();
        shell.close_history(&mut close);
        let disabled = config("[keys]\nhistory_search = []\n");
        assert!(shell
            .config
            .apply_live_config(&disabled, &[], &[])
            .is_empty());
        projection.revision += 1;
        projection.commands = vec![command()];
        shell.set_snapshot(Box::new(projection));
        assert!(shell
            .config
            .keybinds
            .keybinds
            .history_search
            .bindings
            .is_empty());
        let outcome = shell.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ))]);
        assert!(!matches!(
            shell.overlay,
            Some(ClientShellOverlay::PiHistory(_))
        ));
        if source == ClientShellKeybindingSource::Local {
            assert!(outcome
                .actions
                .iter()
                .any(|action| matches!(action, ClientShellAction::Endpoint { .. })));
        } else {
            assert!(outcome.actions.is_empty());
            assert!(!outcome.requests.is_empty());
        }
        let before = shell.config.keybinds.keybinds.history_search.clone();
        shell
            .config
            .apply_live_config(&Config::default(), &[], &["keys".into()]);
        assert_eq!(shell.config.keybinds.keybinds.history_search, before);
    }
}

#[test]
fn old_endpoint_profiles_do_not_let_new_history_default_steal_existing_commands() {
    let mut shell = ClientShellState::new(
        ClientShellConfig::from_config(&Config::default())
            .with_keybinding_source(ClientShellKeybindingSource::Endpoint),
    );
    shell
        .config
        .apply_snapshot_keybindings(Some("[keys]\nprefix = \"ctrl+b\"\n"), &[command()])
        .unwrap();
    assert!(shell
        .config
        .keybinds
        .keybinds
        .history_search
        .bindings
        .is_empty());
    assert!(matches!(
        crate::input::resolve_direct_binding(
            &shell.config.keybinds.keybinds,
            &TerminalKey::new(KeyCode::Char('f'), KeyModifiers::CONTROL)
        ),
        Some(KeybindMatch::Command(_))
    ));
    shell
        .config
        .apply_snapshot_keybindings(Some("[keys]\nprefix = \"ctrl+b\"\n"), &[])
        .unwrap();
    assert_eq!(
        shell
            .config
            .keybinds
            .keybinds
            .history_search
            .label()
            .as_deref(),
        Some("ctrl+f")
    );
    shell
        .config
        .apply_snapshot_keybindings(Some("[keys]\nhistory_search = \"prefix+f\"\n"), &[])
        .unwrap();
    assert_eq!(
        shell
            .config
            .keybinds
            .keybinds
            .history_search
            .label()
            .as_deref(),
        Some("prefix+f")
    );
    assert!(matches!(
        crate::input::resolve_prefix_binding(
            &shell.config.keybinds.keybinds,
            &TerminalKey::new(KeyCode::Char('f'), KeyModifiers::empty())
        ),
        Some(KeybindMatch::Action(KeybindAction::OpenPiHistory))
    ));
    shell
        .config
        .apply_snapshot_keybindings(Some("[keys]\nhistory_search = \"\"\n"), &[])
        .unwrap();
    assert!(shell
        .config
        .keybinds
        .keybinds
        .history_search
        .bindings
        .is_empty());
    shell.config.apply_live_config(&Config::default(), &[], &[]);
    assert!(shell
        .config
        .keybinds
        .keybinds
        .history_search
        .bindings
        .is_empty());
}

#[test]
fn endpoint_command_manifests_keep_existing_bindings_actions_and_labels() {
    let mut config = ClientShellConfig::from_config(&Config::default())
        .with_keybinding_source(ClientShellKeybindingSource::Endpoint);
    let commands: Vec<_> = [
        (
            crate::protocol::ClientShellCommandAction::Shell,
            vec!["prefix+z", "prefix+c"],
        ),
        (
            crate::protocol::ClientShellCommandAction::Pane,
            vec!["ctrl+1", "ctrl+2", "ctrl+3"],
        ),
        (crate::protocol::ClientShellCommandAction::Popup, vec!["x"]),
        (
            crate::protocol::ClientShellCommandAction::PluginAction,
            vec!["ctrl+f", "alt+f"],
        ),
    ]
    .into_iter()
    .enumerate()
    .map(
        |(index, (action, labels))| crate::protocol::ClientShellCommand {
            command_id: format!("cmd_{index}"),
            binding_label: format!("Published label {index}"),
            binding_labels: labels.into_iter().map(str::to_owned).collect(),
            action,
            description: Some(format!("Published description {index}")),
        },
    )
    .collect();
    config
        .apply_snapshot_keybindings(Some("[keys]\nprefix = \"ctrl+b\"\n"), &commands)
        .unwrap();
    let keybinds = &config.keybinds.keybinds;
    assert_eq!(keybinds.custom_commands.len(), commands.len());
    for (actual, expected) in keybinds.custom_commands.iter().zip(&commands) {
        assert_eq!(
            actual.bindings,
            crate::config::ActionKeybinds::from_labels(&expected.binding_labels).unwrap()
        );
        assert_eq!(actual.command, expected.command_id);
        assert_eq!(actual.label, expected.binding_label);
        assert_eq!(actual.description, expected.description);
        assert_eq!(actual.action, expected.action.try_into().unwrap());
    }
    assert!(keybinds.history_search.bindings.is_empty());
    assert_eq!(keybinds.zoom.label().as_deref(), Some("prefix+z"));
    assert_eq!(keybinds.new_tab.label().as_deref(), Some("prefix+c"));
}

#[test]
fn contextual_navigation_bindings_keep_precedence_across_profiles_and_reloads() {
    for source in [
        ClientShellKeybindingSource::Local,
        ClientShellKeybindingSource::RemoteLocal,
        ClientShellKeybindingSource::Endpoint,
    ] {
        let local = config("[keys]\nnavigate_workspace_up = \"ctrl+f\"\n");
        let mut shell = ClientShellState::new(
            ClientShellConfig::from_config(&local).with_keybinding_source(source),
        );
        let mut projection = crate::client::shell::tests::snapshot();
        projection.server_keybindings_toml =
            Some("[keys]\nnavigate_workspace_up = \"ctrl+f\"\n".into());
        shell.set_snapshot(Box::new(projection.clone()));
        shell.config.apply_live_config(&local, &[], &[]);
        projection.revision += 1;
        shell.set_snapshot(Box::new(projection));
        shell.mode = ClientShellMode::Navigate;
        let outcome = shell.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ))]);
        assert!(shell.overlay.is_none());
        assert!(shell.navigate_workspace_id.is_some());
        assert!(!outcome
            .actions
            .iter()
            .any(|action| matches!(action, ClientShellAction::History { .. })));
        shell.mode = ClientShellMode::Terminal;
        let outcome = shell.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ))]);
        assert!(outcome
            .actions
            .iter()
            .any(|action| matches!(action, ClientShellAction::History { .. })));
    }
}

#[test]
fn help_shows_effective_history_shortcut_instead_of_hardcoded_ctrl_f() {
    for (binding, expected) in [("\"alt+f\"", "alt+f"), ("\"\"", "unset")] {
        let config = config(&format!("[keys]\nhistory_search = {binding}\n"));
        let live = config.live_keybinds_with_diagnostics().unwrap().0;
        let groups = crate::input::keybind_help_groups(&live.keybinds, &live.prefix);
        let entry = groups
            .iter()
            .flat_map(|(_, entries)| entries)
            .find(|(_, label)| label.as_ref() == "Pi history search")
            .unwrap();
        assert_eq!(entry.0, expected);
    }
}
