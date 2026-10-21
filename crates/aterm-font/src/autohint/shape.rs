// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/shape.rs and read-fonts
// 0.43.2 src/tables/layout/script.rs (`ScriptTags`, `ScriptList::select`,
// `LangSys::feature_index_for_tag`), and modified by the aterm project in
// 2026: walking aterm-font's `GSUB` reader, in skrifa's default "best effort"
// mode only (the nominal mode served upstream's FreeType validation).
// See ../../LICENSE-FONTATIONS for the upstream license.

//! Shaping support for autohinting: the glyphs a style's reference
//! characters map to (through a style's feature where it has one), and the
//! glyphs `GSUB` reaches, which take the style of the script whose lookups
//! reach them (FreeType afshaper.c). Every walk of a shaper's `GSUB` spends
//! one budget ([`LayoutWork`]), one unit per lookup, subtable and entry it
//! visits; a walk that runs past it refuses the table.

use super::GlyphId;
use super::charmap::Charmap;
use super::font::HintFont;
use super::style::{GlyphStyle, StyleClass};
use crate::Tag;
use crate::gsub::SubstitutionSubtable;
use crate::opentype_layout::{
    ChainedContextLookup, ContextLookup, Coverage, Feature, LayoutTable, LookupList, ScriptList,
};
use crate::work::LayoutWork;
use core::ops::Range;

/// The traversals of `GSUB` a shaper's walks may spend ([`LayoutWork`]).
/// The style walk visits each lookup once per style whose scripts reach it,
/// so a face whose lookups serve several scripts may be walked several
/// times over. The most on this Mac (2026-10-19, 523 faces with `GSUB`) is
/// 0.26 traversals, by a Hiragino Sans W3 face, and a style's metrics walk
/// spends at most 0.04 (Trattatello).
const STYLE_WALK_TRAVERSALS: u64 = 8;

// To prevent infinite recursion in contextual lookups. Matches HB
// <https://github.com/harfbuzz/harfbuzz/blob/c7ef6a2ed58ae8ec108ee0962bef46f42c73a60c/src/hb-limits.hh#L53>
const MAX_NESTING_DEPTH: usize = 64;

#[derive(Copy, Clone, Default, Debug)]
pub(crate) struct ShapedGlyph {
    pub id: GlyphId,
    /// This may be used for computing vertical alignment zones, particularly
    /// for glyphs like super/subscripts which might have adjustments in GPOS.
    ///
    /// Note that we don't do the same in the horizontal direction which
    /// means that we don't care about the x-offset.
    pub y_offset: i32,
}

/// Container for storing the result of shaping a cluster.
///
/// Some of our input "characters" for metrics computations are actually
/// multi-character [grapheme clusters](https://www.unicode.org/reports/tr29/#Grapheme_Cluster_Boundaries)
/// that may expand to multiple glyphs.
pub(crate) type ShapedCluster = Vec<ShapedGlyph>;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum ShaperCoverageKind {
    /// Shaper coverage that traverses a specific script.
    Script,
    /// Shaper coverage that also includes the `Dflt` script.
    ///
    /// This is used as a catch all after all styles are processed.
    Default,
}

/// The tags of a script list's records, unparsed.
fn record_tags(list: &ScriptList) -> Vec<Tag> {
    (0..list.len()).filter_map(|i| list.tag(i)).collect()
}

/// The index of the script with the given tag, by binary search over the
/// record tags as read-fonts does it.
fn index_for_tag(tags: &[Tag], tag: Tag) -> Option<u16> {
    tags.binary_search(&tag).ok().map(|ix| ix as u16)
}

/// The first script among `tags` the list has, else `DFLT`, `dflt` or
/// `latn` (HarfBuzz's `hb_ot_layout_table_select_script`).
fn select_script(list: &ScriptList, tags: &[Tag]) -> Option<u16> {
    let have = record_tags(list);
    for &tag in tags {
        if let Some(index) = index_for_tag(&have, tag) {
            return Some(index);
        }
    }
    [b"DFLT", b"dflt", b"latn"]
        .into_iter()
        .find_map(|tag| index_for_tag(&have, Tag::from_bytes(tag)))
}

/// The OpenType script tags for a Unicode (ISO 15924) script, preferred
/// first (HarfBuzz's `hb_ot_all_tags_from_script`): the version 3 and 2 tags
/// of the Indic scripts that have them, then the old tag.
fn script_tags_from_unicode(unicode_script: Tag) -> Vec<Tag> {
    // Unicode script code to "new" OpenType script tag.
    const NEW_TAGS: &[(&[u8; 4], &[u8; 4])] = &[
        (b"Beng", b"bng2"),
        (b"Deva", b"dev2"),
        (b"Gujr", b"gjr2"),
        (b"Guru", b"gur2"),
        (b"Knda", b"knd2"),
        (b"Mlym", b"mlm2"),
        (b"Mymr", b"mym2"),
        (b"Orya", b"ory2"),
        (b"Taml", b"tml2"),
        (b"Telu", b"tel2"),
    ];
    let mut tags = Vec::with_capacity(3);
    let bytes = unicode_script.to_bytes();
    if let Some((_, new_tag)) = NEW_TAGS.iter().find(|(from, _)| **from == bytes) {
        // Myanmar maps to mym2 but there is no mym3.
        if **new_tag != *b"mym2" {
            let mut v3 = **new_tag;
            v3[3] = b'3';
            tags.push(Tag::from_bytes(&v3));
        }
        tags.push(Tag::from_bytes(new_tag));
    }
    // The old tag (HarfBuzz hb-ot-tag.cc).
    let old: [u8; 4] = match &bytes {
        b"Zmth" => *b"math",
        // Katakana and Hiragana both map to 'kana'
        b"Hira" => *b"kana",
        // Spaces at the end are preserved, unlike ISO 15924
        b"Laoo" => *b"lao ",
        b"Yiii" => *b"yi  ",
        b"Nkoo" => *b"nko ",
        b"Vaii" => *b"vai ",
        _ => {
            // Else, just change the first char to lowercase
            let mut b = bytes;
            b[0] = b[0].to_ascii_lowercase();
            b
        }
    };
    tags.push(Tag::from_bytes(&old));
    tags
}

/// The first feature with `tag` among a language system's features.
fn feature_index_for_tag(
    features: &crate::opentype_layout::FeatureList,
    indices: crate::LazyArray16<u16>,
    tag: Tag,
) -> Option<u16> {
    indices
        .into_iter()
        .find(|&feature_ix| features.tag(feature_ix) == Some(tag))
}

/// The `GSUB` subtables of a lookup, each that parses (a malformed one is
/// skipped, not the rest of the lookup).
fn subtables<'a>(
    lookup: &crate::opentype_layout::Lookup<'a>,
) -> impl Iterator<Item = SubstitutionSubtable<'a>> {
    let subtables = lookup.subtables;
    (0..subtables.len()).filter_map(move |i| subtables.get::<SubstitutionSubtable>(i))
}

/// `items`, one unit of `work` each, ending where the budget does.
fn charged<'w, I: Iterator + 'w>(
    work: &'w LayoutWork,
    items: I,
) -> impl Iterator<Item = I::Item> + 'w {
    items.take_while(move |_| work.spend(1).is_some())
}

/// Every element of a lazy offset array that resolves.
fn each<'a, T: crate::parser::FromSlice<'a>>(
    array: crate::parser::LazyOffsetArray16<'a, T>,
) -> impl Iterator<Item = T> {
    (0..array.len()).filter_map(move |i| array.get(i))
}

/// The glyphs a coverage table lists, in order.
fn coverage_glyphs<'a>(coverage: Coverage<'a>) -> Box<dyn Iterator<Item = u16> + 'a> {
    match coverage {
        Coverage::Format1 { glyphs } => Box::new(glyphs.into_iter().map(|g| g.0)),
        Coverage::Format2 { records } => {
            let mut previous_end = None;
            for record in records {
                if record.start > record.end || previous_end.is_some_and(|end| record.start <= end)
                {
                    return Box::new(core::iter::empty());
                }
                previous_end = Some(record.end);
            }
            Box::new(records.into_iter().flat_map(|r| r.start.0..=r.end.0))
        }
    }
}

fn coverage_get(coverage: &Coverage, gid: GlyphId) -> Option<u16> {
    coverage.get(crate::GlyphId(u16::try_from(gid.to_u32()).ok()?))
}

/// Maps characters to glyphs and handles extended style coverage beyond
/// glyphs that are available in the character map.
///
/// Roughly covers the functionality in <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afshaper.c>.
pub(crate) struct Shaper<'a, 'f> {
    font: &'f HintFont<'a>,
    charmap: Charmap<'a>,
    gsub: Option<LayoutTable<'a>>,
    /// What this shaper's walks of `gsub` may still spend.
    work: LayoutWork,
}

impl<'a, 'f> Shaper<'a, 'f> {
    pub fn new(font: &'f HintFont<'a>) -> Self {
        Self {
            font,
            charmap: *font.charmap(),
            gsub: font.gsub(),
            work: LayoutWork::for_table(font.gsub_len(), STYLE_WALK_TRAVERSALS),
        }
    }

    /// The same face with its `GSUB` dropped, for a face whose walk ran
    /// past its budget ([`Self::refused`]).
    pub fn without_gsub(&self) -> Self {
        Self {
            font: self.font,
            charmap: self.charmap,
            gsub: None,
            work: LayoutWork::for_table(0, 0),
        }
    }

    /// Whether a walk of this shaper's `GSUB` ran past its budget.
    pub fn refused(&self) -> bool {
        self.work.refused()
    }

    pub fn font(&self) -> &'f HintFont<'a> {
        self.font
    }

    pub fn charmap(&self) -> &Charmap<'a> {
        &self.charmap
    }

    pub fn lookup_count(&self) -> u16 {
        self.gsub
            .as_ref()
            .map(|gsub| gsub.lookups.len())
            .unwrap_or_default()
    }

    pub fn cluster_shaper(&self, style: &StyleClass) -> ClusterShaper<'a, '_, 'f> {
        // Only apply substitutions for styles with an associated feature.
        if let Some(feature_tag) = style.feature {
            if let Some((lookup_list, feature)) = self.gsub.as_ref().and_then(|gsub| {
                let selected =
                    select_script(&gsub.scripts, &script_tags_from_unicode(style.script.tag))?;
                let script = gsub.scripts.get(selected)?;
                let lang_sys = script.default_language?;
                let feature_ix =
                    feature_index_for_tag(&gsub.features, lang_sys.feature_indices, feature_tag)?;
                let feature = gsub.features.get(feature_ix)?;
                Some((gsub.lookups, feature))
            }) {
                return ClusterShaper {
                    shaper: self,
                    lookup_list: Some(lookup_list),
                    feature: Some(feature),
                };
            }
        }
        ClusterShaper {
            shaper: self,
            lookup_list: None,
            feature: None,
        }
    }

    /// Uses layout tables to compute coverage for the given style.
    ///
    /// Returns `true` if any glyph styles were updated for this style.
    ///
    /// See <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afshaper.c#L99>
    pub(crate) fn compute_coverage(
        &self,
        style: &StyleClass,
        coverage_kind: ShaperCoverageKind,
        glyph_styles: &mut [GlyphStyle],
        visited_set: &mut VisitedLookupSet<'_>,
    ) -> bool {
        let Some(gsub) = self.gsub.as_ref() else {
            return false;
        };
        let (script_list, feature_list, lookup_list) = (gsub.scripts, gsub.features, gsub.lookups);
        let mut script_tags: [Option<Tag>; 3] = [None; 3];
        for (a, b) in script_tags
            .iter_mut()
            .zip(script_tags_from_unicode(style.script.tag))
        {
            *a = Some(b);
        }
        // <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afshaper.c#L153>
        let default_script = Tag::from_bytes(b"DFLT");
        if coverage_kind == ShaperCoverageKind::Default {
            if script_tags[0].is_none() {
                script_tags[0] = Some(default_script);
            } else if script_tags[1].is_none() {
                script_tags[1] = Some(default_script);
            } else if script_tags[1] != Some(default_script) {
                script_tags[2] = Some(default_script);
            }
        } else {
            // Script classes contain some non-standard tags used for special
            // purposes. We ignore these
            // <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afshaper.c#L167>
            let non_standard_tags = [
                // Khmer symbols
                Some(Tag::from_bytes(b"Khms")),
                // Latin subscript fallbacks
                Some(Tag::from_bytes(b"Latb")),
                // Latin superscript fallbacks
                Some(Tag::from_bytes(b"Latp")),
            ];
            if non_standard_tags.contains(&script_tags[0]) {
                return false;
            }
        }
        // Check each requested script that is available in GSUB
        let mut gsub_handler = GsubHandler::new(
            &self.charmap,
            &lookup_list,
            style,
            glyph_styles,
            visited_set,
            &self.work,
        );
        let have = record_tags(&script_list);
        for script in script_tags.iter().filter_map(|tag| {
            tag.and_then(|tag| index_for_tag(&have, tag))
                .and_then(|ix| script_list.get(ix))
        }) {
            // And all language systems for each script
            let langsys = (0..script.languages.len())
                .filter_map(|i| script.languages.get(i))
                .chain(script.default_language);
            for langsys in charged(&self.work, langsys) {
                for feature_ix in charged(&self.work, langsys.feature_indices.into_iter()) {
                    // If our style has a feature tag, we only look at that
                    // specific feature; otherwise, handle all of them
                    let Some(tag) = feature_list.tag(feature_ix) else {
                        continue;
                    };
                    if !(style.feature == Some(tag) || style.feature.is_none()) {
                        continue;
                    }
                    let Some(feature) = feature_list.get(feature_ix) else {
                        continue;
                    };
                    // And now process associated lookups
                    for index in feature.lookup_indices {
                        // We only care about errors here for testing, and
                        // the budget's: past it the walk stops.
                        if gsub_handler.process_lookup(index) == Err(ProcessLookupError::OverBudget)
                        {
                            break;
                        }
                    }
                }
            }
        }
        if let Some(range) = gsub_handler.finish() {
            // If we get a range then we captured at least some glyphs so
            // let's try to assign our current style
            let mut result = false;
            for glyph_style in &mut glyph_styles[range] {
                // We only want to return true here if we actually assign the
                // style to avoid computing unnecessary metrics
                result |= glyph_style.maybe_assign_gsub_output_style(style);
            }
            result
        } else {
            false
        }
    }
}

pub(crate) struct ClusterShaper<'a, 's, 'f> {
    shaper: &'s Shaper<'a, 'f>,
    lookup_list: Option<LookupList<'a>>,
    feature: Option<Feature<'a>>,
}

impl ClusterShaper<'_, '_, '_> {
    pub(crate) fn shape(&mut self, input: &str, output: &mut ShapedCluster) {
        // First fill the output cluster with the nominal character
        // to glyph id mapping
        output.clear();
        for ch in input.chars() {
            output.push(ShapedGlyph {
                id: self.shaper.charmap.map(ch).unwrap_or_default(),
                y_offset: 0,
            });
        }
        if let Some(feature) = self.feature {
            let mut did_subst = false;
            for lookup_ix in charged(&self.shaper.work, feature.lookup_indices.into_iter()) {
                let mut glyph_ix = 0;
                while glyph_ix < output.len() {
                    did_subst |= self.apply_lookup(lookup_ix, output, glyph_ix, 0);
                    glyph_ix += 1;
                }
            }
            // Reject clusters that weren't modified by the feature.
            // FreeType detects this by shaping twice and comparing gids
            // but we just track substitutions
            // <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afshaper.c#L528>
            if !did_subst || self.shaper.refused() {
                output.clear();
            }
        }
    }

    fn apply_lookup(
        &self,
        lookup_index: u16,
        cluster: &mut ShapedCluster,
        glyph_ix: usize,
        nesting_depth: usize,
    ) -> bool {
        if nesting_depth > MAX_NESTING_DEPTH {
            return false;
        }
        let Some(glyph) = cluster.get_mut(glyph_ix) else {
            return false;
        };
        let Some(lookup) = self
            .lookup_list
            .as_ref()
            .and_then(|list| list.get(lookup_index))
        else {
            return false;
        };
        // For now, just applying single substitutions because we're
        // currently only handling shaping for "feature" styles like
        // c2sc (caps to small caps) which are (almost?) always
        // single substs
        for table in charged(&self.shaper.work, subtables(&lookup)) {
            if let SubstitutionSubtable::Single(table) = table {
                match table {
                    crate::gsub::SingleSubstitution::Format1 { coverage, delta } => {
                        if coverage_get(&coverage, glyph.id).is_none() {
                            continue;
                        }
                        glyph.id =
                            GlyphId::from((glyph.id.to_u32() as i32 + i32::from(delta)) as u16);
                        return true;
                    }
                    crate::gsub::SingleSubstitution::Format2 {
                        coverage,
                        substitutes,
                    } => {
                        let Some(cov_ix) = coverage_get(&coverage, glyph.id) else {
                            continue;
                        };
                        let Some(subst) = substitutes.get(cov_ix) else {
                            continue;
                        };
                        glyph.id = GlyphId::from(subst.0);
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Captures glyphs from the GSUB table that aren't present in cmap.
///
/// FreeType does this in a few phases:
/// 1. Collect all lookups for a given set of scripts and features.
///    <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afshaper.c#L174>
/// 2. For each lookup, collect all _output_ glyphs.
///    <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afshaper.c#L201>
/// 3. If the style represents a specific feature, make sure at least one of
///    the characters in the associated blue string would be substituted by
///    those lookups. If none would be substituted, then we don't assign the
///    style to any glyphs because we don't have any modified alignment
///    zones.
///    <https://gitlab.freedesktop.org/freetype/freetype/-/blob/57617782464411201ce7bbc93b086c1b4d7d84a5/src/autofit/afshaper.c#L264>
///
/// We roll these into one pass over the lookups below so that we don't have
/// to allocate a lookup set or iterate them twice. Note that since
/// substitutions are checked for individual characters, we ignore ligatures
/// and contextual lookups (and alternates since they aren't applicable).
struct GsubHandler<'a, 'b> {
    charmap: &'a Charmap<'a>,
    lookup_list: &'a LookupList<'a>,
    style: &'a StyleClass,
    glyph_styles: &'a mut [GlyphStyle],
    // Set to true when we need to check if any substitutions are available
    // for our blue strings. This is the case when style.feature != None
    need_blue_substs: bool,
    // Keep track of our range of touched gids in the style list
    min_gid: usize,
    max_gid: usize,
    lookup_depth: usize,
    visited_set: &'a mut VisitedLookupSet<'b>,
    work: &'a LayoutWork,
}

impl<'a, 'b> GsubHandler<'a, 'b> {
    fn new(
        charmap: &'a Charmap<'a>,
        lookup_list: &'a LookupList<'a>,
        style: &'a StyleClass,
        glyph_styles: &'a mut [GlyphStyle],
        visited_set: &'a mut VisitedLookupSet<'b>,
        work: &'a LayoutWork,
    ) -> Self {
        let min_gid = glyph_styles.len();
        // If we have a feature, then we need to check the blue string to see
        // if any substitutions are available. If not, we don't enable this
        // style because it won't have any affect on alignment zones
        let need_blue_substs = style.feature.is_some();
        Self {
            charmap,
            lookup_list,
            style,
            glyph_styles,
            need_blue_substs,
            min_gid,
            max_gid: 0,
            lookup_depth: 0,
            visited_set,
            work,
        }
    }

    fn process_lookup(&mut self, lookup_index: u16) -> Result<(), ProcessLookupError> {
        // Every reference spends, those to a lookup already visited too.
        self.work.spend(1).ok_or(ProcessLookupError::OverBudget)?;
        // General protection against stack overflows
        if self.lookup_depth == MAX_NESTING_DEPTH {
            return Err(ProcessLookupError::ExceededMaxDepth);
        }
        // Skip lookups that have already been processed
        if !self.visited_set.insert(lookup_index) {
            return Ok(());
        }
        self.lookup_depth += 1;
        // Actually process the lookup
        let result = self.process_lookup_inner(lookup_index);
        // Out we go again
        self.lookup_depth -= 1;
        result
    }

    #[inline(always)]
    fn process_lookup_inner(&mut self, lookup_index: u16) -> Result<(), ProcessLookupError> {
        let Some(lookup) = self.lookup_list.get(lookup_index) else {
            return Ok(());
        };
        let work = self.work;
        for table in charged(work, subtables(&lookup)) {
            match table {
                SubstitutionSubtable::Single(table) => match table {
                    crate::gsub::SingleSubstitution::Format1 { coverage, delta } => {
                        for gid in charged(work, coverage_glyphs(coverage)) {
                            self.capture_glyph(u32::from(
                                (i32::from(gid) + i32::from(delta)) as u16,
                            ));
                        }
                        // Check input coverage for blue strings if
                        // required and if we're not under a contextual
                        // lookup
                        if self.need_blue_substs && self.lookup_depth == 1 {
                            self.check_blue_coverage(&coverage);
                        }
                    }
                    crate::gsub::SingleSubstitution::Format2 {
                        coverage,
                        substitutes,
                    } => {
                        for gid in charged(work, substitutes.into_iter()) {
                            self.capture_glyph(u32::from(gid.0));
                        }
                        // See above
                        if self.need_blue_substs && self.lookup_depth == 1 {
                            self.check_blue_coverage(&coverage);
                        }
                    }
                },
                SubstitutionSubtable::Multiple(table) => {
                    for seq in charged(work, each(table.sequences)) {
                        for gid in charged(work, seq.substitutes.into_iter()) {
                            self.capture_glyph(u32::from(gid.0));
                        }
                    }
                    // See above
                    if self.need_blue_substs && self.lookup_depth == 1 {
                        self.check_blue_coverage(&table.coverage);
                    }
                }
                SubstitutionSubtable::Ligature(table) => {
                    for set in charged(work, each(table.ligature_sets)) {
                        for lig in charged(work, each(set)) {
                            self.capture_glyph(u32::from(lig.glyph.0));
                        }
                    }
                }
                SubstitutionSubtable::Alternate(table) => {
                    for set in charged(work, each(table.alternate_sets)) {
                        for gid in charged(work, set.alternates.into_iter()) {
                            self.capture_glyph(u32::from(gid.0));
                        }
                    }
                }
                SubstitutionSubtable::Context(table) => match table {
                    ContextLookup::Format1 { sets, .. } | ContextLookup::Format2 { sets, .. } => {
                        for set in charged(work, each(sets)) {
                            for rule in charged(work, each(set)) {
                                for rec in rule.lookups {
                                    self.process_lookup(rec.lookup_list_index)?;
                                }
                            }
                        }
                    }
                    ContextLookup::Format3 { lookups, .. } => {
                        for rec in lookups {
                            self.process_lookup(rec.lookup_list_index)?;
                        }
                    }
                },
                SubstitutionSubtable::ChainContext(table) => match table {
                    ChainedContextLookup::Format1 { sets, .. }
                    | ChainedContextLookup::Format2 { sets, .. } => {
                        for set in charged(work, each(sets)) {
                            for rule in charged(work, each(set)) {
                                for rec in rule.lookups {
                                    self.process_lookup(rec.lookup_list_index)?;
                                }
                            }
                        }
                    }
                    ChainedContextLookup::Format3 { lookups, .. } => {
                        for rec in lookups {
                            self.process_lookup(rec.lookup_list_index)?;
                        }
                    }
                },
                SubstitutionSubtable::ReverseChainSingle(table) => {
                    for gid in charged(work, table.substitutes.into_iter()) {
                        self.capture_glyph(u32::from(gid.0));
                    }
                }
            }
        }
        if work.refused() {
            return Err(ProcessLookupError::OverBudget);
        }
        Ok(())
    }

    /// Finishes processing for this set of GSUB lookups and
    /// returns the range of touched glyphs.
    fn finish(self) -> Option<Range<usize>> {
        self.visited_set.clear();
        if self.min_gid > self.max_gid || self.glyph_styles.is_empty() {
            // We didn't touch any glyphs
            return None;
        }
        let range = self.min_gid..self.max_gid + 1;
        if self.need_blue_substs {
            // We didn't find any substitutions for our blue strings so
            // we ignore the style. Clear the GSUB marker for any touched
            // glyphs
            for glyph in self.glyph_styles.get_mut(range)? {
                glyph.clear_from_gsub();
            }
            None
        } else {
            Some(range)
        }
    }

    /// Checks the given coverage table for any characters in the blue
    /// strings associated with our current style.
    fn check_blue_coverage(&mut self, coverage: &Coverage) {
        for (blue_str, _) in self.style.script.blues {
            if charged(self.work, blue_str.chars())
                .filter_map(|ch| self.charmap.map(ch))
                .filter_map(|gid| coverage_get(coverage, gid))
                .next()
                .is_some()
            {
                // Condition satisfied, so don't check any further subtables
                self.need_blue_substs = false;
                return;
            }
        }
    }

    fn capture_glyph(&mut self, gid: u32) {
        let gid = gid as usize;
        if let Some(style) = self.glyph_styles.get_mut(gid) {
            style.set_from_gsub_output();
            self.min_gid = gid.min(self.min_gid);
            self.max_gid = gid.max(self.max_gid);
        }
    }
}

pub(crate) struct VisitedLookupSet<'a>(&'a mut [u8]);

impl<'a> VisitedLookupSet<'a> {
    pub fn new(storage: &'a mut [u8]) -> Self {
        Self(storage)
    }

    /// If the given lookup index is not already in the set, adds it and
    /// returns `true`. Returns `false` otherwise.
    ///
    /// This follows the behavior of `HashSet::insert`.
    fn insert(&mut self, lookup_index: u16) -> bool {
        let byte_ix = lookup_index as usize / 8;
        let bit_mask = 1 << (lookup_index % 8) as u8;
        if let Some(byte) = self.0.get_mut(byte_ix) {
            if *byte & bit_mask == 0 {
                *byte |= bit_mask;
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    fn clear(&mut self) {
        self.0.fill(0);
    }
}

#[derive(PartialEq, Debug)]
enum ProcessLookupError {
    ExceededMaxDepth,
    /// The walk ran past the shaper's budget ([`LayoutWork`]).
    OverBudget,
}

#[cfg(test)]
#[path = "shape_bounds_tests.rs"]
mod bounds_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // read-fonts 0.43.2 src/tables/layout/script.rs
    // `simple_script_tag_from_unicode`, `exception_script_tag_from_unicode`
    // and `multi_script_tags_from_unicode`.
    #[test]
    fn simple_script_tag_from_unicode() {
        for unicode_tag in [b"Cyrl", b"Grek", b"Hebr", b"Latn"] {
            let mut bytes = *unicode_tag;
            bytes[0] = bytes[0].to_ascii_lowercase();
            let result = script_tags_from_unicode(Tag::from_bytes(unicode_tag));
            assert_eq!(result, [Tag::from_bytes(&bytes)]);
        }
    }

    #[test]
    fn exception_script_tag_from_unicode() {
        let cases = [
            (b"Kana", b"kana"),
            (b"Hira", b"kana"),
            (b"Nkoo", b"nko "),
            (b"Yiii", b"yi  "),
            (b"Vaii", b"vai "),
        ];
        for (unicode_tag, ot_tag) in cases {
            let result = script_tags_from_unicode(Tag::from_bytes(unicode_tag));
            assert_eq!(result, [Tag::from_bytes(ot_tag)]);
        }
    }

    #[test]
    fn multi_script_tags_from_unicode() {
        let cases: [(&[u8; 4], &[&[u8; 4]]); 4] = [
            (b"Beng", &[b"bng3", b"bng2", b"beng"]),
            (b"Orya", &[b"ory3", b"ory2", b"orya"]),
            (b"Mlym", &[b"mlm3", b"mlm2", b"mlym"]),
            // There's no version 3 tag for Myanmar
            (b"Mymr", &[b"mym2", b"mymr"]),
        ];
        for (unicode_tag, ot_tags) in cases {
            let result = script_tags_from_unicode(Tag::from_bytes(unicode_tag));
            let expected: Vec<Tag> = ot_tags.iter().map(|t| Tag::from_bytes(t)).collect();
            assert_eq!(result, expected);
        }
    }

    /// The visited-lookup bit set: an index past its bits reads as
    /// visited, so a lookup index past the lookup list is never walked.
    #[test]
    fn visited_set() {
        let mut storage = [0u8; 2];
        let mut set = VisitedLookupSet::new(&mut storage);
        assert!(set.insert(0));
        assert!(!set.insert(0));
        assert!(set.insert(15));
        // Past the set's bits reads as already visited.
        assert!(!set.insert(16));
        set.clear();
        assert!(set.insert(0));
    }
}
