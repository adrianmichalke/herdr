#[cfg(any(unix, test))]
use std::collections::HashSet;

use super::{App, AppState};
#[cfg(unix)]
use crate::layout::PaneId;
#[cfg(any(unix, test))]
use crate::workspace::Workspace;

#[cfg(unix)]
type PendingSessionSave = std::sync::Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>>;

impl AppState {
    pub(crate) fn workspace_transfer_fenced(&self, workspace_index: usize) -> bool {
        self.workspaces
            .get(workspace_index)
            .is_some_and(|workspace| self.workspace_transfer_fences.contains(&workspace.id))
    }

    #[cfg(any(unix, test))]
    pub(crate) fn extract_workspace_for_transfer(
        &mut self,
        workspace_id: &str,
    ) -> Result<Workspace, String> {
        let index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| "transferred workspace is no longer present".to_owned())?;
        let active_id = self
            .active
            .and_then(|index| self.workspaces.get(index))
            .map(|workspace| workspace.id.clone());
        let selected_id = self
            .workspaces
            .get(self.selected)
            .map(|workspace| workspace.id.clone());
        let workspace = self.workspaces.remove(index);
        let panes: HashSet<_> = workspace
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.keys().copied())
            .collect();
        let terminals: HashSet<_> = workspace
            .tabs
            .iter()
            .flat_map(|tab| {
                tab.panes
                    .values()
                    .map(|pane| pane.attached_terminal_id.clone())
            })
            .collect();
        self.pane_id_aliases.retain(|_, pane| !panes.contains(pane));
        self.public_pane_id_aliases
            .retain(|_, pane| !panes.contains(pane));
        self.plugin_panes.retain(|pane, _| !panes.contains(pane));
        self.pending_agent_notifications
            .retain(|pane, _| !panes.contains(pane));
        self.direct_attach_resize_locks
            .retain(|id| !terminals.contains(id));
        if self
            .previous_pane_focus
            .as_ref()
            .is_some_and(|target| target.workspace_id == workspace_id)
        {
            self.previous_pane_focus = None;
        }
        if self
            .toast
            .as_ref()
            .and_then(|toast| toast.target.as_ref())
            .is_some_and(|target| target.workspace_id == workspace_id)
        {
            self.toast = None;
        }
        for terminal in terminals {
            self.terminals.remove(&terminal);
        }
        self.active = active_id
            .and_then(|id| {
                self.workspaces
                    .iter()
                    .position(|workspace| workspace.id == id)
            })
            .or_else(|| {
                (!self.workspaces.is_empty())
                    .then_some(index.min(self.workspaces.len().saturating_sub(1)))
            });
        self.selected = selected_id
            .and_then(|id| {
                self.workspaces
                    .iter()
                    .position(|workspace| workspace.id == id)
            })
            .unwrap_or_else(|| index.min(self.workspaces.len().saturating_sub(1)));
        self.workspace_transfer_fences.remove(workspace_id);
        self.mark_session_dirty();
        Ok(workspace)
    }
}

impl App {
    pub(crate) fn transfer_request_is_fenced(&self, request: &crate::api::schema::Request) -> bool {
        self.transfer_request_is_fenced_at(request, self.state.active)
    }

    pub(crate) fn transfer_request_is_fenced_at(
        &self,
        request: &crate::api::schema::Request,
        active_workspace: Option<usize>,
    ) -> bool {
        if self.state.workspace_transfer_fences.is_empty() {
            return false;
        }
        if matches!(
            request.method,
            crate::api::schema::Method::WorkspaceTransfer(_)
                | crate::api::schema::Method::WorkspaceTransferStatus(_)
        ) {
            return false;
        }
        let targets = crate::server::workspace_transfer::routing::targets(request);
        if targets.iter().any(|(id, kind)| {
            if *kind == crate::server::workspace_transfer::routing::TargetKind::Agent {
                return self
                    .resolve_agent_target(id)
                    .is_ok_and(|target| self.state.workspace_transfer_fenced(target.ws_idx));
            }
            self.parse_workspace_id(id)
                .is_some_and(|index| self.state.workspace_transfer_fenced(index))
                || self
                    .parse_pane_id(id)
                    .is_some_and(|(index, _)| self.state.workspace_transfer_fenced(index))
                || self
                    .parse_tab_id(id)
                    .is_some_and(|(index, _)| self.state.workspace_transfer_fenced(index))
                || (id.starts_with("term_")
                    && self
                        .state
                        .workspaces
                        .iter()
                        .enumerate()
                        .any(|(index, workspace)| {
                            self.state.workspace_transfer_fenced(index)
                                && workspace.tabs.iter().any(|tab| {
                                    tab.panes
                                        .values()
                                        .any(|pane| pane.attached_terminal_id.as_str() == id)
                                })
                        }))
        }) {
            return true;
        }
        if targets.is_empty() {
            let method = request.method.operation().name;
            if method.starts_with("pane.")
                || method.starts_with("layout.")
                || method.starts_with("tab.")
                || method == "command.invoke"
                || method == "plugin.action.invoke"
                || method == "plugin.pane.open"
            {
                return active_workspace
                    .is_some_and(|index| self.state.workspace_transfer_fenced(index));
            }
        }
        match &request.method {
            crate::api::schema::Method::WorkspaceClose(params) if params.close_group => self
                .parse_workspace_id(&params.workspace_id)
                .is_some_and(|index| {
                    self.state
                        .workspace_group_close_indices(index)
                        .iter()
                        .any(|index| self.state.workspace_transfer_fenced(*index))
                }),
            crate::api::schema::Method::PluginUnlink(params) => {
                self.transfer_fenced_plugin(&params.plugin_id)
            }
            crate::api::schema::Method::PluginDisable(params) => {
                self.transfer_fenced_plugin(&params.plugin_id)
            }
            _ => false,
        }
    }

    fn transfer_fenced_plugin(&self, plugin_id: &str) -> bool {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .any(|(index, workspace)| {
                self.state.workspace_transfer_fenced(index)
                    && workspace.tabs.iter().any(|tab| {
                        tab.panes.keys().any(|pane| {
                            self.state
                                .plugin_panes
                                .get(pane)
                                .is_some_and(|plugin| plugin.plugin_id == plugin_id)
                        })
                    })
            })
    }

    #[cfg(unix)]
    pub(crate) fn release_transferred_workspace(
        &mut self,
        workspace_id: &str,
    ) -> Result<(Vec<PaneId>, Vec<crate::terminal::TerminalRuntime>), String> {
        let index = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| "transferred workspace is no longer present".to_owned())?;
        let workspace_info = self.workspace_info(index);
        let workspace = self.state.extract_workspace_for_transfer(workspace_id)?;
        let mut panes = Vec::new();
        let mut runtimes = Vec::new();
        for tab in workspace.tabs {
            for (pane_id, pane) in tab.panes {
                panes.push(pane_id);
                self.overlay_panes.remove(&pane_id);
                if let Some(runtime) = self.terminal_runtimes.remove(&pane.attached_terminal_id) {
                    runtimes.push(runtime);
                }
            }
        }
        self.last_focus = None;
        self.sync_agent_metadata_deadline();
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::WorkspaceClosed,
            data: crate::api::schema::EventData::WorkspaceClosed {
                workspace_id: workspace_id.into(),
                workspace: Some(workspace_info),
            },
        });
        self.sync_session_save_schedule();
        Ok((panes, runtimes))
    }

    #[cfg(unix)]
    pub(crate) fn receiver_checkpoint_job(
        &mut self,
        imported: &crate::persist::workspace_transfer::ImportedWorkspace,
    ) -> (
        impl FnOnce() -> std::io::Result<()> + Send + 'static,
        Option<PendingSessionSave>,
    ) {
        let mut snapshot = self.capture_session_snapshot();
        snapshot.agent_activity_sequence = Some(
            self.state
                .next_agent_activity_seq
                .max(imported.next_agent_activity_seq),
        );
        let mut incoming = crate::persist::capture(
            std::slice::from_ref(&imported.workspace),
            &imported.terminals,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Some(0),
            0,
            imported.next_agent_activity_seq,
        );
        for workspace in &mut incoming.workspaces {
            for tab in &mut workspace.tabs {
                for (raw, pane) in &mut tab.panes {
                    if let Some(cwd) = imported.checkpoint_cwds.get(raw) {
                        pane.cwd = cwd.clone();
                    }
                }
            }
        }
        snapshot.workspaces.extend(incoming.workspaces);
        if snapshot.active.is_none() {
            snapshot.active = Some(snapshot.workspaces.len().saturating_sub(1));
            snapshot.selected = snapshot.active.unwrap_or(0);
        }
        let history = (self.policy.persist_session && self.persist_pane_history).then(|| {
            crate::persist::SessionWriter::capture_transfer_checkpoint_history(
                &snapshot,
                &self.state.workspaces,
                &self.terminal_runtimes,
                &imported.workspace,
                &imported.runtimes,
            )
        });
        let previous_save = self
            .session_save_thread
            .take()
            .map(|thread| std::sync::Arc::new(std::sync::Mutex::new(Some(thread))));
        let job_previous_save = previous_save.clone();
        let writer = self.session_writer.clone();
        let persist = self.policy.persist_session;
        self.workspace_transfer_checkpoint_pending = true;
        let job = move || {
            if let Some(previous) = job_previous_save {
                let thread = previous
                    .lock()
                    .map_err(|_| std::io::Error::other("previous session save handle is poisoned"))?
                    .take();
                if let Some(thread) = thread {
                    thread
                        .join()
                        .map_err(|_| std::io::Error::other("previous session save panicked"))?;
                }
            }
            if !persist {
                return Ok(());
            }
            writer
                .lock()
                .map_err(|_| std::io::Error::other("session writer is poisoned"))?
                .save_transfer_checkpoint(&snapshot, history.as_ref())
        };
        (job, previous_save)
    }

    #[cfg(unix)]
    pub(crate) fn insert_transferred_workspace(
        &mut self,
        mut imported: crate::persist::workspace_transfer::ImportedWorkspace,
    ) {
        let index = self.state.workspaces.len();
        for (id, terminal) in imported.terminals {
            self.state.terminals.insert(id, terminal);
        }
        for runtime in imported.runtimes.values_mut() {
            runtime.assume_handoff_ownership();
        }
        for (id, runtime) in imported.runtimes {
            self.terminal_runtimes.insert(id, runtime);
        }
        for plugin in imported.plugins {
            self.state
                .installed_plugins
                .entry(plugin.plugin_id.clone())
                .or_insert(plugin);
        }
        self.state.plugin_panes.extend(imported.plugin_panes);
        self.state.workspaces.push(imported.workspace);
        if self.state.active.is_none() {
            self.state.active = Some(index);
            self.state.selected = index;
        }
        self.state.next_agent_state_change_seq = self
            .state
            .next_agent_state_change_seq
            .max(imported.next_agent_state_change_seq);
        self.state.next_agent_activity_seq = self
            .state
            .next_agent_activity_seq
            .max(imported.next_agent_activity_seq);
        self.sync_agent_metadata_deadline();
        self.emit_workspace_open_events(index);
        for tab in 1..self.state.workspaces[index].tabs.len() {
            self.emit_tab_created_events(index, tab);
        }
        self.state.mark_session_dirty();
        self.sync_session_save_schedule();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracting_one_workspace_preserves_other_workspace_identities_and_focus() {
        let mut state = AppState::test_with_adversarial_identity_state();
        let extra = Workspace::test_new("unrelated");
        let extra_id = extra.id.clone();
        state.workspaces.push(extra);
        state.ensure_test_terminals();
        state.active = Some(1);
        state.selected = 1;
        let moved_id = state.workspaces[0].id.clone();
        let terminal_shutdowns = state.terminal_runtime_shutdowns.clone();
        let moved = state.extract_workspace_for_transfer(&moved_id).unwrap();
        moved.assert_invariants_for_test();
        state.assert_invariants_for_test();
        assert_eq!(state.workspaces[0].id, extra_id);
        assert_eq!(state.active, Some(0));
        assert_eq!(state.selected, 0);
        assert_eq!(state.terminal_runtime_shutdowns, terminal_shutdowns);
    }

    #[test]
    fn extracting_last_workspace_cleans_references_without_scheduling_runtime_shutdown() {
        let mut state = AppState::test_with_adversarial_identity_state();
        let id = state.workspaces[0].id.clone();
        let moved = state.extract_workspace_for_transfer(&id).unwrap();
        moved.assert_invariants_for_test();
        state.assert_invariants_for_test();
        assert!(state.terminal_runtime_shutdowns.is_empty());
    }
}
