// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the hold said (§14.2's 2a) over records and reads recorded on main `b42ae72fc3`.

use super::*;
use crate::forever::surfaces::Window;
use crate::forever::{Chain, Row, Scenario, Seen, Verdict};

/// The first prompt (unix ms).
const T0: i64 = 1_792_867_220_000;

/// A journal line at `t` ms after [`T0`].
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
        reason: "the turn-end policy (short turns in a row: 2)".into(),
    }
}

/// The hold's journal line as `b42ae72fc3` wrote it (`Exception::repeats`).
const STALL: &str = "EXCEPTION seq=2681 kind=stalled when=later Its program answers aterm's acts \
                     with the same reply and no work, so aterm types nothing more. => Open the \
                     tab and do what its last reply asks.";

/// When the hold was journaled, as land-c's was on `b42ae72fc3` (+372.0 s).
const HELD_AT: i64 = 372_000;

/// land-c's journal on `b42ae72fc3` (`lab forever --harness claude --mode land`, the strict
/// lens's run): land@v1 at +124.3 s, its defense, the nudge at +368.4 s, the same defense, the
/// hold at +372.0 s.
fn land_c_journal() -> Vec<(i64, String)> {
    vec![
        j(
            3_700,
            "EVENT idle seq=882 ⏺ Implemented the parser module and its unit tests in this turn.",
        ),
        j(
            3_700,
            "WAITING seq=882 until=2026-10-24T18:42:25Z 1 short turn(s) in a row: 2 min back-off",
        ),
        j(
            124_300,
            "CONTINUED seq=882 rule=land@v1 [aterm harness] Autopilot is Land: land ALL of this work",
        ),
        j(
            127_800,
            "EVENT idle seq=1791 ⏺ Nothing left: the parser is committed and pushed to origin/main.",
        ),
        j(
            127_800,
            "WAITING seq=1791 until=2026-10-24T18:46:29Z 2 short turn(s) in a row: 4 min back-off",
        ),
        j(
            368_400,
            "CONTINUED seq=1791 rule=land-nudge@v1 [aterm harness] Still landing: get everything onto the remote main branch",
        ),
        j(
            HELD_AT,
            "EVENT idle seq=2681 ⏺ Nothing left: the parser is committed and pushed to origin/main.",
        ),
        j(HELD_AT, STALL),
        j(HELD_AT, "HOLD seq=2681 point nothing why=policy"),
    ]
}

/// land-c's chip in front on `b42ae72fc3`, past its hold (END's sweep of that run, +999.1 s): the
/// hold's words, no stop word.
const MAIN_LINE: &str = "window=2 program=claude face=land drawn=\"⤓ land\" spoken=\"Autopilot: \
     land.\" description=\"Not landed yet. 1 untracked file. Landing since 11:42, 1 nudge. Last: \
     nudged at 11:46.\" title=\"autopilot: land — not landed yet: 1 untracked file · landing \
     since 11:42, 1 nudge · last: ⤓ nudged 11:46, ⤓ landing 11:42 · click: off · right-click: \
     more\"";

/// The same chip as §14.3 words it once 3a says the hold: description and title led by the stop
/// ([`stop_heads`]), the hold's words after it.
fn said_line() -> String {
    let [spoken, titled, _] = stop_heads("land", HeldBy::SameReply);
    format!(
        "window=2 program=claude face=land drawn=\"⤓ land\" spoken=\"Autopilot: land.\" \
         description=\"{spoken} Not landed yet. 1 untracked file. Landing since 11:42, 1 nudge.\" \
         title=\"{titled} · not landed yet: 1 untracked file · landing since 11:42, 1 nudge · \
         click: off · right-click: more\""
    )
}

/// A footerless program's line in front (Kimi Code draws no footer): no chip and no title.
const FOOTLESS: &str =
    "window=2 program=kimi face=- drawn=- why=no-footer spoken=- description=- title=-";

/// The hover past the hold, as `b42ae72fc3` drew it and as §14.3 words it.
const MAIN_HOVER: &str = "driven by aterm (land: not landed yet, 1 untracked file)";
fn said_hover() -> String {
    let [_, _, hovered] = stop_heads("land", HeldBy::SameReply);
    format!("{hovered}; not landed yet, 1 untracked file)")
}

/// What 3a journals, prints and shows of `seen`'s hold in `chain`: its line names its arm
/// (`held=same-reply`) and is `when=now` where the hold is loud ([`loud`]), `status` prints
/// `held=same-reply`, the Harness page's state line (where the window is read) leads with the
/// stop, and `ctl messages` stands one `Stopped typing` row where it is loud, none otherwise:
/// each read every 2.5 s from a second past the hold to the watch's end.
pub(in crate::forever) fn three_a_said(mut seen: Seen, chain: Chain) -> Seen {
    let (ts, line) = stall_of(&seen, i64::MIN).cloned().expect("a hold");
    let when = if loud(chain) { "now" } else { "later" };
    let (head, rest) = line.split_once(" when=").expect("when=");
    let why = rest.split_once(' ').map_or("", |(_, w)| w);
    let said = format!("{head} when={when} held={} {why}", HeldBy::SameReply.word());
    for (_, l) in &mut seen.journal {
        if *l == line {
            l.clone_from(&said);
        }
    }
    let t = ts + 1_000;
    let reads: Vec<i64> = (0..)
        .map(|i| t + i * 2_500)
        .take_while(|r| *r < seen.end_ms)
        .collect();
    let last = seen
        .status
        .last()
        .map_or_else(String::new, |(_, s)| s.clone());
    seen.status.push((t, format!("{last} held=same-reply")));
    seen.status_reads.extend(&reads);
    let band = if loud(chain) {
        format!(
            "m-7 state=live title={} in window 2, tab 3",
            REPEATS_LAND_TITLE.0
        )
    } else {
        "-".to_string()
    };
    seen.band.push((t, band));
    seen.band_reads.extend(&reads);
    if seen.window.read {
        seen.window.page.push((
            t,
            format!(
                "head={}: Window 2, tab 3\nstate={} · not landed yet: 1 untracked file",
                chain.name(),
                stop_words(HeldBy::SameReply, StopForm::Line)
            ),
        ));
        seen.window.page_reads.extend(&reads);
    }
    seen.window.held_ax = seen
        .window
        .held
        .iter()
        .map(|(t, l)| (*t, nodes_heard(l, chain)))
        .collect();
    seen
}

/// The loud hold's band row as a screen reader is handed it (`chrome`'s `a11y` line, its head cut).
const BAND_NODE: &str = "window=2 band row=0 role=Alert label=\"aterm stopped typing in window 2, \
                         tab 3 \u{b7} Open the tab and tell it what is still open.\" description=-";

/// What a screen reader is handed at a look whose chip `chrome` read as `line` ([`ax_of`]'s
/// lines): the chip's node saying its description, and where `chain`'s hold is loud, its band row.
fn nodes_heard(line: &str, chain: Chain) -> String {
    let description = quoted(line, "description").map_or_else(|| "-".into(), |d| format!("{d:?}"));
    let chip = format!(
        "window=2 autopilot sid=s-c role=Button label=\"Autopilot: land.\" \
         description={description}"
    );
    if loud(chain) {
        format!("{BAND_NODE}\n{chip}")
    } else {
        chip
    }
}

/// land-c's records with its looks in front at +382 s and +412 s each reading `line`, and the
/// hover reading `hover` from +130.3 s on, read every 2.5 s to +432 s.
fn held(chain: Chain, line: &str, hover: &str) -> Seen {
    let reads: Vec<i64> = (0..=120).map(|i| T0 + 130_300 + i * 2_500).collect();
    Seen {
        journal: land_c_journal(),
        ledger: vec![
            typed(124_300, "land@v1", "[aterm harness] Autopilot is Land:"),
            typed(368_400, "land-nudge@v1", "[aterm harness] Still landing:"),
        ],
        end_ms: T0 + HELD_AT + 61_000,
        window: Window {
            read: true,
            label: chain.name().into(),
            hover: vec![j(17_000, "driven by aterm (land)"), j(130_300, hover)],
            hover_reads: reads.into_iter().filter(|t| *t < T0 + 433_000).collect(),
            held: vec![j(HELD_AT + 10_100, line), j(HELD_AT + 40_200, line)],
            ..Window::default()
        },
        ..Seen::default()
    }
}

fn cx(chain: Chain, program: &'static str) -> Ctx<'static> {
    Ctx {
        chain,
        program,
        t0: T0,
    }
}

fn stall(seen: &Seen) -> &(i64, String) {
    stall_of(seen, T0).expect("the hold")
}

/// On `b42ae72fc3` the chip in front past the hold says the hold's words and no stop word: HELD,
/// NOT SAID. With §14.3's words it is said. The negative control of the window, first (limit 8).
#[test]
fn a_held_chip_in_front_must_say_stopped_typing() {
    let main = held(Chain::LandC, MAIN_LINE, MAIN_HOVER);
    let v = said(&main, &cx(Chain::LandC, "claude"), stall(&main));
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    assert!(v.why().contains("HELD, NOT SAID"), "{v:?}");
    assert!(
        v.why().contains("the description \"Not landed yet."),
        "{v:?}"
    );
    assert!(v.why().contains("+382.1 s in front"), "{v:?}");
    let ok = held(Chain::LandC, &said_line(), MAIN_HOVER);
    let v = said(&ok, &cx(Chain::LandC, "claude"), stall(&ok));
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let title_only = said_line().replace(&stop_words(HeldBy::SameReply, StopForm::Spoken), "");
    let half = held(Chain::LandC, &title_only, MAIN_HOVER);
    let v = said(&half, &cx(Chain::LandC, "claude"), stall(&half));
    assert!(v.why().contains("the description"), "{v:?}");
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    let mut dropped = held(Chain::LandC, &said_line(), MAIN_HOVER);
    dropped.window.held[1] = j(HELD_AT + 40_200, MAIN_LINE);
    let v = said(&dropped, &cx(Chain::LandC, "claude"), stall(&dropped));
    assert!(
        matches!(v, Verdict::Fail(_)),
        "said once, dropped by the second look: {v:?}"
    );
}

/// Where no chip is drawn (a composer with no footer, as Kimi Code's) the hover says it, every
/// read of it past the settle.
#[test]
fn where_no_chip_is_drawn_the_hover_says_it() {
    let main = held(Chain::LandC, FOOTLESS, MAIN_HOVER);
    let v = said(&main, &cx(Chain::LandC, "kimi"), stall(&main));
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    assert!(
        v.why().contains("the hover \"driven by aterm (land: not"),
        "{v:?}"
    );
    let ok = held(Chain::LandC, FOOTLESS, &said_hover());
    let v = said(&ok, &cx(Chain::LandC, "kimi"), stall(&ok));
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let mut stale = held(Chain::LandC, FOOTLESS, &said_hover());
    stale.window.hover.push(j(HELD_AT + 30_000, MAIN_HOVER));
    let v = said(&stale, &cx(Chain::LandC, "kimi"), stall(&stale));
    assert!(matches!(v, Verdict::Fail(_)), "the words went stale: {v:?}");
    let mut early = held(Chain::LandC, FOOTLESS, MAIN_HOVER);
    early.window.hover.push(j(HELD_AT + 5_000, &said_hover()));
    let v = said(&early, &cx(Chain::LandC, "kimi"), stall(&early));
    assert!(
        matches!(v, Verdict::Pass(_)),
        "the read before the settle is not judged: {v:?}"
    );
    let mut unhovered = held(Chain::LandC, FOOTLESS, &said_hover());
    unhovered.window.hover_reads.clear();
    unhovered.window.hover.truncate(1);
    let v = said(&unhovered, &cx(Chain::LandC, "kimi"), stall(&unhovered));
    assert!(matches!(v, Verdict::Unobserved(_)), "{v:?}");
}

/// A look before the settle, after the lift, or none within the minute reads nothing.
#[test]
fn only_the_looks_while_held_and_within_the_minute_count() {
    let mut none = held(Chain::LandC, MAIN_LINE, MAIN_HOVER);
    none.window.held.clear();
    let v = said(&none, &cx(Chain::LandC, "claude"), stall(&none));
    assert!(matches!(v, Verdict::Unobserved(_)), "{v:?}");
    let mut early = held(Chain::LandC, MAIN_LINE, MAIN_HOVER);
    early.window.held = vec![j(HELD_AT + 2_000, MAIN_LINE)];
    let v = said(&early, &cx(Chain::LandC, "claude"), stall(&early));
    assert!(
        matches!(v, Verdict::Unobserved(_)),
        "before the settle: {v:?}"
    );
    let mut late = held(Chain::LandC, &said_line(), MAIN_HOVER);
    late.window.held = vec![j(HELD_AT + 70_000, &said_line())];
    let v = said(&late, &cx(Chain::LandC, "claude"), stall(&late));
    assert!(
        matches!(v, Verdict::Unobserved(_)),
        "past the minute: {v:?}"
    );
    let mut lifted = held(Chain::LandC, MAIN_LINE, MAIN_HOVER);
    lifted.journal.push(j(
        HELD_AT + 5_000,
        "CLEARED seq=2700 exception=stalled after=5s a different reply",
    ));
    let v = said(&lifted, &cx(Chain::LandC, "claude"), stall(&lifted));
    assert!(matches!(v, Verdict::Unobserved(_)), "lifted: {v:?}");
    let mut missing = held(Chain::LandC, "-", MAIN_HOVER);
    missing.window.held.truncate(1);
    let v = said(&missing, &cx(Chain::LandC, "claude"), stall(&missing));
    assert!(matches!(v, Verdict::Unobserved(_)), "{v:?}");
}

/// The hold's wants fold into a same-reply chain's last scenario where a hold was journaled, and
/// nowhere else. On `b42ae72fc3`'s records the chip says no stop and 3a's fields are missing; a
/// chip that says it with main's journal still fails 3a's; with 3a's records it passes.
#[test]
fn the_wants_fold_into_a_same_reply_chains_last_scenario() {
    let main = held(Chain::LandC, MAIN_LINE, MAIN_HOVER);
    let pass = || Verdict::Pass("its own judge".into());
    let v = with_held(Scenario::L4, &main, &cx(Chain::LandC, "claude"), pass());
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    assert!(v.why().contains(SAID) && v.why().contains(ADDS), "{v:?}");
    let chip_only = held(Chain::LandC, &said_line(), MAIN_HOVER);
    let v = with_held(
        Scenario::L4,
        &chip_only,
        &cx(Chain::LandC, "claude"),
        pass(),
    );
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    assert!(
        !v.why().contains("HELD, NOT SAID")
            && v.why().contains(
                "its journal line reads when=later held=(none), not when=now held=same-reply"
            ),
        "{v:?}"
    );
    let ok = three_a_said(chip_only, Chain::LandC);
    let v = with_held(Scenario::L4, &ok, &cx(Chain::LandC, "claude"), pass());
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    assert!(
        v.why().contains("journaled when=now held=same-reply")
            && v.why().contains("one `Stopped typing` row"),
        "{v:?}"
    );
    let v = with_held(Scenario::C1, &main, &cx(Chain::Cruise, "claude"), pass());
    assert!(
        matches!(v, Verdict::Pass(_)),
        "not its last scenario: {v:?}"
    );
    let v = with_held(Scenario::C2, &main, &cx(Chain::Cruise, "claude"), pass());
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    assert!(v.why().contains("HELD, NOT SAID"), "{v:?}");
    let v = with_held(Scenario::L1, &main, &cx(Chain::LandA, "claude"), pass());
    assert!(matches!(v, Verdict::Pass(_)), "no same-reply chain: {v:?}");
    let mut unheld = main.clone();
    unheld.journal.retain(|(_, l)| !is_stall(l));
    let v = with_held(Scenario::L4, &unheld, &cx(Chain::LandC, "claude"), pass());
    assert!(matches!(v, Verdict::Pass(_)), "no hold: {v:?}");
    let mut blind = main;
    blind.window.read = false;
    let v = with_held(Scenario::L4, &blind, &cx(Chain::LandC, "claude"), pass());
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("SKIPPED (--no-surfaces)"),
        "main's journal fails under --no-surfaces too: {v:?}"
    );
    let mut blind = ok;
    blind.window.read = false;
    let v = with_held(Scenario::L4, &blind, &cx(Chain::LandC, "claude"), pass());
    assert!(
        matches!(v, Verdict::Pass(_))
            && v.why()
                .contains(&format!("{SAID}: SKIPPED (--no-surfaces)"))
            && v.why().contains("the state line: SKIPPED (--no-surfaces)"),
        "a skip keeps the word: {v:?}"
    );
}

/// WHAT 3A ADDS: the hold's line names its arm and is `when=now` only where the hold is loud,
/// every `status` read prints `held=`, every state line read leads with the stop, and the loud
/// hold alone stands one `Stopped typing` row in `ctl messages`. Each field missing or wrong is
/// the others' negative control.
#[test]
fn three_a_wants_the_arm_journaled_and_printed_and_one_row_for_the_loud_hold_alone() {
    let ok = three_a_said(held(Chain::LandC, &said_line(), MAIN_HOVER), Chain::LandC);
    let judge = |s: &Seen, c: Chain| three_a(s, &cx(c, "claude"), stall(s));
    let v = judge(&ok, Chain::LandC);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let fails = |s: &Seen, c: Chain, why: &str| {
        let v = judge(s, c);
        assert!(
            matches!(v, Verdict::Fail(_)) && v.why().contains(why),
            "{why}: {v:?}"
        );
    };
    let mut later = ok.clone();
    for (_, l) in &mut later.journal {
        *l = l.replace(" when=now ", " when=later ");
    }
    fails(
        &later,
        Chain::LandC,
        "reads when=later held=same-reply, not when=now",
    );
    let mut unnamed = ok.clone();
    for (_, l) in &mut unnamed.journal {
        *l = l.replace(" held=same-reply", "");
    }
    fails(&unnamed, Chain::LandC, "held=(none)");
    let mut unknown = ok.clone();
    for (_, l) in &mut unknown.journal {
        *l = l.replace(" held=same-reply", " held=bogus");
    }
    fails(
        &unknown,
        Chain::LandC,
        "reads when=now held=bogus, not when=now held=same-reply",
    );
    let mut unprinted = ok.clone();
    unprinted
        .status
        .push((T0 + HELD_AT + 20_000, "task=open held=-".into()));
    fails(
        &unprinted,
        Chain::LandC,
        "status did not print held=same-reply",
    );
    let mut unled = ok.clone();
    let line = unled.window.page.pop().expect("its state line").1;
    let lead = stop_words(HeldBy::SameReply, StopForm::Line);
    unled.window.page.push((
        T0 + HELD_AT + 1_000,
        line.replace(&format!("{lead}{FACT_SEPARATOR}"), ""),
    ));
    fails(&unled, Chain::LandC, "leads with no stop");
    let mut below = ok.clone();
    below.window.page.push((T0 + HELD_AT + 1_500, "-".into()));
    let v = judge(&below, Chain::LandC);
    assert!(
        matches!(v, Verdict::Pass(_))
            && v.why()
                .contains("the state line: SKIPPED (its row was not drawn on the Harness page"),
        "a row the page never drew while it held: {v:?}"
    );
    let mut stateless = ok.clone();
    stateless
        .window
        .page
        .push((T0 + HELD_AT + 1_500, "head=land-c: Window 2, tab 3".into()));
    fails(
        &stateless,
        Chain::LandC,
        "its row on the Harness page drew no state line",
    );
    let mut unpaged = ok.clone();
    unpaged.window.page_reads.clear();
    unpaged.window.page.clear();
    let v = judge(&unpaged, Chain::LandC);
    assert!(
        matches!(v, Verdict::Unobserved(_))
            && v.why()
                .contains("the Harness page was not read while it held"),
        "{v:?}"
    );
    let mut rowless = ok.clone();
    rowless.band = vec![(T0 + HELD_AT + 1_000, "-".into())];
    fails(
        &rowless,
        Chain::LandC,
        "stood 0 `Stopped typing` row(s), not one",
    );
    let mut withdrawn = ok.clone();
    withdrawn.band[0].1 = withdrawn.band[0].1.replace("state=live", "state=withdrawn");
    fails(&withdrawn, Chain::LandC, "stood 0 `Stopped typing` row(s)");
    let mut twice = ok.clone();
    twice.band[0].1 = format!("{0} | {0}", twice.band[0].1);
    fails(&twice, Chain::LandC, "stood 2 `Stopped typing` row(s)");
    let quiet = three_a_said(held(Chain::LandB, &said_line(), MAIN_HOVER), Chain::LandB);
    let v = judge(&quiet, Chain::LandB);
    assert!(
        matches!(v, Verdict::Pass(_))
            && v.why().contains("journaled when=later held=same-reply")
            && v.why().contains("no `Stopped typing` row"),
        "{v:?}"
    );
    let mut loud_row = quiet.clone();
    loud_row.band = ok.band.clone();
    fails(
        &loud_row,
        Chain::LandB,
        "a `Stopped typing` row for a hold that is not Land's",
    );
    let mut unread = ok;
    unread.status_reads.clear();
    unread.status.pop();
    let v = judge(&unread, Chain::LandC);
    assert!(
        matches!(v, Verdict::Unobserved(_)) && v.why().contains("no `status` read while it held"),
        "{v:?}"
    );
}

/// WHAT A SCREEN READER IS HANDED (§14.3's windowed read): at each look whose chip was drawn, the
/// chip's node in the window's accessibility tree says the description `chrome` read, led by the
/// stop, and the loud hold's band row node names its row's title. Each node missing or wrong is
/// the others' negative control; a band whose three rows name other holds is a named skip, and a
/// build that prints no `a11y` node is unread.
#[test]
fn three_a_wants_the_chips_node_to_say_the_stop_and_the_loud_holds_band_row_node_to_name_it() {
    let ok = three_a_said(held(Chain::LandC, &said_line(), MAIN_HOVER), Chain::LandC);
    let judge = |s: &Seen| three_a(s, &cx(Chain::LandC, "claude"), stall(s));
    let v = judge(&ok);
    assert!(
        matches!(v, Verdict::Pass(_))
            && v.why().contains("the chip's node says \"Stopped typing:")
            && v.why()
                .contains("a band row's node says \"aterm stopped typing in window 2, tab 3"),
        "{v:?}"
    );
    let fails = |s: &Seen, why: &str| {
        let v = judge(s);
        assert!(
            matches!(v, Verdict::Fail(_)) && v.why().contains(why),
            "{why}: {v:?}"
        );
    };
    let mut stale = ok.clone();
    stale.window.held_ax[1].1 = nodes_heard(MAIN_LINE, Chain::LandC);
    fails(
        &stale,
        "the chip's node says \"Not landed yet. 1 untracked file.",
    );
    let mut nodeless = ok.clone();
    nodeless.window.held_ax[0].1 = BAND_NODE.to_string();
    fails(
        &nodeless,
        "its chip was drawn, yet the window's accessibility tree held no node for it",
    );
    let mut bandless = ok.clone();
    bandless.window.held_ax[1].1 = nodes_heard(&said_line(), Chain::LandB);
    fails(
        &bandless,
        "no band row node names \"aterm stopped typing in window 2, tab 3\" (0 row node(s))",
    );
    let mut crowded = ok.clone();
    let others: Vec<String> = (4..7)
        .map(|tab| BAND_NODE.replace("tab 3", &format!("tab {tab}")))
        .collect();
    crowded.window.held_ax[1].1 = format!(
        "{}\n{}",
        others.join("\n"),
        nodes_heard(&said_line(), Chain::LandB)
    );
    let v = judge(&crowded);
    assert!(
        matches!(v, Verdict::Pass(_))
            && v.why()
                .contains("the band's node: SKIPPED (+412.2 s in front: its three rows named"),
        "{v:?}"
    );
    let mut unprinted = ok.clone();
    unprinted.window.held_ax[0].1 = "-".to_string();
    let v = judge(&unprinted);
    assert!(
        matches!(v, Verdict::Unobserved(_))
            && v.why().contains(
                "+382.1 s in front: `chrome` printed no `a11y` node, so the screen reader's was \
                 not read"
            ),
        "{v:?}"
    );
    let quiet = three_a_said(held(Chain::LandB, &said_line(), MAIN_HOVER), Chain::LandB);
    let v = three_a(&quiet, &cx(Chain::LandB, "claude"), stall(&quiet));
    assert!(
        matches!(v, Verdict::Pass(_))
            && v.why().contains("the chip's node says")
            && !v.why().contains("a band row's node"),
        "a quiet hold wants no band row node: {v:?}"
    );
}

/// `ax_of` keeps `chrome`'s `a11y` lines for the tab's own chip node and every band row node, its
/// head cut; another tab's chip node, other lines and a `chrome` with no `a11y` line read `-`.
#[test]
fn the_screen_readers_nodes_are_the_tabs_chip_and_the_band_rows() {
    let chrome = "OK 6\n\
         autopilot sid=s-c window=2 program=claude face=land drawn=\"⤓ land\"\n\
         a11y window=2 band row=0 role=Alert label=\"aterm stopped typing\" description=-\n\
         a11y window=2 autopilot sid=s-d role=Button label=\"Autopilot: land.\" description=-\n\
         a11y window=2 autopilot sid=s-c role=Button label=\"Autopilot: land.\" description=\"x\"\n\
         statusitem title=❯ needs=0\n\
         presence rim=none pressing=- pill=\"\"";
    assert_eq!(
        ax_of(chrome, "s-c"),
        "window=2 band row=0 role=Alert label=\"aterm stopped typing\" description=-\n\
         window=2 autopilot sid=s-c role=Button label=\"Autopilot: land.\" description=\"x\""
    );
    assert_eq!(ax_of("OK 1\npresence rim=none", "s-c"), "-");
    assert_eq!(
        ax_of(chrome, "s-e"),
        "window=2 band row=0 role=Alert label=\"aterm stopped typing\" description=-"
    );
}

/// THE ONE LOUD HOLD is Land's over a repository leg in a session no manager waits on: the
/// repository chains and same-land; land-b's open task, land-d's need, land-e's manager and the
/// Cruise chains are not.
#[test]
fn the_loud_chains_are_lands_over_a_repository_leg_no_manager_waits_on() {
    for c in [
        Chain::LandC,
        Chain::LandF,
        Chain::LandG,
        Chain::LandH,
        Chain::LandI,
        Chain::SameLand,
    ] {
        assert!(loud(c), "{c:?}");
    }
    for c in [
        Chain::LandB,
        Chain::LandD,
        Chain::LandE,
        Chain::LandT,
        Chain::Cruise,
        Chain::WroteC,
        Chain::SameCruise,
    ] {
        assert!(!loud(c), "{c:?}");
    }
}

/// same-land's person types their turn into the held tab once its looks were had and it was
/// watched past its hold, and only then; the turn lifts the hold and the row goes with it.
#[test]
fn same_lands_person_turn_lifts_the_hold_and_takes_its_row_away() {
    let mut watched = held(Chain::SameLand, &said_line(), MAIN_HOVER);
    watched.end_ms = T0 + HELD_AT + 120_000;
    let base = three_a_said(watched, Chain::SameLand);
    let past = T0 + HELD_AT + 60_000;
    assert!(person_due(Chain::SameLand, &base, T0, past));
    assert!(
        !person_due(Chain::SameLand, &base, T0, past - 1),
        "inside its watch"
    );
    assert!(
        !person_due(Chain::LandC, &base, T0, past),
        "same-land's alone"
    );
    let mut one_look = base.clone();
    one_look.window.held.truncate(1);
    assert!(
        !person_due(Chain::SameLand, &one_look, T0, past),
        "a look still due"
    );
    let mut unheld = base.clone();
    unheld.journal.retain(|(_, l)| !is_stall(l));
    assert!(!person_due(Chain::SameLand, &unheld, T0, past));
    let p = T0 + HELD_AT + 62_000;
    let mut typed = base.clone();
    typed.stop_person_ms = Some(p);
    assert!(
        !person_due(Chain::SameLand, &typed, T0, past + 60_000),
        "once"
    );
    let judge = |s: &Seen| three_a(s, &cx(Chain::SameLand, "claude"), stall(s));
    let v = judge(&base);
    assert!(
        matches!(v, Verdict::Unobserved(_)) && v.why().contains("no person's turn"),
        "{v:?}"
    );
    let v = judge(&typed);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("lifted no hold"),
        "{v:?}"
    );
    let mut lifted = typed.clone();
    lifted.journal.push((
        p + 400,
        "CLEARED seq=3001 exception=stalled after=62s a different reply, someone's turn, or the \
         agent working"
            .into(),
    ));
    let v = judge(&lifted);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("row stood to"),
        "the row outlived the hold: {v:?}"
    );
    let withdrawn = lifted.band[0].1.replace("state=live", "state=withdrawn");
    let mut gone = lifted.clone();
    gone.band.push((p + 2_000, withdrawn));
    let v = judge(&gone);
    assert!(
        matches!(v, Verdict::Pass(_)) && v.why().contains("the row gone by"),
        "{v:?}"
    );
    let mut unread = lifted;
    unread.band_reads.retain(|t| *t < p);
    let v = judge(&unread);
    assert!(
        matches!(v, Verdict::Unobserved(_)) && v.why().contains("not read past the lift"),
        "{v:?}"
    );
}

/// Same-land's chip may say `theirs` from its person's turn on, never before it and never on
/// another chain: the turn opened the person's own exchange (main's `30b7d2149f` read
/// `face=theirs` 12.8 s past it, once Claude Code answered it).
#[test]
fn same_lands_chip_may_be_theirs_from_its_persons_turn() {
    let behind = |face: &str| {
        format!("window=2 program=claude face={face} drawn=- why=not-drawn spoken=- description=-")
    };
    let p = T0 + HELD_AT + 62_000;
    let mut s = held(Chain::SameLand, &said_line(), MAIN_HOVER);
    s.window.chip = vec![j(2_300, &behind("land")), (p + 12_800, behind("theirs"))];
    let judge = |s: &Seen, chain: Chain| super::super::chip_faces(s, &cx(chain, "claude"));
    let v = judge(&s, Chain::SameLand);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("face=theirs (wanted [\"land\"])"),
        "no turn typed: {v:?}"
    );
    s.stop_person_ms = Some(p);
    let v = judge(&s, Chain::SameLand);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    assert!(
        matches!(judge(&s, Chain::LandC), Verdict::Fail(_)),
        "same-land's alone"
    );
    s.window.chip[1].0 = p - 1;
    assert!(
        matches!(judge(&s, Chain::SameLand), Verdict::Fail(_)),
        "theirs before the turn"
    );
}

/// `ctl messages tag=harness` as same-land's held tab printed it on this branch (the limit-6
/// read, `s-d1d1ee8fc5845d9a582e`): the resume ask's row while the hold stood, and after the
/// person's turn.
const LIVE_ROW: &str = "message 7 at=1792903362447 ago_ms=84963 tag=harness sev=warn origin=host \
     state=live glass=0 rep=1 key=harness.resume-ask.s-d1d1ee8fc5845d9a582e \
     title=aterm%20stopped%20typing%20in%20window%203,%20tab%203 \
     detail=Open%20the%20tab%20and%20tell%20it%20what%20is%20still%20open. actions=Show%20tab%203 \
     need=now";

/// A band's rows are read by the tab's key, their titles decoded; only a live one titled for the
/// stop stands for it.
#[test]
fn the_band_is_read_by_its_tabs_key_and_stands_only_while_live() {
    let sid = "s-d1d1ee8fc5845d9a582e";
    let other = LIVE_ROW.replace(sid, "s-0000000000000000aaaa");
    let messages = format!("{other}\n{LIVE_ROW}\nmessage 8 tag=harness key=harness.task.{sid}\n");
    let band = band_rows(&messages, sid);
    assert_eq!(
        band,
        "7 state=live title=aterm stopped typing in window 3, tab 3"
    );
    assert_eq!(stop_rows(&band), [band.as_str()]);
    assert_eq!(band_rows(&other, sid), "-");
    assert!(stop_rows("-").is_empty());
    let withdrawn = band_rows(&LIVE_ROW.replace("state=live", "state=withdrawn"), sid);
    assert!(stop_rows(&withdrawn).is_empty(), "{withdrawn}");
    let untitled = band_rows(
        &LIVE_ROW.replace("aterm%20stopped%20typing", "Claude%20needs%20you"),
        sid,
    );
    assert!(stop_rows(&untitled).is_empty(), "{untitled}");
    let two = format!("{LIVE_ROW}\n{}", LIVE_ROW.replace("message 7", "message 9"));
    assert_eq!(stop_rows(&band_rows(&two, sid)).len(), 2);
}

/// A reader of a hold's words reads them past a stop that leads them, in every form and arm, and
/// a text no stop leads as it is.
#[test]
fn the_holds_words_are_read_past_the_stop() {
    for arm in HeldBy::ALL {
        let line = format!(
            "{}{FACT_SEPARATOR}not landed yet: 1 untracked file",
            stop_words(arm, StopForm::Line)
        );
        assert_eq!(
            unstopped(&line, StopForm::Line),
            "not landed yet: 1 untracked file"
        );
        let title = format!(
            "autopilot: land \u{2014} {}{FACT_SEPARATOR}not landed yet: 1 untracked file",
            stop_words(arm, StopForm::Title)
        );
        assert_eq!(
            unstopped(&title, StopForm::Title),
            "autopilot: land \u{2014} not landed yet: 1 untracked file"
        );
    }
    assert_eq!(
        unstopped(&said_hover(), StopForm::Hover),
        "driven by aterm (land: not landed yet, 1 untracked file)"
    );
    let bare = format!(
        "driven by aterm (cruise: {})",
        stop_words(HeldBy::NoWork, StopForm::Hover)
    );
    assert_eq!(
        unstopped(&bare, StopForm::Hover),
        "driven by aterm (cruise)"
    );
    for (text, form) in [
        (MAIN_HOVER, StopForm::Hover),
        ("not landed yet: 1 untracked file", StopForm::Line),
        (
            "autopilot: land \u{2014} not landed yet: x",
            StopForm::Title,
        ),
        ("no head", StopForm::Title),
    ] {
        assert!(
            matches!(unstopped(text, form), Cow::Borrowed(t) if t == text),
            "{text}"
        );
    }
    let spoken = format!(
        "{} Not landed yet.",
        stop_words(HeldBy::SameReply, StopForm::Spoken)
    );
    assert_eq!(unstopped(&spoken, StopForm::Spoken), "Not landed yet.");
}

/// A hold in a chain whose script makes none fails it; a same-reply chain's and another mode's
/// chain's do not.
#[test]
fn a_hold_fails_a_chain_that_scripts_no_same_reply() {
    let main = held(Chain::LandA, MAIN_LINE, MAIN_HOVER);
    let f = stall_fault(Chain::LandA, &main, T0).expect("land-a's hold is a fault");
    assert!(f.contains("+372.0 s") && f.contains("kind=stalled"), "{f}");
    for c in [
        Chain::CruiseN,
        Chain::InstantCruise,
        Chain::LandW,
        Chain::Worker,
    ] {
        assert!(stall_fault(c, &main, T0).is_some(), "{c:?}");
    }
    for c in [Chain::LandB, Chain::LandC, Chain::Cruise, Chain::SameLand] {
        assert_eq!(stall_fault(c, &main, T0), None, "{c:?}");
    }
    assert_eq!(stall_fault(Chain::Ladder, &main, T0), None);
    assert_eq!(stall_fault(Chain::LandA, &main, T0 + HELD_AT + 1), None);
    assert!(is_stall(STALL));
    assert!(is_stall(
        "EXCEPTION seq=9 kind=stalled when=now held=same-reply Under Land it answered twice"
    ));
    assert!(!is_stall("EXCEPTION seq=40 kind=futile when=later x => y"));
    assert!(!is_stall("HOLD seq=2681 point nothing why=policy"));
}

/// The watch goes on past the last act's point by a minute: past the hold, once both looks were
/// had; past a `WAITING` where none came.
#[test]
fn the_watch_runs_a_minute_past_the_last_acts_point() {
    let main = held(Chain::LandC, MAIN_LINE, MAIN_HOVER);
    assert!(!watched_past(&main, T0 + HELD_AT + 59_000));
    assert!(watched_past(&main, T0 + HELD_AT + 60_000));
    let mut one_look = main.clone();
    one_look.window.held.truncate(1);
    assert!(!watched_past(&one_look, T0 + HELD_AT + 90_000));
    one_look.window.read = false;
    assert!(watched_past(&one_look, T0 + HELD_AT + 60_000));
    let mut waited = main.clone();
    waited.journal.truncate(6);
    assert!(!watched_past(&waited, T0 + HELD_AT + 600_000), "undecided");
    waited.journal.push(j(
        HELD_AT,
        "WAITING seq=2681 until=2026-10-24T18:54:29Z 3 short turn(s) in a row: 8 min back-off",
    ));
    assert!(!watched_past(&waited, T0 + HELD_AT + 59_000));
    assert!(watched_past(&waited, T0 + HELD_AT + 60_000));
    let mut quiet = main;
    quiet.journal.clear();
    quiet.ledger.clear();
    assert!(!watched_past(&quiet, T0 + 900_000));
}

/// HL: two acts answered alike, the hold at the second answer, nothing typed after it.
#[test]
fn hl_passes_on_the_hold_at_the_second_answer() {
    let main = held(Chain::SameLand, MAIN_LINE, MAIN_HOVER);
    let v = judge_hold(Chain::SameLand, &main, T0);
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    assert!(
        v.why().contains("land@v1 answered") && v.why().contains("held at +372.0 s"),
        "{v:?}"
    );
    let mut unheld = main.clone();
    unheld.journal.truncate(7);
    unheld.journal.push(j(
        HELD_AT,
        "WAITING seq=2681 until=2026-10-24T18:54:29Z 3 short turn(s) in a row: 8 min back-off",
    ));
    let v = judge_hold(Chain::SameLand, &unheld, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().starts_with("NOT HELD"),
        "{v:?}"
    );
    let mut soon = main.clone();
    soon.journal.drain(5..7);
    soon.journal.insert(4, j(127_800, STALL));
    let v = judge_hold(Chain::SameLand, &soon, T0);
    assert!(
        matches!(v, Verdict::Fail(_)),
        "held at the first answer: {v:?}"
    );
    let mut again = main.clone();
    again.journal.push(j(
        420_000,
        "CONTINUED seq=2681 rule=land-nudge@v1 [aterm harness] Still landing:",
    ));
    let v = judge_hold(Chain::SameLand, &again, T0);
    assert!(matches!(v, Verdict::Fail(_)), "typed past its hold: {v:?}");
    let mut landed = main.clone();
    landed
        .journal
        .push(j(373_000, "DONE seq=2681 rule=land@v1 x"));
    assert!(matches!(
        judge_hold(Chain::SameLand, &landed, T0),
        Verdict::Fail(_)
    ));
    let v = judge_hold(Chain::SameCruise, &main, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("not continue-done-check@v1"),
        "{v:?}"
    );
    let mut cold = main;
    cold.journal.truncate(2);
    assert!(matches!(
        judge_hold(Chain::SameLand, &cold, T0),
        Verdict::Fail(_)
    ));
}

/// The lift as land-d journaled it on `b42ae72fc3` (the review's `--mode both` run,
/// `s-c71e2740fe2fa36ac9d1`, on Gemini CLI before agy replaced it): its tab brought in front
/// 11.6 s past the hold, the agent redrew the tall reply, and aterm read its last row as a
/// different reply 4 s later. The stub read no
/// request of that conversation from the nudge (+370.3 s, `req-00082.json`) to the next one
/// (+856.6 s, `req-00102.json`).
const LIFT: &str = "CLEARED seq=388136 exception=stalled after=15s a different reply, someone's \
                    turn, or the agent working";

/// land-c's records with that lift 15.6 s past the hold, the stub's requests between `requests`.
fn lifted_held(line: &str, requests: Option<usize>) -> Seen {
    let mut s = held(Chain::LandC, line, MAIN_HOVER);
    s.journal.extend([
        j(HELD_AT + 15_600, "EVENT idle seq=388136 - check 80 passed"),
        j(HELD_AT + 15_600, LIFT),
        j(
            HELD_AT + 15_600,
            "WAITING seq=388136 until=2026-10-24T22:05:12Z 3 short turn(s) in a row: 8 min \
             back-off",
        ),
    ]);
    s.lift_stub = requests;
    s
}

/// The hold stands (limit 1): a lift on no request the stub read fails it, whatever the chip
/// says; a lift on a request is the agent working; one whose requests were not read is a named
/// skip; no lift, or the loop's own end, passes.
#[test]
fn a_lift_with_no_new_reply_fails_the_hold() {
    let footless = lifted_held(FOOTLESS, Some(0));
    let v = stands(&footless, &cx(Chain::LandC, "kimi"), stall(&footless));
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    assert!(
        v.why()
            .starts_with("HELD, THEN LIFTED WITH NO NEW REPLY at +387.6 s")
            && v.why()
                .contains("5.5 s after the look in front at +382.1 s")
            && v.why().contains("from the hold at +372.0 s"),
        "{v:?}"
    );
    let v = with_held(
        Scenario::L4,
        &lifted_held(&said_line(), Some(0)),
        &cx(Chain::LandC, "claude"),
        Verdict::Pass("its own judge".into()),
    );
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains(STANDS),
        "said, but lifted on nothing: {v:?}"
    );
    let mut theirs = lifted_held(&said_line(), Some(0));
    theirs.stop_person_ms = Some(T0 + HELD_AT + 15_000);
    let v = stands(&theirs, &cx(Chain::SameLand, "claude"), stall(&theirs));
    assert!(
        matches!(v, Verdict::Pass(_)) && v.why().contains("on the person's turn at +387.0 s"),
        "same-land's person lifted it: {v:?}"
    );
    theirs.stop_person_ms = Some(T0 + HELD_AT + 17_000);
    let v = stands(&theirs, &cx(Chain::SameLand, "claude"), stall(&theirs));
    assert!(
        matches!(v, Verdict::Fail(_)),
        "lifted before the person's turn: {v:?}"
    );
    let worked = lifted_held(&said_line(), Some(1));
    let v = stands(&worked, &cx(Chain::LandC, "claude"), stall(&worked));
    assert!(
        matches!(v, Verdict::Pass(_)) && v.why().contains("on 1 request(s)"),
        "{v:?}"
    );
    let unread = lifted_held(&said_line(), None);
    let v = stands(&unread, &cx(Chain::LandC, "claude"), stall(&unread));
    assert!(matches!(v, Verdict::Unobserved(_)), "{v:?}");
    let kept = held(Chain::LandC, &said_line(), MAIN_HOVER);
    let v = stands(&kept, &cx(Chain::LandC, "claude"), stall(&kept));
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let mut ended = kept;
    ended.journal.push(j(
        HELD_AT + 544_000,
        "CLEARED seq=0 exception=stalled after=544s the loop ended",
    ));
    assert_eq!(lift_of(&ended, T0 + HELD_AT), None);
    let mut acted = held(Chain::LandC, &said_line(), MAIN_HOVER);
    acted.journal.extend([
        j(
            HELD_AT + 20_000,
            "CONTINUED seq=2681 rule=land-nudge@v1 [aterm harness] Still landing:",
        ),
        j(HELD_AT + 25_000, LIFT),
    ]);
    assert_eq!(lift_of(&acted, T0 + HELD_AT), None, "past the next act");
    assert!(!is_lift(
        "CLEARED seq=2248 needs attention=OK none stands now"
    ));
}

/// Same-land's person's turn is theirs from their first key: aterm lifts the hold as they type
/// (`a person is typing`), seconds before the lab reads their Enter back (agy, +437.8 s against a
/// send at +443.2 s), so a lift between the two is the person's, and one before they typed is not.
#[test]
fn a_lift_while_the_person_types_is_theirs() {
    let mut typing = lifted_held(&said_line(), Some(0));
    person_typed(
        &mut typing,
        T0 + HELD_AT + 12_600,
        Some(T0 + HELD_AT + 18_600),
    );
    let v = stands(&typing, &cx(Chain::SameLand, "agy"), stall(&typing));
    assert!(
        matches!(v, Verdict::Pass(_))
            && v.why()
                .contains("on the person's turn at +384.6 s (sent at +390.6 s)"),
        "lifted at +387.6 s, while they typed: {v:?}"
    );
    let mut late = lifted_held(&said_line(), Some(0));
    person_typed(&mut late, T0 + HELD_AT + 16_600, None);
    let v = stands(&late, &cx(Chain::SameLand, "agy"), stall(&late));
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().starts_with("HELD, THEN LIFTED WITH NO NEW REPLY"),
        "lifted a second before they typed: {v:?}"
    );
}

/// The lift's witness counts the conversation's requests after the hold and by the lift: on the
/// review's run ([`LIFT`]), the nudge's before the hold and the next nudge's after the lift, so
/// none.
#[test]
fn the_lifts_witness_counts_the_requests_between_the_hold_and_the_lift() {
    let hold = 1_792_879_031_600;
    let lift = hold + 15_600;
    let dumps = [
        (1_792_878_656_000, 1),
        (1_792_878_780_000, 1),
        (1_792_879_026_000, 2),
        (1_792_879_512_000, 3),
    ];
    assert_eq!(requests_between(&dumps, hold, lift), 0);
    let worked = [(hold, 2), (hold + 4_000, 2), (lift, 2), (lift + 1, 2)];
    assert_eq!(requests_between(&worked, hold, lift), 2);
}

/// A look whose chip a footer draws but did not is unread, never judged on the hover: only the
/// footless line (`why=no-footer`) is.
#[test]
fn an_undrawn_chip_is_unread_never_judged_on_the_hover() {
    for why in ["not-drawn", "hidden", "switch-only"] {
        let line = format!(
            "window=2 program=claude face=land drawn=- why={why} spoken=- description=- title=-"
        );
        let s = held(Chain::LandC, &line, &said_hover());
        let v = said(&s, &cx(Chain::LandC, "claude"), stall(&s));
        assert!(
            matches!(v, Verdict::Unobserved(_))
                && v.why()
                    .contains(&format!("its chip was not drawn (why={why})")),
            "{why}: {v:?}"
        );
    }
    let s = held(Chain::LandC, FOOTLESS, &said_hover());
    let v = said(&s, &cx(Chain::LandC, "kimi"), stall(&s));
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
}

/// A look that read nothing says why: its label not in the strip, or no `autopilot` line.
#[test]
fn a_look_that_read_nothing_says_why() {
    let s = held(Chain::LandC, NOT_IN_STRIP, MAIN_HOVER);
    let v = said(&s, &cx(Chain::LandC, "claude"), stall(&s));
    assert!(
        matches!(v, Verdict::Unobserved(_))
            && v.why()
                .contains("its label was not in the front window's strip")
            && !v.why().contains("`autopilot` line"),
        "{v:?}"
    );
    let s = held(Chain::LandC, "-", MAIN_HOVER);
    let v = said(&s, &cx(Chain::LandC, "claude"), stall(&s));
    assert!(
        matches!(v, Verdict::Unobserved(_))
            && v.why().contains("`chrome` held no `autopilot` line for it")
            && !v.why().contains("strip"),
        "{v:?}"
    );
}

/// A look puts back the chains' window's front tab, or where the run has none the tab selected
/// before it (`chrome` then read once).
#[test]
fn a_look_puts_back_the_front_tab_or_the_one_selected_before() {
    let chrome = "toolbar-tabs count=2 selected=1 labels=[\"same-land\", \"same-cruise\"] \
                  icons=[None, None] states=[[], [\"selected\"]] tooltips=[None, None]";
    let mut reads = 0;
    let back = back_to(Some("cruise"), || {
        reads += 1;
        chrome.to_string()
    });
    assert_eq!((back.as_deref(), reads), (Some("cruise"), 0));
    assert_eq!(
        back_to(None, || chrome.to_string()).as_deref(),
        Some("same-cruise")
    );
    assert_eq!(back_to(None, String::new), None);
}

/// HL wants the two answers alike, word for word: a hold after two different answers fails.
#[test]
fn hl_fails_where_the_two_answers_differ() {
    let mut differ = held(Chain::SameLand, MAIN_LINE, MAIN_HOVER);
    differ.journal[3] = j(
        127_800,
        "EVENT idle seq=1791 ⏺ Wrote the last checks for the parser.",
    );
    let v = judge_hold(Chain::SameLand, &differ, T0);
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains("the two answers differ"),
        "{v:?}"
    );
}
