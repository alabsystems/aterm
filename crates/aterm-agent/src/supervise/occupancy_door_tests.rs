// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OCCUPANCY DOOR IN THE LOOP (`run/occupancy_loop.rs`'s `occupancy_break`).

use std::sync::atomic::AtomicUsize;

use super::*;
use crate::supervise::config::Autopilot;
use crate::supervise::occupancy::reading::LOOK_EVERY_MS;
use crate::supervise::occupancy::rules::{RuleId, WOULD_RULE};
use crate::supervise::occupancy::{OccupancyReading, OccupancySaid, SaidVerdict, SlotsSeen};
use crate::supervise::policy::turn_end::Reach;

/// A host whose board reads `reading`, counting the asks and keeping what the door told it.
#[derive(Debug, Default)]
struct Board {
    reading: std::sync::Mutex<OccupancyReading>,
    asks: AtomicUsize,
    said: std::sync::Mutex<Vec<OccupancySaid>>,
}

impl Board {
    fn reading(reading: OccupancyReading) -> Arc<Self> {
        Arc::new(Self {
            reading: std::sync::Mutex::new(reading),
            asks: AtomicUsize::new(0),
            said: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn said(&self) -> Vec<OccupancySaid> {
        self.said.lock().expect("the said").clone()
    }

    fn asks(&self) -> usize {
        self.asks.load(Ordering::SeqCst)
    }
}

impl IdleHost for Board {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn occupancy(&self) -> OccupancyReading {
        self.asks.fetch_add(1, Ordering::SeqCst);
        self.reading.lock().expect("the board").clone()
    }
    fn occupancy_said(&self, said: &OccupancySaid) {
        self.said.lock().expect("the said").push(said.clone());
    }
}

/// A board on which every one of `hold-room`'s machine terms holds, a build slot of this
/// session's held a minute before it was taken.
fn room() -> OccupancyReading {
    OccupancyReading {
        taken_ms: T0,
        stale: false,
        avail_mib: Some(96 * 1024),
        pressure: Some(crate::supervise::memory::Pressure::Normal),
        disk: Some(crate::harness::disk::Level::Ok),
        slots: Some(SlotsSeen { usable: 1, n: 3 }),
        slots_free_ms: Some(20 * 60_000),
        room: Some(true),
        held_slot_last_ms: Some(T0 - MIN),
    }
}

/// The `fact-would@v1` rows of the ledger at `path`: each `(rule, held_ms)`, every one `waiting`
/// with no command (nothing follows it, nothing was typed).
fn would_rows(path: &Path) -> Vec<(String, i64)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|v| v.get("rule_id").and_then(aterm_json::Value::as_str) == Some(WOULD_RULE))
        .map(|v| {
            let text = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            assert_eq!(text("decision"), "waiting", "{v:?}");
            assert_eq!(text("command"), "", "{v:?}");
            let words: Vec<String> = text("reason").split(' ').map(str::to_string).collect();
            let rule = words
                .iter()
                .find_map(|w| w.strip_prefix("rule="))
                .unwrap_or_else(|| panic!("no rule in {words:?}"))
                .to_string();
            (rule, held_ms(&words))
        })
        .collect()
}

/// A rule's `OCCUPANCY` records in `noted`: `(seq, rule, verdict's word)`.
fn verdicts(noted: &[String]) -> Vec<(u64, String, String)> {
    noted
        .iter()
        .map(|line| JournalRecord::of_line(0, None, line, None))
        .filter(|r| r.kind == "occupancy" && r.phase.contains('@'))
        .map(|r| {
            let word = r.summary.rsplit(' ').next().unwrap_or("").to_string();
            let word = match word.strip_prefix("next_s=") {
                Some(_) => "paced".to_string(),
                None => word,
            };
            (r.seq.unwrap_or(0), r.phase, word)
        })
        .collect()
}

const NUDGE: &str = "wait-nudge@v1";
const ROOM: &str = "hold-room@v1";

/// The break with usage credits carrying the session: Claude Code's lasting notice drawn
/// right-aligned on the mode row (`aterm_phase::credits`, measured on 2.1.300).
fn credits_break() -> Vec<String> {
    let mut r = break_screen();
    let mode = r.last_mut().expect("the mode row");
    *mode = aterm_phase::prompt::fixtures::right_aligned(mode, "Now using usage credits");
    r
}

/// A break in a session nobody asked anything: the launch card with no message under it, and
/// the person's own dev server left running in a shell.
fn taskless_break() -> Vec<String> {
    let mut r = rows(&[
        " ▐▛███▛█   Claude Code v2.1.300",
        "▝▜██████▀  Sonnet 5.5 · Claude API",
        " ▝▝   ▝▝   ~/proj",
        "",
        "! npm run dev",
        "  ⎿  Running in the background",
        "",
        "✻ Crunched for 2s · done 11:07 AM · 1 shell still running",
        "",
    ]);
    r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
    assert!(aterm_phase::turn::fresh(&r), "no task asked: {r:#?}");
    r
}

/// What a day of breaks gave: the journal's lines, the would-be rows, the requests the session
/// made and the board's asks.
struct Day {
    noted: Vec<String>,
    would: Vec<(String, i64)>,
    requests: Vec<String>,
    asks: usize,
    /// What the door told its host ([`IdleHost::occupancy_said`]).
    said: Vec<OccupancySaid>,
}

/// A held session's day: one break, read at each of `at_s` (seconds from its start, the read's
/// seq its place from 1), on the board `board` where `hosted`, under `policy`, unattended unless
/// `attended`; `before` runs ahead of each read with the read's seq.
fn day(
    tag: &str,
    hosted: bool,
    policy: Option<SupervisorConfig>,
    attended: bool,
    board: OccupancyReading,
    at_s: &[i64],
    before: &mut dyn FnMut(&mut Session<'_, Mock>, u64),
) -> Day {
    day_on(tag, hosted, policy, attended, board, at_s, before, &|_| {
        break_screen()
    })
}

/// [`day`], each read's screen `drawn` of its seq.
#[allow(clippy::too_many_arguments)]
fn day_on(
    tag: &str,
    hosted: bool,
    policy: Option<SupervisorConfig>,
    attended: bool,
    board: OccupancyReading,
    at_s: &[i64],
    before: &mut dyn FnMut(&mut Session<'_, Mock>, u64),
    drawn: &dyn Fn(u64) -> Vec<String>,
) -> Day {
    let (dir, path) = ledger_at(tag);
    let mut m = Mock::new(Vec::new());
    let has_board = board.taken_ms > 0;
    let held_ago = board
        .held_slot_last_ms
        .map(|at| board.taken_ms.saturating_sub(at));
    let host = Board::reading(board);
    let (noted, asks) = {
        let as_host = hosted.then(|| Arc::clone(&host) as Arc<dyn IdleHost>);
        let (mut s, clock) = hold_session(&mut m, &path, as_host, Duration::ZERO);
        let mut heard = Heard::default();
        let mut person = Attended::default();
        let review: &mut dyn Review = if attended { &mut person } else { &mut heard };
        let cfg = policy.as_ref();
        let mut since = None;
        let mut at_ms = T0;
        for (seq, s_at) in (1..).zip(at_s) {
            let ahead = T0 + s_at * 1000 - at_ms;
            if ahead > 0 {
                skip(&clock, &mut at_ms, ahead);
            }
            if has_board {
                let looked_at = T0 + (at_ms - T0).div_euclid(LOOK_EVERY_MS) * LOOK_EVERY_MS;
                let mut reading = host.reading.lock().expect("the board");
                reading.taken_ms = looked_at;
                reading.held_slot_last_ms = held_ago.map(|ago| looked_at.saturating_sub(ago));
            }
            s.clock_marks = Some((s.fx.mono(), at_ms));
            before(&mut s, seq);
            s.host_steps_in_background(&screen_at(seq, drawn(seq)), &mut since, cfg, review)
                .expect("the break's step");
        }
        let noted = if attended {
            person.noted.clone()
        } else {
            heard.noted.clone()
        };
        (noted, host.asks())
    };
    let would = would_rows(&path);
    let _ = std::fs::remove_dir_all(&dir);
    Day {
        noted,
        would,
        requests: m.requests,
        asks,
        said: host.said(),
    }
}

/// Every `step_s` seconds from `from_s` to `to_s`, both included.
fn every(from_s: i64, to_s: i64, step_s: usize) -> Vec<i64> {
    (from_s..=to_s).step_by(step_s).collect()
}

fn nothing_before(_: &mut Session<'_, Mock>, _: u64) {}

/// T-OBS, THE F1 DAY UNDER OBSERVE: a session held six hours on its own background work, read
/// every five minutes, on a board with room from the start.
#[test]
fn the_f1_day_under_observe_books_what_would_go_and_types_nothing() {
    let reads = every(0, 6 * 3600, 300);
    let observe = hosted(30).policy;
    assert_eq!(observe.occupancy, Occupancy::Observe);
    let d = day(
        "door-f1",
        true,
        Some(observe.clone()),
        false,
        room(),
        &reads,
        &mut nothing_before,
    );
    assert_eq!(
        d.would,
        [
            (NUDGE.to_string(), 30 * MIN),
            (ROOM.to_string(), 120 * MIN),
            (ROOM.to_string(), 240 * MIN),
            (ROOM.to_string(), 360 * MIN),
        ]
    );
    let would_lines: Vec<_> = verdicts(&d.noted)
        .into_iter()
        .filter(|(_, _, w)| w == "would")
        .map(|(seq, rule, _)| (seq, rule))
        .collect();
    assert_eq!(
        would_lines,
        [
            (7, NUDGE.to_string()),
            (25, ROOM.to_string()),
            (49, ROOM.to_string()),
            (73, ROOM.to_string()),
        ],
        "one line a would-be, at the read it was due"
    );
    assert_eq!(d.asks, 3, "asked once at each read hold-room could be due");
    let off = SupervisorConfig {
        occupancy: Occupancy::Off,
        ..observe
    };
    let quiet = day(
        "door-f1-off",
        true,
        Some(off),
        false,
        room(),
        &reads,
        &mut nothing_before,
    );
    assert!(quiet.would.is_empty() && verdicts(&quiet.noted).is_empty());
    assert_eq!(quiet.asks, 0, "off asks the board nothing");
    assert_eq!(
        d.requests, quiet.requests,
        "observe asks the server nothing off does not"
    );
    assert!(
        !d.requests.iter().any(|r| ["turn", "send", "key", "status"]
            .iter()
            .any(|p| r.starts_with(p))),
        "{:?}",
        d.requests
    );
}

/// THE DOOR TELLS ITS HOST ONCE A CHANGE (design §6, `IdleHost::occupancy_said`): the F1 day
/// under `observe`, read every five minutes, tells the host the hold, each would-be line and
/// the ready proxy as they change, never the same twice running, and far fewer times than it
/// reads; what it asks the server is what `off` asks. NEGATIVE CONTROL: under `off` nothing is
/// told.
#[test]
fn the_door_tells_its_host_once_a_change() {
    let reads = every(0, 6 * 3600, 300);
    let observe = hosted(30).policy;
    let d = day(
        "door-said",
        true,
        Some(observe.clone()),
        false,
        room(),
        &reads,
        &mut nothing_before,
    );
    assert!(!d.said.is_empty(), "the door told its host");
    assert!(d.said.len() < reads.len() / 4, "{} tells", d.said.len());
    for pair in d.said.windows(2) {
        assert!(!pair[0].same_change(&pair[1]), "told twice: {pair:#?}");
    }
    for said in &d.said {
        assert_eq!(said.mode, Occupancy::Observe);
        assert_eq!(said.hold_word(), "undeclared", "{said:#?}");
        assert!(
            said.spent_under_prompt().is_empty(),
            "observe spends nothing the owner sees"
        );
    }
    let since = d.said[0].hold.expect("the hold").since_ms;
    assert!(
        d.said
            .iter()
            .all(|s| s.hold.map(|h| h.since_ms) == Some(since)),
        "one hold all day"
    );
    let told = |rule: RuleId, verdict: SaidVerdict| {
        d.said
            .iter()
            .filter(|s| s.rules[rule.index()].verdict == verdict)
            .count()
    };
    assert!(
        told(RuleId::WaitNudge, SaidVerdict::Would) >= 1,
        "{:#?}",
        d.said
    );
    assert!(
        told(RuleId::HoldRoom, SaidVerdict::Would) >= 1,
        "{:#?}",
        d.said
    );
    // The ready proxy: told once the hold passed hold-room's two hours on a board with room.
    let first_ready = d.said.iter().position(|s| s.ready).expect("ready told");
    let at_ready = &d.said[first_ready];
    assert!(
        at_ready.at_ms - since >= crate::supervise::occupancy::rules::HOLD_ROOM_AFTER_MS,
        "{at_ready:#?}"
    );
    assert!(d.said[..first_ready].iter().all(|s| !s.ready));

    let off = SupervisorConfig {
        occupancy: Occupancy::Off,
        ..observe
    };
    let quiet = day(
        "door-said-off",
        true,
        Some(off),
        false,
        room(),
        &reads,
        &mut nothing_before,
    );
    assert!(quiet.said.is_empty(), "off tells nothing");
    assert_eq!(
        d.requests, quiet.requests,
        "the tell asks the server nothing"
    );
}

/// THE DOOR RUNS ON HOSTED, UNATTENDED LOOPS UNDER A POLICY WHOSE OCCUPANCY IS NOT OFF ALONE (F2):
/// the same 40 minutes that would nudge under observe book and say nothing with no host, with a
/// person attending, with no policy, or under `off`. Under `prompt` the due line goes to the
/// write instead of a would-be row (`occupancy_write_tests.rs`): here the host's `status` names
/// no person's stamp, which the write reads as someone, so nothing is typed.
#[test]
fn the_door_is_open_on_hosted_unattended_loops_only() {
    let reads = every(0, 40 * 60, 300);
    let observe = hosted(30).policy;
    let run = |tag, hosted, policy, attended| {
        day(
            tag,
            hosted,
            policy,
            attended,
            room(),
            &reads,
            &mut nothing_before,
        )
    };
    let open = run("door-open", true, Some(observe.clone()), false);
    assert_eq!(open.would, [(NUDGE.to_string(), 30 * MIN)]);
    let prompt = SupervisorConfig {
        occupancy: Occupancy::Prompt,
        ..observe.clone()
    };
    let p = run("door-prompt", true, Some(prompt), false);
    assert!(p.would.is_empty(), "prompt books no would-be row");
    let status = |r: &String| r.split(' ').find(|w| !w.starts_with('@')) == Some("status");
    let unread: Vec<&String> = p.requests.iter().filter(|r| !status(r)).collect();
    assert_eq!(
        unread,
        open.requests.iter().collect::<Vec<_>>(),
        "its write read the status and typed nothing"
    );
    assert!(p.requests.len() > open.requests.len());
    let off = SupervisorConfig {
        occupancy: Occupancy::Off,
        ..observe.clone()
    };
    for (tag, hosted, policy, attended) in [
        ("door-unhosted", false, Some(observe.clone()), false),
        ("door-attended", true, Some(observe.clone()), true),
        ("door-no-policy", true, None, false),
        ("door-off", true, Some(off), false),
    ] {
        let d = run(tag, hosted, policy, attended);
        assert!(d.would.is_empty(), "{tag}: {:?}", d.would);
        assert!(verdicts(&d.noted).is_empty(), "{tag}: {:?}", d.noted);
    }
}

/// THE BOARD IS ASKED AT MOST ONCE A LOOK (F9): a break read every ten seconds for ten minutes
/// from two hours in, on a board whose machine terms never hold (so `hold-room` stays wanting),
/// asks it once a minute.
#[test]
fn the_board_is_asked_at_most_once_a_look() {
    let no_room = OccupancyReading {
        room: Some(false),
        ..room()
    };
    let mut reads = every(0, 2 * 3600 - 60, 60);
    let before = day(
        "door-f9-before",
        true,
        Some(hosted(30).policy),
        false,
        no_room.clone(),
        &reads,
        &mut nothing_before,
    );
    assert_eq!(before.asks, 0);
    reads.extend(every(2 * 3600, 2 * 3600 + 600, 10));
    let d = day(
        "door-f9",
        true,
        Some(hosted(30).policy),
        false,
        no_room,
        &reads,
        &mut nothing_before,
    );
    assert_eq!(d.asks, 11, "at 2h00, 2h01, … 2h10: once a minute");
    assert_eq!(
        d.would,
        [(NUDGE.to_string(), 30 * MIN)],
        "no room, no ask past"
    );

    let cached_room = |tag: &str, age_ms, producer_stale, missing_clock, mark_ahead| {
        let (dir, path) = ledger_at(tag);
        let queried_at = T0 + 120 * MIN;
        let host = Board::reading(OccupancyReading {
            taken_ms: queried_at - age_ms,
            stale: producer_stale,
            held_slot_last_ms: Some(queried_at - age_ms - MIN),
            ..room()
        });
        let mut m = Mock::new(Vec::new());
        {
            let (mut s, clock) = hold_session(
                &mut m,
                &path,
                Some(Arc::clone(&host) as Arc<dyn IdleHost>),
                Duration::ZERO,
            );
            let opts = hosted(30);
            let mut heard = Heard::default();
            let mut since = None;
            let mut at_ms = T0;
            for (seq, at) in [
                (1, T0),
                (2, T0 + 30 * MIN),
                (3, queried_at),
                (4, queried_at + 59_000),
            ] {
                let ahead = at - at_ms;
                skip(&clock, &mut at_ms, ahead);
                if seq < 4 {
                    s.clock_marks = Some((s.fx.mono(), at_ms));
                } else if missing_clock {
                    s.clock_marks = None;
                } else if mark_ahead {
                    // A newer mark must not subtract time or panic.
                    s.clock_marks = Some((s.fx.mono() + Duration::from_millis(1), at_ms + 1));
                }
                s.driver = seq == 3;
                s.host_steps_in_background(
                    &screen_at(seq, break_screen()),
                    &mut since,
                    Some(&opts.policy),
                    &mut heard,
                )
                .expect("the break's step");
            }
        }
        assert_eq!(host.asks(), 1, "the second judgment reused the query");
        let rows = would_rows(&path);
        let _ = std::fs::remove_dir_all(&dir);
        rows
    };
    for (tag, age_ms, producer_stale, missing_clock, mark_ahead, room_due) in [
        ("aged-119-to-178", 119_000, false, false, false, false),
        ("aged-fresh", 0, false, false, false, true),
        ("aged-edge", 61_000, false, false, false, true),
        ("aged-past-edge", 61_001, false, false, false, false),
        ("aged-producer-stale", 0, true, false, false, false),
        ("aged-missing-clock", 0, false, true, false, false),
        ("aged-newer-clock", 61_000, false, false, true, false),
    ] {
        let rows = cached_room(tag, age_ms, producer_stale, missing_clock, mark_ahead);
        let mut expected = vec![(NUDGE.to_string(), 30 * MIN)];
        if room_due {
            expected.push((ROOM.to_string(), 120 * MIN + 59_000));
        }
        assert_eq!(rows, expected, "{tag}");
    }
}

/// EACH GUARD THE LOOP HOLDS HOLDS THE DUE LINE, said once, from the loop's own state (F1): a
/// person who typed within their grace, another driver's hand, the halt, the memory wall, the
/// API's route down, an autopilot the rule is not for.
#[test]
fn each_guard_in_hand_holds_the_due_line_once() {
    let reads = [0, 31 * 60, 32 * 60, 33 * 60, 34 * 60];
    let run = |tag: &str, policy: SupervisorConfig, set: &dyn Fn(&mut Session<'_, Mock>, u64)| {
        day(
            tag,
            true,
            Some(policy),
            false,
            room(),
            &reads,
            &mut |s, seq| set(s, seq),
        )
    };
    let policy = hosted(30).policy;
    let control = run("door-guard-none", policy.clone(), &|_, _| {});
    assert_eq!(control.would, [(NUDGE.to_string(), 31 * MIN)]);
    assert_eq!(
        verdicts(&control.noted),
        [(2, NUDGE.to_string(), "would".to_string())]
    );
    assert_eq!(policy.human_grace_s, 120);
    let person = run("door-guard-person", policy.clone(), &|s, seq| {
        if seq == 2 {
            s.person = Some(s.fx.mono());
        }
    });
    assert_eq!(
        verdicts(&person.noted),
        [
            (2, NUDGE.to_string(), "held:person".to_string()),
            (4, NUDGE.to_string(), "would".to_string()),
        ]
    );
    assert_eq!(person.would, [(NUDGE.to_string(), 33 * MIN)]);
    assert!(!policy.usage_credits, "credits carry no session's turns");
    type Set = fn(&mut Session<'_, Mock>, bool);
    let flags: [(&str, &str, Set); 12] = [
        ("driver", "person", |s, on| s.driver = on),
        ("halted", "halted", |s, on| s.halted = on),
        ("memory-told", "memory", |s, on| {
            s.memory_told = on.then_some(crate::supervise::memory::Verdict::Restart);
        }),
        ("memory-seen", "memory", |s, on| {
            s.memory_seen = on.then_some(crate::supervise::memory::MemoryOver {
                footprint_mib: 40 * 1024,
                ceiling_mib: 32 * 1024,
                available_mib: Some(512),
                strain: None,
                dominant: true,
            });
        }),
        ("reach", "reach", |s, on| {
            s.reach_seen = on.then(|| Reach::Down { since: s.fx.mono() });
        }),
        ("stall", "stall", |s, on| {
            s.stalled = on.then_some(crate::supervise::run::stall::WorkerStall {
                word: "stalled",
                bytes: Some(3),
                wait_ms: Some(90_000),
                rss_mb: None,
            });
        }),
        ("limit", "limit-episode", |s, on| {
            s.limit = on.then(|| Episode {
                seq: 1,
                resumes_itself: false,
                reset_unix: None,
                reset_text: "-".to_string(),
                extended: false,
                raised: true,
            });
        }),
        ("act-untaken", "act-untaken", |s, on| {
            if !on {
                s.turn_end = TurnEndState::default();
            } else if !s.turn_end.act_untaken() {
                let reading = s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
                s.turn_end.acted(
                    &TurnEndAction::Type {
                        text: "go on".to_string(),
                        rule_id: RULE_CONTINUE,
                    },
                    &reading,
                    s.fx.mono(),
                );
                assert!(s.turn_end.act_untaken());
            }
        }),
        ("wake-settle", "wake-settle", |s, on| {
            s.wake_settle = on.then(|| {
                let at = s.fx.mono() + Duration::from_secs(30);
                let due = Due {
                    at,
                    unix_ms: T0,
                    why: "the Mac woke".to_string(),
                    wait: WaitKind::Other,
                };
                (due, false)
            });
        }),
        ("credits-kept", "credits", |s, on| {
            s.credits_kept = on.then(|| CreditsKept {
                reset: None,
                since: s.fx.mono(),
                model: None,
            });
        }),
        ("exchange", "exchange", |s, on| {
            if !on {
                s.turn_end = TurnEndState::default();
            } else if !s.turn_end.left_to_person() {
                let mut reading =
                    s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
                reading.left_record = true;
                s.turn_end.observe(&reading, s.fx.mono());
                assert!(s.turn_end.left_to_person());
            }
        }),
        ("draft", "draft", |s, on| {
            // First clocked at the first read, so past the person's grace by 31 min.
            if !on {
                s.draft_seen = None;
            } else if s.draft_seen.is_none() {
                s.draft_seen = Some(("and also the docs".to_string(), s.fx.mono()));
            }
        }),
    ];
    for (tag, word, set) in flags {
        let d = run(&format!("door-guard-{tag}"), policy.clone(), &|s, seq| {
            set(s, seq < 4);
        });
        assert_eq!(
            verdicts(&d.noted),
            [
                (2, NUDGE.to_string(), format!("held:{word}")),
                (4, NUDGE.to_string(), "would".to_string()),
            ],
            "{tag}"
        );
        assert_eq!(d.would, [(NUDGE.to_string(), 33 * MIN)], "{tag}");
    }
    type Drawn = fn() -> Vec<String>;
    let drawn: [(&str, &str, Drawn); 2] = [
        ("credits", "credits", credits_break),
        ("taskless", "taskless", taskless_break),
    ];
    for (tag, word, screen) in drawn {
        let rows = screen();
        assert!(
            aterm_phase::phase::background_wait(&rows).is_some(),
            "{tag}: a break"
        );
        let d = day_on(
            &format!("door-guard-{tag}-drawn"),
            true,
            Some(policy.clone()),
            false,
            room(),
            &reads,
            &mut |_, _| {},
            &|seq| if seq < 4 { screen() } else { break_screen() },
        );
        assert_eq!(
            verdicts(&d.noted),
            [
                (2, NUDGE.to_string(), format!("held:{word}")),
                (4, NUDGE.to_string(), "would".to_string()),
            ],
            "{tag}"
        );
        assert_eq!(d.would, [(NUDGE.to_string(), 33 * MIN)], "{tag}");
    }
    let land = SupervisorConfig {
        autopilot: Autopilot::Land,
        ..policy
    };
    let d = run("door-guard-land", land, &|_, _| {});
    assert_eq!(
        verdicts(&d.noted),
        [(2, NUDGE.to_string(), "held:autopilot".to_string())]
    );
    assert!(d.would.is_empty());
}

/// A CARRIED BREAK IS NO DOOR (F10): a read the session's transcript carried as a break
/// (BG-1, `transcript_wait_seq`), which skips the screen's own break test, judges nothing; the
/// next drawn read would.
#[test]
fn a_carried_break_judges_nothing() {
    let reads = [0, 31 * 60, 32 * 60];
    let policy = hosted(30).policy;
    let carried = day(
        "door-carried",
        true,
        Some(policy.clone()),
        false,
        room(),
        &reads,
        &mut |s, seq| {
            s.transcript_wait_seq = (seq == 2).then_some(2);
        },
    );
    assert_eq!(carried.would, [(NUDGE.to_string(), 32 * MIN)]);
    let drawn = day(
        "door-drawn",
        true,
        Some(policy),
        false,
        room(),
        &reads,
        &mut nothing_before,
    );
    assert_eq!(drawn.would, [(NUDGE.to_string(), 31 * MIN)]);
}

/// A would-be line a loop booked, as a `fact-would@v1` row stamped `ts`.
fn would_row(rule: RuleId, ts: i64, held_ms: i64) -> String {
    let reason = crate::supervise::occupancy::rules::would_reason(rule, 1, held_ms, false);
    approvals::Row {
        rule_id: WOULD_RULE,
        outcome: approvals::Outcome::Waiting,
        command: "",
        reason: &reason,
        box_seq: 1,
    }
    .to_json(ts, Some("@s-1"))
}

/// What a successor makes of the ledger text `body`, started with its wall clock at `now_ms`:
/// it reads a break at once and again `later_ms` on.
fn successor_door(body: &str, now_ms: i64, later_ms: i64) -> (Vec<(String, i64)>, Vec<String>) {
    let s = successor_run(body, now_ms, later_ms, Duration::ZERO);
    let words = s.verdicts.into_iter().map(|(_, _, w)| w).collect();
    (s.would, words)
}

/// What a successor did ([`successor_run`]).
struct Successor {
    /// Its would-be rows.
    would: Vec<(String, i64)>,
    /// Its rules' verdicts, each `(seq, rule, word)`.
    verdicts: Vec<(u64, String, String)>,
    /// The episode each of its would-be rows names (`episode=<hex>`), in order.
    episodes: Vec<String>,
}

/// [`successor_door`] under the break's settle `settle`: the first read is the break's first,
/// inside any settle.
fn successor_run(body: &str, now_ms: i64, later_ms: i64, settle: Duration) -> Successor {
    let (dir, path) = ledger_at("door-successor");
    std::fs::write(&path, body).expect("the ledger carried");
    let before = would_rows(&path).len();
    let mut m = Mock::new(Vec::new());
    let noted = {
        let host = Arc::new(Launched(None)) as Arc<dyn IdleHost>;
        let (mut s, clock) = hold_session(&mut m, &path, Some(host), settle);
        clock.set_at_ms(now_ms);
        let opts = hosted(30);
        let cfg = Some(&opts.policy);
        let mut heard = Heard::default();
        let start = s.fx.mono();
        s.hold_seed_from(body, start, now_ms, false, &opts.policy, &heard);
        let mut since = None;
        let mut at_ms = now_ms;
        s.host_steps_in_background(&screen_at(1, break_screen()), &mut since, cfg, &mut heard)
            .expect("the break's step");
        skip(&clock, &mut at_ms, later_ms);
        s.host_steps_in_background(&screen_at(2, break_screen()), &mut since, cfg, &mut heard)
            .expect("the break's step");
        heard.noted
    };
    let would = would_rows(&path).split_off(before);
    let episodes = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(WOULD_RULE))
        .skip(before)
        .filter_map(|line| {
            let v = aterm_json::from_str::<aterm_json::Value>(line).ok()?;
            let reason = v.get("reason")?.as_str()?.to_string();
            let episode = reason.split(' ').find_map(|w| w.strip_prefix("episode="))?;
            Some(episode.to_string())
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    Successor {
        would,
        verdicts: verdicts(&noted),
        episodes,
    }
}

/// A predecessor's open `fact-hold@v1` row: the hold had stood `held_ms` at `row_ms`.
fn hold_row(row_ms: i64, held_ms: i64) -> String {
    use crate::supervise::occupancy::hold::{RowSaid, row_reason};
    let reason = row_reason(&RowSaid::Open {
        held_ms,
        declared: false,
    });
    approvals::Row {
        rule_id: crate::supervise::facts::FactKind::Hold.rule(),
        outcome: approvals::Outcome::Waiting,
        command: "",
        reason: &reason,
        box_seq: 1,
    }
    .to_json(row_ms, Some("@s-1"))
}

/// A HOLD CARRIED PAST A DAY KEEPS ITS ONE NUDGE (the review of 1b's F1): a successor carries a
/// hold that has stood 30 h, nudged 29.5 h ago by the loop that watched it then; that nudge is
/// this hold's, not the day's, and none would go again.
#[test]
fn a_hold_carried_past_a_day_keeps_its_nudge() {
    let held = 30 * 60 * MIN;
    let row_ms = T0 + held;
    let since_wall = row_ms - held;
    let open = hold_row(row_ms, held);
    let row = crate::supervise::occupancy::hold::seed_row_in(&format!("{open}\n"), Some("@s-1"))
        .expect("the hold row is open");
    assert_eq!(row.since_wall_ms(), since_wall);
    let nudged = format!(
        "{}\n{open}\n",
        would_row(RuleId::WaitNudge, since_wall + 30 * MIN, 30 * MIN)
    );
    let (rows, words) = successor_door(&nudged, row_ms + 4 * MIN, MIN);
    assert!(rows.is_empty(), "nudged again in the same hold: {rows:?}");
    assert!(words.is_empty(), "{words:?}");
    let (rows, _) = successor_door(&format!("{open}\n"), row_ms + 4 * MIN, MIN);
    assert_eq!(rows, [(NUDGE.to_string(), held + 4 * MIN)], "the control");
}

/// THE DOOR WAITS FOR THE BREAK'S SETTLE (the review of 1b's F5): a successor carries a hold
/// that is due its nudge at once, but its first read is the break's first, inside the settle;
/// the read past it would.
#[test]
fn the_door_waits_for_the_breaks_settle() {
    let held = 40 * MIN;
    let row_ms = T0 + held;
    let body = format!("{}\n", hold_row(row_ms, held));
    let settle = crate::supervise::run::background_settle();
    assert!(settle > Duration::ZERO);
    let later = i64::try_from(settle.as_millis()).expect("a short settle");
    let settled = successor_run(&body, row_ms + MIN, later, settle);
    assert_eq!(
        settled.verdicts,
        [(2, NUDGE.to_string(), "would".to_string())],
        "nothing inside the settle"
    );
    assert_eq!(settled.would, [(NUDGE.to_string(), held + MIN + later)]);
    let at_once = successor_run(&body, row_ms + MIN, later, Duration::ZERO);
    assert_eq!(
        at_once.verdicts,
        [(1, NUDGE.to_string(), "would".to_string())]
    );
}

/// EACH HOLD SAYS ITS OWN VERDICTS (the review of 1b's F6): another driver's hand on the session
/// all along holds the nudge in one hold, said once; the agent's own wake ends it, and the next
/// hold, held the same way, says it again, though the door's first break in it is already past
/// the nudge's half hour (the hold opened at a break the transcript carried, which the door
/// skips, F10), so its verdict there is the last hold's last.
#[test]
fn each_hold_says_its_own_verdicts() {
    let (dir, path) = ledger_at("door-two-holds");
    let mut m = Mock::new(Vec::new());
    let host = Board::reading(room());
    let noted = {
        let (mut s, clock) = hold_session(
            &mut m,
            &path,
            Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            Duration::ZERO,
        );
        s.driver = true;
        let opts = hosted(30);
        let cfg = Some(&opts.policy);
        let mut heard = Heard::default();
        let mut at_ms = T0;
        let mut since = None;
        for (seq, at) in [(1, 0), (2, 31 * MIN), (3, 32 * MIN), (4, 33 * MIN)] {
            let ahead = T0 + at - at_ms;
            skip(&clock, &mut at_ms, ahead);
            s.host_steps_in_background(
                &screen_at(seq, break_screen()),
                &mut since,
                cfg,
                &mut heard,
            )
            .expect("the break's step");
        }
        s.took(5, cfg, &mut heard);
        since = None;
        s.transcript_wait_seq = Some(6);
        for (seq, at) in [(6, 34 * MIN), (7, 65 * MIN), (8, 66 * MIN)] {
            let ahead = T0 + at - at_ms;
            skip(&clock, &mut at_ms, ahead);
            s.host_steps_in_background(
                &screen_at(seq, break_screen()),
                &mut since,
                cfg,
                &mut heard,
            )
            .expect("the break's step");
        }
        heard.noted
    };
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        verdicts(&noted),
        [
            (2, NUDGE.to_string(), "held:person".to_string()),
            (7, NUDGE.to_string(), "held:person".to_string()),
        ]
    );
}

/// A CARRIED HOLD'S EPISODES ARE KEYED BY ITS START ON THE WALL, as the predecessor's row puts
/// it (its stamp less how long the hold had stood), never by this loop's own clock: two
/// successors that carry the same row at different times name the same episode, the one
/// `rules::episode` makes of that start.
#[test]
fn a_carried_holds_episode_is_keyed_by_its_start_on_the_wall() {
    let held = 40 * MIN;
    let row_ms = T0 + held;
    let body = format!("{}\n", hold_row(row_ms, held));
    let first = successor_run(&body, row_ms + MIN, MIN, Duration::ZERO);
    let second = successor_run(&body, row_ms + 3 * MIN, MIN, Duration::ZERO);
    let want = format!(
        "{:016x}",
        crate::supervise::occupancy::rules::episode(RuleId::WaitNudge, T0, 0)
    );
    assert_eq!(first.episodes, [want.as_str()]);
    assert_eq!(second.episodes, [want.as_str()]);
    let later = format!("{}\n", hold_row(row_ms, held - MIN));
    let other = successor_run(&later, row_ms + MIN, MIN, Duration::ZERO);
    assert_eq!(other.would.len(), 1, "{:?}", other.would);
    assert_ne!(other.episodes, [want]);
}

/// A SUCCESSOR SEEDS THE DOOR'S BOOKS FROM THE WOULD-BE ROWS: a carried hold whose nudge a loop
/// before booked gets none again, and three nudges booked within the day spend the cap on a new
/// hold.
#[test]
fn a_successor_seeds_the_books_from_the_would_rows() {
    let body = predecessor("door-seed", false, false);
    let row = crate::supervise::occupancy::hold::seed_row_in(&body, Some("@s-1"))
        .expect("the booked hold is open");
    let soon = row.row_ms + 4 * MIN;
    let (rows, _) = successor_door(&body, soon, 20 * MIN);
    assert_eq!(rows, [(NUDGE.to_string(), 35 * MIN)], "the control");
    let nudged = format!(
        "{body}{}\n",
        would_row(RuleId::WaitNudge, row.row_ms, 11 * MIN)
    );
    let (rows, words) = successor_door(&nudged, soon, 20 * MIN);
    assert!(rows.is_empty(), "nudged in this hold already: {rows:?}");
    assert!(words.is_empty(), "{words:?}");
    let now = row.row_ms + 4 * MIN;
    let three: String = (1..=3)
        .map(|k| {
            format!(
                "{}\n",
                would_row(RuleId::WaitNudge, now - k * 3 * 60 * MIN, 30 * MIN)
            )
        })
        .collect();
    let (rows, words) = successor_door(&three, now, 31 * MIN);
    assert!(rows.is_empty(), "{rows:?}");
    assert_eq!(words, ["spent"]);
    let old: String = (1..=3)
        .map(|k| {
            format!(
                "{}\n",
                would_row(RuleId::WaitNudge, now - 24 * 60 * MIN - k * MIN, 30 * MIN)
            )
        })
        .collect();
    let (rows, _) = successor_door(&old, now, 31 * MIN);
    assert_eq!(rows, [(NUDGE.to_string(), 31 * MIN)], "a day old: free");
}

/// A TAPE FROM A BUILD WITH NO OCCUPANCY DOOR REPLAYS WITHOUT IT (the red-team's F8, through
/// the replay's own entry: the review of 1b's F4).
#[test]
fn a_tape_from_a_build_with_no_occupancy_door_replays_without_it() {
    use crate::supervise::tape::codec::Item;
    use crate::supervise::tape::recover::recover;
    use crate::supervise::tape::replay::{Options, replay};
    use crate::supervise::tape::report::Verdict;
    use crate::supervise::tape::testing::{assert_replays, written};

    use crate::supervise::tape::doors;
    use astream_effects::EffectRecord;

    let (dir, ledger) = ledger_at("door-pre-door-tape");
    let board = Board::reading(OccupancyReading::default());
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&board) as Arc<dyn IdleHost>),
        ledger: Some(ledger.clone()),
        journal: Some(dir.join("journal.jsonl")),
        ..hosted(2 * 3600 + 5 * 60)
    };
    assert_eq!(opts.policy.occupancy, Occupancy::Observe);
    let mut m = Mock::new(vec![break_screen()]);
    m.stall_sleep = Some(Duration::from_secs(60));
    let run = record_watch(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    let asks = board.asks();
    assert!(asks > 0, "the door asked the board from 2 h");
    let would = would_rows(&ledger);
    assert_eq!(would.len(), 1, "the live empty board still permits a nudge");
    assert_eq!(would[0].0, NUDGE);
    assert!(would[0].1 >= 30 * MIN);
    assert_replays(&run.tape, &run.header, &run.sent, &run.lines);
    let header = run.header.encode();
    let items = recover(&run.tape.reframed(&header)).items;
    let is_board = |i: &Item| {
        matches!(i.record(), Some(astream_effects::EffectRecord::Exchange { door, .. })
            if door == "host.occupancy")
    };
    assert_eq!(items.iter().filter(|i| is_board(i)).count(), asks);
    let without_board: Vec<Item> = items.iter().filter(|i| !is_board(i)).cloned().collect();
    let report = replay(&written(&header, &without_board), &Options::default())
        .expect("replayed without the board records");
    assert!(
        matches!(report.verdict, Verdict::Diverged(_)),
        "{}",
        report.render()
    );

    let new_rule_output = |item: &Item| -> Option<&'static str> {
        let Some(EffectRecord::Emit { door, bytes }) = item.record() else {
            return None;
        };
        if door == doors::JOURNAL {
            let row = JournalRecord::from_json(std::str::from_utf8(bytes).expect("journal UTF-8"))
                .expect("journal row");
            (row.kind == "occupancy" && RuleId::of_name(&row.phase).is_some()).then_some("journal")
        } else if door == doors::LEDGER {
            let row: aterm_json::Value =
                aterm_json::from_str(std::str::from_utf8(bytes).expect("ledger UTF-8"))
                    .expect("ledger row");
            (row.get("rule_id").and_then(aterm_json::Value::as_str) == Some(WOULD_RULE))
                .then_some("ledger")
        } else {
            None
        }
    };
    // What the door tells its host (1e's `host.occupancy_said`) is the door's too: an emit of
    // its own, with no stamp.
    let is_said = |i: &Item| matches!(i.record(), Some(EffectRecord::Emit { door, .. }) if door == "host.occupancy_said");
    let mut removed = vec![false; items.len()];
    let (mut journals, mut ledgers, mut saids) = (0, 0, 0);
    for (at, item) in items.iter().enumerate() {
        if is_board(item) {
            removed[at] = true;
        } else if is_said(item) {
            removed[at] = true;
            saids += 1;
        } else if let Some(kind) = new_rule_output(item) {
            let stamp = at.checked_sub(1).expect("a rule row follows its stamp");
            assert!(
                matches!(items[stamp].record(), Some(EffectRecord::Clock(_))),
                "the exact immediately preceding stamp: {:?}",
                items[stamp]
            );
            assert!(!removed[stamp], "each rule row owns a different stamp");
            removed[stamp] = true;
            removed[at] = true;
            match kind {
                "journal" => journals += 1,
                "ledger" => ledgers += 1,
                _ => unreachable!(),
            }
        }
    }
    assert_eq!(journals, 1, "the nudge's journal row");
    assert_eq!(ledgers, 1, "the nudge's would row");
    assert!(saids >= 1, "the door told its host");
    assert_eq!(
        removed.iter().filter(|gone| **gone).count(),
        asks + saids + 2 * (journals + ledgers),
        "only board exchanges, the host's tells and identified row/stamp pairs"
    );
    let before_door: Vec<Item> = items
        .iter()
        .zip(&removed)
        .filter(|(_, gone)| !**gone)
        .map(|(item, _)| item.clone())
        .collect();
    assert!(
        before_door.iter().any(|item| {
            let Some(EffectRecord::Emit { door, bytes }) = item.record() else {
                return false;
            };
            door == doors::LEDGER
                && aterm_json::from_str::<aterm_json::Value>(
                    std::str::from_utf8(bytes).expect("ledger UTF-8"),
                )
                .expect("ledger row")
                .get("rule_id")
                .and_then(aterm_json::Value::as_str)
                    == Some(crate::supervise::facts::FactKind::Hold.rule())
        }),
        "the pre-existing hold clock's ledger outputs remain"
    );
    let older: String = header
        .lines()
        .filter(|line| !line.starts_with("host_doors="))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_ne!(older, header, "the header named the doors");
    let report = replay(&written(&older, &before_door), &Options::default()).expect("replayed");
    assert_eq!(report.verdict, Verdict::Reproduced, "{}", report.render());
    let report = replay(&written(&header, &before_door), &Options::default()).expect("replayed");
    assert!(
        matches!(report.verdict, Verdict::Diverged(_)),
        "{}",
        report.render()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `prompt` TAPE FROM A BUILD WITH NO WRITE REPLAYS AS IT WAS RECORDED (the safety review's
/// F-B): a build with no `host.occupancy_pace` door (1b's) read `prompt` as `observe`, so its due
/// line is the would-be row it booked, and no `status` read is asked of the tape. The fixture is
/// a hosted watch recorded under `observe` whose header then says `prompt` and names no door the
/// write added. NEGATIVE CONTROL: the same header naming this build's doors replays the write,
/// whose `status` read the tape does not hold: DIVERGED.
#[test]
fn a_prompt_tape_from_a_build_with_no_write_replays_as_observe() {
    use crate::supervise::tape::recover::recover;
    use crate::supervise::tape::replay::{Options, replay};
    use crate::supervise::tape::report::Verdict;
    use crate::supervise::tape::testing::written;

    let (dir, ledger) = ledger_at("door-pre-write-tape");
    let board = Board::reading(OccupancyReading::default());
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&board) as Arc<dyn IdleHost>),
        ledger: Some(ledger.clone()),
        journal: Some(dir.join("journal.jsonl")),
        ..hosted(35 * 60)
    };
    assert_eq!(opts.policy.occupancy, Occupancy::Observe);
    let mut m = Mock::new(vec![break_screen()]);
    m.stall_sleep = Some(Duration::from_secs(60));
    let run = record_watch(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert_eq!(would_rows(&ledger).len(), 1, "the nudge's would-be row");
    let header = run.header.encode();
    let items = recover(&run.tape.reframed(&header)).items;
    let replayed = |header: &str| {
        replay(&written(header, &items), &Options::default())
            .expect("replayed")
            .verdict
    };
    assert_eq!(replayed(&header), Verdict::Reproduced);
    let write_doors = [
        "host.occupancy_pace",
        "host.upgrade_due",
        "host.break_taken",
    ];
    let rewritten = |strip: bool| -> String {
        header
            .lines()
            .map(|line| {
                let line = if line == "occupancy=observe" {
                    "occupancy=prompt".to_string()
                } else if let Some(doors) = line.strip_prefix("host_doors=")
                    && strip
                {
                    let kept: Vec<&str> = doors
                        .split(' ')
                        .filter(|d| !write_doors.contains(d))
                        .collect();
                    format!("host_doors={}", kept.join(" "))
                } else {
                    line.to_string()
                };
                format!("{line}\n")
            })
            .collect()
    };
    let older = rewritten(true);
    assert!(older.lines().any(|l| l == "occupancy=prompt"), "{older}");
    let doors: Vec<&str> = older
        .lines()
        .find_map(|l| l.strip_prefix("host_doors="))
        .expect("the header names its doors")
        .split(' ')
        .collect();
    assert!(
        doors.contains(&"host.occupancy") && !doors.contains(&"host.occupancy_pace"),
        "{doors:?}"
    );
    assert_eq!(replayed(&older), Verdict::Reproduced);
    assert!(
        matches!(replayed(&rewritten(false)), Verdict::Diverged(_)),
        "this build's doors under prompt: the write"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A TAPE FROM A BUILD WITH THE DOOR BUT NO TELL REPLAYS WITHOUT THE TELLS (the tests review's
/// F1): a 1b–1d build named `host.occupancy` among its doors but not `host.occupancy_said`, and
/// told its host nothing, so a replay of its tape tells nothing either — not a break's word,
/// not a close's, not the loop end's. NEGATIVE CONTROL: the same records under a header that
/// names the tell's door are this build's, whose tells the records lack: DIVERGED.
#[test]
fn a_tape_from_a_build_with_no_tell_replays_without_the_tells() {
    use crate::supervise::tape::codec::Item;
    use crate::supervise::tape::recover::recover;
    use crate::supervise::tape::replay::{Options, replay};
    use crate::supervise::tape::report::Verdict;
    use crate::supervise::tape::testing::written;
    use astream_effects::EffectRecord;

    let (dir, ledger) = ledger_at("door-pre-said-tape");
    let board = Board::reading(OccupancyReading::default());
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&board) as Arc<dyn IdleHost>),
        ledger: Some(ledger.clone()),
        journal: Some(dir.join("journal.jsonl")),
        ..hosted(35 * 60)
    };
    let mut m = Mock::new(vec![break_screen()]);
    m.stall_sleep = Some(Duration::from_secs(60));
    let run = record_watch(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    let header = run.header.encode();
    let items = recover(&run.tape.reframed(&header)).items;
    let is_said = |i: &Item| {
        matches!(i.record(), Some(EffectRecord::Emit { door, .. })
            if door == "host.occupancy_said")
    };
    assert!(items.iter().any(is_said), "this build told its host");
    let kept: Vec<Item> = items.iter().filter(|i| !is_said(i)).cloned().collect();
    let older: String = header
        .lines()
        .map(|line| match line.strip_prefix("host_doors=") {
            Some(doors) => format!(
                "host_doors={}\n",
                doors
                    .split(' ')
                    .filter(|d| *d != "host.occupancy_said")
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            None => format!("{line}\n"),
        })
        .collect();
    let doors: Vec<&str> = older
        .lines()
        .find_map(|l| l.strip_prefix("host_doors="))
        .expect("the header names its doors")
        .split(' ')
        .collect();
    assert!(
        doors.contains(&"host.occupancy") && !doors.contains(&"host.occupancy_said"),
        "{doors:?}"
    );
    let report = replay(&written(&older, &kept), &Options::default()).expect("replayed");
    assert_eq!(report.verdict, Verdict::Reproduced, "{}", report.render());
    let report = replay(&written(&header, &kept), &Options::default()).expect("replayed");
    assert!(
        matches!(report.verdict, Verdict::Diverged(_)),
        "{}",
        report.render()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A CLOSED HOLD IS TOLD (the tests review's F3): the agent's own wake ends the hold the door
/// told its host, and the host is told it closed, so `occupancy_hold=` and `occupancy why` no
/// longer show it. NEGATIVE CONTROL: the word told before the close names the hold.
#[test]
fn a_closed_hold_is_told_to_the_host() {
    let (dir, path) = ledger_at("door-close-told");
    let mut m = Mock::new(Vec::new());
    let host = Board::reading(room());
    let before = {
        let (mut s, clock) = hold_session(
            &mut m,
            &path,
            Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            Duration::ZERO,
        );
        let opts = hosted(30);
        let cfg = Some(&opts.policy);
        let mut heard = Heard::default();
        let mut at_ms = T0;
        let mut since = None;
        for (seq, at) in [(1, 0), (2, 31 * MIN)] {
            let ahead = T0 + at - at_ms;
            skip(&clock, &mut at_ms, ahead);
            s.host_steps_in_background(
                &screen_at(seq, break_screen()),
                &mut since,
                cfg,
                &mut heard,
            )
            .expect("the break's step");
        }
        let before = host.said();
        s.took(3, cfg, &mut heard);
        before
    };
    let _ = std::fs::remove_dir_all(&dir);
    let told = before.last().expect("the door told its host");
    assert_eq!(told.hold_word(), "undeclared", "{before:#?}");
    let said = host.said();
    assert_eq!(
        said.len(),
        before.len() + 1,
        "the close, told once: {said:#?}"
    );
    let closed = said.last().expect("told");
    assert_eq!(closed.hold, None, "{said:#?}");
    assert_eq!(closed.hold_word(), "none");
    assert!(!closed.ready);
}

/// THE LOOP'S END IS TOLD (the safety review's F2): a hosted watch that ends with its hold open
/// — as a policy whose occupancy moved ends a tab's loop, handing it to a successor that under
/// `off` tells nothing — tells its host no hold, once, so the window's board counts none in
/// `occupancy_hold=` or `ready=`; the tape replays it. NEGATIVE CONTROL: the word before it
/// names the open hold.
#[test]
fn a_loop_that_ends_tells_its_host_no_hold() {
    use crate::supervise::tape::testing::assert_replays;

    let (dir, ledger) = ledger_at("door-end-told");
    let board = Board::reading(OccupancyReading::default());
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&board) as Arc<dyn IdleHost>),
        ledger: Some(ledger.clone()),
        journal: Some(dir.join("journal.jsonl")),
        ..hosted(35 * 60)
    };
    let mut m = Mock::new(vec![break_screen()]);
    m.stall_sleep = Some(Duration::from_secs(60));
    let run = record_watch(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    let said = board.said();
    let (last, before) = said.split_last().expect("the door told its host");
    assert_eq!(last.hold, None, "{said:#?}");
    assert!(!last.ready && last.guards.is_none(), "{last:#?}");
    assert!(
        before.last().and_then(|s| s.hold).is_some(),
        "the open hold was told: {said:#?}"
    );
    assert_eq!(
        said.iter().filter(|s| s.hold.is_none()).count(),
        1,
        "told once: {said:#?}"
    );
    assert_replays(&run.tape, &run.header, &run.sent, &run.lines);
    let _ = std::fs::remove_dir_all(&dir);
}
