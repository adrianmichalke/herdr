use crate::agent_activity::{AgentActivity, SavedAgentActivity};
use crate::detect::AgentState;

use super::{EffectiveStateChange, TerminalState};

#[derive(Clone, Copy, Default)]
pub(crate) enum ActivityObservation {
    #[default]
    Other,
    ProcessDiscovery,
    Screen {
        has_agent: bool,
    },
    Reported,
}

pub(super) struct RestoredActivityBaseline {
    agent: String,
    state: AgentState,
}

impl TerminalState {
    pub(crate) fn capture_agent_activity(&self) -> Option<SavedAgentActivity> {
        if self.agent_activity.is_none()
            && self.activity_baseline.is_none()
            && !self.is_agent_terminal()
        {
            return None;
        }
        Some(SavedAgentActivity {
            activity: self.agent_activity.clone(),
            state: self
                .activity_baseline
                .as_ref()
                .map_or(self.state, |baseline| baseline.state),
            agent: self
                .activity_baseline
                .as_ref()
                .map(|baseline| baseline.agent.clone())
                .or_else(|| self.effective_agent_label().map(str::to_owned)),
        })
    }

    pub(crate) fn restore_agent_activity(
        &mut self,
        saved: Option<&SavedAgentActivity>,
        baseline: Option<(String, AgentState)>,
    ) {
        self.agent_activity = saved.and_then(|saved| saved.activity.clone());
        self.activity_baseline =
            baseline.map(|(agent, state)| RestoredActivityBaseline { agent, state });
    }

    pub(crate) fn observe_agent_activity(
        &mut self,
        observation: ActivityObservation,
        change: Option<&EffectiveStateChange>,
    ) -> bool {
        let changed = change.is_some_and(|change| change.previous_state != change.state);
        let Some(baseline) = self.activity_baseline.as_ref() else {
            return changed;
        };
        if matches!(
            observation,
            ActivityObservation::Screen { has_agent: false }
        ) {
            return false;
        }
        let actual = self.effective_agent_label();
        let expected = actual.is_some_and(|actual| {
            actual == baseline.agent
                || crate::detect::parse_agent_label(&baseline.agent).is_some_and(|expected| {
                    crate::detect::parse_agent_label(actual) == Some(expected)
                })
        });
        match observation {
            ActivityObservation::ProcessDiscovery if expected => {
                if self.state == AgentState::Unknown {
                    return false;
                }
                let state = baseline.state;
                self.activity_baseline = None;
                state != self.state
            }
            ActivityObservation::ProcessDiscovery if actual.is_none() => false,
            ActivityObservation::Screen { has_agent: true } | ActivityObservation::Reported
                if expected =>
            {
                let state = baseline.state;
                self.activity_baseline = None;
                state != self.state
            }
            _ => {
                if changed || !matches!(observation, ActivityObservation::Other) {
                    self.activity_baseline = None;
                }
                changed
            }
        }
    }

    pub(crate) fn record_agent_activity(&mut self, sequence: u64) {
        self.agent_activity = Some(AgentActivity::now(sequence));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Agent;
    use crate::terminal::TerminalId;

    fn restored(baseline: AgentState) -> TerminalState {
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        terminal.set_detected_state(Some(Agent::Pi), baseline);
        let saved = SavedAgentActivity {
            activity: Some(AgentActivity {
                sequence: 42,
                changed_at_ms: Some(1_000),
            }),
            state: AgentState::Working,
            agent: Some("pi".into()),
        };
        terminal.restore_agent_activity(Some(&saved), Some(("pi".into(), baseline)));
        terminal
    }

    fn screen(
        terminal: &mut TerminalState,
        state: AgentState,
    ) -> super::super::TerminalStateMutation {
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            state,
            false,
            false,
            false,
            false,
            std::time::Instant::now(),
        )
    }

    #[test]
    fn restored_process_discovery_and_idle_readiness_preserve_activity() {
        let mut terminal = restored(AgentState::Idle);
        let discovery =
            terminal.set_detected_agent_process_at(Agent::Pi, std::time::Instant::now());
        assert!(!terminal.observe_agent_activity(
            ActivityObservation::ProcessDiscovery,
            discovery.effective_state_change.as_ref()
        ));
        let readiness = screen(&mut terminal, AgentState::Idle);
        assert!(!terminal.observe_agent_activity(
            ActivityObservation::Screen { has_agent: true },
            readiness.effective_state_change.as_ref()
        ));
        assert_eq!(terminal.agent_activity.as_ref().unwrap().sequence, 42);
        assert_eq!(
            terminal.agent_activity.as_ref().unwrap().changed_at_ms,
            Some(1_000)
        );
        let working = screen(&mut terminal, AgentState::Working);
        assert!(terminal.observe_agent_activity(
            ActivityObservation::Screen { has_agent: true },
            working.effective_state_change.as_ref()
        ));
    }

    #[test]
    fn restored_first_work_or_block_is_real_activity() {
        for state in [AgentState::Working, AgentState::Blocked] {
            let mut terminal = restored(AgentState::Idle);
            let observation = screen(&mut terminal, state);
            assert!(terminal.observe_agent_activity(
                ActivityObservation::Screen { has_agent: true },
                observation.effective_state_change.as_ref()
            ));
        }
    }

    #[test]
    fn restored_hook_readiness_before_process_discovery_does_not_refresh_activity() {
        let mut terminal = restored(AgentState::Idle);
        let report = terminal
            .set_hook_authority_with_session_ref(
                "custom:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                None,
                Some(1),
            )
            .unwrap();
        assert!(!terminal.observe_agent_activity(
            ActivityObservation::Reported,
            report.effective_state_change.as_ref()
        ));
        let discovery =
            terminal.set_detected_agent_process_at(Agent::Pi, std::time::Instant::now());
        assert!(!terminal.observe_agent_activity(
            ActivityObservation::ProcessDiscovery,
            discovery.effective_state_change.as_ref()
        ));
        let report = terminal
            .set_hook_authority_with_session_ref(
                "custom:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                Some(2),
            )
            .unwrap();
        assert!(terminal.observe_agent_activity(
            ActivityObservation::Reported,
            report.effective_state_change.as_ref()
        ));
    }

    #[test]
    fn handoff_baseline_uses_live_state_without_manufacturing_completion() {
        let mut terminal = restored(AgentState::Working);
        let observation = screen(&mut terminal, AgentState::Working);
        assert!(!terminal.observe_agent_activity(
            ActivityObservation::Screen { has_agent: true },
            observation.effective_state_change.as_ref()
        ));
        assert!(terminal.last_agent_completion_seq.is_none());
        let completed = screen(&mut terminal, AgentState::Idle);
        assert!(terminal.observe_agent_activity(
            ActivityObservation::Screen { has_agent: true },
            completed.effective_state_change.as_ref()
        ));
    }

    #[test]
    fn discovery_that_activates_queued_work_or_block_is_real_activity() {
        for next_state in [AgentState::Working, AgentState::Blocked] {
            let mut terminal = restored(AgentState::Idle);
            let session = crate::agent_resume::AgentSessionRef::path(
                std::env::current_dir()
                    .unwrap()
                    .join("queued-activity.jsonl")
                    .display()
                    .to_string(),
            )
            .unwrap();
            let now = std::time::Instant::now();
            terminal.set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                Some(session.clone()),
                Some(1_000),
                now,
            );
            terminal.set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Idle,
                false,
                true,
                false,
                true,
                now + std::time::Duration::from_millis(1),
            );
            let missing = terminal.set_detected_state_with_screen_signals_at(
                None,
                AgentState::Unknown,
                false,
                false,
                false,
                false,
                now + std::time::Duration::from_millis(2),
            );
            assert!(!terminal.observe_agent_activity(
                ActivityObservation::Screen { has_agent: false },
                missing.effective_state_change.as_ref()
            ));
            terminal.set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                Some(session.clone()),
                Some(2_000),
                Some("resume".into()),
            );
            let queued = terminal.set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                next_state,
                None,
                Some(session.clone()),
                Some(2_001),
                now + std::time::Duration::from_millis(3),
            );
            assert!(queued.is_none());
            let discovery = terminal.set_detected_agent_process_at(
                Agent::Pi,
                now + std::time::Duration::from_millis(4),
            );
            assert_eq!(terminal.state, next_state);
            assert!(terminal.observe_agent_activity(
                ActivityObservation::ProcessDiscovery,
                discovery.effective_state_change.as_ref()
            ));
            let idle = terminal
                .set_hook_authority_at(
                    "herdr:pi".into(),
                    "pi".into(),
                    AgentState::Idle,
                    None,
                    Some(session),
                    Some(2_002),
                    now + std::time::Duration::from_millis(5),
                )
                .unwrap();
            assert!(terminal.observe_agent_activity(
                ActivityObservation::Reported,
                idle.effective_state_change.as_ref()
            ));
        }
    }

    #[test]
    fn metadata_and_same_state_reports_do_not_count_as_activity() {
        let mut terminal = restored(AgentState::Idle);
        assert!(!terminal.observe_agent_activity(ActivityObservation::Other, None));
        let same = screen(&mut terminal, AgentState::Idle);
        assert!(!terminal.observe_agent_activity(
            ActivityObservation::Screen { has_agent: true },
            same.effective_state_change.as_ref()
        ));
        assert!(!terminal.observe_agent_activity(ActivityObservation::Reported, None));
        let captured = terminal.capture_agent_activity().unwrap();
        assert_eq!(
            captured.activity.as_ref().unwrap().changed_at_ms,
            Some(1_000)
        );
        assert_eq!(captured.state, AgentState::Idle);
        terminal.clear_agent_runtime_identity_after_respawn();
        assert!(terminal.agent_activity.is_none());
    }
}
