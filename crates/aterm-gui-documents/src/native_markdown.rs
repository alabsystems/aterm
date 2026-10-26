// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic, non-web Markdown document model for native tab views.

use std::{borrow::Cow, ops::Range};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownDocument {
    pub blocks: Vec<MarkdownBlock>,
    pub outline: Vec<HeadingRef>,
    pub links: Vec<MarkdownLink>,
    pub images: Vec<MarkdownImage>,
    /// Ordered semantic inline annotations.
    pub inline_runs: Vec<MarkdownInlineRun>,
    /// Styled spans of each prose block's display text (heading, paragraph, list item, quote).
    block_styles: std::collections::BTreeMap<usize, Vec<(Range<usize>, InlineStyle)>>,
    anchors: std::collections::BTreeMap<String, usize>,
    /// Exact UTF-8 byte length of the canonical source used to build this projection.
    pub source_len: usize,
    /// Canonical UTF-8 byte offset of every physical source line.
    pub source_line_starts: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownBlock {
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
    #[inline]
    pub const fn source(&self) -> &Range<usize> {
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
pub struct HeadingRef {
    pub level: u8,
    pub id: String,
    pub text: String,
    pub source_start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownLink {
    pub label: String,
    pub destination: String,
    pub source: Range<usize>,
    pub policy: LinkPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownImage {
    pub alt: Option<String>,
    pub source_uri: String,
    pub source: Range<usize>,
    pub remote: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownInlineRun {
    pub kind: MarkdownInlineKind,
    pub text: String,
    /// Complete authored syntax, including delimiters/destination where one exists.
    pub source: Range<usize>,
    /// Source bytes that produced the visible text.
    pub content_source: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownInlineKind {
    Emphasis,
    Strong,
    Strikethrough,
    Code,
    Link { index: usize },
    Image { index: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(test, debug_assertions))]
pub struct MarkdownSourceSegment {
    pub display: Range<usize>,
    pub source: Range<usize>,
    pub inline: Option<MarkdownInlineKind>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg(any(test, debug_assertions))]
pub struct MarkdownSemanticProjection {
    pub text: String,
    pub source_map: Vec<MarkdownSourceSegment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkPolicy {
    LocalAnchor,
    LocalDocument,
    ExplicitExternal,
    DeniedScheme,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownImageAction {
    SelectLocalSource { range: Range<usize> },
    OpenRemote { uri: String },
    Denied { message: &'static str },
}

/// Pure image-policy reducer.
pub fn reduce_image_action(image: &MarkdownImage, user_initiated: bool) -> MarkdownImageAction {
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

/// One exact, renderer-independent reading position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkdownLocation {
    pub source_anchor: usize,
    pub visual_row: usize,
}

impl MarkdownLocation {
    #[inline]
    pub const fn new(source_anchor: usize, visual_row: usize) -> Self {
        Self {
            source_anchor,
            visual_row,
        }
    }
}

/// Per-view history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownHistory {
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
    pub fn visit(&mut self, location: MarkdownLocation) {
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

    pub fn back(&mut self) -> Option<MarkdownLocation> {
        let cursor = self.cursor?;
        if cursor == 0 {
            return self.entries.first().copied();
        }
        self.cursor = Some(cursor - 1);
        self.entries.get(cursor - 1).copied()
    }

    pub fn forward(&mut self) -> Option<MarkdownLocation> {
        let cursor = self.cursor?;
        let next = cursor.saturating_add(1);
        if next >= self.entries.len() {
            return self.entries.get(cursor).copied();
        }
        self.cursor = Some(next);
        self.entries.get(next).copied()
    }

    #[inline]
    pub fn current(&self) -> Option<MarkdownLocation> {
        self.cursor
            .and_then(|cursor| self.entries.get(cursor))
            .copied()
    }

    #[inline]
    pub fn can_back(&self) -> bool {
        self.cursor.is_some_and(|cursor| cursor > 0)
    }

    #[inline]
    pub fn can_forward(&self) -> bool {
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

pub fn parse(source: &str) -> MarkdownDocument {
    let presentation_source = mask_html_comments(source);
    let display_source = presentation_source.as_ref();
    let lines = source_lines(display_source);
    let mut blocks = Vec::new();
    let mut outline = Vec::new();
    let mut links = Vec::new();
    let mut images = Vec::new();
    let mut inline_runs = Vec::new();
    let mut block_styles = std::collections::BTreeMap::new();
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
            let (text, styles) = lower_inline_text_styled(text);
            if !styles.is_empty() {
                block_styles.insert(blocks.len(), styles);
            }
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
            let (text, styles) = lower_inline_text_styled(text);
            if !styles.is_empty() {
                block_styles.insert(blocks.len(), styles);
            }
            blocks.push(MarkdownBlock::ListItem {
                depth,
                ordinal,
                text,
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
            let (text, styles) = lower_inline_text_styled(&quote);
            if !styles.is_empty() {
                block_styles.insert(blocks.len(), styles);
            }
            blocks.push(MarkdownBlock::Quote {
                text,
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
        let (text, styles) = lower_inline_text_styled(&paragraph);
        if !styles.is_empty() {
            block_styles.insert(blocks.len(), styles);
        }
        blocks.push(MarkdownBlock::Paragraph {
            text,
            source: start..end,
        });
    }

    MarkdownDocument {
        blocks,
        outline,
        links,
        images,
        inline_runs,
        block_styles,
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
#[cfg(any(test, debug_assertions))]
pub fn semantic_text(document: &MarkdownDocument) -> String {
    semantic_projection(document).text
}

/// Build the bounded semantic reading projection and its stable source map.
#[cfg(any(test, debug_assertions))]
pub fn semantic_projection(document: &MarkdownDocument) -> MarkdownSemanticProjection {
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

/// How one span of a block's display text is styled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InlineStyle {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
}

impl InlineStyle {
    #[inline]
    const fn is_plain(self) -> bool {
        !self.strong && !self.emphasis && !self.strike && !self.code && !self.link
    }

    #[inline]
    const fn merged(self, other: Self) -> Self {
        Self {
            strong: self.strong || other.strong,
            emphasis: self.emphasis || other.emphasis,
            strike: self.strike || other.strike,
            code: self.code || other.code,
            link: self.link || other.link,
        }
    }
}

impl MarkdownDocument {
    /// The styled spans of block `index`'s display text, clipped to its first `len` bytes.
    pub fn block_styles(&self, index: usize, len: usize) -> Vec<(Range<usize>, InlineStyle)> {
        self.block_styles
            .get(&index)
            .map(|spans| {
                spans
                    .iter()
                    .filter(|(span, _)| span.start < len)
                    .map(|(span, style)| (span.start..span.end.min(len), *style))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(any(test, debug_assertions))]
pub fn inline_runs_in_range(
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

#[inline]
pub fn block_at_source(document: &MarkdownDocument, source_offset: usize) -> Option<usize> {
    document
        .blocks
        .partition_point(|block| block.source().start <= source_offset)
        .checked_sub(1)
}

/// Heading owning the current source anchor.
#[inline]
pub fn heading_at_source(document: &MarkdownDocument, source_offset: usize) -> Option<usize> {
    let next = document
        .outline
        .partition_point(|heading| heading.source_start <= source_offset);
    next.checked_sub(1)
}

/// Resolve a parsed local `#anchor` without interpreting a path or touching the filesystem.
#[inline]
pub fn local_anchor(document: &MarkdownDocument, destination: &str) -> Option<usize> {
    let id = destination.strip_prefix('#')?;
    if id.len() > 512 {
        return None;
    }
    document.anchors.get(id).copied()
}

/// Produce a bounded, active-centered outline window.
pub fn outline_window(
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
pub struct VisibleBlock {
    pub index: usize,
    pub y: f32,
    pub height: f32,
    pub source: Range<usize>,
    pub visual_row: usize,
    pub total_visual_rows: usize,
}

/// Exterior rhythm between reader cards/runs.
pub const VISUAL_BLOCK_GAP: f32 = 4.0;

/// Bounded block layout around a stable source anchor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VirtualBlockLayout {
    pub blocks: Vec<VisibleBlock>,
    pub total_height: f32,
    pub anchor_index: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkdownSourceWindow {
    pub text: String,
    pub source: Range<usize>,
    pub truncated_before: bool,
    pub truncated_after: bool,
}

/// Source-reader window whose first row is the exact requested source line.
pub fn source_window_from_anchor(
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

/// The type steps the reader paints with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderStep {
    Display,
    Title,
    Body,
    Caption,
}

/// The reader's type at the current text size: the pixel size the window paints each
/// [`ReaderStep`] at, and the text scale the reader's insets grow with. The window builds it
/// from its own type scale, so the layout, the row navigation, the painter and the paint audit
/// all measure a block with one set of numbers ([`block_frame`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReaderType {
    /// The text scale, 1.0 at the default size.
    pub scale: f32,
    pub display: f32,
    pub title: f32,
    pub body: f32,
    pub caption: f32,
}

impl ReaderType {
    /// The pixel size of `step`.
    #[must_use]
    pub const fn px(&self, step: ReaderStep) -> f32 {
        match step {
            ReaderStep::Display => self.display,
            ReaderStep::Title => self.title,
            ReaderStep::Body => self.body,
            ReaderStep::Caption => self.caption,
        }
    }

    /// The text scale, or 1.0 where it is not a usable size.
    #[must_use]
    pub fn scale(&self) -> f32 {
        if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        }
    }
}

/// What decides a block's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockShape {
    Heading(u8),
    Paragraph,
    ListItem {
        depth: usize,
    },
    Quote,
    /// A fenced block: `labelled` when it names a language, which paints above its text;
    /// `dense` in the narrow source pane.
    Code {
        labelled: bool,
        dense: bool,
    },
    Table,
    Rule,
}

impl BlockShape {
    /// The shape the reader's preview gives `block`.
    #[must_use]
    pub fn of(block: &MarkdownBlock) -> Self {
        match block {
            MarkdownBlock::Heading { level, .. } => Self::Heading(*level),
            MarkdownBlock::Paragraph { .. } => Self::Paragraph,
            MarkdownBlock::ListItem { depth, .. } => Self::ListItem { depth: *depth },
            MarkdownBlock::Quote { .. } => Self::Quote,
            MarkdownBlock::CodeBlock { language, .. } => Self::Code {
                labelled: language
                    .as_deref()
                    .is_some_and(|language| !language.is_empty()),
                dense: false,
            },
            MarkdownBlock::Table { .. } => Self::Table,
            MarkdownBlock::ThematicBreak { .. } => Self::Rule,
        }
    }
}

/// Where one block's text sits in its rectangle and how it wraps: the one geometry the
/// reader's layout, its row navigation, the painter and the paint audit all read, so the rows
/// a block is laid out with are the rows it paints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockFrame {
    pub step: ReaderStep,
    pub px: f32,
    pub line_h: f32,
    pub mono: bool,
    pub bold: bool,
    /// Source lines stay rows (code, tables); prose re-flows.
    pub preserve_lines: bool,
    pub inset_left: f32,
    pub inset_right: f32,
    pub inset_top: f32,
    pub inset_bottom: f32,
    /// The height of the language caption a labelled code block paints above its text.
    pub label_h: f32,
}

impl BlockFrame {
    /// The width the text wraps in, in a block `width` wide.
    #[must_use]
    pub fn text_width(&self, width: f32) -> f32 {
        (width - self.inset_left - self.inset_right).max(20.0)
    }

    /// The average advance a column is budgeted at.
    #[must_use]
    pub fn advance(&self) -> f32 {
        self.px * if self.mono { 0.62 } else { 0.55 }
    }

    /// The columns a row holds in a block `width` wide; a wide char takes two
    /// ([`reader_columns`]).
    #[must_use]
    pub fn columns(&self, width: f32) -> usize {
        (self.text_width(width) / self.advance().max(1.0))
            .floor()
            .max(4.0) as usize
    }

    /// The height of the block's rectangle with `rows` rows of text.
    #[must_use]
    pub fn height(&self, rows: usize) -> f32 {
        self.inset_top + rows as f32 * self.line_h + self.inset_bottom
    }

    /// The whole rows of text a rectangle `height` tall holds (at least one): the rows the
    /// painter draws in it, none cut by its edge.
    #[must_use]
    pub fn rows_in(&self, height: f32) -> usize {
        ((height - self.inset_top + 0.01) / self.line_h)
            .floor()
            .max(1.0) as usize
    }
}

/// The frame of a block of `shape` at `ty`; `None` for a rule, which has no text.
#[must_use]
pub fn block_frame(shape: BlockShape, ty: ReaderType) -> Option<BlockFrame> {
    let scale = ty.scale();
    let frame = |step: ReaderStep, mono: bool, bold: bool, preserve_lines: bool, leading: f32| {
        let px = ty.px(step);
        BlockFrame {
            step,
            px,
            line_h: (px * leading).max(16.0),
            mono,
            bold,
            preserve_lines,
            inset_left: 0.0,
            inset_right: 0.0,
            inset_top: 0.0,
            inset_bottom: 0.0,
            label_h: 0.0,
        }
    };
    Some(match shape {
        BlockShape::Heading(1) => BlockFrame {
            inset_bottom: 16.0 * scale,
            ..frame(ReaderStep::Display, false, true, false, 1.32)
        },
        BlockShape::Heading(2) => BlockFrame {
            inset_bottom: 14.0 * scale,
            ..frame(ReaderStep::Title, false, true, false, 1.35)
        },
        BlockShape::Heading(_) => BlockFrame {
            inset_bottom: 14.0 * scale,
            ..frame(ReaderStep::Body, false, true, false, 1.45)
        },
        BlockShape::Paragraph => frame(ReaderStep::Body, false, false, false, 1.72),
        BlockShape::ListItem { depth } => BlockFrame {
            inset_left: (28.0 + (depth as f32 * 16.0).min(64.0)) * scale,
            ..frame(ReaderStep::Body, false, false, false, 1.66)
        },
        BlockShape::Quote => BlockFrame {
            inset_left: 18.0 * scale,
            inset_right: 12.0 * scale,
            inset_top: 8.0 * scale,
            inset_bottom: 8.0 * scale,
            ..frame(ReaderStep::Body, false, false, false, 1.66)
        },
        BlockShape::Code { labelled, dense } => {
            let label_h = if labelled {
                (ty.caption * 1.45).max(16.0)
            } else {
                0.0
            };
            let step = if dense {
                ReaderStep::Caption
            } else {
                ReaderStep::Body
            };
            BlockFrame {
                inset_left: 12.0 * scale,
                inset_right: 12.0 * scale,
                inset_top: if labelled {
                    5.0 * scale + label_h + 3.0 * scale
                } else {
                    10.0 * scale
                },
                inset_bottom: 10.0 * scale,
                label_h,
                ..frame(step, true, false, true, 1.55)
            }
        }
        BlockShape::Table => BlockFrame {
            inset_left: 12.0 * scale,
            inset_right: 12.0 * scale,
            inset_top: 8.0 * scale,
            inset_bottom: 8.0 * scale,
            ..frame(ReaderStep::Body, true, false, true, 1.65)
        },
        BlockShape::Rule => return None,
    })
}

/// The height a rule's rectangle takes.
#[must_use]
pub fn rule_height(ty: ReaderType) -> f32 {
    24.0 * ty.scale()
}

/// One block's measure at a width: its rows of text, the height of one, and the height it
/// takes in the reader's column (its rectangle and the gap after it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockMeasure {
    pub rows: usize,
    pub row_h: f32,
    pub height: f32,
}

/// `block`'s measure in a column `width` wide at `ty`. Its rows are the painter's own wrap of
/// the text the block paints ([`block_paint_text`], [`wrap_reader_text`]), so every row it
/// lays out is a row the painter draws and every row the painter draws can be scrolled to.
#[must_use]
pub fn measure_block(block: &MarkdownBlock, width: f32, ty: ReaderType) -> BlockMeasure {
    let Some(frame) = block_frame(BlockShape::of(block), ty) else {
        let row_h = rule_height(ty);
        return BlockMeasure {
            rows: 1,
            row_h,
            height: row_h + VISUAL_BLOCK_GAP,
        };
    };
    let rows = reader_rows(
        &block_paint_text(block),
        frame.columns(width),
        frame.preserve_lines,
    )
    .max(1);
    BlockMeasure {
        rows,
        row_h: frame.line_h,
        height: frame.height(rows) + VISUAL_BLOCK_GAP,
    }
}

/// The most bytes of a block the reader lays out and paints.
pub const BLOCK_PAINT_LIMIT: usize = 128 * 1024;

/// The text a block paints: its own text, a table's rows joined by `│`, at most
/// [`BLOCK_PAINT_LIMIT`] bytes with a closing `…` where it was cut.
#[must_use]
pub fn block_paint_text(block: &MarkdownBlock) -> Cow<'_, str> {
    match block {
        MarkdownBlock::Heading { text, .. }
        | MarkdownBlock::Paragraph { text, .. }
        | MarkdownBlock::ListItem { text, .. }
        | MarkdownBlock::Quote { text, .. }
        | MarkdownBlock::CodeBlock { code: text, .. } => bounded_text(text, BLOCK_PAINT_LIMIT),
        MarkdownBlock::ThematicBreak { .. } => Cow::Borrowed(""),
        MarkdownBlock::Table { header, rows, .. } => {
            let mut output = String::new();
            for row in std::iter::once(header).chain(rows) {
                for (index, cell) in row.iter().enumerate() {
                    if index > 0 {
                        output.push_str("  │  ");
                    }
                    let remaining = BLOCK_PAINT_LIMIT.saturating_sub(output.len());
                    if remaining == 0 {
                        break;
                    }
                    output.push_str(&bounded_text(cell, remaining));
                }
                if output.len() >= BLOCK_PAINT_LIMIT {
                    break;
                }
                output.push('\n');
            }
            Cow::Owned(output)
        }
    }
}

/// `text`, cut to at most `limit` bytes at a char boundary with a closing `…` where it was.
#[must_use]
pub fn bounded_text(text: &str, limit: usize) -> Cow<'_, str> {
    if text.len() <= limit {
        return Cow::Borrowed(text);
    }
    let end = text.floor_char_boundary(limit.saturating_sub('…'.len_utf8()));
    let mut output = text[..end].to_string();
    output.push('…');
    Cow::Owned(output)
}

/// Lay out only the viewport plus a bounded lookahead, each block at its measure
/// ([`measure_block`]) in a column `viewport_width` wide at `ty`.
pub fn layout_visible_blocks(
    document: &MarkdownDocument,
    source_anchor: usize,
    visual_row: usize,
    viewport_width: f32,
    viewport_height: f32,
    ty: ReaderType,
) -> VirtualBlockLayout {
    if document.blocks.is_empty() || viewport_width <= 0.0 || viewport_height <= 0.0 {
        return VirtualBlockLayout::default();
    }
    let anchor_index = block_at_source(document, source_anchor).unwrap_or(0);
    let anchor = measure_block(&document.blocks[anchor_index], viewport_width, ty);
    let visual_row = visual_row.min(anchor.rows.saturating_sub(1));
    let mut y = 0.0;
    let limit = viewport_height + 320.0 * ty.scale();
    let mut blocks = Vec::new();
    let mut measured_height = 0.0;
    for (index, block) in document.blocks.iter().enumerate().skip(anchor_index) {
        if y >= limit {
            break;
        }
        let (measure, row) = if index == anchor_index {
            (anchor, visual_row)
        } else {
            (measure_block(block, viewport_width, ty), 0)
        };
        let height = (measure.height - row as f32 * measure.row_h).max(VISUAL_BLOCK_GAP + 1.0);
        blocks.push(VisibleBlock {
            index,
            y,
            height,
            source: block.source().clone(),
            visual_row: row,
            total_visual_rows: measure.rows,
        });
        measured_height += measure.height;
        y += height;
    }
    let total_height = measured_height / blocks.len().max(1) as f32 * document.blocks.len() as f32;
    VirtualBlockLayout {
        blocks,
        total_height,
        anchor_index,
    }
}

/// HOW FAR A READER AT `source_anchor` / `visual_row` HAS SHOWN THE DOCUMENT, as a source
/// offset: through the last block whose bottom is inside a viewport `viewport_width` by
/// `viewport_height` at `ty`, or into the block the viewport ends in by the share of its
/// rows shown; the whole document once its last block is shown whole. The reader's
/// progress counts this, so the end of a document reads 100% however far its last row is
/// from the top.
#[must_use]
pub fn source_shown_through(
    document: &MarkdownDocument,
    source_anchor: usize,
    visual_row: usize,
    viewport_width: f32,
    viewport_height: f32,
    ty: ReaderType,
) -> usize {
    let layout = layout_visible_blocks(
        document,
        source_anchor,
        visual_row,
        viewport_width,
        viewport_height,
        ty,
    );
    let mut shown = source_anchor.min(document.source_len);
    for block in &layout.blocks {
        if block.y >= viewport_height {
            break;
        }
        if block.y + block.height <= viewport_height + 0.5 {
            shown = if block.index + 1 == document.blocks.len() {
                document.source_len
            } else {
                block.source.end
            };
            continue;
        }
        let measure = measure_block(&document.blocks[block.index], viewport_width, ty);
        let inset = (measure.height - measure.rows as f32 * measure.row_h).max(0.0) / 2.0;
        let rows = ((viewport_height - block.y - inset).max(0.0) / measure.row_h.max(1.0)) as usize;
        let through = block
            .visual_row
            .saturating_add(rows)
            .min(block.total_visual_rows);
        let into = block.source.len() * through / block.total_visual_rows.max(1);
        shown = shown.max(block.source.start + into);
        break;
    }
    shown.min(document.source_len)
}

/// [`source_shown_through`] for the source pane, which shows `rows` source lines from the
/// line at `source_anchor`.
#[must_use]
pub fn source_lines_shown_through(
    document: &MarkdownDocument,
    source_anchor: usize,
    rows: usize,
) -> usize {
    let current = document
        .source_line_starts
        .partition_point(|start| *start <= source_anchor)
        .saturating_sub(1);
    document
        .source_line_starts
        .get(current.saturating_add(rows.max(1)))
        .copied()
        .unwrap_or(document.source_len)
        .min(document.source_len)
}

/// Number of reader rows in one block in a column `viewport_width` wide at `ty`.
#[inline]
#[must_use]
pub fn block_visual_rows(block: &MarkdownBlock, viewport_width: f32, ty: ReaderType) -> usize {
    measure_block(block, viewport_width, ty).rows
}

/// Move by exact visual rows.
pub fn move_visual_rows(
    document: &MarkdownDocument,
    location: MarkdownLocation,
    viewport_width: f32,
    ty: ReaderType,
    delta: isize,
) -> MarkdownLocation {
    if document.blocks.is_empty() {
        return MarkdownLocation::default();
    }
    let mut measured: Option<(usize, usize)> = None;
    let mut visual_rows_of = |index: usize| -> usize {
        if let Some((cached_index, rows)) = measured
            && cached_index == index
        {
            return rows;
        }
        let rows = block_visual_rows(&document.blocks[index], viewport_width, ty);
        measured = Some((index, rows));
        rows
    };
    let mut block = block_at_source(document, location.source_anchor).unwrap_or(0);
    let mut row = location
        .visual_row
        .min(visual_rows_of(block).saturating_sub(1));
    if delta == isize::MIN {
        return MarkdownLocation::new(document.blocks[0].source().start, 0);
    }
    if delta == isize::MAX {
        block = document.blocks.len() - 1;
        row = visual_rows_of(block).saturating_sub(1);
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
                row = visual_rows_of(block).saturating_sub(1);
                remaining -= 1;
            } else {
                break;
            }
        } else {
            let last = visual_rows_of(block).saturating_sub(1);
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

/// The location whole rows away that fill at most `distance` logical pixels (at least one
/// row), forward for a positive `direction` and back for a negative one; each row counts its
/// own block's row height, and leaving a block its insets and gap. A page is
/// [`move_visual_distance`] by most of the viewport's height.
pub fn move_visual_distance(
    document: &MarkdownDocument,
    location: MarkdownLocation,
    viewport_width: f32,
    ty: ReaderType,
    distance: f32,
    direction: i32,
) -> MarkdownLocation {
    if document.blocks.is_empty() || direction == 0 {
        return location;
    }
    let mut block = block_at_source(document, location.source_anchor).unwrap_or(0);
    let mut measure = measure_block(&document.blocks[block], viewport_width, ty);
    let mut row = location.visual_row.min(measure.rows.saturating_sub(1));
    let start = (block, row);
    let mut used = 0.0_f32;
    loop {
        let chrome = (measure.height - measure.rows as f32 * measure.row_h).max(0.0);
        let (next, cost) = if direction > 0 {
            if row + 1 < measure.rows {
                ((block, row + 1, None), measure.row_h)
            } else if block + 1 < document.blocks.len() {
                let next = measure_block(&document.blocks[block + 1], viewport_width, ty);
                ((block + 1, 0, Some(next)), measure.row_h + chrome)
            } else {
                break;
            }
        } else if row > 0 {
            ((block, row - 1, None), measure.row_h)
        } else if block > 0 {
            let previous = measure_block(&document.blocks[block - 1], viewport_width, ty);
            let previous_chrome =
                (previous.height - previous.rows as f32 * previous.row_h).max(0.0);
            (
                (block - 1, previous.rows.saturating_sub(1), Some(previous)),
                previous.row_h + previous_chrome,
            )
        } else {
            break;
        };
        if (block, row) != start && used + cost > distance {
            break;
        }
        used += cost;
        (block, row) = (next.0, next.1);
        if let Some(next) = next.2 {
            measure = next;
        }
    }
    if (block, row) == start {
        return location;
    }
    MarkdownLocation::new(source_anchor_for_visual_row(document, block, row), row)
}

/// Move an exact number of physical source lines using the parser-owned line index.
pub fn move_source_lines(document: &MarkdownDocument, source_anchor: usize, delta: isize) -> usize {
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
#[cfg(any(test, debug_assertions))]
pub fn move_block_anchor(document: &MarkdownDocument, source_anchor: usize, delta: isize) -> usize {
    let Some(current) = block_at_source(document, source_anchor) else {
        return 0;
    };
    let target = current
        .saturating_add_signed(delta)
        .min(document.blocks.len().saturating_sub(1));
    document.blocks[target].source().start
}

/// Columns a character takes in the reader's wrap and block-row estimate.
#[must_use]
#[inline]
pub fn reader_columns(c: char) -> usize {
    aterm_grapheme::char_width(c).max(1)
}

/// The most rows the reader wraps one block into.
pub const READER_ROW_LIMIT: usize = 131_072;

/// `text` wrapped to rows of `max_columns` columns ([`reader_columns`]) the way the reader
/// paints it: prose at word (and ideographic) breaks, `preserve_lines` text (code, tables) at
/// its own lines first. Rows before `skip` are left out; at most `max_lines` rows are made,
/// the last ending in `…` where text was left over.
#[must_use]
pub fn wrap_reader_text(
    text: &str,
    max_columns: usize,
    skip: usize,
    max_lines: usize,
    preserve_lines: bool,
) -> Vec<String> {
    let mut output = Vec::with_capacity(
        max_lines
            .clamp(1, READER_ROW_LIMIT)
            .saturating_sub(skip)
            .min(16),
    );
    wrap_rows(
        text,
        max_columns,
        skip,
        max_lines,
        preserve_lines,
        Some(&mut output),
    );
    output
}

/// How many rows [`wrap_reader_text`] makes of `text`, without making them.
#[must_use]
pub fn reader_rows(text: &str, max_columns: usize, preserve_lines: bool) -> usize {
    wrap_rows(
        text,
        max_columns,
        usize::MAX,
        READER_ROW_LIMIT,
        preserve_lines,
        None,
    )
}

/// One wrapped row into `output`, made only when there is an `output` to keep it.
#[inline]
fn keep(output: &mut Option<&mut Vec<String>>, row: impl FnOnce() -> String) {
    if let Some(output) = output.as_deref_mut() {
        output.push(row());
    }
}

/// The wrap behind [`wrap_reader_text`] and [`reader_rows`]: the rows it made, kept in
/// `output` when there is one.
fn wrap_rows(
    text: &str,
    max_columns: usize,
    skip: usize,
    max_lines: usize,
    preserve_lines: bool,
    mut output: Option<&mut Vec<String>>,
) -> usize {
    use reader_columns as columns;
    let max_columns = max_columns.max(1);
    let max_lines = max_lines.clamp(1, READER_ROW_LIMIT);
    let mut produced = 0usize;
    let mut produced_chars = 0usize;
    let mut characters: Vec<char> = Vec::new();
    let mut cjk_starts: Vec<usize> = Vec::new();
    let mut words: Vec<(bool, &str)> = Vec::new();
    for source_line in text.lines().chain(text.is_empty().then_some("")) {
        if produced >= max_lines {
            break;
        }
        if preserve_lines {
            characters.clear();
            characters.extend(source_line.chars());
            cjk_starts.clear();
            if !source_line.is_ascii() {
                let mut breaks = aterm_grapheme::line_breaks(source_line)
                    .into_iter()
                    .filter(|brk| brk.kind == aterm_grapheme::BreakKind::Ideographic)
                    .map(|brk| brk.at)
                    .peekable();
                for (index, (byte, _)) in source_line.char_indices().enumerate() {
                    if breaks.next_if_eq(&byte).is_some() {
                        cjk_starts.push(index);
                    }
                }
            }
            if characters.is_empty() {
                if produced >= skip {
                    keep(&mut output, String::new);
                }
                produced += 1;
            } else {
                let mut start = 0usize;
                while start < characters.len() {
                    if produced >= max_lines {
                        break;
                    }
                    let mut hard_end = start;
                    let mut used = 0usize;
                    while let Some(&character) = characters.get(hard_end) {
                        let width = columns(character);
                        if hard_end > start && used + width > max_columns {
                            break;
                        }
                        used += width;
                        hard_end += 1;
                    }
                    if hard_end == characters.len() {
                        if produced >= skip {
                            keep(&mut output, || characters[start..hard_end].iter().collect());
                        }
                        produced += 1;
                        produced_chars += hard_end - start;
                        break;
                    }
                    let soft_end = characters[start..=hard_end]
                        .iter()
                        .rposition(|character| character.is_whitespace())
                        .map(|offset| start + offset)
                        .filter(|end| *end > start);
                    let cjk_end = cjk_starts[..cjk_starts.partition_point(|at| *at <= hard_end)]
                        .last()
                        .copied()
                        .filter(|end| *end > start);
                    let end = soft_end.max(cjk_end).unwrap_or(hard_end);
                    if produced >= skip {
                        keep(&mut output, || characters[start..end].iter().collect());
                    }
                    produced += 1;
                    produced_chars += end - start;
                    start = end;
                    while start < characters.len() && characters[start].is_whitespace() {
                        start += 1;
                    }
                }
            }
            continue;
        }
        words.clear();
        if source_line.is_ascii() {
            words.extend(source_line.split_whitespace().map(|word| (true, word)));
        } else {
            words.extend(
                aterm_grapheme::pieces(source_line, aterm_grapheme::BreakKind::is_word_boundary)
                    .into_iter()
                    .map(|piece| (piece.spaced, piece.text)),
            );
        }
        let mut line = String::new();
        let mut line_columns = 0usize;
        let mut line_chars = 0usize;
        for &(spaced, word) in &words {
            let separator = usize::from(!line.is_empty() && spaced);
            let word_columns: usize = word.chars().map(columns).sum();
            if line_columns + separator + word_columns <= max_columns {
                if separator == 1 {
                    line.push(' ');
                }
                line.push_str(word);
                line_columns += separator + word_columns;
                line_chars += separator + word.chars().count();
                continue;
            }
            if !line.is_empty() {
                let row = std::mem::take(&mut line);
                if produced >= skip {
                    keep(&mut output, || row);
                }
                produced += 1;
                produced_chars += line_chars;
                if produced >= max_lines {
                    break;
                }
            }
            let mut chunk = String::new();
            let mut chunk_columns = 0usize;
            let mut chunk_chars = 0usize;
            for character in word.chars() {
                let width = columns(character);
                if chunk_columns + width > max_columns && !chunk.is_empty() {
                    let row = std::mem::take(&mut chunk);
                    if produced >= skip {
                        keep(&mut output, || row);
                    }
                    produced += 1;
                    produced_chars += chunk_chars;
                    chunk_columns = 0;
                    chunk_chars = 0;
                    if produced >= max_lines {
                        break;
                    }
                }
                chunk.push(character);
                chunk_columns += width;
                chunk_chars += 1;
                if chunk_columns >= max_columns {
                    let row = std::mem::take(&mut chunk);
                    if produced >= skip {
                        keep(&mut output, || row);
                    }
                    produced += 1;
                    produced_chars += chunk_chars;
                    chunk_columns = 0;
                    chunk_chars = 0;
                    if produced >= max_lines {
                        break;
                    }
                }
            }
            line = chunk;
            line_columns = chunk_columns;
            line_chars = chunk_chars;
        }
        if produced >= max_lines {
            break;
        }
        if !line.is_empty() || source_line.is_empty() {
            if produced >= skip {
                keep(&mut output, || line);
            }
            produced += 1;
            produced_chars += line_chars;
        }
    }
    if produced == max_lines
        && text.chars().count() > produced_chars
        && let Some(last) = output.and_then(|output| output.last_mut())
    {
        let keep = max_columns.saturating_sub(1);
        let mut used = 0usize;
        *last = last
            .chars()
            .take_while(|character| {
                used += columns(*character);
                used <= keep
            })
            .collect::<String>();
        last.push('…');
    }
    produced
}

/// The `max_lines` rows of [`wrap_reader_text`] from `visual_row`, as a block shows them: a
/// row the next rows continue is shown whole, never ended in `…` (the rest is a scroll
/// away).
pub fn wrap_reader_text_window(
    text: &str,
    max_columns: usize,
    visual_row: usize,
    max_lines: usize,
    preserve_lines: bool,
) -> Vec<String> {
    let take = max_lines.clamp(1, 512);
    let requested = visual_row
        .saturating_add(take)
        .saturating_add(1)
        .clamp(1, READER_ROW_LIMIT);
    let mut lines = wrap_reader_text(text, max_columns, visual_row, requested, preserve_lines);
    lines.truncate(take);
    lines
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

#[inline]
fn find_ascii(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|candidate| candidate == needle)
}

#[inline]
fn ascii_run(bytes: &[u8], start: usize, marker: u8) -> usize {
    bytes[start..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count()
}

#[inline]
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
/// canonical source.
fn lower_inline_text(source: &str) -> String {
    normalize_display_whitespace(&lower_inline_segment(source, 0))
}

/// [`lower_inline_text`] plus the styled spans of the text it returns.
pub fn lower_inline_text_styled(source: &str) -> (String, Vec<(Range<usize>, InlineStyle)>) {
    let mut raw = String::with_capacity(source.len());
    let mut spans = Vec::new();
    lower_segment_into(source, 0, InlineStyle::default(), &mut raw, &mut spans);
    let mut text = String::with_capacity(raw.len());
    let mut map = Vec::with_capacity(raw.len() + 1);
    let mut pending_space = false;
    for character in raw.chars() {
        let at = text.len() + usize::from(pending_space && !character.is_whitespace());
        for _ in 0..character.len_utf8() {
            map.push(at);
        }
        if character.is_whitespace() {
            pending_space = !text.is_empty();
        } else {
            if pending_space {
                text.push(' ');
                pending_space = false;
            }
            text.push(character);
        }
    }
    map.push(text.len());
    let spans: Vec<(Range<usize>, InlineStyle)> = spans
        .into_iter()
        .map(|(span, style): (Range<usize>, InlineStyle)| (map[span.start]..map[span.end], style))
        .filter(|(span, _)| span.start < span.end)
        .collect();
    let mut cuts: Vec<usize> = spans.iter().flat_map(|(s, _)| [s.start, s.end]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut flat: Vec<(Range<usize>, InlineStyle)> = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let style = spans
            .iter()
            .filter(|(span, _)| span.start <= start && end <= span.end)
            .fold(InlineStyle::default(), |style, (_, s)| style.merged(*s));
        if style.is_plain() || text[start..end].trim().is_empty() && !style.code {
            continue;
        }
        match flat.last_mut() {
            Some((prev, prev_style)) if prev.end == start && *prev_style == style => {
                prev.end = end;
            }
            _ => flat.push((start..end, style)),
        }
    }
    (text, flat)
}

const MAX_INLINE_NESTING: usize = 32;

fn lower_inline_segment(source: &str, depth: usize) -> String {
    let mut out = String::with_capacity(source.len());
    lower_segment_into(
        source,
        depth,
        InlineStyle::default(),
        &mut out,
        &mut Vec::new(),
    );
    out
}

/// The lowering pass, appending to `out`. Each recognized construct records the
/// range of `out` it emitted with its style merged over the enclosing `style`
/// (nested constructs record their own, stronger spans); `lower_inline_text`
/// discards them, `lower_inline_text_styled` keeps them.
fn lower_segment_into(
    source: &str,
    depth: usize,
    style: InlineStyle,
    out: &mut String,
    spans: &mut Vec<(Range<usize>, InlineStyle)>,
) {
    if depth >= MAX_INLINE_NESTING {
        out.push_str(source);
        return;
    }
    let bytes = source.as_bytes();
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
                let start = out.len();
                out.push_str(&code);
                spans.push((
                    start..out.len(),
                    style.merged(InlineStyle {
                        code: true,
                        ..InlineStyle::default()
                    }),
                ));
                index = close + ticks;
                continue;
            }
            out.push_str(&source[index..index + ticks]);
            index += ticks;
            continue;
        }

        let image = bytes[index] == b'!' && bytes.get(index + 1) == Some(&b'[');
        let label_open = if image { index + 1 } else { index };
        if bytes.get(label_open) == Some(&b'[')
            && let Some(label_close) = find_balanced_close(source, label_open, b'[', b']')
            && bytes.get(label_close + 1) == Some(&b'(')
            && let Some(destination_close) =
                find_balanced_close(source, label_close + 1, b'(', b')')
        {
            let inner = if image {
                style
            } else {
                style.merged(InlineStyle {
                    link: true,
                    ..InlineStyle::default()
                })
            };
            let start = out.len();
            lower_segment_into(
                &source[label_open + 1..label_close],
                depth + 1,
                inner,
                out,
                spans,
            );
            if !image {
                spans.push((start..out.len(), inner));
            }
            index = destination_close + 1;
            continue;
        }

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
                let own = if marker == b'~' {
                    InlineStyle {
                        strike: true,
                        ..InlineStyle::default()
                    }
                } else if run >= 2 {
                    InlineStyle {
                        strong: true,
                        emphasis: run == 3,
                        ..InlineStyle::default()
                    }
                } else {
                    InlineStyle {
                        emphasis: true,
                        ..InlineStyle::default()
                    }
                };
                let inner = style.merged(own);
                let start = out.len();
                lower_segment_into(&source[index + run..close], depth + 1, inner, out, spans);
                spans.push((start..out.len(), inner));
                index = close + run;
                continue;
            }
            out.push_str(&source[index..index + run]);
            index += run;
            continue;
        }

        if bytes[index] == b'<'
            && let Some(relative) = source[index + 1..].find('>')
        {
            let close = index + 1 + relative;
            let inside = &source[index + 1..close];
            if is_autolink(inside) {
                let start = out.len();
                out.push_str(inside);
                spans.push((
                    start..out.len(),
                    style.merged(InlineStyle {
                        link: true,
                        ..InlineStyle::default()
                    }),
                ));
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

    /// The reader's type at the default size.
    const TYPE: ReaderType = ReaderType {
        scale: 1.0,
        display: 24.0,
        title: 20.0,
        body: 13.0,
        caption: 11.0,
    };
    use aterm_models_native::{native_markdown_history_model, native_markdown_viewport_model};
    use aterm_spec::derive::Model;
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

    /// The PAINT styling of a block, recorded by the lowering pass that produced
    /// its text: nested constructs merged (`**a *b* c**`'s `b` is strong AND
    /// emphasis; `***both***` is both), a repeated word styled where it was
    /// authored (not at its first occurrence), and the plain text between
    /// constructs left unstyled.
    #[test]
    fn inline_styles_locate_nested_and_repeated_runs_in_the_display_text() {
        let source =
            "foo **foo** *em* ***both*** `code` ~~gone~~ [site](https://x.example) **a *b* c**";
        let document = parse(source);
        let block = &document.blocks[0];
        let MarkdownBlock::Paragraph { text, .. } = block else {
            panic!("one paragraph: {block:?}");
        };
        assert_eq!(text, "foo foo em both code gone site a b c");
        let styles = document.block_styles(0, text.len());
        let styled: Vec<(&str, InlineStyle)> = styles
            .iter()
            .map(|(span, style)| (&text[span.clone()], *style))
            .collect();
        let strong = InlineStyle {
            strong: true,
            ..InlineStyle::default()
        };
        let emphasis = InlineStyle {
            emphasis: true,
            ..InlineStyle::default()
        };
        assert_eq!(
            styled,
            vec![
                ("foo", strong),
                ("em", emphasis),
                (
                    "both",
                    InlineStyle {
                        strong: true,
                        emphasis: true,
                        ..InlineStyle::default()
                    }
                ),
                (
                    "code",
                    InlineStyle {
                        code: true,
                        ..InlineStyle::default()
                    }
                ),
                (
                    "gone",
                    InlineStyle {
                        strike: true,
                        ..InlineStyle::default()
                    }
                ),
                (
                    "site",
                    InlineStyle {
                        link: true,
                        ..InlineStyle::default()
                    }
                ),
                ("a ", strong),
                (
                    "b",
                    InlineStyle {
                        strong: true,
                        emphasis: true,
                        ..InlineStyle::default()
                    }
                ),
                (" c", strong),
            ]
        );
        assert_eq!(
            styles[0].0,
            4..7,
            "the STRONG foo is the second one, where it was authored"
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

        let at_newest = real.project(&model);
        assert!(!real.history.can_forward());
        real.history.forward();
        assert_eq!(
            real.project(&model),
            at_newest,
            "forward at the newest entry"
        );
        let mut past_newest = at_newest.clone();
        past_newest.insert("cursor", at_newest["len"] + 1);
        assert_eq!(admits(&model, &at_newest, &past_newest), None);
        assert!(!model.check_invariant("CursorWithinHistory", &past_newest));

        drive_history(&model, &mut real, "Back", RealHistory::back);
        drive_history(&model, &mut real, "Back", RealHistory::back);
        let at_oldest = real.project(&model);
        assert_eq!(at_oldest["cursor"], 1);
        assert!(!real.history.can_back());
        real.history.back();
        assert_eq!(real.project(&model), at_oldest, "back at the oldest entry");
        let mut off_oldest = at_oldest.clone();
        off_oldest.insert("cursor", 0);
        assert_eq!(admits(&model, &at_oldest, &off_oldest), None);
        assert!(!model.check_invariant("EmptyIffNoCursor", &off_oldest));

        let mut unbounded = before_full_visit.clone();
        unbounded.insert("len", 4);
        unbounded.insert("cursor", 4);
        unbounded.insert("visits", before_full_visit["visits"] + 1);
        unbounded.insert("expected_len", 3);
        assert_eq!(admits(&model, &before_full_visit, &unbounded), None);
        assert!(!model.check_invariant("HistoryBounded", &unbounded));

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
        let layout = layout_visible_blocks(&document, middle, 0, 680.0, 720.0, TYPE);
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

    /// The reader's type at `scale` times the default size.
    fn type_at(scale: f32) -> ReaderType {
        ReaderType {
            scale,
            display: TYPE.display * scale,
            title: TYPE.title * scale,
            body: TYPE.body * scale,
            caption: TYPE.caption * scale,
        }
    }

    /// The rows the painter wraps `block` into in a column `width` wide at `ty`.
    fn painted_rows(block: &MarkdownBlock, width: f32, ty: ReaderType) -> usize {
        let frame = block_frame(BlockShape::of(block), ty).expect("a block with text");
        wrap_reader_text(
            &block_paint_text(block),
            frame.columns(width),
            0,
            READER_ROW_LIMIT,
            frame.preserve_lines,
        )
        .len()
    }

    /// A WIDE CHAR RESERVES TWO COLUMNS OF A BLOCK'S ROWS, as the reader's
    /// wrap lays it on two ([`reader_columns`]): a Japanese paragraph, a
    /// heading and a code line take no fewer rows than Latin text of twice
    /// their chars, at every width.
    #[test]
    fn a_wide_char_reserves_two_columns_of_a_blocks_rows() {
        let cjk = "設定ファイルを読み込めませんでした".repeat(12);
        let latin = "ab".repeat(cjk.chars().count());
        for (wide, narrow) in [
            (cjk.clone(), latin.clone()),
            (format!("# {cjk}"), format!("# {latin}")),
            (format!("```\n{cjk}\n```"), format!("```\n{latin}\n```")),
        ] {
            let (wide, narrow) = (parse(&wide), parse(&narrow));
            for width in [240.0_f32, 250.0, 480.0, 760.0] {
                let (wide_rows, narrow_rows) = (
                    block_visual_rows(&wide.blocks[0], width, TYPE),
                    block_visual_rows(&narrow.blocks[0], width, TYPE),
                );
                assert!(
                    wide_rows >= narrow_rows && narrow_rows > 1,
                    "{wide_rows} rows for {narrow_rows} at {width}: {:?}",
                    wide.blocks[0]
                );
            }
        }
        assert_eq!(reader_columns('設'), 2);
        assert_eq!(reader_columns('a'), 1);
        assert_eq!(
            reader_columns('\u{301}'),
            1,
            "a combining mark, as a char always was"
        );
    }

    /// EVERY ROW A BLOCK PAINTS IS A ROW IT IS LAID OUT WITH, at every text
    /// size the reader takes (0.85 to 2.0) and every width: a block's
    /// measure is the painter's own wrap of the text it paints, so its
    /// rectangle holds every row (no row is cut or elided), and moving by
    /// rows to the end of the document reaches the last row of the last
    /// block. At 2x the rows grow; the fixed 1x estimate this replaced kept
    /// the 1x rows, so the painter cut every block and its tail rows could
    /// never be scrolled to.
    #[test]
    fn a_block_lays_out_every_row_it_paints_at_every_text_size() {
        let source = "# 設定ファイルの読み込み\n\n\
            設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\
            コーヒーブレイク中にアップデートをダウンロードしています、ちょっと待ってください。\n\n\
            ## A second heading that runs long enough to wrap at a narrow width\n\n\
            ### Third\n\n\
            - a list item that wraps across the measure of a phone-width reader\n\
            \x20\x20- a nested one, deeper, that wraps too when the reader is narrow\n\n\
            > A quoted paragraph that also wraps when the reader is narrow enough.\n\n\
            ```rust\nfn main() { println!(\"a long line of code that wraps in a narrow pane\"); }\n```\n\n\
            ```\n設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\n```\n\n\
            | Column | Another column with words |\n|---|---|\n| 1 | a cell long enough to wrap the table row |\n\n\
            ---\n\n\
            The quick brown fox jumps over the lazy dog, and aarch64-apple-darwin wraps \
            at its spaces as it always did.\n";
        let document = parse(source);
        assert!(document.blocks.len() >= 10, "{:?}", document.blocks);
        for scale in [0.85_f32, 1.0, 1.5, 2.0] {
            let ty = type_at(scale);
            for width in [206.0_f32, 326.0, 480.0, 732.0] {
                for (index, block) in document.blocks.iter().enumerate() {
                    let measure = measure_block(block, width, ty);
                    let Some(frame) = block_frame(BlockShape::of(block), ty) else {
                        assert_eq!(measure.rows, 1);
                        continue;
                    };
                    let painted = painted_rows(block, width, ty);
                    assert_eq!(
                        measure.rows, painted,
                        "block {index} at {scale}x and {width}: {block:?}"
                    );
                    assert!(
                        measure.height - VISUAL_BLOCK_GAP + 0.01
                            >= frame.inset_top + painted as f32 * frame.line_h,
                        "block {index} at {scale}x and {width}: its rectangle holds every row"
                    );
                    let layout = layout_visible_blocks(
                        &document,
                        block.source().start,
                        0,
                        width,
                        100_000.0,
                        ty,
                    );
                    assert_eq!(layout.blocks[0].index, index);
                    assert_eq!(layout.blocks[0].total_visual_rows, painted);
                }
                let start = MarkdownLocation::new(0, 0);
                let end = move_visual_rows(&document, start, width, ty, isize::MAX);
                let last = document.blocks.len() - 1;
                assert_eq!(
                    end.visual_row,
                    painted_rows(&document.blocks[last], width, ty) - 1,
                    "the last row is reachable at {scale}x and {width}"
                );
                let mut walked = start;
                let mut steps = 0usize;
                loop {
                    let next = move_visual_rows(&document, walked, width, ty, 1);
                    if next == walked {
                        break;
                    }
                    walked = next;
                    steps += 1;
                }
                let total: usize = document
                    .blocks
                    .iter()
                    .map(|block| measure_block(block, width, ty).rows)
                    .sum();
                assert_eq!(
                    steps + 1,
                    total,
                    "one wheel row at a time visits every row at {scale}x and {width}"
                );
                assert_eq!(walked, end);
            }
        }
        let paragraph = &document.blocks[1];
        assert!(
            measure_block(paragraph, 326.0, type_at(2.0)).rows
                > measure_block(paragraph, 326.0, TYPE).rows,
            "larger text wraps into more rows"
        );
    }

    /// A PAGE MOVES BY THE ROWS THAT FILL MOST OF THE VIEWPORT, each counted
    /// at its own height: at 2x a page holds about half the rows it holds at
    /// 1x, so paging never jumps past rows the reader did not show.
    #[test]
    fn a_page_moves_by_the_rows_that_fill_the_viewport_at_their_height() {
        let source = (0..400)
            .map(|line| format!("line-{line:04}\n"))
            .collect::<String>();
        let document = parse(&format!("```\n{source}```\n"));
        let start = MarkdownLocation::new(document.blocks[0].source().start, 0);
        for scale in [1.0_f32, 2.0] {
            let ty = type_at(scale);
            let row_h = measure_block(&document.blocks[0], 600.0, ty).row_h;
            let page = move_visual_distance(&document, start, 600.0, ty, 400.0, 1);
            assert_eq!(
                page.visual_row,
                (400.0 / row_h).floor() as usize,
                "a page at {scale}x"
            );
            let back = move_visual_distance(&document, page, 600.0, ty, 400.0, -1);
            assert_eq!(back.visual_row, 0, "a page back at {scale}x");
            assert_eq!(
                move_visual_distance(&document, start, 600.0, ty, 1.0, 1).visual_row,
                1,
                "at least one row"
            );
        }
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
        let rows = block_visual_rows(&document.blocks[0], width, TYPE);
        assert!(rows >= 1_000, "every authored code line remains reachable");

        let start = MarkdownLocation::new(document.blocks[0].source().start, 0);
        let one_row = move_visual_rows(&document, start, width, TYPE, 1);
        assert_eq!(one_row.visual_row, 1, "wheel motion stays intra-block");

        let mut paged = start;
        for _ in 0..80 {
            paged = move_visual_rows(&document, paged, width, TYPE, 14);
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
            TYPE,
        );
        assert_eq!(visible.blocks[0].index, 0);
        assert_eq!(visible.blocks[0].visual_row, paged.visual_row);

        let end = move_visual_rows(&document, start, width, TYPE, isize::MAX);
        assert_eq!(end.visual_row, rows - 1);
        let back = move_visual_rows(&document, end, width, TYPE, -1);
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
            location = move_visual_rows(&document, location, 680.0, TYPE, 1);
            state.insert("actual_row", location.visual_row as i64);
            state.insert("expected_row", step);
            state.insert("steps", step);
            assert_eq!(admits(&model, &before, &state), Some("Step"));
            assert!(model.check_invariant("ExactIntraBlockProgress", &state));
        }

        let before = model.init_state();
        let mut skipped = before.clone();
        skipped.insert("actual_row", 4);
        skipped.insert("expected_row", 1);
        skipped.insert("steps", 1);
        assert_eq!(admits(&model, &before, &skipped), None);
        assert!(!model.check_invariant("ExactIntraBlockProgress", &skipped));
    }
}
