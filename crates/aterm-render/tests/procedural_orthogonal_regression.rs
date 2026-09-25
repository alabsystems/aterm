// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PROOF (regression corpus) — the ORTHOGONAL procedural families are
//! byte-identical to their pre-AA rasterization, and the ANTIALIASED families
//! are pinned byte-exact to their own corpus.
//!
//! The AA overhaul (4× supersampled diagonals / arcs / Powerline / wedges) is
//! scoped to the DIAGONAL/CURVED families only; every axis-aligned family —
//! solid & dashed box lines, junctions, doubles, half lines, eighth/half
//! blocks, shades (at phase 0), quadrants, braille, sextants — must not move
//! by a single byte. This locks that domain: an FNV-1a-64 hash of every
//! orthogonal glyph's coverage, over an odd/even size lattice, against a
//! corpus fixture generated from the pre-change rasterizer.
//!
//! The AA families (arcs, diagonals, Powerline, wedges) carry interior grey
//! levels by design, so they cannot be "unchanged since before AA"; what the
//! second corpus pins is that they do not move by a byte without a reviewed
//! re-bless. It replaced the two golden-PNG strips (`procedural_golden.rs`,
//! 9x19 and 10x20), which pinned the same coverage bytes for seven AA glyphs
//! at two sizes; the corpus pins every AA code point at every lattice size.
//!
//! The fixtures are INTENTIONALLY hard to regenerate by accident: bless with
//! `ATERM_BLESS_GOLDEN=1 targo --unverified test -p aterm-render --test
//! procedural_orthogonal_regression` and justify the diff in review.

use aterm_render::procedural;

/// Odd/even mixes, squat, tall, degenerate-tiny — the seam-hazard lattice.
const SIZES: &[(usize, usize)] = &[
    (1, 1),
    (2, 2),
    (3, 7),
    (7, 15),
    (8, 16),
    (9, 19),
    (10, 20),
    (11, 21),
    (12, 22),
    (16, 32),
    (20, 8),
];

/// Every ORTHOGONAL (axis-aligned, hard-0/255) procedural code point: the box
/// range minus arcs (U+256D–2570) and diagonals (U+2571–2573), blocks,
/// braille and sextants. Powerline (U+E0B0–E0BF) and the wedge/triangle range
/// (U+1FB3C–1FB6F) are the AA families — excluded by definition.
fn orthogonal_chars() -> impl Iterator<Item = char> + Clone {
    (0x2500u32..=0x256C)
        .chain(0x2574..=0x259F)
        .chain(0x2800..=0x28FF)
        .chain(0x1FB00..=0x1FB3B)
        .map(|cp| char::from_u32(cp).expect("valid code points"))
}

/// FNV-1a 64 over a byte stream — dependency-free, stable across platforms.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// Every ANTIALIASED procedural code point ([`procedural::antialiased`]): the
/// arcs and diagonals (U+256D–2573), the wedge/triangle range
/// (U+1FB3C–1FB6F) and Powerline (U+E0B0–E0BF).
fn aa_chars() -> impl Iterator<Item = char> + Clone {
    (0x256Du32..=0x2573)
        .chain(0x1FB3C..=0x1FB6F)
        .chain(0xE0B0..=0xE0BF)
        .map(|cp| char::from_u32(cp).expect("valid code points"))
}

/// One corpus line per size: `{w}x{h} {hash:016x}`, hashing every glyph's code
/// point + coverage bytes in code-point order. `hard` requires every coverage
/// byte to be 0 or 255 (the orthogonal families' law).
fn corpus(chars: impl Iterator<Item = char> + Clone, hard: bool) -> String {
    let mut out = String::new();
    for &(w, h) in SIZES {
        let mut hash = Fnv::new();
        for ch in chars.clone() {
            let cov = procedural::coverage(ch, w, h)
                .unwrap_or_else(|| panic!("{ch:?} must be procedural at {w}x{h}"));
            assert_eq!(cov.len(), w * h, "{ch:?} at {w}x{h}: wrong size");
            assert!(
                !hard || cov.iter().all(|&b| b == 0 || b == 255),
                "{ch:?} at {w}x{h}: orthogonal families must stay hard 0/255"
            );
            hash.update(&u32::from(ch).to_le_bytes());
            hash.update(&cov);
        }
        out.push_str(&format!("{w}x{h} {:016x}\n", hash.0));
    }
    out
}

/// Compare `got` against the fixture `name`, or re-bless it.
fn check_corpus(got: &str, name: &str) -> Option<String> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("ATERM_BLESS_GOLDEN").is_some() {
        std::fs::write(&path, got).expect("bless corpus");
        return None;
    }
    Some(std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing corpus {path}: {e}; generate with ATERM_BLESS_GOLDEN=1")
    }))
}

#[test]
fn orthogonal_families_match_pre_aa_corpus() {
    assert!(
        orthogonal_chars().all(|ch| !procedural::antialiased(ch)),
        "the orthogonal range must hold no AA code point"
    );
    let got = corpus(orthogonal_chars(), true);
    let Some(want) = check_corpus(&got, "procedural_orthogonal_corpus.txt") else {
        return;
    };
    assert_eq!(
        got, want,
        "an ORTHOGONAL procedural family drifted from the pre-AA corpus; those \
         glyphs are pinned byte-exact (the AA rescope covers ONLY diagonals, \
         arcs, Powerline and wedges). If this change is deliberate, re-bless \
         with ATERM_BLESS_GOLDEN=1 and say why in the commit message."
    );
}

#[test]
fn aa_families_match_their_corpus() {
    assert!(
        aa_chars().all(procedural::antialiased),
        "every code point in the AA corpus must be an AA family member"
    );
    let got = corpus(aa_chars(), false);
    let Some(want) = check_corpus(&got, "procedural_aa_corpus.txt") else {
        return;
    };
    assert_eq!(
        got, want,
        "an ANTIALIASED procedural family (arcs, diagonals, Powerline, wedges) \
         drifted from its corpus. If this change is deliberate, re-bless with \
         ATERM_BLESS_GOLDEN=1 and say why in the commit message."
    );
}
