use std::collections::VecDeque;
use std::io;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use super::endpoint::{ClientEndpointId, ProfileId, SavedSshEndpoint};
use super::session_switch::{MachineFingerprint, SessionSwitchContext, SessionSwitchCoordinator};
use crate::pi_history::types::{
    HistoryAction as RequestAction, HistoryEvent, HistoryLocation, HistoryRequest, HistorySource,
};

mod navigation;
mod transport;

use navigation::Navigation;

pub(super) enum HistoryClientAction {
    Request {
        machine: ClientEndpointId,
        request_id: u64,
        request: Box<HistoryRequest>,
    },
    Cancel {
        request_id: u64,
    },
    Navigate {
        machine: ClientEndpointId,
        request_id: u64,
        request: Box<HistoryRequest>,
        location: Box<HistoryLocation>,
        open_session: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestKey {
    serial: u64,
    request_id: u64,
    fingerprint: MachineFingerprint,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Search,
    Detail,
}

impl Lane {
    fn for_request(request: &HistoryRequest) -> Self {
        if matches!(request.action, RequestAction::Search) {
            Self::Search
        } else {
            Self::Detail
        }
    }
}

struct Pending {
    key: RequestKey,
    request: HistoryRequest,
}

struct Worker {
    key: RequestKey,
    lane: Lane,
    cancelled: Arc<AtomicBool>,
    discarded: bool,
    resolved: Option<HistorySource>,
    problem: Option<String>,
}

pub(super) struct HistoryWorkerEvent {
    key: RequestKey,
    payload: WorkerPayload,
}

enum WorkerPayload {
    Event(HistoryEvent),
    Finished(Result<(), String>),
}

pub(super) struct HistoryCoordinator {
    root_profile: ProfileId,
    next_serial: u64,
    latest_profiles: Option<Vec<SavedSshEndpoint>>,
    pending: VecDeque<Pending>,
    workers: Vec<Worker>,
    navigation: Option<Navigation>,
    event_tx: tokio::sync::mpsc::Sender<HistoryWorkerEvent>,
}

impl HistoryCoordinator {
    pub(super) fn new(event_tx: tokio::sync::mpsc::Sender<HistoryWorkerEvent>) -> Self {
        Self {
            root_profile: ProfileId::generate(),
            next_serial: 0,
            latest_profiles: None,
            pending: VecDeque::new(),
            workers: Vec::new(),
            navigation: None,
            event_tx,
        }
    }

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
        let fingerprint = MachineFingerprint::read(machine, context.catalog, shell)?;
        if self
            .latest_profiles
            .as_ref()
            .is_some_and(|profiles| !matches_profiles(&fingerprint, profiles))
        {
            return Err("The selected machine configuration changed; please try again".into());
        }
        Ok(fingerprint)
    }

    fn current(&self, key: &RequestKey, context: &SessionSwitchContext<'_>) -> bool {
        context.state.shell.as_ref().is_some_and(|shell| {
            shell.history_request_is_current(&key.fingerprint.machine, key.request_id)
        }) && self
            .fingerprint(&key.fingerprint.machine, context)
            .is_ok_and(|current| current == key.fingerprint)
    }

    fn report(
        &self,
        key: &RequestKey,
        event: HistoryEvent,
        context: &mut SessionSwitchContext<'_>,
    ) {
        if context.state.shell.as_mut().is_some_and(|shell| {
            shell.receive_history_event(&key.fingerprint.machine, key.request_id, event)
        }) {
            context.repaint();
        }
    }

    fn cancel_worker(&mut self, request_id: u64) {
        self.pending
            .retain(|pending| pending.key.request_id != request_id);
        for worker in &mut self.workers {
            if worker.key.request_id == request_id {
                worker.discarded = true;
                worker.cancelled.store(true, Ordering::Release);
            }
        }
    }

    fn queue(&mut self, key: RequestKey, request: HistoryRequest) {
        let lane = Lane::for_request(&request);
        self.pending
            .retain(|pending| Lane::for_request(&pending.request) != lane);
        for worker in &mut self.workers {
            if worker.lane == lane {
                worker.discarded = true;
                worker.cancelled.store(true, Ordering::Release);
            }
        }
        self.pending.push_back(Pending { key, request });
    }

    fn key(
        &mut self,
        machine: &ClientEndpointId,
        request_id: u64,
        context: &SessionSwitchContext<'_>,
    ) -> Result<RequestKey, String> {
        let fingerprint = self.fingerprint(machine, context)?;
        self.next_serial = self.next_serial.wrapping_add(1).max(1);
        Ok(RequestKey {
            serial: self.next_serial,
            request_id,
            fingerprint,
        })
    }

    pub(super) fn actions(
        &mut self,
        actions: Vec<HistoryClientAction>,
        sessions: &mut SessionSwitchCoordinator,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), super::ClientError> {
        for action in actions {
            match action {
                HistoryClientAction::Cancel { request_id } => {
                    self.cancel_worker(request_id);
                    if self
                        .navigation
                        .as_ref()
                        .is_some_and(|navigation| navigation.key.request_id == request_id)
                    {
                        self.finish_navigation(
                            Err("History navigation cancelled".into()),
                            sessions,
                            context,
                        );
                    }
                }
                HistoryClientAction::Request {
                    machine,
                    request_id,
                    request,
                } => {
                    if !context.state.shell.as_ref().is_some_and(|shell| {
                        shell.history_request_is_current(&machine.machine_id(), request_id)
                    }) {
                        continue;
                    }
                    if Lane::for_request(&request) == Lane::Detail && self.navigation.is_some() {
                        self.finish_navigation(
                            Err("History navigation cancelled".into()),
                            sessions,
                            context,
                        );
                    }
                    match self.key(&machine, request_id, context) {
                        Ok(key) => self.queue(key, *request),
                        Err(message) => {
                            if let Some(shell) = context.state.shell.as_mut() {
                                shell.receive_history_event(
                                    &machine.machine_id(),
                                    request_id,
                                    HistoryEvent::Error { message },
                                );
                            }
                            context.repaint();
                        }
                    }
                }
                HistoryClientAction::Navigate {
                    machine,
                    request_id,
                    request,
                    location,
                    open_session,
                } => {
                    if !context.state.shell.as_ref().is_some_and(|shell| {
                        shell.history_request_is_current(&machine.machine_id(), request_id)
                    }) {
                        continue;
                    }
                    self.finish_navigation(
                        Err("History navigation superseded".into()),
                        sessions,
                        context,
                    );
                    let result = self.key(&machine, request_id, context).and_then(|key| {
                        let view = context
                            .state
                            .shell
                            .as_ref()
                            .and_then(|shell| shell.history_navigation_view())
                            .ok_or("The current session is unavailable")?;
                        Navigation::new(key, *request, *location, open_session, view)
                    });
                    match result {
                        Ok(navigation) => {
                            self.queue(navigation.key.clone(), navigation.request.clone());
                            self.navigation = Some(navigation);
                        }
                        Err(error) => {
                            if let Some(shell) = context.state.shell.as_mut() {
                                shell.finish_history_navigation(request_id, Err(error));
                            }
                            context.repaint();
                        }
                    }
                }
            }
        }
        self.drive(sessions, context)
    }

    pub(super) fn catalog_changed(
        &mut self,
        profiles: &[SavedSshEndpoint],
        sessions: &mut SessionSwitchCoordinator,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), super::ClientError> {
        self.latest_profiles = Some(profiles.to_vec());
        self.drive(sessions, context)
    }

    pub(super) fn drive(
        &mut self,
        sessions: &mut SessionSwitchCoordinator,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), super::ClientError> {
        if self.navigation.as_ref().is_some_and(|navigation| {
            !self.current(&navigation.key, context) || !navigation.view_is_current(context)
        }) {
            self.finish_navigation(
                Err("The machine or current view changed; please try again".into()),
                sessions,
                context,
            );
        }
        let stale = self
            .workers
            .iter()
            .filter(|worker| !worker.discarded && !self.current(&worker.key, context))
            .map(|worker| worker.key.clone())
            .chain(
                self.pending
                    .iter()
                    .filter(|pending| !self.current(&pending.key, context))
                    .map(|pending| pending.key.clone()),
            )
            .collect::<Vec<_>>();
        for key in stale {
            self.cancel_worker(key.request_id);
            self.report(
                &key,
                HistoryEvent::Error {
                    message: "The history request is no longer current".into(),
                },
                context,
            );
        }
        if let Some((request_id, result)) = sessions.take_history_result() {
            if self
                .navigation
                .as_ref()
                .is_some_and(|navigation| navigation.key.request_id == request_id)
            {
                match result {
                    Ok(()) => self.resolve_after_open(sessions, context),
                    Err(error) => self.finish_navigation(Err(error), sessions, context),
                }
            }
        }
        self.check_focus(sessions, context);
        self.start_pending(context);
        Ok(())
    }

    fn start_pending(&mut self, context: &SessionSwitchContext<'_>) {
        while self.workers.len() < 2 {
            let Some(pending) = self.pending.pop_front() else {
                break;
            };
            if !self.current(&pending.key, context) {
                continue;
            }
            let key = pending.key;
            let cancelled = Arc::new(AtomicBool::new(false));
            self.workers.push(Worker {
                key: key.clone(),
                lane: Lane::for_request(&pending.request),
                cancelled: Arc::clone(&cancelled),
                discarded: false,
                resolved: None,
                problem: None,
            });
            let target = match &key.fingerprint.target {
                Some(target) => transport::HelperTarget::Ssh {
                    profile_id: key
                        .fingerprint
                        .machine
                        .ssh_profile_id()
                        .unwrap_or(&self.root_profile)
                        .as_str()
                        .to_owned(),
                    target: target.clone(),
                    session: key.fingerprint.configured_session.clone(),
                },
                None => transport::HelperTarget::Local,
            };
            let event_tx = self.event_tx.clone();
            tokio::spawn(async move {
                let task_key = key.clone();
                let task_tx = event_tx.clone();
                let result = tokio::task::spawn_blocking(move || {
                    transport::run(target, pending.request, Arc::clone(&cancelled), |event| {
                        let mut output = HistoryWorkerEvent {
                            key: task_key.clone(),
                            payload: WorkerPayload::Event(event),
                        };
                        loop {
                            if cancelled.load(Ordering::Acquire) {
                                return Err(io::ErrorKind::ConnectionAborted.into());
                            }
                            match task_tx.try_send(output) {
                                Ok(()) => return Ok(()),
                                Err(tokio::sync::mpsc::error::TrySendError::Full(value)) => {
                                    output = value;
                                    std::thread::sleep(Duration::from_millis(5));
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                    return Err(io::ErrorKind::BrokenPipe.into())
                                }
                            }
                        }
                    })
                    .map_err(|error| error.to_string())
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()));
                let _ = event_tx
                    .send(HistoryWorkerEvent {
                        key,
                        payload: WorkerPayload::Finished(result),
                    })
                    .await;
            });
        }
    }

    pub(super) fn receive(
        &mut self,
        event: HistoryWorkerEvent,
        sessions: &mut SessionSwitchCoordinator,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), super::ClientError> {
        let Some(index) = self
            .workers
            .iter()
            .position(|worker| worker.key == event.key)
        else {
            return Ok(());
        };
        let current = !self.workers[index].discarded && self.current(&event.key, context);
        match event.payload {
            WorkerPayload::Event(event) if current => {
                match &event {
                    HistoryEvent::Resolved { source } => {
                        if self.workers[index]
                            .resolved
                            .replace(source.clone())
                            .is_some()
                        {
                            self.workers[index].problem =
                                Some("The history helper returned more than one source".into());
                        }
                    }
                    HistoryEvent::Error { message } => {
                        self.workers[index].problem = Some(message.clone())
                    }
                    _ => {}
                }
                self.report(&self.workers[index].key, event, context);
            }
            WorkerPayload::Event(_) => {}
            WorkerPayload::Finished(result) => {
                let worker = self.workers.remove(index);
                if current {
                    let result = result.and_then(|()| worker.problem.map_or(Ok(()), Err));
                    if self
                        .navigation
                        .as_ref()
                        .is_some_and(|navigation| navigation.key == worker.key)
                    {
                        match result {
                            Ok(()) => self.resolved(worker.resolved, sessions, context),
                            Err(error) => self.finish_navigation(Err(error), sessions, context),
                        }
                    } else if let Err(message) = result {
                        self.report(&worker.key, HistoryEvent::Error { message }, context);
                    }
                }
            }
        }
        self.drive(sessions, context)
    }
}

fn matches_profiles(fingerprint: &MachineFingerprint, profiles: &[SavedSshEndpoint]) -> bool {
    fingerprint.machine.ssh_profile_id().is_none_or(|id| {
        profiles.iter().any(|profile| {
            &profile.id == id
                && profile.enabled
                && Some(&profile.target) == fingerprint.target.as_ref()
                && profile.session == fingerprint.configured_session
        })
    })
}

impl Drop for HistoryCoordinator {
    fn drop(&mut self) {
        for worker in &self.workers {
            worker.cancelled.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests;
