// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot_layout_gsubgpos.rs and modified by
// the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Glyph sequence matching, contextual lookups, and ligature application.

use super::ShapeFace;
use super::buffer::{Buffer, GlyphInfo, glyph_props};
use super::digest::Digest;
use super::face::lookup_flags;
use super::layout::{
    MAX_CONTEXT_LENGTH, MAX_NESTING_LEVEL, is_base_glyph, is_mark, lig_comp, lig_id, lig_num_comps,
    set_lig_props_for_ligature, set_lig_props_for_mark,
};
use super::unicode::{is_default_ignorable_info, is_hidden, is_zwj, is_zwnj, set_general_category};
use crate::GlyphId;
use crate::opentype_layout::{
    ChainedContextLookup, ClassDefinition, ContextLookup, LookupIndex, SequenceLookupRecord,
};
use crate::parser::LazyArray16;
use aterm_grapheme::GeneralCategory;

/// Match positions for one rule as indices into the buffer, including the first glyph.
pub(crate) type MatchPositions = [usize; MAX_CONTEXT_LENGTH];

pub(crate) type MatchFn<'a> = dyn Fn(GlyphId, u16) -> bool + 'a;

pub(crate) struct ApplyContext<'a, 'f> {
    pub(crate) face: &'a ShapeFace<'f>,
    pub(crate) buffer: &'a mut Buffer,
    lookup_mask: u32,
    pub(crate) lookup_index: LookupIndex,
    pub(crate) lookup_props: u32,
    pub(crate) nesting_level_left: usize,
    pub(crate) auto_zwnj: bool,
    pub(crate) auto_zwj: bool,
    pub(crate) random: bool,
    random_state: u32,
    /// The glyphs the buffer may hold (grown as substitutions add glyphs).
    pub(crate) digest: Digest,
}

impl<'a, 'f> ApplyContext<'a, 'f> {
    #[inline]
    pub(crate) fn new(face: &'a ShapeFace<'f>, buffer: &'a mut Buffer, digest: Digest) -> Self {
        ApplyContext {
            face,
            buffer,
            lookup_mask: 1,
            lookup_index: u16::MAX,
            lookup_props: 0,
            nesting_level_left: MAX_NESTING_LEVEL,
            auto_zwnj: true,
            auto_zwj: true,
            random: false,
            random_state: 1,
            digest,
        }
    }

    #[inline]
    pub(crate) fn random_number(&mut self) -> u32 {
        self.random_state = self.random_state.wrapping_mul(48271) % 2_147_483_647;
        self.random_state
    }

    #[inline]
    pub(crate) const fn set_lookup_mask(&mut self, mask: u32) {
        self.lookup_mask = mask;
    }

    #[inline]
    pub(crate) const fn lookup_mask(&self) -> u32 {
        self.lookup_mask
    }

    /// Applies nested lookup `index` at the current position.
    pub(crate) fn recurse(&mut self, index: LookupIndex) -> Option<()> {
        if self.nesting_level_left == 0 {
            return None;
        }
        self.buffer.max_ops -= 1;
        if self.buffer.max_ops < 0 {
            return None;
        }
        self.nesting_level_left -= 1;
        let saved_props = self.lookup_props;
        let saved_index = self.lookup_index;
        self.lookup_index = index;
        let face = self.face;
        let applied = face.gsub_lookup(index).and_then(|lookup| {
            self.lookup_props = lookup.props;
            super::gsub::apply_lookup(self, lookup)
        });
        self.lookup_props = saved_props;
        self.lookup_index = saved_index;
        self.nesting_level_left += 1;
        applied
    }

    /// Whether the lookup flags in `match_props` let a lookup see `info`.
    #[inline]
    pub(crate) fn check_glyph_property(&self, info: &GlyphInfo, match_props: u32) -> bool {
        let props = info.glyph_props;
        let flags = match_props as u16;
        if props & flags & lookup_flags::IGNORE_FLAGS != 0 {
            return false;
        }
        if props & glyph_props::MARK != 0 {
            if flags & lookup_flags::USE_MARK_FILTERING_SET != 0 {
                return self
                    .face
                    .is_mark_glyph(info.as_glyph(), (match_props >> 16) as u16);
            }
            if flags & lookup_flags::MARK_ATTACHMENT_TYPE_MASK != 0 {
                return (flags & lookup_flags::MARK_ATTACHMENT_TYPE_MASK)
                    == (props & lookup_flags::MARK_ATTACHMENT_TYPE_MASK);
            }
        }
        true
    }

    #[inline]
    fn set_glyph_class(
        &mut self,
        glyph: GlyphId,
        class_guess: u16,
        ligature: bool,
        component: bool,
    ) {
        self.digest.add(glyph);
        let has_glyph_classes = self.face.has_glyph_classes();
        let gdef_props = self.face.glyph_props(glyph);
        let cur = self.buffer.cur_mut(0);
        let mut props = cur.glyph_props | glyph_props::SUBSTITUTED;
        if ligature {
            props |= glyph_props::LIGATED;
            props &= !glyph_props::MULTIPLIED;
        }
        if component {
            props |= glyph_props::MULTIPLIED;
        }
        cur.glyph_props = if has_glyph_classes {
            (props & glyph_props::PRESERVE) | gdef_props
        } else if class_guess != 0 {
            (props & glyph_props::PRESERVE) | class_guess
        } else {
            props
        };
    }

    #[inline]
    pub(crate) fn replace_glyph(&mut self, glyph: GlyphId) {
        self.set_glyph_class(glyph, 0, false, false);
        self.buffer.replace_glyph(u32::from(glyph.0));
    }

    #[inline]
    pub(crate) fn replace_glyph_inplace(&mut self, glyph: GlyphId) {
        self.set_glyph_class(glyph, 0, false, false);
        self.buffer.cur_mut(0).glyph_id = u32::from(glyph.0);
    }

    #[inline]
    pub(crate) fn replace_glyph_with_ligature(&mut self, glyph: GlyphId, class_guess: u16) {
        self.set_glyph_class(glyph, class_guess, true, false);
        self.buffer.replace_glyph(u32::from(glyph.0));
    }

    #[inline]
    pub(crate) fn output_glyph_for_component(&mut self, glyph: GlyphId, class_guess: u16) {
        self.set_glyph_class(glyph, class_guess, false, true);
        self.buffer.output_glyph(u32::from(glyph.0));
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Outcome {
    Match,
    NoMatch,
    Skip,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum MayMatch {
    No,
    Yes,
    Maybe,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum MaySkip {
    No,
    Yes,
    Maybe,
}

pub(crate) struct SkippyIter<'a, 'b, 'f> {
    ctx: &'a ApplyContext<'b, 'f>,
    lookup_props: u32,
    ignore_zwnj: bool,
    ignore_zwj: bool,
    mask: u32,
    matching: Option<&'a MatchFn<'a>>,
    buf_len: usize,
    glyph_data: u16,
    pub(crate) buf_idx: usize,
}

impl<'a, 'b, 'f> SkippyIter<'a, 'b, 'f> {
    #[inline]
    pub(crate) fn new(ctx: &'a ApplyContext<'b, 'f>, start: usize, context_match: bool) -> Self {
        SkippyIter {
            ctx,
            lookup_props: ctx.lookup_props,
            ignore_zwnj: context_match && ctx.auto_zwnj,
            ignore_zwj: context_match || ctx.auto_zwj,
            mask: if context_match {
                u32::MAX
            } else {
                ctx.lookup_mask()
            },
            matching: None,
            buf_len: ctx.buffer.len,
            glyph_data: 0,
            buf_idx: start,
        }
    }

    #[inline]
    pub(crate) fn enable_matching(&mut self, func: &'a MatchFn<'a>) {
        self.matching = Some(func);
    }

    #[inline]
    pub(crate) const fn index(&self) -> usize {
        self.buf_idx
    }

    #[inline]
    pub(crate) fn next(&mut self) -> bool {
        while self.buf_idx + 1 < self.buf_len {
            self.buf_idx += 1;
            let Some(info) = self.ctx.buffer.info.get(self.buf_idx) else {
                return false;
            };
            match self.match_(info) {
                Outcome::Match => {
                    self.glyph_data = self.glyph_data.wrapping_add(1);
                    return true;
                }
                Outcome::NoMatch => return false,
                Outcome::Skip => continue,
            }
        }
        false
    }

    #[inline]
    pub(crate) fn prev(&mut self) -> bool {
        while self.buf_idx > 0 {
            self.buf_idx -= 1;
            let Some(info) = self.ctx.buffer.out_info().get(self.buf_idx) else {
                return false;
            };
            match self.match_(info) {
                Outcome::Match => {
                    self.glyph_data = self.glyph_data.wrapping_add(1);
                    return true;
                }
                Outcome::NoMatch => return false,
                Outcome::Skip => continue,
            }
        }
        false
    }

    #[inline]
    fn match_(&self, info: &GlyphInfo) -> Outcome {
        let skip = self.may_skip(info);
        if skip == MaySkip::Yes {
            return Outcome::Skip;
        }
        let m = self.may_match(info);
        if m == MayMatch::Yes || (m == MayMatch::Maybe && skip == MaySkip::No) {
            return Outcome::Match;
        }
        if skip == MaySkip::No {
            return Outcome::NoMatch;
        }
        Outcome::Skip
    }

    #[inline]
    fn may_match(&self, info: &GlyphInfo) -> MayMatch {
        if info.mask & self.mask == 0 {
            return MayMatch::No;
        }
        match self.matching {
            Some(f) => {
                if f(info.as_glyph(), self.glyph_data) {
                    MayMatch::Yes
                } else {
                    MayMatch::No
                }
            }
            None => MayMatch::Maybe,
        }
    }

    #[inline]
    fn may_skip(&self, info: &GlyphInfo) -> MaySkip {
        if !self.ctx.check_glyph_property(info, self.lookup_props) {
            return MaySkip::Yes;
        }
        if is_default_ignorable_info(info)
            && (self.ignore_zwnj || !is_zwnj(info))
            && (self.ignore_zwj || !is_zwj(info))
            && !is_hidden(info)
        {
            return MaySkip::Maybe;
        }
        MaySkip::No
    }

    #[inline]
    pub(crate) fn may_skip_yes(&self, info: &GlyphInfo) -> bool {
        self.may_skip(info) == MaySkip::Yes
    }
}

/// Matches `input_len` more glyphs after the current one and returns the match end index.
pub(crate) fn match_input(
    ctx: &ApplyContext,
    input_len: u16,
    match_func: &MatchFn,
    positions: &mut MatchPositions,
    components: Option<&mut u8>,
) -> Option<usize> {
    #[derive(PartialEq)]
    enum Ligbase {
        NotChecked,
        MayNotSkip,
        MaySkip,
    }
    let count = usize::from(input_len) + 1;
    if count > MAX_CONTEXT_LENGTH {
        return None;
    }
    let mut iter = SkippyIter::new(ctx, ctx.buffer.idx, false);
    iter.enable_matching(match_func);
    let first = *ctx.buffer.cur(0);
    let first_lig_id = lig_id(&first);
    let first_lig_comp = lig_comp(&first);
    let mut total_component_count: u8 = 0;
    let mut ligbase = Ligbase::NotChecked;
    for position in positions.iter_mut().take(count).skip(1) {
        if !iter.next() {
            return None;
        }
        *position = iter.index();
        let this = ctx.buffer.info[iter.index()];
        let this_lig_id = lig_id(&this);
        let this_lig_comp = lig_comp(&this);
        if first_lig_id != 0 && first_lig_comp != 0 {
            // A first glyph attached to an earlier ligature's component: the
            // rest must attach to the same component...
            if first_lig_id != this_lig_id || first_lig_comp != this_lig_comp {
                // ...unless the ligature they attach to may be skipped.
                if ligbase == Ligbase::NotChecked {
                    let out = ctx.buffer.out_info();
                    let mut j = ctx.buffer.out_len;
                    let mut found = false;
                    while j > 0 && lig_id(&out[j - 1]) == first_lig_id {
                        if lig_comp(&out[j - 1]) == 0 {
                            j -= 1;
                            found = true;
                            break;
                        }
                        j -= 1;
                    }
                    ligbase = if found && iter.may_skip_yes(&out[j]) {
                        Ligbase::MaySkip
                    } else {
                        Ligbase::MayNotSkip
                    };
                }
                if ligbase == Ligbase::MayNotSkip {
                    return None;
                }
            }
        } else if this_lig_id != 0 && this_lig_comp != 0 && this_lig_id != first_lig_id {
            return None;
        }
        total_component_count = total_component_count.wrapping_add(lig_num_comps(&this));
    }
    let end = iter.index() + 1;
    if let Some(components) = components {
        *components = total_component_count.wrapping_add(lig_num_comps(&first));
    }
    positions[0] = ctx.buffer.idx;
    Some(end)
}

/// Matches `backtrack_len` glyphs before the current one, in the output.
pub(crate) fn match_backtrack(
    ctx: &ApplyContext,
    backtrack_len: u16,
    match_func: &MatchFn,
) -> bool {
    let mut iter = SkippyIter::new(ctx, ctx.buffer.backtrack_len(), true);
    iter.enable_matching(match_func);
    (0..backtrack_len).all(|_| iter.prev())
}

/// Matches `lookahead_len` glyphs from `start_index` on.
pub(crate) fn match_lookahead(
    ctx: &ApplyContext,
    lookahead_len: u16,
    match_func: &MatchFn,
    start_index: usize,
) -> bool {
    let Some(start) = start_index.checked_sub(1) else {
        return false;
    };
    let mut iter = SkippyIter::new(ctx, start, true);
    iter.enable_matching(match_func);
    (0..lookahead_len).all(|_| iter.next())
}

#[inline]
pub(crate) fn match_glyph(glyph: GlyphId, value: u16) -> bool {
    glyph == GlyphId(value)
}

#[inline]
fn match_class<'a>(class_def: ClassDefinition<'a>) -> impl Fn(GlyphId, u16) -> bool + 'a {
    move |glyph, value| class_def.get(glyph) == value
}

pub(crate) fn apply_context_lookup(ctx: &mut ApplyContext, table: &ContextLookup) -> Option<()> {
    let glyph = ctx.buffer.cur(0).as_glyph();
    match *table {
        ContextLookup::Format1 { coverage, sets } => {
            let index = coverage.get(glyph)?;
            let set = sets.get(index)?;
            apply_rule_set(ctx, set, &match_glyph)
        }
        ContextLookup::Format2 {
            coverage,
            classes,
            sets,
        } => {
            coverage.get(glyph)?;
            let class = classes.get(glyph);
            let set = sets.get(class)?;
            apply_rule_set(ctx, set, &match_class(classes))
        }
        ContextLookup::Format3 {
            coverage,
            coverages,
            lookups,
        } => {
            coverage.get(glyph)?;
            let match_func =
                |glyph, index| coverages.get(index).is_some_and(|c| c.get(glyph).is_some());
            let mut positions = [0; MAX_CONTEXT_LENGTH];
            let end = match_input(ctx, coverages.len(), &match_func, &mut positions, None)?;
            apply_lookup(
                ctx,
                usize::from(coverages.len()),
                &mut positions,
                end,
                lookups,
            );
            Some(())
        }
    }
}

fn apply_rule_set(
    ctx: &mut ApplyContext,
    set: crate::opentype_layout::SequenceRuleSet,
    match_func: &MatchFn,
) -> Option<()> {
    for rule in set {
        let input = rule.input;
        let f = |glyph, index| {
            input
                .get(index)
                .is_some_and(|value| match_func(glyph, value))
        };
        let mut positions = [0; MAX_CONTEXT_LENGTH];
        if let Some(end) = match_input(ctx, input.len(), &f, &mut positions, None) {
            apply_lookup(
                ctx,
                usize::from(input.len()),
                &mut positions,
                end,
                rule.lookups,
            );
            return Some(());
        }
    }
    None
}

pub(crate) fn apply_chain_context_lookup(
    ctx: &mut ApplyContext,
    table: &ChainedContextLookup,
) -> Option<()> {
    let glyph = ctx.buffer.cur(0).as_glyph();
    match *table {
        ChainedContextLookup::Format1 { coverage, sets } => {
            let index = coverage.get(glyph)?;
            let set = sets.get(index)?;
            for rule in set {
                if apply_chain_rule(
                    ctx,
                    rule.backtrack,
                    rule.input,
                    rule.lookahead,
                    [&match_glyph, &match_glyph, &match_glyph],
                    rule.lookups,
                )
                .is_some()
                {
                    return Some(());
                }
            }
            None
        }
        ChainedContextLookup::Format2 {
            coverage,
            backtrack_classes,
            input_classes,
            lookahead_classes,
            sets,
        } => {
            coverage.get(glyph)?;
            let class = input_classes.get(glyph);
            let set = sets.get(class)?;
            let back = match_class(backtrack_classes);
            let input = match_class(input_classes);
            let ahead = match_class(lookahead_classes);
            for rule in set {
                if apply_chain_rule(
                    ctx,
                    rule.backtrack,
                    rule.input,
                    rule.lookahead,
                    [&back, &input, &ahead],
                    rule.lookups,
                )
                .is_some()
                {
                    return Some(());
                }
            }
            None
        }
        ChainedContextLookup::Format3 {
            coverage,
            backtrack_coverages,
            input_coverages,
            lookahead_coverages,
            lookups,
        } => {
            coverage.get(glyph)?;
            let back = |glyph, index| {
                backtrack_coverages
                    .get(index)
                    .is_some_and(|c| c.contains(glyph))
            };
            let ahead = |glyph, index| {
                lookahead_coverages
                    .get(index)
                    .is_some_and(|c| c.contains(glyph))
            };
            let input = |glyph, index| {
                input_coverages
                    .get(index)
                    .is_some_and(|c| c.contains(glyph))
            };
            let mut positions = [0; MAX_CONTEXT_LENGTH];
            let end = match_input(ctx, input_coverages.len(), &input, &mut positions, None)?;
            if !match_lookahead(ctx, lookahead_coverages.len(), &ahead, end) {
                return None;
            }
            if !match_backtrack(ctx, backtrack_coverages.len(), &back) {
                return None;
            }
            apply_lookup(
                ctx,
                usize::from(input_coverages.len()),
                &mut positions,
                end,
                lookups,
            );
            Some(())
        }
    }
}

fn apply_chain_rule(
    ctx: &mut ApplyContext,
    backtrack: LazyArray16<u16>,
    input: LazyArray16<u16>,
    lookahead: LazyArray16<u16>,
    match_funcs: [&MatchFn; 3],
    lookups: LazyArray16<SequenceLookupRecord>,
) -> Option<()> {
    let back = |glyph, index| {
        backtrack
            .get(index)
            .is_some_and(|v| match_funcs[0](glyph, v))
    };
    let ahead = |glyph, index| {
        lookahead
            .get(index)
            .is_some_and(|v| match_funcs[2](glyph, v))
    };
    let inp = |glyph, index| input.get(index).is_some_and(|v| match_funcs[1](glyph, v));
    let mut positions = [0; MAX_CONTEXT_LENGTH];
    let end = match_input(ctx, input.len(), &inp, &mut positions, None)?;
    if !match_lookahead(ctx, lookahead.len(), &ahead, end) {
        return None;
    }
    if !match_backtrack(ctx, backtrack.len(), &back) {
        return None;
    }
    apply_lookup(ctx, usize::from(input.len()), &mut positions, end, lookups);
    Some(())
}

/// Runs a rule's nested lookups at their sequence positions, keeping the
/// positions after each in step with whatever it added or removed.
fn apply_lookup(
    ctx: &mut ApplyContext,
    input_len: usize,
    positions: &mut MatchPositions,
    match_end: usize,
    lookups: LazyArray16<SequenceLookupRecord>,
) {
    let mut count = input_len + 1;
    let backtrack_len = ctx.buffer.backtrack_len() as isize;
    let delta = backtrack_len - ctx.buffer.idx as isize;
    for position in positions.iter_mut().take(count) {
        *position = (*position as isize + delta) as usize;
    }
    let mut end: isize = backtrack_len + match_end as isize - ctx.buffer.idx as isize;

    for record in lookups {
        if !ctx.buffer.successful {
            break;
        }
        let idx = usize::from(record.sequence_index);
        if idx >= count {
            continue;
        }
        let orig_len = ctx.buffer.backtrack_len() + ctx.buffer.lookahead_len();
        if positions[idx] >= orig_len {
            continue;
        }
        if !ctx.buffer.move_to(positions[idx]) {
            break;
        }
        if ctx.buffer.max_ops <= 0 {
            break;
        }
        if ctx.recurse(record.lookup_list_index).is_none() {
            continue;
        }
        let new_len = ctx.buffer.backtrack_len() + ctx.buffer.lookahead_len();
        let mut delta = new_len as isize - orig_len as isize;
        if delta == 0 {
            continue;
        }
        end += delta;
        if end < positions[idx] as isize {
            delta += positions[idx] as isize - end;
            end = positions[idx] as isize;
        }
        let mut next = idx + 1;
        if delta > 0 {
            if delta as usize + count > MAX_CONTEXT_LENGTH {
                break;
            }
        } else {
            delta = delta.max(next as isize - count as isize);
            next = (next as isize - delta) as usize;
        }
        let to = (next as isize + delta) as usize;
        if next > count || to + (count - next) > MAX_CONTEXT_LENGTH {
            break;
        }
        positions.copy_within(next..count, to);
        next = to;
        count = (count as isize + delta) as usize;
        for j in idx + 1..next {
            positions[j] = positions[j - 1] + 1;
        }
        while next < count {
            positions[next] = (positions[next] as isize + delta) as usize;
            next += 1;
        }
    }
    if let Ok(end) = usize::try_from(end) {
        ctx.buffer.move_to(end);
    }
}

/// Replaces the matched glyphs with `lig_glyph`, merging clusters and renumbering mark components.
pub(crate) fn ligate_input(
    ctx: &mut ApplyContext,
    count: usize,
    positions: &MatchPositions,
    match_end: usize,
    total_component_count: u8,
    lig_glyph: GlyphId,
) {
    let buffer = &mut *ctx.buffer;
    buffer.merge_clusters(buffer.idx, match_end);
    let mut is_base_ligature = is_base_glyph(&buffer.info[positions[0]]);
    let mut is_mark_ligature = is_mark(&buffer.info[positions[0]]);
    for &position in positions.iter().take(count).skip(1) {
        if !is_mark(&buffer.info[position]) {
            is_base_ligature = false;
            is_mark_ligature = false;
        }
    }
    let is_ligature = !is_base_ligature && !is_mark_ligature;
    let class = if is_ligature {
        glyph_props::LIGATURE
    } else {
        0
    };
    let lig_id_new = if is_ligature {
        buffer.allocate_lig_id()
    } else {
        0
    };
    let first = buffer.cur_mut(0);
    let mut last_lig_id = lig_id(first);
    let mut last_num_comps = lig_num_comps(first);
    let mut comps_so_far = last_num_comps;
    if is_ligature {
        set_lig_props_for_ligature(first, lig_id_new, total_component_count);
        if first.gc == GeneralCategory::NonspacingMark {
            set_general_category(first, GeneralCategory::OtherLetter);
        }
    }
    ctx.replace_glyph_with_ligature(lig_glyph, class);
    let buffer = &mut *ctx.buffer;
    for &position in positions.iter().take(count).skip(1) {
        while buffer.idx < position && buffer.successful {
            if is_ligature {
                let cur = buffer.cur_mut(0);
                let mut this_comp = lig_comp(cur);
                if this_comp == 0 {
                    this_comp = last_num_comps;
                }
                let Some(base) = comps_so_far.checked_sub(last_num_comps) else {
                    return;
                };
                let new_lig_comp = base.wrapping_add(this_comp.min(last_num_comps));
                set_lig_props_for_mark(cur, lig_id_new, new_lig_comp);
            }
            buffer.next_glyph();
        }
        if buffer.idx >= buffer.len {
            return;
        }
        let cur = buffer.cur(0);
        last_lig_id = lig_id(cur);
        last_num_comps = lig_num_comps(cur);
        comps_so_far = comps_so_far.wrapping_add(last_num_comps);
        buffer.idx += 1;
    }
    if !is_mark_ligature && last_lig_id != 0 {
        for i in buffer.idx..buffer.len {
            let info = &mut buffer.info[i];
            if last_lig_id != lig_id(info) {
                break;
            }
            let this_comp = lig_comp(info);
            if this_comp == 0 {
                break;
            }
            let Some(base) = comps_so_far.checked_sub(last_num_comps) else {
                return;
            };
            let new_lig_comp = base.wrapping_add(this_comp.min(last_num_comps));
            set_lig_props_for_mark(info, lig_id_new, new_lig_comp);
        }
    }
}
