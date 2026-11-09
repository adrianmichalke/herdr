use super::*;
use crate::api::schema::{
    Method, Request, ResponseResult, WorkspaceTransferInfo, WorkspaceTransferParams,
    WorkspaceTransferState, WorkspaceTransferStatusParams,
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const REQUEST_PREFIX: &str = "client-workspace-transfer:";

#[derive(Clone)]
pub(super) struct TransferFollow {
    pub(super) source: shell::WorkspaceTransferSource,
    pub(super) target: shell::ClientEndpointFocusTarget,
}

impl TransferFollow {
    pub(super) fn is_current(
        &self,
        request_id: u64,
        activating: bool,
        context: &SessionSwitchContext<'_>,
    ) -> bool {
        context
            .endpoints
            .connection(&self.source.endpoint_id)
            .map_or(activating, |connection| {
                connection.generation == self.source.generation
            })
            && context.state.shell.as_ref().is_some_and(|shell| {
                shell.workspace_transfer_view_matches(&self.source, activating)
                    && shell.workspace_transfer_picker_matches(request_id, &self.source)
            })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    Submit,
    Status,
}

struct TransferCommand {
    request_id: String,
    kind: CommandKind,
}

pub(super) struct WorkspaceTransfer {
    pub(super) key: SessionRequestKey,
    pub(super) source: shell::WorkspaceTransferSource,
    destination_session: String,
    transfer_id: String,
    command_serial: u64,
    submission_started: bool,
    outstanding: Option<TransferCommand>,
    next_poll: Instant,
}

pub(super) enum TransferProgress {
    Failed(String),
    Committed(WorkspaceTransferInfo),
    Unknown(String),
}

pub(super) fn is_transfer_result(result: &endpoint_commands::EndpointCommandResult) -> bool {
    result.request_id.starts_with(REQUEST_PREFIX)
}

fn transfer_id() -> String {
    let id = ProfileId::generate();
    let hex = id.as_str();
    let variant = ["8", "9", "a", "b"][usize::from(hex.as_bytes()[16] & 3)];
    format!(
        "{}-{}-4{}-{}{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[13..16],
        variant,
        &hex[17..20],
        &hex[20..],
    )
}

impl WorkspaceTransfer {
    pub(super) fn submit(
        key: SessionRequestKey,
        source: shell::WorkspaceTransferSource,
        destination_session: String,
        context: &mut SessionSwitchContext<'_>,
    ) -> Self {
        let mut transfer = Self {
            key,
            source,
            destination_session,
            transfer_id: transfer_id(),
            command_serial: 0,
            submission_started: false,
            outstanding: None,
            next_poll: Instant::now(),
        };
        transfer.enqueue(
            Method::WorkspaceTransfer(WorkspaceTransferParams {
                workspace_id: transfer.source.workspace_id.clone(),
                destination_session: transfer.destination_session.clone(),
                transfer_id: transfer.transfer_id.clone(),
            }),
            CommandKind::Submit,
            context,
        );
        transfer
    }

    fn message(
        &self,
        context: &mut SessionSwitchContext<'_>,
        message: Option<String>,
        finished: bool,
    ) {
        if context.state.shell.as_mut().is_some_and(|shell| {
            shell.workspace_transfer_message(self.key.request_id, message, finished)
        }) {
            context.repaint();
        }
    }

    fn enqueue(
        &mut self,
        method: Method,
        kind: CommandKind,
        context: &mut SessionSwitchContext<'_>,
    ) {
        self.command_serial = self.command_serial.saturating_add(1);
        let request_id = format!(
            "{REQUEST_PREFIX}{}:{}",
            self.transfer_id, self.command_serial
        );
        context.commands.enqueue(
            self.source.endpoint_id.clone(),
            self.source.generation,
            self.source.boot_id.clone(),
            Box::new(Request {
                id: request_id.clone(),
                method,
            }),
        );
        self.outstanding = Some(TransferCommand { request_id, kind });
        self.next_poll = Instant::now() + POLL_INTERVAL;
        let cancelled = context
            .commands
            .send_next(&self.source.endpoint_id, context.endpoints);
        if kind == CommandKind::Submit {
            self.observe_submission(context);
        }
        if let Some(shell) = context.state.shell.as_mut() {
            for request_id in cancelled {
                shell.cancel_endpoint_request(&request_id);
            }
        }
    }

    fn observe_submission(&mut self, context: &SessionSwitchContext<'_>) {
        if let Some(command) = self
            .outstanding
            .as_ref()
            .filter(|command| command.kind == CommandKind::Submit)
        {
            self.submission_started |= context.commands.request_may_have_been_sent(
                &self.source.endpoint_id,
                self.source.generation,
                &self.source.boot_id,
                &command.request_id,
            );
        }
    }

    pub(super) fn drive(
        &mut self,
        configuration_current: bool,
        context: &mut SessionSwitchContext<'_>,
    ) -> Option<TransferProgress> {
        self.observe_submission(context);
        let interrupted = self.outstanding.as_ref().is_some_and(|command| {
            !context.commands.contains_request(
                &self.source.endpoint_id,
                self.source.generation,
                &self.source.boot_id,
                &command.request_id,
            )
        });
        if interrupted && !self.submission_started {
            return Some(TransferProgress::Failed(
                "Workspace move was not submitted; the source was not changed.".into(),
            ));
        }
        if !configuration_current
            || !self.key.is_current(context)
            || !context
                .endpoints
                .accepts(&self.source.endpoint_id, self.source.generation)
            || !context
                .state
                .shell
                .as_ref()
                .is_some_and(|shell| shell.workspace_transfer_source_matches(&self.source))
        {
            if !self.submission_started && self.cancel_queued(context) {
                return Some(TransferProgress::Failed("Workspace move was not submitted because the source changed; the source was not modified.".into()));
            }
            return Some(TransferProgress::Unknown(format!(
                "Move submitted, but the source connection or machine settings changed. Check session '{}' before moving again.",
                self.destination_session,
            )));
        }
        if interrupted {
            self.outstanding = None;
            self.message(
                context,
                Some("Move submitted; its response was interrupted. Checking the outcome…".into()),
                false,
            );
        }
        if self.outstanding.is_none() && Instant::now() >= self.next_poll {
            self.enqueue(
                Method::WorkspaceTransferStatus(WorkspaceTransferStatusParams {
                    transfer_id: self.transfer_id.clone(),
                }),
                CommandKind::Status,
                context,
            );
        } else if self.outstanding.is_some() {
            let cancelled = context
                .commands
                .send_next(&self.source.endpoint_id, context.endpoints);
            if let Some(shell) = context.state.shell.as_mut() {
                for request_id in cancelled {
                    shell.cancel_endpoint_request(&request_id);
                }
            }
        }
        None
    }

    pub(super) fn receive(
        &mut self,
        completed: endpoint_commands::EndpointCommandResult,
        context: &mut SessionSwitchContext<'_>,
    ) -> Option<TransferProgress> {
        let command = self.outstanding.as_ref()?;
        if completed.endpoint_id != self.source.endpoint_id
            || completed.generation != self.source.generation
            || completed.boot_id != self.source.boot_id
            || completed.request_id != command.request_id
        {
            return None;
        }
        let kind = command.kind;
        self.submission_started = true;
        self.outstanding = None;
        self.next_poll = Instant::now() + POLL_INTERVAL;
        match completed.result {
            Ok(ResponseResult::WorkspaceTransfer { transfer })
                if transfer.transfer_id == self.transfer_id
                    && transfer.destination_session == self.destination_session =>
            {
                match transfer.state {
                    WorkspaceTransferState::Pending => {
                        self.message(context, transfer.error, false);
                        None
                    }
                    WorkspaceTransferState::Committed => Some(TransferProgress::Committed(transfer)),
                    WorkspaceTransferState::Failed => Some(TransferProgress::Failed(
                        transfer.error.unwrap_or_else(|| "Workspace move failed".into()),
                    )),
                }
            }
            Ok(_) => Some(TransferProgress::Unknown(
                "Move submitted, but the server returned an unexpected result. Check the destination session before moving again.".into(),
            )),
            Err(error) => {
                let unsupported = matches!(
                    error.code.as_deref(),
                    Some("unknown_method" | "method_not_found" | "unsupported_endpoint_command")
                );
                let uncertain = matches!(
                    error.code.as_deref(),
                    None | Some("endpoint_timeout" | "endpoint_cancelled" | "server_unavailable")
                );
                if kind == CommandKind::Submit && !uncertain {
                    Some(TransferProgress::Failed(format!("Workspace move rejected: {}", error.message)))
                } else if error.code.as_deref() == Some("not_found") {
                    Some(TransferProgress::Unknown(format!(
                        "Move outcome unknown: the source has no record of this transfer. Check session '{}' before moving again.", self.destination_session,
                    )))
                } else if error.code.as_deref() == Some("surface_inactive") {
                    Some(TransferProgress::Unknown(format!(
                        "Move submitted, but the source no longer accepts status requests: {}. Check the destination session before moving again.", error.message,
                    )))
                } else if unsupported {
                    Some(TransferProgress::Unknown(format!(
                        "Move submitted, but this server cannot report its outcome: {}. Check the destination session before moving again.", error.message,
                    )))
                } else {
                    self.message(
                        context,
                        Some(format!("Move submitted; checking outcome… {}", error.message)),
                        false,
                    );
                    None
                }
            }
        }
    }

    fn cancel_queued(&self, context: &mut SessionSwitchContext<'_>) -> bool {
        self.outstanding.as_ref().is_some_and(|command| {
            context.commands.cancel_queued_request(
                &self.source.endpoint_id,
                self.source.generation,
                &self.source.boot_id,
                &command.request_id,
            )
        })
    }

    pub(super) fn finish_unknown(&self, message: String, context: &mut SessionSwitchContext<'_>) {
        self.message(context, Some(message), true);
    }
}

#[cfg(test)]
#[path = "workspace_transfer/tests.rs"]
mod tests;

impl SessionSwitchCoordinator {
    pub(in crate::client) fn is_transfer_result(
        result: &endpoint_commands::EndpointCommandResult,
    ) -> bool {
        is_transfer_result(result)
    }

    pub(super) fn start_transfer(
        &mut self,
        source: shell::WorkspaceTransferSource,
        request_id: u64,
        destination_session: String,
        context: &mut SessionSwitchContext<'_>,
    ) {
        if self.transfer.is_some() {
            context.finish(
                request_id,
                Err("Another workspace move is still being checked".into()),
            );
            return;
        }
        let fingerprint = match self.fingerprint(&source.endpoint_id.machine_id(), context) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                context.finish(request_id, Err(error));
                return;
            }
        };
        let current = context.state.shell.as_ref().is_some_and(|shell| {
            shell
                .workspace_transfer_source(&source.workspace_id)
                .as_ref()
                == Some(&source)
                && shell.workspace_transfer_view_matches(&source, false)
                && shell.workspace_transfer_picker_matches(request_id, &source)
        });
        let supported = context
            .endpoints
            .connection(&source.endpoint_id)
            .is_some_and(|connection| {
                connection.generation == source.generation
                    && shell::WORKSPACE_TRANSFER_METHODS
                        .iter()
                        .all(|method| connection.negotiation.supports_method(method))
            });
        if !current || !supported {
            context.finish(request_id, Err(if supported {
                "This workspace or source connection changed; reopen Move to session".into()
            } else {
                "This server does not support workspace moves. Update and restart it to enable Move to session.".into()
            }));
            return;
        }
        if let Err(error) = crate::session::parse_target_name(&destination_session) {
            context.finish(request_id, Err(error));
            return;
        }
        if destination_session == source.session {
            context.finish(
                request_id,
                Err("Choose a different destination session".into()),
            );
            return;
        }
        self.transfer = Some(WorkspaceTransfer::submit(
            SessionRequestKey {
                request_id,
                fingerprint,
            },
            source,
            destination_session,
            context,
        ));
    }

    pub(super) fn transfer_progress(
        &mut self,
        progress: TransferProgress,
        context: &mut SessionSwitchContext<'_>,
    ) {
        let Some(transfer) = self.transfer.take() else {
            return;
        };
        transfer.cancel_queued(context);
        match progress {
            TransferProgress::Failed(error) => context.finish(transfer.key.request_id, Err(error)),
            TransferProgress::Unknown(error) => transfer.finish_unknown(error, context),
            TransferProgress::Committed(info) => {
                let request_id = transfer.key.request_id;
                let target = info
                    .focused_pane_id
                    .map(shell::ClientEndpointFocusTarget::Pane)
                    .or_else(|| {
                        info.focused_tab_id
                            .map(shell::ClientEndpointFocusTarget::Tab)
                    })
                    .or_else(|| {
                        info.workspace_id
                            .map(shell::ClientEndpointFocusTarget::Workspace)
                    });
                let current = transfer.key.is_current(context)
                    && self
                        .fingerprint(&transfer.key.fingerprint.machine, context)
                        .is_ok()
                    && self.latest_serial == request_id
                    && context
                        .endpoints
                        .accepts(&transfer.source.endpoint_id, transfer.source.generation)
                    && context.state.shell.as_ref().is_some_and(|shell| {
                        shell.workspace_transfer_view_matches(&transfer.source, false)
                            && shell.workspace_transfer_picker_matches(request_id, &transfer.source)
                    });
                if !transfer.source.was_active() || !current {
                    context.finish(request_id, Ok(()));
                    return;
                }
                let Some(target) = target else {
                    transfer.finish_unknown("Workspace moved, but the server did not return a destination focus target. Open its session manually.".into(), context);
                    return;
                };
                if let Some(shell) = context.state.shell.as_mut() {
                    shell.workspace_transfer_following(request_id);
                }
                self.queued = Some(QueuedOpen {
                    key: transfer.key,
                    session: info.destination_session,
                    create: false,
                    follow: Some(TransferFollow {
                        source: transfer.source,
                        target,
                    }),
                });
            }
        }
    }

    pub(super) fn drive_transfer(&mut self, context: &mut SessionSwitchContext<'_>) {
        let configuration_current = self.transfer.as_ref().is_none_or(|transfer| {
            self.fingerprint(&transfer.key.fingerprint.machine, context)
                .is_ok()
        });
        let progress = self
            .transfer
            .as_mut()
            .and_then(|transfer| transfer.drive(configuration_current, context));
        if let Some(progress) = progress {
            self.transfer_progress(progress, context);
        }
    }

    pub(super) fn receive_transfer(
        &mut self,
        completed: endpoint_commands::EndpointCommandResult,
        context: &mut SessionSwitchContext<'_>,
    ) {
        let progress = self
            .transfer
            .as_mut()
            .and_then(|transfer| transfer.receive(completed, context));
        if let Some(progress) = progress {
            self.transfer_progress(progress, context);
        }
    }
}
