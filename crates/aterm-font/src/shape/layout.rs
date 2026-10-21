// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot_layout.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Applying a plan's `GSUB` lookups, stage by stage, and the per-glyph
//! layout properties they read and write: the glyph class (`GDEF`, else
//! guessed from the general category) and the ligature id and component.

use super::ShapeFace;
use super::apply::ApplyContext;
use super::buffer::{Buffer, GlyphInfo, glyph_props};
use super::plan::ShapePlan;

pub(crate) const MAX_NESTING_LEVEL: usize = 64;
pub(crate) const MAX_CONTEXT_LENGTH: usize = 64;

/// Sets every glyph's class from `GDEF` and clears its ligature props:
/// done once before substitution.
pub(crate) fn substitute_start(face: &ShapeFace, buffer: &mut Buffer) {
    for info in &mut buffer.info[..buffer.len] {
        info.glyph_props = face.glyph_props(info.as_glyph());
        info.lig_props = 0;
    }
}

/// Applies the plan's `GSUB` lookups, running each stage's pause after it.
pub(crate) fn substitute(plan: &ShapePlan, face: &ShapeFace, buffer: &mut Buffer) {
    // The first `gsub_lookup` parses every lookup of the face, so a face
    // whose `GSUB` runs past its parse budget is shaped with none of it
    // rather than with the lookups before the cut (`ShapeFace::gsub_lookups`).
    let digest = buffer_digest(buffer);
    let mut ctx = ApplyContext::new(face, buffer, digest);
    for (stage_index, stage) in plan.map.stages().iter().enumerate() {
        if face.gsub().is_some() {
            for lookup_map in plan.map.stage_lookups(stage_index) {
                let Some(lookup) = face.gsub_lookup(lookup_map.index) else {
                    continue;
                };
                if lookup.digest.may_have(&ctx.digest) {
                    ctx.lookup_index = lookup_map.index;
                    ctx.set_lookup_mask(lookup_map.mask);
                    ctx.auto_zwj = lookup_map.auto_zwj;
                    ctx.auto_zwnj = lookup_map.auto_zwnj;
                    ctx.random = lookup_map.random;
                    apply_string(&mut ctx, lookup);
                }
            }
        }
        if let Some(pause) = stage.pause {
            pause(plan, face, ctx.buffer);
            ctx.digest = buffer_digest(ctx.buffer);
        }
    }
}

fn buffer_digest(buffer: &Buffer) -> super::digest::Digest {
    let mut digest = super::digest::Digest::new();
    for info in &buffer.info[..buffer.len] {
        digest.add(crate::GlyphId(info.glyph_id as u16));
    }
    digest
}

fn apply_string(ctx: &mut ApplyContext, lookup: &super::face::SubstLookup) {
    if ctx.buffer.len == 0 || ctx.lookup_mask() == 0 {
        return;
    }
    ctx.lookup_props = lookup.props;
    if !lookup.reverse {
        // In/out forward substitution.
        ctx.buffer.clear_output();
        ctx.buffer.idx = 0;
        while ctx.buffer.idx < ctx.buffer.len && ctx.buffer.successful {
            let cur = *ctx.buffer.cur(0);
            if (cur.mask & ctx.lookup_mask()) != 0
                && ctx.check_glyph_property(&cur, ctx.lookup_props)
                && super::gsub::apply_lookup(ctx, lookup).is_some()
            {
                continue;
            }
            ctx.buffer.next_glyph();
        }
        ctx.buffer.sync();
    } else {
        // In-place backward substitution.
        if ctx.buffer.have_output {
            return;
        }
        ctx.buffer.idx = ctx.buffer.len - 1;
        loop {
            let cur = *ctx.buffer.cur(0);
            if (cur.mask & ctx.lookup_mask()) != 0
                && ctx.check_glyph_property(&cur, ctx.lookup_props)
            {
                let _ = super::gsub::apply_lookup(ctx, lookup);
            }
            if ctx.buffer.idx == 0 {
                break;
            }
            ctx.buffer.idx -= 1;
        }
    }
}

const IS_LIG_BASE: u8 = 0x10;

// lig_props: when a ligature forms, the ligature glyph and the marks between
// its components get a fresh ligature id; the ligature records its component
// count, and each mark the component it attached to. A multiple
// substitution's glyphs get id 0 and components 0, 1, 2, ... GPOS reads
// them to attach marks; here they keep later ligatures from forming across
// another ligature's components.

#[inline]
pub(crate) fn set_lig_props_for_ligature(info: &mut GlyphInfo, lig_id: u8, lig_num_comps: u8) {
    info.lig_props = (lig_id << 5) | IS_LIG_BASE | (lig_num_comps & 0x0F);
}

#[inline]
pub(crate) fn set_lig_props_for_mark(info: &mut GlyphInfo, lig_id: u8, lig_comp: u8) {
    info.lig_props = (lig_id << 5) | (lig_comp & 0x0F);
}

#[inline]
pub(crate) fn set_lig_props_for_component(info: &mut GlyphInfo, comp: u8) {
    set_lig_props_for_mark(info, 0, comp);
}

#[inline]
pub(crate) fn lig_id(info: &GlyphInfo) -> u8 {
    info.lig_props >> 5
}

#[inline]
fn ligated_internal(info: &GlyphInfo) -> bool {
    info.lig_props & IS_LIG_BASE != 0
}

#[inline]
pub(crate) fn lig_comp(info: &GlyphInfo) -> u8 {
    if ligated_internal(info) {
        0
    } else {
        info.lig_props & 0x0F
    }
}

#[inline]
pub(crate) fn lig_num_comps(info: &GlyphInfo) -> u8 {
    if info.glyph_props & glyph_props::LIGATURE != 0 && ligated_internal(info) {
        info.lig_props & 0x0F
    } else {
        1
    }
}

#[inline]
pub(crate) fn is_base_glyph(info: &GlyphInfo) -> bool {
    info.glyph_props & glyph_props::BASE_GLYPH != 0
}

#[inline]
pub(crate) fn is_ligature(info: &GlyphInfo) -> bool {
    info.glyph_props & glyph_props::LIGATURE != 0
}

#[inline]
pub(crate) fn is_mark(info: &GlyphInfo) -> bool {
    info.glyph_props & glyph_props::MARK != 0
}

#[inline]
pub(crate) fn substituted(info: &GlyphInfo) -> bool {
    info.glyph_props & glyph_props::SUBSTITUTED != 0
}
