// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn ast(pattern: &str) -> Ast {
    parse(pattern, Flags::default()).expect("pattern parses")
}

fn message(pattern: &str) -> String {
    match parse(pattern, Flags::default()) {
        Err(Error::Syntax(msg)) => msg,
        other => panic!("expected a syntax error for {pattern:?}, got {other:?}"),
    }
}

/// The rendered error quotes the pattern and puts a caret under the
/// offending position (column, not wording, is what this pins).
#[test]
fn errors_quote_the_pattern_and_point_at_the_position() {
    for (pattern, column) in [("ab[cd", 2usize), ("a(b", 1)] {
        let msg = message(pattern);
        let mut lines = msg.lines().skip(1);
        let quoted = lines.next().expect("the pattern line");
        let caret = lines.next().expect("the caret line");
        let indent = quoted.len() - pattern.len();
        assert_eq!(&quoted[indent..], pattern, "{msg}");
        assert_eq!(caret.find('^'), Some(indent + column), "{msg}");
    }
}

/// POSIX names are ASCII-only, and an unknown name is not silently read as a
/// nested class full of colons.
#[test]
fn posix_classes_are_ascii_and_closed() {
    let Ast::Class(set) = ast("[[:alpha:]]") else {
        panic!("a class")
    };
    assert!(set.matches('a') && set.matches('Z') && !set.matches('\u{e9}'));
    let Ast::Class(set) = ast("[[:^digit:]]") else {
        panic!("a class")
    };
    assert!(set.matches('x') && !set.matches('4'));
    assert!(message("[[:nosuch:]]").contains("nested character classes"));
    let Ast::Concat(items) = ast("(?x)[0 - # trailing dash\n ]a") else {
        panic!("a concat")
    };
    let Ast::Class(trailing_dash) = &items[0] else {
        panic!("a class")
    };
    assert!(
        trailing_dash.matches('0') && trailing_dash.matches('-') && !trailing_dash.matches(']')
    );
    let Ast::Class(shorthand_dash) = ast(r"(?x)[\d - ]") else {
        panic!("a class")
    };
    assert!(shorthand_dash.matches('7') && shorthand_dash.matches('-'));
    assert!(message(r"(?x)[\d - a]").contains("invalid range boundary"));
    assert!(message("(?x)[[:alpha:] - a]").contains("invalid range boundary"));
    assert!(message(r"[\d--a]").contains("set operations"));
    assert!(message("[[:alpha:]--a]").contains("set operations"));
}

/// An item's own `^` is the exact complement of its folded table, over every code
/// point a POSIX table or its fold reaches and the edges of the scalar-value space.
#[test]
fn a_negated_posix_item_is_the_complement_of_its_folded_table() {
    let names = [
        "alnum", "alpha", "ascii", "blank", "cntrl", "digit", "graph", "lower", "print", "punct",
        "space", "upper", "word", "xdigit",
    ];
    let edges = [0xd7ff, 0xe000, 0x10_ffff];
    for name in names {
        let table = posix_class(name).expect("a known name");
        for case_insensitive in [false, true] {
            let (mut yes, mut no) = (ClassSet::default(), ClassSet::default());
            yes.add_posix(table, false, case_insensitive);
            no.add_posix(table, true, case_insensitive);
            yes.normalize();
            no.normalize();
            for c in (0..=0x2200u32).chain(edges).filter_map(char::from_u32) {
                assert_ne!(
                    yes.matches(c),
                    no.matches(c),
                    "[:{name}:] and [:^{name}:] (case-insensitive: {case_insensitive}) \
                     agree about U+{:04X}",
                    u32::from(c)
                );
            }
        }
    }
    let mut no = ClassSet::default();
    no.add_posix(posix_class("lower").expect("known"), true, true);
    for c in ['a', 'Z', '\u{17f}', '\u{212a}'] {
        assert!(!no.matches(c), "(?i)[:^lower:] must not hold {c:?}");
    }
    assert!(no.matches('1') && no.matches('\u{e9}'));
}

/// Whitespace inside `{…}` is insignificant even outside `x` mode.
#[test]
fn counted_repetition_tolerates_spaces() {
    assert!(matches!(
        ast("a{ 2 , 3 }"),
        Ast::Repeat {
            min: 2,
            max: Some(3),
            ..
        }
    ));
    assert!(matches!(
        ast("a{2,}"),
        Ast::Repeat {
            min: 2,
            max: None,
            ..
        }
    ));
    assert!(matches!(
        ast("a{2}?"),
        Ast::Repeat {
            min: 2,
            max: Some(2),
            greedy: false,
            ..
        }
    ));
}

/// `(?U)` swaps greediness, and an explicit `?` swaps it back.
#[test]
fn swap_greed_inverts_and_composes() {
    let u = Flags {
        swap_greed: true,
        ..Flags::default()
    };
    assert!(matches!(
        parse("a*", u),
        Ok(Ast::Repeat { greedy: false, .. })
    ));
    assert!(matches!(
        parse("a*?", u),
        Ok(Ast::Repeat { greedy: true, .. })
    ));
}

/// A class costs bytes, and those bytes are charged against the compile
/// ceiling — so the accounting has to see them.
#[test]
fn classes_report_their_size() {
    let Ast::Class(small) = ast("[a]") else {
        panic!("a class")
    };
    let Ast::Class(big) = ast("[a-cx-zA-F0-9_]") else {
        panic!("a class")
    };
    assert!(big.byte_size() > small.byte_size());
    assert!(small.byte_size() >= size_of::<ClassSet>());
}
