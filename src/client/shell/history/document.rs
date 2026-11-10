use super::*;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget, Wrap};
use unicode_segmentation::UnicodeSegmentation;

const MAX_CHUNK_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryLineKind {
    Plain,
    Heading,
    Warning,
    Matched,
}

#[derive(Debug)]
pub(super) struct HistoryLine {
    text: String,
    kind: HistoryLineKind,
    pub(super) hit: Option<HistorySelection>,
}

impl HistoryLine {
    pub(super) fn plain(text: String) -> Self {
        Self {
            text,
            kind: HistoryLineKind::Plain,
            hit: None,
        }
    }

    pub(super) fn heading(text: String) -> Self {
        Self {
            text,
            kind: HistoryLineKind::Heading,
            hit: None,
        }
    }

    pub(super) fn warning(text: String) -> Self {
        Self {
            text,
            kind: HistoryLineKind::Warning,
            hit: None,
        }
    }

    pub(super) fn hit(text: String, hit: HistorySelection) -> Self {
        Self {
            text,
            kind: HistoryLineKind::Plain,
            hit: Some(hit),
        }
    }

    fn as_line(&self) -> Line<'_> {
        Line::from(self.text.as_str())
    }
}

#[derive(Debug)]
pub(super) enum ScrollTarget {
    Top(usize),
    Reveal(usize),
}

#[derive(Debug)]
struct DocumentLayout {
    size: (u16, u16),
    width: u16,
    offsets: Vec<usize>,
    total: usize,
    scrollbar: bool,
}

#[derive(Debug, Default)]
pub(super) struct HistoryDocument {
    pub(super) lines: Vec<HistoryLine>,
    pub(super) scroll: usize,
    pub(super) target: Option<ScrollTarget>,
    layout: Option<DocumentLayout>,
}

impl HistoryDocument {
    pub(super) fn replace(&mut self, lines: Vec<HistoryLine>) {
        self.lines.clear();
        for line in lines {
            self.push_line(line);
        }
        self.layout = None;
    }

    fn push_line(&mut self, line: HistoryLine) {
        let sanitized: String = line
            .text
            .chars()
            .filter_map(|character| {
                if character == '\t' {
                    Some(' ')
                } else if !character.is_control() {
                    Some(character)
                } else {
                    None
                }
            })
            .collect();
        let mut text = String::new();
        let mut columns = 0;
        for grapheme in sanitized.graphemes(true) {
            let grapheme = if grapheme.len() > MAX_CHUNK_SIZE {
                "[oversized grapheme omitted]"
            } else {
                grapheme
            };
            let width = grapheme.width();
            if (columns + width > MAX_CHUNK_SIZE || text.len() + grapheme.len() > MAX_CHUNK_SIZE)
                && !text.is_empty()
            {
                self.lines.push(HistoryLine {
                    text: std::mem::take(&mut text),
                    kind: line.kind,
                    hit: line.hit,
                });
                columns = 0;
            }
            text.push_str(grapheme);
            columns += width;
        }
        self.lines.push(HistoryLine {
            text,
            kind: line.kind,
            hit: line.hit,
        });
    }

    pub(super) fn from_preview(preview: &HistoryPreview) -> Self {
        let mut document = Self::default();
        document.push_line(HistoryLine::heading(preview.source.name.clone()));
        for location in &preview.source.locations {
            document.push_line(HistoryLine::plain(location_text(location)));
        }
        document.push_line(HistoryLine::plain(String::new()));
        let mut anchor = 0;
        for (index, entry) in preview.entries.iter().enumerate() {
            if index == preview.matched_entry {
                anchor = document.lines.len();
            }
            let matched = index == preview.matched_entry;
            let kind = if matched {
                HistoryLineKind::Matched
            } else {
                HistoryLineKind::Plain
            };
            document.push_line(HistoryLine {
                text: format!(
                    "{} · entry {}{}{}",
                    entry.role,
                    entry.entry_line,
                    if matched { " · match" } else { "" },
                    if entry.truncated {
                        " · entry fragment"
                    } else {
                        ""
                    }
                ),
                kind: if matched {
                    kind
                } else {
                    HistoryLineKind::Heading
                },
                hit: None,
            });
            for text in entry.text.split('\n') {
                document.push_line(HistoryLine {
                    text: text.to_owned(),
                    kind,
                    hit: None,
                });
            }
            document.push_line(HistoryLine::plain(String::new()));
        }
        if preview.truncated {
            document.push_line(HistoryLine::warning(
                "Bounded transcript window · some saved content is omitted".to_owned(),
            ));
        }
        document.target = Some(ScrollTarget::Top(anchor));
        document
    }

    fn measure(&self, width: u16) -> Vec<usize> {
        self.lines
            .iter()
            .map(|line| {
                Paragraph::new(line.as_line())
                    .wrap(Wrap { trim: false })
                    .line_count(width.max(1))
                    .max(1)
            })
            .collect()
    }

    pub(super) fn prepare(&mut self, body: Rect) {
        let size = (body.width, body.height);
        if self
            .layout
            .as_ref()
            .is_none_or(|layout| layout.size != size)
        {
            let mut width = body.width.max(1);
            let mut heights = self.measure(width);
            let mut total: usize = heights.iter().sum();
            let scrollbar = total > usize::from(body.height) && body.width > 1;
            if scrollbar {
                width = width.saturating_sub(1).max(1);
                heights = self.measure(width);
                total = heights.iter().sum();
            }
            let mut offsets = Vec::with_capacity(heights.len() + 1);
            offsets.push(0);
            for height in heights {
                offsets.push(offsets.last().copied().unwrap_or(0) + height);
            }
            self.layout = Some(DocumentLayout {
                size,
                width,
                offsets,
                total,
                scrollbar,
            });
        }
        if let Some(layout) = &self.layout {
            if let Some(target) = self.target.take() {
                let (line, top) = match target {
                    ScrollTarget::Top(line) => (line, true),
                    ScrollTarget::Reveal(line) => (line, false),
                };
                let start = layout.offsets.get(line).copied().unwrap_or(layout.total);
                let end = layout.offsets.get(line + 1).copied().unwrap_or(start + 1);
                let height = usize::from(body.height).max(1);
                if top || start < self.scroll || end.saturating_sub(start) > height {
                    self.scroll = start;
                } else if end > self.scroll.saturating_add(height) {
                    self.scroll = end.saturating_sub(height);
                }
            }
            self.scroll = self
                .scroll
                .min(layout.total.saturating_sub(usize::from(body.height).max(1)));
        }
    }

    pub(super) fn metrics(&self, body: Rect) -> crate::pane::ScrollMetrics {
        let viewport_rows = usize::from(body.height).max(1);
        let max_offset_from_bottom = self
            .layout
            .as_ref()
            .map_or(0, |layout| layout.total.saturating_sub(viewport_rows));
        crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom.saturating_sub(self.scroll),
            max_offset_from_bottom,
            viewport_rows,
        }
    }

    pub(super) fn render_window(&self, height: u16) -> std::ops::Range<usize> {
        let Some(layout) = self.layout.as_ref() else {
            return 0..0;
        };
        let start = layout
            .offsets
            .partition_point(|offset| *offset <= self.scroll)
            .saturating_sub(1)
            .min(self.lines.len());
        let end = layout
            .offsets
            .partition_point(|offset| *offset < self.scroll.saturating_add(usize::from(height)))
            .min(self.lines.len());
        start..end.max(start)
    }

    pub(super) fn fill_hits(&self, body: Rect, hits: &mut HistoryHitMap) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        for index in self.render_window(body.height) {
            let line = &self.lines[index];
            let top = layout.offsets[index]
                .saturating_sub(self.scroll)
                .min(usize::from(body.height));
            let bottom = layout.offsets[index + 1]
                .saturating_sub(self.scroll)
                .min(usize::from(body.height));
            if let Some(hit) = line.hit.filter(|_| bottom > top) {
                hits.rows.push((
                    Rect::new(
                        body.x,
                        body.y + top as u16,
                        layout.width.min(body.width),
                        (bottom - top) as u16,
                    ),
                    hit,
                ));
            }
        }
        hits.metrics = Some(self.metrics(body));
        if layout.scrollbar && !body.is_empty() {
            hits.scrollbar = Rect::new(body.right() - 1, body.y, 1, body.height);
        }
    }

    pub(super) fn render(
        &self,
        buffer: &mut Buffer,
        body: Rect,
        selected: Option<HistorySelection>,
        palette: &Palette,
    ) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        if body.is_empty() {
            return;
        }
        let text_area = Rect::new(body.x, body.y, layout.width.min(body.width), body.height);
        let window = self.render_window(body.height);
        let within_line = self.scroll.saturating_sub(layout.offsets[window.start]);
        let mut lines = Vec::with_capacity(window.len());
        for line in &self.lines[window] {
            let is_selected = line.hit.is_some() && line.hit == selected;
            let style = Style::default()
                .fg(palette.text)
                .bg(palette.panel_bg)
                .remove_modifier(Modifier::DIM);
            let style = if is_selected {
                style
                    .fg(palette.accent)
                    .bg(palette.surface0)
                    .add_modifier(Modifier::BOLD)
            } else {
                match line.kind {
                    HistoryLineKind::Heading => {
                        style.fg(palette.accent).add_modifier(Modifier::BOLD)
                    }
                    HistoryLineKind::Warning => style.fg(palette.yellow),
                    HistoryLineKind::Matched => style.fg(palette.accent).bg(palette.surface0),
                    HistoryLineKind::Plain => style,
                }
            };
            lines.push(line.as_line().style(style));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(within_line).unwrap_or(u16::MAX), 0))
            .render(text_area, buffer);
        let metrics = self.metrics(body);
        if layout.scrollbar {
            let track = Rect::new(body.right() - 1, body.y, 1, body.height);
            crate::ui::render_scrollbar_buffer(
                buffer,
                metrics,
                track,
                palette.overlay0,
                palette.overlay1,
                "▐",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_hit_testing_only_visit_the_visible_window_at_large_offsets() {
        for count in [64, 15_000] {
            let mut document = HistoryDocument::default();
            document.replace(
                (0..count)
                    .map(|index| {
                        HistoryLine::hit(
                            format!("row {index} 界 e\u{301}"),
                            HistorySelection {
                                result: index,
                                hit: 0,
                            },
                        )
                    })
                    .collect(),
            );
            let body = Rect::new(0, 0, 12, 8);
            document.prepare(body);
            document.scroll = usize::MAX;
            document.prepare(body);
            let window = document.render_window(body.height);
            assert!(window.start >= count - usize::from(body.height));
            assert!(window.len() <= usize::from(body.height));
            let mut hits = HistoryHitMap::default();
            document.fill_hits(body, &mut hits);
            assert_eq!(hits.rows.len(), window.len());
            assert!(hits.rows.iter().all(|(rect, selection)| {
                window.contains(&selection.result) && rect.bottom() <= body.bottom()
            }));
            let mut buffer = Buffer::empty(body);
            document.render(&mut buffer, body, None, &Palette::catppuccin());
            assert!(buffer.content.iter().any(|cell| cell.symbol() == "界"));
        }
    }

    #[test]
    fn zero_width_and_oversized_graphemes_cannot_create_unbounded_render_chunks() {
        let mut document = HistoryDocument::default();
        document.replace(vec![HistoryLine::plain(format!(
            "{} e{} 界🙂",
            "\u{200b}".repeat(40_000),
            "\u{301}".repeat(20_000)
        ))]);
        assert!(document
            .lines
            .iter()
            .all(|line| line.text.len() <= MAX_CHUNK_SIZE));
        assert!(document
            .lines
            .iter()
            .any(|line| line.text.contains("[oversized grapheme omitted]")));
        assert!(document.lines.iter().any(|line| line.text.contains("界🙂")));
        let body = Rect::new(0, 0, 8, 4);
        document.prepare(body);
        document.scroll = usize::MAX;
        document.prepare(body);
        assert!(document.render_window(body.height).len() <= usize::from(body.height));
        document.render(&mut Buffer::empty(body), body, None, &Palette::catppuccin());
    }
}
