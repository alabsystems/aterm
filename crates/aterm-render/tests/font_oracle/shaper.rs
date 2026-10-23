// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shaping half of the font oracle on aterm's own shaper.

use std::fmt::Write as _;

use super::corpus::{pin, tag_str};

/// Shape `text` as `probe::shape` does with rustybuzz.
pub(crate) fn shape(
    bytes: &[u8],
    index: u32,
    coords: &[([u8; 4], f32)],
    features: &[([u8; 4], u32)],
    text: &str,
) -> String {
    use aterm_font::Tag;
    use aterm_font::shape::{Direction, Feature, ShapeFace, ShapePlan, UnicodeBuffer};
    let Ok(mut face) = aterm_font::Face::parse(bytes, index) else {
        return "unparsed".to_string();
    };
    for &(t, value) in coords {
        face.set_variation(Tag::from_bytes(&t), value);
    }
    let face = ShapeFace::new(face);
    let feats: Vec<Feature> = features
        .iter()
        .map(|&(t, v)| Feature::new(Tag::from_bytes(&t), v))
        .collect();
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let direction = buffer.direction();
    let script = buffer.script();
    let plan = ShapePlan::new(&face, direction, script, &feats);
    let shaped = aterm_font::shape::shape_with_plan(&face, &plan, buffer);
    let dir = match direction {
        Direction::LeftToRight => "LTR".to_string(),
        Direction::RightToLeft => "RTL".to_string(),
        other => format!("{other:?}"),
    };
    let code = script.map_or(*b"Zzzz", |s| s.code());
    let mut out = format!("{dir} {} |", tag_str(code));
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
