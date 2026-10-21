// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PUBLISHED FRAME: each session's screen text and styled frame, built
//! once per change of what they are a function of, and served from one slot
//! to every reader of that session.
//!
//! The visible rows with their hash, and the styled frame, are views derived
//! from the cells. Every reader used to rebuild them under the term lock:
//! `text`, `screen`, `status hash=`, `turn`, the `if-rows=` and `if-fp=`
//! fences, the agent sweep, `offscreen screen=1`, `cast drift`, the operator's
//! evidence, and each subscriber for each change it pushed. A [`FrameSlot`] on
//! the session's context keeps the last build of each, keyed by what it is a
//! function of: the [`TextKey`] for the text, the [`FrameKey`] for the styled
//! frame. A reader whose key, read under its own hold of the term lock, equals
//! the published one serves the published build. That build is the frame OF
//! THAT KEY, so the seq, cursor and frame a reply pairs still describe one
//! instant (R6).
//!
//! THE TWO LOCKS NEVER NEST. A reader takes a [`Look`] (the slot's `Arc`s,
//! cloned under the slot's mutex) BEFORE it takes the term lock. Under the
//! term lock it compares keys and, on a miss, builds. The look publishes what
//! it built when it drops, after the term guard is gone; a styled miss is
//! serialized and published once the guard is gone
//! ([`Look::publish_styled`]). The slot's mutex is held only to clone or store
//! an `Arc` or move the cells count: never across a row build, a gather or a
//! serialize, and nothing is locked under it. The lock-order census follows
//! one hop and does not see a call through a field, so this rule is stated
//! here, not checked there.
//!
//! RETENTION. The text (about rows × cols bytes) stays once built. The styled
//! frame (about 180 bytes a cell, plus the base64 of every image on screen) is
//! kept only while a `subscribe … cells` watch holds the slot ([`CellsHold`]).
//! A `screen` read serves a published frame but stores none of its own, so a
//! single read never pins a frame for the session's life.

use std::sync::{Arc, Mutex};

use aterm_core::terminal::Terminal;
use aterm_types::MutexExt as _;

use crate::control::StyledFrameSnapshot;
use crate::frame_key::{FrameKey, TextKey};

/// The live screen's text: every row as [`crate::control::visible_row`]
/// serves it, each followed by `\n`, as [`crate::control::screen_text`] (THE
/// ONE CONSTRUCTION) builds it, with each row's end and the FNV-1a-64 of the
/// whole. `status hash=`, `turn hash=`, `history`, the `if-fp=` and `if-rows=`
/// fences and the `text` rows are all read off it, so they cannot disagree
/// about a trailing space.
pub(crate) struct ScreenText {
    key: TextKey,
    text: String,
    /// The byte offset of each row's `\n` in `text`.
    ends: Vec<usize>,
    fnv: u64,
}

impl ScreenText {
    /// `t`'s text now, built fresh; the caller holds its lock. For a reader
    /// with no session slot to share: a replay engine, a test.
    pub(crate) fn of(t: &Terminal) -> ScreenText {
        Self::build(t, TextKey::read(t))
    }

    fn build(t: &Terminal, key: TextKey) -> ScreenText {
        let text = crate::control::screen_text(t);
        // A row holds no `\n` (`visible_row` collapses every control char), so
        // the newlines are exactly the row ends.
        let ends = text.match_indices('\n').map(|(at, _)| at).collect();
        ScreenText {
            key,
            fnv: aterm_hash::fnv1a_64(text.as_bytes()),
            text,
            ends,
        }
    }

    /// The whole screen, `\n` after every row: the untrimmed body `text`
    /// serves.
    pub(crate) fn whole(&self) -> &str {
        &self.text
    }

    /// FNV-1a-64 of [`Self::whole`]: the screen's `hash=` (`status`, `turn`,
    /// `history`, and what `if-fp=` names).
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

    /// Row `r`, without its `\n`. Panics past the last row, as a slice does.
    pub(crate) fn row(&self, r: usize) -> &str {
        &self.text[self.start(r)..self.ends[r]]
    }

    /// Every row, top to bottom, without its `\n`.
    pub(crate) fn rows(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        (0..self.row_count()).map(|r| self.row(r))
    }

    /// Rows `first..end`, each followed by its `\n`, as one slice of
    /// [`Self::whole`]; empty when `first >= end`. Panics past the last row.
    pub(crate) fn span(&self, first: usize, end: usize) -> &str {
        if first >= end {
            return "";
        }
        &self.text[self.start(first)..=self.ends[end - 1]]
    }

    /// FNV-1a-64 of rows `first..=last`, each followed by `\n`: the
    /// `turn if-rows=` fence's value by the one definition both ends share
    /// ([`aterm_types::control_verbs::RowsFence::hash_rows`]), the screen's own
    /// `hash=` over just those rows. `None` for an empty range or one past the
    /// grid: the fence then reads the screen as moved.
    pub(crate) fn rows_hash(&self, first: usize, last: usize) -> Option<u64> {
        (first <= last && last < self.row_count())
            .then(|| aterm_hash::fnv1a_64(self.span(first, last + 1).as_bytes()))
    }
}

/// One serialized styled frame: [`crate::control::serialize_styled_frame`]'s
/// body (no etag key), the [`FrameKey`] it is the frame of, its id, and whether
/// the body carries an image payload.
pub(crate) struct StyledFrame {
    key: FrameKey,
    body: String,
    id: (u64, usize),
    image_payloads: bool,
}

impl StyledFrame {
    /// Serialize `snap`, gathered under the hold that read `key`. No lock is
    /// held here: this is the per-cell JSON and the base64 of every image.
    pub(crate) fn of(key: FrameKey, snap: &StyledFrameSnapshot) -> StyledFrame {
        let body = crate::control::serialize_styled_frame(snap);
        StyledFrame {
            key,
            id: (aterm_hash::fnv1a_64(body.as_bytes()), body.len()),
            body,
            image_payloads: snap.carries_image_payloads(),
        }
    }

    /// Whether the body carries an image payload, which a `screen` reply past the
    /// line cap leaves out ([`crate::control::cmd_screen_styled_json`]).
    pub(crate) fn carries_image_payloads(&self) -> bool {
        self.image_payloads
    }

    /// The frame's one-line JSON object.
    pub(crate) fn body(&self) -> &str {
        &self.body
    }

    /// `(FNV-1a-64, length)` of [`Self::body`], computed once per serialize:
    /// the push compares ids, never bodies.
    pub(crate) fn id(&self) -> (u64, usize) {
        self.id
    }
}

/// A [`Look::styled`] answer: the published frame of the live key, or the
/// snapshot a miss gathered under the hold, for [`Look::publish_styled`] to
/// serialize once the guard is gone.
pub(crate) enum Styled {
    Hit(Arc<StyledFrame>),
    Miss(StyledFrameSnapshot),
}

/// One session's published text and styled frame. A field of the session's
/// context, cloned into each subscribe watch; see the module doc for the lock
/// rule and what is kept.
#[derive(Default)]
pub(crate) struct FrameSlot {
    /// A STRICT LEAF, never nested with the term lock in either order: held
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
    /// What the slot holds now. Take it BEFORE the term lock: the look is
    /// compared and filled under it, and publishes what it built when it drops.
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

    /// Publish `text`, built by a reader that took no look: the status sweep,
    /// whose contended session must cost one `try_lock` and nothing else.
    /// Call it with no term guard held.
    pub(crate) fn publish_text(&self, text: Arc<ScreenText>) {
        let old = self.inner.lock_or_recover().text.replace(text);
        drop(old);
    }

    /// Keep the styled frame for a `cells` watch, for as long as the hold
    /// lives.
    pub(crate) fn hold_cells(slot: &Arc<FrameSlot>) -> CellsHold {
        slot.inner.lock_or_recover().cells_watches += 1;
        CellsHold(Arc::clone(slot))
    }
}

/// What a [`FrameSlot`] held when a reader looked, and what the reader built
/// under its hold of the term lock. Dropping it publishes the text it built,
/// unless another reader published since the look: the slot is never rolled
/// back to an older build by a slower reader.
pub(crate) struct Look<'a> {
    slot: &'a FrameSlot,
    text: Option<Arc<ScreenText>>,
    styled: Option<Arc<StyledFrame>>,
    keeps_styled: bool,
    built: Option<Arc<ScreenText>>,
}

impl Look<'_> {
    /// `t`'s screen text: the published one when its key is `t`'s key now,
    /// else built under this hold. The caller holds `t`'s lock.
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

    /// `t`'s styled frame under `key`, which the caller read under this same
    /// hold: the published frame when it is that key's, else the snapshot
    /// gathered now.
    pub(crate) fn styled(&self, t: &Terminal, key: &FrameKey) -> Styled {
        debug_assert_eq!(*key, FrameKey::read(t), "the key is read under this hold");
        match self.styled.as_ref().filter(|frame| frame.key == *key) {
            Some(frame) => Styled::Hit(Arc::clone(frame)),
            None => Styled::Miss(crate::control::gather_styled_frame(t)),
        }
    }

    /// Whether a `cells` watch held the slot when this look was taken: a
    /// reader that would only copy a frame nobody keeps writes it straight
    /// into its reply instead.
    pub(crate) fn keeps_styled(&self) -> bool {
        self.keeps_styled
    }

    /// Serialize a miss's snapshot (no term guard held) and publish the frame
    /// while a `cells` watch holds the slot and no newer frame was published
    /// since the look.
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
        // The replaced frame and its images are freed with the slot's mutex
        // released.
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

/// A `cells` watch's hold on its session's styled frame: while one lives, the
/// slot keeps the frames readers publish, and the last to drop frees the frame.
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
        // A frame and its images are freed with the slot's mutex released.
        drop(freed);
    }
}

#[cfg(test)]
mod tests {
    use aterm_core::terminal::Terminal;

    use super::{FrameSlot, ScreenText};

    /// The text's slices are the rows `text` serves, and its fence hash is the
    /// row-by-row `rows_hash` over every span, the empty and out-of-grid ones
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

    /// A RE-ENTERED ALTERNATE SCREEN AT THE SAME SEQ IS NOT SERVED THE OLD
    /// TEXT. Box A is drawn on the alternate screen and its text published;
    /// the program leaves and re-enters it and draws box B, which reads the
    /// same `content_seq` (the premise). The slot serves box B. NEGATIVE
    /// CONTROL: a memo keyed on `content_seq` alone serves box A's text for
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
        // The control: the seq that keyed box A's build keys box B's read.
        let by_seq = (seq_a, box_a.whole().to_string());
        assert!(
            by_seq.0 == t.content_seq() && by_seq.1 != fresh.whole(),
            "a memo keyed on content_seq alone serves box A for box B"
        );
    }
}
