// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SGR 38, 48 and 58 in every spelling, on both engines, against answers written out by hand.
//!
//! The comparison cannot see what both engines read the same wrong way, and it could not see a
//! colour-space id taken for red. So each engine is held to these answers rather than to the
//! other (the rule is stated at aterm's `handle_sgr_with_subparams` and `astream_term`'s
//! `Screen::sgr`). The answers for 38 and 48 come from ITU T.416 13.1.8 and xterm's
//! `parse_extended_colors` where those two agree. Where they do not, the answer is this rule's,
//! and it is a choice:
//!
//! - 58 is in neither: xterm's `CASE_SGR` has no `case 58`, and T.416 13.1.8 names 38 and 48. Every
//!   58 answer extends the rule for 38 and 48 to it, and on the fold 58 is only consumed.
//! - All-semicolon CMY and CMYK (`;3;c;m;y`) are not in T.416, and xterm runs their values as
//!   codes. Here they change nothing.
//! - A sub-parameter after an all-semicolon colour (`;5;9:4`) is not in T.416, and xterm runs it
//!   as a code. Here it is skipped.
//! - `4:n` is ignored whole by xterm. Here it is a styled underline, and `4:` is `4:0`.
//! - A short group (`:2:10:20`) names nothing, as in xterm. T.416 says trailing empty elements need
//!   not be included, which would make it id 10, red 20.
//!
//! The values are chosen to show a misreading: 3, the colour-space id, is italic if it reaches
//! the SGR loop; 9, the last value, is crossed-out if it does; a reading off by one gives
//! (3, 10, 20) for (10, 20, 9); and the `;1` after each spelling shows it stopped where it should.

use super::caps::Caps;
use super::engines::{Astream, Aterm, Ink, Paint};
use super::generator;
use std::collections::BTreeMap;

/// What a spelling names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Named {
    Rgb(u8, u8, u8),
    Palette(u8),
    /// No colour: what the pen had stays.
    Nothing,
}
use Named::{Nothing, Palette, Rgb};

const CODES: [u16; 3] = [38, 48, 58];

/// What follows the code. Each is fed at the end of the list and with a `;1` after it: a trailing
/// colon is the one thing the end of the list can change.
const SPELLINGS: &[(&str, Named)] = &[
    // Direct colour. All semicolons carry no id (konsole's variation, which xterm accepts).
    (";2;10;20;9", Rgb(10, 20, 9)),
    // T.416: element 2 is the colour-space id, elements 3 to 5 are red, green and blue.
    (":2::10:20:9", Rgb(10, 20, 9)),
    (":2:3:10:20:9", Rgb(10, 20, 9)),
    // No id slot, which programs emit: three values are the channels.
    (":2:10:20:9", Rgb(10, 20, 9)),
    // After the first colon, colons hold (xterm's ctlseqs): the selector's group, read alike.
    (";2:3:10:20:9", Rgb(10, 20, 9)),
    (";2::10:20:9", Rgb(10, 20, 9)),
    (";2:10:20:9", Rgb(10, 20, 9)),
    // T.416's element 6 (no meaning), 7 (tolerance) and 8 (its colour space) change nothing.
    (":2:3:10:20:9:0:1:1", Rgb(10, 20, 9)),
    (";2:3:10:20:9:0:1:1", Rgb(10, 20, 9)),
    // A channel past 255 clamps; the id is still the first of four.
    (";2:3:10:300:9", Rgb(10, 255, 9)),
    // A trailing colon is an empty element (T.416: "represents a default value"; xterm's
    // CASE_ESC_COLON stores a 0 for it), and the count sees it: it can be the blue of any form, and
    // it makes three values into four.
    (":2::10:20:", Rgb(10, 20, 0)),
    (":2:3:10:20:", Rgb(10, 20, 0)),
    (";2::10:20:", Rgb(10, 20, 0)),
    (";2:3:10:20:", Rgb(10, 20, 0)),
    (":2:10:20:", Rgb(10, 20, 0)),
    (";2:10:20:", Rgb(10, 20, 0)),
    (":2:10:20:9:", Rgb(20, 9, 0)),
    (":5:", Palette(0)),
    (";5:", Palette(0)),
    (":2:", Nothing),
    (":", Nothing),
    (":3:3:10:20:", Nothing),
    // Indexed colour.
    (";5;9", Palette(9)),
    (":5:9", Palette(9)),
    (";5:9", Palette(9)),
    (":5:9:7", Palette(9)),
    (";5:9:7", Palette(9)),
    // CMY and CMYK name a colour nothing here renders: a clean no-op, in each spelling.
    (":3:3:10:20:9", Nothing),
    (":3::10:20:9", Nothing),
    (":4:3:10:20:9:8", Nothing),
    (":4::10:20:9:8", Nothing),
    (";3:3:10:20:9", Nothing),
    (";3::10:20:9", Nothing),
    (";4:3:10:20:9:8", Nothing),
    (";4::10:20:9:8", Nothing),
    (";3;10;20;9", Nothing),
    (";4;10;20;9;8", Nothing),
    // T.416's 0 and 1 take no further elements, and an unknown selector consumes itself.
    (":0", Nothing),
    (":1", Nothing),
    (";0", Nothing),
    (";1", Nothing),
    (";7", Nothing),
    (":7:9:9:9", Nothing),
    (";7:9:9:9", Nothing),
    // A group too short for its selector names nothing, and is still one group.
    (":2:10:20", Nothing),
    (";2:10:20", Nothing),
    (":2", Nothing),
    (":5", Nothing),
    // A sub-parameter is never a code, whatever a semicolon colour's values overlap.
    (";2;10;20;9:4", Rgb(10, 20, 9)),
    (";5;9:4", Palette(9)),
    (";5;9:", Palette(9)),
    (";3;10;20;9:4", Nothing),
    (";4;10;20;9;8:4", Nothing),
];

/// Semicolon spellings cut short, which have no room for a `;1` after them.
const CUT_SHORT: &[&str] = &[
    ";2;10;20",
    ";2;10",
    ";2",
    ";5",
    ";3;10;20",
    ";4;10;20;9",
    "",
];

/// The pen every case starts from, so that "nothing changed" shows.
const PEN: &str = "\x1b[33;44m";
const FG: Paint = Paint::Indexed(3);
const BG: Paint = Paint::Indexed(4);

/// The ink of the first cell after the pen, then `sgr`, then a glyph, read off each engine fed
/// whole and a byte at a time: the parsers have a fast path and a slow one.
fn drawn(sgr: &str) -> [(&'static str, Ink); 4] {
    let bytes = format!("{PEN}\x1b[{sgr}mX").into_bytes();
    let fold = |chunk: usize| {
        let mut a = Astream::new(4, 1);
        for piece in bytes.chunks(chunk) {
            a.feed(piece);
        }
        a.snap().cells[0].ink
    };
    let aterm = |chunk: usize| {
        let mut t = Aterm::new(4, 1);
        for piece in bytes.chunks(chunk) {
            t.feed(piece);
        }
        t.snap().cells[0].ink
    };
    [
        ("the fold", fold(bytes.len())),
        ("the fold, a byte at a time", fold(1)),
        ("aterm", aterm(bytes.len())),
        ("aterm, a byte at a time", aterm(1)),
    ]
}

/// The ink `named` on `code` leaves. The fold has no underline colour and the oracle's ink none
/// either, so on 58 this says only that nothing else moved; `every_58_spelling_…` reads the colour.
fn written(code: u16, named: Named) -> Ink {
    let paint = |kept: Paint| match named {
        Rgb(r, g, b) => Paint::Rgb(r, g, b),
        Palette(n) => Paint::Indexed(n),
        Nothing => kept,
    };
    Ink {
        fg: if code == 38 { paint(FG) } else { FG },
        bg: if code == 48 { paint(BG) } else { BG },
        ..Ink::PLAIN
    }
}

#[test]
fn every_spelling_draws_its_written_answer_on_both_engines() {
    let mut wrong = Vec::new();
    let mut check = |sgr: String, want: Ink| {
        for (engine, got) in drawn(&sgr) {
            if got != want {
                wrong.push(format!("{sgr} on {engine}: {got:?}, not {want:?}"));
            }
        }
    };
    for code in CODES {
        for &(tail, named) in SPELLINGS {
            check(format!("{code}{tail};1"), written(code, named).bold());
            check(format!("{code}{tail}"), written(code, named));
        }
        for tail in CUT_SHORT {
            check(format!("{code}{tail}"), written(code, Nothing));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// 58 on aterm, the one engine that keeps it: over an underline already coloured (1, 2, 3), which
/// a spelling that names nothing leaves, drawn fed whole and a byte at a time.
#[test]
fn every_58_spelling_sets_aterms_underline_colour_to_its_written_answer() {
    const BEFORE: [u8; 3] = [1, 2, 3];
    let mut wrong = Vec::new();
    for &(tail, named) in SPELLINGS {
        for after in ["", ";1"] {
            let bytes = format!("\x1b[4;58:2::1:2:3m\x1b[58{tail}{after}mX").into_bytes();
            for chunk in [bytes.len(), 1] {
                let mut t = Aterm::new(4, 1);
                for piece in bytes.chunks(chunk) {
                    t.feed(piece);
                }
                let want = match named {
                    Rgb(r, g, b) => [r, g, b],
                    Palette(n) => t.palette(n),
                    Nothing => BEFORE,
                };
                let got = t.underline_colour(0, 0);
                if got != Some(want) {
                    wrong.push(format!("58{tail}{after}: {got:?}, not {want:?}"));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// Sub-parameters on a code that takes none are ignored with it, a trailing colon's empty one too
/// (xterm's "unexpected subparameter in SGR", Ghostty's and Alacritty's unknown), on both engines;
/// `4:n` is the one that takes one, and `4:` is `4:0`.
#[test]
fn a_group_on_a_code_that_takes_none_is_ignored_whole() {
    let reversed = |underline| Ink {
        reverse: true,
        underline,
        ..written(0, Nothing)
    };
    for (sgr, want) in [
        ("7;1:4", reversed(false)),
        ("7;3:4:9", reversed(false)),
        ("7;0:1", reversed(false)),
        ("7;22:9", reversed(false)),
        ("7;1:", reversed(false)),
        ("7;0:", reversed(false)),
        ("7;22:", reversed(false)),
        ("7;1:;4", reversed(true)),
        ("7;4:3:9", reversed(true)),
        ("7;4;4:", reversed(false)),
        ("7;4;4:;1", reversed(false).bold()),
    ] {
        for (engine, got) in drawn(sgr) {
            assert_eq!(got, want, "{sgr} on {engine}");
        }
    }
}

/// aterm's parser keeps 24 parameters and the fold 128 bytes, so past 24 they differ, and the
/// oracle excludes it (`sgr_past_the_parameter_cap`). What aterm must not do there is misread: a
/// group the cap cut is short of what it named, and no reading makes the id a channel. A group the
/// cap only cut past the end of its colour still draws it, and one that ends at the cap is whole.
#[test]
fn a_colour_the_parameter_cap_cuts_is_never_misread() {
    for (lead, tail, named) in [
        // The blue fell off: four values left would read as three channels.
        (19, ":2:3:10:20:9", Nothing),
        (19, ";2:3:10:20:9", Nothing),
        (19, ":2::10:20:9", Nothing),
        (19, ";2::10:20:9", Nothing),
        (19, ":2:3:10:20:", Nothing),
        (19, ":2:3:10:20:9:0:1:1", Nothing),
        // Shorter still names nothing by the count.
        (20, ":2:3:10:20:9", Nothing),
        (21, ":2:3:10:20:9", Nothing),
        (22, ":2:3:10:20:9", Nothing),
        (23, ":2:3:10:20:9", Nothing),
        (22, ";5:9", Nothing),
        // It ends at the cap: nothing was cut.
        (18, ":2:3:10:20:9", Rgb(10, 20, 9)),
        (18, ";2:3:10:20:9", Rgb(10, 20, 9)),
        (19, ":2:10:20:9", Rgb(10, 20, 9)),
        (19, ";2:10:20:9", Rgb(10, 20, 9)),
        (19, ";2;10;20;9", Rgb(10, 20, 9)),
        (21, ":5:9", Palette(9)),
        // Cut past the colour's end: what is left names it whole.
        (18, ":2:3:10:20:9:0:1:1", Rgb(10, 20, 9)),
        (21, ":5:9:7", Palette(9)),
    ] {
        let list = "7;".repeat(lead);
        for code in CODES {
            let bytes = format!("\x1b[33;44m\x1b[4;58:2::1:2:3m\x1b[{list}{code}{tail}mX");
            for chunk in [bytes.len(), 1] {
                let mut t = Aterm::new(4, 1);
                for piece in bytes.as_bytes().chunks(chunk) {
                    t.feed(piece);
                }
                let want = Ink {
                    reverse: true,
                    underline: true,
                    ..written(code, named)
                };
                let got = t.snap().cells[0].ink;
                assert_eq!(got, want, "{lead} parameters, then {code}{tail}");
                let underline = match (code, named) {
                    (58, Rgb(r, g, b)) => [r, g, b],
                    (58, Palette(n)) => t.palette(n),
                    _ => [1, 2, 3],
                };
                assert_eq!(
                    t.underline_colour(0, 0),
                    Some(underline),
                    "{lead} parameters, then {code}{tail}: the underline colour"
                );
            }
        }
    }
}

/// Negative control: the readings these answers replaced, each one spelled in a way the live
/// engines still read so, are rejected. (Both engines read `38;2:3:10:20:9` as `38;2;3;10;20;9`;
/// aterm read `38;3;10;20;9` as an unknown selector; the fold ran `58;5;9`'s arguments as codes.)
#[test]
fn the_written_answers_reject_the_readings_they_replaced() {
    for (code, tail, read_as) in [
        (38, ";2:3:10:20:9", "38;2;3;10;20;9"),
        (48, ";2:3:10:20:9", "48;2;3;10;20;9"),
        (38, ";2::10:20:9", "38;2;0;10;20;9"),
        (38, ";3;10;20;9", "38;7;10;20;9"),
        (48, ";4;10;20;9;8", "48;7;10;20;9;8"),
        (58, ";5;9", "5;9"),
        (58, ";2;10;20;9", "2;10;20;9"),
    ] {
        let named = SPELLINGS
            .iter()
            .find(|row| row.0 == tail)
            .unwrap_or_else(|| panic!("no row {tail}"))
            .1;
        let want = written(code, named).bold();
        for (engine, got) in drawn(&format!("{read_as};1")) {
            assert_ne!(got, want, "{code}{tail}: the old reading on {engine}");
        }
    }
}

/// The spelling of each extended colour in an SGR's parameters, as the code, the separator before
/// the selector, the selector and the separator after it (none for a group that stops at its
/// selector), then what its values do: whether a direct colour's leave room for an id, whether they
/// run past the colour's end, whether a trailing colon leaves the group open, whether a semicolon
/// form has its last value carry a sub-parameter or runs out of list. A code that takes no colon
/// group but has one counts under `{code} group {elements}`, or `{code} group open` for `{code}:`.
fn spellings_in(params: &str, seen: &mut BTreeMap<String, u64>) {
    let mut see = |key: String| *seen.entry(key).or_default() += 1;
    if generator::sgr_params(params) == generator::SGR_PARAMS {
        see("at the cap".to_string());
    }
    let parts: Vec<&str> = params.split(';').collect();
    let mut i = 0;
    while let Some(part) = parts.get(i).copied() {
        let (code, group) = match part.split_once(':') {
            Some((code, group)) => (code, Some(group)),
            None => (part, None),
        };
        i += 1;
        if !matches!(code, "38" | "48" | "58") {
            match group {
                Some("") if i == parts.len() => see(format!("{code} group open last")),
                Some("") => see(format!("{code} group open")),
                Some(group) => see(format!("{code} group {}", group.split(':').count())),
                None => {}
            }
            continue;
        }
        let (first, rest) = match group {
            Some(group) => (':', group),
            None => {
                let Some(next) = parts.get(i).copied() else {
                    see(format!("{code} alone"));
                    continue;
                };
                i += 1;
                (';', next)
            }
        };
        let (selector, second, values) = match rest.split_once(':') {
            Some((selector, values)) => (selector, ":", values.split(':').count()),
            None if first == ':' => (rest, "", 0),
            None => (rest, ";", 0),
        };
        let mut room = match (selector, second, values) {
            // Only the end of the list shows a trailing colon: with more after it, the next
            // separator pushes the empty element whatever the parser does at the final byte.
            (_, ":", _) if rest.ends_with(':') && i == parts.len() => " open last",
            (_, ":", _) if rest.ends_with(':') => " open",
            ("2", ":", ..3) => " short",
            ("2", ":", 3) => " no id",
            ("2", ":", 4) if rest.starts_with("2::") => " empty id",
            ("2", ":", 4) => " id",
            ("2", ":", _) => " past the blue",
            ("5", ":", 2..) => " past the index",
            _ => "",
        };
        if second == ";" {
            // All semicolons: the values follow as parameters.
            let takes = match selector {
                "2" | "3" => 3,
                "4" => 4,
                "5" => 1,
                _ => 0,
            };
            if i + takes > parts.len() {
                room = " cut short";
            } else if takes > 0 && parts[i + takes - 1].contains(':') {
                room = " carrying one";
            }
            i += takes;
        }
        see(format!("{code}{first}{selector}{second}{room}"));
    }
}

/// The generator reaches every spelling on every code, and every group on a code that takes none,
/// so random programs compare them.
#[test]
fn the_generated_colours_reach_every_spelling() {
    let caps = Caps::probe();
    assert!(caps.truecolour, "{caps:?}");
    let mut seen = BTreeMap::new();
    for seed in 0..20_000 {
        let bytes = generator::program(seed, caps).bytes();
        let text = String::from_utf8_lossy(&bytes);
        for piece in text.split("\x1b[").skip(1) {
            if let Some((params, _)) = piece.split_once('m')
                && params
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b":;".contains(&b))
            {
                spellings_in(params, &mut seen);
            }
        }
    }
    let mut short = Vec::new();
    let mut check = |key: String, floor: u64| {
        let got = seen.get(&key).copied().unwrap_or(0);
        if got < floor {
            short.push(format!("{key}: {got} < {floor}"));
        }
    };
    for code in CODES {
        for (spelling, floor) in FLOORS {
            check(format!("{code}{spelling}"), *floor);
        }
    }
    for (spelling, floor) in OTHER_FLOORS {
        check((*spelling).to_string(), *floor);
    }
    assert!(short.is_empty(), "{short:?}\nreached: {seen:?}");
}

/// About a third of the fewest any code reached over these seeds when the floors were set.
const FLOORS: &[(&str, u64)] = &[
    (";2;", 41),
    (";2; carrying one", 33),
    (";2; cut short", 91),
    (":2: no id", 28),
    (":2: empty id", 17),
    (":2: id", 17),
    (":2: short", 28),
    (":2: past the blue", 15),
    (":2: open", 8),
    (":2: open last", 4),
    (":2", 27),
    (";2: no id", 16),
    (";2: empty id", 16),
    (";2: id", 27),
    (";2: short", 24),
    (";2: past the blue", 14),
    (";2: open", 9),
    (";2: open last", 4),
    (";5;", 23),
    (";5; carrying one", 18),
    (";5; cut short", 31),
    (":5:", 34),
    (":5: past the index", 10),
    (":5: open", 6),
    (":5: open last", 5),
    (":5", 28),
    (";5:", 21),
    (";5: past the index", 4),
    (";5: open", 5),
    (";5: open last", 3),
    (";3;", 13),
    (";3; carrying one", 13),
    (";3; cut short", 29),
    (":3:", 12),
    (";3:", 13),
    (";4;", 13),
    (";4; carrying one", 13),
    (";4; cut short", 32),
    (":4:", 13),
    (";4:", 13),
    (":7:", 26),
    (";7:", 26),
    (":0", 7),
    (":1", 7),
    (":7", 6),
    (":", 6),
    (";0;", 8),
    (";1;", 10),
    (";7;", 9),
    (" alone", 32),
];

/// The same, for what is not per colour code: a group on a code that takes none, 4's, and a list
/// that ends at aterm's cap, so a colon group's last value is the parser's last.
const OTHER_FLOORS: &[(&str, u64)] = &[
    ("1 group 1", 135),
    ("3 group 2", 63),
    ("0 group 1", 60),
    ("22 group 1", 66),
    ("4 group 2", 60),
    ("1 group open", 10),
    ("1 group open last", 6),
    ("3 group open", 10),
    ("3 group open last", 10),
    ("0 group open", 10),
    ("0 group open last", 8),
    ("22 group open", 7),
    ("22 group open last", 10),
    ("4 group open", 33),
    ("4 group open last", 32),
    ("at the cap", 143),
];
