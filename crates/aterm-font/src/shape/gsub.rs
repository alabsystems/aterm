// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot/layout/GSUB/*.rs and
// src/hb/ot_layout_gsub_table.rs and modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Applying `GSUB` subtables, lookup types 1 to 8 (an extension, type 7,
//! is resolved by the reader into the subtable it wraps).

use super::apply::{
    ApplyContext, apply_chain_context_lookup, apply_context_lookup, ligate_input, match_backtrack,
    match_glyph, match_input, match_lookahead,
};
use super::buffer::glyph_props;
use super::face::SubstLookup;
use super::layout::{
    MAX_CONTEXT_LENGTH, MAX_NESTING_LEVEL, is_ligature, lig_id, set_lig_props_for_component,
};
use super::map::MAX_VALUE;
use crate::GlyphId;
use crate::gsub::{
    AlternateSubstitution, LigatureSubstitution, MultipleSubstitution,
    ReverseChainSingleSubstitution, SingleSubstitution, SubstitutionSubtable,
};

/// Applies the first of the lookup's subtables that applies at the current
/// glyph.
#[inline]
pub(crate) fn apply_lookup(ctx: &mut ApplyContext, lookup: &SubstLookup) -> Option<()> {
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
fn apply_subtable(ctx: &mut ApplyContext, subtable: &SubstitutionSubtable) -> Option<()> {
    match subtable {
        SubstitutionSubtable::Single(t) => apply_single(ctx, t),
        SubstitutionSubtable::Multiple(t) => apply_multiple(ctx, t),
        SubstitutionSubtable::Alternate(t) => apply_alternate(ctx, t),
        SubstitutionSubtable::Ligature(t) => apply_ligature(ctx, t),
        SubstitutionSubtable::Context(t) => apply_context_lookup(ctx, t),
        SubstitutionSubtable::ChainContext(t) => apply_chain_context_lookup(ctx, t),
        SubstitutionSubtable::ReverseChainSingle(t) => apply_reverse_chain(ctx, t),
    }
}

#[inline]
fn apply_single(ctx: &mut ApplyContext, table: &SingleSubstitution) -> Option<()> {
    let glyph = ctx.buffer.cur(0).as_glyph();
    let subst = match *table {
        SingleSubstitution::Format1 { coverage, delta } => {
            coverage.get(glyph)?;
            // The Adobe Annotated OpenType Suite: the result wraps to 16 bits.
            GlyphId((i32::from(glyph.0) + i32::from(delta)) as u16)
        }
        SingleSubstitution::Format2 {
            coverage,
            substitutes,
        } => {
            let index = coverage.get(glyph)?;
            substitutes.get(index)?
        }
    };
    ctx.replace_glyph(subst);
    Some(())
}

fn apply_multiple(ctx: &mut ApplyContext, table: &MultipleSubstitution) -> Option<()> {
    let glyph = ctx.buffer.cur(0).as_glyph();
    let index = table.coverage.get(glyph)?;
    let seq = table.sequences.get(index)?;
    match seq.substitutes.len() {
        // The spec disallows an empty sequence; Uniscribe deletes the glyph
        // (harfbuzz#253).
        0 => ctx.buffer.delete_glyph(),
        // In place, and not a "multiplied" substitution.
        1 => ctx.replace_glyph(seq.substitutes.get(0)?),
        _ => {
            let class = if is_ligature(ctx.buffer.cur(0)) {
                glyph_props::BASE_GLYPH
            } else {
                0
            };
            let lig = lig_id(ctx.buffer.cur(0));
            for (i, subst) in seq.substitutes.into_iter().enumerate() {
                // A glyph attached to a ligature keeps that (harfbuzz#3069).
                if lig == 0 {
                    // Components keep four bits, as upstream truncates.
                    set_lig_props_for_component(ctx.buffer.cur_mut(0), i as u8);
                }
                ctx.output_glyph_for_component(subst, class);
            }
            ctx.buffer.skip_glyph();
        }
    }
    Some(())
}

fn apply_alternate(ctx: &mut ApplyContext, table: &AlternateSubstitution) -> Option<()> {
    let glyph = ctx.buffer.cur(0).as_glyph();
    let index = table.coverage.get(glyph)?;
    let set = table.alternate_sets.get(index)?;
    let len = set.alternates.len();
    if len == 0 {
        return None;
    }
    let glyph_mask = ctx.buffer.cur(0).mask;
    // This breaks if two features enabled the lookup together, as upstream.
    let shift = ctx.lookup_mask().trailing_zeros();
    let mut alt_index = (ctx.lookup_mask() & glyph_mask) >> shift;
    // The `rand` feature's MAX_VALUE picks at random.
    if alt_index == MAX_VALUE && ctx.random {
        alt_index = ctx.random_number() % u32::from(len) + 1;
    }
    let idx = u16::try_from(alt_index).ok()?.checked_sub(1)?;
    ctx.replace_glyph(set.alternates.get(idx)?);
    Some(())
}

fn apply_ligature(ctx: &mut ApplyContext, table: &LigatureSubstitution) -> Option<()> {
    let glyph = ctx.buffer.cur(0).as_glyph();
    let index = table.coverage.get(glyph)?;
    let set = table.ligature_sets.get(index)?;
    for lig in set {
        // An empty component list replaces in place, not as a ligature.
        if lig.components.is_empty() {
            ctx.replace_glyph(lig.glyph);
            return Some(());
        }
        let components = lig.components;
        let f = |glyph, index| {
            components
                .get(index)
                .is_some_and(|c| match_glyph(glyph, c.0))
        };
        let mut positions = [0; MAX_CONTEXT_LENGTH];
        let mut total_component_count = 0;
        let Some(end) = match_input(
            ctx,
            components.len(),
            &f,
            &mut positions,
            Some(&mut total_component_count),
        ) else {
            continue;
        };
        let count = usize::from(components.len()) + 1;
        ligate_input(
            ctx,
            count,
            &positions,
            end,
            total_component_count,
            lig.glyph,
        );
        return Some(());
    }
    None
}

fn apply_reverse_chain(
    ctx: &mut ApplyContext,
    table: &ReverseChainSingleSubstitution,
) -> Option<()> {
    let glyph = ctx.buffer.cur(0).as_glyph();
    let index = table.coverage.get(glyph)?;
    if index >= table.substitutes.len() {
        return None;
    }
    // No chaining to this type.
    if ctx.nesting_level_left != MAX_NESTING_LEVEL {
        return None;
    }
    let subst = table.substitutes.get(index)?;
    let back = |glyph, index| {
        table
            .backtrack_coverages
            .get(index)
            .is_some_and(|c| c.contains(glyph))
    };
    let ahead = |glyph, index| {
        table
            .lookahead_coverages
            .get(index)
            .is_some_and(|c| c.contains(glyph))
    };
    if match_backtrack(ctx, table.backtrack_coverages.len(), &back)
        && match_lookahead(
            ctx,
            table.lookahead_coverages.len(),
            &ahead,
            ctx.buffer.idx + 1,
        )
    {
        ctx.replace_glyph_inplace(subst);
        // The main loop moves `idx`, not this: no surprise for a context
        // lookup that calls in.
        return Some(());
    }
    None
}
