// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic, non-web Markdown document model for native tab views.
//!
//! This is intentionally a document AST and navigation layer, not HTML. Every block and
//! link retains a UTF-8 source range so outline, preview, source editor, accessibility,
//! and restore all speak the same coordinates. Rendering lowers these nodes through the
//! native semantic UI tree.

#![allow(
    dead_code,
    reason = "native tab-app integration lands in staged consumers"
)]

use std::{borrow::Cow, ops::Range};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkdownDocument {
    pub(crate) blocks: Vec<MarkdownBlock>,
    pub(crate) outline: Vec<HeadingRef>,
    pub(crate) links: Vec<MarkdownLink>,
    pub(crate) images: Vec<MarkdownImage>,
    /// Ordered semantic inline annotations. Unlike the block presentation text,
    /// these retain both the complete authored syntax range and the inner
    /// content range, making source navigation stable across reflow and theme
    /// changes without retaining an HTML tree.
    pub(crate) inline_runs: Vec<MarkdownInlineRun>,
    anchors: std::collections::BTreeMap<String, usize>,
    /// Exact UTF-8 byte length of the canonical source used to build this
    /// projection. Reader selection remains source-addressed even though block
    /// text is lowered into a presentation-friendly form.
    pub(crate) source_len: usize,
    /// Canonical UTF-8 byte offset of every physical source line.  Source-mode
    /// navigation uses this parsed projection instead of retaining a second
    /// copy of the document or asking paint to inspect host-owned bytes.
    pub(crate) source_line_starts: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MarkdownBlock {
    Heading {
        level: u8,
        id: String,
        text: String,
        source: Range<usize>,
    },
    Paragraph {
        text: String,
        source: Range<usize>,
    },
    ListItem {
        depth: usize,
        ordinal: Option<u64>,
        text: String,
        source: Range<usize>,
    },
    Quote {
        text: String,
        source: Range<usize>,
    },
    CodeBlock {
        language: Option<String>,
        code: String,
        source: Range<usize>,
    },
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
        source: Range<usize>,
    },
    ThematicBreak {
        source: Range<usize>,
    },
}

impl MarkdownBlock {
    pub(crate) fn source(&self) -> &Range<usize> {
        match self {
            Self::Heading { source, .. }
            | Self::Paragraph { source, .. }
            | Self::ListItem { source, .. }
            | Self::Quote { source, .. }
            | Self::CodeBlock { source, .. }
            | Self::Table { source, .. }
            | Self::ThematicBreak { source } => source,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeadingRef {
    pub(crate) level: u8,
    pub(crate) id: String,
    pub(crate) text: String,
    pub(crate) source_start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkdownLink {
    pub(crate) label: String,
    pub(crate) destination: String,
    pub(crate) source: Range<usize>,
    pub(crate) policy: LinkPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkdownImage {
    pub(crate) alt: Option<String>,
    pub(crate) source_uri: String,
    pub(crate) source: Range<usize>,
    pub(crate) remote: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkdownInlineRun {
    pub(crate) kind: MarkdownInlineKind,
    pub(crate) text: String,
    /// Complete authored syntax, including delimiters/destination where one
    /// exists. This is the range selected by an inline-object action.
    pub(crate) source: Range<usize>,
    /// Source bytes that produced the visible text. Consumers use this smaller
    /// range for caret/source-map projection.
    pub(crate) content_source: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MarkdownInlineKind {
    Emphasis,
    Strong,
    Strikethrough,
    Code,
    Link { index: usize },
    Image { index: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkdownSourceSegment {
    pub(crate) display: Range<usize>,
    pub(crate) source: Range<usize>,
    pub(crate) inline: Option<MarkdownInlineKind>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MarkdownSemanticProjection {
    pub(crate) text: String,
    pub(crate) source_map: Vec<MarkdownSourceSegment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkPolicy {
    LocalAnchor,
    LocalDocument,
    ExplicitExternal,
    DeniedScheme,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MarkdownImageAction {
    SelectLocalSource { range: Range<usize> },
    OpenRemote { uri: String },
    Denied { message: &'static str },
}

/// Pure image-policy reducer. It has no ambient filesystem/network authority:
/// a relative local image becomes a source-selection action, while a bounded
/// HTTP(S) image may leave the process only after the activating user gesture.
pub(crate) fn reduce_image_action(
    image: &MarkdownImage,
    user_initiated: bool,
) -> MarkdownImageAction {
    if image.remote {
        let safe = image.source_uri.len() <= 2_048
            && (image.source_uri.starts_with("https://")
                || image.source_uri.starts_with("http://"));
        return if safe && user_initiated {
            MarkdownImageAction::OpenRemote {
                uri: image.source_uri.clone(),
            }
        } else if safe {
            MarkdownImageAction::Denied {
                message: "remote images require an explicit user gesture",
            }
        } else {
            MarkdownImageAction::Denied {
                message: "unsupported or oversized remote image URL",
            }
        };
    }

    let local = !image.source_uri.is_empty()
        && image.source_uri.len() <= 4_096
        && !image.source_uri.starts_with('/')
        && !image.source_uri.contains('\0')
        && !image
            .source_uri
            .split(['/', '\\'])
            .any(|component| component == "..");
    if local {
        MarkdownImageAction::SelectLocalSource {
            range: image.source.clone(),
        }
    } else {
        MarkdownImageAction::Denied {
            message: "image path escapes the document capability",
        }
    }
}

/// One exact, renderer-independent reading position. `source_anchor` identifies
/// the canonical block/source line while `visual_row` retains progress through
/// a block that wraps to more than one viewport row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MarkdownLocation {
    pub(crate) source_anchor: usize,
    pub(crate) visual_row: usize,
}

impl MarkdownLocation {
    pub(crate) const fn new(source_anchor: usize, visual_row: usize) -> Self {
        Self {
            source_anchor,
            visual_row,
        }
    }
}

/// Per-view history. Entries are source/visual-row anchors rather than pixel
/// offsets, so reparse and resize retain meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkdownHistory {
    entries: Vec<MarkdownLocation>,
    cursor: Option<usize>,
    capacity: usize,
}

const MARKDOWN_HISTORY_CAPACITY: usize = 128;

impl Default for MarkdownHistory {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            cursor: None,
            capacity: MARKDOWN_HISTORY_CAPACITY,
        }
    }
}

impl MarkdownHistory {
    pub(crate) fn visit(&mut self, location: MarkdownLocation) {
        if self
            .cursor
            .and_then(|cursor| self.entries.get(cursor))
            .copied()
            == Some(location)
        {
            return;
        }
        if let Some(cursor) = self.cursor {
            self.entries.truncate(cursor.saturating_add(1));
        }
        self.entries.push(location);
        let overflow = self.entries.len().saturating_sub(self.capacity.max(1));
        if overflow > 0 {
            self.entries.drain(..overflow);
        }
        self.cursor = self.entries.len().checked_sub(1);
    }

    pub(crate) fn back(&mut self) -> Option<MarkdownLocation> {
        let cursor = self.cursor?;
        if cursor == 0 {
            return self.entries.first().copied();
        }
        self.cursor = Some(cursor - 1);
        self.entries.get(cursor - 1).copied()
    }

    pub(crate) fn forward(&mut self) -> Option<MarkdownLocation> {
        let cursor = self.cursor?;
        let next = cursor.saturating_add(1);
        if next >= self.entries.len() {
            return self.entries.get(cursor).copied();
        }
        self.cursor = Some(next);
        self.entries.get(next).copied()
    }

    pub(crate) fn current(&self) -> Option<MarkdownLocation> {
        self.cursor
            .and_then(|cursor| self.entries.get(cursor))
            .copied()
    }

    pub(crate) fn can_back(&self) -> bool {
        self.cursor.is_some_and(|cursor| cursor > 0)
    }

    pub(crate) fn can_forward(&self) -> bool {
        self.cursor
            .is_some_and(|cursor| cursor.saturating_add(1) < self.entries.len())
    }

    #[cfg(test)]
    fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            ..Self::default()
        }
    }
}

pub(crate) fn parse(source: &str) -> MarkdownDocument {
    // Comments are presentation metadata, not reader content. The masked
    // projection has exactly the same byte length as `source`, so every block,
    // link, selection, and history coordinate remains a canonical source byte.
    let presentation_source = mask_html_comments(source);
    let display_source = presentation_source.as_ref();
    let lines = source_lines(display_source);
    let mut blocks = Vec::new();
    let mut outline = Vec::new();
    let mut links = Vec::new();
    let mut images = Vec::new();
    let mut inline_runs = Vec::new();
    let mut anchors = std::collections::BTreeMap::new();
    let mut heading_ids = std::collections::BTreeMap::<String, usize>::new();
    let mut i = 0usize;

    while i < lines.len() {
        let line = &lines[i];
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        if let Some(fence) = fence_start(trimmed) {
            let start = line.start;
            let language = trimmed[fence.len()..].trim();
            let language = (!language.is_empty()).then(|| language.to_string());
            i += 1;
            let mut code = String::new();
            let mut end = line.end;
            while i < lines.len() {
                let candidate = &lines[i];
                end = candidate.end;
                if candidate.text.trim_start().starts_with(fence) {
                    i += 1;
                    break;
                }
                code.push_str(candidate.text);
                if candidate.has_newline {
                    code.push('\n');
                }
                i += 1;
            }
            blocks.push(MarkdownBlock::CodeBlock {
                language,
                code,
                source: start..end,
            });
            continue;
        }

        if let Some((level, text)) = heading(trimmed) {
            let text = lower_inline_text(text);
            let base = slug(&text);
            let count = heading_ids.entry(base.clone()).or_insert(0);
            let id = if *count == 0 {
                base
            } else {
                format!("{base}-{}", *count + 1)
            };
            *count += 1;
            let block = MarkdownBlock::Heading {
                level,
                id: id.clone(),
                text: text.clone(),
                source: line.start..line.end,
            };
            outline.push(HeadingRef {
                level,
                id: id.clone(),
                text,
                source_start: line.start,
            });
            anchors.insert(id, line.start);
            scan_inlines(
                display_source,
                line.start..line.end,
                &mut links,
                &mut images,
                &mut inline_runs,
            );
            blocks.push(block);
            i += 1;
            continue;
        }

        if is_thematic(trimmed) {
            blocks.push(MarkdownBlock::ThematicBreak {
                source: line.start..line.end,
            });
            i += 1;
            continue;
        }

        if i + 1 < lines.len() && is_table_separator(lines[i + 1].text.trim()) {
            let start = line.start;
            scan_inlines(
                display_source,
                line.start..line.end,
                &mut links,
                &mut images,
                &mut inline_runs,
            );
            let header = table_cells(line.text);
            i += 2;
            let mut rows = Vec::new();
            let mut end = lines[i - 1].end;
            while i < lines.len() && lines[i].text.contains('|') && !lines[i].text.trim().is_empty()
            {
                scan_inlines(
                    display_source,
                    lines[i].start..lines[i].end,
                    &mut links,
                    &mut images,
                    &mut inline_runs,
                );
                rows.push(table_cells(lines[i].text));
                end = lines[i].end;
                i += 1;
            }
            blocks.push(MarkdownBlock::Table {
                header,
                rows,
                source: start..end,
            });
            continue;
        }

        if let Some((depth, ordinal, text)) = list_item(line.text) {
            scan_inlines(
                display_source,
                line.start..line.end,
                &mut links,
                &mut images,
                &mut inline_runs,
            );
            blocks.push(MarkdownBlock::ListItem {
                depth,
                ordinal,
                text: lower_inline_text(text),
                source: line.start..line.end,
            });
            i += 1;
            continue;
        }

        if trimmed.starts_with('>') {
            let start = line.start;
            let mut end = line.end;
            let mut quote = String::new();
            while i < lines.len() {
                let candidate = &lines[i];
                let candidate_trimmed = candidate.text.trim();
                let Some(text) = candidate_trimmed.strip_prefix('>') else {
                    break;
                };
                if !quote.is_empty() {
                    quote.push(' ');
                }
                quote.push_str(text.trim_start());
                scan_inlines(
                    display_source,
                    candidate.start..candidate.end,
                    &mut links,
                    &mut images,
                    &mut inline_runs,
                );
                end = candidate.end;
                i += 1;
            }
            blocks.push(MarkdownBlock::Quote {
                text: lower_inline_text(&quote),
                source: start..end,
            });
            continue;
        }

        let start = line.start;
        let mut end = line.end;
        let mut paragraph = line.text.trim().to_string();
        scan_inlines(
            display_source,
            line.start..line.end,
            &mut links,
            &mut images,
            &mut inline_runs,
        );
        i += 1;
        while i < lines.len() {
            let candidate = &lines[i];
            let candidate_trimmed = candidate.text.trim();
            if candidate_trimmed.is_empty()
                || heading(candidate_trimmed).is_some()
                || fence_start(candidate_trimmed).is_some()
                || list_item(candidate.text).is_some()
                || candidate_trimmed.starts_with('>')
                || is_thematic(candidate_trimmed)
                || (i + 1 < lines.len() && is_table_separator(lines[i + 1].text.trim()))
            {
                break;
            }
            paragraph.push(' ');
            paragraph.push_str(candidate_trimmed);
            end = candidate.end;
            scan_inlines(
                display_source,
                candidate.start..candidate.end,
                &mut links,
                &mut images,
                &mut inline_runs,
            );
            i += 1;
        }
        blocks.push(MarkdownBlock::Paragraph {
            text: lower_inline_text(&paragraph),
            source: start..end,
        });
    }

    MarkdownDocument {
        blocks,
        outline,
        links,
        images,
        inline_runs,
        anchors,
        source_len: source.len(),
        source_line_starts: std::iter::once(0)
            .chain(source.bytes().enumerate().filter_map(|(index, byte)| {
                (byte == b'\n' && index + 1 < source.len()).then_some(index + 1)
            }))
            .collect(),
    }
}

/// Canonical semantic reading order used by app introspection and clipboard selection.
pub(crate) fn semantic_text(document: &MarkdownDocument) -> String {
    semantic_projection(document).text
}

/// Build the bounded semantic reading projection and its stable source map.
/// Block segments guarantee total coverage; nested inline segments refine the
/// exact authored range for styled/link/image content without invalidating the
/// coarser mapping used by assistive technology.
pub(crate) fn semantic_projection(document: &MarkdownDocument) -> MarkdownSemanticProjection {
    let mut out = String::new();
    let mut source_map = Vec::new();
    for block in &document.blocks {
        let display_start = out.len();
        let text = match block {
            MarkdownBlock::Heading { text, .. }
            | MarkdownBlock::Paragraph { text, .. }
            | MarkdownBlock::ListItem { text, .. }
            | MarkdownBlock::Quote { text, .. } => text.clone(),
            MarkdownBlock::CodeBlock { code, .. } => code.clone(),
            MarkdownBlock::Table { header, rows, .. } => {
                let mut table = header.join("\t");
                for row in rows {
                    table.push('\n');
                    table.push_str(&row.join("\t"));
                }
                table
            }
            MarkdownBlock::ThematicBreak { .. } => continue,
        };
        out.push_str(&text);
        let display_end = out.len();
        source_map.push(MarkdownSourceSegment {
            display: display_start..display_end,
            source: block.source().clone(),
            inline: None,
        });

        let mut search_from = display_start;
        for run in inline_runs_in_range(document, block.source().clone()) {
            if run.text.is_empty() {
                continue;
            }
            let Some(relative) = out[search_from..display_end].find(&run.text) else {
                continue;
            };
            let start = search_from + relative;
            let end = start + run.text.len();
            source_map.push(MarkdownSourceSegment {
                display: start..end,
                source: run.content_source.clone(),
                inline: Some(run.kind.clone()),
            });
            search_from = end;
        }
        out.push('\n');
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    source_map.retain(|segment| segment.display.start < trimmed);
    for segment in &mut source_map {
        segment.display.end = segment.display.end.min(trimmed);
    }
    MarkdownSemanticProjection {
        text: out,
        source_map,
    }
}

pub(crate) fn inline_runs_in_range(
    document: &MarkdownDocument,
    source: Range<usize>,
) -> impl Iterator<Item = &MarkdownInlineRun> {
    let start = document
        .inline_runs
        .partition_point(|run| run.source.end <= source.start);
    document.inline_runs[start..]
        .iter()
        .take_while(move |run| run.source.start < source.end)
}

pub(crate) fn block_at_source(document: &MarkdownDocument, source_offset: usize) -> Option<usize> {
    document
        .blocks
        .partition_point(|block| block.source().start <= source_offset)
        .checked_sub(1)
}

/// Heading owning the current source anchor. This is O(log n) and is shared by
/// the wide outline, compact status, and command projection so they cannot
/// disagree about the active section.
pub(crate) fn heading_at_source(
    document: &MarkdownDocument,
    source_offset: usize,
) -> Option<usize> {
    let next = document
        .outline
        .partition_point(|heading| heading.source_start <= source_offset);
    next.checked_sub(1)
}

/// Resolve a parsed local `#anchor` without interpreting a path or touching the
/// filesystem. Heading ids are parser-owned and deterministic.
pub(crate) fn local_anchor(document: &MarkdownDocument, destination: &str) -> Option<usize> {
    let id = destination.strip_prefix('#')?;
    if id.len() > 512 {
        return None;
    }
    document.anchors.get(id).copied()
}

/// Produce a bounded, active-centered outline window. Building a frame or
/// command palette is therefore independent of an adversarial document's total
/// heading count.
pub(crate) fn outline_window(
    document: &MarkdownDocument,
    source_anchor: usize,
    limit: usize,
) -> Range<usize> {
    let limit = limit.max(1).min(document.outline.len());
    if limit == 0 {
        return 0..0;
    }
    let active = heading_at_source(document, source_anchor).unwrap_or(0);
    let start = active
        .saturating_sub(limit / 2)
        .min(document.outline.len().saturating_sub(limit));
    start..start.saturating_add(limit)
}

/// One lazily materialized Markdown block in document-space logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VisibleBlock {
    pub(crate) index: usize,
    pub(crate) y: f32,
    pub(crate) height: f32,
    pub(crate) source: Range<usize>,
    pub(crate) visual_row: usize,
    pub(crate) total_visual_rows: usize,
}

/// Exterior rhythm between reader cards/runs. The estimator already reserves
/// at least this much trailing space per block; the app shell moves that slice
/// outside the painted rect, so scroll geometry and total height stay exact.
pub(crate) const VISUAL_BLOCK_GAP: f32 = 4.0;

/// Bounded block layout around a stable source anchor. `total_height` is an estimate
/// suitable for a scrollbar; navigation/restoration continues to use source offsets.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct VirtualBlockLayout {
    pub(crate) blocks: Vec<VisibleBlock>,
    pub(crate) total_height: f32,
    pub(crate) anchor_index: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MarkdownSourceWindow {
    pub(crate) text: String,
    pub(crate) source: Range<usize>,
    pub(crate) truncated_before: bool,
    pub(crate) truncated_after: bool,
}

/// Materialize a UTF-8-safe, line-aligned source window around a reading
/// anchor. Source and split modes stay bounded even for adversarial files while
/// retaining canonical byte coordinates for selection/copy.
pub(crate) fn source_window(
    source: &str,
    anchor: usize,
    max_bytes: usize,
    max_lines: usize,
) -> MarkdownSourceWindow {
    if source.is_empty() || max_bytes == 0 || max_lines == 0 {
        return MarkdownSourceWindow::default();
    }
    let mut anchor = anchor.min(source.len());
    while !source.is_char_boundary(anchor) {
        anchor = anchor.saturating_sub(1);
    }
    let line_start = source[..anchor].rfind('\n').map_or(0, |at| at + 1);
    let context_start = source[..line_start]
        .rmatch_indices('\n')
        .nth(max_lines / 4)
        .map_or(0, |(at, _)| at + 1);
    let byte_limit = context_start.saturating_add(max_bytes).min(source.len());
    let mut end = byte_limit;
    while !source.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let line_limited = source[context_start..end]
        .match_indices('\n')
        .nth(max_lines)
        .map_or(end, |(relative, _)| context_start + relative + 1);
    end = line_limited;
    MarkdownSourceWindow {
        text: source[context_start..end].to_string(),
        source: context_start..end,
        truncated_before: context_start > 0,
        truncated_after: end < source.len(),
    }
}

/// Source-reader window whose first row is the exact requested source line.
/// Unlike the contextual inspection helper above, repeated page moves therefore
/// make monotonic progress through even a single enormous fenced block.
pub(crate) fn source_window_from_anchor(
    source: &str,
    anchor: usize,
    max_bytes: usize,
    max_lines: usize,
) -> MarkdownSourceWindow {
    if source.is_empty() || max_bytes == 0 || max_lines == 0 {
        return MarkdownSourceWindow::default();
    }
    let mut anchor = anchor.min(source.len());
    while !source.is_char_boundary(anchor) {
        anchor = anchor.saturating_sub(1);
    }
    let start = source[..anchor].rfind('\n').map_or(0, |at| at + 1);
    let byte_limit = start.saturating_add(max_bytes).min(source.len());
    let mut end = byte_limit;
    while !source.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    end = source[start..end]
        .match_indices('\n')
        .nth(max_lines.saturating_sub(1))
        .map_or(end, |(relative, _)| start + relative + 1);
    MarkdownSourceWindow {
        text: source[start..end].to_string(),
        source: start..end,
        truncated_before: start > 0,
        truncated_after: end < source.len(),
    }
}

/// Lay out only the viewport plus a bounded lookahead. Work is proportional to the
/// visible block count, not total document length. The anchor block begins at y=0;
/// callers keep a small per-view intra-block offset for smooth scrolling.
pub(crate) fn layout_visible_blocks(
    document: &MarkdownDocument,
    source_anchor: usize,
    visual_row: usize,
    viewport_width: f32,
    viewport_height: f32,
) -> VirtualBlockLayout {
    if document.blocks.is_empty() || viewport_width <= 0.0 || viewport_height <= 0.0 {
        return VirtualBlockLayout::default();
    }
    let anchor_index = block_at_source(document, source_anchor).unwrap_or(0);
    let sample_start = anchor_index.saturating_sub(32);
    let sample_end = sample_start.saturating_add(64).min(document.blocks.len());
    let sample = &document.blocks[sample_start..sample_end];
    let mean_height = sample
        .iter()
        .map(|block| estimated_block_height(block, viewport_width))
        .sum::<f32>()
        / sample.len().max(1) as f32;
    let total_height = mean_height * document.blocks.len() as f32;
    let start = anchor_index;
    let total_anchor_rows = block_visual_rows(&document.blocks[anchor_index], viewport_width);
    let visual_row = visual_row.min(total_anchor_rows.saturating_sub(1));
    let mut y = 0.0;
    let limit = viewport_height + 320.0;
    let mut blocks = Vec::new();
    for (index, block) in document.blocks.iter().enumerate().skip(start) {
        let total_visual_rows = block_visual_rows(block, viewport_width);
        let row = if index == anchor_index { visual_row } else { 0 };
        let full_height = estimated_block_height(block, viewport_width);
        let skipped = row as f32 * block_visual_row_height(block);
        let height = (full_height - skipped).max(VISUAL_BLOCK_GAP + 1.0);
        if y >= limit {
            break;
        }
        blocks.push(VisibleBlock {
            index,
            y,
            height,
            source: block.source().clone(),
            visual_row: row,
            total_visual_rows,
        });
        y += height;
    }
    VirtualBlockLayout {
        blocks,
        total_height,
        anchor_index,
    }
}

/// Number of reader rows in one block at the current measure. This is the
/// canonical unit for wheel/page navigation and the typed Markdown painter's
/// `visual_row` window.
pub(crate) fn block_visual_rows(block: &MarkdownBlock, viewport_width: f32) -> usize {
    let height = estimated_block_height(block, viewport_width);
    let padding = block_visual_padding(block);
    ((height - padding).max(1.0) / block_visual_row_height(block))
        .ceil()
        .max(1.0) as usize
}

pub(crate) fn block_visual_row_height(block: &MarkdownBlock) -> f32 {
    match block {
        MarkdownBlock::Heading { level, .. } => match level {
            1 => 38.0,
            2 => 32.0,
            3 => 28.0,
            _ => 24.0,
        },
        MarkdownBlock::Paragraph { .. } | MarkdownBlock::ListItem { .. } => 24.0,
        MarkdownBlock::Quote { .. } => 24.0,
        MarkdownBlock::CodeBlock { .. } => 22.0,
        MarkdownBlock::Table { .. } => 30.0,
        MarkdownBlock::ThematicBreak { .. } => 28.0,
    }
}

fn block_visual_padding(block: &MarkdownBlock) -> f32 {
    match block {
        MarkdownBlock::Heading { level, .. } => {
            if *level == 1 {
                16.0
            } else {
                14.0
            }
        }
        MarkdownBlock::Paragraph { .. } | MarkdownBlock::ListItem { .. } => 4.0,
        MarkdownBlock::Quote { .. } => 12.0,
        MarkdownBlock::CodeBlock { .. } => 28.0,
        MarkdownBlock::Table { .. } => 20.0,
        MarkdownBlock::ThematicBreak { .. } => 0.0,
    }
}

/// Move by exact visual rows, crossing block boundaries without collapsing a
/// tall block to a single step. Work is bounded by the requested row delta plus
/// crossed empty blocks; host input clamps ordinary wheel deltas.
pub(crate) fn move_visual_rows(
    document: &MarkdownDocument,
    location: MarkdownLocation,
    viewport_width: f32,
    delta: isize,
) -> MarkdownLocation {
    if document.blocks.is_empty() {
        return MarkdownLocation::default();
    }
    let mut block = block_at_source(document, location.source_anchor).unwrap_or(0);
    let mut row = location
        .visual_row
        .min(block_visual_rows(&document.blocks[block], viewport_width).saturating_sub(1));
    if delta == isize::MIN {
        return MarkdownLocation::new(document.blocks[0].source().start, 0);
    }
    if delta == isize::MAX {
        block = document.blocks.len() - 1;
        row = block_visual_rows(&document.blocks[block], viewport_width).saturating_sub(1);
        return MarkdownLocation::new(source_anchor_for_visual_row(document, block, row), row);
    }
    let mut remaining = delta.unsigned_abs().min(100_000);
    while remaining > 0 {
        if delta < 0 {
            if row > 0 {
                let step = remaining.min(row);
                row -= step;
                remaining -= step;
            } else if block > 0 {
                block -= 1;
                row = block_visual_rows(&document.blocks[block], viewport_width).saturating_sub(1);
                remaining -= 1;
            } else {
                break;
            }
        } else {
            let last = block_visual_rows(&document.blocks[block], viewport_width).saturating_sub(1);
            if row < last {
                let step = remaining.min(last - row);
                row += step;
                remaining -= step;
            } else if block + 1 < document.blocks.len() {
                block += 1;
                row = 0;
                remaining -= 1;
            } else {
                break;
            }
        }
    }
    MarkdownLocation::new(source_anchor_for_visual_row(document, block, row), row)
}

/// Move an exact number of physical source lines using the parser-owned line
/// index. This is the source-mode equivalent of [`move_visual_rows`].
pub(crate) fn move_source_lines(
    document: &MarkdownDocument,
    source_anchor: usize,
    delta: isize,
) -> usize {
    if document.source_line_starts.is_empty() {
        return 0;
    }
    if delta == isize::MIN {
        return 0;
    }
    if delta == isize::MAX {
        return *document.source_line_starts.last().unwrap_or(&0);
    }
    let current = document
        .source_line_starts
        .partition_point(|start| *start <= source_anchor)
        .saturating_sub(1);
    let target = current
        .saturating_add_signed(delta)
        .min(document.source_line_starts.len().saturating_sub(1));
    document.source_line_starts[target]
}

fn source_anchor_for_visual_row(document: &MarkdownDocument, block: usize, row: usize) -> usize {
    let source = document.blocks[block].source();
    let first_line = document
        .source_line_starts
        .partition_point(|start| *start <= source.start)
        .saturating_sub(1);
    let last_line = document
        .source_line_starts
        .partition_point(|start| *start < source.end)
        .saturating_sub(1)
        .max(first_line);
    document.source_line_starts[first_line.saturating_add(row).min(last_line)]
}

/// Move a reading anchor by block count without converting through fragile pixels.
pub(crate) fn move_block_anchor(
    document: &MarkdownDocument,
    source_anchor: usize,
    delta: isize,
) -> usize {
    let Some(current) = block_at_source(document, source_anchor) else {
        return 0;
    };
    let target = current
        .saturating_add_signed(delta)
        .min(document.blocks.len().saturating_sub(1));
    document.blocks[target].source().start
}

/// Resolve a viewport-sized reading move to an exact block index. Both
/// directions use the same width-aware block estimator as the visible layout,
/// so a short final page never shrinks the following "previous page" move to a
/// single block. Work is capped independently of document size.
pub(crate) fn page_target_block(
    document: &MarkdownDocument,
    source_anchor: usize,
    viewport_width: f32,
    viewport_height: f32,
    direction: isize,
) -> usize {
    const MAX_PAGE_BLOCKS: usize = 256;

    let Some(current) = block_at_source(document, source_anchor) else {
        return 0;
    };
    if direction == 0 || document.blocks.len() <= 1 {
        return current;
    }
    let target_height = if viewport_height.is_finite() {
        (viewport_height * 0.82).clamp(1.0, 100_000.0)
    } else {
        1.0
    };
    let width = if viewport_width.is_finite() {
        viewport_width.max(1.0)
    } else {
        1.0
    };
    let mut target = current;
    let mut distance = 0.0;
    for _ in 0..MAX_PAGE_BLOCKS {
        let next = if direction < 0 {
            let Some(previous) = target.checked_sub(1) else {
                break;
            };
            previous
        } else {
            let next = target.saturating_add(1);
            if next >= document.blocks.len() {
                break;
            }
            next
        };
        target = next;
        distance += estimated_block_height(&document.blocks[target], width);
        if distance >= target_height {
            break;
        }
    }
    target
}

fn estimated_block_height(block: &MarkdownBlock, viewport_width: f32) -> f32 {
    const MAX_LAYOUT_CHARS_PER_BLOCK: usize = 128 * 1024;
    let measure = (viewport_width.min(760.0) - 24.0).max(120.0);
    let (text_len, line_height, vertical_padding, average_advance) = match block {
        MarkdownBlock::Heading { level, text, .. } => {
            let line = match level {
                1 => 38.0,
                2 => 32.0,
                3 => 28.0,
                _ => 24.0,
            };
            let advance = match level {
                1 => 12.0,
                2 => 10.0,
                3 => 9.0,
                _ => 8.0,
            };
            (
                text.chars().take(MAX_LAYOUT_CHARS_PER_BLOCK).count(),
                line,
                if *level == 1 { 16.0 } else { 14.0 },
                advance,
            )
        }
        MarkdownBlock::Paragraph { text, .. } | MarkdownBlock::ListItem { text, .. } => (
            text.chars().take(MAX_LAYOUT_CHARS_PER_BLOCK).count(),
            24.0,
            4.0,
            8.0,
        ),
        MarkdownBlock::Quote { text, .. } => (
            text.chars().take(MAX_LAYOUT_CHARS_PER_BLOCK).count(),
            24.0,
            12.0,
            8.0,
        ),
        MarkdownBlock::CodeBlock { code, .. } => {
            let chars_per_line = (measure / 8.0).max(12.0) as usize;
            let mut visual_lines = 1usize;
            let mut column = 0usize;
            for character in code.chars().take(MAX_LAYOUT_CHARS_PER_BLOCK) {
                if character == '\n' {
                    visual_lines = visual_lines.saturating_add(1);
                    column = 0;
                    continue;
                }
                column = column.saturating_add(1);
                if column >= chars_per_line {
                    visual_lines = visual_lines.saturating_add(1);
                    column = 0;
                }
            }
            return visual_lines as f32 * 22.0 + 28.0;
        }
        MarkdownBlock::Table { rows, .. } => {
            return (rows.len().saturating_add(1) as f32 * 30.0) + 20.0;
        }
        MarkdownBlock::ThematicBreak { .. } => return 28.0,
    };
    // The retained UI heading faces are materially wider than body/mono text.
    // A single body-width divisor clipped wrapped H1/H2 rows in real pixels;
    // use a conservative per-style average so virtualization reserves every
    // painted line even when the exact active face is unusually wide.
    let chars_per_line = (measure / average_advance).max(8.0) as usize;
    let lines = text_len.div_ceil(chars_per_line).max(1);
    lines as f32 * line_height + vertical_padding
}

fn link_policy(destination: &str) -> LinkPolicy {
    if destination.starts_with('#') {
        return LinkPolicy::LocalAnchor;
    }
    if !destination.contains(':') {
        return LinkPolicy::LocalDocument;
    }
    let scheme = destination
        .split_once(':')
        .map_or("", |(scheme, _)| scheme)
        .to_ascii_lowercase();
    match scheme.as_str() {
        "https" | "http" | "mailto" => LinkPolicy::ExplicitExternal,
        _ => LinkPolicy::DeniedScheme,
    }
}

#[derive(Clone, Copy)]
struct SourceLine<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    has_newline: bool,
}

fn source_lines(source: &str) -> Vec<SourceLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for segment in source.split_inclusive('\n') {
        let has_newline = segment.ends_with('\n');
        let text = segment.strip_suffix('\n').unwrap_or(segment);
        let end = start.saturating_add(segment.len());
        lines.push(SourceLine {
            text,
            start,
            end,
            has_newline,
        });
        start = end;
    }
    if source.is_empty() {
        return lines;
    }
    if start < source.len() {
        lines.push(SourceLine {
            text: &source[start..],
            start,
            end: source.len(),
            has_newline: false,
        });
    }
    lines
}

/// Hide HTML comments without moving a single canonical source byte.
///
/// The reader is deliberately not an HTML renderer. Comments therefore act as
/// non-rendering Markdown metadata, while identical bytes inside inline/fenced
/// code remain authored code. Replacing every non-newline comment byte with an
/// ASCII space keeps the projection valid UTF-8 and preserves all ranges.
fn mask_html_comments(source: &str) -> Cow<'_, str> {
    let mut masked = None::<Vec<u8>>;
    let mut in_comment = false;
    let mut inline_code_ticks = None::<usize>;
    let mut fence_marker = None::<u8>;

    for line in source_lines(source) {
        let bytes = line.text.as_bytes();
        let trimmed = line.text.trim_start().as_bytes();

        if let Some(marker) = fence_marker {
            if trimmed.starts_with(&[marker, marker, marker]) {
                fence_marker = None;
            }
            continue;
        }
        if !in_comment && inline_code_ticks.is_none() {
            let marker = trimmed.first().copied();
            if matches!(marker, Some(b'`' | b'~'))
                && marker.is_some_and(|marker| trimmed.starts_with(&[marker, marker, marker]))
            {
                fence_marker = marker;
                continue;
            }
        }

        let mut index = 0usize;
        while index < bytes.len() {
            if in_comment {
                let close = find_ascii(&bytes[index..], b"-->");
                let end = close.map_or(bytes.len(), |relative| index + relative + 3);
                mask_source_range(&mut masked, source, line.start + index..line.start + end);
                index = end;
                in_comment = close.is_none();
                continue;
            }

            if bytes[index] == b'`' {
                let run = ascii_run(bytes, index, b'`');
                if let Some(open) = inline_code_ticks {
                    if run == open {
                        inline_code_ticks = None;
                    }
                } else if !is_backslash_escaped(bytes, index) {
                    inline_code_ticks = Some(run);
                }
                index += run;
                continue;
            }

            if inline_code_ticks.is_none() && bytes[index..].starts_with(b"<!--") {
                in_comment = true;
                continue;
            }
            index += line.text[index..].chars().next().map_or(1, char::len_utf8);
        }
    }

    masked.map_or(Cow::Borrowed(source), |bytes| {
        Cow::Owned(String::from_utf8(bytes).expect("comment masking preserves UTF-8"))
    })
}

fn mask_source_range(masked: &mut Option<Vec<u8>>, source: &str, range: Range<usize>) {
    let bytes = masked.get_or_insert_with(|| source.as_bytes().to_vec());
    for byte in &mut bytes[range] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
}

fn find_ascii(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|candidate| candidate == needle)
}

fn ascii_run(bytes: &[u8], start: usize, marker: u8) -> usize {
    bytes[start..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count()
}

fn is_backslash_escaped(bytes: &[u8], index: usize) -> bool {
    bytes[..index]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

/// Lower inline Markdown into the authored reader text without changing the
/// canonical source. This intentionally produces one flat presentation run—the
/// native renderer does not yet carry styled inline spans—but it removes syntax
/// that should never be visible in a reading surface. Every scanner is linear
/// within its recursion level and nesting is capped, so malformed input remains
/// bounded and round-trips literally instead of triggering backtracking.
fn lower_inline_text(source: &str) -> String {
    normalize_display_whitespace(&lower_inline_segment(source, 0))
}

const MAX_INLINE_NESTING: usize = 32;

fn lower_inline_segment(source: &str, depth: usize) -> String {
    if depth >= MAX_INLINE_NESTING {
        return source.to_string();
    }
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut index = 0usize;
    while index < bytes.len() {
        // Backslash escapes are opaque authored punctuation, not formatting.
        if bytes[index] == b'\\'
            && let Some(next) = bytes.get(index + 1)
            && next.is_ascii_punctuation()
        {
            out.push(char::from(*next));
            index += 2;
            continue;
        }

        // Exact-length code-span delimiters preserve every Markdown-looking
        // byte inside. Whitespace follows CommonMark's single-line projection.
        if bytes[index] == b'`' {
            let ticks = ascii_run(bytes, index, b'`');
            if let Some(close) = find_exact_run(bytes, index + ticks, b'`', ticks) {
                let mut code = source[index + ticks..close]
                    .chars()
                    .map(|character| {
                        if matches!(character, '\r' | '\n') {
                            ' '
                        } else {
                            character
                        }
                    })
                    .collect::<String>();
                if code.starts_with(' ')
                    && code.ends_with(' ')
                    && code.chars().any(|character| character != ' ')
                {
                    code.remove(0);
                    code.pop();
                }
                out.push_str(&code);
                index = close + ticks;
                continue;
            }
            out.push_str(&source[index..index + ticks]);
            index += ticks;
            continue;
        }

        // Inline links/images present their recursively lowered label or alt
        // text. Destination bytes remain in the typed link metadata and exact
        // source selection, but do not clutter the reading projection.
        let image = bytes[index] == b'!' && bytes.get(index + 1) == Some(&b'[');
        let label_open = if image { index + 1 } else { index };
        if bytes.get(label_open) == Some(&b'[')
            && let Some(label_close) = find_balanced_close(source, label_open, b'[', b']')
            && bytes.get(label_close + 1) == Some(&b'(')
            && let Some(destination_close) =
                find_balanced_close(source, label_close + 1, b'(', b')')
        {
            out.push_str(&lower_inline_segment(
                &source[label_open + 1..label_close],
                depth + 1,
            ));
            index = destination_close + 1;
            continue;
        }

        // Paired emphasis/strong/strike delimiters disappear; unmatched math
        // stars and intraword underscores remain literal.
        if matches!(bytes[index], b'*' | b'_' | b'~') {
            let marker = bytes[index];
            let run = ascii_run(bytes, index, marker);
            let supported = if marker == b'~' {
                run == 2
            } else {
                (1..=3).contains(&run)
            };
            if supported
                && delimiter_can_open(bytes, index, marker, run)
                && let Some(close) = find_emphasis_close(bytes, index + run, marker, run)
            {
                out.push_str(&lower_inline_segment(
                    &source[index + run..close],
                    depth + 1,
                ));
                index = close + run;
                continue;
            }
            out.push_str(&source[index..index + run]);
            index += run;
            continue;
        }

        // Raw inline HTML is not executable in this non-web reader. Autolinks
        // retain their useful destination; ordinary tags disappear.
        if bytes[index] == b'<'
            && let Some(relative) = source[index + 1..].find('>')
        {
            let close = index + 1 + relative;
            let inside = &source[index + 1..close];
            if is_autolink(inside) {
                out.push_str(inside);
                index = close + 1;
                continue;
            }
            if looks_like_html_tag(inside) {
                index = close + 1;
                continue;
            }
        }

        if bytes[index] == b'&'
            && let Some(relative) = source[index + 1..]
                .bytes()
                .take(20)
                .position(|byte| byte == b';')
        {
            let close = index + 1 + relative;
            if let Some(entity) = decode_entity(&source[index + 1..close]) {
                out.push(entity);
                index = close + 1;
                continue;
            }
        }

        let character = source[index..]
            .chars()
            .next()
            .expect("index remains on a UTF-8 boundary");
        out.push(character);
        index += character.len_utf8();
    }
    out
}

fn normalize_display_whitespace(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut pending_space = false;
    for character in source.chars() {
        if character.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(character);
        }
    }
    out
}

fn find_exact_run(bytes: &[u8], mut index: usize, marker: u8, wanted: usize) -> Option<usize> {
    while index < bytes.len() {
        if bytes[index] != marker {
            index += 1;
            continue;
        }
        let run = ascii_run(bytes, index, marker);
        if run == wanted && !is_backslash_escaped(bytes, index) {
            return Some(index);
        }
        index += run;
    }
    None
}

fn find_balanced_close(source: &str, open: usize, opening: u8, closing: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut nesting = 0usize;
    let mut index = open;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && bytes
                .get(index + 1)
                .is_some_and(|byte| byte.is_ascii_punctuation())
        {
            index = (index + 2).min(bytes.len());
            continue;
        }
        if bytes[index] == opening {
            nesting = nesting.saturating_add(1);
        } else if bytes[index] == closing {
            nesting = nesting.checked_sub(1)?;
            if nesting == 0 {
                return Some(index);
            }
        }
        index += source[index..].chars().next()?.len_utf8();
    }
    None
}

fn delimiter_can_open(bytes: &[u8], index: usize, marker: u8, run: usize) -> bool {
    let before = index.checked_sub(1).and_then(|at| bytes.get(at)).copied();
    let after = bytes.get(index + run).copied();
    let after_is_space = after.is_none_or(|byte| byte.is_ascii_whitespace());
    let intraword_underscore = marker == b'_'
        && before.is_some_and(|byte| byte.is_ascii_alphanumeric())
        && after.is_some_and(|byte| byte.is_ascii_alphanumeric());
    !after_is_space && !intraword_underscore
}

fn delimiter_can_close(bytes: &[u8], index: usize, marker: u8, run: usize) -> bool {
    let before = index.checked_sub(1).and_then(|at| bytes.get(at)).copied();
    let after = bytes.get(index + run).copied();
    let before_is_space = before.is_none_or(|byte| byte.is_ascii_whitespace());
    let intraword_underscore = marker == b'_'
        && before.is_some_and(|byte| byte.is_ascii_alphanumeric())
        && after.is_some_and(|byte| byte.is_ascii_alphanumeric());
    !before_is_space && !intraword_underscore
}

fn find_emphasis_close(bytes: &[u8], mut index: usize, marker: u8, wanted: usize) -> Option<usize> {
    while index < bytes.len() {
        if bytes[index] != marker {
            index += 1;
            continue;
        }
        let run = ascii_run(bytes, index, marker);
        if run == wanted
            && !is_backslash_escaped(bytes, index)
            && delimiter_can_close(bytes, index, marker, wanted)
        {
            return Some(index);
        }
        index += run;
    }
    None
}

fn is_autolink(value: &str) -> bool {
    !value.bytes().any(|byte| byte.is_ascii_whitespace())
        && (value.starts_with("https://")
            || value.starts_with("http://")
            || value.starts_with("mailto:"))
}

fn looks_like_html_tag(value: &str) -> bool {
    let candidate = value.trim_start_matches('/').trim_start_matches(['!', '?']);
    candidate
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        value if value.starts_with("#x") || value.starts_with("#X") => {
            char::from_u32(u32::from_str_radix(&value[2..], 16).ok()?)
        }
        value if value.starts_with('#') => char::from_u32(value[1..].parse().ok()?),
        _ => None,
    }
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let count = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&count) || line.as_bytes().get(count) != Some(&b' ') {
        return None;
    }
    Some((count as u8, line[count + 1..].trim_end_matches('#').trim()))
}

fn fence_start(line: &str) -> Option<&str> {
    if line.starts_with("```") {
        Some("```")
    } else if line.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn is_thematic(line: &str) -> bool {
    for marker in ['-', '*', '_'] {
        let compact = line
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        if compact.len() >= 3 && compact.chars().all(|ch| ch == marker) {
            return true;
        }
    }
    false
}

fn list_item(line: &str) -> Option<(usize, Option<u64>, &str)> {
    let trimmed = line.trim_start();
    let depth = line.len().saturating_sub(trimmed.len()) / 2;
    if let Some(text) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        return Some((depth, None, text));
    }
    let digits = trimmed
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    let ordinal = trimmed[..digits].parse().ok()?;
    let tail = &trimmed[digits..];
    let text = tail
        .strip_prefix(". ")
        .or_else(|| tail.strip_prefix(") "))?;
    Some((depth, Some(ordinal), text))
}

fn table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| lower_inline_text(cell.trim()))
        .collect()
}

fn is_table_separator(line: &str) -> bool {
    let cells = table_cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim_matches(':').trim();
            cell.len() >= 3 && cell.chars().all(|ch| ch == '-')
        })
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            out.push(ch);
            dash = false;
        } else if !out.is_empty() {
            dash = true;
        }
    }
    if out.is_empty() {
        "section".into()
    } else {
        out
    }
}

fn scan_inlines(
    source: &str,
    range: Range<usize>,
    links: &mut Vec<MarkdownLink>,
    images: &mut Vec<MarkdownImage>,
    runs: &mut Vec<MarkdownInlineRun>,
) {
    scan_inline_segment(source, range, links, images, runs, 0);
}

fn scan_inline_segment(
    source: &str,
    range: Range<usize>,
    links: &mut Vec<MarkdownLink>,
    images: &mut Vec<MarkdownImage>,
    runs: &mut Vec<MarkdownInlineRun>,
    depth: usize,
) {
    if depth >= MAX_INLINE_NESTING || range.start >= range.end {
        return;
    }
    let text = &source[range.clone()];
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && bytes
                .get(i + 1)
                .is_some_and(|byte| byte.is_ascii_punctuation())
        {
            i += 2;
            continue;
        }

        if bytes[i] == b'`' {
            let ticks = ascii_run(bytes, i, b'`');
            if let Some(close) = find_exact_run(bytes, i + ticks, b'`', ticks) {
                let content = i + ticks..close;
                let authored = range.start + i..range.start + close + ticks;
                let mut code = text[content.clone()]
                    .chars()
                    .map(|character| {
                        if matches!(character, '\r' | '\n') {
                            ' '
                        } else {
                            character
                        }
                    })
                    .collect::<String>();
                if code.starts_with(' ')
                    && code.ends_with(' ')
                    && code.chars().any(|character| character != ' ')
                {
                    code.remove(0);
                    code.pop();
                }
                runs.push(MarkdownInlineRun {
                    kind: MarkdownInlineKind::Code,
                    text: code,
                    source: authored,
                    content_source: range.start + content.start..range.start + content.end,
                });
                i = close + ticks;
                continue;
            }
            i += ticks;
            continue;
        }

        let image = bytes[i] == b'!' && bytes.get(i + 1) == Some(&b'[');
        let open = if image { i + 1 } else { i };
        if bytes.get(open) == Some(&b'[')
            && let Some(close) = find_balanced_close(text, open, b'[', b']')
            && bytes.get(close + 1) == Some(&b'(')
            && let Some(end) = find_balanced_close(text, close + 1, b'(', b')')
        {
            let label_range = open + 1..close;
            let label = &text[label_range.clone()];
            let destination = text[close + 2..end].trim();
            let absolute = range.start + i..range.start + end + 1;
            let content_source = range.start + label_range.start..range.start + label_range.end;
            let lowered = lower_inline_text(label);
            let kind = if image {
                let index = images.len();
                images.push(MarkdownImage {
                    alt: (!label.is_empty()).then(|| lowered.clone()),
                    source_uri: destination.to_string(),
                    source: absolute.clone(),
                    remote: matches!(link_policy(destination), LinkPolicy::ExplicitExternal),
                });
                MarkdownInlineKind::Image { index }
            } else {
                let index = links.len();
                links.push(MarkdownLink {
                    label: lowered.clone(),
                    destination: destination.to_string(),
                    source: absolute.clone(),
                    policy: link_policy(destination),
                });
                MarkdownInlineKind::Link { index }
            };
            runs.push(MarkdownInlineRun {
                kind,
                text: lowered,
                source: absolute,
                content_source: content_source.clone(),
            });
            scan_inline_segment(source, content_source, links, images, runs, depth + 1);
            i = end + 1;
            continue;
        }

        if matches!(bytes[i], b'*' | b'_' | b'~') {
            let marker = bytes[i];
            let run = ascii_run(bytes, i, marker);
            let supported = if marker == b'~' {
                run == 2
            } else {
                (1..=3).contains(&run)
            };
            if supported
                && delimiter_can_open(bytes, i, marker, run)
                && let Some(close) = find_emphasis_close(bytes, i + run, marker, run)
            {
                let content = i + run..close;
                let kind = if marker == b'~' {
                    MarkdownInlineKind::Strikethrough
                } else if run >= 2 {
                    MarkdownInlineKind::Strong
                } else {
                    MarkdownInlineKind::Emphasis
                };
                let content_source = range.start + content.start..range.start + content.end;
                runs.push(MarkdownInlineRun {
                    kind,
                    text: lower_inline_text(&text[content]),
                    source: range.start + i..range.start + close + run,
                    content_source: content_source.clone(),
                });
                scan_inline_segment(source, content_source, links, images, runs, depth + 1);
                i = close + run;
                continue;
            }
            i += run;
            continue;
        }

        if bytes[i] == b'<'
            && let Some(relative) = text[i + 1..].find('>')
        {
            let close = i + 1 + relative;
            let destination = &text[i + 1..close];
            if is_autolink(destination) {
                let index = links.len();
                let authored = range.start + i..range.start + close + 1;
                let content = range.start + i + 1..range.start + close;
                links.push(MarkdownLink {
                    label: destination.to_string(),
                    destination: destination.to_string(),
                    source: authored.clone(),
                    policy: link_policy(destination),
                });
                runs.push(MarkdownInlineRun {
                    kind: MarkdownInlineKind::Link { index },
                    text: destination.to_string(),
                    source: authored,
                    content_source: content,
                });
                i = close + 1;
                continue;
            }
        }

        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_spec::derive::{
        Model, native_markdown_history_model, native_markdown_viewport_model,
    };
    use aterm_spec::interp::{State, admits};

    #[test]
    fn parses_blocks_outline_links_images_and_stable_ranges() {
        let source = "# Hello World\n\nText [site](https://example.com).\n\n- one\n\n```rust\nfn x() {}\n```\n\n![diagram](local.png)\n";
        let document = parse(source);
        assert_eq!(document.outline[0].id, "hello-world");
        assert!(matches!(
            document.blocks[0],
            MarkdownBlock::Heading { level: 1, .. }
        ));
        assert!(document
            .blocks
            .iter()
            .any(|block| matches!(block, MarkdownBlock::CodeBlock { language: Some(lang), .. } if lang == "rust")));
        assert_eq!(document.links[0].policy, LinkPolicy::ExplicitExternal);
        assert_eq!(document.images[0].alt.as_deref(), Some("diagram"));
        for block in &document.blocks {
            assert!(block.source().end <= source.len());
            assert!(block.source().start < block.source().end);
        }
    }

    #[test]
    fn reader_hides_comments_but_preserves_code_and_canonical_ranges() {
        let source = "<!-- hidden [bad](https://bad.example) -->\n# **Visible**\n\nBefore <!-- private --> after.\n\n`<!-- inline code -->`\n\n```md\n<!-- fenced code -->\n```\n\n<!-- unclosed";
        let document = parse(source);
        let text = semantic_text(&document);

        assert!(text.contains("Visible"));
        assert!(text.contains("Before after."));
        assert!(text.contains("<!-- inline code -->"));
        assert!(text.contains("<!-- fenced code -->"));
        assert!(!text.contains("hidden"));
        assert!(!text.contains("private"));
        assert!(!text.contains("unclosed"));
        assert!(
            document.links.is_empty(),
            "comment links never become actions"
        );
        assert_eq!(document.source_len, source.len());
        assert!(document.blocks.iter().all(|block| {
            block.source().start <= block.source().end && block.source().end <= source.len()
        }));
    }

    #[test]
    fn inline_lowering_presents_authored_text_and_keeps_malformed_literals() {
        let source = r"**strong** *em* ***both*** **outer *inner* tail** foo_bar_baz 2 * 3 \*literal\* `**code** [x](y) &amp;` [**site**](https://example.com) ![*alt*](pic.png) &amp; &#65; &#x42; &bogus;";
        assert_eq!(
            lower_inline_text(source),
            "strong em both outer inner tail foo_bar_baz 2 * 3 *literal* **code** [x](y) &amp; site alt & A B &bogus;"
        );

        let document = parse("[**site**](https://example.com) ![*alt*](pic.png)");
        assert_eq!(semantic_text(&document), "site alt");
        assert_eq!(document.links[0].label, "site");
        assert_eq!(document.links[0].destination, "https://example.com");
        assert_eq!(document.images[0].alt.as_deref(), Some("alt"));
        assert_eq!(
            &"[**site**](https://example.com) ![*alt*](pic.png)"[document.links[0].source.clone()],
            "[**site**](https://example.com)"
        );
    }

    #[test]
    fn inline_semantics_and_reading_source_map_retain_exact_authored_ranges() {
        let source = "Before **strong _em_** and `code` [site](https://example.com).";
        let document = parse(source);
        let authored = document
            .inline_runs
            .iter()
            .map(|run| (&run.kind, &source[run.source.clone()]))
            .collect::<Vec<_>>();
        assert!(authored.iter().any(|(kind, text)| {
            matches!(kind, MarkdownInlineKind::Strong) && *text == "**strong _em_**"
        }));
        assert!(authored.iter().any(|(kind, text)| {
            matches!(kind, MarkdownInlineKind::Emphasis) && *text == "_em_"
        }));
        assert!(
            authored.iter().any(|(kind, text)| {
                matches!(kind, MarkdownInlineKind::Code) && *text == "`code`"
            })
        );
        assert!(authored.iter().any(|(kind, text)| {
            matches!(kind, MarkdownInlineKind::Link { .. })
                && *text == "[site](https://example.com)"
        }));

        let projection = semantic_projection(&document);
        assert_eq!(projection.text, "Before strong em and code site.");
        let link = projection
            .source_map
            .iter()
            .find(|segment| matches!(segment.inline, Some(MarkdownInlineKind::Link { .. })))
            .expect("link has a fine-grained source mapping");
        assert_eq!(&projection.text[link.display.clone()], "site");
        assert_eq!(&source[link.source.clone()], "site");
        assert!(projection.source_map.iter().any(|segment| {
            segment.inline.is_none() && &source[segment.source.clone()] == source
        }));
    }

    #[test]
    fn source_window_is_utf8_safe_line_aligned_and_bounded() {
        let source = (0..400)
            .map(|line| format!("line {line} 🦀\n"))
            .collect::<String>();
        let anchor = source.find("line 220").unwrap();
        let window = source_window(&source, anchor, 512, 24);
        assert!(source.is_char_boundary(window.source.start));
        assert!(source.is_char_boundary(window.source.end));
        assert_eq!(&source[window.source.clone()], window.text);
        assert!(window.text.lines().count() <= 25);
        assert!(window.text.len() <= 512);
        assert!(window.truncated_before);
        assert!(window.truncated_after);
        assert!(window.source.start <= anchor && anchor < window.source.end);
    }

    #[test]
    fn image_policy_requires_gesture_and_never_allows_capability_escape() {
        let document = parse(
            "![local](assets/diagram.png) ![remote](https://example.com/x.png) ![bad](../../secret.png)",
        );
        assert!(matches!(
            reduce_image_action(&document.images[0], true),
            MarkdownImageAction::SelectLocalSource { .. }
        ));
        assert!(matches!(
            reduce_image_action(&document.images[1], false),
            MarkdownImageAction::Denied { .. }
        ));
        assert!(matches!(
            reduce_image_action(&document.images[1], true),
            MarkdownImageAction::OpenRemote { .. }
        ));
        // Negative control: treating every non-remote string as local would
        // grant a path escape. The real reducer must reject this exact mutant.
        assert!(matches!(
            reduce_image_action(&document.images[2], true),
            MarkdownImageAction::Denied { .. }
        ));
    }

    #[test]
    fn quote_continuations_lower_inline_markup_across_source_lines() {
        let source = "> **A terminal you can read,\n> reason about, and trust.**\n\nAfter.";
        let document = parse(source);

        assert!(matches!(
            &document.blocks[0],
            MarkdownBlock::Quote { text, source: range }
                if text == "A terminal you can read, reason about, and trust."
                    && &source[range.clone()]
                        == "> **A terminal you can read,\n> reason about, and trust.**\n"
        ));
        assert_eq!(
            semantic_text(&document),
            "A terminal you can read, reason about, and trust.\nAfter."
        );
    }

    #[test]
    fn malformed_and_utf8_inline_markup_never_panics_or_eats_unmatched_text() {
        assert_eq!(lower_inline_text("🦀 **unterminated"), "🦀 **unterminated");
        assert_eq!(
            lower_inline_text("[label](unterminated"),
            "[label](unterminated"
        );
        assert_eq!(
            lower_inline_text("`unterminated **code"),
            "`unterminated **code"
        );
        assert_eq!(lower_inline_text("<kbd>⌘K</kbd>"), "⌘K");
        assert_eq!(
            lower_inline_text("<https://example.com>"),
            "https://example.com"
        );
    }

    #[test]
    fn duplicate_headings_get_deterministic_ids() {
        let document = parse("## Same\n## Same\n## !!!\n");
        assert_eq!(document.outline[0].id, "same");
        assert_eq!(document.outline[1].id, "same-2");
        assert_eq!(document.outline[2].id, "section");
    }

    #[test]
    fn unsafe_schemes_are_never_external_open_requests() {
        let document =
            parse("[ok](https://example.com) [bad](javascript:alert) [local](README.md)");
        assert_eq!(document.links[0].policy, LinkPolicy::ExplicitExternal);
        assert_eq!(document.links[1].policy, LinkPolicy::DeniedScheme);
        assert_eq!(document.links[2].policy, LinkPolicy::LocalDocument);
    }

    #[test]
    fn history_truncates_forward_branch_and_uses_source_anchors() {
        let mut history = MarkdownHistory::default();
        for anchor in [10, 20, 30] {
            history.visit(MarkdownLocation::new(anchor, 0));
        }
        assert_eq!(history.back(), Some(MarkdownLocation::new(20, 0)));
        assert_eq!(history.back(), Some(MarkdownLocation::new(10, 0)));
        assert_eq!(history.forward(), Some(MarkdownLocation::new(20, 0)));
        history.visit(MarkdownLocation::new(25, 4));
        assert_eq!(history.forward(), Some(MarkdownLocation::new(25, 4)));
        assert_eq!(history.current(), Some(MarkdownLocation::new(25, 4)));
    }

    #[test]
    fn history_evicts_only_the_oldest_entry_at_its_fixed_capacity() {
        let mut history = MarkdownHistory::with_capacity(3);
        for anchor in [10, 20, 30, 40] {
            history.visit(MarkdownLocation::new(anchor, 0));
        }
        assert_eq!(
            history.entries,
            [20, 30, 40].map(|anchor| MarkdownLocation::new(anchor, 0))
        );
        assert_eq!(history.current(), Some(MarkdownLocation::new(40, 0)));
        assert_eq!(history.back(), Some(MarkdownLocation::new(30, 0)));
        assert_eq!(history.back(), Some(MarkdownLocation::new(20, 0)));
        assert_eq!(history.forward(), Some(MarkdownLocation::new(30, 0)));
        history.visit(MarkdownLocation::new(35, 2));
        assert_eq!(
            history.entries,
            [
                MarkdownLocation::new(20, 0),
                MarkdownLocation::new(30, 0),
                MarkdownLocation::new(35, 2),
            ]
        );
        assert_eq!(history.current(), Some(MarkdownLocation::new(35, 2)));
        assert!(!history.can_forward());
    }

    struct RealHistory {
        history: MarkdownHistory,
        visits: i64,
        last_visit_was_branch: i64,
        expected_len: i64,
    }

    impl RealHistory {
        fn new() -> Self {
            Self {
                history: MarkdownHistory::with_capacity(3),
                visits: 0,
                last_visit_was_branch: 0,
                expected_len: 0,
            }
        }

        fn project(&self, model: &Model) -> State {
            let mut state = model.init_state();
            state.insert("len", self.history.entries.len() as i64);
            state.insert(
                "cursor",
                self.history.cursor.map_or(0, |cursor| cursor as i64 + 1),
            );
            state.insert("visits", self.visits);
            state.insert("last_visit_was_branch", self.last_visit_was_branch);
            state.insert("expected_len", self.expected_len);
            state
        }

        fn visit(&mut self, anchor: usize) {
            let len = self.history.entries.len();
            let cursor = self.history.cursor.map_or(0, |cursor| cursor + 1);
            self.last_visit_was_branch = i64::from(cursor < len);
            self.expected_len = if cursor == 0 {
                1
            } else if cursor < self.history.capacity {
                cursor + 1
            } else {
                self.history.capacity
            } as i64;
            self.history.visit(MarkdownLocation::new(anchor, 0));
            self.visits += 1;
        }

        fn back(&mut self) {
            assert!(self.history.can_back());
            self.history.back();
            self.last_visit_was_branch = 0;
        }

        fn forward(&mut self) {
            assert!(self.history.can_forward());
            self.history.forward();
            self.last_visit_was_branch = 0;
        }

        fn duplicate(&mut self) {
            let current = self.history.current().expect("non-empty history");
            self.history.visit(current);
            self.last_visit_was_branch = 0;
        }
    }

    fn drive_history(
        model: &Model,
        real: &mut RealHistory,
        action: &'static str,
        operation: impl FnOnce(&mut RealHistory),
    ) -> (State, State) {
        let before = real.project(model);
        operation(real);
        let after = real.project(model);
        assert_eq!(admits(model, &before, &after), Some(action));
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &after),
                "real transition violates {}::{}: {after:?}",
                model.name,
                invariant.name,
            );
        }
        (before, after)
    }

    #[test]
    fn real_markdown_history_conforms_to_derived_capacity_and_branch_model() {
        let model = native_markdown_history_model();
        let mut real = RealHistory::new();
        assert_eq!(real.project(&model), model.init_state());

        drive_history(&model, &mut real, "Visit", |real| real.visit(10));
        drive_history(&model, &mut real, "Visit", |real| real.visit(20));
        drive_history(&model, &mut real, "Visit", |real| real.visit(30));
        drive_history(&model, &mut real, "Back", RealHistory::back);
        drive_history(&model, &mut real, "Back", RealHistory::back);
        let before_branch = real.project(&model);
        drive_history(&model, &mut real, "Visit", |real| real.visit(15));
        assert_eq!(
            real.history.entries,
            [MarkdownLocation::new(10, 0), MarkdownLocation::new(15, 0)]
        );
        drive_history(&model, &mut real, "Visit", |real| real.visit(25));
        let before_full_visit = real.project(&model);
        drive_history(&model, &mut real, "Visit", |real| real.visit(35));
        drive_history(&model, &mut real, "Back", RealHistory::back);
        drive_history(&model, &mut real, "Forward", RealHistory::forward);
        drive_history(&model, &mut real, "Duplicate", RealHistory::duplicate);

        // Negative control 1: an append beyond capacity is neither admitted by
        // the real model nor able to satisfy its named bound.
        let mut unbounded = before_full_visit.clone();
        unbounded.insert("len", 4);
        unbounded.insert("cursor", 4);
        unbounded.insert("visits", before_full_visit["visits"] + 1);
        unbounded.insert("expected_len", 3);
        assert_eq!(admits(&model, &before_full_visit, &unbounded), None);
        assert!(!model.check_invariant("HistoryBounded", &unbounded));

        // Negative control 2: keeping the abandoned future after a branch
        // violates the transition-specific length witness.
        let mut untrimmed = before_branch.clone();
        untrimmed.insert("len", 3);
        untrimmed.insert("cursor", 3);
        untrimmed.insert("visits", before_branch["visits"] + 1);
        untrimmed.insert("last_visit_was_branch", 1);
        untrimmed.insert("expected_len", 2);
        assert_eq!(admits(&model, &before_branch, &untrimmed), None);
        assert!(!model.check_invariant("ForwardBranchTruncated", &untrimmed));
    }

    #[test]
    fn tables_and_semantic_text_are_structured() {
        let document = parse("| A | B |\n|---|:---:|\n| 1 | 2 |\n");
        assert!(matches!(document.blocks[0], MarkdownBlock::Table { .. }));
        assert_eq!(semantic_text(&document), "A\tB\n1\t2");
    }

    #[test]
    fn source_to_block_mapping_survives_edges() {
        let document = parse("# A\n\nBody\n");
        assert_eq!(block_at_source(&document, 0), Some(0));
        assert_eq!(block_at_source(&document, 7), Some(1));
        assert_eq!(block_at_source(&document, 99), Some(1));
    }

    #[test]
    fn virtual_layout_materializes_a_bounded_window_with_source_anchors() {
        let source = (0..5_000)
            .map(|index| format!("Paragraph {index} with enough words to wrap in a reader.\n\n"))
            .collect::<String>();
        let document = parse(&source);
        let middle = document.blocks[2_500].source().start;
        let layout = layout_visible_blocks(&document, middle, 0, 680.0, 720.0);
        assert_eq!(layout.anchor_index, 2_500);
        assert!(
            layout.blocks.len() < 60,
            "layout work stays viewport-bounded"
        );
        assert!(layout.blocks.iter().any(|block| block.index == 2_500));
        assert!(layout.total_height > 100_000.0);
        assert_eq!(
            move_block_anchor(&document, middle, 3),
            document.blocks[2_503].source().start
        );
        assert_eq!(
            move_block_anchor(&document, middle, -3),
            document.blocks[2_497].source().start
        );
    }

    #[test]
    fn thousand_line_fence_is_reachable_by_rows_pages_source_lines_and_end() {
        let source = format!(
            "```text\n{}```\n",
            (0..1_000)
                .map(|line| format!("line-{line:04}\n"))
                .collect::<String>()
        );
        let document = parse(&source);
        assert_eq!(document.blocks.len(), 1);
        let width = 680.0;
        let rows = block_visual_rows(&document.blocks[0], width);
        assert!(rows >= 1_000, "every authored code line remains reachable");

        let start = MarkdownLocation::new(document.blocks[0].source().start, 0);
        let one_row = move_visual_rows(&document, start, width, 1);
        assert_eq!(one_row.visual_row, 1, "wheel motion stays intra-block");

        let mut paged = start;
        for _ in 0..80 {
            paged = move_visual_rows(&document, paged, width, 14);
        }
        assert!(
            paged.visual_row >= 900,
            "page motion reaches the fenced tail"
        );
        let visible = layout_visible_blocks(
            &document,
            paged.source_anchor,
            paged.visual_row,
            width,
            320.0,
        );
        assert_eq!(visible.blocks[0].index, 0);
        assert_eq!(visible.blocks[0].visual_row, paged.visual_row);

        let end = move_visual_rows(&document, start, width, isize::MAX);
        assert_eq!(end.visual_row, rows - 1);
        let back = move_visual_rows(&document, end, width, -1);
        assert_eq!(back.visual_row, rows - 2);

        let source_end = move_source_lines(&document, 0, isize::MAX);
        let source_tail = source_window_from_anchor(&source, source_end, 4_096, 32);
        assert!(source_tail.text.contains("line-0999") || source_tail.text.contains("```"));
        assert!(source_tail.truncated_before);
        assert!(!source_tail.text.is_empty());
    }

    #[test]
    fn real_markdown_row_reducer_conforms_to_exact_viewport_model() {
        let document = parse("```text\na\nb\nc\nd\ne\nf\ng\n```\n");
        let model = native_markdown_viewport_model();
        let mut location = MarkdownLocation::new(document.blocks[0].source().start, 0);
        let mut state = model.init_state();
        for step in 1..=6_i64 {
            let before = state.clone();
            location = move_visual_rows(&document, location, 680.0, 1);
            state.insert("actual_row", location.visual_row as i64);
            state.insert("expected_row", step);
            state.insert("steps", step);
            assert_eq!(admits(&model, &before, &state), Some("Step"));
            assert!(model.check_invariant("ExactIntraBlockProgress", &state));
        }

        // Negative control: the old block-only reducer treated one wheel row as
        // a complete four-row block. It is neither admitted by the shipping
        // model nor able to satisfy the exact-progress invariant.
        let before = model.init_state();
        let mut skipped = before.clone();
        skipped.insert("actual_row", 4);
        skipped.insert("expected_row", 1);
        skipped.insert("steps", 1);
        assert_eq!(admits(&model, &before, &skipped), None);
        assert!(!model.check_invariant("ExactIntraBlockProgress", &skipped));
    }

    #[test]
    fn page_targets_remain_symmetric_at_the_short_final_page() {
        let source = (0..24)
            .map(|index| format!("Paragraph {index} with enough words for one row.\n\n"))
            .collect::<String>();
        let document = parse(&source);
        let first = document.blocks[0].source().start;
        let forward = page_target_block(&document, first, 680.0, 420.0, 1);
        assert!(forward > 1);
        let final_index = document.blocks.len() - 1;
        let final_anchor = document.blocks[final_index].source().start;
        let backward = page_target_block(&document, final_anchor, 680.0, 420.0, -1);
        assert!(
            final_index.saturating_sub(backward) > 1,
            "a short remaining forward page must not collapse the backward page"
        );
        assert_eq!(page_target_block(&document, first, 680.0, 420.0, -1), 0);
        assert_eq!(
            page_target_block(&document, final_anchor, 680.0, 420.0, 1),
            final_index
        );
    }
}
