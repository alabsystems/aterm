// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/lib.rs, src/hb/shape.rs and
// src/hb/buffer.rs (`UnicodeBuffer`, `GlyphBuffer`) and modified by the aterm
// project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! aterm's text shaper (TF.6 of docs/AUDIT-third-party-2026-10-15.md), carved
//! from rustybuzz 0.20.1 to the surface a terminal reaches.
//!
//! - `GSUB` lookup types 1 to 8 with lookup flags and mark filtering sets,
//!   feature variations, and the skipping rules for ZWJ, ZWNJ and default
//!   ignorables;
//! - the default features (`ccmp`, `locl`, `rlig`, `calt`, `liga`, ...),
//!   automatic fractions, and a caller's features over the whole run;
//! - OpenType script selection from the run's first strong script;
//! - the Arabic shaper's joining forms, computed in buffer (logical) order;
//! - default ignorables hidden behind the space glyph.
//!
//! [`ShapePlan`] and the buffers have rustybuzz's shape, so a caller swaps
//! one for the other: build a [`ShapeFace`] over an [`crate::Face`] at its
//! instance, guess a [`UnicodeBuffer`]'s segment properties, and
//! [`shape_with_plan`] with a plan for them. Output clusters are the UTF-8
//! byte offsets [`UnicodeBuffer::push_str`] assigns, or the clusters given to
//! [`UnicodeBuffer::add`]. `pipeline.rs` lists what upstream does that this
//! does not.
//!
//! Every answer on the TF.1 shaping corpus is rustybuzz's, glyph id for glyph
//! id and cluster for cluster, apart from those deliberate deltas; aterm-render's
//! `font_oracle` replays it.

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
    fn default() -> Self {
        Self::new()
    }
}

impl UnicodeBuffer {
    /// An empty buffer.
    pub fn new() -> Self {
        UnicodeBuffer(Buffer::new())
    }

    /// The number of characters.
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.len == 0
    }

    /// Appends `text`, each character's cluster its UTF-8 byte offset in it.
    pub fn push_str(&mut self, text: &str) {
        self.0.push_str(text);
    }

    /// Appends one character with the cluster value `cluster`.
    pub fn add(&mut self, c: char, cluster: u32) {
        self.0.add(c as u32, cluster);
    }

    /// Sets the direction (left unset, [`Self::guess_segment_properties`]
    /// takes the script's).
    pub fn set_direction(&mut self, direction: Direction) {
        self.0.direction = direction;
    }

    /// The direction, [`Direction::Invalid`] until set or guessed.
    pub fn direction(&self) -> Direction {
        self.0.direction
    }

    /// The script, if set or guessed and the text has one.
    pub fn script(&self) -> Option<Script> {
        self.0.script
    }

    /// Sets the script to the first that is not Common, Inherited or
    /// Unknown, and the direction to the one it is written in, for whichever
    /// of the two is unset.
    pub fn guess_segment_properties(&mut self) {
        self.0.guess_segment_properties();
    }

    /// Empties the buffer and its segment properties, keeping its storage.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// Glyphs coming out of the shaper.
pub struct GlyphBuffer(Buffer);

impl GlyphBuffer {
    /// The number of glyphs.
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.len == 0
    }

    /// The glyphs, in visual order (left to right for a right-to-left run).
    pub fn glyph_infos(&self) -> &[GlyphInfo] {
        self.0.info.get(..self.0.len).unwrap_or(&[])
    }

    /// Their positions, parallel to [`Self::glyph_infos`].
    pub fn glyph_positions(&self) -> &[GlyphPosition] {
        self.0.pos.get(..self.0.len).unwrap_or(&[])
    }

    /// Empties the buffer and hands its storage back for the next run.
    pub fn clear(mut self) -> UnicodeBuffer {
        self.0.clear();
        UnicodeBuffer(self.0)
    }
}

/// Shapes `buffer` on `face` with `features`, guessing its segment
/// properties and building a plan for them. Prefer [`shape_with_plan`] with a
/// kept plan when shaping many runs.
pub fn shape(face: &ShapeFace, features: &[Feature], mut buffer: UnicodeBuffer) -> GlyphBuffer {
    buffer.guess_segment_properties();
    let plan = ShapePlan::new(face, buffer.0.direction, buffer.0.script, features);
    shape_with_plan(face, &plan, buffer)
}

/// Shapes `buffer` on `face` with `plan`, which must have been built for the
/// buffer's direction and script (else the plan's are used).
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
