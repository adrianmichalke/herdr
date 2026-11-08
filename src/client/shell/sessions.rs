use super::*;
use crossterm::event::{KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

#[derive(Debug, Clone)]
pub(super) struct SessionPickerOverlay {
    pub(super) machine: ClientEndpointId,
    pub(super) machine_label: String,
    pub(super) current: String,
    pub(super) request_id: u64,
    pub(super) entries: Vec<crate::session::SessionInfo>,
    pub(super) loading: bool,
    pub(super) loaded: bool,
    pub(super) error: Option<String>,
    pub(super) selected: usize,
    pub(super) new_name: Option<TextEditor>,
    pub(super) switching: bool,
}

impl SessionPickerOverlay {
    pub(super) fn name_error(&self) -> Option<String> {
        let name = self.new_name.as_ref()?.as_str();
        if let Err(error) = crate::session::validate_name(name) {
            return Some(error);
        }
        if name == crate::session::DEFAULT_SESSION_NAME
            || name == self.current
            || self.entries.iter().any(|entry| entry.name == name)
        {
            return Some(format!("session '{name}' already exists"));
        }
        None
    }
}

pub(super) fn active_session_name<'a>(
    endpoints: &'a [ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
) -> &'a str {
    endpoints
        .iter()
        .find(|endpoint| {
            !endpoint.session_hidden
                && endpoint.endpoint_id.machine_ref() == active_endpoint_id.machine_ref()
        })
        .map_or("unavailable", |endpoint| endpoint.session_name.as_str())
}

pub(super) fn render_selector(
    buffer: &mut Buffer,
    area: Rect,
    name: &str,
    palette: &Palette,
    hits: &mut ShellHitMap,
) {
    let area = area.intersection(buffer.area);
    if area.is_empty() {
        return;
    }
    let rect = Rect::new(area.x, area.y, area.width, 1);
    let label = if rect.width >= 12 {
        format!(
            "Session: {} ▾",
            crate::ui::truncate_end(name, usize::from(rect.width.saturating_sub(11)))
        )
    } else if rect.width >= 9 {
        "Session ▾".to_owned()
    } else if rect.width >= 2 {
        "S▾".to_owned()
    } else {
        "▾".to_owned()
    };
    let style = Style::default()
        .fg(palette.text)
        .bg(palette.surface0)
        .add_modifier(Modifier::BOLD);
    for x in rect.x..rect.right() {
        buffer[(x, rect.y)].set_symbol(" ").set_style(style);
    }
    render::put_text(buffer, rect.x, rect.y, rect.width, &label, style);
    hits.session_selector = rect;
}

pub(super) fn render_sidebar_selector(
    buffer: &mut Buffer,
    area: Rect,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    palette: &Palette,
    hits: &mut ShellHitMap,
) -> Rect {
    render_selector(
        buffer,
        Rect::new(
            area.x,
            area.y,
            area.width.saturating_sub(u16::from(area.width > 1)),
            area.height.min(1),
        ),
        active_session_name(endpoints, active_endpoint_id),
        palette,
        hits,
    );
    Rect::new(
        area.x,
        area.y.saturating_add(u16::from(area.height > 0)),
        area.width,
        area.height.saturating_sub(1),
    )
}

impl ClientShellState {
    pub(super) fn session_selector_blocks_patch(
        &self,
        patch: &crate::protocol::PaneSurfacePatch,
    ) -> bool {
        let Some((cols, rows)) = self.last_composed_size else {
            return false;
        };
        let pane = self.layout(cols, rows).pane_surface;
        let selector = self.hits.session_selector.intersection(pane);
        if selector.is_empty() {
            return false;
        }
        let relative = Rect::new(
            selector.x.saturating_sub(pane.x),
            selector.y.saturating_sub(pane.y),
            selector.width,
            selector.height,
        );
        patch
            .cursor
            .as_ref()
            .is_some_and(|cursor| contains(relative, (cursor.x, cursor.y)))
            || patch.rows.iter().any(|row| {
                !relative
                    .intersection(Rect::new(
                        row.x,
                        row.y,
                        row.cells.len().min(u16::MAX as usize) as u16,
                        1,
                    ))
                    .is_empty()
            })
    }

    fn next_session_picker_request(&mut self) -> u64 {
        self.session_picker_serial = self.session_picker_serial.wrapping_add(1).max(1);
        self.session_picker_serial
    }

    pub(crate) fn open_session_picker(&mut self, outcome: &mut ClientShellInput) {
        let machine = self.active_endpoint_id.machine_id();
        self.open_machine_session_picker(&machine, outcome);
    }

    pub(super) fn open_machine_session_picker(
        &mut self,
        machine: &ClientEndpointId,
        outcome: &mut ClientShellInput,
    ) {
        let Some(endpoint) = self.visible_endpoint_for_machine(machine) else {
            return;
        };
        let machine = endpoint.endpoint_id.machine_id();
        let machine_label = endpoint.label.clone();
        let current = endpoint.session_name.clone();
        if let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_ref() {
            outcome.actions.push(ClientShellAction::SessionCancel {
                request_id: picker.request_id,
            });
        }
        let request_id = self.next_session_picker_request();
        self.overlay = Some(ClientShellOverlay::SessionPicker(SessionPickerOverlay {
            machine: machine.clone(),
            machine_label,
            current,
            request_id,
            entries: Vec::new(),
            loading: true,
            loaded: false,
            error: None,
            selected: 0,
            new_name: None,
            switching: false,
        }));
        self.hits.worktree_rows.clear();
        self.hits.overlay_primary = Rect::default();
        self.hits.overlay_clear = Rect::default();
        self.hits.overlay_cancel = Rect::default();
        self.hits.worktree_search = Rect::default();
        outcome.actions.push(ClientShellAction::SessionList {
            machine,
            request_id,
        });
        outcome.repaint = true;
    }

    pub(crate) fn receive_session_list(
        &mut self,
        machine: &ClientEndpointId,
        request_id: u64,
        result: Result<Vec<crate::session::SessionInfo>, String>,
    ) -> bool {
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return false;
        };
        if &picker.machine != machine
            || picker.request_id != request_id
            || !picker.loading
            || picker.switching
        {
            return false;
        }
        picker.loading = false;
        match result {
            Ok(mut entries) => {
                entries.sort_by(|left, right| {
                    right
                        .default
                        .cmp(&left.default)
                        .then_with(|| left.name.cmp(&right.name))
                });
                entries.dedup_by(|left, right| left.name == right.name);
                picker.selected = entries
                    .iter()
                    .position(|entry| entry.name == picker.current)
                    .unwrap_or(0);
                picker.entries = entries;
                picker.loaded = true;
                picker.error = None;
            }
            Err(error) => {
                picker.loaded = false;
                picker.error = Some(error);
            }
        }
        true
    }

    pub(crate) fn finish_session_switch(
        &mut self,
        request_id: u64,
        result: Result<(), String>,
    ) -> bool {
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return false;
        };
        if picker.request_id != request_id || !picker.switching {
            return false;
        }
        match result {
            Ok(()) => self.overlay = None,
            Err(error) => {
                picker.switching = false;
                picker.error = Some(error);
            }
        }
        true
    }

    pub(super) fn cancel_session_picker(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        if !picker.switching && picker.new_name.take().is_some() {
            picker.error = None;
        } else {
            outcome.actions.push(ClientShellAction::SessionCancel {
                request_id: picker.request_id,
            });
            self.overlay = None;
        }
        outcome.repaint = true;
    }

    fn reload_session_picker(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_ref() else {
            return;
        };
        if picker.switching || picker.loading || picker.new_name.is_some() {
            return;
        }
        let request_id = self.next_session_picker_request();
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        picker.request_id = request_id;
        picker.loading = true;
        picker.loaded = false;
        picker.error = None;
        outcome.actions.push(ClientShellAction::SessionList {
            machine: picker.machine.clone(),
            request_id,
        });
        outcome.repaint = true;
    }

    fn new_session_form(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        if picker.loaded && !picker.loading && !picker.switching {
            picker.new_name = Some(TextEditor::default());
            picker.error = None;
            self.hits.worktree_rows.clear();
            self.hits.overlay_primary = Rect::default();
            self.hits.overlay_clear = Rect::default();
            self.hits.worktree_search = Rect::default();
            outcome.repaint = true;
        }
    }

    fn accept_session_picker(&mut self, outcome: &mut ClientShellInput) {
        let current_is_active = match self.overlay.as_ref() {
            Some(ClientShellOverlay::SessionPicker(picker)) => self
                .visible_endpoint_for_machine(&picker.machine)
                .is_some_and(|endpoint| {
                    endpoint.endpoint_id == self.active_endpoint_id
                        && endpoint.status == ClientEndpointStatus::Online
                }),
            _ => false,
        };
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        if picker.loading || picker.switching {
            return;
        }
        if !picker.loaded {
            self.reload_session_picker(outcome);
            return;
        }
        let create = picker.new_name.is_some();
        let session = if let Some(input) = picker.new_name.as_ref() {
            if let Some(error) = picker.name_error() {
                picker.error = Some(error);
                outcome.repaint = true;
                return;
            }
            input.as_str().to_owned()
        } else if let Some(entry) = picker.entries.get(picker.selected) {
            if entry.name == picker.current
                && entry.running
                && entry.connection_error.is_none()
                && current_is_active
            {
                self.cancel_session_picker(outcome);
                return;
            }
            entry.name.clone()
        } else {
            self.new_session_form(outcome);
            return;
        };
        let request_id = self.next_session_picker_request();
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        picker.request_id = request_id;
        picker.switching = true;
        picker.error = None;
        outcome.actions.push(ClientShellAction::SessionOpen {
            machine: picker.machine.clone(),
            request_id,
            session,
            create,
        });
        outcome.repaint = true;
    }

    fn move_session_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return;
        };
        if picker.loaded && !picker.loading && !picker.switching && picker.new_name.is_none() {
            picker.selected = picker
                .selected
                .saturating_add_signed(delta)
                .min(picker.entries.len());
            picker.error = None;
        }
    }

    pub(super) fn insert_session_picker_text(&mut self, text: &str) -> bool {
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return false;
        };
        if !picker.switching {
            if let Some(input) = picker.new_name.as_mut() {
                input.insert(text);
                picker.error = None;
            }
        }
        true
    }

    pub(super) fn route_session_picker_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            return true;
        }
        if key.code == KeyCode::Esc {
            self.cancel_session_picker(outcome);
            return true;
        }
        if picker.switching {
            return true;
        }
        if key.code == KeyCode::Enter {
            self.accept_session_picker(outcome);
        } else if let Some(input) = picker.new_name.as_mut() {
            if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
                input.clear();
                picker.error = None;
            } else if input.handle_key(key).is_some_and(|changed| changed) {
                picker.error = None;
            }
        } else {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_session_selection(-1),
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.move_session_selection(1),
                KeyCode::BackTab => self.move_session_selection(-1),
                KeyCode::Char('p') if key.modifiers == KeyModifiers::CONTROL => {
                    self.move_session_selection(-1)
                }
                KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                    self.move_session_selection(1)
                }
                KeyCode::PageUp => self.move_session_selection(-8),
                KeyCode::PageDown => self.move_session_selection(8),
                KeyCode::Home => picker.selected = 0,
                KeyCode::End => picker.selected = picker.entries.len(),
                KeyCode::Char('n') => self.new_session_form(outcome),
                KeyCode::Char('r') => self.reload_session_picker(outcome),
                _ => {}
            }
        }
        outcome.repaint = true;
        true
    }

    pub(super) fn handle_session_picker_mouse(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::SessionPicker(_))) {
            return false;
        }
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_session_selection(-3),
            MouseEventKind::ScrollDown => self.move_session_selection(3),
            MouseEventKind::Down(MouseButton::Left) => {
                if contains(self.hits.overlay_cancel, point)
                    || !contains(self.hits.navigator_popup, point)
                {
                    self.cancel_session_picker(outcome);
                } else if contains(self.hits.overlay_primary, point) {
                    if matches!(self.overlay.as_ref(), Some(ClientShellOverlay::SessionPicker(picker)) if picker.new_name.is_some())
                    {
                        self.accept_session_picker(outcome);
                    } else {
                        self.new_session_form(outcome);
                    }
                } else if contains(self.hits.overlay_clear, point) {
                    self.reload_session_picker(outcome);
                } else if let Some(index) = self
                    .hits
                    .worktree_rows
                    .iter()
                    .find_map(|(rect, index)| contains(*rect, point).then_some(*index))
                {
                    if let Some(ClientShellOverlay::SessionPicker(picker)) = self.overlay.as_mut() {
                        if picker.loaded
                            && !picker.loading
                            && !picker.switching
                            && picker.new_name.is_none()
                        {
                            picker.selected = index;
                            self.accept_session_picker(outcome);
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
