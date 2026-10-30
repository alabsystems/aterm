// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for Cruise's carry at a break of the agent's own background work
//! (`Session::cruise_carry`) and the needs such a break's report names (`Session::break_needs`):
//! CRUISE NEVER PARKS (owner ruling 2026-10-26).

use super::*;
use crate::supervise::policy::cruise_carry::{BACKGROUND_CARRY, RULE_BACKGROUND};

/// A turn that ended with the agent's own dynamic workflows running, its report `said` (each
/// line under the first in the reply's gutter).
fn waiting(said: &[&str]) -> Vec<String> {
    waiting_under(&[], said)
}

/// [`waiting`] with `before`'s rows drawn above the report: the turn's earlier messages and its
/// tool rows.
fn waiting_under(before: &[&str], said: &[&str]) -> Vec<String> {
    let mut lines: Vec<String> = before.iter().map(ToString::to_string).collect();
    lines.extend(said.iter().enumerate().map(|(i, l)| {
        if i == 0 {
            format!("⏺ {l}")
        } else {
            format!("  {l}")
        }
    }));
    lines.extend(
        ["", "✻ Waiting for 2 dynamic workflows to finish", ""]
            .iter()
            .map(ToString::to_string),
    );
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut r = rows(&refs);
    r.extend(composer("  ⏵⏵ auto mode on · 2 workflows · ← for agents"));
    r
}

const PLAIN: &[&str] = &["Both workflows are running; I will pick up when they finish."];
const NEEDS: &[&str] = &[
    "Both workflows are running; the docs are next.",
    "NEEDS YOU: the release credential to sign 0.145.0",
];
const DONE: &[&str] = &[
    "Nothing left: the fix is on main and every check passed.",
    "ATERM-TASK-DONE",
];

/// The policy of the run: `autopilot`, the occupancy door at its default (`observe`, which types
/// nothing of its own).
fn under(autopilot: Autopilot) -> SupervisorConfig {
    SupervisorConfig {
        autopilot,
        continue_policy: true,
        ..policy(Occupancy::Observe)
    }
}

/// The breaks of one turn that ended on its background work, read at `at_s`, under `autopilot`,
/// the report `said`, the mock as `set` leaves it.
fn ran(
    tag: &str,
    autopilot: Autopilot,
    said: &'static [&'static str],
    at_s: &[i64],
    set: &dyn Fn(&mut Mock),
    before: Before<'_>,
) -> Ran {
    let drawn = move |_| waiting(said);
    Script {
        policy: under(autopilot),
        drawn: &drawn,
        set,
        ..Script::new(tag, Occupancy::Observe, at_s)
    }
    .run(before)
}

/// The carries the run typed ([`is_carry`]).
fn carries(ran: &Ran) -> usize {
    ran.requests.iter().filter(|(r, _)| is_carry(r)).count()
}

/// A `send` of [`BACKGROUND_CARRY`].
fn is_carry(request: &str) -> bool {
    request.starts_with("send ") && request.ends_with(&format!(" -- {BACKGROUND_CARRY}"))
}

/// The `meta set attention` requests the run made: the needs entry raised.
fn raised(ran: &Ran) -> Vec<String> {
    ran.requests()
        .into_iter()
        .filter(|r| r.starts_with("meta set attention "))
        .collect()
}

#[test]
fn cruise_carries_a_settled_background_break_and_off_and_land_type_nothing() {
    let cruise = ran(
        "carry-cruise",
        Autopilot::Cruise,
        PLAIN,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    assert_eq!(carries(&cruise), 1, "{:#?}", cruise.writes());
    let writes = cruise.writes();
    assert!(
        writes[0].starts_with("send if-human=7 if-gen=1."),
        "fenced on the person's last key and the screen: {writes:#?}"
    );
    assert!(
        writes[1].starts_with("key if-human=7 if-gen=1.") && writes[1].ends_with(" enter"),
        "{writes:#?}"
    );
    let requests = cruise.requests();
    let status = requests.iter().position(|r| r == "status").expect("status");
    let send = requests
        .iter()
        .position(|r| r.starts_with("send "))
        .expect("send");
    assert!(
        status < send,
        "the person's hands are read just before the write"
    );
    assert!(
        cruise
            .ledger
            .contains(&format!("\"rule_id\":\"{RULE_BACKGROUND}\"")),
        "{}",
        cruise.ledger
    );
    assert!(
        cruise.calls.iter().any(|c| c == "break_taken"),
        "the host hears the break is taken: {:?}",
        cruise.calls
    );
    for mode in [Autopilot::Off, Autopilot::Land] {
        let other = ran("carry-other", mode, PLAIN, &[0, 30], &|_| {}, &mut nothing);
        assert!(other.writes().is_empty(), "{mode:?}: {:#?}", other.writes());
        assert!(
            !other.requests().iter().any(|r| r == "status"),
            "{mode:?} reads nothing for a write it never makes: {:?}",
            other.requests()
        );
    }
}

#[test]
fn a_person_typing_or_their_draft_holds_the_carry() {
    let typing = ran(
        "carry-person",
        Autopilot::Cruise,
        PLAIN,
        &[0, 30],
        &|m| m.human_ms = Some(1_000),
        &mut nothing,
    );
    assert!(typing.writes().is_empty(), "{:#?}", typing.writes());
    assert!(
        typing.noted.iter().any(|l| l.starts_with("DEFERRED ")
            && l.contains(RULE_BACKGROUND)
            && l.ends_with("held:person")),
        "{:#?}",
        typing.noted
    );
    // A draft in the composer, the cursor after it, is the person's too.
    let drafted_run = Script {
        policy: under(Autopilot::Cruise),
        drawn: &|_| drafted(waiting(PLAIN), "wait, one more thing"),
        set: &|m| m.cursor_col = Some(22),
        ..Script::new("carry-draft", Occupancy::Observe, &[0, 30])
    }
    .run(&mut nothing);
    assert!(
        drafted_run.writes().is_empty(),
        "{:#?}",
        drafted_run.writes()
    );
    // The draft is the person's last key too: either guard holds it.
    assert!(
        drafted_run.noted.iter().any(|l| l.starts_with("DEFERRED ")
            && (l.ends_with("held:draft") || l.ends_with("held:person"))),
        "{:#?}",
        drafted_run.noted
    );
}

#[test]
fn a_needs_you_report_at_a_break_is_raised_and_the_work_is_carried() {
    let cruise = ran(
        "carry-needs",
        Autopilot::Cruise,
        NEEDS,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    let up = raised(&cruise);
    assert_eq!(up.len(), 1, "raised once, not at every read: {up:#?}");
    assert!(up[0].contains("the release credential"), "{up:#?}");
    assert_eq!(
        carries(&cruise),
        1,
        "and the work goes on: {:#?}",
        cruise.writes()
    );
    // Off types nothing and raises nothing.
    let off = ran(
        "carry-needs-off",
        Autopilot::Off,
        NEEDS,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    assert!(
        off.writes().is_empty() && raised(&off).is_empty(),
        "{:#?}",
        off.requests()
    );
    // A person typing holds the carry, never the needs entry.
    let typing = ran(
        "carry-needs-person",
        Autopilot::Cruise,
        NEEDS,
        &[0, 30],
        &|m| m.human_ms = Some(1_000),
        &mut nothing,
    );
    assert!(typing.writes().is_empty(), "{:#?}", typing.writes());
    assert_eq!(raised(&typing).len(), 1, "{:#?}", typing.requests());
}

#[test]
fn a_done_report_at_a_break_ends_nothing_under_cruise() {
    let cruise = ran(
        "carry-done",
        Autopilot::Cruise,
        DONE,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    assert_eq!(carries(&cruise), 1, "{:#?}", cruise.writes());
    let off = ran(
        "carry-done-off",
        Autopilot::Off,
        DONE,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    assert!(off.writes().is_empty(), "{:#?}", off.writes());
    let typing = ran(
        "carry-done-person",
        Autopilot::Cruise,
        DONE,
        &[0, 30],
        &|m| m.human_ms = Some(1_000),
        &mut nothing,
    );
    assert!(typing.writes().is_empty(), "{:#?}", typing.writes());
}

/// A carry the agent answers in seconds, its turn ending on the work again, is asked again on the
/// policy's short-turn ladder (2 min, then 4), never every read and never never.
#[test]
fn a_short_answer_backs_the_next_carry_off_on_the_ladder() {
    let cfg = under(Autopilot::Cruise);
    let ran = ran(
        "carry-ladder",
        Autopilot::Cruise,
        PLAIN,
        // The first carry at 30 s; the agent answers it at 40 s and its turn ends on the work
        // again: 60 s is short of the 2-minute back-off, 160 s past it; the next answer comes at
        // 170 s, and 300 s is short of 4 minutes after 160, 410 s past it.
        &[30, 40, 60, 160, 170, 300, 410],
        &|_| {},
        &mut |s, i| {
            if i == 1 || i == 4 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        },
    );
    assert_eq!(carries(&ran), 3, "{:#?}", ran.writes());
    assert!(
        ran.noted
            .iter()
            .any(|l| l.starts_with("DEFERRED ") && l.contains("(1 short answer(s) in a row)")),
        "{:#?}",
        ran.noted
    );
}

/// The session's own mode, switched from the chip since the loop last read it, is read afresh
/// just before the carry is typed: Land or Off there types nothing, Cruise there types it.
#[test]
fn a_switch_since_the_loops_last_read_is_read_before_the_carry() {
    for (word, typed) in [("off", 0), ("land", 0), ("cruise", 1)] {
        let meta = format!(
            "OK title=- attention=- cwd=- state=idle questions=- autopilot={word} land_at=-\n"
        );
        let set = move |m: &mut Mock| {
            m.verb_replies
                .entry("meta")
                .or_default()
                .push_back(ok(&meta));
        };
        let run = ran(
            "carry-switched",
            Autopilot::Cruise,
            PLAIN,
            &[0, 30],
            &set,
            &mut nothing,
        );
        let requests = run.requests();
        assert_eq!(carries(&run), typed, "{word}: {requests:#?}");
        let status = requests.iter().position(|r| r == "status");
        let read = requests.iter().rposition(|r| r == "meta");
        assert!(
            status.is_some() && read > status,
            "{word}: the mode is read after the hands, before the write: {requests:#?}"
        );
    }
}

/// What a read of [`carried_over`] draws, and whose hand moved the agent there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// A break of its background work.
    Break,
    /// A break no person's count is served on.
    Uncounted,
    /// It takes the carry typed last, and the break its short answer ends on is drawn.
    Taken,
    /// Its turn runs, drawn live.
    Live,
    /// The program's own wake opens a turn nobody typed, drawn live under the carry's prompt row:
    /// the person's count and their last key stand.
    Woke,
    /// The person's message opens a turn, or lands in the one running: their key 2 s before the
    /// read and their count past the carry's from then on.
    Theirs,
    /// A break the person scrolls back to read: their gesture 2 s before the read and their count
    /// past the carry's from then on, and no message.
    Scrolled,
    /// A message no key in this window typed (Claude Code's Remote Control) opens a turn, its
    /// prompt row drawn above the live turn and the person's count standing.
    Remote,
}

/// One read of [`carried_by`]: when (in seconds), what it draws, how the loop steps it, and the
/// session's submissions served on it (none served at `None`).
type Then = (i64, Read, Step, Option<u64>);

/// A MEASURED OUTAGE HOLDS THE CARRY: the host's reach, read afresh at the write (the loop's own
/// was read at its last point, before the background work began), holds the carry while it is
/// Down (`held:reach`), and the first break after its return carries the session. The control:
/// an outage that outlasts every break carries nothing; with no outage the carry goes at the
/// first settled break (`cruise_carries_a_settled_background_break_and_off_and_land_type_nothing`).
/// NEGATIVE CONTROL (run): with `write_guards`' peek removed, nothing holds the carry, and the
/// test fails at the `held:reach` note it no longer finds.
#[test]
fn a_measured_outage_holds_the_carry_until_its_return() {
    use crate::supervise::policy::turn_end::Reach;
    let run = |back_at: Option<usize>| {
        let host = Arc::new(DoorHost::default());
        *host.reach.lock().expect("reach") = Reach::Down {
            since: Instant::now(),
        };
        let drawn = |_| waiting(PLAIN);
        let h = Arc::clone(&host);
        Script {
            policy: under(Autopilot::Cruise),
            drawn: &drawn,
            host,
            ..Script::new("carry-outage", Occupancy::Observe, &[0, 30, 60])
        }
        .run(&mut |_, i| {
            if Some(i) == back_at {
                *h.reach.lock().expect("reach") = Reach::Unknown;
            }
        })
    };
    let back = run(Some(2));
    assert_eq!(carries(&back), 1, "{:#?}", back.requests);
    assert!(
        back.noted.iter().any(|l| l.starts_with("DEFERRED ")
            && l.contains(RULE_BACKGROUND)
            && l.ends_with("held:reach")),
        "{:#?}",
        back.noted
    );
    let down = run(None);
    assert_eq!(carries(&down), 0, "{:#?}", down.requests);
    assert!(
        down.noted.iter().any(|l| l.ends_with("held:reach")),
        "{:#?}",
        down.noted
    );
}

/// AN OUTAGE PAST ITS CAP HOLDS NO CARRY: the same cap a point's act is held to
/// (`TurnEndTiming::outage_holds_until`, `outage_cap` from when the outage began) bounds a break's
/// line, so a Down older than the cap, whether the host reads it afresh at the write or the loop
/// read it at its last point, holds nothing and the first settled break carries the session
/// (2026-10-26: Cruise never parks; a probe that reads Down while the agent's own requests get
/// through holds no break for ever). The control: the same outage within its cap holds the carry
/// (`held:reach`). NEGATIVE CONTROLS (run): with the cap dropped from `write_guards`' fresh read
/// (any Down holds), or from `occupancy_guards`' read of the point's (`reach_seen`), the
/// capped run carries nothing and fails at "a Down past its cap holds no carry".
#[test]
fn an_outage_past_its_cap_holds_no_carry() {
    use crate::supervise::policy::turn_end::{Reach, TurnEndTiming};
    let cap = TurnEndTiming::default().outage_cap;
    let run = |age: Duration| {
        let host = Arc::new(DoorHost::default());
        let drawn = |_| waiting(PLAIN);
        let h = Arc::clone(&host);
        Script {
            policy: under(Autopilot::Cruise),
            drawn: &drawn,
            host,
            ..Script::new("carry-outage-cap", Occupancy::Observe, &[0, 30])
        }
        .run(&mut |s, i| {
            if i == 0 {
                let down = Reach::Down {
                    since: s.fx.mono().checked_sub(age).expect("an earlier instant"),
                };
                *h.reach.lock().expect("reach") = down;
                // The loop's own read at its last point, before the background work began.
                s.reach_seen = Some(down);
            }
        })
    };
    let capped = run(cap + Duration::from_secs(1));
    assert_eq!(
        carries(&capped),
        1,
        "a Down past its cap holds no carry: {:#?}\n{:#?}",
        capped.noted,
        capped.requests
    );
    let within = run(cap - Duration::from_secs(120));
    assert_eq!(carries(&within), 0, "{:#?}", within.requests);
    assert!(
        within.noted.iter().any(|l| l.starts_with("DEFERRED ")
            && l.contains(RULE_BACKGROUND)
            && l.ends_with("held:reach")),
        "{:#?}",
        within.noted
    );
}

/// `reads`, each stepped as the loop steps a break and served no submissions.
fn stepped(reads: &[(i64, Read)]) -> Vec<Then> {
    reads
        .iter()
        .map(|&(at, read)| (at, read, Step::Stepped, None))
        .collect()
}

/// The carry's own prompt row, as Claude Code draws it in the transcript.
fn carry_row() -> String {
    format!("❯ {BACKGROUND_CARRY}")
}

/// A live turn under `prompts` (prompt rows, each with the reply under it).
fn live_under(prompts: &[&str], said: &str) -> Vec<String> {
    let mut lines: Vec<&str> = Vec::new();
    for prompt in prompts {
        lines.extend([*prompt, "", "⏺ Both workflows are running.", ""]);
    }
    lines.extend([said, "", "✻ Synthesizing… (8s)", ""]);
    let mut r = rows(&lines);
    r.extend(composer("  esc to interrupt"));
    r
}

/// The carries Cruise typed at each of the `reads` (when, in seconds, and what it drew), and what
/// the run noted, the break's settle `settle` long ([`carried_by`], each read stepped and served
/// no submissions).
fn carried_over(tag: &str, settle: Duration, reads: &[(i64, Read)]) -> (Vec<usize>, Vec<String>) {
    carried_by(tag, settle, &stepped(reads))
}

/// The carries Cruise typed at each of the `reads`, and what the run noted, the break's settle
/// `settle` long ([`carried_after`] a session no loop ran before).
fn carried_by(tag: &str, settle: Duration, reads: &[Then]) -> (Vec<usize>, Vec<String>) {
    carried_after(tag, settle, reads, None)
}

/// What a loop before this one left the session: its ledger's text, the agent's launch the host
/// saw since (unix ms), whether the host lifted the session off Off, and the person's last
/// gesture before the loop's start (in seconds from it, as a read's: their key 2 s before it and
/// their count past the carry's from then on), none at `None`.
#[derive(Debug, Clone, Default)]
struct Earlier {
    ledger: String,
    launch: Option<i64>,
    lifted_off: bool,
    touched: Option<i64>,
}

/// The carries Cruise typed at each of the `reads`, and what the run noted, the break's settle
/// `settle` long, by a loop that starts on the books `earlier` left, seeded as a loop's start
/// seeds them. A turn opened (any but [`Read::Break`], [`Read::Uncounted`] and
/// [`Read::Scrolled`]) is read running (`Session::took`) before its read, and a turn drawn live
/// ends the break before it, as in the loop. Each read is served the screen drawn for it, whatever
/// reads a carry made.
fn carried_after(
    tag: &str,
    settle: Duration,
    reads: &[Then],
    earlier: Option<&Earlier>,
) -> (Vec<usize>, Vec<String>) {
    let cfg = under(Autopilot::Cruise);
    let at_s: Vec<i64> = reads.iter().map(|&(at, ..)| at).collect();
    let mut typed = Vec::new();
    let mut gestured: Option<i64> = earlier.and_then(|e| e.touched);
    let drawn = |_| waiting(PLAIN);
    let steps = |i: usize| reads[i].2;
    // No earlier tab's books and no journal beside them.
    let books = crate::supervise::test_scratch_path("carry-books", tag).join("s-1.jsonl");
    let run = Script {
        policy: under(Autopilot::Cruise),
        drawn: &drawn,
        steps: &steps,
        host: Arc::new(DoorHost {
            launch: earlier.and_then(|e| e.launch),
            ..DoorHost::default()
        }),
        seed: earlier.map(|e| (e.ledger.clone(), T0)),
        ..Script::new(tag, Occupancy::Observe, &at_s)
    }
    .run(&mut |s, i| {
        if i == 0
            && let Some(earlier) = earlier
        {
            s.seed_from_ledger_text(&books, &earlier.ledger, T0, earlier.lifted_off);
        }
        s.setup.background_settle = settle;
        typed.push(s.ctl.seen.iter().filter(|(r, _)| is_carry(r)).count());
        let (at, read, _, submits) = reads[i];
        let carry = carry_row();
        s.ctl.m.screens = vec![match read {
            Read::Break | Read::Uncounted | Read::Taken | Read::Scrolled => waiting(PLAIN),
            Read::Live | Read::Theirs => live_turn(),
            Read::Woke => live_under(
                &[&carry],
                "⏺ Background command \"Watch the release\" completed (exit code 0)",
            ),
            Read::Remote => live_under(&[&carry, "❯ what's the status?"], "⏺ Checking."),
        }];
        s.ctl.m.served = 0;
        s.ctl.m.submits_on = submits.map(|n| (0, n)).into_iter().collect();
        if matches!(read, Read::Theirs | Read::Scrolled) {
            gestured = Some(at);
        }
        s.ctl.m.human_seq = match (read, gestured) {
            (Read::Uncounted, _) => None,
            (_, Some(_)) => Some(8),
            (_, None) => Some(7),
        };
        if let Some(first) = gestured {
            let ms = ((at - first + 2) * 1000).unsigned_abs();
            s.ctl.m.human_ms = Some(ms);
            // The screen's read says it too.
            let stamp: &'static str = Box::leak(ms.to_string().into_boxed_str());
            s.ctl.m.human = vec![stamp];
        }
        if !matches!(read, Read::Break | Read::Uncounted | Read::Scrolled) {
            s.took(7, Some(&cfg), &mut Heard::default());
        }
    });
    typed.push(carries(&run));
    let at = typed.windows(2).map(|w| w[1] - w[0]).collect();
    (at, run.noted)
}

/// The carries Cruise typed at each of the reads `at_s` of breaks of the agent's background work,
/// and what the run noted. Before the read at each of `taken` the agent takes the carry typed
/// last; at each of `woke` a background task's wake starts a turn nobody typed, the person's count
/// and their last key where they stood; at `theirs` the person's message starts one, their key 2 s
/// before it and their count past it ([`carried_over`], the break's settle none).
fn carried_at(
    tag: &str,
    at_s: &[i64],
    taken: &[usize],
    woke: &[usize],
    theirs: Option<usize>,
) -> (Vec<usize>, Vec<String>) {
    let reads: Vec<(i64, Read)> = at_s
        .iter()
        .enumerate()
        .map(|(i, &at)| {
            let read = if theirs == Some(i) {
                Read::Theirs
            } else if woke.contains(&i) {
                Read::Woke
            } else if taken.contains(&i) {
                Read::Taken
            } else {
                Read::Break
            };
            (at, read)
        })
        .collect();
    carried_over(tag, Duration::ZERO, &reads)
}

/// The `DEFERRED … waits until` notes' streaks, in order.
fn waits(noted: &[String]) -> Vec<String> {
    noted
        .iter()
        .filter(|l| l.starts_with("DEFERRED ") && l.contains(" waits until "))
        .filter_map(|l| {
            l.rsplit_once(" (")
                .map(|(_, n)| n.trim_end_matches(')').to_string())
        })
        .collect()
}

/// THE PROGRAM'S OWN WAKE IS NO ONE ELSE'S TURN: a background task that finishes starts a turn
/// nobody typed (Claude Code's `<task-notification>`), and a wake turn the agent answers in
/// seconds is a short answer like any other, so the carry at its break waits on the ladder, never
/// goes at once, and still goes when the ladder says. Within the back-off the wake must not start
/// the books again, and past `min_work` from the last carry the wall time since it is no work the
/// wake's turn did. A person's message still starts the books again.
#[test]
fn a_short_wake_turn_keeps_the_carry_on_the_ladder() {
    // The first carry at 30 s, answered at 50; the wake at 100 runs 20 s and its turn ends on the
    // work at 120, short of the carry's 2-minute back-off.
    let (within, within_noted) =
        carried_at("carry-wake-within", &[30, 50, 100, 120], &[1], &[2], None);
    // The second carry at 150, answered at 160, waits 4 minutes (to 390); the wake at 330, three
    // minutes after it, runs 20 s and its turn ends on the work at 350, and the ladder's carry
    // goes at 390.
    let (past, past_noted) = carried_at(
        "carry-wake-past",
        &[30, 50, 150, 160, 330, 350, 390],
        &[1, 3],
        &[4],
        None,
    );
    assert_eq!(
        [within, past],
        [vec![1, 0, 0, 0], vec![1, 0, 1, 0, 0, 0, 1]],
        "the carries at each read: {within_noted:#?} {past_noted:#?}"
    );
    // The person's message at 170, after the second carry's short answer, runs 20 s: the person
    // typing holds the carry at 190, and at 300, past their grace and short of the ladder's 390,
    // it goes.
    let (person, person_noted) = carried_at(
        "carry-wake-person",
        &[30, 50, 150, 160, 170, 190, 300],
        &[1, 3],
        &[],
        Some(4),
    );
    assert_eq!(person, [1, 0, 1, 0, 0, 0, 1], "{person_noted:#?}");
    assert!(
        person_noted
            .iter()
            .any(|l| l.starts_with("DEFERRED ") && l.ends_with("held:person")),
        "{person_noted:#?}"
    );
}

/// A wake turn that ran real work (2 minutes from its live edge) ends on a break that goes at
/// once, as the ladder would not, and starts the ladder again; a wake whose break is served no
/// person's count is unsure, so it is a person's turn.
#[test]
fn a_wake_is_judged_by_its_own_work_and_an_unknown_count_as_a_persons() {
    // The second carry at 150 waits 4 minutes for 390; the wake at 170 runs 130 s and its break
    // at 300 goes at once, its short answer at 310 on the ladder's first rung again.
    let (work, work_noted) = carried_at(
        "carry-wake-work",
        &[30, 50, 150, 160, 170, 300, 310, 330],
        &[1, 3, 6],
        &[4],
        None,
    );
    assert_eq!(work, [1, 0, 1, 0, 0, 1, 0, 0], "{work_noted:#?}");
    assert_eq!(
        waits(&work_noted).last().map(String::as_str),
        Some("1 short answer(s) in a row"),
        "{work_noted:#?}"
    );
    // The wake's break at 120 is served no person's count: unsure, the books start again, the
    // carry there wants its fence, and at 125 it goes, short of the ladder's 150.
    let (unknown, unknown_noted) = carried_over(
        "carry-wake-unknown",
        Duration::ZERO,
        &[
            (30, Read::Break),
            (50, Read::Taken),
            (100, Read::Woke),
            (120, Read::Uncounted),
            (125, Read::Break),
        ],
    );
    assert_eq!(unknown, [1, 0, 0, 0, 1], "{unknown_noted:#?}");
}

/// A person's message that lands while the carry's own answer runs is no wake and no part of the
/// harness's turn: their count moved past the carry's, so the break that ends the turn starts the
/// books again, whoever opened it.
#[test]
fn a_persons_key_inside_the_carrys_answer_starts_the_ladder_again() {
    // The second carry at 150 waits 4 minutes for 390. The person's key lands at 170 while its
    // answer runs (taken at 160, live); their key holds the carry at the break at 190, and at
    // 300 it goes.
    let (folded, folded_noted) = carried_over(
        "carry-wake-folded",
        Duration::ZERO,
        &[
            (30, Read::Break),
            (50, Read::Taken),
            (150, Read::Break),
            (160, Read::Live),
            (170, Read::Theirs),
            (190, Read::Break),
            (300, Read::Break),
        ],
    );
    assert_eq!(folded, [1, 0, 1, 0, 0, 0, 1], "{folded_noted:#?}");
}

/// A TURN ENDS ONLY AT A SETTLED BREAK: under a status line Claude Code draws a reply's text
/// between tool calls with no busy sign, the work's footer pill still counting, so the turn reads
/// as a break for seconds at a time. Such a pause ends nothing: the carry's answer that ran past
/// min_work with its last stretch short goes at its final break's settle, never on the ladder.
#[test]
fn a_pause_inside_a_turn_ends_no_turn() {
    let settle = Duration::from_secs(20);
    let (paused, noted) = carried_over(
        "carry-paused",
        settle,
        &[
            (0, Read::Break),
            (30, Read::Break),
            (40, Read::Taken),
            (60, Read::Break),
            // The second carry at 150 waits 4 minutes for 390. Its answer runs from 160, a
            // reply's text at 200 drawn as a break, the next tool call at 205, and the final
            // reply at 290: 140 s after the carry, its last stretch 85 s.
            (150, Read::Break),
            (160, Read::Live),
            (200, Read::Break),
            (205, Read::Live),
            (290, Read::Break),
            (310, Read::Break),
        ],
    );
    assert_eq!(paused, [0, 1, 0, 0, 1, 0, 0, 0, 0, 1], "{noted:#?}");
}

/// REAL WORK IS STICKY: a wake that ran real work ends on a break whose carry a halt holds; a
/// short wake after it, the halt lifted, still goes at once and starts the ladder at one, never
/// on the ladder the work had reset.
#[test]
fn real_work_held_at_its_break_survives_a_short_wake() {
    let cfg = under(Autopilot::Cruise);
    let at_s = [30, 50, 150, 160, 170, 300, 310, 330, 340, 350];
    let mut typed = Vec::new();
    let carry = carry_row();
    let run = ran(
        "carry-sticky",
        Autopilot::Cruise,
        PLAIN,
        &at_s,
        &|_| {},
        &mut |s, i| {
            typed.push(s.ctl.seen.iter().filter(|(r, _)| is_carry(r)).count());
            let woke = live_under(
                &[&carry],
                "⏺ Background command \"Watch the release\" completed (exit code 0)",
            );
            // The wake at 170 runs 130 s; a halt holds its break at 300 and 310; a second wake
            // at 330 runs 10 s, and its break at 340 goes, the halt lifted.
            s.ctl.m.screens = vec![if [4, 7].contains(&i) {
                woke
            } else {
                waiting(PLAIN)
            }];
            s.ctl.m.served = 0;
            s.ctl.m.hold = if [5, 6].contains(&i) { "1" } else { "0" };
            if [1, 3, 4, 7, 9].contains(&i) {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        },
    );
    typed.push(carries(&run));
    let at: Vec<usize> = typed.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(at, [1, 0, 1, 0, 0, 0, 0, 0, 1, 0], "{:#?}", run.noted);
    assert!(
        run.noted
            .iter()
            .any(|l| l.starts_with("DEFERRED ") && l.ends_with("held:halted")),
        "{:#?}",
        run.noted
    );
    assert_eq!(
        waits(&run.noted).last().map(String::as_str),
        Some("1 short answer(s) in a row"),
        "{:#?}",
        run.noted
    );
}

/// A message no key in this window typed (Claude Code's Remote Control, from the person's phone)
/// moves no person's count, but its prompt row is drawn above the turn it opens: that turn is
/// someone else's and starts the books again, though the break that ends it has scrolled the row
/// away.
#[test]
fn a_remote_message_is_someone_elses_turn() {
    let (remote, noted) = carried_over(
        "carry-remote",
        Duration::ZERO,
        &[
            (30, Read::Break),
            (50, Read::Taken),
            (150, Read::Break),
            (160, Read::Taken),
            (170, Read::Remote),
            (190, Read::Break),
            (300, Read::Break),
        ],
    );
    assert_eq!(remote, [1, 0, 1, 0, 0, 1, 0], "{noted:#?}");
}

/// A TURN IS JUDGED ONCE: the loop that comes back to a break afresh (a lost call rode out, the
/// break's start read again from there) reads no new turn there, so the wall time since the last
/// judged turn is no work, and the carry the ladder holds still waits for the ladder.
#[test]
fn a_break_read_afresh_is_no_turns_work() {
    // The second carry at 150, answered short at 160, waits 4 minutes for 390; the loop comes
    // back to the break at 300, 2 minutes and more after the carry and its answer.
    let mut reads = stepped(&[
        (30, Read::Break),
        (50, Read::Taken),
        (150, Read::Break),
        (160, Read::Taken),
        (200, Read::Break),
        (300, Read::Break),
        (390, Read::Break),
    ]);
    reads[5].2 = Step::Again;
    let (again, noted) = carried_by("carry-again", Duration::ZERO, &reads);
    assert_eq!(again, [1, 0, 1, 0, 0, 0, 1], "{noted:#?}");
    assert_eq!(
        waits(&noted).last().map(String::as_str),
        Some("2 short answer(s) in a row"),
        "{noted:#?}"
    );
}

/// REAL WORK IS BUSY TIME: wakes that come closer together than the break's settle are one turn,
/// and the breaks between them are no work, so a burst of short wakes 2 minutes and more long
/// with little busy time in it is a short answer and the carry waits for the ladder.
#[test]
fn a_burst_of_short_wakes_is_no_real_work() {
    let settle = Duration::from_secs(20);
    // The second carry at 150, answered short at 160, waits 4 minutes for 390. Nine wakes from
    // 200, 15 s apart, each drawn live 5 s and then at a break short of its settle: 125 s from
    // the first wake's edge to the last break, 45 s of it busy.
    let mut reads = vec![
        (0, Read::Break),
        (30, Read::Break),
        (40, Read::Taken),
        (65, Read::Break),
        (150, Read::Break),
        (160, Read::Taken),
        (185, Read::Break),
    ];
    for k in 0..9 {
        reads.push((200 + 15 * k, Read::Woke));
        reads.push((205 + 15 * k, Read::Break));
    }
    reads.extend([(360, Read::Break), (390, Read::Break)]);
    let (burst, noted) = carried_over("carry-burst", settle, &reads);
    let mut want = vec![0, 1, 0, 0, 1, 0, 0];
    want.extend([0; 18]);
    want.extend([0, 1]);
    assert_eq!(burst, want, "{noted:#?}");
}

/// SOMEONE ELSE'S MESSAGE is a submission the harness did not make: a scroll since the last
/// carry (the person's count moved, nothing submitted) is no person's turn, so the program's own
/// short wake after it waits for the ladder; a submission past the harness's own line (another
/// driver's `send`, or a message the transcript does not show newest) starts the books again.
#[test]
fn a_scroll_is_no_message_and_a_submission_not_the_harnesss_is() {
    // The carries at 30 and 150 each submit one; the second, answered short at 160, waits 4
    // minutes for 390. The person scrolls back at 170, and the program's own wake runs from 300.
    let mut scroll = stepped(&[
        (30, Read::Break),
        (50, Read::Taken),
        (150, Read::Break),
        (160, Read::Taken),
        (170, Read::Scrolled),
        (300, Read::Woke),
        (310, Read::Break),
        (390, Read::Break),
    ]);
    for (read, submits) in scroll.iter_mut().zip([3, 4, 4, 5, 5, 5, 5, 5]) {
        read.3 = Some(submits);
    }
    let (scrolled, scrolled_noted) = carried_by("carry-scrolled", Duration::ZERO, &scroll);
    assert_eq!(scrolled, [1, 0, 1, 0, 0, 0, 0, 1], "{scrolled_noted:#?}");
    // No scroll; a message lands at 300 with the wake, drawn under the carry's own prompt row.
    let mut message = stepped(&[
        (30, Read::Break),
        (50, Read::Taken),
        (150, Read::Break),
        (160, Read::Taken),
        (170, Read::Break),
        (300, Read::Woke),
        (310, Read::Break),
        (390, Read::Break),
    ]);
    for (read, submits) in message.iter_mut().zip([3, 4, 4, 5, 5, 6, 6, 7]) {
        read.3 = Some(submits);
    }
    let (sent, sent_noted) = carried_by("carry-submitted", Duration::ZERO, &message);
    assert_eq!(sent, [1, 0, 1, 0, 0, 0, 1, 0], "{sent_noted:#?}");
}

/// A turn whose end the loop did not read is unsure: under Off the loop takes a live turn but
/// steps no break, so the break read back on Cruise began, as the loop holds it, before the turn
/// did, and its work is unknown. The books start again, the carry goes at its settle and its short
/// answer is on the ladder's first rung, never a rung the turn's work would have reset.
#[test]
fn a_turn_run_under_off_is_unread_and_starts_the_books_again() {
    // The second carry at 150 is answered short at 160. Off from 165: the program's own wake
    // works from 170 to 1970 and ends on a break at 1980. Back on Cruise at 2000 the carry goes,
    // and its short answer at 2010 waits 2 minutes, to 2120.
    let mut reads = stepped(&[
        (30, Read::Break),
        (50, Read::Taken),
        (150, Read::Break),
        (160, Read::Taken),
        (170, Read::Woke),
        (1000, Read::Live),
        (1970, Read::Live),
        (1980, Read::Break),
        (2000, Read::Break),
        (2010, Read::Taken),
        (2130, Read::Break),
    ]);
    for read in &mut reads[4..8] {
        read.2 = Step::Off;
    }
    let (off, noted) = carried_by("carry-off", Duration::ZERO, &reads);
    assert_eq!(off, [1, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1], "{noted:#?}");
}

/// A ledger row of the session's, `min_ago` minutes before the loop's start.
fn row_before(min_ago: i64, rule_id: &str, outcome: approvals::Outcome, reason: &str) -> String {
    approvals::Row {
        rule_id,
        outcome,
        command: if outcome == approvals::Outcome::Typed {
            BACKGROUND_CARRY
        } else {
            ""
        },
        reason,
        box_seq: 3,
    }
    .to_json(T0 - min_ago * MIN, Some("@s-1"))
}

/// A loop before this one's carry at a background break, `min_ago` minutes before the loop's
/// start.
fn carry_before(min_ago: i64) -> String {
    row_before(
        min_ago,
        RULE_BACKGROUND,
        approvals::Outcome::Typed,
        "the turn ended while the agent's own background work runs (autopilot cruise)",
    )
}

/// A loop before this one's carries at background breaks, each `min_ago` minutes before the
/// loop's start (oldest first), and `after` its rows after them.
fn carried_before(min_ago: &[i64], after: &[String]) -> Earlier {
    let rows: Vec<String> = min_ago.iter().map(|&ago| carry_before(ago)).collect();
    books_of(&[rows.as_slice(), after].concat())
}

/// A loop before this one's ledger of `rows`, oldest first.
fn books_of(rows: &[String]) -> Earlier {
    Earlier {
        ledger: rows.iter().map(|row| format!("{row}\n")).collect(),
        ..Earlier::default()
    }
}

/// THE LADDER SURVIVES A LOOP'S RESTART (a hand-over on a `[harness]` change, a roster change, a
/// fault's restart, an app's relaunch): the carries a loop before this one typed are in the
/// ledger, so a loop that starts at the break they carried keeps their ladder instead of carrying
/// at once and starting it over, and the program's own short wake after the restart is judged by
/// its busy time. NEGATIVE CONTROLS, each carried at the first settled break and its next on the
/// ladder's first rung: no carry in the books; the last the ladder's cap past the rung it waited;
/// an act of another rule, a point, or a person's exchange after it; the agent launched since it;
/// the session lifted off Off. And a person's message after the restart starts the books again.
#[test]
fn a_restarted_loop_keeps_the_background_ladder() {
    use crate::supervise::policy::turn_end::{RULE_CONTINUE, RULE_LEFT};
    use approvals::Outcome::{Skipped, Typed, Waiting};
    // Six carries 2, 4, 8, 16 and 32 minutes apart, each answered short, the last 5 minutes
    // before the loop's start: the next waits the ladder's 60 minutes from it, to 55 minutes in.
    // The agent was launched before them all.
    let ladder = [67, 65, 61, 53, 37, 5];
    let kept = Earlier {
        launch: Some(T0 - 120 * MIN),
        ..carried_before(&ladder, &[])
    };
    let at_break = stepped(&[
        (0, Read::Break),
        (30, Read::Break),
        (54 * 60, Read::Break),
        (55 * 60 + 30, Read::Break),
    ]);
    let (restarted, noted) = carried_after("carry-restart", Duration::ZERO, &at_break, Some(&kept));
    assert_eq!(restarted, [0, 0, 0, 1], "{noted:#?}");
    assert_eq!(
        waits(&noted).first().map(String::as_str),
        Some("6 short answer(s) in a row"),
        "{noted:#?}"
    );
    // The program's own wake at 2 minutes runs 20 s: a short answer, the ladder kept.
    let woke = stepped(&[
        (0, Read::Break),
        (30, Read::Break),
        (120, Read::Woke),
        (140, Read::Break),
        (54 * 60, Read::Break),
        (55 * 60 + 30, Read::Break),
    ]);
    let (wake, wake_noted) =
        carried_after("carry-restart-wake", Duration::ZERO, &woke, Some(&kept));
    assert_eq!(wake, [0, 0, 0, 0, 0, 1], "{wake_noted:#?}");
    let first = stepped(&[(0, Read::Break), (30, Read::Break)]);
    let point = approvals::point_reason(approvals::Carried::streak(0), false);
    let left = approvals::LEFT_REASON;
    let controls = [
        ("none", carried_before(&[], &[])),
        // The ladder's cap past the 60 minutes it waited, the last carry is no ladder's.
        (
            "lapsed",
            carried_before(&[181, 179, 175, 167, 151, 120], &[]),
        ),
        (
            "continue",
            carried_before(
                &ladder,
                &[row_before(3, RULE_CONTINUE, Typed, "the turn ended")],
            ),
        ),
        (
            "point",
            carried_before(
                &ladder,
                &[row_before(3, approvals::RULE_POINT, Waiting, &point)],
            ),
        ),
        (
            "left",
            carried_before(&ladder, &[row_before(3, RULE_LEFT, Skipped, left)]),
        ),
        (
            "launched",
            Earlier {
                launch: Some(T0 - 3 * MIN),
                ..kept.clone()
            },
        ),
        (
            "lifted",
            Earlier {
                lifted_off: true,
                ..kept.clone()
            },
        ),
    ];
    // Each carried at once, and its next waits the ladder's first rung.
    let wrong: Vec<_> = controls
        .iter()
        .map(|(tag, earlier)| {
            let (at, noted) = carried_after(
                &format!("carry-restart-{tag}"),
                Duration::ZERO,
                &first,
                Some(earlier),
            );
            (*tag, at, waits(&noted))
        })
        .filter(|(_, at, waits)| {
            *at != [1, 0] || waits.first().map(String::as_str) != Some("1 short answer(s) in a row")
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
    // The person's message after the restart is someone else's turn: their key holds the carry
    // at its break, and past their grace it goes.
    let theirs = stepped(&[
        (0, Read::Break),
        (30, Read::Break),
        (120, Read::Theirs),
        (140, Read::Break),
        (330, Read::Break),
    ]);
    let (person, person_noted) =
        carried_after("carry-restart-theirs", Duration::ZERO, &theirs, Some(&kept));
    assert_eq!(person, [0, 0, 0, 0, 1], "{person_noted:#?}");
}

/// A restart's case: its tag, the books it starts on, its reads, the carries typed at each and
/// the first wait it noted.
type Replayed<'a> = (&'a str, Earlier, &'a [Then], &'a [usize], &'a str);

/// THE BOOKS ARE REPLAYED AS THE LIVE LADDER TYPED THEM: a loop's books start again in memory
/// alone (a turn of real work, someone else's message at a break) or at a restart that took no
/// ladder, and the carries' own times show where. A carry closer to the one before than the rung
/// that one's streak waits came on books started again, and so did one the ladder's cap past that
/// rung, where a restart takes no ladder; a carry before the agent's launch is another program's.
/// Each restart here is a minute after the newest carry, whose next is due 2 or 4 minutes from it
/// where the run starts again there, never the 32 or 60 a count of every carry waits. A point
/// between the carries ends the run as one after them does. And a restart that missed a carry the
/// cap held keeps the cap, as the live ladder does.
#[test]
fn a_restart_replays_the_ladder_as_its_carries_were_typed() {
    let point = approvals::point_reason(approvals::Carried::streak(0), false);
    let two = stepped(&[(0, Read::Break), (30, Read::Break), (90, Read::Break)]);
    let four = stepped(&[
        (0, Read::Break),
        (30, Read::Break),
        (150, Read::Break),
        (200, Read::Break),
    ]);
    let cases: [Replayed<'_>; 5] = [
        // 2, 4, 8 and 16 minutes apart, then 20 where the ladder waits 32.
        (
            "short",
            carried_before(&[51, 49, 45, 37, 21, 1], &[]),
            &two,
            &[0, 0, 1],
            "1 short answer(s) in a row",
        ),
        // Six on the ladder, then 130 minutes where it waits 60, and one 2 minutes after.
        (
            "lapsed",
            carried_before(&[195, 193, 189, 181, 165, 133, 3, 1], &[]),
            &four,
            &[0, 0, 0, 1],
            "2 short answer(s) in a row",
        ),
        // Three, the agent's launch, and two 16 minutes apart: a count of all five waits 32.
        (
            "launched",
            Earlier {
                launch: Some(T0 - 20 * MIN),
                ..carried_before(&[31, 29, 25, 17, 1], &[])
            },
            &four,
            &[0, 0, 0, 1],
            "2 short answer(s) in a row",
        ),
        (
            "point",
            books_of(&[
                carry_before(31),
                carry_before(29),
                carry_before(25),
                row_before(
                    20,
                    approvals::RULE_POINT,
                    approvals::Outcome::Waiting,
                    &point,
                ),
                carry_before(17),
                carry_before(1),
            ]),
            &four,
            &[0, 0, 0, 1],
            "2 short answer(s) in a row",
        ),
        // Seven on the ladder, the last 61 minutes before the start: its next was due a minute
        // before it, on the cap.
        (
            "capped",
            carried_before(&[183, 181, 177, 169, 153, 121, 61], &[]),
            &two[..2],
            &[1, 0],
            "8 short answer(s) in a row",
        ),
    ];
    let wrong: Vec<_> = cases
        .iter()
        .filter_map(|(tag, earlier, reads, carried, wait)| {
            let (at, noted) = carried_after(
                &format!("carry-replay-{tag}"),
                Duration::ZERO,
                reads,
                Some(earlier),
            );
            let first = waits(&noted).first().cloned();
            (at != *carried || first.as_deref() != Some(*wait)).then_some((*tag, at, first))
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A PERSON'S MESSAGE BETWEEN THE LAST CARRY AND A RESTART IS STILL SOMEONE ELSE'S: the counts the
/// last carry was typed under were the loop's before this one, and the restart's first read stands
/// for them only where the person's last gesture came before that carry. After it, what they sent
/// is in the counts that read sees, and a prompt row tells it only on Claude Code with the row in
/// view (none here, nor ever on Muse Code or agy). The seeded books start again whether their
/// turn is still running or already reached its background break before the restart, and the
/// break is carried once their grace is past. NEGATIVE CONTROL: their gesture before the last
/// carry keeps the ladder through both readings.
#[test]
fn a_persons_message_between_the_last_carry_and_a_restart_starts_the_books_again() {
    let kept = carried_before(&[67, 65, 61, 53, 37, 5], &[]);
    let mut wrong = Vec::new();
    for (start, first, after) in [
        ("live", Read::Live, [0, 1, 0]),
        ("break", Read::Break, [1, 0, 0]),
    ] {
        let reads = stepped(&[(0, first), (20, Read::Break), (40, Read::Break)]);
        for (tag, touched, carried) in [("after", -180, after), ("before", -400, [0, 0, 0])] {
            let earlier = Earlier {
                touched: Some(touched),
                ..kept.clone()
            };
            let (at, noted) = carried_after(
                &format!("carry-restart-touched-{start}-{tag}"),
                Duration::ZERO,
                &reads,
                Some(&earlier),
            );
            if at != carried {
                wrong.push((start, tag, at, noted));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

const PARKED: &[&str] = &[
    "Both workflows are running; the build is waiting on your go-ahead and on disk room.",
    "NEEDS YOU: say \"rerun the checks\" so I can build and test the branch and push it to main.",
];

/// The `send` requests the run made, their text after ` -- `.
fn sent(ran: &Ran) -> Vec<String> {
    ran.requests()
        .into_iter()
        .filter(|r| r.starts_with("send "))
        .filter_map(|r| r.split_once(" -- ").map(|(_, text)| text.to_string()))
        .collect()
}

/// A BREAK THAT PARKS ON A GO-AHEAD (owner, 2026-10-29: "how dare you block on me for go
/// ahead!"): the owner's standing go-ahead goes in the carry's place, on its ladder, and the need
/// stays raised until the work runs; with answers off the carry goes as before.
#[test]
fn a_break_that_parks_on_a_go_ahead_gets_the_owners_standing_go_ahead() {
    use crate::supervise::policy::go_ahead::{GO_AHEAD_ANSWER, RULE_GO_AHEAD};
    let drawn = |_| waiting(PARKED);
    let cruise = Script {
        policy: SupervisorConfig {
            answer_questions: true,
            ..under(Autopilot::Cruise)
        },
        drawn: &drawn,
        set: &|_| {},
        ..Script::new("carry-go-ahead", Occupancy::Observe, &[0, 30])
    }
    .run(&mut nothing);
    let sent = sent(&cruise);
    assert_eq!(sent.len(), 1, "{sent:#?}");
    assert!(
        sent[0].starts_with(GO_AHEAD_ANSWER)
            && sent[0]
                .contains("You asked the owner to say \"rerun the checks\": consider it said."),
        "{sent:#?}"
    );
    assert_eq!(carries(&cruise), 0);
    assert!(
        cruise
            .ledger
            .contains(&format!("\"rule_id\":\"{RULE_GO_AHEAD}\"")),
        "{}",
        cruise.ledger
    );
    assert_eq!(raised(&cruise).len(), 1, "{:#?}", cruise.requests());
    // This harness's policy answers no question: the carry goes as before, without the sentence
    // that hands a parked agent the go-ahead the person chose to answer themselves (2026-10-29
    // review 2).
    let off = ran(
        "carry-go-ahead-off",
        Autopilot::Cruise,
        PARKED,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    let carried = self::sent(&off);
    assert_eq!(
        carried,
        vec![crate::supervise::policy::go_ahead::unless_held(BACKGROUND_CARRY, true).into_owned()],
        "{:#?}",
        off.writes()
    );
    assert!(
        carried
            .iter()
            .all(|s| !s.contains(crate::supervise::policy::go_ahead::NEVER_A_NEED)),
        "{carried:#?}"
    );
}

/// A BREAK READS ITS WHOLE TURN, AS A TURN END DOES (2026-10-29 review 2): a plan to delete
/// release tags in an earlier message of the turn is one the go-ahead would be read as covering,
/// so the carry goes, not the go-ahead.
#[test]
fn a_break_whose_turn_plans_an_outward_act_gets_the_carry_not_the_go_ahead() {
    use crate::supervise::policy::go_ahead::RULE_GO_AHEAD;
    let drawn = |_| {
        waiting_under(
            &[
                "⏺ Plan: delete the stale release tags on the remote, then push my branch to main. \
                 Tests are green.",
                "",
                "⏺ Bash(git fetch origin)",
                "  ⎿  (No content)",
                "",
            ],
            &["Waiting for your go-ahead to push."],
        )
    };
    let cruise = Script {
        policy: SupervisorConfig {
            answer_questions: true,
            ..under(Autopilot::Cruise)
        },
        drawn: &drawn,
        set: &|_| {},
        ..Script::new("carry-go-ahead-planned", Occupancy::Observe, &[0, 30])
    }
    .run(&mut nothing);
    assert!(
        !cruise
            .ledger
            .contains(&format!("\"rule_id\":\"{RULE_GO_AHEAD}\"")),
        "{}",
        cruise.ledger
    );
    assert!(
        cruise
            .ledger
            .contains(&format!("\"rule_id\":\"{RULE_BACKGROUND}\"")),
        "{}",
        cruise.ledger
    );
}

/// A BREAK WHOSE REPLY ASKS AN IRREVERSIBLE STEP keeps the carry: the go-ahead goes at a break only
/// where it would at a turn end (2026-10-29 review).
#[test]
fn a_break_that_asks_an_irreversible_step_gets_the_carry_not_the_go_ahead() {
    use crate::supervise::policy::go_ahead::RULE_GO_AHEAD;
    // The reader parks it (its force-push is negated), and the base reads an irreversible ask.
    let drawn = |_| {
        waiting(&[
            "No force-push and nothing published. I'm still waiting for your go-ahead before \
             rebuilding.",
        ])
    };
    let cruise = Script {
        policy: SupervisorConfig {
            answer_questions: true,
            ..under(Autopilot::Cruise)
        },
        drawn: &drawn,
        set: &|_| {},
        ..Script::new("carry-go-ahead-irreversible", Occupancy::Observe, &[0, 30])
    }
    .run(&mut nothing);
    assert_eq!(carries(&cruise), 1, "{:#?}", cruise.writes());
    assert!(
        !cruise
            .ledger
            .contains(&format!("\"rule_id\":\"{RULE_GO_AHEAD}\"")),
        "{}",
        cruise.ledger
    );
}

/// THE GO-AHEAD'S BOUND HOLDS AT BREAKS TOO (2026-10-29 review): each go-ahead typed at a break
/// counts on its park, and once the park is spent no more goes, carry or go-ahead.
#[test]
fn a_park_at_breaks_gets_the_go_ahead_its_limit_of_times() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    use crate::supervise::policy::turn_end::GO_AHEAD_ACTS;
    let cfg = SupervisorConfig {
        answer_questions: true,
        ..under(Autopilot::Cruise)
    };
    let drawn = |_| waiting(PARKED);
    let host = Arc::new(DoorHost::default());
    let ran = Script {
        policy: cfg.clone(),
        drawn: &drawn,
        set: &|_| {},
        host: Arc::clone(&host),
        // Each go-ahead is answered ten seconds on, the turn ending on the work again: the
        // carry's ladder (2, 4, 8, 16 minutes) brings the next break due.
        ..Script::new(
            "carry-go-ahead-spent",
            Occupancy::Observe,
            &[
                30, 40, 160, 170, 410, 420, 890, 900, 1850, 1860, 3770, 3780, 5700, 5710, 7600,
            ],
        )
    }
    .run(&mut |s, i| {
        if i % 2 == 1 {
            s.took(7, Some(&cfg), &mut Heard::default());
        }
    });
    let sent = sent(&ran);
    assert_eq!(
        sent.len(),
        usize::try_from(GO_AHEAD_ACTS).expect("small"),
        "{sent:#?}"
    );
    assert!(
        sent.iter().all(|s| s.starts_with(GO_AHEAD_ANSWER)),
        "{sent:#?}"
    );
    // The spent park is told now, as a turn end tells it (2026-10-29 review 2): the person sees
    // it on the menu bar, and the agent's breaks after it end nothing.
    let told: Vec<&String> = ran
        .noted
        .iter()
        .filter(|n| n.starts_with("EXCEPTION ") && n.contains(" kind=stalled when=now "))
        .collect();
    assert_eq!(told.len(), 1, "{:#?}", ran.noted);
    assert_eq!(
        *host.exceptions.lock().expect("exceptions"),
        vec![Some(
            crate::supervise::policy::exception::Exception::refuses_go_ahead().why
        )],
        "{:#?}",
        ran.noted
    );
}
