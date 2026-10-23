// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential shaper comparison between rustybuzz and aterm-font's shaper.

use crate::{Loaded, load, probe, shaper};

/// splitmix64: a fixed seed draws the same runs on every host.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick(&mut self, xs: &[char]) -> char {
        xs[self.below(xs.len())]
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn range(lo: u32, hi: u32) -> Vec<char> {
    (lo..=hi).filter_map(char::from_u32).collect()
}

struct Pools {
    ascii: Vec<char>,
    letters: Vec<char>,
    hebrew: Vec<char>,
    arabic: Vec<char>,
    arabic_digits: Vec<char>,
    harakat: Vec<char>,
    neutral: Vec<char>,
    ignorables: Vec<char>,
    selectors: Vec<char>,
}

impl Pools {
    fn new() -> Self {
        let mut letters: Vec<char> = range(0xC0, 0xFF)
            .into_iter()
            .filter(|&c| c != '\u{D7}' && c != '\u{F7}')
            .collect();
        letters.extend(range(0x391, 0x3A9));
        letters.extend(range(0x3B1, 0x3C9));
        letters.extend(range(0x410, 0x44F));
        let mut arabic = range(0x621, 0x63A);
        arabic.extend(range(0x641, 0x64A));
        arabic.extend([
            '\u{671}', '\u{67E}', '\u{686}', '\u{698}', '\u{6A9}', '\u{6AF}', '\u{6CC}',
        ]);
        let ascii = range(0x21, 0x7E);
        let neutral = ascii
            .iter()
            .copied()
            .filter(|c| !c.is_ascii_alphabetic() && aterm_bidi::mirror_glyph(*c).is_none())
            .collect();
        Self {
            ascii,
            letters,
            hebrew: range(0x5D0, 0x5EA),
            arabic,
            arabic_digits: range(0x660, 0x669),
            harakat: range(0x64B, 0x652),
            neutral,
            ignorables: vec!['\u{200B}', '\u{2060}', '\u{AD}', '\u{200E}', '\u{200F}'],
            selectors: vec!['\u{FE00}', '\u{FE01}', '\u{FE0E}', '\u{FE0F}'],
        }
    }

    /// These pools on `face`: a letter the face lacks is dropped, so no run
    /// holds one upstream would decompose.
    fn on(&self, face: &ttf_parser::Face<'_>) -> Self {
        let mapped = |pool: &[char]| -> Vec<char> {
            pool.iter()
                .copied()
                .filter(|&c| face.glyph_index(c).is_some())
                .collect()
        };
        let or_ascii = |pool: Vec<char>| {
            if pool.is_empty() {
                self.ascii.clone()
            } else {
                pool
            }
        };
        Self {
            ascii: self.ascii.clone(),
            letters: or_ascii(mapped(&self.letters)),
            hebrew: self.hebrew.clone(),
            arabic: self
                .arabic
                .iter()
                .copied()
                .filter(|&c| !('\u{622}'..='\u{626}').contains(&c) || face.glyph_index(c).is_some())
                .collect(),
            arabic_digits: self.arabic_digits.clone(),
            harakat: self.harakat.clone(),
            neutral: self.neutral.clone(),
            ignorables: self.ignorables.clone(),
            selectors: self.selectors.clone(),
        }
    }

    /// One run of 2 to 10 characters from the domain the module doc states.
    fn run(&self, rng: &mut Rng) -> String {
        let len = 2 + rng.below(9);
        let kind = rng.below(10);
        let mut out = String::new();
        for _ in 0..len {
            let c = match kind {
                0..=3 => {
                    if rng.chance(60) {
                        rng.pick(&self.ascii)
                    } else {
                        rng.pick(&self.letters)
                    }
                }
                4 | 5 => {
                    if rng.chance(75) {
                        rng.pick(&self.hebrew)
                    } else {
                        rng.pick(&self.neutral)
                    }
                }
                6..=8 => match rng.below(20) {
                    0 => '\u{640}',
                    1 => rng.pick(&self.arabic_digits),
                    2 | 3 => rng.pick(&self.neutral),
                    4 if out.chars().last().is_some_and(|p| self.arabic.contains(&p)) => {
                        if rng.chance(50) {
                            '\u{200D}'
                        } else {
                            '\u{200C}'
                        }
                    }
                    _ => rng.pick(&self.arabic),
                },
                _ => match rng.below(3) {
                    0 => rng.pick(&self.hebrew),
                    1 => rng.pick(&self.arabic),
                    _ => {
                        if rng.chance(50) {
                            rng.pick(&self.neutral)
                        } else {
                            let ascii_letters: Vec<char> = ('a'..='z').collect();
                            rng.pick(&ascii_letters)
                        }
                    }
                },
            };
            out.push(c);
            if self.arabic.contains(&c) && rng.chance(20) {
                out.push(rng.pick(&self.harakat));
            }
            if rng.chance(4) {
                out.push(rng.pick(&self.selectors));
            }
            if rng.chance(4) {
                out.push(rng.pick(&self.ignorables));
            }
        }
        out
    }
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

/// Whether upstream's answer changed any glyph from the face's nominal one
/// (the run is not a plain `cmap` walk, so the comparison is not vacuous).
fn substituted(face: &ttf_parser::Face<'_>, text: &str, answer: &str) -> bool {
    let rest = answer.split_once('|').map_or("", |(_, r)| r);
    let mut ids: Vec<u32> = rest
        .split_whitespace()
        .filter_map(|t| t.split(':').next()?.parse().ok())
        .collect();
    if answer.starts_with("RTL") {
        ids.reverse();
    }
    let nominal: Vec<u32> = text
        .chars()
        .map(|c| face.glyph_index(c).map_or(0, |g| u32::from(g.0)))
        .collect();
    ids != nominal
}

fn has_morx(face: &ttf_parser::Face<'_>) -> bool {
    face.raw_face()
        .table(ttf_parser::Tag::from_bytes(b"morx"))
        .is_some()
}

const FEATURE_LISTS: [&[([u8; 4], u32)]; 4] = [
    &[(*b"liga", 1), (*b"calt", 1)],
    &[(*b"liga", 0), (*b"calt", 0)],
    &[
        (*b"liga", 1),
        (*b"calt", 1),
        (*b"zero", 1),
        (*b"ss01", 1),
        (*b"cv01", 1),
    ],
    &[
        (*b"liga", 0),
        (*b"calt", 0),
        (*b"zero", 1),
        (*b"ss01", 1),
        (*b"cv01", 1),
    ],
];

/// The macOS faces for the scripts whose shapers TF.6 did not carve.
const COMPLEX_FACES: [&str; 8] = [
    "/System/Library/Fonts/Kohinoor.ttc",
    "/System/Library/Fonts/KohinoorBangla.ttc",
    "/System/Library/Fonts/ThonburiUI.ttc",
    "/System/Library/Fonts/Supplemental/Lao MN.ttc",
    "/System/Library/Fonts/Supplemental/Khmer MN.ttc",
    "/System/Library/Fonts/NotoSansMyanmar.ttc",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    "/System/Library/Fonts/Supplemental/DevanagariMT.ttc",
];

const COMPLEX_SCRIPTS: [(&str, u32, u32); 7] = [
    ("Devanagari", 0x900, 0x97F),
    ("Bengali", 0x980, 0x9FF),
    ("Thai", 0xE00, 0xE7F),
    ("Lao", 0xE80, 0xEFF),
    ("Myanmar", 0x1000, 0x109F),
    ("Hangul jamo", 0x1100, 0x11FF),
    ("Khmer", 0x1780, 0x17FF),
];

pub(crate) fn run(dir: &str, runs_per_config: usize) {
    let pools = Pools::new();
    let mut rng = Rng(0x7F6_2026_1016);
    let faces: Vec<Loaded> = load(dir);
    let (mut faces_used, mut compared, mut exact, mut positions, mut subst) = (0, 0, 0, 0, 0);
    let mut differ: Vec<String> = Vec::new();
    let mut differ_count = 0usize;
    for l in &faces {
        for &index in l.face.indices {
            let Ok(face) = ttf_parser::Face::parse(&l.bytes, index) else {
                continue;
            };
            if has_morx(&face) {
                continue;
            }
            faces_used += 1;
            let pools = pools.on(&face);
            let mut instances: Vec<Vec<([u8; 4], f32)>> = vec![Vec::new()];
            if face
                .variation_axes()
                .into_iter()
                .any(|a| a.tag == ttf_parser::Tag::from_bytes(b"wght"))
            {
                instances.push(vec![(*b"wght", 700.0)]);
            }
            for coords in &instances {
                for features in FEATURE_LISTS {
                    for _ in 0..runs_per_config {
                        let text = pools.run(&mut rng);
                        let old = probe::shape(&l.bytes, index, coords, features, &text);
                        let new = shaper::shape(&l.bytes, index, coords, features, &text);
                        compared += 1;
                        subst += usize::from(substituted(&face, &text, &old));
                        if old == new {
                            exact += 1;
                        } else if ids_and_clusters(&old) == ids_and_clusters(&new) {
                            positions += 1;
                        } else {
                            differ_count += 1;
                            if differ.len() < 20 {
                                differ.push(format!(
                                    "  {} #{index} {} {:?}\n    rustybuzz {old}\n    aterm     {new}",
                                    l.face.label,
                                    probe::fmt_features(features),
                                    text
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    println!(
        "differential: {compared} runs over {faces_used} faces: {exact} identical, \
         {positions} differ in positions only, {differ_count} differ in glyph ids or clusters; \
         rustybuzz changed a glyph from its cmap on {subst}"
    );
    for d in &differ {
        println!("{d}");
    }
    complex(&faces, &mut rng, runs_per_config);
}

/// The scripts whose shapers TF.6 did not carve: how many runs of their
/// own-cell characters answer differently, per script, on every face that
/// maps every character of the run.
fn complex(faces: &[Loaded], rng: &mut Rng, runs: usize) {
    let mut files: Vec<(String, Vec<u8>)> = faces
        .iter()
        .map(|l| (l.face.label.to_string(), l.bytes.clone()))
        .collect();
    for path in COMPLEX_FACES {
        if let Ok(bytes) = std::fs::read(path)
            && !files.iter().any(|(_, held)| *held == bytes)
        {
            files.push((path.rsplit('/').next().unwrap_or(path).to_string(), bytes));
        }
    }
    let features: &[([u8; 4], u32)] = FEATURE_LISTS[0];
    for (script, lo, hi) in COMPLEX_SCRIPTS {
        let pool: Vec<char> = range(lo, hi)
            .into_iter()
            .filter(|&c| {
                aterm_grapheme::char_width(c) == 1
                    && aterm_grapheme::general_category(c)
                        != aterm_grapheme::GeneralCategory::Unassigned
            })
            .collect();
        let (mut compared, mut differ) = (0usize, 0usize);
        let mut faces_used: Vec<&str> = Vec::new();
        let mut example = None;
        for (label, bytes) in &files {
            let Ok(face) = ttf_parser::Face::parse(bytes, 0) else {
                continue;
            };
            if has_morx(&face) {
                continue;
            }
            let covered: Vec<char> = pool
                .iter()
                .copied()
                .filter(|&c| face.glyph_index(c).is_some())
                .collect();
            if covered.is_empty() || covered.len() < pool.len() / 2 {
                continue;
            }
            faces_used.push(label);
            for _ in 0..runs {
                let len = 2 + rng.below(5);
                let text: String = (0..len).map(|_| rng.pick(&covered)).collect();
                let old = probe::shape(bytes, 0, &[], features, &text);
                let new = shaper::shape(bytes, 0, &[], features, &text);
                compared += 1;
                if ids_and_clusters(&old) != ids_and_clusters(&new) {
                    differ += 1;
                    example.get_or_insert_with(|| {
                        format!("{label} {text:?}\n    rustybuzz {old}\n    aterm     {new}")
                    });
                }
            }
        }
        println!(
            "complex {script}: {} own-cell characters, {compared} runs on {} faces {faces_used:?}, \
             {differ} differ in glyph ids or clusters",
            pool.len(),
            faces_used.len()
        );
        if let Some(e) = example {
            println!("  e.g. {e}");
        }
    }
}
