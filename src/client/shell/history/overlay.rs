use super::*;
use crate::client::shell::render::OverlayRender;
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

pub(super) struct HistoryGeometry {
    area: Rect,
    inner: Rect,
    title: Rect,
    context: Rect,
    query: Rect,
    scopes: Rect,
    status: Rect,
    pub(super) body: Rect,
    navigation: Rect,
    actions: Rect,
    footer: Rect,
}

pub(super) fn history_geometry(screen: Rect) -> HistoryGeometry {
    let area = if screen.width < 28 || screen.height < 10 {
        screen
    } else {
        crate::ui::centered_popup_rect(screen, 104, 40).unwrap_or(screen)
    };
    let inner = if area.width >= 12 && area.height >= 6 {
        Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2)
    } else {
        area
    };
    let row = |offset: u16| {
        Rect::new(
            inner.x,
            inner.y + offset.min(inner.height),
            inner.width,
            u16::from(offset < inner.height),
        )
    };
    let full = inner.height >= 9;
    let title = row(0);
    let context = if full { row(1) } else { Rect::default() };
    let query = if full {
        row(2)
    } else if inner.height >= 4 {
        row(1)
    } else {
        Rect::default()
    };
    let scopes = if full { row(3) } else { Rect::default() };
    let status = if full {
        row(4)
    } else if inner.height >= 4 {
        row(2)
    } else if inner.height >= 2 {
        row(1)
    } else {
        Rect::default()
    };
    let header = if full {
        5
    } else if inner.height >= 4 {
        3
    } else {
        2.min(inner.height)
    };
    let footer_rows = if inner.height >= 8 {
        3
    } else {
        u16::from(inner.height >= 3) + u16::from(inner.height >= 6)
    };
    let body = Rect::new(
        inner.x,
        inner.y + header,
        inner.width,
        inner.height.saturating_sub(header + footer_rows),
    );
    HistoryGeometry {
        area,
        inner,
        title,
        context,
        query,
        scopes,
        status,
        body,
        navigation: if inner.height >= 8 {
            row(inner.height - 3)
        } else if inner.height >= 6 {
            row(inner.height - 2)
        } else {
            Rect::default()
        },
        actions: if inner.height >= 8 {
            row(inner.height - 2)
        } else if inner.height >= 3 {
            row(inner.height - 1)
        } else {
            Rect::default()
        },
        footer: if inner.height >= 8 {
            row(inner.height - 1)
        } else {
            Rect::default()
        },
    }
}

struct HistoryButton {
    area: Rect,
    label: String,
    control: HistoryControl,
    enabled: bool,
    selected: bool,
}

fn button_row(
    area: Rect,
    values: impl IntoIterator<Item = (String, HistoryControl, bool, bool)>,
) -> Vec<HistoryButton> {
    let mut x = area.x;
    let mut buttons = Vec::new();
    if area.is_empty() {
        return buttons;
    }
    for (label, control, enabled, selected) in values {
        let padding = if area.width >= 12 { 2 } else { 0 };
        let width = (label
            .width()
            .saturating_add(padding)
            .min(usize::from(u16::MAX)) as u16)
            .min(area.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        buttons.push(HistoryButton {
            area: Rect::new(x, area.y, width, 1),
            label,
            control,
            enabled,
            selected,
        });
        x = x.saturating_add(width).saturating_add(1);
    }
    buttons
}

fn history_buttons(history: &PiHistoryOverlay, geometry: &HistoryGeometry) -> Vec<HistoryButton> {
    let close_width = geometry.title.width.min(11);
    let mut buttons = button_row(
        Rect::new(
            geometry.title.right().saturating_sub(close_width),
            geometry.title.y,
            close_width,
            geometry.title.height,
        ),
        [(
            if close_width >= 11 {
                "Esc close"
            } else if close_width >= 5 {
                "Close"
            } else {
                "×"
            }
            .into(),
            HistoryControl::Close,
            true,
            false,
        )],
    );
    let scopes = if geometry.scopes.is_empty() && !history.preview_open {
        geometry.actions
    } else {
        geometry.scopes
    };
    let scope_labels = if scopes.width >= 58 {
        ["^1 Current space", "^2 Current session", "^3 All sessions"]
    } else if scopes.width >= 36 {
        ["^1 Space", "^2 Session", "^3 All"]
    } else if scopes.width >= 24 {
        ["Space", "Session", "All"]
    } else {
        ["1", "2", "3"]
    };
    let mut scope_controls = vec![
        (
            scope_labels[0].into(),
            HistoryControl::Scope(HistoryScope::Space),
            true,
            history.scope == HistoryScope::Space,
        ),
        (
            scope_labels[1].into(),
            HistoryControl::Scope(HistoryScope::Session),
            true,
            history.scope == HistoryScope::Session,
        ),
        (
            scope_labels[2].into(),
            HistoryControl::Scope(HistoryScope::All),
            true,
            history.scope == HistoryScope::All,
        ),
    ];
    if geometry.scopes.is_empty() && !history.preview_open {
        scope_controls.push((
            "F5 Retry".into(),
            HistoryControl::Retry,
            history.search_request.is_none(),
            false,
        ));
    }
    buttons.extend(button_row(scopes, scope_controls));
    if history.preview_open {
        let previous_hit = history
            .summary
            .first
            .is_some_and(|first| history.selected > first);
        let next_hit = history
            .summary
            .last
            .is_some_and(|last| history.selected < last);
        buttons.extend(button_row(
            geometry.navigation,
            [
                (
                    if geometry.navigation.width < 60 {
                        "↩"
                    } else {
                        "Esc back"
                    }
                    .into(),
                    HistoryControl::Back,
                    true,
                    false,
                ),
                (
                    if geometry.navigation.width < 60 {
                        "["
                    } else {
                        "[ Prev page"
                    }
                    .into(),
                    HistoryControl::PreviousPage,
                    history
                        .preview
                        .as_ref()
                        .is_some_and(|preview| preview.value.previous.is_some()),
                    false,
                ),
                (
                    if geometry.navigation.width < 60 {
                        "]"
                    } else {
                        "] Next page"
                    }
                    .into(),
                    HistoryControl::NextPage,
                    history
                        .preview
                        .as_ref()
                        .is_some_and(|preview| preview.value.next.is_some()),
                    false,
                ),
                (
                    if geometry.navigation.width < 60 {
                        "N"
                    } else {
                        "N Prev hit"
                    }
                    .into(),
                    HistoryControl::PreviousHit,
                    previous_hit,
                    false,
                ),
                (
                    if geometry.navigation.width < 60 {
                        "n"
                    } else {
                        "n Next hit"
                    }
                    .into(),
                    HistoryControl::NextHit,
                    next_hit,
                    false,
                ),
            ],
        ));
        let location = history
            .preview
            .as_ref()
            .and_then(|preview| preview.location);
        let locations = history
            .preview
            .as_ref()
            .map_or(0, |preview| preview.value.source.locations.len());
        let ready = history.navigation_request.is_none() && history.preview_request.is_none();
        if geometry.actions.width < 80 {
            let mut actions = Vec::new();
            if let Some(location) = history.selected_location() {
                let (label, control) = if location.running {
                    (
                        if geometry.actions.width >= 24 {
                            "^G Focus"
                        } else if geometry.actions.width >= 8 {
                            "Focus"
                        } else if geometry.actions.width >= 2 {
                            "^G"
                        } else {
                            "G"
                        },
                        HistoryControl::FocusRunning,
                    )
                } else {
                    (
                        if geometry.actions.width >= 24 {
                            "^O Open"
                        } else if geometry.actions.width >= 8 {
                            "Open"
                        } else if geometry.actions.width >= 2 {
                            "^O"
                        } else {
                            "O"
                        },
                        HistoryControl::OpenSession,
                    )
                };
                actions.push((label.into(), control, ready, false));
            }
            actions.extend([
                (
                    "‹".into(),
                    HistoryControl::PreviousLocation,
                    location.is_some_and(|index| index > 0) && ready,
                    false,
                ),
                (
                    "›".into(),
                    HistoryControl::NextLocation,
                    location.map_or(locations > 0, |index| index + 1 < locations) && ready,
                    false,
                ),
                ("F5".into(), HistoryControl::Retry, ready, false),
            ]);
            buttons.extend(button_row(geometry.actions, actions));
        } else {
            buttons.extend(button_row(
                geometry.actions,
                [
                    (
                        "S-Tab ‹".into(),
                        HistoryControl::PreviousLocation,
                        location.is_some_and(|index| index > 0) && ready,
                        false,
                    ),
                    (
                        format!(
                            "Tab › location {}/{}",
                            location.map_or(0, |index| index + 1),
                            locations
                        ),
                        HistoryControl::NextLocation,
                        location.map_or(locations > 0, |index| index + 1 < locations) && ready,
                        false,
                    ),
                    (
                        "^G Focus running".into(),
                        HistoryControl::FocusRunning,
                        ready
                            && history
                                .selected_location()
                                .is_some_and(|location| location.running),
                        false,
                    ),
                    (
                        "^O Open session".into(),
                        HistoryControl::OpenSession,
                        ready
                            && history
                                .selected_location()
                                .is_some_and(|location| !location.running),
                        false,
                    ),
                ],
            ));
        }
    } else if !geometry.scopes.is_empty() {
        buttons.extend(button_row(
            geometry.actions,
            [(
                "F5 Retry".into(),
                HistoryControl::Retry,
                history.search_request.is_none(),
                false,
            )],
        ));
    }
    buttons
}

fn status(history: &PiHistoryOverlay) -> String {
    let mut parts = Vec::new();
    if let Some(error) = &history.navigation_error {
        parts.push(error.clone());
    } else if let Some(error) = &history.preview_error {
        parts.push(error.clone());
    } else if let Some(error) = &history.error {
        parts.push(error.clone());
    }
    if history.navigation_request.is_some() {
        parts.push("Resolving selected location…".to_owned());
    } else if history.preview_open && history.preview_request.is_some() {
        parts.push("Loading read-only transcript…".to_owned());
    }
    if history.truncated
        || history.summary.truncated
        || history
            .preview
            .as_ref()
            .is_some_and(|preview| preview.value.truncated)
    {
        parts.push("truncated · partial saved content".to_owned());
    }
    if !history.unavailable.is_empty() {
        parts.push(format!(
            "{} unavailable sources (details in results)",
            history.unavailable.len()
        ));
    }
    if let Some((scanned, total)) = history.progress {
        parts.push(format!("{scanned}/{total} sources scanned"));
    } else if history.search_request.is_some() {
        parts.push("Searching…".to_owned());
    }
    parts.push(format!(
        "{} histories · {} hits",
        history.results.len(),
        history.summary.hits
    ));
    parts.join(" · ")
}

pub(super) fn history_hit_map(history: &PiHistoryOverlay, screen: Rect) -> HistoryHitMap {
    let geometry = history_geometry(screen);
    let controls = history_buttons(history, &geometry)
        .into_iter()
        .filter(|button| button.enabled)
        .map(|button| (button.area, button.control))
        .collect();
    let mut hits = HistoryHitMap {
        area: geometry.area,
        query: geometry.query,
        body: geometry.body,
        controls,
        ..HistoryHitMap::default()
    };
    if let Some(document) = history.document() {
        document.fill_hits(geometry.body, &mut hits);
    }
    hits
}

pub(in crate::client::shell) fn render_history_overlay(
    buffer: &mut Buffer,
    history: &PiHistoryOverlay,
    palette: &Palette,
) -> Option<OverlayRender> {
    let geometry = history_geometry(buffer.area);
    if geometry.area.is_empty() {
        return None;
    }
    let base = Style::default()
        .fg(palette.text)
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::DIM);
    for y in buffer.area.y..buffer.area.bottom() {
        for x in buffer.area.x..buffer.area.right() {
            let cell = &mut buffer[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::DIM));
        }
    }
    ratatui::widgets::Clear.render(geometry.area, buffer);
    buffer.set_style(geometry.area, base);
    if geometry.inner != geometry.area {
        Block::default()
            .borders(Borders::ALL)
            .border_style(base.fg(palette.accent))
            .render(geometry.area, buffer);
    }
    let put = |buffer: &mut Buffer, area: Rect, text: &str, style: Style| {
        if !area.is_empty() {
            if text.chars().any(char::is_control) {
                let text: String = text
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect();
                render::put_text(buffer, area.x, area.y, area.width, &text, style);
            } else {
                render::put_text(buffer, area.x, area.y, area.width, text, style);
            }
        }
    };
    let title = Rect::new(
        geometry.title.x,
        geometry.title.y,
        geometry.title.width.saturating_sub(12),
        geometry.title.height,
    );
    put(
        buffer,
        title,
        &format!("Pi history · {} · read only", history.machine_label),
        base.add_modifier(Modifier::BOLD),
    );
    put(
        buffer,
        geometry.context,
        &format!(
            "{} / {} [{}]",
            history.session,
            history.workspace_label,
            history.workspace_id.as_deref().unwrap_or("unavailable")
        ),
        base.fg(palette.overlay1),
    );
    let mut cursor = None;
    if !geometry.query.is_empty() {
        buffer.set_style(geometry.query, base.bg(palette.surface0));
        let label_width = if geometry.query.width >= 12 {
            8
        } else if geometry.query.width >= 5 {
            2
        } else {
            0
        };
        put(
            buffer,
            Rect::new(geometry.query.x, geometry.query.y, label_width, 1),
            if label_width == 8 { "Search: " } else { "Q " },
            base.fg(palette.overlay1).bg(palette.surface0),
        );
        let query = Rect::new(
            geometry.query.x + label_width,
            geometry.query.y,
            geometry.query.width - label_width,
            1,
        );
        cursor = text_editor::render(buffer, query, &history.query, base.bg(palette.surface0))
            .filter(|_| !history.preview_open && history.query_focused);
    }
    let warning = history.error.is_some()
        || history.preview_error.is_some()
        || history.navigation_error.is_some()
        || history.truncated
        || history.summary.truncated
        || history
            .preview
            .as_ref()
            .is_some_and(|preview| preview.value.truncated)
        || !history.unavailable.is_empty();
    put(
        buffer,
        geometry.status,
        &status(history),
        base.fg(if warning {
            palette.yellow
        } else {
            palette.overlay1
        }),
    );
    let hits = history_hit_map(history, buffer.area);
    if let Some(document) = history.document() {
        document.render(
            buffer,
            geometry.body,
            (!history.preview_open).then_some(history.selected),
            palette,
        );
    } else if !geometry.body.is_empty() {
        Paragraph::new(
            history
                .preview_error
                .as_deref()
                .unwrap_or("Loading read-only transcript…"),
        )
        .style(base)
        .wrap(Wrap { trim: false })
        .render(geometry.body, buffer);
    }
    if history.preview_open {
        let footer = history
            .selected_location()
            .map(location_text)
            .unwrap_or_else(|| {
                "No selected location · Tab choose a location · F5 retry · Esc back".to_owned()
            });
        put(buffer, geometry.footer, &footer, base.fg(palette.overlay1));
    } else {
        put(buffer, geometry.footer, "Enter / click preview · ↑↓ select · wheel / PgUp/PgDn scroll · Tab query/results · Esc close", base.fg(palette.overlay1));
    }
    for button in history_buttons(history, &geometry) {
        let style = if button.selected {
            base.fg(palette.panel_bg)
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else if button.enabled {
            base.fg(palette.accent).bg(palette.surface0)
        } else {
            base.fg(palette.overlay0)
        };
        buffer.set_style(button.area, style);
        let label = if usize::from(button.area.width) > button.label.width() {
            format!(" {}", button.label)
        } else {
            button.label
        };
        put(buffer, button.area, &label, style);
    }
    Some(OverlayRender {
        area: geometry.area,
        history: hits,
        cursor,
        ..OverlayRender::default()
    })
}
