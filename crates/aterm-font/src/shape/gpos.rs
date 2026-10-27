// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot/layout/GPOS/*.rs and
// src/hb/ot_layout_gpos_table.rs and modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Applying `GPOS` lookup subtables (types 1 to 8) and propagating attachment chains.

use super::Direction;
use super::ShapeFace;
use super::apply::{
    ApplyContext, Outcome, SkippyIter, apply_chain_context_lookup, apply_context_lookup,
};
use super::buffer::{Attachment, Buffer, GlyphPosition, SCRATCH_HAS_GPOS_ATTACHMENT};
use super::face::{PosLookup, lookup_flags};
use super::layout::{is_mark, lig_comp, lig_id, multiplied};
use crate::gpos::{
    Anchor, AnchorMatrix, CursiveAdjustment, Device, MarkArray, MarkToBaseAdjustment,
    MarkToLigatureAdjustment, MarkToMarkAdjustment, PairAdjustment, PositioningSubtable,
    SingleAdjustment, ValueRecord,
};
use crate::parser::TryNumFrom;

pub(crate) mod attach_type {
    pub(crate) const MARK: u8 = 1;
    pub(crate) const CURSIVE: u8 = 2;
}

/// Applies the first of the lookup's subtables that applies at the current glyph.
#[inline]
pub(crate) fn apply_lookup(ctx: &mut ApplyContext, lookup: &PosLookup) -> Option<()> {
    if !lookup.digest.may_have_glyph(ctx.buffer.cur(0).as_glyph()) {
        return None;
    }
    for subtable in &lookup.subtables {
        if apply_subtable(ctx, subtable).is_some() {
            return Some(());
        }
    }
    None
}

#[inline]
fn apply_subtable(ctx: &mut ApplyContext, subtable: &PositioningSubtable) -> Option<()> {
    match subtable {
        PositioningSubtable::Single(t) => apply_single(ctx, t),
        PositioningSubtable::Pair(t) => apply_pair(ctx, t),
        PositioningSubtable::Cursive(t) => apply_cursive(ctx, t),
        PositioningSubtable::MarkToBase(t) => apply_mark_to_base(ctx, t),
        PositioningSubtable::MarkToLigature(t) => apply_mark_to_ligature(ctx, t),
        PositioningSubtable::MarkToMark(t) => apply_mark_to_mark(ctx, t),
        PositioningSubtable::Context(t) => apply_context_lookup(ctx, t),
        PositioningSubtable::ChainContext(t) => apply_chain_context_lookup(ctx, t),
    }
}

#[inline]
fn record_is_empty(record: &ValueRecord<'_>) -> bool {
    record.x_placement == 0
        && record.y_placement == 0
        && record.x_advance == 0
        && record.y_advance == 0
        && record.x_placement_device.is_none()
        && record.y_placement_device.is_none()
        && record.x_advance_device.is_none()
        && record.y_advance_device.is_none()
}

#[inline]
fn device_delta(device: Device<'_>, face: &ShapeFace<'_>, y_axis: bool) -> Option<i32> {
    match device {
        Device::Hinting(hinting) if y_axis => hinting.y_delta(face.face.units_per_em(), None),
        Device::Hinting(hinting) => hinting.x_delta(face.face.units_per_em(), None),
        Device::Variation(variation) => face
            .face
            .tables()
            .gdef?
            .glyph_variation_delta(
                variation.outer_index,
                variation.inner_index,
                face.face.variation_coordinates(),
            )
            .and_then(|float| i32::try_num_from(float.round())),
    }
}

fn apply_value_record(ctx: &mut ApplyContext, idx: usize, record: &ValueRecord<'_>) -> bool {
    let horizontal = ctx.buffer.direction.is_horizontal();
    let has_coords = !ctx.face.face.variation_coordinates().is_empty();
    let mut pos = ctx.buffer.pos[idx];
    let mut worked = false;

    if record.x_placement != 0 {
        pos.x_offset = pos.x_offset.wrapping_add(i32::from(record.x_placement));
        worked = true;
    }
    if record.y_placement != 0 {
        pos.y_offset = pos.y_offset.wrapping_add(i32::from(record.y_placement));
        worked = true;
    }
    if record.x_advance != 0 && horizontal {
        pos.x_advance = pos.x_advance.wrapping_add(i32::from(record.x_advance));
        worked = true;
    }
    if record.y_advance != 0 && !horizontal {
        // y_advance grows downward while font-space grows upward.
        pos.y_advance = pos.y_advance.wrapping_sub(i32::from(record.y_advance));
        worked = true;
    }

    if has_coords {
        if let Some(device) = record.x_placement_device {
            pos.x_offset = pos
                .x_offset
                .wrapping_add(device_delta(device, ctx.face, false).unwrap_or(0));
            worked = true;
        }
        if let Some(device) = record.y_placement_device {
            pos.y_offset = pos
                .y_offset
                .wrapping_add(device_delta(device, ctx.face, true).unwrap_or(0));
            worked = true;
        }
        if horizontal && let Some(device) = record.x_advance_device {
            pos.x_advance = pos
                .x_advance
                .wrapping_add(device_delta(device, ctx.face, false).unwrap_or(0));
            worked = true;
        }
        if !horizontal && let Some(device) = record.y_advance_device {
            pos.y_advance = pos
                .y_advance
                .wrapping_sub(device_delta(device, ctx.face, true).unwrap_or(0));
            worked = true;
        }
    }

    ctx.buffer.pos[idx] = pos;
    worked
}

#[inline]
fn resolve_anchor(anchor: Anchor<'_>, face: &ShapeFace<'_>) -> (i32, i32) {
    let mut x = i32::from(anchor.x);
    let mut y = i32::from(anchor.y);
    if !face.face.variation_coordinates().is_empty() {
        if let Some(device) = anchor.x_device {
            x = x.wrapping_add(device_delta(device, face, false).unwrap_or(0));
        }
        if let Some(device) = anchor.y_device {
            y = y.wrapping_add(device_delta(device, face, true).unwrap_or(0));
        }
    }
    (x, y)
}

#[inline]
fn apply_single(ctx: &mut ApplyContext, table: &SingleAdjustment<'_>) -> Option<()> {
    let glyph = ctx.buffer.cur(0).as_glyph();
    let record = match table {
        SingleAdjustment::Format1 { coverage, value } => {
            coverage.get(glyph)?;
            *value
        }
        SingleAdjustment::Format2 { coverage, values } => {
            let index = coverage.get(glyph)?;
            values.get(index)?
        }
    };
    apply_value_record(ctx, ctx.buffer.idx, &record);
    ctx.buffer.idx += 1;
    Some(())
}

fn apply_pair(ctx: &mut ApplyContext, table: &PairAdjustment<'_>) -> Option<()> {
    let first_glyph = ctx.buffer.cur(0).as_glyph();
    let first_coverage_index = table.coverage().get(first_glyph)?;

    let mut iter = SkippyIter::new(ctx, ctx.buffer.idx, false);
    if !iter.next() {
        return None;
    }

    let second_glyph_index = iter.index();
    let second_glyph = ctx.buffer.info[second_glyph_index].as_glyph();
    let mut next_idx = iter.buf_idx;

    let records = match table {
        PairAdjustment::Format1 { sets, .. } => {
            sets.get(first_coverage_index)?.get(second_glyph)?
        }
        PairAdjustment::Format2 {
            classes, matrix, ..
        } => {
            let pair = (classes.0.get(first_glyph), classes.1.get(second_glyph));
            matrix.get(pair)?
        }
    };

    let has_record1 = !record_is_empty(&records.0);
    let has_record2 = !record_is_empty(&records.1);

    if has_record1 {
        apply_value_record(ctx, ctx.buffer.idx, &records.0);
    }
    if has_record2 {
        apply_value_record(ctx, second_glyph_index, &records.1);
        next_idx += 1;
    }

    ctx.buffer.idx = next_idx;
    Some(())
}

fn apply_cursive(ctx: &mut ApplyContext, table: &CursiveAdjustment<'_>) -> Option<()> {
    let this = ctx.buffer.cur(0).as_glyph();
    let index_this = table.coverage.get(this)?;
    let entry_this = table.sets.entry(index_this)?;

    let mut iter = SkippyIter::new(ctx, ctx.buffer.idx, false);
    if !iter.prev() {
        return None;
    }

    let i = iter.index();
    let prev = ctx.buffer.info[i].as_glyph();
    let index_prev = table.coverage.get(prev)?;
    let exit_prev = table.sets.exit(index_prev)?;

    let (exit_x, exit_y) = resolve_anchor(exit_prev, ctx.face);
    let (entry_x, entry_y) = resolve_anchor(entry_this, ctx.face);

    let direction = ctx.buffer.direction;
    let j = ctx.buffer.idx;

    let pos = &mut ctx.buffer.pos;
    match direction {
        Direction::LeftToRight => {
            pos[i].x_advance = exit_x.wrapping_add(pos[i].x_offset);
            let d = entry_x.wrapping_add(pos[j].x_offset);
            pos[j].x_advance = pos[j].x_advance.wrapping_sub(d);
            pos[j].x_offset = pos[j].x_offset.wrapping_sub(d);
        }
        Direction::RightToLeft => {
            let d = exit_x.wrapping_add(pos[i].x_offset);
            pos[i].x_advance = pos[i].x_advance.wrapping_sub(d);
            pos[i].x_offset = pos[i].x_offset.wrapping_sub(d);
            pos[j].x_advance = entry_x.wrapping_add(pos[j].x_offset);
        }
        Direction::TopToBottom => {
            pos[i].y_advance = exit_y.wrapping_add(pos[i].y_offset);
            let d = entry_y.wrapping_add(pos[j].y_offset);
            pos[j].y_advance = pos[j].y_advance.wrapping_sub(d);
            pos[j].y_offset = pos[j].y_offset.wrapping_sub(d);
        }
        Direction::BottomToTop => {
            let d = exit_y.wrapping_add(pos[i].y_offset);
            pos[i].y_advance = pos[i].y_advance.wrapping_sub(d);
            pos[i].y_offset = pos[i].y_offset.wrapping_sub(d);
            pos[j].y_advance = entry_y;
        }
        Direction::Invalid => {}
    }

    let mut child = i;
    let mut parent = j;
    let mut x_offset = entry_x.wrapping_sub(exit_x);
    let mut y_offset = entry_y.wrapping_sub(exit_y);

    if ctx.lookup_props as u16 & lookup_flags::RIGHT_TO_LEFT == 0 {
        core::mem::swap(&mut child, &mut parent);
        x_offset = x_offset.wrapping_neg();
        y_offset = y_offset.wrapping_neg();
    }

    let Ok(chain) =
        i16::try_from(parent as isize - child as isize).map(|c| if c == i16::MIN { 0 } else { c })
    else {
        ctx.buffer.idx += 1;
        return Some(());
    };
    if chain == 0 {
        ctx.buffer.idx += 1;
        return Some(());
    }

    let len = ctx.buffer.len;
    reverse_cursive_minor_offset(
        &mut ctx.buffer.pos[..len],
        &mut ctx.buffer.attach[..len],
        child,
        direction,
        parent,
    );

    ctx.buffer.attach[child] = Attachment {
        chain,
        kind: attach_type::CURSIVE,
    };
    ctx.buffer.scratch_flags |= SCRATCH_HAS_GPOS_ATTACHMENT;
    if direction.is_horizontal() {
        ctx.buffer.pos[child].y_offset = y_offset;
    } else {
        ctx.buffer.pos[child].x_offset = x_offset;
    }

    // Break mutual two-node cycle if parent was already attached to child (harfbuzz#2469).
    if ctx.buffer.attach[parent].chain == chain.wrapping_neg() {
        ctx.buffer.attach[parent].chain = 0;
        if direction.is_horizontal() {
            ctx.buffer.pos[parent].y_offset = 0;
        } else {
            ctx.buffer.pos[parent].x_offset = 0;
        }
    }

    ctx.buffer.idx += 1;
    Some(())
}

fn reverse_cursive_minor_offset(
    pos: &mut [GlyphPosition],
    attach: &mut [Attachment],
    i: usize,
    direction: Direction,
    new_parent: usize,
) {
    let chain = attach[i].chain;
    let kind = attach[i].kind;
    if chain == 0 || kind & attach_type::CURSIVE == 0 {
        return;
    }

    attach[i].chain = 0;

    let Some(j) = i
        .checked_add_signed(isize::from(chain))
        .filter(|&j| j < pos.len())
    else {
        return;
    };
    if j == new_parent {
        return;
    }

    reverse_cursive_minor_offset(pos, attach, j, direction, new_parent);

    if direction.is_horizontal() {
        pos[j].y_offset = pos[i].y_offset.wrapping_neg();
    } else {
        pos[j].x_offset = pos[i].x_offset.wrapping_neg();
    }

    attach[j] = Attachment {
        chain: chain.wrapping_neg(),
        kind,
    };
}

fn apply_mark_array(
    ctx: &mut ApplyContext,
    marks: MarkArray<'_>,
    anchors: AnchorMatrix<'_>,
    mark_index: u16,
    glyph_index: u16,
    glyph_pos: usize,
) -> Option<()> {
    let (mark_class, mark_anchor) = marks.get(mark_index)?;
    let base_anchor = anchors.get(glyph_index, mark_class)?;

    let (mark_x, mark_y) = resolve_anchor(mark_anchor, ctx.face);
    let (base_x, base_y) = resolve_anchor(base_anchor, ctx.face);

    let idx = ctx.buffer.idx;
    let pos = &mut ctx.buffer.pos[idx];
    pos.x_offset = base_x.wrapping_sub(mark_x);
    pos.y_offset = base_y.wrapping_sub(mark_y);

    if let Ok(chain) = i16::try_from(glyph_pos as isize - idx as isize)
        && chain != i16::MIN
    {
        ctx.buffer.attach[idx] = Attachment {
            chain,
            kind: attach_type::MARK,
        };
        ctx.buffer.scratch_flags |= SCRATCH_HAS_GPOS_ATTACHMENT;
    }

    ctx.buffer.idx += 1;
    Some(())
}

fn accept_base(buffer: &Buffer, idx: usize) -> bool {
    // Only attach to the first glyph of a MultipleSubst sequence unless a mark intervenes
    // (harfbuzz#740, harfbuzz#1020).
    !multiplied(&buffer.info[idx])
        || lig_comp(&buffer.info[idx]) == 0
        || idx == 0
        || is_mark(&buffer.info[idx - 1])
        || !multiplied(&buffer.info[idx - 1])
        || lig_id(&buffer.info[idx]) != lig_id(&buffer.info[idx - 1])
        || lig_comp(&buffer.info[idx]) != lig_comp(&buffer.info[idx - 1]).wrapping_add(1)
}

fn apply_mark_to_base(ctx: &mut ApplyContext, table: &MarkToBaseAdjustment<'_>) -> Option<()> {
    let mark_glyph = ctx.buffer.cur(0).as_glyph();
    let mark_index = table.mark_coverage.get(mark_glyph)?;

    if ctx.last_base_until as usize > ctx.buffer.idx {
        ctx.last_base_until = 0;
        ctx.last_base = -1;
    }

    let found_base = {
        let mut iter = SkippyIter::new(ctx, 0, false);
        iter.set_lookup_props(u32::from(lookup_flags::IGNORE_MARKS));
        let mut found = None;
        let mut j = ctx.buffer.idx;
        let stop = ctx.last_base_until as usize;
        while j > stop {
            let mut outcome = iter.match_(&ctx.buffer.info[j - 1]);
            if outcome == Outcome::Match
                && !accept_base(ctx.buffer, j - 1)
                && !table
                    .base_coverage
                    .contains(ctx.buffer.info[j - 1].as_glyph())
            {
                outcome = Outcome::Skip;
            }
            if outcome == Outcome::Match {
                found = i32::try_from(j - 1).ok();
                break;
            }
            j -= 1;
        }
        found
    };
    if let Some(base) = found_base {
        ctx.last_base = base;
    }
    ctx.last_base_until = u32::try_from(ctx.buffer.idx).unwrap_or(u32::MAX);

    let Ok(base_pos) = usize::try_from(ctx.last_base) else {
        return None;
    };
    let base_glyph = ctx.buffer.info[base_pos].as_glyph();
    let base_index = table.base_coverage.get(base_glyph)?;

    apply_mark_array(
        ctx,
        table.marks,
        table.anchors,
        mark_index,
        base_index,
        base_pos,
    )
}

fn apply_mark_to_ligature(
    ctx: &mut ApplyContext,
    table: &MarkToLigatureAdjustment<'_>,
) -> Option<()> {
    let mark_glyph = ctx.buffer.cur(0).as_glyph();
    let mark_index = table.mark_coverage.get(mark_glyph)?;

    if ctx.last_base_until as usize > ctx.buffer.idx {
        ctx.last_base_until = 0;
        ctx.last_base = -1;
    }

    let found_base = {
        let mut iter = SkippyIter::new(ctx, 0, false);
        iter.set_lookup_props(u32::from(lookup_flags::IGNORE_MARKS));
        let mut found = None;
        let mut j = ctx.buffer.idx;
        let stop = ctx.last_base_until as usize;
        while j > stop {
            if iter.match_(&ctx.buffer.info[j - 1]) == Outcome::Match {
                found = i32::try_from(j - 1).ok();
                break;
            }
            j -= 1;
        }
        found
    };
    if let Some(base) = found_base {
        ctx.last_base = base;
    }
    ctx.last_base_until = u32::try_from(ctx.buffer.idx).unwrap_or(u32::MAX);

    let Ok(lig_pos) = usize::try_from(ctx.last_base) else {
        return None;
    };
    let lig_glyph = ctx.buffer.info[lig_pos].as_glyph();
    let lig_index = table.ligature_coverage.get(lig_glyph)?;
    let lig_attach = table.ligature_array.get(lig_index)?;

    let comp_count = lig_attach.rows;
    if comp_count == 0 {
        return None;
    }

    let lig_glyph_id = lig_id(&ctx.buffer.info[lig_pos]);
    let mark_id = lig_id(ctx.buffer.cur(0));
    let mark_comp = u16::from(lig_comp(ctx.buffer.cur(0)));
    let matches = lig_glyph_id != 0 && lig_glyph_id == mark_id && mark_comp > 0;
    let comp_index = if matches {
        mark_comp.min(comp_count)
    } else {
        comp_count
    } - 1;

    apply_mark_array(
        ctx,
        table.marks,
        lig_attach,
        mark_index,
        comp_index,
        lig_pos,
    )
}

fn apply_mark_to_mark(ctx: &mut ApplyContext, table: &MarkToMarkAdjustment<'_>) -> Option<()> {
    let mark1_glyph = ctx.buffer.cur(0).as_glyph();
    let mark1_index = table.mark1_coverage.get(mark1_glyph)?;

    let iter_idx = {
        let mut iter = SkippyIter::new(ctx, ctx.buffer.idx, false);
        iter.set_lookup_props(ctx.lookup_props & !u32::from(lookup_flags::IGNORE_FLAGS));
        if !iter.prev() {
            return None;
        }
        iter.index()
    };

    if !is_mark(&ctx.buffer.info[iter_idx]) {
        return None;
    }

    let id1 = lig_id(ctx.buffer.cur(0));
    let id2 = lig_id(&ctx.buffer.info[iter_idx]);
    let comp1 = lig_comp(ctx.buffer.cur(0));
    let comp2 = lig_comp(&ctx.buffer.info[iter_idx]);

    let matches = if id1 == id2 {
        id1 == 0 || comp1 == comp2
    } else {
        (id1 > 0 && comp1 == 0) || (id2 > 0 && comp2 == 0)
    };
    if !matches {
        return None;
    }

    let mark2_glyph = ctx.buffer.info[iter_idx].as_glyph();
    let mark2_index = table.mark2_coverage.get(mark2_glyph)?;

    apply_mark_array(
        ctx,
        table.marks,
        table.mark2_matrix,
        mark1_index,
        mark2_index,
        iter_idx,
    )
}

/// Accumulates cursive and mark attachment offsets across every attached chain in `buffer`.
pub(crate) fn propagate_attachments(buffer: &mut Buffer) {
    if buffer.scratch_flags & SCRATCH_HAS_GPOS_ATTACHMENT == 0 {
        return;
    }
    let len = buffer.len;
    let direction = buffer.direction;
    for i in 0..len {
        propagate_attachment_offsets(
            &mut buffer.pos[..len],
            &mut buffer.attach[..len],
            len,
            i,
            direction,
        );
    }
}

fn propagate_attachment_offsets(
    pos: &mut [GlyphPosition],
    attach: &mut [Attachment],
    len: usize,
    i: usize,
    direction: Direction,
) {
    let chain = attach[i].chain;
    let kind = attach[i].kind;
    if chain == 0 {
        return;
    }

    attach[i].chain = 0;

    let Some(j) = i
        .checked_add_signed(isize::from(chain))
        .filter(|&j| j < len)
    else {
        return;
    };

    propagate_attachment_offsets(pos, attach, len, j, direction);

    match kind {
        attach_type::MARK if j < i => {
            pos[i].x_offset = pos[i].x_offset.wrapping_add(pos[j].x_offset);
            pos[i].y_offset = pos[i].y_offset.wrapping_add(pos[j].y_offset);

            if direction.is_forward() {
                for k in j..i {
                    pos[i].x_offset = pos[i].x_offset.wrapping_sub(pos[k].x_advance);
                    pos[i].y_offset = pos[i].y_offset.wrapping_sub(pos[k].y_advance);
                }
            } else {
                for k in j + 1..=i {
                    pos[i].x_offset = pos[i].x_offset.wrapping_add(pos[k].x_advance);
                    pos[i].y_offset = pos[i].y_offset.wrapping_add(pos[k].y_advance);
                }
            }
        }
        attach_type::CURSIVE => {
            if direction.is_horizontal() {
                pos[i].y_offset = pos[i].y_offset.wrapping_add(pos[j].y_offset);
            } else {
                pos[i].x_offset = pos[i].x_offset.wrapping_add(pos[j].x_offset);
            }
        }
        _ => {}
    }
}
