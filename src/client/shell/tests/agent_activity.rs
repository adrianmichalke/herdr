use super::*;
use crate::{agent_activity::AgentActivity, protocol::endpoint::EndpointAgentCompletions};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

struct TestPreferences(PathBuf);

impl TestPreferences {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "herdr-shell-agent-activity-{}-{}.json",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        )))
    }

    fn config(&self) -> ClientShellConfig {
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(self.0.clone())
    }
}

impl Drop for TestPreferences {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("agent-recency.json"));
    }
}

fn projected(name: &str, boot: &str, sequence: u64) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.boot_id = boot.into();
    snapshot.agents = vec![ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some(name.into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Idle,
        state_change_seq: sequence,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    }];
    snapshot
}

fn update(
    state: &mut ClientShellState,
    endpoint: &ClientEndpointId,
    snapshot: ClientShellSnapshot,
    generation: u64,
    sequence: u64,
    timestamp: Option<u64>,
) {
    state.set_endpoint_agent_completions(
        endpoint,
        generation,
        EndpointAgentCompletions {
            boot_id: snapshot.boot_id.clone(),
            revision: snapshot.revision,
            completions: Default::default(),
            activity: Some(std::collections::BTreeMap::from([(
                "pane_1".into(),
                Some(AgentActivity {
                    sequence,
                    changed_at_ms: timestamp,
                }),
            )])),
        },
    );
    state.set_endpoint_snapshot_for_generation(endpoint, generation, Box::new(snapshot));
}

fn rank(state: &ClientShellState, endpoint: &ClientEndpointId) -> u64 {
    state
        .endpoints
        .iter()
        .find(|cached| &cached.endpoint_id == endpoint)
        .unwrap()
        .agent_recency["pane_1"]
}

fn ordered_names(state: &ClientShellState) -> Vec<&str> {
    aggregate_navigation::aggregate_agent_rows(
        &state.endpoints,
        &state.active_endpoint_id,
        crate::config::AgentPanelSortConfig::Priority,
    )
    .into_iter()
    .map(|row| row.agent.name.as_deref().unwrap())
    .collect()
}

#[test]
fn missing_activity_companions_do_not_promote_legacy_startup_counters() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let local = ClientEndpointId::Local;
    update(
        &mut state,
        &local,
        projected("local", "boot", 10),
        1,
        5,
        Some(1_000),
    );
    let original = rank(&state, &local);
    state.set_endpoint_snapshot_for_generation(&local, 1, Box::new(projected("local", "boot", 11)));
    assert_eq!(rank(&state, &local), original);
    let next = projected("local", "boot", 12);
    state.set_endpoint_agent_completions(
        &local,
        1,
        EndpointAgentCompletions {
            boot_id: next.boot_id.clone(),
            revision: next.revision,
            completions: Default::default(),
            activity: None,
        },
    );
    state.set_endpoint_snapshot_for_generation(&local, 1, Box::new(next));
    assert_eq!(rank(&state, &local), original);
    update(
        &mut state,
        &local,
        projected("local", "boot", 13),
        1,
        6,
        Some(2_000),
    );
    assert!(rank(&state, &local) > original);
}

#[test]
fn shell_restart_hydrates_remote_ranks_and_unchanged_reverse_arrival_preserves_order() {
    let preferences = TestPreferences::new();
    let profile = SavedSshEndpoint::new("Build", "dev@build.example", "agents").unwrap();
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    let local = ClientEndpointId::Local;
    let mut state = ClientShellState::new(preferences.config());
    state.set_endpoint_catalog(std::slice::from_ref(&profile));
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    update(
        &mut state,
        &local,
        projected("local", "local-boot", 90),
        1,
        900,
        Some(u64::MAX),
    );
    update(
        &mut state,
        &remote,
        projected("remote", "remote-boot", 1),
        1,
        1,
        Some(1),
    );
    let local_rank = rank(&state, &local);
    let remote_rank = rank(&state, &remote);
    assert!(remote_rank > local_rank);
    assert_eq!(ordered_names(&state), ["remote", "local"]);
    drop(state);

    let mut state = ClientShellState::new(preferences.config());
    assert_eq!(rank(&state, &local), local_rank);
    state.set_endpoint_catalog(&[profile]);
    assert_eq!(rank(&state, &remote), remote_rank);
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    update(
        &mut state,
        &remote,
        projected("remote", "remote-after-restart", 99),
        2,
        1,
        None,
    );
    update(
        &mut state,
        &local,
        projected("local", "local-after-restart", 1),
        2,
        900,
        None,
    );
    assert_eq!(rank(&state, &local), local_rank);
    assert_eq!(rank(&state, &remote), remote_rank);
    assert_eq!(ordered_names(&state), ["remote", "local"]);
    let mut changed = projected("local", "local-after-restart", 2);
    changed.revision = 2;
    update(&mut state, &local, changed, 2, 901, Some(0));
    assert!(rank(&state, &local) > remote_rank);
    assert_eq!(ordered_names(&state), ["local", "remote"]);
}

#[test]
fn shell_reconnect_and_agent_absence_do_not_move_durable_rank_or_prune_existing_pane() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let local = ClientEndpointId::Local;
    update(
        &mut state,
        &local,
        projected("local", "boot", 1),
        1,
        7,
        None,
    );
    let first = rank(&state, &local);
    let mut absent = projected("local", "boot", 1);
    absent.agents.clear();
    absent.revision = 2;
    state.set_endpoint_snapshot_for_generation(&local, 1, Box::new(absent));
    assert_eq!(rank(&state, &local), first);
    state.mark_endpoint_disconnected(&local);
    update(
        &mut state,
        &local,
        projected("local", "boot", 2),
        2,
        7,
        None,
    );
    assert_eq!(rank(&state, &local), first);
    let mut changed = projected("local", "boot", 3);
    changed.revision = 2;
    update(&mut state, &local, changed, 2, 8, None);
    assert!(rank(&state, &local) > first);
}

#[test]
fn status_priority_and_stale_precedence_still_override_client_recency() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let profile = SavedSshEndpoint::new("Build", "dev@build.example", "agents").unwrap();
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    let local = ClientEndpointId::Local;
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    let mut blocked = projected("remote", "remote", 1);
    blocked.agents[0].agent_status = AgentStatus::Blocked;
    update(&mut state, &remote, blocked, 1, 1, None);
    update(
        &mut state,
        &local,
        projected("local", "local", 1),
        1,
        9_000,
        None,
    );
    assert!(rank(&state, &local) > rank(&state, &remote));
    assert_eq!(ordered_names(&state), ["remote", "local"]);
    state.set_endpoint_status(&remote, ClientEndpointStatus::Reconnecting);
    assert_eq!(ordered_names(&state), ["local", "remote"]);
}

#[test]
fn local_agent_row_renders_cached_timestamp_and_missing_timestamp_has_no_separator() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.agents.rows = vec![vec![
        crate::config::AgentSidebarToken::Agent,
        crate::config::AgentSidebarToken::ActivityTime,
    ]];
    let mut state = ClientShellState::new(config);
    let local = ClientEndpointId::Local;
    let timestamp = 1_700_000_000_123;
    update(
        &mut state,
        &local,
        projected("pi", "boot-1", 1),
        1,
        1,
        Some(timestamp),
    );
    let snapshot = state.snapshot.as_deref().unwrap();
    let key = crate::config::AgentSidebarToken::ACTIVITY_TIME_CACHE_KEY;
    let expected = snapshot.agents[0]
        .tokens
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone());
    let row = agent_sidebar::agent_row(snapshot, "pane_1", &state.config, None).unwrap();
    for width in [0, 1, 4, 10, 40] {
        let area = Rect::new(0, 0, width, 1);
        let mut buffer = Buffer::empty(area);
        agent_sidebar::render_agent_row(&mut buffer, area, &row, &state.config);
        assert_eq!(buffer.content.len(), usize::from(width));
        if width == 40 {
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            if let Some(expected) = expected.as_ref() {
                assert!(text.contains(&format!("pi · {expected}")));
                let position = text.find(expected).unwrap();
                let column = text[..position].chars().count();
                assert!(buffer.content[column].modifier.contains(Modifier::DIM));
            } else {
                assert!(text.trim() == "pi");
            }
        }
    }
    let mut snapshot = projected("pi", "boot-1", 1);
    snapshot.revision = 2;
    update(&mut state, &local, snapshot, 1, 1, None);
    let snapshot = state.snapshot.as_deref().unwrap();
    let row = agent_sidebar::agent_row(snapshot, "pane_1", &state.config, None).unwrap();
    let area = Rect::new(0, 0, 40, 1);
    let mut buffer = Buffer::empty(area);
    agent_sidebar::render_agent_row(&mut buffer, area, &row, &state.config);
    let text = buffer
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert_eq!(text.trim(), "pi");
}
