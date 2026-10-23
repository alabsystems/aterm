// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/lib.rs, src/hb/shape.rs and
// src/hb/buffer.rs (`UnicodeBuffer`, `GlyphBuffer`) and modified by the aterm
// project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! aterm's text shaper, carved from rustybuzz 0.20.1 to the surface a terminal reaches.

mod apply;
mod arabic;
mod arabic_table;
mod buffer;
mod common;
mod digest;
mod face;
mod gsub;
mod layout;
mod map;
mod pipeline;
mod plan;
mod unicode;

pub use buffer::{GlyphInfo, GlyphPosition};
pub use common::Direction;
pub use face::ShapeFace;
pub use plan::{Feature, ShapePlan};

use aterm_grapheme::Script;
use buffer::Buffer;

/// Text going into the shaper.
pub struct UnicodeBuffer(Buffer);

impl Default for UnicodeBuffer {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl UnicodeBuffer {
    /// An empty buffer.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        UnicodeBuffer(Buffer::new())
    }

    /// The number of characters.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len
    }

    /// Whether there are none.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.len == 0
    }

    /// Appends `text`, each character's cluster its UTF-8 byte offset in it.
    #[inline]
    pub fn push_str(&mut self, text: &str) {
        self.0.push_str(text);
    }

    /// Appends one character with the cluster value `cluster`.
    #[inline]
    pub fn add(&mut self, c: char, cluster: u32) {
        self.0.add(c as u32, cluster);
    }

    /// Sets the direction (left unset, [`Self::guess_segment_properties`] takes the script's).
    #[inline]
    pub fn set_direction(&mut self, direction: Direction) {
        self.0.direction = direction;
    }

    /// The direction, [`Direction::Invalid`] until set or guessed.
    #[inline]
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.0.direction
    }

    /// The script, if set or guessed and the text has one.
    #[inline]
    #[must_use]
    pub const fn script(&self) -> Option<Script> {
        self.0.script
    }

    /// Sets the script to the first that is not Common, Inherited or Unknown, and the direction to
    /// the one it is written in, for whichever of the two is unset.
    #[inline]
    pub fn guess_segment_properties(&mut self) {
        self.0.guess_segment_properties();
    }

    /// Empties the buffer and its segment properties, keeping its storage.
    #[inline]
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// Glyphs coming out of the shaper.
pub struct GlyphBuffer(Buffer);

impl GlyphBuffer {
    /// The number of glyphs.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len
    }

    /// Whether there are none.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.len == 0
    }

    /// The glyphs, in visual order (left to right for a right-to-left run).
    #[inline]
    #[must_use]
    pub fn glyph_infos(&self) -> &[GlyphInfo] {
        self.0.info.get(..self.0.len).unwrap_or(&[])
    }

    /// Their positions, parallel to [`Self::glyph_infos`].
    #[inline]
    #[must_use]
    pub fn glyph_positions(&self) -> &[GlyphPosition] {
        self.0.pos.get(..self.0.len).unwrap_or(&[])
    }

    /// Empties the buffer and hands its storage back for the next run.
    #[inline]
    #[must_use]
    pub fn clear(mut self) -> UnicodeBuffer {
        self.0.clear();
        UnicodeBuffer(self.0)
    }
}

/// Shapes `buffer` on `face` with `features`, guessing its segment properties and building a plan
/// for them.
#[inline]
pub fn shape(face: &ShapeFace, features: &[Feature], mut buffer: UnicodeBuffer) -> GlyphBuffer {
    buffer.guess_segment_properties();
    let plan = ShapePlan::new(face, buffer.0.direction, buffer.0.script, features);
    shape_with_plan(face, &plan, buffer)
}

/// Shapes `buffer` on `face` with `plan`, which must have been built for the buffer's direction and
/// script (else the plan's are used).
#[inline]
pub fn shape_with_plan(face: &ShapeFace, plan: &ShapePlan, buffer: UnicodeBuffer) -> GlyphBuffer {
    let mut buffer = buffer.0;
    buffer.guess_segment_properties();
    buffer.direction = plan.direction;
    buffer.script = plan.script;
    if buffer.len > 0 {
        pipeline::shape_internal(plan, face, &mut buffer);
    }
    GlyphBuffer(buffer)
}
