// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    BLOCK_PAINT_LIMIT, Found, InlineStyle, SourceLine, read_inline_text, reader_columns,
    table_cell_ranges,
};
use aterm_grapheme::GraphemeClusters;
use std::ops::Range;

type Spans = Vec<(Range<usize>, InlineStyle)>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Cell {
    text: String,
    spans: Spans,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownTable {
    alignment: Vec<(bool, bool)>,
    widths: Vec<usize>,
    rows: Vec<Vec<Cell>>,
}

impl MarkdownTable {
    pub(super) fn read(lines: &[SourceLine<'_>], found: &mut Found) -> Self {
        let separator = lines[1].text;
        let alignment: Vec<_> = table_cell_ranges(separator)
            .map(|range| {
                let marker = &separator[range];
                (marker.starts_with(':'), marker.ends_with(':'))
            })
            .collect();
        let mut widths = vec![1; alignment.len()];
        let rows = std::iter::once(&lines[0])
            .chain(&lines[2..])
            .map(|line| {
                table_cell_ranges(line.text)
                    .take(widths.len())
                    .enumerate()
                    .map(|(i, cell)| {
                        let (text, spans) = read_inline_text(
                            &line.text[cell.clone()],
                            line.start + cell.start,
                            Some(found),
                        );
                        widths[i] = widths[i].max(text.chars().map(reader_columns).sum());
                        Cell { text, spans }
                    })
                    .collect()
            })
            .collect();
        Self {
            alignment,
            widths,
            rows,
        }
    }

    /// Distribute the available columns among cells, letting short columns release
    /// their unused width to longer ones. Each continuation keeps the same dividers.
    pub fn project(&self, columns: usize) -> (String, Spans) {
        let divider = if columns / self.widths.len() >= 9 {
            "  │  "
        } else {
            "│"
        };
        let available = columns.saturating_sub(divider.chars().count() * (self.widths.len() - 1));
        let mut low = 1;
        let mut high = *self.widths.iter().max().unwrap_or(&1);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if self.widths.iter().map(|&w| w.min(mid)).sum::<usize>() <= available {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        let widths: Vec<_> = self.widths.iter().map(|&w| w.min(low)).collect();
        let mut text = String::new();
        let mut styles = Vec::new();
        for (r, row) in self.rows.iter().enumerate() {
            let mut offsets = vec![0; row.len()];
            loop {
                for (i, &width) in widths.iter().enumerate() {
                    if i > 0 {
                        text.push_str(divider);
                    }
                    let (value, spans, from) = match row.get(i) {
                        Some(cell) => (cell.text.as_str(), cell.spans.as_slice(), offsets[i]),
                        None => ("", &[][..], 0),
                    };
                    let mut used = 0;
                    let mut end = from;
                    let mut last_space = None;
                    for cluster in value[from..].graphemes() {
                        let advance = cluster.chars().map(reader_columns).sum::<usize>();
                        if end > from && used + advance > width {
                            break;
                        }
                        if end > from && cluster == " " {
                            last_space = Some((end, used));
                        }
                        used += advance;
                        end += cluster.len();
                        if end - from >= BLOCK_PAINT_LIMIT {
                            break;
                        }
                    }
                    if end < value.len()
                        && !value[end..].starts_with(' ')
                        && let Some((word_end, columns)) = last_space
                    {
                        end = word_end;
                        used = columns;
                    }
                    let padding = width.saturating_sub(used);
                    let left = match self.alignment[i] {
                        (true, true) => padding / 2,
                        (false, true) => padding,
                        _ => 0,
                    };
                    text.extend(std::iter::repeat_n(
                        ' ',
                        left.min(BLOCK_PAINT_LIMIT.saturating_sub(text.len())),
                    ));
                    let at = text.len();
                    text.push_str(&value[from..end]);
                    if let Some(offset) = offsets.get_mut(i) {
                        *offset = end + value[end..].len() - value[end..].trim_start().len();
                    }
                    let mut cursor = from;
                    let header = InlineStyle {
                        strong: true,
                        ..Default::default()
                    };
                    for (range, mut style) in spans
                        [spans.partition_point(|(span, _)| span.end <= from)..]
                        .iter()
                        .take_while(|(span, _)| span.start < end)
                        .cloned()
                    {
                        let start = range.start.max(from);
                        if r == 0 && cursor < start {
                            styles.push((at + (cursor - from)..at + (start - from), header));
                        }
                        cursor = range.end.min(end);
                        style.strong |= r == 0;
                        styles.push((at + (start - from)..at + (cursor - from), style));
                    }
                    if r == 0 && cursor < end {
                        styles.push((at + (cursor - from)..at + (end - from), header));
                    }
                    if i + 1 < widths.len() {
                        text.extend(std::iter::repeat_n(
                            ' ',
                            (padding - left).min(BLOCK_PAINT_LIMIT.saturating_sub(text.len())),
                        ));
                    }
                    if text.len() >= BLOCK_PAINT_LIMIT {
                        let kept = text.floor_char_boundary(BLOCK_PAINT_LIMIT - '…'.len_utf8());
                        text.truncate(kept);
                        text.push('…');
                        styles.retain_mut(|(range, _)| {
                            range.end = range.end.min(kept);
                            range.start < range.end
                        });
                        return (text, styles);
                    }
                }
                text.push('\n');
                if offsets
                    .iter()
                    .zip(row)
                    .all(|(&offset, cell)| offset == cell.text.len())
                {
                    break;
                }
            }
        }
        (text, styles)
    }
}
