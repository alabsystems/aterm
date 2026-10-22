// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OCCUPANCY DOOR IN THE LOOP (`run/occupancy_loop.rs`'s `occupancy_break`):
//! what `wait-nudge@v1` and `hold-room@v1` would do at the breaks of a held
//! session, booked as `fact-would@v1` rows and said once a change — on hosted, unattended loops
//! under a policy whose occupancy is not `off` alone (F2), with guards judged from the loop's own
//! state and no read made for them (F1), the board asked at most once a look (F9), and nothing
//! ever typed. Mounted from `occupancy_loop_tests.rs`, so its sessions, hosts and readers are in
//! scope. Asserts on ledger rows and on the `OCCUPANCY` records' structure (kind, phase, the
//! verdict's word), never on prose (F17); a day is scripted reads that jump the clock (F18).

use std::sync::atomic::AtomicUsize;

use super::*;
use crate::supervise::config::Autopilot;
use crate::supervise::occupancy::rules::{RuleId, WOULD_RULE};
use crate::supervise::occupancy::{OccupancyReading, SlotsSeen};
use crate::supervise::policy::turn_end::Reach;

/// A host whose board reads `reading`, counting the asks.
#[derive(Debug, Default)]
struct Board {
    reading: std::sync::Mutex<OccupancyReading>,
    asks: AtomicUsize,
}

impl Board {
    fn reading(reading: OccupancyReading) -> Arc<Self> {
        Arc::new(Self {
            reading: std::sync::Mutex::new(reading),
            asks: AtomicUsize::new(0),
        })
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
            before(&mut s, seq);
            s.host_steps_in_background(&screen_at(seq, drawn(seq)), &mut since, cfg, review);
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
    }
}

/// Every `step_s` seconds from `from_s` to `to_s`, both included.
fn every(from_s: i64, to_s: i64, step_s: usize) -> Vec<i64> {
    (from_s..=to_s).step_by(step_s).collect()
}

fn nothing_before(_: &mut Session<'_, Mock>, _: u64) {}

/// T-OBS, THE F1 DAY UNDER OBSERVE: a session held six hours on its own background work, read
/// every five minutes, on a board with room from the start. `wait-nudge` would go at 30 min,
/// `hold-room` at 2 h and every two hours after — booked as `fact-would@v1` rows that type
/// nothing — and the session made exactly the requests it makes under `off`: no `turn`, `send`
/// or key, no `status`. The board was asked only where `hold-room` could be due (F9).
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

/// THE DOOR RUNS ON HOSTED, UNATTENDED LOOPS UNDER A POLICY WHOSE OCCUPANCY IS NOT OFF ALONE (F2):
/// the same 40 minutes that would nudge under observe book and say nothing with no host, with a
/// person attending, with no policy, or under `off`. `prompt` is `observe` until 1d's write path.
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
    assert_eq!(
        p.would, open.would,
        "prompt books as observe: no write path"
    );
    assert_eq!(p.requests, open.requests, "and types nothing");
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
/// asks it once a minute. NEGATIVE CONTROL: the two hours before, read every minute, ask it
/// nothing: `hold-room` could not be due.
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
}

/// EACH GUARD THE LOOP HOLDS HOLDS THE DUE LINE, said once, from the loop's own state (F1): a
/// person who typed within their grace, another driver's hand, the halt, the memory wall, the
/// API's route down, an autopilot the rule is not for. The line would go at the first read the
/// guard no longer holds. NEGATIVE CONTROL: the same reads with nothing set would at 31 min.
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
    // A person typed at the read at 31 min: held through their grace (`human_grace_s`, 120 s:
    // the read at 32 min too, said once), then would at 33 min, the grace spent.
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
    // Each flag the loop keeps, set until the read at 33 min: its tag, the guard's word, and
    // the flag. Every brake, the person's exchange, both memory readings and the draft each
    // read was clocked with (the reviews of 1b: a brake dropped from the door's table went
    // unseen).
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
                // A point an earlier loop left to the person (its row read back).
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
    // What the break's own screen says, until the read at 33 min: a credits notice drawn
    // beside the composer, and a launch card with nothing asked under it.
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
    // Land: the nudge is Cruise's.
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
/// next drawn read would. NEGATIVE CONTROL: the same read drawn would at once.
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
/// it reads a break at once and again `later_ms` on. Its would-be rows and its rules' verdicts.
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
        s.host_steps_in_background(&screen_at(1, break_screen()), &mut since, cfg, &mut heard);
        skip(&clock, &mut at_ms, later_ms);
        s.host_steps_in_background(&screen_at(2, break_screen()), &mut since, cfg, &mut heard);
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
/// this hold's, not the day's, and none would go again. NEGATIVE CONTROL: the same hold with no
/// nudge booked would be nudged at once.
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
/// the read past it would. NEGATIVE CONTROL: with no settle the first read would.
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
/// skips, F10), so its verdict there is the last hold's last. NEGATIVE CONTROL: within one hold
/// the same verdict is said once however often it is read.
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
        // The first hold: read at 0, 31, 32 and 33 min.
        for (seq, at) in [(1, 0), (2, 31 * MIN), (3, 32 * MIN), (4, 33 * MIN)] {
            let ahead = T0 + at - at_ms;
            skip(&clock, &mut at_ms, ahead);
            s.host_steps_in_background(
                &screen_at(seq, break_screen()),
                &mut since,
                cfg,
                &mut heard,
            );
        }
        // The agent's own wake ends it; a break the transcript carries opens the second at 34
        // min, first drawn at 65, held 31 min.
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
            );
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
/// `rules::episode` makes of that start. NEGATIVE CONTROL: a hold that began a minute later
/// names another.
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
/// hold. NEGATIVE CONTROLS: the carried hold with no such row is nudged; nudges a day old leave
/// the cap free.
#[test]
fn a_successor_seeds_the_books_from_the_would_rows() {
    let body = predecessor("door-seed", false, false);
    let row = crate::supervise::occupancy::hold::seed_row_in(&body, Some("@s-1"))
        .expect("the booked hold is open");
    let soon = row.row_ms + 4 * MIN;
    // Carried: held 15 min at the first read, 35 at the second.
    let (rows, _) = successor_door(&body, soon, 20 * MIN);
    assert_eq!(rows, [(NUDGE.to_string(), 35 * MIN)], "the control");
    let nudged = format!(
        "{body}{}\n",
        would_row(RuleId::WaitNudge, row.row_ms, 11 * MIN)
    );
    let (rows, words) = successor_door(&nudged, soon, 20 * MIN);
    assert!(rows.is_empty(), "nudged in this hold already: {rows:?}");
    assert!(words.is_empty(), "{words:?}");
    // A new hold (no hold row to carry), three nudges booked in the last day.
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
/// the replay's own entry: the review of 1b's F4). A hosted watch held on a break past two
/// hours, on an empty board, still books its machine-independent nudge at 30 min and asks
/// the board once a look from 2 h: its current tape replays strictly and in acts mode.
/// A pre-door fixture keeps the old hold effects and every unrelated record, removing
/// only the positively identified new rule outputs with their own immediately preceding
/// stamps, and the board exchanges. Without `host_doors` it replays REPRODUCED; the whole
/// new rule observer must be absent, not only the board's answer. NEGATIVE CONTROLS: the
/// current header with either just the board exchanges or all new rule effects missing
/// DIVERGES, so a current build never silently loses records.
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
    // Each wait on the sitting break is skipped whole on the run's clock, up to a minute.
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

    // These two output paths each read their own stamp immediately before
    // emitting: Lines::note -> Sink::record, and Session::ledger_row. By
    // supplying both private files above, neither stamp can be an unlabelled
    // clock read. Only a positively identified 1b row and that exact stamp
    // are removed; 1a hold rows, other clocks, notes and marks stay verbatim.
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
    let mut removed = vec![false; items.len()];
    let (mut journals, mut ledgers) = (0, 0);
    for (at, item) in items.iter().enumerate() {
        if is_board(item) {
            removed[at] = true;
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
    assert_eq!(
        removed.iter().filter(|gone| **gone).count(),
        asks + 2 * (journals + ledgers),
        "only board exchanges and identified row/stamp pairs"
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
