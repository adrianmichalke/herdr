use std::time::{Duration, Instant};

use super::{App, SESSION_SAVE_DEBOUNCE};

enum SessionSaveJob {
    Clear,
    Save {
        snapshot: Box<crate::persist::SessionSnapshot>,
        history: Option<crate::persist::SessionHistorySnapshot>,
    },
}

impl super::AppState {
    pub(crate) fn remove_pane_aliases(
        &mut self,
        pane_ids: impl IntoIterator<Item = crate::layout::PaneId>,
    ) {
        let pane_ids: std::collections::HashSet<_> = pane_ids.into_iter().collect();
        self.pane_id_aliases
            .retain(|_, pane| !pane_ids.contains(pane));
        self.public_pane_id_aliases
            .retain(|_, pane| !pane_ids.contains(pane));
    }

    pub(crate) fn reserve_pane_alias_namespaces(&mut self) {
        crate::persist::reserve_pane_alias_namespaces(
            &mut self.workspaces,
            self.public_pane_id_aliases.keys(),
        );
    }
}

impl App {
    pub(crate) fn capture_session_snapshot(&self) -> crate::persist::SessionSnapshot {
        let mut snapshot = crate::persist::capture(
            &self.state.workspaces,
            &self.state.terminals,
            &self.terminal_runtimes,
            self.state.active,
            self.state.selected,
            self.state.next_agent_activity_seq,
        );
        snapshot.pane_aliases = crate::persist::PaneAliasesSnapshot::capture(
            &self.state.workspaces,
            &self.state.pane_id_aliases,
            &self.state.public_pane_id_aliases,
        );
        snapshot.reserve_pane_alias_namespaces();
        snapshot
    }

    pub(super) fn schedule_session_save(&mut self) {
        if self.policy.persist_session {
            self.pane_exit_checkpoint_pending = false;
            self.session_save_deadline
                .get_or_insert(Instant::now() + SESSION_SAVE_DEBOUNCE);
        }
    }

    pub(crate) fn sync_session_save_schedule(&mut self) {
        if self.state.session_dirty {
            self.state.session_dirty = false;
            self.schedule_session_save();
        }
    }

    fn reap_finished_session_save(&mut self) {
        if self
            .session_save_thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            if let Some(thread) = self.session_save_thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn capture_session_save_job(&self) -> SessionSaveJob {
        if self.state.workspaces.is_empty() {
            SessionSaveJob::Clear
        } else {
            let snapshot = self.capture_session_snapshot();
            let history = self.persist_pane_history.then(|| {
                crate::persist::capture_history(
                    &snapshot,
                    &self.state.workspaces,
                    &self.terminal_runtimes,
                )
            });
            SessionSaveJob::Save {
                snapshot: Box::new(snapshot),
                history,
            }
        }
    }

    pub(crate) fn start_background_session_save(&mut self) {
        if self.workspace_transfer_checkpoint_pending {
            self.session_save_deadline = Some(Instant::now() + Duration::from_millis(250));
            return;
        }
        if !self.policy.persist_session {
            self.session_save_deadline = None;
            return;
        }

        if self.pane_exit_checkpoint_pending && !self.state.session_dirty {
            self.session_save_deadline = None;
            return;
        }

        self.reap_finished_session_save();
        if self.session_save_thread.is_some() {
            self.session_save_deadline = Some(Instant::now() + Duration::from_millis(250));
            return;
        }

        let job = self.capture_session_save_job();
        self.pane_exit_checkpoint_pending = false;
        self.session_save_deadline = None;
        let writer = self.session_writer.clone();
        match std::thread::Builder::new()
            .name("herdr-session-save".into())
            .spawn(move || run_session_save_job(job, &writer))
        {
            Ok(thread) => self.session_save_thread = Some(thread),
            Err(err) => {
                tracing::warn!(err = %err, "failed to spawn session save thread; saving inline");
                run_session_save_job(self.capture_session_save_job(), &self.session_writer);
            }
        }
    }

    pub(crate) fn save_session_now(&mut self) {
        if self.workspace_transfer_checkpoint_pending {
            return;
        }
        if let Some(thread) = self.session_save_thread.take() {
            let _ = thread.join();
        }

        if !self.policy.persist_session {
            self.session_save_deadline = None;
            return;
        }

        run_session_save_job(self.capture_session_save_job(), &self.session_writer);
        self.pane_exit_checkpoint_pending = false;
        self.session_save_deadline = None;
    }

    pub(crate) fn checkpoint_session_before_pane_exit(&mut self) {
        if !self.policy.persist_session
            || (self.pane_exit_checkpoint_pending && !self.state.session_dirty)
        {
            return;
        }
        self.save_session_now();
        self.pane_exit_checkpoint_pending = true;
        self.state.session_dirty = false;
    }

    pub(crate) fn finish_checkpointed_pane_exit(&mut self) {
        if self.pane_exit_checkpoint_pending {
            self.state.session_dirty = false;
            self.session_save_deadline = None;
        }
    }

    pub(crate) fn save_session_on_shutdown(&mut self) {
        if self.pane_exit_checkpoint_pending && !self.state.session_dirty {
            self.session_save_deadline = None;
            return;
        }
        self.save_session_now();
    }
}

fn run_session_save_job(
    job: SessionSaveJob,
    writer: &std::sync::Mutex<crate::persist::SessionWriter>,
) {
    let mut writer = match writer.lock() {
        Ok(writer) => writer,
        Err(err) => {
            tracing::warn!(err = %err, "session writer is poisoned; refusing to modify session");
            return;
        }
    };
    match job {
        SessionSaveJob::Clear => writer.clear(),
        SessionSaveJob::Save { snapshot, history } => writer.save(&snapshot, history.as_ref()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{AppPolicy, AppState};
    use crate::config::Config;
    use crate::layout::PaneId;

    fn app() -> App {
        let (_, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &Config::default(),
            AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn session_save_job_captures_aliases_and_activity_without_client_config_state() {
        let mut app = app();
        app.state = AppState::test_with_adversarial_identity_state();
        let target = app.state.workspaces[0].tabs[0].root_pane;
        app.state.pane_id_aliases.insert(91, target);
        app.state
            .public_pane_id_aliases
            .insert("w9:p7".into(), target);
        app.state.next_agent_activity_seq = 73;
        let snapshot = app.capture_session_snapshot();
        assert_eq!(snapshot.pane_aliases.raw.get(&91), Some(&target.raw()));
        assert_eq!(
            snapshot.pane_aliases.public.get("w9:p7"),
            Some(&target.raw())
        );
        assert_eq!(snapshot.agent_activity_sequence, Some(73));
        assert_eq!(snapshot.sidebar_width, None);
        assert_eq!(snapshot.sidebar_section_split, None);
        assert!(snapshot.collapsed_space_keys.is_empty());
        let SessionSaveJob::Save {
            snapshot: saved, ..
        } = app.capture_session_save_job()
        else {
            panic!("populated session must be saved");
        };
        assert_eq!(saved.pane_aliases, snapshot.pane_aliases);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn moved_aliases_survive_json_and_cold_restore_with_fresh_actual_pane_ids() {
        let mut app = app();
        app.state = AppState::test_with_adversarial_identity_state();
        let target = app.state.workspaces[0].tabs[1].root_pane;
        app.state.pane_id_aliases.insert(91, target);
        let alias = format!(
            "{}:p{}",
            app.state.workspaces[0].id,
            crate::workspace::encode_public_number(80)
        );
        app.state
            .public_pane_id_aliases
            .insert(alias.clone(), target);
        let mut snapshot = app.capture_session_snapshot();
        for workspace in &mut snapshot.workspaces {
            for tab in &mut workspace.tabs {
                for pane in tab.panes.values_mut() {
                    pane.cwd = std::env::temp_dir().join(format!(
                        "herdr-missing-alias-restore-{}",
                        std::process::id()
                    ));
                }
            }
        }
        let mut dropped: crate::persist::WorkspaceSnapshot =
            serde_json::from_value(serde_json::to_value(&snapshot.workspaces[0]).unwrap()).unwrap();
        dropped.id = Some("dropped-workspace".into());
        dropped.tabs.clear();
        snapshot.workspaces.insert(0, dropped);
        let snapshot = serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        let ((workspaces, terminals, runtimes), aliases) = crate::persist::restore_with_aliases(
            &snapshot,
            None,
            24,
            80,
            1024,
            "",
            crate::config::ShellModeConfig::default(),
            false,
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        );
        app.state.workspaces = workspaces;
        app.state.terminals = terminals;
        app.terminal_runtimes = runtimes.into();
        app.state.pane_id_aliases = aliases.raw;
        app.state.public_pane_id_aliases = aliases.public;
        let (_, actual) = app.parse_pane_id(&alias).unwrap();
        assert_ne!(actual, target);
        assert_eq!(
            app.parse_pane_id("p_91").map(|(_, pane)| pane),
            Some(actual)
        );
        assert!(app.state.workspaces[0].next_public_pane_number > 80);
        app.state.assert_invariants_for_test();
    }

    #[cfg(unix)]
    #[test]
    fn two_handoff_restores_compose_aliases_for_original_hook_targets() {
        let mut app = app();
        app.state = AppState::test_with_adversarial_identity_state();
        let old_root = app.state.workspaces[0].tabs[0].root_pane;
        let old_target = app.state.workspaces[0].tabs[1].root_pane;
        app.state.pane_id_aliases.insert(old_root.raw(), old_target);
        let old_public = format!(
            "{}:p{}",
            app.state.workspaces[0].id,
            crate::workspace::encode_public_number(80)
        );
        app.state
            .public_pane_id_aliases
            .insert(old_public.clone(), old_target);
        for _ in 0..2 {
            let mut snapshot = app.capture_session_snapshot();
            for workspace in &mut snapshot.workspaces {
                for tab in &mut workspace.tabs {
                    for pane in tab.panes.values_mut() {
                        pane.cwd = std::env::temp_dir().join(format!(
                            "herdr-missing-handoff-alias-{}",
                            std::process::id()
                        ));
                    }
                }
            }
            let snapshot =
                serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
            let ((workspaces, terminals, runtimes), aliases) =
                crate::persist::restore_handoff_with_aliases(
                    &snapshot,
                    1024,
                    "",
                    crate::config::ShellModeConfig::default(),
                    &mut std::collections::HashMap::new(),
                    app.event_tx.clone(),
                    app.render_notify.clone(),
                    app.render_dirty.clone(),
                )
                .unwrap();
            app.state.workspaces = workspaces;
            app.state.terminals = terminals;
            app.terminal_runtimes = runtimes.into();
            app.state.pane_id_aliases = aliases.raw;
            app.state.public_pane_id_aliases = aliases.public;
            let (_, actual) = app.parse_pane_id(&old_public).unwrap();
            assert_eq!(
                app.parse_pane_id(&format!("p_{}", old_root.raw()))
                    .map(|(_, pane)| pane),
                Some(actual)
            );
            assert_eq!(
                app.parse_pane_id(&format!("p_{}", old_target.raw()))
                    .map(|(_, pane)| pane),
                Some(actual)
            );
            assert_ne!(actual, old_target);
            app.state.assert_invariants_for_test();
        }
    }

    #[test]
    fn removing_aliases_cleans_targets_without_removing_overlapping_live_raw_keys() {
        let mut app = app();
        app.state = AppState::test_with_adversarial_identity_state();
        let closed = app.state.workspaces[0].tabs[0].root_pane;
        let live = app.state.workspaces[0].tabs[1].root_pane;
        app.state.pane_id_aliases.insert(91, closed);
        app.state.pane_id_aliases.insert(closed.raw(), live);
        app.state
            .public_pane_id_aliases
            .insert("w9:p7".into(), closed);
        app.state
            .public_pane_id_aliases
            .insert("w9:p8".into(), live);
        app.state.remove_pane_aliases([closed]);
        assert_eq!(app.state.pane_id_aliases.get(&closed.raw()), Some(&live));
        assert!(!app.state.pane_id_aliases.contains_key(&91));
        assert!(!app.state.public_pane_id_aliases.contains_key("w9:p7"));
        assert_eq!(app.state.public_pane_id_aliases.get("w9:p8"), Some(&live));
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn closing_or_dying_panes_remove_alias_targets_without_removing_other_raw_bindings() {
        for died in [false, true] {
            let mut app = app();
            app.state = AppState::test_with_adversarial_identity_state();
            let closed = app.state.workspaces[0].tabs[0].root_pane;
            let live = app.state.workspaces[0].tabs[1].root_pane;
            app.state.pane_id_aliases.insert(91, closed);
            app.state.pane_id_aliases.insert(closed.raw(), live);
            app.state
                .public_pane_id_aliases
                .insert("w9:p7".into(), closed);
            app.state
                .public_pane_id_aliases
                .insert("w9:p8".into(), live);
            if died {
                app.state
                    .handle_app_event(crate::events::AppEvent::PaneDied {
                        pane_id: closed,
                        exit_reason: crate::platform::ChildExitReason::Exited,
                    });
            } else {
                assert!(!app.state.close_tab());
            }
            assert!(!app.state.pane_id_aliases.contains_key(&91));
            assert!(!app.state.public_pane_id_aliases.contains_key("w9:p7"));
            assert_eq!(app.state.pane_id_aliases.get(&closed.raw()), Some(&live));
            assert_eq!(app.state.public_pane_id_aliases.get("w9:p8"), Some(&live));
            app.state.assert_invariants_for_test();
        }
    }

    #[test]
    fn capture_does_not_persist_aliases_to_closed_or_dead_panes() {
        let mut app = app();
        app.state = AppState::test_with_adversarial_identity_state();
        app.state
            .pane_id_aliases
            .insert(91, PaneId::from_raw(u32::MAX));
        app.state
            .public_pane_id_aliases
            .insert("w9:p7".into(), PaneId::from_raw(u32::MAX));
        assert!(app.capture_session_snapshot().pane_aliases.is_empty());
    }
}
