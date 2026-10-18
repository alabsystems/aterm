// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shaping half of the font oracle on rustybuzz: what `rustybuzz` answers
//! for a face, written as text. Only `tests/font_oracle/recorder` includes it
//! (by `#[path]`), a crate outside the workspace that links the registry
//! crates without aterm's `[patch.crates-io]` facades, so this file names only
//! `std`, `rustybuzz` and the std-only `corpus.rs` (the corpus line format, the
//! digest and the face table).
//!
//! In the workspace, `tests/font_oracle.rs` answers the same `shape` lines
//! with aterm-font's shaper (`shaper.rs`), which writes its answer as `shape`
//! does here; a line it answers differently must be one of the accepted deltas
//! `shaping_answers_hold` names.

use std::fmt::Write as _;

#[path = "corpus.rs"]
pub(crate) mod corpus;
pub(crate) use corpus::*;

/// Shape `text` with rustybuzz as `ligature_shaping::RowShaper::shape_buffer`
/// shapes with aterm-font: a fresh buffer, `guess_segment_properties`, a plan
/// built from the guessed direction, script (absent when unknown) and language
/// (rustybuzz guesses none, and aterm-font's plan takes none), then
/// `shape_with_plan`. Variation coordinates are set only when there are any,
/// as the primary-face path does; the non-macOS `shape_colour_cluster`'s
/// `shape(&face, &[], ..)` is the same with no features.
///
/// The answer is `<direction> <script> |` and one `gid:cluster:x_advance`
/// token per output glyph, with `,y_advance` when it is non-zero and
/// `@x_offset,y_offset` when either offset is.
pub(crate) fn shape(
    bytes: &[u8],
    index: u32,
    coords: &[([u8; 4], f32)],
    features: &[([u8; 4], u32)],
    text: &str,
) -> String {
    use rustybuzz::ttf_parser::Tag;
    let Some(mut face) = rustybuzz::Face::from_slice(bytes, index) else {
        return "unparsed".to_string();
    };
    if !coords.is_empty() {
        let vars: Vec<rustybuzz::Variation> = coords
            .iter()
            .map(|&(t, value)| rustybuzz::Variation {
                tag: Tag::from_bytes(&t),
                value,
            })
            .collect();
        face.set_variations(&vars);
    }
    let feats: Vec<rustybuzz::Feature> = features
        .iter()
        .map(|&(t, v)| rustybuzz::Feature::new(Tag::from_bytes(&t), v, ..))
        .collect();
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let direction = buffer.direction();
    let script = buffer.script();
    let language = buffer.language();
    let plan = rustybuzz::ShapePlan::new(
        &face,
        direction,
        (script != rustybuzz::script::UNKNOWN).then_some(script),
        language.as_ref(),
        &feats,
    );
    let shaped = rustybuzz::shape_with_plan(&face, &plan, buffer);
    let dir = match direction {
        rustybuzz::Direction::LeftToRight => "LTR".to_string(),
        rustybuzz::Direction::RightToLeft => "RTL".to_string(),
        other => format!("{other:?}"),
    };
    let mut out = format!("{dir} {} |", tag_str(script.tag().to_bytes()));
    for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        let _ = write!(out, " {}:{}:{}", info.glyph_id, info.cluster, pos.x_advance);
        if pos.y_advance != 0 {
            let _ = write!(out, ",{}", pos.y_advance);
        }
        if pos.x_offset != 0 || pos.y_offset != 0 {
            let _ = write!(out, "@{},{}", pos.x_offset, pos.y_offset);
        }
    }
    pin(out)
}
