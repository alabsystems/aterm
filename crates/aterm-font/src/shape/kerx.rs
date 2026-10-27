// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/aat_layout_kerx_table.rs and modified
// by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Apple Advanced Typography `kerx` (Extended Kerning) table positioning.

use super::ShapeFace;
use super::buffer::{Buffer, SCRATCH_HAS_GPOS_ATTACHMENT};
use super::gpos::attach_type;
use super::kerning::machine_kern;
use super::plan::ShapePlan;
use crate::{FromData, GlyphId, aat, ankr, kerx};

trait ExtendedStateTableExt<T: FromData + Copy> {
    fn class(&self, glyph_id: GlyphId) -> Option<u16>;
    fn entry(&self, state: u16, class: u16) -> Option<aat::GenericStateEntry<T>>;
}

impl ExtendedStateTableExt<kerx::EntryData> for kerx::Subtable1<'_> {
    fn class(&self, glyph_id: GlyphId) -> Option<u16> {
        self.state_table.class(glyph_id)
    }

    fn entry(&self, state: u16, class: u16) -> Option<aat::GenericStateEntry<kerx::EntryData>> {
        self.state_table.entry(state, class)
    }
}

impl ExtendedStateTableExt<kerx::EntryData> for kerx::Subtable4<'_> {
    fn class(&self, glyph_id: GlyphId) -> Option<u16> {
        self.state_table.class(glyph_id)
    }

    fn entry(&self, state: u16, class: u16) -> Option<aat::GenericStateEntry<kerx::EntryData>> {
        self.state_table.entry(state, class)
    }
}

pub(crate) fn apply(plan: &ShapePlan, face: &ShapeFace, buffer: &mut Buffer) {
    let Some(ref kerx_table) = face.face.tables().kerx else {
        return;
    };

    let mut seen_cross_stream = false;
    for subtable in kerx_table.subtables {
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

        match subtable.format {
            kerx::Format::Format0(_) | kerx::Format::Format2(_) | kerx::Format::Format6(_) => {
                machine_kern(
                    face,
                    buffer,
                    plan.kern_mask,
                    subtable.has_cross_stream,
                    |left, right| subtable.glyphs_kerning(left, right).map_or(0, i32::from),
                );
            }
            kerx::Format::Format1(ref sub) => {
                let mut driver = Driver1 {
                    stack: [0; 8],
                    depth: 0,
                };
                apply_state_machine_kerning(&subtable, sub, &mut driver, plan, buffer);
            }
            kerx::Format::Format4(ref sub) => {
                let mut driver = Driver4 {
                    mark_set: false,
                    mark: 0,
                    ankr_table: face.face.tables().ankr.as_ref(),
                };
                apply_state_machine_kerning(&subtable, sub, &mut driver, plan, buffer);
            }
        }

        if reverse {
            buffer.reverse();
        }
    }
}

trait KerxEntryDataExt {
    fn action_index(self) -> u16;
    fn is_actionable(&self) -> bool;
}

impl KerxEntryDataExt for aat::GenericStateEntry<kerx::EntryData> {
    #[inline]
    fn action_index(self) -> u16 {
        self.extra.action_index
    }

    #[inline]
    fn is_actionable(&self) -> bool {
        self.extra.action_index != 0xFFFF
    }
}

trait StateTableDriver<Table, E: FromData> {
    fn transition(
        &mut self,
        aat: &Table,
        entry: aat::GenericStateEntry<E>,
        has_cross_stream: bool,
        tuple_count: u32,
        plan: &ShapePlan,
        buffer: &mut Buffer,
    ) -> Option<()>;
}

fn apply_state_machine_kerning<T, E>(
    subtable: &kerx::Subtable,
    state_table: &T,
    driver: &mut dyn StateTableDriver<T, E>,
    plan: &ShapePlan,
    buffer: &mut Buffer,
) where
    T: ExtendedStateTableExt<E>,
    E: FromData + Copy,
    aat::GenericStateEntry<E>: KerxEntryDataExt,
{
    let mut state = aat::state::START_OF_TEXT;
    buffer.idx = 0;
    loop {
        let class = if buffer.idx < buffer.len {
            state_table
                .class(buffer.info[buffer.idx].as_glyph())
                .unwrap_or(u16::from(aat::class::OUT_OF_BOUNDS))
        } else {
            u16::from(aat::class::END_OF_TEXT)
        };

        let Some(entry) = state_table.entry(state, class) else {
            break;
        };

        if buffer.idx + 2 <= buffer.len
            && state_table
                .entry(state, u16::from(aat::class::END_OF_TEXT))
                .is_none()
        {
            break;
        }

        let _ = driver.transition(
            state_table,
            entry,
            subtable.has_cross_stream,
            subtable.tuple_count,
            plan,
            buffer,
        );

        state = entry.new_state;

        if buffer.idx >= buffer.len {
            break;
        }

        if entry.has_advance() || buffer.max_ops <= 0 {
            buffer.next_glyph();
        }
        buffer.max_ops = buffer.max_ops.saturating_sub(1);
    }
}

struct Driver1 {
    stack: [usize; 8],
    depth: usize,
}

impl StateTableDriver<kerx::Subtable1<'_>, kerx::EntryData> for Driver1 {
    fn transition(
        &mut self,
        aat: &kerx::Subtable1,
        entry: aat::GenericStateEntry<kerx::EntryData>,
        has_cross_stream: bool,
        tuple_count: u32,
        plan: &ShapePlan,
        buffer: &mut Buffer,
    ) -> Option<()> {
        if entry.has_reset() {
            self.depth = 0;
        }

        if entry.has_push() {
            if self.depth < self.stack.len() {
                self.stack[self.depth] = buffer.idx;
                self.depth += 1;
            } else {
                self.depth = 0;
            }
        }

        if entry.is_actionable() && self.depth != 0 {
            let tuple_count = u16::try_from(tuple_count.max(1)).ok()?;
            let mut action_index = entry.action_index();
            let mut last = false;
            while !last && self.depth != 0 {
                self.depth -= 1;
                let idx = self.stack[self.depth];
                let mut v = i32::from(aat.glyphs_kerning(action_index)?);
                action_index = action_index.checked_add(tuple_count)?;
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
                    } else if glyph_mask & plan.kern_mask != 0 {
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
                } else if glyph_mask & plan.kern_mask != 0 && pos.y_offset == 0 {
                    pos.y_advance = pos.y_advance.wrapping_add(v);
                    pos.y_offset = pos.y_offset.wrapping_add(v);
                }

                if has_gpos_attachment {
                    buffer.scratch_flags |= SCRATCH_HAS_GPOS_ATTACHMENT;
                }
            }
        }

        Some(())
    }
}

struct Driver4<'a, 'b> {
    mark_set: bool,
    mark: usize,
    ankr_table: Option<&'b ankr::Table<'a>>,
}

impl StateTableDriver<kerx::Subtable4<'_>, kerx::EntryData> for Driver4<'_, '_> {
    fn transition(
        &mut self,
        aat: &kerx::Subtable4,
        entry: aat::GenericStateEntry<kerx::EntryData>,
        _has_cross_stream: bool,
        _tuple_count: u32,
        _plan: &ShapePlan,
        buffer: &mut Buffer,
    ) -> Option<()> {
        if self.mark_set && entry.is_actionable() && buffer.idx < buffer.len {
            let idx = buffer.idx;
            if let Some(ankr_table) = self.ankr_table {
                let point = aat.anchor_points.get(entry.action_index())?;
                let mark_idx = buffer.info[self.mark].as_glyph();
                let mark_anchor = ankr_table
                    .points(mark_idx)
                    .and_then(|list| list.get(u32::from(point.0)))
                    .unwrap_or_default();
                let curr_idx = buffer.info[idx].as_glyph();
                let curr_anchor = ankr_table
                    .points(curr_idx)
                    .and_then(|list| list.get(u32::from(point.1)))
                    .unwrap_or_default();

                let pos = &mut buffer.pos[idx];
                pos.x_offset = i32::from(mark_anchor.x) - i32::from(curr_anchor.x);
                pos.y_offset = i32::from(mark_anchor.y) - i32::from(curr_anchor.y);
            }

            let delta = (self.mark as isize).checked_sub(idx as isize)?;
            if let Ok(chain) = i16::try_from(delta) {
                buffer.attach[idx].kind = attach_type::MARK;
                buffer.attach[idx].chain = chain;
                buffer.scratch_flags |= SCRATCH_HAS_GPOS_ATTACHMENT;
            }
        }

        if entry.has_mark() {
            self.mark_set = true;
            self.mark = buffer.idx;
        }

        Some(())
    }
}
