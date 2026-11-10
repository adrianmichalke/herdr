use super::*;
use std::time::Instant;

use crate::api::schema::{Method, PaneFocusIfSessionParams, Request, ResponseResult};
use crate::client::endpoint_commands::EndpointCommandResult;
use crate::client::shell::HistoryNavigationView;

const FOCUS_PREFIX: &str = "client-history-focus:";
const FOCUS_TIMEOUT: Duration = Duration::from_secs(15);

struct FocusCommand {
    endpoint_id: ClientEndpointId,
    generation: u64,
    boot_id: String,
    request_id: String,
    location: HistoryLocation,
    deadline: Instant,
}

enum Phase {
    Resolving { after_open: bool },
    Opening,
    Focusing(Box<FocusCommand>),
}

pub(super) struct Navigation {
    pub(super) key: RequestKey,
    pub(super) request: HistoryRequest,
    location: HistoryLocation,
    open_session: bool,
    view: HistoryNavigationView,
    phase: Phase,
}

impl Navigation {
    pub(super) fn new(
        key: RequestKey,
        request: HistoryRequest,
        location: HistoryLocation,
        open_session: bool,
        view: HistoryNavigationView,
    ) -> Result<Self, String> {
        if !matches!(&request.action, RequestAction::Resolve { source_id } if !source_id.is_empty())
        {
            return Err("History navigation requires a fresh source resolution".into());
        }
        crate::session::parse_target_name(&location.session)?;
        Ok(Self {
            key,
            request,
            location,
            open_session,
            view,
            phase: Phase::Resolving { after_open: false },
        })
    }

    pub(super) fn view_is_current(&self, context: &SessionSwitchContext<'_>) -> bool {
        let Some(view) = context
            .state
            .shell
            .as_ref()
            .and_then(|shell| shell.history_navigation_view())
        else {
            return false;
        };
        if view.navigation_serial != self.view.navigation_serial {
            return false;
        }
        match &self.phase {
            Phase::Opening => true,
            Phase::Resolving { .. } => view == self.view,
            Phase::Focusing(_) => {
                view.endpoint_id == self.view.endpoint_id
                    && view.session == self.view.session
                    && view.generation == self.view.generation
                    && view.boot_id == self.view.boot_id
            }
        }
    }

    fn refreshed(&self, source: Option<HistorySource>) -> Result<HistoryLocation, String> {
        let source = source.ok_or("The source is no longer available in this search scope")?;
        if !matches!(&self.request.action, RequestAction::Resolve { source_id } if source_id == &source.id)
        {
            return Err("The resolved history source changed".into());
        }
        source
            .locations
            .into_iter()
            .find(|location| same_association(location, &self.location))
            .ok_or_else(|| {
                "This pane is no longer associated with the selected Pi conversation".into()
            })
    }
}

fn same_association(left: &HistoryLocation, right: &HistoryLocation) -> bool {
    left.session == right.session
        && left.workspace_id == right.workspace_id
        && left.tab_id == right.tab_id
        && left.pane_id == right.pane_id
        && left.agent_session == right.agent_session
}

impl HistoryCoordinator {
    pub(in crate::client) fn is_focus_result(result: &EndpointCommandResult) -> bool {
        result.request_id.starts_with(FOCUS_PREFIX)
    }

    pub(super) fn finish_navigation(
        &mut self,
        result: Result<(), String>,
        sessions: &mut SessionSwitchCoordinator,
        context: &mut SessionSwitchContext<'_>,
    ) {
        let Some(navigation) = self.navigation.take() else {
            return;
        };
        self.cancel_worker(navigation.key.request_id);
        sessions.finish_history(navigation.key.request_id, result.is_ok(), context);
        if let Phase::Focusing(command) = navigation.phase {
            context.commands.cancel_queued_request(
                &command.endpoint_id,
                command.generation,
                &command.boot_id,
                &command.request_id,
            );
        }
        if context
            .state
            .shell
            .as_mut()
            .is_some_and(|shell| shell.finish_history_navigation(navigation.key.request_id, result))
        {
            context.repaint();
        }
    }

    pub(super) fn resolved(
        &mut self,
        source: Option<HistorySource>,
        sessions: &mut SessionSwitchCoordinator,
        context: &mut SessionSwitchContext<'_>,
    ) {
        let Some(mut navigation) = self.navigation.take() else {
            return;
        };
        let result = (|| {
            let location = navigation.refreshed(source)?;
            if !location.running && !navigation.open_session {
                return Err("The session stopped; use Open session explicitly to start it".into());
            }
            let view = context
                .state
                .shell
                .as_ref()
                .and_then(|shell| shell.history_navigation_view())
                .ok_or("The current session is unavailable")?;
            let current_session = view.endpoint_id.machine_id()
                == navigation.key.fingerprint.machine
                && view.session == location.session;
            if matches!(navigation.phase, Phase::Resolving { after_open: true })
                && (!current_session || !location.running)
            {
                return Err("The opened session or pane changed; please try again".into());
            }
            if current_session && location.running {
                let command = enqueue_focus(&navigation, &location, context)?;
                navigation.view = view;
                navigation.phase = Phase::Focusing(Box::new(command));
            } else {
                sessions.open_history(
                    navigation.key.request_id,
                    &navigation.key.fingerprint.machine,
                    &location.session,
                    navigation.open_session,
                    context,
                )?;
                navigation.phase = Phase::Opening;
            }
            Ok(())
        })();
        self.navigation = Some(navigation);
        if let Err(error) = result {
            self.finish_navigation(Err(error), sessions, context);
        }
    }

    pub(super) fn resolve_after_open(
        &mut self,
        sessions: &mut SessionSwitchCoordinator,
        context: &mut SessionSwitchContext<'_>,
    ) {
        let view = context
            .state
            .shell
            .as_ref()
            .and_then(|shell| shell.history_navigation_view());
        let Some(navigation) = self.navigation.as_mut() else {
            return;
        };
        if !matches!(navigation.phase, Phase::Opening) {
            return;
        }
        let Some(view) = view.filter(|view| {
            view.endpoint_id.machine_id() == navigation.key.fingerprint.machine
                && view.session == navigation.location.session
        }) else {
            self.finish_navigation(
                Err("The session could not be opened safely".into()),
                sessions,
                context,
            );
            return;
        };
        self.next_serial = self.next_serial.wrapping_add(1).max(1);
        navigation.key.serial = self.next_serial;
        navigation.view = view;
        navigation.phase = Phase::Resolving { after_open: true };
        let key = navigation.key.clone();
        let request = navigation.request.clone();
        self.queue(key, request);
    }

    pub(super) fn check_focus(
        &mut self,
        sessions: &mut SessionSwitchCoordinator,
        context: &mut SessionSwitchContext<'_>,
    ) {
        let Some(navigation) = &self.navigation else {
            return;
        };
        let Phase::Focusing(command) = &navigation.phase else {
            return;
        };
        if Instant::now() >= command.deadline
            || !context
                .endpoints
                .accepts(&command.endpoint_id, command.generation)
            || context.endpoints.active_id() != &command.endpoint_id
            || !context.endpoints.active_surface_available()
        {
            self.finish_navigation(
                Err("The session changed or did not respond to conditional focus".into()),
                sessions,
                context,
            );
        }
    }

    pub(in crate::client) fn receive_focus_result(
        &mut self,
        result: EndpointCommandResult,
        sessions: &mut SessionSwitchCoordinator,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), crate::client::ClientError> {
        let Some(navigation) = &self.navigation else {
            return Ok(());
        };
        let Phase::Focusing(command) = &navigation.phase else {
            return Ok(());
        };
        if result.endpoint_id != command.endpoint_id
            || result.generation != command.generation
            || result.boot_id != command.boot_id
            || result.request_id != command.request_id
        {
            return Ok(());
        }
        let current = self.current(&navigation.key, context) && navigation.view_is_current(context);
        let outcome = if !current {
            Err("The current view changed; please try again".into())
        } else {
            result
                .result
                .map_err(|error| error.message)
                .and_then(|result| match result {
                    ResponseResult::PaneInfo { pane }
                        if pane.focused
                            && pane.pane_id == command.location.pane_id
                            && Some(&pane.terminal_id) == command.location.terminal_id.as_ref()
                            && pane.workspace_id == command.location.workspace_id
                            && pane.tab_id == command.location.tab_id
                            && pane.agent_session.as_ref()
                                == Some(&command.location.agent_session) =>
                    {
                        Ok(())
                    }
                    _ => Err("The server did not confirm the selected Pi conversation".into()),
                })
        };
        self.finish_navigation(outcome, sessions, context);
        self.drive(sessions, context)
    }
}

fn enqueue_focus(
    navigation: &Navigation,
    location: &HistoryLocation,
    context: &mut SessionSwitchContext<'_>,
) -> Result<FocusCommand, String> {
    let view = context
        .state
        .shell
        .as_ref()
        .and_then(|shell| shell.history_navigation_view())
        .ok_or("The current session is unavailable")?;
    if view.endpoint_id.machine_id() != navigation.key.fingerprint.machine
        || view.session != location.session
        || !context.endpoints.active_surface_available()
        || context.endpoints.active_id() != &view.endpoint_id
    {
        return Err("The selected session is not currently attached".into());
    }
    let generation = view
        .generation
        .ok_or("The current session metadata is unavailable")?;
    let boot_id = view
        .boot_id
        .ok_or("The current session metadata is unavailable")?;
    let connection = context
        .endpoints
        .connection(&view.endpoint_id)
        .filter(|connection| connection.generation == generation)
        .ok_or("The session connection changed")?;
    if !connection
        .negotiation
        .supports_method("pane.focus_if_session")
    {
        return Err(
            "This server does not support safe history navigation; update and restart it".into(),
        );
    }
    let terminal_id = location
        .terminal_id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or("The pane no longer has a running terminal")?;
    let request_id = format!(
        "{FOCUS_PREFIX}{}:{}",
        navigation.key.request_id, navigation.key.serial
    );
    context.commands.enqueue(
        view.endpoint_id.clone(),
        generation,
        boot_id.clone(),
        Box::new(Request {
            id: request_id.clone(),
            method: Method::PaneFocusIfSession(PaneFocusIfSessionParams {
                pane_id: location.pane_id.clone(),
                terminal_id,
                agent_session: location.agent_session.clone(),
            }),
        }),
    );
    let _ = context
        .commands
        .send_next(&view.endpoint_id, context.endpoints);
    if !context
        .commands
        .contains_request(&view.endpoint_id, generation, &boot_id, &request_id)
        || !context.commands.request_may_have_been_sent(
            &view.endpoint_id,
            generation,
            &boot_id,
            &request_id,
        )
    {
        context.commands.cancel_queued_request(
            &view.endpoint_id,
            generation,
            &boot_id,
            &request_id,
        );
        return Err("Another server action is pending; please try again".into());
    }
    Ok(FocusCommand {
        endpoint_id: view.endpoint_id,
        generation,
        boot_id,
        request_id,
        location: location.clone(),
        deadline: Instant::now() + FOCUS_TIMEOUT,
    })
}

#[cfg(test)]
mod tests;
