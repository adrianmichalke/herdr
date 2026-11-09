use std::collections::HashMap;
use std::path::PathBuf;

use crate::detect::{Agent, AgentState};
use crate::metadata_tokens::{MetadataTokensTransferState, TransferClock, TransferTimestamp};

use super::metadata::AgentMetadataTransferState;
use super::{
    AgentNameOwner, FullLifecycleHookSuppressionReason, HookAuthority, ManagedAgent,
    ManagedAgentPhase, PendingFullLifecycleHookReport, RecentAgentProcessExit,
    StaleFullLifecycleHookSession, SuppressedFullLifecycleHookReport, TerminalId, TerminalState,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TerminalTransferState {
    captured_at: TransferTimestamp,
    id: TerminalId,
    cwd: PathBuf,
    detected_agent: Option<Agent>,
    fallback_state: AgentState,
    fallback_visible_blocker: bool,
    fallback_observed_at: Option<TransferTimestamp>,
    hook_authority: Option<HookAuthorityTransferState>,
    agent_metadata: HashMap<String, AgentMetadataTransferState>,
    metadata_tokens: MetadataTokensTransferState,
    persisted_agent_session: Option<crate::agent_resume::PersistedAgentSession>,
    reported_resume: Option<crate::agent_resume::ReportedAgentResume>,
    reported_resume_revision: u64,
    terminal_title: Option<String>,
    manual_label: Option<String>,
    agent_name: Option<String>,
    agent_name_owner: Option<AgentNameOwner>,
    managed_agent: Option<ManagedAgentTransferState>,
    codex_prompt_ready: bool,
    managed_agent_launch_session: Option<crate::agent_resume::PersistedAgentSession>,
    hook_report_sequences: HashMap<String, u64>,
    suppressed_full_lifecycle_hook_reports: HashMap<String, SuppressedHookReportTransferState>,
    stale_full_lifecycle_hook_sessions: HashMap<String, Vec<StaleFullLifecycleHookSession>>,
    metadata_report_sequences: HashMap<String, u64>,
    metadata_report_agents: HashMap<String, Agent>,
    metadata_token_sequence_sources: std::collections::HashSet<String>,
    state: AgentState,
    last_agent_state_change_seq: Option<u64>,
    last_agent_completion_seq: Option<u64>,
    agent_activity: Option<ActivityTransferState>,
    activity_baseline: Option<super::super::activity::RestoredActivityBaseline>,
    revision: u64,
    launch_argv: Option<Vec<String>>,
    respawn_shell_on_exit: bool,
    recent_agent_process_exit: Option<RecentAgentProcessExitTransferState>,
    agent_process_acquisition_pending: bool,
    pending_agent_resume_plan: Option<crate::agent_resume::AgentResumePlan>,
    restore_error: Option<String>,
}

impl TerminalState {
    pub(crate) fn workspace_transfer_state(&self) -> Result<TerminalTransferState, String> {
        self.workspace_transfer_state_at(&TransferClock::now()?)
    }

    pub(crate) fn from_workspace_transfer_state(
        snapshot: TerminalTransferState,
    ) -> Result<Self, String> {
        Self::from_workspace_transfer_state_at(snapshot, &TransferClock::now()?)
    }

    fn workspace_transfer_state_at(
        &self,
        clock: &TransferClock,
    ) -> Result<TerminalTransferState, String> {
        if self.cwd.to_str().is_none() {
            return Err("terminal cwd is not representable as UTF-8 for workspace transfer".into());
        }
        self.validate_workspace_transfer_metadata()?;
        let Self {
            id,
            cwd,
            detected_agent,
            fallback_state,
            fallback_visible_blocker,
            fallback_observed_at,
            hook_authority,
            agent_metadata,
            metadata_tokens,
            persisted_agent_session,
            reported_resume,
            reported_resume_revision,
            terminal_title,
            manual_label,
            agent_name,
            agent_name_owner,
            managed_agent,
            codex_prompt_ready,
            managed_agent_launch_session,
            hook_report_sequences,
            suppressed_full_lifecycle_hook_reports,
            stale_full_lifecycle_hook_sessions,
            metadata_report_sequences,
            metadata_report_agents,
            metadata_token_sequence_sources,
            state,
            last_agent_state_change_seq,
            last_agent_completion_seq,
            agent_activity,
            activity_baseline,
            revision,
            launch_argv,
            respawn_shell_on_exit,
            recent_agent_process_exit,
            agent_process_acquisition_pending,
            pending_agent_resume_plan,
            restore_error,
        } = self;
        Ok(TerminalTransferState {
            captured_at: clock.captured_at()?,
            id: id.clone(),
            cwd: cwd.clone(),
            detected_agent: *detected_agent,
            fallback_state: *fallback_state,
            fallback_visible_blocker: *fallback_visible_blocker,
            fallback_observed_at: fallback_observed_at
                .map(|at| clock.encode(at))
                .transpose()?,
            hook_authority: hook_authority
                .as_ref()
                .map(|authority| HookAuthorityTransferState::capture(authority, clock))
                .transpose()?,
            agent_metadata: agent_metadata
                .iter()
                .map(|(source, metadata)| Ok((source.clone(), metadata.transfer_state(clock)?)))
                .collect::<Result<_, String>>()?,
            metadata_tokens: metadata_tokens.transfer_state(clock)?,
            persisted_agent_session: persisted_agent_session.clone(),
            reported_resume: reported_resume.clone(),
            reported_resume_revision: *reported_resume_revision,
            terminal_title: terminal_title.clone(),
            manual_label: manual_label.clone(),
            agent_name: agent_name.clone(),
            agent_name_owner: agent_name_owner.clone(),
            managed_agent: managed_agent
                .map(|managed| ManagedAgentTransferState::capture(managed, clock))
                .transpose()?,
            codex_prompt_ready: *codex_prompt_ready,
            managed_agent_launch_session: managed_agent_launch_session.clone(),
            hook_report_sequences: hook_report_sequences.clone(),
            suppressed_full_lifecycle_hook_reports: suppressed_full_lifecycle_hook_reports
                .iter()
                .map(|(source, report)| {
                    Ok((
                        source.clone(),
                        SuppressedHookReportTransferState::capture(report, clock)?,
                    ))
                })
                .collect::<Result<_, String>>()?,
            stale_full_lifecycle_hook_sessions: stale_full_lifecycle_hook_sessions.clone(),
            metadata_report_sequences: metadata_report_sequences.clone(),
            metadata_report_agents: metadata_report_agents.clone(),
            metadata_token_sequence_sources: metadata_token_sequence_sources.clone(),
            state: *state,
            last_agent_state_change_seq: *last_agent_state_change_seq,
            last_agent_completion_seq: *last_agent_completion_seq,
            agent_activity: agent_activity.as_ref().map(ActivityTransferState::from),
            activity_baseline: activity_baseline.clone(),
            revision: *revision,
            launch_argv: launch_argv.clone(),
            respawn_shell_on_exit: *respawn_shell_on_exit,
            recent_agent_process_exit: recent_agent_process_exit
                .map(|exit| {
                    Ok::<_, String>(RecentAgentProcessExitTransferState {
                        agent: exit.agent,
                        observed_at: clock.encode(exit.observed_at)?,
                    })
                })
                .transpose()?,
            agent_process_acquisition_pending: *agent_process_acquisition_pending,
            pending_agent_resume_plan: pending_agent_resume_plan.clone(),
            restore_error: restore_error.clone(),
        })
    }

    fn from_workspace_transfer_state_at(
        snapshot: TerminalTransferState,
        clock: &TransferClock,
    ) -> Result<Self, String> {
        let TerminalTransferState {
            captured_at,
            id,
            cwd,
            detected_agent,
            fallback_state,
            fallback_visible_blocker,
            fallback_observed_at,
            hook_authority,
            agent_metadata,
            metadata_tokens,
            persisted_agent_session,
            reported_resume,
            reported_resume_revision,
            terminal_title,
            manual_label,
            agent_name,
            agent_name_owner,
            managed_agent,
            codex_prompt_ready,
            managed_agent_launch_session,
            hook_report_sequences,
            suppressed_full_lifecycle_hook_reports,
            stale_full_lifecycle_hook_sessions,
            metadata_report_sequences,
            metadata_report_agents,
            metadata_token_sequence_sources,
            state,
            last_agent_state_change_seq,
            last_agent_completion_seq,
            agent_activity,
            activity_baseline,
            revision,
            launch_argv,
            respawn_shell_on_exit,
            recent_agent_process_exit,
            agent_process_acquisition_pending,
            pending_agent_resume_plan,
            restore_error,
        } = snapshot;
        clock.validate_capture(captured_at)?;
        let terminal = Self {
            id,
            cwd,
            detected_agent,
            fallback_state,
            fallback_visible_blocker,
            fallback_observed_at: fallback_observed_at
                .map(|at| clock.decode(at))
                .transpose()?,
            hook_authority: hook_authority
                .map(|authority| authority.restore(clock))
                .transpose()?,
            agent_metadata: agent_metadata
                .into_iter()
                .map(|(source, metadata)| Ok((source, metadata.restore(clock)?)))
                .collect::<Result<_, String>>()?,
            metadata_tokens: crate::metadata_tokens::MetadataTokens::from_transfer_state(
                metadata_tokens,
                clock,
            )?,
            persisted_agent_session,
            reported_resume,
            reported_resume_revision,
            terminal_title,
            manual_label,
            agent_name,
            agent_name_owner,
            managed_agent: managed_agent
                .map(|managed| managed.restore(clock))
                .transpose()?,
            codex_prompt_ready,
            managed_agent_launch_session,
            hook_report_sequences,
            suppressed_full_lifecycle_hook_reports: suppressed_full_lifecycle_hook_reports
                .into_iter()
                .map(|(source, report)| Ok((source, report.restore(clock)?)))
                .collect::<Result<_, String>>()?,
            stale_full_lifecycle_hook_sessions,
            metadata_report_sequences,
            metadata_report_agents,
            metadata_token_sequence_sources,
            state,
            last_agent_state_change_seq,
            last_agent_completion_seq,
            agent_activity: agent_activity.map(crate::agent_activity::AgentActivity::from),
            activity_baseline,
            revision,
            launch_argv,
            respawn_shell_on_exit,
            recent_agent_process_exit: recent_agent_process_exit
                .map(|exit| {
                    Ok::<_, String>(RecentAgentProcessExit {
                        agent: exit.agent,
                        observed_at: clock.decode(exit.observed_at)?,
                    })
                })
                .transpose()?,
            agent_process_acquisition_pending,
            pending_agent_resume_plan,
            restore_error,
        };
        terminal.validate_workspace_transfer_metadata()?;
        Ok(terminal)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ActivityTransferState {
    sequence: u64,
    changed_at_ms: Option<u64>,
}

impl From<&crate::agent_activity::AgentActivity> for ActivityTransferState {
    fn from(activity: &crate::agent_activity::AgentActivity) -> Self {
        let crate::agent_activity::AgentActivity {
            sequence,
            changed_at_ms,
        } = activity;
        Self {
            sequence: *sequence,
            changed_at_ms: *changed_at_ms,
        }
    }
}

impl From<ActivityTransferState> for crate::agent_activity::AgentActivity {
    fn from(activity: ActivityTransferState) -> Self {
        let ActivityTransferState {
            sequence,
            changed_at_ms,
        } = activity;
        Self {
            sequence,
            changed_at_ms,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HookAuthorityTransferState {
    source: String,
    agent_label: String,
    state: AgentState,
    message: Option<String>,
    reported_at: TransferTimestamp,
    session_ref: Option<crate::agent_resume::AgentSessionRef>,
}

impl HookAuthorityTransferState {
    fn capture(authority: &HookAuthority, clock: &TransferClock) -> Result<Self, String> {
        let HookAuthority {
            source,
            agent_label,
            state,
            message,
            reported_at,
            session_ref,
        } = authority;
        Ok(Self {
            source: source.clone(),
            agent_label: agent_label.clone(),
            state: *state,
            message: message.clone(),
            reported_at: clock.encode(*reported_at)?,
            session_ref: session_ref.clone(),
        })
    }

    fn restore(self, clock: &TransferClock) -> Result<HookAuthority, String> {
        let Self {
            source,
            agent_label,
            state,
            message,
            reported_at,
            session_ref,
        } = self;
        Ok(HookAuthority {
            source,
            agent_label,
            state,
            message,
            reported_at: clock.decode(reported_at)?,
            session_ref,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PendingHookReportTransferState {
    authority: HookAuthorityTransferState,
    seq: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SuppressedHookReportTransferState {
    agent_label: String,
    session_ref: Option<crate::agent_resume::AgentSessionRef>,
    observed_at: TransferTimestamp,
    reason: FullLifecycleHookSuppressionReason,
    replacement_session_ref: Option<crate::agent_resume::AgentSessionRef>,
    pending_replacement_report: Option<PendingHookReportTransferState>,
}

impl SuppressedHookReportTransferState {
    fn capture(
        report: &SuppressedFullLifecycleHookReport,
        clock: &TransferClock,
    ) -> Result<Self, String> {
        let SuppressedFullLifecycleHookReport {
            agent_label,
            session_ref,
            observed_at,
            reason,
            replacement_session_ref,
            pending_replacement_report,
        } = report;
        Ok(Self {
            agent_label: agent_label.clone(),
            session_ref: session_ref.clone(),
            observed_at: clock.encode(*observed_at)?,
            reason: *reason,
            replacement_session_ref: replacement_session_ref.clone(),
            pending_replacement_report: pending_replacement_report
                .as_ref()
                .map(|PendingFullLifecycleHookReport { authority, seq }| {
                    Ok::<_, String>(PendingHookReportTransferState {
                        authority: HookAuthorityTransferState::capture(authority, clock)?,
                        seq: *seq,
                    })
                })
                .transpose()?,
        })
    }

    fn restore(self, clock: &TransferClock) -> Result<SuppressedFullLifecycleHookReport, String> {
        let Self {
            agent_label,
            session_ref,
            observed_at,
            reason,
            replacement_session_ref,
            pending_replacement_report,
        } = self;
        Ok(SuppressedFullLifecycleHookReport {
            agent_label,
            session_ref,
            observed_at: clock.decode(observed_at)?,
            reason,
            replacement_session_ref,
            pending_replacement_report: pending_replacement_report
                .map(|PendingHookReportTransferState { authority, seq }| {
                    Ok::<_, String>(PendingFullLifecycleHookReport {
                        authority: authority.restore(clock)?,
                        seq,
                    })
                })
                .transpose()?,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum ManagedAgentPhaseTransferState {
    Pending {
        ready_after: Option<TransferTimestamp>,
        deadline: TransferTimestamp,
        observed_expected: bool,
    },
    Blocked,
    Active,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ManagedAgentTransferState {
    kind: Agent,
    phase: ManagedAgentPhaseTransferState,
}

impl ManagedAgentTransferState {
    fn capture(managed: ManagedAgent, clock: &TransferClock) -> Result<Self, String> {
        let ManagedAgent { kind, phase } = managed;
        Ok(Self {
            kind,
            phase: match phase {
                ManagedAgentPhase::Pending {
                    ready_after,
                    deadline,
                    observed_expected,
                } => ManagedAgentPhaseTransferState::Pending {
                    ready_after: ready_after.map(|at| clock.encode(at)).transpose()?,
                    deadline: clock.encode(deadline)?,
                    observed_expected,
                },
                ManagedAgentPhase::Blocked => ManagedAgentPhaseTransferState::Blocked,
                ManagedAgentPhase::Active => ManagedAgentPhaseTransferState::Active,
            },
        })
    }

    fn restore(self, clock: &TransferClock) -> Result<ManagedAgent, String> {
        let Self { kind, phase } = self;
        Ok(ManagedAgent {
            kind,
            phase: match phase {
                ManagedAgentPhaseTransferState::Pending {
                    ready_after,
                    deadline,
                    observed_expected,
                } => ManagedAgentPhase::Pending {
                    ready_after: ready_after.map(|at| clock.decode(at)).transpose()?,
                    deadline: clock.decode(deadline)?,
                    observed_expected,
                },
                ManagedAgentPhaseTransferState::Blocked => ManagedAgentPhase::Blocked,
                ManagedAgentPhaseTransferState::Active => ManagedAgentPhase::Active,
            },
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RecentAgentProcessExitTransferState {
    agent: Agent,
    observed_at: TransferTimestamp,
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use crate::agent_activity::{AgentActivity, SavedAgentActivity};
    use crate::agent_resume::{
        AgentResumePlan, AgentSessionRef, PersistedAgentSession, ReportedAgentResume,
    };
    use crate::terminal::{ActivityObservation, AgentMetadataReport};

    use super::*;

    fn wall() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn serialized(snapshot: &TerminalTransferState) -> TerminalTransferState {
        serde_json::from_slice(&serde_json::to_vec(snapshot).unwrap()).unwrap()
    }

    fn normalized(snapshot: &TerminalTransferState) -> serde_json::Value {
        let mut value = serde_json::to_value(snapshot).unwrap();
        value.as_object_mut().unwrap().remove("captured_at");
        value["metadata_tokens"]
            .as_object_mut()
            .unwrap()
            .remove("captured_at");
        value["metadata_token_sequence_sources"]
            .as_array_mut()
            .unwrap()
            .sort_by_key(|source| source.as_str().unwrap().to_string());
        value
    }

    fn metadata_report(source: &str, title: &str, seq: u64) -> AgentMetadataReport {
        AgentMetadataReport {
            source: source.into(),
            agent_label: None,
            applies_to_source: None,
            title: Some(title.into()),
            display_agent: Some("Display agent".into()),
            state_labels: HashMap::from([("working".into(), "Building".into())]),
            clear_title: false,
            clear_display_agent: false,
            clear_state_labels: false,
            ttl: Some(Duration::from_secs(60)),
            seq: Some(seq),
        }
    }

    fn populated_terminal(now: Instant) -> TerminalState {
        let session_ref = AgentSessionRef::id("active-session").unwrap();
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp/transfer cwd".into());
        terminal.set_agent_metadata(metadata_report("user:status", "Keep title", 61));
        terminal.detected_agent = Some(Agent::Claude);
        terminal.fallback_state = AgentState::Blocked;
        terminal.fallback_visible_blocker = true;
        terminal.fallback_observed_at = Some(now - Duration::from_secs(7));
        terminal.hook_authority = Some(HookAuthority {
            source: "herdr:claude".into(),
            agent_label: "claude".into(),
            state: AgentState::Working,
            message: Some("Keep authority message".into()),
            reported_at: now - Duration::from_secs(8),
            session_ref: Some(session_ref.clone()),
        });
        terminal.metadata_tokens.patch(
            HashMap::from([("summary".into(), Some("Keep token".into()))]),
            Some(Duration::from_secs(45)),
            now - Duration::from_secs(3),
        );
        terminal.persisted_agent_session = Some(PersistedAgentSession {
            source: "user:saved".into(),
            agent: "custom-agent".into(),
            session_ref: AgentSessionRef::id("persisted-session").unwrap(),
        });
        terminal.reported_resume = Some(ReportedAgentResume {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            argv: vec!["claude".into(), "--resume".into(), "active-session".into()],
        });
        terminal.reported_resume_revision = 73;
        terminal.terminal_title = Some("raw title\u{1b}[0m".into());
        terminal.manual_label = Some("manual label".into());
        terminal.agent_name = Some("Agent name".into());
        terminal.agent_name_owner = Some(AgentNameOwner {
            agent_label: "claude".into(),
            session_ref: Some(session_ref.clone()),
        });
        terminal.managed_agent = Some(ManagedAgent {
            kind: Agent::Claude,
            phase: ManagedAgentPhase::Pending {
                ready_after: Some(now + Duration::from_secs(5)),
                deadline: now + Duration::from_secs(20),
                observed_expected: true,
            },
        });
        terminal.codex_prompt_ready = true;
        terminal.managed_agent_launch_session = Some(PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref,
        });
        terminal.hook_report_sequences =
            HashMap::from([("herdr:claude".into(), 80), ("released-hook".into(), 999)]);
        for (source, reason) in [
            ("herdr:pi", FullLifecycleHookSuppressionReason::ProcessExit),
            ("herdr:omp", FullLifecycleHookSuppressionReason::HookClear),
        ] {
            terminal.suppressed_full_lifecycle_hook_reports.insert(
                source.into(),
                SuppressedFullLifecycleHookReport {
                    agent_label: "pi".into(),
                    session_ref: AgentSessionRef::path("/tmp/old-session.jsonl"),
                    observed_at: now - Duration::from_secs(4),
                    reason,
                    replacement_session_ref: AgentSessionRef::path("/tmp/new-session.jsonl"),
                    pending_replacement_report: Some(PendingFullLifecycleHookReport {
                        authority: HookAuthority {
                            source: source.into(),
                            agent_label: "pi".into(),
                            state: AgentState::Working,
                            message: Some("Buffered report".into()),
                            reported_at: now - Duration::from_secs(2),
                            session_ref: AgentSessionRef::path("/tmp/new-session.jsonl"),
                        },
                        seq: 1001,
                    }),
                },
            );
        }
        terminal.stale_full_lifecycle_hook_sessions.insert(
            "herdr:pi".into(),
            vec![StaleFullLifecycleHookSession {
                agent_label: "pi".into(),
                session_ref: AgentSessionRef::path("/tmp/stale-session.jsonl").unwrap(),
            }],
        );
        terminal
            .metadata_report_sequences
            .insert("released-metadata".into(), 501);
        terminal
            .metadata_report_agents
            .insert("released-metadata".into(), Agent::Codex);
        assert_eq!(
            terminal.accept_metadata_report("user:tokens", Some(601), true, Some(Agent::Claude)),
            Ok(true)
        );
        terminal.state = AgentState::Working;
        terminal.last_agent_state_change_seq = Some(901);
        terminal.last_agent_completion_seq = Some(900);
        terminal.restore_agent_activity(
            Some(&SavedAgentActivity {
                activity: Some(AgentActivity {
                    sequence: 899,
                    changed_at_ms: Some(123_456),
                }),
                state: AgentState::Idle,
                agent: Some("claude".into()),
            }),
            Some(("claude".into(), AgentState::Working)),
        );
        terminal.revision = 991;
        terminal.launch_argv = Some(vec!["claude".into(), "--resume".into()]);
        terminal.respawn_shell_on_exit = true;
        terminal.recent_agent_process_exit = Some(RecentAgentProcessExit {
            agent: Agent::Codex,
            observed_at: now - Duration::from_secs(3),
        });
        terminal.agent_process_acquisition_pending = true;
        terminal.pending_agent_resume_plan = Some(AgentResumePlan {
            agent: "claude".into(),
            argv: vec!["claude".into(), "--resume".into(), "pending-session".into()],
            dedupe_key: "preserved-dedupe-key".into(),
        });
        terminal.restore_error = Some("preserved restore error".into());
        terminal
    }

    #[test]
    fn workspace_transfer_roundtrips_every_pure_terminal_field() {
        let now = Instant::now();
        let terminal = populated_terminal(now);
        let source_clock = TransferClock::at(now, wall());
        let arrival = now + Duration::from_secs(700);
        let destination_clock = TransferClock::at(arrival, wall() + Duration::from_secs(2));
        let snapshot = terminal.workspace_transfer_state_at(&source_clock).unwrap();
        let expected = normalized(&snapshot);
        let mut restored = TerminalState::from_workspace_transfer_state_at(
            serialized(&snapshot),
            &destination_clock,
        )
        .unwrap();

        assert_eq!(restored.id, terminal.id);
        assert_eq!(restored.cwd, terminal.cwd);
        assert_eq!(restored.reported_resume_revision(), 73);
        assert_eq!(restored.agent_activity, terminal.agent_activity);
        assert_eq!(
            restored.fallback_observed_at,
            Some(arrival - Duration::from_secs(9))
        );
        assert_eq!(
            restored.hook_authority.as_ref().unwrap().reported_at,
            arrival - Duration::from_secs(10)
        );
        assert_eq!(
            normalized(
                &restored
                    .workspace_transfer_state_at(&destination_clock)
                    .unwrap()
            ),
            expected
        );
        assert!(!restored.observe_agent_activity(ActivityObservation::Reported, None));
        assert!(restored.activity_baseline.is_none());
        assert!(restored.agent_process_acquisition_pending);
        restored.reconcile_agent_name_owner("different-owner", None);
        assert_eq!(restored.agent_name, None);
    }

    #[test]
    fn workspace_transfer_binary_snapshot_preserves_optional_activity_time() {
        let now = Instant::now();
        let clock = TransferClock::at(now, wall());
        for changed_at_ms in [None, Some(123_456)] {
            let mut terminal = populated_terminal(now);
            terminal.agent_activity = Some(AgentActivity {
                sequence: 899,
                changed_at_ms,
            });
            let snapshot = terminal.workspace_transfer_state_at(&clock).unwrap();
            let bytes =
                bincode::serde::encode_to_vec(&snapshot, bincode::config::standard()).unwrap();
            let (snapshot, consumed): (TerminalTransferState, _) =
                bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
            assert_eq!(consumed, bytes.len());
            let restored =
                TerminalState::from_workspace_transfer_state_at(snapshot, &clock).unwrap();
            assert_eq!(restored.agent_activity, terminal.agent_activity);
            assert_eq!(
                normalized(&restored.workspace_transfer_state_at(&clock).unwrap()),
                normalized(&terminal.workspace_transfer_state_at(&clock).unwrap())
            );
        }
    }

    #[test]
    fn workspace_transfer_keeps_custom_and_official_hook_sequence_guards() {
        for (source, agent) in [
            ("user:custom", "custom-agent"),
            ("user:claude", "claude"),
            ("herdr:claude", "claude"),
        ] {
            let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
            let reported_at = Instant::now() - Duration::from_secs(5);
            terminal
                .set_hook_authority_at(
                    source.into(),
                    agent.into(),
                    AgentState::Working,
                    Some("Working message".into()),
                    AgentSessionRef::id("same-session"),
                    Some(80),
                    reported_at,
                )
                .unwrap();
            terminal.set_agent_name("Keep name".into());
            assert!(terminal.record_reported_resume(
                source,
                agent,
                Some(80),
                vec!["custom-agent".into(), "--continue".into()]
            ));
            terminal.set_agent_metadata(metadata_report("user:status", "Keep title", 60));
            assert_eq!(
                terminal.accept_metadata_report("user:tokens", Some(70), true, None),
                Ok(true)
            );
            let before = Instant::now();
            let mut restored = TerminalState::from_workspace_transfer_state(serialized(
                &terminal.workspace_transfer_state().unwrap(),
            ))
            .unwrap();

            assert_eq!(restored.id, terminal.id);
            assert_eq!(restored.hook_authority.as_ref().unwrap().source, source);
            assert!(restored.hook_authority.as_ref().unwrap().reported_at < before);
            assert_eq!(restored.reported_resume(), terminal.reported_resume());
            assert_eq!(restored.agent_name_owner, terminal.agent_name_owner);
            for stale in [None, Some(79), Some(80)] {
                assert!(!restored.hook_report_is_newer(source, stale));
                assert!(restored
                    .set_hook_authority(source.into(), agent.into(), AgentState::Idle, None, stale)
                    .is_none());
            }
            assert!(restored
                .set_agent_metadata(metadata_report("user:status", "Stale title", 60))
                .is_none());
            assert_eq!(restored.effective_title().as_deref(), Some("Keep title"));
            assert_eq!(
                restored.accept_metadata_report("user:tokens", Some(70), true, None),
                Ok(false)
            );
            assert_eq!(
                restored.accept_metadata_report("user:tokens", Some(71), true, None),
                Ok(true)
            );
            assert!(restored.hook_report_is_newer(source, Some(81)));
            assert!(restored
                .set_hook_authority(
                    source.into(),
                    agent.into(),
                    AgentState::Idle,
                    None,
                    Some(81)
                )
                .is_some());
        }
    }

    #[test]
    fn workspace_transfer_preserves_remaining_metadata_ttl_and_pending_expiry() {
        let now = Instant::now();
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        terminal.set_agent_metadata(metadata_report("user:status", "Expiring title", 5));
        let metadata = terminal.agent_metadata.get_mut("user:status").unwrap();
        metadata.reported_at = now - Duration::from_secs(7);
        metadata.ttl = Some(Duration::from_secs(10));
        terminal.metadata_tokens.patch(
            HashMap::from([("short".into(), Some("temporary".into()))]),
            Some(Duration::from_secs(10)),
            now - Duration::from_secs(7),
        );
        terminal.metadata_tokens.patch(
            HashMap::from([("expired".into(), Some("still pending sweep".into()))]),
            Some(Duration::from_secs(1)),
            now - Duration::from_secs(7),
        );
        terminal.metadata_tokens.patch(
            HashMap::from([("permanent".into(), Some("keep".into()))]),
            None,
            now,
        );
        let snapshot = serialized(
            &terminal
                .workspace_transfer_state_at(&TransferClock::at(now, wall()))
                .unwrap(),
        );
        let arrival = now + Duration::from_secs(500);
        let mut restored = TerminalState::from_workspace_transfer_state_at(
            snapshot.clone(),
            &TransferClock::at(arrival, wall() + Duration::from_secs(2)),
        )
        .unwrap();
        let deadline = arrival + Duration::from_secs(1);

        assert_eq!(restored.next_agent_metadata_expiry(), Some(deadline));
        assert_eq!(restored.metadata_tokens.values().len(), 3);
        assert!(restored.metadata_tokens.expire_at(arrival));
        assert_eq!(restored.metadata_tokens.next_expiry(), Some(deadline));
        assert!(!restored
            .metadata_tokens
            .expire_at(deadline - Duration::from_nanos(1)));
        assert!(restored.metadata_tokens.expire_at(deadline));
        assert_eq!(
            restored.metadata_tokens.values(),
            HashMap::from([("permanent".into(), "keep".into())])
        );
        let change = restored
            .expire_agent_metadata_at(deadline, deadline)
            .unwrap()
            .effective_state_change
            .unwrap();
        assert_eq!(
            change.previous_presentation.title.as_deref(),
            Some("Expiring title")
        );
        assert_eq!(change.presentation.title, None);

        let mut expired_in_transit = TerminalState::from_workspace_transfer_state_at(
            snapshot,
            &TransferClock::at(arrival, wall() + Duration::from_secs(4)),
        )
        .unwrap();
        let past_deadline = arrival - Duration::from_secs(1);
        assert!(expired_in_transit
            .agent_metadata
            .contains_key("user:status"));
        assert_eq!(
            expired_in_transit.next_agent_metadata_expiry(),
            Some(past_deadline)
        );
        assert_eq!(
            expired_in_transit
                .effective_presentation_for_state_at(expired_in_transit.state, arrival)
                .title,
            None
        );
        assert!(expired_in_transit
            .expire_agent_metadata_at(past_deadline, arrival)
            .unwrap()
            .effective_state_change
            .is_some());
    }

    #[test]
    fn workspace_transfer_preserves_all_managed_launch_phases() {
        let now = Instant::now();
        for phase in [
            ManagedAgentPhase::Pending {
                ready_after: Some(now + Duration::from_secs(5)),
                deadline: now + Duration::from_secs(20),
                observed_expected: true,
            },
            ManagedAgentPhase::Pending {
                ready_after: None,
                deadline: now + Duration::from_secs(20),
                observed_expected: false,
            },
            ManagedAgentPhase::Blocked,
            ManagedAgentPhase::Active,
        ] {
            let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
            terminal.managed_agent = Some(ManagedAgent {
                kind: Agent::Codex,
                phase,
            });
            terminal.codex_prompt_ready = true;
            terminal.agent_process_acquisition_pending = true;
            let clock = TransferClock::at(now, wall());
            let snapshot = terminal.workspace_transfer_state_at(&clock).unwrap();
            let restored =
                TerminalState::from_workspace_transfer_state_at(serialized(&snapshot), &clock)
                    .unwrap();
            assert_eq!(restored.managed_agent, terminal.managed_agent);
            assert_eq!(
                restored.next_managed_agent_deadline(),
                terminal.next_managed_agent_deadline()
            );
            assert!(restored.codex_prompt_ready);
            assert!(restored.agent_process_acquisition_pending);
        }
    }

    #[test]
    fn workspace_transfer_keeps_buffered_restart_reports_and_release_suppression() {
        let now = Instant::now();
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        let session_ref = AgentSessionRef::path("/tmp/workspace-transfer-pi.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            session_ref.clone(),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );
        assert!(terminal
            .set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                session_ref.clone(),
                Some(2001),
                now + Duration::from_millis(2)
            )
            .is_none());
        assert!(terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                session_ref,
                Some(2000),
                Some("startup".into())
            )
            .is_none());
        assert!(terminal.suppressed_full_lifecycle_hook_reports["herdr:pi"]
            .pending_replacement_report
            .is_some());
        let clock = TransferClock::at(now + Duration::from_millis(4), wall());
        let mut restored = TerminalState::from_workspace_transfer_state_at(
            serialized(&terminal.workspace_transfer_state_at(&clock).unwrap()),
            &clock,
        )
        .unwrap();
        assert!(!restored.full_lifecycle_hook_authority_active());

        for state in [&mut terminal, &mut restored] {
            state.set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Idle,
                false,
                true,
                false,
                false,
                now + Duration::from_millis(5),
            );
            assert!(state.full_lifecycle_hook_authority_active());
            assert_eq!(state.state, AgentState::Working);
            assert!(!state.hook_report_is_newer("herdr:pi", Some(2000)));
        }
        assert_eq!(terminal.hook_authority, restored.hook_authority);
        assert_eq!(
            terminal.hook_report_sequences,
            restored.hook_report_sequences
        );
        assert_eq!(
            terminal.stale_full_lifecycle_hook_sessions,
            restored.stale_full_lifecycle_hook_sessions
        );
    }

    #[test]
    fn workspace_transfer_keeps_hook_clear_and_stale_session_rejections() {
        let now = Instant::now();
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        let previous = AgentSessionRef::path("/tmp/previous-transfer-session.jsonl").unwrap();
        let stale = AgentSessionRef::path("/tmp/stale-transfer-session.jsonl").unwrap();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_persisted_agent_session(PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: previous.clone(),
        });
        terminal
            .set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                Some(previous.clone()),
                Some(10),
                now,
            )
            .unwrap();
        terminal
            .clear_hook_authority_with_mutation(Some("herdr:pi"), Some(11))
            .unwrap();
        terminal.remember_stale_full_lifecycle_hook_session(
            "herdr:pi".into(),
            "pi".into(),
            stale.clone(),
        );
        let clock = TransferClock::at(Instant::now(), wall());
        let mut restored = TerminalState::from_workspace_transfer_state_at(
            serialized(&terminal.workspace_transfer_state_at(&clock).unwrap()),
            &clock,
        )
        .unwrap();
        for state in [&mut terminal, &mut restored] {
            assert!(state
                .set_hook_authority_with_session_ref(
                    "herdr:pi".into(),
                    "pi".into(),
                    AgentState::Working,
                    None,
                    Some(previous.clone()),
                    Some(12)
                )
                .is_none());
            assert!(state
                .set_hook_authority_with_session_ref(
                    "herdr:pi".into(),
                    "pi".into(),
                    AgentState::Working,
                    None,
                    Some(stale.clone()),
                    Some(10_000)
                )
                .is_none());
            assert!(state.hook_authority.is_none());
            assert!(state
                .set_hook_authority_with_session_ref(
                    "herdr:pi".into(),
                    "pi".into(),
                    AgentState::Working,
                    None,
                    AgentSessionRef::path("/tmp/new-transfer-session.jsonl"),
                    Some(1)
                )
                .is_some());
            assert_eq!(state.hook_report_sequences["herdr:pi"], 1);
        }
        assert_eq!(
            terminal.stale_full_lifecycle_hook_sessions,
            restored.stale_full_lifecycle_hook_sessions
        );
    }

    #[test]
    fn workspace_transfer_import_rejects_invalid_metadata_and_precedence() {
        let now = Instant::now();
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        terminal.set_agent_metadata(metadata_report("user:status", "Keep title", 1));
        let clock = TransferClock::at(now, wall());
        let snapshot = terminal.workspace_transfer_state_at(&clock).unwrap();
        let mut invalid = serde_json::to_value(&snapshot).unwrap();
        invalid["agent_metadata"]["user:status"]["ttl"] =
            serde_json::to_value(Duration::MAX).unwrap();
        let invalid = serde_json::from_value(invalid).unwrap();
        assert!(
            TerminalState::from_workspace_transfer_state_at(invalid, &clock)
                .err()
                .unwrap()
                .contains("TTL")
        );
        for field in ["title", "display_agent", "state_labels"] {
            let mut invalid = serde_json::to_value(&snapshot).unwrap();
            let mut tied = invalid["agent_metadata"]["user:status"].clone();
            tied["source"] = serde_json::json!("user:tied");
            tied[field] = if field == "state_labels" {
                serde_json::json!({"working": "Different state label"})
            } else {
                serde_json::json!("Different value")
            };
            invalid["agent_metadata"]["user:tied"] = tied;
            let invalid = serde_json::from_value(invalid).unwrap();
            assert!(
                TerminalState::from_workspace_transfer_state_at(invalid, &clock)
                    .err()
                    .unwrap()
                    .contains("ambiguous")
            );
        }
    }

    #[test]
    fn workspace_transfer_rejects_invalid_clock_ranges_and_ttls() {
        let now = Instant::now();
        let source_clock = TransferClock::at(now, wall());
        let terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        let snapshot = terminal.workspace_transfer_state_at(&source_clock).unwrap();
        let backwards = TransferClock::at(
            now + Duration::from_secs(1),
            wall() - Duration::from_nanos(1),
        );
        assert!(
            TerminalState::from_workspace_transfer_state_at(snapshot.clone(), &backwards)
                .err()
                .unwrap()
                .contains("backwards")
        );
        for timestamp in [
            serde_json::json!({"seconds": 1_800_000_000, "nanoseconds": 1_000_000_000}),
            serde_json::json!({"seconds": u64::MAX, "nanoseconds": 0}),
            serde_json::json!({"seconds": 5_000_000_000_u64, "nanoseconds": 0}),
        ] {
            let mut invalid = snapshot.clone();
            invalid.fallback_observed_at = Some(serde_json::from_value(timestamp).unwrap());
            assert!(
                TerminalState::from_workspace_transfer_state_at(invalid, &source_clock).is_err()
            );
        }
        let mut terminal = terminal;
        terminal.set_agent_metadata(metadata_report("user:status", "Invalid TTL", 1));
        terminal.agent_metadata.get_mut("user:status").unwrap().ttl = Some(Duration::MAX);
        assert!(terminal
            .workspace_transfer_state()
            .unwrap_err()
            .contains("TTL"));
    }

    #[test]
    fn workspace_transfer_rejects_ambiguous_metadata_precedence() {
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        terminal.set_agent_metadata(metadata_report("user:first", "First", 1));
        let mut second = terminal.agent_metadata["user:first"].clone();
        second.source = "user:second".into();
        second.title = Some("Different title at the same instant".into());
        terminal
            .agent_metadata
            .insert(second.source.clone(), second);
        assert!(terminal
            .workspace_transfer_state()
            .unwrap_err()
            .contains("ambiguous"));
        terminal
            .agent_metadata
            .get_mut("user:first")
            .unwrap()
            .agent_label = Some("claude".into());
        terminal
            .agent_metadata
            .get_mut("user:second")
            .unwrap()
            .agent_label = Some("codex".into());
        assert!(terminal.workspace_transfer_state().is_ok());
    }
}
