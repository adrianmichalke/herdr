use super::*;
use crate::api::schema::{Method, ResponseResult, TabMoveToWorkspaceParams, TabTarget};
use crossterm::event::{KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

pub(super) const TAB_MOVE_METHOD: &str = "tab.move_to_workspace";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TabMoveSource {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) session: String,
    pub(super) generation: u64,
    pub(super) boot_id: String,
    pub(super) workspace_id: String,
    pub(super) tab_id: String,
    pub(super) focused_workspace_id: Option<String>,
    pub(super) focused_tab_id: Option<String>,
    pub(super) focused_pane_id: Option<String>,
    pub(super) navigation_serial: u64,
}

impl TabMoveSource {
    pub(super) fn was_active(&self) -> bool {
        self.focused_workspace_id.as_deref() == Some(self.workspace_id.as_str())
            && self.focused_tab_id.as_deref() == Some(self.tab_id.as_str())
    }

    fn focus_matches(&self, snapshot: &ClientShellSnapshot) -> bool {
        snapshot.focused_workspace_id == self.focused_workspace_id
            && snapshot.focused_tab_id == self.focused_tab_id
            && snapshot.focused_pane_id == self.focused_pane_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TabMoveDestination {
    pub(super) workspace_id: String,
    pub(super) label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabMovePhase {
    Selecting,
    Moving,
    Following,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabMoveOutcome {
    NotSubmitted,
    Unknown,
    Rejected,
    Moved,
}

impl TabMoveOutcome {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::NotSubmitted => "Not submitted",
            Self::Unknown => "Outcome unknown",
            Self::Rejected => "Move rejected",
            Self::Moved => "Tab moved",
        }
    }

    pub(super) fn symbol(self) -> &'static str {
        match self {
            Self::NotSubmitted => "×",
            Self::Unknown => "?",
            Self::Rejected => "!",
            Self::Moved => "✓",
        }
    }
}

#[derive(Debug)]
pub(super) struct TabMovePicker {
    pub(super) source: TabMoveSource,
    pub(super) entries: Vec<TabMoveDestination>,
    pub(super) selected: Option<String>,
    pub(super) request_id: Option<String>,
    pub(super) phase: TabMovePhase,
    pub(super) outcome: TabMoveOutcome,
    pub(super) error: Option<String>,
}

impl TabMovePicker {
    pub(super) fn busy(&self) -> bool {
        matches!(self.phase, TabMovePhase::Moving | TabMovePhase::Following)
    }
}

impl ClientShellState {
    pub(super) fn tab_move_source(&self, tab_id: &str) -> Option<TabMoveSource> {
        let endpoint = self.endpoints.iter().find(|endpoint| {
            endpoint.endpoint_id == self.active_endpoint_id
                && endpoint.status == ClientEndpointStatus::Online
                && !endpoint.session_hidden
        })?;
        if !endpoint.methods.as_ref()?.contains(TAB_MOVE_METHOD) {
            return None;
        }
        let generation = endpoint.snapshot_generation?;
        let snapshot = self.snapshot.as_deref()?;
        if self.active_snapshot_generation != Some(generation)
            || endpoint.snapshot.as_deref()?.boot_id != snapshot.boot_id
        {
            return None;
        }
        let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)?;
        if !snapshot
            .workspaces
            .iter()
            .any(|workspace| workspace.workspace_id == tab.workspace_id)
        {
            return None;
        }
        Some(TabMoveSource {
            endpoint_id: endpoint.endpoint_id.clone(),
            session: endpoint.session_name.clone(),
            generation,
            boot_id: snapshot.boot_id.clone(),
            workspace_id: tab.workspace_id.clone(),
            tab_id: tab.tab_id.clone(),
            focused_workspace_id: snapshot.focused_workspace_id.clone(),
            focused_tab_id: snapshot.focused_tab_id.clone(),
            focused_pane_id: snapshot.focused_pane_id.clone(),
            navigation_serial: self.session_navigation_serial,
        })
    }

    fn tab_move_identity_matches(&self, source: &TabMoveSource) -> bool {
        self.active_endpoint_id == source.endpoint_id
            && self.active_snapshot_generation == Some(source.generation)
            && self
                .snapshot
                .as_deref()
                .is_some_and(|snapshot| snapshot.boot_id == source.boot_id)
            && self.endpoints.iter().any(|endpoint| {
                endpoint.endpoint_id == source.endpoint_id
                    && endpoint.session_name == source.session
                    && !endpoint.session_hidden
                    && endpoint.status == ClientEndpointStatus::Online
                    && endpoint.snapshot_generation == Some(source.generation)
                    && endpoint
                        .snapshot
                        .as_deref()
                        .is_some_and(|snapshot| snapshot.boot_id == source.boot_id)
            })
    }

    fn tab_move_destinations(&self, source: &TabMoveSource) -> Vec<TabMoveDestination> {
        if !self.tab_move_identity_matches(source) {
            return Vec::new();
        }
        self.snapshot.as_deref().map_or_else(Vec::new, |snapshot| {
            snapshot
                .workspaces
                .iter()
                .filter(|workspace| workspace.workspace_id != source.workspace_id)
                .map(|workspace| TabMoveDestination {
                    workspace_id: workspace.workspace_id.clone(),
                    label: workspace.label.clone(),
                })
                .collect()
        })
    }

    pub(super) fn open_tab_move_picker(
        &mut self,
        source: TabMoveSource,
        outcome: &mut ClientShellInput,
    ) {
        if self.tab_move_source(&source.tab_id).as_ref() != Some(&source) {
            outcome.repaint |= self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                TAB_MOVE_METHOD,
                "Move unavailable",
                "This tab, view, or server changed. Reopen Move to space.",
            );
            return;
        }
        let entries = self.tab_move_destinations(&source);
        let selected = entries.first().map(|entry| entry.workspace_id.clone());
        self.overlay = Some(ClientShellOverlay::TabMove(TabMovePicker {
            source,
            entries,
            selected,
            request_id: None,
            phase: TabMovePhase::Selecting,
            outcome: TabMoveOutcome::NotSubmitted,
            error: None,
        }));
        self.chrome_drag = None;
        self.workspace_press = None;
        self.tab_press = None;
        outcome.repaint = true;
    }

    pub(super) fn interrupt_tab_move_picker(&mut self) {
        if let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_mut() {
            if picker.phase != TabMovePhase::Finished {
                picker.error = Some(match picker.phase {
                    TabMovePhase::Following => "Tab moved, but opening its destination was interrupted. Open it manually.".into(),
                    TabMovePhase::Moving => "The server or view changed; the move may have completed. Check both spaces before trying again.".into(),
                    TabMovePhase::Selecting | TabMovePhase::Finished => "This tab, view, or server changed. No move was submitted. Reopen Move to space.".into(),
                });
                picker.phase = TabMovePhase::Finished;
            }
        }
    }

    pub(super) fn reconcile_tab_move_picker(&mut self) {
        let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_ref() else {
            return;
        };
        if picker.phase == TabMovePhase::Finished {
            return;
        }
        let source = picker.source.clone();
        if !self.tab_move_identity_matches(&source)
            || (picker.phase == TabMovePhase::Selecting
                && self.tab_move_source(&source.tab_id).as_ref() != Some(&source))
        {
            self.interrupt_tab_move_picker();
            return;
        }
        if picker.phase != TabMovePhase::Selecting {
            return;
        }
        let entries = self.tab_move_destinations(&source);
        if let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_mut() {
            if picker.selected.as_ref().is_some_and(|selected| {
                !entries.iter().any(|entry| &entry.workspace_id == selected)
            }) {
                picker.error =
                    Some("The selected space is no longer available. Choose another space.".into());
            }
            picker.entries = entries;
        }
    }

    fn move_tab_move_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_mut() else {
            return;
        };
        if picker.phase != TabMovePhase::Selecting {
            return;
        }
        let current = picker
            .entries
            .iter()
            .position(|entry| Some(&entry.workspace_id) == picker.selected.as_ref());
        let index = current
            .map(|index| index.saturating_add_signed(delta))
            .unwrap_or(0)
            .min(picker.entries.len().saturating_sub(1));
        picker.selected = picker
            .entries
            .get(index)
            .map(|entry| entry.workspace_id.clone());
        picker.error = None;
    }

    fn cancel_tab_move_picker(&mut self) {
        if matches!(self.overlay.as_ref(), Some(ClientShellOverlay::TabMove(picker)) if !picker.busy())
        {
            self.overlay = None;
            self.resume_mobile_switcher_if_ready();
        }
    }

    fn submit_tab_move(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_ref() else {
            return;
        };
        if picker.phase != TabMovePhase::Selecting {
            return;
        }
        let source = picker.source.clone();
        if self.tab_move_source(&source.tab_id).as_ref() != Some(&source) {
            self.interrupt_tab_move_picker();
            return;
        }
        let Some(workspace_id) = picker.selected.clone().filter(|selected| {
            self.tab_move_destinations(&source)
                .iter()
                .any(|entry| entry.workspace_id == *selected)
        }) else {
            if let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_mut() {
                picker.error =
                    Some("Choose another space in this session. No move was submitted.".into());
            }
            return;
        };
        if self.push_endpoint_method_with_kind(
            Method::TabMoveToWorkspace(TabMoveToWorkspaceParams {
                tab_id: source.tab_id.clone(),
                workspace_id: workspace_id.clone(),
            }),
            PendingEndpointKind::TabMove {
                source: Box::new(source),
                workspace_id,
            },
            outcome,
        ) {
            if let (
                Some(ClientShellOverlay::TabMove(picker)),
                Some(ClientShellAction::Endpoint { request, .. }),
            ) = (self.overlay.as_mut(), outcome.actions.last())
            {
                picker.request_id = Some(request.id.clone());
                picker.phase = TabMovePhase::Moving;
                picker.outcome = TabMoveOutcome::Unknown;
                picker.error = None;
            }
        } else {
            self.interrupt_tab_move_picker();
        }
    }

    fn tab_move_follow_current(
        &self,
        source: &TabMoveSource,
        workspace_id: &str,
        tab_id: &str,
    ) -> bool {
        if !self.tab_move_identity_matches(source)
            || self.session_navigation_serial != source.navigation_serial
        {
            return false;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };
        if !snapshot
            .workspaces
            .iter()
            .any(|workspace| workspace.workspace_id == source.workspace_id)
        {
            return false;
        }
        if let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == source.tab_id) {
            tab.workspace_id == source.workspace_id && source.focus_matches(snapshot)
        } else {
            snapshot.focused_workspace_id.as_deref() == Some(source.workspace_id.as_str())
                && snapshot
                    .tabs
                    .iter()
                    .any(|tab| tab.tab_id == tab_id && tab.workspace_id == workspace_id)
        }
    }

    fn finish_tab_move(&mut self, request_id: &str, message: String) -> bool {
        let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_mut() else {
            return false;
        };
        if picker.request_id.as_deref() != Some(request_id) {
            return false;
        }
        picker.phase = TabMovePhase::Finished;
        picker.error = Some(message);
        true
    }

    pub(super) fn complete_tab_move(
        &mut self,
        boot_id: &str,
        request_id: &str,
        source: TabMoveSource,
        workspace_id: String,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        if !matches!(self.overlay.as_ref(), Some(ClientShellOverlay::TabMove(picker))
            if picker.request_id.as_deref() == Some(request_id)
                && picker.source == source && picker.phase == TabMovePhase::Moving)
        {
            return (false, Vec::new());
        }
        let failure = |state: &mut Self, message: String| {
            (state.finish_tab_move(request_id, message), Vec::new())
        };
        if boot_id != source.boot_id || !self.tab_move_identity_matches(&source) {
            return failure(self, "The server or session changed. The move may have completed; check both spaces before trying again.".into());
        }
        let tab = match result {
            Ok(ResponseResult::TabInfo { tab })
                if tab.workspace_id == workspace_id
                    && workspace_id != source.workspace_id
                    && tab.number > 0
                    && tab.pane_count > 0
                    && tab.tab_id != source.tab_id
                    && tab.tab_id == crate::workspace::public_tab_id_for_number(&workspace_id, tab.number) => tab,
            Ok(_) => return failure(self, "The server returned an invalid move result. Check both spaces before trying again.".into()),
            Err(error) => {
                if !matches!(error.code.as_deref(), None | Some("endpoint_timeout" | "endpoint_cancelled" | "server_unavailable" | "invalid_response")) {
                    if let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_mut() {
                        picker.outcome = TabMoveOutcome::Rejected;
                    }
                }
                let message = match error.code.as_deref() {
                    Some("endpoint_timeout") => "Move timed out; it may have completed. Check both spaces before trying again.".into(),
                    Some("endpoint_cancelled" | "server_unavailable") => "Move interrupted; it may have completed. Check both spaces before trying again.".into(),
                    None | Some("invalid_response") => "Move outcome unknown; the server reply could not be read. It may have completed. Check both spaces before trying again.".into(),
                    _ => format!("Move rejected: {}", error.message),
                };
                return failure(self, message);
            }
        };
        if let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_mut() {
            picker.outcome = TabMoveOutcome::Moved;
        }
        if !self
            .tab_move_destinations(&source)
            .iter()
            .any(|entry| entry.workspace_id == workspace_id)
        {
            return failure(self, "The destination space is no longer available. Check the tab's current location; no follow was requested.".into());
        }
        if !source.was_active() {
            self.overlay = None;
            self.resume_mobile_switcher_if_ready();
            return (true, Vec::new());
        }
        if !self.tab_move_follow_current(&source, &workspace_id, &tab.tab_id) {
            return failure(
                self,
                "Tab moved; your current view was kept because the tab or navigation changed."
                    .into(),
            );
        }
        let mut outcome = ClientShellInput::default();
        if self.push_endpoint_method_with_kind(
            Method::TabFocus(TabTarget {
                tab_id: tab.tab_id.clone(),
            }),
            PendingEndpointKind::TabMoveFollow {
                source: Box::new(source),
                tab_id: tab.tab_id,
            },
            &mut outcome,
        ) {
            if let (
                Some(ClientShellOverlay::TabMove(picker)),
                Some(ClientShellAction::Endpoint { request, .. }),
            ) = (self.overlay.as_mut(), outcome.actions.last())
            {
                picker.phase = TabMovePhase::Following;
                picker.request_id = Some(request.id.clone());
            }
        } else {
            self.finish_tab_move(
                request_id,
                "Tab moved, but opening its destination was unavailable. Open it manually.".into(),
            );
        }
        (true, outcome.actions)
    }

    pub(super) fn complete_tab_move_follow(
        &mut self,
        boot_id: &str,
        request_id: &str,
        source: TabMoveSource,
        tab_id: String,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        if !matches!(self.overlay.as_ref(), Some(ClientShellOverlay::TabMove(picker))
            if picker.request_id.as_deref() == Some(request_id)
                && picker.source == source && picker.phase == TabMovePhase::Following)
        {
            return (false, Vec::new());
        }
        let message = if boot_id != source.boot_id || !self.tab_move_identity_matches(&source) {
            Some("Tab moved, but the server or session changed before it could be opened. Open it manually.".into())
        } else {
            match result {
                Ok(ResponseResult::TabInfo { tab }) if tab.tab_id == tab_id => None,
                Ok(_) => Some(
                    "Tab moved, but the server returned an invalid focus result. Open it manually."
                        .into(),
                ),
                Err(error) => Some(format!(
                    "Tab moved, but opening it was not confirmed: {}. Open it manually.",
                    error.message
                )),
            }
        };
        if let Some(message) = message {
            self.finish_tab_move(request_id, message);
        } else {
            self.overlay = None;
            self.resume_mobile_switcher_if_ready();
        }
        (true, Vec::new())
    }

    pub(super) fn route_tab_move_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_mut() else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            return true;
        }
        if key.code == KeyCode::Esc {
            self.cancel_tab_move_picker();
        } else if picker.phase == TabMovePhase::Selecting {
            match key.code {
                KeyCode::Enter => self.submit_tab_move(outcome),
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    self.move_tab_move_selection(-1)
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    self.move_tab_move_selection(1)
                }
                KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                    self.move_tab_move_selection(-1)
                }
                KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                    self.move_tab_move_selection(1)
                }
                KeyCode::PageUp => self.move_tab_move_selection(-8),
                KeyCode::PageDown => self.move_tab_move_selection(8),
                KeyCode::Home => {
                    picker.selected = picker
                        .entries
                        .first()
                        .map(|entry| entry.workspace_id.clone())
                }
                KeyCode::End => {
                    picker.selected = picker
                        .entries
                        .last()
                        .map(|entry| entry.workspace_id.clone())
                }
                _ => {}
            }
        }
        outcome.repaint = true;
        true
    }

    pub(super) fn handle_tab_move_mouse(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::TabMove(_))) {
            return false;
        }
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_tab_move_selection(-3),
            MouseEventKind::ScrollDown => self.move_tab_move_selection(3),
            MouseEventKind::Down(MouseButton::Left) => {
                if contains(self.hits.overlay_cancel, point)
                    || !contains(self.hits.navigator_popup, point)
                {
                    self.cancel_tab_move_picker();
                } else if contains(self.hits.overlay_primary, point) {
                    self.submit_tab_move(outcome);
                } else if let Some(workspace_id) =
                    self.hits
                        .tab_move_rows
                        .iter()
                        .find_map(|(rect, workspace_id)| {
                            contains(*rect, point).then(|| workspace_id.clone())
                        })
                {
                    if let Some(ClientShellOverlay::TabMove(picker)) = self.overlay.as_mut() {
                        if picker.phase == TabMovePhase::Selecting
                            && picker
                                .entries
                                .iter()
                                .any(|entry| entry.workspace_id == workspace_id)
                        {
                            picker.selected = Some(workspace_id);
                            self.submit_tab_move(outcome);
                        }
                    }
                }
            }
            _ => {}
        }
        outcome.repaint = true;
        true
    }
}
