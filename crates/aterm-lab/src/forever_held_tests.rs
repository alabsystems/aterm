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

/// The same chip as §14.3 words it once 3a says the hold.
const SAID_LINE: &str = "window=2 program=claude face=land drawn=\"⤓ land\" spoken=\"Autopilot: \
     land.\" description=\"Stopped typing: the same reply twice, with no work. It types again at \
     your turn, or once the agent works. Not landed yet. 1 untracked file. Landing since 11:42, 1 \
     nudge.\" title=\"autopilot: land — stopped typing: the same reply twice, no work; types \
     again at your turn or once the agent works · not landed yet: 1 untracked file · landing \
     since 11:42, 1 nudge · click: off · right-click: more\"";

/// Gemini CLI's line in front: no footer, so no chip and no title.
const FOOTLESS: &str =
    "window=2 program=gemini face=- drawn=- why=no-footer spoken=- description=- title=-";

/// The hover past the hold, as `b42ae72fc3` drew it and as §14.3 words it.
const MAIN_HOVER: &str = "driven by aterm (land: not landed yet, 1 untracked file)";
const SAID_HOVER: &str = "driven by aterm (land: stopped typing; not landed yet, 1 untracked file)";

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
    let ok = held(Chain::LandC, SAID_LINE, MAIN_HOVER);
    let v = said(&ok, &cx(Chain::LandC, "claude"), stall(&ok));
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let title_only = SAID_LINE.replace("Stopped typing: ", "");
    let half = held(Chain::LandC, &title_only, MAIN_HOVER);
    let v = said(&half, &cx(Chain::LandC, "claude"), stall(&half));
    assert!(v.why().contains("the description"), "{v:?}");
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    let mut dropped = held(Chain::LandC, SAID_LINE, MAIN_HOVER);
    dropped.window.held[1] = j(HELD_AT + 40_200, MAIN_LINE);
    let v = said(&dropped, &cx(Chain::LandC, "claude"), stall(&dropped));
    assert!(
        matches!(v, Verdict::Fail(_)),
        "said once, dropped by the second look: {v:?}"
    );
}

/// Where no chip is drawn (Gemini CLI's composer has no footer) the hover says it, every read of
/// it past the settle.
#[test]
fn where_no_chip_is_drawn_the_hover_says_it() {
    let main = held(Chain::LandC, FOOTLESS, MAIN_HOVER);
    let v = said(&main, &cx(Chain::LandC, "gemini"), stall(&main));
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    assert!(
        v.why().contains("the hover \"driven by aterm (land: not"),
        "{v:?}"
    );
    let ok = held(Chain::LandC, FOOTLESS, SAID_HOVER);
    let v = said(&ok, &cx(Chain::LandC, "gemini"), stall(&ok));
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let mut stale = held(Chain::LandC, FOOTLESS, SAID_HOVER);
    stale.window.hover.push(j(HELD_AT + 30_000, MAIN_HOVER));
    let v = said(&stale, &cx(Chain::LandC, "gemini"), stall(&stale));
    assert!(matches!(v, Verdict::Fail(_)), "the words went stale: {v:?}");
    let mut early = held(Chain::LandC, FOOTLESS, MAIN_HOVER);
    early.window.hover.push(j(HELD_AT + 5_000, SAID_HOVER));
    let v = said(&early, &cx(Chain::LandC, "gemini"), stall(&early));
    assert!(
        matches!(v, Verdict::Pass(_)),
        "the read before the settle is not judged: {v:?}"
    );
    let mut unhovered = held(Chain::LandC, FOOTLESS, SAID_HOVER);
    unhovered.window.hover_reads.clear();
    unhovered.window.hover.truncate(1);
    let v = said(&unhovered, &cx(Chain::LandC, "gemini"), stall(&unhovered));
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
    let mut late = held(Chain::LandC, SAID_LINE, MAIN_HOVER);
    late.window.held = vec![j(HELD_AT + 70_000, SAID_LINE)];
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
/// nowhere else; 3a's fields are a named skip.
#[test]
fn the_wants_fold_into_a_same_reply_chains_last_scenario() {
    let main = held(Chain::LandC, MAIN_LINE, MAIN_HOVER);
    let pass = || Verdict::Pass("its own judge".into());
    let v = with_held(Scenario::L4, &main, &cx(Chain::LandC, "claude"), pass());
    assert!(matches!(v, Verdict::Fail(_)), "{v:?}");
    assert!(v.why().contains(SAID) && v.why().contains(ADDS), "{v:?}");
    let ok = held(Chain::LandC, SAID_LINE, MAIN_HOVER);
    let v = with_held(Scenario::L4, &ok, &cx(Chain::LandC, "claude"), pass());
    assert!(matches!(v, Verdict::Unobserved(_)), "{v:?}");
    assert!(
        v.why().contains("SKIPPED until 3a is on main")
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
    assert!(v.why().contains("no `Stopped typing` row"), "{v:?}");
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
        matches!(v, Verdict::Unobserved(_)) && v.why().contains("SKIPPED (--no-surfaces)"),
        "{v:?}"
    );
    let mut printed = ok;
    printed.status = vec![j(HELD_AT + 1_000, "task=open held=same-reply")];
    let v = three_a(&printed, &cx(Chain::LandB, "claude"), stall(&printed));
    assert!(
        matches!(v, Verdict::Unobserved(_))
            && v.why().contains("held=same-reply")
            && v.why().contains("no `Stopped typing` row"),
        "{v:?}"
    );
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

/// The lift as Gemini CLI's land-d journaled it on `b42ae72fc3` (the review's `--mode both` run,
/// `s-c71e2740fe2fa36ac9d1`): its tab brought in front 11.6 s past the hold, Gemini CLI redrew
/// the tall reply, and aterm read its last row as a different reply 4 s later. The stub read no
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
    let gemini = lifted_held(FOOTLESS, Some(0));
    let v = stands(&gemini, &cx(Chain::LandC, "gemini"), stall(&gemini));
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
        &lifted_held(SAID_LINE, Some(0)),
        &cx(Chain::LandC, "claude"),
        Verdict::Pass("its own judge".into()),
    );
    assert!(
        matches!(v, Verdict::Fail(_)) && v.why().contains(STANDS),
        "said, but lifted on nothing: {v:?}"
    );
    let worked = lifted_held(SAID_LINE, Some(1));
    let v = stands(&worked, &cx(Chain::LandC, "claude"), stall(&worked));
    assert!(
        matches!(v, Verdict::Pass(_)) && v.why().contains("on 1 request(s)"),
        "{v:?}"
    );
    let unread = lifted_held(SAID_LINE, None);
    let v = stands(&unread, &cx(Chain::LandC, "claude"), stall(&unread));
    assert!(matches!(v, Verdict::Unobserved(_)), "{v:?}");
    let kept = held(Chain::LandC, SAID_LINE, MAIN_HOVER);
    let v = stands(&kept, &cx(Chain::LandC, "claude"), stall(&kept));
    assert!(matches!(v, Verdict::Pass(_)), "{v:?}");
    let mut ended = kept;
    ended.journal.push(j(
        HELD_AT + 544_000,
        "CLEARED seq=0 exception=stalled after=544s the loop ended",
    ));
    assert_eq!(lift_of(&ended, T0 + HELD_AT), None);
    let mut acted = held(Chain::LandC, SAID_LINE, MAIN_HOVER);
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

/// The lift's witness counts the conversation's requests after the hold and by the lift: on the
/// review's Gemini CLI run, the nudge's before the hold and the next nudge's after the lift, so
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
        let s = held(Chain::LandC, &line, SAID_HOVER);
        let v = said(&s, &cx(Chain::LandC, "claude"), stall(&s));
        assert!(
            matches!(v, Verdict::Unobserved(_))
                && v.why()
                    .contains(&format!("its chip was not drawn (why={why})")),
            "{why}: {v:?}"
        );
    }
    let s = held(Chain::LandC, FOOTLESS, SAID_HOVER);
    let v = said(&s, &cx(Chain::LandC, "gemini"), stall(&s));
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
