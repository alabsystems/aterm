// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`blanks_read_as_drawn`], and the barred Bash boxes Claude Code 2.1.307
//! and 2.1.308 really drew (MEASURED 2026-10-22 in the real-Claude lab,
//! `docs/measured/breaker-transcript-and-zsh-2026-10-22.md`): each box
//! capture `claude-2.1.30[78]-box-bash-*`, which the approval policy's tests
//! decide on the screen's readings alone
//! (`approval::tests::every_measured_barred_box_is_judged_by_its_screen_alone`).

use aterm_phase::PromptV2;

use super::*;

/// Each capture's name and screen: `Probe <name>`'s box.
pub(crate) const SCREENS: &[(&str, &str)] = &[
    ("too-many-newline-twin", TWIN_SCREEN),
    (
        "wrap-long-word",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-long-word.txt"),
    ),
    (
        "wrap-spaces-at-wrap",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-spaces-at-wrap.txt"),
    ),
    (
        "wrap-newlines",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-newlines.txt"),
    ),
    (
        "wrap-word-of-width",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-word-of-width.txt"),
    ),
    (
        "wrap-word-past-width",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-word-past-width.txt"),
    ),
    (
        "wrap-row-139",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-row-139.txt"),
    ),
    (
        "wrap-edge-spaces",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-edge-spaces.txt"),
    ),
    (
        "wrap-100-columns",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-100-columns.txt"),
    ),
];

/// The newline twin's own box: the measured too-many box's rows, under the
/// twin's call's row.
pub(crate) const TWIN_SCREEN: &str =
    include_str!("fixtures/claude-2.1.307-box-bash-too-many-newline-twin.txt");

/// The tabs capture: a tab drawn to the next stop of eight.
pub(crate) const TABS: &str = include_str!("fixtures/claude-2.1.307-box-bash-wrap-tabs.txt");

/// Each 2.1.308 capture's name and screen: `Probe <name>`'s box, under its
/// call's row (MEASURED 2026-10-22, the measured note's §H and §W): the
/// call's row's width, cut and trim (`h-…`), tabs, CJK, kana, Hangul and
/// fullwidth forms, and both ends of every wide block, in the box
/// (`w-ranges`) and in the call's row (`w-ranges` for the two CJK blocks,
/// `hr-…` for the other five).
pub(crate) const SCREENS_308: &[(&str, &str)] = &[
    (
        "h-one-line",
        include_str!("fixtures/claude-2.1.308-box-bash-h-one-line.txt"),
    ),
    (
        "h-twin",
        include_str!("fixtures/claude-2.1.308-box-bash-h-twin.txt"),
    ),
    (
        "h-width-a",
        include_str!("fixtures/claude-2.1.308-box-bash-h-width-a.txt"),
    ),
    (
        "h-width-b",
        include_str!("fixtures/claude-2.1.308-box-bash-h-width-b.txt"),
    ),
    (
        "h-long-word",
        include_str!("fixtures/claude-2.1.308-box-bash-h-long-word.txt"),
    ),
    (
        "h-newlines",
        include_str!("fixtures/claude-2.1.308-box-bash-h-newlines.txt"),
    ),
    (
        "h-edge-spaces",
        include_str!("fixtures/claude-2.1.308-box-bash-h-edge-spaces.txt"),
    ),
    (
        "h-full-split",
        include_str!("fixtures/claude-2.1.308-box-bash-h-full-split.txt"),
    ),
    (
        "h-full-join",
        include_str!("fixtures/claude-2.1.308-box-bash-h-full-join.txt"),
    ),
    (
        "h-short-lines",
        include_str!("fixtures/claude-2.1.308-box-bash-h-short-lines.txt"),
    ),
    (
        "h-wrap-same",
        include_str!("fixtures/claude-2.1.308-box-bash-h-wrap-same.txt"),
    ),
    (
        "h-wrap-same-twin",
        include_str!("fixtures/claude-2.1.308-box-bash-h-wrap-same-twin.txt"),
    ),
    (
        "w-tabs",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tabs.txt"),
    ),
    (
        "w-tab-at-wrap",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-at-wrap.txt"),
    ),
    (
        "w-tab-late",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-late.txt"),
    ),
    (
        "w-tab-lead",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-lead.txt"),
    ),
    (
        "w-tab-runs",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-runs.txt"),
    ),
    (
        "w-tab-second-line",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-second-line.txt"),
    ),
    (
        "w-tab-header-line2",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-header-line2.txt"),
    ),
    (
        "w-tab-after-wide",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-after-wide.txt"),
    ),
    (
        "w-cjk-long",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-long.txt"),
    ),
    (
        "w-cjk-odd",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-odd.txt"),
    ),
    (
        "w-cjk-words",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-words.txt"),
    ),
    (
        "w-cjk-edge",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-edge.txt"),
    ),
    (
        "w-cjk-header",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-header.txt"),
    ),
    (
        "w-hangul",
        include_str!("fixtures/claude-2.1.308-box-bash-w-hangul.txt"),
    ),
    (
        "w-ranges",
        include_str!("fixtures/claude-2.1.308-box-bash-w-ranges.txt"),
    ),
    (
        "hr-kana",
        include_str!("fixtures/claude-2.1.308-box-bash-hr-kana.txt"),
    ),
    (
        "hr-hangul-fullwidth",
        include_str!("fixtures/claude-2.1.308-box-bash-hr-hangul-fullwidth.txt"),
    ),
    (
        "hr-signs",
        include_str!("fixtures/claude-2.1.308-box-bash-hr-signs.txt"),
    ),
];

/// The 2.1.308 captures whose commands hold a character breaker-4's model of
/// the renderer refused:
/// an ideographic space inside (`ＡＢＣ　ｘｙｚ`) and at both ends, and a
/// zero-width space and a soft hyphen (the box draws each as `U+FFFD`, the
/// call's row draws neither).
pub(crate) const REFUSED_308: &[(&str, &str, &str)] = &[
    (
        "w-fullwidth",
        include_str!("fixtures/claude-2.1.308-box-bash-w-fullwidth.txt"),
        "\\u{3000}",
    ),
    (
        "w-ideo-space-edges",
        include_str!("fixtures/claude-2.1.308-box-bash-w-ideo-space-edges.txt"),
        "\\u{3000}",
    ),
    (
        "w-zero-width",
        include_str!("fixtures/claude-2.1.308-box-bash-w-zero-width.txt"),
        "\\u{200b}",
    ),
];

/// The Bash row of a command of two lines and under 160 units (`  ` and 65
/// empty backtick pairs, then `echo x`), shown as it is: its blanks kept,
/// no `…`. Its box is the parser's (`Parse error`), not the too-many box.
pub(crate) const SHORT_BACKTICKS: &str =
    include_str!("fixtures/claude-2.1.308-box-bash-w-short-backticks.txt");

/// N's box (`Probe stale`, h-wrap-same's newline twin), under the answered
/// row of an older call S and N's own `Waiting…` row: breaker-4's stale call.
pub(crate) const STALE_SCREEN: &str = include_str!("fixtures/claude-2.1.308-box-bash-stale-n.txt");

/// The rows Claude Code draws `text` in where no line wraps: each line with
/// its tabs expanded to the next stop of eight and its trailing blanks
/// dropped (MEASURED on 2.1.308, the measured note's §W; ASCII only).
fn drawn(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|line| {
            let mut out = String::new();
            for c in line.chars() {
                if c == '\t' {
                    let blanks = 8 - out.chars().count() % 8;
                    out.extend(std::iter::repeat_n(' ', blanks));
                } else {
                    out.push(c);
                }
            }
            out.trim_end().to_string()
        })
        .collect()
}

/// The measured too-many box with its barred rows drawn for `text`
/// ([`drawn`]), read.
fn boxed(text: &str) -> PromptV2 {
    let rows = super::super::approval::tests::too_many_box(&drawn(text), false);
    aterm_phase::prompt::parse_prompt_v2(&rows).expect("the box")
}

/// A READING OF THE ROWS PROVES NOTHING WHERE ITS WORDS TURN ON A BLANK THE
/// SCREEN CANNOT SHOW ([`blanks_read_as_drawn`]; MEASURED 2026-10-22 under
/// `/bin/bash` 3.2, the measured note's §B). Each command below draws as the
/// same rows as its twin ([`drawn`]: a tab to its stop, no blank at a row's
/// end), the text the screen shows typed as a command; the classifier reads
/// the twin a read and the command
/// not, since bash runs the command's `tee x` where the twin holds it as
/// here-document text or as a word: `<<-` over a tab-led `EOF`, and over
/// the blanks the tab draws as; a backslash before a blank that ends a line,
/// and one ending it; a backslash before a tab and `#`, and one before the
/// blanks the tab draws as. The screen's newline reading of each box is
/// refused, the form named. Where the screen's reading is the twin (both
/// backslashes) it would otherwise be proven a read; the rows of the
/// here-document lose their leading blanks, so there the screen's own
/// reading ends the body and runs `tee x`.
///
/// NEGATIVE CONTROLS: blanks and a tab that only separate words, a backslash
/// before one blank and a word (`My\ File`, read alike whether the blank is
/// a tab drawn as one), a here-string and the measured lines of `echo` are
/// read as drawn; a backslash at the text's end, one before a tab, one
/// before a blank that ends a line, and an arithmetic `<<` are refused by
/// name (stricter than the shell there).
#[test]
fn a_reading_whose_words_turn_on_a_blank_the_screen_cannot_show_proves_nothing() {
    use crate::supervise::classify::classify_command;
    let pre = ["echo $(true) $(true) $(true) $(true) $(true)"; 13].join("\n");
    let newline_reading = |p: &PromptV2| {
        p.readings()
            .into_iter()
            .find(|r| r.contains('\n'))
            .expect("the newline reading")
    };
    for (what, run, twin, fooled, named) in [
        (
            "<<- and a tab",
            format!("{pre}\ncat <<-'EOF'\n\tEOF\ntee x"),
            format!("{pre}\ncat <<-'EOF'\n        EOF\ntee x"),
            false,
            "it holds a here-document",
        ),
        (
            "a backslash before a blank at a line's end",
            format!("{pre} a\\ \ntee x"),
            format!("{pre} a\\\ntee x"),
            true,
            "a backslash ends a line",
        ),
        (
            "a backslash before a tab and #",
            format!("{pre}\necho ab\\\t#; tee x"),
            format!("{pre}\necho ab\\        #; tee x"),
            true,
            "a backslash stands before blanks that may be a tab",
        ),
    ] {
        assert!(!classify_command(&run).read_only, "{what}");
        assert!(classify_command(&twin).read_only, "{what}");
        assert_eq!(drawn(&run), drawn(&twin), "{what}: drawn alike");
        let p = boxed(&run);
        let shown = newline_reading(&p);
        assert_eq!(shown == twin, fooled, "{what}: {shown:?}");
        assert_eq!(classify_command(&shown).read_only, fooled, "{what}");
        let got = blanks_read_as_drawn(&shown);
        assert!(
            got.as_ref().is_err_and(|e| e.contains(named)),
            "{what}: {got:?}"
        );
    }
    for plain in [
        format!("{pre}\necho ab        x"),
        format!("{pre}\necho\tab x"),
        format!("{pre}\ncat My\\ File"),
        format!("{pre}\ncat <<< x"),
        pre.clone(),
    ] {
        for reading in boxed(&plain).readings() {
            assert_eq!(blanks_read_as_drawn(&reading), Ok(()), "{reading:?}");
        }
    }
    for (odd, named) in [
        ("echo a\\", "a backslash ends a line"),
        ("echo a\\\tb", "may be a tab"),
        ("echo a\\ \necho b", "may be a tab"),
        ("echo $((1<<2))", "here-document"),
    ] {
        let got = blanks_read_as_drawn(odd);
        assert!(
            got.as_ref().is_err_and(|e| e.contains(named)),
            "{odd:?}: {got:?}"
        );
    }
}
