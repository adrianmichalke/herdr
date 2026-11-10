use super::*;
use std::time::Instant;

use endpoint::{ClientEndpointId, EndpointSessionTarget, ProfileId, SavedSshEndpoint};

const PREPARATION_TIMEOUT: Duration = Duration::from_secs(120);
const METADATA_TIMEOUT: Duration = Duration::from_secs(15);

#[path = "workspace_transfer.rs"]
mod workspace_transfer;
use workspace_transfer::{TransferFollow, WorkspaceTransfer};
mod history;
use history::{HistorySessionConnection, HistorySessionOpen};

#[derive(Debug)]
pub(super) enum SessionAction {
    List {
        machine: ClientEndpointId,
        request_id: u64,
    },
    Open {
        machine: ClientEndpointId,
        request_id: u64,
        session: String,
        create: bool,
    },
    Transfer {
        source: shell::WorkspaceTransferSource,
        request_id: u64,
        destination_session: String,
    },
    Cancel {
        request_id: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MachineFingerprint {
    pub(super) machine: ClientEndpointId,
    pub(super) target: Option<String>,
    pub(super) configured_session: String,
}

impl MachineFingerprint {
    pub(super) fn read(
        machine: &ClientEndpointId,
        catalog: &endpoint::EndpointCatalog,
        shell: &shell::ClientShellState,
    ) -> Result<Self, String> {
        let machine = machine.machine_id();
        if let Some(profile_id) = machine.ssh_profile_id() {
            let profile = catalog
                .ssh
                .iter()
                .find(|profile| &profile.id == profile_id && profile.enabled)
                .ok_or("The selected machine is no longer enabled")?;
            Ok(Self {
                machine: machine.clone(),
                target: Some(profile.target.clone()),
                configured_session: profile.session.clone(),
            })
        } else {
            Ok(Self {
                machine,
                target: shell.root_remote_target().map(str::to_owned),
                configured_session: shell.initial_session_name().to_owned(),
            })
        }
    }

    fn connection_target(&self, session: &str, root_profile: &ProfileId) -> EndpointSessionTarget {
        match &self.target {
            Some(target) => EndpointSessionTarget::Ssh(SavedSshEndpoint {
                id: self
                    .machine
                    .ssh_profile_id()
                    .unwrap_or(root_profile)
                    .clone(),
                label: target.clone(),
                target: target.clone(),
                session: session.to_owned(),
                enabled: true,
            }),
            None => EndpointSessionTarget::Local(session.to_owned()),
        }
    }

    fn list(
        &self,
        root_profile: &ProfileId,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Vec<crate::session::SessionInfo>, String> {
        if cancelled.load(Ordering::Acquire) {
            return Err("Session discovery cancelled".into());
        }
        let result = match &self.target {
            Some(target) => crate::remote::list_saved_sessions(
                self.machine
                    .ssh_profile_id()
                    .unwrap_or(root_profile)
                    .as_str(),
                target,
                &self.configured_session,
                Arc::clone(&cancelled),
            ),
            None => crate::session::list_sessions(),
        }
        .map_err(|error| error.to_string());
        if cancelled.load(Ordering::Acquire) {
            return Err("Session discovery cancelled".into());
        }
        result
    }

    fn endpoint_id(&self, session: &str) -> Result<ClientEndpointId, String> {
        crate::session::parse_target_name(session)?;
        if session == self.configured_session {
            Ok(self.machine.clone())
        } else {
            self.machine.with_session(session)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SessionRequestKey {
    request_id: u64,
    fingerprint: MachineFingerprint,
}

impl SessionRequestKey {
    fn is_current(&self, context: &SessionSwitchContext<'_>) -> bool {
        context.state.shell.as_ref().is_some_and(|shell| {
            MachineFingerprint::read(&self.fingerprint.machine, context.catalog, shell)
                .is_ok_and(|current| current == self.fingerprint)
        })
    }
}

pub(super) enum SessionSwitchEvent {
    Listed {
        key: SessionRequestKey,
        result: Result<Vec<crate::session::SessionInfo>, String>,
    },
    Prepared {
        key: SessionRequestKey,
        generation: u64,
        result: Result<endpoint::EndpointSupervisorEvent, String>,
    },
}

struct Discovery {
    key: SessionRequestKey,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreparationPhase {
    Preparing,
    Metadata,
    Activating,
}

struct Preparation {
    key: SessionRequestKey,
    endpoint_id: ClientEndpointId,
    generation: u64,
    target: EndpointSessionTarget,
    staged: bool,
    owns_connection: bool,
    allow_start: bool,
    cancelled: Arc<AtomicBool>,
    phase: PreparationPhase,
    deadline: Instant,
    follow: Option<TransferFollow>,
    activation_serial: Option<u64>,
}

struct QueuedOpen {
    key: SessionRequestKey,
    session: String,
    create: bool,
    follow: Option<TransferFollow>,
}

pub(super) struct SessionSwitchCoordinator {
    root_profile: ProfileId,
    work_slots: Arc<tokio::sync::Semaphore>,
    latest_serial: u64,
    latest_profiles: Option<Vec<SavedSshEndpoint>>,
    discovery: Option<Discovery>,
    preparation: Option<Preparation>,
    queued: Option<QueuedOpen>,
    transfer: Option<WorkspaceTransfer>,
    history_open: Option<HistorySessionOpen>,
    history_connection: Option<HistorySessionConnection>,
    history_result: Option<(u64, Result<(), String>)>,
}

impl Default for SessionSwitchCoordinator {
    fn default() -> Self {
        Self {
            root_profile: ProfileId::generate(),
            work_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            latest_serial: 0,
            latest_profiles: None,
            discovery: None,
            preparation: None,
            queued: None,
            transfer: None,
            history_open: None,
            history_connection: None,
            history_result: None,
        }
    }
}

pub(super) fn connect_options(
    state: &ClientState,
    endpoint_keybindings: bool,
) -> Option<endpoint::EndpointConnectOptions> {
    let shell = state.shell.as_ref()?;
    Some(endpoint::EndpointConnectOptions {
        cols: state.reported_size.0,
        rows: state.reported_size.1,
        cell_width_px: state.reported_cell_size.0,
        cell_height_px: state.reported_cell_size.1,
        pixel_geometry_exact: state.pixel_geometry_exact,
        pixel_mouse: state.pixel_mouse_available(),
        surface_size: shell.surface_size(state.reported_size.0, state.reported_size.1),
        endpoint_keybindings,
        mouse_capture: state.shell_mouse_capture_preference,
    })
}

pub(super) struct SessionSwitchContext<'a> {
    pub(super) state: &'a mut ClientState,
    pub(super) endpoints: &'a mut endpoint::EndpointRegistry,
    pub(super) commands: &'a mut endpoint_commands::EndpointCommands,
    pub(super) supervisors: &'a mut endpoint::EndpointSupervisors,
    pub(super) catalog: &'a mut endpoint::EndpointCatalog,
    pub(super) activation: &'a mut Option<endpoint::PendingEndpointActivation>,
    pub(super) next_surface_serial: &'a mut u64,
    pub(super) event_tx: &'a tokio::sync::mpsc::Sender<ClientLoopEvent>,
    pub(super) options: endpoint::EndpointConnectOptions,
}

impl SessionSwitchContext<'_> {
    pub(super) fn repaint(&mut self) {
        let frame = self.state.shell.as_mut().and_then(|shell| {
            shell.compose(self.state.reported_size.0, self.state.reported_size.1)
        });
        if let Some(frame) = frame {
            self.state.present_frozen_chrome(frame);
        }
    }

    fn finish(&mut self, request_id: u64, result: Result<(), String>) {
        if let Some(shell) = self.state.shell.as_mut() {
            if shell.finish_session_switch(request_id, result) {
                self.repaint();
            }
        }
    }

    fn discard(&mut self, preparation: Preparation) {
        preparation.cancelled.store(true, Ordering::Release);
        if preparation.owns_connection
            && self
                .endpoints
                .accepts(&preparation.endpoint_id, preparation.generation)
        {
            self.endpoints.disconnect(&preparation.endpoint_id);
        }
        let cancelled = self.commands.disconnect(&preparation.endpoint_id);
        if preparation.owns_connection {
            #[cfg(unix)]
            self.state
                .forget_endpoint_graphics(&preparation.endpoint_id);
        }
        if let Some(shell) = self.state.shell.as_mut() {
            for request_id in cancelled {
                shell.cancel_endpoint_request(&request_id);
            }
            if preparation.staged {
                shell.remove_session_endpoint(&preparation.endpoint_id);
            } else if preparation.owns_connection {
                shell.mark_endpoint_disconnected(&preparation.endpoint_id);
            }
        }
        if !preparation.staged && preparation.owns_connection && preparation.allow_start {
            let configured = preparation
                .endpoint_id
                .ssh_profile_id()
                .and_then(|id| self.catalog.ssh.iter().find(|profile| &profile.id == id))
                .cloned();
            self.supervisors.retry_session(
                preparation.endpoint_id,
                preparation.target,
                configured,
                Instant::now(),
            );
        }
    }
}

impl SessionSwitchCoordinator {
    fn fingerprint(
        &self,
        machine: &ClientEndpointId,
        context: &SessionSwitchContext<'_>,
    ) -> Result<MachineFingerprint, String> {
        let shell = context
            .state
            .shell
            .as_ref()
            .ok_or("Client shell is unavailable")?;
        let current = MachineFingerprint::read(machine, context.catalog, shell)?;
        let pending_change = self.latest_profiles.as_ref().is_some_and(|profiles| {
            current.machine.ssh_profile_id().is_some_and(|id| {
                !profiles.iter().any(|profile| {
                    &profile.id == id
                        && profile.enabled
                        && Some(&profile.target) == current.target.as_ref()
                        && profile.session == current.configured_session
                })
            })
        });
        if pending_change {
            Err("Machine settings are being applied; please try again".into())
        } else {
            Ok(current)
        }
    }

    pub(super) fn catalog_changed(
        &mut self,
        profiles: &[SavedSshEndpoint],
        context: &mut SessionSwitchContext<'_>,
    ) {
        self.latest_profiles = Some(profiles.to_vec());
        let changed = |key: &SessionRequestKey| {
            key.fingerprint.machine.ssh_profile_id().is_some_and(|id| {
                !profiles.iter().any(|profile| {
                    &profile.id == id
                        && profile.enabled
                        && Some(&profile.target) == key.fingerprint.target.as_ref()
                        && profile.session == key.fingerprint.configured_session
                })
            })
        };
        if self
            .discovery
            .as_ref()
            .is_some_and(|discovery| changed(&discovery.key))
        {
            if let Some(discovery) = self.discovery.take() {
                discovery.cancelled.store(true, Ordering::Release);
                if let Some(shell) = context.state.shell.as_mut() {
                    shell.receive_session_list(
                        &discovery.key.fingerprint.machine,
                        discovery.key.request_id,
                        Err("The selected machine configuration changed".into()),
                    );
                }
                context.repaint();
            }
        }
        if self
            .queued
            .as_ref()
            .is_some_and(|queued| changed(&queued.key))
        {
            if let Some(queued) = self.queued.take() {
                self.finish(
                    context,
                    queued.key.request_id,
                    Err("The selected machine configuration changed".into()),
                );
            }
        }
        if self
            .preparation
            .as_ref()
            .is_some_and(|preparation| changed(&preparation.key))
        {
            let request_id = self
                .preparation
                .as_ref()
                .expect("changed session preparation")
                .key
                .request_id;
            self.cancel(request_id, context);
            self.finish(
                context,
                request_id,
                Err("The selected machine configuration changed".into()),
            );
        }
    }

    pub(super) fn cancel_for_navigation(&mut self, context: &mut SessionSwitchContext<'_>) {
        if let Some(shell) = context.state.shell.as_mut() {
            shell.note_session_navigation();
        }
        if let Some(queued) = self.queued.take() {
            self.finish(
                context,
                queued.key.request_id,
                Err("Session switch cancelled".into()),
            );
        }
        if let Some(preparation) = &self.preparation {
            self.cancel(preparation.key.request_id, context);
        }
    }

    pub(super) fn busy(&self) -> bool {
        self.preparation.is_some() || self.queued.is_some() || self.history_connection.is_some()
    }

    pub(super) fn actions(
        &mut self,
        actions: Vec<SessionAction>,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), ClientError> {
        self.cancel_obsolete_history(context);
        for action in actions {
            match action {
                SessionAction::Cancel { request_id } => self.cancel(request_id, context),
                SessionAction::Transfer {
                    source,
                    request_id,
                    destination_session,
                } => {
                    if request_id <= self.latest_serial {
                        continue;
                    }
                    self.latest_serial = request_id;
                    self.cancel_discovery();
                    self.start_transfer(source, request_id, destination_session, context);
                }
                SessionAction::List {
                    machine,
                    request_id,
                } => {
                    if request_id <= self.latest_serial {
                        continue;
                    }
                    self.latest_serial = request_id;
                    self.cancel_discovery();
                    let fingerprint = self.fingerprint(&machine, context);
                    match fingerprint {
                        Ok(fingerprint) => {
                            let key = SessionRequestKey {
                                request_id,
                                fingerprint,
                            };
                            let cancelled = Arc::new(AtomicBool::new(false));
                            self.discovery = Some(Discovery {
                                key: key.clone(),
                                cancelled: Arc::clone(&cancelled),
                            });
                            let root_profile = self.root_profile.clone();
                            let work_slots = Arc::clone(&self.work_slots);
                            let event_tx = context.event_tx.clone();
                            tokio::spawn(async move {
                                let Ok(_permit) = work_slots.acquire_owned().await else {
                                    return;
                                };
                                let task_key = key.clone();
                                let result = tokio::task::spawn_blocking(move || {
                                    task_key.fingerprint.list(&root_profile, cancelled)
                                })
                                .await
                                .unwrap_or_else(|error| Err(error.to_string()));
                                let _ = event_tx
                                    .send(ClientLoopEvent::SessionSwitch(
                                        SessionSwitchEvent::Listed { key, result },
                                    ))
                                    .await;
                            });
                        }
                        Err(error) => {
                            if let Some(shell) = context.state.shell.as_mut() {
                                if shell.receive_session_list(
                                    &machine.machine_id(),
                                    request_id,
                                    Err(error),
                                ) {
                                    context.repaint();
                                }
                            }
                        }
                    }
                }
                SessionAction::Open {
                    machine,
                    request_id,
                    session,
                    create,
                } => {
                    if request_id <= self.latest_serial {
                        continue;
                    }
                    self.latest_serial = request_id;
                    self.cancel_discovery();
                    let fingerprint = self.fingerprint(&machine, context);
                    let fingerprint = match fingerprint {
                        Ok(fingerprint) => fingerprint,
                        Err(error) => {
                            self.finish(context, request_id, Err(error));
                            continue;
                        }
                    };
                    if let Some(previous) = &self.preparation {
                        self.cancel(previous.key.request_id, context);
                    }
                    if let Some(activation) = context.activation.as_mut() {
                        activation.clear_successor();
                    }
                    self.queued = Some(QueuedOpen {
                        key: SessionRequestKey {
                            request_id,
                            fingerprint,
                        },
                        session,
                        create,
                        follow: None,
                    });
                }
            }
        }
        self.drive(context)
    }

    fn cancel_discovery(&mut self) {
        if let Some(discovery) = self.discovery.take() {
            discovery.cancelled.store(true, Ordering::Release);
        }
    }

    fn cancel(&mut self, request_id: u64, context: &mut SessionSwitchContext<'_>) {
        if self
            .discovery
            .as_ref()
            .is_some_and(|discovery| discovery.key.request_id == request_id)
        {
            self.cancel_discovery();
        }
        if self
            .queued
            .as_ref()
            .is_some_and(|queued| queued.key.request_id == request_id)
        {
            self.queued = None;
        }
        let Some(preparation) = self
            .preparation
            .as_mut()
            .filter(|preparation| preparation.key.request_id == request_id)
        else {
            return;
        };
        if preparation.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        if preparation.phase == PreparationPhase::Activating {
            if context
                .activation
                .as_ref()
                .is_some_and(|activation| activation.restoring_source())
            {
                return;
            }
            rollback_endpoint_activation(
                context.state,
                context.endpoints,
                context.activation,
                "Session switch cancelled".into(),
                false,
            );
        } else if let Some(preparation) = self.preparation.take() {
            context.discard(preparation);
        }
    }

    fn start(&mut self, queued: QueuedOpen, context: &mut SessionSwitchContext<'_>) {
        let request_id = queued.key.request_id;
        if queued
            .follow
            .as_ref()
            .is_some_and(|follow| !follow.is_current(request_id, false, context))
        {
            self.finish(
                context,
                request_id,
                Err("The view changed; open the destination session manually".into()),
            );
            return;
        }
        if !self.history_is_current(request_id, context)
            || !queued.key.is_current(context)
            || self
                .fingerprint(&queued.key.fingerprint.machine, context)
                .is_err()
        {
            self.finish(
                context,
                request_id,
                Err("The selected machine configuration changed".into()),
            );
            return;
        }
        let endpoint_id = match queued.key.fingerprint.endpoint_id(&queued.session) {
            Ok(endpoint_id) => endpoint_id,
            Err(error) => {
                self.finish(context, request_id, Err(error));
                return;
            }
        };
        let current =
            context.state.shell.as_ref().and_then(|shell| {
                shell.visible_endpoint_for_machine(&queued.key.fingerprint.machine)
            });
        let retained_binding = current
            .filter(|current| current.session_name == queued.session)
            .map(|current| current.endpoint_id.clone());
        if retained_binding.is_some() && queued.create {
            self.finish(
                context,
                request_id,
                Err("A session with this name already exists".into()),
            );
            return;
        }
        let allow_start = self.history_allow_start(request_id).unwrap_or(true);
        let endpoint_id = retained_binding.clone().unwrap_or(endpoint_id);
        let target = queued
            .key
            .fingerprint
            .connection_target(&queued.session, &self.root_profile);
        if retained_binding.is_some()
            && context.endpoints.active_id() == &endpoint_id
            && context.endpoints.active_surface_available()
        {
            self.finish(context, request_id, Ok(()));
            return;
        }
        if retained_binding.is_some() {
            if let Some(connection) = context.endpoints.connection(&endpoint_id) {
                self.preparation = Some(Preparation {
                    key: queued.key,
                    endpoint_id,
                    generation: connection.generation,
                    target,
                    staged: false,
                    owns_connection: false,
                    allow_start,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    phase: PreparationPhase::Metadata,
                    deadline: Instant::now() + METADATA_TIMEOUT,
                    follow: queued.follow,
                    activation_serial: None,
                });
                return;
            }
        }
        if retained_binding.is_none()
            && !context.state.shell.as_mut().is_some_and(|shell| {
                shell.stage_session_endpoint(
                    &queued.key.fingerprint.machine,
                    endpoint_id.clone(),
                    queued.session.clone(),
                )
            })
        {
            self.finish(
                context,
                request_id,
                Err("The selected session cannot be staged".into()),
            );
            return;
        }
        let generation = context.supervisors.reserve_generation();
        let cancelled = Arc::new(AtomicBool::new(false));
        if retained_binding.is_some() {
            context.supervisors.remove(&endpoint_id);
            if let Some(shell) = context.state.shell.as_mut() {
                shell.set_endpoint_status(&endpoint_id, endpoint::ClientEndpointStatus::Connecting);
            }
        }
        self.preparation = Some(Preparation {
            key: queued.key.clone(),
            endpoint_id: endpoint_id.clone(),
            generation,
            target: target.clone(),
            staged: retained_binding.is_none(),
            owns_connection: true,
            allow_start,
            cancelled: Arc::clone(&cancelled),
            phase: PreparationPhase::Preparing,
            deadline: Instant::now() + PREPARATION_TIMEOUT,
            follow: queued.follow,
            activation_serial: None,
        });
        let options = context.options;
        let root_profile = self.root_profile.clone();
        let work_slots = Arc::clone(&self.work_slots);
        let event_tx = context.event_tx.clone();
        tokio::spawn(async move {
            let Ok(_permit) = work_slots.acquire_owned().await else {
                return;
            };
            let key = queued.key;
            let task_key = key.clone();
            let result = tokio::task::spawn_blocking(move || {
                if queued.create {
                    let sessions = task_key
                        .fingerprint
                        .list(&root_profile, Arc::clone(&cancelled))?;
                    if queued.session == crate::session::DEFAULT_SESSION_NAME
                        || sessions
                            .iter()
                            .any(|session| session.name == queued.session)
                    {
                        return Err("A session with this name already exists".into());
                    }
                }
                if allow_start {
                    endpoint::connect_session_once(
                        target,
                        options,
                        endpoint_id,
                        generation,
                        cancelled,
                    )
                } else {
                    history::connect_existing(target, options, endpoint_id, generation, cancelled)
                }
                .map_err(|error| error.to_string())
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
            let _ = event_tx
                .send(ClientLoopEvent::SessionSwitch(
                    SessionSwitchEvent::Prepared {
                        key,
                        generation,
                        result,
                    },
                ))
                .await;
        });
    }

    pub(super) fn receive_transfer_result(
        &mut self,
        completed: endpoint_commands::EndpointCommandResult,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), ClientError> {
        self.receive_transfer(completed, context);
        self.drive(context)
    }

    pub(super) fn receive(
        &mut self,
        event: SessionSwitchEvent,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), ClientError> {
        match event {
            SessionSwitchEvent::Listed { key, result } => {
                if !self.discovery.as_ref().is_some_and(|discovery| {
                    discovery.key == key && !discovery.cancelled.load(Ordering::Acquire)
                }) {
                    return Ok(());
                }
                self.discovery = None;
                let result = if key.is_current(context) {
                    result
                } else {
                    Err("The selected machine configuration changed".into())
                };
                if let Some(shell) = context.state.shell.as_mut() {
                    if shell.receive_session_list(&key.fingerprint.machine, key.request_id, result)
                    {
                        context.repaint();
                    }
                }
            }
            SessionSwitchEvent::Prepared {
                key,
                generation,
                result,
            } => {
                if !self.preparation.as_ref().is_some_and(|preparation| {
                    preparation.key == key
                        && preparation.generation == generation
                        && !preparation.cancelled.load(Ordering::Acquire)
                }) {
                    return Ok(());
                }
                let follow_current = self.preparation.as_ref().is_some_and(|preparation| {
                    preparation
                        .follow
                        .as_ref()
                        .is_none_or(|follow| follow.is_current(key.request_id, false, context))
                });
                let result = if key.is_current(context)
                    && follow_current
                    && self.history_is_current(key.request_id, context)
                {
                    result
                } else {
                    Err("The selected machine or view changed; open the session manually".into())
                };
                match result {
                    Ok(endpoint::EndpointSupervisorEvent::Connected {
                        endpoint_id,
                        generation,
                        reader,
                        writer,
                        negotiation,
                    }) => {
                        let preparation = self
                            .preparation
                            .as_mut()
                            .expect("matched session preparation");
                        if endpoint_id != preparation.endpoint_id
                            || generation != preparation.generation
                        {
                            return Ok(());
                        }
                        install_endpoint_connection(
                            context.state,
                            context.endpoints,
                            endpoint_id,
                            generation,
                            reader,
                            writer,
                            negotiation,
                            context.event_tx,
                        );
                        preparation.phase = PreparationPhase::Metadata;
                        preparation.deadline = Instant::now() + METADATA_TIMEOUT;
                    }
                    Ok(endpoint::EndpointSupervisorEvent::Status { message, .. })
                    | Err(message) => {
                        let preparation = self
                            .preparation
                            .take()
                            .expect("matched session preparation");
                        context.discard(preparation);
                        self.finish(context, key.request_id, Err(message));
                    }
                }
            }
        }
        self.drive(context)
    }

    pub(super) fn drive(
        &mut self,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), ClientError> {
        self.cancel_obsolete_history(context);
        self.drive_transfer(context);
        if let Some((endpoint_id, generation, serial, result)) =
            context.state.activation_completion.take()
        {
            if self.preparation.as_ref().is_some_and(|preparation| {
                preparation.endpoint_id == endpoint_id
                    && preparation.generation == generation
                    && preparation.phase == PreparationPhase::Activating
                    && preparation.activation_serial == Some(serial)
            }) {
                let preparation = self.preparation.take().expect("matched session activation");
                let request_id = preparation.key.request_id;
                let committed = result.is_ok()
                    && self.history_is_current(request_id, context)
                    && !preparation.cancelled.load(Ordering::Acquire)
                    && preparation
                        .follow
                        .as_ref()
                        .is_none_or(|follow| follow.is_current(request_id, true, context))
                    && preparation.key.is_current(context)
                    && self
                        .fingerprint(&preparation.key.fingerprint.machine, context)
                        .is_ok()
                    && context.endpoints.active_id() == &endpoint_id
                    && context
                        .endpoints
                        .accepts(&endpoint_id, preparation.generation)
                    && context.endpoints.active_surface_available();
                if committed {
                    let retired = context
                        .state
                        .shell
                        .as_mut()
                        .map(|shell| shell.commit_session_endpoint(&endpoint_id))
                        .unwrap_or_default();
                    for retired_id in retired {
                        context.supervisors.remove(&retired_id);
                        context.endpoints.disconnect(&retired_id);
                        let cancelled = context.commands.disconnect(&retired_id);
                        #[cfg(unix)]
                        context.state.forget_endpoint_graphics(&retired_id);
                        if let Some(shell) = context.state.shell.as_mut() {
                            for request_id in cancelled {
                                shell.cancel_endpoint_request(&request_id);
                            }
                            shell.remove_session_endpoint(&retired_id);
                        }
                    }
                    let configured = endpoint_id
                        .ssh_profile_id()
                        .and_then(|id| context.catalog.ssh.iter().find(|profile| &profile.id == id))
                        .cloned();
                    if preparation.owns_connection {
                        if preparation.allow_start {
                            context.supervisors.adopt_session(
                                endpoint_id.clone(),
                                preparation.target,
                                configured,
                                preparation.generation,
                                Instant::now(),
                            );
                        } else {
                            self.retain_history_connection(
                                endpoint_id.clone(),
                                preparation.target,
                                configured,
                                preparation.generation,
                            );
                        }
                    }
                    let previous_selection = context.catalog.selected_profile.clone();
                    context.catalog.select_endpoint(&endpoint_id);
                    if context.catalog.selected_profile != previous_selection {
                        if let Err(error) = context.catalog.store_selection() {
                            warn!(%error, "failed to persist selected machine");
                        }
                    }
                    self.finish(context, request_id, Ok(()));
                } else {
                    context.discard(preparation);
                    self.finish(
                        context,
                        request_id,
                        Err(result
                            .err()
                            .unwrap_or_else(|| "Session switch cancelled".into())),
                    );
                }
            }
        }
        let unsupported_history = self.preparation.as_ref().is_some_and(|preparation| {
            preparation.phase == PreparationPhase::Metadata
                && self
                    .history_allow_start(preparation.key.request_id)
                    .is_some()
                && context
                    .endpoints
                    .connection(&preparation.endpoint_id)
                    .is_some_and(|connection| {
                        !connection
                            .negotiation
                            .supports_method("pane.focus_if_session")
                    })
        });
        if unsupported_history {
            let preparation = self
                .preparation
                .take()
                .expect("unsupported history session");
            let request_id = preparation.key.request_id;
            context.discard(preparation);
            self.finish(
                context,
                request_id,
                Err(
                    "This server does not support safe history navigation; update and restart it"
                        .into(),
                ),
            );
        }
        let stale_follow = self.preparation.as_ref().is_some_and(|preparation| {
            preparation.follow.as_ref().is_some_and(|follow| {
                !follow.is_current(
                    preparation.key.request_id,
                    preparation.phase == PreparationPhase::Activating,
                    context,
                )
            })
        });
        if stale_follow {
            if let Some(preparation) = &self.preparation {
                let request_id = preparation.key.request_id;
                self.cancel(request_id, context);
                self.finish(
                    context,
                    request_id,
                    Err("The view changed; open the destination session manually".into()),
                );
            }
        }
        if let Some(preparation) = &self.preparation {
            if preparation.phase == PreparationPhase::Activating {
                if context.activation.is_none() {
                    let preparation = self
                        .preparation
                        .take()
                        .expect("checked session preparation");
                    let request_id = preparation.key.request_id;
                    context.discard(preparation);
                    self.finish(
                        context,
                        request_id,
                        Err("The session could not be activated safely".into()),
                    );
                }
            } else if Instant::now() >= preparation.deadline
                || (preparation.phase == PreparationPhase::Metadata
                    && !context
                        .endpoints
                        .accepts(&preparation.endpoint_id, preparation.generation))
            {
                let preparation = self
                    .preparation
                    .take()
                    .expect("checked session preparation");
                let request_id = preparation.key.request_id;
                context.discard(preparation);
                self.finish(
                    context,
                    request_id,
                    Err("The target session did not become ready".into()),
                );
            } else if preparation.phase == PreparationPhase::Metadata
                && context.activation.is_none()
                && context.state.shell.as_ref().is_some_and(|shell| {
                    shell
                        .endpoint_snapshot_identity(
                            &preparation.endpoint_id,
                            preparation.generation,
                        )
                        .is_some()
                })
            {
                let pixel_mouse_available = context.state.pixel_mouse_available();
                let shell = context.state.shell.as_mut().expect("session client shell");
                let resize = client_shell_resize_message(
                    shell,
                    context.state.reported_size.0,
                    context.state.reported_size.1,
                    context.state.reported_cell_size.0,
                    context.state.reported_cell_size.1,
                    pixel_mouse_available,
                );
                let activation_serial = *context.next_surface_serial;
                let activation = endpoint::PendingEndpointActivation::prepare(
                    shell,
                    context.endpoints,
                    preparation.endpoint_id.clone(),
                    preparation
                        .follow
                        .as_ref()
                        .map(|follow| follow.target.clone()),
                    resize,
                    *context.next_surface_serial,
                    Instant::now(),
                )
                .map(endpoint::PendingEndpointActivation::for_session_switch)
                .and_then(|activation| activation.start(context.endpoints));
                if let Some(preparation) = self.preparation.as_mut() {
                    preparation.activation_serial = Some(activation_serial);
                }
                match activation {
                    Ok(activation) => {
                        install_pending_activation(
                            context.state,
                            context.commands,
                            context.activation,
                            context.next_surface_serial,
                            activation,
                        );
                        self.preparation
                            .as_mut()
                            .expect("checked session preparation")
                            .phase = PreparationPhase::Activating;
                    }
                    Err(endpoint::ActivationBeginError::Partial { activation, error }) => {
                        install_pending_activation(
                            context.state,
                            context.commands,
                            context.activation,
                            context.next_surface_serial,
                            *activation,
                        );
                        self.preparation
                            .as_mut()
                            .expect("checked session preparation")
                            .phase = PreparationPhase::Activating;
                        rollback_endpoint_activation(
                            context.state,
                            context.endpoints,
                            context.activation,
                            error,
                            false,
                        );
                    }
                    Err(endpoint::ActivationBeginError::Preflight(error)) => {
                        let preparation = self
                            .preparation
                            .take()
                            .expect("checked session preparation");
                        let request_id = preparation.key.request_id;
                        context.discard(preparation);
                        self.finish(context, request_id, Err(error));
                    }
                }
            }
        }
        if self.preparation.is_none() && context.activation.is_none() {
            if let Some(queued) = self.queued.take() {
                self.start(queued, context);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

impl Drop for SessionSwitchCoordinator {
    fn drop(&mut self) {
        self.work_slots.close();
        self.cancel_discovery();
        if let Some(preparation) = &self.preparation {
            preparation.cancelled.store(true, Ordering::Release);
        }
    }
}
