use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::api::schema::{WorkspaceTransferInfo, WorkspaceTransferParams, WorkspaceTransferState};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Route {
    pub transfer_id: String,
    pub destination_session: String,
    pub origin_workspace: String,
    pub destination_workspace: String,
    pub identities: BTreeMap<String, String>,
    #[serde(default)]
    pub canonical_inverse: BTreeMap<String, String>,
    #[serde(default)]
    pub terminals: BTreeMap<String, String>,
    pub names: BTreeMap<String, String>,
    #[serde(default)]
    pub pending_names: std::collections::BTreeSet<String>,
    pub ready: bool,
}

impl Route {
    pub(crate) fn terminal_for_name(&self, name: &str) -> Option<&str> {
        let pane = self.names.get(name)?;
        self.terminals
            .iter()
            .find_map(|(terminal, target)| (target == pane).then_some(terminal.as_str()))
    }

    #[cfg(unix)]
    pub(crate) fn conflicts(&self, other: &Self) -> bool {
        self.identities.iter().any(|(id, destination)| {
            other.identities.get(id).is_some_and(|existing| {
                !(existing == id
                    && destination == id
                    && self.terminals.contains_key(id)
                    && other.terminals.contains_key(id))
            })
        }) || self.names.keys().any(|name| {
            other.names.contains_key(name)
                && (self.terminal_for_name(name).is_none()
                    || self.terminal_for_name(name) != other.terminal_for_name(name))
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum SourcePhase {
    Preparing,
    CommitDecided,
    Committed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SourceRecord {
    pub params: WorkspaceTransferParams,
    pub info: WorkspaceTransferInfo,
    pub phase: SourcePhase,
    pub route: Option<Route>,
    #[serde(default)]
    pub source_identities: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub source_names: BTreeMap<String, String>,
    #[serde(default)]
    pub runtime_released: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum ReceivePhase {
    Prepared,
    Activating,
    Owned,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReceiveRecord {
    pub source_session: String,
    pub source_workspace: String,
    pub digest: String,
    pub phase: ReceivePhase,
    pub info: WorkspaceTransferInfo,
    pub route: Route,
    #[serde(default)]
    pub owner_boot_id: Option<String>,
    #[serde(default)]
    pub forwarding_depth: u8,
    #[serde(default)]
    pub settled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Journal {
    #[serde(default)]
    pub sources: BTreeMap<String, SourceRecord>,
    #[serde(default)]
    pub receivers: BTreeMap<String, ReceiveRecord>,
    #[serde(default)]
    pub routes: BTreeMap<String, Route>,
    #[serde(default)]
    pub terminal_routes: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub name_routes: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub reserved_workspaces: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub aborted_receivers: BTreeMap<String, String>,
    #[serde(skip)]
    pub live_names: BTreeMap<String, String>,
    #[serde(skip)]
    pub live_terminals: std::collections::HashSet<String>,
    #[serde(skip)]
    updates_suspended: bool,
}

impl Journal {
    #[cfg(unix)]
    pub(crate) fn install_route(&mut self, route: Route) {
        for terminal in route.terminals.keys() {
            let stack = self.terminal_routes.entry(terminal.clone()).or_default();
            if stack.last() != Some(&route.transfer_id) {
                stack.push(route.transfer_id.clone());
            }
        }
        for name in route.names.keys() {
            let stack = self.name_routes.entry(name.clone()).or_default();
            if stack.last() != Some(&route.transfer_id) {
                stack.push(route.transfer_id.clone());
            }
        }
        self.routes.insert(route.transfer_id.clone(), route);
    }

    #[cfg(unix)]
    pub(crate) fn remove_route(&mut self, id: &str) {
        self.routes.remove(id);
        for stack in self
            .terminal_routes
            .values_mut()
            .chain(self.name_routes.values_mut())
        {
            stack.retain(|route| route != id);
        }
        self.terminal_routes.retain(|_, stack| !stack.is_empty());
        self.name_routes.retain(|_, stack| !stack.is_empty());
    }
}

#[derive(Clone)]
pub(crate) struct TransferRegistry {
    path: Option<PathBuf>,
    state: Arc<Mutex<Journal>>,
}

impl TransferRegistry {
    pub(crate) fn open(path: PathBuf) -> io::Result<Self> {
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Journal::default(),
            Err(error) => return Err(error),
        };
        let registry = Self {
            path: Some(path),
            state: Arc::new(Mutex::new(state)),
        };
        let journal = registry.snapshot()?;
        for id in &journal.reserved_workspaces {
            crate::workspace::reserve_workspace_id(id);
        }
        for source in journal.sources.values() {
            for id in &source.source_identities {
                if let Some(raw) = id
                    .strip_prefix("p_")
                    .and_then(|raw| raw.parse::<u32>().ok())
                {
                    crate::layout::PaneId::reserve(raw);
                }
                let workspace = id
                    .split_once(':')
                    .map_or(id.as_str(), |(workspace, _)| workspace);
                crate::workspace::reserve_workspace_id(workspace);
            }
        }
        for route in journal.routes.values() {
            for id in route.identities.keys() {
                if let Some(raw) = id
                    .strip_prefix("p_")
                    .and_then(|raw| raw.parse::<u32>().ok())
                {
                    crate::layout::PaneId::reserve(raw);
                }
            }
        }
        Ok(registry)
    }

    #[cfg(test)]
    pub(crate) fn memory() -> Self {
        Self {
            path: None,
            state: Arc::new(Mutex::new(Journal::default())),
        }
    }

    pub(crate) fn snapshot(&self) -> io::Result<Journal> {
        self.state
            .lock()
            .map(|journal| journal.clone())
            .map_err(|_| io::Error::other("workspace transfer journal is poisoned"))
    }

    pub(crate) fn read<T>(&self, read: impl FnOnce(&Journal) -> T) -> io::Result<T> {
        self.state
            .lock()
            .map(|journal| read(&journal))
            .map_err(|_| io::Error::other("workspace transfer journal is poisoned"))
    }

    pub(crate) fn sync_live_bindings(&self, state: &crate::app::AppState) -> io::Result<()> {
        let mut journal = self
            .state
            .lock()
            .map_err(|_| io::Error::other("workspace transfer journal is poisoned"))?;
        journal.live_names = state
            .terminals
            .values()
            .filter_map(|terminal| {
                terminal
                    .agent_name
                    .as_ref()
                    .map(|name| (name.clone(), terminal.id.to_string()))
            })
            .collect();
        journal.live_terminals = state.terminals.keys().map(ToString::to_string).collect();
        Ok(())
    }

    pub(crate) fn reserve_local_name(&self, name: &str, terminal_id: String) -> io::Result<()> {
        let mut journal = self
            .state
            .lock()
            .map_err(|_| io::Error::other("workspace transfer journal is poisoned"))?;
        if journal.routes.values().any(|route| {
            route
                .names
                .get(name)
                .is_some_and(|target| route.terminals.get(&terminal_id) != Some(target))
        }) || journal.receivers.values().any(|record| {
            record.phase == ReceivePhase::Prepared && record.route.names.contains_key(name)
        }) || journal.sources.values().any(|record| {
            matches!(
                record.phase,
                SourcePhase::Preparing | SourcePhase::CommitDecided
            ) && record
                .source_names
                .get(name)
                .is_some_and(|id| id != &terminal_id)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "agent name is reserved by a transferred workspace",
            ));
        }
        journal.live_names.insert(name.into(), terminal_id);
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn suspend_updates(&self, suspended: bool) -> io::Result<()> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| io::Error::other("workspace transfer journal is poisoned"))?;
        if !suspended && guard.updates_suspended {
            if let Some(path) = &self.path {
                let mut disk: Journal =
                    serde_json::from_slice(&std::fs::read(path)?).map_err(io::Error::other)?;
                disk.live_names = std::mem::take(&mut guard.live_names);
                disk.live_terminals = std::mem::take(&mut guard.live_terminals);
                for id in &disk.reserved_workspaces {
                    crate::workspace::reserve_workspace_id(id);
                }
                for route in disk.routes.values() {
                    for id in route.identities.keys() {
                        if let Some(raw) = id
                            .strip_prefix("p_")
                            .and_then(|raw| raw.parse::<u32>().ok())
                        {
                            crate::layout::PaneId::reserve(raw);
                        }
                    }
                }
                *guard = disk;
            }
        }
        guard.updates_suspended = suspended;
        Ok(())
    }

    pub(crate) fn update<T>(
        &self,
        change: impl FnOnce(&mut Journal) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| io::Error::other("workspace transfer journal is poisoned"))?;
        if guard.updates_suspended {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "transfer journal updates are paused for server handoff",
            ));
        }
        let mut next = guard.clone();
        let result = change(&mut next)?;
        if let Some(path) = &self.path {
            save(path, &next)?;
        }
        *guard = next;
        Ok(result)
    }

    #[cfg(unix)]
    pub(crate) fn recover(&self) -> io::Result<()> {
        self.update(|journal| {
            for record in journal.receivers.values_mut() {
                if record.phase == ReceivePhase::Prepared {
                    record.phase = ReceivePhase::Aborted;
                    record.info.state = WorkspaceTransferState::Failed;
                    record.info.error = Some("receiver restarted before ownership commit".into());
                }
            }
            for record in journal.sources.values_mut() {
                if record.phase == SourcePhase::Preparing {
                    record.info.error = Some("source restarted before ownership commit; original runtime ownership is being resolved".into());
                }
            }
            Ok(())
        })
    }

    pub(crate) fn suppress_exported_workspaces(
        &self,
        snapshot: &mut crate::persist::SessionSnapshot,
    ) -> io::Result<()> {
        let journal = self.snapshot()?;
        let mut exported: std::collections::HashSet<String> = journal
            .sources
            .values()
            .filter(|record| {
                matches!(
                    record.phase,
                    SourcePhase::Preparing | SourcePhase::CommitDecided | SourcePhase::Committed
                )
            })
            .map(|record| record.params.workspace_id.clone())
            .collect();
        for receiver in journal.receivers.values() {
            if receiver.phase == ReceivePhase::Owned && source_released_workspace(receiver) {
                continue;
            }
            if let Some(id) = &receiver.info.workspace_id {
                exported.insert(id.clone());
            }
        }
        let active_id = snapshot
            .active
            .and_then(|index| snapshot.workspaces.get(index))
            .and_then(|workspace| workspace.id.clone());
        let selected_id = snapshot
            .workspaces
            .get(snapshot.selected)
            .and_then(|workspace| workspace.id.clone());
        snapshot.workspaces.retain(|workspace| {
            !workspace
                .id
                .as_deref()
                .is_some_and(|id| exported.contains(id))
        });
        snapshot.active = active_id
            .and_then(|id| {
                snapshot
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id.as_deref() == Some(&id))
            })
            .or_else(|| (!snapshot.workspaces.is_empty()).then_some(0));
        snapshot.selected = selected_id
            .and_then(|id| {
                snapshot
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id.as_deref() == Some(&id))
            })
            .unwrap_or(0);
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn pending_workspaces(
        &self,
    ) -> io::Result<std::collections::HashMap<String, String>> {
        Ok(self
            .snapshot()?
            .sources
            .into_iter()
            .filter_map(|(id, record)| {
                matches!(
                    record.phase,
                    SourcePhase::Preparing | SourcePhase::CommitDecided
                )
                .then_some((record.params.workspace_id, id))
            })
            .collect())
    }
}

pub(crate) fn source_released_workspace(receiver: &ReceiveRecord) -> bool {
    if receiver.phase != ReceivePhase::Owned {
        return false;
    }
    if receiver.settled {
        return true;
    }
    let Ok(source) = crate::session::parse_target_name(&receiver.source_session) else {
        return false;
    };
    let path = crate::session::data_dir_for(source.as_deref()).join("workspace-transfers.json");
    source_released_workspace_from_path(receiver, &path)
}

fn source_released_workspace_from_path(receiver: &ReceiveRecord, path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(journal) = serde_json::from_slice::<Journal>(&bytes) else {
        return false;
    };
    journal
        .sources
        .get(&receiver.info.transfer_id)
        .is_some_and(|source| {
            source.runtime_released
                && matches!(
                    source.phase,
                    SourcePhase::CommitDecided | SourcePhase::Committed
                )
                && source.params.transfer_id == receiver.info.transfer_id
                && source.params.workspace_id == receiver.source_workspace
                && source.params.destination_session == receiver.info.destination_session
                && source.route.as_ref().is_some_and(|route| {
                    Some(&route.destination_workspace) == receiver.info.workspace_id.as_ref()
                })
        })
}

pub(crate) fn path() -> PathBuf {
    crate::session::data_dir().join("workspace-transfers.json")
}

fn save(path: &Path, journal: &Journal) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing journal directory"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = serde_json::to_writer(&mut file, journal)
        .map_err(io::Error::other)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result.and_then(|()| crate::platform::replace_file(&temporary, path)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    crate::platform::sync_parent_directory(parent)
}

#[cfg(test)]
#[path = "journal/tests.rs"]
mod ownership_tests;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) struct TestDirectory(PathBuf);

    impl TestDirectory {
        pub(super) fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "herdr-transfer-journal-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn failed_journal_write_does_not_publish_a_decision() {
        let dir = TestDirectory::new();
        let path = dir.path().join("journal.json");
        let registry = TransferRegistry::open(path.clone()).unwrap();
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(registry
            .update(|journal| {
                journal.reserved_workspaces.insert("wz".into());
                Ok(())
            })
            .is_err());
        assert!(registry.snapshot().unwrap().reserved_workspaces.is_empty());
    }

    #[test]
    fn reservations_survive_without_a_session_snapshot() {
        let dir = TestDirectory::new();
        let path = dir.path().join("journal.json");
        let registry = TransferRegistry::open(path.clone()).unwrap();
        registry
            .update(|journal| {
                journal.reserved_workspaces.insert("wzz".into());
                Ok(())
            })
            .unwrap();
        assert!(TransferRegistry::open(path)
            .unwrap()
            .snapshot()
            .unwrap()
            .reserved_workspaces
            .contains("wzz"));
    }
}
