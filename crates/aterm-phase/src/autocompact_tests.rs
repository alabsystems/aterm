// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Autocompact offer reader tests (`crate::prompt::fixtures::AUTOCOMPACT_*`).

use super::*;
use crate::phase::Phase;
use crate::prompt::fixtures::{
    AUTOCOMPACT_80COL_500K, AUTOCOMPACT_FULLSCREEN, AUTOCOMPACT_KEEP_FIRST,
    AUTOCOMPACT_LOWER_FIRST, AUTOCOMPACT_PRESS_AGAIN, AUTOCOMPACT_SAVED, screen,
};
use crate::prompt::parse_prompt_v2;

fn offer(rows: &[String]) -> Option<Offer> {
    parse_prompt_v2(rows).and_then(|p| p.autocompact_offer())
}

/// Whether `rows` read as the offer at all (any reading of it).
fn is_offer(rows: &[String]) -> bool {
    parse_prompt_v2(rows).is_some_and(|p| p.kind == PromptKind::AutocompactOffer)
}

/// `rows` with the first row whose text is `from` replaced by the row `row`,
/// drawn as given.
fn swap(rows: &[String], from: &str, row: &str) -> Vec<String> {
    let mut out = rows.to_vec();
    let at = out
        .iter()
        .position(|r| crate::text::trim(r) == from)
        .unwrap_or_else(|| panic!("no row {from:?}"));
    out[at] = row.to_string();
    out
}

/// `rows` with the first row whose text is `from` replaced by `to`.
fn edit(rows: &[String], from: &str, to: &str) -> Vec<String> {
    let mut out = rows.to_vec();
    let at = out
        .iter()
        .position(|r| crate::text::trim(r) == from)
        .unwrap_or_else(|| panic!("no row {from:?}"));
    let col = crate::phase::leading_spaces(&out[at]);
    out[at] = format!("{}{to}", " ".repeat(col));
    out
}

#[test]
fn the_keep_first_offer_is_read_whole_with_the_focus_on_keep() {
    let rows = screen(AUTOCOMPACT_KEEP_FIRST);
    let p = parse_prompt_v2(&rows).expect("the offer");
    assert_eq!(p.kind, PromptKind::AutocompactOffer);
    assert_eq!(p.kind.name(), "autocompact-offer");
    assert_eq!(p.title, "We recommend compacting earlier");
    assert_eq!(p.select, Select::ArrowsEnter);
    assert_eq!(p.cancel, None, "Esc is never offered");
    assert!(
        p.options
            .iter()
            .all(|o| o.role == Role::Other && o.n.is_none())
    );
    let o = p.autocompact_offer().expect("read whole");
    assert_eq!(
        (o.keep, o.accept, o.tokens, o.focused),
        (0, 1, 400_000, OfferFocus::Keep)
    );
    assert_eq!(p.options[o.keep].label, "Keep auto (1m tokens)");
    assert_eq!(p.options[o.accept].label, "Autocompact at ‹ 400k › tokens");
    assert_eq!(
        rows[p.options[o.keep].row].trim(),
        "❯ Keep auto (1m tokens)"
    );
    assert_eq!(crate::prompt::prompt_box_span(&rows), Some(p.span));
    assert_eq!(crate::prompt::prompt_box_first_row(&rows), Some(p.span.0));
    assert_eq!(
        crate::prompt::title_row(&p, &rows),
        Some(" We recommend compacting earlier")
    );
    let v1 = crate::prompt::parse_prompt(&rows).expect("v1");
    assert_eq!(
        (v1.kind, &v1.command, v1.options.len()),
        (p.kind, &p.command, 0)
    );
}

#[test]
fn the_lower_first_offer_reads_its_focus_and_its_description() {
    let rows = screen(AUTOCOMPACT_LOWER_FIRST);
    let p = parse_prompt_v2(&rows).expect("the box");
    let o = p.autocompact_offer().expect("the offer");
    assert_eq!(
        (o.accept, o.keep, o.tokens, o.focused),
        (0, 1, 400_000, OfferFocus::Accept)
    );
    let (_, described) = p.description.split_once('\n').expect("a description");
    assert!(
        described.starts_with("This conversation is at "),
        "{described}"
    );
}

#[test]
fn a_narrow_offer_with_the_stepper_moved_reads_its_size() {
    let rows = screen(AUTOCOMPACT_80COL_500K);
    let o = offer(&rows).expect("the offer");
    assert_eq!((o.tokens, o.focused), (500_000, OfferFocus::Accept));
}

#[test]
fn the_fullscreen_panel_and_the_more_text_warning_are_read() {
    let rows = screen(AUTOCOMPACT_FULLSCREEN);
    let o = offer(&rows).expect("the offer");
    assert_eq!((o.tokens, o.focused), (400_000, OfferFocus::Keep));
}

/// The reader names the box: authoritative prompt, so the supervisor's
/// approval loop reads it (before: non-authoritative idle, no composer).
#[test]
fn the_offer_reads_as_an_authoritative_prompt() {
    for text in [AUTOCOMPACT_KEEP_FIRST, AUTOCOMPACT_LOWER_FIRST] {
        let rows = screen(text);
        let r = crate::reader::read(None, &rows, None);
        assert_eq!(r.phase, Phase::Prompt);
        assert!(r.phase_authoritative);
        assert_eq!(r.prompt.map(|p| p.kind), Some(PromptKind::AutocompactOffer));
    }
}

/// CONTROL: an exit pending replaces the footer — a person's screen, never
/// this kind.
#[test]
fn an_exit_pending_is_not_the_offer() {
    assert!(!is_offer(&screen(AUTOCOMPACT_PRESS_AGAIN)));
}

/// CONTROL: the dialog answered — its outcome line in the transcript, the
/// composer back — reads idle with no box.
#[test]
fn the_saved_offer_reads_idle() {
    let rows = screen(AUTOCOMPACT_SAVED);
    let r = crate::reader::read(None, &rows, None);
    assert_eq!(r.phase, Phase::Idle);
    assert!(r.prompt.is_none());
}

/// CONTROLS, one per condition: each edit of the keep-first screen is no
/// offer, and the unedited screen is one.
#[test]
fn every_condition_is_needed() {
    let rows = screen(AUTOCOMPACT_KEEP_FIRST);
    assert!(is_offer(&rows), "the screen the controls edit");
    let accept = "Autocompact at ‹ 400k › tokens";
    let keep = "❯ Keep auto (1m tokens)";
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "another title",
            edit(
                &rows,
                "We recommend compacting earlier",
                "We recommend compacting",
            ),
        ),
        (
            "a footer in other words",
            edit(
                &rows,
                "↑/↓ choose · ←/→ change size · enter confirm · esc not now",
                "↑/↓ choose · enter confirm · esc not now",
            ),
        ),
        (
            "a size in words the vendor never draws",
            edit(&rows, accept, "Autocompact at ‹ 1000k › tokens"),
        ),
        (
            "a size the question does not ask",
            edit(&rows, accept, "Autocompact at ‹ 500k › tokens"),
        ),
        (
            "the screen-reader label",
            edit(&rows, accept, "Autocompact at 400k tokens"),
        ),
        (
            "a keep row in other words",
            edit(&rows, keep, "❯ Keep going"),
        ),
        (
            "two focused rows",
            swap(&rows, accept, "   ❯ Autocompact at ‹ 400k › tokens"),
        ),
        (
            "no focused row",
            edit(&rows, keep, "  Keep auto (1m tokens)"),
        ),
        ("a row left of the title", swap(&rows, "time.", "⏺ time.")),
    ];
    for (what, edited) in cases {
        assert_eq!(edited.len(), rows.len(), "{what}");
        assert!(!is_offer(&edited), "{what}");
    }
    let mut third = rows.clone();
    let at = third
        .iter()
        .position(|r| r.trim() == accept)
        .expect("the accept row");
    third.insert(at + 1, "     Keep 500k tokens".to_string());
    assert!(!is_offer(&third), "a third option");
    let mut quoted = rows.clone();
    let w = 120;
    quoted.extend([
        "─".repeat(w),
        "❯ ".to_string(),
        "─".repeat(w),
        "  ? for shortcuts".to_string(),
    ]);
    assert!(!is_offer(&quoted), "a composer under it");
    let mut under = rows.clone();
    under.push("  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string());
    assert!(!is_offer(&under), "a row under the footer");
}

/// The footer typed as a draft into the composer is no offer.
#[test]
fn the_footer_as_a_draft_is_not_the_offer() {
    let w = 120;
    let rows: Vec<String> = [
        "⏺ Done.".to_string(),
        String::new(),
        "─".repeat(w),
        "❯ ↑/↓ choose · ←/→ change size · enter confirm · esc not now".to_string(),
        "─".repeat(w),
        "  esc to clear".to_string(),
    ]
    .into();
    assert!(!is_offer(&rows));
}

/// A position label beside the title (`1 of 2`, the frame's `aside`) and a
/// description drawn on the accept row itself are still the offer.
#[test]
fn a_position_label_and_an_inline_description_are_read() {
    let rows = screen(AUTOCOMPACT_KEEP_FIRST);
    let labelled = edit(
        &rows,
        "We recommend compacting earlier",
        "We recommend compacting earlier                                  1 of 2",
    );
    let p = parse_prompt_v2(&labelled).expect("position label");
    assert!(p.autocompact_offer().is_some());
    assert_eq!(
        crate::prompt::title_row(&p, &labelled).map(crate::text::trim),
        Some("We recommend compacting earlier                                  1 of 2")
    );
    let inline = edit(
        &rows,
        "Autocompact at ‹ 400k › tokens",
        "Autocompact at ‹ 400k › tokens  This conversation is at 401k, so it will be summarized \
         on your next message.",
    );
    let p = parse_prompt_v2(&inline).expect("inline description");
    assert!(p.autocompact_offer().is_some());
    assert!(
        p.description
            .split_once('\n')
            .is_some_and(|(_, d)| d.starts_with("This conversation is at 401k")),
        "{}",
        p.description
    );
}

/// THE SIZE IS READ IN THE VENDOR'S WORDS, ANY SIZE IT CAN DRAW (a served
/// list holds sizes off [`LADDER`]): `250k`, `350.5k`, `1m` beside the
/// ladder's own, each drawn back the same by [`size_words`]. CONTROLS: a
/// leading zero, a `.0` the vendor drops, two decimals, four digits, no
/// unit, the screen-reader label, a trailing stop.
#[test]
fn accept_size_reads_every_size_the_vendor_draws() {
    for (words, tokens) in [
        ("300k", 300_000),
        ("900k", 900_000),
        ("250k", 250_000),
        ("350k", 350_000),
        ("350.5k", 350_500),
        ("1m", 1_000_000),
    ] {
        let label = format!("Autocompact at ‹ {words} › tokens");
        assert_eq!(accept_size(&label), Some(tokens), "{label}");
        assert_eq!(size_words(tokens), words);
    }
    for bad in [
        "Autocompact at ‹ 0400k › tokens",
        "Autocompact at ‹ 400.0k › tokens",
        "Autocompact at ‹ 350.25k › tokens",
        "Autocompact at ‹ 1000k › tokens",
        "Autocompact at ‹ 1.5m › tokens",
        "Autocompact at ‹ k › tokens",
        "Autocompact at ‹ 400 › tokens",
        "Autocompact at 400k tokens",
        "Autocompact at ‹ 400k › tokens.",
    ] {
        assert_eq!(accept_size(bad), None, "{bad}");
    }
}

/// A SERVED SIZE OFF THE LADDER (350k, 450k, 250k: the 80-column screen
/// with its stepper at 500k redrawn at each) is read whole, an
/// authoritative prompt with its size — the dialog a ladder-only reader
/// left unread, its walk stalled. CONTROL: the body asking another size than
/// the accept row's is still no offer.
#[test]
fn a_served_size_off_the_ladder_is_read() {
    let rows = screen(AUTOCOMPACT_80COL_500K);
    assert_eq!(offer(&rows).map(|o| o.tokens), Some(500_000));
    for (words, tokens) in [("350k", 350_000), ("450k", 450_000), ("250k", 250_000)] {
        let at: Vec<String> = rows.iter().map(|r| r.replace("500k", words)).collect();
        assert_ne!(at, rows, "{words}");
        let r = crate::reader::read(None, &at, None);
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Prompt, true),
            "{words}"
        );
        let o = r.prompt.and_then(|p| p.autocompact_offer()).expect(words);
        assert_eq!(
            (o.tokens, o.focused),
            (tokens, OfferFocus::Accept),
            "{words}"
        );
        let asked_other: Vec<String> = at
            .iter()
            .map(|r| {
                if r.contains("Want to autocompact at") {
                    r.replace(words, "400k")
                } else {
                    r.clone()
                }
            })
            .collect();
        assert!(!is_offer(&asked_other), "{words}: the body asks 400k");
    }
}
