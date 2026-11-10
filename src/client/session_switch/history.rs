use super::*;
use crate::protocol::RenderEncoding;

pub(super) struct HistorySessionOpen {
    session_request: u64,
    history_request: u64,
    machine: ClientEndpointId,
    navigation_serial: u64,
    allow_start: bool,
}

pub(super) struct HistorySessionConnection {
    history_request: u64,
    endpoint_id: ClientEndpointId,
    target: EndpointSessionTarget,
    configured: Option<SavedSshEndpoint>,
    generation: u64,
}

impl HistorySessionOpen {
    fn is_current(&self, context: &SessionSwitchContext<'_>) -> bool {
        context.state.shell.as_ref().is_some_and(|shell| {
            shell.history_request_is_current(&self.machine, self.history_request)
                && shell
                    .history_navigation_view()
                    .is_some_and(|view| view.navigation_serial == self.navigation_serial)
        })
    }
}

impl SessionSwitchCoordinator {
    pub(in crate::client) fn open_history(
        &mut self,
        history_request: u64,
        machine: &ClientEndpointId,
        session: &str,
        allow_start: bool,
        context: &mut SessionSwitchContext<'_>,
    ) -> Result<(), String> {
        if self.busy() || self.transfer.is_some() || context.activation.is_some() {
            return Err("Another session change is pending; please try again".into());
        }
        crate::session::parse_target_name(session)?;
        let fingerprint = self.fingerprint(machine, context)?;
        let shell = context
            .state
            .shell
            .as_mut()
            .ok_or("Client shell is unavailable")?;
        if !shell.history_request_is_current(&fingerprint.machine, history_request) {
            return Err("History navigation was cancelled".into());
        }
        let navigation_serial = shell
            .history_navigation_view()
            .ok_or("The current session is unavailable")?
            .navigation_serial;
        let session_request = shell.allocate_history_session_request();
        self.latest_serial = self.latest_serial.max(session_request);
        self.history_result = None;
        self.history_open = Some(HistorySessionOpen {
            session_request,
            history_request,
            machine: fingerprint.machine.clone(),
            navigation_serial,
            allow_start,
        });
        self.queued = Some(QueuedOpen {
            key: SessionRequestKey {
                request_id: session_request,
                fingerprint,
            },
            session: session.to_owned(),
            create: false,
            follow: None,
        });
        Ok(())
    }

    pub(in crate::client) fn cancel_history(
        &mut self,
        history_request: u64,
        context: &mut SessionSwitchContext<'_>,
    ) {
        if self
            .history_result
            .as_ref()
            .is_some_and(|(id, _)| *id == history_request)
        {
            self.history_result = None;
        }
        if self
            .history_connection
            .as_ref()
            .is_some_and(|connection| connection.history_request == history_request)
        {
            self.history_connection = None;
        }
        if self
            .history_open
            .as_ref()
            .is_some_and(|open| open.history_request == history_request)
        {
            let open = self
                .history_open
                .take()
                .expect("matched history session open");
            self.cancel(open.session_request, context);
        }
    }

    pub(in crate::client) fn finish_history(
        &mut self,
        history_request: u64,
        success: bool,
        context: &mut SessionSwitchContext<'_>,
    ) {
        let connection = if self
            .history_connection
            .as_ref()
            .is_some_and(|connection| connection.history_request == history_request)
        {
            self.history_connection.take()
        } else {
            None
        };
        self.cancel_history(history_request, context);
        if let Some(connection) = connection.filter(|connection| {
            success
                && context.endpoints.active_id() == &connection.endpoint_id
                && context
                    .endpoints
                    .accepts(&connection.endpoint_id, connection.generation)
                && context.endpoints.active_surface_available()
        }) {
            context.supervisors.adopt_session(
                connection.endpoint_id,
                connection.target,
                connection.configured,
                connection.generation,
                Instant::now(),
            );
        }
    }

    pub(super) fn retain_history_connection(
        &mut self,
        endpoint_id: ClientEndpointId,
        target: EndpointSessionTarget,
        configured: Option<SavedSshEndpoint>,
        generation: u64,
    ) {
        if let Some(open) = &self.history_open {
            self.history_connection = Some(HistorySessionConnection {
                history_request: open.history_request,
                endpoint_id,
                target,
                configured,
                generation,
            });
        }
    }

    pub(in crate::client) fn take_history_result(&mut self) -> Option<(u64, Result<(), String>)> {
        self.history_result.take()
    }

    pub(in crate::client) fn history_allow_start(&self, session_request: u64) -> Option<bool> {
        self.history_open
            .as_ref()
            .filter(|open| open.session_request == session_request)
            .map(|open| open.allow_start)
    }

    pub(super) fn history_is_current(
        &self,
        session_request: u64,
        context: &SessionSwitchContext<'_>,
    ) -> bool {
        self.history_open
            .as_ref()
            .filter(|open| open.session_request == session_request)
            .is_none_or(|open| open.is_current(context))
    }

    pub(super) fn cancel_obsolete_history(&mut self, context: &mut SessionSwitchContext<'_>) {
        if let Some(open) = self
            .history_open
            .as_ref()
            .filter(|open| !open.is_current(context))
        {
            self.cancel_history(open.history_request, context);
        }
    }

    pub(super) fn finish(
        &mut self,
        context: &mut SessionSwitchContext<'_>,
        request_id: u64,
        result: Result<(), String>,
    ) {
        if self
            .history_open
            .as_ref()
            .is_some_and(|open| open.session_request == request_id)
        {
            let open = self
                .history_open
                .take()
                .expect("matched history session open");
            self.history_result = Some((open.history_request, result));
        } else {
            context.finish(request_id, result);
        }
    }
}

pub(super) fn connect_existing(
    target: EndpointSessionTarget,
    options: endpoint::EndpointConnectOptions,
    endpoint_id: ClientEndpointId,
    generation: u64,
    cancelled: Arc<AtomicBool>,
) -> io::Result<endpoint::EndpointSupervisorEvent> {
    let local_transport = matches!(target, EndpointSessionTarget::Local(_));
    let (mut stream, lifetime): (_, Box<dyn Send>) = match target {
        EndpointSessionTarget::Local(session) => {
            let name =
                (session != crate::session::DEFAULT_SESSION_NAME).then_some(session.as_str());
            let path = crate::session::client_socket_path_for(name);
            let stream = crate::ipc::connect_local_stream_cancellable(
                &path,
                &cancelled,
                Instant::now() + super::super::handshake::LOCAL_HANDSHAKE_READ_TIMEOUT,
            )?;
            (stream, Box::new(()))
        }
        EndpointSessionTarget::Ssh(profile) => {
            let saved = crate::remote::connect_saved_existing_ssh(
                profile.id.as_str(),
                &profile.target,
                &profile.session,
                Arc::clone(&cancelled),
            )?;
            (saved.stream, Box::new(saved.bridge))
        }
    };
    let handshake = do_handshake(
        &mut stream,
        options.cols,
        options.rows,
        options.cell_width_px,
        options.cell_height_px,
        HandshakePixelCapabilities {
            exact_geometry: options.pixel_geometry_exact,
            sgr_mouse: options.pixel_mouse,
        },
        Some(options.surface_size),
        options.endpoint_keybindings,
        options.mouse_capture,
        false,
        local_transport,
        &cancelled,
    )
    .map_err(io::Error::other)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "History navigation cancelled",
        ));
    }
    let negotiation = endpoint::EndpointNegotiation::new(
        handshake.endpoint_methods.unwrap_or_default(),
        handshake.endpoint_capabilities.unwrap_or_default(),
    );
    if handshake.encoding != RenderEncoding::SemanticFrame
        || !negotiation.supports_surface_interest()
        || (!local_transport && !negotiation.supports_health_check())
        || !negotiation.supports_method("pane.focus_if_session")
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "This server does not support safe history navigation; update and restart it",
        ));
    }
    let reader = stream.try_clone()?;
    let writer = endpoint::NativeEndpointTransport::with_lifetime(stream, lifetime)?;
    Ok(endpoint::EndpointSupervisorEvent::Connected {
        endpoint_id,
        generation,
        reader,
        writer,
        negotiation,
    })
}
