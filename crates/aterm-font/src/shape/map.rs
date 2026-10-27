// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot_map.rs and src/hb/ot_layout.rs
// (`LayoutTableExt`) and modified by the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Feature map compilation: active features, mask bit allocation, and per-stage `GSUB` lookups.

use super::ShapeFace;
use super::buffer::Buffer;
use super::plan::ShapePlan;
use crate::Tag;
use crate::opentype_layout::{
    FeatureIndex, LanguageIndex, LayoutTable, LookupIndex, ScriptIndex, VariationIndex,
};

/// Callback invoked between shaping stages.
pub(crate) type PauseFn = fn(&ShapePlan, &ShapeFace, &mut Buffer);

pub(crate) const F_NONE: u32 = 0x0000;
/// Applies to every character: no mask bits of its own.
pub(crate) const F_GLOBAL: u32 = 0x0001;
/// Has a fallback, so it gets a mask bit even when the font lacks it.
pub(crate) const F_HAS_FALLBACK: u32 = 0x0002;
/// Do not skip ZWNJ when matching context.
pub(crate) const F_MANUAL_ZWNJ: u32 = 0x0004;
/// Do not skip ZWJ when matching input.
pub(crate) const F_MANUAL_ZWJ: u32 = 0x0008;
pub(crate) const F_MANUAL_JOINERS: u32 = F_MANUAL_ZWNJ | F_MANUAL_ZWJ;
pub(crate) const F_GLOBAL_MANUAL_JOINERS: u32 = F_GLOBAL | F_MANUAL_JOINERS;
pub(crate) const F_GLOBAL_HAS_FALLBACK: u32 = F_GLOBAL | F_HAS_FALLBACK;
/// Not in the language system: search the whole feature list.
pub(crate) const F_GLOBAL_SEARCH: u32 = 0x0010;
/// Picks an alternate at random.
pub(crate) const F_RANDOM: u32 = 0x0020;

pub(crate) const MAX_BITS: u32 = 8;
pub(crate) const MAX_VALUE: u32 = (1 << MAX_BITS) - 1;

const GLOBAL_BIT_SHIFT: u32 = 31;
const GLOBAL_BIT_MASK: u32 = 1 << GLOBAL_BIT_SHIFT;
const FIRST_FEATURE_BIT: u32 = 4;

pub(crate) const GSUB: usize = 0;
pub(crate) const GPOS: usize = 1;

#[derive(Clone, Copy, Debug)]
struct FeatureMap {
    tag: Tag,
    index: [Option<FeatureIndex>; 2],
    stage: [usize; 2],
    mask: u32,
    one_mask: u32,
    auto_zwnj: bool,
    auto_zwj: bool,
    random: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LookupMap {
    pub(crate) index: LookupIndex,
    pub(crate) auto_zwnj: bool,
    pub(crate) auto_zwj: bool,
    pub(crate) random: bool,
    pub(crate) mask: u32,
}

#[derive(Clone, Copy)]
pub(crate) struct StageMap {
    /// Cumulative: the stage's lookups end here.
    pub(crate) last_lookup: usize,
    pub(crate) pause: Option<PauseFn>,
}

/// A compiled feature map.
pub(crate) struct Map {
    global_mask: u32,
    features: Vec<FeatureMap>,
    lookups: [Vec<LookupMap>; 2],
    stages: [Vec<StageMap>; 2],
}

impl Map {
    #[inline]
    pub(crate) const fn global_mask(&self) -> u32 {
        self.global_mask
    }

    #[inline]
    fn feature(&self, tag: Tag) -> Option<&FeatureMap> {
        self.features
            .binary_search_by_key(&tag, |f| f.tag)
            .ok()
            .and_then(|i| self.features.get(i))
    }

    #[inline]
    pub(crate) fn get_mask(&self, tag: Tag) -> u32 {
        self.feature(tag).map_or(0, |f| f.mask)
    }

    #[inline]
    pub(crate) fn get_1_mask(&self, tag: Tag) -> u32 {
        self.feature(tag).map_or(0, |f| f.one_mask)
    }

    #[inline]
    pub(crate) fn get_feature_index(&self, table: usize, tag: Tag) -> Option<FeatureIndex> {
        self.feature(tag).and_then(|f| f.index[table])
    }

    #[inline]
    pub(crate) const fn stages(&self, table: usize) -> &[StageMap] {
        self.stages[table].as_slice()
    }

    #[inline]
    pub(crate) fn stage_lookups(&self, table: usize, stage: usize) -> &[LookupMap] {
        let start = stage
            .checked_sub(1)
            .and_then(|prev| self.stages[table].get(prev))
            .map_or(0, |s| s.last_lookup);
        let end = self.stages[table]
            .get(stage)
            .map_or(self.lookups[table].len(), |s| s.last_lookup);
        self.lookups[table].get(start..end).unwrap_or(&[])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FeatureInfo {
    tag: Tag,
    seq: usize,
    max_value: u32,
    flags: u32,
    default_value: u32,
    stage: [usize; 2],
}

#[derive(Clone, Copy)]
struct StageInfo {
    index: usize,
    pause: Option<PauseFn>,
}

pub(crate) struct MapBuilder<'a, 'f> {
    face: &'a ShapeFace<'f>,
    script_index: [Option<ScriptIndex>; 2],
    chosen_script: [Option<Tag>; 2],
    lang_index: [Option<LanguageIndex>; 2],
    current_stage: [usize; 2],
    feature_infos: Vec<FeatureInfo>,
    stages: [Vec<StageInfo>; 2],
    pub(crate) is_simple: bool,
}

fn tables<'f>(face: &ShapeFace<'f>) -> [Option<LayoutTable<'f>>; 2] {
    [face.face.tables().gsub, face.face.tables().gpos]
}

fn select_script(table: &LayoutTable, script_tags: &[Tag]) -> Option<(bool, ScriptIndex, Tag)> {
    for &tag in script_tags {
        if let Some(index) = table.scripts.index(tag) {
            return Some((true, index, tag));
        }
    }
    for tag in [
        Tag::from_bytes(b"DFLT"),
        Tag::from_bytes(b"dflt"),
        Tag::from_bytes(b"latn"),
    ] {
        if let Some(index) = table.scripts.index(tag) {
            return Some((false, index, tag));
        }
    }
    None
}

fn select_language(table: &LayoutTable, script_index: ScriptIndex) -> Option<LanguageIndex> {
    let script = table.scripts.get(script_index)?;
    script.languages.index(Tag::from_bytes(b"dflt"))
}

fn language_system<'f>(
    table: &LayoutTable<'f>,
    script_index: ScriptIndex,
    lang_index: Option<LanguageIndex>,
) -> Option<crate::opentype_layout::LanguageSystem<'f>> {
    let script = table.scripts.get(script_index)?;
    match lang_index {
        Some(index) => script.languages.get(index),
        None => script.default_language,
    }
}

fn required_feature(
    table: &LayoutTable,
    script_index: ScriptIndex,
    lang_index: Option<LanguageIndex>,
) -> Option<(FeatureIndex, Tag)> {
    let sys = language_system(table, script_index, lang_index)?;
    let index = sys.required_feature?;
    let tag = table.features.get(index)?.tag;
    Some((index, tag))
}

fn find_language_feature(
    table: &LayoutTable,
    script_index: ScriptIndex,
    lang_index: Option<LanguageIndex>,
    tag: Tag,
) -> Option<FeatureIndex> {
    let sys = language_system(table, script_index, lang_index)?;
    sys.feature_indices
        .into_iter()
        .find(|&index| table.features.get(index).map(|f| f.tag) == Some(tag))
}

impl<'a, 'f> MapBuilder<'a, 'f> {
    pub(crate) fn new(face: &'a ShapeFace<'f>, script_tags: &[Tag]) -> Self {
        let mut script_index = [None; 2];
        let mut chosen_script = [None; 2];
        let mut lang_index = [None; 2];
        for (i, table) in tables(face).iter().enumerate() {
            let Some(table) = table else { continue };
            if let Some((_found, index, tag)) = select_script(table, script_tags) {
                chosen_script[i] = Some(tag);
                script_index[i] = Some(index);
                lang_index[i] = select_language(table, index);
            }
        }
        MapBuilder {
            face,
            script_index,
            chosen_script,
            lang_index,
            current_stage: [0, 0],
            feature_infos: Vec::new(),
            stages: [Vec::new(), Vec::new()],
            is_simple: false,
        }
    }

    pub(crate) fn chosen_script_gsub(&self) -> Option<Tag> {
        self.chosen_script[GSUB]
    }

    pub(crate) fn chosen_script_gpos(&self) -> Option<Tag> {
        self.chosen_script[GPOS]
    }

    pub(crate) fn has_feature(&self, tag: Tag) -> bool {
        tables(self.face)
            .iter()
            .enumerate()
            .any(|(i, table)| match (table, self.script_index[i]) {
                (Some(table), Some(script)) => {
                    find_language_feature(table, script, self.lang_index[i], tag).is_some()
                }
                _ => false,
            })
    }

    pub(crate) fn add_feature(&mut self, tag: Tag, flags: u32, value: u32) {
        if tag.0 == 0 {
            return;
        }
        let seq = self.feature_infos.len();
        self.feature_infos.push(FeatureInfo {
            tag,
            seq,
            max_value: value,
            flags,
            default_value: if flags & F_GLOBAL != 0 { value } else { 0 },
            stage: self.current_stage,
        });
    }

    pub(crate) fn enable_feature(&mut self, tag: Tag, flags: u32, value: u32) {
        self.add_feature(tag, flags | F_GLOBAL, value);
    }

    pub(crate) fn add_gsub_pause(&mut self, pause: Option<PauseFn>) {
        self.add_pause(GSUB, pause);
    }

    fn add_pause(&mut self, table: usize, pause: Option<PauseFn>) {
        self.stages[table].push(StageInfo {
            index: self.current_stage[table],
            pause,
        });
        self.current_stage[table] += 1;
    }

    pub(crate) fn compile(&mut self) -> Map {
        let mut required_index = [None; 2];
        let mut required_tag = [None; 2];
        for (i, table) in tables(self.face).iter().enumerate() {
            if let (Some(table), Some(script)) = (table, self.script_index[i])
                && let Some((index, tag)) = required_feature(table, script, self.lang_index[i])
            {
                required_index[i] = Some(index);
                required_tag[i] = Some(tag);
            }
        }
        let (features, required_stage, global_mask) = self.collect_feature_maps(required_tag);
        self.add_gsub_pause(None);
        self.add_pause(GPOS, None);
        let (gsub_lookups, gsub_stages) =
            self.collect_lookups(GSUB, &features, required_index[GSUB], required_stage[GSUB]);
        let (gpos_lookups, gpos_stages) =
            self.collect_lookups(GPOS, &features, required_index[GPOS], required_stage[GPOS]);
        Map {
            global_mask,
            features,
            lookups: [gsub_lookups, gpos_lookups],
            stages: [gsub_stages, gpos_stages],
        }
    }

    fn collect_feature_maps(
        &mut self,
        required_tag: [Option<Tag>; 2],
    ) -> (Vec<FeatureMap>, [usize; 2], u32) {
        let mut map_features = Vec::new();
        let mut required_stage = [0; 2];
        let mut global_mask = GLOBAL_BIT_MASK;
        let mut next_bit = FIRST_FEATURE_BIT;
        self.dedup_feature_infos();
        let layout = tables(self.face);
        for info in &self.feature_infos {
            let bits_needed = if info.flags & F_GLOBAL != 0 && info.max_value == 1 {
                0
            } else {
                let v = info.max_value;
                MAX_BITS.min(32 - v.leading_zeros())
            };
            if info.max_value == 0 || next_bit + bits_needed >= GLOBAL_BIT_SHIFT {
                continue;
            }
            let mut found = false;
            let mut feature_index = [None; 2];
            for (i, table) in layout.iter().enumerate() {
                let Some(table) = table else { continue };
                if required_tag[i] == Some(info.tag) {
                    required_stage[i] = info.stage[i];
                }
                if let Some(script) = self.script_index[i]
                    && let Some(index) =
                        find_language_feature(table, script, self.lang_index[i], info.tag)
                {
                    feature_index[i] = Some(index);
                    found = true;
                }
            }
            if !found && info.flags & F_GLOBAL_SEARCH != 0 {
                for (i, table) in layout.iter().enumerate() {
                    if let Some(table) = table
                        && let Some(index) = table.features.index(info.tag)
                    {
                        feature_index[i] = Some(index);
                        found = true;
                    }
                }
            }
            if !found && info.flags & F_HAS_FALLBACK == 0 {
                continue;
            }
            let (shift, mask) = if info.flags & F_GLOBAL != 0 && info.max_value == 1 {
                (GLOBAL_BIT_SHIFT, GLOBAL_BIT_MASK)
            } else {
                let shift = next_bit;
                let mask = (1u32 << (next_bit + bits_needed)) - (1u32 << next_bit);
                next_bit += bits_needed;
                global_mask |= (info.default_value << shift) & mask;
                (shift, mask)
            };
            map_features.push(FeatureMap {
                tag: info.tag,
                index: feature_index,
                stage: info.stage,
                mask,
                one_mask: (1u32 << shift) & mask,
                auto_zwnj: info.flags & F_MANUAL_ZWNJ == 0,
                auto_zwj: info.flags & F_MANUAL_ZWJ == 0,
                random: info.flags & F_RANDOM != 0,
            });
        }
        if self.is_simple {
            map_features.sort_by_key(|f| f.tag);
        }
        (map_features, required_stage, global_mask)
    }

    fn dedup_feature_infos(&mut self) {
        let infos = &mut self.feature_infos;
        if infos.is_empty() {
            return;
        }
        if !self.is_simple {
            infos.sort();
        }
        let mut j = 0;
        for i in 1..infos.len() {
            if infos[i].tag != infos[j].tag {
                j += 1;
                infos[j] = infos[i];
            } else {
                if infos[i].flags & F_GLOBAL != 0 {
                    infos[j].flags |= F_GLOBAL;
                    infos[j].max_value = infos[i].max_value;
                    infos[j].default_value = infos[i].default_value;
                } else {
                    if infos[j].flags & F_GLOBAL != 0 {
                        infos[j].flags ^= F_GLOBAL;
                    }
                    infos[j].max_value = infos[j].max_value.max(infos[i].max_value);
                }
                let fallback = infos[i].flags & F_HAS_FALLBACK;
                infos[j].flags |= fallback;
                infos[j].stage[0] = infos[j].stage[0].min(infos[i].stage[0]);
                infos[j].stage[1] = infos[j].stage[1].min(infos[i].stage[1]);
            }
        }
        infos.truncate(j + 1);
    }

    fn collect_lookups(
        &self,
        table_index: usize,
        features: &[FeatureMap],
        required_index: Option<FeatureIndex>,
        required_stage: usize,
    ) -> (Vec<LookupMap>, Vec<StageMap>) {
        let mut lookups: Vec<LookupMap> = Vec::new();
        let mut stages = Vec::new();
        let Some(table) = tables(self.face)[table_index] else {
            for info in &self.stages[table_index] {
                stages.push(StageMap {
                    last_lookup: 0,
                    pause: info.pause,
                });
            }
            return (lookups, stages);
        };
        let coords = self.face.face.variation_coordinates();
        let variation_index = table.variations.and_then(|v| v.find_index(coords));
        let mut stage_index = 0;
        let mut last_lookup = 0;
        for stage in 0..self.current_stage[table_index] {
            if let Some(index) = required_index
                && required_stage == stage
            {
                add_lookups(
                    &table,
                    &mut lookups,
                    index,
                    variation_index,
                    GLOBAL_BIT_MASK,
                    true,
                    true,
                    false,
                );
            }
            for feature in features {
                if let Some(index) = feature.index[table_index]
                    && feature.stage[table_index] == stage
                {
                    add_lookups(
                        &table,
                        &mut lookups,
                        index,
                        variation_index,
                        feature.mask,
                        feature.auto_zwnj,
                        feature.auto_zwj,
                        feature.random,
                    );
                }
            }
            let len = lookups.len();
            if last_lookup + 1 < len {
                lookups[last_lookup..].sort();
                let mut j = last_lookup;
                for i in j + 1..len {
                    if lookups[i].index != lookups[j].index {
                        j += 1;
                        lookups[j] = lookups[i];
                    } else {
                        lookups[j].mask |= lookups[i].mask;
                        lookups[j].auto_zwnj &= lookups[i].auto_zwnj;
                        lookups[j].auto_zwj &= lookups[i].auto_zwj;
                    }
                }
                lookups.truncate(j + 1);
            }
            last_lookup = lookups.len();
            if let Some(info) = self.stages[table_index].get(stage_index)
                && info.index == stage
            {
                stages.push(StageMap {
                    last_lookup,
                    pause: info.pause,
                });
                stage_index += 1;
            }
        }
        (lookups, stages)
    }
}

#[allow(clippy::too_many_arguments)]
fn add_lookups(
    table: &LayoutTable,
    lookups: &mut Vec<LookupMap>,
    feature_index: FeatureIndex,
    variation_index: Option<VariationIndex>,
    mask: u32,
    auto_zwnj: bool,
    auto_zwj: bool,
    random: bool,
) -> Option<()> {
    let lookup_count = table.lookups.len();
    let feature = match variation_index {
        Some(idx) => table
            .variations
            .and_then(|var| var.find_substitute(feature_index, idx))
            .or_else(|| table.features.get(feature_index))?,
        None => table.features.get(feature_index)?,
    };
    for index in feature.lookup_indices {
        if index < lookup_count {
            lookups.push(LookupMap {
                index,
                auto_zwnj,
                auto_zwj,
                random,
                mask,
            });
        }
    }
    Some(())
}
