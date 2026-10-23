// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Screen and styled-frame change keys (`TextKey` and `FrameKey`) for conditional reads and `subscribe` push streams.

use aterm_core::terminal::Terminal;

use crate::control::ScreenGen;

/// Fingerprint of everything the visible screen text depends on.
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
    /// Read the current [`TextKey`] of `t` under its lock in O(1).
    pub(crate) const fn read(t: &Terminal) -> TextKey {
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

/// Fingerprint of everything the styled `screen` frame depends on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct FrameKey {
    text: TextKey,
    alt: bool,
    render: u64,
    line_sizes: u64,
}

impl FrameKey {
    /// Read the current [`FrameKey`] of `t` under its lock.
    pub(crate) fn read(t: &Terminal) -> FrameKey {
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

    /// Return the visible-text key subset.
    pub(crate) const fn text(&self) -> TextKey {
        self.text
    }

    /// Content sequence number at the read.
    pub(crate) const fn seq(&self) -> u64 {
        self.text.seq
    }

    /// Screen generation (`gen=`) at the read.
    pub(crate) const fn generation(&self) -> ScreenGen {
        ScreenGen {
            epoch: self.text.epoch,
            seq: self.text.seq,
        }
    }

    /// Fold all frame-key fields and a per-process salt into a conditional `etag` `u64`.
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

/// Fingerprint render-state overlays that change styled output without advancing `content_seq`.
pub(crate) fn render_sig(t: &Terminal) -> u64 {
    let mut h = aterm_hash::Fnv1a64::new();
    h.write_u8(u8::from(t.cursor_visible()));
    let cursor = t.cursor();
    h.write(&cursor.row.to_le_bytes());
    h.write(&cursor.col.to_le_bytes());
    h.write(crate::control::cursor_style_name(t.cursor_style()).as_bytes());
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
