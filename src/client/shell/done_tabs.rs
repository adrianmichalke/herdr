use std::{collections::BTreeSet, path::PathBuf};

use sha2::{Digest as _, Sha256};

use super::*;

#[derive(Debug)]
pub(super) struct DoneTabTarget {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) session: String,
    pub(super) boot_id: String,
    pub(super) tab_id: String,
    pub(super) done: bool,
}

pub(super) struct DoneTabPreferences {
    directory: Option<PathBuf>,
    local_scope: String,
}

impl DoneTabPreferences {
    pub(super) fn new(config: &ClientShellConfig) -> Self {
        Self {
            directory: config
                .preferences_path
                .as_deref()
                .and_then(|path| path.parent())
                .map(|path| path.join("done-tabs")),
            local_scope: config.root_remote_target.as_ref().map_or_else(
                || config.done_tabs_local_scope.clone(),
                |target| format!("remote:{target}"),
            ),
        }
    }

    fn path(&self, endpoint: &ClientShellEndpoint) -> Option<PathBuf> {
        let machine = if endpoint.endpoint_id.is_local() {
            self.local_scope.clone()
        } else {
            endpoint.endpoint_id.machine_id().storage_key()
        };
        let mut hash = Sha256::new();
        for part in [machine.as_str(), endpoint.session_name.as_str()] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        Some(
            self.directory
                .as_ref()?
                .join(format!("{:x}.json", hash.finalize())),
        )
    }

    pub(super) fn load(&self, endpoint: &ClientShellEndpoint) -> HashSet<String> {
        self.path(endpoint)
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn store(&self, endpoint: &ClientShellEndpoint) -> Result<(), String> {
        let Some(path) = self.path(endpoint) else {
            return Ok(());
        };
        preferences::store(&path, endpoint.done_tabs.iter().collect::<BTreeSet<_>>())
    }
}

impl ClientShellState {
    pub(super) fn active_done_tabs(&self) -> Option<&HashSet<String>> {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .map(|endpoint| &endpoint.done_tabs)
    }

    pub(super) fn done_tab_target(&self, tab_id: &str) -> Option<DoneTabTarget> {
        let endpoint = self.endpoints.iter().find(|endpoint| {
            endpoint.endpoint_id == self.active_endpoint_id
                && !endpoint.session_hidden
                && endpoint.status == ClientEndpointStatus::Online
        })?;
        let snapshot = endpoint.snapshot.as_deref()?;
        if self.snapshot.as_deref()?.boot_id != snapshot.boot_id
            || self.active_snapshot_generation != endpoint.snapshot_generation
        {
            return None;
        }
        snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)?;
        Some(DoneTabTarget {
            endpoint_id: endpoint.endpoint_id.clone(),
            session: endpoint.session_name.clone(),
            boot_id: snapshot.boot_id.clone(),
            tab_id: tab_id.to_owned(),
            done: endpoint.done_tabs.contains(tab_id),
        })
    }

    pub(super) fn set_tab_done(
        &mut self,
        target: DoneTabTarget,
        done: bool,
        outcome: &mut ClientShellInput,
    ) {
        let Some(index) = self.endpoints.iter().position(|endpoint| {
            endpoint.endpoint_id == target.endpoint_id
                && endpoint.session_name == target.session
                && !endpoint.session_hidden
                && endpoint.status == ClientEndpointStatus::Online
                && endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                    snapshot.boot_id == target.boot_id
                        && snapshot.tabs.iter().any(|tab| tab.tab_id == target.tab_id)
                })
        }) else {
            return;
        };
        let previous_size = self
            .last_composed_size
            .map(|(cols, rows)| self.surface_size(cols, rows));
        let changed = if done {
            self.endpoints[index].done_tabs.insert(target.tab_id)
        } else {
            self.endpoints[index].done_tabs.remove(&target.tab_id)
        };
        if changed {
            self.persist_done_tabs(index);
            if self
                .last_composed_size
                .map(|(cols, rows)| self.surface_size(cols, rows))
                != previous_size
            {
                self.invalidate_pane_surface();
                outcome.resize = true;
            }
            outcome.repaint = true;
        }
    }

    pub(super) fn prune_done_tabs(&mut self, index: usize, snapshot: &ClientShellSnapshot) {
        let endpoint = &mut self.endpoints[index];
        if endpoint.done_tabs.is_empty()
            || endpoint.snapshot.as_deref().is_some_and(|previous| {
                previous.boot_id == snapshot.boot_id
                    && previous
                        .tabs
                        .iter()
                        .map(|tab| &tab.tab_id)
                        .eq(snapshot.tabs.iter().map(|tab| &tab.tab_id))
            })
        {
            return;
        }
        let tabs = snapshot
            .tabs
            .iter()
            .map(|tab| tab.tab_id.as_str())
            .collect::<HashSet<_>>();
        let previous = endpoint.done_tabs.len();
        endpoint.done_tabs.retain(|tab| tabs.contains(tab.as_str()));
        if previous != endpoint.done_tabs.len() {
            self.persist_done_tabs(index);
        }
    }

    fn persist_done_tabs(&mut self, index: usize) {
        if let Err(error) = self.done_tab_preferences.store(&self.endpoints[index]) {
            self.set_endpoint_error(error);
        }
    }
}
