// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn complete_cell_styles_keep_dim_and_resolved_colours() {
    assert_eq!(
        parse_style("OK  999999 111318 dim,wide_cont\n"),
        Some(Style {
            fg: "999999",
            bg: "111318",
            dim: true,
            inverse: false,
            wide: false,
        })
    );
    let plain =
        parse_style("OK %E2%9D%AF d0d0d0 111318 bold,underline link=https%3A%2F%2Fexample.test\n")
            .expect("actual complete shape");
    assert!(!plain.dim && !plain.inverse && !plain.wide);
    assert_eq!(plain.fg, "d0d0d0");
    assert!(parse_style("OK x 8A8F99 111318 none\r\n").is_some());
}

/// Claude Code 2.1.310's drawn caret (`inverse`) over a narrow and a wide glyph, read from the
/// one attribute set. NEGATIVE CONTROLS: a wide glyph's continuation is not its leading cell,
/// and a plain or dim cell is no caret.
#[test]
fn a_cell_style_says_inverse_and_wide() {
    let caret = parse_style("OK k 111318 d0d0d0 inverse").expect("a caret");
    assert!(caret.inverse && !caret.wide && !caret.dim);
    let wide = parse_style("OK %E4%BD%A0 111318 d0d0d0 bold,inverse,wide").expect("wide");
    assert!(wide.inverse && wide.wide);
    let cont = parse_style("OK  111318 d0d0d0 inverse,wide_cont").expect("continuation");
    assert!(cont.inverse && !cont.wide);
    for plain in ["OK e 999999 111318 dim", "OK k d0d0d0 111318 none"] {
        let style = parse_style(plain).expect("a plain cell");
        assert!(!style.inverse && !style.wide, "{plain}");
    }
}

#[test]
fn incomplete_or_unknown_cell_styles_prove_neither_draft_nor_placeholder() {
    for raw in [
        "ERR no such verb",
        "OK x d0d0d0 111318",
        "OK x d0d0d0 111318 ",
        "OK x  111318 none",
        "OK x d0d0d0  none",
        "OK x nothex 111318 none",
        "OK x d0d0d0 - none",
        "OK x d0d0d0 111318 unrecognized",
        "OK x d0d0d0 111318 none unexpected",
        "OK x d0d0d0 111318 dim,,bold",
        "OK x d0d0d0 111318 dim,dim",
        "OK x d0d0d0 111318 none,bold",
        "OK x d0d0d0 111318 wide,wide_cont",
        "OK x d0d0d0 111318 none link=a link=b",
        "OK %zz d0d0d0 111318 dim",
        "OK x d0d0d0 111318 none\nOK x d0d0d0 111318 dim\n",
    ] {
        assert_eq!(parse_style(raw), None, "{raw}");
    }
}

#[test]
fn a_cell_of_an_unfinished_frame_proves_nothing() {
    for whole in [
        "OK x d0d0d0 111318 none",
        "OK  999999 111318 dim,wide_cont",
        "OK %E2%8F%BA e5e5e5 000000 bold,wide",
        "OK %E2%9D%AF d0d0d0 111318 bold,underline link=https%3A%2F%2Fexample.test",
    ] {
        assert!(parse_style(&format!("{whole}\n")).is_some(), "{whole}");
        let open = format!("{whole} frame=open\n");
        assert_eq!(parse_style(&open), None, "{open}");
    }
}
