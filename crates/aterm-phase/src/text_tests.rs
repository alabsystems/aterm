// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_byte_trims_are_the_char_trims() {
    let spaces: Vec<char> = (0..=0x10_FFFF_u32)
        .filter_map(char::from_u32)
        .filter(|c| c.is_whitespace())
        .collect();
    let check = |s: &str| {
        assert_eq!(trim_start(s), s.trim_start(), "{s:?}");
        assert_eq!(trim_end(s), s.trim_end(), "{s:?}");
        assert_eq!(trim(s), s.trim(), "{s:?}");
        assert_eq!(
            leading_space_chars(s),
            s.chars().take_while(|c| c.is_whitespace()).count(),
            "{s:?}"
        );
    };
    for c in (0..=0x10_FFFF_u32).filter_map(char::from_u32) {
        let mut buf = [0; 4];
        let one = c.encode_utf8(&mut buf);
        assert_eq!(
            space_at(one.as_bytes(), 0),
            if c.is_whitespace() { one.len() } else { 0 },
            "{c:?}"
        );
        assert_eq!(
            space_before(one.as_bytes(), one.len()),
            if c.is_whitespace() { one.len() } else { 0 },
            "{c:?}"
        );
        check(one);
    }
    let near: Vec<char> = (0x80..=0x3100_u32)
        .filter_map(char::from_u32)
        .filter(|&c| {
            let (mut cb, mut sb) = ([0; 4], [0; 4]);
            let cb = c.encode_utf8(&mut cb).as_bytes();
            spaces.iter().any(|s| {
                let sb = s.encode_utf8(&mut sb).as_bytes();
                sb.len() > 1 && sb.len() == cb.len() && sb[..sb.len() - 1] == cb[..cb.len() - 1]
            })
        })
        .collect();
    for &a in &spaces {
        for &b in &spaces {
            check(&format!("{a}{b}x{a}y{b}{a}"));
        }
        for &c in &near {
            check(&format!("{a}{c}{a}"));
            check(&format!("{c}{a}{c}"));
        }
    }
    for s in [
        "",
        " ",
        "  ❯ 1. Yes  ",
        "\u{a0}❯\u{a0}",
        "─── ┬ ───",
        "⏺ said\t\n",
    ] {
        check(s);
    }
}

#[test]
fn the_byte_search_is_the_str_search() {
    let hays = [
        "",
        "Esc to cancel · Tab to amend",
        "  Esc to Esc to ",
        "aaab",
        "axb aab",
        "❯ 1. Yes ❯",
        "3 shells · 2 shells ·",
        "·",
    ];
    let needles = [
        "",
        "Esc to ",
        "aab",
        "❯",
        "· ",
        " shells ·",
        "·",
        "zz",
        "Esc to cancel · Tab to amend!",
    ];
    for h in hays {
        for n in needles {
            assert_eq!(find(h, n), h.find(n), "{h:?} {n:?}");
            assert_eq!(contains(h, n), h.contains(n), "{h:?} {n:?}");
        }
    }
}
