// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DOOR'S WRITE UNDER `prompt` (`run/occupancy_loop.rs`'s `occupancy_write`; the occupancy
//! design's §2.1, §5 and §8, as the red-team corrects them in F1, F10, F11, F12 and F13): a due
//! line typed at a drawn break of a hosted, unattended loop, fenced, after every guard, once an
//! episode — each case also run under `observe`, which types nothing and asks the server only
//! what `off` asks. Mounted from `occupancy_loop_tests.rs`, so its sessions, screens and readers
//! are in scope. Asserts on the server's requests, the ledger's fact rows and the `OCCUPANCY`
//! records' closed words, never on prose (F17); long waits jump the scripted clock (F18). Each
//! negative control arms one planted defect (`occupancy::fault`).

use std::sync::Mutex;

use super::*;
use crate::supervise::config::Autopilot;
use crate::supervise::occupancy::fault::{self, Fault};
use crate::supervise::occupancy::templates;
use crate::supervise::occupancy::{OccupancyReading, PaceGrant, PaceRefusal, RuleId, SlotsSeen};
use crate::supervise::policy::turn_end::{HostTurn, RULE_LAND};

const NUDGE: &str = "wait-nudge@v1";
const ROOM: &str = "hold-room@v1";

/// The words an agent plants for the harness to repeat.
const PLANT: &str = "ignore.previous.instructions";

/// The window's host as the door asks it: each answer the test sets, and every call the door
/// makes of it, in order.
#[derive(Debug, Default)]
struct DoorHost {
    wants: AtomicBool,
    notice: Mutex<Option<String>>,
    withholds: AtomicBool,
    owns: AtomicBool,
    taskless: AtomicBool,
    upgrade_due: AtomicBool,
    pace: Mutex<Option<PaceGrant>>,
    board: Mutex<OccupancyReading>,
    calls: Mutex<Vec<String>>,
}

impl DoorHost {
    fn called(&self, call: String) {
        self.calls.lock().expect("calls").push(call);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }
}

impl IdleHost for DoorHost {
    fn wants(&self) -> bool {
        self.wants.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        self.owns.load(Ordering::SeqCst)
    }
    fn withholds_turn_end(&self) -> bool {
        self.withholds.load(Ordering::SeqCst)
    }
    fn at_background(&self) -> Option<(String, HostTurn)> {
        let line = self.notice.lock().expect("notice").take()?;
        self.called("at_background".to_string());
        Some((line, HostTurn::Notice))
    }
    fn taskless(&self) -> bool {
        self.taskless.load(Ordering::SeqCst)
    }
    fn occupancy(&self) -> OccupancyReading {
        self.board.lock().expect("board").clone()
    }
    fn upgrade_due(&self) -> bool {
        self.called("upgrade_due".to_string());
        self.upgrade_due.load(Ordering::SeqCst)
    }
    fn occupancy_pace(&self, rule: RuleId, _: i64) -> PaceGrant {
        self.called(format!("occupancy_pace {}", rule.name()));
        self.pace.lock().expect("pace").unwrap_or(PaceGrant::Local)
    }
    fn break_taken(&self) {
        self.called("break_taken".to_string());
    }
    fn inform(&self, text: &str) {
        self.called(format!("inform {text}"));
    }
}

/// THE SERVER AS THE WRITE MEETS IT: the scripted [`Mock`], each request logged with the door's
/// rows the ledger held just before it, and a line the server took (`send` answered `OK seq=`)
/// drawn in the composer of the read that follows it, as Claude Code draws it.
struct Logged<'m> {
    m: &'m mut Mock,
    ledger: PathBuf,
    seen: Vec<(String, DoorRows)>,
}

/// How many of the door's rows the ledger held: intents, and outcomes typed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DoorRows {
    intents: usize,
    typed: usize,
}

impl Ctl for Logged<'_> {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let rows = fact_rows(&self.ledger);
        let held = DoorRows {
            intents: rows.iter().filter(|r| r.0 == INTENT).count(),
            typed: rows
                .iter()
                .filter(|r| r.0 == HOLD && r.1 == "typed")
                .count(),
        };
        // The request as the server reads it, the session's `@sid` dropped.
        let words: Vec<&str> = args
            .iter()
            .copied()
            .filter(|a| !a.starts_with('@'))
            .collect();
        self.seen.push((words.join(" "), held));
        let r = self.m.call(args)?;
        if words.first() == Some(&"send")
            && r.ok()
            && r.stdout.starts_with("OK seq=")
            && let Some(text) = words
                .iter()
                .position(|a| *a == "--")
                .map(|i| words[i + 1..].join(" "))
        {
            let at = self.m.served;
            let mut drawn = self.m.last_served().to_vec();
            let caret = drawn
                .iter()
                .rposition(|row| row.starts_with('❯'))
                .expect("a caret row");
            drawn[caret] = format!("❯ {text}");
            self.m.screens.insert(at, drawn);
            self.m.screen_cols.insert(at, 12);
        }
        Ok(r)
    }
}

const INTENT: &str = "fact-intent@v1";
const HOLD: &str = "fact-hold@v1";

/// The door's fact rows of the ledger at `path`, oldest first: `(rule_id, decision, command,
/// reason)` of each `fact-intent@v1` and `fact-would@v1` row and each `fact-hold@v1` row that is
/// an outcome (`typed` or `skipped`; the hold clock's own rows are `waiting`).
fn fact_rows(path: &Path) -> Vec<(String, String, String, String)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .map(|v| {
            let text = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            (
                text("rule_id"),
                text("decision"),
                text("command"),
                text("reason"),
            )
        })
        .filter(|(rule, decision, _, _)| {
            rule == INTENT
                || rule == crate::supervise::occupancy::rules::WOULD_RULE
                || (rule == HOLD && decision != "waiting")
        })
        .collect()
}

/// The door's `OCCUPANCY` records in `noted`: `(rule, what the write came to)`, the words after
/// `door=break`.
fn wrote(noted: &[String]) -> Vec<(String, String)> {
    noted
        .iter()
        .filter(|line| line.starts_with("OCCUPANCY "))
        .filter_map(|line| {
            let rule = line
                .split(' ')
                .find_map(|w| w.strip_prefix("rule="))?
                .to_string();
            let (_, words) = line.split_once(" door=break ")?;
            Some((rule, words.to_string()))
        })
        .collect()
}

/// `(rule, words)` as [`wrote`] gives them.
fn said(rule: &str, words: &str) -> (String, String) {
    (rule.to_string(), words.to_string())
}

/// The writes of `requests`: every `send`, `key` and `turn`.
fn writes(requests: &[String]) -> Vec<String> {
    requests
        .iter()
        .filter(|r| ["send ", "key ", "turn "].iter().any(|w| r.starts_with(w)))
        .cloned()
        .collect()
}

/// A board on which every one of `hold-room`'s machine terms holds, refreshed at each look.
fn room_at(taken_ms: i64) -> OccupancyReading {
    OccupancyReading {
        taken_ms,
        stale: false,
        avail_mib: Some(96 * 1024),
        pressure: Some(crate::supervise::memory::Pressure::Normal),
        disk: Some(crate::harness::disk::Level::Ok),
        slots: Some(SlotsSeen { usable: 1, n: 3 }),
        slots_free_ms: Some(20 * 60_000),
        room: Some(true),
        held_slot_last_ms: Some(taken_ms - MIN),
    }
}

/// What a run of a held session's breaks gave.
#[derive(Debug)]
struct Ran {
    noted: Vec<String>,
    requests: Vec<(String, DoorRows)>,
    rows: Vec<(String, String, String, String)>,
    calls: Vec<String>,
    /// The ledger's text as the run left it.
    ledger: String,
    /// Whether the land act a loop before this one typed is still owed its answer
    /// (`land_act_seeded`), as the run left it.
    land: bool,
}

impl Ran {
    fn requests(&self) -> Vec<String> {
        self.requests.iter().map(|(r, _)| r.clone()).collect()
    }

    fn writes(&self) -> Vec<String> {
        writes(&self.requests())
    }

    /// The door's rows as `(rule_id, decision)`.
    fn kinds(&self) -> Vec<(String, String)> {
        self.rows
            .iter()
            .map(|(r, d, _, _)| (r.clone(), d.clone()))
            .collect()
    }
}

/// The step before a read: the session and the read's index.
type Before<'a> = &'a mut dyn FnMut(&mut Session<'_, Logged<'_>>, usize);

/// A run: under `occupancy` and the hosted policy `policy`, a held session read at each of
/// `at_s` (seconds from its start; each read's screen `drawn` of its index), the server `set`
/// up first, on `host`; `seed` a ledger a loop before this one left, which the loop reads at its
/// start, `start_ms` the wall then.
struct Script<'a> {
    tag: &'a str,
    policy: SupervisorConfig,
    at_s: Vec<i64>,
    drawn: &'a dyn Fn(usize) -> Vec<String>,
    set: &'a dyn Fn(&mut Mock),
    host: Arc<DoorHost>,
    seed: Option<(String, i64)>,
}

/// The hosted policy under `occupancy`.
fn policy(occupancy: Occupancy) -> SupervisorConfig {
    SupervisorConfig {
        occupancy,
        ..hosted(30).policy
    }
}

impl<'a> Script<'a> {
    fn new(tag: &'a str, occupancy: Occupancy, at_s: &[i64]) -> Self {
        Self {
            tag,
            policy: policy(occupancy),
            at_s: at_s.to_vec(),
            drawn: &|_| break_screen(),
            set: &|_| {},
            host: Arc::new(DoorHost::default()),
            seed: None,
        }
    }

    fn run(self, before: Before<'_>) -> Ran {
        let (dir, path) = ledger_at(self.tag);
        let screens: Vec<Vec<String>> = (0..self.at_s.len()).map(self.drawn).collect();
        let mut m = Mock::new(screens);
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = Some(7);
        m.human_ms = Some(60 * MIN.unsigned_abs());
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named \
                  key\nsend [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <text>: write \
                  text\n"
            .to_string();
        (self.set)(&mut m);
        let start_ms = self.seed.as_ref().map_or(T0, |(_, at)| *at);
        if let Some((body, _)) = &self.seed {
            std::fs::write(&path, body).expect("the predecessor's ledger");
        }
        let mut logged = Logged {
            m: &mut m,
            ledger: path.clone(),
            seen: Vec::new(),
        };
        let (noted, land) = {
            let clock = Arc::new(crate::supervise::fx::ScriptedClock::default());
            clock.stand_mono();
            clock.set_at_ms(start_ms);
            let mut s = session(&mut logged, Some("@s-1".to_string()));
            s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
            s.ledger = approvals::Ledger::open(Some(&path), Some("@s-1"), &mut Vec::new());
            s.stall_host = Some(Arc::clone(&self.host) as Arc<dyn IdleHost>);
            s.setup.background_settle = Duration::ZERO;
            let mut heard = Heard::default();
            // The loop's start reads its ledger, as every loop does: the wall placed on the
            // clock, and what a loop before this one left.
            let body = self.seed.as_ref().map_or("", |(body, _)| body.as_str());
            let start = s.fx.mono();
            s.hold_seed_from(body, start, start_ms, false, &self.policy, &heard);
            let mut since = None;
            let mut at_ms = start_ms;
            for (i, s_at) in self.at_s.iter().enumerate() {
                let ahead = start_ms + s_at * 1000 - at_ms;
                if ahead > 0 {
                    skip(&clock, &mut at_ms, ahead);
                }
                let looked_at = start_ms + (at_ms - start_ms).div_euclid(60_000) * 60_000;
                let board = self.host.board.lock().expect("board").taken_ms > 0;
                if board {
                    *self.host.board.lock().expect("board") = room_at(looked_at);
                }
                before(&mut s, i);
                let screen = s.screen().expect("the break's read");
                s.clock_marks = Some((s.fx.mono(), at_ms));
                s.host_steps_in_background(&screen, &mut since, Some(&self.policy), &mut heard)
                    .expect("the break's step");
            }
            s.hold_left(Some(&self.policy), &heard);
            (heard.noted.clone(), s.turn_end.land_act_seeded())
        };
        let requests = logged.seen;
        let rows = fact_rows(&path);
        let ledger = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        Ran {
            noted,
            requests,
            rows,
            calls: self.host.calls(),
            ledger,
            land,
        }
    }
}

fn nothing(_: &mut Session<'_, Logged<'_>>, _: usize) {}

/// The same script under `observe` and `off`: neither writes, reads `status` or asks the host's
/// pace, and observe asks the server exactly what off asks.
fn observe_types_nothing(make: &dyn Fn(Occupancy) -> Ran) {
    let observe = make(Occupancy::Observe);
    let off = make(Occupancy::Off);
    for (mode, ran) in [("observe", &observe), ("off", &off)] {
        assert!(ran.writes().is_empty(), "{mode}: {:?}", ran.writes());
        assert!(
            !ran.requests().iter().any(|r| r == "status"),
            "{mode}: {:?}",
            ran.requests()
        );
        assert!(
            ran.rows
                .iter()
                .all(|(rule, _, _, _)| rule == crate::supervise::occupancy::rules::WOULD_RULE),
            "{mode}: {:?}",
            ran.rows
        );
        assert!(
            !ran.calls
                .iter()
                .any(|c| c.starts_with("occupancy_pace") || c == "break_taken"),
            "{mode}: {:?}",
            ran.calls
        );
    }
    assert_eq!(
        observe.requests(),
        off.requests(),
        "observe asks what off asks"
    );
}

/// `rows` with the composer drawn holding `draft`.
fn drafted(mut rows: Vec<String>, draft: &str) -> Vec<String> {
    let caret = rows.iter().position(|r| r == "❯").expect("caret row");
    rows[caret] = format!("❯ {draft}");
    rows
}

/// A DUE NUDGE IS TYPED ONCE, AT A DRAWN BREAK, FENCED, AFTER EVERY GUARD: at 31 min of a hold
/// the session reads `status` first, asks the host whether the upgrade's notice is due and then
/// its pace, books the intent, writes the line under the person count and the generation, and
/// presses its Enter fenced; then the host hears the break taken and the line, and the outcome
/// is booked `typed` after the write. The agent takes it; the read after its answer types
/// nothing more. Under observe and off: nothing written, nothing read for it.
#[test]
fn a_due_nudge_is_typed_once_fenced_after_every_guard() {
    let line = templates::wait_nudge(31 * MIN);
    let run = |occupancy| {
        Script::new("write-typed", occupancy, &[0, 31 * 60, 32 * 60]).run(&mut |s, i| {
            if i == 2 {
                // The agent takes the line: a live turn read, as the loop reads one.
                s.took(7, Some(&policy(Occupancy::Prompt)), &mut Heard::default());
            }
        })
    };
    let ran = run(Occupancy::Prompt);
    assert_eq!(wrote(&ran.noted).first(), Some(&said(NUDGE, "typed")));
    let writes = ran.writes();
    assert_eq!(writes.len(), 2, "{writes:#?}");
    assert!(
        writes[0].starts_with("send if-human=7 if-gen=1.")
            && writes[0].ends_with(&format!(" -- {}", line.text())),
        "{writes:#?}"
    );
    assert!(
        writes[1].starts_with("key if-human=7 if-gen=1.") && writes[1].ends_with(" enter"),
        "{writes:#?}"
    );
    let requests = ran.requests();
    let status = requests
        .iter()
        .position(|r| r == "status")
        .expect("a status read");
    let send = requests
        .iter()
        .position(|r| r.starts_with("send "))
        .expect("a send");
    assert!(status < send, "the status read comes before the write");
    assert_eq!(
        ran.requests[send].1,
        DoorRows {
            intents: 1,
            typed: 0
        },
        "the intent booked before the write, its outcome not yet"
    );
    assert_eq!(
        ran.kinds(),
        [
            (INTENT.to_string(), "waiting".to_string()),
            (HOLD.to_string(), "typed".to_string()),
        ]
    );
    assert_eq!(
        ran.rows[1].2,
        line.text(),
        "the outcome row carries the line"
    );
    let episode = |reason: &str| {
        reason
            .split(' ')
            .find(|w| w.starts_with("episode="))
            .map(str::to_string)
    };
    assert_eq!(episode(&ran.rows[0].3), episode(&ran.rows[1].3));
    assert!(episode(&ran.rows[0].3).is_some());
    assert_eq!(
        ran.calls,
        [
            "upgrade_due".to_string(),
            format!("occupancy_pace {NUDGE}"),
            "break_taken".to_string(),
            format!("inform {}", line.text()),
        ]
    );
    observe_types_nothing(&run);
}

/// A CARRIED-OVER WORKFLOWS BREAK IS A BREAK: `Waiting for 2 dynamic workflows` drawn under the
/// status row types the nudge as a shell's break does.
#[test]
fn a_workflows_break_types_the_nudge() {
    let workflows = || {
        let mut r = rows(&[
            "⏺ Both workflows are running; I will pick up when they finish.",
            "",
            "✻ Waiting for 2 dynamic workflows to finish",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on · 2 workflows · ← for agents"));
        r
    };
    assert!(aterm_phase::phase::background_wait(&workflows()).is_some());
    let run = |occupancy| {
        Script {
            drawn: &|_| workflows(),
            ..Script::new("write-workflows", occupancy, &[0, 31 * 60])
        }
        .run(&mut nothing)
    };
    let ran = run(Occupancy::Prompt);
    assert_eq!(wrote(&ran.noted), [said(NUDGE, "typed")]);
    assert_eq!(ran.writes().len(), 2);
    observe_types_nothing(&run);
}

/// ONLY A DRAWN BREAK IS A DOOR: the same 31 min read as a live turn or an idle point judges
/// nothing and writes nothing; the next drawn break of a hold that stood is where it goes.
#[test]
fn a_live_turn_or_an_idle_point_is_no_door() {
    let busy = || {
        let mut r = rows(&["⏺ Working.", "", "✻ Synthesizing… (18s)", ""]);
        r.extend(composer("  esc to interrupt"));
        r
    };
    let idle = || {
        let mut r = rows(&[
            "⏺ Stage 1 landed.",
            "",
            "✻ Worked for 12m 2s · done 4:24 PM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    type Drawn = fn() -> Vec<String>;
    for (tag, screen) in [("write-live", busy as Drawn), ("write-idle", idle as Drawn)] {
        let run = |occupancy| {
            Script {
                drawn: &|i| if i == 1 { screen() } else { break_screen() },
                ..Script::new(tag, occupancy, &[0, 31 * 60])
            }
            .run(&mut nothing)
        };
        let ran = run(Occupancy::Prompt);
        assert!(wrote(&ran.noted).is_empty(), "{tag}: {:?}", ran.noted);
        assert!(ran.writes().is_empty(), "{tag}");
        assert!(ran.rows.is_empty(), "{tag}: {:?}", ran.rows);
        assert!(!ran.requests().iter().any(|r| r == "status"), "{tag}");
        observe_types_nothing(&run);
    }
}

/// One guard case: its tag, the words the write comes to at 31 min, and what sets it up — the
/// server, the host, the session before each read, the policy's autopilot, the break's screen.
struct Case {
    tag: &'static str,
    words: &'static str,
    /// Whether the guard is judged at the write (after the `status` read) rather than by the
    /// decider from the loop's own state.
    at_write: bool,
    set: fn(&mut Mock),
    host: fn(&DoorHost),
    session: fn(&mut Session<'_, Logged<'_>>),
    autopilot: Option<Autopilot>,
    drawn: fn() -> Vec<String>,
}

const CASE: Case = Case {
    tag: "",
    words: "",
    at_write: true,
    set: |_| {},
    host: |_| {},
    session: |_| {},
    autopilot: None,
    drawn: break_screen,
};

fn drafted_break() -> Vec<String> {
    drafted(break_screen(), "and also the docs")
}

/// EACH GUARD HOLDS THE DUE LINE, AT THE DECIDER OR AT THE WRITE, and nothing is written or
/// booked as an intent: a person's keys within their grace, a driver's hand, `hold=1` and
/// unread input (the `status` read just before the write), and a `status` the server would not
/// answer; a draft drawn in the composer; a session the host's record shows taskless; the
/// person's own exchange; autopilot off, and Land (the nudge is Cruise's); a stall; the full
/// disk (the host's withhold) and the upgrade's wind-down (the host owns the turn ends); the
/// memory wall; an act untaken; a manager waiting on the session; the upgrade's notice due
/// beside a nudge; the host's machine pace and its refusal. Each case under observe and off:
/// nothing written, no `status`.
#[test]
fn each_guard_holds_the_due_line_and_nothing_is_written() {
    let cases = [
        Case {
            tag: "person",
            words: "held:person",
            set: |m| m.human_ms = Some(1_000),
            ..CASE
        },
        Case {
            tag: "driver",
            words: "held:person",
            set: |m| m.hand = "lease:another-driver",
            ..CASE
        },
        Case {
            tag: "halted",
            words: "held:halted",
            set: |m| m.hold = "1",
            ..CASE
        },
        Case {
            // A `status` the server would not answer names no person's stamp: someone.
            tag: "status-refused",
            words: "held:person",
            set: |m| {
                m.verb_replies
                    .insert("status", VecDeque::from([err("ERR busy")]));
            },
            ..CASE
        },
        Case {
            tag: "unread",
            words: "held:draft",
            set: |m| m.input = "input=pending input_bytes=3 input_wait_ms=10 fg_rss_mb=-",
            ..CASE
        },
        Case {
            tag: "draft",
            words: "held:draft",
            at_write: false,
            // The cursor after the draft's words: a person's, not the composer's dim hint.
            set: |m| m.cursor_col = Some(20),
            drawn: drafted_break,
            ..CASE
        },
        Case {
            tag: "taskless",
            words: "held:taskless",
            host: |h| h.taskless.store(true, Ordering::SeqCst),
            ..CASE
        },
        Case {
            tag: "exchange",
            words: "held:exchange",
            at_write: false,
            session: |s| {
                if !s.turn_end.left_to_person() {
                    let mut reading =
                        s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
                    reading.left_record = true;
                    s.turn_end.observe(&reading, s.fx.mono());
                }
            },
            ..CASE
        },
        Case {
            tag: "off",
            words: "held:autopilot",
            at_write: false,
            autopilot: Some(Autopilot::Off),
            ..CASE
        },
        Case {
            tag: "land",
            words: "held:autopilot",
            at_write: false,
            autopilot: Some(Autopilot::Land),
            ..CASE
        },
        Case {
            tag: "stall",
            words: "held:stall",
            at_write: false,
            session: |s| {
                s.stalled = Some(crate::supervise::run::stall::WorkerStall {
                    word: "stalled",
                    bytes: Some(3),
                    wait_ms: Some(90_000),
                    rss_mb: None,
                });
            },
            ..CASE
        },
        Case {
            tag: "disk",
            words: "held:host-withheld",
            host: |h| h.withholds.store(true, Ordering::SeqCst),
            ..CASE
        },
        Case {
            // The upgrade's wind-down: the host owns the session's turn ends.
            tag: "owns",
            words: "held:host-withheld",
            host: |h| h.owns.store(true, Ordering::SeqCst),
            ..CASE
        },
        Case {
            tag: "memory",
            words: "held:memory",
            at_write: false,
            session: |s| s.memory_told = Some(crate::supervise::memory::Verdict::Restart),
            ..CASE
        },
        Case {
            tag: "untaken",
            words: "held:act-untaken",
            at_write: false,
            session: |s| {
                if !s.turn_end.act_untaken() {
                    let reading =
                        s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
                    s.turn_end.acted(
                        &TurnEndAction::Type {
                            text: "go on".to_string(),
                            rule_id: RULE_CONTINUE,
                        },
                        &reading,
                        s.fx.mono(),
                    );
                }
            },
            ..CASE
        },
        Case {
            tag: "managed",
            words: "held:managed",
            at_write: false,
            session: |s| s.turn_end.set_managed(true),
            ..CASE
        },
        Case {
            tag: "upgrade",
            words: "held:upgrade-due",
            host: |h| h.upgrade_due.store(true, Ordering::SeqCst),
            ..CASE
        },
        Case {
            tag: "pace",
            words: "paced why=machine",
            host: |h| *h.pace.lock().expect("pace") = Some(PaceGrant::Machine),
            ..CASE
        },
        Case {
            tag: "refused",
            words: "refused why=owns",
            host: |h| {
                *h.pace.lock().expect("pace") = Some(PaceGrant::Refused(PaceRefusal::Owns));
            },
            ..CASE
        },
    ];
    for case in &cases {
        let tag = format!("write-guard-{}", case.tag);
        let run = |occupancy| {
            let host = Arc::new(DoorHost::default());
            (case.host)(&host);
            let mut script = Script {
                drawn: &|_| (case.drawn)(),
                set: &case.set,
                host,
                ..Script::new(&tag, occupancy, &[0, 31 * 60])
            };
            if let Some(autopilot) = case.autopilot {
                script.policy.autopilot = autopilot;
            }
            script.run(&mut |s, _| (case.session)(s))
        };
        let ran = run(Occupancy::Prompt);
        assert_eq!(
            wrote(&ran.noted),
            [said(NUDGE, case.words)],
            "{}: {:#?}",
            case.tag,
            ran.noted
        );
        assert!(ran.writes().is_empty(), "{}: {:?}", case.tag, ran.writes());
        assert!(ran.rows.is_empty(), "{}: {:?}", case.tag, ran.rows);
        assert_eq!(
            ran.requests().iter().any(|r| r == "status"),
            case.at_write,
            "{}: the status read is the write's alone",
            case.tag
        );
        assert!(
            !ran.calls.iter().any(|c| c == "break_taken"),
            "{}",
            case.tag
        );
        observe_types_nothing(&run);
    }
}

/// A planted defect's case: its tag, the defect, and how the server and the host stand.
type Control = (&'static str, Fault, fn(&mut Mock), fn(&DoorHost));

/// THE DOOR'S OWN GUARDS ARE WHAT HOLD IT (§7's negative controls): each defect planted, the
/// case its guard held types the line — no `status` read before the write, the person's hand
/// ignored, a draft ignored, the brakes and the host's withhold ignored, the upgrade's notice
/// due ignored.
#[test]
fn without_its_guards_the_door_would_type() {
    let controls: [Control; 5] = [
        (
            "no-status",
            Fault::DoorSkipsStatus,
            |m| m.human_ms = Some(1_000),
            |_| {},
        ),
        (
            "person",
            Fault::DoorIgnoresPerson,
            |m| m.hand = "lease:another-driver",
            |_| {},
        ),
        (
            "draft",
            Fault::DoorIgnoresDraft,
            |m| m.input = "input=pending input_bytes=3 input_wait_ms=10 fg_rss_mb=-",
            |_| {},
        ),
        (
            "brakes",
            Fault::DoorIgnoresBrakes,
            |_| {},
            |h| {
                h.withholds.store(true, Ordering::SeqCst);
            },
        ),
        (
            "upgrade",
            Fault::NudgeOverNotice,
            |_| {},
            |h| h.upgrade_due.store(true, Ordering::SeqCst),
        ),
    ];
    for (tag, planted, set, host) in controls {
        let tag = format!("write-control-{tag}");
        let run = || {
            let door = Arc::new(DoorHost::default());
            host(&door);
            Script {
                set: &set,
                host: door,
                ..Script::new(&tag, Occupancy::Prompt, &[0, 31 * 60])
            }
            .run(&mut nothing)
        };
        let held = run();
        assert!(held.writes().is_empty(), "{tag}: held");
        let typed = fault::with(planted, run);
        assert_eq!(
            wrote(&typed.noted),
            [said(NUDGE, "typed")],
            "{tag}: {:#?}",
            typed.noted
        );
        assert_eq!(typed.writes().len(), 2, "{tag}");
        if planted == Fault::DoorSkipsStatus {
            assert!(!typed.requests().iter().any(|r| r == "status"));
        }
    }
}

/// THE OUTCOME ROW IS BOOKED AFTER THE WRITE: the ledger holds no `typed` row of the door's when
/// the line is sent. NEGATIVE CONTROL: booked before it, the send finds it there.
#[test]
fn the_outcome_row_follows_the_write() {
    let run = || Script::new("write-order", Occupancy::Prompt, &[0, 31 * 60]).run(&mut nothing);
    let at_send = |ran: &Ran| {
        ran.requests
            .iter()
            .find(|(r, _)| r.starts_with("send "))
            .map(|(_, rows)| *rows)
            .expect("a send")
    };
    assert_eq!(
        at_send(&run()),
        DoorRows {
            intents: 1,
            typed: 0
        }
    );
    assert_eq!(
        at_send(&fault::with(Fault::DoorOutcomeFirst, run)),
        DoorRows {
            intents: 1,
            typed: 1
        }
    );
}

/// A READ THAT CANNOT BE FENCED WRITES NOTHING (§5 row 1): a host whose `send` and `key` take no
/// `if-gen=` books the intent and a `skipped why=unfenced` outcome, and writes nothing — never
/// the guarded submit — and the episode is not tried again within the session's pace.
/// NEGATIVE CONTROL: the guarded fallback allowed, the same read submits.
#[test]
fn an_unfenced_read_is_skipped_never_submitted_guarded() {
    let unfenced = |m: &mut Mock| {
        m.gen_fence = false;
        m.sends_gen = false;
        m.help = "key [id=<key>] [if=<re>] <name>: send a named key\n".to_string();
    };
    let run_under = |occupancy| {
        Script {
            set: &unfenced,
            ..Script::new("write-unfenced", occupancy, &[0, 31 * 60, 32 * 60, 33 * 60])
        }
        .run(&mut nothing)
    };
    let run = || run_under(Occupancy::Prompt);
    observe_types_nothing(&run_under);
    let ran = run();
    assert_eq!(wrote(&ran.noted), [said(NUDGE, "skipped why=unfenced")]);
    assert!(ran.writes().is_empty(), "{:?}", ran.writes());
    assert_eq!(
        ran.kinds(),
        [
            (INTENT.to_string(), "waiting".to_string()),
            (HOLD.to_string(), "skipped".to_string()),
        ],
        "one try in the session's pace"
    );
    assert!(ran.rows[1].3.ends_with(" why=unfenced"), "{:?}", ran.rows);
    assert!(!ran.calls.iter().any(|c| c == "break_taken"));
    let guarded = fault::with(Fault::DoorGuardedFallback, run);
    assert!(
        !guarded.writes().is_empty(),
        "the fallback writes: {:?}",
        guarded.requests()
    );
}

/// THE LAND STATE SURVIVES A FACT LINE (F12): a land episode the session stands in keeps its
/// nudges and its act across the door's line. NEGATIVE CONTROL: the line taken as the host's
/// notice ends it.
#[test]
fn a_fact_line_keeps_the_land_episode() {
    let run = || {
        Script::new("write-land", Occupancy::Prompt, &[0, 31 * 60]).run(&mut |s, i| {
            if i == 0 {
                s.turn_end.seed_land(2, true, false, false);
                s.turn_end.seed_land_owed(RULE_LAND);
                let now = s.fx.mono();
                s.turn_end.seed_land_act(1, 1, Duration::ZERO, now);
            }
        })
    };
    let ran = run();
    assert_eq!(wrote(&ran.noted), [said(NUDGE, "typed")]);
    assert!(ran.land, "the land act is still owed its answer");
    let control = fault::with(Fault::DoorHostTyped, run);
    assert_eq!(wrote(&control.noted), [said(NUDGE, "typed")]);
    assert!(!control.land, "the host's notice ends the episode");
}

/// THE HOST'S LINE IS THE BREAK'S ONE (§2.1): where the host types the upgrade's notice at the
/// break the nudge is due at, the nudge is held for it, and stays due: once that line is taken,
/// the next drawn break types it.
#[test]
fn the_hosts_line_goes_first_and_the_nudge_stays_due() {
    let run = |occupancy| {
        let host = Arc::new(DoorHost::default());
        host.wants.store(true, Ordering::SeqCst);
        let notice = Arc::clone(&host);
        Script {
            host,
            ..Script::new("write-host-line", occupancy, &[0, 31 * 60, 32 * 60])
        }
        .run(&mut |s, i| match i {
            // The upgrade's notice comes due at the break the nudge does.
            1 => *notice.notice.lock().expect("notice") = Some("the notice".to_string()),
            2 => s.took(7, Some(&policy(Occupancy::Prompt)), &mut Heard::default()),
            _ => {}
        })
    };
    let ran = run(Occupancy::Prompt);
    assert_eq!(
        wrote(&ran.noted),
        [said(NUDGE, "held:host-line"), said(NUDGE, "typed")],
        "{:#?}",
        ran.noted
    );
    assert_eq!(ran.calls.first().map(String::as_str), Some("at_background"));
    assert_eq!(ran.writes().len(), 2, "the nudge alone is the loop's write");
    let status = |ran: &Ran| ran.requests().iter().filter(|r| *r == "status").count();
    assert_eq!(
        status(&ran),
        1,
        "the host's break made no read for the nudge"
    );
    observe_types_nothing(&run);
    // NEGATIVE CONTROL: a write that stacks on the host's line goes on to its reads at that
    // break, where only the notice standing untaken holds it
    // (`the_nudge_waits_for_the_hosts_line_to_be_taken` plants that one).
    let stacked = fault::with(Fault::StackOnHostLine, || run(Occupancy::Prompt));
    assert_eq!(
        wrote(&stacked.noted),
        [said(NUDGE, "held:act-untaken"), said(NUDGE, "typed")],
        "{:#?}",
        stacked.noted
    );
    assert_eq!(status(&stacked), 2, "{:?}", stacked.requests());
}

/// THE HOST'S LINE STANDS UNTAKEN PAST ITS BREAK (the safety review's F-A): the upgrade's notice
/// typed at one read of a break, and not taken, holds the nudge at every later read of that
/// break (`held:act-untaken`, decided before anything is read for the write); once the agent's
/// turn is read, the next break types it, and a notice never taken holds it at every read.
/// NEGATIVE CONTROL: an untaken-act guard that leaves out the host's line (the policy's acts
/// alone) types the nudge over the notice never taken.
#[test]
fn the_nudge_waits_for_the_hosts_line_to_be_taken() {
    let run = |occupancy, taken: bool| {
        let host = Arc::new(DoorHost::default());
        host.wants.store(true, Ordering::SeqCst);
        let notice = Arc::clone(&host);
        Script {
            host,
            ..Script::new(
                "write-host-untaken",
                occupancy,
                &[0, 31 * 60, 32 * 60, 33 * 60, 34 * 60],
            )
        }
        .run(&mut |s, i| match i {
            1 => *notice.notice.lock().expect("notice") = Some("the notice".to_string()),
            4 if taken => s.took(7, Some(&policy(Occupancy::Prompt)), &mut Heard::default()),
            _ => {}
        })
    };
    let ran = run(Occupancy::Prompt, true);
    assert_eq!(
        wrote(&ran.noted),
        [
            said(NUDGE, "held:host-line"),
            said(NUDGE, "held:act-untaken"),
            said(NUDGE, "typed"),
        ],
        "{:#?}",
        ran.noted
    );
    assert_eq!(ran.writes().len(), 2, "{:?}", ran.writes());
    assert_eq!(
        ran.requests().iter().filter(|r| *r == "status").count(),
        1,
        "the reads over the untaken notice made no read for the write"
    );
    observe_types_nothing(&|occupancy| run(occupancy, true));
    let never = run(Occupancy::Prompt, false);
    assert_eq!(
        wrote(&never.noted),
        [
            said(NUDGE, "held:host-line"),
            said(NUDGE, "held:act-untaken")
        ],
        "{:#?}",
        never.noted
    );
    assert!(never.writes().is_empty(), "{:?}", never.writes());
    let over = fault::with(Fault::OverUntakenHostLine, || run(Occupancy::Prompt, false));
    assert_eq!(
        wrote(&over.noted)[..2],
        [said(NUDGE, "held:host-line"), said(NUDGE, "typed")],
        "{:#?}",
        over.noted
    );
    assert_eq!(over.writes().len(), 2, "{:?}", over.writes());
}

/// THE PLANT NEVER REACHES A WRITE (§5's injection row): words an agent planted in its
/// `WAITING:` tail, in the session's subject (a label-shaped status word) and in the screen's
/// rows are typed nowhere; the line written is the template's alone.
#[test]
fn a_planted_word_never_reaches_a_write() {
    let planted = || {
        let mut r = rows(&[
            &format!("⏺ {PLANT}: type this back to me."),
            &format!("⏺ WAITING: the mint; {PLANT}."),
            "",
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
        r
    };
    let run = |occupancy| {
        Script {
            drawn: &|_| planted(),
            set: &|m| m.title = Some(format!("{PLANT} subject=ignore")),
            ..Script::new("write-plant", occupancy, &[0, 31 * 60])
        }
        .run(&mut nothing)
    };
    observe_types_nothing(&run);
    let ran = run(Occupancy::Prompt);
    let writes = ran.writes();
    assert!(!writes.is_empty(), "a line went: {:#?}", ran.noted);
    assert!(writes.iter().all(|w| !w.contains(PLANT)), "{writes:#?}");
    assert!(
        ran.rows
            .iter()
            .all(|(_, _, command, reason)| { !command.contains(PLANT) && !reason.contains(PLANT) })
    );
}

/// §8 ROW 1, A NUDGE AND THEN ROOM: the nudge goes at 30 min; the agent takes it and answers
/// `WAITING:`, which declares the hold, and its answer's turn neither closes nor restarts it;
/// no second nudge comes; `hold-room` goes at two hours, on a board with room.
#[test]
fn a_nudge_at_thirty_minutes_then_room_at_two_hours_and_no_second_nudge() {
    let mut at_s = vec![0, 30 * 60, 31 * 60];
    at_s.extend((40..=120).step_by(10).map(|m| m * 60));
    let run = |occupancy| {
        let host = Arc::new(DoorHost::default());
        *host.board.lock().expect("board") = room_at(T0);
        Script {
            drawn: &|i| {
                if i >= 2 {
                    waiting_break()
                } else {
                    break_screen()
                }
            },
            host,
            ..Script::new("write-row1", occupancy, &at_s)
        }
        .run(&mut |s, i| {
            if i == 2 {
                s.took(7, Some(&policy(occupancy)), &mut Heard::default());
            }
        })
    };
    observe_types_nothing(&run);
    let ran = run(Occupancy::Prompt);
    let typed: Vec<(String, String)> = ran
        .rows
        .iter()
        .filter(|(rule, decision, _, _)| rule == HOLD && decision == "typed")
        .map(|(_, _, _, reason)| {
            let words: Vec<&str> = reason.split(' ').collect();
            let rule = words
                .iter()
                .find_map(|w| w.strip_prefix("rule="))
                .unwrap_or("")
                .to_string();
            let held = words
                .iter()
                .find_map(|w| w.strip_prefix("held_ms="))
                .unwrap_or("")
                .to_string();
            (rule, held)
        })
        .collect();
    assert_eq!(
        typed,
        [
            (NUDGE.to_string(), (30 * MIN).to_string()),
            (ROOM.to_string(), (120 * MIN).to_string()),
        ],
        "{:#?}",
        ran.noted
    );
    assert!(
        !ran.noted
            .iter()
            .any(|l| l.starts_with("OCCUPANCY ") && l.contains(" hold closed")),
        "the hold stood: {:#?}",
        ran.noted
    );
    assert!(
        ran.noted
            .iter()
            .any(|l| l.starts_with("OCCUPANCY ") && l.contains(" hold declared")),
        "the answer declared it: {:#?}",
        ran.noted
    );
    assert_eq!(
        ran.writes()
            .iter()
            .filter(|w| w.starts_with("send "))
            .count(),
        2
    );
}

/// `body` with every row stamped `ts`, in its order: the scripted clock's time, where the rows a
/// test's ledger stamps are the machine's.
fn stamped(body: &str, ts: i64) -> String {
    body.lines()
        .map(|line| {
            let rest = line
                .strip_prefix("{\"ts\":")
                .and_then(|r| r.split_once(','))
                .map_or(line, |(_, rest)| rest);
            format!("{{\"ts\":{ts},{rest}\n")
        })
        .collect()
}

/// A CRASH AFTER THE INTENT ROW IS RETOLD ONCE (`OccupancyDoor`'s `Crash`): a loop that booked
/// the intent and died before its outcome leaves the episode untold, so its successor, carrying
/// the hold, types it; that successor's own outcome row then holds the next one from telling it
/// again. CONTROL: the predecessor's outcome standing, its successor tells nothing.
#[test]
fn a_crash_after_the_intent_is_retold_once() {
    let first = Script::new("write-crash-1", Occupancy::Prompt, &[0, 31 * 60]).run(&mut nothing);
    assert_eq!(wrote(&first.noted), [said(NUDGE, "typed")]);
    let typed_row = |l: &&str| {
        l.contains("\"rule_id\":\"fact-hold@v1\"") && l.contains("\"decision\":\"typed\"")
    };
    // The loop died between the write and its outcome row.
    let crashed: String = first
        .ledger
        .lines()
        .filter(|l| !typed_row(l))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(crashed.contains(INTENT));
    let successor = |tag: &str, body: &str, at_ms: i64| {
        Script {
            seed: Some((stamped(body, at_ms - MIN), at_ms)),
            ..Script::new(tag, Occupancy::Prompt, &[0, 60])
        }
        .run(&mut nothing)
    };
    let sends = |ran: &Ran| {
        ran.writes()
            .iter()
            .filter(|w| w.starts_with("send "))
            .count()
    };
    let retold = successor("write-crash-2", &crashed, T0 + 32 * MIN);
    assert_eq!(sends(&retold), 1, "retold: {:#?}", retold.noted);
    let episodes = |ran: &Ran| -> Vec<String> {
        ran.rows
            .iter()
            .filter(|(rule, _, _, _)| rule == INTENT)
            .filter_map(|(_, _, _, reason)| {
                reason
                    .split(' ')
                    .find(|w| w.starts_with("episode="))
                    .map(str::to_string)
            })
            .collect()
    };
    assert_eq!(
        episodes(&retold).last(),
        episodes(&first).last(),
        "the same episode, its hold carried"
    );
    // NEGATIVE CONTROL: an intent with no outcome read as told is never retold.
    let lost = fault::with(Fault::SeedIntentAsTold, || {
        successor("write-crash-lost", &crashed, T0 + 32 * MIN)
    });
    assert_eq!(sends(&lost), 0, "{:#?}", lost.noted);
    let again = successor("write-crash-3", &retold.ledger, T0 + 34 * MIN);
    assert_eq!(sends(&again), 0, "told once: {:#?}", again.noted);
    let kept = successor("write-crash-kept", &first.ledger, T0 + 32 * MIN);
    assert_eq!(sends(&kept), 0, "{:#?}", kept.noted);
    assert!(first.ledger.lines().any(|l| typed_row(&l)));
}

/// A WRITE HELD AFTER ITS READS WAITS A LOOK (the safety review's F-D): a nudge a person's keys
/// hold at the write, or the machine's pace, is read for again at the next board look, not at
/// every settled read: reads 20 s, 40 s and 70 s after the first make one more `status` read
/// and ask the pace no more often. NEGATIVE CONTROL: a write that reads again at every settled
/// read makes all four.
#[test]
fn a_write_held_after_its_reads_waits_a_look() {
    let at_s = [0, 31 * 60, 31 * 60 + 20, 31 * 60 + 40, 32 * 60 + 10];
    let person = |m: &mut Mock| m.human_ms = Some(1_000);
    type Count = fn(&Ran) -> usize;
    let status: Count = |ran| ran.requests().iter().filter(|r| *r == "status").count();
    let paced: Count = |ran| {
        ran.calls
            .iter()
            .filter(|c| c.starts_with("occupancy_pace"))
            .count()
    };
    type Held = (
        &'static str,
        &'static str,
        fn(&mut Mock),
        fn(&DoorHost),
        Count,
    );
    let cases: [Held; 2] = [
        ("person", "held:person", person, |_| {}, status),
        (
            "machine",
            "paced why=machine",
            |_| {},
            |h| *h.pace.lock().expect("pace") = Some(PaceGrant::Machine),
            paced,
        ),
    ];
    for (tag, words, set, host, count) in cases {
        let tag = format!("write-again-{tag}");
        let run = |occupancy| {
            let door = Arc::new(DoorHost::default());
            host(&door);
            Script {
                set: &set,
                host: door,
                ..Script::new(&tag, occupancy, &at_s)
            }
            .run(&mut nothing)
        };
        let ran = run(Occupancy::Prompt);
        assert_eq!(wrote(&ran.noted), [said(NUDGE, words)], "{tag}");
        assert!(ran.writes().is_empty(), "{tag}");
        assert_eq!(
            count(&ran),
            2,
            "{tag}: {:?} {:?}",
            ran.requests(),
            ran.calls
        );
        observe_types_nothing(&run);
        let rereads = fault::with(Fault::DoorRereads, || run(Occupancy::Prompt));
        assert_eq!(count(&rereads), 4, "{tag}: {:?}", rereads.requests());
    }
}

/// A COMPOSER THE READ CANNOT JUDGE IS A DRAFT (F10): words at the caret with the cursor where
/// the composer's dim hint leaves it read as no draft to the loop's own clock, so the write asks
/// the cell under the caret: a cell it cannot read holds the line (`held:draft`), as a cell drawn
/// as typed does; the hint's dim cell lets it go.
#[test]
fn a_composer_the_read_cannot_judge_holds_the_line() {
    let hinted = || drafted(break_screen(), "Try \"refactor the parser\"");
    type Cell = (&'static str, &'static str, fn(&mut Mock));
    let cells: [Cell; 3] = [
        ("unread", "held:draft", |m| m.cell_unreadable = true),
        ("typed", "held:draft", |m| {
            m.cell_attrs = "none";
            m.cell_fg = "ffffff";
        }),
        ("hint", "typed", |_| {}),
    ];
    for (tag, words, cell) in cells {
        let tag = format!("write-unjudged-{tag}");
        let set = |m: &mut Mock| {
            m.cursor_col = Some(2);
            cell(m);
        };
        let run = |occupancy| {
            Script {
                drawn: &|_| hinted(),
                set: &set,
                ..Script::new(&tag, occupancy, &[0, 31 * 60])
            }
            .run(&mut nothing)
        };
        let ran = run(Occupancy::Prompt);
        assert_eq!(
            wrote(&ran.noted),
            [said(NUDGE, words)],
            "{tag}: {:#?}",
            ran.noted
        );
        assert!(
            ran.requests().iter().any(|r| r.starts_with("cell ")),
            "{tag}: the write read the caret's cell: {:?}",
            ran.requests()
        );
        assert_eq!(ran.writes().is_empty(), words != "typed", "{tag}");
        observe_types_nothing(&run);
    }
}

/// The holder the window's host names its harness by in these runs.
const HOLDER: &str = "aterm-harness@1";

/// THE DOOR'S LINE IS TYPED UNDER THE HAND (§5 row 1, `Session::type_fact`): where the window's
/// host types under its hand, the fenced send and its Enter go between the hand's take and its
/// give-back. CONTROL: with no hand asked for, no lease is taken.
#[test]
fn the_doors_line_is_typed_under_the_hand() {
    let take = crate::harness::upgrade_drive::hand_take_here_request_as(HOLDER);
    let back = crate::harness::upgrade_drive::hand_give_back_here_request_as(HOLDER);
    let run = |hand: bool| {
        let lease = |m: &mut Mock| {
            m.verb_replies.insert(
                "lease",
                VecDeque::from([
                    ok("OK lease acquired holder=aterm-harness@1 ttl_ms=60000 \
                        expires_in_ms=60000 hard=1\n"),
                    ok("OK lease released\n"),
                ]),
            );
        };
        Script {
            set: &lease,
            ..Script::new("write-hand", Occupancy::Prompt, &[0, 31 * 60])
        }
        .run(&mut |s, i| {
            if i == 0 {
                s.set_supervisor_name(Some(HOLDER.to_string()));
                s.type_under_hand(hand);
            }
        })
    };
    let acts = |ran: &Ran| -> Vec<String> {
        ran.requests()
            .into_iter()
            .filter(|r| ["lease ", "send ", "key "].iter().any(|v| r.starts_with(v)))
            .map(|r| {
                if r.starts_with("lease ") {
                    r
                } else {
                    r.split(' ').next().unwrap_or_default().to_string()
                }
            })
            .collect()
    };
    let held = run(true);
    assert_eq!(wrote(&held.noted), [said(NUDGE, "typed")]);
    assert_eq!(
        acts(&held),
        [take.clone(), "send".to_string(), "key".to_string(), back],
        "{:?}",
        held.requests()
    );
    let free = run(false);
    assert_eq!(wrote(&free.noted), [said(NUDGE, "typed")]);
    assert_eq!(acts(&free), ["send", "key"], "{:?}", free.requests());
}

/// THE LINE'S ANSWER ENDS AT THE BREAK AFTER IT (`TurnEndState::fact_answered`, at the break's
/// read): the agent's turn read after the door's line is the harness's; once a break of its own
/// background work is read after that turn, the next turn read is someone else's. CONTROL: two
/// turns read with no break between are both the line's answer.
#[test]
fn the_lines_answer_ends_at_the_break_after_it() {
    let others = |between: bool| {
        let mut seen = Vec::new();
        let ran = Script::new(
            "write-answered",
            Occupancy::Prompt,
            &[0, 31 * 60, 32 * 60, 33 * 60],
        )
        .run(&mut |s, i| {
            let mut took = |s: &mut Session<'_, Logged<'_>>| {
                s.took(7, Some(&policy(Occupancy::Prompt)), &mut Heard::default());
                seen.push(s.turn_end.other_turn());
            };
            match i {
                2 => {
                    took(s);
                    if !between {
                        took(s);
                    }
                }
                3 if between => took(s),
                _ => {}
            }
        });
        assert_eq!(wrote(&ran.noted).first(), Some(&said(NUDGE, "typed")));
        seen
    };
    assert_eq!(others(true), [false, true], "the break ended the answer");
    assert_eq!(others(false), [false, false], "still the line's answer");
}

/// A FACT ROW CLOSES NO REMEMBERED WALL (F13, `Session::ledger_row`): a wall the loop keeps is
/// booked once and the door's typed row leaves it booked. CONTROL: a policy's typed act closes
/// the ledger's wall, which the next persist books again.
#[test]
fn a_fact_row_closes_no_remembered_wall() {
    use crate::supervise::approvals::{self, Outcome};
    let reason = approvals::wall_wait_reason(
        Some(TEST_NOW + 3600),
        Some("3pm"),
        aterm_phase::WallKind::UsageSession,
        "Session limit reached",
    );
    let wall = approvals::Row {
        rule_id: approvals::RULE_WALL_WAIT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &reason,
        box_seq: 1,
    }
    .to_json(TEST_NOW * 1000 - 60_000, Some("@s-1"));
    let rebooked = |rule: &str| -> usize {
        let (dir, path) = ledger_at("write-wall");
        std::fs::write(&path, format!("{wall}\n")).expect("the wall's row");
        let walls = || {
            std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.contains(&format!(",\"rule_id\":\"{}\",", approvals::RULE_WALL_WAIT)))
                .count()
        };
        let mut m = Mock::new(vec![break_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.ledger = approvals::Ledger::open(Some(&path), Some("@s-1"), &mut Vec::new());
        let body = std::fs::read_to_string(&path).expect("the ledger");
        s.seed_wall_memory(approvals::open_wall_in(&body, Some("@s-1")));
        assert!(s.wall_memory.is_some(), "the wall is remembered");
        s.persist_wall(1);
        let before = walls();
        s.ledger_row(rule, Outcome::Typed, "the line", "-", 2);
        s.persist_wall(3);
        let after = walls();
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
        after - before
    };
    assert_eq!(rebooked(crate::supervise::facts::FactKind::Hold.rule()), 0);
    assert_eq!(rebooked(RULE_CONTINUE), 1, "a policy's act closes it");
}
