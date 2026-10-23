// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rustybuzz shaping probe for the font oracle recorder.

use std::fmt::Write as _;

#[path = "corpus.rs"]
pub(crate) mod corpus;
pub(crate) use corpus::*;

/// Shapes `text` with `rustybuzz` and formats `<direction> <script> |` followed by `gid:cluster:x_advance[,y_advance][@x_offset,y_offset]` tokens.
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
