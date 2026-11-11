// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::parse::{Flags, parse};

fn program(pattern: &str) -> Program {
    let ast = parse(pattern, Flags::default()).expect("parses");
    compile(&ast, usize::MAX).expect("compiles")
}

#[test]
fn the_second_byte_filter_arms_only_when_a_second_code_point_is_certain() {
    let second = |pattern: &str| {
        program(pattern)
            .first
            .and_then(|f| f.second_bytes)
            .map(|bytes| {
                (0..256usize)
                    .filter(|&b| bytes[b])
                    .filter_map(|b| u8::try_from(b).ok())
                    .map(char::from)
                    .collect::<Vec<char>>()
            })
    };

    assert_eq!(second("ERROR"), Some(vec!['R']));
    assert_eq!(second("(?i)ERROR"), Some(vec!['R', 'r']));
    assert_eq!(second("[ab]c"), Some(vec!['c']));
    assert_eq!(second("ab|cd"), Some(vec!['b', 'd']));
    assert_eq!(second(r"c\bo"), Some(vec!['o']));

    for pattern in [
        "E",    // one code point, so there is no second
        "Ex?",  // the tail is optional
        "Ex*",  // the tail is a star
        "E|Ex", // one branch stops after the first
        "E.*",  // the star can take nothing
        r"E\b", // only an assertion follows, which consumes nothing
    ] {
        assert_eq!(
            second(pattern),
            None,
            "{pattern} can match on one code point, so the filter must stay disarmed",
        );
    }
}

#[test]
fn the_ascii_bitmap_agrees_with_the_class_it_stands_in_for() {
    for pattern in [
        r"[abc]",
        r"[^abc]",
        r"[a-z]",
        r"[^a-z]",
        r"\d",
        r"\D",
        r"\w",
        r"\W",
        r"\s",
        r"\S",
        r"[^\s<>]",
        r"(?i)[^k]",
        r"(?i)[a-z]",
        r".",
        r"(?s).",
        r"[\x{100}-\x{200}]",
        r"[\x{70}-\x{200}]",
        r"[[:alpha:]]",
        r"[[:^digit:]]",
        r"[]a]",
        r"[^]a]",
    ] {
        let prog = program(pattern);
        assert_eq!(
            prog.classes.len(),
            prog.class_ascii.len(),
            "{pattern}: the map must be parallel to the classes",
        );
        for (i, set) in prog.classes.iter().enumerate() {
            let bits = prog.class_ascii[i];
            for code in 0u32..128 {
                let c = char::from_u32(code).expect("ASCII is a valid scalar value");
                assert_eq!(
                    bits >> code & 1 != 0,
                    set.matches(c),
                    "{pattern}: class {i} disagrees about {c:?} (U+{code:04X})",
                );
            }
        }
    }
}

#[test]
fn start_anchored_is_exact_about_which_patterns_can_only_match_at_zero() {
    for pattern in [
        r"^a",
        r"^",
        r"^$",
        r"^a$",
        r"\Aa",
        r"\A\Aa",
        r"^a*",
        r"^a|^b",
        r"^(?:a|b)",
        r"^(?:a|ab)+$",
        r"^\d+",
        r"(?i)^ERROR",
    ] {
        assert!(
            program(pattern).start_anchored,
            "{pattern} can only match at offset 0",
        );
    }
    for pattern in [
        r"a|^b",
        r"^a|b",
        r"(^|x)y",
        r"(?:^)|a",
        r"(^a)*",
        r"(?:^a)?b",
        r"(?m)^a",
        r"(?m:^)a",
        r"(?:(?m)^)a",
        r"a",
        r"\bcommit",
        r"",
        r"a$",
    ] {
        assert!(
            !program(pattern).start_anchored,
            "{pattern} can match somewhere other than offset 0",
        );
    }
}

#[test]
fn the_prefilter_byte_map_marks_lead_bytes_only() {
    let mut buf = [0u8; 4];
    for pattern in [
        "abc",
        r"\bcommit",
        "[a-z]+x",
        "foo|bar",
        r"\d+z",
        r"[^\s]x",
        "\u{4f60}\u{597d}",
        "(?i)Hello",
        r"[\w-]+@",
        ".x",
    ] {
        let prog = program(pattern);
        let prefilter = prog.first.as_ref().expect("armed");
        for b in (0x80..=0xc1u8).chain(0xf5..=0xffu8) {
            assert!(
                !prefilter.lead_bytes[b as usize],
                "{pattern:?} marked non-UTF-8-lead byte {b:#04x}"
            );
        }
        assert!(
            !prefilter.pcs.is_empty(),
            "{pattern:?}: armed prefilter must carry consuming pcs"
        );
        let mut checked_any = false;
        for &pc in &prefilter.pcs {
            match prog.insts[pc] {
                Inst::Char { c, .. } => {
                    let lead = c.encode_utf8(&mut buf).as_bytes()[0];
                    assert!(
                        prefilter.lead_bytes[lead as usize],
                        "{pattern:?} pc {pc} consumes {c:?} but lead {lead:#04x} is unmarked"
                    );
                    checked_any = true;
                }
                Inst::Class { class, .. } => {
                    let set = &prog.classes[class];
                    for &(lo, hi) in &set.ranges {
                        for c in [lo, hi] {
                            if set.matches(c) {
                                let lead = c.encode_utf8(&mut buf).as_bytes()[0];
                                assert!(
                                    prefilter.lead_bytes[lead as usize],
                                    "{pattern:?} class {class} accepts {c:?} but lead {lead:#04x} is unmarked"
                                );
                                checked_any = true;
                            }
                        }
                    }
                    for cp in [
                        0u32,
                        b'a'.into(),
                        b'Z'.into(),
                        b'0'.into(),
                        b'-'.into(),
                        0xe9,
                        0x0661,
                        0x4f60,
                        0x1f600,
                    ] {
                        let Some(c) = char::from_u32(cp) else {
                            continue;
                        };
                        if set.matches(c) {
                            let lead = c.encode_utf8(&mut buf).as_bytes()[0];
                            assert!(
                                prefilter.lead_bytes[lead as usize],
                                "{pattern:?} class {class} accepts {c:?} but lead {lead:#04x} is unmarked"
                            );
                            checked_any = true;
                        }
                    }
                }
                other => panic!("{pattern:?}: non-consuming pc {pc} ({other:?}) in prefilter"),
            }
        }
        assert!(
            checked_any,
            "{pattern:?}: prefilter checked zero accepted code points"
        );
    }
}
