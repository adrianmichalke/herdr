use super::*;

pub(super) async fn receive_runtime_event(
    client: &mut tokio::sync::mpsc::Receiver<ClientLoopEvent>,
    history: &mut tokio::sync::mpsc::Receiver<pi_history::HistoryWorkerEvent>,
    prefer_history: &mut bool,
) -> ClientLoopEvent {
    let event = if *prefer_history {
        tokio::select! {
            biased;
            event = history.recv() => event.map(ClientLoopEvent::PiHistory).unwrap_or(ClientLoopEvent::Timer),
            event = client.recv() => event.unwrap_or(ClientLoopEvent::Timer),
        }
    } else {
        tokio::select! {
            biased;
            event = client.recv() => event.unwrap_or(ClientLoopEvent::Timer),
            event = history.recv() => event.map(ClientLoopEvent::PiHistory).unwrap_or(ClientLoopEvent::Timer),
        }
    };
    *prefer_history = !matches!(event, ClientLoopEvent::PiHistory(_));
    event
}

/// Internal events for the client event loop.
pub(super) enum ClientLoopEvent {
    #[cfg(unix)]
    StdinInput(Vec<u8>),
    #[cfg(unix)]
    PixelMouse(Vec<u8>, crate::input::mouse::HostGeometry),
    #[cfg(unix)]
    DirectGraphicsResponse(direct_graphics::Response),
    #[cfg(windows)]
    StdinEvents(Vec<crate::protocol::ClientInputEvent>),
    #[cfg(windows)]
    NotificationActivated(shell::ClientSystemNotificationTarget),
    Resize(u16, u16, u32, u32, bool),
    TerminalUnavailable(io::Error),
    ServerMessage {
        endpoint_id: endpoint::ClientEndpointId,
        generation: u64,
        message: Box<ServerMessage>,
    },
    ServerDisconnected {
        endpoint_id: endpoint::ClientEndpointId,
        generation: u64,
    },
    EndpointSupervisor(endpoint::EndpointSupervisorEvent),
    EndpointCatalog(Result<Vec<endpoint::SavedSshEndpoint>, String>),
    ShellActions {
        sessions: Vec<session_switch::SessionAction>,
        history: Vec<pi_history::HistoryClientAction>,
    },
    SessionSwitch(session_switch::SessionSwitchEvent),
    PiHistory(pi_history::HistoryWorkerEvent),
    HostPower(crate::platform::HostPowerEvent),
    ActivateEndpoint {
        endpoint_id: endpoint::ClientEndpointId,
        target: Option<shell::ClientEndpointFocusTarget>,
        /// A superseded handoff deliberately starts a fresh target-on epoch even when source and
        /// latest target have the same identity after restoration.
        force: bool,
    },
    Timer,
}
