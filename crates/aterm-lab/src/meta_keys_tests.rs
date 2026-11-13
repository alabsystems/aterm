// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::HashMap;

use super::*;

#[test]
fn a_case_reads_its_own_line_without_the_enter() {
    let screen = "key-dump ready\n#1: c3 a9 0d\n#2: 1b 62 0d\n#12: 1b 65 0d\n";
    assert_eq!(case_bytes(screen, 1).as_deref(), Some("c3 a9"));
    assert_eq!(case_bytes(screen, 2).as_deref(), Some("1b 62"));
    assert_eq!(case_bytes(screen, 12).as_deref(), Some("1b 65"));
    assert_eq!(case_bytes(screen, 3), None);
}

#[test]
fn the_wanted_bytes_print_as_the_dump_prints_them() {
    assert_eq!(hex(b"\x1bb"), "1b 62");
    assert_eq!(hex("∫".as_bytes()), "e2 88 ab");
    assert_eq!(hex("é".as_bytes()), "c3 a9");
}

/// A layout as a table: `(key code, mods) -> text`, a dead key as `dead:N`,
/// and `(N, key code) -> text` for what a key types after dead key N.
#[derive(Default)]
struct Table {
    keys: HashMap<(u16, u32), &'static str>,
    after: HashMap<(u32, u16), &'static str>,
}

impl Table {
    fn key(mut self, code: u16, mods: u32, text: &'static str) -> Table {
        self.keys.insert((code, mods), text);
        self
    }
    fn after(mut self, dead: u32, code: u16, text: &'static str) -> Table {
        self.after.insert((dead, code), text);
        self
    }
}

impl Keys for Table {
    fn press(&self, code: u16, mods: u32, dead: &mut u32) -> Option<String> {
        if *dead != 0 {
            let d = std::mem::take(dead);
            return self.after.get(&(d, code)).map(|s| (*s).to_string());
        }
        let text = self.keys.get(&(code, mods))?;
        if let Some(n) = text.strip_prefix("dead:") {
            *dead = n.parse().ok()?;
            return Some(String::new());
        }
        Some((*text).to_string())
    }
}

/// The U.S. keys these tests use: a (0), 1 (18), 2 (19), [ (33), e (14),
/// u (32), ; (41) and b (11).
fn us() -> Table {
    Table::default()
        .key(0, 0, "a")
        .key(0, SHIFT, "A")
        .key(0, OPTION, "å")
        .key(11, 0, "b")
        .key(11, OPTION, "∫")
        .key(14, 0, "e")
        .key(14, OPTION, "dead:1")
        .key(18, 0, "1")
        .key(18, SHIFT, "!")
        .key(19, 0, "2")
        .key(19, SHIFT, "@")
        .key(32, 0, "u")
        .key(32, OPTION, "dead:2")
        .key(33, 0, "[")
        .key(41, 0, ";")
        .key(SPACE, 0, " ")
        .after(1, SPACE, "´")
        .after(1, 0, "á")
        .after(1, 14, "é")
        .after(2, SPACE, "¨")
        .after(2, 0, "ä")
}

fn whats(cases: &[Case]) -> Vec<&str> {
    cases.iter().map(|c| c.what.as_str()).collect()
}

/// On the U.S. layout the rows are the measured ones, read off the layout: each
/// Option dead key as ESC and its letter, the control's composition and Option chord.
#[test]
fn the_us_rows_come_from_the_us_layout() {
    let r = rows(&us(), &us());
    assert_eq!(
        whats(&r.control),
        ["Option+e (dead ´) then a composes á", "Option+a types å",]
    );
    assert_eq!(r.control[0].keys, ["e mods=alt", "a"]);
    assert_eq!(r.control[0].want, "á".as_bytes());
    assert_eq!(
        whats(&r.meta),
        [
            "Option+e (dead ´) goes out as ESC e",
            "Option+u (dead ¨) goes out as ESC u",
            "Option+e then a is ESC e a: no dead key pending",
            "Option+a goes out as ESC a, not å",
            "Option+Shift+1 goes out as ESC !",
        ]
    );
    assert_eq!(r.meta[2].want, b"\x1bea");
    assert_eq!(r.meta[4].keys, ["1 mods=alt+shift"]);
}

/// A French-shaped layout: a bare dead `^` on the U.S. `[` key, `é` on the `2`
/// key, `&` with Shift `1` on the `1` key, an Option dead key on `&`, and `{`
/// under Option on the U.S. `;` key.
fn fr() -> Table {
    Table::default()
        .key(0, 0, "q")
        .key(0, SHIFT, "Q")
        .key(0, OPTION, "‡")
        .key(14, 0, "e")
        .key(14, OPTION, "ê")
        .key(18, 0, "&")
        .key(18, SHIFT, "1")
        .key(18, OPTION, "dead:3")
        .key(19, 0, "é")
        .key(19, SHIFT, "2")
        .key(19, OPTION, "ë")
        .key(33, 0, "dead:4")
        .key(41, 0, "m")
        .key(41, OPTION, "{")
        .key(SPACE, 0, " ")
        .after(3, SPACE, "´")
        .after(3, 14, "é")
        .after(4, SPACE, "^")
        .after(4, 14, "ê")
}

/// A foreign layout's rows press the U.S. names of its keys and want the
/// layout's own characters.
#[test]
fn a_foreign_layouts_rows_want_its_own_characters() {
    let r = rows(&fr(), &us());
    assert_eq!(
        whats(&r.control),
        [
            "Option+& (dead ´) then e composes é",
            "Option+q types ‡",
            "Option+m types {",
        ]
    );
    assert_eq!(r.control[0].keys, ["1 mods=alt", "e"]);
    assert_eq!(
        r.control[2].keys,
        ["; mods=alt"],
        "the U.S. name of the key"
    );
    assert_eq!(
        whats(&r.meta),
        [
            "Option+& (dead ´) goes out as ESC &",
            "Option+& then e is ESC & e: no dead key pending",
            "Option+q goes out as ESC q, not ‡",
            "Option+m types {: only Option reaches it",
            "Option+Shift+& goes out as ESC 1",
            "Option+é goes out as ESC é",
            "^ (a dead key, no Option) then e composes ê",
        ]
    );
    assert_eq!(r.meta[3].want, b"{", "no ESC: the layout's own character");
    assert_eq!(r.meta[4].want, b"\x1b1");
    assert_eq!(r.meta[5].want, "\x1bé".as_bytes());
    assert_eq!(r.meta[6].keys, ["[", "e"]);
    assert_eq!(r.meta[6].want, "ê".as_bytes());
}

/// A German-shaped layout: `@` only under Option on the U.S. `l` key, the tilde
/// dead key (its `~` reached no other way) under Option on `n`, and `'` under
/// Option on the U.S. `\` key, whose Shift types `'` too.
fn de() -> Table {
    Table::default()
        .key(0, 0, "a")
        .key(0, OPTION, "å")
        .key(37, 0, "l")
        .key(37, OPTION, "@")
        .key(45, 0, "n")
        .key(45, OPTION, "dead:5")
        .key(42, 0, "#")
        .key(42, SHIFT, "'")
        .key(42, OPTION, "'")
        .key(SPACE, 0, " ")
        .after(5, SPACE, "~")
        .after(5, 0, "ã")
}

/// The U.S. names of the German-shaped keys: a (0), \\ (42), l (37) and n (45).
fn us_de() -> Table {
    Table::default()
        .key(0, 0, "a")
        .key(37, 0, "l")
        .key(42, 0, "\\")
        .key(45, 0, "n")
        .key(SPACE, 0, " ")
}

/// Meta mode types a character only Option reaches, a dead key's accent too,
/// and nothing is left pending after it; an ASCII Option character a key also
/// types without Option is still ESC and the key.
#[test]
fn a_character_only_option_reaches_is_typed_in_meta_mode() {
    let r = rows(&de(), &us_de());
    assert_eq!(
        whats(&r.control),
        [
            "Option+n (dead ~) then a composes ã",
            "Option+a types å",
            "Option+l types @",
            "Option+# types '",
        ]
    );
    assert_eq!(
        whats(&r.meta),
        [
            "Option+n (dead ~) types ~: only Option reaches it",
            "Option+n then a is ~ a: only Option reaches ~, no dead key pending",
            "Option+a goes out as ESC a, not å",
            "Option+l types @: only Option reaches it",
            "Option+# goes out as ESC #, not '",
            "Option+Shift+# goes out as ESC '",
        ]
    );
    assert_eq!(r.meta[0].want, b"~");
    assert_eq!(r.meta[1].want, b"~a");
    assert_eq!(r.meta[3].keys, ["l mods=alt"]);
    assert_eq!(r.meta[3].want, b"@");
    assert_eq!(r.meta[4].keys, ["\\ mods=alt"]);
    assert_eq!(r.meta[4].want, b"\x1b#");
}

/// Every Meta row but a bare dead key's and a character only Option reaches
/// wants ESC first, and no control row wants an ESC: the two cannot both pass
/// on the same bytes.
#[test]
fn meta_wants_an_escape_and_the_control_never_does() {
    for r in [
        rows(&us(), &us()),
        rows(&fr(), &us()),
        rows(&de(), &us_de()),
    ] {
        for case in &r.meta {
            if case.what.contains("no Option") || case.what.contains("only Option") {
                continue;
            }
            assert_eq!(case.want[0], 0x1b, "{}", case.what);
        }
        for case in &r.control {
            assert!(!case.want.contains(&0x1b), "{}", case.what);
        }
    }
}

/// A key the U.S. layout types no printable ASCII on (here `§`) cannot be named to
/// `hwkey`, so it makes no row.
#[test]
fn a_key_with_no_us_name_makes_no_row() {
    let only = Table::default().key(10, 0, "§").key(10, OPTION, "dead:1");
    let us_without = Table::default().key(10, 0, "§");
    let r = rows(&only, &us_without);
    assert!(r.control.is_empty() && r.meta.is_empty());
}
