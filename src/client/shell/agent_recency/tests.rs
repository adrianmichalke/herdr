use super::*;
use crate::{api::schema::AgentStatus, client::endpoint::ProfileId, protocol::ClientShellAgent};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestPaths(PathBuf);

impl TestPaths {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "herdr-agent-recency-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("test directory");
        Self(path)
    }

    fn preferences(&self) -> PathBuf {
        self.0.join("chrome.json")
    }

    fn recency(&self) -> PathBuf {
        self.preferences().with_extension("agent-recency.json")
    }
}

impl Drop for TestPaths {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn remote() -> ClientEndpointId {
    ClientEndpointId::Ssh(ProfileId::parse("0123456789abcdef0123456789abcdef").expect("profile ID"))
}

fn projected(boot: &str, agents: &[(&str, u64)]) -> ClientShellSnapshot {
    let mut snapshot = crate::client::shell::tests::snapshot();
    snapshot.boot_id = boot.into();
    snapshot.agents = agents
        .iter()
        .map(|(pane_id, sequence)| ClientShellAgent {
            pane_id: (*pane_id).into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some((*pane_id).into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: *sequence,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        })
        .collect();
    let pane = snapshot.panes[0].clone();
    snapshot.panes = agents
        .iter()
        .map(|(pane_id, _)| {
            let mut pane = pane.clone();
            pane.pane_id = (*pane_id).into();
            pane
        })
        .collect();
    snapshot
}

fn activity(agents: &[(&str, Option<u64>)]) -> ActivityProjection {
    agents
        .iter()
        .map(|(pane_id, sequence)| {
            (
                (*pane_id).into(),
                sequence.map(|sequence| AgentActivity {
                    sequence,
                    changed_at_ms: None,
                }),
            )
        })
        .collect()
}

fn rank(cache: &AgentRecencyCache, endpoint: &ClientEndpointId, pane: &str) -> u64 {
    cache.ranks(endpoint)[pane]
}

#[test]
fn restart_hydrates_all_endpoints_before_reverse_arrival_and_keeps_offline_high_water() {
    let paths = TestPaths::new();
    let now = Instant::now();
    let local = ClientEndpointId::Local;
    let remote = remote();
    let local_activity = activity(&[("pane_1", Some(900))]);
    let remote_activity = activity(&[("pane_1", Some(1))]);
    let mut cache = AgentRecencyCache::load(Some(&paths.preferences()));
    cache.observe(
        &local,
        &projected("local", &[("pane_1", 30)]),
        Some(&local_activity),
        now,
    );
    cache.observe(
        &remote,
        &projected("remote", &[("pane_1", 1)]),
        Some(&remote_activity),
        now,
    );
    let original_local = rank(&cache, &local, "pane_1");
    let original_remote = rank(&cache, &remote, "pane_1");
    assert!(original_remote > original_local);
    cache.flush().expect("save recency");
    drop(cache);

    let mut cache = AgentRecencyCache::load(Some(&paths.preferences()));
    assert_eq!(rank(&cache, &local, "pane_1"), original_local);
    assert_eq!(rank(&cache, &remote, "pane_1"), original_remote);
    cache.observe(
        &remote,
        &projected("remote-restarted", &[("pane_1", 500)]),
        Some(&remote_activity),
        now,
    );
    cache.observe(
        &local,
        &projected("local-restarted", &[("pane_1", 1)]),
        Some(&local_activity),
        now,
    );
    assert_eq!(rank(&cache, &local, "pane_1"), original_local);
    assert_eq!(rank(&cache, &remote, "pane_1"), original_remote);

    let new_activity = activity(&[("pane_1", Some(900)), ("pane_2", Some(901))]);
    cache.observe(
        &local,
        &projected("local-restarted", &[("pane_1", 1), ("pane_2", 2)]),
        Some(&new_activity),
        now,
    );
    assert!(rank(&cache, &local, "pane_2") > original_remote);
}

#[test]
fn genuine_modern_sequence_changes_move_rank_but_clock_and_legacy_counters_do_not() {
    let local = ClientEndpointId::Local;
    let now = Instant::now();
    let mut cache = AgentRecencyCache::load(None);
    let mut records = activity(&[("pane_1", Some(5))]);
    cache.observe(
        &local,
        &projected("boot", &[("pane_1", 10)]),
        Some(&records),
        now,
    );
    let first = rank(&cache, &local, "pane_1");
    records
        .get_mut("pane_1")
        .unwrap()
        .as_mut()
        .unwrap()
        .changed_at_ms = Some(u64::MAX);
    cache.observe(
        &local,
        &projected("boot", &[("pane_1", 11)]),
        Some(&records),
        now,
    );
    assert_eq!(rank(&cache, &local, "pane_1"), first);
    records
        .get_mut("pane_1")
        .unwrap()
        .as_mut()
        .unwrap()
        .sequence = 6;
    cache.observe(
        &local,
        &projected("another-boot", &[("pane_1", 1)]),
        Some(&records),
        now,
    );
    assert!(rank(&cache, &local, "pane_1") > first);
}

#[test]
fn timestamp_metadata_changes_do_not_dirty_persisted_observations() {
    let paths = TestPaths::new();
    let now = Instant::now();
    let local = ClientEndpointId::Local;
    let mut cache = AgentRecencyCache::load(Some(&paths.preferences()));
    let snapshot = projected("boot", &[("pane_1", 1)]);
    let mut records = activity(&[("pane_1", Some(5))]);
    cache.observe(&local, &snapshot, Some(&records), now);
    cache.flush().expect("save observation");
    let initial = rank(&cache, &local, "pane_1");
    records
        .get_mut("pane_1")
        .unwrap()
        .as_mut()
        .unwrap()
        .changed_at_ms = Some(123);
    cache.observe(&local, &snapshot, Some(&records), now + SAVE_DELAY);
    assert_eq!(rank(&cache, &local, "pane_1"), initial);
    assert!(cache.deadline().is_none());
}

#[test]
fn modern_empty_map_is_not_legacy_and_first_real_activity_moves_rank() {
    let now = Instant::now();
    let local = ClientEndpointId::Local;
    let mut cache = AgentRecencyCache::load(None);
    let empty = BTreeMap::new();
    cache.observe(
        &local,
        &projected("boot", &[("pane_1", 1)]),
        Some(&empty),
        now,
    );
    let initial = rank(&cache, &local, "pane_1");
    cache.observe(
        &local,
        &projected("boot", &[("pane_1", 10)]),
        Some(&empty),
        now,
    );
    assert_eq!(rank(&cache, &local, "pane_1"), initial);
    let records = activity(&[("pane_1", Some(1))]);
    cache.observe(
        &local,
        &projected("boot", &[("pane_1", 11)]),
        Some(&records),
        now,
    );
    let changed = rank(&cache, &local, "pane_1");
    assert!(changed > initial);
    cache.observe(
        &local,
        &projected("boot", &[("pane_1", 12)]),
        Some(&empty),
        now,
    );
    cache.observe(
        &local,
        &projected("boot", &[("pane_1", 13)]),
        Some(&records),
        now,
    );
    assert_eq!(rank(&cache, &local, "pane_1"), changed);
}

#[test]
fn explicit_missing_history_resets_the_observation_baseline_not_the_rank() {
    let local = ClientEndpointId::Local;
    let now = Instant::now();
    let mut cache = AgentRecencyCache::load(None);
    let snapshot = projected("boot", &[("pane_1", 7)]);
    let records = activity(&[("pane_1", Some(1))]);
    cache.observe(&local, &snapshot, Some(&records), now);
    let original = rank(&cache, &local, "pane_1");
    let reset = activity(&[("pane_1", None)]);
    cache.observe(&local, &snapshot, Some(&reset), now);
    assert_eq!(rank(&cache, &local, "pane_1"), original);
    cache.observe(&local, &snapshot, Some(&records), now);
    assert!(rank(&cache, &local, "pane_1") > original);
}

#[test]
fn absent_agents_keep_baselines_until_their_panes_are_deleted() {
    let local = ClientEndpointId::Local;
    let now = Instant::now();
    let mut cache = AgentRecencyCache::load(None);
    let records = activity(&[("pane_1", Some(5))]);
    let initial = projected("boot", &[("pane_1", 5)]);
    cache.observe(&local, &initial, Some(&records), now);
    let first = rank(&cache, &local, "pane_1");
    let mut absent = initial.clone();
    absent.agents.clear();
    cache.observe(&local, &absent, Some(&BTreeMap::new()), now);
    assert_eq!(rank(&cache, &local, "pane_1"), first);
    cache.observe(&local, &initial, Some(&records), now);
    assert_eq!(rank(&cache, &local, "pane_1"), first);
    absent.panes.clear();
    cache.observe(&local, &absent, Some(&BTreeMap::new()), now);
    assert!(cache.ranks(&local).is_empty());
    cache.observe(&local, &initial, Some(&records), now);
    assert!(rank(&cache, &local, "pane_1") > first);
}

#[test]
fn legacy_restart_requires_same_boot_evidence_and_keeps_absent_agents() {
    let paths = TestPaths::new();
    let local = ClientEndpointId::Local;
    let now = Instant::now();
    let mut cache = AgentRecencyCache::load(Some(&paths.preferences()));
    let snapshot = projected("boot", &[("pane_1", 5)]);
    cache.observe(&local, &snapshot, None, now);
    let initial = rank(&cache, &local, "pane_1");
    drop(cache);

    let mut cache = AgentRecencyCache::load(Some(&paths.preferences()));
    cache.observe(&local, &snapshot, None, now);
    assert_eq!(rank(&cache, &local, "pane_1"), initial);
    cache.observe(&local, &projected("boot", &[("pane_1", 6)]), None, now);
    let changed = rank(&cache, &local, "pane_1");
    assert!(changed > initial);
    cache.observe(
        &local,
        &projected("new-boot", &[("pane_1", 100)]),
        None,
        now,
    );
    assert_eq!(rank(&cache, &local, "pane_1"), changed);
    let mut absent = projected("new-boot", &[("pane_1", 100)]);
    absent.agents.clear();
    cache.observe(&local, &absent, None, now);
    cache.observe(
        &local,
        &projected("new-boot", &[("pane_1", 100)]),
        None,
        now,
    );
    assert_eq!(rank(&cache, &local, "pane_1"), changed);
    cache.observe(
        &local,
        &projected("new-boot", &[("pane_1", 101)]),
        None,
        now,
    );
    assert!(rank(&cache, &local, "pane_1") > changed);
}

#[test]
fn equal_sequences_keep_existing_observation_order_without_lexical_ids() {
    let now = Instant::now();
    let local = ClientEndpointId::Local;
    let mut cache = AgentRecencyCache::load(None);
    let mut snapshot = projected("boot", &[("ws_1:p2", 7), ("ws_1:p9", 7)]);
    snapshot.panes.reverse();
    snapshot.agent_order = vec!["ws_1:p2".into(), "ws_1:p9".into()];
    order_snapshot_agents(&mut snapshot);
    let records = activity(&[("ws_1:p2", Some(7)), ("ws_1:p9", Some(7))]);
    cache.observe(&local, &snapshot, Some(&records), now);
    assert!(rank(&cache, &local, "ws_1:p9") < rank(&cache, &local, "ws_1:p2"));
    let ranks = cache.ranks(&local);
    assert_eq!(
        crate::client::shell::agent_sidebar::ordered_agent_pane_ids(
            &snapshot,
            crate::config::AgentPanelSortConfig::Spaces,
            Some(&ranks)
        ),
        ["ws_1:p9", "ws_1:p2"]
    );
    assert_eq!(
        crate::client::shell::agent_sidebar::ordered_agent_pane_ids(
            &snapshot,
            crate::config::AgentPanelSortConfig::Priority,
            Some(&ranks)
        ),
        ["ws_1:p2", "ws_1:p9"]
    );
}

#[test]
fn grouped_layout_ignores_server_priority_but_explicit_views_keep_their_order() {
    let mut snapshot = projected("boot", &[("ws_1:p9", 1), ("ws_1:p2", 100)]);
    snapshot.agent_order = vec!["ws_1:p2".into(), "ws_1:p9".into()];
    snapshot.agents.reverse();
    order_snapshot_agents(&mut snapshot);
    assert_eq!(
        crate::client::shell::agent_sidebar::ordered_agent_pane_ids(
            &snapshot,
            crate::config::AgentPanelSortConfig::Spaces,
            None
        ),
        ["ws_1:p9", "ws_1:p2"]
    );
    snapshot.agent_view_label = Some("explicit".into());
    assert_eq!(
        crate::client::shell::agent_sidebar::ordered_agent_pane_ids(
            &snapshot,
            crate::config::AgentPanelSortConfig::Spaces,
            None
        ),
        ["ws_1:p2", "ws_1:p9"]
    );
}

#[test]
fn workspace_and_tab_layout_precede_pane_order_without_id_sorting() {
    let mut snapshot = projected("boot", &[("ws_2:p1", 1), ("ws_1:p9", 1), ("ws_1:p2", 1)]);
    let mut other_workspace = snapshot.workspaces[0].clone();
    other_workspace.workspace_id = "ws_2".into();
    snapshot.workspaces.push(other_workspace);
    let mut other_tab = snapshot.tabs[0].clone();
    other_tab.tab_id = "tab_2".into();
    snapshot.tabs.push(other_tab);
    snapshot.agents[0].workspace_id = "ws_2".into();
    snapshot.agents[1].tab_id = "tab_2".into();
    snapshot.agent_order = vec!["ws_2:p1".into(), "ws_1:p9".into(), "ws_1:p2".into()];
    order_snapshot_agents(&mut snapshot);
    assert_eq!(
        snapshot
            .agents
            .iter()
            .map(|agent| agent.pane_id.as_str())
            .collect::<Vec<_>>(),
        ["ws_1:p2", "ws_1:p9", "ws_2:p1"]
    );
}

#[test]
fn profile_deletion_prunes_only_confirmed_profiles_and_keeps_high_water() {
    let now = Instant::now();
    let local = ClientEndpointId::Local;
    let remote = remote();
    let mut cache = AgentRecencyCache::load(None);
    cache.observe(&local, &projected("local", &[("pane_1", 1)]), None, now);
    cache.observe(&remote, &projected("remote", &[("pane_1", 1)]), None, now);
    let high_water = cache.saved.high_water;
    cache.retain_endpoints([local.storage_key(), remote.storage_key()].into_iter(), now);
    assert!(!cache.ranks(&remote).is_empty());
    cache.retain_endpoints([local.storage_key()].into_iter(), now);
    assert!(cache.ranks(&remote).is_empty());
    assert!(!cache.ranks(&local).is_empty());
    assert_eq!(cache.saved.high_water, high_water);
}

#[test]
fn persistence_has_a_fixed_deadline_does_not_rewrite_clean_state_or_chrome_preferences() {
    let paths = TestPaths::new();
    let chrome = br#"{"sidebar_width":31,"collapsed_groups":["other-client"]}"#;
    std::fs::write(paths.preferences(), chrome).expect("chrome preferences");
    let now = Instant::now();
    let local = ClientEndpointId::Local;
    let mut cache = AgentRecencyCache::load(Some(&paths.preferences()));
    for sequence in 1..=20 {
        cache.observe(
            &local,
            &projected("boot", &[("pane_1", sequence)]),
            None,
            now + Duration::from_millis(sequence * 40),
        );
    }
    let deadline = now + Duration::from_millis(40) + SAVE_DELAY;
    assert_eq!(cache.deadline(), Some(deadline));
    cache.tick(deadline - Duration::from_millis(1));
    assert!(!paths.recency().exists());
    cache.tick(deadline);
    assert!(paths.recency().exists());
    assert_eq!(std::fs::read(paths.preferences()).unwrap(), chrome);
    assert!(cache.deadline().is_none());
    std::fs::remove_file(paths.recency()).expect("remove saved recency");
    cache.observe(
        &local,
        &projected("boot", &[("pane_1", 20)]),
        None,
        deadline,
    );
    cache.tick(deadline + SAVE_DELAY);
    assert!(!paths.recency().exists());
    cache.observe(
        &local,
        &projected("boot", &[("pane_1", 21)]),
        None,
        deadline + SAVE_DELAY,
    );
    drop(cache);
    let loaded = AgentRecencyCache::load(Some(&paths.preferences()));
    assert_eq!(rank(&loaded, &local, "pane_1"), 21);
}

#[test]
fn hydration_repairs_low_high_water_and_saturation_does_not_wrap() {
    let paths = TestPaths::new();
    let saved = serde_json::json!({
        "high_water": 1,
        "endpoints": {"local": {"pane_1": {
            "rank": u64::MAX,
            "activity_sequence": null,
            "legacy": null,
        }}},
    });
    std::fs::write(paths.recency(), serde_json::to_vec(&saved).unwrap())
        .expect("saved high-water state");
    let mut cache = AgentRecencyCache::load(Some(&paths.preferences()));
    assert_eq!(cache.saved.high_water, u64::MAX);
    cache.observe(
        &ClientEndpointId::Local,
        &projected("boot", &[("pane_1", 1), ("pane_2", 2)]),
        None,
        Instant::now(),
    );
    assert_eq!(rank(&cache, &ClientEndpointId::Local, "pane_2"), u64::MAX);
}

#[test]
fn malformed_or_missing_cache_uses_empty_baselines() {
    let paths = TestPaths::new();
    assert!(AgentRecencyCache::load(Some(&paths.preferences()))
        .saved
        .endpoints
        .is_empty());
    std::fs::write(paths.recency(), b"not json").expect("malformed state");
    assert!(AgentRecencyCache::load(Some(&paths.preferences()))
        .saved
        .endpoints
        .is_empty());
}
