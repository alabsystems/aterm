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
    let good = seen("repo", "1 untracked file", "x", false);
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
    let l4 = seen("repo", "1 untracked file", "x", false);
    assert!(
        matches!(legs(&l4, &cx(), L4), Verdict::Pass(_)),
        "{:?}",
        legs(&l4, &cx(), L4)
    );
    assert!(
        matches!(legs(&l4, &cx(), L8), Verdict::Fail(_)),
        "L8's count"
    );
    let two = seen("repo", "1 untracked file, 1 stash", "x", false);
    assert!(matches!(legs(&two, &cx(), L4), Verdict::Fail(_)));
    let open = seen("repo,open", "1 untracked file", "x", false);
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
    let hover = "driven by aterm (land: not landed yet, 1 untracked file)";
    read_window(&mut stale, 1_000, hover, "-");
    read_window(&mut stale, 40_000, hover, "-");
    assert_eq!(stale.window.page.len(), 1, "unchanged row was deduped");
    assert!(matches!(legs(&stale, &cx(), L4), Verdict::Fail(_)));
    read_window(
        &mut stale,
        71_000,
        hover,
        "not landed yet: 1 untracked file",
    );
    assert!(matches!(legs(&stale, &cx(), L4), Verdict::Pass(_)));
}

/// The line the first look read counts even when later reads draw the row with no state line.
#[test]
fn the_first_looks_state_line_counts_though_its_page_read_came_first() {
    let head = "head=land-c: Window 2, tab 4";
    let said = format!("{head}\nstate=not landed yet: 1 untracked file \u{b7} landing since 03:20");
    let mut bare = seen("repo", "1 untracked file", "x", false);
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

/// L6: the tall reply read whole, so its hold is its need alone.
#[test]
fn l6_is_its_need_alone() {
    let good = seen("needs", "needs you", "x", false);
    assert!(
        matches!(legs(&good, &cx(), L6), Verdict::Pass(_)),
        "{:?}",
        legs(&good, &cx(), L6)
    );
    let cut = seen(
        "needs,cut",
        "needs you; a reply too tall to read whole",
        "x",
        false,
    );
    assert!(matches!(legs(&cut, &cx(), L6), Verdict::Fail(_)));
}

/// L2: the hover and the state line name the words its nudge named, as it
/// named them (a word with its own comma split alike on both), never in
/// quotation marks.
#[test]
fn l2_names_the_words_its_nudge_named() {
    let named = "open work in its report: still open, approved, not landed";
    let good = seen("open", named, "still open, approved, not landed", false);
    assert!(
        matches!(legs(&good, &cx(), L2), Verdict::Pass(_)),
        "{:?}",
        legs(&good, &cx(), L2)
    );
    let other = seen("open", named, "follow-up", false);
    assert!(matches!(legs(&other, &cx(), L2), Verdict::Fail(_)));
    let quoted = seen(
        "open",
        "\u{201c}still open\u{201d} in its report",
        "still open",
        false,
    );
    assert!(
        matches!(legs(&quoted, &cx(), L2), Verdict::Fail(_)),
        "{:?}",
        legs(&quoted, &cx(), L2)
    );
}

/// A HOVER PUSHED BEFORE THE HOLD AND NEVER AGAIN is a stale push, failed:
/// the strip still shows `driven by aterm (land)` ten seconds and more after
/// `status` read the hold, while it stands.
#[test]
fn a_hover_pushed_before_the_hold_and_never_again_fails() {
    let mut stale = seen("repo", "1 untracked file", "x", false);
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
