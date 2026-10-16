// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shaping half of the font oracle: what `rustybuzz` answers for a face,
//! written as text. `tests/font_oracle.rs` replays these answers against the
//! committed corpus; the same file is included by `#[path]` from
//! `tests/font_oracle/recorder`, a crate outside the workspace, which links the
//! registry crates WITHOUT aterm's `[patch.crates-io]` facades and checks they
//! answer alike.
//!
//! So this file names only `std` and `rustybuzz`, and every input a line needs
//! (face, collection index, variation instance, features, text) is written
//! into the line itself, where a replay can parse it back.
//!
//! The reader half (`reader.rs`) moved out at TF.5 of
//! docs/AUDIT-third-party-2026-10-15.md, when aterm-font replaced ttf-parser
//! in the renderer: it is written once for both readers. Since `aterm-font`
//! replaced the shaper (TF.6), its adapter (`shaper.rs`) produces the same
//! strings and is held to the same lines in the workspace; an answer it
//! changes on purpose is an accepted delta pinned by its own test. This file
//! is the recorder's alone now.
//!
//! The corpus format, the digest and the face table live in `corpus.rs`,
//! which names only `std` and which `aterm-font`'s replay includes too.

use std::fmt::Write as _;

#[path = "corpus.rs"]
pub(crate) mod corpus;
pub(crate) use corpus::*;

/// Shape `text` the way `ligature_shaping::RowShaper::shape_buffer` does: a
/// fresh buffer, `guess_segment_properties`, a plan built from the guessed
/// direction, script (absent when unknown) and language, then
/// `shape_with_plan`. Variation coordinates are set only when there are any,
/// as the primary-face path does; `Renderer::shape_cluster`'s
/// `rustybuzz::shape(&face, &[], ..)` is the same with no features.
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
