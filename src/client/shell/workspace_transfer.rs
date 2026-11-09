use super::*;

pub(crate) const WORKSPACE_TRANSFER_METHODS: [&str; 2] =
    ["workspace.transfer", "workspace.transfer.status"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceTransferSource {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) session: String,
    pub(crate) workspace_id: String,
    pub(crate) generation: u64,
    pub(crate) boot_id: String,
    pub(crate) focused_workspace_id: Option<String>,
    pub(crate) focused_tab_id: Option<String>,
    pub(crate) focused_pane_id: Option<String>,
    pub(crate) navigation_serial: u64,
}

impl WorkspaceTransferSource {
    pub(crate) fn was_active(&self) -> bool {
        self.focused_workspace_id.as_deref() == Some(self.workspace_id.as_str())
    }
}

impl ClientShellState {
    pub(crate) fn workspace_transfer_source(
        &self,
        workspace_id: &str,
    ) -> Option<WorkspaceTransferSource> {
        let endpoint = self.endpoints.iter().find(|endpoint| {
            endpoint.endpoint_id == self.active_endpoint_id
                && endpoint.status == ClientEndpointStatus::Online
                && !endpoint.session_hidden
        })?;
        let methods = endpoint.methods.as_ref()?;
        if !WORKSPACE_TRANSFER_METHODS
            .iter()
            .all(|method| methods.contains(*method))
        {
            return None;
        }
        let generation = endpoint.snapshot_generation?;
        let snapshot = self.snapshot.as_deref()?;
        if self.active_snapshot_generation != Some(generation)
            || endpoint.snapshot.as_deref()?.boot_id != snapshot.boot_id
            || !snapshot
                .workspaces
                .iter()
                .any(|workspace| workspace.workspace_id == workspace_id)
        {
            return None;
        }
        Some(WorkspaceTransferSource {
            endpoint_id: endpoint.endpoint_id.clone(),
            session: endpoint.session_name.clone(),
            workspace_id: workspace_id.to_owned(),
            generation,
            boot_id: snapshot.boot_id.clone(),
            focused_workspace_id: snapshot.focused_workspace_id.clone(),
            focused_tab_id: snapshot.focused_tab_id.clone(),
            focused_pane_id: snapshot.focused_pane_id.clone(),
            navigation_serial: self.session_navigation_serial,
        })
    }

    pub(crate) fn workspace_transfer_source_matches(
        &self,
        source: &WorkspaceTransferSource,
    ) -> bool {
        self.workspace_transfer_identity_matches(source, false)
    }

    fn workspace_transfer_identity_matches(
        &self,
        source: &WorkspaceTransferSource,
        activating: bool,
    ) -> bool {
        self.endpoints.iter().any(|endpoint| {
            endpoint.endpoint_id == source.endpoint_id
                && endpoint.session_name == source.session
                && (activating || endpoint.status == ClientEndpointStatus::Online)
                && endpoint.snapshot_generation == Some(source.generation)
                && endpoint
                    .snapshot
                    .as_deref()
                    .is_some_and(|snapshot| snapshot.boot_id == source.boot_id)
        })
    }

    pub(crate) fn workspace_transfer_view_matches(
        &self,
        source: &WorkspaceTransferSource,
        activating: bool,
    ) -> bool {
        if !self.workspace_transfer_identity_matches(source, activating)
            || self.session_navigation_serial != source.navigation_serial
            || (!activating && self.active_endpoint_id != source.endpoint_id)
        {
            return false;
        }
        let Some(snapshot) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == source.endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
        else {
            return false;
        };
        !snapshot
            .workspaces
            .iter()
            .any(|workspace| workspace.workspace_id == source.workspace_id)
            || (snapshot.focused_workspace_id == source.focused_workspace_id
                && snapshot.focused_tab_id == source.focused_tab_id
                && snapshot.focused_pane_id == source.focused_pane_id)
    }

    pub(crate) fn note_session_navigation(&mut self) {
        self.session_navigation_serial = self.session_navigation_serial.wrapping_add(1);
    }

    pub(crate) fn workspace_transfer_picker_matches(
        &self,
        request_id: u64,
        source: &WorkspaceTransferSource,
    ) -> bool {
        matches!(
            self.overlay.as_ref(),
            Some(ClientShellOverlay::SessionPicker(picker))
                if picker.request_id == request_id
                    && picker.switching
                    && matches!(&picker.purpose,
                        super::sessions::SessionPickerPurpose::WorkspaceTransfer { source: current, .. }
                            if current.as_ref() == source)
        )
    }

    pub(crate) fn workspace_transfer_following(&mut self, request_id: u64) {
        if let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() {
            if picker.request_id == request_id {
                if let super::sessions::SessionPickerPurpose::WorkspaceTransfer { phase, .. } =
                    &mut picker.purpose
                {
                    *phase = super::sessions::WorkspaceTransferPhase::Following;
                    picker.error = None;
                }
            }
        }
    }

    pub(crate) fn workspace_transfer_message(
        &mut self,
        request_id: u64,
        message: Option<String>,
        finished: bool,
    ) -> bool {
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return false;
        };
        let super::sessions::SessionPickerPurpose::WorkspaceTransfer { phase, .. } =
            &mut picker.purpose
        else {
            return false;
        };
        if picker.request_id != request_id {
            return false;
        }
        if finished {
            *phase = super::sessions::WorkspaceTransferPhase::Finished;
            picker.switching = false;
        }
        if picker.error == message && !finished {
            return false;
        }
        picker.error = message;
        true
    }
}
