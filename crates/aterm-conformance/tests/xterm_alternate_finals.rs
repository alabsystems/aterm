// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Operations xterm reaches under a SECOND final byte.
//
// Each sequence here names an operation the engine already implements under its
// primary spelling; the alternate spelling is not a different feature but the
// same one an emitter may legitimately choose, so the two must be
// indistinguishable from the screen. A terminal that recognizes only the
// primary spelling looks, to the program that picked the other one, exactly like
// a terminal that dropped the sequence — silently, with no error to notice.
//
// Every expectation is quoted from xterm's ctlseqs ("XTerm Control Sequences").

use aterm_conformance::Screen;

/// Feed `input` to a fresh screen prefilled with one glyph per row, and return
/// the visible rows.
///
/// Four rows of distinct single characters make a vertical scroll readable as a
/// plain string comparison.
fn scrolled(input: &[u8]) -> Vec<String> {
    let mut s = Screen::new(4, 6);
    s.feed(b"A\r\nB\r\nC\r\nD");
    s.feed(input);
    (0..4).map(|r| s.row(r)).collect()
}

// =========================================================================
// SD — Scroll Down, the ECMA-48 spelling (CSI Ps ^)
//
// xterm ctlseqs: "CSI Ps ^  Scroll down Ps lines (default = 1) (SD), ECMA-48.
// This was a publication error in the original ECMA-48 5th edition (1991)
// corrected in 2003."
//
// The engine's primary spelling is the VT420 "CSI Ps T".
// =========================================================================

#[test]
fn sd_spelled_with_caret_scrolls_down_like_sd_spelled_with_t() {
    assert_eq!(
        scrolled(b"\x1b[2^"),
        scrolled(b"\x1b[2T"),
        "CSI Ps ^ and CSI Ps T are one operation, so one screen"
    );
    assert_eq!(
        scrolled(b"\x1b[2^"),
        vec!["", "", "A", "B"],
        "two lines of blank scrolled in at the top, D pushed off the bottom"
    );
}

#[test]
fn sd_spelled_with_caret_defaults_to_one_line() {
    // "Scroll down Ps lines (default = 1)".
    assert_eq!(scrolled(b"\x1b[^"), vec!["", "A", "B", "C"]);
    assert_eq!(
        scrolled(b"\x1b[0^"),
        scrolled(b"\x1b[^"),
        "Ps = 0 takes the default of 1, as it does for CSI Ps T"
    );
}

#[test]
fn sd_spelled_with_caret_scrolls_only_inside_the_scroll_region() {
    // DECSTBM bounds SD under either spelling: the alternate final routes to
    // the same handler, so it cannot forget the margins.
    let mut s = Screen::new(5, 6);
    s.feed(b"1\r\n2\r\n3\r\n4\r\n5");
    s.feed(b"\x1b[2;4r\x1b[1^");
    let rows: Vec<String> = (0..5).map(|r| s.row(r)).collect();
    assert_eq!(
        rows,
        vec!["1", "", "2", "3", "5"],
        "rows 2-4 scroll down; rows 1 and 5 are outside the region"
    );
}

// =========================================================================
// XTPUSHSGR / XTPOPSGR spelled `# p` / `# q`
//
// xterm ctlseqs: "CSI # p / CSI Pm # p  Push video attributes onto stack
// (XTPUSHSGR), xterm. This is an alias for CSI # { , used to work around
// language limitations of C#." and "CSI # q  Pop video attributes from stack
// (XTPOPSGR), xterm. This is an alias for CSI # } ".
//
// An unrecognized push is worse than a no-op: the program's later pop is then
// unbalanced, so every attribute it set after the push leaks past the point it
// meant to restore.
// =========================================================================

/// Style fingerprint after feeding `input` — `(fg, bg, flags)`.
fn style_after(input: &[u8]) -> (u32, u32, u16) {
    let mut s = Screen::new(2, 20);
    s.feed(input);
    s.style_fingerprint()
}

#[test]
fn xtpushsgr_spelled_hash_p_restores_the_same_style_as_hash_brace() {
    // Bold, push, reset everything, pop: the bold must come back.
    let aliased = style_after(b"\x1b[1m\x1b[#p\x1b[0m\x1b[#q");
    let primary = style_after(b"\x1b[1m\x1b[#{\x1b[0m\x1b[#}");
    assert_eq!(aliased, primary, "the alias is the same operation");
    assert_ne!(
        aliased,
        style_after(b"\x1b[1m\x1b[0m"),
        "precondition: the pop really restored something"
    );
}

#[test]
fn xtpushsgr_spelled_hash_p_takes_the_same_pm_selection_as_hash_brace() {
    // "CSI Pm # p" — the parameterized form pushes only the named attributes,
    // so a pop restores those and leaves the rest at whatever the stream set.
    // Bold + underline, push only bold (Pm = 1), clear both, pop.
    let aliased = style_after(b"\x1b[1;4m\x1b[1#p\x1b[0m\x1b[#q");
    let primary = style_after(b"\x1b[1;4m\x1b[1#{\x1b[0m\x1b[#}");
    assert_eq!(aliased, primary, "Pm is read identically under both finals");
    // The selection has to actually bite, or the comparison above would hold
    // for an alias that threw its parameters away: pushing only bold must NOT
    // bring the underline back the way an unparameterized push does.
    assert_ne!(
        aliased,
        style_after(b"\x1b[1;4m\x1b[#p\x1b[0m\x1b[#q"),
        "Pm = 1 restores bold alone; the bare push restores bold and underline"
    );
}

#[test]
fn xtpopsgr_spelled_hash_q_pops_a_stack_pushed_with_hash_brace() {
    // The two spellings share one stack — an emitter that mixes them (or a
    // program piped through one that rewrites only one spelling) must not see
    // a push and a pop land on different stacks.
    let mixed = style_after(b"\x1b[1m\x1b[#{\x1b[0m\x1b[#q");
    let primary = style_after(b"\x1b[1m\x1b[#{\x1b[0m\x1b[#}");
    assert_eq!(mixed, primary, "one stack, reachable under either spelling");
}

#[test]
fn unbalanced_alias_pops_are_ignored_without_disturbing_the_style() {
    // A pop with nothing pushed must not invent a style. xterm's stack is
    // bounded; over-popping is a no-op, not an underflow.
    let over_popped = style_after(b"\x1b[1m\x1b[#q\x1b[#q\x1b[#q");
    assert_eq!(
        over_popped,
        style_after(b"\x1b[1m"),
        "pops with an empty stack leave the current style alone"
    );
}
