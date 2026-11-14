// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the hold's verdicts over records and reads.

use super::*;
use crate::forever::surfaces::Window;
use crate::forever::{Chain, Row, Seen};

/// The watch's start (unix ms).
const T0: i64 = 1_792_000_000_000;

/// When Land's instruction was ledgered, and its second.
const LAND_MS: i64 = T0 + 10_000;
const LAND_S: i64 = LAND_MS / 1000;

/// A journal line at `t` after [`T0`].
fn j(t: i64, line: &str) -> (i64, String) {
    (T0 + t, line.to_string())
}

/// A ledger row typed at `t` after [`T0`].
fn typed(t: i64, rule: &str, command: &str) -> Row {
    Row {
        ts: T0 + t,
        box_seq: None,
        rule: rule.into(),
        decision: "typed".into(),
        command: command.into(),
        reason: "the turn-end policy".into(),
    }
}

/// A `status` read at `t` after [`T0`] with its hold's three fields.
fn st(t: i64, held: &str, nudges: &str, since: &str) -> (i64, String) {
    j(
        t,
        &format!(
            "task=open needs=- needs_n=- autopilot=land supervisor=aterm agent=idle end=- \
             end_seen=- end_cheered=- land_held={held} land_nudges={nudges} land_since={since}"
        ),
    )
}

/// The judge's context: land-c, under Claude Code, from [`T0`].
fn cx() -> Ctx<'static> {
    Ctx {
        chain: Chain::LandC,
        program: "claude",
        t0: T0,
    }
}

/// A Land chain's records.
fn seen(held: &str, why: &str, named: &str, landed: bool) -> Seen {
    let since = LAND_S.to_string();
    let mut s = Seen {
        journal: vec![
            j(
                9_800,
                "TYPED seq=1 rule=land@v1 [aterm harness] Autopilot is Land: land",
            ),
            j(
                70_000,
                "TYPED seq=2 rule=land-nudge@v1 [aterm harness] Still landing:",
            ),
        ],
        ledger: vec![
            typed(10_000, "land@v1", "[aterm harness] Autopilot is Land: land"),
            typed(
                70_000,
                "land-nudge@v1",
                &format!(
                    "[aterm harness] Still landing: Your report still names open work \
                     ({named}): finish and land it first."
                ),
            ),
        ],
        status: vec![
            st(1_000, "-", "-", "-"),
            st(11_000, "-", "0", &since),
            st(40_000, held, "0", &since),
            st(71_000, held, "1", &since),
        ],
        status_reads: [1_000, 11_000, 40_000, 71_000, 150_000]
            .map(|t| T0 + t)
            .to_vec(),
        end_ms: T0 + 200_000,
        window: Window {
            read: true,
            label: "land-c".into(),
            hover: vec![
                j(1_000, "driven by aterm (land)"),
                j(
                    40_000,
                    &format!("driven by aterm (land: not landed yet, {why})"),
                ),
            ],
            hover_reads: [1_000, 11_000, 40_000, 71_000, 150_000]
                .map(|t| T0 + t)
                .to_vec(),
            page: vec![
                j(1_000, "head=land-c: Window 2, tab 4\nstate=-"),
                j(
                    11_000,
                    "head=land-c: Window 2, tab 4\nstate=landing since 03:20",
                ),
                j(
                    40_000,
                    &format!(
                        "head=land-c: Window 2, tab 4\nstate=not landed yet: {why} \u{b7} landing \
                         since 03:20"
                    ),
                ),
            ],
            page_reads: [1_000, 11_000, 40_000, 71_000, 150_000]
                .map(|t| T0 + t)
                .to_vec(),
            ..Window::default()
        },
        ..Seen::default()
    };
    if landed {
        s.journal.push(j(130_000, "DONE seq=3 rule=land@v1 x"));
        s.status.push(st(131_000, "-", "-", "-"));
        s.window
            .hover
            .push(j(131_000, "driven by aterm (land, landed)"));
        s.window.page.push(j(
            131_000,
            "head=land-c: Window 2, tab 4\nstate=landed 03:22",
        ));
    }
    s
}

/// A full real capture path, including unchanged reads suppressed by `put`.
fn read_window(seen: &mut Seen, t: i64, hover: &str, state: &str) {
    let label = &seen.window.label;
    let tip = format!("{label}\n{hover}");
    let chrome = format!(
        "toolbar-tabs count=1 selected=0 labels=[{label:?}] states=[[]] tooltips=[Some({tip:?})]"
    );
    let head = format!("{label}: Window 2, tab 4");
    let page = format!(
        "text key=\"harness/session/1\" label={head:?}\n\
         text key=\"harness/session/1/state\" label={state:?}"
    );
    let glance = super::super::Glance::of(T0 + t, &chrome, &page);
    let _ = seen.window.note(&glance, "s-c");
}

/// L1: no hold before the instruction, then its ledgered second and the nudges typed.
#[test]
fn a_hold_begins_at_the_instruction_and_counts_its_nudges() {
    let good = seen("repo", "1 untracked file: wip-notes.txt", "x", false);
    assert!(
        matches!(begins(&good, &cx()), Verdict::Pass(_)),
        "{:?}",
        begins(&good, &cx())
    );
    let mut early = good.clone();
    early.status[0] = st(1_000, "-", "0", &LAND_S.to_string());
    assert!(matches!(begins(&early, &cx()), Verdict::Fail(_)));
    let mut off = good.clone();
    off.status[1] = st(11_000, "-", "0", &(LAND_S + 60).to_string());
    assert!(matches!(begins(&off, &cx()), Verdict::Fail(_)));
    let mut counted = good.clone();
    counted.status[3] = st(71_000, "repo", "2", &LAND_S.to_string());
    assert!(matches!(begins(&counted, &cx()), Verdict::Fail(_)));
    let mut old = good;
    old.status[2] = j(40_000, "task=open needs=- end=-");
    assert!(matches!(begins(&old, &cx()), Verdict::Fail(_)));
}

/// L4 and L8: each repository chain says its own count and no other leg, so each is the other's negative control.
#[test]
fn each_repository_chain_says_its_own_count_alone() {
    let l4 = seen("repo", "1 untracked file: wip-notes.txt", "x", false);
    assert!(
        matches!(legs(&l4, &cx(), L4), Verdict::Pass(_)),
        "{:?}",
        legs(&l4, &cx(), L4)
    );
    assert!(
        matches!(legs(&l4, &cx(), L8), Verdict::Fail(_)),
        "L8's count"
    );
    let two = seen(
        "repo",
        "1 untracked file: wip-notes.txt, 1 stash",
        "x",
        false,
    );
    assert!(matches!(legs(&two, &cx(), L4), Verdict::Fail(_)));
    let open = seen("repo,open", "1 untracked file: wip-notes.txt", "x", false);
    assert!(matches!(legs(&open, &cx(), L4), Verdict::Fail(_)));
    let mut unsaid = l4.clone();
    unsaid.window.page[2] = j(
        40_000,
        "head=land-c: Window 2, tab 4\nstate=not landed yet: x",
    );
    assert!(matches!(legs(&unsaid, &cx(), L4), Verdict::Fail(_)));
    let mut off = l4.clone();
    off.window.page = vec![j(1_000, "-")];
    assert!(matches!(legs(&off, &cx(), L4), Verdict::Pass(_)));
    let mut scrolled = l4.clone();
    scrolled.window.page = vec![
        j(1_000, "head=land-c: Window 2, tab 4\nstate=-"),
        j(40_000, "-"),
    ];
    assert!(
        matches!(legs(&scrolled, &cx(), L4), Verdict::Pass(_)),
        "{:?}",
        legs(&scrolled, &cx(), L4)
    );
    let mut stale = l4;
    stale.window = Window {
        label: "land-c".into(),
        ..Window::default()
    };
    let hover = "driven by aterm (land: not landed yet, 1 untracked file: wip-notes.txt)";
    read_window(&mut stale, 1_000, hover, "-");
    read_window(&mut stale, 40_000, hover, "-");
    assert_eq!(stale.window.page.len(), 1, "unchanged row was deduped");
    assert!(matches!(legs(&stale, &cx(), L4), Verdict::Fail(_)));
    read_window(
        &mut stale,
        71_000,
        hover,
        "not landed yet: 1 untracked file: wip-notes.txt",
    );
    assert!(matches!(legs(&stale, &cx(), L4), Verdict::Pass(_)));
}

/// A stopped tab's hover and state line lead with the stop (3a), the hold's words behind it: each
/// leg is read past the stop, so a stopped land-c still says its own count alone.
#[test]
fn a_stopped_tabs_legs_are_read_past_its_stop() {
    use aterm_agent::harness::acts::{FACT_SEPARATOR, HeldBy, stop_words};
    let mut stopped = seen("repo", "1 untracked file: wip-notes.txt", "x", false);
    let hover = stop_words(HeldBy::SameReply, StopForm::Hover, false);
    let line = stop_words(HeldBy::SameReply, StopForm::Line, false);
    stopped.window.hover[1].1 =
        format!("driven by aterm (land: {hover}; not landed yet, 1 untracked file: wip-notes.txt)");
    stopped.window.page[2].1 = format!(
        "head=land-c: Window 2, tab 4\nstate={line}{FACT_SEPARATOR}not landed yet: 1 untracked \
         file: wip-notes.txt \u{b7} landing since 03:20"
    );
    let v = legs(&stopped, &cx(), L4);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    assert!(
        matches!(legs(&stopped, &cx(), L8), Verdict::Fail(_)),
        "L8's count"
    );
}

/// The line the first look read counts even when later reads draw the row with no state line.
#[test]
fn the_first_looks_state_line_counts_though_its_page_read_came_first() {
    let head = "head=land-c: Window 2, tab 4";
    let said = format!(
        "{head}\nstate=not landed yet: 1 untracked file: wip-notes.txt \u{b7} landing since 03:20"
    );
    let mut bare = seen("repo", "1 untracked file: wip-notes.txt", "x", false);
    bare.window.page.truncate(2);
    bare.window.page.push(j(40_000, head));
    assert!(
        matches!(legs(&bare, &cx(), L4), Verdict::Fail(_)),
        "{:?}",
        legs(&bare, &cx(), L4)
    );
    let mut first = bare.clone();
    first.window.page.insert(2, j(39_900, &said));
    first.window.page_reads.push(T0 + 39_900);
    first.window.page_reads.sort_unstable();
    assert!(
        matches!(legs(&first, &cx(), L4), Verdict::Pass(ref w) if w.contains("+39.9 s not landed yet")),
        "{:?}",
        legs(&first, &cx(), L4)
    );
    let mut unsaid = bare.clone();
    unsaid.window.page[2] = j(40_000, &format!("{head}\nstate=landing since 03:20"));
    assert!(
        matches!(legs(&unsaid, &cx(), L4), Verdict::Fail(_)),
        "{:?}",
        legs(&unsaid, &cx(), L4)
    );
    let mut before = unsaid;
    before.window.page.insert(1, j(5_000, &said));
    before.window.page_reads.push(T0 + 5_000);
    before.window.page_reads.sort_unstable();
    assert!(
        matches!(legs(&before, &cx(), L4), Verdict::Fail(_)),
        "{:?}",
        legs(&before, &cx(), L4)
    );
}

/// [`seen`] whose hold its need alone holds, said once as the glass says it (slice 4b): the
/// hover `waits only on you`, the state line the need under it.
fn need_alone(held: &str) -> Seen {
    let mut s = seen(held, "x", "x", false);
    s.window.hover[1].1 = format!("driven by aterm (land: {WAITS_ON_YOU})");
    s.window.page[2].1 = format!(
        "head=land-c: Window 2, tab 4\nstate={WAITS_ON_YOU}: {} \u{b7} landing since 03:20",
        crate::forever::NEED
    );
    s
}

/// L6: the tall reply read whole, so its hold is its need alone, said once: it waits only on
/// the person, the need named under it, never `not landed yet: needs you`.
/// Negative controls: the need worded as another leg fails, and so does a cut leg beside it.
#[test]
fn l6_is_its_need_alone() {
    let good = need_alone("needs");
    assert!(
        matches!(legs(&good, &cx(), L6), Verdict::Pass(_)),
        "{:?}",
        legs(&good, &cx(), L6)
    );
    let worded = seen(
        "needs",
        &format!("needs you: {}", crate::forever::NEED),
        "x",
        false,
    );
    assert!(
        matches!(legs(&worded, &cx(), L6), Verdict::Fail(_)),
        "{:?}",
        legs(&worded, &cx(), L6)
    );
    let cut = need_alone("needs,cut");
    assert!(matches!(legs(&cut, &cx(), L6), Verdict::Fail(_)));
}

/// [`seen`] for L2 under the land check: the check typed, its answer's list quoted by the nudge
/// after it as `quoted` (`None`: a nudge quoting nothing), and the hover and the state line
/// saying `open` after their heads.
fn listed(quoted: Option<&str>, open: &str) -> Seen {
    let mut s = seen("open", open, "x", false);
    let text = aterm_agent::supervise::policy::land_check::land_check_text(None, false, None);
    s.journal[1] = j(
        30_000,
        "CONTINUED seq=2 rule=land-check@v1 [aterm harness] Land check:",
    );
    s.ledger[1] = typed(30_000, turn_end::RULE_LAND_CHECK, &text);
    s.journal.push(j(
        70_000,
        "TYPED seq=3 rule=land-nudge@v1 [aterm harness] Still landing:",
    ));
    let suffix = quoted
        .and_then(aterm_agent::supervise::policy::land_check::listed_suffix)
        .unwrap_or_default();
    s.ledger.push(typed(
        70_000,
        "land-nudge@v1",
        &format!("[aterm harness] Still landing: {suffix}"),
    ));
    s
}

/// L2: THE HOVER AND THE STATE LINE QUOTE THE LIST ITS NUDGE QUOTED (slice 4b): the agent's own
/// words in curly quotes after `open in its report:`, the line every item, the hover the first.
/// Negative controls: the reader's family words in place of the quote fail, as does a quote of
/// another list; with no nudge quoting a list, the quote goes unjudged.
#[test]
fn l2_quotes_the_list_its_nudge_quoted() {
    let list = "The docs PR is still open.";
    let shown = format!("{OPEN_IN_REPORT}: \u{201c}{list}\u{201d}");
    let good = listed(Some(list), &shown);
    assert!(
        matches!(legs(&good, &cx(), L2), Verdict::Pass(_)),
        "{:?}",
        legs(&good, &cx(), L2)
    );
    let words = listed(Some(list), &format!("{OPEN_IN_REPORT}: still open"));
    assert!(
        matches!(legs(&words, &cx(), L2), Verdict::Fail(_)),
        "{:?}",
        legs(&words, &cx(), L2)
    );
    let other = listed(Some("The docs PR is merged."), &shown);
    assert!(
        matches!(legs(&other, &cx(), L2), Verdict::Fail(_)),
        "{:?}",
        legs(&other, &cx(), L2)
    );
    let unquoted = listed(None, &shown);
    assert!(
        matches!(legs(&unquoted, &cx(), L2), Verdict::Unobserved(_)),
        "{:?}",
        legs(&unquoted, &cx(), L2)
    );
}

/// L2: THE CHIP'S DESCRIPTION SAYS THE LIST TOO, as `chrome` printed it beside the node the
/// screen reader is handed, at a look in front: the spoken form, the first item whole with no
/// quote marks; a tab behind (no description) is no read.
/// Negative controls: a description in the reader's family words fails; so does one that never
/// leads with 4b's words (main's, a defense owed, no Open leg), which a picker choosing
/// descriptions by 4b's own lead (`Open in its report`) passed as not read.
#[test]
fn l2_the_chips_description_says_the_list() {
    let list = "The docs PR is still open.";
    let shown = format!("{OPEN_IN_REPORT}: \u{201c}{list}\u{201d}");
    let chip = |open: &str| {
        format!(
            "window=2 program=claude face=land drawn=\"\u{2913}\" spoken=\"Autopilot: \
             land.\" description=\"Not landed yet. Open in its report: {open} Landing since \
             10:22, 1 nudge.\" title=\"-\""
        )
    };
    let mut good = listed(Some(list), &shown);
    good.window.held = vec![
        j(
            80_000,
            "window=2 program=claude face=land drawn=- why=not-drawn description=-",
        ),
        j(81_000, &chip(list)),
    ];
    match legs(&good, &cx(), L2) {
        Verdict::Pass(said) => assert!(said.contains("the chip's description +"), "{said}"),
        other => panic!("{other:?}"),
    }
    let mut words = listed(Some(list), &shown);
    words.window.held = vec![j(81_000, &chip("still open."))];
    assert!(
        matches!(legs(&words, &cx(), L2), Verdict::Fail(_)),
        "{:?}",
        legs(&words, &cx(), L2)
    );
    // A description in front that never leads with 4b's words is read all the same: main's
    // family words, a defense owed, and no Open leg at all each fail.
    for description in [
        "Not landed yet. Open work in its report: still open. Landing since 10:22, 1 nudge.",
        "Not landed yet. No Nothing left defense yet. Landing since 10:22, 1 nudge.",
        "Not landed yet.  Landing since 10:22, 1 nudge.",
    ] {
        let mut other = listed(Some(list), &shown);
        other.window.held = vec![j(
            81_000,
            &format!(
                "window=2 program=claude face=land drawn=\"\u{2913}\" spoken=\"Autopilot: \
                 land.\" description=\"{description}\" title=\"-\""
            ),
        )];
        match legs(&other, &cx(), L2) {
            Verdict::Fail(why) => assert!(why.contains("description"), "{why}"),
            v => panic!("{description}: {v:?}"),
        }
    }
    let mut behind = listed(Some(list), &shown);
    behind.window.held = vec![j(
        81_000,
        "window=2 program=claude face=land drawn=- why=not-drawn description=-",
    )];
    match legs(&behind, &cx(), L2) {
        Verdict::Pass(said) => assert!(said.contains("not read in front"), "{said}"),
        v => panic!("{v:?}"),
    }
}

/// [`seen`] for LR: two checks typed, the state line and the hover quoting the first list,
/// then `then` after the second check, and `later` past that where given.
fn reworded_seen(then: &str, later: Option<&str>) -> Seen {
    let old = format!("{OPEN_IN_REPORT}: {}", quoted(crate::forever::CHECK_LIST));
    reworded_from(&old, then, later)
}

/// [`reworded_seen`] with the state line and the hover saying `old` before the second check.
fn reworded_from(old: &str, then: &str, later: Option<&str>) -> Seen {
    let page = |words: &str| {
        format!(
            "head=land-c: Window 2, tab 4\nstate=not landed yet: {words} \u{b7} landing since \
             03:20"
        )
    };
    let hover = |words: &str| format!("driven by aterm (land: not landed yet, {words})");
    let mut s = seen("open", "x", "x", false);
    for (t, n) in [(30_000, 2), (90_000, 4)] {
        s.journal.push(j(
            t,
            &format!("CONTINUED seq={n} rule=land-check@v1 [aterm harness] Land check:"),
        ));
    }
    s.journal.sort();
    s.window.page = vec![
        j(1_000, "head=land-c: Window 2, tab 4\nstate=-"),
        j(40_000, &page(old)),
        j(100_000, &page(then)),
    ];
    s.window.hover = vec![
        j(1_000, "driven by aterm (land)"),
        j(40_000, &hover(old)),
        j(100_000, &hover(then)),
    ];
    if let Some(later) = later {
        s.window.page.push(j(120_000, &page(later)));
        s.window.hover.push(j(120_000, &hover(later)));
    }
    s
}

/// LR: A REWORDED ITEM REFRESHES THE QUOTE on the state line and the hover, which never go back
/// to the old words. Negative controls: the old words kept after the second check (the hold's
/// equality hiding the rewording) fail, as do the old words come back after the new ones; with
/// only one check typed, the new words go unjudged.
#[test]
fn lr_quotes_the_list_then_its_rewording() {
    let quote = |list: &str| format!("{OPEN_IN_REPORT}: {}", quoted(list));
    let (old, new) = (
        quote(crate::forever::CHECK_LIST),
        quote(crate::forever::REWORDED_LIST),
    );
    let good = reworded_seen(&new, None);
    assert!(
        matches!(reworded(&good, &cx()), Verdict::Pass(_)),
        "{:?}",
        reworded(&good, &cx())
    );
    let stale = reworded_seen(&old, None);
    assert!(
        matches!(reworded(&stale, &cx()), Verdict::Fail(_)),
        "{:?}",
        reworded(&stale, &cx())
    );
    let back = reworded_seen(&new, Some(&old));
    assert!(
        matches!(reworded(&back, &cx()), Verdict::Fail(_)),
        "{:?}",
        reworded(&back, &cx())
    );
    let mut once = reworded_seen(&old, None);
    once.journal.retain(|(t, _)| *t != T0 + 90_000);
    assert!(
        matches!(reworded(&once, &cx()), Verdict::Unobserved(_)),
        "{:?}",
        reworded(&once, &cx())
    );
}

/// LR ON A ROW THE PAGE DID NOT DRAW: a full run's tab past the page's first sessions, every read
/// while `status` held the Open leg `-`, is judged on its hover and names the state line a skip.
/// Negative controls: a page never read while the leg stood stays unobserved (a read before it
/// does not count), so does a row drawn in one read and silent, and the hover's own words still
/// fail it.
#[test]
fn lr_names_a_row_the_page_did_not_draw_a_skip() {
    let (old, new) = (
        format!("{OPEN_IN_REPORT}: {}", quoted(crate::forever::CHECK_LIST)),
        format!(
            "{OPEN_IN_REPORT}: {}",
            quoted(crate::forever::REWORDED_LIST)
        ),
    );
    let mut undrawn = reworded_seen(&new, None);
    undrawn.window.page = vec![j(1_000, "-")];
    undrawn.window.page_reads = vec![T0 + 40_000, T0 + 100_000];
    match reworded(&undrawn, &cx()) {
        Verdict::Pass(said) => assert!(
            said.contains("the state line: SKIPPED") && said.contains("in 2 read(s)"),
            "{said}"
        ),
        v => panic!("{v:?}"),
    }
    let mut before = undrawn.clone();
    before.window.page_reads = vec![T0 + 20_000];
    assert!(
        matches!(reworded(&before, &cx()), Verdict::Unobserved(ref w) if w.contains("state line")),
        "{:?}",
        reworded(&before, &cx())
    );
    let mut once_drawn = undrawn.clone();
    once_drawn
        .window
        .page
        .push(j(40_000, "head=land-c: Window 2, tab 4\nstate=-"));
    once_drawn.window.page.push(j(100_000, "-"));
    assert!(
        matches!(reworded(&once_drawn, &cx()), Verdict::Unobserved(ref w) if w.contains("state line")),
        "{:?}",
        reworded(&once_drawn, &cx())
    );
    let mut stale = reworded_seen(&old, None);
    stale.window.page = undrawn.window.page.clone();
    stale.window.page_reads = undrawn.window.page_reads.clone();
    assert!(
        matches!(reworded(&stale, &cx()), Verdict::Fail(ref w) if w.contains("hover")),
        "{:?}",
        reworded(&stale, &cx())
    );
}

/// LR FAILS A BUILD WITHOUT THE QUOTE: the reader's family words a build without 4b says on the
/// Open leg (`open work in its report: still open`, 13a's main), read while `status` held the
/// Open leg, fail, before the second check and after it; the same words read while `status` held
/// no Open leg are no read of it.
/// Negative control: the Open leg chosen by 4b's own lead words (`open in its report: “`) leaves
/// main's words unobserved, which fails this test.
#[test]
fn lr_fails_the_family_words_of_a_build_without_the_quote() {
    let main = "open work in its report: still open";
    let family = reworded_from(main, main, None);
    assert!(
        matches!(reworded(&family, &cx()), Verdict::Fail(_)),
        "{:?}",
        reworded(&family, &cx())
    );
    let new = format!(
        "{OPEN_IN_REPORT}: {}",
        quoted(crate::forever::REWORDED_LIST)
    );
    let first = reworded_from(main, &new, None);
    assert!(
        matches!(reworded(&first, &cx()), Verdict::Fail(ref w) if w.contains("without quoting")),
        "{:?}",
        reworded(&first, &cx())
    );
    let mut repo = reworded_from(main, main, None);
    for e in &mut repo.status {
        e.1 = e.1.replace("land_held=open", "land_held=repo");
    }
    assert!(
        matches!(reworded(&repo, &cx()), Verdict::Unobserved(_)),
        "{:?}",
        reworded(&repo, &cx())
    );
}

/// A HOVER PUSHED BEFORE THE HOLD AND NEVER AGAIN is a stale push, failed:
/// the strip still shows `driven by aterm (land)` ten seconds and more after
/// `status` read the hold, while it stands.
#[test]
fn a_hover_pushed_before_the_hold_and_never_again_fails() {
    let mut stale = seen("repo", "1 untracked file: wip-notes.txt", "x", false);
    stale.window.hover.truncate(1);
    assert!(
        matches!(legs(&stale, &cx(), L4), Verdict::Fail(ref w) if w.contains("stale push")),
        "{:?}",
        legs(&stale, &cx(), L4)
    );
    let mut brief = stale.clone();
    brief.status.truncate(3);
    brief.status.push(st(45_000, "-", "0", &LAND_S.to_string()));
    assert!(
        matches!(legs(&brief, &cx(), L4), Verdict::Unobserved(_)),
        "{:?}",
        legs(&brief, &cx(), L4)
    );
}

/// L5: at the landing the hold goes.
#[test]
fn the_hold_is_gone_at_the_landing() {
    let good = seen("needs", "needs you", "x", true);
    assert!(
        matches!(gone(&good, &cx()), Verdict::Pass(_)),
        "{:?}",
        gone(&good, &cx())
    );
    let mut kept = good.clone();
    kept.status
        .push(st(140_000, "needs", "1", &LAND_S.to_string()));
    assert!(matches!(gone(&kept, &cx()), Verdict::Fail(_)));
    let mut said = good.clone();
    said.window.hover.push(j(
        140_000,
        "driven by aterm (land: not landed yet, needs you)",
    ));
    assert!(matches!(gone(&said, &cx()), Verdict::Fail(_)));
    assert!(matches!(begins_then_gone(&good, &cx()), Verdict::Pass(_)));
    assert!(matches!(begins_then_gone(&kept, &cx()), Verdict::Fail(_)));
    let mut late = good;
    let next = |list: &mut Vec<(i64, String)>| {
        for e in list.iter_mut().filter(|(t, _)| *t == T0 + 131_000) {
            e.0 = T0 + 150_000;
        }
    };
    next(&mut late.status);
    next(&mut late.window.hover);
    next(&mut late.window.page);
    assert!(
        matches!(gone(&late, &cx()), Verdict::Pass(_)),
        "{:?}",
        gone(&late, &cx())
    );
    let held_hover = "driven by aterm (land: not landed yet, needs you)";
    let held_page = "not landed yet: needs you · landing since 03:20";
    let clear_hover = "driven by aterm (land, landed)";
    let clear_page = "landed 03:22";
    for (stale_hover, stale_page) in [(true, false), (false, true)] {
        let mut observed = seen("needs", "needs you", "x", true);
        observed.window = Window {
            label: "land-c".into(),
            ..Window::default()
        };
        read_window(&mut observed, 40_000, held_hover, held_page);
        let mut cleared = observed.clone();
        read_window(
            &mut observed,
            140_000,
            if stale_hover { held_hover } else { clear_hover },
            if stale_page { held_page } else { clear_page },
        );
        assert_eq!(observed.window.hover.len(), if stale_hover { 1 } else { 2 });
        assert_eq!(observed.window.page.len(), if stale_page { 1 } else { 2 });
        assert!(matches!(gone(&observed, &cx()), Verdict::Fail(_)));
        read_window(&mut cleared, 150_000, clear_hover, clear_page);
        assert!(matches!(gone(&cleared, &cx()), Verdict::Pass(_)));
    }
    let mut unread = seen("needs", "needs you", "x", true);
    unread.window = Window {
        label: "land-c".into(),
        ..Window::default()
    };
    read_window(&mut unread, 120_000, clear_hover, clear_page);
    assert!(matches!(gone(&unread, &cx()), Verdict::Unobserved(_)));
    let partial = super::super::Glance::of(
        T0 + 140_000,
        "toolbar-tabs count=1 labels=[\"land-c\"]",
        "text key=\"another/page\" label=\"unrelated\"",
    );
    let _ = unread.window.note(&partial, "s-c");
    let short = super::super::Glance::of(
        T0 + 145_000,
        "toolbar-tabs count=2 labels=[\"land-c\"] tooltips=[None]",
        "",
    );
    let _ = unread.window.note(&short, "s-c");
    assert_eq!(unread.window.hover.len(), 1);
    assert_eq!(unread.window.page.len(), 1);
    assert!(matches!(gone(&unread, &cx()), Verdict::Unobserved(_)));
    read_window(&mut unread, 150_000, clear_hover, clear_page);
    assert_eq!(unread.window.hover.len(), 1, "unchanged clearing is a read");
    assert_eq!(unread.window.page.len(), 1);
    assert!(matches!(gone(&unread, &cx()), Verdict::Pass(_)));
}

/// The hover is its tab's tooltip's who-drives line, read off the strip as
/// it was last stamped where the line prints that (`pushed_tooltips=`, a
/// debug build's), else as composed at the read.
#[test]
fn the_hover_is_read_off_the_strips_tooltip() {
    let stale = "toolbar-tabs count=1 selected=0 labels=[\"land-c\"] icons=[None] \
                 states=[[\"selected\"]] tooltips=[Some(\"land-c\\ndriven by aterm (land: not \
                 landed yet, 1 untracked file)\")] pushed_tooltips=[Some(\"land-c\\ndriven by \
                 aterm (land)\")]";
    assert_eq!(
        super::super::strip_tabs(stale)[0].driven().as_deref(),
        Some("driven by aterm (land)"),
        "as the strip shows it"
    );
    let chrome = "toolbar-tabs count=2 selected=1 labels=[\"aterm\", \"land-c\"] icons=[None, \
                  None] states=[[], [\"selected\"]] tooltips=[None, Some(\"land-c\\nactivity: \
                  idle\\ndriven by aterm (land: not landed yet, 1 untracked file)\")]";
    let tabs = super::super::strip_tabs(chrome);
    assert_eq!(tabs[0].driven(), None);
    assert_eq!(
        tabs[1].driven().as_deref(),
        Some("driven by aterm (land: not landed yet, 1 untracked file)")
    );
    let codex = "toolbar-tabs count=2 selected=0 labels=[\"land\", \"worker\"] icons=[None, \
                 None] states=[[\"selected\"], []] tooltips=[Some(\"land\\ncodex-alab autopilot \
                 (land: codex-alab: not landed yet)\"), Some(\"worker\\nworker of manager\")]";
    let tabs = super::super::strip_tabs(codex);
    assert_eq!(
        tabs[0].driven().as_deref(),
        Some("codex-alab autopilot (land: codex-alab: not landed yet)"),
        "a codex-alab tab's own Autopilot is its driver line"
    );
    assert_eq!(
        tabs[1].driven(),
        None,
        "CONTROL: a worker line names no Autopilot"
    );
    let g = super::super::Glance::of(T0, chrome, "");
    let mut w = Window {
        label: "land-c".into(),
        ..Window::default()
    };
    let _ = w.note(&g, "s-c");
    assert_eq!(
        w.hover.last().map(|(_, h)| h.as_str()),
        Some("driven by aterm (land: not landed yet, 1 untracked file)")
    );
    let mut none = Window {
        label: "aterm".into(),
        ..Window::default()
    };
    let _ = none.note(&g, "s-a");
    assert_eq!(none.hover.last().map(|(_, h)| h.as_str()), Some("-"));
}

/// P5 and L12 (cover's land-t and land-q): a work reply under Land holds on its defense alone;
/// L3's need is its negative control, and it L3's.
#[test]
fn a_work_reply_under_land_holds_on_its_defense_alone() {
    let d = seen("defense", DEFENSE_WORDS, "x", false);
    assert!(
        matches!(legs(&d, &cx(), DEFENSE), Verdict::Pass(_)),
        "{:?}",
        legs(&d, &cx(), DEFENSE)
    );
    assert!(matches!(legs(&d, &cx(), L3), Verdict::Fail(_)));
    let need = seen("needs", "needs you", "x", false);
    assert!(matches!(legs(&need, &cx(), DEFENSE), Verdict::Fail(_)));
    let both = seen(
        "defense",
        &format!("{DEFENSE_WORDS}; needs you"),
        "x",
        false,
    );
    assert!(matches!(legs(&both, &cx(), DEFENSE), Verdict::Fail(_)));
}

/// THE LIST IS READ FROM EITHER LAND NUDGE THAT QUOTES IT: Land's nudge (`You said this is left:
/// “…”.`) and its contradiction of a defense while the list stands (slice 4c, `you said this is
/// left: “…” Finish it …`), each item whole; no quote, no list.
/// Negative control: the contradiction's opening dropped from [`listed_in_nudge`]'s openings
/// misses the contradiction's list.
#[test]
fn the_list_is_read_from_the_nudge_and_from_the_contradiction() {
    let list = "The docs PR is still open.";
    let nudge = format!(
        "{} {}",
        turn_end::LAND_NUDGE,
        land_check::listed_suffix(list).expect("quoted")
    );
    let contradiction = turn_end::land_contradicted(
        &aterm_agent::harness::repo_work::RepoRead::Clean,
        Some(list),
        &[],
        None,
    )
    .expect("the Open leg");
    for text in [&nudge, &contradiction] {
        assert_eq!(listed_in_nudge(text), Some(list), "{text}");
    }
    assert_eq!(listed_in_nudge(turn_end::LAND_NUDGE), None);
}
