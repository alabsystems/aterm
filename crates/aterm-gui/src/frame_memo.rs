// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PUBLISHED FRAME.

use std::sync::{Arc, Mutex};

use aterm_core::terminal::Terminal;
use aterm_types::MutexExt as _;

use crate::control::StyledFrameSnapshot;
use crate::frame_key::{FrameKey, TextKey};

/// The live screen's text.
pub(crate) struct ScreenText {
    key: TextKey,
    text: String,
    /// The byte offset of each row's `\n` in `text`.
    ends: Vec<usize>,
    fnv: u64,
}

impl ScreenText {
    /// `t`'s text now, built fresh; the caller holds its lock.
    pub(crate) fn of(t: &Terminal) -> ScreenText {
        Self::build(t, TextKey::read(t))
    }

    fn build(t: &Terminal, key: TextKey) -> ScreenText {
        let text = crate::control::screen_text(t);
        let ends = text.match_indices('\n').map(|(at, _)| at).collect();
        ScreenText {
            key,
            fnv: aterm_hash::fnv1a_64(text.as_bytes()),
            text,
            ends,
        }
    }

    /// The whole screen, `\n` after every row: the untrimmed body `text` serves.
    pub(crate) fn whole(&self) -> &str {
        &self.text
    }

    /// FNV-1a-64 of [`Self::whole`]: the screen's `hash=`.
    pub(crate) fn fnv(&self) -> u64 {
        self.fnv
    }

    /// The number of rows: the grid's row count when this was built.
    pub(crate) fn row_count(&self) -> usize {
        self.ends.len()
    }

    fn start(&self, r: usize) -> usize {
        match r {
            0 => 0,
            r => self.ends[r - 1] + 1,
        }
    }

    /// Row `r`, without its `\n`.
    pub(crate) fn row(&self, r: usize) -> &str {
        &self.text[self.start(r)..self.ends[r]]
    }

    /// Every row, top to bottom, without its `\n`.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        (0..self.row_count()).map(|r| self.row(r))
    }

    /// Rows `first..end`, each followed by its `\n`, as one slice of [`Self::whole`].
    pub(crate) fn span(&self, first: usize, end: usize) -> &str {
        if first >= end {
            return "";
        }
        &self.text[self.start(first)..=self.ends[end - 1]]
    }

    /// FNV-1a-64 of rows `first..=last`, each followed by `\n`.
    pub(crate) fn rows_hash(&self, first: usize, last: usize) -> Option<u64> {
        (first <= last && last < self.row_count())
            .then(|| aterm_hash::fnv1a_64(self.span(first, last + 1).as_bytes()))
    }
}

/// One serialized styled frame.
pub(crate) struct StyledFrame {
    key: FrameKey,
    body: String,
    id: (u64, usize),
    image_payloads: bool,
}

impl StyledFrame {
    /// Serialize `snap`, gathered under the hold that read `key`.
    pub(crate) fn of(key: FrameKey, snap: &StyledFrameSnapshot) -> StyledFrame {
        let body = crate::control::serialize_styled_frame(snap);
        StyledFrame {
            key,
            id: (aterm_hash::fnv1a_64(body.as_bytes()), body.len()),
            body,
            image_payloads: snap.carries_image_payloads(),
        }
    }

    /// Whether the body carries an image payload.
    pub(crate) fn carries_image_payloads(&self) -> bool {
        self.image_payloads
    }

    /// The frame's one-line JSON object.
    pub(crate) fn body(&self) -> &str {
        &self.body
    }

    /// `` of [`Self::body`], computed once per serialize: the push compares ids, never bodies.
    pub(crate) fn id(&self) -> (u64, usize) {
        self.id
    }
}

/// A [`Look::styled`] answer.
pub(crate) enum Styled {
    Hit(Arc<StyledFrame>),
    Miss(StyledFrameSnapshot),
}

/// One session's published text and styled frame.
#[derive(Default)]
pub(crate) struct FrameSlot {
    /// only to clone or store an `Arc` or move the cells count.
    inner: Mutex<Published>,
}

#[derive(Default)]
struct Published {
    text: Option<Arc<ScreenText>>,
    /// Kept only while `cells_watches` is nonzero.
    styled: Option<Arc<StyledFrame>>,
    /// Live [`CellsHold`]s.
    cells_watches: usize,
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
            built: None,
        }
    }

    /// Publish `text`, built by a reader that took no look.
    pub(crate) fn publish_text(&self, text: Arc<ScreenText>) {
        let old = self.inner.lock_or_recover().text.replace(text);
        drop(old);
    }

    /// Keep the styled frame for a `cells` watch, for as long as the hold lives.
    pub(crate) fn hold_cells(slot: &Arc<FrameSlot>) -> CellsHold {
        slot.inner.lock_or_recover().cells_watches += 1;
        CellsHold(Arc::clone(slot))
    }
}

/// What a [`FrameSlot`] held when a reader looked.
pub(crate) struct Look<'a> {
    slot: &'a FrameSlot,
    text: Option<Arc<ScreenText>>,
    styled: Option<Arc<StyledFrame>>,
    keeps_styled: bool,
    built: Option<Arc<ScreenText>>,
}

impl Look<'_> {
    /// `t`'s screen text.
    pub(crate) fn text(&mut self, t: &Terminal) -> Arc<ScreenText> {
        let key = TextKey::read(t);
        if let Some(text) = self
            .built
            .as_ref()
            .or(self.text.as_ref())
            .filter(|text| text.key == key)
        {
            return Arc::clone(text);
        }
        let text = Arc::new(ScreenText::build(t, key));
        self.built = Some(Arc::clone(&text));
        text
    }

    /// `t`'s styled frame under `key`, which the caller read under this same hold.
    pub(crate) fn styled(&self, t: &Terminal, key: &FrameKey) -> Styled {
        debug_assert_eq!(*key, FrameKey::read(t), "the key is read under this hold");
        match self.styled.as_ref().filter(|frame| frame.key == *key) {
            Some(frame) => Styled::Hit(Arc::clone(frame)),
            None => Styled::Miss(crate::control::gather_styled_frame(t)),
        }
    }

    /// Whether a `cells` watch held the slot when this look was taken.
    pub(crate) fn keeps_styled(&self) -> bool {
        self.keeps_styled
    }

    /// Serialize a miss's snapshot (no term guard held) and publish the frame while a cells.
    pub(crate) fn publish_styled(
        &mut self,
        key: FrameKey,
        snap: &StyledFrameSnapshot,
    ) -> Arc<StyledFrame> {
        let frame = Arc::new(StyledFrame::of(key, snap));
        let old = {
            let mut published = self.slot.inner.lock_or_recover();
            (published.cells_watches > 0 && same(&published.styled, &self.styled))
                .then(|| published.styled.replace(Arc::clone(&frame)))
        };
        drop(old);
        self.styled = Some(Arc::clone(&frame));
        frame
    }
}

impl Drop for Look<'_> {
    fn drop(&mut self) {
        if let Some(built) = self.built.take() {
            let old = {
                let mut published = self.slot.inner.lock_or_recover();
                same(&published.text, &self.text).then(|| published.text.replace(built))
            };
            drop(old);
        }
    }
}

/// Whether the slot still holds the build a look saw.
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
                published.styled.take()
            } else {
                None
            }
        };
        drop(freed);
    }
}

#[cfg(test)]
mod tests {
    use aterm_core::terminal::Terminal;

    use super::{FrameSlot, ScreenText};

    /// included: wide glyphs, a combining mark, trailing spaces and blank rows.
    #[test]
    fn screen_text_slices_are_the_rows_and_the_fence_hash() {
        let mut t = Terminal::new(5, 16);
        t.process("世界 e\u{301}   \r\n\r\ntrail   \r\n\x1b[7mx\x1b[0m".as_bytes());
        let text = ScreenText::of(&t);
        let rows = t.rows() as usize;
        assert_eq!(text.row_count(), rows);
        let mut joined = String::new();
        for r in 0..rows {
            assert_eq!(text.row(r), crate::control::visible_row(&t, r), "row {r}");
            joined.push_str(text.row(r));
            joined.push('\n');
        }
        assert_eq!(text.whole(), joined);
        assert_eq!(text.rows().count(), rows);
        for first in 0..=rows {
            for last in 0..=rows {
                assert_eq!(
                    text.rows_hash(first, last),
                    crate::control::rows_hash(&t, first, last),
                    "rows {first}..={last}"
                );
            }
            assert_eq!(text.span(first, first), "");
        }
        assert_eq!(
            text.span(1, 3),
            format!("{}\n{}\n", text.row(1), text.row(2))
        );
    }

    /// box B, the press-into-the-wrong-box case the `if-fp=` fence exists for.
    #[test]
    fn a_re_entered_alt_screen_at_the_same_seq_is_not_served_the_old_text() {
        let slot = FrameSlot::default();
        let mut t = Terminal::new(6, 30);
        t.process(b"\x1b[?1049h\x1b[H ls -la /tmp/work\r\n [y/n]\r\n");
        let box_a = slot.look_frame().text(&t);
        let seq_a = t.content_seq();
        t.process(b"\x1b[?1049l\x1b[?1049h\x1b[H rm -rf /tmp/work\r\n [y/n]\r\n");
        assert_eq!(
            t.content_seq(),
            seq_a,
            "the premise: both boxes read one seq"
        );
        let fresh = ScreenText::of(&t);
        assert_ne!(
            fresh.whole(),
            box_a.whole(),
            "the premise: the boxes differ"
        );
        assert_eq!(slot.look_frame().text(&t).whole(), fresh.whole());
        assert!(fresh.whole().contains("rm -rf"));
        let by_seq = (seq_a, box_a.whole().to_string());
        assert!(
            by_seq.0 == t.content_seq() && by_seq.1 != fresh.whole(),
            "a memo keyed on content_seq alone serves box A for box B"
        );
    }
}
