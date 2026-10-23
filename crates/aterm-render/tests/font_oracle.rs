// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The font oracle (TF.1 of docs/AUDIT-third-party-2026-10-15.md): answers recorded from the third-party font crates, which the first-party code replacing them is held to.

mod corpus;
/// aterm-font's shaper, which answers the `shape` lines here. `probe.rs`, the
mod shaper;
/// The reader `reader.rs` answers the `read` lines with: aterm-font, under
mod font {
    pub(crate) use aterm_font::OutlineSink as OutlineBuilder;
    pub(crate) use aterm_font::*;
}
#[path = "font_oracle/reader.rs"]
mod reader;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use aterm_render::font::{Font, FontSettings};
use aterm_render::{ligature_shaping, variation};
use aterm_types::text_shaping::FontFeature;
use corpus::{Coords, FACES, Features, Fnv, Line, Origin};

/// One face file this host has.
struct Loaded {
    face: &'static corpus::FaceFile,
    bytes: Arc<Vec<u8>>,
    key: String,
}

/// The face file keys this host has: `true` for a committed face, `false`
type Present = BTreeMap<String, bool>;

/// Run `f` over every face file this host has, one file in memory at a time
fn for_each_face(mut f: impl FnMut(&Loaded)) -> Present {
    let mut present = Present::new();
    for face in FACES {
        let path = match face.origin {
            Origin::Repo(rel) => format!("{}/{rel}", env!("CARGO_MANIFEST_DIR")),
            Origin::System(abs) => abs.to_string(),
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if matches!(face.origin, Origin::Repo(_)) => {
                panic!("committed face {path} is unreadable: {e}")
            }
            Err(_) => continue,
        };
        let loaded = Loaded {
            face,
            key: corpus::file_key(&bytes),
            bytes: Arc::new(bytes),
        };
        f(&loaded);
        present.insert(loaded.key, matches!(face.origin, Origin::Repo(_)));
    }
    present
}

fn tag(t: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*t)
}

/// The variation instances a face's reader answers are recorded at: the
fn reader_instances(bytes: &[u8], index: u32) -> Vec<Coords> {
    let mut out: Vec<Coords> = vec![Vec::new()];
    let Some(p) = variation::probe(bytes, index) else {
        return out;
    };
    let to_coords = |values: &[f32]| -> Coords {
        p.axes
            .iter()
            .zip(values)
            .map(|(a, &v)| (a.tag.to_be_bytes(), v))
            .collect()
    };
    let aterm = variation::resolve_default_coords(&p.axes, p.regular_coords.as_deref());
    out.push(to_coords(&aterm));
    let bold = variation::apply_requests(&p.axes, aterm.clone(), &[(tag(b"wght"), 700.0)]);
    out.push(to_coords(&bold));
    if p.axes.len() <= 4 {
        for (i, axis) in p.axes.iter().enumerate() {
            for v in [axis.min, axis.max] {
                let mut c = aterm.clone();
                c[i] = v;
                out.push(to_coords(&c));
            }
        }
    } else {
        let min: Vec<f32> = p.axes.iter().map(|a| a.min).collect();
        let max: Vec<f32> = p.axes.iter().map(|a| a.max).collect();
        out.push(to_coords(&min));
        out.push(to_coords(&max));
    }
    let mut seen = BTreeSet::new();
    out.retain(|c| seen.insert(corpus::fmt_coords(c)));
    out
}

/// The feature list the renderer shapes with for a `font_features` config
fn feature_list(user: &[FontFeature], ligatures_on: bool) -> Features {
    ligature_shaping::build_feature_list(user, ligatures_on)
        .iter()
        .map(|f| (f.tag, f.value))
        .collect()
}

/// A typical multi-feature `font_features` config: a slashed zero, a
fn typical_config() -> [FontFeature; 3] {
    [
        FontFeature::new(*b"zero", 1),
        FontFeature::new(*b"ss01", 1),
        FontFeature::new(*b"cv01", 2),
    ]
}

/// A user `-calt`, which overrides the base pair's `calt` (last writer wins).
fn no_calt_config() -> [FontFeature; 1] {
    [FontFeature::new(*b"calt", 0)]
}

/// Whether `tag` is one a user sets in `font_features` to pick a glyph
fn user_configurable(tag: [u8; 4]) -> bool {
    let numbered = |prefix: &[u8]| {
        tag.starts_with(prefix) && tag[2].is_ascii_digit() && tag[3].is_ascii_digit()
    };
    numbered(b"ss")
        || numbered(b"cv")
        || [
            b"aalt", b"afrc", b"c2pc", b"c2sc", b"case", b"cswh", b"dlig", b"dnom", b"frac",
            b"fwid", b"hist", b"hlig", b"hwid", b"lnum", b"nalt", b"numr", b"onum", b"ordn",
            b"ornm", b"pcap", b"pnum", b"pwid", b"salt", b"sinf", b"smcp", b"smpl", b"subs",
            b"sups", b"swsh", b"titl", b"tnum", b"trad", b"unic", b"zero",
        ]
        .contains(&&tag)
}

/// What the run shaper's gate reads from a primary face, which also gates
struct Gate {
    ligatures: bool,
    /// The tags `font_advertises_feature` answers yes for, among the
    advertised: BTreeSet<[u8; 4]>,
    /// The user-configurable tags face 0's `GSUB` lists, in tag order.
    user_tags: Vec<[u8; 4]>,
}

impl Gate {
    fn of(bytes: &[u8]) -> Self {
        let mut user_tags: Vec<[u8; 4]> = aterm_font::Face::parse(bytes, 0)
            .ok()
            .and_then(|face| face.tables().gsub)
            .map(|gsub| {
                gsub.features
                    .into_iter()
                    .map(|f| f.tag.to_bytes())
                    .collect()
            })
            .unwrap_or_default();
        user_tags.retain(|&t| user_configurable(t));
        user_tags.sort_unstable();
        user_tags.dedup();
        let advertised = typical_config()
            .iter()
            .chain(&no_calt_config())
            .map(|f| f.tag)
            .chain(user_tags.iter().copied())
            .filter(|&t| ligature_shaping::font_advertises_feature(bytes, 0, t))
            .collect();
        Self {
            ligatures: ligature_shaping::font_has_ligature_features(bytes, 0),
            advertised,
            user_tags,
        }
    }

    /// `should_run_shaping` for a renderer configured with `user` features.
    fn reaches(&self, user: &[FontFeature], ligatures_on: bool) -> bool {
        let has_user_features = user.iter().any(|f| self.advertised.contains(&f.tag));
        ligature_shaping::should_run_shaping(true, has_user_features, !ligatures_on, self.ligatures)
    }
}

struct Case {
    id: String,
    text: String,
}

fn case(id: &str, text: &str) -> Case {
    Case {
        id: id.to_string(),
        text: text.to_string(),
    }
}

/// Programming ligatures and the strings around them: the feature-sensitive
fn ligature_cases() -> Vec<Case> {
    [
        ("lig.arrow", "=>"),
        ("lig.thin-arrow", "->"),
        ("lig.left-arrow", "<-"),
        ("lig.neq", "!="),
        ("lig.eq2", "=="),
        ("lig.eq3", "==="),
        ("lig.neq2", "!=="),
        ("lig.le", "<="),
        ("lig.ge", ">="),
        ("lig.spaceship", "<=>"),
        ("lig.path", "::"),
        ("lig.path3", ":::"),
        ("lig.shl", "<<"),
        ("lig.shr", ">>"),
        ("lig.and", "&&"),
        ("lig.or", "||"),
        ("lig.pipe", "|>"),
        ("lig.alt", "<|>"),
        ("lig.comment-open", "/*"),
        ("lig.comment-close", "*/"),
        ("lig.line-comment", "//"),
        ("lig.html-comment", "<!--"),
        ("lig.html-comment-close", "-->"),
        ("lig.ellipsis", "..."),
        ("lig.optional", "?."),
        ("lig.coalesce", "??"),
        ("lig.www", "www"),
        ("lig.hex", "0xFF"),
        ("lig.zero", "0"),
        ("lig.zero-oh", "0O0"),
        ("lig.in-word", "x=>y"),
        ("lig.chain", "a!=b&&c"),
        ("lig.eq-run", "====="),
        ("lig.dash-run", "--------"),
        ("lig.hash-run", "####"),
        ("lig.office", "office"),
        ("lig.hello", "Hello"),
        ("lig.aterm", "aterm"),
    ]
    .into_iter()
    .map(|(id, text)| case(id, text))
    .collect()
}

/// The subset the `font_features` configs are shaped over.
const FEATURE_CASES: &[&str] = &[
    "lig.arrow",
    "lig.thin-arrow",
    "lig.neq",
    "lig.eq3",
    "lig.spaceship",
    "lig.path",
    "lig.and",
    "lig.html-comment",
    "lig.www",
    "lig.hex",
    "lig.zero",
    "lig.zero-oh",
    "lig.eq-run",
    "lig.office",
    "lig.hello",
];

/// Automatic fractions, the normalization fallbacks rustybuzz applies before
fn misc_cases() -> Vec<Case> {
    [
        ("frac.half", "1\u{2044}2"),
        ("frac.long", "12\u{2044}345"),
        ("frac.letters", "a\u{2044}b"),
        ("frac.ascii-slash", "1/2"),
        ("norm.ohm", "\u{2126}"),
        ("norm.angstrom", "\u{212B}"),
        ("norm.a-ring", "A\u{30A}"),
        ("norm.e-acute", "e\u{301}"),
        ("norm.nb-hyphen", "x\u{2011}y"),
        ("norm.em-space", "a\u{2003}b"),
        ("norm.nbsp", "a\u{A0}b"),
        ("ign.zwsp", "a\u{200B}b"),
        ("ign.zwj", "a\u{200D}b"),
        ("ign.soft-hyphen", "a\u{AD}b"),
        ("ign.word-joiner", "a\u{2060}b"),
        ("ign.vs16", "a\u{FE0F}"),
        ("mirror.parens", "(a)"),
        ("mirror.brackets", "[x]"),
        ("mirror.angles", "<>"),
        ("sym.arrows", "\u{2190}\u{2192}"),
        ("sym.relations", "\u{2260}\u{2264}\u{2265}"),
        ("sym.box", "\u{2500}\u{253C}\u{2500}"),
    ]
    .into_iter()
    .map(|(id, text)| case(id, text))
    .collect()
}

/// Right-to-left strings, each recorded in logical order and as the runs a
fn rtl_cases() -> Vec<Case> {
    let mut out = Vec::new();
    for (id, logical) in [
        ("ar.marhaba", "\u{645}\u{631}\u{62D}\u{628}\u{627}"),
        (
            "ar.salam",
            "\u{627}\u{644}\u{633}\u{644}\u{627}\u{645} \u{639}\u{644}\u{64A}\u{643}\u{645}",
        ),
        ("ar.lam-alef", "\u{644}\u{627}"),
        ("ar.parens", "(\u{645}\u{631}\u{62D}\u{628}\u{627})"),
        ("ar.digit", "\u{628}2"),
        ("ar.indic-digits", "\u{661}\u{662}\u{663}"),
        ("ar.mixed", "\u{639}\u{62F}\u{62F} 123"),
        ("he.shalom", "\u{5E9}\u{5DC}\u{5D5}\u{5DD}"),
        ("he.ivrit", "\u{5E2}\u{5D1}\u{5E8}\u{5D9}\u{5EA}"),
        ("he.parens", "(\u{5E9}\u{5DC}\u{5D5}\u{5DD})"),
        (
            "he.niqqud",
            "\u{5E9}\u{5B8}\u{5C1}\u{5DC}\u{5D5}\u{5B9}\u{5DD}",
        ),
        ("he.digit", "\u{5D0}1"),
    ] {
        out.push(case(&format!("{id}.logical"), logical));
        for (k, run) in visual_runs(logical).into_iter().enumerate() {
            out.push(case(&format!("{id}.visual.{k}"), &run));
        }
    }
    out
}

fn visual_runs(logical: &str) -> Vec<String> {
    let chars: Vec<char> = logical.chars().collect();
    let classes: Vec<aterm_bidi::BidiClass> =
        chars.iter().map(|&c| aterm_bidi::bidi_class(c)).collect();
    let brackets: Vec<aterm_bidi::Bracket> =
        chars.iter().map(|&c| aterm_bidi::bracket_of(c)).collect();
    let wide = vec![false; chars.len()];
    let order = aterm_bidi::reorder_cells_with_classes(
        &classes,
        &brackets,
        &wide,
        aterm_bidi::BaseDirection::Ltr,
    );
    let visual: String = order.iter().map(|&l| chars[l]).collect();
    visual
        .split(' ')
        .filter(|run| !run.is_empty())
        .map(str::to_string)
        .collect()
}

/// Devanagari and CJK. Apart from `deva.bare` and `cjk.halfwidth-kana`, these
fn complex_cases() -> Vec<Case> {
    [
        ("deva.namaste", "\u{928}\u{92E}\u{938}\u{94D}\u{924}\u{947}"),
        (
            "deva.kshatriya",
            "\u{915}\u{94D}\u{937}\u{924}\u{94D}\u{930}\u{93F}\u{92F}",
        ),
        ("deva.hindi", "\u{939}\u{93F}\u{928}\u{94D}\u{926}\u{940}"),
        ("deva.bare", "\u{915}\u{916}\u{917}"),
        ("cjk.nihongo", "\u{65E5}\u{672C}\u{8A9E}"),
        ("cjk.zhongwen", "\u{4E2D}\u{6587}"),
        ("cjk.hangul", "\u{D55C}\u{AD6D}\u{C5B4}"),
        ("cjk.halfwidth-kana", "\u{FF71}\u{FF72}\u{FF73}"),
        ("cjk.kana", "\u{304B}\u{306A}"),
    ]
    .into_iter()
    .map(|(id, text)| case(id, text))
    .collect()
}

/// The clusters `Renderer::shape_cluster` shapes on the colour face
fn emoji_cases() -> Vec<Case> {
    corpus::EMOJI_CASES
        .iter()
        .map(|&(id, text)| case(id, text))
        .collect()
}

/// Strings a user feature changes on its own, with no ligature in sight:
fn feature_probe_cases() -> Vec<Case> {
    [
        ("feat.alternates", "a0l1Il|g@&$"),
        ("feat.caps", "HelloWorld"),
        ("feat.digits", "0123456789"),
        ("feat.case", "(A)-[B]{C}"),
        ("feat.cell", "g"),
    ]
    .into_iter()
    .map(|(id, text)| case(id, text))
    .collect()
}

/// How much of the case set an instance gets.
#[derive(Clone, Copy)]
enum Tier {
    /// The instance aterm draws a primary at by default: every case.
    Full,
    /// A styled sibling, and a variable primary's other instances.
    Reduced,
}

/// The instances the run shaper shapes a face at, each with its tier,
fn shaping_instances(bytes: &[u8], index: u32, roles: u8) -> Vec<(Tier, Coords)> {
    if roles & corpus::PRIMARY == 0 {
        return vec![(Tier::Reduced, Vec::new())];
    }
    let Some(p) = variation::probe(bytes, index) else {
        return vec![(Tier::Full, Vec::new())];
    };
    let resolve = |requests: &[(u32, f32)]| -> Coords {
        let base = variation::resolve_default_coords(&p.axes, p.regular_coords.as_deref());
        let coords = variation::apply_requests(&p.axes, base, requests);
        let all_default = p
            .axes
            .iter()
            .zip(&coords)
            .all(|(a, &v)| (v - a.def).abs() < 1e-4);
        if all_default {
            return Vec::new();
        }
        p.axes
            .iter()
            .zip(&coords)
            .map(|(a, &v)| (a.tag.to_be_bytes(), v))
            .collect()
    };
    let mut out = vec![(Tier::Full, resolve(&[])), (Tier::Reduced, Vec::new())];
    for weight in [700.0, 900.0] {
        out.push((Tier::Reduced, resolve(&[(variation::WGHT_TAG, weight)])));
    }
    let mut seen = BTreeSet::new();
    out.retain(|(_, c)| seen.insert(corpus::fmt_coords(c)));
    out
}

/// Every shaping case a primary or styled face gets at one tier, each with its
fn shaping_plan(gate: &Gate, tier: Tier) -> Vec<(Features, Case)> {
    let mut out = Vec::new();
    let mut add = |user: &[FontFeature], ligatures_on: bool, cases: Vec<Case>| {
        if gate.reaches(user, ligatures_on) {
            let features = feature_list(user, ligatures_on);
            out.extend(cases.into_iter().map(|c| (features.clone(), c)));
        }
    };
    let feature_cases = || -> Vec<Case> {
        ligature_cases()
            .into_iter()
            .filter(|c| FEATURE_CASES.contains(&c.id.as_str()))
            .collect()
    };
    match tier {
        Tier::Full => {
            let all = ligature_cases()
                .into_iter()
                .chain(misc_cases())
                .chain(rtl_cases())
                .chain(complex_cases())
                .collect();
            add(&[], true, all);
            for ligatures_on in [true, false] {
                add(&typical_config(), ligatures_on, feature_cases());
            }
            add(&no_calt_config(), true, feature_cases());
        }
        Tier::Reduced => {
            let styled = ligature_cases()
                .into_iter()
                .chain(
                    rtl_cases()
                        .into_iter()
                        .filter(|c| c.id.contains(".visual.")),
                )
                .chain(misc_cases().into_iter().filter(|c| c.id == "frac.half"))
                .collect();
            add(&[], true, styled);
        }
    }
    if !gate.ligatures {
        for &tag in &gate.user_tags {
            let user = [FontFeature::new(tag, 1)];
            for ligatures_on in [true, false] {
                let cases = match tier {
                    Tier::Full => feature_cases()
                        .into_iter()
                        .chain(feature_probe_cases())
                        .collect(),
                    Tier::Reduced => feature_probe_cases(),
                };
                add(&user, ligatures_on, cases);
            }
        }
    }
    out
}

/// Shape every case `l`'s roles ask for. `gates` holds each primary's [`Gate`]
fn shaping_lines(l: &Loaded, gates: &mut BTreeMap<&'static str, Gate>, out: &mut Vec<Line>) {
    if l.face.roles & corpus::PRIMARY != 0 {
        gates.insert(l.face.label, Gate::of(&l.bytes));
    }
    let gate = match l.face.primary {
        Some(primary) => gates.get(primary),
        None => gates.get(l.face.label),
    };
    for &index in l.face.indices {
        let mut plan: Vec<(Coords, Features, Case)> = Vec::new();
        if l.face.roles & corpus::EMOJI != 0 {
            plan.extend(
                emoji_cases()
                    .into_iter()
                    .map(|c| (Vec::new(), Vec::new(), c)),
            );
        }
        if let Some(gate) = gate {
            for (tier, coords) in shaping_instances(&l.bytes, index, l.face.roles) {
                for (features, c) in shaping_plan(gate, tier) {
                    plan.push((coords.clone(), features, c));
                }
            }
        }
        for (coords, features, c) in plan {
            let answer = shaper::shape(&l.bytes, index, &coords, &features, &c.text);
            assert_eq!(
                corpus::parse_coords(&corpus::fmt_coords(&coords)),
                Some(coords.clone())
            );
            assert_eq!(
                corpus::parse_features(&corpus::fmt_features(&features)),
                Some(features.clone())
            );
            assert_eq!(
                corpus::unescape(&corpus::escape(&c.text)).as_deref(),
                Some(c.text.as_str())
            );
            out.push(Line {
                kind: "shape".to_string(),
                label: l.face.label.to_string(),
                key: l.key.clone(),
                index,
                inst: corpus::fmt_coords(&coords),
                feats: corpus::fmt_features(&features),
                case: c.id,
                input: corpus::escape(&c.text),
                answer,
            });
        }
    }
}

fn reader_lines(l: &Loaded, out: &mut Vec<Line>) {
    if l.face.roles & corpus::READ == 0 {
        return;
    }
    let mut push = |kind: &str, index: u32, coords: &Coords, case: &str, answer: String| {
        out.push(Line {
            kind: kind.to_string(),
            label: l.face.label.to_string(),
            key: l.key.clone(),
            index,
            inst: corpus::fmt_coords(coords),
            feats: "-".to_string(),
            case: case.to_string(),
            input: "-".to_string(),
            answer,
        });
    };
    let bless = blessing();
    for &index in l.face.indices {
        for coords in reader_instances(&l.bytes, index) {
            if bless {
                for (query, answer) in reader::read(&l.bytes, index, &coords) {
                    push("read", index, &coords, &query, answer);
                }
            }
            for (query, answer) in vary_answers(&l.bytes, index, &coords) {
                push("vary", index, &coords, &query, answer);
            }
        }
        for (query, answer) in font_answers(&l.bytes, index) {
            push("font", index, &Vec::new(), &query, answer);
        }
    }
}

/// `variation::varied_glyph_raster`'s answer: `(width, height, xmin, ymin,
type GlyphRaster = Option<(usize, usize, i32, i32, f32, Vec<u8>)>;

fn glyph_raster(h: &mut Fnv, gid: u16, r: &GlyphRaster) {
    h.u16(gid);
    match r {
        Some((w, hgt, xmin, ymin, adv, cov)) => {
            h.u32(*w as u32);
            h.u32(*hgt as u32);
            h.i32(*xmin);
            h.i32(*ymin);
            h.f32(*adv);
            h.bytes(cov);
        }
        None => h.bytes(b"-"),
    }
}

/// aterm's own `font::Font` over this face: the cmap it builds, line metrics,
fn font_answers(bytes: &Arc<Vec<u8>>, index: u32) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if index == 0 {
        out.push((
            "ligature_gate".to_string(),
            u8::from(aterm_render::ligature_shaping::font_has_ligature_features(
                bytes, 0,
            ))
            .to_string(),
        ));
    }
    let font = match Font::from_shared_vec(
        bytes.clone(),
        FontSettings {
            collection_index: index,
        },
    ) {
        Ok(font) => font,
        Err(e) => {
            out.push(("parse".to_string(), format!("error {e}")));
            return out;
        }
    };
    let Ok(face) = aterm_font::Face::parse(bytes, index) else {
        return out;
    };
    let mut chars: Vec<char> = reader::unicode_coverage(&face)
        .into_iter()
        .filter_map(char::from_u32)
        .chain(corpus::probe_chars())
        .collect();
    chars.sort_unstable();
    chars.dedup();
    let mut h = Fnv::new();
    let mut mapped = 0u32;
    for &ch in &chars {
        let g = font.lookup_glyph_index(ch);
        mapped += u32::from(g != 0);
        h.u32(u32::from(ch));
        h.u16(g);
    }
    out.push((
        "cmap".to_string(),
        format!("probed={} mapped={mapped} fnv={}", chars.len(), h.hex()),
    ));
    out.push((
        "lines13".to_string(),
        font.horizontal_line_metrics(13.0)
            .map_or("-".to_string(), |m| {
                format!(
                    "asc={:?} desc={:?} gap={:?} new_line={:?}",
                    m.ascent, m.descent, m.line_gap, m.new_line_size
                )
            }),
    ));
    let mut h = Fnv::new();
    for g in 0..face.number_of_glyphs() {
        let m = font.metrics_indexed(g, 13.0);
        h.i32(m.xmin);
        h.i32(m.ymin);
        h.u32(m.width as u32);
        h.u32(m.height as u32);
        h.f32(m.advance_width);
    }
    out.push((
        "metrics13".to_string(),
        format!("n={} fnv={}", face.number_of_glyphs(), h.hex()),
    ));
    let gids = reader::latin_gids(&face);
    let mut h = Fnv::new();
    let mut pairs = 0u32;
    for &l in &gids {
        for &r in &gids {
            if let Some(k) = font.horizontal_kern_indexed(l, r, 13.0) {
                pairs += 1;
                h.u16(l);
                h.u16(r);
                h.f32(k);
            }
        }
    }
    out.push((
        "kern13".to_string(),
        format!("glyphs={} pairs={pairs} fnv={}", gids.len(), h.hex()),
    ));
    let mut raster_gids: Vec<u16> = gids
        .iter()
        .copied()
        .chain(
            corpus::probe_chars()
                .into_iter()
                .map(|c| font.lookup_glyph_index(c)),
        )
        .filter(|&g| g != 0)
        .collect();
    raster_gids.sort_unstable();
    raster_gids.dedup();
    for px in [13.0f32, 26.0] {
        let mut h = Fnv::new();
        for &g in &raster_gids {
            let (m, cov) = font.rasterize_indexed(g, px);
            h.u16(g);
            h.i32(m.xmin);
            h.i32(m.ymin);
            h.u32(m.width as u32);
            h.u32(m.height as u32);
            h.f32(m.advance_width);
            h.bytes(&cov);
        }
        out.push((
            format!("raster{px}"),
            format!("glyphs={} fnv={}", raster_gids.len(), h.hex()),
        ));
    }
    out
}

/// aterm's `variation` module over a variable face: the axes and `Regular`
fn vary_answers(bytes: &[u8], index: u32, coords: &[([u8; 4], f32)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(p) = variation::probe(bytes, index) else {
        return out;
    };
    let c: Vec<(u32, f32)> = coords.iter().map(|(t, v)| (tag(t), *v)).collect();
    if coords.is_empty() {
        let axes: Vec<String> = p
            .axes
            .iter()
            .map(|a| {
                format!(
                    "{}:{:?},{:?},{:?}",
                    String::from_utf8_lossy(&a.tag.to_be_bytes()),
                    a.min,
                    a.def,
                    a.max
                )
            })
            .collect();
        out.push((
            "probe".to_string(),
            format!("axes={} regular={:?}", axes.join(" "), p.regular_coords),
        ));
    }
    out.push((
        "metrics13".to_string(),
        variation::varied_metrics_px(bytes, index, &c, 13.0).map_or("-".to_string(), |m| {
            format!(
                "asc={:?} desc={:?} gap={:?} m_adv={:?}",
                m.ascent, m.descent, m.line_gap, m.m_advance
            )
        }),
    ));
    let Ok(face) = aterm_font::Face::parse(bytes, index) else {
        return out;
    };
    let gids = reader::latin_gids(&face);
    let Some(varied) = variation::VariedFace::parse(bytes, index, &c) else {
        out.push(("raster13".to_string(), "unparsed".to_string()));
        return out;
    };
    let mut h = Fnv::new();
    for &g in &gids {
        glyph_raster(&mut h, g, &varied.glyph_raster(g, 13.0));
    }
    out.push((
        "raster13".to_string(),
        format!("glyphs={} fnv={}", gids.len(), h.hex()),
    ));
    out
}

/// What a corpus check found: answers that moved, lines it could not check.
#[derive(Default, Debug)]
struct Report {
    moved: Vec<String>,
    compared: usize,
    skipped: BTreeSet<String>,
    skipped_lines: usize,
    unrecorded: BTreeSet<String>,
}

/// Hold `got` to the committed `want` text, leaving out the recorded lines of
fn compare(want: &str, got: &[Line], present: &Present, elsewhere: &[&str]) -> Report {
    let mut report = Report::default();
    let mut recorded: BTreeMap<String, Line> = BTreeMap::new();
    let mut keys_recorded: BTreeSet<String> = BTreeSet::new();
    for text in want
        .lines()
        .filter(|t| !t.is_empty() && !t.starts_with('#'))
    {
        let line = Line::parse(text).unwrap_or_else(|| panic!("malformed corpus line: {text:?}"));
        if elsewhere.contains(&line.kind.as_str()) {
            continue;
        }
        if !present.contains_key(&line.key) {
            report
                .skipped
                .insert(format!("{} {}", line.label, line.key));
            report.skipped_lines += 1;
            continue;
        }
        keys_recorded.insert(line.key.clone());
        recorded.insert(line.id(), line);
    }
    let mut seen = BTreeSet::new();
    for line in got {
        let id = line.id();
        seen.insert(id.clone());
        match recorded.get(&id) {
            Some(r) if r.answer == line.answer => report.compared += 1,
            Some(r) => report.moved.push(format!(
                "  {id}\n    recorded {}\n    now      {}",
                r.answer, line.answer
            )),
            None if !keys_recorded.contains(&line.key)
                && present.get(&line.key) == Some(&false) =>
            {
                report
                    .unrecorded
                    .insert(format!("{} {}", line.label, line.key));
            }
            None => report
                .moved
                .push(format!("  {id}\n    new case {}", line.answer)),
        }
    }
    for (id, r) in &recorded {
        if !seen.contains(id) {
            report
                .moved
                .push(format!("  {id}\n    no longer recorded (was {})", r.answer));
        }
    }
    report
}

const HEADER: &str = "\
# The font oracle (TF.1, docs/AUDIT-third-party-2026-10-15.md), written by
# crates/aterm-render/tests/font_oracle.rs. One case per line, TAB-separated:
#   kind  label  file-key#index  instance  features  case  input  answer
# kind: shape (rustybuzz 0.20.1), read (recorded from ttf-parser 0.25.1;
# since TF.5 answered here by aterm-font, and re-answered with ttf-parser by
# the recorder's `replay`), font (aterm's font::Font), vary (aterm's
# variation module). file-key: FNV-1a 64 of the font file; a system face is
# never committed, so its lines are skipped on a host without that exact
# file. instance: `-` sets no variation, else the user-space coordinates
# applied. input: the shaped text, non-ASCII as \\u{..}.
# Digests are FNV-1a 64; an answer over 480 bytes is its digest and length.
";

/// Whether this run re-records the corpus (`ATERM_BLESS_GOLDEN`).
fn blessing() -> bool {
    std::env::var_os("ATERM_BLESS_GOLDEN").is_some()
}

/// Bless `got` into `{name}.txt`, or hold it to that file. The recorded lines
fn finish(name: &str, mut got: Vec<Line>, present: &Present, elsewhere: &[&str]) {
    let path = format!(
        "{}/tests/fixtures/font-oracle/{name}.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut ids = BTreeSet::new();
    for line in &got {
        assert!(
            ids.insert(line.id()),
            "{name}: case recorded twice: {}",
            line.id()
        );
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    if blessing() {
        for text in want
            .lines()
            .filter(|t| !t.is_empty() && !t.starts_with('#'))
        {
            let line =
                Line::parse(text).unwrap_or_else(|| panic!("malformed corpus line: {text:?}"));
            if !present.contains_key(&line.key) {
                got.push(line);
            }
        }
        got.sort_by_key(Line::id);
        let mut text = HEADER.to_string();
        for line in &got {
            let _ = writeln!(text, "{}", line.render());
        }
        std::fs::create_dir_all(format!(
            "{}/tests/fixtures/font-oracle",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("corpus directory");
        std::fs::write(&path, text).expect("bless font oracle corpus");
        return;
    }
    assert!(
        !want.is_empty(),
        "missing font oracle corpus {path}; record it with ATERM_BLESS_GOLDEN=1"
    );
    let report = compare(&want, &got, present, elsewhere);
    if !report.skipped.is_empty() || !report.unrecorded.is_empty() {
        eprintln!(
            "font oracle `{name}`: {} answers compared; {} lines skipped for faces this host lacks {:?}; \
             faces present but never recorded {:?}",
            report.compared, report.skipped_lines, report.skipped, report.unrecorded
        );
    }
    assert!(
        report.moved.is_empty(),
        "font oracle `{name}`: {} answer(s) moved from {path}:\n{}\nA font engine or renderer change \
         moved these. Fix the change, or re-record with ATERM_BLESS_GOLDEN=1 and say in the commit \
         which answer moved and why.",
        report.moved.len(),
        report.moved.join("\n")
    );
    assert!(
        report.compared >= 200,
        "font oracle `{name}`: only {} answers compared",
        report.compared
    );
}

fn answer<'a>(lines: &'a [Line], label: &str, feats: &str, case: &str) -> &'a str {
    lines
        .iter()
        .find(|l| l.label == label && l.feats == feats && l.case == case && l.inst == "-")
        .map(|l| l.answer.as_str())
        .unwrap_or_else(|| panic!("no {label} {case} [{feats}] answer"))
}

/// The lines of committed faces whose answer at a non-default instance
fn moved_by_coordinates(lines: &[Line], present: &Present) -> usize {
    let case_of = |l: &Line| {
        format!(
            "{}\t{}#{}\t{}\t{}\t{}",
            l.label, l.key, l.index, l.feats, l.case, l.input
        )
    };
    let committed = |l: &&Line| present.get(&l.key) == Some(&true);
    let at_default: BTreeMap<String, &str> = lines
        .iter()
        .filter(committed)
        .filter(|l| l.inst == "-")
        .map(|l| (case_of(l), l.answer.as_str()))
        .collect();
    lines
        .iter()
        .filter(committed)
        .filter(|l| l.inst != "-")
        .filter(|l| at_default.get(&case_of(l)).is_some_and(|&a| a != l.answer))
        .count()
}

/// What a shaping line's face has, for telling the replay's deltas apart: a
#[derive(Clone, Default)]
struct ShapingTraits {
    positions: bool,
    morx: bool,
    nominal: BTreeMap<char, u32>,
}

fn shaping_traits(bytes: &[u8], index: u32, chars: &BTreeSet<char>) -> ShapingTraits {
    let Ok(face) = aterm_font::Face::parse(bytes, index) else {
        return ShapingTraits::default();
    };
    let raw = face.raw_face();
    ShapingTraits {
        positions: face.tables().gpos.is_some() || face.tables().kern.is_some(),
        morx: raw.table(aterm_font::Tag::from_bytes(b"morx")).is_some(),
        nominal: chars
            .iter()
            .map(|&c| (c, face.glyph_index(c).map_or(0, |g| u32::from(g.0))))
            .collect(),
    }
}

/// Every character the corpus's `shape` lines feed the shaper, each with its
fn corpus_chars(corpus_text: &str) -> BTreeSet<char> {
    let mut out = BTreeSet::from([' ']);
    for line in corpus_text
        .lines()
        .filter(|t| t.starts_with("shape\t"))
        .filter_map(Line::parse)
    {
        for c in corpus::unescape(&line.input).unwrap_or_default().chars() {
            out.insert(c);
            out.extend(aterm_bidi::mirror_glyph(c));
        }
    }
    out
}

/// The accepted ways aterm-font's shaper differs from a recorded rustybuzz
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ShapingDelta {
    /// Upstream normalizes before mapping: it decomposes a character the face
    Normalization,
    /// Upstream mirrors every mirrorable character of a right-to-left run,
    Mirroring,
    /// Upstream positions with `GPOS`, `kern` or fallback mark placement; the
    Positions,
}

/// `<header> | gid:cluster ...`: an answer without its advances and offsets.
fn ids_and_clusters(answer: &str) -> String {
    let (head, rest) = answer.split_once('|').unwrap_or((answer, ""));
    let glyphs: Vec<String> = rest
        .split_whitespace()
        .map(|t| t.splitn(3, ':').take(2).collect::<Vec<_>>().join(":"))
        .collect();
    format!("{head}| {}", glyphs.join(" "))
}

/// An answer's `(glyph id, cluster)` pairs in logical order: a right-to-left
fn logical_glyphs(answer: &str) -> Option<Vec<(u32, usize)>> {
    let (head, rest) = answer.split_once('|')?;
    let mut out = Vec::new();
    for token in rest.split_whitespace() {
        let mut fields = token.split(':');
        let gid = fields.next()?.parse().ok()?;
        let cluster = fields.next()?.parse().ok()?;
        out.push((gid, cluster));
    }
    if head.starts_with("RTL") {
        out.reverse();
    }
    Some(out)
}

/// The recorded answer as the shaper gives it, without mirroring: in a
fn unmirrored(line: &Line, traits: &ShapingTraits) -> Option<String> {
    let text = corpus::unescape(&line.input)?;
    let (head, rest) = line.answer.split_once('|')?;
    if !head.starts_with("RTL") {
        return None;
    }
    let mut tokens = Vec::new();
    for token in rest.split_whitespace() {
        let mut fields = token.splitn(3, ':');
        let gid: u32 = fields.next()?.parse().ok()?;
        let cluster: usize = fields.next()?.parse().ok()?;
        tokens.push((gid, cluster, fields.next()?));
    }
    let advance_of = |gid: u32| tokens.iter().find(|t| t.0 == gid).map(|t| t.2);
    let mut swapped = 0usize;
    let mut out = Vec::new();
    for &(gid, cluster, advance) in &tokens {
        let ch = text.get(cluster..)?.chars().next()?;
        match aterm_bidi::mirror_glyph(ch) {
            Some(mirror) if traits.nominal.get(&mirror) == Some(&gid) => {
                let own = *traits.nominal.get(&ch)?;
                swapped += 1;
                out.push(format!("{own}:{cluster}:{}", advance_of(own)?));
            }
            _ => out.push(format!("{gid}:{cluster}:{advance}")),
        }
    }
    (swapped > 0).then(|| format!("{head}| {}", out.join(" ")))
}

/// Whether `now` maps the line's text as written, with no normalization: read
fn unnormalized(line: &Line, now: &str, traits: &ShapingTraits) -> bool {
    let ignorable = |c: char| matches!(c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}');
    let (Some(text), Some(glyphs), Some(recorded)) = (
        corpus::unescape(&line.input),
        logical_glyphs(now),
        logical_glyphs(&line.answer),
    ) else {
        return false;
    };
    let clustered = |glyphs: &[(u32, usize)]| {
        let mut groups: Vec<(usize, Vec<u32>)> = Vec::new();
        for &(gid, cluster) in glyphs {
            match groups.last_mut() {
                Some((c, ids)) if *c == cluster => ids.push(gid),
                _ => groups.push((cluster, vec![gid])),
            }
        }
        groups
    };
    let groups = clustered(&glyphs);
    let recorded = clustered(&recorded);
    if groups.first().map(|g| g.0) != Some(0)
        || groups.windows(2).any(|w| w[0].0 >= w[1].0)
        || groups.iter().any(|g| !text.is_char_boundary(g.0))
    {
        return false;
    }
    for (i, (cluster, ids)) in groups.iter().enumerate() {
        let end = groups.get(i + 1).map_or(text.len(), |g| g.0);
        let Some(chars) = text.get(*cluster..end) else {
            return false;
        };
        let chars: Vec<char> = chars.chars().collect();
        if ids.len() < chars.len() {
            if !recorded.contains(&(*cluster, ids.clone())) {
                return false;
            }
            continue;
        }
        if ids.len() > chars.len() {
            return false;
        }
        for (&gid, &c) in ids.iter().zip(&chars) {
            if aterm_grapheme::general_category(c).is_mark() {
                continue;
            }
            let own = if ignorable(c) { ' ' } else { c };
            if traits.nominal.get(&own) != Some(&gid) {
                return false;
            }
        }
    }
    true
}

fn shaping_delta(line: &Line, now: &str, traits: &ShapingTraits) -> Option<ShapingDelta> {
    if unmirrored(line, traits).as_deref() == Some(now) {
        return Some(ShapingDelta::Mirroring);
    }
    if (line.case.starts_with("norm.") || line.case.ends_with(".niqqud.logical"))
        && unnormalized(line, now, traits)
    {
        return Some(ShapingDelta::Normalization);
    }
    let recorded_offsets = line.answer.contains('@');
    (ids_and_clusters(now) == ids_and_clusters(&line.answer)
        && (traits.positions || recorded_offsets))
        .then_some(ShapingDelta::Positions)
}

/// What the shaping replay found.
#[derive(Default, Debug)]
struct ShapingReport {
    /// Answered exactly as recorded.
    exact: usize,
    /// Accepted deltas, by class.
    deltas: BTreeMap<ShapingDelta, usize>,
    /// Colour-face lines on a `morx` face: CoreText's (`coretext_answers_the_emoji_lines`).
    coretext: usize,
    moved: Vec<String>,
    skipped_lines: usize,
    /// System faces this host has with no recorded lines.
    unrecorded: BTreeSet<String>,
}

fn compare_shaping(
    want: &str,
    got: &[Line],
    present: &Present,
    traits: &BTreeMap<(String, u32), ShapingTraits>,
) -> ShapingReport {
    let mut report = ShapingReport::default();
    let now: BTreeMap<String, &Line> = got.iter().map(|l| (l.id(), l)).collect();
    let mut seen = BTreeSet::new();
    let mut keys_recorded = BTreeSet::new();
    for text in want
        .lines()
        .filter(|t| !t.is_empty() && !t.starts_with('#'))
    {
        let line = Line::parse(text).unwrap_or_else(|| panic!("malformed corpus line: {text:?}"));
        if !present.contains_key(&line.key) {
            report.skipped_lines += 1;
            continue;
        }
        let t = traits
            .get(&(line.key.clone(), line.index))
            .cloned()
            .unwrap_or_default();
        keys_recorded.insert(line.key.clone());
        let id = line.id();
        seen.insert(id.clone());
        if t.morx && line.case.starts_with("emoji.") {
            report.coretext += 1;
            continue;
        }
        let Some(now) = now.get(&id) else {
            report.moved.push(format!(
                "  {id}\n    no longer shaped (was {})",
                line.answer
            ));
            continue;
        };
        if now.answer == line.answer {
            report.exact += 1;
        } else if let Some(delta) = shaping_delta(&line, &now.answer, &t) {
            *report.deltas.entry(delta).or_default() += 1;
        } else {
            report.moved.push(format!(
                "  {id}\n    recorded {}\n    now      {}",
                line.answer, now.answer
            ));
        }
    }
    for line in got {
        if seen.contains(&line.id()) {
            continue;
        }
        if !keys_recorded.contains(&line.key) && present.get(&line.key) == Some(&false) {
            report
                .unrecorded
                .insert(format!("{} {}", line.label, line.key));
            continue;
        }
        report
            .moved
            .push(format!("  {}\n    new case {}", line.id(), line.answer));
    }
    report
}

/// The shaping replay: every `shape` line, recorded by rustybuzz 0.20.1,
#[test]
fn shaping_answers_hold() {
    let path = format!(
        "{}/tests/fixtures/font-oracle/shaping.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    let want = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    let chars = corpus_chars(&want);
    let mut got = Vec::new();
    let mut gates = BTreeMap::new();
    let mut traits = BTreeMap::new();
    let present = for_each_face(|l| {
        for &index in l.face.indices {
            traits.insert(
                (l.key.clone(), index),
                shaping_traits(&l.bytes, index, &chars),
            );
        }
        shaping_lines(l, &mut gates, &mut got);
    });
    let on = corpus::fmt_features(&feature_list(&[], true));
    let off = corpus::fmt_features(&feature_list(&typical_config(), false));
    assert_ne!(
        answer(&got, "JetBrainsMono-Regular", &on, "lig.arrow"),
        answer(&got, "JetBrainsMono-Regular", &off, "lig.arrow"),
        "=> must ligate under liga/calt and not without them"
    );
    assert!(
        answer(&got, "CascadiaMonoPL-Regular", &on, "ar.marhaba.visual.0")
            .starts_with("RTL Arab |"),
        "a visual-order Arabic run is guessed right-to-left"
    );
    assert!(
        moved_by_coordinates(&got, &present) > 0,
        "no committed face shapes differently at a non-default instance"
    );
    let report = compare_shaping(&want, &got, &present, &traits);
    eprintln!(
        "font oracle `shaping`: {} exact, deltas {:?}, {} CoreText, {} lines skipped for faces this host lacks; \
         faces present but never recorded {:?}",
        report.exact, report.deltas, report.coretext, report.skipped_lines, report.unrecorded
    );
    assert!(
        report.moved.is_empty(),
        "font oracle `shaping`: {} answer(s) moved from {path}:\n{}\nOnly the TF.6 deltas \
         (normalization, mirroring, positions) may differ from rustybuzz's recorded answers.",
        report.moved.len(),
        report.moved.join("\n")
    );
    assert!(
        report.exact >= 500,
        "font oracle `shaping`: only {} answers compared",
        report.exact
    );
}

/// Negative control for the delta classes: an advance that moves on a face
#[test]
fn a_shaping_delta_is_classified_narrowly() {
    let line = |case: &str, input: &str, answer: &str| Line {
        kind: "shape".to_string(),
        label: "face".to_string(),
        key: "aa".to_string(),
        index: 0,
        inst: "-".to_string(),
        feats: "-".to_string(),
        case: case.to_string(),
        input: input.to_string(),
        answer: answer.to_string(),
    };
    let nominal = BTreeMap::from([
        ('(', 10),
        (')', 11),
        ('\u{5D0}', 30),
        ('e', 20),
        ('\u{301}', 40),
        (' ', 3),
    ]);
    let positioned = ShapingTraits {
        positions: true,
        morx: false,
        nominal: nominal.clone(),
    };
    let plain = ShapingTraits {
        nominal,
        ..ShapingTraits::default()
    };
    let recorded = line("lig.arrow", "=>", "LTR Zzzz | 7:0:600 8:1:600");
    assert_eq!(
        shaping_delta(&recorded, "LTR Zzzz | 7:0:590 8:1:600", &positioned),
        Some(ShapingDelta::Positions)
    );
    assert_eq!(
        shaping_delta(&recorded, "LTR Zzzz | 7:0:590 8:1:600", &plain),
        None
    );
    assert_eq!(
        shaping_delta(&recorded, "LTR Zzzz | 7:0:600 9:1:600", &positioned),
        None
    );
    assert_eq!(
        shaping_delta(&recorded, "RTL Zzzz | 7:0:600 8:1:600", &positioned),
        None
    );

    let parens = line(
        "he.parens.logical",
        "(\\u{5d0})",
        "RTL Hebr | 10:3:600 30:1:600 11:0:600",
    );
    assert_eq!(
        shaping_delta(&parens, "RTL Hebr | 11:3:600 30:1:600 10:0:600", &plain),
        Some(ShapingDelta::Mirroring)
    );
    assert_eq!(
        shaping_delta(&parens, "RTL Hebr | 11:3:600 31:1:600 10:0:600", &plain),
        None
    );
    assert_eq!(
        shaping_delta(&parens, "RTL Hebr | 11:3:600 30:1:600 11:0:600", &plain),
        None
    );
    let ltr = line(
        "mirror.parens",
        "(\\u{5d0})",
        "LTR Hebr | 11:0:600 30:1:600 10:3:600",
    );
    assert_eq!(
        shaping_delta(&ltr, "LTR Hebr | 10:0:600 30:1:600 11:3:600", &plain),
        None
    );

    let acute = line("norm.e-acute", "e\\u{301}", "LTR Latn | 233:0:600");
    assert_eq!(
        shaping_delta(&acute, "LTR Latn | 20:0:600 40:0:0", &plain),
        Some(ShapingDelta::Normalization)
    );
    assert_eq!(
        shaping_delta(&acute, "LTR Latn | 21:0:600 40:0:0", &plain),
        None
    );
    assert_eq!(
        shaping_delta(&acute, "LTR Latn | 20:0:600 40:0:0 40:0:0", &plain),
        None
    );
    assert_eq!(shaping_delta(&acute, "LTR Latn | 234:0:600", &plain), None);
    let other = line("lig.e-acute", "e\\u{301}", "LTR Latn | 233:0:600");
    assert_eq!(
        shaping_delta(&other, "LTR Latn | 20:0:600 40:0:0", &plain),
        None
    );
}

/// [`ShapingDelta::Normalization`]: on every `norm.*` and `*.niqqud.logical`
#[test]
fn normalization_is_not_replayed() {
    let corpus_text = std::fs::read_to_string(format!(
        "{}/tests/fixtures/font-oracle/shaping.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let chars = corpus_chars(&corpus_text);
    let lines: Vec<Line> = corpus_text
        .lines()
        .filter(|t| t.starts_with("shape\t"))
        .filter_map(Line::parse)
        .filter(|l| l.case.starts_with("norm.") || l.case.ends_with(".niqqud.logical"))
        .collect();
    let (mut checked, mut niqqud, mut differ, mut recorded_fails) =
        (0usize, 0usize, 0usize, 0usize);
    for_each_face(|l| {
        for line in lines.iter().filter(|line| line.key == l.key) {
            let traits = shaping_traits(&l.bytes, line.index, &chars);
            let text = corpus::unescape(&line.input).unwrap();
            let coords = corpus::parse_coords(&line.inst).unwrap();
            let features = corpus::parse_features(&line.feats).unwrap();
            let now = shaper::shape(&l.bytes, line.index, &coords, &features, &text);
            assert!(
                unnormalized(line, &now, &traits),
                "{} {}: {now}\n  (recorded {})",
                line.label,
                line.case,
                line.answer
            );
            checked += 1;
            niqqud += usize::from(line.case.contains("niqqud"));
            differ += usize::from(ids_and_clusters(&line.answer) != ids_and_clusters(&now));
            recorded_fails += usize::from(!unnormalized(line, &line.answer, &traits));
        }
    });
    assert!(checked >= 20, "only {checked} normalization lines checked");
    assert!(niqqud >= 8, "only {niqqud} niqqud lines checked");
    assert!(
        differ >= 10,
        "rustybuzz's normalization moved only {differ} lines"
    );
    assert!(
        recorded_fails >= 10,
        "only {recorded_fails} recorded answers fail the check"
    );
}

#[test]
fn reader_answers_hold() {
    let mut got = Vec::new();
    let present = for_each_face(|l| reader_lines(l, &mut got));
    finish("reader", got, &present, &["read"]);
}
