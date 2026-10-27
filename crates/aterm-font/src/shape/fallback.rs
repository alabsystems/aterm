// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot_shape_fallback.rs and modified by
// the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Fallback mark recategorization and geometric mark placement when `GPOS`/`kerx` do not run.

use super::buffer::{Buffer, GlyphPosition};
use super::ccc::{CanonicalCombiningClass, conv_combining_class, mcc};
use super::face::GlyphExtents;
use super::layout::{lig_comp, lig_id, lig_num_comps};
use super::plan::ShapePlan;
use super::unicode::{is_unicode_mark, modified_combining_class, set_modified_combining_class};
use super::{Direction, ShapeFace};
use crate::GlyphId;
use aterm_grapheme::GeneralCategory;

fn recategorize_combining_class(u: u32, mut class: u8) -> u8 {
    use CanonicalCombiningClass as Class;

    if class >= 200 {
        return class;
    }

    if u & !0xFF == 0x0E00 {
        if class == 0 {
            match u {
                0x0E31 | 0x0E34 | 0x0E35 | 0x0E36 | 0x0E37 | 0x0E47 | 0x0E4C | 0x0E4D | 0x0E4E => {
                    class = Class::AboveRight as u8
                }
                0x0EB1 | 0x0EB4 | 0x0EB5 | 0x0EB6 | 0x0EB7 | 0x0EBB | 0x0ECC | 0x0ECD => {
                    class = Class::Above as u8;
                }
                0x0EBC => class = Class::Below as u8,
                _ => {}
            }
        } else if u == 0x0E3A {
            class = Class::BelowRight as u8;
        }
    }

    match class {
        mcc::CCC10
        | mcc::CCC11
        | mcc::CCC12
        | mcc::CCC13
        | mcc::CCC14
        | mcc::CCC15
        | mcc::CCC16
        | mcc::CCC17
        | mcc::CCC18
        | mcc::CCC20
        | mcc::CCC22 => Class::Below as u8,
        mcc::CCC23 => Class::AttachedAbove as u8,
        mcc::CCC24 => Class::AboveRight as u8,
        mcc::CCC25 | mcc::CCC19 => Class::AboveLeft as u8,
        mcc::CCC26 => Class::Above as u8,
        mcc::CCC21 => class,

        mcc::CCC27
        | mcc::CCC28
        | mcc::CCC30
        | mcc::CCC31
        | mcc::CCC33
        | mcc::CCC34
        | mcc::CCC35
        | mcc::CCC36 => Class::Above as u8,
        mcc::CCC29 | mcc::CCC32 => Class::Below as u8,

        mcc::CCC103 => Class::BelowRight as u8,
        mcc::CCC107 => Class::AboveRight as u8,

        mcc::CCC118 => Class::Below as u8,
        mcc::CCC122 => Class::Above as u8,

        mcc::CCC129 | mcc::CCC132 => Class::Below as u8,
        mcc::CCC130 => Class::Above as u8,

        _ => class,
    }
}

/// Recategorizes nonspacing marks' modified combining classes before `glyph_id` is overwritten
/// with the mapped glyph index.
pub(crate) fn recategorize_marks(buffer: &mut Buffer) {
    for info in &mut buffer.info[..buffer.len] {
        if info.gc == GeneralCategory::NonspacingMark {
            let class = recategorize_combining_class(info.glyph_id, modified_combining_class(info));
            set_modified_combining_class(info, class);
        }
    }
}

fn zero_mark_advances(
    buffer: &mut Buffer,
    start: usize,
    end: usize,
    adjust_offsets_when_zeroing: bool,
) {
    for (info, pos) in buffer.info[start..end]
        .iter()
        .zip(&mut buffer.pos[start..end])
    {
        if info.gc == GeneralCategory::NonspacingMark {
            if adjust_offsets_when_zeroing {
                pos.x_offset = pos.x_offset.wrapping_sub(pos.x_advance);
                pos.y_offset = pos.y_offset.wrapping_sub(pos.y_advance);
            }
            pos.x_advance = 0;
            pos.y_advance = 0;
        }
    }
}

fn position_mark(
    face: &ShapeFace,
    direction: Direction,
    glyph: GlyphId,
    pos: &mut GlyphPosition,
    base_extents: &mut GlyphExtents,
    combining_class: CanonicalCombiningClass,
) {
    use CanonicalCombiningClass as Class;

    let Some(mark_extents) = face.glyph_extents(glyph) else {
        return;
    };

    let y_gap = i32::from(face.face.units_per_em()) / 16;
    pos.x_offset = 0;
    pos.y_offset = 0;

    match combining_class {
        Class::DoubleBelow | Class::DoubleAbove if direction.is_horizontal() => {
            let edge = if direction.is_forward() {
                base_extents.width
            } else {
                0
            };
            pos.x_offset = base_extents
                .x_bearing
                .wrapping_add(edge)
                .wrapping_sub(mark_extents.width / 2)
                .wrapping_sub(mark_extents.x_bearing);
        }
        Class::AttachedBelowLeft | Class::BelowLeft | Class::AboveLeft => {
            pos.x_offset = base_extents.x_bearing.wrapping_sub(mark_extents.x_bearing);
        }
        Class::AttachedAboveRight | Class::BelowRight | Class::AboveRight => {
            pos.x_offset = base_extents
                .x_bearing
                .wrapping_add(base_extents.width)
                .wrapping_sub(mark_extents.width)
                .wrapping_sub(mark_extents.x_bearing);
        }
        _ => {
            pos.x_offset = base_extents
                .x_bearing
                .wrapping_add(base_extents.width.wrapping_sub(mark_extents.width) / 2)
                .wrapping_sub(mark_extents.x_bearing);
        }
    }

    let is_attached = matches!(
        combining_class,
        Class::AttachedBelowLeft
            | Class::AttachedBelow
            | Class::AttachedAbove
            | Class::AttachedAboveRight
    );

    match combining_class {
        Class::DoubleBelow
        | Class::BelowLeft
        | Class::Below
        | Class::BelowRight
        | Class::AttachedBelowLeft
        | Class::AttachedBelow => {
            if !is_attached {
                base_extents.height = base_extents.height.wrapping_sub(y_gap);
            }
            pos.y_offset = base_extents
                .y_bearing
                .wrapping_add(base_extents.height)
                .wrapping_sub(mark_extents.y_bearing);
            if (y_gap > 0) == (pos.y_offset > 0) {
                base_extents.height = base_extents.height.wrapping_sub(pos.y_offset);
                pos.y_offset = 0;
            }
            base_extents.height = base_extents.height.wrapping_add(mark_extents.height);
        }
        Class::DoubleAbove
        | Class::AboveLeft
        | Class::Above
        | Class::AboveRight
        | Class::AttachedAbove
        | Class::AttachedAboveRight => {
            if !is_attached {
                base_extents.y_bearing = base_extents.y_bearing.wrapping_add(y_gap);
                base_extents.height = base_extents.height.wrapping_sub(y_gap);
            }
            pos.y_offset = base_extents
                .y_bearing
                .wrapping_sub(mark_extents.y_bearing.wrapping_add(mark_extents.height));
            if (y_gap > 0) != (pos.y_offset > 0) {
                let correction = (-pos.y_offset) / 2;
                base_extents.y_bearing = base_extents.y_bearing.wrapping_add(correction);
                base_extents.height = base_extents.height.wrapping_sub(correction);
                pos.y_offset = pos.y_offset.wrapping_add(correction);
            }
            base_extents.y_bearing = base_extents.y_bearing.wrapping_sub(mark_extents.height);
            base_extents.height = base_extents.height.wrapping_add(mark_extents.height);
        }
        _ => {}
    }
}

fn position_around_base(
    plan: &ShapePlan,
    face: &ShapeFace,
    buffer: &mut Buffer,
    base: usize,
    end: usize,
    adjust_offsets_when_zeroing: bool,
) {
    let mut horizontal_dir = Direction::Invalid;

    let base_info = buffer.info[base];
    let base_pos = buffer.pos[base];
    let base_glyph = base_info.as_glyph();

    let Some(mut base_extents) = face.glyph_extents(base_glyph) else {
        zero_mark_advances(buffer, base + 1, end, adjust_offsets_when_zeroing);
        return;
    };

    base_extents.y_bearing = base_extents.y_bearing.wrapping_add(base_pos.y_offset);
    base_extents.x_bearing = 0;
    base_extents.width = face.glyph_h_advance(base_glyph);

    let base_lig_id = u32::from(lig_id(&base_info));
    let num_lig_components = i32::from(lig_num_comps(&base_info));

    let mut x_offset = 0i32;
    let mut y_offset = 0i32;
    if buffer.direction.is_forward() {
        x_offset = x_offset.wrapping_sub(base_pos.x_advance);
        y_offset = y_offset.wrapping_sub(base_pos.y_advance);
    }

    let mut last_lig_component: i32 = -1;
    let mut last_combining_class: u8 = 255;
    let mut component_extents = base_extents;
    let mut cluster_extents = base_extents;

    for (info, pos) in buffer.info[base + 1..end]
        .iter()
        .zip(&mut buffer.pos[base + 1..end])
    {
        let this_combining_class = modified_combining_class(info);
        if this_combining_class != 0 {
            if num_lig_components > 1 {
                let this_lig_id = u32::from(lig_id(info));
                let mut this_lig_component = i32::from(lig_comp(info)) - 1;
                if base_lig_id == 0
                    || base_lig_id != this_lig_id
                    || this_lig_component < 0
                    || this_lig_component >= num_lig_components
                {
                    this_lig_component = num_lig_components - 1;
                }

                if last_lig_component != this_lig_component {
                    last_lig_component = this_lig_component;
                    last_combining_class = 255;
                    component_extents = base_extents;

                    if horizontal_dir == Direction::Invalid {
                        horizontal_dir = if plan.direction.is_horizontal() {
                            plan.direction
                        } else {
                            plan.script
                                .and_then(Direction::from_script)
                                .unwrap_or(Direction::LeftToRight)
                        };
                    }

                    let comp_index = if horizontal_dir == Direction::LeftToRight {
                        this_lig_component
                    } else {
                        num_lig_components - 1 - this_lig_component
                    };
                    let comp_shift = i64::from(comp_index) * i64::from(component_extents.width)
                        / i64::from(num_lig_components);
                    component_extents.x_bearing =
                        component_extents.x_bearing.wrapping_add(comp_shift as i32);
                    component_extents.width /= num_lig_components;
                }
            }

            if last_combining_class != this_combining_class {
                last_combining_class = this_combining_class;
                cluster_extents = component_extents;
            }

            position_mark(
                face,
                buffer.direction,
                info.as_glyph(),
                pos,
                &mut cluster_extents,
                conv_combining_class(this_combining_class),
            );

            pos.x_advance = 0;
            pos.y_advance = 0;
            pos.x_offset = pos.x_offset.wrapping_add(x_offset);
            pos.y_offset = pos.y_offset.wrapping_add(y_offset);
        } else if buffer.direction.is_forward() {
            x_offset = x_offset.wrapping_sub(pos.x_advance);
            y_offset = y_offset.wrapping_sub(pos.y_advance);
        } else {
            x_offset = x_offset.wrapping_add(pos.x_advance);
            y_offset = y_offset.wrapping_add(pos.y_advance);
        }
    }
}

fn position_cluster(
    plan: &ShapePlan,
    face: &ShapeFace,
    buffer: &mut Buffer,
    start: usize,
    end: usize,
    adjust_offsets_when_zeroing: bool,
) {
    if end.saturating_sub(start) < 2 {
        return;
    }

    let mut i = start;
    while i < end {
        if !is_unicode_mark(&buffer.info[i]) {
            let mut j = i + 1;
            while j < end && is_unicode_mark(&buffer.info[j]) {
                j += 1;
            }
            position_around_base(plan, face, buffer, i, j, adjust_offsets_when_zeroing);
            i = j - 1;
        }
        i += 1;
    }
}

pub(crate) fn position_marks(
    plan: &ShapePlan,
    face: &ShapeFace,
    buffer: &mut Buffer,
    adjust_offsets_when_zeroing: bool,
) {
    let mut start = 0;
    let len = buffer.len;
    for i in 1..len {
        if !is_unicode_mark(&buffer.info[i]) {
            position_cluster(plan, face, buffer, start, i, adjust_offsets_when_zeroing);
            start = i;
        }
    }
    position_cluster(plan, face, buffer, start, len, adjust_offsets_when_zeroing);
}
