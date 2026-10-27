// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot_shape.rs and
// src/hb/ot_shape_normalize.rs and modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! The shaping pipeline.

use super::ShapeFace;
use super::buffer::{
    Buffer, SCRATCH_HAS_CGJ, SCRATCH_HAS_DEFAULT_IGNORABLES, SCRATCH_HAS_NON_ASCII, glyph_props,
    grapheme_group,
};
use super::layout;
use super::plan::{ShapePlan, Shaper, ZeroWidthMarks};
use super::unicode::{
    init_unicode_props, is_continuation, is_default_ignorable_info, is_emoji_extended_pictographic,
    is_unicode_mark, is_variation_selector, is_zwj, modified_combining_class, set_continuation,
    set_variation_selector, unhide,
};
use super::{Direction, arabic};
use aterm_grapheme::GeneralCategory;

pub(crate) fn shape_internal(plan: &ShapePlan, face: &ShapeFace, buffer: &mut Buffer) {
    let target_direction = buffer.direction;
    buffer.enter();
    buffer.reset_masks(plan.map.global_mask());
    set_unicode_props(buffer);
    form_clusters(buffer);
    ensure_native_direction(buffer);
    rotate_chars(plan, face, buffer, target_direction);
    map_characters(face, buffer);
    setup_masks(plan, face, buffer);
    if plan.fallback_mark_positioning {
        super::fallback::recategorize_marks(buffer);
    }
    for i in 0..buffer.len {
        buffer.info[i].glyph_id = buffer.info[i].glyph_index;
    }
    layout::substitute_start(face, buffer);
    if plan.fallback_glyph_classes {
        synthesize_glyph_classes(buffer);
    }
    layout::substitute(plan, face, buffer);
    position(plan, face, buffer);
    hide_default_ignorables(face, buffer);
    buffer.direction = target_direction;
    buffer.leave();
}

fn set_unicode_props(buffer: &mut Buffer) {
    let len = buffer.len;
    let mut i = 0;
    while i < len {
        let mut flags = buffer.scratch_flags;
        init_unicode_props(&mut buffer.info[i], &mut flags);
        buffer.scratch_flags = flags;
        let info = buffer.info[i];
        let gc = info.gc;
        if matches!(
            gc,
            GeneralCategory::LowercaseLetter
                | GeneralCategory::UppercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::OtherLetter
                | GeneralCategory::SpaceSeparator
        ) {
            i += 1;
            continue;
        }
        if gc == GeneralCategory::ModifierSymbol && matches!(info.glyph_id, 0x1F3FB..=0x1F3FF) {
            set_continuation(&mut buffer.info[i]);
        } else if i != 0 && matches!(info.glyph_id, 0x1F1E6..=0x1F1FF) {
            let prev = buffer.info[i - 1];
            if matches!(prev.glyph_id, 0x1F1E6..=0x1F1FF) && !is_continuation(&prev) {
                set_continuation(&mut buffer.info[i]);
            }
        } else if is_zwj(&info) {
            set_continuation(&mut buffer.info[i]);
            if i + 1 < len && is_emoji_extended_pictographic(buffer.info[i + 1].as_char()) {
                let mut flags = buffer.scratch_flags;
                init_unicode_props(&mut buffer.info[i + 1], &mut flags);
                buffer.scratch_flags = flags;
                set_continuation(&mut buffer.info[i + 1]);
                i += 1;
            }
        } else if matches!(info.glyph_id, 0xFF9E..=0xFF9F | 0xE0020..=0xE007F) {
            set_continuation(&mut buffer.info[i]);
        }
        i += 1;
    }
}

fn form_clusters(buffer: &mut Buffer) {
    if buffer.scratch_flags & SCRATCH_HAS_NON_ASCII == 0 {
        return;
    }
    let count = buffer.len;
    let mut start = 0;
    let mut end = if count > 0 {
        buffer.group_end(0, grapheme_group)
    } else {
        0
    };
    while start < count {
        buffer.merge_clusters(start, end);
        start = end;
        end = buffer.group_end(start, grapheme_group);
    }
}

fn ensure_native_direction(buffer: &mut Buffer) {
    let dir = buffer.direction;
    let mut hor = buffer
        .script
        .and_then(Direction::from_script)
        .unwrap_or_default();
    if hor == Direction::RightToLeft && dir == Direction::LeftToRight {
        let mut found_number = false;
        let mut found_letter = false;
        let mut found_ri = false;
        for info in &buffer.info[..buffer.len] {
            if info.gc == GeneralCategory::DecimalNumber {
                found_number = true;
            } else if info.gc.is_letter() {
                found_letter = true;
                break;
            } else if matches!(info.glyph_id, 0x1F1E6..=0x1F1FF) {
                found_ri = true;
            }
        }
        if (found_number || found_ri) && !found_letter {
            hor = Direction::LeftToRight;
        }
    }
    if (dir.is_horizontal() && dir != hor && hor != Direction::Invalid)
        || (!dir.is_horizontal() && dir != Direction::TopToBottom)
    {
        buffer.reverse_groups(grapheme_group, false);
        buffer.direction = buffer.direction.reverse();
    }
}

/// The bidi step that produced a backward run already mirrored its
/// characters, so one whose mirror the font has is skipped, not mirrored again.
fn rotate_chars(plan: &ShapePlan, face: &ShapeFace, buffer: &mut Buffer, target: Direction) {
    if !target.is_backward() || plan.rtlm_mask == 0 {
        return;
    }
    for info in &mut buffer.info[..buffer.len] {
        let mirror = aterm_bidi::mirror_glyph(info.as_char());
        if mirror.is_some_and(|m| face.has_glyph(u32::from(m))) {
            continue;
        }
        info.mask |= plan.rtlm_mask;
    }
}

fn map_characters(face: &ShapeFace, buffer: &mut Buffer) {
    if buffer.len == 0 {
        return;
    }
    buffer.clear_output();
    let count = buffer.len;
    buffer.idx = 0;
    loop {
        let mut end = buffer.idx + 1;
        while end < count && !is_unicode_mark(&buffer.info[end]) {
            end += 1;
        }
        if end < count {
            end -= 1;
        }
        while buffer.idx < end && buffer.successful {
            set_glyph(face, buffer, 0);
            buffer.next_glyph();
        }
        if buffer.idx == count || !buffer.successful {
            break;
        }
        end = buffer.idx + 1;
        while end < count && is_unicode_mark(&buffer.info[end]) {
            end += 1;
        }
        map_mark_cluster(face, buffer, end);
        if buffer.idx >= count || !buffer.successful {
            break;
        }
    }
    buffer.sync();
    if buffer.scratch_flags & SCRATCH_HAS_CGJ != 0 {
        for i in 1..buffer.len.saturating_sub(1) {
            if buffer.info[i].glyph_id == 0x034F {
                let last = modified_combining_class(&buffer.info[i - 1]);
                let next = modified_combining_class(&buffer.info[i + 1]);
                if next == 0 || last <= next {
                    unhide(&mut buffer.info[i]);
                }
            }
        }
    }
}

fn set_glyph(face: &ShapeFace, buffer: &mut Buffer, i: usize) {
    let info = buffer.cur_mut(i);
    info.glyph_index = face
        .get_nominal_glyph(info.glyph_id)
        .map_or(0, |g| u32::from(g.0));
}

fn map_mark_cluster(face: &ShapeFace, buffer: &mut Buffer, end: usize) {
    let has_vs = (buffer.idx..end).any(|i| is_variation_selector(buffer.info[i].as_char()));
    if !has_vs {
        while buffer.idx < end && buffer.successful {
            set_glyph(face, buffer, 0);
            buffer.next_glyph();
        }
        return;
    }
    while buffer.idx + 1 < end && buffer.successful {
        if is_variation_selector(buffer.cur(1).as_char()) {
            let base = buffer.cur(0).as_char();
            let vs = buffer.cur(1).as_char();
            if let Some(glyph) = face.glyph_variation_index(base, vs) {
                buffer.cur_mut(0).glyph_index = u32::from(glyph.0);
                let unicode = buffer.cur(0).glyph_id;
                buffer.replace_glyphs(2, 1, &[unicode]);
            } else {
                set_glyph(face, buffer, 0);
                buffer.next_glyph();
                set_variation_selector(buffer.cur_mut(0), true);
                set_glyph(face, buffer, 0);
                buffer.next_glyph();
            }
            while buffer.idx < end && is_variation_selector(buffer.cur(0).as_char()) {
                set_glyph(face, buffer, 0);
                buffer.next_glyph();
            }
        } else {
            set_glyph(face, buffer, 0);
            buffer.next_glyph();
        }
    }
    if buffer.idx < end {
        set_glyph(face, buffer, 0);
        buffer.next_glyph();
    }
}

fn setup_masks(plan: &ShapePlan, face: &ShapeFace, buffer: &mut Buffer) {
    setup_masks_fraction(plan, buffer);
    if plan.shaper == Shaper::Arabic {
        arabic::setup_masks(plan, face, buffer);
    }
}

fn setup_masks_fraction(plan: &ShapePlan, buffer: &mut Buffer) {
    if buffer.scratch_flags & SCRATCH_HAS_NON_ASCII == 0 || !plan.has_frac {
        return;
    }
    let (pre_mask, post_mask) = if buffer.direction.is_forward() {
        (
            plan.numr_mask | plan.frac_mask,
            plan.frac_mask | plan.dnom_mask,
        )
    } else {
        (
            plan.frac_mask | plan.dnom_mask,
            plan.numr_mask | plan.frac_mask,
        )
    };
    let len = buffer.len;
    let mut i = 0;
    while i < len {
        if buffer.info[i].glyph_id != 0x2044 {
            i += 1;
            continue;
        }
        let mut start = i;
        while start > 0 && buffer.info[start - 1].gc == GeneralCategory::DecimalNumber {
            start -= 1;
        }
        let mut end = i + 1;
        while end < len && buffer.info[end].gc == GeneralCategory::DecimalNumber {
            end += 1;
        }
        if start == i || end == i + 1 {
            i += 1;
            continue;
        }
        for info in &mut buffer.info[start..i] {
            info.mask |= pre_mask;
        }
        buffer.info[i].mask |= plan.frac_mask;
        for info in &mut buffer.info[i + 1..end] {
            info.mask |= post_mask;
        }
        i = end;
    }
}

// Uniscribe treats default-ignorable nonspacing marks (Mongolian VS, CGJ) as bases.
fn synthesize_glyph_classes(buffer: &mut Buffer) {
    for info in &mut buffer.info[..buffer.len] {
        let class = if info.gc != GeneralCategory::NonspacingMark || is_default_ignorable_info(info)
        {
            glyph_props::BASE_GLYPH
        } else {
            glyph_props::MARK
        };
        info.glyph_props = class;
    }
}

fn position(plan: &ShapePlan, face: &ShapeFace, buffer: &mut Buffer) {
    buffer.clear_positions();
    let len = buffer.len;
    if buffer.direction.is_horizontal() {
        for i in 0..len {
            buffer.pos[i].x_advance = face.glyph_h_advance(buffer.info[i].as_glyph());
        }
    }
    let adjust_offsets = plan.adjust_mark_positioning_when_zeroing && buffer.direction.is_forward();
    if plan.zero_marks && plan.zero_width_marks == ZeroWidthMarks::ByGdefEarly {
        zero_mark_widths_by_gdef(buffer, adjust_offsets);
    }
    if plan.apply_gpos {
        layout::position(plan, face, buffer);
    } else if plan.apply_kerx {
        super::kerx::apply(plan, face, buffer);
    }
    if plan.apply_kern {
        super::kerning::apply(plan, face, buffer);
    }
    if plan.zero_marks && plan.zero_width_marks == ZeroWidthMarks::ByGdefLate {
        zero_mark_widths_by_gdef(buffer, adjust_offsets);
    }
    let len = buffer.len;
    if buffer.scratch_flags & SCRATCH_HAS_DEFAULT_IGNORABLES != 0 {
        for i in 0..len {
            if is_default_ignorable_info(&buffer.info[i]) {
                buffer.pos[i] = super::GlyphPosition::default();
            }
        }
    }
    super::gpos::propagate_attachments(buffer);
    if plan.fallback_mark_positioning {
        super::fallback::position_marks(plan, face, buffer, adjust_offsets);
    }
    if buffer.direction.is_backward() {
        buffer.reverse();
    }
}

fn zero_mark_widths_by_gdef(buffer: &mut Buffer, adjust_offsets: bool) {
    let len = buffer.len;
    for (info, pos) in buffer.info[..len].iter().zip(&mut buffer.pos[..len]) {
        if layout::is_mark(info) {
            if adjust_offsets {
                pos.x_offset = pos.x_offset.wrapping_sub(pos.x_advance);
                pos.y_offset = pos.y_offset.wrapping_sub(pos.y_advance);
            }
            pos.x_advance = 0;
            pos.y_advance = 0;
        }
    }
}

fn hide_default_ignorables(face: &ShapeFace, buffer: &mut Buffer) {
    if buffer.scratch_flags & SCRATCH_HAS_DEFAULT_IGNORABLES == 0 {
        return;
    }
    if let Some(invisible) = face.get_nominal_glyph(u32::from(' ')) {
        for info in &mut buffer.info[..buffer.len] {
            if is_default_ignorable_info(info) {
                info.glyph_id = u32::from(invisible.0);
            }
        }
        return;
    }
    buffer.delete_glyphs_inplace(is_default_ignorable_info);
}
