use std::collections::{HashMap, HashSet};

use crate::api::schema::AgentStatus;
use crate::protocol::{ClientShellAgent, ClientShellSnapshot, PaneSurfaceFrame};

#[derive(Clone, Debug, Default)]
pub(super) struct EndpointAgentPresentation {
    boot_id: Option<String>,
    acknowledged: HashMap<String, u64>,
    manually_unseen: HashMap<String, u64>,
    completed: HashMap<String, u64>,
    working: HashSet<String>,
    activity_times: HashMap<String, (u64, Option<String>)>,
    pending_completions: Option<(
        Option<u64>,
        crate::protocol::endpoint::EndpointAgentCompletions,
    )>,
}

impl EndpointAgentPresentation {
    pub(super) fn receive_completions(
        &mut self,
        generation: Option<u64>,
        completions: crate::protocol::endpoint::EndpointAgentCompletions,
    ) {
        if self
            .pending_completions
            .as_ref()
            .is_some_and(|(current_generation, current)| {
                *current_generation == generation
                    && current.boot_id == completions.boot_id
                    && current.revision >= completions.revision
            })
        {
            return;
        }
        self.pending_completions = Some((generation, completions));
    }

    #[cfg(test)]
    pub(super) fn project_snapshot(&mut self, snapshot: &mut ClientShellSnapshot) {
        let _ = self.project_snapshot_for_generation(snapshot, None);
    }

    pub(super) fn project_snapshot_for_generation(
        &mut self,
        snapshot: &mut ClientShellSnapshot,
        generation: Option<u64>,
    ) -> Option<crate::protocol::endpoint::EndpointAgentCompletions> {
        if self.boot_id.as_deref() != Some(snapshot.boot_id.as_str()) {
            self.boot_id = Some(snapshot.boot_id.clone());
            self.acknowledged.clear();
            self.manually_unseen.clear();
            self.completed.clear();
            self.working.clear();
            self.acknowledged.extend(
                snapshot
                    .agents
                    .iter()
                    .map(|agent| (agent.pane_id.clone(), agent.state_change_seq)),
            );
        }
        let pane_ids: HashSet<&str> = snapshot
            .agents
            .iter()
            .map(|agent| agent.pane_id.as_str())
            .collect();
        self.acknowledged
            .retain(|pane_id, _| pane_ids.contains(pane_id.as_str()));
        self.manually_unseen
            .retain(|pane_id, _| pane_ids.contains(pane_id.as_str()));
        self.completed
            .retain(|pane_id, _| pane_ids.contains(pane_id.as_str()));
        self.working
            .retain(|pane_id| pane_ids.contains(pane_id.as_str()));
        let projection = self
            .pending_completions
            .take()
            .filter(|(received_generation, projection)| {
                *received_generation == generation
                    && projection.boot_id == snapshot.boot_id
                    && projection.revision == snapshot.revision
            })
            .map(|(_, projection)| projection);
        let completions = projection
            .as_ref()
            .map(|projection| &projection.completions);
        self.project_activity_times(snapshot, projection.as_ref());
        for agent in &mut snapshot.agents {
            if !matches!(agent.agent_status, AgentStatus::Idle | AgentStatus::Done)
                || self
                    .manually_unseen
                    .get(&agent.pane_id)
                    .is_some_and(|sequence| *sequence != agent.state_change_seq)
            {
                self.manually_unseen.remove(&agent.pane_id);
            }
            match agent.agent_status {
                AgentStatus::Working => {
                    self.working.insert(agent.pane_id.clone());
                    self.completed.remove(&agent.pane_id);
                }
                AgentStatus::Blocked => {
                    self.completed.remove(&agent.pane_id);
                }
                AgentStatus::Idle | AgentStatus::Done => {
                    let observed_work = self.working.remove(&agent.pane_id);
                    let completed = completions.as_ref().map_or_else(
                        || {
                            observed_work
                                || self.completed.get(&agent.pane_id)
                                    == Some(&agent.state_change_seq)
                        },
                        |completions| {
                            completions.get(&agent.pane_id) == Some(&agent.state_change_seq)
                        },
                    );
                    if completed {
                        self.completed
                            .insert(agent.pane_id.clone(), agent.state_change_seq);
                    } else {
                        self.completed.remove(&agent.pane_id);
                    }
                }
                _ => {
                    self.working.remove(&agent.pane_id);
                    self.completed.remove(&agent.pane_id);
                }
            }
            agent.agent_status = self.projected_status(agent);
        }
        project_aggregate_status(snapshot);
        projection
    }

    fn project_activity_times(
        &mut self,
        snapshot: &mut ClientShellSnapshot,
        projection: Option<&crate::protocol::endpoint::EndpointAgentCompletions>,
    ) {
        let activity = projection.and_then(|projection| projection.activity.as_ref());
        let panes = snapshot
            .panes
            .iter()
            .map(|pane| pane.pane_id.as_str())
            .chain(snapshot.agents.iter().map(|agent| agent.pane_id.as_str()))
            .collect::<HashSet<_>>();
        self.activity_times
            .retain(|pane_id, _| panes.contains(pane_id.as_str()));
        let key = crate::config::AgentSidebarToken::ACTIVITY_TIME_CACHE_KEY;
        for agent in &mut snapshot.agents {
            agent.tokens.retain(|(name, _)| name != key);
            let timestamp = activity
                .and_then(|activity| activity.get(&agent.pane_id))
                .and_then(Option::as_ref)
                .and_then(|activity| activity.changed_at_ms);
            let Some(timestamp) = timestamp else {
                continue;
            };
            let cached = self
                .activity_times
                .entry(agent.pane_id.clone())
                .or_insert_with(|| (timestamp, activity_time(timestamp)));
            if cached.0 != timestamp {
                *cached = (timestamp, activity_time(timestamp));
            }
            if let Some(formatted) = cached.1.as_ref() {
                agent.tokens.push((key.to_owned(), formatted.clone()));
            }
        }
    }

    pub(super) fn mark_unseen(
        &mut self,
        snapshot: &mut ClientShellSnapshot,
        pane_id: &str,
    ) -> bool {
        if self.boot_id.as_deref() != Some(snapshot.boot_id.as_str()) {
            return false;
        }
        let Some(agent) = snapshot
            .agents
            .iter_mut()
            .find(|agent| agent.pane_id == pane_id)
        else {
            return false;
        };
        if !matches!(agent.agent_status, AgentStatus::Idle | AgentStatus::Done) || !self.seen(agent)
        {
            return false;
        }
        self.manually_unseen
            .insert(agent.pane_id.clone(), agent.state_change_seq);
        agent.agent_status = AgentStatus::Done;
        project_aggregate_status(snapshot);
        true
    }

    pub(super) fn acknowledge_surface(
        &mut self,
        snapshot: &mut ClientShellSnapshot,
        surface: &PaneSurfaceFrame,
        outer_focused: Option<bool>,
    ) -> bool {
        if outer_focused == Some(false)
            || self.boot_id.as_deref() != Some(surface.boot_id.as_str())
            || snapshot.boot_id != surface.boot_id
            || snapshot.revision != surface.projection_revision
        {
            return false;
        }

        let mut changed = false;
        for pane in &surface.panes {
            let Some(agent) = snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane.pane_id)
            else {
                continue;
            };
            changed |= self.manually_unseen.remove(&agent.pane_id).is_some();
            let acknowledged = self.acknowledged.entry(agent.pane_id.clone()).or_default();
            if *acknowledged < agent.state_change_seq {
                *acknowledged = agent.state_change_seq;
                changed = true;
            }
        }
        if changed {
            for agent in &mut snapshot.agents {
                agent.agent_status = self.projected_status(agent);
            }
            project_aggregate_status(snapshot);
        }
        changed
    }

    pub(super) fn marked_unseen(&self, agent: &ClientShellAgent) -> bool {
        self.manually_unseen.get(&agent.pane_id) == Some(&agent.state_change_seq)
    }

    pub(super) fn has_marked_surface(&self, surface: &PaneSurfaceFrame) -> bool {
        !self.manually_unseen.is_empty()
            && surface
                .panes
                .iter()
                .any(|pane| self.manually_unseen.contains_key(&pane.pane_id))
    }

    pub(super) fn seen(&self, agent: &ClientShellAgent) -> bool {
        if self.marked_unseen(agent) {
            return false;
        }
        self.completed.get(&agent.pane_id).is_none_or(|completion| {
            self.acknowledged
                .get(&agent.pane_id)
                .is_some_and(|sequence| sequence >= completion)
        })
    }

    fn projected_status(&self, agent: &ClientShellAgent) -> AgentStatus {
        match agent.agent_status {
            AgentStatus::Idle | AgentStatus::Done => {
                if self.seen(agent) {
                    AgentStatus::Idle
                } else {
                    AgentStatus::Done
                }
            }
            status => status,
        }
    }
}

fn activity_time(timestamp_ms: u64) -> Option<String> {
    let local = crate::platform::local_datetime_at(timestamp_ms / 1_000)?;
    Some(format!(
        "{:02}:{:02}:{:02}",
        local.hour(),
        local.minute(),
        local.second()
    ))
}

fn project_aggregate_status(snapshot: &mut ClientShellSnapshot) {
    for tab in &mut snapshot.tabs {
        if let Some(status) = snapshot
            .agents
            .iter()
            .filter(|agent| agent.tab_id == tab.tab_id)
            .map(|agent| agent.agent_status)
            .max_by_key(|status| super::status_priority(*status))
        {
            tab.agent_status = status;
        }
    }
    for workspace in &mut snapshot.workspaces {
        if let Some(status) = snapshot
            .agents
            .iter()
            .filter(|agent| agent.workspace_id == workspace.workspace_id)
            .map(|agent| agent.agent_status)
            .max_by_key(|status| super::status_priority(*status))
        {
            workspace.agent_status = status;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        endpoint::EndpointAgentCompletions, FrameData, PaneSurfacePane, SurfaceRect,
    };

    fn agent(status: AgentStatus, sequence: u64) -> ClientShellAgent {
        ClientShellAgent {
            pane_id: "agent-pane".into(),
            workspace_id: "workspace".into(),
            tab_id: "tab".into(),
            name: None,
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: sequence,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        }
    }

    fn snapshot(status: AgentStatus, sequence: u64, revision: u64) -> ClientShellSnapshot {
        let mut snapshot = crate::client::shell::tests::snapshot();
        snapshot.boot_id = "endpoint-boot".into();
        snapshot.revision = revision;
        snapshot.agents = vec![agent(status, sequence)];
        snapshot
    }

    fn surface(revision: u64) -> PaneSurfaceFrame {
        let rect = SurfaceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        PaneSurfaceFrame {
            boot_id: "endpoint-boot".into(),
            projection_revision: revision,
            surface_revision: 1,
            frame: FrameData {
                cells: Vec::new(),
                width: 0,
                height: 0,
                cursor: None,
                hyperlinks: Vec::new(),
                graphics: Vec::new(),
            },
            panes: vec![PaneSurfacePane {
                pane_id: "agent-pane".into(),
                content_revision: 1,
                rect,
                inner_rect: rect,
                scrollbar_rect: None,
                scroll: None,
                focused: true,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
            splits: Vec::new(),
            popup: None,
            graphics: Default::default(),
        }
    }

    #[test]
    fn mark_unseen_preserves_completion_evidence_and_acknowledges_equal_sequences() {
        for sequence in [0, 4, u64::MAX] {
            let mut presentation = EndpointAgentPresentation::default();
            let mut projected = snapshot(AgentStatus::Idle, sequence, 1);
            presentation.project_snapshot(&mut projected);
            let acknowledged = presentation.acknowledged.clone();
            let completed = presentation.completed.clone();
            let activity = presentation.activity_times.clone();

            assert!(presentation.mark_unseen(&mut projected, "agent-pane"));
            assert!(!presentation.mark_unseen(&mut projected, "agent-pane"));
            assert_eq!(projected.agents[0].agent_status, AgentStatus::Done);
            assert_eq!(presentation.acknowledged, acknowledged);
            assert_eq!(presentation.completed, completed);
            assert_eq!(presentation.activity_times, activity);
            assert_eq!(projected.agents[0].state_change_seq, sequence);

            presentation.receive_completions(None, completions("endpoint-boot", 2, None));
            let mut refreshed = snapshot(AgentStatus::Idle, sequence, 2);
            presentation.project_snapshot(&mut refreshed);
            assert_eq!(refreshed.agents[0].agent_status, AgentStatus::Done);
            assert!(!presentation.acknowledge_surface(&mut refreshed, &surface(1), Some(true)));
            assert!(!presentation.acknowledge_surface(&mut refreshed, &surface(2), Some(false)));
            assert_eq!(refreshed.agents[0].agent_status, AgentStatus::Done);
            assert!(presentation.acknowledge_surface(&mut refreshed, &surface(2), Some(true)));
            assert_eq!(refreshed.agents[0].agent_status, AgentStatus::Idle);
            assert!(presentation.manually_unseen.is_empty());
            assert_eq!(presentation.acknowledged, acknowledged);
        }
    }

    #[test]
    fn mark_unseen_reuses_a_viewed_completion_without_manufacturing_another() {
        let mut presentation = EndpointAgentPresentation::default();
        let mut working = snapshot(AgentStatus::Working, 4, 1);
        presentation.project_snapshot(&mut working);
        let mut finished = snapshot(AgentStatus::Idle, 5, 2);
        presentation.project_snapshot(&mut finished);
        assert_eq!(finished.agents[0].agent_status, AgentStatus::Done);
        assert!(!presentation.mark_unseen(&mut finished, "agent-pane"));
        assert!(presentation.manually_unseen.is_empty());
        assert!(presentation.acknowledge_surface(&mut finished, &surface(2), Some(true)));
        assert!(presentation.mark_unseen(&mut finished, "agent-pane"));
        assert_eq!(presentation.completed["agent-pane"], 5);
        assert_eq!(presentation.acknowledged["agent-pane"], 5);
        assert_eq!(finished.agents[0].state_change_seq, 5);
        assert_eq!(finished.agents[0].agent_status, AgentStatus::Done);
    }

    #[test]
    fn mark_unseen_is_scoped_to_a_client_and_the_current_idle_lifecycle() {
        let mut first = EndpointAgentPresentation::default();
        let mut second = EndpointAgentPresentation::default();
        let mut first_snapshot = snapshot(AgentStatus::Idle, 4, 1);
        let mut second_snapshot = first_snapshot.clone();
        first.project_snapshot(&mut first_snapshot);
        second.project_snapshot(&mut second_snapshot);
        assert!(first.mark_unseen(&mut first_snapshot, "agent-pane"));
        assert!(!first.seen(&first_snapshot.agents[0]));
        assert!(second.seen(&second_snapshot.agents[0]));
        assert_eq!(second_snapshot.agents[0].agent_status, AgentStatus::Idle);

        for (status, sequence) in [
            (AgentStatus::Working, 4),
            (AgentStatus::Blocked, 4),
            (AgentStatus::Unknown, 4),
            (AgentStatus::Idle, 5),
        ] {
            let mut presentation = first.clone();
            let mut changed = snapshot(status, sequence, 2);
            presentation.project_snapshot(&mut changed);
            assert!(presentation.manually_unseen.is_empty());
            assert_eq!(changed.agents[0].agent_status, status);
            if status != AgentStatus::Idle {
                assert!(!presentation.mark_unseen(&mut changed, "agent-pane"));
            }
        }
    }

    #[test]
    fn mark_unseen_does_not_survive_a_removed_agent_or_a_new_boot() {
        for new_boot in [false, true] {
            let mut presentation = EndpointAgentPresentation::default();
            let mut initial = snapshot(AgentStatus::Idle, 4, 1);
            presentation.project_snapshot(&mut initial);
            assert!(presentation.mark_unseen(&mut initial, "agent-pane"));
            let mut changed = snapshot(AgentStatus::Idle, 4, 2);
            if new_boot {
                changed.boot_id = "replacement-boot".into();
            } else {
                changed.agents.clear();
            }
            presentation.project_snapshot(&mut changed);
            assert!(presentation.manually_unseen.is_empty());
            if new_boot {
                assert_eq!(changed.agents[0].agent_status, AgentStatus::Idle);
            }
        }
        let mut presentation = EndpointAgentPresentation::default();
        let mut unknown = snapshot(AgentStatus::Idle, 4, 1);
        assert!(!presentation.mark_unseen(&mut unknown, "agent-pane"));
        presentation.project_snapshot(&mut unknown);
        assert!(!presentation.mark_unseen(&mut unknown, "missing-pane"));
    }

    #[test]
    fn first_snapshot_establishes_an_idle_baseline_without_server_seen_authority() {
        let mut presentation = EndpointAgentPresentation::default();
        let mut snapshot = snapshot(AgentStatus::Done, 4, 1);

        presentation.project_snapshot(&mut snapshot);

        assert_eq!(snapshot.agents[0].agent_status, AgentStatus::Idle);
    }

    fn assert_idle_sequence(states: &[(AgentStatus, u64)]) {
        let mut presentation = EndpointAgentPresentation::default();
        let mut projected = AgentStatus::Unknown;
        for (revision, &(status, seq)) in states.iter().enumerate() {
            let mut snapshot = snapshot(status, seq, revision as u64 + 1);
            presentation.project_snapshot(&mut snapshot);
            projected = snapshot.agents[0].agent_status;
        }
        assert_eq!(projected, AgentStatus::Idle, "{states:?}");
    }

    fn completions(boot: &str, revision: u64, seq: Option<u64>) -> EndpointAgentCompletions {
        EndpointAgentCompletions {
            activity: None,
            boot_id: boot.into(),
            revision,
            completions: seq
                .map(|seq| ("agent-pane".into(), seq))
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn matching_activity_companion_is_returned_and_formats_cached_local_time() {
        let mut presentation = EndpointAgentPresentation::default();
        let mut companion = completions("endpoint-boot", 1, None);
        let timestamp = 1_700_000_000_123;
        companion.activity = Some(std::collections::BTreeMap::from([(
            "agent-pane".into(),
            Some(crate::agent_activity::AgentActivity {
                sequence: 99,
                changed_at_ms: Some(timestamp),
            }),
        )]));
        presentation.receive_completions(Some(3), companion.clone());
        let mut projected = snapshot(AgentStatus::Idle, 1, 1);
        let returned = presentation.project_snapshot_for_generation(&mut projected, Some(3));
        assert_eq!(returned, Some(companion));
        let key = crate::config::AgentSidebarToken::ACTIVITY_TIME_CACHE_KEY;
        let expected = activity_time(timestamp);
        assert_eq!(
            projected.agents[0]
                .tokens
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            expected.as_ref()
        );
        assert_eq!(
            presentation.activity_times["agent-pane"],
            (timestamp, expected)
        );
        assert_eq!(projected.agents[0].state_change_seq, 1);
        assert_eq!(projected.agents[0].agent_status, AgentStatus::Idle);
    }

    #[test]
    fn activity_timestamp_requires_matching_boot_revision_and_generation() {
        for (boot, revision, generation) in [
            ("old-boot", 1, Some(1)),
            ("endpoint-boot", 2, Some(1)),
            ("endpoint-boot", 1, Some(2)),
        ] {
            let mut presentation = EndpointAgentPresentation::default();
            let mut companion = completions(boot, revision, None);
            companion.activity = Some(std::collections::BTreeMap::from([(
                "agent-pane".into(),
                Some(crate::agent_activity::AgentActivity {
                    sequence: 4,
                    changed_at_ms: Some(1_700_000_000_000),
                }),
            )]));
            presentation.receive_completions(generation, companion);
            let mut projected = snapshot(AgentStatus::Idle, 1, 1);
            assert!(presentation
                .project_snapshot_for_generation(&mut projected, Some(1))
                .is_none());
            assert!(projected.agents[0].tokens.is_empty());
        }
    }

    #[test]
    fn missing_malformed_and_out_of_range_timestamps_do_not_invent_times() {
        for activity in [
            serde_json::json!({}),
            serde_json::json!({"agent-pane": null}),
            serde_json::json!({"agent-pane": {"sequence": 4}}),
            serde_json::json!({"agent-pane": {"sequence": 4, "changed_at_ms": "invalid"}}),
            serde_json::json!({"agent-pane": {"sequence": 4, "changed_at_ms": u64::MAX}}),
        ] {
            let mut presentation = EndpointAgentPresentation::default();
            let companion = serde_json::from_value(serde_json::json!({
                "boot_id": "endpoint-boot",
                "revision": 1,
                "completions": {},
                "activity": activity,
            }))
            .expect("tolerant optional activity");
            presentation.receive_completions(None, companion);
            let mut projected = snapshot(AgentStatus::Idle, 1, 1);
            let companion = presentation
                .project_snapshot_for_generation(&mut projected, None)
                .unwrap();
            assert!(companion.activity.is_some());
            assert!(projected.agents[0].tokens.is_empty());
        }
        assert!(activity_time(u64::MAX).is_none());
    }

    #[test]
    fn authoritative_activity_replaces_spoofed_tokens_and_legacy_omits_unknown_time() {
        let key = crate::config::AgentSidebarToken::ACTIVITY_TIME_CACHE_KEY;
        let mut presentation = EndpointAgentPresentation::default();
        let mut projected = snapshot(AgentStatus::Idle, 1, 1);
        projected.agents[0].tokens = vec![
            (key.into(), "spoofed".into()),
            (key.into(), "another spoof".into()),
            ("activity_time".into(), "custom value".into()),
        ];
        presentation.project_snapshot(&mut projected);
        assert_eq!(
            projected.agents[0].tokens,
            [("activity_time".into(), "custom value".into())]
        );
        let mut companion = completions("endpoint-boot", 2, None);
        let timestamp = 1_700_000_000_000;
        companion.activity = Some(std::collections::BTreeMap::from([(
            "agent-pane".into(),
            Some(crate::agent_activity::AgentActivity {
                sequence: 1,
                changed_at_ms: Some(timestamp),
            }),
        )]));
        presentation.receive_completions(None, companion);
        projected.revision = 2;
        projected.agents[0]
            .tokens
            .push((key.into(), "spoofed".into()));
        presentation.project_snapshot(&mut projected);
        assert_eq!(
            projected.agents[0]
                .tokens
                .iter()
                .filter(|(name, _)| name == key)
                .count(),
            usize::from(activity_time(timestamp).is_some())
        );
        assert!(!projected.agents[0]
            .tokens
            .iter()
            .any(|(_, value)| value == "spoofed"));
        projected.revision = 3;
        presentation.project_snapshot(&mut projected);
        assert!(!projected.agents[0]
            .tokens
            .iter()
            .any(|(name, _)| name == key));
    }

    #[test]
    fn completion_guard_initial_unknown_to_idle_does_not_project_done() {
        assert_idle_sequence(&[(AgentStatus::Unknown, 0), (AgentStatus::Idle, 1)]);
    }

    #[test]
    fn completion_guard_new_idle_agent_is_not_completed_work() {
        let mut presentation = EndpointAgentPresentation::default();
        let mut baseline = snapshot(AgentStatus::Idle, 0, 1);
        baseline.agents.clear();
        presentation.project_snapshot(&mut baseline);
        let mut ready = snapshot(AgentStatus::Idle, 1, 2);

        presentation.project_snapshot(&mut ready);

        assert_eq!(ready.agents[0].agent_status, AgentStatus::Idle);
    }

    #[test]
    fn completion_guard_session_rebind_does_not_project_done() {
        assert_idle_sequence(&[
            (AgentStatus::Idle, 4),
            (AgentStatus::Unknown, 5),
            (AgentStatus::Idle, 6),
        ]);
    }

    #[test]
    fn completion_guard_startup_blocker_does_not_project_done() {
        assert_idle_sequence(&[
            (AgentStatus::Unknown, 0),
            (AgentStatus::Blocked, 1),
            (AgentStatus::Idle, 2),
        ]);
    }

    #[test]
    fn completion_guard_distinguishes_coalesced_work_from_rebind() {
        for completion in [None, Some(6)] {
            let mut presentation = EndpointAgentPresentation::default();
            let mut baseline = snapshot(AgentStatus::Idle, 4, 1);
            presentation.project_snapshot(&mut baseline);
            presentation.receive_completions(None, completions(&baseline.boot_id, 2, completion));
            let mut settled = snapshot(AgentStatus::Idle, 6, 2);

            presentation.project_snapshot(&mut settled);

            let expected = if completion.is_some() {
                AgentStatus::Done
            } else {
                AgentStatus::Idle
            };
            assert_eq!(settled.agents[0].agent_status, expected);
            presentation.project_snapshot(&mut settled);
            assert_eq!(settled.agents[0].agent_status, expected);
        }
    }

    #[test]
    fn completion_guard_server_suppression_overrides_client_observed_work() {
        let mut presentation = EndpointAgentPresentation::default();
        let mut baseline = snapshot(AgentStatus::Working, 1, 1);
        presentation.project_snapshot(&mut baseline);
        presentation.receive_completions(None, completions(&baseline.boot_id, 2, None));
        let mut settled = snapshot(AgentStatus::Idle, 2, 2);

        presentation.project_snapshot(&mut settled);

        assert_eq!(settled.agents[0].agent_status, AgentStatus::Idle);
    }

    #[test]
    fn completion_guard_companion_is_scoped_to_boot_revision_and_connection() {
        for (boot, revision, generation, seq) in [
            ("old-boot", 2, Some(1), 6),
            ("endpoint-boot", 1, Some(1), 6),
            ("endpoint-boot", 2, Some(0), 6),
            ("endpoint-boot", 2, Some(1), 5),
        ] {
            let mut presentation = EndpointAgentPresentation::default();
            let mut baseline = snapshot(AgentStatus::Idle, 4, 1);
            let _ = presentation.project_snapshot_for_generation(&mut baseline, Some(1));
            presentation.receive_completions(generation, completions(boot, revision, Some(seq)));
            let mut settled = snapshot(AgentStatus::Idle, 6, 2);

            let _ = presentation.project_snapshot_for_generation(&mut settled, Some(1));

            assert_eq!(settled.agents[0].agent_status, AgentStatus::Idle);
        }
    }

    #[test]
    fn completion_guard_new_boot_baselines_existing_completions() {
        let mut presentation = EndpointAgentPresentation::default();
        let mut old = snapshot(AgentStatus::Working, 1, 1);
        old.boot_id = "old-boot".into();
        presentation.project_snapshot(&mut old);
        presentation.receive_completions(None, completions("endpoint-boot", 1, Some(6)));
        let mut restored = snapshot(AgentStatus::Idle, 6, 1);

        presentation.project_snapshot(&mut restored);

        assert_eq!(restored.agents[0].agent_status, AgentStatus::Idle);
    }

    #[test]
    fn unpresented_working_completion_projects_done() {
        let mut presentation = EndpointAgentPresentation::default();
        let mut initial = snapshot(AgentStatus::Working, 4, 1);
        presentation.project_snapshot(&mut initial);
        let mut completed = snapshot(AgentStatus::Idle, 5, 2);

        presentation.project_snapshot(&mut completed);

        assert_eq!(completed.agents[0].agent_status, AgentStatus::Done);
    }

    #[test]
    fn coherent_presented_surface_acknowledges_completion() {
        let mut presentation = EndpointAgentPresentation::default();
        let mut initial = snapshot(AgentStatus::Working, 4, 1);
        presentation.project_snapshot(&mut initial);
        let mut completed = snapshot(AgentStatus::Idle, 5, 2);
        presentation.project_snapshot(&mut completed);

        assert!(presentation.acknowledge_surface(&mut completed, &surface(2), Some(true)));
        assert_eq!(completed.agents[0].agent_status, AgentStatus::Idle);
    }

    #[test]
    fn clients_acknowledge_the_same_endpoint_completion_independently() {
        let mut viewing_client = EndpointAgentPresentation::default();
        let mut background_client = EndpointAgentPresentation::default();
        let mut initial_for_viewer = snapshot(AgentStatus::Working, 4, 1);
        let mut initial_for_background = initial_for_viewer.clone();
        viewing_client.project_snapshot(&mut initial_for_viewer);
        background_client.project_snapshot(&mut initial_for_background);
        let mut completed_for_viewer = snapshot(AgentStatus::Idle, 5, 2);
        let mut completed_for_background = completed_for_viewer.clone();
        viewing_client.project_snapshot(&mut completed_for_viewer);
        background_client.project_snapshot(&mut completed_for_background);

        assert!(viewing_client.acknowledge_surface(
            &mut completed_for_viewer,
            &surface(2),
            Some(true)
        ));

        assert_eq!(
            completed_for_viewer.agents[0].agent_status,
            AgentStatus::Idle
        );
        assert_eq!(
            completed_for_background.agents[0].agent_status,
            AgentStatus::Done
        );
    }

    #[test]
    fn stale_or_unfocused_surface_does_not_acknowledge_completion() {
        let mut presentation = EndpointAgentPresentation::default();
        let mut initial = snapshot(AgentStatus::Working, 4, 1);
        presentation.project_snapshot(&mut initial);
        let mut completed = snapshot(AgentStatus::Idle, 5, 2);
        presentation.project_snapshot(&mut completed);

        assert!(!presentation.acknowledge_surface(&mut completed, &surface(1), Some(true)));
        assert!(!presentation.acknowledge_surface(&mut completed, &surface(2), Some(false)));
        assert_eq!(completed.agents[0].agent_status, AgentStatus::Done);
    }
}
