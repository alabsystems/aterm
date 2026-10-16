// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The OpenType layout answers of the font oracle, written once for both
//! readers: `reader.rs` includes this file, with `font` naming `ttf_parser`
//! in the recorder and `aterm_font` in aterm-render's and aterm-font's
//! replays (TF.3 and TF.5 of docs/AUDIT-third-party-2026-10-15.md); the
//! layout reader is carved from ttf-parser's and keeps its names. One
//! digest, two readers.
//!
//! - [`layout`]: a `GSUB` or `GPOS` table's script, feature and lookup lists.
//! - [`gsub_lookups`]: every `GSUB` lookup's subtables, whole: coverage,
//!   class definitions, substitutes, sequences, alternates, ligatures and
//!   (chained) context rules, extension subtables resolved.
//!
//! The including module provides `font`, `Fnv`, `pin` and `tag_str`.

use super::font::GlyphId;
use super::font::gsub::{SingleSubstitution, SubstitutionSubtable};
use super::font::opentype_layout::{
    ChainedContextLookup, ClassDefinition, ContextLookup, Coverage, LayoutTable,
    SequenceLookupRecord,
};
use super::{Fnv, pin, tag_str};

pub(crate) fn layout(table: Option<LayoutTable>) -> String {
    let Some(t) = table else {
        return "absent".to_string();
    };
    let mut scripts: Vec<String> = t
        .scripts
        .into_iter()
        .map(|s| tag_str(s.tag.to_bytes()))
        .collect();
    scripts.sort_unstable();
    scripts.dedup();
    let mut tags: Vec<String> = t
        .features
        .into_iter()
        .map(|f| tag_str(f.tag.to_bytes()))
        .collect();
    let n_features = tags.len();
    tags.sort_unstable();
    tags.dedup();
    let mut h = Fnv::new();
    for f in t.features {
        h.bytes(&f.tag.to_bytes());
        for l in f.lookup_indices {
            h.u16(l);
        }
    }
    for s in t.scripts {
        h.bytes(&s.tag.to_bytes());
        for lang in s.default_language.into_iter().chain(s.languages) {
            h.bytes(&lang.tag.to_bytes());
            h.u16(lang.required_feature.unwrap_or(0xFFFF));
            for fi in lang.feature_indices {
                h.u16(fi);
            }
        }
    }
    for i in 0..t.lookups.len() {
        match t.lookups.get(i) {
            Some(l) => {
                h.u16(l.flags.0);
                h.u16(l.subtables.len());
                h.u16(l.mark_filtering_set.unwrap_or(0xFFFF));
            }
            None => h.bytes(b"-"),
        }
    }
    let answer = format!(
        "scripts={} features={n_features} tags={} lookups={} fv={} fnv={}",
        scripts.join(","),
        tags.join(","),
        t.lookups.len(),
        u8::from(t.variations.is_some()),
        h.hex()
    );
    pin(answer)
}

fn glyphs(h: &mut Fnv, glyphs: impl IntoIterator<Item = GlyphId>) {
    let mut n = 0u32;
    for g in glyphs {
        h.u16(g.0);
        n += 1;
    }
    h.u32(n);
}

fn coverage(h: &mut Fnv, c: Option<Coverage>) {
    match c {
        Some(Coverage::Format1 { glyphs: list }) => {
            h.bytes(b"C1");
            glyphs(h, list);
        }
        Some(Coverage::Format2 { records }) => {
            h.bytes(b"C2");
            for r in records {
                h.u16(r.start.0);
                h.u16(r.end.0);
                h.u16(r.value);
            }
            h.u32(u32::from(records.len()));
        }
        None => h.bytes(b"-"),
    }
}

fn classes(h: &mut Fnv, c: ClassDefinition) {
    match c {
        ClassDefinition::Format1 { start, classes } => {
            h.bytes(b"K1");
            h.u16(start.0);
            for c in classes {
                h.u16(c);
            }
            h.u32(u32::from(classes.len()));
        }
        ClassDefinition::Format2 { records } => {
            h.bytes(b"K2");
            for r in records {
                h.u16(r.start.0);
                h.u16(r.end.0);
                h.u16(r.value);
            }
            h.u32(u32::from(records.len()));
        }
        ClassDefinition::Empty => h.bytes(b"K0"),
    }
}

fn numbers(h: &mut Fnv, values: impl IntoIterator<Item = u16>) {
    let mut n = 0u32;
    for v in values {
        h.u16(v);
        n += 1;
    }
    h.u32(n);
}

fn records(h: &mut Fnv, lookups: impl IntoIterator<Item = SequenceLookupRecord>) {
    let mut n = 0u32;
    for r in lookups {
        h.u16(r.sequence_index);
        h.u16(r.lookup_list_index);
        n += 1;
    }
    h.u32(n);
}

fn context(h: &mut Fnv, c: ContextLookup) {
    match c {
        ContextLookup::Format1 {
            coverage: cov,
            sets,
        } => {
            h.bytes(b"x1");
            coverage(h, Some(cov));
            for i in 0..sets.len() {
                let Some(set) = sets.get(i) else {
                    h.bytes(b"-");
                    continue;
                };
                for j in 0..set.len() {
                    match set.get(j) {
                        Some(rule) => {
                            numbers(h, rule.input);
                            records(h, rule.lookups);
                        }
                        None => h.bytes(b"-"),
                    }
                }
            }
        }
        ContextLookup::Format2 {
            coverage: cov,
            classes: k,
            sets,
        } => {
            h.bytes(b"x2");
            coverage(h, Some(cov));
            classes(h, k);
            for i in 0..sets.len() {
                let Some(set) = sets.get(i) else {
                    h.bytes(b"-");
                    continue;
                };
                for j in 0..set.len() {
                    match set.get(j) {
                        Some(rule) => {
                            numbers(h, rule.input);
                            records(h, rule.lookups);
                        }
                        None => h.bytes(b"-"),
                    }
                }
            }
        }
        ContextLookup::Format3 {
            coverage: cov,
            coverages,
            lookups,
        } => {
            h.bytes(b"x3");
            coverage(h, Some(cov));
            for i in 0..coverages.len() {
                coverage(h, coverages.get(i));
            }
            records(h, lookups);
        }
    }
}

fn chained(h: &mut Fnv, c: ChainedContextLookup) {
    match c {
        ChainedContextLookup::Format1 {
            coverage: cov,
            sets,
        } => {
            h.bytes(b"y1");
            coverage(h, Some(cov));
            for i in 0..sets.len() {
                let Some(set) = sets.get(i) else {
                    h.bytes(b"-");
                    continue;
                };
                for j in 0..set.len() {
                    match set.get(j) {
                        Some(rule) => {
                            numbers(h, rule.backtrack);
                            numbers(h, rule.input);
                            numbers(h, rule.lookahead);
                            records(h, rule.lookups);
                        }
                        None => h.bytes(b"-"),
                    }
                }
            }
        }
        ChainedContextLookup::Format2 {
            coverage: cov,
            backtrack_classes,
            input_classes,
            lookahead_classes,
            sets,
        } => {
            h.bytes(b"y2");
            coverage(h, Some(cov));
            classes(h, backtrack_classes);
            classes(h, input_classes);
            classes(h, lookahead_classes);
            for i in 0..sets.len() {
                let Some(set) = sets.get(i) else {
                    h.bytes(b"-");
                    continue;
                };
                for j in 0..set.len() {
                    match set.get(j) {
                        Some(rule) => {
                            numbers(h, rule.backtrack);
                            numbers(h, rule.input);
                            numbers(h, rule.lookahead);
                            records(h, rule.lookups);
                        }
                        None => h.bytes(b"-"),
                    }
                }
            }
        }
        ChainedContextLookup::Format3 {
            coverage: cov,
            backtrack_coverages,
            input_coverages,
            lookahead_coverages,
            lookups,
        } => {
            h.bytes(b"y3");
            coverage(h, Some(cov));
            for list in [backtrack_coverages, input_coverages, lookahead_coverages] {
                for i in 0..list.len() {
                    coverage(h, list.get(i));
                }
                h.u16(list.len());
            }
            records(h, lookups);
        }
    }
}

fn subtable(h: &mut Fnv, s: SubstitutionSubtable) {
    match s {
        SubstitutionSubtable::Single(SingleSubstitution::Format1 {
            coverage: cov,
            delta,
        }) => {
            h.bytes(b"s1");
            coverage(h, Some(cov));
            h.i16(delta);
        }
        SubstitutionSubtable::Single(SingleSubstitution::Format2 {
            coverage: cov,
            substitutes,
        }) => {
            h.bytes(b"s2");
            coverage(h, Some(cov));
            glyphs(h, substitutes);
        }
        SubstitutionSubtable::Multiple(m) => {
            h.bytes(b"m");
            coverage(h, Some(m.coverage));
            for i in 0..m.sequences.len() {
                match m.sequences.get(i) {
                    Some(seq) => glyphs(h, seq.substitutes),
                    None => h.bytes(b"-"),
                }
            }
        }
        SubstitutionSubtable::Alternate(a) => {
            h.bytes(b"a");
            coverage(h, Some(a.coverage));
            for i in 0..a.alternate_sets.len() {
                match a.alternate_sets.get(i) {
                    Some(set) => glyphs(h, set.alternates),
                    None => h.bytes(b"-"),
                }
            }
        }
        SubstitutionSubtable::Ligature(l) => {
            h.bytes(b"l");
            coverage(h, Some(l.coverage));
            for i in 0..l.ligature_sets.len() {
                let Some(set) = l.ligature_sets.get(i) else {
                    h.bytes(b"-");
                    continue;
                };
                for j in 0..set.len() {
                    match set.get(j) {
                        Some(lig) => {
                            h.u16(lig.glyph.0);
                            glyphs(h, lig.components);
                        }
                        None => h.bytes(b"-"),
                    }
                }
            }
        }
        SubstitutionSubtable::Context(c) => context(h, c),
        SubstitutionSubtable::ChainContext(c) => chained(h, c),
        SubstitutionSubtable::ReverseChainSingle(r) => {
            h.bytes(b"r");
            coverage(h, Some(r.coverage));
            for list in [r.backtrack_coverages, r.lookahead_coverages] {
                for i in 0..list.len() {
                    coverage(h, list.get(i));
                }
                h.u16(list.len());
            }
            glyphs(h, r.substitutes);
        }
    }
}

/// Every `GSUB` lookup's subtables, digested whole, with the count of each
/// lookup type the subtables resolved to (`kinds=<type>:<count>`, an
/// extension counted as what it wraps).
pub(crate) fn gsub_lookups(table: Option<LayoutTable>) -> String {
    let Some(t) = table else {
        return "absent".to_string();
    };
    let mut h = Fnv::new();
    let mut kinds = [0u32; 9];
    let mut unparsed = 0u32;
    for i in 0..t.lookups.len() {
        h.u16(i);
        let Some(lookup) = t.lookups.get(i) else {
            h.bytes(b"-");
            continue;
        };
        for j in 0..lookup.subtables.len() {
            match lookup.subtables.get::<SubstitutionSubtable>(j) {
                Some(s) => {
                    let kind = match s {
                        SubstitutionSubtable::Single(_) => 1,
                        SubstitutionSubtable::Multiple(_) => 2,
                        SubstitutionSubtable::Alternate(_) => 3,
                        SubstitutionSubtable::Ligature(_) => 4,
                        SubstitutionSubtable::Context(_) => 5,
                        SubstitutionSubtable::ChainContext(_) => 6,
                        SubstitutionSubtable::ReverseChainSingle(_) => 8,
                    };
                    kinds[kind] += 1;
                    subtable(&mut h, s);
                }
                None => {
                    unparsed += 1;
                    h.bytes(b"!");
                }
            }
        }
    }
    let kinds: Vec<String> = kinds
        .iter()
        .enumerate()
        .filter(|&(_, &n)| n > 0)
        .map(|(k, n)| format!("{k}:{n}"))
        .collect();
    pin(format!(
        "lookups={} kinds={} unparsed={unparsed} fnv={}",
        t.lookups.len(),
        if kinds.is_empty() {
            "-".to_string()
        } else {
            kinds.join(",")
        },
        h.hex()
    ))
}
