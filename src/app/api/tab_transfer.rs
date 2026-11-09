use std::collections::HashSet;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, TabMoveToWorkspaceParams,
};
use crate::app::App;
use crate::workspace::{public_pane_id_for_number, Workspace};

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_tab_move_to_workspace(
        &mut self,
        id: String,
        params: TabMoveToWorkspaceParams,
    ) -> String {
        let Some((source_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return encode_error(id, "tab_not_found", "source tab not found");
        };
        let Some(destination_idx) = self.parse_workspace_id(&params.workspace_id) else {
            return encode_error(id, "workspace_not_found", "destination workspace not found");
        };
        if source_idx == destination_idx {
            return encode_error(id, "tab_move_failed", "tab is already in this workspace");
        }
        for index in [source_idx, destination_idx] {
            if self.state.workspace_transfer_fenced(index) {
                return encode_error(id, "workspace_busy", "workspace transfer is in progress");
            }
            if self.tab_transfer_workspace_is_busy(index) {
                return encode_error(
                    id,
                    "workspace_busy",
                    "workspace has a transient operation in progress",
                );
            }
        }
        let source = &self.state.workspaces[source_idx];
        let destination = &self.state.workspaces[destination_idx];
        if source.id == destination.id
            || source.active_tab >= source.tabs.len()
            || destination.active_tab >= destination.tabs.len()
        {
            return encode_error(id, "tab_move_failed", "workspace has no valid active tab");
        }
        let tab = &source.tabs[tab_idx];
        let pane_ids = tab.layout.pane_ids();
        let pane_set: HashSet<_> = pane_ids.iter().copied().collect();
        let public_numbers: HashSet<_> = source.public_pane_numbers.values().copied().collect();
        if public_numbers.len() != source.public_pane_numbers.len()
            || tab.number == 0
            || source
                .tabs
                .iter()
                .filter(|other| other.number == tab.number)
                .count()
                != 1
            || pane_ids.is_empty()
            || pane_set.len() != pane_ids.len()
            || pane_ids.len() != tab.panes.len()
            || !pane_set.contains(&tab.root_pane)
            || !pane_set.contains(&tab.layout.focused())
            || tab
                .layout
                .previous_focus()
                .is_some_and(|pane| !pane_set.contains(&pane))
            || pane_ids.iter().any(|pane| {
                !tab.panes.contains_key(pane)
                    || source
                        .public_pane_number(*pane)
                        .is_none_or(|number| number == 0)
                    || destination.public_pane_numbers.contains_key(pane)
                    || tab
                        .terminal_id(*pane)
                        .is_none_or(|terminal| !self.state.terminals.contains_key(terminal))
            })
        {
            return encode_error(
                id,
                "tab_move_failed",
                "source tab has invalid pane identities",
            );
        }
        let old_pane_ids: Vec<_> = pane_ids
            .iter()
            .map(|pane| {
                (
                    public_pane_id_for_number(&source.id, source.public_pane_numbers[pane]),
                    *pane,
                )
            })
            .collect();
        if old_pane_ids.iter().any(|(public_id, pane)| {
            self.state
                .public_pane_id_aliases
                .get(public_id)
                .is_some_and(|alias| alias != pane)
        }) {
            return encode_error(
                id,
                "tab_move_failed",
                "source pane alias collides with another pane",
            );
        }
        if !self.tab_transfer_numbers_are_available(destination, pane_ids.len()) {
            return encode_error(
                id,
                "tab_move_failed",
                "destination public identities are unavailable",
            );
        }
        let needs_replacement = source.tabs.len() == 1;
        if needs_replacement && !self.tab_transfer_numbers_are_available(source, 1) {
            return encode_error(
                id,
                "tab_move_failed",
                "replacement public identities are unavailable",
            );
        }
        let previous_workspace_id = source.id.clone();
        let destination_workspace_id = destination.id.clone();
        let previous_tab_id = crate::workspace::public_tab_id_for_number(&source.id, tab.number);

        if needs_replacement {
            let cwd = self.resolve_new_terminal_cwd(self.focused_pane_cwd_in_workspace(source_idx));
            let (rows, cols) = self.state.new_pane_size(crate::ui::NewPanePlacement::Alone);
            let appearance = self.pane_host_terminal_appearance();
            let default_shell = self.state.default_shell.clone();
            let result = self.state.workspaces[source_idx].create_tab(
                rows,
                cols,
                cwd,
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
                appearance,
                crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                Vec::new(),
            );
            let (replacement_idx, terminal, runtime) = match result {
                Ok(created) => created,
                Err(error) => return encode_error(id, "tab_create_failed", error.to_string()),
            };
            self.terminal_runtimes.insert(terminal.id.clone(), runtime);
            self.state.terminals.insert(terminal.id.clone(), terminal);
            self.state.remove_alias_shadowed_by_new_pane(
                self.state.workspaces[source_idx].tabs[replacement_idx].root_pane,
            );
        }

        let destination_tab_idx = if source_idx < destination_idx {
            let (before, after) = self.state.workspaces.split_at_mut(destination_idx);
            before[source_idx].move_tab_to_workspace(tab_idx, &mut after[0])
        } else {
            let (before, after) = self.state.workspaces.split_at_mut(source_idx);
            after[0].move_tab_to_workspace(tab_idx, &mut before[destination_idx])
        };
        let Some(destination_tab_idx) = destination_tab_idx else {
            return encode_error(id, "tab_move_failed", "tab could not be moved");
        };
        for (public_id, pane) in &old_pane_ids {
            self.state
                .public_pane_id_aliases
                .insert(public_id.clone(), *pane);
        }
        if let Some(previous) = &mut self.state.previous_pane_focus {
            if pane_set.contains(&previous.pane_id) {
                previous.workspace_id = destination_workspace_id.clone();
            }
        }
        if let Some((workspace, pane)) = &mut self.last_focus {
            if pane_set.contains(pane) {
                *workspace = destination_idx;
            }
        }
        if let Some(target) = self
            .state
            .toast
            .as_mut()
            .and_then(|toast| toast.target.as_mut())
        {
            if pane_set.contains(&target.pane_id) {
                target.workspace_id = destination_workspace_id.clone();
            }
        }
        for notification in self.state.pending_agent_notifications.values_mut() {
            if pane_set.contains(&notification.pane_id) {
                notification.workspace_id = destination_workspace_id.clone();
            }
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabClosed,
            data: EventData::TabClosed {
                tab_id: previous_tab_id.clone(),
                workspace_id: previous_workspace_id.clone(),
            },
        });
        if needs_replacement {
            self.emit_tab_created_events(source_idx, 0);
        }
        let Some(tab) = self.tab_info(destination_idx, destination_tab_idx) else {
            return encode_error(id, "tab_move_failed", "moved tab is unavailable");
        };
        self.emit_event(EventEnvelope {
            event: EventKind::TabCreated,
            data: EventData::TabCreated { tab: tab.clone() },
        });
        for (previous_pane_id, pane_id) in old_pane_ids {
            if let Some(pane) = self.pane_info(destination_idx, pane_id) {
                self.emit_event(EventEnvelope {
                    event: EventKind::PaneMoved,
                    data: EventData::PaneMoved {
                        previous_pane_id,
                        previous_workspace_id: previous_workspace_id.clone(),
                        previous_tab_id: previous_tab_id.clone(),
                        pane: Box::new(pane),
                        created_workspace: None,
                        created_tab: Some(tab.clone()),
                        closed_workspace_id: None,
                        closed_tab_id: Some(previous_tab_id.clone()),
                    },
                });
            }
        }
        self.emit_layout_updated_event(destination_idx, destination_tab_idx);
        encode_success(id, ResponseResult::TabInfo { tab })
    }

    fn tab_transfer_numbers_are_available(&self, workspace: &Workspace, pane_count: usize) -> bool {
        workspace
            .tab_transfer_numbers(pane_count)
            .is_some_and(|(_, mut numbers)| {
                numbers.all(|number| {
                    !self
                        .state
                        .public_pane_id_aliases
                        .contains_key(&public_pane_id_for_number(&workspace.id, number))
                })
            })
    }

    fn tab_transfer_workspace_is_busy(&self, index: usize) -> bool {
        let workspace = &self.state.workspaces[index];
        self.state.popup_pane.is_some()
            || self
                .overlay_panes
                .values()
                .any(|overlay| overlay.ws_idx == index)
            || self
                .pending_api_worktree_removes
                .contains_key(&workspace.id)
            || self
                .pending_restored_worktree_spaces
                .iter()
                .any(|(id, _)| id == &workspace.id)
            || workspace.worktree_space.as_ref().is_some_and(|space| {
                let checkout = crate::worktree::canonical_or_original(&space.checkout_path);
                self.pending_api_worktree_creates.contains_key(&checkout)
                    || self
                        .pending_api_worktree_remove_paths
                        .contains_key(&checkout)
            })
            || workspace
                .tabs
                .iter()
                .flat_map(|tab| tab.panes.keys())
                .any(|pane| {
                    self.pending_worktree_remove_runtime_exits
                        .contains_key(pane)
                        || self
                            .pending_worktree_remove_runtime_restores
                            .contains_key(pane)
                })
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{
        ErrorResponse, Method, Request, ResponseResult, SuccessResponse, TabInfo,
        TabMoveToWorkspaceParams,
    };
    use crate::app::state::{
        PaneFocusTarget, PendingAgentNotification, ToastNotification, ToastTarget,
    };
    use crate::app::{App, AppState, Mode, ToastKind};
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::layout::Node;
    use crate::workspace::Workspace;

    fn app() -> App {
        let (_, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state = AppState::test_with_adversarial_identity_state();
        let mut destination = Workspace::test_adversarial_identity_state();
        destination.set_custom_name("destination".into());
        app.state.workspaces.push(destination);
        app.state.ensure_test_terminals();
        app.state.mode = Mode::Terminal;
        app.state.assert_invariants_for_test();
        app
    }

    fn transfer(app: &mut App, tab_id: String, workspace_id: String) -> String {
        app.handle_api_request_after_internal_events_drained(Request {
            id: "move-tab".into(),
            method: Method::TabMoveToWorkspace(TabMoveToWorkspaceParams {
                tab_id,
                workspace_id,
            }),
        })
    }

    fn moved_tab(response: &str) -> TabInfo {
        let response: SuccessResponse = serde_json::from_str(response).expect(response);
        let ResponseResult::TabInfo { tab } = response.result else {
            panic!("expected tab info");
        };
        tab
    }

    fn tree(node: &Node) -> serde_json::Value {
        match node {
            Node::Pane(pane) => serde_json::json!(pane.raw()),
            Node::Split {
                direction,
                ratio,
                first,
                second,
            } => serde_json::json!({
                "direction": format!("{direction:?}"),
                "ratio": ratio,
                "first": tree(first),
                "second": tree(second),
            }),
        }
    }

    #[test]
    fn whole_tab_transfer_preserves_adversarial_layout_focus_zoom_and_terminal_state() {
        let mut app = app();
        let source_idx = 2;
        let pane_ids = app.state.workspaces[0].tabs[source_idx].layout.pane_ids();
        assert!(pane_ids.len() > 1);
        {
            let tab = &mut app.state.workspaces[0].tabs[source_idx];
            tab.set_custom_name("live tab".into());
            tab.zoomed = true;
            tab.layout.set_ratio_at(&[], 0.31);
            tab.layout.focus_pane(pane_ids[0]);
            tab.layout.focus_pane(pane_ids[1]);
            tab.panes.get_mut(&pane_ids[0]).unwrap().seen = false;
            tab.panes
                .get_mut(&pane_ids[0])
                .unwrap()
                .right_click_passthrough = true;
        }
        let source = &app.state.workspaces[0].tabs[source_idx];
        let root = source.root_pane;
        let original_tree = tree(source.layout.root());
        let focus = source.layout.focused();
        let previous_focus = source.layout.previous_focus();
        let render_notify = source.render_notify.clone();
        let render_dirty = source.render_dirty.clone();
        let terminal_ids: Vec<_> = pane_ids
            .iter()
            .map(|pane| source.terminal_id(*pane).unwrap().clone())
            .collect();
        for terminal_id in &terminal_ids {
            let terminal = app.state.terminals.get_mut(terminal_id).unwrap();
            terminal.manual_label = Some("retained label".into());
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        }
        let terminal_addresses: Vec<_> = terminal_ids
            .iter()
            .map(|id| &app.state.terminals[id] as *const _)
            .collect();
        let source_active = app.state.workspaces[0].active_tab().unwrap().root_pane;
        let destination_active = app.state.workspaces[1].active_tab().unwrap().root_pane;
        let old_ids: Vec<_> = pane_ids
            .iter()
            .map(|pane| app.public_pane_id(0, *pane).unwrap())
            .collect();
        let destination_len = app.state.workspaces[1].tabs.len();
        let source_tab_id = app.public_tab_id(0, source_idx).unwrap();
        let destination = app.public_workspace_id(1);
        let response = transfer(&mut app, source_tab_id.clone(), destination.clone());
        let result = moved_tab(&response);
        assert_eq!(result.workspace_id, destination);
        assert_ne!(result.tab_id, source_tab_id);
        assert!(!result.focused);
        assert_eq!(app.parse_tab_id(&result.tab_id), Some((1, destination_len)));
        assert!(app.parse_tab_id(&source_tab_id).is_none());
        let tab = &app.state.workspaces[1].tabs[destination_len];
        assert_eq!(tab.root_pane, root);
        assert_eq!(tree(tab.layout.root()), original_tree);
        assert_eq!(tab.layout.pane_ids(), pane_ids);
        assert_eq!(tab.layout.focused(), focus);
        assert_eq!(tab.layout.previous_focus(), previous_focus);
        assert!(tab.zoomed);
        assert_eq!(tab.custom_name.as_deref(), Some("live tab"));
        assert!(std::sync::Arc::ptr_eq(&tab.render_notify, &render_notify));
        assert!(std::sync::Arc::ptr_eq(&tab.render_dirty, &render_dirty));
        assert!(!tab.panes[&pane_ids[0]].seen);
        assert!(tab.panes[&pane_ids[0]].right_click_passthrough);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.selected, 0);
        assert_eq!(
            app.state.workspaces[0].active_tab().unwrap().root_pane,
            source_active
        );
        assert_eq!(
            app.state.workspaces[1].active_tab().unwrap().root_pane,
            destination_active
        );
        for ((pane, old_id), (terminal_id, address)) in pane_ids
            .iter()
            .zip(old_ids)
            .zip(terminal_ids.iter().zip(terminal_addresses))
        {
            assert_eq!(tab.terminal_id(*pane), Some(terminal_id));
            assert_eq!(&app.state.terminals[terminal_id] as *const _, address);
            assert_eq!(
                app.state.terminals[terminal_id].manual_label.as_deref(),
                Some("retained label")
            );
            assert_eq!(app.parse_pane_id(&old_id), Some((1, *pane)));
        }
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn tab_transfer_retains_space_metadata_and_terminal_hook_sequences_and_ttls() {
        let mut app = app();
        let source_id = app.public_workspace_id(0);
        let destination_id = app.public_workspace_id(1);
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "source-repo".into(),
            label: "source-group".into(),
            repo_root: "/herdr-test/source".into(),
            checkout_path: "/herdr-test/source-branch".into(),
            is_linked_worktree: true,
        });
        app.state.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([("summary".into(), Some("space token".into()))]),
            None,
            std::time::Instant::now(),
        );
        app.state.workspaces[0]
            .metadata_token_sequences
            .insert("custom:space".into(), 23);
        let membership = app.state.workspaces[0].worktree_space.clone();
        let space_tokens = app.state.workspaces[0].metadata_tokens.clone();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane)
            .unwrap()
            .clone();
        for value in [
            serde_json::json!({
                "id":"hook", "method":"pane.report_agent", "params":{
                    "pane_id":pane_id, "source":"custom:transfer", "agent":"pi", "state":"working", "seq":17
                }
            }),
            serde_json::json!({
                "id":"metadata", "method":"pane.report_metadata", "params":{
                    "pane_id":pane_id, "source":"custom:transfer", "agent":"pi", "title":"retained metadata",
                    "tokens":{"summary":"retained token"}, "seq":19, "ttl_ms":60000
                }
            }),
        ] {
            let response = app.handle_api_request_after_internal_events_drained(
                serde_json::from_value(value).unwrap(),
            );
            let _: SuccessResponse = serde_json::from_str(&response).expect(&response);
        }
        let before = &app.state.terminals[&terminal_id];
        let authority = before.hook_authority.clone();
        let metadata = before.agent_metadata.clone();
        let tokens = before.metadata_tokens.clone();
        assert!(before.metadata_tokens.next_expiry().is_some());
        let source_tab = app.public_tab_id(0, 0).unwrap();
        moved_tab(&transfer(&mut app, source_tab, destination_id));
        assert_eq!(app.public_workspace_id(0), source_id);
        assert_eq!(app.state.workspaces[0].worktree_space, membership);
        assert_eq!(app.state.workspaces[0].metadata_tokens, space_tokens);
        assert_eq!(
            app.state.workspaces[0].metadata_token_sequences["custom:space"],
            23
        );
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.hook_authority, authority);
        assert_eq!(terminal.agent_metadata, metadata);
        assert_eq!(terminal.metadata_tokens, tokens);
        assert!(!terminal.hook_report_is_newer("custom:transfer", Some(16)));
        assert!(!terminal.metadata_report_sequence_is_fresh("custom:transfer", Some(18)));
        let response = app.handle_api_request_after_internal_events_drained(
            serde_json::from_value(serde_json::json!({
                "id":"old-pane-cli", "method":"pane.get", "params":{"pane_id":pane_id}
            }))
            .unwrap(),
        );
        let response: SuccessResponse = serde_json::from_str(&response).expect(&response);
        let ResponseResult::PaneInfo { pane: resolved } = response.result else {
            panic!("expected pane info");
        };
        assert_eq!(resolved.terminal_id, terminal_id.as_str());
        assert_eq!(resolved.pane_id, app.public_pane_id(1, pane).unwrap());
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn tab_transfer_keeps_background_source_and_destination_logical_selection() {
        let mut app = app();
        app.state.workspaces[0].switch_tab(2);
        app.state.workspaces[1].switch_tab(2);
        let source_active = app.public_tab_id(0, 2).unwrap();
        let destination_active = app.public_tab_id(1, 2).unwrap();
        let source_tab = app.public_tab_id(0, 0).unwrap();
        let destination = app.public_workspace_id(1);
        moved_tab(&transfer(&mut app, source_tab, destination));
        assert_eq!(
            app.public_tab_id(0, app.state.workspaces[0].active_tab),
            Some(source_active)
        );
        assert_eq!(
            app.public_tab_id(1, app.state.workspaces[1].active_tab),
            Some(destination_active)
        );
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn tab_transfer_remaps_notifications_focus_and_preserves_all_alias_generations() {
        let mut app = app();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let other = app.state.workspaces[1].tabs[0].root_pane;
        let source = app.public_workspace_id(0);
        let destination = app.public_workspace_id(1);
        let old_id = app.public_pane_id(0, pane).unwrap();
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane)
            .unwrap()
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Working);
        app.state.pane_id_aliases.insert(pane.raw(), other);
        app.state.pane_id_aliases.insert(u32::MAX - 1, pane);
        app.state
            .public_pane_id_aliases
            .insert("wold:p1".into(), pane);
        let raw_aliases = app.state.pane_id_aliases.clone();
        app.state.previous_pane_focus = Some(PaneFocusTarget {
            workspace_id: source.clone(),
            pane_id: pane,
        });
        app.last_focus = Some((0, pane));
        app.state.toast = Some(ToastNotification {
            kind: ToastKind::Finished,
            title: "done".into(),
            context: "retained".into(),
            position: None,
            target: Some(ToastTarget {
                workspace_id: source.clone(),
                pane_id: pane,
            }),
        });
        let pending = PendingAgentNotification {
            pane_id: pane,
            workspace_id: source.clone(),
            agent_label: "pi".into(),
            known_agent: Some(Agent::Pi),
            kind: ToastKind::Finished,
            state: AgentState::Idle,
            deadline: std::time::Instant::now(),
        };
        app.state
            .pending_agent_notifications
            .insert(pane, pending.clone());
        let source_tab = app.public_tab_id(0, 0).unwrap();
        let first = moved_tab(&transfer(&mut app, source_tab, destination.clone()));
        let intermediate = app.public_pane_id(1, pane).unwrap();
        assert_eq!(
            app.state.previous_pane_focus.as_ref().unwrap().workspace_id,
            destination
        );
        assert_eq!(app.last_focus, Some((1, pane)));
        assert_eq!(
            app.state
                .toast
                .as_ref()
                .unwrap()
                .target
                .as_ref()
                .unwrap()
                .workspace_id,
            destination
        );
        let mut expected = pending;
        expected.workspace_id = destination;
        assert_eq!(app.state.pending_agent_notifications[&pane], expected);
        assert_eq!(app.resolve_agent_target(&old_id).unwrap().pane_id, pane);
        assert_eq!(app.resolve_terminal_target(&old_id).unwrap().pane_id, pane);
        for target in [
            format!("p_{}", pane.raw()),
            format!("{}-1", source),
            "wold:p1".into(),
        ] {
            assert!(app.resolve_agent_target(&target).is_err(), "{target}");
        }
        moved_tab(&transfer(&mut app, first.tab_id, source));
        for target in [old_id, intermediate] {
            assert_eq!(app.parse_pane_id(&target), Some((0, pane)));
            let response = app.handle_api_request_after_internal_events_drained(serde_json::from_value(serde_json::json!({
                "id":"hook", "method":"pane.report_agent", "params": {
                    "pane_id":target, "source":"custom:tab-transfer", "agent":"pi", "state":"working", "seq":17
                }
            })).unwrap());
            let _: SuccessResponse = serde_json::from_str(&response).expect(&response);
        }
        assert_eq!(app.state.pane_id_aliases, raw_aliases);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn tab_transfer_preflight_rejects_invalid_targets_and_identity_collisions_atomically() {
        for case in [
            "same",
            "missing",
            "missing-tab",
            "replacement-tab-overflow",
            "replacement-pane-overflow",
            "replacement-alias",
            "fenced-source",
            "fenced-destination",
            "tab-overflow",
            "pane-overflow",
            "new-alias",
            "old-alias",
            "overlay",
            "destination-overlay",
            "runtime-restore",
            "empty-destination",
        ] {
            let mut app = app();
            if case.starts_with("replacement-") {
                app.state.workspaces[0] = Workspace::test_new("last-tab");
                app.state.ensure_test_terminals();
            }
            let source_tab = if case == "missing-tab" {
                "missing-tab".into()
            } else {
                app.public_tab_id(0, 0).unwrap()
            };
            let source = app.public_workspace_id(0);
            let destination = app.public_workspace_id(1);
            let pane = app.state.workspaces[0].tabs[0].root_pane;
            match case {
                "fenced-source" => {
                    app.state.workspace_transfer_fences.insert(source.clone());
                }
                "fenced-destination" => {
                    app.state
                        .workspace_transfer_fences
                        .insert(destination.clone());
                }
                "tab-overflow" => app.state.workspaces[1].next_public_tab_number = usize::MAX,
                "pane-overflow" => app.state.workspaces[1].next_public_pane_number = usize::MAX,
                "replacement-tab-overflow" => {
                    app.state.workspaces[0].next_public_tab_number = usize::MAX
                }
                "replacement-pane-overflow" => {
                    app.state.workspaces[0].next_public_pane_number = usize::MAX
                }
                "replacement-alias" => {
                    let number = app.state.workspaces[0].next_public_pane_number;
                    app.state.public_pane_id_aliases.insert(
                        crate::workspace::public_pane_id_for_number(&source, number),
                        pane,
                    );
                }
                "runtime-restore" => {
                    let root = app.state.workspaces[1].tabs[0].root_pane;
                    app.pending_worktree_remove_runtime_restores.insert(root, 7);
                }
                "empty-destination" => {
                    app.state.workspaces[1].tabs.clear();
                    app.state.workspaces[1].public_pane_numbers.clear();
                }
                "new-alias" => {
                    let number = app.state.workspaces[1].next_public_pane_number;
                    app.state.public_pane_id_aliases.insert(
                        crate::workspace::public_pane_id_for_number(&destination, number),
                        pane,
                    );
                }
                "old-alias" => {
                    let old = app.public_pane_id(0, pane).unwrap();
                    let other = app.state.workspaces[1].tabs[0].root_pane;
                    app.state.public_pane_id_aliases.insert(old, other);
                }
                "overlay" | "destination-overlay" => {
                    app.overlay_panes.insert(
                        pane,
                        crate::app::OverlayPaneState {
                            ws_idx: usize::from(case == "destination-overlay"),
                            tab_idx: 0,
                            previous_focus: pane,
                            previous_zoomed: false,
                            temp_files: Vec::new(),
                        },
                    );
                }
                _ => {}
            }
            let target = match case {
                "same" => source,
                "missing" => "wmissing".into(),
                _ => destination,
            };
            let before: Vec<_> = app
                .state
                .workspaces
                .iter()
                .map(|ws| {
                    (
                        ws.tabs
                            .iter()
                            .map(|tab| (tab.root_pane, tab.number))
                            .collect::<Vec<_>>(),
                        ws.public_pane_numbers.clone(),
                        ws.active_tab,
                        ws.next_public_tab_number,
                        ws.next_public_pane_number,
                    )
                })
                .collect();
            let aliases = app.state.public_pane_id_aliases.clone();
            let response = transfer(&mut app, source_tab, target);
            let _: ErrorResponse = serde_json::from_str(&response).expect(&response);
            let after: Vec<_> = app
                .state
                .workspaces
                .iter()
                .map(|ws| {
                    (
                        ws.tabs
                            .iter()
                            .map(|tab| (tab.root_pane, tab.number))
                            .collect::<Vec<_>>(),
                        ws.public_pane_numbers.clone(),
                        ws.active_tab,
                        ws.next_public_tab_number,
                        ws.next_public_pane_number,
                    )
                })
                .collect();
            assert_eq!(after, before, "{case}");
            assert_eq!(app.state.public_pane_id_aliases, aliases, "{case}");
        }
    }

    #[tokio::test]
    async fn last_tab_replacement_failure_leaves_original_live_tab_in_place() {
        let mut app = app();
        app.state.workspaces = vec![
            Workspace::test_new("source"),
            Workspace::test_new("destination"),
        ];
        app.state.ensure_test_terminals();
        app.state.default_shell = "/herdr-does-not-exist/tab-transfer-shell".into();
        let source = app.public_tab_id(0, 0).unwrap();
        let destination = app.public_workspace_id(1);
        let before = app.state.workspaces[0].tabs[0].root_pane;
        let counters = (
            app.state.workspaces[0].next_public_tab_number,
            app.state.workspaces[0].next_public_pane_number,
        );
        let response = transfer(&mut app, source.clone(), destination);
        let response: ErrorResponse = serde_json::from_str(&response).expect(&response);
        assert_eq!(response.error.code, "tab_create_failed");
        assert_eq!(app.public_tab_id(0, 0), Some(source));
        assert_eq!(app.state.workspaces[0].tabs[0].root_pane, before);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[1].tabs.len(), 1);
        assert_eq!(
            (
                app.state.workspaces[0].next_public_tab_number,
                app.state.workspaces[0].next_public_pane_number
            ),
            counters
        );
        assert_eq!(app.terminal_runtimes.len(), 0);
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn last_tab_transfer_creates_only_a_source_replacement_and_retains_runtime_history() {
        let mut app = app();
        app.state.workspaces = vec![
            Workspace::test_new("source"),
            Workspace::test_new("destination"),
        ];
        app.state.ensure_test_terminals();
        app.state.default_shell = super::super::test_support::exiting_test_command().into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
        let source = app.public_tab_id(0, 0).unwrap();
        let destination = app.public_workspace_id(1);
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane)
            .unwrap()
            .clone();
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(
            80,
            24,
            b"retained scrollback\r\n",
        );
        let history = runtime.snapshot_history();
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let workspace_id = app.public_workspace_id(0);
        let result = moved_tab(&transfer(&mut app, source, destination));
        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.public_workspace_id(0), workspace_id);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_ne!(app.state.workspaces[0].tabs[0].root_pane, pane);
        assert_eq!(app.state.workspaces[1].tabs.len(), 2);
        assert_eq!(app.parse_tab_id(&result.tab_id), Some((1, 1)));
        assert_eq!(
            app.state.workspaces[1].tabs[1].terminal_id(pane),
            Some(&terminal_id)
        );
        assert_eq!(app.terminal_runtimes.len(), 2);
        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .snapshot_history(),
            history
        );
        app.state.assert_invariants_for_test();
        super::super::test_support::shutdown_test_runtimes(&mut app);
    }
}
