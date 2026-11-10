use super::tabs::{
    render_tab_bar_status, tab_bar_content_area, tab_label, tab_width, MIN_TAB_STRIP_WIDTH,
    TAB_SCROLL_BUTTON_WIDTH,
};
use super::*;

#[derive(Clone, Copy)]
struct Placement {
    row: usize,
    column: u16,
    width: u16,
}

fn pack_tabs(
    widths: impl Iterator<Item = u16>,
    width: u16,
    mut place: impl FnMut(Placement),
) -> usize {
    if width == 0 {
        return 1;
    }
    let mut row = 0usize;
    let mut column = 0u32;
    for desired in widths {
        let item_width = desired.min(width);
        if column > 0 && column + u32::from(item_width) > u32::from(width) {
            row = row.saturating_add(1);
            column = 0;
        }
        place(Placement {
            row,
            column: column as u16,
            width: item_width,
        });
        column += u32::from(item_width) + 1;
    }
    row.saturating_add(1)
}

pub(crate) fn wrapped_tab_bar_height(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    width: u16,
    max_height: u16,
) -> u16 {
    let content = tab_bar_content_area(snapshot, Rect::new(0, 0, width, 1));
    let width = content.width.saturating_sub(if config.mouse_capture {
        NEW_TAB_WIDTH
    } else {
        0
    });
    let rows = pack_tabs(
        snapshot
            .tabs
            .iter()
            .filter(|tab| {
                Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref()
            })
            .map(tab_width),
        width,
        |_| {},
    );
    rows.min(usize::from(max_height.max(1))) as u16
}

pub(super) fn render_wrapped_tab_bar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    tab_scroll: &mut usize,
    reveal_focused_tab: &mut bool,
    hits: &mut ShellHitMap,
) {
    if area.is_empty() {
        return;
    }
    let palette = &config.palette;
    buffer.set_style(area, Style::default().bg(palette.panel_bg));
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref())
        .collect::<Vec<_>>();
    let content = tab_bar_content_area(snapshot, area);
    let new_width = if config.mouse_capture {
        NEW_TAB_WIDTH.min(content.width)
    } else {
        0
    };
    let mut placements = Vec::with_capacity(tabs.len());
    let mut width = content.width.saturating_sub(new_width);
    let mut rows = pack_tabs(tabs.iter().map(|tab| tab_width(tab)), width, |placement| {
        placements.push(placement)
    });
    let controls = rows > usize::from(area.height)
        && config.mouse_capture
        && content.width >= MIN_TAB_STRIP_WIDTH;
    let left = if controls { TAB_SCROLL_BUTTON_WIDTH } else { 0 };
    if controls {
        width = width.saturating_sub(TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2));
        placements.clear();
        rows = pack_tabs(tabs.iter().map(|tab| tab_width(tab)), width, |placement| {
            placements.push(placement)
        });
    }
    let body = Rect::new(content.x.saturating_add(left), area.y, width, area.height);
    let max_scroll = rows.saturating_sub(usize::from(area.height));
    *tab_scroll = (*tab_scroll).min(max_scroll);
    if *reveal_focused_tab {
        if let Some(focused) = tabs
            .iter()
            .position(|tab| tab.focused)
            .and_then(|index| placements.get(index))
        {
            if focused.row < *tab_scroll {
                *tab_scroll = focused.row;
            } else if focused.row >= tab_scroll.saturating_add(usize::from(area.height)) {
                *tab_scroll = focused
                    .row
                    .saturating_add(1)
                    .saturating_sub(usize::from(area.height))
                    .min(max_scroll);
            }
        }
    }
    *reveal_focused_tab = false;
    hits.tab_body = body;
    hits.tab_max_scroll = max_scroll;

    for (index, placement) in placements.iter().enumerate() {
        let Some(rect) = visible_rect(*placement, body, *tab_scroll) else {
            continue;
        };
        let tab = tabs[index];
        let name = tab_label(tab);
        let style = if tab.focused {
            let style = Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent);
            if tab.custom_label {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        } else if tab.custom_label {
            Style::default().fg(palette.overlay1).bg(palette.surface0)
        } else {
            Style::default().fg(palette.overlay0).bg(palette.surface0)
        };
        let padding = rect.width.saturating_sub(display_width(&name));
        let left_padding = padding / 2;
        let text = format!(
            "{empty:left$}{name}{empty:right$}",
            empty = "",
            left = usize::from(left_padding),
            right = usize::from(padding.saturating_sub(left_padding))
        );
        put_text(buffer, rect.x, rect.y, rect.width, &text, style);
        hits.tabs.push((rect, tab.tab_id.clone()));
    }
    if controls {
        hits.tab_scroll_left = Rect::new(content.x, area.y, TAB_SCROLL_BUTTON_WIDTH, 1);
        hits.tab_scroll_right = Rect::new(body.right(), area.y, TAB_SCROLL_BUTTON_WIDTH, 1);
        for (rect, text, enabled) in [
            (hits.tab_scroll_left, " ↑ ", *tab_scroll > 0),
            (hits.tab_scroll_right, " ↓ ", *tab_scroll < max_scroll),
        ] {
            put_text(
                buffer,
                rect.x,
                rect.y,
                rect.width,
                text,
                Style::default()
                    .fg(if enabled {
                        palette.overlay1
                    } else {
                        palette.overlay0
                    })
                    .bg(palette.surface0),
            );
        }
    }
    if config.mouse_capture {
        hits.new_tab = Rect::new(
            content.right().saturating_sub(new_width),
            area.y,
            new_width,
            1,
        );
        put_text(
            buffer,
            hits.new_tab.x,
            hits.new_tab.y,
            hits.new_tab.width,
            " + ",
            Style::default().fg(palette.overlay1).bg(palette.panel_bg),
        );
    }
    render_tab_bar_status(buffer, area, snapshot, palette);
}

fn visible_rect(placement: Placement, body: Rect, scroll: usize) -> Option<Rect> {
    let row = placement.row.checked_sub(scroll)?;
    (row < usize::from(body.height) && placement.width > 0).then(|| {
        Rect::new(
            body.x.saturating_add(placement.column),
            body.y.saturating_add(row as u16),
            placement.width,
            1,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_packing_keeps_tabs_whole_and_bounds_overwide_titles() {
        let mut positions = Vec::new();
        assert_eq!(
            pack_tabs([8, 8, 8, 40, 8].into_iter(), 17, |p| positions
                .push((p.row, p.column, p.width))),
            4
        );
        assert_eq!(
            positions,
            vec![(0, 0, 8), (0, 9, 8), (1, 0, 8), (2, 0, 17), (3, 0, 8)]
        );
        assert_eq!(
            pack_tabs([u16::MAX, u16::MAX].into_iter(), u16::MAX, |_| {}),
            2
        );
        assert_eq!(
            pack_tabs([8, 8].into_iter(), 0, |_| panic!("zero width has no hits")),
            1
        );
    }
}
