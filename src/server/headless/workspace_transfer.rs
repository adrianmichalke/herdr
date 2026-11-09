use super::*;
#[cfg(unix)]
use crate::api::schema::WorkspaceTransferState;
use crate::api::schema::{WorkspaceTransferInfo, WorkspaceTransferParams};
use crate::server::client_shell::resize_popup_runtime;

#[cfg(all(test, unix))]
mod tests;
use crate::server::workspace_transfer::{self as transfer, journal::ReceivePhase};

#[cfg(unix)]
use crate::server::workspace_transfer::journal::{
    ReceiveRecord, SourcePhase, SourceRecord, TransferRegistry,
};
#[cfg(unix)]
use crate::server::workspace_transfer::transport::{
    self, ControlAction, EventSender, SourceOutcome, TransferEvent,
};

#[cfg(unix)]
pub(super) struct Coordinator {
    pub sender: EventSender,
    receiver: std::sync::mpsc::Receiver<TransferEvent>,
    staged: HashMap<String, crate::persist::workspace_transfer::ImportedWorkspace>,
    restoring: HashSet<String>,
    checkpoint_in_flight: Option<String>,
    deferred_exits: Vec<(String, AppEvent)>,
    receive_deadlines: HashMap<String, Instant>,
    next_receive_deadline: Option<Instant>,
    _listener: Option<transport::ControlServer>,
    listen_enabled: bool,
}

#[cfg(unix)]
impl Coordinator {
    pub fn new(
        registry: TransferRegistry,
        notify: Arc<tokio::sync::Notify>,
        listen: bool,
    ) -> io::Result<Self> {
        let (tx, receiver) = std::sync::mpsc::channel();
        let sender = EventSender { tx, notify };
        let listener = if listen {
            Some(transport::start(sender.clone())?)
        } else {
            None
        };
        if listen {
            registry.recover()?;
        }
        Ok(Self {
            sender,
            receiver,
            staged: HashMap::new(),
            restoring: HashSet::new(),
            checkpoint_in_flight: None,
            deferred_exits: Vec::new(),
            receive_deadlines: HashMap::new(),
            next_receive_deadline: None,
            _listener: listener,
            listen_enabled: listen,
        })
    }
}

impl HeadlessServer {
    pub(super) fn handle_workspace_transfer_api(&mut self, msg: api::ApiRequestMessage) -> bool {
        let result = match &msg.request.method {
            api::schema::Method::WorkspaceTransfer(params) => {
                self.submit_workspace_transfer(params.clone())
            }
            api::schema::Method::WorkspaceTransferStatus(params) => {
                if !transfer::valid_transfer_id(&params.transfer_id) {
                    let _ = msg.respond_to.send(transfer::error_response(
                        msg.request.id,
                        "invalid_request",
                        "transfer_id must be a UUID",
                    ));
                    return false;
                }
                self.transfer_registry
                    .read(|journal| {
                        journal
                            .sources
                            .get(&params.transfer_id)
                            .map(|record| record.info.clone())
                    })
                    .and_then(|info| {
                        info.ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::NotFound,
                                "workspace transfer was not found on this source endpoint",
                            )
                        })
                    })
            }
            _ => return false,
        };
        let response = match result {
            Ok(transfer) => serde_json::to_string(&api::schema::SuccessResponse {
                id: msg.request.id,
                result: api::schema::ResponseResult::WorkspaceTransfer { transfer },
            })
            .unwrap_or_else(|_| "{}".into()),
            Err(error) => transfer::error_response(
                msg.request.id,
                match error.kind() {
                    io::ErrorKind::Unsupported => "unsupported_transfer",
                    io::ErrorKind::WouldBlock => "workspace_busy",
                    io::ErrorKind::AlreadyExists => "transfer_conflict",
                    io::ErrorKind::NotFound => "not_found",
                    _ => "transfer_failed",
                },
                error.to_string(),
            ),
        };
        let _ = msg.respond_to.send(response);
        false
    }

    #[cfg(not(unix))]
    fn submit_workspace_transfer(
        &mut self,
        _params: WorkspaceTransferParams,
    ) -> io::Result<WorkspaceTransferInfo> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "live workspace transfer requires same-machine Unix PTY descriptor passing",
        ))
    }

    #[cfg(unix)]
    fn submit_workspace_transfer(
        &mut self,
        mut params: WorkspaceTransferParams,
    ) -> io::Result<WorkspaceTransferInfo> {
        if !transfer::valid_transfer_id(&params.transfer_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "transfer_id must be a UUID",
            ));
        }
        let destination = crate::session::parse_target_name(&params.destination_session)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        params.destination_session =
            destination.unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.into());
        if params.destination_session == transfer::session_name() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination must be another session on this machine",
            ));
        }
        let journal = self.transfer_registry.snapshot()?;
        if let Some(record) = journal.sources.get(&params.transfer_id) {
            if record.params != params {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "transfer_id was already used with different parameters",
                ));
            }
            return Ok(record.info.clone());
        }
        if self.handoff_in_progress
            || journal.sources.values().any(|record| {
                matches!(
                    record.phase,
                    SourcePhase::Preparing | SourcePhase::CommitDecided
                ) && (record.params.workspace_id == params.workspace_id
                    || record.params.destination_session == params.destination_session)
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "workspace or destination already has a transfer in progress",
            ));
        }
        let index = self
            .app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == params.workspace_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "workspace was not found"))?;
        if self.app.state.workspace_transfer_fenced(index) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "workspace transfer is already in progress",
            ));
        }
        if self.workspace_forwarding_depth(&params.workspace_id, &journal)
            >= transfer::MAX_FORWARD_HOPS
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "workspace forwarding hop capacity is exhausted",
            ));
        }
        let workspace = &self.app.state.workspaces[index];
        let mut handles = Vec::new();
        let mut terminal_ids = HashSet::new();
        for tab in &workspace.tabs {
            for (&pane_id, pane) in &tab.panes {
                if !terminal_ids.insert(pane.attached_terminal_id.clone()) {
                    return Err(io::Error::other(
                        "workspace shares a terminal between panes",
                    ));
                }
                if self.app.overlay_panes.contains_key(&pane_id)
                    || self
                        .app
                        .pending_worktree_remove_runtime_exits
                        .contains_key(&pane_id)
                    || self
                        .app
                        .pending_worktree_remove_runtime_restores
                        .contains_key(&pane_id)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "workspace has a transient pane operation in progress",
                    ));
                }
                let terminal = self
                    .app
                    .state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .ok_or_else(|| io::Error::other("workspace terminal is missing"))?;
                if terminal.pending_agent_resume_plan.is_some()
                    || terminal.managed_agent_launch_pending()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "workspace has an agent launch or resume in progress",
                    ));
                }
                terminal
                    .workspace_transfer_state()
                    .map_err(|error| io::Error::new(io::ErrorKind::Unsupported, error))?;
                if let Some(name) = &terminal.agent_name {
                    if self
                        .app
                        .state
                        .terminals
                        .values()
                        .filter(|terminal| terminal.agent_name.as_ref() == Some(name))
                        .count()
                        != 1
                        || journal.routes.values().any(|route| {
                            route.names.get(name).is_some_and(|target| {
                                route.terminals.get(terminal.id.as_str()) != Some(target)
                            })
                        })
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("agent name {name:?} collides with another source binding"),
                        ));
                    }
                }
                let runtime = self
                    .app
                    .terminal_runtimes
                    .get(&pane.attached_terminal_id)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::Unsupported,
                            "every transferred pane must have a live PTY",
                        )
                    })?;
                handles.push(runtime.workspace_transfer_handle(pane_id.raw())?);
            }
        }
        if self
            .app
            .state
            .workspaces
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .any(|(_, workspace)| {
                workspace.tabs.iter().any(|tab| {
                    tab.panes
                        .values()
                        .any(|pane| terminal_ids.contains(&pane.attached_terminal_id))
                })
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "moving workspace shares a terminal identity with another workspace",
            ));
        }
        if self
            .pending_alt_screen_reads
            .iter()
            .any(|read| terminal_ids.contains(&read.terminal_id))
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "workspace has an alternate-screen read in progress",
            ));
        }
        if self
            .app
            .pending_restored_worktree_spaces
            .iter()
            .any(|(id, _)| id == &params.workspace_id)
            || self
                .app
                .pending_api_worktree_removes
                .contains_key(&params.workspace_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "workspace has a worktree operation in progress",
            ));
        }
        let pane_ids: HashSet<_> = workspace
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.keys().copied())
            .collect();
        let mut source_identities = std::collections::BTreeSet::from([workspace.id.clone()]);
        for tab in &workspace.tabs {
            source_identities.insert(crate::workspace::public_tab_id_for_number(
                &workspace.id,
                tab.number,
            ));
        }
        for (pane, number) in &workspace.public_pane_numbers {
            source_identities.insert(crate::workspace::public_pane_id_for_number(
                &workspace.id,
                *number,
            ));
            if self
                .app
                .state
                .pane_id_aliases
                .get(&pane.raw())
                .is_none_or(|target| target == pane)
            {
                source_identities.insert(format!("p_{}", pane.raw()));
            }
        }
        source_identities.extend(terminal_ids.iter().map(ToString::to_string));
        source_identities.extend(
            self.app
                .state
                .public_pane_id_aliases
                .iter()
                .filter(|(_, pane)| pane_ids.contains(*pane))
                .map(|(alias, _)| alias.clone()),
        );
        source_identities.extend(
            self.app
                .state
                .pane_id_aliases
                .iter()
                .filter(|(_, pane)| pane_ids.contains(*pane))
                .map(|(alias, _)| format!("p_{alias}")),
        );
        let source_names = self
            .app
            .state
            .terminals
            .values()
            .filter(|terminal| terminal_ids.contains(&terminal.id))
            .filter_map(|terminal| {
                terminal
                    .agent_name
                    .as_ref()
                    .map(|name| (name.clone(), terminal.id.to_string()))
            })
            .collect();
        let info = WorkspaceTransferInfo {
            transfer_id: params.transfer_id.clone(),
            state: WorkspaceTransferState::Pending,
            destination_session: params.destination_session.clone(),
            workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            error: None,
        };
        self.transfer_registry.update(|journal| {
            journal
                .reserved_workspaces
                .insert(params.workspace_id.clone());
            for identity in &source_identities {
                let workspace = identity
                    .split_once(':')
                    .map_or(identity.as_str(), |(workspace, _)| workspace);
                if crate::workspace::public_workspace_number(workspace).is_some() {
                    journal.reserved_workspaces.insert(workspace.into());
                    crate::workspace::reserve_workspace_id(workspace);
                }
                if let Some(raw) = identity
                    .strip_prefix("p_")
                    .and_then(|raw| raw.parse::<u32>().ok())
                {
                    crate::layout::PaneId::reserve(raw);
                }
            }
            journal.sources.insert(
                params.transfer_id.clone(),
                SourceRecord {
                    params: params.clone(),
                    info: info.clone(),
                    phase: SourcePhase::Preparing,
                    route: None,
                    source_identities: source_identities.clone(),
                    source_names,
                    runtime_released: false,
                },
            );
            Ok(())
        })?;
        self.app
            .state
            .workspace_transfer_fences
            .insert(params.workspace_id.clone());
        self.set_source_drop_protection(&params.workspace_id, true);
        let sender = self.workspace_transfers.sender.clone();
        let stopped = self.should_quit.clone();
        let worker_params = params.clone();
        if let Err(error) =
            crate::thread_spawn::spawn_named("herdr-workspace-transfer", move || {
                transport::run_source(worker_params, handles, sender, stopped)
            })
        {
            self.fail_source_transfer(&params.transfer_id, error.to_string())?;
        }
        Ok(info)
    }

    #[cfg(unix)]
    pub(super) fn pump_workspace_transfers(&mut self) -> bool {
        let mut changed = self.expire_prepared_workspace_transfers(Instant::now());
        while let Ok(event) = self.workspace_transfers.receiver.try_recv() {
            match event {
                TransferEvent::Snapshot {
                    transfer_id,
                    runtimes,
                    reply,
                } => {
                    self.drain_all_internal_events_with_forwarding();
                    let result = self.transfer_registry.snapshot().and_then(|journal| {
                        let source = journal
                            .sources
                            .get(&transfer_id)
                            .ok_or_else(|| io::Error::other("source transfer disappeared"))?;
                        if source.phase != SourcePhase::Preparing {
                            return Err(io::Error::other("source transfer is no longer preparing"));
                        }
                        crate::persist::workspace_transfer::capture(
                            &self.app,
                            &source.params.workspace_id,
                            transfer_id,
                            source.params.destination_session.clone(),
                            runtimes,
                            self.workspace_forwarding_depth(&source.params.workspace_id, &journal),
                        )
                    });
                    let _ = reply.send(result);
                }
                TransferEvent::Prepare {
                    bundle,
                    fds,
                    digest,
                    reply,
                } => match self.prepare_received_workspace(*bundle, fds, digest, reply.clone()) {
                    Ok(Some(record)) => {
                        let _ = reply.send(Ok(record));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                },
                TransferEvent::Restored {
                    transfer_id,
                    result,
                    reply,
                } => {
                    let result = self.finish_received_restore(&transfer_id, result);
                    let _ = reply.send(result);
                }
                TransferEvent::Control {
                    transfer_id,
                    source_session,
                    action,
                    reply,
                } => {
                    let result =
                        self.control_received_workspace(&transfer_id, &source_session, action);
                    changed |= matches!(action, ControlAction::Commit | ControlAction::Settle);
                    let _ = reply.send(result);
                }
                TransferEvent::CheckpointComplete {
                    transfer_id,
                    result,
                } => {
                    if let Err(error) = self.finish_receiver_checkpoint(&transfer_id, result) {
                        tracing::error!(%transfer_id, %error, "receiver checkpoint could not finalize ownership");
                    }
                    changed = true;
                }
                TransferEvent::Decision {
                    transfer_id,
                    record,
                    reply,
                } => {
                    let result = self.decide_source_commit(&transfer_id, *record);
                    let _ = reply.send(result);
                }
                TransferEvent::Finished {
                    transfer_id,
                    outcome,
                    reply,
                } => {
                    let result = match outcome {
                        SourceOutcome::Owned(record) => {
                            self.release_source_transfer(&transfer_id, *record)
                        }
                        SourceOutcome::Released => self
                            .record_source_release(&transfer_id)
                            .map(|()| Vec::new()),
                        SourceOutcome::Settled(record) => self
                            .finish_source_transfer(&transfer_id, *record)
                            .map(|()| Vec::new()),
                        SourceOutcome::Aborted(error) => self
                            .fail_source_transfer(&transfer_id, error)
                            .map(|()| Vec::new()),
                        SourceOutcome::Uncertain(error) => {
                            if let Ok(journal) = self.transfer_registry.snapshot() {
                                if let Some(source) = journal.sources.get(&transfer_id) {
                                    self.set_source_drop_protection(
                                        &source.params.workspace_id,
                                        true,
                                    );
                                }
                            }
                            self.transfer_registry.update(|journal| {
                                let record =
                                    journal.sources.get_mut(&transfer_id).ok_or_else(|| {
                                        io::Error::other("source transfer disappeared")
                                    })?;
                                record.info.error = Some(error);
                                Ok(Vec::new())
                            })
                        }
                    };
                    if let Err(error) = &result {
                        tracing::error!(%transfer_id, %error, "workspace transfer outcome remains pending");
                    }
                    if let Some(reply) = reply {
                        if let Err(error) = reply.send(result) {
                            if let Ok(runtimes) = error.0 {
                                for runtime in runtimes {
                                    runtime.preserve_for_handoff();
                                }
                            }
                        }
                    }
                    changed = true;
                }
            }
        }
        changed
    }

    #[cfg(unix)]
    fn validate_received_collisions(
        &self,
        imported: &crate::persist::workspace_transfer::ImportedWorkspace,
        except: Option<&str>,
    ) -> io::Result<()> {
        if self
            .app
            .state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == imported.workspace.id)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination workspace identity collision",
            ));
        }
        let journal = self.transfer_registry.snapshot()?;
        let names: HashSet<_> = imported
            .terminals
            .values()
            .filter_map(|terminal| terminal.agent_name.as_deref())
            .collect();
        if imported
            .terminals
            .keys()
            .any(|id| self.app.state.terminals.contains_key(id))
            || self.app.state.terminals.values().any(|terminal| {
                terminal
                    .agent_name
                    .as_deref()
                    .is_some_and(|name| names.contains(name))
            })
            || journal.routes.values().any(|route| {
                route.names.iter().any(|(name, target)| {
                    names.contains(name.as_str())
                        && !imported.terminals.values().any(|terminal| {
                            terminal.agent_name.as_ref() == Some(name)
                                && route.terminals.get(terminal.id.as_str()) == Some(target)
                        })
                })
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination terminal identity or agent name collision",
            ));
        }
        for (id, other) in &self.workspace_transfers.staged {
            if except == Some(id.as_str()) {
                continue;
            }
            if other.workspace.id == imported.workspace.id
                || other
                    .terminals
                    .keys()
                    .any(|id| imported.terminals.contains_key(id))
                || other.terminals.values().any(|terminal| {
                    terminal
                        .agent_name
                        .as_deref()
                        .is_some_and(|name| names.contains(name))
                })
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "destination has a colliding prepared transfer",
                ));
            }
        }
        for plugin in &imported.plugins {
            if self
                .app
                .state
                .installed_plugins
                .get(&plugin.plugin_id)
                .is_some_and(|existing| existing != plugin)
                || self
                    .workspace_transfers
                    .staged
                    .iter()
                    .filter(|(id, _)| except != Some(id.as_str()))
                    .any(|(_, other)| {
                        other.plugins.iter().any(|existing| {
                            existing.plugin_id == plugin.plugin_id && existing != plugin
                        })
                    })
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "destination plugin identity collision",
                ));
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn prepare_received_workspace(
        &mut self,
        bundle: crate::persist::workspace_transfer::WorkspaceTransferBundle,
        fds: Vec<std::os::fd::OwnedFd>,
        digest: String,
        reply: std::sync::mpsc::Sender<io::Result<ReceiveRecord>>,
    ) -> io::Result<Option<ReceiveRecord>> {
        if self.shutting_down || self.handoff_in_progress {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "destination is unavailable for workspace transfer",
            ));
        }
        crate::persist::workspace_transfer::validate(&bundle)?;
        if bundle.destination_session != transfer::session_name() {
            return Err(io::Error::other(
                "transfer names another destination session",
            ));
        }
        let journal = self.transfer_registry.snapshot()?;
        if journal.aborted_receivers.contains_key(&bundle.transfer_id) {
            return Err(io::Error::other(
                "transfer UUID was aborted before receiver preparation",
            ));
        }
        if let Some(existing) = journal.receivers.get(&bundle.transfer_id) {
            if existing.digest != digest || existing.source_session != bundle.source_session {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "transfer UUID conflicts with an existing receiver transaction",
                ));
            }
            if self
                .workspace_transfers
                .restoring
                .contains(&bundle.transfer_id)
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "workspace import is still being restored",
                ));
            }
            if matches!(
                existing.phase,
                ReceivePhase::Activating | ReceivePhase::Owned
            ) && existing.owner_boot_id.as_deref() != Some(&self.client_shell_boot_id)
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "ownership from an earlier receiver generation is unconfirmed",
                ));
            }
            return Ok(Some(existing.clone()));
        }
        if !self.workspace_transfers.restoring.is_empty()
            || !self.workspace_transfers.staged.is_empty()
            || self.workspace_transfers.checkpoint_in_flight.is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "destination already has a workspace transfer in progress",
            ));
        }
        let mut destination_id = crate::workspace::generate_workspace_id();
        while self
            .app
            .state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == destination_id)
            || journal.reserved_workspaces.contains(&destination_id)
        {
            destination_id = crate::workspace::generate_workspace_id();
        }
        let route = crate::persist::workspace_transfer::route(&bundle, &destination_id)?;
        if route
            .terminals
            .keys()
            .any(|id| self.app.state.terminals.contains_key(id.as_str()))
            || route.names.keys().any(|name| {
                self.app
                    .state
                    .terminals
                    .values()
                    .any(|terminal| terminal.agent_name.as_ref() == Some(name))
            })
            || journal.routes.values().any(|existing| {
                route.names.keys().any(|name| {
                    existing.names.contains_key(name)
                        && (route.terminal_for_name(name).is_none()
                            || route.terminal_for_name(name) != existing.terminal_for_name(name))
                })
            })
            || journal
                .receivers
                .values()
                .filter(|existing| existing.phase == ReceivePhase::Prepared)
                .any(|existing| {
                    route
                        .terminals
                        .keys()
                        .any(|id| existing.route.terminals.contains_key(id))
                        || route
                            .names
                            .keys()
                            .any(|name| existing.route.names.contains_key(name))
                })
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination terminal identity or agent name collision",
            ));
        }
        for plugin in &bundle.plugins {
            if self
                .app
                .state
                .installed_plugins
                .get(&plugin.plugin_id)
                .is_some_and(|existing| existing != plugin)
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "destination plugin identity collision",
                ));
            }
        }
        let workspace = &bundle.snapshot.workspaces[0];
        let tab = &workspace.tabs[workspace.active_tab];
        let focused = tab
            .focused
            .ok_or_else(|| io::Error::other("transfer omitted tab focus"))?;
        let pane_number = workspace
            .public_pane_numbers
            .get(&focused)
            .copied()
            .ok_or_else(|| io::Error::other("transfer omitted focused pane number"))?;
        let transfer_id = bundle.transfer_id.clone();
        let info = WorkspaceTransferInfo {
            transfer_id: transfer_id.clone(),
            state: WorkspaceTransferState::Pending,
            destination_session: transfer::session_name(),
            workspace_id: Some(destination_id.clone()),
            focused_tab_id: Some(crate::workspace::public_tab_id_for_number(
                &destination_id,
                workspace.public_tab_numbers[workspace.active_tab],
            )),
            focused_pane_id: Some(crate::workspace::public_pane_id_for_number(
                &destination_id,
                pane_number,
            )),
            error: None,
        };
        let record = ReceiveRecord {
            source_session: bundle.source_session.clone(),
            source_workspace: route.origin_workspace.clone(),
            digest,
            phase: ReceivePhase::Prepared,
            info,
            route,
            owner_boot_id: None,
            forwarding_depth: bundle.forwarding_depth + 1,
            settled: false,
        };
        let context = crate::persist::workspace_transfer::RestoreContext::from_app(&self.app);
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        self.transfer_registry.update(|journal| {
            journal.reserved_workspaces.insert(destination_id.clone());
            journal.receivers.insert(transfer_id.clone(), record);
            Ok(())
        })?;
        self.workspace_transfers
            .restoring
            .insert(transfer_id.clone());
        self.workspace_transfers.receive_deadlines.insert(
            transfer_id.clone(),
            Instant::now() + Duration::from_secs(120),
        );
        self.workspace_transfers.next_receive_deadline = self
            .workspace_transfers
            .receive_deadlines
            .values()
            .copied()
            .min();
        let sender = self.workspace_transfers.sender.clone();
        let worker_id = transfer_id.clone();
        if let Err(error) = crate::thread_spawn::spawn_named("herdr-transfer-restore", move || {
            let _runtime = runtime.enter();
            let result =
                crate::persist::workspace_transfer::restore(context, bundle, fds, destination_id)
                    .and_then(|imported| {
                        let mut leases = crate::plugin_installations::Leases::new();
                        crate::plugin_installations::retain_transferred(
                            &mut leases,
                            &imported.plugins,
                        )?;
                        Ok((Box::new(imported), leases))
                    });
            let _ = sender.send(TransferEvent::Restored {
                transfer_id: worker_id,
                result,
                reply,
            });
        }) {
            let message = error.to_string();
            self.transfer_registry.update(|journal| {
                if let Some(record) = journal.receivers.get_mut(&transfer_id) {
                    record.phase = ReceivePhase::Aborted;
                    record.info.state = WorkspaceTransferState::Failed;
                    record.info.error = Some(message);
                }
                Ok(())
            })?;
            self.workspace_transfers.restoring.remove(&transfer_id);
            self.workspace_transfers
                .receive_deadlines
                .remove(&transfer_id);
            self.workspace_transfers.next_receive_deadline = self
                .workspace_transfers
                .receive_deadlines
                .values()
                .copied()
                .min();
            return Err(error);
        }
        Ok(None)
    }

    #[cfg(unix)]
    fn finish_received_restore(
        &mut self,
        id: &str,
        result: io::Result<(
            Box<crate::persist::workspace_transfer::ImportedWorkspace>,
            crate::plugin_installations::Leases,
        )>,
    ) -> io::Result<ReceiveRecord> {
        self.workspace_transfers.restoring.remove(id);
        let journal = self.transfer_registry.snapshot()?;
        let record = journal.receivers.get(id).cloned().ok_or_else(|| {
            io::Error::other("receiver transaction disappeared during restoration")
        })?;
        if record.phase != ReceivePhase::Prepared {
            return Err(io::Error::other(
                "workspace import was aborted before restoration completed",
            ));
        }
        let restored = result.and_then(|(imported, leases)| {
            self.validate_received_collisions(&imported, Some(id))?;
            Ok((imported, leases))
        });
        match restored {
            Ok((imported, leases)) => {
                self.app.plugin_installation_leases.extend(leases);
                self.workspace_transfers.staged.insert(id.into(), *imported);
                Ok(record)
            }
            Err(error) => {
                let message = error.to_string();
                self.transfer_registry.update(|journal| {
                    if let Some(record) = journal.receivers.get_mut(id) {
                        record.phase = ReceivePhase::Aborted;
                        record.info.state = WorkspaceTransferState::Failed;
                        record.info.error = Some(message);
                    }
                    Ok(())
                })?;
                self.workspace_transfers.receive_deadlines.remove(id);
                self.workspace_transfers.next_receive_deadline = self
                    .workspace_transfers
                    .receive_deadlines
                    .values()
                    .copied()
                    .min();
                Err(error)
            }
        }
    }

    #[cfg(unix)]
    fn control_received_workspace(
        &mut self,
        id: &str,
        source_session: &str,
        action: ControlAction,
    ) -> io::Result<ReceiveRecord> {
        let journal = self.transfer_registry.snapshot()?;
        let Some(mut record) = journal.receivers.get(id).cloned() else {
            if matches!(action, ControlAction::Abort) {
                self.transfer_registry.update(|journal| {
                    if journal
                        .aborted_receivers
                        .get(id)
                        .is_some_and(|source| source != source_session)
                    {
                        return Err(io::Error::other("transfer UUID belongs to another source"));
                    }
                    journal
                        .aborted_receivers
                        .insert(id.into(), source_session.into());
                    Ok(())
                })?;
            }
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "destination transfer was not found",
            ));
        };
        if record.source_session != source_session {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "transfer belongs to another source session",
            ));
        }
        if matches!(action, ControlAction::Settle) {
            if record.phase != ReceivePhase::Owned
                || !transfer::journal::source_released_workspace(&record)
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "source runtime release is not durably confirmed",
                ));
            }
            return self.record_receiver_settlement(id, record);
        }
        if matches!(record.phase, ReceivePhase::Activating | ReceivePhase::Owned)
            && record.owner_boot_id.as_deref() != Some(&self.client_shell_boot_id)
        {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "destination ownership belongs to an earlier runtime generation and cannot be confirmed"));
        }
        if record.phase == ReceivePhase::Prepared
            && (self.workspace_transfers.staged.contains_key(id)
                || self.workspace_transfers.restoring.contains(id))
        {
            self.workspace_transfers
                .receive_deadlines
                .insert(id.into(), Instant::now() + Duration::from_secs(120));
            self.workspace_transfers.next_receive_deadline = self
                .workspace_transfers
                .receive_deadlines
                .values()
                .copied()
                .min();
        }
        match action {
            ControlAction::Status => {}
            ControlAction::Abort if record.phase == ReceivePhase::Prepared => {
                record.phase = ReceivePhase::Aborted;
                record.info.state = WorkspaceTransferState::Failed;
                record.info.error = Some("source aborted before ownership commit".into());
                self.transfer_registry.update(|journal| {
                    journal.receivers.insert(id.into(), record.clone());
                    Ok(())
                })?;
                self.workspace_transfers.staged.remove(id);
                self.workspace_transfers.restoring.remove(id);
                self.workspace_transfers.receive_deadlines.remove(id);
                self.workspace_transfers.next_receive_deadline = self
                    .workspace_transfers
                    .receive_deadlines
                    .values()
                    .copied()
                    .min();
            }
            ControlAction::Commit if record.phase == ReceivePhase::Prepared => {
                if self.workspace_transfers.restoring.contains(id) {
                    return Ok(record);
                }
                let imported =
                    self.workspace_transfers.staged.get(id).ok_or_else(|| {
                        io::Error::other("prepared workspace runtime is unavailable")
                    })?;
                self.validate_received_collisions(imported, Some(id))?;
                if self.workspace_transfers.checkpoint_in_flight.is_some() {
                    return Ok(record);
                }
                let (job, previous_save) = self.app.receiver_checkpoint_job(imported);
                self.workspace_transfers.checkpoint_in_flight = Some(id.into());
                let transfer_id = id.to_owned();
                let sender = self.workspace_transfers.sender.clone();
                if let Err(error) =
                    crate::thread_spawn::spawn_named("herdr-transfer-checkpoint", move || {
                        let result = job();
                        let _ = sender.send(TransferEvent::CheckpointComplete {
                            transfer_id,
                            result,
                        });
                    })
                {
                    self.workspace_transfers.checkpoint_in_flight = None;
                    self.app.workspace_transfer_checkpoint_pending = false;
                    if let Some(previous) = previous_save {
                        self.app.session_save_thread = previous
                            .lock()
                            .map_err(|_| {
                                io::Error::other("previous session save handle is poisoned")
                            })?
                            .take();
                    }
                    return Err(error);
                }
            }
            ControlAction::Commit if record.phase == ReceivePhase::Activating => {
                return self.activate_received_workspace(id)
            }
            _ => {}
        }
        Ok(record)
    }

    #[cfg(unix)]
    fn record_receiver_settlement(
        &mut self,
        id: &str,
        mut record: ReceiveRecord,
    ) -> io::Result<ReceiveRecord> {
        if record.settled {
            return Ok(record);
        }
        if !self
            .app
            .state
            .workspaces
            .iter()
            .any(|workspace| Some(&workspace.id) == record.info.workspace_id.as_ref())
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "released receiver workspace is unavailable in this runtime generation",
            ));
        }
        record.settled = true;
        record.owner_boot_id = Some(self.client_shell_boot_id.clone());
        self.transfer_registry.update(|journal| {
            journal.receivers.insert(id.into(), record.clone());
            Ok(())
        })?;
        if let Some(workspace) = &record.info.workspace_id {
            self.app.state.workspace_transfer_fences.remove(workspace);
            self.replay_workspace_transfer_exits(workspace);
        }
        Ok(record)
    }

    #[cfg(unix)]
    fn finish_receiver_checkpoint(&mut self, id: &str, result: io::Result<()>) -> io::Result<()> {
        if self.workspace_transfers.checkpoint_in_flight.as_deref() != Some(id) {
            return Ok(());
        }
        self.workspace_transfers.checkpoint_in_flight = None;
        self.app.workspace_transfer_checkpoint_pending = false;
        let journal = self.transfer_registry.snapshot()?;
        let Some(mut record) = journal.receivers.get(id).cloned() else {
            return Err(io::Error::other(
                "receiver transaction disappeared while checkpointing",
            ));
        };
        if record.phase != ReceivePhase::Prepared {
            self.app.state.mark_session_dirty();
            self.app.sync_session_save_schedule();
            return Ok(());
        }
        let ready = result.and_then(|()| {
            let imported = self
                .workspace_transfers
                .staged
                .get(id)
                .ok_or_else(|| io::Error::other("prepared workspace runtime disappeared"))?;
            self.validate_received_collisions(imported, Some(id))
        });
        if let Err(error) = ready {
            record.phase = ReceivePhase::Aborted;
            record.info.state = WorkspaceTransferState::Failed;
            record.info.error = Some(format!("receiver could not checkpoint workspace: {error}"));
            self.transfer_registry.update(|journal| {
                journal.receivers.insert(id.into(), record);
                Ok(())
            })?;
            self.workspace_transfers.staged.remove(id);
            self.workspace_transfers.receive_deadlines.remove(id);
            self.workspace_transfers.next_receive_deadline = self
                .workspace_transfers
                .receive_deadlines
                .values()
                .copied()
                .min();
            self.app.state.mark_session_dirty();
            self.app.sync_session_save_schedule();
            return Ok(());
        }
        record.phase = ReceivePhase::Activating;
        record.owner_boot_id = Some(self.client_shell_boot_id.clone());
        self.transfer_registry.update(|journal| {
            journal.receivers.insert(id.into(), record);
            Ok(())
        })?;
        if let Some(imported) = self.workspace_transfers.staged.remove(id) {
            self.app
                .state
                .workspace_transfer_fences
                .insert(imported.workspace.id.clone());
            self.app.insert_transferred_workspace(imported);
        }
        self.workspace_transfers.receive_deadlines.remove(id);
        self.workspace_transfers.next_receive_deadline = self
            .workspace_transfers
            .receive_deadlines
            .values()
            .copied()
            .min();
        self.transfer_registry.sync_live_bindings(&self.app.state)?;
        self.activate_received_workspace(id)?;
        self.reconcile_client_shell_locations();
        Ok(())
    }

    #[cfg(unix)]
    fn activate_received_workspace(&mut self, id: &str) -> io::Result<ReceiveRecord> {
        let mut record = self
            .transfer_registry
            .read(|journal| journal.receivers.get(id).cloned())?
            .ok_or_else(|| io::Error::other("receiver ownership decision disappeared"))?;
        if record.phase != ReceivePhase::Activating {
            return Ok(record);
        }
        if record.owner_boot_id.as_deref() != Some(&self.client_shell_boot_id) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "receiver activation belongs to an earlier runtime generation",
            ));
        }
        let workspace_id =
            record.info.workspace_id.clone().ok_or_else(|| {
                io::Error::other("receiver activation omitted workspace identity")
            })?;
        let workspace = self
            .app
            .state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| io::Error::other("receiver activation lost its workspace"))?;
        let mut ready = true;
        for pane in workspace.tabs.iter().flat_map(|tab| tab.panes.values()) {
            let Some(runtime) = self.app.terminal_runtimes.get(&pane.attached_terminal_id) else {
                ready = false;
                continue;
            };
            runtime.set_handoff_reader_paused(false);
            ready &= runtime.try_send_bytes(Bytes::new()).is_ok();
        }
        if !ready {
            return Ok(record);
        }
        for pane in workspace.tabs.iter().flat_map(|tab| tab.panes.values()) {
            if let Some(runtime) = self.app.terminal_runtimes.get(&pane.attached_terminal_id) {
                runtime.reset_agent_detection();
            }
        }
        record.phase = ReceivePhase::Owned;
        record.info = transport::committed_info(&record);
        self.transfer_registry.update(|journal| {
            journal.receivers.insert(id.into(), record.clone());
            Ok(())
        })?;
        Ok(record)
    }

    #[cfg(unix)]
    fn set_source_drop_protection(&mut self, workspace_id: &str, protected: bool) {
        let terminal_ids: Vec<_> = self
            .app
            .state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .into_iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| {
                tab.panes
                    .values()
                    .map(|pane| pane.attached_terminal_id.clone())
            })
            .collect();
        for id in terminal_ids {
            if let Some(mut runtime) = self.app.terminal_runtimes.remove(&id) {
                runtime.set_transfer_drop_protection(protected);
                self.app.terminal_runtimes.insert(id, runtime);
            }
        }
    }

    #[cfg(unix)]
    fn decide_source_commit(&mut self, id: &str, record: ReceiveRecord) -> io::Result<()> {
        let journal = self.transfer_registry.snapshot()?;
        let source = journal
            .sources
            .get(id)
            .ok_or_else(|| io::Error::other("source transfer was not found"))?;
        if record.source_workspace != source.params.workspace_id
            || record.info.destination_session != source.params.destination_session
            || record.info.transfer_id != id
            || record.source_session != transfer::session_name()
        {
            return Err(io::Error::other(
                "prepared destination does not match source transaction",
            ));
        }
        if matches!(
            source.phase,
            SourcePhase::CommitDecided | SourcePhase::Committed
        ) && source
            .route
            .as_ref()
            .is_some_and(|route| route.destination_workspace == record.route.destination_workspace)
        {
            return Ok(());
        }
        if source.phase == SourcePhase::Failed || record.phase == ReceivePhase::Aborted {
            return Err(io::Error::other(
                "transfer no longer permits an ownership decision",
            ));
        }
        let mut route = transport::pending_route(&record);
        if !source
            .source_identities
            .iter()
            .all(|identity| route.identities.contains_key(identity))
        {
            return Err(io::Error::other(
                "receiver omitted reserved source identities",
            ));
        }
        for (name, terminal) in &source.source_names {
            let target = route
                .terminals
                .get(terminal)
                .ok_or_else(|| io::Error::other("receiver omitted a reserved agent terminal"))?;
            route.names.insert(name.clone(), target.clone());
        }
        for existing in journal.routes.values() {
            for name in existing.names.keys() {
                if let Some(terminal) = existing.terminal_for_name(name) {
                    if let Some(target) = route.terminals.get(terminal) {
                        route.names.insert(name.clone(), target.clone());
                    }
                }
            }
        }
        let local_ids: HashSet<_> = self
            .app
            .state
            .workspaces
            .iter()
            .filter(|workspace| workspace.id != source.params.workspace_id)
            .flat_map(|workspace| {
                std::iter::once(workspace.id.clone())
                    .chain(workspace.tabs.iter().map(|tab| {
                        crate::workspace::public_tab_id_for_number(&workspace.id, tab.number)
                    }))
                    .chain(workspace.public_pane_numbers.values().map(|number| {
                        crate::workspace::public_pane_id_for_number(&workspace.id, *number)
                    }))
                    .collect::<Vec<_>>()
            })
            .collect();
        if route.identities.keys().any(|id| {
            local_ids.contains(id)
                || (id.starts_with("p_")
                    && self.app.parse_pane_id(id).is_some_and(|(index, _)| {
                        self.app.state.workspaces[index].id != source.params.workspace_id
                    }))
        }) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "source alias collides with a live local resource",
            ));
        }
        if route.names.keys().any(|name| {
            journal
                .live_names
                .get(name)
                .is_some_and(|terminal| route.terminal_for_name(name) != Some(terminal.as_str()))
        }) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "source agent name collides with a live local resource",
            ));
        }
        if journal
            .routes
            .values()
            .any(|existing| route.conflicts(existing))
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "source forwarding identity collision",
            ));
        }
        self.transfer_registry.update(|journal| {
            let source = journal
                .sources
                .get_mut(id)
                .ok_or_else(|| io::Error::other("source transfer was not found"))?;
            source.phase = SourcePhase::CommitDecided;
            source.route = Some(route.clone());
            for identity in route.identities.keys() {
                let workspace = identity
                    .split_once(':')
                    .map_or(identity.as_str(), |(workspace, _)| workspace);
                if crate::workspace::public_workspace_number(workspace).is_some() {
                    crate::workspace::reserve_workspace_id(workspace);
                    journal.reserved_workspaces.insert(workspace.into());
                }
                if let Some(raw) = identity
                    .strip_prefix("p_")
                    .and_then(|raw| raw.parse::<u32>().ok())
                {
                    crate::layout::PaneId::reserve(raw);
                }
            }
            journal.install_route(route);
            Ok(())
        })?;
        self.set_source_drop_protection(&record.source_workspace, true);
        Ok(())
    }

    #[cfg(unix)]
    fn release_source_transfer(
        &mut self,
        id: &str,
        record: ReceiveRecord,
    ) -> io::Result<Vec<crate::terminal::TerminalRuntime>> {
        let journal = self.transfer_registry.snapshot()?;
        let source = journal
            .sources
            .get(id)
            .ok_or_else(|| io::Error::other("source transfer was not found"))?;
        if source.phase == SourcePhase::Committed {
            return Ok(Vec::new());
        }
        if record.phase != ReceivePhase::Owned
            || source.phase != SourcePhase::CommitDecided
            || record.info.transfer_id != id
            || record.source_session != transfer::session_name()
            || record.source_workspace != source.params.workspace_id
            || record.info.destination_session != source.params.destination_session
            || source.route.as_ref().is_none_or(|route| {
                Some(&route.destination_workspace) != record.info.workspace_id.as_ref()
            })
        {
            return Err(io::Error::other(
                "ownership acknowledgement does not match source transfer",
            ));
        }
        let workspace_id = source.params.workspace_id.clone();
        let mut released = Vec::new();
        if self
            .app
            .state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == workspace_id)
        {
            let (panes, runtimes) = self
                .app
                .release_transferred_workspace(&workspace_id)
                .map_err(io::Error::other)?;
            released = runtimes;
            self.interactive_input_deadlines
                .retain(|pane, _| !panes.contains(pane));
        }
        self.app
            .state
            .workspace_transfer_fences
            .insert(workspace_id);
        self.transfer_registry.sync_live_bindings(&self.app.state)?;
        self.reconcile_client_shell_locations();
        self.app.sync_session_save_schedule();
        Ok(released)
    }

    #[cfg(unix)]
    fn record_source_release(&mut self, id: &str) -> io::Result<()> {
        self.transfer_registry.update(|journal| {
            let source = journal
                .sources
                .get_mut(id)
                .ok_or_else(|| io::Error::other("source transfer was not found"))?;
            if !matches!(
                source.phase,
                SourcePhase::CommitDecided | SourcePhase::Committed
            ) || source.route.is_none()
                || self
                    .app
                    .state
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == source.params.workspace_id)
            {
                return Err(io::Error::other("source workspace has not been released"));
            }
            source.runtime_released = true;
            Ok(())
        })
    }

    #[cfg(unix)]
    fn finish_source_transfer(&mut self, id: &str, record: ReceiveRecord) -> io::Result<()> {
        let journal = self.transfer_registry.snapshot()?;
        let source = journal
            .sources
            .get(id)
            .ok_or_else(|| io::Error::other("source transfer was not found"))?;
        if source.phase == SourcePhase::Committed {
            return Ok(());
        }
        if record.phase != ReceivePhase::Owned
            || !record.settled
            || !source.runtime_released
            || source.phase != SourcePhase::CommitDecided
            || record.info.transfer_id != id
            || record.source_session != transfer::session_name()
            || record.source_workspace != source.params.workspace_id
            || record.info.destination_session != source.params.destination_session
        {
            return Err(io::Error::other(
                "receiver settlement does not match source transfer",
            ));
        }
        let mut route = source
            .route
            .clone()
            .ok_or_else(|| io::Error::other("source forwarding decision disappeared"))?;
        if Some(&route.destination_workspace) != record.info.workspace_id.as_ref()
            || journal
                .routes
                .iter()
                .any(|(other_id, other)| other_id != id && route.conflicts(other))
        {
            return Err(io::Error::other(
                "source forwarding namespace collision during settlement",
            ));
        }
        route.ready = true;
        let workspace_id = source.params.workspace_id.clone();
        self.transfer_registry.update(|journal| {
            let source = journal
                .sources
                .get_mut(id)
                .ok_or_else(|| io::Error::other("source transfer was not found"))?;
            source.phase = SourcePhase::Committed;
            source.info = transport::committed_info(&record);
            source.route = Some(route.clone());
            journal.install_route(route);
            Ok(())
        })?;
        self.app
            .state
            .workspace_transfer_fences
            .remove(&workspace_id);
        self.workspace_transfers
            .deferred_exits
            .retain(|(id, _)| id != &workspace_id);
        Ok(())
    }

    #[cfg(unix)]
    fn fail_source_transfer(&mut self, id: &str, error: String) -> io::Result<()> {
        let journal = self.transfer_registry.snapshot()?;
        let source = journal
            .sources
            .get(id)
            .ok_or_else(|| io::Error::other("source transfer was not found"))?;
        if source.phase == SourcePhase::Failed {
            return Ok(());
        }
        if source.phase == SourcePhase::Committed || source.runtime_released {
            return Err(io::Error::other(
                "cannot roll back released destination ownership",
            ));
        }
        let workspace_id = source.params.workspace_id.clone();
        self.transfer_registry.update(|journal| {
            let source = journal
                .sources
                .get_mut(id)
                .ok_or_else(|| io::Error::other("source transfer was not found"))?;
            source.phase = SourcePhase::Failed;
            source.info.state = WorkspaceTransferState::Failed;
            source.info.error = Some(error);
            source.route = None;
            journal.remove_route(id);
            Ok(())
        })?;
        self.set_source_drop_protection(&workspace_id, false);
        self.app
            .state
            .workspace_transfer_fences
            .remove(&workspace_id);
        self.replay_workspace_transfer_exits(&workspace_id);
        Ok(())
    }

    #[cfg(unix)]
    fn replay_workspace_transfer_exits(&mut self, workspace_id: &str) {
        let deferred = std::mem::take(&mut self.workspace_transfers.deferred_exits);
        for (id, event) in deferred {
            if id == workspace_id {
                self.handle_internal_event_with_forwarding(event);
            } else {
                self.workspace_transfers.deferred_exits.push((id, event));
            }
        }
    }

    pub(super) fn workspace_transfers_preclude_handoff(&self) -> bool {
        #[cfg(unix)]
        if !self.workspace_transfers.staged.is_empty()
            || !self.workspace_transfers.restoring.is_empty()
        {
            return true;
        }
        !self.app.state.workspace_transfer_fences.is_empty()
            || self
                .transfer_registry
                .read(|journal| {
                    journal
                        .receivers
                        .values()
                        .any(|record| record.phase == ReceivePhase::Owned && !record.settled)
                })
                .unwrap_or(true)
    }

    #[cfg(unix)]
    pub(super) fn suspend_workspace_transfer_listener(&mut self) -> io::Result<()> {
        self.transfer_registry.suspend_updates(true)?;
        self.workspace_transfers._listener.take();
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn restore_workspace_transfer_listener(&mut self) -> io::Result<()> {
        self.transfer_registry.suspend_updates(false)?;
        if self.workspace_transfers.listen_enabled && self.workspace_transfers._listener.is_none() {
            self.workspace_transfers._listener =
                Some(transport::start(self.workspace_transfers.sender.clone())?);
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn start_workspace_transfer_recovery(&mut self) -> io::Result<()> {
        self.app
            .state
            .workspace_transfer_fences
            .extend(self.transfer_registry.pending_workspaces()?.into_keys());
        self.app.state.workspace_transfer_fences.extend(
            self.transfer_registry
                .snapshot()?
                .receivers
                .into_values()
                .filter(|record| record.phase == ReceivePhase::Owned && !record.settled)
                .filter_map(|record| record.info.workspace_id),
        );
        for source in self
            .transfer_registry
            .snapshot()?
            .sources
            .into_values()
            .filter(|source| {
                matches!(
                    source.phase,
                    SourcePhase::Preparing | SourcePhase::CommitDecided
                )
            })
        {
            let sender = self.workspace_transfers.sender.clone();
            let stopped = self.should_quit.clone();
            crate::thread_spawn::spawn_named("herdr-transfer-recover", move || {
                transport::recover_source(
                    source.params,
                    sender,
                    stopped,
                    source.phase == SourcePhase::Preparing || source.route.is_none(),
                    source.runtime_released,
                )
            })?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn workspace_forwarding_depth(
        &self,
        workspace_id: &str,
        journal: &transfer::journal::Journal,
    ) -> u8 {
        let terminals: HashSet<_> = self
            .app
            .state
            .workspaces
            .iter()
            .filter(|workspace| workspace.id == workspace_id)
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| {
                tab.panes
                    .values()
                    .map(|pane| pane.attached_terminal_id.as_str())
            })
            .collect();
        journal
            .receivers
            .values()
            .filter(|record| {
                record.phase == ReceivePhase::Owned
                    && (record.info.workspace_id.as_deref() == Some(workspace_id)
                        || record
                            .route
                            .identities
                            .keys()
                            .any(|id| terminals.contains(id.as_str())))
            })
            .map(|record| record.forwarding_depth)
            .max()
            .unwrap_or(0)
    }

    #[cfg(unix)]
    pub(super) fn defer_workspace_transfer_exit(&mut self, event: AppEvent) -> Option<AppEvent> {
        if self.app.state.workspace_transfer_fences.is_empty() {
            return Some(event);
        }
        let pane_id = match &event {
            AppEvent::PaneDied { pane_id, .. }
            | AppEvent::WorktreeRuntimeRestoreFailed { pane_id, .. } => *pane_id,
            _ => return Some(event),
        };
        let workspace_id = self
            .app
            .find_pane(pane_id)
            .and_then(|(index, _)| self.app.state.workspaces.get(index))
            .filter(|workspace| {
                self.app
                    .state
                    .workspace_transfer_fences
                    .contains(&workspace.id)
            })
            .map(|workspace| workspace.id.clone());
        if let Some(id) = workspace_id {
            self.workspace_transfers.deferred_exits.push((id, event));
            None
        } else {
            Some(event)
        }
    }

    #[cfg(unix)]
    fn expire_prepared_workspace_transfers(&mut self, now: Instant) -> bool {
        if self
            .workspace_transfers
            .next_receive_deadline
            .is_none_or(|deadline| deadline > now)
        {
            return false;
        }
        let expired: Vec<_> = self
            .workspace_transfers
            .receive_deadlines
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        let mut changed = false;
        for id in expired {
            let result = self.transfer_registry.update(|journal| {
                if let Some(record) = journal
                    .receivers
                    .get_mut(&id)
                    .filter(|record| record.phase == ReceivePhase::Prepared)
                {
                    record.phase = ReceivePhase::Aborted;
                    record.info.state = WorkspaceTransferState::Failed;
                    record.info.error = Some(
                        "source stopped renewing the prepared transfer lease before ownership"
                            .into(),
                    );
                }
                Ok(())
            });
            match result {
                Ok(()) => {
                    self.workspace_transfers.staged.remove(&id);
                    self.workspace_transfers.restoring.remove(&id);
                    self.workspace_transfers.receive_deadlines.remove(&id);
                    changed = true;
                }
                Err(error) => {
                    tracing::warn!(%id, %error, "could not expire nonowning workspace import");
                    self.workspace_transfers
                        .receive_deadlines
                        .insert(id, now + Duration::from_secs(5));
                }
            }
        }
        self.workspace_transfers.next_receive_deadline = self
            .workspace_transfers
            .receive_deadlines
            .values()
            .copied()
            .min();
        changed
    }

    pub(super) fn compute_view_preserving_transfers(
        &mut self,
        area: Rect,
        cell_size: crate::kitty_graphics::HostCellSize,
    ) {
        crate::ui::compute_view_without_resizing_panes(
            &mut self.app.state,
            &self.app.terminal_runtimes,
            area,
        );
        for (workspace_index, workspace) in self.app.state.workspaces.iter().enumerate() {
            if self.app.state.workspace_transfer_fenced(workspace_index) {
                continue;
            }
            for tab_index in 0..workspace.tabs.len() {
                crate::ui::resize_tab_surface(
                    &self.app.state,
                    &self.app.terminal_runtimes,
                    workspace_index,
                    tab_index,
                    area,
                    cell_size,
                );
            }
        }
        let _ = resize_popup_runtime(&self.app, area, cell_size);
    }

    pub(super) fn terminal_is_transfer_fenced(&self, terminal_id: &str) -> bool {
        if self.app.state.workspace_transfer_fences.is_empty() {
            return false;
        }
        self.app
            .state
            .workspaces
            .iter()
            .enumerate()
            .any(|(index, workspace)| {
                self.app.state.workspace_transfer_fenced(index)
                    && workspace.tabs.iter().any(|tab| {
                        tab.panes
                            .values()
                            .any(|pane| pane.attached_terminal_id.as_str() == terminal_id)
                    })
            })
    }

    fn client_terminal_is_transfer_fenced(&self, client_id: u64) -> bool {
        self.clients
            .get(&client_id)
            .is_some_and(|client| match &client.mode {
                ClientConnectionMode::TerminalAttach { terminal_id } => {
                    self.terminal_is_transfer_fenced(terminal_id)
                }
                _ => false,
            })
    }

    pub(super) fn server_event_is_transfer_fenced(&self, event: &ServerEvent) -> bool {
        if self.app.state.workspace_transfer_fences.is_empty() {
            return false;
        }
        match event {
            ServerEvent::ClientShellPaneInput { pane_id, .. } => self
                .app
                .parse_pane_id(pane_id)
                .is_some_and(|(index, _)| self.app.state.workspace_transfer_fenced(index)),
            ServerEvent::ClientInput { client_id, .. }
            | ServerEvent::ClientAttachMouse { client_id, .. }
            | ServerEvent::ClientAttachScroll { client_id, .. }
            | ServerEvent::ClientResize { client_id, .. } => {
                self.client_terminal_is_transfer_fenced(*client_id)
            }
            ServerEvent::ClientClipboardImage {
                client_id, target, ..
            } => match target {
                protocol::ClientClipboardImageTarget::Pane(pane_id) => self
                    .app
                    .parse_pane_id(pane_id)
                    .is_some_and(|(index, _)| self.app.state.workspace_transfer_fenced(index)),
                protocol::ClientClipboardImageTarget::DirectTerminal => {
                    self.client_terminal_is_transfer_fenced(*client_id)
                }
                _ => false,
            },
            _ => false,
        }
    }

    pub(super) fn reserve_local_transfer_name(
        &self,
        request: &api::schema::Request,
    ) -> io::Result<()> {
        let (name, target) = match &request.method {
            api::schema::Method::AgentStart(params) => {
                (Some(params.name.as_str()), params.pane_id.as_str())
            }
            api::schema::Method::AgentRename(params) => {
                (params.name.as_deref(), params.target.as_str())
            }
            _ => return Ok(()),
        };
        let Some(name) = name else { return Ok(()) };
        let terminal_id = self
            .app
            .parse_pane_id(target)
            .and_then(|(index, pane)| {
                self.app
                    .state
                    .workspaces
                    .get(index)
                    .and_then(|workspace| workspace.terminal_id(pane))
            })
            .map(ToString::to_string)
            .or_else(|| {
                self.app
                    .state
                    .terminals
                    .values()
                    .find(|terminal| terminal.agent_name.as_deref() == Some(target))
                    .map(|terminal| terminal.id.to_string())
            });
        if let Some(id) = terminal_id {
            self.transfer_registry.reserve_local_name(name, id)?;
        }
        Ok(())
    }
}
