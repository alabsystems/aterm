// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/kerning.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! OpenType and legacy AAT `kern` table positioning.

use super::ShapeFace;
use super::apply::{ApplyContext, SkippyIter};
use super::buffer::{Buffer, SCRATCH_HAS_GPOS_ATTACHMENT};
use super::digest::Digest;
use super::face::lookup_flags;
use super::gpos::attach_type;
use super::plan::ShapePlan;
use crate::{GlyphId, aat, kern};

pub(crate) fn apply(plan: &ShapePlan, face: &ShapeFace, buffer: &mut Buffer) {
    let Some(table) = face.face.tables().kern else {
        return;
    };

    let mut seen_cross_stream = false;
    for (subtable, state_table) in table.subtables.shaper_iter() {
        if subtable.variable {
            continue;
        }
        if buffer.direction.is_horizontal() != subtable.horizontal {
            continue;
        }
        if !subtable.has_state_machine && !plan.requested_kerning {
            continue;
        }

        let reverse = buffer.direction.is_backward();
        if !seen_cross_stream && subtable.has_cross_stream {
            seen_cross_stream = true;
            let chain = if buffer.direction.is_forward() { -1 } else { 1 };
            for attach in &mut buffer.attach[..buffer.len] {
                attach.kind = attach_type::CURSIVE;
                attach.chain = chain;
            }
        }

        if reverse {
            buffer.reverse();
        }

        if subtable.has_state_machine {
            if let Some(ref st) = state_table {
                apply_state_machine_kerning(&subtable, st, plan.kern_mask, buffer);
            }
        } else {
            machine_kern(
                face,
                buffer,
                plan.kern_mask,
                subtable.has_cross_stream,
                |left, right| subtable.glyphs_kerning(left, right).map_or(0, i32::from),
            );
        }

        if reverse {
            buffer.reverse();
        }
    }
}

pub(crate) fn machine_kern(
    face: &ShapeFace,
    buffer: &mut Buffer,
    kern_mask: u32,
    cross_stream: bool,
    get_kerning: impl Fn(GlyphId, GlyphId) -> i32,
) {
    let mut ctx = ApplyContext::new(super::map::GPOS, face, buffer, Digest::new());
    ctx.set_lookup_mask(kern_mask);
    ctx.lookup_props = u32::from(lookup_flags::IGNORE_MARKS);

    let horizontal = ctx.buffer.direction.is_horizontal();
    let mut i = 0;
    while i < ctx.buffer.len {
        if (ctx.buffer.info[i].mask & kern_mask) == 0 {
            i += 1;
            continue;
        }

        let mut iter = SkippyIter::new(&ctx, i, false);
        if !iter.next() {
            i += 1;
            continue;
        }

        let j = iter.index();
        let kern = get_kerning(ctx.buffer.info[i].as_glyph(), ctx.buffer.info[j].as_glyph());
        if kern != 0 {
            let pos = &mut ctx.buffer.pos;
            if horizontal {
                if cross_stream {
                    pos[j].y_offset = kern;
                    ctx.buffer.scratch_flags |= SCRATCH_HAS_GPOS_ATTACHMENT;
                } else {
                    let kern1 = kern >> 1;
                    let kern2 = kern - kern1;
                    pos[i].x_advance = pos[i].x_advance.wrapping_add(kern1);
                    pos[j].x_advance = pos[j].x_advance.wrapping_add(kern2);
                    pos[j].x_offset = pos[j].x_offset.wrapping_add(kern2);
                }
            } else if cross_stream {
                pos[j].x_offset = kern;
                ctx.buffer.scratch_flags |= SCRATCH_HAS_GPOS_ATTACHMENT;
            } else {
                let kern1 = kern >> 1;
                let kern2 = kern - kern1;
                pos[i].y_advance = pos[i].y_advance.wrapping_add(kern1);
                pos[j].y_advance = pos[j].y_advance.wrapping_add(kern2);
                pos[j].y_offset = pos[j].y_offset.wrapping_add(kern2);
            }
        }

        i = j;
    }
}

struct StateMachineDriver {
    stack: [usize; 8],
    depth: usize,
}

fn apply_state_machine_kerning(
    subtable: &kern::Subtable,
    state_table: &aat::StateTable,
    kern_mask: u32,
    buffer: &mut Buffer,
) {
    let mut driver = StateMachineDriver {
        stack: [0; 8],
        depth: 0,
    };

    let mut state = aat::state::START_OF_TEXT;
    buffer.idx = 0;
    loop {
        let class = if buffer.idx < buffer.len {
            state_table
                .class(buffer.info[buffer.idx].as_glyph())
                .unwrap_or(aat::class::OUT_OF_BOUNDS)
        } else {
            aat::class::END_OF_TEXT
        };

        let Some(entry) = state_table.entry(state, class) else {
            break;
        };

        if buffer.idx + 2 <= buffer.len
            && state_table.entry(state, aat::class::END_OF_TEXT).is_none()
        {
            break;
        }

        state_machine_transition(
            entry,
            subtable.has_cross_stream,
            kern_mask,
            state_table,
            &mut driver,
            buffer,
        );

        state = state_table.new_state(entry.new_state);

        if buffer.idx >= buffer.len {
            break;
        }

        buffer.max_ops = buffer.max_ops.saturating_sub(1);
        if entry.has_advance() || buffer.max_ops <= 0 {
            buffer.next_glyph();
        }
    }
}

fn state_machine_transition(
    entry: aat::StateEntry,
    has_cross_stream: bool,
    kern_mask: u32,
    state_table: &aat::StateTable,
    driver: &mut StateMachineDriver,
    buffer: &mut Buffer,
) {
    if entry.has_push() {
        if driver.depth < driver.stack.len() {
            driver.stack[driver.depth] = buffer.idx;
            driver.depth += 1;
        } else {
            driver.depth = 0;
        }
    }

    if entry.has_offset() && driver.depth != 0 {
        let mut value_offset = entry.value_offset();
        let Some(mut value) = state_table.kerning(value_offset) else {
            driver.depth = 0;
            return;
        };

        let mut last = false;
        while !last && driver.depth != 0 {
            driver.depth -= 1;
            let idx = driver.stack[driver.depth];
            let mut v = i32::from(value);
            value_offset = value_offset.next();
            value = state_table.kerning(value_offset).unwrap_or(0);
            if idx >= buffer.len {
                continue;
            }

            last = v & 1 != 0;
            v &= !1;

            let mut has_gpos_attachment = false;
            let glyph_mask = buffer.info[idx].mask;
            let pos = &mut buffer.pos[idx];
            let attach = &mut buffer.attach[idx];

            if buffer.direction.is_horizontal() {
                if has_cross_stream {
                    if v == -0x8000 {
                        attach.kind = 0;
                        attach.chain = 0;
                        pos.y_offset = 0;
                    } else if attach.kind != 0 {
                        pos.y_offset = pos.y_offset.wrapping_add(v);
                        has_gpos_attachment = true;
                    }
                } else if glyph_mask & kern_mask != 0 {
                    pos.x_advance = pos.x_advance.wrapping_add(v);
                    pos.x_offset = pos.x_offset.wrapping_add(v);
                }
            } else if has_cross_stream {
                if v == -0x8000 {
                    attach.kind = 0;
                    attach.chain = 0;
                    pos.x_offset = 0;
                } else if attach.kind != 0 {
                    pos.x_offset = pos.x_offset.wrapping_add(v);
                    has_gpos_attachment = true;
                }
            } else if glyph_mask & kern_mask != 0 && pos.y_offset == 0 {
                pos.y_advance = pos.y_advance.wrapping_add(v);
                pos.y_offset = pos.y_offset.wrapping_add(v);
            }

            if has_gpos_attachment {
                buffer.scratch_flags |= SCRATCH_HAS_GPOS_ATTACHMENT;
            }
        }
    }
}
