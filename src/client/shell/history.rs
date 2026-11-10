use super::*;
use crate::pi_history::types::{
    HistoryAction, HistoryEvent, HistoryLocation, HistoryPreview, HistoryRequest, HistoryResult,
    HistoryScope, HistorySource, PROTOCOL_VERSION,
};

mod document;
mod input;
#[cfg(test)]
mod keybinding_tests;
mod overlay;
#[cfg(test)]
mod tests;

use document::{HistoryDocument, HistoryLine, ScrollTarget};
pub(super) use overlay::render_history_overlay;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct HistorySelection {
    result: usize,
    hit: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryControl {
    Close,
    Back,
    Scope(HistoryScope),
    PreviousHit,
    NextHit,
    PreviousPage,
    NextPage,
    PreviousLocation,
    NextLocation,
    FocusRunning,
    OpenSession,
    Retry,
}

#[derive(Default)]
pub(super) struct HistoryHitMap {
    pub(super) area: Rect,
    query: Rect,
    body: Rect,
    controls: Vec<(Rect, HistoryControl)>,
    rows: Vec<(Rect, HistorySelection)>,
    scrollbar: Rect,
    metrics: Option<crate::pane::ScrollMetrics>,
}

#[derive(Debug, Default)]
struct HistoryResultsSummary {
    hits: usize,
    truncated: bool,
    first: Option<HistorySelection>,
    last: Option<HistorySelection>,
}

#[derive(Debug)]
struct PreviewRequest {
    id: u64,
    source_id: String,
}

#[derive(Debug)]
struct HistoryPreviewState {
    value: HistoryPreview,
    document: HistoryDocument,
    location: Option<usize>,
}

#[derive(Debug)]
pub(super) struct PiHistoryOverlay {
    machine: ClientEndpointId,
    machine_label: String,
    session: String,
    workspace_id: Option<String>,
    workspace_label: String,
    query: TextEditor,
    query_focused: bool,
    scope: HistoryScope,
    search_request: Option<u64>,
    preview_request: Option<PreviewRequest>,
    navigation_request: Option<u64>,
    results: Vec<HistoryResult>,
    summary: HistoryResultsSummary,
    selected: HistorySelection,
    results_document: HistoryDocument,
    preview: Option<HistoryPreviewState>,
    preview_feedback: HistoryDocument,
    preview_anchor: Option<u64>,
    preview_open: bool,
    progress: Option<(usize, usize)>,
    unavailable: Vec<String>,
    truncated: bool,
    error: Option<String>,
    preview_error: Option<String>,
    navigation_error: Option<String>,
    scrollbar_grab: Option<u16>,
}

fn location_text(location: &HistoryLocation) -> String {
    format!(
        "{} / {} [{}] / {} [{}] / {} [{}] · {}{} · {} {:?}: {} ({})",
        location.session,
        location.workspace_label,
        location.workspace_id,
        location.tab_label,
        location.tab_id,
        location.pane_label.as_deref().unwrap_or("pane"),
        location.pane_id,
        if location.running {
            "running"
        } else {
            "stopped"
        },
        location
            .terminal_id
            .as_ref()
            .map(|id| format!(" · terminal {id}"))
            .unwrap_or_default(),
        location.agent_session.agent,
        location.agent_session.kind,
        location.agent_session.value,
        location.agent_session.source,
    )
}

impl PiHistoryOverlay {
    pub(super) fn navigation_active(&self) -> bool {
        self.navigation_request.is_some()
    }

    pub(super) fn query_active(&self) -> bool {
        !self.preview_open && self.query_focused && self.navigation_request.is_none()
    }

    pub(super) fn release_pointer(&mut self) {
        self.scrollbar_grab = None;
    }

    fn request(&self, action: HistoryAction) -> Box<HistoryRequest> {
        Box::new(HistoryRequest {
            scope: self.scope,
            session: self.session.clone(),
            workspace_id: self.workspace_id.clone(),
            query: self.query.as_str().to_owned(),
            action,
        })
    }

    fn selected_result(&self) -> Option<&HistoryResult> {
        self.results.get(self.selected.result)
    }

    fn selected_source(&self) -> Option<&HistorySource> {
        self.preview
            .as_ref()
            .filter(|_| self.preview_open)
            .map(|preview| &preview.value.source)
            .or_else(|| self.selected_result().map(|result| &result.source))
    }

    fn selected_location(&self) -> Option<&HistoryLocation> {
        let preview = self.preview.as_ref().filter(|_| self.preview_open)?;
        preview.value.source.locations.get(preview.location?)
    }

    fn cancel_requests(&mut self, outcome: &mut ClientShellInput) {
        for request_id in [
            self.search_request.take(),
            self.preview_request.take().map(|request| request.id),
            self.navigation_request.take(),
        ]
        .into_iter()
        .flatten()
        {
            outcome
                .actions
                .push(ClientShellAction::HistoryCancel { request_id });
        }
    }

    fn rebuild_results(&mut self) {
        let mut lines = Vec::new();
        self.summary = HistoryResultsSummary::default();
        for (result_index, result) in self.results.iter().enumerate() {
            self.summary.hits += result.matches.len();
            self.summary.truncated |= result.truncated;
            lines.push(HistoryLine::heading(format!(
                "{} · {} hit{}{}",
                result.source.name,
                result.matches.len(),
                if result.matches.len() == 1 { "" } else { "s" },
                if result.truncated {
                    " · truncated"
                } else {
                    ""
                },
            )));
            for location in &result.source.locations {
                lines.push(HistoryLine::plain(location_text(location)));
            }
            for (hit_index, hit) in result.matches.iter().enumerate() {
                let selection = HistorySelection {
                    result: result_index,
                    hit: hit_index,
                };
                self.summary.first.get_or_insert(selection);
                self.summary.last = Some(selection);
                lines.push(HistoryLine::hit(
                    format!("  {} · entry {}: {}", hit.role, hit.entry_line, hit.excerpt),
                    selection,
                ));
            }
            lines.push(HistoryLine::plain(String::new()));
        }
        if let Some(error) = &self.error {
            lines.push(HistoryLine::warning(error.clone()));
        }
        for message in &self.unavailable {
            lines.push(HistoryLine::warning(message.clone()));
        }
        if lines.is_empty() {
            lines.push(HistoryLine::plain(if self.search_request.is_some() {
                "Searching saved Pi entries…".to_owned()
            } else if self.query.is_empty() {
                "Type a literal query to search saved Pi entries.".to_owned()
            } else {
                "No matching saved entries in this scope.".to_owned()
            }));
        }
        self.results_document.replace(lines);
    }

    fn reveal_selection(&mut self) {
        if let Some(line) = self
            .results_document
            .lines
            .iter()
            .position(|line| line.hit == Some(self.selected))
        {
            self.results_document.target = Some(ScrollTarget::Reveal(line));
        }
    }

    fn document_mut(&mut self) -> &mut HistoryDocument {
        if self.preview_open {
            if let Some(preview) = self.preview.as_mut() {
                return &mut preview.document;
            }
            return &mut self.preview_feedback;
        }
        &mut self.results_document
    }

    fn rebuild_preview_feedback(&mut self) {
        self.preview_feedback
            .replace(vec![match &self.preview_error {
                Some(error) => HistoryLine::warning(error.clone()),
                None => HistoryLine::plain("Loading read-only transcript…".to_owned()),
            }]);
    }

    fn document(&self) -> Option<&HistoryDocument> {
        if self.preview_open {
            Some(
                self.preview
                    .as_ref()
                    .map_or(&self.preview_feedback, |preview| &preview.document),
            )
        } else {
            Some(&self.results_document)
        }
    }
}

impl ClientShellState {
    fn next_history_request(&mut self) -> u64 {
        self.history_serial = self.history_serial.wrapping_add(1).max(1);
        self.history_serial
    }

    pub(super) fn open_history(&mut self, outcome: &mut ClientShellInput) {
        if self.overlay.is_some()
            || self.popup_pending
            || self.popup_terminal_id.is_some()
            || !matches!(
                self.mode,
                ClientShellMode::Terminal | ClientShellMode::Navigate | ClientShellMode::Resize
            )
        {
            return;
        }
        let target = self
            .navigate_workspace_id
            .as_ref()
            .filter(|target| self.navigation_target_valid(target));
        let selected_machine = target
            .map(|target| &target.endpoint_id)
            .unwrap_or(&self.active_endpoint_id);
        let Some(endpoint) = self.visible_endpoint_for_machine(selected_machine) else {
            return;
        };
        let machine = endpoint.endpoint_id.machine_id();
        let workspace_id = target
            .map(|target| target.workspace_id.clone())
            .or_else(|| {
                endpoint
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.focused_workspace_id.clone())
            });
        let workspace_label = workspace_id
            .as_deref()
            .and_then(|id| {
                endpoint
                    .snapshot
                    .as_deref()?
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.workspace_id == id)
            })
            .map(|workspace| workspace.label.clone())
            .unwrap_or_else(|| "unavailable space".to_owned());
        self.overlay = Some(ClientShellOverlay::PiHistory(Box::new(PiHistoryOverlay {
            machine,
            machine_label: endpoint.label.clone(),
            session: endpoint.session_name.clone(),
            workspace_id,
            workspace_label,
            query: TextEditor::default(),
            query_focused: true,
            scope: HistoryScope::Space,
            search_request: None,
            preview_request: None,
            navigation_request: None,
            results: Vec::new(),
            summary: HistoryResultsSummary::default(),
            selected: HistorySelection::default(),
            results_document: HistoryDocument::default(),
            preview: None,
            preview_feedback: HistoryDocument::default(),
            preview_anchor: None,
            preview_open: false,
            progress: None,
            unavailable: Vec::new(),
            truncated: false,
            error: None,
            preview_error: None,
            navigation_error: None,
            scrollbar_grab: None,
        })));
        self.chrome_drag = None;
        self.word_selection_gesture = None;
        self.stop_selection_autoscroll();
        self.restart_history_search(outcome);
    }

    fn restart_history_search(&mut self, outcome: &mut ClientShellInput) {
        let request_id = self.next_history_request();
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return;
        };
        history.cancel_requests(outcome);
        history.search_request = Some(request_id);
        history.results.clear();
        history.selected = HistorySelection::default();
        history.results_document = HistoryDocument::default();
        history.preview = None;
        history.preview_anchor = None;
        history.preview_open = false;
        history.progress = None;
        history.unavailable.clear();
        history.truncated = false;
        history.error = None;
        history.preview_error = None;
        history.navigation_error = None;
        history.scrollbar_grab = None;
        history.rebuild_results();
        self.hits.history = HistoryHitMap::default();
        outcome.actions.push(ClientShellAction::History {
            machine: history.machine.clone(),
            request_id,
            request: history.request(HistoryAction::Search),
        });
        outcome.repaint = true;
    }

    pub(super) fn close_history(&mut self, outcome: &mut ClientShellInput) {
        if let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() {
            history.cancel_requests(outcome);
            self.overlay = None;
            self.hits.history = HistoryHitMap::default();
            outcome.repaint = true;
        }
    }

    pub(crate) fn history_request_is_current(
        &self,
        machine: &ClientEndpointId,
        request_id: u64,
    ) -> bool {
        matches!(self.overlay.as_ref(), Some(ClientShellOverlay::PiHistory(history))
            if &history.machine == machine && (history.search_request == Some(request_id)
                || history.preview_request.as_ref().is_some_and(|request| request.id == request_id)
                || history.navigation_request == Some(request_id)))
    }

    pub(crate) fn receive_history_event(
        &mut self,
        machine: &ClientEndpointId,
        request_id: u64,
        event: HistoryEvent,
    ) -> bool {
        if !self.history_request_is_current(machine, request_id) {
            return false;
        }
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return false;
        };
        let search = history.search_request == Some(request_id);
        let preview = history
            .preview_request
            .as_ref()
            .is_some_and(|request| request.id == request_id);
        let navigation = history.navigation_request == Some(request_id);
        match event {
            HistoryEvent::Hello { version } if version != PROTOCOL_VERSION => {
                let message = format!("Pi history helper version {version} is unsupported");
                if search {
                    history.error = Some(message);
                    history.search_request = None;
                    history.rebuild_results();
                } else if preview {
                    history.preview_error = Some(message);
                    history.preview_request = None;
                } else {
                    history.navigation_error = Some(message);
                }
            }
            HistoryEvent::Hello { .. } => return false,
            HistoryEvent::Result { result } if search => {
                if let Some(existing) = history
                    .results
                    .iter_mut()
                    .find(|existing| existing.source.id == result.source.id)
                {
                    *existing = result;
                } else {
                    history.results.push(result);
                }
                history.rebuild_results();
            }
            HistoryEvent::Progress { scanned, total } if search => {
                history.progress = Some((scanned, total))
            }
            HistoryEvent::Unavailable { locations, message } => {
                let detail = if locations.is_empty() {
                    message
                } else {
                    format!(
                        "{message}: {}",
                        locations
                            .iter()
                            .map(location_text)
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                };
                if search {
                    history.unavailable.push(detail);
                    history.rebuild_results();
                } else if preview {
                    history.preview_error = Some(detail);
                } else {
                    history.navigation_error = Some(detail);
                }
            }
            HistoryEvent::Preview { preview: value } if preview => {
                if history
                    .preview_request
                    .as_ref()
                    .is_none_or(|request| request.source_id != value.source.id)
                {
                    return false;
                }
                let document = HistoryDocument::from_preview(&value);
                history.preview = Some(HistoryPreviewState {
                    location: (!value.source.locations.is_empty()).then_some(0),
                    value,
                    document,
                });
                self.hits.history = HistoryHitMap::default();
            }
            HistoryEvent::Resolved { source } if navigation => {
                if history
                    .selected_source()
                    .is_none_or(|selected| selected.id != source.id)
                {
                    return false;
                }
                if let Some(result) = history
                    .results
                    .iter_mut()
                    .find(|result| result.source.id == source.id)
                {
                    result.source = source.clone();
                }
                if let Some(preview) = history.preview.as_mut() {
                    let selected = preview
                        .location
                        .and_then(|index| preview.value.source.locations.get(index));
                    let location = selected.and_then(|selected| {
                        source.locations.iter().position(|location| {
                            location.session == selected.session
                                && location.workspace_id == selected.workspace_id
                                && location.tab_id == selected.tab_id
                                && location.pane_id == selected.pane_id
                                && location.terminal_id == selected.terminal_id
                                && location.agent_session == selected.agent_session
                        })
                    });
                    let scroll = preview.document.scroll;
                    preview.value.source = source;
                    preview.location = location;
                    preview.document = HistoryDocument::from_preview(&preview.value);
                    preview.document.target = None;
                    preview.document.scroll = scroll;
                }
                history.rebuild_results();
            }
            HistoryEvent::Done { truncated } => {
                if search {
                    history.search_request = None;
                    history.truncated |= truncated;
                    history.rebuild_results();
                } else if preview {
                    history.preview_request = None;
                    if let Some(value) = history.preview.as_mut() {
                        value.value.truncated |= truncated;
                    } else if history.preview_error.is_none() {
                        history.preview_error = Some("No transcript was returned".to_owned());
                    }
                }
            }
            HistoryEvent::Error { message } => {
                if search {
                    history.search_request = None;
                    history.error = Some(message);
                    history.rebuild_results();
                } else if preview {
                    history.preview_request = None;
                    history.preview_error = Some(message);
                } else {
                    history.navigation_error = Some(message);
                }
            }
            _ => return false,
        }
        if preview && history.preview.is_none() {
            history.rebuild_preview_feedback();
        }
        true
    }

    pub(crate) fn finish_history_navigation(
        &mut self,
        request_id: u64,
        result: Result<(), String>,
    ) -> bool {
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return false;
        };
        if history.navigation_request != Some(request_id) {
            return false;
        }
        history.navigation_request = None;
        match result {
            Ok(()) => {
                self.overlay = None;
                self.hits.history = HistoryHitMap::default();
            }
            Err(error) => history.navigation_error = Some(error),
        }
        true
    }

    pub(super) fn reconcile_history_machine(&mut self) {
        if matches!(self.overlay.as_ref(), Some(ClientShellOverlay::PiHistory(history)) if history.machine != self.active_endpoint_id.machine_id())
        {
            self.overlay = None;
            self.hits.history = HistoryHitMap::default();
        }
    }

    pub(super) fn prepare_history_layout(&mut self, cols: u16, rows: u16) {
        let Some(ClientShellOverlay::PiHistory(history)) = self.overlay.as_mut() else {
            return;
        };
        let geometry = overlay::history_geometry(Rect::new(0, 0, cols, rows));
        history.document_mut().prepare(geometry.body);
    }
}
