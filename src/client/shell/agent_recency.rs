use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use super::{preferences, ClientEndpointId, ClientShellSnapshot, ClientShellState};
use crate::agent_activity::AgentActivity;

const SAVE_DELAY: Duration = Duration::from_secs(1);

type ActivityProjection = BTreeMap<String, Option<AgentActivity>>;

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct SavedRecency {
    high_water: u64,
    endpoints: BTreeMap<String, BTreeMap<String, AgentRecency>>,
}

#[derive(Default, Deserialize, Serialize)]
struct AgentRecency {
    rank: u64,
    activity_sequence: Option<u64>,
    #[serde(default)]
    activity_observed: bool,
    legacy: Option<LegacyObservation>,
}

#[derive(Deserialize, Serialize)]
struct LegacyObservation {
    boot_id: String,
    sequence: u64,
}

pub(super) struct AgentRecencyCache {
    path: Option<PathBuf>,
    saved: SavedRecency,
    deadline: Option<Instant>,
}

impl AgentRecencyCache {
    pub(super) fn load(preferences_path: Option<&Path>) -> Self {
        let path = preferences_path.map(|path| path.with_extension("agent-recency.json"));
        let mut saved: SavedRecency = path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|content| serde_json::from_slice(&content).ok())
            .unwrap_or_default();
        saved.high_water = saved
            .endpoints
            .values()
            .flat_map(|panes| panes.values().map(|record| record.rank))
            .max()
            .unwrap_or_default()
            .max(saved.high_water);
        Self {
            path,
            saved,
            deadline: None,
        }
    }

    pub(super) fn ranks(&self, endpoint_id: &ClientEndpointId) -> HashMap<String, u64> {
        self.saved
            .endpoints
            .get(&endpoint_id.storage_key())
            .into_iter()
            .flat_map(|panes| panes.iter())
            .map(|(pane_id, record)| (pane_id.clone(), record.rank))
            .collect()
    }

    pub(super) fn observe(
        &mut self,
        endpoint_id: &ClientEndpointId,
        snapshot: &ClientShellSnapshot,
        activity: Option<&ActivityProjection>,
        now: Instant,
    ) {
        let observed_sequence = |agent: &crate::protocol::ClientShellAgent| {
            activity.map_or(agent.state_change_seq, |activity| {
                activity
                    .get(&agent.pane_id)
                    .and_then(Option::as_ref)
                    .map_or(0, |record| record.sequence)
            })
        };
        let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
        agents.sort_by_key(|agent| observed_sequence(agent));
        let records = self
            .saved
            .endpoints
            .entry(endpoint_id.storage_key())
            .or_default();
        let mut dirty = false;
        for agent in agents {
            let modern_entry = activity.and_then(|activity| activity.get(&agent.pane_id));
            let modern_sequence = modern_entry
                .and_then(Option::as_ref)
                .map(|record| record.sequence);
            let changed = records.get(&agent.pane_id).is_none_or(|previous| {
                if activity.is_some() {
                    previous.activity_observed
                        && modern_sequence.is_some()
                        && previous.activity_sequence != modern_sequence
                } else {
                    previous.legacy.as_ref().is_some_and(|previous| {
                        previous.boot_id == snapshot.boot_id
                            && previous.sequence != agent.state_change_seq
                    })
                }
            });
            let rank = changed.then(|| {
                self.saved.high_water = self.saved.high_water.saturating_add(1);
                self.saved.high_water
            });
            let record = records.entry(agent.pane_id.clone()).or_default();
            if let Some(rank) = rank {
                record.rank = rank;
                dirty = true;
            }
            if activity.is_some() && !record.activity_observed {
                record.activity_observed = true;
                dirty = true;
            }
            if modern_entry.is_some() && record.activity_sequence != modern_sequence {
                record.activity_sequence = modern_sequence;
                dirty = true;
            }
            if record.legacy.as_ref().is_none_or(|previous| {
                previous.boot_id != snapshot.boot_id || previous.sequence != agent.state_change_seq
            }) {
                record.legacy = Some(LegacyObservation {
                    boot_id: snapshot.boot_id.clone(),
                    sequence: agent.state_change_seq,
                });
                dirty = true;
            }
        }
        let panes = snapshot
            .panes
            .iter()
            .map(|pane| pane.pane_id.as_str())
            .chain(snapshot.agents.iter().map(|agent| agent.pane_id.as_str()))
            .collect::<HashSet<_>>();
        let previous_len = records.len();
        records.retain(|pane_id, _| panes.contains(pane_id.as_str()));
        dirty |= records.len() != previous_len;
        if dirty {
            self.mark_dirty(now);
        }
    }

    pub(super) fn retain_endpoints(
        &mut self,
        endpoints: impl Iterator<Item = String>,
        now: Instant,
    ) {
        let endpoints = endpoints.collect::<HashSet<_>>();
        let previous_len = self.saved.endpoints.len();
        self.saved
            .endpoints
            .retain(|endpoint_id, _| endpoints.contains(endpoint_id));
        if self.saved.endpoints.len() != previous_len {
            self.mark_dirty(now);
        }
    }

    fn mark_dirty(&mut self, now: Instant) {
        if self.path.is_some() {
            self.deadline.get_or_insert(now + SAVE_DELAY);
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.deadline.is_none() {
            return Ok(());
        }
        if let Some(path) = self.path.as_deref() {
            preferences::store(path, &self.saved)?;
        }
        self.deadline = None;
        Ok(())
    }

    fn tick(&mut self, now: Instant) {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            if let Err(error) = self.flush() {
                tracing::warn!(%error, "failed to persist agent recency");
                self.deadline = Some(now + SAVE_DELAY);
            }
        }
    }
}

impl Drop for AgentRecencyCache {
    fn drop(&mut self) {
        if let Err(error) = self.flush() {
            tracing::warn!(%error, "failed to persist agent recency on teardown");
        }
    }
}

impl ClientShellState {
    pub(crate) fn tick_agent_recency(&mut self, now: Instant) {
        self.agent_recency.tick(now);
    }

    pub(crate) fn flush_agent_recency(&mut self) {
        if let Err(error) = self.agent_recency.flush() {
            tracing::warn!(%error, "failed to persist agent recency on teardown");
        }
    }
}

pub(super) fn order_snapshot_agents(snapshot: &mut ClientShellSnapshot) {
    let workspace_order = snapshot
        .workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| (workspace.workspace_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let tab_order = snapshot
        .tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| (tab.tab_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let pane_order = snapshot
        .panes
        .iter()
        .map(|pane| &pane.pane_id)
        .chain(snapshot.agent_order.iter())
        .enumerate()
        .fold(HashMap::new(), |mut order, (index, pane_id)| {
            order.entry(pane_id.as_str()).or_insert(index);
            order
        });
    snapshot.agents.sort_by_key(|agent| {
        (
            workspace_order
                .get(agent.workspace_id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
            tab_order
                .get(agent.tab_id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
            pane_order
                .get(agent.pane_id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
        )
    });
}

#[cfg(test)]
mod tests;
