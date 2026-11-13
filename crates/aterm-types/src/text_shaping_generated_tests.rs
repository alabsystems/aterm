// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

use aterm_spec::rng::XorShift;

fn string(rng: &mut XorShift, alphabet: &[char], max: usize) -> String {
    (0..rng.index(max + 1))
        .map(|_| *rng.pick(alphabet))
        .collect()
}

const ALNUM: &[char] = &['a', 'k', 'z', 'A', 'L', 'Z', '0', '5', '9'];
const SOUP: &[char] = &['a', 'l', 'z', 'L', '0', '1', '9', ' ', '+', '-', '='];
const ANY: &[char] = &[
    'a', 'Z', '0', ' ', '\t', '\n', '\u{a0}', '\u{3000}', '+', '-', '=', ',', '\0', '\u{7f}', 'é',
    '世', '🎉', '\u{200b}',
];

fn assert_well_formed(f: &FontFeature) {
    assert!(f.tag.iter().all(u8::is_ascii), "tag bytes must be ASCII");
    let first_space = f.tag.iter().position(|&b| b == b' ').unwrap_or(4);
    assert!(
        f.tag[first_space..].iter().all(|&b| b == b' '),
        "tag must be right-padded with spaces (no interior space): {:?}",
        f.tag
    );
    assert!(
        first_space >= 1,
        "tag must have at least one non-space byte"
    );
    assert!(
        f.tag[..first_space]
            .iter()
            .all(|b| !b.is_ascii_whitespace()),
        "the tag prefix must not contain whitespace"
    );
}

#[test]
fn parse_never_panics_and_is_well_formed() {
    let mut rng = XorShift::new(0xF047_FEA7);
    for i in 0..1024 {
        let s = string(&mut rng, if i % 2 == 0 { ANY } else { SOUP }, 48);
        let out = parse_font_features(&s);
        assert!(out.len() <= s.split_whitespace().count(), "{s:?}");
        for f in &out {
            assert_well_formed(f);
        }
    }
}

#[test]
fn affixes_and_explicit_values() {
    let mut rng = XorShift::new(0xAF_F1CE5);
    for _ in 0..256 {
        let tag = {
            let mut t = string(&mut rng, ALNUM, 3);
            t.push(*rng.pick(ALNUM));
            t
        };
        let bare = parse_font_features(&tag);
        assert_eq!(bare.len(), 1, "{tag:?}");
        assert_eq!(bare[0].value, 1);
        assert_eq!(parse_font_features(&format!("+{tag}")), bare);
        let minus = parse_font_features(&format!("-{tag}"));
        assert_eq!(minus.len(), 1);
        assert_eq!(minus[0].value, 0);
        assert_eq!(minus[0].tag, bare[0].tag);

        let alpha: String = tag.chars().filter(char::is_ascii_alphabetic).collect();
        if !alpha.is_empty() {
            let v = rng.below(u64::from(u32::MAX) + 1) as u32;
            let out = parse_font_features(&format!("{alpha}={v}"));
            assert_eq!(out.len(), 1, "{alpha}={v}");
            assert_eq!(out[0].value, v);
        }
    }
    for v in [0, 1, u32::MAX] {
        assert_eq!(parse_font_features(&format!("liga={v}"))[0].value, v);
    }
}
