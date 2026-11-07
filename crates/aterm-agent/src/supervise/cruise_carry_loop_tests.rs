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

/// A `send` of [`BACKGROUND_CARRY`], held or not.
fn is_carry(request: &str) -> bool {
    // After a person's turn the carry keeps their hold: it says no word that their go-ahead is
    // no need (`go_ahead::unless_held`).
    request.starts_with("send ")
        && [false, true].into_iter().any(|held| {
            let carry = crate::supervise::policy::go_ahead::unless_held(BACKGROUND_CARRY, held);
            request.ends_with(&format!(" -- {carry}"))
        })
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

/// A CALL LOST AT A BREAK LOSES NO NEED: the break's report names a need only the person can
/// give. Where the carry's `status` read is lost, the loop rides it out, and the need is still
/// raised at that read, before the carry the next read types. Where the raise itself is lost, the
/// next read raises it again: no retry of the loop's own runs inside the agent's background work.
/// NEGATIVE CONTROL: with no call lost the need is raised once.
#[test]
fn a_call_lost_at_a_break_still_raises_its_need() {
    let drawn = |_| waiting(NEEDS);
    let steps = |i| if i == 0 { Step::Lost } else { Step::Again };
    let lines_lost = Script {
        policy: under(Autopilot::Cruise),
        drawn: &drawn,
        set: &|m| {
            m.verb_replies.insert(
                "status",
                VecDeque::from([failed(
                    124,
                    "Resource temporarily unavailable (os error 35)",
                )]),
            );
        },
        steps: &steps,
        ..Script::new("carry-needs-lines-lost", Occupancy::Observe, &[0, 30])
    }
    .run(&mut nothing);
    let requests = lines_lost.requests();
    let up: Vec<usize> = requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("meta set attention owner=needs "))
        .map(|(at, _)| at)
        .collect();
    let send = requests.iter().position(|r| r.starts_with("send "));
    assert!(
        up.len() == 1 && send.is_some_and(|send| up[0] < send),
        "{up:?} before {send:?}: {requests:#?}"
    );
    assert!(
        requests[up[0]].contains("the release credential"),
        "{requests:#?}"
    );
    assert_eq!(carries(&lines_lost), 1, "{:#?}", lines_lost.writes());
    let raise_lost = Script {
        policy: under(Autopilot::Cruise),
        drawn: &drawn,
        set: &|m| {
            m.needs_replies.push_back(failed(
                124,
                "Resource temporarily unavailable (os error 35)",
            ));
        },
        ..Script::new("carry-needs-raise-lost", Occupancy::Observe, &[0, 30, 60])
    }
    .run(&mut nothing);
    let up = raised(&raise_lost);
    assert!(
        up.len() == 2 && up.iter().all(|r| r.contains("the release credential")),
        "the lost raise, then the raise: {up:#?}"
    );
    let control = ran(
        "carry-needs-none-lost",
        Autopilot::Cruise,
        NEEDS,
        &[0, 30, 60],
        &|_| {},
        &mut nothing,
    );
    assert_eq!(raised(&control).len(), 1, "{:#?}", control.requests());
}

/// A turn of several messages under the person's `❯ ship 0.145.0` that ended on its background
/// work: `earlier` (one message, or none), a background `Bash` call, then the report `last`;
/// `above`, a message of the turn before it. Each message's rows are its `\n`-split text, the
/// first behind its `⏺`, the rest in its gutter.
fn waiting_turn(above: Option<&str>, earlier: Option<&str>, last: &str) -> Vec<String> {
    let message = |text: &str| -> Vec<String> {
        text.split('\n')
            .enumerate()
            .map(|(i, row)| format!("{}{row}", if i == 0 { "⏺ " } else { "  " }))
            .collect()
    };
    let mut lines: Vec<String> = Vec::new();
    if let Some(above) = above {
        lines.extend(message(above));
        lines.push(String::new());
    }
    lines.extend(["❯ ship 0.145.0".to_string(), String::new()]);
    if let Some(earlier) = earlier {
        lines.extend(message(earlier));
        lines.push(String::new());
    }
    lines.extend(
        [
            "⏺ Bash(targo --unverified test --workspace)",
            "  ⎿  Running in the background (↓ to manage)",
            "",
        ]
        .map(ToString::to_string),
    );
    lines.extend(message(last));
    let mut r = waiting(&[]);
    r.splice(0..0, lines.into_iter().chain([String::new()]));
    r
}

/// F1: a NEEDS YOU line in an earlier message of a turn that ends on its background work is raised
/// at the break, as a point reads the whole turn: the break's report alone names no need, and the
/// carry then closes the turn off from every later point. NEGATIVE CONTROLS: the same turn with no
/// need raises nothing, and a need in the turn before the person's `❯` row is not this turn's.
#[test]
fn a_need_earlier_in_the_turn_is_raised_at_its_break() {
    const LAST: &str = "The suite runs in the background; the docs are next.";
    let run = |tag: &str, above: Option<&'static str>, earlier: Option<&'static str>| {
        let drawn = move |_| waiting_turn(above, earlier, LAST);
        Script {
            policy: under(Autopilot::Cruise),
            drawn: &drawn,
            ..Script::new(tag, Occupancy::Observe, &[0, 30])
        }
        .run(&mut nothing)
    };
    let need =
        Some("NEEDS YOU: the release credential to sign 0.145.0; carrying on with the docs.");
    let cruise = run("carry-needs-earlier", None, need);
    let up = raised(&cruise);
    assert_eq!(up.len(), 1, "{:#?}", cruise.requests());
    assert!(
        up[0].contains("the release credential to sign 0.145.0"),
        "{up:#?}"
    );
    assert_eq!(carries(&cruise), 1, "{:#?}", cruise.writes());
    let plain = run(
        "carry-needs-earlier-none",
        None,
        Some("Tagged 0.145.0 on its branch."),
    );
    assert!(
        raised(&plain).is_empty(),
        "CONTROL: {:#?}",
        plain.requests()
    );
    assert_eq!(carries(&plain), 1, "{:#?}", plain.writes());
    let before = run("carry-needs-earlier-before", need, None);
    assert!(
        raised(&before).is_empty(),
        "CONTROL: the turn before the person's row is not this turn's: {:#?}",
        before.requests()
    );
}

/// A turn too tall for the pane under the person's `❯ ship 0.145.0`: `before` (rows above that
/// row), the row, a NEEDS YOU message, twenty tool calls and a background `Bash` call, then the
/// report; its first `above` rows are in the history, the rest on the screen.
fn tall_turn(before: &[&str], above: usize) -> (Vec<String>, Vec<String>) {
    let mut lines: Vec<String> = before.iter().map(ToString::to_string).collect();
    lines.extend(
        [
            "❯ ship 0.145.0",
            "",
            "⏺ NEEDS YOU: the release credential to sign 0.145.0; carrying on with the docs.",
            "",
        ]
        .map(ToString::to_string),
    );
    for i in 0..20 {
        lines.push(format!("⏺ Read(crates/aterm-agent/src/part_{i:02}.rs)"));
        lines.push("  ⎿  Read 40 lines".to_string());
    }
    lines.extend(
        [
            "⏺ Bash(targo --unverified test --workspace)",
            "  ⎿  Running in the background (↓ to manage)",
            "",
            "⏺ The suite runs in the background; the docs are next.",
            "",
        ]
        .map(ToString::to_string),
    );
    let mut screen = waiting(&[]);
    screen.splice(0..0, lines);
    let shown = screen.split_off(above);
    (screen, shown)
}

/// F1 past the screen's top: a NEEDS YOU line in a message of the turn that has scrolled into the
/// history is raised at the break, the turn read whole from the history as a point reads it, and
/// read once while the screen's words stand, though the break is read twice. NEGATIVE
/// CONTROL: a need in the history above the person's `❯` row is not this turn's.
#[test]
fn a_need_above_the_screen_is_raised_at_its_break() {
    let run = |tag: &str, before: &'static [&'static str], above: usize| {
        let (history, shown) = tall_turn(before, above);
        let drawn = move |_| shown.clone();
        let set = move |m: &mut Mock| {
            m.scrollback = Some(history.clone());
            m.alt_screen = Some(false);
        };
        Script {
            policy: under(Autopilot::Cruise),
            drawn: &drawn,
            set: &set,
            ..Script::new(tag, Occupancy::Observe, &[0, 30])
        }
        .run(&mut nothing)
    };
    let cruise = run("carry-needs-above", &[], 4);
    let up = raised(&cruise);
    assert_eq!(up.len(), 1, "{:#?}", cruise.requests());
    assert!(
        up[0].contains("the release credential to sign 0.145.0"),
        "{up:#?}"
    );
    // The mock moves its screen's generation on the walk's own read, so the carry's first send
    // is fenced off and sent again; one is entered.
    let entered = cruise
        .writes()
        .iter()
        .filter(|w| w.starts_with("key ") && w.ends_with(" enter"))
        .count();
    assert_eq!(entered, 1, "{:#?}", cruise.writes());
    let searches = cruise.requests().iter().filter(|r| *r == "search").count();
    assert_eq!(
        searches,
        2,
        "one walk up the history: {:#?}",
        cruise.requests()
    );
    let before = run(
        "carry-needs-above-before",
        &["⏺ NEEDS YOU: the signing key for 0.144.0", ""],
        6,
    );
    let up = raised(&before);
    assert!(
        up.iter().all(|r| !r.contains("0.144.0")),
        "CONTROL: {:#?}",
        before.requests()
    );
}

/// F2 at a break: a NEEDS YOU line Claude Code wrapped in its 118 cells onto a row that opens on a
/// capital (`… under Privacy and` / `Security in System Settings …`) reaches the window's host
/// whole, in the break's report and in an earlier message of its turn alike. NEGATIVE CONTROL: a
/// short need's next row, opening on a capital too, is a new line and stays out of the need.
#[test]
fn a_wrapped_need_at_a_break_is_raised_whole() {
    const NEED: &str = "grant aterm (dev).app both the Accessibility and the Screen Recording \
                        permissions under Privacy and Security in System Settings so the window \
                        lab can drive it.";
    const WRAPPED: &str = "NEEDS YOU: grant aterm (dev).app both the Accessibility and the Screen \
                           Recording permissions under Privacy and\nSecurity in System Settings \
                           so the window lab can drive it.";
    const LAST: &str = "The suite runs in the background; the docs are next.";
    let run = |tag: &str, earlier: Option<String>, last: String| {
        let host = Arc::new(DoorHost::default());
        let drawn = move |_| waiting_turn(None, earlier.as_deref(), &last);
        let cruise = Script {
            policy: under(Autopilot::Cruise),
            drawn: &drawn,
            host: Arc::clone(&host),
            ..Script::new(tag, Occupancy::Observe, &[0, 30])
        }
        .run(&mut nothing);
        let standing = host.standing.lock().expect("standing").clone();
        (cruise, standing)
    };
    let (report, told) = run(
        "carry-needs-wrapped",
        None,
        format!("The suite runs in the background.\n{WRAPPED}"),
    );
    assert_eq!(told, [NEED], "the break's report: {:#?}", report.requests());
    assert_eq!(raised(&report).len(), 1, "{:#?}", report.requests());
    let (earlier, told) = run(
        "carry-needs-wrapped-earlier",
        Some(WRAPPED.to_string()),
        LAST.to_string(),
    );
    assert_eq!(
        told,
        [NEED],
        "an earlier message: {:#?}",
        earlier.requests()
    );
    let (short, told) = run(
        "carry-needs-wrapped-short",
        None,
        "NEEDS YOU: the signing key\nEverything else is on main.".to_string(),
    );
    assert_eq!(
        told,
        ["the signing key"],
        "CONTROL: {:#?}",
        short.requests()
    );
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

/// The screen of the cruise stall of 2026-10-24..27, captured read-only from the live tab: a
/// Claude Code 2.1.310 turn over while three dynamic workflows it started run on, Claude Code's
/// own dim suggestion `keep going` in the composer, the caret at column 2 of row 54.
fn captured() -> Vec<String> {
    use aterm_phase::prompt::fixtures::{WAITING_WORKFLOWS_2_1_310, screen};
    screen(WAITING_WORKFLOWS_2_1_310)
}

/// The mock as the live tab measured it: the cursor on the composer's row at column 2, the
/// suggestion's glyphs `dim` (`cell 54 2` answered `OK k 999999 111318 dim`), or `none` where a
/// person typed them. The captured turn opens above the screen's top, so its needs are read
/// whole after the tail, and the mock serves that read the tail's screen at its generation (no
/// redraw between the two). A redraw between them, as a ticking panel timer makes, fences the
/// carry's first send off and the carry is sent again, entered once
/// (`a_need_above_the_screen_is_raised_at_its_break`).
fn as_measured(m: &mut Mock, attrs: &'static str) {
    m.cursor_on_caret = true;
    m.cursor_col = Some(2);
    m.cell_attrs = attrs;
    m.whole_sees_the_tail = true;
}

/// [`captured`] with its waiting row replaced by `live`, the diff panel beside it kept.
fn live_over_workflows(live: &str) -> Vec<String> {
    let mut r = captured();
    let at = r
        .iter()
        .position(|x| x.starts_with("✻ Waiting for 3 dynamic workflows"))
        .expect("the waiting row");
    let (head, panel) = r[at].split_once('│').expect("the panel's rule");
    r[at] = format!("{live:<w$}│{panel}", w = head.chars().count());
    r
}

fn captured_run(
    tag: &str,
    autopilot: Autopilot,
    drawn: &dyn Fn(usize) -> Vec<String>,
    attrs: &'static str,
    at_s: &[i64],
    before: Before<'_>,
) -> Ran {
    Script {
        policy: under(autopilot),
        drawn,
        set: &move |m| as_measured(m, attrs),
        ..Script::new(tag, Occupancy::Observe, at_s)
    }
    .run(before)
}

/// The carries the run entered: each carry's Enter, guarded on the carry's own row.
fn entered(ran: &Ran) -> usize {
    ran.writes()
        .iter()
        .filter(|w| {
            w.starts_with("key ")
                && w.ends_with(" enter")
                && w.contains(r"Autopilot\x20is\x20Cruise:\x20your\x20turn\x20ended")
        })
        .count()
}

/// THE STALL'S OWN SCREEN IS CARRIED, ONCE: under Cruise the captured break is a turn end the
/// harness continues; every later read of the same break, an hour of them, types nothing more
/// while the line stands untaken. Off types nothing there.
#[test]
fn the_captured_stall_screen_is_carried_once_under_cruise() {
    let cruise = captured_run(
        "carry-captured",
        Autopilot::Cruise,
        &|_| captured(),
        "dim",
        &[0, 30, 90, 600, 3600],
        &mut nothing,
    );
    assert_eq!(
        entered(&cruise),
        1,
        "{:#?}\n{:#?}",
        cruise.writes(),
        cruise.noted
    );
    assert!(
        cruise
            .requests()
            .iter()
            .filter(|r| r.starts_with("send "))
            .all(|r| is_carry(r)),
        "every send is the carry: {:#?}",
        cruise.writes()
    );
    let off = captured_run(
        "carry-captured-off",
        Autopilot::Off,
        &|_| captured(),
        "dim",
        &[0, 30, 3600],
        &mut nothing,
    );
    assert!(off.writes().is_empty(), "{:#?}", off.writes());
    // CONTROL: the same words typed by a person (their cells plain) are their draft.
    let drafted = captured_run(
        "carry-captured-draft",
        Autopilot::Cruise,
        &|_| captured(),
        "none",
        &[0, 30, 3600],
        &mut nothing,
    );
    assert!(drafted.writes().is_empty(), "{:#?}", drafted.writes());
    assert!(
        drafted
            .noted
            .iter()
            .any(|l| l.starts_with("DEFERRED ") && l.ends_with("held:draft")),
        "{:#?}",
        drafted.noted
    );
}

/// THE BREAK OF THE TAB OF 2026-10-27, whose composer held `ok, keep going` over a shell its turn
/// left, with an idle compaction under the done row, is a break either way the words were drawn:
/// Claude Code's dim suggestion is no draft, so Cruise carries the break once; the same words
/// typed by a person hold it.
#[test]
fn the_break_under_an_idle_compaction_is_carried_over_a_suggestion_and_held_by_a_draft() {
    use aterm_phase::prompt::fixtures::{IDLE_COMPACTED_OVER_A_SHELL_2_1_312, screen};
    let drawn = |_: usize| screen(IDLE_COMPACTED_OVER_A_SHELL_2_1_312);
    let suggested = captured_run(
        "carry-compacted",
        Autopilot::Cruise,
        &drawn,
        "dim",
        &[0, 30, 3600],
        &mut nothing,
    );
    assert_eq!(
        entered(&suggested),
        1,
        "{:#?}\n{:#?}",
        suggested.writes(),
        suggested.noted
    );
    let drafted = captured_run(
        "carry-compacted-draft",
        Autopilot::Cruise,
        &drawn,
        "none",
        &[0, 30, 3600],
        &mut nothing,
    );
    assert!(drafted.writes().is_empty(), "{:#?}", drafted.writes());
    assert!(
        drafted
            .noted
            .iter()
            .any(|l| l.starts_with("DEFERRED ") && l.ends_with("held:draft")),
        "{:#?}",
        drafted.noted
    );
}

/// A WAKE'S TURN END IS CARRIED TOO: the agent takes the carry (or the work it waits on ends and
/// Claude Code wakes it with a notification turn), works, and its turn ends over its work again:
/// that end is carried at once after real work, and once.
#[test]
fn the_turn_a_wake_opens_is_carried_at_its_end() {
    let cfg = under(Autopilot::Cruise);
    let drawn = |i: usize| {
        if i == 1 {
            live_over_workflows("· Wibbling… (1m 2s · ↓ 3.1k tokens)")
        } else {
            captured()
        }
    };
    let ran = captured_run(
        "carry-captured-wake",
        Autopilot::Cruise,
        &drawn,
        "dim",
        // The carry at 0 s (the settle is zero here); a live turn read at 150 s (the carry's
        // answer, or Claude Code's own wake when the work ends); its end at 300 s, past
        // `min_work` after the carry, read again at 310 s and 900 s.
        &[0, 150, 300, 310, 900],
        &mut |s, i| {
            if i == 1 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        },
    );
    assert_eq!(entered(&ran), 2, "{:#?}\n{:#?}", ran.writes(), ran.noted);
}

/// NEGATIVE CONTROLS: a live turn over the same three workflows (a spinner, or a foreground
/// command still running) is no break, and nothing is carried there however long it runs.
#[test]
fn a_live_turn_over_the_captured_workflows_is_never_carried() {
    for live in [
        "· Wibbling… (4m 36s · ↓ 16.4k tokens)",
        "✢ Running… (12s · esc to interrupt)",
    ] {
        let drawn = move |_| live_over_workflows(live);
        let ran = captured_run(
            "carry-captured-live",
            Autopilot::Cruise,
            &drawn,
            "dim",
            &[0, 30, 600, 3600],
            &mut nothing,
        );
        assert_eq!(carries(&ran), 0, "{live}: {:#?}", ran.writes());
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
    /// The turn running asks a question the harness answers ([`answer_the_box`]), and runs on,
    /// drawn live: the host counts the answer's Enter a submission, as it counts any Enter over a
    /// key pressed since the last.
    Asked,
    /// As [`Read::Asked`], the recommended option focused as drawn, so the answer is an Enter
    /// alone: over nothing pressed since the last Enter, the host counts no submission.
    Focused,
    /// As [`Read::Asked`], and the break the turn ends on is drawn before any read sees it run.
    Unseen,
    /// A break where the host types its own line (`IdleHost::at_background`).
    Hosted,
}

/// One read of [`carried_by`]: when (in seconds), what it draws, how the loop steps it, and the
/// session's submissions served on it, past those a box's answer adds (none served at `None`).
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

/// A POINT'S DOWN READ IS SPENT ONCE THE AGENT MOVES ON: the loop's reach read at a point
/// (`reach_seen`, which held that point's act) is that point's alone. Once a turn runs past it
/// (`Session::took`; the person's message here), a break that turn ends on is held only by the
/// reach its write reads afresh, so a Down from before the turn holds no carry while the route is
/// back (it held `held:reach` at every break for the rest of the outage's cap, up to an hour from
/// its start). The controls: the break's own read Down still holds the carry, and a break with no
/// turn read since the point is still held on the point's read. NEGATIVE CONTROLS (run): with
/// `Session::took`'s `self.reach_seen = None` removed, the moved-on run carries nothing and fails
/// at "a Down read before the turn holds no carry"; with the read cleared at every break's step
/// instead, the run with no turn carries and fails at its count.
#[test]
fn a_points_down_read_holds_no_break_once_the_agent_moved_on() {
    use crate::supervise::policy::turn_end::Reach;
    let cfg = under(Autopilot::Cruise);
    // `moved`: the person's message runs a turn at 0 s, its break at 150 s, past their grace;
    // `down`: the break's write reads the point's Down afresh too.
    let run = |moved: bool, down: bool| {
        let host = Arc::new(DoorHost::default());
        let h = Arc::clone(&host);
        Script {
            policy: under(Autopilot::Cruise),
            drawn: &|_| waiting(PLAIN),
            host,
            ..Script::new("carry-reach-spent", Occupancy::Observe, &[0, 150])
        }
        .run(&mut |s, i| {
            if i == 0 {
                let seen = Reach::Down {
                    since: s
                        .fx
                        .mono()
                        .checked_sub(Duration::from_secs(120))
                        .expect("an earlier instant"),
                };
                // The point's read, its act held on it, two minutes into the outage.
                s.reach_seen = Some(seen);
                if down {
                    *h.reach.lock().expect("reach") = seen;
                }
            }
            if !moved {
                return;
            }
            let at: u64 = if i == 0 { 0 } else { 150 };
            s.ctl.m.screens = vec![if i == 0 { live_turn() } else { waiting(PLAIN) }];
            s.ctl.m.served = 0;
            // Their key 2 s before the turn's read, and their count past the carry's: the screen's
            // read says it too.
            let ms = (at + 2) * 1000;
            s.ctl.m.human_seq = Some(8);
            s.ctl.m.human_ms = Some(ms);
            s.ctl.m.human = vec![Box::leak(ms.to_string().into_boxed_str())];
            if i == 0 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        })
    };
    let held = |ran: &Ran| {
        ran.noted.iter().any(|l| {
            l.starts_with("DEFERRED ") && l.contains(RULE_BACKGROUND) && l.ends_with("held:reach")
        })
    };
    let moved = run(true, false);
    assert_eq!(
        carries(&moved),
        1,
        "a Down read before the turn holds no carry: {:#?}\n{:#?}",
        moved.noted,
        moved.requests
    );
    assert!(!held(&moved), "{:#?}", moved.noted);
    let down = run(true, true);
    assert_eq!(carries(&down), 0, "{:#?}", down.requests);
    assert!(
        held(&down),
        "the break's own Down holds it: {:#?}",
        down.noted
    );
    let stood = run(false, false);
    assert_eq!(carries(&stood), 0, "{:#?}", stood.requests);
    assert!(
        held(&stood),
        "the point's Down holds a break with no turn since: {:#?}",
        stood.noted
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

/// The harness answers the question box the turn raised, as the loop answers a box read up
/// ([`Session::auto_read`]): Claude Code's AskUserQuestion, its recommended option the second row,
/// so a Down and then Enter on that row, or with `focused` the first, focused as drawn, so Enter
/// alone. Each screen it reads is served `submits`, and the person's last key as the run stamps
/// it, as never where it stamps none (a host stamps every read); returns the submissions the host
/// counts of its keys, an Enter over a key pressed since the last (the carry's own Enter the last
/// before them).
fn answer_the_box(
    s: &mut Session<'_, Logged<'_>>,
    cfg: &SupervisorConfig,
    submits: Option<u64>,
    focused: bool,
) -> u64 {
    use aterm_phase::prompt::fixtures as f;
    let asked = f::screen(if focused {
        f::QUESTION_ONE
    } else {
        f::QUESTION_RECOMMENDED_SECOND
    });
    let moved: Vec<String> = asked
        .iter()
        .map(|row| match row.as_str() {
            "❯ 1. Built-in runner" => "  1. Built-in runner".to_string(),
            "  2. Nextest (Recommended)" => "❯ 2. Nextest (Recommended)".to_string(),
            _ => row.clone(),
        })
        .collect();
    assert_eq!(
        asked == moved,
        focused,
        "PRECONDITION: the focus moves to the second row, or stands on the first"
    );
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: true,
            ..cfg.clone()
        },
        ..auto(30)
    };
    s.ctl.m.screens = if focused {
        vec![asked, live_turn()]
    } else {
        vec![asked, moved, live_turn()]
    };
    s.ctl.m.served = 0;
    s.ctl.m.submits_on = submits
        .map(|n| (0..3).map(|k| (k, n)).collect())
        .unwrap_or_default();
    s.set_box_settle(Duration::ZERO);
    let stamped = s.ctl.m.human.clone();
    if stamped.is_empty() {
        s.ctl.m.human = vec!["null"];
    }
    let from = s.ctl.seen.len();
    let screen = s.screen().expect("the box's read");
    let turn = s.turn_of(screen);
    assert_eq!(turn.phase, Phase::Prompt, "PRECONDITION: a box is up");
    let deadline = s.fx.mono() + Duration::from_secs(60);
    let mut heard = Heard::default();
    let step = s
        .auto_read(turn, &opts, &[], &mut Vec::new(), deadline, &mut heard)
        .expect("the box's answer");
    s.ctl.m.human = stamped;
    let requests: Vec<&str> = s.ctl.seen[from..].iter().map(|(r, _)| r.as_str()).collect();
    assert!(
        matches!(step, crate::supervise::run::Step::Again { .. }),
        "PRECONDITION: answered, the loop reads again: {requests:#?} {:#?}",
        heard.noted
    );
    let keys: Vec<&str> = requests
        .iter()
        .copied()
        .filter(|r| r.starts_with("key "))
        .collect();
    let pressed: Vec<&str> = keys
        .iter()
        .map(|k| k.rsplit_once(' ').map_or(*k, |(_, key)| key))
        .collect();
    let expected: &[&str] = if focused {
        &["enter"]
    } else {
        &["down", "enter"]
    };
    assert_eq!(
        pressed, expected,
        "PRECONDITION: the answer's keys: {requests:#?} {:#?}",
        heard.noted
    );
    let mut composed = false;
    let mut submitted = 0;
    for key in pressed {
        if key == "enter" {
            submitted += u64::from(std::mem::take(&mut composed));
        } else {
            composed = true;
        }
    }
    submitted
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
/// seeds them. A turn opened (any but [`Read::Break`], [`Read::Uncounted`], [`Read::Scrolled`],
/// [`Read::Unseen`] and [`Read::Hosted`]) is read running (`Session::took`) before its read, and a
/// turn drawn live ends the break before it, as in the loop. Each read is served the screen drawn
/// for it, whatever reads a carry made, and its submissions past those, each a box's answer made
/// or the host's line typed before it, as the host counts them.
fn carried_after(
    tag: &str,
    settle: Duration,
    reads: &[Then],
    earlier: Option<&Earlier>,
) -> (Vec<usize>, Vec<String>) {
    let (at, noted, _) = carried_into(tag, settle, reads, earlier, None);
    (at, noted)
}

/// [`carried_after`], and the ledger's text as the run left it. A run `from` a start of its own
/// (unix ms; [`T0`] at `None`) keeps the virtual clock, so its rows are stamped on the clock its
/// reads run on, as a later loop's books read them.
fn carried_into(
    tag: &str,
    settle: Duration,
    reads: &[Then],
    earlier: Option<&Earlier>,
    from: Option<i64>,
) -> (Vec<usize>, Vec<String>, String) {
    let cfg = under(Autopilot::Cruise);
    let start = from.unwrap_or(T0);
    let at_s: Vec<i64> = reads.iter().map(|&(at, ..)| at).collect();
    let mut typed = Vec::new();
    let mut gestured: Option<i64> = earlier.and_then(|e| e.touched);
    let mut pressed = 0;
    let mut lined = 0;
    let drawn = |_| waiting(PLAIN);
    let steps = |i: usize| reads[i].2;
    // No earlier tab's books and no journal beside them.
    let books = crate::supervise::test_scratch_path("carry-books", tag).join("s-1.jsonl");
    let host = Arc::new(DoorHost {
        launch: earlier.and_then(|e| e.launch),
        ..DoorHost::default()
    });
    let run = Script {
        policy: under(Autopilot::Cruise),
        drawn: &drawn,
        steps: &steps,
        host: Arc::clone(&host),
        seed: (earlier.is_some() || from.is_some())
            .then(|| (earlier.map(|e| e.ledger.clone()).unwrap_or_default(), start)),
        virtual_time: from.is_some(),
        ..Script::new(tag, Occupancy::Observe, &at_s)
    }
    .run(&mut |s, i| {
        if i == 0
            && let Some(earlier) = earlier
        {
            s.seed_from_ledger_text(&books, &earlier.ledger, start, earlier.lifted_off);
        }
        s.setup.background_settle = settle;
        typed.push(s.ctl.seen.iter().filter(|(r, _)| is_carry(r)).count());
        let (at, read, _, submits) = reads[i];
        pressed += std::mem::take(&mut lined);
        host.wants.store(read == Read::Hosted, Ordering::SeqCst);
        if read == Read::Hosted {
            *host.notice.lock().expect("notice") = Some("[harness] the host's own line".into());
            lined = 1;
        }
        let carry = carry_row();
        s.ctl.m.screens = vec![match read {
            Read::Break
            | Read::Uncounted
            | Read::Taken
            | Read::Scrolled
            | Read::Unseen
            | Read::Hosted => waiting(PLAIN),
            Read::Live | Read::Theirs | Read::Asked | Read::Focused => live_turn(),
            Read::Woke => live_under(
                &[&carry],
                "⏺ Background command \"Watch the release\" completed (exit code 0)",
            ),
            Read::Remote => live_under(&[&carry, "❯ what's the status?"], "⏺ Checking."),
        }];
        s.ctl.m.served = 0;
        s.ctl.m.submits_on = submits.map(|n| (0, n + pressed)).into_iter().collect();
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
        if matches!(read, Read::Asked | Read::Focused | Read::Unseen) {
            pressed += answer_the_box(s, &cfg, submits.map(|n| n + pressed), read == Read::Focused);
            s.ctl.m.screens = vec![if read == Read::Unseen {
                waiting(PLAIN)
            } else {
                live_turn()
            }];
            s.ctl.m.served = 0;
            s.ctl.m.submits_on = submits.map(|n| (0, n + pressed)).into_iter().collect();
        }
        if !matches!(
            read,
            Read::Break | Read::Uncounted | Read::Scrolled | Read::Unseen | Read::Hosted
        ) {
            s.took(7, Some(&cfg), &mut Heard::default());
        }
    });
    let hosted = reads.iter().filter(|r| r.1 == Read::Hosted).count();
    assert_eq!(
        host.calls()
            .iter()
            .filter(|c| *c == "at_background")
            .count(),
        hosted,
        "PRECONDITION: the host typed its line at each break it was offered: {:#?}",
        run.noted
    );
    typed.push(carries(&run));
    let at = typed.windows(2).map(|w| w[1] - w[0]).collect();
    (at, run.noted, run.ledger)
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

/// A CARRY'S ANSWER NO READ SAW ENDS AT THE SETTLED BREAK AFTER IT: the agent that answers the
/// carry before the loop's next read leaves a break there, and the wake that opens a turn later
/// is measured from its own live edge, never from the carry's typing. A break short of its
/// settle before the answer is drawn live ends no claim: that answer still counts from the typing,
/// but a wake the first read of the turn draws under the carry's prompt row counts from that read.
#[test]
fn a_carrys_answer_no_read_saw_ends_at_its_settled_break() {
    let settle = Duration::from_secs(20);
    let lead = [
        (0, Read::Break),
        (30, Read::Break),
        (40, Read::Taken),
        (60, Read::Break),
        // The second carry at 150 waits 4 minutes for 390.
        (150, Read::Break),
        (153, Read::Break),
    ];
    // Its answer is over by the read at 153, and the break settles at 173. The wake at 330 runs
    // 10 s, 180 s and more after the carry, and its break at 340 settles at 360: a short answer,
    // so the carry waits there and goes on the ladder at 390.
    let mut unseen = lead.to_vec();
    unseen.extend([
        (173, Read::Break),
        (330, Read::Woke),
        (340, Read::Break),
        (360, Read::Break),
        (390, Read::Break),
    ]);
    let (wake, wake_noted) = carried_over("carry-unseen-answer", settle, &unseen);
    assert_eq!(wake, [0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 1], "{wake_noted:#?}");
    assert_eq!(
        waits(&wake_noted).last().map(String::as_str),
        Some("2 short answer(s) in a row"),
        "{wake_noted:#?}"
    );
    // The answer is drawn live at 160, short of the break's settle, and runs to 275: 125 s from
    // the typing, 115 s from its live edge. That is real work, and its break goes at its settle.
    let mut seen = lead.to_vec();
    seen.extend([
        (160, Read::Live),
        (275, Read::Break),
        (295, Read::Break),
        (390, Read::Break),
    ]);
    let (answer, answer_noted) = carried_over("carry-late-answer", settle, &seen);
    assert_eq!(answer, [0, 1, 0, 0, 1, 0, 0, 0, 1, 0], "{answer_noted:#?}");
    // The program's own wake drawn at 160 instead, the answer over unseen: 115 s of its own, a
    // short answer, so the carry goes on the ladder at 390.
    let mut woke = seen;
    woke[6].1 = Read::Woke;
    let (wake, wake_noted) = carried_over("carry-wake-in-settle", settle, &woke);
    assert_eq!(wake, [0, 1, 0, 0, 1, 0, 0, 0, 0, 1], "{wake_noted:#?}");
}

/// A WAKE IN THE ANSWER'S PLACE OPENS ITS TURN AT ITS FIRST READ: the turn a settled break ends
/// bounds the person's key it holds the go-ahead for by when it opened
/// (`TurnEndState::break_keyed`), so the program's own wake drawn under the carry's prompt row
/// at the first read after the carry (its answer over unseen) opens the turn at that read, as its
/// busy time counts from there, and a key between the carry and the wake is no hand in it. The
/// control: an answer drawn live opens the turn at the carry's typing.
#[test]
fn a_wake_in_the_answers_place_opens_its_turn_at_its_first_read() {
    let opened = |tag: &str, rows: Vec<String>| -> Option<Duration> {
        let cfg = under(Autopilot::Cruise);
        let drawn = |_| waiting(PLAIN);
        let mut ago = None;
        let ran = Script {
            policy: cfg,
            drawn: &drawn,
            ..Script::new(tag, Occupancy::Observe, &[0, 30, 40])
        }
        .run(&mut |s, i| {
            s.setup.background_settle = Duration::from_secs(20);
            if i != 2 {
                return;
            }
            assert_eq!(
                s.ctl.seen.iter().filter(|(r, _)| is_carry(r)).count(),
                1,
                "PRECONDITION: the carry went at 30"
            );
            s.ctl.m.screens = vec![rows.clone()];
            s.ctl.m.served = 0;
            let screen = s.screen().expect("the turn's first read");
            let now = s.fx.mono();
            s.cruise_carry.live(false, || now);
            let reader = s.reader(&screen.rows);
            let _ = s.cruise_carry.read(reader, &screen, || now);
            let (_, turn) = s
                .cruise_carry
                .at_break(&s.turn_end.timing, &screen, now, true);
            ago = turn.map(|(from, _)| now.saturating_duration_since(from));
            s.ctl.m.screens = vec![waiting(PLAIN)];
            s.ctl.m.served = 0;
        });
        assert_eq!(carries(&ran), 1, "{:#?}", ran.noted);
        ago
    };
    let woke = live_under(
        &[&carry_row()],
        "⏺ Background command \"Watch the release\" completed (exit code 0)",
    );
    assert_eq!(opened("carry-wake-opens", woke), Some(Duration::ZERO));
    assert_eq!(
        opened("carry-answer-opens", live_turn()),
        Some(Duration::from_secs(10))
    );
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

/// THE HARNESS'S OWN ANSWER IS NO ONE ELSE'S MESSAGE: the host counts the Enter the harness
/// answers a box with inside a turn (a Down, then Enter) a submission, so a wake whose question
/// the harness answered, and the carry's answer with one in it, keep the carry on the ladder.
/// Another driver's Enter there starts the books again, beside the harness's answer or alone.
#[test]
fn a_box_the_harness_answers_inside_a_turn_keeps_the_ladder() {
    let settle = Duration::from_secs(20);
    let then = |reads: &[(i64, Read, u64)]| -> Vec<Then> {
        reads
            .iter()
            .map(|&(at, read, n)| (at, read, Step::Stepped, Some(n)))
            .collect()
    };
    // The carries at 30 and 150 each submit one; the second, answered short at 160, waits 4
    // minutes for 390. The program's own wake at 330 asks a question at 335, which the harness
    // answers; its turn ends on the work at 345, settled at 365.
    let ladder = [
        (0, Read::Break, 3),
        (30, Read::Break, 3),
        (40, Read::Taken, 4),
        (60, Read::Break, 4),
        (150, Read::Break, 4),
        (160, Read::Taken, 5),
        (180, Read::Break, 5),
        (330, Read::Woke, 5),
        (335, Read::Asked, 5),
        (345, Read::Break, 5),
        (365, Read::Break, 5),
        (390, Read::Break, 5),
    ];
    let (woke, woke_noted) = carried_by("carry-asked-wake", settle, &then(&ladder));
    assert_eq!(
        woke,
        [0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1],
        "{woke_noted:#?}"
    );
    assert_eq!(
        waits(&woke_noted).last().map(String::as_str),
        Some("2 short answer(s) in a row"),
        "{woke_noted:#?}"
    );
    // The second carry's own answer asks the question at 165: its turn ends at 180, settled at
    // 200, the carry's Enter and the answer's both the harness's.
    let carried = [
        (0, Read::Break, 3),
        (30, Read::Break, 3),
        (40, Read::Taken, 4),
        (60, Read::Break, 4),
        (150, Read::Break, 4),
        (160, Read::Live, 5),
        (165, Read::Asked, 5),
        (180, Read::Break, 5),
        (200, Read::Break, 5),
        (390, Read::Break, 5),
    ];
    let (answer, answer_noted) = carried_by("carry-asked-answer", settle, &then(&carried));
    assert_eq!(answer, [0, 1, 0, 0, 1, 0, 0, 0, 0, 1], "{answer_noted:#?}");
    // Its question asked at 152, before any read sees the answer run: the carry's Enter still
    // counts from the break it was typed at, the turn opened on both Enters.
    let mut early = carried;
    early[5] = (152, Read::Asked, 5);
    early[6] = (165, Read::Live, 5);
    let (answer, answer_noted) = carried_by("carry-asked-early", settle, &then(&early));
    assert_eq!(answer, [0, 1, 0, 0, 1, 0, 0, 0, 0, 1], "{answer_noted:#?}");
    // Another driver's Enter where the harness answered, beside its answer or in place of it:
    // someone else's message, so the carry goes at the turn's settle, the wake's and the carry's
    // answer's alike.
    let theirs = |tag: &str, reads: &[(i64, Read, u64)], at: usize, read: Read| {
        let mut reads = reads.to_vec();
        for r in &mut reads[at..] {
            r.2 += 1;
        }
        reads[at].1 = read;
        carried_by(tag, settle, &then(&reads))
    };
    for read in [Read::Asked, Read::Live] {
        let (woke, noted) = theirs(&format!("carry-theirs-wake-{read:?}"), &ladder, 8, read);
        assert_eq!(
            woke,
            [0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0],
            "{read:?}: {noted:#?}"
        );
        let (answer, noted) = theirs(&format!("carry-theirs-answer-{read:?}"), &carried, 6, read);
        assert_eq!(
            answer,
            [0, 1, 0, 0, 1, 0, 0, 0, 1, 0],
            "{read:?}: {noted:#?}"
        );
    }
}

/// AN ENTER OVER NOTHING PRESSED IS NO SUBMISSION: the host counts an Enter only over a key pressed
/// since the last, so the harness's answer to a box whose option has the focus as drawn (Enter
/// alone, as a focused recommendation or a Muse Code approval is answered) submits nothing, and
/// another driver's Enter beside it is still someone else's message: the carry goes at the turn's
/// settle. Alone, the answer keeps the ladder, as the Down and Enter does
/// (`a_box_the_harness_answers_inside_a_turn_keeps_the_ladder`).
#[test]
fn an_enter_over_nothing_pressed_hides_no_one_elses_message() {
    let settle = Duration::from_secs(20);
    let then = |reads: &[(i64, Read, u64)]| -> Vec<Then> {
        reads
            .iter()
            .map(|&(at, read, n)| (at, read, Step::Stepped, Some(n)))
            .collect()
    };
    // The second carry, answered short at 160, waits 4 minutes for 390. The program's own wake
    // at 330 asks at 335; its turn ends on the work at 345, settled at 365.
    let wake = [
        (0, Read::Break, 3),
        (30, Read::Break, 3),
        (40, Read::Taken, 4),
        (60, Read::Break, 4),
        (150, Read::Break, 4),
        (160, Read::Taken, 5),
        (180, Read::Break, 5),
        (330, Read::Woke, 5),
        (335, Read::Focused, 5),
        (345, Read::Break, 5),
        (365, Read::Break, 5),
        (390, Read::Break, 5),
    ];
    // The second carry's own answer asks at 165; its turn ends at 180, settled at 200.
    let answer = [
        (0, Read::Break, 3),
        (30, Read::Break, 3),
        (40, Read::Taken, 4),
        (60, Read::Break, 4),
        (150, Read::Break, 4),
        (160, Read::Live, 5),
        (165, Read::Focused, 5),
        (180, Read::Break, 5),
        (200, Read::Break, 5),
        (390, Read::Break, 5),
    ];
    for (tag, reads, at, ladder, settled) in [
        (
            "wake",
            &wake[..],
            8,
            &[0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1][..],
            &[0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0][..],
        ),
        (
            "answer",
            &answer[..],
            6,
            &[0, 1, 0, 0, 1, 0, 0, 0, 0, 1],
            &[0, 1, 0, 0, 1, 0, 0, 0, 1, 0],
        ),
    ] {
        let (alone, noted) = carried_by(&format!("carry-focused-{tag}"), settle, &then(reads));
        assert_eq!(alone, ladder, "{tag}: {noted:#?}");
        let mut theirs = reads.to_vec();
        for r in &mut theirs[at..] {
            r.2 += 1;
        }
        let (beside, noted) = carried_by(
            &format!("carry-focused-theirs-{tag}"),
            settle,
            &then(&theirs),
        );
        assert_eq!(beside, settled, "{tag}: {noted:#?}");
    }
}

/// THE HOST'S OWN LINE IS THE HARNESS'S: the host types its line at a break, apart from the loop's
/// writes, and the turn it opens keeps the ladder though the loop answered a box no read saw run
/// before it (that Enter in the count beside the line's). Another driver's Enter in the line's
/// turn still starts the books again.
#[test]
fn the_hosts_line_after_an_answer_no_read_saw_is_the_harnesss() {
    let settle = Duration::from_secs(20);
    // The second carry, answered short at 160, settled at 200, waits 4 minutes for 390. A wake's
    // question the harness answers is up at 210, its work back on the break there, read afresh;
    // the break settles at 230, where the host types its line. Its turn runs from 240 to 250,
    // settled at 270.
    let reads = [
        (0, Read::Break, 3),
        (30, Read::Break, 3),
        (40, Read::Taken, 4),
        (60, Read::Break, 4),
        (150, Read::Break, 4),
        (160, Read::Taken, 5),
        (180, Read::Break, 5),
        (200, Read::Break, 5),
        (210, Read::Unseen, 5),
        (230, Read::Hosted, 5),
        (240, Read::Live, 5),
        (250, Read::Break, 5),
        (270, Read::Break, 5),
        (390, Read::Break, 5),
    ];
    let then = |reads: &[(i64, Read, u64)]| -> Vec<Then> {
        reads
            .iter()
            .map(|&(at, read, n)| {
                let step = if read == Read::Unseen {
                    Step::Again
                } else {
                    Step::Stepped
                };
                (at, read, step, Some(n))
            })
            .collect()
    };
    let (lined, noted) = carried_by("carry-host-line", settle, &then(&reads));
    assert_eq!(
        lined,
        [0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        "{noted:#?}"
    );
    let mut theirs = reads;
    for r in &mut theirs[10..] {
        r.2 += 1;
    }
    let (beside, noted) = carried_by("carry-host-line-theirs", settle, &then(&theirs));
    assert_eq!(
        beside,
        [0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0],
        "{noted:#?}"
    );
}

/// An Enter the server took submits only over a key pressed since the last, as the host counts
/// them: a `key` Enter over the loop's own key, a `turn` that pressed one, a `send` ending in a
/// newline over its text. An Enter over nothing pressed submits nothing; a key that is no Enter,
/// text with no newline, and a `turn` that typed and pressed none leave a key pressed. An Enter
/// skipped or refused, a `turn` refused before it typed, and a read change nothing.
#[test]
fn an_enter_submits_only_over_a_key_pressed_since_the_last() {
    use crate::supervise::run::cruise_carry_loop::submits_of;
    let took = ok("OK seq=9\n");
    let skipped = ok("OK skipped reason=changed seq=9\n");
    let halted = err("halted");
    let turned = ok("OK id=1 submitted=1 status=settled seq=9 dur_ms=600 hash=0\n");
    let pressed = ok("OK id=1 submitted=0 pressed=1 status=settled seq=9 dur_ms=600 hash=0\n");
    let unpressed = ok("OK id=1 submitted=0 pressed=0 status=settled seq=9 dur_ms=600 hash=0\n");
    let unsent = err(aterm_types::control_verbs::TURN_SUBMIT_BUSY_REPLY
        .strip_prefix("ERR ")
        .expect("an ERR reply"));
    let enter = &["key", "if-gen=1.9", "if=x", "enter"][..];
    let turn = &["turn", "idle=600", "go on"][..];
    for (args, reply, composed, after) in [
        (enter, &took, true, (1, false)),
        (enter, &took, false, (0, false)),
        (
            &["@s-1", "key", "act=answer", "if=x", "enter"],
            &took,
            true,
            (1, false),
        ),
        (
            &["key", "if-gen=1.9", "if=x", "down"],
            &took,
            false,
            (0, true),
        ),
        (&["key", "if-gen=1.9", "if=x", "1"], &took, false, (0, true)),
        (enter, &skipped, true, (0, true)),
        (enter, &halted, true, (0, true)),
        (turn, &turned, false, (1, false)),
        (turn, &pressed, false, (1, false)),
        (turn, &unpressed, false, (0, true)),
        (turn, &unsent, false, (0, true)),
        (turn, &halted, false, (0, false)),
        (
            &["send", "if-gen=1.9", "--", "go on\\n"],
            &took,
            false,
            (1, false),
        ),
        (
            &["send", "if-gen=1.9", "--", "go on\n"],
            &took,
            false,
            (1, false),
        ),
        (
            &["send", "if-gen=1.9", "--", "\\n"],
            &took,
            false,
            (0, false),
        ),
        (
            &["send", "if-gen=1.9", "--", "\\n"],
            &took,
            true,
            (1, false),
        ),
        (
            &["send", "if-gen=1.9", "--", "go on"],
            &took,
            false,
            (0, true),
        ),
        (
            &["send", "if-gen=1.9", "--", "go on"],
            &skipped,
            false,
            (0, false),
        ),
        (&["text", "--json", "tail=40"], &took, true, (0, true)),
    ] {
        assert_eq!(
            submits_of(args, reply, composed),
            after,
            "{args:?} {reply:?} over {composed}"
        );
    }
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

/// A loop before this one's ledger, its text as that loop left it.
fn ledger_of(ledger: String) -> Earlier {
    Earlier {
        ledger,
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

/// EACH CARRY'S ROW SAYS THE STREAK IT WAS TYPED AT: a carry whose answer before it ran real
/// work goes at once on books started again in memory alone, so its gap from the one before is
/// longer than that one's rung, never shorter, and no gap tells it from the next carry of a run.
/// A loop that restarts on carries 15, 12, 20 and 25 minutes apart, each typed at a streak of one,
/// and the agent's 20-second answer to the last carries on the ladder's first rung, as the loop
/// that typed them would have, never on the climbing streak their gaps read. NEGATIVE CONTROL:
/// rows written before they said it (none of the note) still replay by their gaps, a run of five.
#[test]
fn a_restart_takes_the_streak_each_carry_was_typed_at() {
    // Carries at 0, 15, 27, 47 and 72 minutes, each answer but the last running to the break
    // the next goes at; the last answered in 20 s. The loop restarts a minute after it.
    let mut before = vec![(0, Read::Break)];
    for (from, to) in [(0, 15), (15, 27), (27, 47), (47, 72)] {
        before.extend([(from * 60 + 10, Read::Live), (to * 60, Read::Break)]);
    }
    before.extend([(72 * 60 + 10, Read::Live), (72 * 60 + 30, Read::Break)]);
    let (typed, typed_noted, ledger) = carried_into(
        "carry-streak-typed",
        Duration::ZERO,
        &stepped(&before),
        None,
        Some(T0 - 73 * MIN),
    );
    assert_eq!(
        typed,
        [1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 0],
        "PRECONDITION: each carry goes at once after real work: {typed_noted:#?}"
    );
    let stamped: Vec<i64> = ledger
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|v| v.get("rule_id").and_then(aterm_json::Value::as_str) == Some(RULE_BACKGROUND))
        .filter_map(|v| v.get("ts").and_then(aterm_json::Value::as_i64))
        .map(|ts| (T0 - ts) / 1000)
        .collect();
    assert_eq!(
        stamped,
        [73 * 60, 58 * 60, 46 * 60, 26 * 60, 60],
        "PRECONDITION: the carries' rows are stamped when they went, in seconds before the \
         restart: {ledger}"
    );
    let restart = stepped(&[(0, Read::Break), (30, Read::Break), (90, Read::Break)]);
    let legacy = carried_before(&[73, 58, 46, 26, 1], &[]);
    let wrong: Vec<_> = [
        (
            "noted",
            ledger_of(ledger),
            [0, 0, 1],
            "1 short answer(s) in a row",
        ),
        ("legacy", legacy, [0, 0, 0], "5 short answer(s) in a row"),
    ]
    .into_iter()
    .filter_map(|(tag, earlier, carried, wait)| {
        let (at, noted) = carried_after(
            &format!("carry-streak-{tag}"),
            Duration::ZERO,
            &restart,
            Some(&earlier),
        );
        let first = waits(&noted).first().cloned();
        (at != carried || first.as_deref() != Some(wait)).then_some((tag, at, first, noted))
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
/// carry keeps the ladder at a restart on the break; a turn still running at the restart starts
/// the books again anyway, its work unread (`a_turn_running_at_a_restart_starts_the_books_again`).
#[test]
fn a_persons_message_between_the_last_carry_and_a_restart_starts_the_books_again() {
    let kept = carried_before(&[67, 65, 61, 53, 37, 5], &[]);
    let mut wrong = Vec::new();
    for (start, first, after, before) in [
        ("live", Read::Live, [0, 1, 0], [0, 1, 0]),
        ("break", Read::Break, [1, 0, 0], [0, 0, 0]),
    ] {
        let reads = stepped(&[(0, first), (20, Read::Break), (40, Read::Break)]);
        for (tag, touched, carried) in [("after", -180, after), ("before", -400, before)] {
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

/// A MAC WAKE'S SETTLE HOLDS THE CARRY UNTIL IT RUNS OUT, NEVER FOR GOOD (2026-10-29): a wake the
/// loop measures at a break (`Session::watch_sleep`) settles for `WAKE_SETTLE` and the session's
/// share of the spread, and the carry waits that out though no point comes to drop the settle (a
/// shell or a monitor that never ends). Before, the break's brake read whether a settle was set,
/// not whether it was still ahead, and held every break after a wake. NEGATIVE CONTROL: inside the
/// settle the carry is held (`held:wake-settle`).
#[test]
fn a_wakes_settle_holds_the_carry_until_it_runs_out() {
    let cfg = under(Autopilot::Cruise);
    let mut typed = Vec::new();
    // The first carry at 0 s, taken at 10 s, its answer short: the next waits to 120 s. The Mac
    // slept 8 h before the read at 130 s; 140 s is inside the settle (30 s, and less than 60 s of
    // spread), 400 s past it.
    let ran = ran(
        "carry-wake-settle",
        Autopilot::Cruise,
        PLAIN,
        &[0, 10, 130, 140, 400],
        &|_| {},
        &mut |s, i| {
            typed.push(s.ctl.seen.iter().filter(|(r, _)| is_carry(r)).count());
            match i {
                1 => s.took(7, Some(&cfg), &mut Heard::default()),
                2 => {
                    let (mono, wall_ms) = s.clock_marks.expect("the last read's marks");
                    s.clock_marks = Some((mono, wall_ms - 8 * 3_600_000));
                }
                _ => {}
            }
        },
    );
    typed.push(carries(&ran));
    let at: Vec<usize> = typed.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(at, [1, 0, 0, 0, 1], "{:#?}", ran.noted);
    assert!(
        ran.noted.iter().any(|l| l.starts_with("DEFERRED ")
            && l.contains(RULE_BACKGROUND)
            && l.ends_with("held:wake-settle")),
        "{:#?}",
        ran.noted
    );
}

/// A LIMIT EPISODE PAST ITS RESET HOLDS NO CARRY (2026-10-29): an episode whose reset and its
/// grace (`LIMIT_GRACE_S`) have passed holds no line at a break, as the host's step there reads it
/// (`host_steps_in_background`), and one the agent's live turn worked past inside the grace (the
/// policy's resume taken) closes at the break that turn ends at. Before, the break's brake held
/// every carry while any episode stood, and an episode only a point closes stood for good at
/// breaks (the resume's turn ending on a shell). NEGATIVE CONTROL: inside the grace an episode no
/// live turn worked past holds the carry.
#[test]
fn a_limit_episode_past_its_reset_holds_no_carry() {
    let grace = i64::try_from(crate::harness::upgrade_drive::LIMIT_GRACE_S).expect("the grace");
    let run = |ago_s: i64, worked: bool| {
        ran(
            &format!("carry-limit-{ago_s}-{worked}"),
            Autopilot::Cruise,
            PLAIN,
            &[0, 30],
            &|_| {},
            &mut |s, i| {
                if i == 0 {
                    s.limit = Some(Episode {
                        seq: 1,
                        resumes_itself: false,
                        reset_unix: Some(T0 / 1000 - ago_s),
                        reset_text: "3pm".to_string(),
                        extended: false,
                        raised: false,
                        worked: false,
                        message: String::new(),
                    });
                    if worked {
                        s.limit_on_busy(5, true, &mut Heard::default())
                            .expect("the resumed turn's live read");
                    }
                }
            },
        )
    };
    let spent = run(grace + 60, false);
    assert_eq!(carries(&spent), 1, "{:#?}", spent.noted);
    let resumed = run(300, true);
    assert_eq!(carries(&resumed), 1, "{:#?}", resumed.noted);
    assert!(
        resumed.noted.iter().any(|l| l.starts_with("CLEARED ")
            && l.ends_with("the worker worked past the limit to a break of its background work")),
        "{:#?}",
        resumed.noted
    );
    let within = run(grace - 60, false);
    assert_eq!(carries(&within), 0, "{:#?}", within.requests());
    assert!(
        within.noted.iter().any(|l| l.starts_with("DEFERRED ")
            && l.contains(RULE_BACKGROUND)
            && l.ends_with("held:limit-episode")),
        "{:#?}",
        within.noted
    );
}

/// A COMPACTION'S OWED CONTINUATION IS TYPED AT THE BREAK (2026-10-29): a context wall compacted
/// while the agent's background work runs owes its continuation, and the compaction ends at a
/// break of that work, where no point comes. The break types the continuation in the carry's
/// place, under its own rule, recorded as the policy's act, and once the agent takes it the carry
/// follows on the ladder. Where the host wants the break, its step (an upgrade's notice) waits
/// behind the continuation, to the next break. Before, the owed continuation read as an act
/// untaken and held every line at every break. The continuation also goes ahead of a go-ahead
/// over the break's park and of a spent park's change of approach (`BreakPark::answers`,
/// `BreakPark::verdict`): the park's line stays up, as no go-ahead answers it at that break, no
/// refusal is recorded there, and a park still spent at a later break gets the change of approach
/// and the refusal (`recovering:go-ahead-refused`). With answers off the continuation over a park
/// leaves out its sentence that the go-ahead is no need, as the carry does there. NEGATIVE
/// CONTROLS: a `/compact` the agent has not taken yet holds the break (`held:act-untaken`), and so
/// do a wake's settle and a credits hold with the continuation owed; the run that stops at the
/// continuation records no refusal; with answers off a break that parks nothing keeps the whole
/// continuation. (These combined rows were first written in fab2c44f2f, ca72930fc1 and
/// 7ab54b4e39, and lost in the merge ff6b099179.)
#[test]
fn a_compactions_owed_continuation_is_typed_at_the_break() {
    use crate::supervise::policy::go_ahead::{NEVER_A_NEED, unless_held};
    use crate::supervise::policy::turn_end::{
        GO_AHEAD_ACTS, RULE_COMPACT, Then, continuation_text,
    };
    /// What else the break holds.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Also {
        Nothing,
        /// The host has its own step for the break.
        Notice,
        /// The Mac woke from 8 h of sleep just before the first read.
        Wake,
        /// The agent is spending usage credits, which the policy keeps off.
        Credits,
    }
    /// One run: the agent took the `/compact` before the first read, the host wants the break,
    /// what else holds it, the reads, each read's report ([`PLAIN`] past the last given), the
    /// go-ahead spent on [`PARKED`]'s park before the first read, and answers on.
    #[derive(Debug, Clone, Copy)]
    struct Run {
        compacted: bool,
        wants: bool,
        also: Also,
        at_s: &'static [i64],
        said: &'static [&'static [&'static str]],
        spent: bool,
        answers: bool,
    }
    // The shipped continuation, which says the go-ahead is no need (this harness's own is short).
    let cruise = |answers: bool| SupervisorConfig {
        answer_questions: answers,
        continue_text: crate::supervise::config::CONTINUE_TEXT.to_string(),
        ..under(Autopilot::Cruise)
    };
    let continuation = continuation_text(&cruise(true), None);
    assert!(continuation.contains(NEVER_A_NEED), "{continuation}");
    let owed = format!(" -- {continuation}");
    let run = |r: Run| {
        let cfg = cruise(r.answers);
        let host = Arc::new(DoorHost::default());
        host.wants.store(r.wants, Ordering::SeqCst);
        if r.also == Also::Notice {
            *host.notice.lock().expect("notice") = Some("the upgrade's notice".to_string());
        }
        let drawn = move |i: usize| {
            let mut rows = waiting(r.said.get(i).copied().unwrap_or(PLAIN));
            if r.also == Also::Credits {
                let mode = rows.last_mut().expect("the mode row");
                *mode =
                    aterm_phase::prompt::fixtures::right_aligned(mode, "Now using usage credits");
            }
            rows
        };
        Script {
            policy: cfg.clone(),
            drawn: &drawn,
            host,
            // The compaction ran before 0 s; the agent takes the continuation at 10 s and its turn
            // ends on the work again; the carry is due 2 minutes after the continuation.
            ..Script::new("carry-owed", Occupancy::Observe, r.at_s)
        }
        .run(&mut |s, i| {
            if i == 0 {
                let reading = s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
                let compact = TurnEndAction::TypeCommand {
                    command: "/compact".to_string(),
                    rule_id: RULE_COMPACT,
                    then: Then::Continue,
                };
                s.turn_end.acted(&compact, &reading, s.fx.mono());
                if r.spent {
                    let park = crate::supervise::policy::go_ahead::parked("", &PARKED.join("\n"))
                        .expect("the existing positive go-ahead fixture");
                    for _ in 0..GO_AHEAD_ACTS {
                        s.turn_end.go_ahead_typed(&park, s.fx.mono());
                    }
                    assert!(s.turn_end.go_ahead_spent(s.fx.mono()));
                }
                if r.also == Also::Wake {
                    s.clock_marks = Some((s.fx.mono(), T0 - 8 * 3_600_000));
                }
            }
            if r.compacted && i < 2 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        })
    };
    let sends = |ran: &Ran| -> Vec<String> {
        ran.requests()
            .into_iter()
            .filter(|r| r.starts_with("send "))
            .collect()
    };
    let refused = |ran: &Ran| {
        ran.noted
            .iter()
            .any(|l| l.contains("recovering:go-ahead-refused"))
    };
    let base = Run {
        compacted: true,
        wants: false,
        also: Also::Nothing,
        at_s: &[0, 10, 200],
        said: &[PLAIN],
        spent: false,
        answers: true,
    };
    // The host's own step at the break goes at the next one, once the continuation is in.
    let ran = run(Run {
        wants: true,
        also: Also::Notice,
        ..base
    });
    let sent = sends(&ran);
    assert!(
        sent.first().is_some_and(|s| s.ends_with(&owed)),
        "{sent:#?}\n{:#?}",
        ran.noted
    );
    let informed = ran
        .calls
        .iter()
        .position(|c| *c == format!("inform {continuation}"));
    let stepped = ran.calls.iter().position(|c| c == "at_background");
    assert!(
        informed.zip(stepped).is_some_and(|(i, s)| i < s),
        "{:?}",
        ran.calls
    );
    assert!(
        ran.noted
            .iter()
            .any(|l| l.contains(" background the upgrade's notice")),
        "{:#?}",
        ran.noted
    );
    for (wants, said, spent) in [
        (false, &[PLAIN], false),
        (true, &[PLAIN], false),
        (false, &[PARKED], false),
        (true, &[PARKED], false),
        (false, &[PARKED], true),
        (true, &[PARKED], true),
    ] {
        let row = Run {
            wants,
            said,
            spent,
            ..base
        };
        let ran = run(row);
        let sent = sends(&ran);
        assert!(
            sent.len() == 2 && sent[0].ends_with(&owed) && is_carry(&sent[1]),
            "{row:?}: the continuation, then the carry: {sent:#?}\n{:#?}",
            ran.noted
        );
        let typed_compact = ran
            .ledger
            .lines()
            .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
            .filter(|v| {
                let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
                text("rule_id") == Some(RULE_COMPACT) && text("decision") == Some("typed")
            })
            .count();
        assert_eq!(typed_compact, 1, "{row:?}: {}", ran.ledger);
        assert!(
            !ran.calls.iter().any(|c| c == "at_background"),
            "{row:?}: {:?}",
            ran.calls
        );
        // No go-ahead answers the park at the continuation's break, so its line goes up there;
        // the continuation records no refusal, spent park or not.
        if said[0] == PARKED {
            let up = raised(&ran);
            assert!(
                up.iter().any(|u| u.contains("rerun the checks")),
                "{row:?}: {up:#?}"
            );
        }
        assert!(!refused(&ran), "{row:?}: {:#?}", ran.noted);
        let untaken = run(Run {
            compacted: false,
            ..row
        });
        assert!(
            untaken.writes().is_empty(),
            "{row:?}: {:#?}",
            untaken.writes()
        );
        assert!(
            untaken.noted.iter().any(|l| l.contains("act-untaken")),
            "{row:?}: {:#?}",
            untaken.noted
        );
        for (also, held) in [
            (Also::Wake, "held:wake-settle"),
            (Also::Credits, "held:credits"),
        ] {
            let ran = run(Run {
                also,
                at_s: &[0, 10],
                ..row
            });
            assert!(
                ran.writes().is_empty(),
                "{row:?} {also:?}: {:#?}",
                ran.writes()
            );
            assert!(
                ran.noted
                    .iter()
                    .any(|l| l.starts_with("DEFERRED ") && l.ends_with(held)),
                "{row:?} {also:?}: {:#?}",
                ran.noted
            );
        }
    }
    // A park still spent after the continuation: the next due break changes approach and records
    // the refusal. NEGATIVE CONTROL: stopped at the continuation's break, nothing is refused.
    let spent = Run {
        said: &[PARKED, PARKED, PARKED],
        spent: true,
        ..base
    };
    let ran = run(spent);
    let sent = sends(&ran);
    assert!(
        sent.len() == 2 && sent[0].ends_with(&owed) && sent[1].contains("different safe approach"),
        "{sent:#?}\n{:#?}",
        ran.noted
    );
    assert!(refused(&ran), "{:#?}", ran.noted);
    let first = run(Run {
        at_s: &[0],
        ..spent
    });
    assert!(
        sends(&first).len() == 1 && !refused(&first),
        "NEGATIVE CONTROL: {:#?}\n{:#?}",
        first.requests(),
        first.noted
    );
    // Answers off: the continuation over the park leaves its go-ahead sentence out, and the carry
    // follows. NEGATIVE CONTROL: a break that parks nothing keeps the whole continuation.
    let cut = format!(" -- {}", unless_held(&continuation, true));
    for (said, want) in [(&[PARKED], &cut), (&[PLAIN], &owed)] {
        let ran = run(Run {
            said,
            answers: false,
            ..base
        });
        let sent = sends(&ran);
        assert!(
            sent.len() == 2 && sent[0].ends_with(want.as_str()) && is_carry(&sent[1]),
            "answers off, {said:?}: {sent:#?}\n{:#?}",
            ran.noted
        );
    }
}

/// A restart's case: its tag, its reads, the carries typed at each and the first wait it noted.
type Restarted<'a> = (&'a str, &'a [(i64, Read)], &'a [usize], &'a str);

/// A TURN RUNNING AT A RESTART BEGAN WHERE NO READ SAW: a loop that starts while the agent's turn
/// runs, or in a pause inside it (a break short of its settle), reads only that turn's remainder.
/// Its work before the restart is unknown, and so is a message another driver sent it then (no
/// prompt row tells one on Muse Code or agy), so the settled break that ends it starts the seeded
/// books again and goes at once, as a turn whose work went unread does, never on the ladder its
/// short remainder keeps, and the agent's 20-second answer to that carry waits the ladder's first
/// rung. The agent here worked from the last carry to 20 s after the restart. But a wake Claude
/// Code draws after the restart's first read found a break opened a turn of its own there, its
/// start read: a restart on a break that has stood for minutes looks, until it settles, as one in
/// a pause, and the program's own short wake 5 s in keeps the ladder, as the live loop's would.
/// A wake drawn at the first read may have opened its turn before the restart, and starts them.
/// NEGATIVE CONTROLS: a restart on a break that settles before the program's own short wake opens
/// a turn keeps the ladder; the wake drawn at the first read and the turn resumed after a pause
/// start it again. Run: with no unread mark both restarts inside the turn keep the ladder, with
/// every turn after a seed unread the settled control's wake starts it again, and with the wake's
/// row read at any first read of a turn the first read's wake keeps it.
#[test]
fn a_turn_running_at_a_restart_starts_the_books_again() {
    let kept = carried_before(&[67, 65, 61, 53, 37, 5], &[]);
    let cases: [Restarted<'_>; 5] = [
        (
            "live",
            &[
                (0, Read::Live),
                (20, Read::Break),
                (40, Read::Break),
                (50, Read::Live),
                (60, Read::Break),
                (80, Read::Break),
            ],
            &[0, 0, 1, 0, 0, 0],
            "1 short answer(s) in a row",
        ),
        (
            "paused",
            &[
                (0, Read::Break),
                (5, Read::Live),
                (25, Read::Break),
                (45, Read::Break),
                (55, Read::Live),
                (65, Read::Break),
                (85, Read::Break),
            ],
            &[0, 0, 0, 1, 0, 0, 0],
            "1 short answer(s) in a row",
        ),
        (
            "settled",
            &[
                (0, Read::Break),
                (20, Read::Break),
                (120, Read::Woke),
                (140, Read::Break),
                (160, Read::Break),
            ],
            &[0, 0, 0, 0, 0],
            "6 short answer(s) in a row",
        ),
        (
            "woke",
            &[
                (0, Read::Break),
                (5, Read::Woke),
                (25, Read::Break),
                (45, Read::Break),
            ],
            &[0, 0, 0, 0],
            "6 short answer(s) in a row",
        ),
        (
            "woke-first",
            &[
                (0, Read::Woke),
                (20, Read::Break),
                (40, Read::Break),
                (50, Read::Live),
                (60, Read::Break),
                (80, Read::Break),
            ],
            &[0, 0, 1, 0, 0, 0],
            "1 short answer(s) in a row",
        ),
    ];
    let wrong: Vec<_> = cases
        .iter()
        .filter_map(|(tag, reads, carried, wait)| {
            let (at, noted) = carried_after(
                &format!("carry-restart-running-{tag}"),
                Duration::from_secs(20),
                &stepped(reads),
                Some(&kept),
            );
            let first = waits(&noted).first().cloned();
            (at != *carried || first.as_deref() != Some(*wait)).then_some((*tag, at, first, noted))
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// THE BOOKS STARTED AGAIN ARE IN THE LEDGER: books a loop starts again in memory after its last
/// carry, no carry typed since, end a later loop's replay there, so a restart carries at once, as
/// the live ladder would have, never on the ladder before. The agent answers a third carry with
/// five minutes of real work, the host types its own line (an upgrade's notice) in the carry's
/// place at the break it ends on, and the loop restarts a minute later. And a restart whose
/// seeded books a person's gesture after the last carry started again, its carry held by their
/// grace, leaves them started for the loop after it, whose host no longer reports that gesture
/// (an app's relaunch). NEGATIVE CONTROLS, each keeping the ladder: a short answer to the third
/// carry, the host's line at a break after it, and a gesture before the last carry. Run: with the
/// walk not stopped at the row both restarts keep the old ladder, a row at every judged break
/// carries the short answer's at once, and no row for the gesture keeps its ladder.
#[test]
fn a_restart_after_the_books_started_again_carries_at_once() {
    let restart = stepped(&[(0, Read::Break), (30, Read::Break)]);
    let replayed = |tag: &str, ledger: String| {
        let (at, noted) = carried_after(tag, Duration::ZERO, &restart, Some(&ledger_of(ledger)));
        let first = waits(&noted).first().cloned();
        (at, first, noted)
    };
    let mut wrong = Vec::new();
    // Carries at 0, 2 and 6 minutes, the first two answered in 20 s; the third's answer runs
    // from 6:10 to the break the host's line goes at, at 11 minutes, or to one at 6:30.
    for (tag, answer, wait) in [
        ("worked", &[][..], "1 short answer(s) in a row"),
        (
            "short",
            &[(390, Read::Break)][..],
            "3 short answer(s) in a row",
        ),
    ] {
        let mut before = vec![
            (0, Read::Break),
            (20, Read::Taken),
            (120, Read::Break),
            (140, Read::Taken),
            (360, Read::Break),
            (370, Read::Live),
        ];
        before.extend_from_slice(answer);
        before.push((660, Read::Hosted));
        let (typed, typed_noted, ledger) = carried_into(
            &format!("carry-again-{tag}-before"),
            Duration::ZERO,
            &stepped(&before),
            None,
            Some(T0 - 12 * MIN),
        );
        let mut three = vec![1, 0, 1, 0, 1, 0, 0];
        three.extend(answer.iter().map(|_| 0));
        assert_eq!(
            typed, three,
            "PRECONDITION: three carries, and the host's line in the fourth's place: \
             {typed_noted:#?}"
        );
        let (at, first, noted) = replayed(&format!("carry-again-{tag}"), ledger);
        let carried = if tag == "worked" { [1, 0] } else { [0, 0] };
        if at != carried || first.as_deref() != Some(wait) {
            wrong.push((tag, at, first, noted));
        }
    }
    // Six carries on the ladder, the last 15 minutes before the third loop's start. The second
    // starts 5 minutes after it, and the person's gesture a minute before that start holds its
    // carry at both its reads; the control's came before the last carry.
    let ladder = carried_before(&[77, 75, 71, 63, 47, 15], &[]);
    for (tag, touched, carried, wait) in [
        ("touched", -60, [1, 0], "1 short answer(s) in a row"),
        ("untouched", -400, [0, 0], "6 short answer(s) in a row"),
    ] {
        let earlier = Earlier {
            touched: Some(touched),
            ..ladder.clone()
        };
        let (held, held_noted, ledger) = carried_into(
            &format!("carry-again-{tag}-before"),
            Duration::ZERO,
            &stepped(&[(0, Read::Break), (20, Read::Break)]),
            Some(&earlier),
            Some(T0 - 10 * MIN),
        );
        assert_eq!(held, [0, 0], "PRECONDITION: {held_noted:#?}");
        let (at, first, noted) = replayed(&format!("carry-again-{tag}"), ledger);
        if at != carried || first.as_deref() != Some(wait) {
            wrong.push((tag, at, first, noted));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// SOMEONE ELSE'S MESSAGE AND A TURN WHOSE WORK WENT UNREAD ARE IN THE LEDGER TOO: each starts a
/// loop's books again after its last carry, the host's own line goes in the carry's place at the
/// break it ends on, and the loop restarts a minute later, which carries at once, as the live
/// ladder would have, never on the ladder before. Someone else's message is Claude Code's Remote
/// Control in the third carry's answer; the unread turn is the program's own wake under Off after
/// the second's answer. NEGATIVE CONTROLS, each keeping the ladder: the third carry answered
/// short by the agent itself, and the wake read on Cruise, short. Run: with the books started
/// again there in memory alone, both restarts keep the ladder.
#[test]
fn a_restart_after_someone_elses_message_or_an_unread_turn_carries_at_once() {
    let restart = stepped(&[(0, Read::Break), (30, Read::Break)]);
    let mut wrong = Vec::new();
    // Carries at 0, 2 and 6 minutes, the first two answered in 20 s; the third's answer runs
    // from 6:10 to the host's line at 6:30. The loop restarts at 7:30.
    for (tag, answer, carried, wait) in [
        ("remote", Read::Remote, [1, 0], "1 short answer(s) in a row"),
        ("own", Read::Live, [0, 0], "3 short answer(s) in a row"),
    ] {
        let before = stepped(&[
            (0, Read::Break),
            (20, Read::Taken),
            (120, Read::Break),
            (140, Read::Taken),
            (360, Read::Break),
            (370, answer),
            (390, Read::Hosted),
        ]);
        let (typed, typed_noted, ledger) = carried_into(
            &format!("carry-again-{tag}-before"),
            Duration::ZERO,
            &before,
            None,
            Some(T0 - 450_000),
        );
        assert_eq!(
            typed,
            [1, 0, 1, 0, 1, 0, 0],
            "PRECONDITION: three carries, and the host's line in the fourth's place: \
             {typed_noted:#?}"
        );
        let (at, noted) = carried_after(
            &format!("carry-again-{tag}"),
            Duration::ZERO,
            &restart,
            Some(&ledger_of(ledger)),
        );
        let first = waits(&noted).first().cloned();
        if at != carried || first.as_deref() != Some(wait) {
            wrong.push((tag, at, first, noted));
        }
    }
    // Carries at 0 and 2 minutes, the second answered in 20 s; the program's own wake runs from
    // 2:30 to its break at 3:00, under Off or read on Cruise, and the host's line goes at 3:20.
    // The loop restarts at 4:20.
    for (tag, step, carried, wait) in [
        ("unread", Step::Off, [1, 0], "1 short answer(s) in a row"),
        ("read", Step::Stepped, [0, 0], "2 short answer(s) in a row"),
    ] {
        let mut before = stepped(&[
            (0, Read::Break),
            (20, Read::Taken),
            (120, Read::Break),
            (140, Read::Taken),
            (150, Read::Woke),
            (170, Read::Live),
            (180, Read::Break),
            (200, Read::Hosted),
        ]);
        for read in &mut before[4..7] {
            read.2 = step;
        }
        let (typed, typed_noted, ledger) = carried_into(
            &format!("carry-again-{tag}-before"),
            Duration::ZERO,
            &before,
            None,
            Some(T0 - 260_000),
        );
        assert_eq!(
            typed,
            [1, 0, 1, 0, 0, 0, 0, 0],
            "PRECONDITION: two carries, and the host's line in the third's place: \
             {typed_noted:#?}"
        );
        let (at, noted) = carried_after(
            &format!("carry-again-{tag}"),
            Duration::ZERO,
            &restart,
            Some(&ledger_of(ledger)),
        );
        let first = waits(&noted).first().cloned();
        if at != carried || first.as_deref() != Some(wait) {
            wrong.push((tag, at, first, noted));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A TURN READ RUNNING AFTER A LOOP'S FIRST CARRY ENDS NO LADDER: a loop that no loop ran before
/// writes a turn-ran row at its first live read, which, where it began at a break, is the first
/// carry's answer. That row starts no books (a judged point does, and its own row ends the
/// books' run), so a loop that restarts within the carry's rung takes the ladder up, never
/// carrying at once. NEGATIVE CONTROL: a point before the turn-ran row still ends the run
/// (`the_books_walk_takes_each_rung_its_row_names`).
#[test]
fn a_turn_read_running_after_a_loops_first_carry_ends_no_ladder() {
    // The carry at a minute before the restart, answered in 20 s.
    let (typed, typed_noted, ledger) = carried_into(
        "carry-ran-before",
        Duration::ZERO,
        &stepped(&[(0, Read::Break), (20, Read::Taken), (40, Read::Break)]),
        None,
        Some(T0 - MIN),
    );
    assert_eq!(typed, [1, 0, 0], "PRECONDITION: {typed_noted:#?}");
    let rules: Vec<&str> = ledger
        .lines()
        .filter_map(|line| line.split("\"rule_id\":\"").nth(1)?.split('"').next())
        .filter(|rule| !approvals::is_fact_rule(rule))
        .collect();
    assert_eq!(
        rules,
        [RULE_BACKGROUND, approvals::RULE_TURN_RAN],
        "PRECONDITION: the turn-ran row follows the carry's: {ledger}"
    );
    let (at, noted) = carried_after(
        "carry-ran",
        Duration::ZERO,
        &stepped(&[(0, Read::Break), (30, Read::Break), (90, Read::Break)]),
        Some(&ledger_of(ledger)),
    );
    assert_eq!(at, [0, 0, 1], "{noted:#?}");
    assert_eq!(
        waits(&noted).first().map(String::as_str),
        Some("1 short answer(s) in a row"),
        "{noted:#?}"
    );
}

/// THE BOOKS' RUN TAKES EACH RUNG ITS ROW NAMES: a carry's row, and a go-ahead's typed in the
/// carry's place at a break, name the streak each went at, and a turn-ran row after them starts no
/// books. NEGATIVE CONTROLS, each ending the run: a go-ahead typed at a turn end, which names no
/// streak; a point judged, before its turn-ran row or alone; the books started again.
#[test]
fn the_books_walk_takes_each_rung_its_row_names() {
    use crate::supervise::policy::cruise_carry::RULE_BACKGROUND_RESET;
    use crate::supervise::policy::go_ahead::RULE_GO_AHEAD;
    use approvals::Outcome::{Typed, Waiting};
    let named = |min_ago: i64, rule: &str, streak: u32| {
        row_before(
            min_ago,
            rule,
            Typed,
            &approvals::background_carry_reason("at a break (autopilot cruise)", streak),
        )
    };
    let walk = |rows: &[String]| -> Vec<(i64, Option<u32>)> {
        approvals::background_carries_in(&books_of(rows).ledger, Some("@s-1"))
            .into_iter()
            .map(|(ts, streak)| ((T0 - ts) / MIN, streak))
            .collect()
    };
    let ran = row_before(4, approvals::RULE_TURN_RAN, Waiting, approvals::TURN_RAN);
    let point = approvals::point_reason(approvals::Carried::streak(0), false);
    assert_eq!(
        walk(&[
            carry_before(9),
            named(8, RULE_BACKGROUND, 2),
            named(6, RULE_GO_AHEAD, 3),
            ran.clone(),
        ]),
        [(9, None), (8, Some(2)), (6, Some(3))]
    );
    let wrong: Vec<_> = [
        (
            "go-ahead at a turn end",
            row_before(
                5,
                RULE_GO_AHEAD,
                Typed,
                "the turn-end policy judged the point",
            ),
        ),
        (
            "point",
            row_before(5, approvals::RULE_POINT, Waiting, &point),
        ),
        (
            "started again",
            row_before(5, RULE_BACKGROUND_RESET, Waiting, "real work"),
        ),
    ]
    .into_iter()
    .filter_map(|(tag, row)| {
        let rows = walk(&[named(6, RULE_BACKGROUND, 1), row, ran.clone()]);
        (!rows.is_empty()).then_some((tag, rows))
    })
    .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A GO-AHEAD AT A BREAK IS A RUNG OF THE CARRY'S LADDER: the owner's standing go-ahead goes in
/// the carry's place on the carry's ladder, so its row names the streak it went at as a carry's
/// does, and a loop that restarts after it takes the ladder up, never carrying at once. Two
/// go-aheads 2 minutes apart, each answered in 10 s; the loop restarts a minute after the second,
/// whose next is due 4 minutes from it. NEGATIVE CONTROL: a go-ahead typed at a turn end names no
/// streak and ends the run (`the_books_walk_takes_each_rung_its_row_names`).
#[test]
fn a_restart_takes_the_ladder_up_from_a_go_ahead_at_a_break() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    let cfg = SupervisorConfig {
        answer_questions: true,
        ..under(Autopilot::Cruise)
    };
    let drawn = |_| waiting(PARKED);
    let parked = |tag: &str, at_s: &[i64], answered: &[usize], earlier: (String, i64)| {
        let books = crate::supervise::test_scratch_path("carry-books", tag).join("s-1.jsonl");
        let (ledger, start) = earlier.clone();
        Script {
            policy: cfg.clone(),
            drawn: &drawn,
            seed: Some(earlier),
            virtual_time: true,
            ..Script::new(tag, Occupancy::Observe, at_s)
        }
        .run(&mut |s, i| {
            if i == 0 && !ledger.is_empty() {
                s.seed_from_ledger_text(&books, &ledger, start, false);
            }
            if answered.contains(&i) {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        })
    };
    let before = parked(
        "carry-go-ahead-restart-before",
        &[0, 10, 120, 130],
        &[1, 3],
        (String::new(), T0 - 3 * MIN),
    );
    let went = sent(&before);
    assert!(
        went.len() == 2 && went.iter().all(|s| s.starts_with(GO_AHEAD_ANSWER)),
        "PRECONDITION: two go-aheads at the breaks: {went:#?} {:#?}",
        before.noted
    );
    let restarted = parked(
        "carry-go-ahead-restart",
        &[0, 30],
        &[],
        (before.ledger.clone(), T0),
    );
    assert_eq!(
        sent(&restarted),
        Vec::<String>::new(),
        "{:#?}",
        restarted.noted
    );
    assert_eq!(
        waits(&restarted.noted).first().map(String::as_str),
        Some("2 short answer(s) in a row"),
        "{:#?}",
        restarted.noted
    );
}

/// WHAT GOES IN THE CARRY'S PLACE NAMES ITS STREAK: a compaction's continuation owed at a break,
/// under its own rule, and the change of approach once a park's routine go-aheads are spent go
/// on the carry's ladder as the go-ahead in its place does, so each one's row names the streak it
/// went at, and the books' walk takes every one as a rung, never ending at the continuation's
/// other rule. The continuation and the carry after it, each answered in 10 s, are rungs one and
/// two; the go-aheads and the changes of approach after them climb a rung each. NEGATIVE CONTROL:
/// a row typed at a turn end names none and ends the walk
/// (`the_books_walk_takes_each_rung_its_row_names`).
#[test]
fn what_goes_in_the_carrys_place_names_its_streak() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    use crate::supervise::policy::turn_end::{RULE_COMPACT, Then};
    let walk = |ran: &Ran| -> Vec<Option<u32>> {
        approvals::background_carries_in(&ran.ledger, Some("@s-1"))
            .into_iter()
            .map(|(_, streak)| streak)
            .collect()
    };
    let cfg = under(Autopilot::Cruise);
    let plain = |_| waiting(PLAIN);
    let owed = Script {
        policy: cfg.clone(),
        drawn: &plain,
        ..Script::new("carry-owed-streak", Occupancy::Observe, &[0, 10, 200])
    }
    .run(&mut |s, i| {
        if i == 0 {
            let reading = s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
            let compact = TurnEndAction::TypeCommand {
                command: "/compact".to_string(),
                rule_id: RULE_COMPACT,
                then: Then::Continue,
            };
            s.turn_end.acted(&compact, &reading, s.fx.mono());
        }
        if i < 2 {
            s.took(7, Some(&cfg), &mut Heard::default());
        }
    });
    let went = sent(&owed);
    assert!(
        went.len() == 2
            && !is_carry(&format!("send -- {}", went[0]))
            && is_carry(&format!("send -- {}", went[1])),
        "PRECONDITION: the continuation, then the carry: {went:#?}\n{:#?}",
        owed.noted
    );
    assert_eq!(walk(&owed), [Some(1), Some(2)], "{}", owed.ledger);
    let cfg = SupervisorConfig {
        answer_questions: true,
        ..under(Autopilot::Cruise)
    };
    let parked = |_| waiting(PARKED);
    let recovered = Script {
        policy: cfg.clone(),
        drawn: &parked,
        // Each line is answered ten seconds on, the turn ending on the work again.
        ..Script::new(
            "carry-recovery-streak",
            Occupancy::Observe,
            &[30, 40, 160, 170, 410, 420, 890, 900, 1850, 1860, 3770],
        )
    }
    .run(&mut |s, i| {
        if i % 2 == 1 {
            s.took(7, Some(&cfg), &mut Heard::default());
        }
    });
    let went = sent(&recovered);
    assert!(
        went.iter().any(|s| s.starts_with(GO_AHEAD_ANSWER))
            && went
                .last()
                .is_some_and(|s| s.contains("different safe approach")),
        "PRECONDITION: go-aheads, then a change of approach: {went:#?}\n{:#?}",
        recovered.noted
    );
    let climbing: Vec<Option<u32>> = (1..=u32::try_from(went.len()).expect("small"))
        .map(Some)
        .collect();
    assert_eq!(walk(&recovered), climbing, "{}", recovered.ledger);
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
/// ahead!"): the owner's standing go-ahead goes in the carry's place, on its ladder, and the line
/// it answers never goes up (§15.6); with answers off the carry goes as before, and the line goes
/// up.
#[test]
fn a_break_that_parks_on_a_go_ahead_gets_the_owners_standing_go_ahead() {
    use crate::supervise::policy::go_ahead::{
        GO_AHEAD_ANSWER, RULE_GO_AHEAD, TOKEN_CLOSE, TOKEN_OPEN,
    };
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
            && sent[0].contains(&format!("{TOKEN_OPEN}rerun the checks{TOKEN_CLOSE}")),
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
    assert!(raised(&cruise).is_empty(), "{:#?}", cruise.requests());
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
    let up = raised(&off);
    assert!(
        up.len() == 1 && up[0].contains("rerun the checks"),
        "NEGATIVE CONTROL: answers off: {up:#?}"
    );
}

/// A WRAPPED PARK AT A BREAK KEEPS ITS LINE OFF (§15.6): the break's turn parks on a need Claude
/// Code wrapped in its 118 cells before a capital the go-ahead reader's own view does not join
/// (`… with the small fix` above `Claude Code needs.`), in its report or in an earlier message;
/// the break's needs fold reads the reader's measured lines, and the go-ahead's park reads the
/// same words, so the go-ahead goes in the carry's place and the line it answers never goes up.
/// CONTROL: the need on one row, a pane wide enough.
#[test]
fn a_wrapped_park_at_a_break_gets_the_go_ahead_and_never_goes_up() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    const NEED: &str = "NEEDS YOU: say \"rerun the checks\" so I can build and test my parser \
                        branch and push it to main with the small fix Claude Code needs.";
    const LAST: &str = "The suite runs in the background; the docs are next.";
    let wrapped = aterm_phase::prompt::fixtures::ink_wrap(NEED, 118);
    assert!(
        wrapped.len() == 2 && wrapped[1].starts_with("Claude"),
        "the pane wraps before a capital: {wrapped:#?}"
    );
    let report = |need: &[String]| {
        let mut said = vec!["Both workflows are running.".to_string()];
        said.extend(need.iter().cloned());
        waiting(&said.iter().map(String::as_str).collect::<Vec<_>>())
    };
    let earlier = |need: &[String]| waiting_turn(None, Some(&need.join("\n")), LAST);
    let one_row = [NEED.to_string()];
    for (tag, screen) in [
        ("carry-park-one-row", report(&one_row)),
        ("carry-park-earlier-one-row", earlier(&one_row)),
        ("carry-park-earlier-wrapped", earlier(&wrapped)),
        ("carry-park-wrapped", report(&wrapped)),
    ] {
        let drawn = move |_| screen.clone();
        let cruise = Script {
            policy: SupervisorConfig {
                answer_questions: true,
                ..under(Autopilot::Cruise)
            },
            drawn: &drawn,
            set: &|_| {},
            ..Script::new(tag, Occupancy::Observe, &[0, 30])
        }
        .run(&mut nothing);
        let sent = sent(&cruise);
        assert!(
            sent.len() == 1 && sent[0].starts_with(GO_AHEAD_ANSWER),
            "{tag}: {sent:#?}"
        );
        assert!(
            raised(&cruise).is_empty(),
            "{tag}: the go-ahead answers the line, yet it went up: {:#?}",
            cruise.requests()
        );
    }
}

/// A PERSON'S TURN THAT ENDS AT A BREAK HOLDS THE HOUR, AS AT A TURN END (2026-10-29 review 4,
/// read by keys): their key opens a turn (`human_ms` times it) whose agent ends it with its
/// background work running, whatever their message said: the break gets the carry without its
/// sentence that the go-ahead is no need, never the go-ahead. The control: the same turn and
/// break with no key of theirs get the go-ahead.
#[test]
fn a_persons_turn_ending_at_a_break_holds_the_hour() {
    use crate::supervise::policy::go_ahead::{GO_AHEAD_ANSWER, unless_held};
    let prompt = "❯ Commit it, but don't push until I say so.";
    let at_break = waiting_under(&[prompt, ""], PARKED);
    let live = live_under(&[prompt], "⏺ Committing.");
    let reads = [0, 30, 200];
    let run = |tag: &str, keyed: bool| {
        let cfg = SupervisorConfig {
            answer_questions: true,
            ..under(Autopilot::Cruise)
        };
        let ran = Script {
            policy: cfg.clone(),
            set: &|_| {},
            ..Script::new(tag, Occupancy::Observe, &reads)
        }
        .run(&mut |s, i| {
            s.ctl.m.screens = vec![if i == 0 {
                live.clone()
            } else {
                at_break.clone()
            }];
            s.ctl.m.served = 0;
            // Their key five seconds before the turn's first read, as `status` and the screen
            // time it.
            if keyed {
                let ago = u64::try_from(reads[i] + 5).expect("forward");
                s.ctl.m.human_ms = Some(ago * 1_000);
                s.ctl.m.human = vec![["5000", "35000", "205000"][i]];
            }
            if i == 0 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        });
        sent(&ran)
    };
    let keyed = run("carry-person-turn", true);
    assert_eq!(keyed, [unless_held(BACKGROUND_CARRY, true)], "{keyed:#?}");
    let unkeyed = run("carry-person-none", false);
    assert!(
        unkeyed.len() == 1 && unkeyed[0].starts_with(GO_AHEAD_ANSWER),
        "{unkeyed:#?}"
    );
}

/// A TURN THE HARNESS'S OWN WORDS OPENED HOLDS NOTHING AT A BREAK EITHER (2026-10-29 review 5):
/// a turn Cruise's continuation opened, which the break's count of submissions reads as someone
/// else's (no carry ladder in the books), with no key of the person's, sets no hour, so the park
/// restated at the break gets the go-ahead at once.
#[test]
fn a_turn_the_harness_opened_ending_at_a_break_is_no_persons() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    let prompt = format!("❯ {}", crate::supervise::config::CONTINUE_TEXT);
    let restated = ["The build is waiting only for your go-ahead to rerun the checks."];
    let at_break = waiting_under(&[&prompt, ""], &restated);
    let live = live_under(&[&prompt], "⏺ Committing.");
    let cfg = SupervisorConfig {
        answer_questions: true,
        ..under(Autopilot::Cruise)
    };
    let ran = Script {
        policy: cfg.clone(),
        set: &|_| {},
        ..Script::new("carry-harness-opened", Occupancy::Observe, &[0, 30, 90])
    }
    .run(&mut |s, i| {
        s.ctl.m.screens = vec![if i == 0 {
            live.clone()
        } else {
            at_break.clone()
        }];
        s.ctl.m.served = 0;
        if i == 0 {
            s.took(7, Some(&cfg), &mut Heard::default());
        }
    });
    assert!(
        sent(&ran)
            .first()
            .is_some_and(|s| s.starts_with(GO_AHEAD_ANSWER)),
        "{:#?}",
        sent(&ran)
    );
}

/// A KEY OLDER THAN THE BREAK'S TURN HOLDS NOTHING PAST ITS OWN HOUR (2026-10-30 review 8): the
/// person types at a break of the agent's background work, after the go-ahead it got there and
/// with no act typed since; an hour later the agent's own wake opens a turn that ends at another
/// break. Their key came before that turn, so its hour runs from the key, not from the break,
/// and the break gets the go-ahead. The control is the same key read inside its hour, which
/// holds that break's go-ahead.
#[test]
fn a_key_older_than_the_breaks_turn_holds_nothing_past_its_own_hour() {
    use crate::supervise::policy::go_ahead::{GO_AHEAD_ANSWER, unless_held};
    let live = live_under(&[], "⏺ Checking the workflows.");
    let run = |tag: &str, late: i64| {
        let cfg = SupervisorConfig {
            answer_questions: true,
            ..under(Autopilot::Cruise)
        };
        let reads = [0, 30, 60, 100, late, late + 30];
        let ran = Script {
            policy: cfg.clone(),
            set: &|_| {},
            ..Script::new(tag, Occupancy::Observe, &reads)
        }
        .run(&mut |s, i| {
            s.ctl.m.screens = vec![if [1, 4].contains(&i) {
                live.clone()
            } else {
                waiting(PARKED)
            }];
            s.ctl.m.served = 0;
            // Their key at 90 s, as `status` and the screen time it from each read.
            if i >= 3 {
                let ago = u64::try_from(reads[i] - 90).expect("forward") * 1_000;
                s.ctl.m.human_seq = Some(8);
                s.ctl.m.human_ms = Some(ago);
                s.ctl.m.human = vec![Box::leak(ago.to_string().into_boxed_str())];
            }
            if [1, 4].contains(&i) {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        });
        sent(&ran)
    };
    let past = run("carry-key-past-its-hour", 3700);
    assert_eq!(past.len(), 2, "{past:#?}");
    assert!(
        past.iter().all(|s| s.starts_with(GO_AHEAD_ANSWER)),
        "{past:#?}"
    );
    let within = run("carry-key-within-its-hour", 1800);
    assert_eq!(
        within.get(1),
        Some(&unless_held(BACKGROUND_CARRY, true).into_owned()),
        "{within:#?}"
    );
}

/// A KEY OF THE PERSON'S DURING THE AGENT'S BACKGROUND WORK HOLDS THE GO-AHEAD FOR ITS HOUR (the
/// judge panel's design, 2026-10-29): after the first go-ahead at a break, the person types in
/// the tab (`human_seq` moves, `human_ms` times it) and submits nothing (`submits` stands); the
/// typing guard holds the next break's write, and once past it the break, still parked, gets the
/// carry without its sentence that the go-ahead is no need, not the go-ahead. The control: no
/// key, and the go-ahead goes at that next break.
#[test]
fn a_persons_key_during_background_work_holds_the_go_ahead() {
    use crate::supervise::policy::go_ahead::{GO_AHEAD_ANSWER, unless_held};
    let run = |tag: &str, keyed: bool| {
        let cfg = SupervisorConfig {
            answer_questions: true,
            ..under(Autopilot::Cruise)
        };
        let ran = Script {
            policy: cfg.clone(),
            drawn: &|_| waiting(PARKED),
            set: &|m| m.submits_on = (0..64).map(|i| (i, 5)).collect(),
            ..Script::new(tag, Occupancy::Observe, &[30, 40, 160, 300])
        }
        .run(&mut |s, i| {
            if i == 1 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
            // Their key at 100 s, as `status` and the screen time it: read at 160 s inside the
            // typing guard's grace, at 300 s past it.
            if keyed && i >= 2 {
                s.ctl.m.human_seq = Some(8);
                s.ctl.m.human_ms = Some(if i == 2 { 60_000 } else { 200_000 });
                s.ctl.m.human = vec![if i == 2 { "60000" } else { "200000" }];
            }
        });
        sent(&ran)
    };
    let keyed = run("carry-keyed", true);
    assert_eq!(keyed.len(), 2, "{keyed:#?}");
    assert!(keyed[0].starts_with(GO_AHEAD_ANSWER), "{keyed:#?}");
    assert_eq!(keyed[1], unless_held(BACKGROUND_CARRY, true), "{keyed:#?}");
    let unkeyed = run("carry-unkeyed", false);
    assert_eq!(unkeyed.len(), 2, "{unkeyed:#?}");
    assert!(
        unkeyed.iter().all(|s| s.starts_with(GO_AHEAD_ANSWER)),
        "{unkeyed:#?}"
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

/// A BREAK READS ITS WHOLE TURN PAST THE SCREEN'S TOP (2026-10-30 review 9): the turn's first
/// message plans to delete the release tags on the remote, then twenty tool calls and a background
/// build push it into the history, and the report parks on the go-ahead. The break reads the turn
/// whole from the history, as its needs fold and a point do, so the carry goes, not the go-ahead,
/// and the line goes up. NEGATIVE CONTROL: the same turn opening on a plain message gets the
/// go-ahead.
#[test]
fn a_break_whose_plan_scrolled_off_gets_the_carry_not_the_go_ahead() {
    use crate::supervise::policy::go_ahead::RULE_GO_AHEAD;
    let run = |tag: &str, first: &'static str| {
        let mut lines: Vec<String> = ["❯ ship 0.145.0", "", first, ""]
            .map(ToString::to_string)
            .to_vec();
        for i in 0..20 {
            lines.push(format!("⏺ Read(crates/aterm-agent/src/part_{i:02}.rs)"));
            lines.push("  ⎿  Read 40 lines".to_string());
        }
        lines.extend(
            [
                "⏺ Bash(targo --unverified test --workspace)",
                "  ⎿  Running in the background (↓ to manage)",
                "",
            ]
            .map(ToString::to_string),
        );
        let mut screen = waiting(&[
            "Build started.",
            "NEEDS YOU: say \"rerun the checks\" so I can build and test my branch and push it \
             to main.",
        ]);
        screen.splice(0..0, lines);
        let shown = screen.split_off(6);
        let history = screen;
        let drawn = move |_| shown.clone();
        let set = move |m: &mut Mock| {
            m.scrollback = Some(history.clone());
            m.alt_screen = Some(false);
        };
        Script {
            policy: SupervisorConfig {
                answer_questions: true,
                ..under(Autopilot::Cruise)
            },
            drawn: &drawn,
            set: &set,
            ..Script::new(tag, Occupancy::Observe, &[0, 30])
        }
        .run(&mut nothing)
    };
    let planned = run(
        "carry-go-ahead-planned-above",
        "⏺ Plan: delete the stale release tags on the remote, then push my branch to main.",
    );
    assert!(
        !planned
            .ledger
            .contains(&format!("\"rule_id\":\"{RULE_GO_AHEAD}\"")),
        "{}",
        planned.ledger
    );
    // The mock moves its screen's generation on the walk's own read, so the carry's first send
    // is fenced off and sent again; the ledger books the one typed.
    assert!(
        planned
            .ledger
            .contains(&format!("\"rule_id\":\"{RULE_BACKGROUND}\"")),
        "{}",
        planned.ledger
    );
    let up = raised(&planned);
    assert!(
        up.len() == 1 && up[0].contains("rerun the checks"),
        "{up:#?}"
    );
    let plain = run(
        "carry-go-ahead-plain-above",
        "⏺ Checked the parser; the suite is next.",
    );
    assert!(
        plain
            .ledger
            .contains(&format!("\"rule_id\":\"{RULE_GO_AHEAD}\"")),
        "NEGATIVE CONTROL: {}",
        plain.ledger
    );
}

/// A tall turn that ended on its background work, its first message `first` scrolled off with its
/// prompt row: the history above the screen and the screen, whose rows are the same whatever
/// `first` says (twenty tool calls, a background build and the report that parks on the
/// go-ahead).
fn tall_parked_turn(first: &str) -> (Vec<String>, Vec<String>) {
    let mut lines: Vec<String> = ["❯ ship 0.145.0", "", first, ""]
        .map(ToString::to_string)
        .to_vec();
    for i in 0..20 {
        lines.push(format!("⏺ Read(crates/aterm-agent/src/part_{i:02}.rs)"));
        lines.push("  ⎿  Read 40 lines".to_string());
    }
    lines.extend(
        [
            "⏺ Bash(targo --unverified test --workspace)",
            "  ⎿  Running in the background (↓ to manage)",
            "",
        ]
        .map(ToString::to_string),
    );
    let mut screen = waiting(&[
        "Build started.",
        "NEEDS YOU: say \"rerun the checks\" so I can build and test my branch and push it to \
         main.",
    ]);
    screen.splice(0..0, lines);
    let shown = screen.split_off(6);
    (screen, shown)
}

/// THE BREAK'S WHOLE READ IS ITS OWN TURN'S (2026-10-30 review 9, second pass): two tall turns
/// end at breaks of the agent's background work with the same rows on screen, the report it
/// repeats under tool rows; only their first messages, scrolled off, differ. The second turn's
/// break reads its own turn whole, not the first's from when the same words stood: its plan to
/// delete the release tags gets the carry, not the go-ahead, and the line goes up. The control
/// runs the other way: a plan in the first turn withholds no go-ahead from a plain second one.
#[test]
fn a_breaks_whole_read_is_its_own_turns() {
    use crate::supervise::policy::go_ahead::RULE_GO_AHEAD;
    let plan = "⏺ Plan: delete the stale release tags on the remote, then push my branch to main.";
    let plain = "⏺ Checked the parser; the suite is next.";
    let live = live_under(&[], "⏺ Checking the workflows.");
    let reads = [0, 30, 900, 1000, 5000];
    let run = |tag: &str, first: &str, second: &str| {
        let (above_a, shown_a) = tall_parked_turn(first);
        let (above_b, shown_b) = tall_parked_turn(second);
        assert_eq!(shown_a, shown_b, "PRECONDITION: the same rows on screen");
        let cfg = SupervisorConfig {
            answer_questions: true,
            ..under(Autopilot::Cruise)
        };
        Script {
            policy: cfg.clone(),
            set: &|m| m.alt_screen = Some(false),
            ..Script::new(tag, Occupancy::Observe, &reads)
        }
        .run(&mut |s, i| {
            let (above, screen) = match i {
                0 | 1 => (&above_a, &shown_a),
                2 => (&above_b, &live),
                _ => (&above_b, &shown_b),
            };
            s.ctl.m.scrollback = Some(above.clone());
            s.ctl.m.screens = vec![screen.clone()];
            s.ctl.m.served = 0;
            if i == 2 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        })
    };
    let typed = |ran: &Ran| -> Vec<String> {
        ran.ledger
            .lines()
            .filter(|l| l.contains("\"decision\":\"typed\""))
            .filter_map(|l| {
                [RULE_GO_AHEAD, RULE_BACKGROUND]
                    .into_iter()
                    .find(|rule| l.contains(&format!("\"rule_id\":\"{rule}\"")))
                    .map(ToString::to_string)
            })
            .collect()
    };
    let planned = run("carry-whole-own-turn", plain, plan);
    let rules = typed(&planned);
    assert_eq!(
        rules.first().map(String::as_str),
        Some(RULE_GO_AHEAD),
        "the first turn's go-ahead: {}",
        planned.ledger
    );
    assert!(
        rules.len() >= 2 && rules[1..].iter().all(|r| r == RULE_BACKGROUND),
        "the second turn's carry, not the go-ahead: {rules:?}\n{}",
        planned.ledger
    );
    let up = raised(&planned);
    assert!(
        up.iter().any(|r| r.contains("rerun the checks")),
        "the second turn's line goes up: {up:#?}"
    );
    let freed = run("carry-whole-own-turn-control", plan, plain);
    let rules = typed(&freed);
    assert_eq!(
        rules.first().map(String::as_str),
        Some(RULE_BACKGROUND),
        "the first turn's plan gets the carry: {}",
        freed.ledger
    );
    assert!(
        rules.iter().any(|r| r == RULE_GO_AHEAD),
        "CONTROL: the plain second turn gets the go-ahead: {rules:?}\n{}",
        freed.ledger
    );
}

/// A BREAK THAT JUDGES NO GO-AHEAD HOLDS BACK NO PERSON'S NEED (2026-10-30 review 9, second
/// pass): the go-ahead answers a park at a Cruise break (`say "go" when you're back so` above `I
/// can rebuild …`), the session switches to Land, and the agent's next turn ends at a break on a
/// credential need that opens with the same words (above `I can use your App Store Connect key
/// …`), which the menu bar reads as the line the go-ahead answered. Land types nothing at a
/// break, so no go-ahead is judged there; the line goes up all the same. NEGATIVE CONTROL: a turn
/// that restates the park keeps its line off.
#[test]
fn a_break_that_judges_no_go_ahead_holds_back_no_persons_need() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    let park = waiting(&[
        "Build started.",
        "NEEDS YOU: say \"go\" when you're back so",
        "I can rebuild and push my branch to main.",
    ]);
    let credential = waiting(&[
        "Upload ready.",
        "NEEDS YOU: say \"go\" when you're back so",
        "I can use your App Store Connect key for the upload.",
    ]);
    let live = live_under(&[], "⏺ Checking the workflows.");
    let run = |tag: &str, then: &Vec<String>| {
        let cfg = SupervisorConfig {
            answer_questions: true,
            ..under(Autopilot::Cruise)
        };
        Script {
            policy: cfg.clone(),
            set: &|_| {},
            ..Script::new(tag, Occupancy::Observe, &[0, 30, 900, 1000, 1030])
        }
        .run(&mut |s, i| {
            s.ctl.m.screens = vec![match i {
                0 | 1 => park.clone(),
                2 => live.clone(),
                _ => then.clone(),
            }];
            s.ctl.m.served = 0;
            if i == 2 {
                s.took(7, Some(&cfg), &mut Heard::default());
                s.note_meta("OK autopilot=land");
            }
        })
    };
    let ran = run("carry-land-credential", &credential);
    let went = sent(&ran);
    assert!(
        went.len() == 1 && went[0].starts_with(GO_AHEAD_ANSWER),
        "PRECONDITION: the go-ahead at the Cruise break, nothing under Land: {went:#?}"
    );
    let up = raised(&ran);
    assert!(
        up.iter().any(|r| r.contains("back so")),
        "the credential's line goes up: {up:#?}"
    );
    let restated = run("carry-land-restated", &park);
    let up = raised(&restated);
    assert!(
        up.is_empty(),
        "NEGATIVE CONTROL: the restated park's line stays off: {up:#?}"
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
    // Its NEEDS YOU line is no go-ahead's to answer: it goes up (§15.6).
    let needing = |_| {
        waiting(&[
            "No force-push and nothing published. I'm still waiting for your go-ahead before \
             rebuilding.",
            "NEEDS YOU: say \"rerun the checks\" so I can rebuild the branch.",
        ])
    };
    let needed = Script {
        policy: SupervisorConfig {
            answer_questions: true,
            ..under(Autopilot::Cruise)
        },
        drawn: &needing,
        set: &|_| {},
        ..Script::new(
            "carry-go-ahead-irreversible-need",
            Occupancy::Observe,
            &[0, 30],
        )
    }
    .run(&mut nothing);
    assert_eq!(carries(&needed), 1, "{:#?}", needed.writes());
    let up = raised(&needed);
    assert!(
        up.len() == 1 && up[0].contains("rerun the checks"),
        "NEGATIVE CONTROL: an irreversible reading: {up:#?}"
    );
}

/// A LINE THE AGENT WENT ON PAST NEVER GOES UP AT A BREAK (§15.6): the carry typed at 30 is
/// answered short, so the break at 40 parks on the go-ahead while the carry's ladder waits, and
/// the line is held for the go-ahead. The agent's own wake then ends a turn at the break at 60
/// that names no need: the line it went on past never goes up. NEGATIVE CONTROL: with answers
/// off, the same run puts the line up.
#[test]
fn a_line_the_agent_went_on_past_never_goes_up_at_a_break() {
    const WENT_ON: &[&str] = &["Pushed the branch; the docs are next."];
    let run = |tag: &str, answer_questions: bool| {
        let cfg = SupervisorConfig {
            answer_questions,
            ..under(Autopilot::Cruise)
        };
        let drawn = |i| match i {
            0 => waiting(PLAIN),
            1 => waiting(PARKED),
            _ => waiting(WENT_ON),
        };
        // The wake's live turn ends the break at 40: the loop reads the one at 60 afresh.
        let steps = |i| if i == 2 { Step::Again } else { Step::Stepped };
        Script {
            policy: cfg.clone(),
            drawn: &drawn,
            steps: &steps,
            ..Script::new(tag, Occupancy::Observe, &[30, 40, 60])
        }
        .run(&mut |s, i| {
            if i > 0 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        })
    };
    let cruise = run("carry-went-on", true);
    assert_eq!(carries(&cruise), 1, "{:#?}", cruise.writes());
    assert!(
        cruise
            .noted
            .iter()
            .any(|n| n.contains("1 short answer(s) in a row")),
        "the ladder waits at 40: {:#?}",
        cruise.noted
    );
    assert!(raised(&cruise).is_empty(), "{:#?}", cruise.requests());
    let off = run("carry-went-on-off", false);
    let up = raised(&off);
    assert!(
        up.len() == 1 && up[0].contains("rerun the checks"),
        "NEGATIVE CONTROL: answers off: {up:#?}"
    );
}

/// A LINE HELD FOR THE GO-AHEAD STAYS OFF BEHIND ANOTHER LINE AT THE BREAK (2026-10-30 review 9):
/// the carry typed at 30 is answered short, so the break at 40 parks on the go-ahead while the
/// carry's ladder waits, and its line is held back. At 50 the host's own fact (`disk-free@v1`)
/// takes the same break: the go-ahead is still decided, so its line stays off, and the agent's
/// turn at 60 that frees the space and names no need answers it. NEGATIVE CONTROL: with answers
/// off the same run puts the line up.
#[test]
fn a_line_held_for_the_go_ahead_stays_off_behind_the_hosts_line() {
    const WENT_ON: &[&str] = &["Freed the space; the docs are next."];
    let run = |tag: &str, answer_questions: bool| {
        let host = Arc::new(DoorHost::default());
        host.wants.store(true, Ordering::SeqCst);
        let cfg = SupervisorConfig {
            answer_questions,
            ..under(Autopilot::Cruise)
        };
        // The turn is over and only a shell it left runs on, where the host's fact is offered.
        let shell = |said: &[&str]| -> Vec<String> {
            waiting(said)
                .into_iter()
                .map(|row| {
                    if row.starts_with("✻ Waiting") {
                        "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running".to_string()
                    } else {
                        row.replace("2 workflows", "1 shell")
                    }
                })
                .collect()
        };
        let drawn = |i| match i {
            0 => shell(PLAIN),
            1 | 2 => shell(PARKED),
            _ => shell(WENT_ON),
        };
        Script {
            policy: cfg.clone(),
            drawn: &drawn,
            host: Arc::clone(&host),
            ..Script::new(tag, Occupancy::Observe, &[30, 40, 50, 60])
        }
        .run(&mut |s, i| {
            if matches!(i, 1 | 3) {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
            if i == 2 {
                *host.fact.lock().expect("fact") = Some(disk_free_typed());
            }
        })
    };
    let cruise = run("carry-go-ahead-host-line", true);
    assert!(
        cruise
            .noted
            .iter()
            .any(|n| n.ends_with("background disk-free@v1 step=typed")),
        "the host's line takes the break at 50: {:#?}",
        cruise.noted
    );
    assert_eq!(carries(&cruise), 1, "{:#?}", cruise.writes());
    assert!(raised(&cruise).is_empty(), "{:#?}", cruise.requests());
    let off = run("carry-go-ahead-host-line-off", false);
    let up = raised(&off);
    assert!(
        up.len() == 1 && up[0].contains("rerun the checks"),
        "NEGATIVE CONTROL: answers off: {up:#?}"
    );
}

/// A BREAK WHOSE TURN PARKS NOTHING ENDS THE PARK (2026-10-29 review 3): the go-ahead's count
/// starts again there, as it does at a turn end, so a park the agent takes up again after it
/// gets the go-ahead anew, past the bound one unbroken park gets
/// ([`a_park_at_breaks_recovers_after_its_routine_answer_limit`], the negative control), and the
/// carry goes at the break between.
#[test]
fn a_break_that_parks_nothing_starts_the_go_ahead_count_again() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    use crate::supervise::policy::turn_end::GO_AHEAD_ACTS;
    let cfg = SupervisorConfig {
        answer_questions: true,
        ..under(Autopilot::Cruise)
    };
    // The third break's turn parks nothing.
    let drawn = |i| {
        if matches!(i, 4 | 5) {
            waiting(PLAIN)
        } else {
            waiting(PARKED)
        }
    };
    let ran = Script {
        policy: cfg.clone(),
        drawn: &drawn,
        set: &|_| {},
        ..Script::new(
            "carry-go-ahead-broken",
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
    let go_aheads = sent
        .iter()
        .filter(|s| s.starts_with(GO_AHEAD_ANSWER))
        .count();
    assert!(
        go_aheads > usize::try_from(GO_AHEAD_ACTS).expect("small"),
        "{sent:#?}"
    );
    assert_eq!(carries(&ran), 1, "{sent:#?}");
}

/// THE GO-AHEAD AT A BREAK WAITS NO LONGER THAN AT A POINT (§15.6): six carries answered short
/// put the carry's ladder at its 60 minutes from the carry typed at 3770, and the agent's answer
/// to that carry parks on the go-ahead at 3780. Its line is held off the menu bar for the
/// go-ahead, which goes once `GO_AHEAD_WAIT` (15 minutes) from the last carry is up: by 4680, and
/// not by 4600, as a point's go-ahead waits. NEGATIVE CONTROL: the same run whose answer parks on
/// nothing gets no carry before the ladder's 60 minutes.
#[test]
fn a_go_ahead_at_a_break_waits_no_longer_than_at_a_point() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    let cfg = SupervisorConfig {
        answer_questions: true,
        ..under(Autopilot::Cruise)
    };
    let run = |tag: &str, last: &'static [&'static str], until: i64| {
        let drawn = move |i| {
            if i < 11 {
                waiting(PLAIN)
            } else {
                waiting(last)
            }
        };
        // Each carry is answered ten seconds on, the turn ending on the work again: the ladder
        // (2, 4, 8, 16, 32, then 60 minutes) brings the next break due.
        let mut at_s = vec![30, 40, 160, 170, 410, 420, 890, 900, 1850, 1860, 3770, 3780];
        at_s.push(until);
        Script {
            policy: cfg.clone(),
            drawn: &drawn,
            ..Script::new(tag, Occupancy::Observe, &at_s)
        }
        .run(&mut |s, i| {
            if i % 2 == 1 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        })
    };
    let go_aheads = |ran: &Ran| {
        sent(ran)
            .iter()
            .filter(|s| s.starts_with(GO_AHEAD_ANSWER))
            .count()
    };
    for (until, want) in [(4600, 0), (4680, 1)] {
        let parked = run(&format!("carry-go-ahead-wait-{until}"), PARKED, until);
        assert_eq!(carries(&parked), 6, "{until}: {:#?}", parked.writes());
        assert_eq!(go_aheads(&parked), want, "{until}: {:#?}", parked.writes());
        assert!(
            raised(&parked).is_empty(),
            "{until}: {:#?}",
            parked.requests()
        );
    }
    let plain = run("carry-go-ahead-wait-plain", PLAIN, 4680);
    assert_eq!(
        sent(&plain).len(),
        6,
        "NEGATIVE CONTROL: the carry keeps its ladder: {:#?}",
        plain.writes()
    );
    assert_eq!(carries(&plain), 6, "{:#?}", plain.writes());
}

/// Exhausting routine answers changes approach on the carry ladder without an owner alert.
#[test]
fn a_park_at_breaks_recovers_after_its_routine_answer_limit() {
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
    let routine = usize::try_from(GO_AHEAD_ACTS).expect("small");
    assert!(sent.len() > routine, "the session keeps moving: {sent:#?}");
    assert!(
        sent[..routine]
            .iter()
            .all(|s| s.starts_with(GO_AHEAD_ANSWER))
    );
    assert!(
        sent[routine..]
            .iter()
            .all(|s| s.contains("different safe approach"))
    );
    assert!(
        host.exceptions
            .lock()
            .expect("exceptions")
            .iter()
            .all(Option::is_none)
    );
    assert!(
        !ran.noted
            .iter()
            .any(|n| n.starts_with("EXCEPTION ") && n.contains("when=now"))
    );
}

/// The pacing interval remains in force after the routine answer budget is exhausted.
#[test]
fn a_spent_park_at_a_break_waits_for_recovery_without_alerting() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    use crate::supervise::policy::turn_end::GO_AHEAD_ACTS;
    let cfg = SupervisorConfig {
        answer_questions: true,
        ..under(Autopilot::Cruise)
    };
    let host = Arc::new(DoorHost::default());
    let ran = Script {
        policy: cfg.clone(),
        drawn: &|_| waiting(PARKED),
        host: Arc::clone(&host),
        ..Script::new(
            "carry-go-ahead-spent-at-once",
            Occupancy::Observe,
            &[30, 40, 160, 170, 410, 420],
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
    assert!(
        host.exceptions
            .lock()
            .expect("exceptions")
            .iter()
            .all(Option::is_none)
    );
    assert!(
        ran.noted
            .iter()
            .any(|n| n.contains("recovering:go-ahead-refused"))
    );
}

/// REAL WORK AT A BREAK STARTS THE SPAN AGAIN (2026-10-30 review 9): an agent that took the
/// go-ahead and worked 70 minutes before its next break, parked again, gets the go-ahead again,
/// not recovery; a short answer that stands for an hour changes approach without alerting.
#[test]
fn a_break_after_an_hour_of_work_gets_the_go_ahead_again() {
    use crate::supervise::policy::go_ahead::GO_AHEAD_ANSWER;
    let cfg = SupervisorConfig {
        answer_questions: true,
        ..under(Autopilot::Cruise)
    };
    let drawn = |_| waiting(PARKED);
    // The go-ahead at 30 s, taken at once; the turn reaches its next break `worked_s` after it,
    // and a break is read again ten seconds later, or at 3640 s, an hour and ten seconds after
    // the first act, if that is later.
    let run = |tag: &str, worked_s: i64| {
        let host = Arc::new(DoorHost::default());
        let ran = Script {
            policy: cfg.clone(),
            drawn: &drawn,
            set: &|_| {},
            host: Arc::clone(&host),
            ..Script::new(
                tag,
                Occupancy::Observe,
                &[30, 30 + worked_s, (40 + worked_s).max(3640)],
            )
        }
        .run(&mut |s, i| {
            if i == 1 {
                s.took(7, Some(&cfg), &mut Heard::default());
            }
        });
        let refused = host
            .exceptions
            .lock()
            .expect("exceptions")
            .iter()
            .flatten()
            .any(|why| {
                *why == crate::supervise::policy::exception::Exception::refuses_go_ahead().why
            });
        (ran, refused)
    };
    let (worked, refused) = run("carry-go-ahead-worked", 70 * 60);
    let go_aheads = sent(&worked);
    assert_eq!(go_aheads.len(), 2, "{go_aheads:#?}");
    assert!(
        go_aheads.iter().all(|s| s.starts_with(GO_AHEAD_ANSWER)),
        "{go_aheads:#?}"
    );
    assert!(!refused, "{:#?}", worked.noted);
    let (stood, refused) = run("carry-go-ahead-stood", 10);
    assert_eq!(
        sent(&stood).len(),
        2,
        "NEGATIVE CONTROL: ten seconds is no work: {:#?}",
        stood.writes()
    );
    assert!(!refused, "{:#?}", stood.noted);
    assert!(
        sent(&stood)
            .last()
            .unwrap()
            .contains("different safe approach")
    );
}

/// The table's row of Cruise's carry at a background break: a fenced `send`, then Enter
/// ([`cruise_carries_a_settled_background_break_and_off_and_land_type_nothing`]).
pub(crate) fn observe_row_cruise_carry() -> &'static [&'static str] {
    let _ = ran(
        "observe-carry-cruise",
        Autopilot::Cruise,
        PLAIN,
        &[0, 30],
        &|_| {},
        &mut nothing,
    );
    &[BACKGROUND_CARRY, " enter"]
}
