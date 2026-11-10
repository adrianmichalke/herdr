use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::api::schema::{AgentSessionInfo, EmptyParams, Method, Request, ResponseResult};
use crate::session::SessionInfo;

use super::parser::sanitize;
use super::types::{HistoryEvent, HistoryLocation, HistoryRequest, HistoryScope, HistorySource};

const MAX_SESSIONS: usize = 128;
const MAX_LOCATIONS: usize = 1_024;
const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
const METADATA_TIMEOUT: Duration = Duration::from_secs(3);
const CATALOG_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) struct CatalogSource {
    pub source: HistorySource,
    pub path: PathBuf,
}

#[derive(Default)]
pub(super) struct Catalog {
    pub sources: BTreeMap<String, CatalogSource>,
    pub truncated: bool,
    locations: usize,
}

impl Catalog {
    pub fn collect(
        request: &HistoryRequest,
        emit: &mut dyn FnMut(HistoryEvent) -> io::Result<()>,
    ) -> io::Result<Self> {
        let deadline = Instant::now() + CATALOG_TIMEOUT;
        let (names, mut discovery_limited) = if request.scope == HistoryScope::All {
            session_names(&crate::config::config_dir().join("sessions"))?
        } else {
            (vec![request.session.clone()], false)
        };
        let mut sessions = Vec::new();
        for name in names {
            if Instant::now() >= deadline {
                discovery_limited = true;
                break;
            }
            sessions.push(session_info(&name, deadline)?);
        }
        let mut catalog = Self::from_sessions_until(
            request,
            sessions,
            |session| read_live_snapshot_until(session, deadline),
            emit,
            deadline,
        )?;
        if discovery_limited {
            catalog.unavailable(
                vec![],
                "Session discovery limit reached; not all saved sessions were inspected".into(),
                emit,
            )?;
        }
        Ok(catalog)
    }

    #[cfg(test)]
    fn from_sessions(
        request: &HistoryRequest,
        sessions: Vec<SessionInfo>,
        live: impl FnMut(&SessionInfo) -> io::Result<crate::api::schema::SessionSnapshot>,
        emit: &mut dyn FnMut(HistoryEvent) -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::from_sessions_until(
            request,
            sessions,
            live,
            emit,
            Instant::now() + CATALOG_TIMEOUT,
        )
    }

    fn from_sessions_until(
        request: &HistoryRequest,
        sessions: Vec<SessionInfo>,
        mut live: impl FnMut(&SessionInfo) -> io::Result<crate::api::schema::SessionSnapshot>,
        emit: &mut dyn FnMut(HistoryEvent) -> io::Result<()>,
        deadline: Instant,
    ) -> io::Result<Self> {
        let mut catalog = Self::default();
        if sessions.len() > MAX_SESSIONS {
            catalog.unavailable(
                vec![],
                "Session discovery limit reached; not all saved sessions were inspected".into(),
                emit,
            )?;
        }
        for session in sessions.into_iter().take(MAX_SESSIONS) {
            if catalog.locations >= MAX_LOCATIONS {
                break;
            }
            if request.scope != HistoryScope::All && session.name != request.session {
                continue;
            }
            if Instant::now() >= deadline {
                catalog.unavailable(
                    vec![],
                    format!(
                        "Session {}: metadata discovery time limit reached",
                        sanitize(&session.name, 80)
                    ),
                    emit,
                )?;
                continue;
            }
            if let Some(error) = session.connection_error.as_ref() {
                catalog.unavailable(
                    vec![],
                    format!(
                        "Session {}: live metadata unavailable: {}",
                        session.name,
                        sanitize(error, 256)
                    ),
                    emit,
                )?;
                continue;
            }
            if session.running {
                match live(&session) {
                    Ok(snapshot) => catalog.add_live(request, &session.name, snapshot, emit)?,
                    Err(error) => catalog.unavailable(vec![], format!(
                        "Session {}: live history metadata unavailable (requires session.history_snapshot): {}",
                        session.name, sanitize(&error.to_string(), 256),
                    ), emit)?,
                }
            } else {
                match crate::persist::load_from_path(
                    &Path::new(&session.session_dir).join("session.json"),
                ) {
                    Ok(snapshot) => catalog.add_saved(request, &session.name, snapshot, emit)?,
                    Err(error) => catalog.unavailable(
                        vec![],
                        format!(
                            "Session {}: saved metadata unavailable: {}",
                            session.name,
                            sanitize(&error.to_string(), 256)
                        ),
                        emit,
                    )?,
                }
            }
        }
        Ok(catalog)
    }

    fn add_live(
        &mut self,
        request: &HistoryRequest,
        session: &str,
        snapshot: crate::api::schema::SessionSnapshot,
        emit: &mut dyn FnMut(HistoryEvent) -> io::Result<()>,
    ) -> io::Result<()> {
        for pane in snapshot.panes {
            if self.locations >= MAX_LOCATIONS {
                break;
            }
            if request.scope == HistoryScope::Space
                && request.workspace_id.as_deref() != Some(pane.workspace_id.as_str())
            {
                continue;
            }
            let Some(agent_session) = pane.agent_session else {
                if pane.agent.as_deref() == Some("pi") && self.claim_location(emit)? {
                    self.unavailable(vec![], format!("Session {session}, pane {}: Pi has not reported a native session reference", sanitize(&pane.pane_id, 256)), emit)?;
                }
                continue;
            };
            if !is_pi(&agent_session) {
                continue;
            }
            let workspace_label = snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == pane.workspace_id)
                .map(|workspace| sanitize(&workspace.label, 256))
                .unwrap_or_default();
            let tab_label = snapshot
                .tabs
                .iter()
                .find(|tab| tab.tab_id == pane.tab_id)
                .map(|tab| sanitize(&tab.label, 256))
                .unwrap_or_default();
            self.add_location(
                HistoryLocation {
                    session: session.to_owned(),
                    workspace_id: pane.workspace_id,
                    workspace_label,
                    tab_id: pane.tab_id,
                    tab_label,
                    pane_id: pane.pane_id,
                    pane_label: pane.label.as_deref().map(|label| sanitize(label, 256)),
                    terminal_id: Some(pane.terminal_id),
                    running: true,
                    agent_session,
                },
                emit,
            )?;
            if self.locations >= MAX_LOCATIONS {
                break;
            }
        }
        Ok(())
    }

    fn add_saved(
        &mut self,
        request: &HistoryRequest,
        session: &str,
        snapshot: crate::persist::SessionSnapshot,
        emit: &mut dyn FnMut(HistoryEvent) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut workspace_counts = HashMap::new();
        for workspace in &snapshot.workspaces {
            if let Some(id) = &workspace.id {
                *workspace_counts.entry(id.as_str()).or_insert(0usize) += 1;
            }
        }
        for workspace in &snapshot.workspaces {
            if request.scope == HistoryScope::Space && workspace.id.is_none() {
                self.unavailable(vec![], format!("Session {session}: saved workspace identity is unavailable for current-space history"), emit)?;
                continue;
            }
            if request.scope == HistoryScope::Space
                && request.workspace_id.as_deref() != workspace.id.as_deref()
            {
                continue;
            }
            let workspace_label = sanitize(
                workspace
                    .custom_name
                    .as_deref()
                    .unwrap_or_else(|| workspace.identity_cwd.to_str().unwrap_or_default()),
                256,
            );
            let mut pane_counts = HashMap::new();
            let mut tab_counts = HashMap::new();
            let mut number_counts = HashMap::new();
            for (index, tab) in workspace.tabs.iter().enumerate() {
                count_layout_panes(&tab.layout, &mut pane_counts);
                if let Some(number) = workspace.public_tab_numbers.get(index) {
                    *tab_counts.entry(*number).or_insert(0usize) += 1;
                }
            }
            for raw in pane_counts.keys() {
                if let Some(number) = workspace.public_pane_numbers.get(raw) {
                    *number_counts.entry(*number).or_insert(0usize) += 1;
                }
            }
            for (tab_index, tab) in workspace.tabs.iter().enumerate() {
                for (raw_pane_id, pane) in &tab.panes {
                    if self.locations >= MAX_LOCATIONS {
                        return Ok(());
                    }
                    let Some(reference) = &pane.agent_session else {
                        if (pane.agent_name.as_deref() == Some("pi")
                            || pane.managed_agent_kind.as_deref() == Some("pi"))
                            && self.claim_location(emit)?
                        {
                            self.unavailable(vec![], format!("Session {session}: saved Pi pane has no native session reference"), emit)?;
                        }
                        continue;
                    };
                    let agent_session = AgentSessionInfo {
                        source: reference.source.clone(),
                        agent: reference.agent.clone(),
                        kind: reference.kind,
                        value: reference.value.clone(),
                    };
                    if !is_pi(&agent_session) {
                        continue;
                    }
                    let identities = workspace
                        .id
                        .as_deref()
                        .zip(workspace.public_tab_numbers.get(tab_index))
                        .zip(workspace.public_pane_numbers.get(raw_pane_id));
                    let Some(((workspace_id, tab_number), pane_number)) =
                        identities.filter(|((id, tab_number), pane_number)| {
                            !id.is_empty()
                                && **tab_number > 0
                                && **pane_number > 0
                                && workspace_counts.get(*id).copied() == Some(1)
                                && tab_counts.get(*tab_number).copied() == Some(1)
                                && number_counts.get(*pane_number).copied() == Some(1)
                                && pane_counts.get(raw_pane_id).copied() == Some(1)
                                && layout_contains(&tab.layout, *raw_pane_id)
                        })
                    else {
                        if self.claim_location(emit)? {
                            self.unavailable(vec![], format!("Session {session}: saved Pi association lacks unambiguous stable workspace/tab/pane identity"), emit)?;
                        }
                        continue;
                    };
                    self.add_location(
                        HistoryLocation {
                            session: session.to_owned(),
                            workspace_id: workspace_id.to_owned(),
                            workspace_label: workspace_label.clone(),
                            tab_id: crate::workspace::public_tab_id_for_number(
                                workspace_id,
                                *tab_number,
                            ),
                            tab_label: sanitize(
                                tab.custom_name.as_deref().unwrap_or_default(),
                                256,
                            ),
                            pane_id: crate::workspace::public_pane_id_for_number(
                                workspace_id,
                                *pane_number,
                            ),
                            pane_label: pane.label.as_deref().map(|label| sanitize(label, 256)),
                            terminal_id: None,
                            running: false,
                            agent_session,
                        },
                        emit,
                    )?;
                    if self.locations >= MAX_LOCATIONS {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    fn add_location(
        &mut self,
        location: HistoryLocation,
        emit: &mut dyn FnMut(HistoryEvent) -> io::Result<()>,
    ) -> io::Result<()> {
        if !self.claim_location(emit)? {
            return Ok(());
        }
        let reference = &location.agent_session;
        if reference.value.len() > 4096
            || reference.source.len() > 128
            || reference.agent.len() > 128
            || [&location.workspace_id, &location.tab_id, &location.pane_id]
                .iter()
                .any(|id| id.len() > 256)
            || location
                .terminal_id
                .as_ref()
                .is_some_and(|id| id.len() > 256)
        {
            return self.unavailable(
                vec![],
                "Oversized Pi metadata reference was not inspected".into(),
                emit,
            );
        }
        if reference.source != "herdr:pi"
            || reference.agent != "pi"
            || reference.kind != crate::agent_resume::AgentSessionRefKind::Path
            || reference.value.contains('\0')
            || !Path::new(&reference.value).is_absolute()
        {
            return self.unavailable(vec![location], "Unsupported Pi session reference; an absolute official Pi file path is required (ID-only references are not resolved by guessing directories)".into(), emit);
        }
        let path = match std::fs::canonicalize(&reference.value).and_then(|path| {
            if std::fs::metadata(&path)?.is_file() {
                Ok(path)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Pi history is not a regular file",
                ))
            }
        }) {
            Ok(path) => path,
            Err(error) => {
                return self.unavailable(
                    vec![location],
                    format!(
                        "Pi history unavailable: {}",
                        sanitize(&error.to_string(), 256)
                    ),
                    emit,
                )
            }
        };
        let id = source_id(&path);
        let name = sanitize(&path.file_name().unwrap_or_default().to_string_lossy(), 256);
        let item = self
            .sources
            .entry(id.clone())
            .or_insert_with(|| CatalogSource {
                source: HistorySource {
                    id,
                    name,
                    locations: Vec::new(),
                },
                path,
            });
        if !item.source.locations.contains(&location) {
            item.source.locations.push(location);
            if serde_json::to_vec(&item.source)?.len() > MAX_SOURCE_BYTES {
                let location = item.source.locations.pop().into_iter().collect();
                return self.unavailable(
                    location,
                    "Pi source provenance limit reached; not all associations can be returned"
                        .into(),
                    emit,
                );
            }
        }
        Ok(())
    }

    fn claim_location(
        &mut self,
        emit: &mut dyn FnMut(HistoryEvent) -> io::Result<()>,
    ) -> io::Result<bool> {
        if self.locations >= MAX_LOCATIONS {
            return Ok(false);
        }
        self.locations += 1;
        if self.locations == MAX_LOCATIONS {
            self.unavailable(
                vec![],
                "Pi association limit reached; additional references may not have been inspected"
                    .into(),
                emit,
            )?;
        }
        Ok(true)
    }

    fn unavailable(
        &mut self,
        locations: Vec<HistoryLocation>,
        message: String,
        emit: &mut dyn FnMut(HistoryEvent) -> io::Result<()>,
    ) -> io::Result<()> {
        self.truncated = true;
        emit(HistoryEvent::Unavailable { locations, message })
    }
}

fn session_names(directory: &Path) -> io::Result<(Vec<String>, bool)> {
    let mut names = vec![crate::session::DEFAULT_SESSION_NAME.to_owned()];
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((names, false)),
        Err(error) => return Err(error),
    };
    let mut limited = false;
    for (index, entry) in entries.enumerate() {
        if index == MAX_LOCATIONS {
            limited = true;
            break;
        }
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name == crate::session::DEFAULT_SESSION_NAME
            || crate::session::validate_name(&name).is_err()
        {
            continue;
        }
        if names.len() == MAX_SESSIONS {
            limited = true;
            break;
        }
        names.push(name);
    }
    names[1..].sort();
    Ok((names, limited))
}

fn session_info(name: &str, deadline: Instant) -> io::Result<SessionInfo> {
    let target = crate::session::parse_target_name(name).map_err(io::Error::other)?;
    let socket = crate::session::api_socket_path_for(target.as_deref());
    let connection = crate::ipc::connect_local_stream_cancellable(
        &socket,
        &std::sync::atomic::AtomicBool::new(false),
        deadline.min(Instant::now() + METADATA_TIMEOUT),
    );
    let running = connection.is_ok();
    let connection_error = connection
        .err()
        .filter(|error| !crate::cli::server_not_running_error(error))
        .map(|error| error.to_string());
    Ok(SessionInfo {
        name: name.to_owned(),
        default: target.is_none(),
        running,
        connection_error,
        socket_path: socket.display().to_string(),
        session_dir: crate::session::data_dir_for(target.as_deref())
            .display()
            .to_string(),
    })
}

fn count_layout_panes(layout: &crate::persist::LayoutSnapshot, counts: &mut HashMap<u32, usize>) {
    match layout {
        crate::persist::LayoutSnapshot::Pane(id) => *counts.entry(*id).or_default() += 1,
        crate::persist::LayoutSnapshot::Split { first, second, .. } => {
            count_layout_panes(first, counts);
            count_layout_panes(second, counts);
        }
    }
}

fn layout_contains(layout: &crate::persist::LayoutSnapshot, raw: u32) -> bool {
    match layout {
        crate::persist::LayoutSnapshot::Pane(id) => *id == raw,
        crate::persist::LayoutSnapshot::Split { first, second, .. } => {
            layout_contains(first, raw) || layout_contains(second, raw)
        }
    }
}

fn is_pi(reference: &AgentSessionInfo) -> bool {
    reference.agent == "pi" || reference.source == "herdr:pi"
}

fn source_id(path: &Path) -> String {
    format!(
        "pi:{:x}",
        Sha256::digest(path.as_os_str().as_encoded_bytes())
    )
}

#[cfg(test)]
fn read_live_snapshot(session: &SessionInfo) -> io::Result<crate::api::schema::SessionSnapshot> {
    read_live_snapshot_until(session, Instant::now() + METADATA_TIMEOUT)
}

fn read_live_snapshot_until(
    session: &SessionInfo,
    deadline: Instant,
) -> io::Result<crate::api::schema::SessionSnapshot> {
    let deadline = deadline.min(Instant::now() + METADATA_TIMEOUT);
    let mut stream = crate::ipc::connect_local_stream_cancellable(
        Path::new(&session.socket_path),
        &std::sync::atomic::AtomicBool::new(false),
        deadline,
    )?;
    let request = Request {
        id: "history:metadata".into(),
        method: Method::SessionHistorySnapshot(EmptyParams::default()),
    };
    let mut bytes = serde_json::to_vec(&request)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()?;
    crate::ipc::set_local_stream_polling(&mut stream, true)?;
    let mut reader =
        BufReader::new(MetadataReader { stream, deadline }.take(MAX_METADATA_BYTES + 1));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    if line.len() as u64 > MAX_METADATA_BYTES || !line.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete or oversized live metadata response",
        ));
    }
    let value = serde_json::from_slice(&line)?;
    let response = crate::api::client::parse_response_value(value).map_err(io::Error::other)?;
    if response.id != request.id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mismatched history metadata response",
        ));
    }
    match response.result {
        ResponseResult::SessionSnapshot { snapshot } => Ok(*snapshot),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected history metadata response",
        )),
    }
}

struct MetadataReader {
    stream: crate::ipc::LocalStream,
    deadline: Instant,
}

impl Read for MetadataReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            if Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "history metadata request timed out",
                ));
            }
            match crate::ipc::poll_local_stream_read_count(&mut self.stream, buffer)? {
                crate::ipc::LocalStreamReadCount::Data(count) => return Ok(count),
                crate::ipc::LocalStreamReadCount::Closed => return Ok(0),
                crate::ipc::LocalStreamReadCount::Pending => {
                    std::thread::sleep(Duration::from_millis(2))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
