// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE DEFINITION OF "THE SCREEN CHANGED": what the live screen a reader
//! is served is a function of, read once under the term lock.
//!
//! Three readers ask it. The conditional `screen since=<etag>` read answers
//! `unchanged` when [`FrameKey::etag`] is the one the poller holds, and the
//! `subscribe` push asks one question per stream: `screen` of the
//! [`TextKey`] (the visible text), `cells` of the whole [`FrameKey`] (when to
//! re-read the styled frame, which is sent when its bytes changed). The push
//! gate used to key every stream on `content_seq`, the alt flag and the render
//! signature at once, so it missed each frame change that writes no content (a
//! `C=1` Kitty placement, a VS15/VS16 width fix, an OSC 1337 mark, an image
//! re-sent under its placeholder) while the conditional read caught them, and
//! it re-sent the screen text on every caret move, selection drag and recolour.
//! The third, a host's own hold of a session's terminal ([`crate::TermGuard`]),
//! compares the [`FrameKey`] at its start and end to decide whether to wake
//! those push streams, so a write the PTY reader did not make reaches them
//! exactly when one of their questions could answer yes.
//!
//! Every field is read off the live screen; the viewport is not an input, so
//! a person scrolling back through history moves neither key.

use aterm_core::terminal::Terminal;

use crate::control::ScreenGen;

/// Everything the live screen's TEXT is a function of: every row as
/// [`crate::control::visible_row`] serves it, which is what the `text` verb,
/// the screen's `hash=` and a `subscribe … screen` frame carry.
///
/// * The live grid: output reaches it only through `process`, and
///   [`Terminal::screen_mutations`] moves on every non-empty batch. The host's
///   own grid writers (resize, reset, clearing history) move the content seq,
///   a reset the epoch.
/// * The generation (`epoch`, `seq`): the seq repeats across an alt-screen
///   exit and re-entry, the epoch never does.
/// * The dimensions.
/// * The terminal ([`Terminal::render_identity`], not its reusable address),
///   so another terminal behind the same handle is another key. Identities
///   start at 1, so the [`Default`] key (all zero) is no terminal's.
///
/// It is finer than the text: a caret move or an OSC colour change is a batch
/// that moves `screen_mutations` and no glyph. A reader that must not resend
/// unchanged text compares the text when this moves (the `screen` stream
/// compares the screen's `hash=`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct TextKey {
    identity: u64,
    epoch: u64,
    seq: u64,
    mutations: u64,
    rows: u16,
    cols: u16,
}

impl TextKey {
    /// The key of `t` now. The caller holds its lock; O(1).
    pub(crate) fn read(t: &Terminal) -> TextKey {
        TextKey {
            identity: t.render_identity(),
            epoch: t.content_scroll_state().invalidation_epoch,
            seq: t.content_seq(),
            mutations: t.screen_mutations(),
            rows: t.rows(),
            cols: t.cols(),
        }
    }
}

/// Everything the STYLED FRAME ([`crate::control::gather_styled_frame`]) is a
/// function of. The argument goes read by read through the gather (the
/// serializer reads only its snapshot); each read is folded by value, or
/// reached only by writers that move a folded counter:
///
/// * The [`TextKey`]: the grid's glyphs, wide flags, links, decorations and
///   image placements, and the Kitty image store, are output, which moves
///   `screen_mutations`, as does every Kitty animation step. `content_seq`
///   alone misses a VS15/VS16 width fix, an OSC 1337 mark, a `C=1` Kitty
///   placement and a Kitty image re-sent or deleted under a placeholder.
/// * The alt flag the frame reports.
/// * By value through [`render_sig`]: the cursor, the selection and its
///   colours, and everything a cell is coloured against
///   ([`Terminal::presentation_fingerprint`]: palette, default colours,
///   DECSCNM and the host style policy, which a config reload changes with no
///   output and no seq while every bold and dim cell re-colours).
/// * Each row's DEC line size, by the name the frame serializes.
///
/// It is finer than the frame (every batch of output moves `screen_mutations`,
/// a title or a bell included); that costs one full frame read, never a wrong
/// answer, and the push sends a re-read frame only when its bytes changed.
/// `control_screen_etag_tests.rs` holds [`Self::etag`] to the frame over a
/// corpus and a seeded walk, with incomplete etags as negative controls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct FrameKey {
    text: TextKey,
    alt: bool,
    render: u64,
    line_sizes: u64,
}

impl FrameKey {
    /// The key of `t` now. The caller holds its lock; O(rows) for the line
    /// sizes, plus [`render_sig`].
    pub(crate) fn read(t: &Terminal) -> FrameKey {
        // One stable wire name per row (the names the frame serializes),
        // delimited so two rows cannot alias by concatenation.
        let mut line_sizes = aterm_hash::Fnv1a64::new();
        for r in 0..t.rows() as usize {
            let size = crate::control::row_line_size(t, r);
            line_sizes.write(crate::control::line_size_name(size).as_bytes());
            line_sizes.write_u8(0);
        }
        FrameKey {
            text: TextKey::read(t),
            alt: t.is_alternate_screen(),
            render: render_sig(t),
            line_sizes: line_sizes.finish(),
        }
    }

    /// The text half: equal whenever the visible rows are.
    pub(crate) fn text(&self) -> TextKey {
        self.text
    }

    /// The content seq the key was read at.
    pub(crate) fn seq(&self) -> u64 {
        self.text.seq
    }

    /// The screen generation the key was read at (`gen=` on the wire).
    pub(crate) fn generation(&self) -> ScreenGen {
        ScreenGen {
            epoch: self.text.epoch,
            seq: self.text.seq,
        }
    }

    /// The `screen` frame's CONDITIONAL ETAG: every field of the key, and this
    /// process, folded into one `u64`. The process salt keeps a token handed
    /// out by another process (an earlier aterm, a peer instance) from
    /// matching here.
    pub(crate) fn etag(&self) -> u64 {
        static PROCESS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let process = *PROCESS.get_or_init(|| {
            let nanos = aterm_time::since_epoch().map_or(0, |d| d.as_nanos() as u64);
            nanos ^ (u64::from(std::process::id()) << 32)
        });
        let TextKey {
            identity,
            epoch,
            seq,
            mutations,
            rows,
            cols,
        } = self.text;
        let mut h = aterm_hash::Fnv1a64::new();
        for v in [
            process,
            identity,
            epoch,
            seq,
            mutations,
            u64::from(rows),
            u64::from(cols),
            u64::from(self.alt),
            self.render,
            self.line_sizes,
        ] {
            h.write(&v.to_le_bytes());
        }
        h.finish()
    }
}

/// A cheap fingerprint of the RENDER state that changes the styled output
/// without advancing `content_seq`: the overlays — DECTCEM visibility, cursor
/// position/style/colour, the side-adjusted selection kind and range, the
/// selection colours — and everything a cell's colours are RESOLVED against,
/// taken from aterm-core's own list ([`Terminal::presentation_fingerprint`]:
/// the palette, the default colours, DECSCNM and the host style policy, which a
/// config reload changes with no output and no seq). Bounded (one 256-entry
/// palette pass + a few dynamics), and read only for a [`FrameKey`].
pub(crate) fn render_sig(t: &Terminal) -> u64 {
    let mut h = aterm_hash::Fnv1a64::new();
    h.write_u8(u8::from(t.cursor_visible()));
    let cursor = t.cursor();
    h.write(&cursor.row.to_le_bytes());
    h.write(&cursor.col.to_le_bytes());
    h.write(crate::control::cursor_style_name(t.cursor_style()).as_bytes());
    // Delimit the variable-length style name from the next field.
    h.write_u8(0);
    for color in [
        t.cursor_color(),
        t.selection_background(),
        t.selection_foreground(),
    ] {
        match color {
            Some(c) => h.write(&[1, c.r, c.g, c.b]),
            None => h.write_u8(0),
        }
    }
    let selection = t.text_selection();
    match selection.project_range(t.cols().saturating_sub(1)) {
        Some(projected) => {
            let kind = match selection.selection_type() {
                aterm_core::selection::SelectionType::Simple => 0,
                aterm_core::selection::SelectionType::Block => 1,
                aterm_core::selection::SelectionType::Semantic => 2,
                aterm_core::selection::SelectionType::Lines => 3,
                _ => u8::MAX,
            };
            h.write(&[1, kind, u8::from(projected.is_block)]);
            h.write(&projected.start_row.to_le_bytes());
            h.write(&projected.start_col.to_le_bytes());
            h.write(&projected.end_row.to_le_bytes());
            h.write(&projected.end_col.to_le_bytes());
        }
        None => h.write_u8(0),
    }
    h.write(&t.presentation_fingerprint().to_le_bytes());
    h.finish()
}
