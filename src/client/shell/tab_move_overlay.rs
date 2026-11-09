use super::*;
use crate::client::shell::tab_move::{TabMovePhase, TabMovePicker};

pub(in crate::client::shell) fn render_tab_move_overlay(
    buffer: &mut Buffer,
    picker: &TabMovePicker,
    palette: &Palette,
) -> Option<OverlayRender> {
    let height = (picker.entries.len().min(12) as u16 + 9).max(11);
    let area = popup(buffer.area, 68, height).unwrap_or(buffer.area);
    if area.is_empty() {
        return None;
    }
    let inner = if area.width >= 4 && area.height >= 4 {
        panel(buffer, area, palette.accent, palette.panel_bg)?
    } else {
        buffer.set_style(
            area,
            Style::default()
                .fg(palette.text)
                .bg(palette.panel_bg)
                .remove_modifier(Modifier::DIM),
        );
        area
    };
    let mut rendered = OverlayRender {
        area,
        navigator_popup: area,
        ..OverlayRender::default()
    };
    let text = Style::default().fg(palette.text).bg(palette.panel_bg);
    let heading_rows = if inner.height >= 7 { 2 } else { 0 };
    if heading_rows > 0 {
        put_text(
            buffer,
            inner.x,
            inner.y,
            inner.width,
            "Move to space",
            text.add_modifier(Modifier::BOLD),
        );
        put_text(
            buffer,
            inner.x,
            inner.y + 1,
            inner.width,
            &format!(
                "session {} · from {}",
                picker.source.session, picker.source.workspace_id
            ),
            Style::default().fg(palette.overlay0).bg(palette.panel_bg),
        );
    }
    let outcome_visible =
        picker.busy() || picker.phase == TabMovePhase::Finished || picker.error.is_some();
    let footer_rows = u16::from(inner.height >= if outcome_visible { 3 } else { 2 });
    let minimum_body_rows = u16::from(!outcome_visible && !picker.entries.is_empty());
    let message_rows = 3.min(
        inner
            .height
            .saturating_sub(heading_rows + footer_rows + minimum_body_rows),
    );
    let body = Rect::new(
        inner.x,
        inner.y + heading_rows,
        inner.width,
        inner
            .height
            .saturating_sub(heading_rows + message_rows + footer_rows),
    );
    let selected = picker
        .entries
        .iter()
        .position(|entry| Some(&entry.workspace_id) == picker.selected.as_ref());
    let visible = usize::from(body.height);
    let start = selected
        .unwrap_or(0)
        .saturating_sub(visible.saturating_sub(1));
    for (index, entry) in picker.entries.iter().enumerate().skip(start).take(visible) {
        let rect = Rect::new(body.x, body.y + (index - start) as u16, body.width, 1);
        let style = if Some(index) == selected {
            Style::default()
                .fg(contrast(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            text
        };
        buffer.set_style(rect, style);
        let id_width = display_width(&entry.workspace_id).min(rect.width / 2);
        let label_width = rect
            .width
            .saturating_sub(id_width + u16::from(id_width > 0));
        put_text(
            buffer,
            rect.x,
            rect.y,
            label_width,
            &crate::ui::truncate_end(&entry.label, usize::from(label_width)),
            style,
        );
        put_right_text(
            buffer,
            rect,
            rect.y,
            &crate::ui::truncate_end(&entry.workspace_id, usize::from(id_width)),
            style,
        );
        if picker.phase == TabMovePhase::Selecting {
            rendered
                .tab_move_rows
                .push((rect, entry.workspace_id.clone()));
        }
    }
    if message_rows > 0 {
        let message = picker.error.as_deref().unwrap_or(match picker.phase {
            TabMovePhase::Moving => "Moving tab… this cannot be cancelled after submission.",
            TabMovePhase::Following => "Tab moved · opening destination…",
            TabMovePhase::Finished => "Close this dialog to inspect the tab's current location.",
            TabMovePhase::Selecting if picker.entries.is_empty() => {
                "No other spaces in this session. Create another space, then reopen Move to space."
            }
            TabMovePhase::Selecting => "↑↓ select · ↵ move · esc cancel",
        });
        let compact = (outcome_visible && inner.height < 7).then(|| {
            let summary = if picker.phase == TabMovePhase::Moving {
                "Moving..."
            } else if inner.width < 8 {
                picker.outcome.symbol()
            } else {
                picker.outcome.label()
            };
            if message_rows < 3 {
                summary.to_owned()
            } else {
                format!("{summary}\n{message}")
            }
        });
        let message = compact.as_deref().unwrap_or(message);
        let message_area = Rect::new(
            inner.x,
            inner.bottom().saturating_sub(footer_rows + message_rows),
            inner.width,
            message_rows,
        );
        ratatui::widgets::Widget::render(
            ratatui::widgets::Paragraph::new(message)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .style(
                    Style::default()
                        .fg(if picker.error.is_some() {
                            palette.red
                        } else {
                            palette.overlay0
                        })
                        .bg(palette.panel_bg),
                ),
            message_area,
            buffer,
        );
    }
    if footer_rows > 0 {
        let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        let normal = Style::default().fg(palette.text).bg(palette.surface0);
        if picker.busy() {
            button(buffer, footer, "move submitted · cannot cancel", normal);
        } else if picker.phase == TabMovePhase::Finished || selected.is_none() {
            rendered.cancel = footer;
            button(buffer, footer, "esc close", normal);
        } else {
            let gap = u16::from(inner.width >= 8);
            let width = inner.width.saturating_sub(gap);
            let buttons = row(
                inner,
                &[width / 2, width - width / 2],
                gap,
                inner.height - 1,
            );
            rendered.primary = buttons[0];
            rendered.cancel = buttons[1];
            button(
                buffer,
                rendered.primary,
                "↵ move",
                Style::default()
                    .fg(contrast(palette))
                    .bg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            );
            button(buffer, rendered.cancel, "esc cancel", normal);
        }
    }
    Some(rendered)
}
