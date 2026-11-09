use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use super::journal::{ReceivePhase, ReceiveRecord, Route};
use crate::api::schema::{WorkspaceTransferInfo, WorkspaceTransferParams};
use crate::handoff_runtime::HandoffRuntimeState;
use crate::persist::workspace_transfer::WorkspaceTransferBundle;

#[cfg(test)]
mod tests;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TRANSFER_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct EventSender {
    pub tx: mpsc::Sender<TransferEvent>,
    pub notify: Arc<Notify>,
}

impl EventSender {
    pub fn send(&self, event: TransferEvent) -> io::Result<()> {
        self.tx
            .send(event)
            .map_err(|_| io::Error::other("workspace transfer coordinator stopped"))?;
        self.notify.notify_one();
        Ok(())
    }
}

pub(crate) enum TransferEvent {
    Snapshot {
        transfer_id: String,
        runtimes: Vec<HandoffRuntimeState>,
        reply: mpsc::Sender<io::Result<WorkspaceTransferBundle>>,
    },
    Decision {
        transfer_id: String,
        record: Box<ReceiveRecord>,
        reply: mpsc::Sender<io::Result<()>>,
    },
    Finished {
        transfer_id: String,
        outcome: SourceOutcome,
        reply: Option<mpsc::Sender<io::Result<Vec<crate::terminal::TerminalRuntime>>>>,
    },
    Prepare {
        bundle: Box<WorkspaceTransferBundle>,
        fds: Vec<OwnedFd>,
        digest: String,
        reply: mpsc::Sender<io::Result<ReceiveRecord>>,
    },
    Control {
        transfer_id: String,
        source_session: String,
        action: ControlAction,
        reply: mpsc::Sender<io::Result<ReceiveRecord>>,
    },
    CheckpointComplete {
        transfer_id: String,
        result: io::Result<()>,
    },
    Restored {
        transfer_id: String,
        result: io::Result<(
            Box<crate::persist::workspace_transfer::ImportedWorkspace>,
            crate::plugin_installations::Leases,
        )>,
        reply: mpsc::Sender<io::Result<ReceiveRecord>>,
    },
}

#[derive(Clone)]
pub(crate) enum SourceOutcome {
    Owned(Box<ReceiveRecord>),
    Released,
    Settled(Box<ReceiveRecord>),
    Aborted(String),
    Uncertain(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) enum ControlAction {
    Commit,
    Status,
    Abort,
    Settle,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Command {
    Hello,
    Prepare {
        bundle: Box<WorkspaceTransferBundle>,
    },
    Control {
        transfer_id: String,
        source_session: String,
        action: ControlAction,
    },
}

#[derive(Serialize, Deserialize)]
struct Reply {
    transfer_protocol: u32,
    runtime_domain: Option<String>,
    version: String,
    protocol: u32,
    session: String,
    record: Option<ReceiveRecord>,
    error: Option<String>,
}

pub(crate) struct ControlServer {
    running: Arc<AtomicBool>,
    path: PathBuf,
    identity: crate::ipc::SocketFileIdentity,
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        let _ = crate::ipc::remove_socket_file_if_owned(&self.path, &self.identity);
    }
}

pub(crate) fn socket_path(session: &str) -> io::Result<PathBuf> {
    let session = crate::session::parse_target_name(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(crate::session::data_dir_for(session.as_deref()).join("herdr-transfer.sock"))
}

pub(crate) fn start(sender: EventSender) -> io::Result<ControlServer> {
    let path = socket_path(&super::session_name())?;
    crate::ipc::prepare_socket_path(&path, |_| {
        "workspace transfer listener is already running".into()
    })?;
    let listener = UnixListener::bind(&path)?;
    crate::server::socket_paths::restrict_socket_permissions(&path)?;
    let identity = crate::ipc::socket_file_identity(&path)?;
    listener.set_nonblocking(true)?;
    let running = Arc::new(AtomicBool::new(true));
    let active = running.clone();
    let session = super::session_name();
    let result = crate::thread_spawn::spawn_named("herdr-transfer-listen", move || {
        while active.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let sender = sender.clone();
                    let session = session.clone();
                    if let Err(error) =
                        crate::thread_spawn::spawn_named("herdr-transfer-recv", move || {
                            if let Err(error) = receive(stream, sender, session) {
                                tracing::debug!(%error, "workspace transfer control disconnected");
                            }
                        })
                    {
                        tracing::warn!(%error, "failed to start workspace transfer receiver");
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    tracing::warn!(%error, "workspace transfer listener failed");
                    break;
                }
            }
        }
    });
    if let Err(error) = result {
        let _ = crate::ipc::remove_socket_file_if_owned(&path, &identity);
        return Err(error);
    }
    Ok(ControlServer {
        running,
        path,
        identity,
    })
}

fn frame_bytes<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "workspace terminal snapshots exceed the transfer size limit",
        ));
    }
    Ok(bytes)
}

fn write_bytes(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(&(bytes.len() as u64).to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

fn read_bytes(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut length = [0; 8];
    stream.read_exact(&mut length)?;
    let length = usize::try_from(u64::from_be_bytes(length)).map_err(io::Error::other)?;
    if length > MAX_TRANSFER_BYTES {
        return Err(io::Error::other(
            "workspace transfer frame exceeds size limit",
        ));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_frame<T: DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    serde_json::from_slice(&read_bytes(stream)?).map_err(io::Error::other)
}

fn reply(
    stream: &mut UnixStream,
    session: String,
    result: io::Result<Option<ReceiveRecord>>,
) -> io::Result<()> {
    let (runtime_domain, result) = match crate::platform::workspace_transfer_domain() {
        Ok(domain) => (Some(domain), result),
        Err(error) => (
            None,
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("workspace transfer runtime domain is unavailable: {error}"),
            )),
        ),
    };
    let (record, error) = match result {
        Ok(record) => (record, None),
        Err(error) => (None, Some(error.to_string())),
    };
    write_bytes(
        stream,
        &frame_bytes(&Reply {
            transfer_protocol: super::TRANSFER_PROTOCOL_VERSION,
            runtime_domain,
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            session,
            record,
            error,
        })?,
    )
}

fn receive(mut stream: UnixStream, sender: EventSender, session: String) -> io::Result<()> {
    stream.set_read_timeout(Some(CONTROL_TIMEOUT))?;
    stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
    let bytes = read_bytes(&mut stream)?;
    let command: Command = match serde_json::from_slice(&bytes) {
        Ok(command) => command,
        Err(error) => {
            return reply(
                &mut stream,
                session,
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid workspace transfer frame: {error}"),
                )),
            )
        }
    };
    let (tx, rx) = mpsc::channel();
    match command {
        Command::Hello => return reply(&mut stream, session, Ok(None)),
        Command::Prepare { bundle } => {
            let result = crate::persist::workspace_transfer::validate(&bundle).and_then(|()| {
                if bundle.source_domain != crate::platform::workspace_transfer_domain()? {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "workspace transfer requires the same machine, user, and process namespace",
                    ));
                }
                if bundle.destination_session != session || bundle.source_session == session {
                    return Err(io::Error::other("workspace transfer session mismatch"));
                }
                Ok(())
            });
            if let Err(error) = result {
                return reply(&mut stream, session, Err(error));
            }
            reply(&mut stream, session.clone(), Ok(None))?;
            let raw_fds = crate::server::handoff::recv_fds(&stream, bundle.runtimes.len())?;
            let fds = raw_fds
                .into_iter()
                .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
                .collect();
            let digest = format!("{:x}", Sha256::digest(&bytes));
            sender.send(TransferEvent::Prepare {
                bundle,
                fds,
                digest,
                reply: tx,
            })?;
        }
        Command::Control {
            transfer_id,
            source_session,
            action,
        } => sender.send(TransferEvent::Control {
            transfer_id,
            source_session,
            action,
            reply: tx,
        })?,
    }
    let result = rx
        .recv_timeout(CONTROL_TIMEOUT)
        .map_err(io::Error::other)
        .and_then(|record| record)
        .map(Some);
    reply(&mut stream, session, result)
}

fn connect(session: &str) -> io::Result<UnixStream> {
    let stream = UnixStream::connect(socket_path(session)?)?;
    stream.set_read_timeout(Some(CONTROL_TIMEOUT))?;
    stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
    Ok(stream)
}

fn read_reply(stream: &mut UnixStream, destination: &str) -> io::Result<Option<ReceiveRecord>> {
    let reply: Reply = read_frame(stream)?;
    if reply.transfer_protocol != super::TRANSFER_PROTOCOL_VERSION
        || reply.version != crate::build_info::version()
        || reply.protocol != crate::protocol::PROTOCOL_VERSION
        || reply.session != destination
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "destination workspace transfer server is incompatible",
        ));
    }
    if let Some(error) = reply.error {
        return Err(io::Error::other(error));
    }
    if reply.runtime_domain.as_deref()
        != Some(crate::platform::workspace_transfer_domain()?.as_str())
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "workspace transfer requires the same machine, user, and process namespace",
        ));
    }
    Ok(reply.record)
}

pub(crate) fn control(
    destination: &str,
    transfer_id: &str,
    source_session: &str,
    action: ControlAction,
) -> io::Result<ReceiveRecord> {
    let mut stream = connect(destination)?;
    write_bytes(
        &mut stream,
        &frame_bytes(&Command::Control {
            transfer_id: transfer_id.into(),
            source_session: source_session.into(),
            action,
        })?,
    )?;
    read_reply(&mut stream, destination)?
        .ok_or_else(|| io::Error::other("destination omitted transfer status"))
}

pub(crate) fn run_source(
    params: WorkspaceTransferParams,
    handles: Vec<crate::pane::WorkspaceTransferHandle>,
    sender: EventSender,
    stopped: Arc<AtomicBool>,
) {
    let source_session = super::session_name();
    let mut ownership_possible = false;
    let result = prepare_source(
        &params,
        &handles,
        &sender,
        &stopped,
        &mut ownership_possible,
    );
    let record = match result {
        Ok(record) => record,
        Err(error) => {
            let aborted = control(
                &params.destination_session,
                &params.transfer_id,
                &source_session,
                ControlAction::Abort,
            );
            if ownership_possible
                || aborted.as_ref().is_ok_and(|record| {
                    matches!(record.phase, ReceivePhase::Activating | ReceivePhase::Owned)
                })
            {
                let _ = sender.send(TransferEvent::Finished {
                    transfer_id: params.transfer_id,
                    outcome: SourceOutcome::Uncertain(error.to_string()),
                    reply: None,
                });
                return;
            }
            let errors: Vec<_> = handles
                .iter()
                .filter_map(|handle| handle.rollback().err())
                .map(|error| error.to_string())
                .collect();
            let message = if errors.is_empty() {
                error.to_string()
            } else {
                format!("{error}; reader rollback failed: {}", errors.join("; "))
            };
            let outcome = if errors.is_empty() {
                SourceOutcome::Aborted(message)
            } else {
                SourceOutcome::Uncertain(message)
            };
            report_finished(&params.transfer_id, outcome, &sender, &stopped);
            return;
        }
    };
    let mut observed = None;
    while !stopped.load(Ordering::Acquire) {
        let result = control(
            &params.destination_session,
            &params.transfer_id,
            &source_session,
            ControlAction::Commit,
        )
        .or_else(|_| {
            control(
                &params.destination_session,
                &params.transfer_id,
                &source_session,
                ControlAction::Status,
            )
        });
        match result {
            Ok(record) if record.phase == ReceivePhase::Owned => {
                observed = Some(record);
                break;
            }
            Ok(record) if record.phase == ReceivePhase::Aborted => {
                let errors: Vec<_> = handles
                    .iter()
                    .filter_map(|handle| handle.rollback().err())
                    .collect();
                if errors.is_empty() {
                    report_finished(
                        &params.transfer_id,
                        SourceOutcome::Aborted(
                            record
                                .info
                                .error
                                .unwrap_or_else(|| "receiver aborted before ownership".into()),
                        ),
                        &sender,
                        &stopped,
                    );
                } else {
                    tracing::error!("workspace transfer aborted but source reader rollback failed; keeping transaction pending");
                }
                return;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(transfer_id = %params.transfer_id, %error, "workspace ownership outcome pending")
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if let Some(record) = observed {
        report_owned(&params, record, &sender, &stopped);
    } else {
        let _ = record;
    }
}

fn prepare_source(
    params: &WorkspaceTransferParams,
    handles: &[crate::pane::WorkspaceTransferHandle],
    sender: &EventSender,
    stopped: &AtomicBool,
    ownership_possible: &mut bool,
) -> io::Result<ReceiveRecord> {
    crate::server::ensure_session_server(&params.destination_session, stopped)?;
    let mut hello = connect(&params.destination_session)?;
    write_bytes(&mut hello, &frame_bytes(&Command::Hello)?)?;
    read_reply(&mut hello, &params.destination_session)?;
    let mut runtimes = Vec::new();
    let mut fds = Vec::new();
    for handle in handles {
        let (state, fd) = handle.capture().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not capture live terminal: {error}"),
            )
        })?;
        if state.child_pid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "pane process exited before workspace transfer capture",
            ));
        }
        runtimes.push(state);
        fds.push(fd);
    }
    let (reply, rx) = mpsc::channel();
    sender.send(TransferEvent::Snapshot {
        transfer_id: params.transfer_id.clone(),
        runtimes,
        reply,
    })?;
    let bundle = rx
        .recv_timeout(CONTROL_TIMEOUT)
        .map_err(io::Error::other)??;
    let bytes = frame_bytes(&Command::Prepare {
        bundle: Box::new(bundle),
    })?;
    let mut stream = connect(&params.destination_session)?;
    write_bytes(&mut stream, &bytes)?;
    if read_reply(&mut stream, &params.destination_session)?.is_some() {
        return Err(io::Error::other(
            "destination skipped workspace transfer validation",
        ));
    }
    let raw_fds: Vec<_> = fds.iter().map(AsRawFd::as_raw_fd).collect();
    crate::server::handoff::send_fds(&stream, &raw_fds)?;
    let record = read_reply(&mut stream, &params.destination_session)?
        .ok_or_else(|| io::Error::other("destination omitted prepared status"))?;
    if record.phase == ReceivePhase::Aborted {
        return Err(io::Error::other(
            "destination already aborted this transfer UUID",
        ));
    }
    *ownership_possible = matches!(record.phase, ReceivePhase::Activating | ReceivePhase::Owned);
    let (reply, rx) = mpsc::channel();
    sender.send(TransferEvent::Decision {
        transfer_id: params.transfer_id.clone(),
        record: Box::new(record.clone()),
        reply,
    })?;
    rx.recv_timeout(CONTROL_TIMEOUT)
        .map_err(io::Error::other)??;
    Ok(record)
}

fn report_owned(
    params: &WorkspaceTransferParams,
    record: ReceiveRecord,
    sender: &EventSender,
    stopped: &AtomicBool,
) {
    let Some(runtimes) = report_finished(
        &params.transfer_id,
        SourceOutcome::Owned(Box::new(record)),
        sender,
        stopped,
    ) else {
        return;
    };
    for runtime in runtimes {
        runtime.preserve_for_handoff();
    }
    if report_finished(
        &params.transfer_id,
        SourceOutcome::Released,
        sender,
        stopped,
    )
    .is_some()
    {
        settle_source(params, sender, stopped);
    }
}

fn report_finished(
    id: &str,
    outcome: SourceOutcome,
    sender: &EventSender,
    stopped: &AtomicBool,
) -> Option<Vec<crate::terminal::TerminalRuntime>> {
    while !stopped.load(Ordering::Acquire) {
        let (reply, rx) = mpsc::channel();
        if sender
            .send(TransferEvent::Finished {
                transfer_id: id.into(),
                outcome: outcome.clone(),
                reply: Some(reply),
            })
            .is_err()
        {
            return None;
        }
        if let Ok(Ok(runtimes)) = rx.recv() {
            return Some(runtimes);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

fn settle_source(params: &WorkspaceTransferParams, sender: &EventSender, stopped: &AtomicBool) {
    while !stopped.load(Ordering::Acquire) {
        match control(
            &params.destination_session,
            &params.transfer_id,
            &super::session_name(),
            ControlAction::Settle,
        ) {
            Ok(record) if record.phase == ReceivePhase::Owned && record.settled => {
                report_finished(
                    &params.transfer_id,
                    SourceOutcome::Settled(Box::new(record)),
                    sender,
                    stopped,
                );
                return;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(transfer_id = %params.transfer_id, %error, "workspace settlement pending");
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

pub(crate) fn recover_source(
    params: WorkspaceTransferParams,
    sender: EventSender,
    stopped: Arc<AtomicBool>,
    mut needs_decision: bool,
    runtime_released: bool,
) {
    if runtime_released {
        settle_source(&params, &sender, &stopped);
        return;
    }
    let source_session = super::session_name();
    while !stopped.load(Ordering::Acquire) {
        let result = control(
            &params.destination_session,
            &params.transfer_id,
            &source_session,
            ControlAction::Status,
        )
        .and_then(|record| {
            if record.phase == ReceivePhase::Aborted {
                return Ok(record);
            }
            if needs_decision {
                let (reply, rx) = mpsc::channel();
                sender.send(TransferEvent::Decision {
                    transfer_id: params.transfer_id.clone(),
                    record: Box::new(record.clone()),
                    reply,
                })?;
                rx.recv_timeout(CONTROL_TIMEOUT)
                    .map_err(io::Error::other)??;
                needs_decision = false;
            }
            if matches!(
                record.phase,
                ReceivePhase::Prepared | ReceivePhase::Activating
            ) {
                control(
                    &params.destination_session,
                    &params.transfer_id,
                    &source_session,
                    ControlAction::Commit,
                )
            } else {
                Ok(record)
            }
        });
        if let Ok(record) = result {
            if record.phase == ReceivePhase::Owned {
                report_owned(&params, record, &sender, &stopped);
                return;
            }
            if record.phase == ReceivePhase::Aborted {
                let _ = sender.send(TransferEvent::Finished { transfer_id: params.transfer_id, outcome: SourceOutcome::Uncertain("destination aborted after source restart; original runtime ownership cannot be recovered".into()), reply: None });
                return;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

pub(crate) fn committed_info(record: &ReceiveRecord) -> WorkspaceTransferInfo {
    let mut info = record.info.clone();
    info.state = crate::api::schema::WorkspaceTransferState::Committed;
    info.error = None;
    info
}

pub(crate) fn pending_route(record: &ReceiveRecord) -> Route {
    let mut route = record.route.clone();
    route.ready = false;
    route
}
