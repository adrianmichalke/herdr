use super::*;
use crossterm::event::{KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

impl ClientShellState {
    pub(in crate::client::shell) fn history_shortcut_available(
        &self,
        key: &crate::input::TerminalKey,
    ) -> bool {
        if self.mode == ClientShellMode::Navigate {
            let bindings = &self.config.keybinds.keybinds.navigate;
            return ![
                &bindings.workspace_up,
                &bindings.workspace_down,
                &bindings.pane_left,
                &bindings.pane_down,
                &bindings.pane_up,
                &bindings.pane_right,
            ]
            .iter()
            .any(|binding| binding.matches_direct_key(key))
                && !matches!(
                    key.code,
                    KeyCode::Esc
                        | KeyCode::Enter
                        | KeyCode::Tab
                        | KeyCode::BackTab
                        | KeyCode::Left
                        | KeyCode::Right
                );
        }
        if self.mode == ClientShellMode::Resize {
            return !matches!(
                key.code,
                KeyCode::Esc
                    | KeyCode::Enter
                    | KeyCode::Left
                    | KeyCode::Down
                    | KeyCode::Up
                    | KeyCode::Right
            );
        }
        true
    }

    pub(in crate::client::shell) fn insert_history_text(
        &mut self,
        text: &str,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return false;
        };
        if !history.preview_open
            && history.navigation_request.is_none()
            && history.query.insert(text)
        {
            history.query_focused = true;
            self.restart_history_search(outcome);
        }
        true
    }

    fn preview_history_at(&mut self, anchor: Option<u64>, outcome: &mut ClientShellInput) {
        let request_id = self.next_history_request();
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return;
        };
        let Some(result) = history.selected_result() else {
            return;
        };
        let source_id = result.source.id.clone();
        let Some(anchor) = anchor.or_else(|| {
            result
                .matches
                .get(history.selected.hit)
                .map(|hit| hit.entry_line)
        }) else {
            return;
        };
        if let Some(request) = history.preview_request.take() {
            outcome.actions.push(ClientShellAction::HistoryCancel {
                request_id: request.id,
            });
        }
        if let Some(request_id) = history.navigation_request.take() {
            outcome
                .actions
                .push(ClientShellAction::HistoryCancel { request_id });
        }
        history.preview_request = Some(PreviewRequest {
            id: request_id,
            source_id: source_id.clone(),
        });
        history.preview_open = true;
        history.preview = None;
        history.preview_anchor = Some(anchor);
        history.preview_error = None;
        history.preview_feedback = HistoryDocument::default();
        history.rebuild_preview_feedback();
        history.navigation_error = None;
        history.scrollbar_grab = None;
        self.hits.history = HistoryHitMap::default();
        outcome.actions.push(ClientShellAction::History {
            machine: history.machine.clone(),
            request_id,
            request: history.request(HistoryAction::Preview { source_id, anchor }),
        });
        outcome.repaint = true;
    }

    fn back_from_history_preview(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return;
        };
        if let Some(request) = history.preview_request.take() {
            outcome.actions.push(ClientShellAction::HistoryCancel {
                request_id: request.id,
            });
        }
        if let Some(request_id) = history.navigation_request.take() {
            outcome
                .actions
                .push(ClientShellAction::HistoryCancel { request_id });
        }
        history.preview = None;
        history.preview_anchor = None;
        history.preview_open = false;
        history.preview_error = None;
        history.navigation_error = None;
        history.scrollbar_grab = None;
        history.reveal_selection();
        self.hits.history = HistoryHitMap::default();
        outcome.repaint = true;
    }

    fn move_history_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return;
        };
        let selections: Vec<_> = history
            .results
            .iter()
            .enumerate()
            .flat_map(|(result, value)| {
                (0..value.matches.len()).map(move |hit| HistorySelection { result, hit })
            })
            .collect();
        let index = selections
            .iter()
            .position(|selection| *selection == history.selected)
            .unwrap_or(0);
        if let Some(selection) = selections.get(
            index
                .saturating_add_signed(delta)
                .min(selections.len().saturating_sub(1)),
        ) {
            history.selected = *selection;
            history.reveal_selection();
        }
    }

    fn navigate_history(&mut self, open_session: bool, outcome: &mut ClientShellInput) {
        let request_id = self.next_history_request();
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return;
        };
        if history.navigation_request.is_some() || history.preview_request.is_some() {
            return;
        }
        let Some(location) = history.selected_location().cloned() else {
            return;
        };
        if location.running == open_session {
            return;
        }
        let Some(source) = history.selected_source() else {
            return;
        };
        let source_id = source.id.clone();
        history.navigation_request = Some(request_id);
        history.navigation_error = None;
        outcome.actions.push(ClientShellAction::HistoryNavigate {
            machine: history.machine.clone(),
            request_id,
            request: history.request(HistoryAction::Resolve { source_id }),
            location,
            open_session,
        });
        outcome.repaint = true;
    }

    fn activate_history_control(
        &mut self,
        control: HistoryControl,
        outcome: &mut ClientShellInput,
    ) {
        match control {
            HistoryControl::Close => self.close_history(outcome),
            HistoryControl::Back => self.back_from_history_preview(outcome),
            HistoryControl::Scope(scope) => {
                if let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() {
                    if history.scope != scope {
                        history.scope = scope;
                        self.restart_history_search(outcome);
                    }
                }
            }
            HistoryControl::PreviousHit | HistoryControl::NextHit => {
                let delta = if control == HistoryControl::PreviousHit {
                    -1
                } else {
                    1
                };
                let previous = match self.overlay.as_ref() {
                    Some(ClientShellOverlay::PiHistory(history)) => history.selected,
                    _ => return,
                };
                self.move_history_selection(delta);
                if matches!(self.overlay.as_ref(), Some(ClientShellOverlay::PiHistory(history)) if history.selected != previous)
                {
                    self.preview_history_at(None, outcome);
                }
            }
            HistoryControl::PreviousPage | HistoryControl::NextPage => {
                let anchor = match self.overlay.as_ref() {
                    Some(ClientShellOverlay::PiHistory(history)) => {
                        history.preview.as_ref().and_then(|preview| {
                            if control == HistoryControl::PreviousPage {
                                preview.value.previous
                            } else {
                                preview.value.next
                            }
                        })
                    }
                    _ => None,
                };
                if let Some(anchor) = anchor {
                    self.preview_history_at(Some(anchor), outcome);
                }
            }
            HistoryControl::PreviousLocation | HistoryControl::NextLocation => {
                if let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() {
                    if history.navigation_request.is_none() {
                        if let Some(preview) = history.preview.as_mut() {
                            let delta = if control == HistoryControl::PreviousLocation {
                                -1
                            } else {
                                1
                            };
                            preview.location =
                                (!preview.value.source.locations.is_empty()).then(|| {
                                    preview.location.map_or(0, |index| {
                                        index
                                            .saturating_add_signed(delta)
                                            .min(preview.value.source.locations.len() - 1)
                                    })
                                });
                            history.navigation_error = None;
                        }
                    }
                }
                self.hits.history = HistoryHitMap::default();
            }
            HistoryControl::FocusRunning => self.navigate_history(false, outcome),
            HistoryControl::OpenSession => self.navigate_history(true, outcome),
            HistoryControl::Retry => {
                if matches!(self.overlay.as_ref(), Some(ClientShellOverlay::PiHistory(history)) if history.preview_open)
                {
                    let anchor = match self.overlay.as_ref() {
                        Some(ClientShellOverlay::PiHistory(history)) => history.preview_anchor,
                        _ => None,
                    };
                    self.preview_history_at(anchor, outcome);
                } else {
                    self.restart_history_search(outcome);
                }
            }
        }
        outcome.repaint = true;
    }

    fn prepare_history_input(&mut self) {
        let (cols, rows) = self.last_composed_size.unwrap_or((100, 30));
        self.prepare_history_layout(cols, rows);
    }

    fn scroll_history(&mut self, delta: isize) {
        self.prepare_history_input();
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return;
        };
        let document = history.document_mut();
        document.target = None;
        document.scroll = document.scroll.saturating_add_signed(delta);
        self.prepare_history_input();
    }

    pub(in crate::client::shell) fn route_history_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            return true;
        }
        let preview = history.preview_open;
        let text_character = crate::input::keybind_help_text_char(key);
        let control = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) if preview => Some(HistoryControl::Back),
            (KeyCode::Esc, _) => Some(HistoryControl::Close),
            (KeyCode::Char('1'), KeyModifiers::CONTROL) => {
                Some(HistoryControl::Scope(HistoryScope::Space))
            }
            (KeyCode::Char('2'), KeyModifiers::CONTROL) => {
                Some(HistoryControl::Scope(HistoryScope::Session))
            }
            (KeyCode::Char('3'), KeyModifiers::CONTROL) => {
                Some(HistoryControl::Scope(HistoryScope::All))
            }
            (KeyCode::F(5), _) => Some(HistoryControl::Retry),
            (KeyCode::Char('g'), KeyModifiers::CONTROL) if preview => {
                Some(HistoryControl::FocusRunning)
            }
            (KeyCode::Char('o'), KeyModifiers::CONTROL) if preview => {
                Some(HistoryControl::OpenSession)
            }
            (KeyCode::Char('n'), KeyModifiers::CONTROL) if preview => Some(HistoryControl::NextHit),
            (KeyCode::Char('p'), KeyModifiers::CONTROL) if preview => {
                Some(HistoryControl::PreviousHit)
            }
            _ if preview
                && (text_character == Some('N')
                    || key.code == KeyCode::Char('n') && key.modifiers == KeyModifiers::SHIFT) =>
            {
                Some(HistoryControl::PreviousHit)
            }
            _ if preview && text_character == Some('n') => Some(HistoryControl::NextHit),
            (KeyCode::Left | KeyCode::Char('['), KeyModifiers::NONE) if preview => {
                Some(HistoryControl::PreviousPage)
            }
            (KeyCode::Right | KeyCode::Char(']'), KeyModifiers::NONE) if preview => {
                Some(HistoryControl::NextPage)
            }
            (KeyCode::Backspace, KeyModifiers::NONE) if preview => Some(HistoryControl::Back),
            (KeyCode::Tab, KeyModifiers::NONE) if preview => Some(HistoryControl::NextLocation),
            (KeyCode::BackTab, _) | (KeyCode::Tab, KeyModifiers::SHIFT) if preview => {
                Some(HistoryControl::PreviousLocation)
            }
            _ => None,
        };
        if let Some(control) = control {
            self.activate_history_control(control, outcome);
            return true;
        }
        if preview {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.scroll_history(-1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_history(1),
                KeyCode::PageUp => self.scroll_history(-8),
                KeyCode::PageDown => self.scroll_history(8),
                KeyCode::Home | KeyCode::End => {
                    self.scroll_history(if key.code == KeyCode::Home {
                        isize::MIN
                    } else {
                        isize::MAX
                    });
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Enter => self.preview_history_at(None, outcome),
                KeyCode::Up => self.move_history_selection(-1),
                KeyCode::Down => self.move_history_selection(1),
                KeyCode::PageUp => self.scroll_history(-8),
                KeyCode::PageDown => self.scroll_history(8),
                KeyCode::Tab | KeyCode::BackTab => history.query_focused = !history.query_focused,
                _ if history.query_focused => {
                    if history.query.handle_key(key).is_some_and(|changed| changed) {
                        self.restart_history_search(outcome);
                    }
                }
                KeyCode::Char('/') => history.query_focused = true,
                _ => {}
            }
        }
        outcome.repaint = true;
        true
    }

    pub(in crate::client::shell) fn handle_history_mouse(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::PiHistory(_))) {
            return false;
        }
        self.prepare_history_input();
        if let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_ref() {
            let (cols, rows) = self.last_composed_size.unwrap_or((100, 30));
            self.hits.history = overlay::history_hit_map(history, Rect::new(0, 0, cols, rows));
        }
        let point = (mouse.column, mouse.row);
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && !contains(self.hits.history.area, point)
        {
            return true;
        }
        let control = self
            .hits
            .history
            .controls
            .iter()
            .find_map(|(rect, control)| contains(*rect, point).then_some(*control));
        let hit = self
            .hits
            .history
            .rows
            .iter()
            .find_map(|(rect, selection)| contains(*rect, point).then_some(*selection));
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return true;
        };
        let preview = history.preview_open;
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(control) = control {
                    self.activate_history_control(control, outcome);
                } else if contains(self.hits.history.query, point) && !preview {
                    history.query_focused = true;
                } else if let (true, Some(metrics)) = (
                    contains(self.hits.history.scrollbar, point),
                    self.hits.history.metrics,
                ) {
                    let track = self.hits.history.scrollbar;
                    history.scrollbar_grab =
                        crate::ui::scrollbar_thumb_grab_offset(metrics, track, mouse.row);
                    if history.scrollbar_grab.is_none() {
                        let offset =
                            crate::ui::scrollbar_offset_from_row(metrics, track, mouse.row);
                        history.document_mut().scroll =
                            metrics.max_offset_from_bottom.saturating_sub(offset);
                    }
                } else if let Some(hit) = hit.filter(|_| !preview) {
                    history.selected = hit;
                    history.query_focused = false;
                    self.preview_history_at(None, outcome);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let (Some(grab), Some(metrics)) =
                    (history.scrollbar_grab, self.hits.history.metrics)
                {
                    let offset = crate::ui::scrollbar_offset_from_drag_row(
                        metrics,
                        self.hits.history.scrollbar,
                        mouse.row,
                        grab,
                    );
                    history.document_mut().scroll =
                        metrics.max_offset_from_bottom.saturating_sub(offset);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => history.scrollbar_grab = None,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if contains(self.hits.history.body, point) =>
            {
                self.scroll_history(if mouse.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                });
            }
            _ => {}
        }
        outcome.repaint = true;
        true
    }
}
