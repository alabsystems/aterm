// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MEASURE (v6 §14.17.5, slice 12a): the 2026-10-25 outage's turn ends replayed through the
//! rule (`fixtures/outage-2026-10-25-turn-ends.txt`, scrubbed from the drive ledgers and `pmset`).
//!
//! The replay decides each turn end and, through every span the Mac was awake, the point again
//! every 20 s, at the probe's edges and at a person's grace's end, with the real decider (slice
//! 12b's outage wait, §14.17.5.2). It is a decider-level model of the loop, not the loop: the
//! wake's settle (an act in a wake's first 30 s waits, its spread left out) and a person's grace
//! are the replay's own. Slice 12a's two gates are the real predicates the loop calls: G1, the
//! wall-confirm verdict over a transcript that ends on the prompt (`wall_confirm::unanswered`:
//! the outage's wait; refused, on main, so the turn end is a plain point), and G6, a wait's
//! resume held through a dark wake while the Mac sleeps between them
//! (`turn_end_loop::dark_wake_holds`, over the real `SleepWindow`; its hour's bound, which this
//! day's settles never let it reach, is the loop's). Turning either predicate off
//! fails this test; their call sites in the loop are held by the loop tests
//! (`an_outage_over_a_prompt_the_api_never_answered_is_a_wait_not_a_turn_end`,
//! `a_waits_resume_waits_out_a_dark_wake_while_the_mac_is_sleeping`). The `main` arms are the
//! branch's decider with both gates off, not main's build. The fixture's `main` rows are the
//! journal of the day's build, an older rule than either arm (its typed `api-back@v1` retired by
//! 12b); they are the record the arms are read against, not replayed.

use super::*;

use crate::supervise::run::wall_confirm::{Entry, TranscriptEnd, unanswered};
use crate::supervise::run::{DARK_WAKE_STEP, SleepWindow, WAKE_SETTLE, dark_wake_holds};

const FIXTURE: &str = include_str!("../fixtures/outage-2026-10-25-turn-ends.txt");
const CLOSED_LID: &str = include_str!("../fixtures/closed-lid-on-power-2026-11-01.txt");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Last {
    User,
    Notice,
    Reread,
}

#[derive(Debug, Default)]
struct Day {
    sessions: Vec<(String, String, usize)>,
    spans: Vec<(bool, i64, i64)>,
    reach: Vec<(i64, &'static str)>,
    person: Vec<(i64, i64)>,
    ends: Vec<(i64, Last, String)>,
    main: Vec<(i64, String)>,
    exit: i64,
}

fn secs(hms: &str) -> i64 {
    let mut it = hms.split(':').map(|p| p.parse::<i64>().expect(hms));
    let (h, m, s) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
    h * 3600 + m * 60 + s
}

fn hms(s: i64) -> String {
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn day() -> Day {
    let mut d = Day::default();
    for line in FIXTURE
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let (head, rest) = line.split_once(' ').expect(line);
        let words: Vec<&str> = rest.splitn(3, ' ').collect();
        match head {
            "session" => d.sessions.push((
                words[0].to_string(),
                words[1].to_string(),
                words[2].parse().expect(line),
            )),
            "dark" | "full" => d
                .spans
                .push((head == "full", secs(words[0]), secs(words[1]))),
            "reach" => d.reach.push((
                secs(words[1]),
                match words[0] {
                    "unknown" => "unknown",
                    "down" => "down",
                    _ => "up",
                },
            )),
            "person" => d.person.push((secs(words[0]), secs(words[1]))),
            "end" => d.ends.push((
                secs(words[0]),
                match words[1] {
                    "user" => Last::User,
                    "notice" => Last::Notice,
                    _ => Last::Reread,
                },
                words[2].trim_start_matches("⏺ ").to_string(),
            )),
            "main" => d
                .main
                .push((secs(words[0]), rest[words[0].len() + 1..].to_string())),
            "exit" => d.exit = secs(words[0]),
            other => panic!("unknown row {other}: {line}"),
        }
    }
    d
}

/// What one arm of the replay did.
#[derive(Debug, Default)]
struct Run {
    /// Acts other than a wait's resume, typed while the screen showed the system message.
    prompts: Vec<(i64, &'static str)>,
    /// A wait's resumes typed before the route came back: the wall answers each again.
    into: Vec<(i64, &'static str)>,
    /// The resume typed once the route was back, with whether the Mac was in a dark wake. The
    /// replay ends at it: what the program did next is not in the ledgers.
    resume: Option<(i64, &'static str, bool)>,
    /// Resumes the dark-wake gate held.
    gated: usize,
}

#[derive(Debug, Clone, Copy)]
struct Arm {
    /// G1 (`unanswered`): a wall over a transcript that ends on the prompt is a wait (the
    /// branch), not refused.
    g1: bool,
    /// G6 (`dark_wake_holds`): a wait's resume held through a dark wake while the Mac sleeps.
    gate: bool,
    /// The day's sleep: decided only in its wakes, under the wake's settle and a person's grace.
    /// Off, the counterfactual of a Mac awake all along: decided every step from the first wall,
    /// its probe measuring the route down from the first wall until the route came back.
    asleep: bool,
}

fn replay(d: &Day, autopilot: Autopilot, arm: Arm) -> Run {
    let step = i64::try_from(DARK_WAKE_STEP.as_secs()).unwrap();
    let settle = i64::try_from(WAKE_SETTLE.as_secs()).unwrap();
    let base = t0();
    let first = d.ends[0].0;
    let back = d
        .reach
        .iter()
        .find(|(_, w)| *w == "up")
        .map(|(t, _)| *t)
        .expect("the clearing");
    let at_s = |s: i64| base + Duration::from_secs(u64::try_from(s - first + 3600).unwrap());
    let reach_at = |s: i64| {
        let (since, word) = if arm.asleep {
            d.reach
                .iter()
                .rfind(|(t, _)| *t <= s)
                .copied()
                .unwrap_or((s, "unknown"))
        } else if s < back {
            (first, "down")
        } else {
            (back, "up")
        };
        match word {
            "down" => Reach::Down { since: at_s(since) },
            "up" => Reach::Up { since: at_s(since) },
            _ => Reach::Unknown,
        }
    };
    let span_of = |s: i64| d.spans.iter().find(|(_, a, b)| *a <= s && s <= *b).copied();
    // How much of `from..to` the Mac was awake: what the loop's awake clock would have gained.
    let awake_in = |from: i64, to: i64| -> i64 {
        d.spans
            .iter()
            .map(|(_, a, b)| (to.min(*b) - from.max(*a)).max(0))
            .sum()
    };
    let mut instants: Vec<i64> = Vec::new();
    if arm.asleep {
        for &(_, a, b) in &d.spans {
            instants.extend((a..=b).step_by(usize::try_from(step).unwrap()));
            instants.push(a + settle);
        }
        let edges = d
            .reach
            .iter()
            .map(|(t, _)| *t)
            .chain(d.person.iter().map(|(_, b)| *b));
        instants.extend(edges);
        instants.retain(|t| span_of(*t).is_some());
    } else {
        instants.extend((first..d.exit).step_by(usize::try_from(step).unwrap()));
        instants.push(back);
    }
    let mut events: Vec<(i64, Option<usize>)> = instants.into_iter().map(|t| (t, None)).collect();
    events.extend(d.ends.iter().enumerate().map(|(i, e)| (e.0, Some(i))));
    events.sort_unstable();
    events.dedup();
    // The day's transcript at each end on the prompt: the person's prompt, no reply after it.
    let on_prompt = TranscriptEnd {
        last: Entry::Input,
        closed: false,
        row: None,
        at: None,
        prompted: true,
    };
    let mut st = TurnEndState::default();
    let mut run = Run::default();
    let mut last: Option<TurnEndReading> = None;
    let mut window = SleepWindow::default();
    let mut marked: Option<i64> = None;
    for (s, end) in events {
        if s >= d.exit || run.resume.is_some() {
            break;
        }
        if let Some(from) = marked.replace(s) {
            let awake = awake_in(from, s);
            let of = |n: i64| Duration::from_secs(u64::try_from(n).unwrap());
            window.add(of(awake), of(s - from - awake));
        }
        let reading = match end {
            Some(i) => {
                let (_, seen, ref row) = d.ends[i];
                let kind = aterm_phase::classify_wall(row).expect(row);
                assert_eq!(outage_named(kind), Some(Outage::Connection), "{kind:?}");
                match seen {
                    Last::Reread => last.as_ref().map(again).expect("a read before it"),
                    Last::User if !(arm.g1 && unanswered(kind, &on_prompt)) => TurnEndReading {
                        autopilot,
                        ..idle(row, Some(Duration::from_secs(8)))
                    },
                    Last::User | Last::Notice => TurnEndReading {
                        autopilot,
                        ..walled(kind, row, Some(Duration::from_secs(8)))
                    },
                }
            }
            None => match &last {
                Some(r) => again(r),
                None => continue,
            },
        };
        let reading = TurnEndReading {
            reach: if reading.wall.is_some() {
                reach_at(s)
            } else {
                Reach::Unknown
            },
            ..reading
        };
        let now = at_s(s);
        let action = at(&mut st, &reading, now);
        last = Some(reading.clone());
        let TurnEndAction::Type { rule_id, .. } = &action else {
            continue;
        };
        let span = span_of(s);
        let settling = span.is_some_and(|(_, a, _)| s - a < settle);
        let graced = d.person.iter().any(|(a, b)| *a <= s && s < *b);
        if arm.asleep && (settling || graced) {
            continue;
        }
        let wake = if arm.asleep {
            span.map(|(full, _, _)| full)
        } else {
            Some(true)
        };
        let dark = wake == Some(false);
        let rule: &'static str = rule_id;
        if arm.gate && dark_wake_holds(Some(rule), || wake, || window.mostly_asleep()) {
            run.gated += 1;
            continue;
        }
        if !is_wait_resume(rule) {
            run.prompts.push((s, rule));
        } else if s < back {
            run.into.push((s, rule));
        } else {
            run.resume = Some((s, rule, dark));
        }
        st.acted(&action, &reading, now);
    }
    run
}

const MAIN: Arm = Arm {
    g1: false,
    gate: false,
    asleep: true,
};
const MAIN_AWAKE: Arm = Arm {
    g1: false,
    gate: false,
    asleep: false,
};
const BRANCH: Arm = Arm {
    g1: true,
    gate: true,
    asleep: true,
};
const BRANCH_AWAKE: Arm = Arm {
    g1: true,
    gate: true,
    asleep: false,
};

fn said(run: &Run) -> String {
    let list = |v: &[(i64, &str)]| {
        if v.is_empty() {
            "0".to_string()
        } else {
            let first = v.iter().map(|(t, r)| format!("{} {r}", hms(*t)));
            format!(
                "{} ({})",
                v.len(),
                first.take(3).collect::<Vec<_>>().join(", ")
            )
        }
    };
    format!(
        "prompts into the system message {} · resumes into the standing wall {} · the resume {} · \
         held in dark wakes {}",
        list(&run.prompts),
        list(&run.into),
        run.resume
            .map_or("none".to_string(), |(t, r, dark)| format!(
                "{} {r} in a {} wake",
                hms(t),
                if dark { "dark" } else { "full" }
            )),
        run.gated
    )
}

/// The day's record: the instant the day's build typed its resume (`api-back@v1`, retired by 12b).
fn the_days_resume(d: &Day) -> i64 {
    d.main
        .iter()
        .find(|(_, l)| l.contains(&format!("rule={RULE_API_BACK}")))
        .map(|(t, _)| *t)
        .expect("the day's resume")
}

#[test]
fn the_outage_replayed_types_nothing_into_a_system_message_and_resumes_once_in_a_full_wake() {
    let d = day();
    assert_eq!(d.ends.len(), 3, "the day's turn ends on a system message");
    let back = the_days_resume(&d);
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        // Main as the day ran: nothing typed into the screens (every wake's settle outlasted the
        // dark wakes), and its one resume typed the instant the route read up, in the dark wake
        // the Mac slept again 26 s after, as the day's build typed its own then.
        let main = replay(&d, autopilot, MAIN);
        assert!(
            main.prompts.is_empty() && main.into.is_empty(),
            "{autopilot:?}: {main:?}"
        );
        assert_eq!(
            main.resume,
            Some((back, RULE_OUTAGE_RESUME, true)),
            "{autopilot:?}: {main:?}"
        );
        // THE NEGATIVE CONTROL: main's G1 on a Mac awake reads the wall over the prompt the API
        // never answered as a plain turn end, and types its prompt into the ENOTFOUND screen on
        // the short turn's back-off, before the program's own notice of the wall. A
        // counterfactual: its second prompt assumes the screen did not move after the first.
        let main_awake = replay(&d, autopilot, MAIN_AWAKE);
        let want = match autopilot {
            Autopilot::Land => RULE_LAND,
            _ => RULE_CONTINUE,
        };
        assert!(
            main_awake.prompts.len() == 2
                && main_awake.prompts.first().is_some_and(|(_, r)| *r == want)
                && main_awake
                    .prompts
                    .iter()
                    .all(|(t, _)| (d.ends[0].0..d.ends[1].0).contains(t)),
            "{autopilot:?}: {main_awake:?}"
        );
        // 12a: nothing typed into a system message, the resume held through every dark wake and
        // typed once, in the full wake at 18:19:13 past its settle.
        let branch = replay(&d, autopilot, BRANCH);
        assert!(
            branch.prompts.is_empty() && branch.into.is_empty(),
            "{autopilot:?}: {branch:?}"
        );
        let (t, rule, dark) = branch.resume.expect("the branch's resume");
        assert_eq!(
            (hms(t).as_str(), rule, dark),
            ("18:19:43", RULE_OUTAGE_RESUME, false)
        );
        assert!(
            branch.gated > 0,
            "the dark wake's resume was held: {branch:?}"
        );
        // Awake, 12a types nothing until the route is back, then the one resume.
        let branch_awake = replay(&d, autopilot, BRANCH_AWAKE);
        assert!(
            branch_awake.prompts.is_empty() && branch_awake.into.is_empty(),
            "{autopilot:?}: {branch_awake:?}"
        );
        assert_eq!(branch_awake.resume, Some((back, RULE_OUTAGE_RESUME, false)));
        for (arm, run) in [
            ("main as the day ran", &main),
            ("main on a Mac awake", &main_awake),
            ("12a as the day ran", &branch),
            ("12a on a Mac awake", &branch_awake),
        ] {
            println!(
                "MEASURE 2026-10-25 claude s-fb6b {autopilot:?} · {arm}: {}",
                said(run)
            );
        }
    }
    for (harness, sid, ends) in &d.sessions {
        println!("MEASURE 2026-10-25 {harness} {sid}: turn ends on a system message {ends}");
    }
    let met: Vec<&str> = d
        .sessions
        .iter()
        .filter(|s| s.2 > 0)
        .map(|s| s.1.as_str())
        .collect();
    assert_eq!(met, ["s-fb6b"], "one session met a system message");
}

/// THE SECOND MEASURE (v6 §14.17.5.5): a Mac kept running with its lid closed, its wakes as
/// `pmset` logged them (`fixtures/closed-lid-on-power-2026-11-01.txt`). It reads a dark wake all
/// day and never a full one, so a gate on the wake alone holds a wait's resume for good; it runs
/// most of the time, and the real gate holds nothing. `pmset`'s gaps count the way down as sleep,
/// which the kernel's clocks do not (94% awake in a live read of them that day), so this is the
/// harder case for the rule. THE CONTROL is the same wakes after the shortest real sleep of the
/// 2026-10-25 replay above (48 s): nearly all are held, and each let through is one with minutes
/// of running behind it.
#[test]
fn a_closed_lid_kept_running_holds_no_resume_but_the_same_wakes_after_real_sleeps_do() {
    let spans: Vec<(i64, i64)> = CLOSED_LID
        .lines()
        .filter_map(|l| l.strip_prefix("dark "))
        .map(|r| {
            let (a, b) = r.split_once(' ').expect(r);
            (secs(a), secs(b))
        })
        .collect();
    let whole = spans[spans.len() - 1].1 - spans[0].0;
    let up: i64 = spans.iter().map(|(a, b)| b - a).sum();
    assert!(up * 100 >= whole * 80, "the Mac ran: {up} s of {whole} s");
    let longest = spans.windows(2).map(|w| w[1].0 - w[0].1).max();
    assert_eq!(longest, Some(29), "the longest sleep between its wakes");
    // The wake's settle comes before the gate and reads the wall clock's gain between two marks, up
    // to one 20 s wait of waking apart, several naps perhaps: never enough on this day to arm it,
    // wherever the waits fall.
    let mut gain = 0;
    for first in 0..20 {
        let (mut mark, mut since) = (spans[0].0, first);
        for &(woke, until) in &spans {
            let mut at = woke;
            while at < until {
                let step = (until - at).min(20 - since);
                at += step;
                since += step;
                if since == 20 {
                    if at - mark >= 20 {
                        gain = gain.max(at - mark - 20);
                    }
                    (mark, since) = (at, 0);
                }
            }
        }
    }
    assert_eq!(gain, 41, "the largest gain over one wait");
    assert!(!crate::supervise::run::slept_through(gain, 0));
    let of = |n: i64| Duration::from_secs(u64::try_from(n).unwrap());
    let held_of = |slept: Option<i64>| {
        let mut window = SleepWindow::default();
        let (mut decided, mut held) = (0, 0);
        for w in spans.windows(2) {
            let (woke, until) = w[1];
            window.add(Duration::ZERO, of(slept.unwrap_or(woke - w[0].1)));
            let mut at = woke;
            while at < until {
                let step = (until - at).min(20);
                window.add(of(step), Duration::ZERO);
                at += step;
                decided += 1;
                held += usize::from(dark_wake_holds(
                    Some(RULE_OUTAGE_RESUME),
                    || Some(false),
                    || window.mostly_asleep(),
                ));
            }
        }
        (decided, held)
    };
    let (decided, held) = held_of(None);
    assert_eq!(decided, 913);
    assert_eq!(held, 0, "the resume is typed at its first decision");
    let (decided, held) = held_of(Some(48));
    assert_eq!(
        (decided, held),
        (913, 764),
        "the day's long wakes run a turn"
    );
}

/// THE WITNESS ARM (v6 §14.17.5.4, slice 12c): a long overload on one route, two sessions waiting
/// it out. Synthetic, not a day's record: no ledger holds a long overload two sessions met. The
/// first session walls at 0, the second 7 minutes later; the route clears at 125 minutes, each try
/// before it meets the wall again (read one step after it), and a try past it is answered three
/// minutes after it was typed.
/// Decided every 20 s (the loop's longest wait step) with the real decider; the host's book of
/// answers is the real [`Witnesses`], fed only when an answer lands.
mod witness_arm {
    use super::*;

    use crate::harness::witness::{Base, RouteKey, Witnesses};
    use aterm_phase::Program;

    const OVERLOADED: &str = "API Error: 529 Overloaded.";
    const STEP: i64 = 20;
    const WORK: i64 = 180;
    const CLEAR: i64 = 125 * 60;
    const WALLS: [i64; 2] = [0, 7 * 60];
    const SIDS: [&str; 2] = ["s-first", "s-mate"];

    /// One cleared line: when it was typed, what it said, and whether it met the wall again.
    type Line = (i64, Clearing, bool);

    fn wait_out(witnesses: bool) -> Vec<Vec<Line>> {
        let at_s = |s: i64| t0() + Duration::from_secs(u64::try_from(s).unwrap());
        let route = RouteKey {
            program: Program::Claude,
            base: Base::anthropic(),
        };
        let wall = walled(WallKind::Overloaded, OVERLOADED, Some(3 * MIN));
        let met = TurnEndReading {
            worked: Some(Duration::from_secs(4)),
            ..wall.clone()
        };
        let measured = cleared_text(Cleared::Outage(Outage::Overload), Clearing::Measured);
        let mut book = Witnesses::default();
        let mut landing: Vec<(i64, usize)> = Vec::new();
        let mut st = [TurnEndState::default(), TurnEndState::default()];
        let mut next: [Option<TurnEndReading>; 2] = [None, None];
        let mut done = [false; 2];
        let mut lines = vec![Vec::new(); 2];
        let mut s = 0;
        while s <= CLEAR + 3 * 3600 && done.iter().any(|d| !d) {
            for &(t, i) in landing.iter().filter(|(t, _)| *t == s) {
                book.answered(route.clone(), SIDS[i], at_s(t));
            }
            for i in 0..2 {
                if done[i] || s < WALLS[i] {
                    continue;
                }
                let r = next[i].take().unwrap_or_else(|| wall.clone());
                let r = TurnEndReading {
                    witness: book.witness(&route, SIDS[i]).filter(|_| witnesses),
                    ..r
                };
                let a = act(&mut st[i], &r, at_s(s));
                next[i] = Some(again(&r));
                let TurnEndAction::Type { rule_id, text } = &a else {
                    continue;
                };
                assert_eq!(*rule_id, RULE_OUTAGE_RESUME, "{a:?}");
                let clearing = if *text == measured {
                    Clearing::Measured
                } else {
                    Clearing::Blind
                };
                lines[i].push((s, clearing, s < CLEAR));
                if s < CLEAR {
                    next[i] = Some(met.clone());
                } else {
                    done[i] = true;
                    landing.push((s + WORK, i));
                }
            }
            s += STEP;
        }
        assert!(done.iter().all(|d| *d), "each session resumed: {lines:?}");
        lines
    }

    fn says(lines: &[Line]) -> String {
        let into = lines.iter().filter(|l| l.2).count();
        let (t, clearing, _) = lines.last().copied().expect("a resume");
        format!(
            "{} cleared lines ({into} into the standing wall), the resume {} {:?}, {} min {} s \
             past the clearing",
            lines.len(),
            hms(t),
            clearing,
            (t - CLEAR) / 60,
            (t - CLEAR) % 60
        )
    }

    #[test]
    fn a_long_overload_on_one_route_resumes_the_waiting_session_on_its_mates_answer_not_its_rung() {
        let blind = wait_out(false);
        let seen = wait_out(true);
        let past = |lines: &[Vec<Line>]| -> Vec<Line> {
            lines.iter().flatten().filter(|l| !l.2).copied().collect()
        };
        // The outage itself is the ladder's in both arms: every line into the standing wall is a
        // blind try on its rung, none of them measured, and as many with witnesses as without.
        for lines in [&blind, &seen] {
            assert!(
                lines
                    .iter()
                    .flatten()
                    .filter(|l| l.2)
                    .all(|l| l.1 == Clearing::Blind),
                "{lines:?}"
            );
        }
        // Each try on its rung: the first from the wall, each later one from the step the try's
        // wall was read again, one step after it.
        let rungs = |from: i64, n: usize| -> Vec<i64> {
            let mut at = from;
            (0..n)
                .map(|k| {
                    let gap =
                        outage_gap(&TurnEndTiming::default(), u32::try_from(k).unwrap(), None);
                    at += i64::try_from(gap.as_secs()).unwrap() + if k == 0 { 0 } else { STEP };
                    at
                })
                .collect()
        };
        assert_eq!(
            blind[0].iter().map(|l| l.0).collect::<Vec<_>>(),
            rungs(WALLS[0], 7),
            "{blind:?}"
        );
        assert_eq!(
            blind[1].iter().map(|l| l.0).collect::<Vec<_>>(),
            rungs(WALLS[1], 6)
        );
        assert_eq!(
            seen[1], blind[1],
            "the mate finds the clearing on its own rung"
        );
        // THE NEGATIVE CONTROL: blind, the first session's next rung lands 59 minutes past the
        // clearing; on the mate's answer it resumes once, measured, the instant the answer lands,
        // and nothing is tried blind past the clearing but the mate's own find.
        let found = (CLEAR + 5 * 60 + 40, Clearing::Blind, false);
        assert_eq!(blind[1].last(), Some(&found));
        assert_eq!(
            past(&blind),
            [(CLEAR + 59 * 60, Clearing::Blind, false), found],
            "{blind:?}"
        );
        let answer = found.0 + WORK;
        assert_eq!(
            past(&seen),
            [(answer, Clearing::Measured, false), found],
            "{seen:?}"
        );
        assert_eq!(seen[0][..6], blind[0][..6], "{seen:?}");
        for (arm, lines) in [("without witnesses", &blind), ("with witnesses", &seen)] {
            for (i, l) in lines.iter().enumerate() {
                println!(
                    "MEASURE long overload (synthetic, one route) {} · {arm}: {}",
                    SIDS[i],
                    says(l)
                );
            }
        }
    }
}
