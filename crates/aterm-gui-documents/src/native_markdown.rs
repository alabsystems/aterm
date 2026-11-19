// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic, non-web Markdown document model for native tab views.

use std::{borrow::Cow, ops::Range, sync::Arc};

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
    /// Sorted (later block index, shared quote depth) for joined boundaries.
    quote_continuations: Vec<(usize, usize)>,
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
        depth: usize,
        block: Box<MarkdownBlock>,
    },
    CodeBlock {
        language: Option<String>,
        code: String,
        source: Range<usize>,
    },
    Table {
        table: MarkdownTable,
        source: Range<usize>,
    },
    ThematicBreak {
        source: Range<usize>,
    },
}

impl MarkdownBlock {
    #[inline]
    pub fn content(&self) -> &Self {
        let mut leaf = self;
        while let Self::Quote { block, .. } = leaf {
            leaf = block;
        }
        leaf
    }

    #[inline]
    pub fn quote_depth(&self) -> usize {
        let mut leaf = self;
        let mut total = 0usize;
        while let Self::Quote { depth, block } = leaf {
            total = total.saturating_add(*depth);
            leaf = block;
        }
        total
    }

    #[inline]
    pub fn source(&self) -> &Range<usize> {
        match self.content() {
            Self::Heading { source, .. }
            | Self::Paragraph { source, .. }
            | Self::ListItem { source, .. }
            | Self::CodeBlock { source, .. }
            | Self::Table { source, .. }
            | Self::ThematicBreak { source } => source,
            Self::Quote { .. } => unreachable!("content returns a leaf"),
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
    pub destination: Arc<str>,
    pub source: Range<usize>,
    pub policy: LinkPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownImage {
    pub alt: Option<String>,
    pub source_uri: Arc<str>,
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

fn has_http_authority(after_slashes: &str) -> bool {
    let authority_end = after_slashes
        .find(['/', '?', '#', '\\'])
        .unwrap_or(after_slashes.len());
    let authority = &after_slashes[..authority_end];
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, rest)| rest);
    !host_port.is_empty() && !host_port.starts_with(':')
}

fn percent_decode_utf8(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = char::from(*bytes.get(i + 1)?).to_digit(16)?;
            let lo = char::from(*bytes.get(i + 2)?).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn safe_local_image_path(uri: &str) -> bool {
    if uri.is_empty()
        || uri.len() > 4_096
        || uri.starts_with('/')
        || uri.starts_with('\\')
        || uri.starts_with('#')
        || uri.contains(':')
        || uri.chars().any(|c| c.is_whitespace() || c.is_control())
        || uri.split(['/', '\\']).any(|component| component == "..")
    {
        return false;
    }
    let Some(decoded) = percent_decode_utf8(uri) else {
        return false;
    };
    !decoded.starts_with('/')
        && !decoded.starts_with('\\')
        && !decoded.starts_with('#')
        && !decoded.contains(':')
        && !decoded.chars().any(char::is_control)
        && !decoded
            .split(['/', '\\'])
            .any(|component| component == "..")
}

/// Pure image-policy reducer.
pub fn reduce_image_action(image: &MarkdownImage, user_initiated: bool) -> MarkdownImageAction {
    if image.remote {
        let lower = image.source_uri.to_ascii_lowercase();
        let after_slashes = lower
            .strip_prefix("https://")
            .or_else(|| lower.strip_prefix("http://"));
        let safe = image.source_uri.len() <= 2_048
            && after_slashes.is_some_and(has_http_authority)
            && !image
                .source_uri
                .chars()
                .any(|c| c.is_whitespace() || c.is_control());
        return if safe && user_initiated {
            MarkdownImageAction::OpenRemote {
                uri: image.source_uri.to_string(),
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

    if !safe_local_image_path(&image.source_uri) {
        return MarkdownImageAction::Denied {
            message: "image path escapes the document capability",
        };
    }
    if !user_initiated {
        return MarkdownImageAction::Denied {
            message: "image actions require an explicit user gesture",
        };
    }
    MarkdownImageAction::SelectLocalSource {
        range: image.source.clone(),
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
    /// Carry retained locations through a canonical document edit without visiting a new
    /// entry or changing which history entry is current.
    pub fn map_locations(&mut self, mut map: impl FnMut(MarkdownLocation) -> MarkdownLocation) {
        for location in &mut self.entries {
            *location = map(*location);
        }
    }

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

/// Read `source` into its blocks, outline, links and images, with its HTML comments hidden.
///
/// The comment scan runs before any block exists and judges code spans line by line
/// ([`mask_html_comments`]); the readers then judge them per block. Where the scan kept a comment
/// opener that the readers show outside code, the source is read again with every comment hidden,
/// so no comment shows or lends a link (`md-comments-hidden-code-preserved`).
pub fn parse(source: &str) -> MarkdownDocument {
    let kept = mask_html_comments(source, CommentScan::KeepCode);
    let (document, shows_a_comment) = read_document(source, &kept);
    if !shows_a_comment {
        return document;
    }
    read_document(source, &mask_html_comments(source, CommentScan::HideAll)).0
}

/// [`parse`] of `source` read from `display_source`, its comment-masked copy, and whether an
/// HTML comment opener still shows there outside the code the readers found.
fn read_document(source: &str, display_source: &str) -> (MarkdownDocument, bool) {
    let lines = source_lines(display_source);
    // In multiline prose and definitions the markers occupy whitespace. Table
    // cells and fence bodies use the individual lines, without that padding.
    let mut padded = None;
    for line in &lines {
        if line.quote_depth > 0 {
            mask_source_range(&mut padded, display_source, line.whole_start..line.start);
        }
    }
    let padded = padded.map(|bytes| String::from_utf8(bytes).expect("masked ASCII quote markers"));
    let inline_source = padded.as_deref().unwrap_or(display_source);
    let mut found = Found::default();
    let first = read_blocks(source, display_source, inline_source, &lines, &mut found);
    if found.references.is_empty() {
        return first;
    }
    drop(first);
    read_blocks(source, display_source, inline_source, &lines, &mut found)
}

/// The block reader recognizes definitions only at the beginning of a paragraph.
/// When it finds any, a second reading can resolve references before their definitions.
fn read_blocks(
    source: &str,
    display_source: &str,
    inline_source: &str,
    all_lines: &[SourceLine<'_>],
    found: &mut Found,
) -> (MarkdownDocument, bool) {
    let mut blocks = Vec::new();
    let mut outline = Vec::new();
    let mut block_styles = std::collections::BTreeMap::new();
    let mut anchors = std::collections::BTreeMap::new();
    let mut heading_ids = std::collections::BTreeMap::<String, usize>::new();
    let mut make_heading = |level, text: String, source: Range<usize>| {
        let base = slug(&text);
        let count = heading_ids.entry(base.clone()).or_insert(0);
        let id = if *count == 0 {
            base
        } else {
            format!("{base}-{}", *count + 1)
        };
        *count += 1;
        outline.push(HeadingRef {
            level,
            id: id.clone(),
            text: text.clone(),
            source_start: source.start,
        });
        anchors.insert(id.clone(), source.start);
        MarkdownBlock::Heading {
            level,
            id,
            text,
            source,
        }
    };
    let mut i = 0usize;

    while i < all_lines.len() {
        let group = i;
        let depth = all_lines[i].quote_depth;
        let end_of_group = i + all_lines[i..]
            .iter()
            .take_while(|line| line.quote_depth == depth)
            .count();
        let lines = &all_lines[..end_of_group];
        let before = blocks.len();
        let wrap = |block| {
            if depth == 0 {
                block
            } else {
                MarkdownBlock::Quote {
                    depth,
                    block: Box::new(block),
                }
            }
        };
        while i < end_of_group {
            let line = &lines[i];
            let trimmed = line.text.trim();
            if trimmed.is_empty() {
                i += 1;
                continue;
            }

            if let Some(fence) = fence_start(trimmed) {
                let start = line.whole_start;
                let language = trimmed[fence.len()..].trim();
                let language = (!language.is_empty()).then(|| language.to_string());
                i += 1;
                let mut code = String::new();
                let mut end = line.end;
                while let Some(candidate) =
                    all_lines.get(i).filter(|next| next.quote_depth >= depth)
                {
                    // Any further markers inside this fence are literal code.
                    let raw = &display_source
                        [candidate.whole_start..candidate.end - usize::from(candidate.has_newline)];
                    let content = if candidate.quote_depth == depth {
                        candidate.text
                    } else {
                        quote_prefix(raw, depth).1
                    };
                    end = candidate.end;
                    if is_fence_close(content, fence) {
                        i += 1;
                        break;
                    }
                    code.push_str(content);
                    if candidate.has_newline {
                        code.push('\n');
                    }
                    i += 1;
                }
                blocks.push(wrap(MarkdownBlock::CodeBlock {
                    language,
                    code,
                    source: start..end,
                }));
                continue;
            }

            if let Some((level, text)) = heading(trimmed) {
                let at = line.start + (line.text.len() - line.text.trim_start().len()) + text.start;
                let (text, styles) =
                    read_inline_text(&display_source[at..at + text.len()], at, Some(found));
                if !styles.is_empty() {
                    block_styles.insert(blocks.len(), styles);
                }
                blocks.push(wrap(make_heading(level, text, line.whole_start..line.end)));
                i += 1;
                continue;
            }

            if is_thematic(trimmed) {
                blocks.push(wrap(MarkdownBlock::ThematicBreak {
                    source: line.whole_start..line.end,
                }));
                i += 1;
                continue;
            }

            if i + 1 < lines.len() && table_header(line.text, lines[i + 1].text) {
                let first = i;
                i += 2;
                while i < lines.len() && lines[i].text.contains('|') {
                    i += 1;
                }
                let table = MarkdownTable::read(&lines[first..i], found);
                blocks.push(wrap(MarkdownBlock::Table {
                    table,
                    source: line.whole_start..lines[i - 1].end,
                }));
                continue;
            }

            if trimmed.starts_with('[')
                && let Some(bytes) = found
                    .references
                    .read(&inline_source[line.start..lines[end_of_group - 1].end])
            {
                let end = line.start + bytes;
                i += lines[i..].partition_point(|candidate| candidate.start < end);
                continue;
            }

            // List items and paragraphs have the same soft line breaks. Read their
            // contiguous source so delimiters and link labels can span those breaks.
            let first = i;
            let start = line.start;
            let item = list_item(line.text);
            let content_start =
                item.map_or(start, |(_, _, text)| start + line.text.len() - text.len());
            while paragraph_continues_after(lines, i) {
                i += 1;
            }
            let end = lines[i].end;
            i += 1;
            let body = &inline_source[start..end];
            let body = if item.is_some() {
                let indent = content_start - start;
                let contents = lines[first..i].iter().map(|candidate| {
                    let prefix = if candidate.start == start {
                        indent
                    } else {
                        candidate
                            .text
                            .bytes()
                            .take(indent)
                            .take_while(|&b| b == b' ')
                            .count()
                    };
                    (candidate.start - start + prefix, &candidate.text[prefix..])
                });
                hide_container_definitions(body, contents, &mut found.references)
            } else {
                Cow::Borrowed(body)
            };
            let (text, styles) =
                read_inline_text(&body[content_start - start..], content_start, Some(found));
            if !styles.is_empty() {
                block_styles.insert(blocks.len(), styles);
            }
            if let Some((depth, ordinal, _)) = item {
                blocks.push(wrap(MarkdownBlock::ListItem {
                    depth,
                    ordinal,
                    text,
                    source: line.whole_start..end,
                }));
                continue;
            }
            if let Some(level) = lines.get(i).and_then(|next| setext_level(next.text.trim())) {
                blocks.push(wrap(make_heading(
                    level,
                    text,
                    line.whole_start..lines[i].end,
                )));
                i += 1;
                continue;
            }
            blocks.push(wrap(MarkdownBlock::Paragraph {
                text,
                source: line.whole_start..end,
            }));
        }
        // A deeper neighbor already represents this enclosing quote. A blank
        // group beside it separates leaves and has no paragraph of its own.
        if depth > 0
            && before == blocks.len()
            && all_lines[..group]
                .last()
                .is_none_or(|prev| prev.quote_depth <= depth)
            && all_lines
                .get(end_of_group)
                .is_none_or(|next| next.quote_depth <= depth)
        {
            blocks.push(wrap(MarkdownBlock::Paragraph {
                text: String::new(),
                source: lines[group].whole_start..lines[end_of_group - 1].end,
            }));
        }
    }

    let shows_a_comment = shows_a_comment(display_source, &blocks, &found.runs);
    let document = MarkdownDocument {
        quote_continuations: quote_continuations(&blocks, all_lines),
        blocks,
        outline,
        links: std::mem::take(&mut found.links),
        images: std::mem::take(&mut found.images),
        inline_runs: std::mem::take(&mut found.runs),
        block_styles,
        anchors,
        source_len: source.len(),
        source_line_starts: std::iter::once(0)
            .chain(source.bytes().enumerate().filter_map(|(index, byte)| {
                (byte == b'\n' && index + 1 < source.len()).then_some(index + 1)
            }))
            .collect(),
    };
    (document, shows_a_comment)
}

fn quote_continuations(blocks: &[MarkdownBlock], lines: &[SourceLine<'_>]) -> Vec<(usize, usize)> {
    let mut joined = Vec::new();
    let mut lines = lines.iter().peekable();
    for (before, pair) in blocks.windows(2).enumerate() {
        let mut depth = pair[0].quote_depth().min(pair[1].quote_depth());
        if depth == 0 {
            continue;
        }
        let end = pair[0].source().end;
        let start = pair[1].source().start;
        if end < start {
            // A marker-only line can leave an outer quote open while closing
            // an inner one. An unquoted blank closes every level.
            while let Some(line) = lines.next_if(|line| line.whole_start < start) {
                if line.whole_start >= end {
                    depth = depth.min(line.quote_depth);
                }
            }
        }
        if depth > 0 {
            joined.push((before + 1, depth));
        }
    }
    joined
}

mod references;
mod table;
pub use table::MarkdownTable;

/// Definitions belong to the paragraph inside its container. Each line gives the offset
/// of its content after the marker or continuation indent. Keep the bytes of every
/// remaining link and style at their original positions, including on wrapped lines.
fn hide_container_definitions<'a, 'line>(
    source: &'a str,
    lines: impl Iterator<Item = (usize, &'line str)>,
    definitions: &mut references::Definitions,
) -> Cow<'a, str> {
    let mut masked = None;
    let mut hidden_through = 0;
    let mut paragraph_start = true;
    let mut open_fence = None;
    for (at, content) in lines {
        if at < hidden_through {
            continue;
        }
        let trimmed = content.trim();
        let after_spaces = content.trim_start_matches(' ');
        let indented = content.len() - after_spaces.len() > 3 || after_spaces.starts_with('\t');
        if let Some(fence) = open_fence {
            if !indented && is_fence_close(content, fence) {
                open_fence = None;
                paragraph_start = true;
            }
            continue;
        }
        if trimmed.is_empty() {
            paragraph_start = true;
            continue;
        }
        if indented {
            // At a paragraph start this is code. Within prose it continues that prose.
            continue;
        }
        if let Some(fence) = fence_start(trimmed) {
            open_fence = Some(fence);
            continue;
        }
        if paragraph_start
            && trimmed.starts_with('[')
            && let Some(bytes) = definitions.read(&source[at..])
        {
            hidden_through = at + bytes;
            mask_source_range(&mut masked, source, at..hidden_through);
            continue;
        }
        paragraph_start = heading(trimmed).is_some()
            || trimmed.starts_with(['-', '*', '_']) && is_thematic(trimmed);
    }
    masked.map_or(Cow::Borrowed(source), |bytes| {
        Cow::Owned(String::from_utf8(bytes).expect("definition masking preserves UTF-8"))
    })
}

/// Whether an HTML comment opener shows in `display_source` outside the code the readers found:
/// a fenced block, or the content of an inline code run.
fn shows_a_comment(
    display_source: &str,
    blocks: &[MarkdownBlock],
    runs: &[MarkdownInlineRun],
) -> bool {
    let mut code = blocks
        .iter()
        .filter_map(|block| match block.content() {
            MarkdownBlock::CodeBlock { source, .. } => Some(source.clone()),
            _ => None,
        })
        .chain(
            runs.iter()
                .filter(|run| run.kind == MarkdownInlineKind::Code)
                .map(|run| run.content_source.clone()),
        )
        .collect::<Vec<_>>();
    // Code blocks and code runs never overlap, so the one starting last at or before an opener
    // is the only one that can hold it.
    code.sort_unstable_by_key(|range| range.start);
    display_source.match_indices("<!--").any(|(at, opener)| {
        let holder = code.partition_point(|range| range.start <= at);
        !holder
            .checked_sub(1)
            .is_some_and(|holder| at + opener.len() <= code[holder].end)
    })
}

/// How one span of a block's display text is styled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InlineStyle {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
    pub link_index: Option<usize>,
}

impl InlineStyle {
    #[inline]
    const fn is_plain(self) -> bool {
        !self.strong && !self.emphasis && !self.strike && !self.code && !self.link
    }

    #[inline]
    fn merged(self, other: Self) -> Self {
        Self {
            strong: self.strong || other.strong,
            emphasis: self.emphasis || other.emphasis,
            strike: self.strike || other.strike,
            code: self.code || other.code,
            link: self.link || other.link,
            link_index: other.link_index.or(self.link_index),
        }
    }
}

impl MarkdownDocument {
    /// Number of outer quote levels continuing from the previous block into
    /// block `index`. The first block and out-of-range indices return zero.
    pub fn quote_continuation(&self, index: usize) -> usize {
        self.quote_continuations
            .binary_search_by_key(&index, |&(block, _)| block)
            .map_or(0, |slot| self.quote_continuations[slot].1)
    }

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
    if id.contains('%') {
        document.anchors.get(&percent_decode_utf8(id)?).copied()
    } else {
        document.anchors.get(id).copied()
    }
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
            MarkdownBlock::Quote { block, .. } => Self::of(block),
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

/// The rails in the margin of a quoted block. They share at most one fifth of the column
/// with each other, leaving the rest for the block's own list marker, table or prose.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuoteGutter {
    pub rails: usize,
    pub step: f32,
}

impl QuoteGutter {
    #[must_use]
    pub fn new(depth: usize, width: f32, ty: ReaderType) -> Self {
        let budget = width.max(0.0) * 0.20;
        // A shared outer rail keeps its x coordinate as inner quotes begin and end.
        let capacity = ((budget / 10.0).floor() as usize).clamp(1, 4);
        let step = (18.0 * ty.scale()).min(budget / capacity as f32);
        Self {
            rails: depth.min(capacity),
            step,
        }
    }

    #[must_use]
    pub fn width(self) -> f32 {
        self.rails as f32 * self.step
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
        if !width.is_finite() || !self.inset_left.is_finite() || !self.inset_right.is_finite() {
            return 4;
        }
        let adv = self.advance();
        let denom = if adv.is_finite() { adv.max(1.0) } else { 1.0 };
        let cols = (self.text_width(width) / denom).floor();
        if cols.is_finite() && cols >= 4.0 {
            cols.min(1_000_000.0) as usize
        } else {
            4
        }
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
        if !height.is_finite()
            || !self.inset_top.is_finite()
            || !self.line_h.is_finite()
            || self.line_h <= 0.0
        {
            return 1;
        }
        let rows = ((height - self.inset_top + 0.01) / self.line_h).floor();
        if rows.is_finite() && rows >= 1.0 {
            rows.min(1_000_000.0) as usize
        } else {
            1
        }
    }
}

/// The frame of a block of `shape` at `ty`; `None` for a rule, which has no text.
#[must_use]
pub fn block_frame(
    shape: BlockShape,
    quote_depth: usize,
    width: f32,
    ty: ReaderType,
) -> Option<BlockFrame> {
    let scale = ty.scale();
    let gutter = QuoteGutter::new(quote_depth, width, ty);
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
    let mut result = match shape {
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
            inset_left: ((28.0 + (depth as f32 * 16.0).min(64.0)) * scale)
                .min((width - gutter.width()).max(0.0) * 0.5),
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
    };
    result.inset_left += gutter.width();
    Some(result)
}

/// The full text frame for a document block, with its quote margin fitted to the column.
#[must_use]
pub fn preview_frame(block: &MarkdownBlock, width: f32, ty: ReaderType) -> Option<BlockFrame> {
    block_frame(BlockShape::of(block), block.quote_depth(), width, ty)
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
    let Some(frame) = preview_frame(block, width, ty) else {
        let row_h = rule_height(ty);
        return BlockMeasure {
            rows: 1,
            row_h,
            height: row_h + VISUAL_BLOCK_GAP,
        };
    };
    let rows = reader_rows(
        &block_text_at_columns(block, frame.columns(width)),
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

pub fn block_text_at_columns(block: &MarkdownBlock, columns: usize) -> Cow<'_, str> {
    match block.content() {
        MarkdownBlock::Table { table, .. } => Cow::Owned(table.project(columns).0),
        _ => block_paint_text(block),
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
        | MarkdownBlock::CodeBlock { code: text, .. } => bounded_text(text, BLOCK_PAINT_LIMIT),
        MarkdownBlock::Quote { block, .. } => block_paint_text(block),
        MarkdownBlock::ThematicBreak { .. } => Cow::Borrowed(""),
        MarkdownBlock::Table { table, .. } => Cow::Owned(table.project(usize::MAX).0),
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
    if document.blocks.is_empty()
        || !viewport_width.is_finite()
        || viewport_width <= 0.0
        || !viewport_height.is_finite()
        || viewport_height <= 0.0
    {
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
    if document.blocks.is_empty() || direction == 0 || !distance.is_finite() {
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

/// An optional quoted title is separate from the path the opener receives.
fn link_destination(raw: &str) -> &str {
    let raw = raw.trim();
    let (dest, title) = raw.split_once(char::is_whitespace).unwrap_or((raw, ""));
    let title = title.trim();
    if !title.is_empty()
        && ![('"', '"'), ('\'', '\''), ('(', ')')]
            .iter()
            .any(|&(open, close)| {
                title
                    .strip_prefix(open)
                    .is_some_and(|rest| rest.ends_with(close))
            })
    {
        return raw;
    }
    dest.strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or(dest)
}

/// Decode each authored escape once. A literal URL with an ordinary ampersand
/// stays borrowed, and text produced by an escape is never scanned again.
fn unescape_destination(value: &str) -> Cow<'_, str> {
    let mut out = String::new();
    let mut copied = 0;
    for (index, marker) in value.match_indices(['\\', '&']) {
        if index < copied {
            continue;
        }
        let after = &value[index + 1..];
        let replacement = if marker == "\\" {
            after
                .as_bytes()
                .first()
                .copied()
                .filter(u8::is_ascii_punctuation)
                .map(|byte| (char::from(byte), 1))
        } else {
            after
                .bytes()
                .take(20)
                .position(|byte| byte == b';')
                .and_then(|end| decode_entity(&after[..end]).map(|ch| (ch, end + 1)))
        };
        if let Some((ch, consumed)) = replacement {
            out.push_str(&value[copied..index]);
            out.push(ch);
            copied = index + 1 + consumed;
        }
    }
    if copied == 0 {
        return Cow::Borrowed(value);
    }
    out.push_str(&value[copied..]);
    Cow::Owned(out)
}

fn link_policy(destination: &str) -> LinkPolicy {
    if destination.is_empty()
        || destination
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return LinkPolicy::DeniedScheme;
    }
    if destination.starts_with('#') {
        return LinkPolicy::LocalAnchor;
    }
    if destination.starts_with("//")
        || destination.starts_with("/\\")
        || destination.starts_with('\\')
    {
        return LinkPolicy::DeniedScheme;
    }
    let Some((scheme, rest)) = destination.split_once(':') else {
        return LinkPolicy::LocalDocument;
    };
    match scheme.to_ascii_lowercase().as_str() {
        "https" | "http" => match rest.strip_prefix("//") {
            Some(after_slashes) if has_http_authority(after_slashes) => {
                LinkPolicy::ExplicitExternal
            }
            _ => LinkPolicy::DeniedScheme,
        },
        "mailto" if !rest.is_empty() && !rest.starts_with("//") => LinkPolicy::ExplicitExternal,
        _ => LinkPolicy::DeniedScheme,
    }
}

#[derive(Clone, Copy)]
struct SourceLine<'a> {
    text: &'a str,
    /// Start of the physical line and of the content after the quote markers.
    whole_start: usize,
    start: usize,
    end: usize,
    has_newline: bool,
    quote_depth: usize,
}

/// Consume at most `limit` quote markers, each indented by up to three spaces
/// and followed by one optional space or tab. Extra indentation belongs to the leaf.
fn quote_prefix(mut text: &str, limit: usize) -> (usize, &str) {
    let mut depth = 0;
    while depth < limit {
        let indent = text.bytes().take(3).take_while(|&b| b == b' ').count();
        let Some(rest) = text[indent..].strip_prefix('>') else {
            break;
        };
        text = rest.strip_prefix([' ', '\t']).unwrap_or(rest);
        depth += 1;
    }
    (depth, text)
}

fn source_lines(source: &str) -> Vec<SourceLine<'_>> {
    let mut start = 0;
    source
        .split_inclusive('\n')
        .map(|segment| {
            let raw = segment.strip_suffix('\n').unwrap_or(segment);
            let (quote_depth, text) = quote_prefix(raw, usize::MAX);
            let line = SourceLine {
                text,
                whole_start: start,
                start: start + raw.len() - text.len(),
                end: start + segment.len(),
                has_newline: segment.ends_with('\n'),
                quote_depth,
            };
            start = line.end;
            line
        })
        .collect()
}

/// Which comments [`mask_html_comments`] leaves in place.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommentScan {
    /// Those inside fenced code and inside code spans, which are code text.
    KeepCode,
    /// None: every comment is hidden, in code too.
    HideAll,
}

/// Hide HTML comments without moving a single canonical source byte.
///
/// With [`CommentScan::KeepCode`] a comment inside fenced code or a code span is code text and
/// stays. A backtick run opens a span only when its inline block closes it
/// ([`CodeTicks::close`]), and the span closes at the first equal run that is not
/// backslash-escaped, the pairing the inline readers use ([`find_exact_run`]); an unmatched run
/// is literal and keeps no comment visible. Nothing is masked inside a span, so its closer stays
/// where the readers find it, and a span is closed again before any line that starts another
/// block. With [`CommentScan::HideAll`] no fence or span keeps a comment.
fn mask_html_comments(source: &str, scan: CommentScan) -> Cow<'_, str> {
    if !source.contains("<!--") {
        return Cow::Borrowed(source);
    }
    let keep_code = scan == CommentScan::KeepCode;
    let lines = source_lines(source);
    let mut code_ticks = CodeTicks::new(&lines);
    let mut masked = None::<Vec<u8>>;
    let mut comment_depth = None;
    let mut inline_code_ticks = None::<usize>;
    let mut open_fence = None::<(usize, &str)>;

    for (at, line) in lines.iter().enumerate() {
        let bytes = line.text.as_bytes();
        let trimmed = line.text.trim();

        if let Some((depth, fence)) = open_fence {
            if line.quote_depth >= depth {
                if line.quote_depth == depth && is_fence_close(line.text, fence) {
                    open_fence = None;
                }
                continue;
            }
            open_fence = None;
        }
        if keep_code
            && comment_depth.is_none()
            && inline_code_ticks.is_none()
            && let Some(fence) = fence_start(trimmed)
        {
            open_fence = Some((line.quote_depth, fence));
            continue;
        }

        // Markers deeper than the comment opener are hidden comment text.
        if let Some(depth) = comment_depth.filter(|&depth| line.quote_depth > depth) {
            let prefix = &source[line.whole_start..line.start];
            let extra = quote_prefix(prefix, depth).1.len();
            mask_source_range(&mut masked, source, line.start - extra..line.start);
        }
        let mut index = 0usize;
        while index < bytes.len() {
            if comment_depth.is_some() {
                let close = find_ascii(&bytes[index..], b"-->");
                let end = close.map_or(bytes.len(), |relative| index + relative + 3);
                mask_source_range(&mut masked, source, line.start + index..line.start + end);
                index = end;
                if close.is_some() {
                    comment_depth = None;
                }
                continue;
            }

            if keep_code && bytes[index] == b'`' {
                let run = ascii_run(bytes, index, b'`');
                if !is_backslash_escaped(bytes, index) {
                    inline_code_ticks = match inline_code_ticks {
                        Some(open) if open == run => None,
                        Some(open) => Some(open),
                        None => code_ticks.close(at, index + run, run).then_some(run),
                    };
                }
                index += run;
                continue;
            }

            if inline_code_ticks.is_none() && bytes[index..].starts_with(b"<!--") {
                comment_depth = Some(line.quote_depth);
                continue;
            }
            index += line.text[index..].chars().next().map_or(1, char::len_utf8);
        }
    }

    masked.map_or(Cow::Borrowed(source), |bytes| {
        Cow::Owned(String::from_utf8(bytes).expect("comment masking preserves UTF-8"))
    })
}

/// The look ahead of [`mask_html_comments`] for the closer of a code span, over lines it reads
/// in order. Each line's [`joins_next_line`] is judged once, and a run length found unclosed
/// stays unclosed for every later opener in the same block, so a block of many unmatched runs
/// is not read again for each of them.
struct CodeTicks<'s, 'l> {
    lines: &'l [SourceLine<'s>],
    joins: Vec<Option<bool>>,
    /// For each run length found unclosed, the last line of the block it was looked for in.
    unclosed: std::collections::HashMap<usize, usize>,
}

impl<'s, 'l> CodeTicks<'s, 'l> {
    fn new(lines: &'l [SourceLine<'s>]) -> Self {
        Self {
            lines,
            joins: vec![None; lines.len()],
            unclosed: std::collections::HashMap::new(),
        }
    }

    /// Whether a code span of `ticks` backticks opened just before byte `from` of line `at`
    /// closes in its inline block: at an equal run that is not backslash-escaped
    /// ([`find_exact_run`]) later on that line or on a later line `parse` reads into the same
    /// block ([`joins_next_line`]). Openers are asked about in source order.
    fn close(&mut self, at: usize, from: usize, ticks: usize) -> bool {
        if self.unclosed.get(&ticks).is_some_and(|&last| at <= last) {
            return false;
        }
        let (mut line, mut from) = (at, from);
        loop {
            if find_exact_run(self.lines[line].text.as_bytes(), from, b'`', ticks).is_some() {
                return true;
            }
            let joins = *self.joins[line].get_or_insert_with(|| joins_next_line(self.lines, line));
            if !joins {
                self.unclosed.insert(ticks, line);
                return false;
            }
            (line, from) = (line + 1, 0);
        }
    }
}

/// Whether the paragraph or list item on line `at` continues on the following line.
fn paragraph_continues_after(lines: &[SourceLine<'_>], at: usize) -> bool {
    let Some(next) = lines.get(at + 1) else {
        return false;
    };
    if next.quote_depth != lines[at].quote_depth {
        return false;
    }
    let trimmed = next.text.trim();
    if trimmed.is_empty()
        || setext_level(trimmed).is_some()
        || heading(trimmed).is_some()
        || fence_start(trimmed).is_some()
        || list_item(next.text).is_some()
        || is_thematic(trimmed)
        || lines.get(at + 2).is_some_and(|after| {
            after.quote_depth == next.quote_depth && table_header(next.text, after.text)
        })
    {
        return false;
    }
    // An empty marker has no paragraph to continue lazily. Its first text
    // line must reach the item's content indent.
    let previous = lines[at].text;
    if let Some((_, _, text)) = list_item(previous)
        && text.trim().is_empty()
    {
        return next.text.len() - next.text.trim_start().len() >= previous.len() - text.len();
    }
    true
}

/// Whether the code-span lookahead can cross the boundary after line `at`.
fn joins_next_line(lines: &[SourceLine<'_>], at: usize) -> bool {
    let Some(next) = lines.get(at + 1) else {
        return false;
    };
    let this = lines[at].text;
    let trimmed = this.trim();
    if heading(trimmed).is_some() || is_thematic(trimmed) || table_header(this, next.text) {
        return false;
    }
    paragraph_continues_after(lines, at)
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

/// Lower inline Markdown to display text and ordered style spans.
pub fn lower_inline_text_styled(source: &str) -> (String, Vec<(Range<usize>, InlineStyle)>) {
    read_inline_text(source, 0, None)
}

/// What reading a block's inline text finds besides the text: its links, images and semantic
/// runs, each at its canonical source range.
#[derive(Default)]
struct Found {
    references: references::Definitions,
    links: Vec<MarkdownLink>,
    images: Vec<MarkdownImage>,
    runs: Vec<MarkdownInlineRun>,
}

impl Found {
    /// Insert the accepted link or image before the runs discovered in its label.
    fn record_link_or_image(
        &mut self,
        run_slot: usize,
        image: bool,
        target: references::Target,
        source: Range<usize>,
        content_source: Range<usize>,
        label: String,
    ) {
        let kind = if image {
            let index = self.images.len();
            self.images.push(MarkdownImage {
                alt: (!content_source.is_empty()).then(|| label.clone()),
                source_uri: target.destination,
                source: source.clone(),
                remote: matches!(target.policy, LinkPolicy::ExplicitExternal),
            });
            MarkdownInlineKind::Image { index }
        } else {
            let index = self.links.len();
            self.links.push(MarkdownLink {
                label: label.clone(),
                destination: target.destination,
                source: source.clone(),
                policy: target.policy,
            });
            MarkdownInlineKind::Link { index }
        };
        self.runs.insert(
            run_slot,
            MarkdownInlineRun {
                kind,
                text: label,
                source,
                content_source,
            },
        );
    }
}

/// The display text and styled spans of the inline text `source`, whose first byte is canonical
/// byte `at`, and with `found` its links, images and runs. A block's text is read here once, so
/// what it shows and the links it lends are one reading of one text.
fn read_inline_text(
    source: &str,
    at: usize,
    found: Option<&mut Found>,
) -> (String, Vec<(Range<usize>, InlineStyle)>) {
    let mut raw = String::with_capacity(source.len());
    let mut spans = Vec::new();
    lower_segment_into(
        source,
        at,
        InlineNesting::default(),
        InlineStyle::default(),
        &mut raw,
        &mut spans,
        found,
    );
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

#[derive(Clone, Copy, Default)]
struct InlineNesting {
    depth: usize,
    image_description: bool,
}

impl InlineNesting {
    fn child(self, image: bool) -> Self {
        Self {
            depth: self.depth + 1,
            image_description: self.image_description || image,
        }
    }
}

/// The lowering pass over `source`, whose first byte is canonical byte `at`, appending to `out`.
/// Each recognized construct records the range of `out` it emitted with its style merged over
/// the enclosing `style` (nested constructs record their own, stronger spans);
/// With `found`, each
/// link, image, code span and emphasis is recorded there too, at its canonical range, in the
/// order it starts. Return whether this segment contains a link outside any image description
/// nested in the segment: a link inside a label takes precedence over its enclosing brackets.
fn lower_segment_into(
    source: &str,
    at: usize,
    nesting: InlineNesting,
    style: InlineStyle,
    out: &mut String,
    spans: &mut Vec<(Range<usize>, InlineStyle)>,
    mut found: Option<&mut Found>,
) -> bool {
    if nesting.depth >= MAX_INLINE_NESTING {
        out.push_str(source);
        return false;
    }
    let mut contains_link = false;
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
                if let Some(found) = found.as_deref_mut() {
                    found.runs.push(MarkdownInlineRun {
                        kind: MarkdownInlineKind::Code,
                        text: code,
                        source: at + index..at + close + ticks,
                        content_source: at + index + ticks..at + close,
                    });
                }
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
            && let Some((end, target)) =
                inline_link_target(source, label_open, label_close, found.as_deref())
        {
            let label = label_open + 1..label_close;
            let run_slot = found.as_ref().map_or(0, |items| items.runs.len());
            let span_start = spans.len();
            let start = out.len();
            let label_has_link = lower_segment_into(
                &source[label.clone()],
                at + label.start,
                nesting.child(image),
                style,
                out,
                spans,
                found.as_deref_mut(),
            );
            if !image && label_has_link {
                // Keep the outer brackets literal. Parse their suffix independently, so
                // a following shortcut reference can still stand on its own.
                out.insert(start, '[');
                for (range, _) in &mut spans[span_start..] {
                    range.start += 1;
                    range.end += 1;
                }
                out.push(']');
                contains_link = true;
                index = label_close + 1;
                continue;
            }
            contains_link |= !image;
            if !nesting.image_description {
                if !image {
                    spans.push((
                        start..out.len(),
                        style.merged(InlineStyle {
                            link: true,
                            link_index: found.as_ref().map(|items| items.links.len()),
                            ..InlineStyle::default()
                        }),
                    ));
                }
                if let Some(found) = found.as_deref_mut() {
                    found.record_link_or_image(
                        run_slot,
                        image,
                        target,
                        at + index..at + end,
                        at + label.start..at + label.end,
                        normalize_display_whitespace(&out[start..]),
                    );
                }
            }
            index = end;
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
                let (own, kind) = if marker == b'~' {
                    (
                        InlineStyle {
                            strike: true,
                            ..InlineStyle::default()
                        },
                        MarkdownInlineKind::Strikethrough,
                    )
                } else if run >= 2 {
                    (
                        InlineStyle {
                            strong: true,
                            emphasis: run == 3,
                            ..InlineStyle::default()
                        },
                        MarkdownInlineKind::Strong,
                    )
                } else {
                    (
                        InlineStyle {
                            emphasis: true,
                            ..InlineStyle::default()
                        },
                        MarkdownInlineKind::Emphasis,
                    )
                };
                let inner = style.merged(own);
                let content = index + run..close;
                let slot = found.as_deref_mut().map(|found| {
                    found.runs.push(MarkdownInlineRun {
                        kind,
                        text: String::new(),
                        source: at + index..at + close + run,
                        content_source: at + content.start..at + content.end,
                    });
                    found.runs.len() - 1
                });
                let start = out.len();
                contains_link |= lower_segment_into(
                    &source[content.clone()],
                    at + content.start,
                    nesting.child(false),
                    inner,
                    out,
                    spans,
                    found.as_deref_mut(),
                );
                spans.push((start..out.len(), inner));
                if let (Some(found), Some(slot)) = (found.as_deref_mut(), slot) {
                    found.runs[slot].text = normalize_display_whitespace(&out[start..]);
                }
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
            if let Some(destination) = autolink_destination(inside) {
                contains_link = true;
                let start = out.len();
                out.push_str(inside);
                if nesting.image_description {
                    index = close + 1;
                    continue;
                }
                spans.push((
                    start..out.len(),
                    style.merged(InlineStyle {
                        link: true,
                        link_index: found.as_ref().map(|items| items.links.len()),
                        ..InlineStyle::default()
                    }),
                ));
                if let Some(found) = found.as_deref_mut() {
                    found.record_link_or_image(
                        found.runs.len(),
                        false,
                        references::Target::new(&destination),
                        at + index..at + close + 1,
                        at + index + 1..at + close,
                        inside.to_string(),
                    );
                }
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
    contains_link
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

fn inline_link_target(
    source: &str,
    open: usize,
    close: usize,
    found: Option<&Found>,
) -> Option<(usize, references::Target)> {
    if source.as_bytes().get(close + 1) == Some(&b'(')
        && let Some(end) = find_balanced_close(source, close + 1, b'(', b')')
    {
        return Some((
            end + 1,
            references::Target::authored(link_destination(&source[close + 2..end])),
        ));
    }
    found?
        .references
        .resolve(source, close, &source[open + 1..close])
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

/// CommonMark angle autolinks. An email address is displayed as authored, while
/// URI delimiters in its local part stay mailbox data in the mailto destination.
fn autolink_destination(value: &str) -> Option<Cow<'_, str>> {
    if matches!(link_policy(value), LinkPolicy::ExplicitExternal) {
        return Some(Cow::Borrowed(value));
    }
    let (local, domain) = value.split_once('@')?;
    if local.is_empty()
        || !local
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&byte))
        || !domain.split('.').all(|label| {
            let bytes = label.as_bytes();
            (1..=63).contains(&bytes.len())
                && bytes[0].is_ascii_alphanumeric()
                && bytes[bytes.len() - 1].is_ascii_alphanumeric()
                && bytes
                    .iter()
                    .all(|&byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return None;
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut uri = String::from("mailto:");
    for byte in local.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~!$'*+".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[usize::from(byte >> 4)]));
            uri.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    uri.push('@');
    uri.push_str(domain);
    Some(Cow::Owned(uri))
}

fn looks_like_html_tag(value: &str) -> bool {
    let candidate = value.trim_start_matches('/').trim_start_matches(['!', '?']);
    let bytes = candidate.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_alphabetic) {
        return false;
    }
    let name = bytes
        .iter()
        .take_while(|&&b| b.is_ascii_alphanumeric() || b == b'-')
        .count();
    matches!(&candidate[name..], "" | "/")
        || bytes.get(name).is_some_and(u8::is_ascii_whitespace)
        || value.starts_with('?') && value.ends_with('?')
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
            let hex = &value[2..];
            if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            char::from_u32(u32::from_str_radix(hex, 16).ok()?).filter(|&c| c != '\0')
        }
        value if value.starts_with('#') => {
            let dec = &value[1..];
            if dec.is_empty() || !dec.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            char::from_u32(dec.parse().ok()?).filter(|&c| c != '\0')
        }
        _ => None,
    }
}

fn setext_level(line: &str) -> Option<u8> {
    match line.as_bytes().first()? {
        b'=' if line.bytes().all(|b| b == b'=') => Some(1),
        b'-' if line.bytes().all(|b| b == b'-') => Some(2),
        _ => None,
    }
}

/// The level of the heading `line` and the range of `line` its text is: past the `#` run and
/// its space, trimmed, without a closing `#` run.
fn heading(line: &str) -> Option<(u8, Range<usize>)> {
    let count = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&count) || line.as_bytes().get(count) != Some(&b' ') {
        return None;
    }
    let rest = &line[count + 1..];
    let text = rest.trim_end_matches('#').trim();
    let start = count + 1 + (rest.len() - rest.trim_start().len());
    Some((count as u8, start..start + text.len()))
}

fn fence_start(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let marker = *bytes.first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let run = ascii_run(bytes, 0, marker);
    if run < 3 || (marker == b'`' && line[run..].contains('`')) {
        return None;
    }
    Some(&line[..run])
}

fn is_fence_close(line: &str, open_fence: &str) -> bool {
    let trimmed = line.trim();
    let marker = open_fence.as_bytes()[0];
    trimmed.len() >= open_fence.len() && trimmed.bytes().all(|byte| byte == marker)
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

fn table_cell_ranges(line: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let trimmed = line.trim();
    let offset = line.len() - line.trim_start().len();
    let start = usize::from(trimmed.starts_with('|'));
    let mut escaped = false;
    trimmed
        .bytes()
        .enumerate()
        .filter_map(move |(i, byte)| {
            let boundary = i >= start && byte == b'|' && !escaped;
            escaped = byte == b'\\' && !escaped;
            boundary.then_some(i)
        })
        .chain(std::iter::once(trimmed.len()))
        .scan(start, move |from, end| {
            if *from > end || *from == trimmed.len() && trimmed.ends_with('|') {
                return None;
            }
            let cell = &trimmed[*from..end];
            let start = offset + *from + cell.len() - cell.trim_start().len();
            *from = end + 1;
            Some(start..start + cell.trim().len())
        })
}

fn table_header(line: &str, separator: &str) -> bool {
    if !separator.contains('|')
        || !separator
            .trim()
            .bytes()
            .all(|b| matches!(b, b'-' | b':' | b'|' | b' ' | b'\t'))
    {
        return false;
    }
    let mut count = 0;
    for cell in table_cell_ranges(separator) {
        let marker = &separator[cell];
        let marker = marker.strip_prefix(':').unwrap_or(marker);
        let marker = marker.strip_suffix(':').unwrap_or(marker);
        if marker.is_empty() || !marker.bytes().all(|b| b == b'-') {
            return false;
        }
        count += 1;
    }
    count > 0 && table_cell_ranges(line).count() == count
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

#[cfg(test)]
#[path = "native_markdown_tests.rs"]
mod tests;
