// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Memoized screen text and styled frame snapshots.

use std::sync::{Arc, Mutex};

use aterm_core::terminal::Terminal;
use aterm_types::MutexExt as _;

use crate::control::StyledFrameSnapshot;
use crate::frame_key::{FrameKey, TextKey};

/// The live screen's text.
pub(crate) struct ScreenText {
    key: TextKey,
    rows: LogicalRows,
    fnv: u64,
}

struct LogicalRows {
    text: String,
    /// The byte offset of each row's `\n` in `text`.
    ends: Vec<usize>,
}

impl LogicalRows {
    fn read(t: &Terminal, first: usize, end: usize) -> Self {
        let mut text = String::with_capacity((end - first) * (usize::from(t.cols()) + 1));
        let mut ends = Vec::with_capacity(end - first);
        for r in first..end {
            aterm_control::wire::visible_row_into(t, r, &mut text);
            ends.push(text.len());
            text.push('\n');
        }
        Self { text, ends }
    }

    fn start(&self, r: usize) -> usize {
        match r {
            0 => 0,
            r => self.ends[r - 1] + 1,
        }
    }

    fn row(&self, r: usize) -> &str {
        &self.text[self.start(r)..self.ends[r]]
    }

    fn span(&self, first: usize, end: usize) -> &str {
        if first >= end {
            return "";
        }
        &self.text[self.start(first)..=self.ends[end - 1]]
    }
}

impl ScreenText {
    /// `t`'s text now, built fresh; the caller holds its lock.
    pub(crate) fn of(t: &Terminal) -> ScreenText {
        Self::build(t, TextKey::read(t))
    }

    fn build(t: &Terminal, key: TextKey) -> ScreenText {
        let rows = LogicalRows::read(t, 0, usize::from(t.rows()));
        ScreenText {
            key,
            fnv: aterm_hash::fnv1a_64(rows.text.as_bytes()),
            rows,
        }
    }

    /// The whole screen, `\n` after every row: the untrimmed body `text` serves.
    pub(crate) fn whole(&self) -> &str {
        &self.rows.text
    }

    /// FNV-1a-64 of [`Self::whole`]: the screen's `hash=`.
    pub(crate) const fn fnv(&self) -> u64 {
        self.fnv
    }

    /// The number of rows: the grid's row count when this was built.
    pub(crate) const fn row_count(&self) -> usize {
        self.rows.ends.len()
    }

    /// Row `r`, without its `\n`.
    pub(crate) fn row(&self, r: usize) -> &str {
        self.rows.row(r)
    }

    /// Every row, top to bottom, without its `\n`.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        (0..self.row_count()).map(|r| self.row(r))
    }

    /// Rows `first..end`, each followed by its `\n`, as one slice of [`Self::whole`].
    pub(crate) fn span(&self, first: usize, end: usize) -> &str {
        self.rows.span(first, end)
    }

    /// FNV-1a-64 of rows `first..=last`, each followed by `\n`.
    pub(crate) fn rows_hash(&self, first: usize, last: usize) -> Option<u64> {
        (first <= last && last < self.row_count())
            .then(|| aterm_hash::fnv1a_64(self.span(first, last + 1).as_bytes()))
    }
}

struct CapturedRows {
    key: TextKey,
    first: usize,
    rows: LogicalRows,
}

#[derive(Clone)]
enum TextMemo {
    Whole(Arc<ScreenText>),
    Range(Arc<CapturedRows>),
}

impl TextMemo {
    fn key(&self) -> TextKey {
        match self {
            Self::Whole(text) => text.key,
            Self::Range(text) => text.key,
        }
    }

    fn covers(&self, first: usize, end: usize) -> bool {
        match self {
            Self::Whole(_) => true,
            Self::Range(text) => first >= text.first && end <= text.first + text.rows.ends.len(),
        }
    }
}

/// A selected span whose bytes can be read after the terminal guard is released.
pub(crate) struct TextSpan {
    memo: TextMemo,
    first: usize,
    end: usize,
}

impl TextSpan {
    fn selected(&self) -> (&LogicalRows, usize, usize) {
        let Self { memo, first, end } = self;
        match memo {
            TextMemo::Whole(text) => (&text.rows, *first, *end),
            TextMemo::Range(text) => (&text.rows, *first - text.first, *end - text.first),
        }
    }

    /// The selected logical rows, each followed by its newline.
    pub(crate) fn body(&self) -> &str {
        let (rows, first, end) = self.selected();
        rows.span(first, end)
    }

    /// The selected logical rows without their newlines.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = &str> + DoubleEndedIterator + '_ {
        let (rows, first, end) = self.selected();
        (first..end).map(move |r| rows.row(r))
    }
}

/// One serialized styled frame.
pub(crate) struct StyledFrame {
    key: FrameKey,
    body: String,
    id: (u64, usize),
    image_payloads: bool,
}

/// The most bytes a styled frame's body may hold: a `screen --json` reply's line cap, which a
/// `cells` push is held to as well.
pub(crate) const STYLED_FRAME_CAP: usize = aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES;

impl StyledFrame {
    /// Serialize `snap` within [`STYLED_FRAME_CAP`] ([`styled_body_within`]).
    pub(crate) fn within(key: FrameKey, snap: &StyledFrameSnapshot) -> Result<StyledFrame, usize> {
        styled_body_within(snap).map(|(body, payloads)| Self::with_body(key, body, payloads))
    }

    fn with_body(key: FrameKey, body: String, image_payloads: bool) -> StyledFrame {
        StyledFrame {
            key,
            id: (aterm_hash::fnv1a_64(body.as_bytes()), body.len()),
            body,
            image_payloads,
        }
    }

    /// Whether the body carries an image payload.
    pub(crate) const fn carries_image_payloads(&self) -> bool {
        self.image_payloads
    }

    /// The frame's one-line JSON object.
    pub(crate) fn body(&self) -> &str {
        &self.body
    }

    /// `(FNV-1a-64, length)` of [`Self::body`], computed once per serialize: the push compares
    /// ids, never bodies.
    pub(crate) const fn id(&self) -> (u64, usize) {
        self.id
    }
}

/// Serialize `snap` within [`STYLED_FRAME_CAP`]: with its image payloads when they fit (and
/// whether it carries them), else without them, else `Err` with the bytes it reached before it
/// stopped.
pub(crate) fn styled_body_within(snap: &StyledFrameSnapshot) -> Result<(String, bool), usize> {
    let payloads = snap.carries_image_payloads();
    match crate::control::serialize_styled_frame_within(snap, payloads, STYLED_FRAME_CAP) {
        Ok(body) => Ok((body, payloads)),
        Err(_) if payloads => {
            crate::control::serialize_styled_frame_within(snap, false, STYLED_FRAME_CAP)
                .map(|body| (body, false))
        }
        Err(reached) => Err(reached),
    }
}

/// A [`Look::styled`] answer.
pub(crate) enum Styled {
    Hit(Arc<StyledFrame>),
    Miss(StyledFrameSnapshot),
}

/// The most `cells` watches one session serves at once ([`FrameSlot::hold_cells`]).
pub(crate) const MAX_CELLS_WATCHES: usize = 16;

/// One session's published text and styled frame.
#[derive(Default)]
pub(crate) struct FrameSlot {
    /// A STRICT LEAF, never nested with the term lock in either order: held
    /// only to clone or store an `Arc` or move the cells count.
    inner: Mutex<Published>,
    /// A `cells` watch's turn to read (and on a miss build) the styled frame, taken for its
    /// content pass before the term lock and never under it, so watches of one session that
    /// miss together build its styled frame once: each after the first finds it published.
    pub(crate) build: Mutex<()>,
}

#[derive(Default)]
struct Published {
    text: Option<TextMemo>,
    /// Kept only while `cells_watches` is nonzero.
    styled: Option<Arc<StyledFrame>>,
    /// Live [`CellsHold`]s.
    cells_watches: usize,
    /// The key of the last styled frame a `cells` watch found past [`STYLED_FRAME_CAP`], and the
    /// bytes it reached; kept only while `cells_watches` is nonzero.
    refused: Option<(FrameKey, usize)>,
}

impl FrameSlot {
    /// What the slot holds now.
    pub(crate) fn look_frame(&self) -> Look<'_> {
        let published = self.inner.lock_or_recover();
        Look {
            slot: self,
            text: published.text.clone(),
            styled: published.styled.clone(),
            keeps_styled: published.cells_watches > 0,
            refused: published.refused,
            built: None,
        }
    }

    /// Publish `text`, built by a reader that took no look.
    pub(crate) fn publish_text(&self, text: Arc<ScreenText>) {
        let old = self
            .inner
            .lock_or_recover()
            .text
            .replace(TextMemo::Whole(text));
        drop(old);
    }

    /// Keep the styled frame for a `cells` watch, for as long as the hold lives; `None` when the
    /// session already serves [`MAX_CELLS_WATCHES`].
    pub(crate) fn hold_cells(slot: &Arc<FrameSlot>) -> Option<CellsHold> {
        let mut published = slot.inner.lock_or_recover();
        if published.cells_watches >= MAX_CELLS_WATCHES {
            return None;
        }
        published.cells_watches += 1;
        Some(CellsHold(Arc::clone(slot)))
    }
}

/// What a [`FrameSlot`] held when a reader looked.
pub(crate) struct Look<'a> {
    slot: &'a FrameSlot,
    text: Option<TextMemo>,
    styled: Option<Arc<StyledFrame>>,
    keeps_styled: bool,
    refused: Option<(FrameKey, usize)>,
    built: Option<TextMemo>,
}

impl Look<'_> {
    fn matching_text(&self, key: TextKey) -> Option<&TextMemo> {
        self.built
            .as_ref()
            .or(self.text.as_ref())
            .filter(|text| text.key() == key)
    }

    /// `t`'s screen text.
    pub(crate) fn text(&mut self, t: &Terminal) -> Arc<ScreenText> {
        let key = TextKey::read(t);
        if let Some(TextMemo::Whole(text)) = self.matching_text(key) {
            return Arc::clone(text);
        }
        let text = Arc::new(ScreenText::build(t, key));
        self.built = Some(TextMemo::Whole(Arc::clone(&text)));
        text
    }

    /// The logical rows in an already validated range. A miss visits only those rows.
    pub(crate) fn text_span(&mut self, t: &Terminal, first: usize, end: usize) -> TextSpan {
        if first == 0 && end == usize::from(t.rows()) {
            return TextSpan {
                memo: TextMemo::Whole(self.text(t)),
                first,
                end,
            };
        }
        let key = TextKey::read(t);
        let memo = if let Some(text) = self
            .matching_text(key)
            .filter(|text| text.covers(first, end))
        {
            text.clone()
        } else {
            let text = TextMemo::Range(Arc::new(CapturedRows {
                key,
                first,
                rows: LogicalRows::read(t, first, end),
            }));
            self.built = Some(text.clone());
            text
        };
        TextSpan { memo, first, end }
    }

    /// `t`'s styled frame under `key`, which the caller read under this same hold.
    pub(crate) fn styled(&self, t: &Terminal, key: &FrameKey) -> Styled {
        debug_assert_eq!(*key, FrameKey::read(t), "the key is read under this hold");
        match self.styled.as_ref().filter(|frame| frame.key == *key) {
            Some(frame) => Styled::Hit(Arc::clone(frame)),
            None => Styled::Miss(crate::control::gather_styled_frame(t)),
        }
    }

    /// The styled frame the slot held when this look was taken.
    #[cfg(test)]
    pub(crate) fn styled_for_test(&self) -> Option<&Arc<StyledFrame>> {
        self.styled.as_ref()
    }

    /// The bytes the styled frame under `key` reached before a `cells` watch refused it at
    /// [`STYLED_FRAME_CAP`], when the slot held that refusal as this look was taken.
    pub(crate) fn refused(&self, key: &FrameKey) -> Option<usize> {
        self.refused
            .filter(|(refused, _)| refused == key)
            .map(|(_, reached)| reached)
    }

    /// Whether a `cells` watch held the slot when this look was taken.
    pub(crate) const fn keeps_styled(&self) -> bool {
        self.keeps_styled
    }

    /// Serialize a miss's snapshot (no term guard held) within [`STYLED_FRAME_CAP`]
    /// ([`StyledFrame::within`]) and publish the frame while a cells watch holds the slot, or
    /// `Err` with the bytes it reached, the refusal kept under `key` while a cells watch holds
    /// the slot ([`Self::refused`]).
    pub(crate) fn publish_styled_within(
        &mut self,
        key: FrameKey,
        snap: &StyledFrameSnapshot,
    ) -> Result<Arc<StyledFrame>, usize> {
        match StyledFrame::within(key, snap) {
            Ok(frame) => {
                let frame = Arc::new(frame);
                self.publish(&frame);
                Ok(frame)
            }
            Err(reached) => {
                let mut published = self.slot.inner.lock_or_recover();
                if published.cells_watches > 0 {
                    published.refused = Some((key, reached));
                }
                Err(reached)
            }
        }
    }

    fn publish(&mut self, frame: &Arc<StyledFrame>) {
        let old = {
            let mut published = self.slot.inner.lock_or_recover();
            (published.cells_watches > 0
                && frame.body().len() <= STYLED_FRAME_CAP
                && same(&published.styled, &self.styled))
            .then(|| published.styled.replace(Arc::clone(frame)))
        };
        drop(old);
        self.styled = Some(Arc::clone(frame));
    }
}

impl Drop for Look<'_> {
    fn drop(&mut self) {
        if let Some(built) = self.built.take() {
            let old = {
                let mut published = self.slot.inner.lock_or_recover();
                let same_publication = match (published.text.as_ref(), self.text.as_ref()) {
                    (Some(TextMemo::Whole(now)), Some(TextMemo::Whole(seen))) => {
                        Arc::ptr_eq(now, seen)
                    }
                    (Some(TextMemo::Range(now)), Some(TextMemo::Range(seen))) => {
                        Arc::ptr_eq(now, seen)
                    }
                    (None, None) => true,
                    _ => false,
                };
                let promotes = matches!(
                    (&published.text, &built),
                    (Some(TextMemo::Range(now)), TextMemo::Whole(whole)) if now.key == whole.key
                );
                if same_publication || promotes {
                    published.text.replace(built)
                } else {
                    Some(built)
                }
            };
            drop(old);
        }
    }
}

fn same<T>(now: &Option<Arc<T>>, seen: &Option<Arc<T>>) -> bool {
    match (now, seen) {
        (Some(now), Some(seen)) => Arc::ptr_eq(now, seen),
        (None, None) => true,
        _ => false,
    }
}

/// A `cells` watch's hold on its session's styled frame.
pub(crate) struct CellsHold(Arc<FrameSlot>);

impl Drop for CellsHold {
    fn drop(&mut self) {
        let freed = {
            let mut published = self.0.inner.lock_or_recover();
            published.cells_watches = published.cells_watches.saturating_sub(1);
            if published.cells_watches == 0 {
                published.refused = None;
                published.styled.take()
            } else {
                None
            }
        };
        drop(freed);
    }
}

#[cfg(test)]
#[path = "frame_memo_tests.rs"]
mod tests;
