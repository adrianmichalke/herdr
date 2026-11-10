use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryNavigationView {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) session: String,
    pub(crate) generation: Option<u64>,
    pub(crate) boot_id: Option<String>,
    pub(crate) navigation_serial: u64,
    focused_workspace: Option<String>,
    focused_tab: Option<String>,
    focused_pane: Option<String>,
}

impl ClientShellState {
    pub(crate) fn history_navigation_view(&self) -> Option<HistoryNavigationView> {
        let endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)?;
        let snapshot = self.snapshot.as_deref();
        Some(HistoryNavigationView {
            endpoint_id: self.active_endpoint_id.clone(),
            session: endpoint.session_name.clone(),
            generation: self.active_snapshot_generation,
            boot_id: snapshot.map(|snapshot| snapshot.boot_id.clone()),
            navigation_serial: self.session_navigation_serial,
            focused_workspace: snapshot.and_then(|snapshot| snapshot.focused_workspace_id.clone()),
            focused_tab: snapshot.and_then(|snapshot| snapshot.focused_tab_id.clone()),
            focused_pane: snapshot.and_then(|snapshot| snapshot.focused_pane_id.clone()),
        })
    }

    pub(crate) fn allocate_history_session_request(&mut self) -> u64 {
        self.session_picker_serial = self.session_picker_serial.wrapping_add(1).max(1);
        self.session_picker_serial
    }
}
