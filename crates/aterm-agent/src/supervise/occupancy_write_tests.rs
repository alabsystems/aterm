// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `occupancy_write` under `prompt`, `observe`, and `off`.

use std::sync::Mutex;

use super::*;
use crate::supervise::config::Autopilot;
use crate::supervise::occupancy::fault::{self, Fault};
use crate::supervise::occupancy::templates;
use crate::supervise::occupancy::{
    FactWord, NoteOutcome, OccupancyReading, OccupancySaid, PaceGrant, PaceRefusal, RuleId,
    SaidReadyIdle, SlotsSeen,
};
use crate::supervise::policy::turn_end::{HostTurn, RULE_LAND};

const NUDGE: &str = "wait-nudge@v1";
const ROOM: &str = "hold-room@v1";
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
    /// The board as the host re-reads it for a slot rule's write; `None`, its board.
    reread: Mutex<Option<OccupancyReading>>,
    /// The hung calls the host's facts reading names (`IdleHost::facts`); `None`, no reading.
    facts: Mutex<Option<Vec<crate::supervise::BackgroundCall>>>,
    /// The wall (unix ms) of the read the run is at, when the facts reading is taken.
    facts_at: Mutex<i64>,
    calls: Mutex<Vec<String>>,
    /// What the door told the host ([`IdleHost::occupancy_said`]), in order.
    said: Mutex<Vec<OccupancySaid>>,
    /// The task record's answer to the task door ([`IdleHost::task_fact`]); `None`, a record's:
    /// `noted` for a word, `cleared` for none.
    note: Mutex<Option<NoteOutcome>>,
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
        *self.board.lock().expect("board")
    }
    fn upgrade_due(&self) -> bool {
        self.called("upgrade_due".to_string());
        self.upgrade_due.load(Ordering::SeqCst)
    }
    fn occupancy_pace(&self, rule: RuleId, _: i64) -> PaceGrant {
        self.called(format!("occupancy_pace {}", rule.name()));
        let reread = self
            .reread
            .lock()
            .expect("reread")
            .unwrap_or_else(|| *self.board.lock().expect("board"));
        self.pace.lock().expect("pace").unwrap_or(if rule.slot() {
            PaceGrant::Room(reread)
        } else {
            PaceGrant::Local
        })
    }
    fn break_taken(&self) {
        self.called("break_taken".to_string());
    }
    fn inform(&self, text: &str) {
        self.called(format!("inform {text}"));
    }
    fn facts(&self) -> Option<crate::supervise::FactsReading> {
        self.called("facts".to_string());
        let calls = self.facts.lock().expect("facts").clone()?;
        Some(crate::supervise::FactsReading {
            as_of_ms: *self.facts_at.lock().expect("facts_at"),
            no_progress: calls,
        })
    }
    fn occupancy_said(&self, said: &OccupancySaid) {
        self.said.lock().expect("said").push(said.clone());
    }
    fn task_fact(&self, word: Option<FactWord>) -> NoteOutcome {
        self.called(format!(
            "task_fact {}",
            word.map_or_else(|| "-".to_string(), FactWord::word)
        ));
        let answer = *self.note.lock().expect("note");
        answer.unwrap_or(if word.is_some() {
            NoteOutcome::Noted
        } else {
            NoteOutcome::Cleared
        })
    }
}

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

/// The typed door's fact rows in the ledger at `path`: its intents, its would-be lines and its
/// outcomes, and never the task door's ([`task_rows`]).
fn fact_rows(path: &Path) -> Vec<(String, String, String, String)> {
    door_rows(&std::fs::read_to_string(path).unwrap_or_default())
        .into_iter()
        .filter(|(_, _, _, reason)| !crate::supervise::occupancy::door::is_task_row(reason))
        .collect()
}

/// The task door's rows (`door=task`) in ledger text `body`, oldest first.
fn task_rows(body: &str) -> Vec<(String, String, String, String)> {
    door_rows(body)
        .into_iter()
        .filter(|(_, _, _, reason)| crate::supervise::occupancy::door::is_task_row(reason))
        .collect()
}

fn door_rows(body: &str) -> Vec<(String, String, String, String)> {
    body.lines()
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
        .filter(|(rule, decision, _, reason)| {
            rule == INTENT
                || rule == crate::supervise::occupancy::rules::WOULD_RULE
                || (rule == HOLD && (decision != "waiting" || reason.starts_with("rule=")))
        })
        .collect()
}

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

fn said(rule: &str, words: &str) -> (String, String) {
    (rule.to_string(), words.to_string())
}

fn writes(requests: &[String]) -> Vec<String> {
    requests
        .iter()
        .filter(|r| ["send ", "key ", "turn "].iter().any(|w| r.starts_with(w)))
        .cloned()
        .collect()
}

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
    /// What the door told the host, in order.
    said: Vec<OccupancySaid>,
}

impl Ran {
    fn requests(&self) -> Vec<String> {
        self.requests.iter().map(|(r, _)| r.clone()).collect()
    }

    fn writes(&self) -> Vec<String> {
        writes(&self.requests())
    }

    fn kinds(&self) -> Vec<(String, String)> {
        self.rows
            .iter()
            .map(|(r, d, _, _)| (r.clone(), d.clone()))
            .collect()
    }
}

type Before<'a> = &'a mut dyn FnMut(&mut Session<'_, Logged<'_>>, usize);

struct Script<'a> {
    tag: &'a str,
    policy: SupervisorConfig,
    at_s: Vec<i64>,
    drawn: &'a dyn Fn(usize) -> Vec<String>,
    set: &'a dyn Fn(&mut Mock),
    host: Arc<DoorHost>,
    seed: Option<(String, i64)>,
}

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
                *self.host.facts_at.lock().expect("facts_at") = at_ms;
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
            said: self.host.said.lock().expect("said").clone(),
        }
    }
}

fn nothing(_: &mut Session<'_, Logged<'_>>, _: usize) {}

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
            !ran.calls.iter().any(|c| {
                c.starts_with("occupancy_pace") || c == "break_taken" || c.starts_with("task_fact")
            }),
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

fn drafted(mut rows: Vec<String>, draft: &str) -> Vec<String> {
    let caret = rows.iter().position(|r| r == "❯").expect("caret row");
    rows[caret] = format!("❯ {draft}");
    rows
}

#[test]
fn a_due_nudge_is_typed_once_fenced_after_every_guard() {
    let line = templates::wait_nudge(31 * MIN);
    let run = |occupancy| {
        Script::new("write-typed", occupancy, &[0, 31 * 60, 32 * 60]).run(&mut |s, i| {
            if i == 2 {
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

#[test]
fn the_setups_nudge_wait_reaches_the_door() {
    let run = |after: Option<Duration>| {
        Script::new("write-seam", Occupancy::Prompt, &[0, 61]).run(&mut |s, i| {
            if let (0, Some(after)) = (i, after) {
                s.setup.occupancy_nudge_after = after;
            }
        })
    };
    let short = run(Some(Duration::from_secs(60)));
    assert_eq!(wrote(&short.noted), [said(NUDGE, "typed")]);
    let writes = short.writes();
    assert_eq!(writes.len(), 2, "{writes:#?}");
    let line = templates::wait_nudge(61_000);
    assert!(
        writes[0].ends_with(&format!(" -- {}", line.text())),
        "{writes:#?}"
    );
    let shipped = run(None);
    assert!(shipped.writes().is_empty(), "{:?}", shipped.writes());
    assert!(shipped.rows.is_empty(), "{:?}", shipped.rows);
}

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
        let booked: Vec<(String, String)> = case
            .words
            .strip_prefix("held:")
            .map(|guard| held_by(NUDGE, guard))
            .into_iter()
            .collect();
        assert_eq!(
            held_rows_of(&ran.ledger),
            booked,
            "{}: a held verdict's row, a pace's none",
            case.tag
        );
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

type Control = (&'static str, Fault, fn(&mut Mock), fn(&DoorHost));

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

#[test]
fn a_line_left_in_the_composer_is_not_said_unwritten() {
    let run = |set: &dyn Fn(&mut Mock)| {
        Script {
            set,
            ..Script::new("write-left", Occupancy::Prompt, &[0, 31 * 60])
        }
        .run(&mut nothing)
    };
    let left = run(&|m| {
        m.key_replies
            .push_back(ok("OK skipped reason=person seq=101\n"));
    });
    assert_eq!(
        wrote(&left.noted),
        [said(NUDGE, "skipped why=not-submitted")]
    );
    let writes = left.writes();
    assert!(
        writes.first().is_some_and(|w| w.starts_with("send "))
            && writes.get(1).is_some_and(|w| w.ends_with(" enter")),
        "the line written, its Enter skipped: {writes:#?}"
    );
    assert_eq!(
        left.kinds(),
        [
            (INTENT.to_string(), "waiting".to_string()),
            (HOLD.to_string(), "skipped".to_string()),
        ]
    );
    assert!(
        left.rows[1].3.ends_with(" why=not-submitted"),
        "{:?}",
        left.rows
    );
    let unwritten = run(&|m| {
        m.send_replies
            .push_back(ok("OK skipped reason=changed seq=101\n"));
    });
    assert_eq!(
        wrote(&unwritten.noted),
        [said(NUDGE, "skipped why=not-written")]
    );
    assert!(
        !unwritten.writes().iter().any(|w| w.starts_with("key ")),
        "{:?}",
        unwritten.writes()
    );
    assert!(
        unwritten.rows[1].3.ends_with(" why=not-written"),
        "{:?}",
        unwritten.rows
    );
}

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
    let stacked = fault::with(Fault::StackOnHostLine, || run(Occupancy::Prompt));
    assert_eq!(
        wrote(&stacked.noted),
        [said(NUDGE, "held:act-untaken"), said(NUDGE, "typed")],
        "{:#?}",
        stacked.noted
    );
    assert_eq!(status(&stacked), 2, "{:?}", stacked.requests());
}

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

fn room_run(
    tag: &str,
    occupancy: Occupancy,
    after: &[i64],
    reread: &dyn Fn(&DoorHost, usize, i64),
) -> Ran {
    let mut at_s = vec![0, 30 * 60, 31 * 60];
    at_s.extend((40..=120).step_by(10).map(|m| m * 60));
    at_s.extend(after.iter().map(|m| m * 60));
    let host = Arc::new(DoorHost::default());
    *host.board.lock().expect("board") = room_at(T0);
    let seen = Arc::clone(&host);
    let at = at_s.clone();
    Script {
        drawn: &|i| {
            if i >= 2 {
                waiting_break()
            } else {
                break_screen()
            }
        },
        host,
        ..Script::new(tag, occupancy, &at_s)
    }
    .run(&mut |s, i| {
        if i == 2 {
            s.took(7, Some(&policy(occupancy)), &mut Heard::default());
        }
        reread(&seen, i, T0 + at[i] * 1000);
    })
}

type AtRoom<'a> = &'a dyn Fn(&DoorHost, i64);

const ROOM_DUE: usize = 11;

fn room_with(at_ms: i64, usable: u16) -> OccupancyReading {
    OccupancyReading {
        slots: Some(SlotsSeen { usable, n: 3 }),
        ..room_at(at_ms)
    }
}

fn rooms_typed(ran: &Ran) -> Vec<(i64, String)> {
    ran.rows
        .iter()
        .filter(|(rule, decision, _, reason)| {
            rule == HOLD && decision == "typed" && reason.contains(ROOM)
        })
        .map(|(_, _, command, reason)| {
            let held = reason
                .split(' ')
                .find_map(|w| w.strip_prefix("held_ms=")?.parse().ok())
                .expect("held_ms");
            (held, command.clone())
        })
        .collect()
}

fn room_said(ran: &Ran) -> Vec<String> {
    wrote(&ran.noted)
        .into_iter()
        .filter(|(rule, _)| rule == ROOM)
        .map(|(_, words)| words)
        .collect()
}

#[test]
fn a_slot_taken_between_decide_and_write_holds_the_line() {
    let line = |held_ms: i64| templates::hold_room(held_ms, 1, 3, 20 * 60_000, 96 * 1024);
    let taken: [AtRoom<'_>; 2] = [
        &|h, at| *h.reread.lock().expect("reread") = Some(room_with(at, 0)),
        &|h, _| *h.pace.lock().expect("pace") = Some(PaceGrant::Unmet),
    ];
    for (n, take) in taken.into_iter().enumerate() {
        let ran = room_run(
            &format!("write-room-taken-{n}"),
            Occupancy::Prompt,
            &[121],
            &|h, i, at| {
                *h.reread.lock().expect("reread") = None;
                *h.pace.lock().expect("pace") = None;
                if i == ROOM_DUE {
                    take(h, at);
                }
            },
        );
        assert_eq!(
            room_said(&ran),
            ["paced why=unmet", "typed"],
            "{n}: {:#?}",
            ran.noted
        );
        assert_eq!(
            rooms_typed(&ran),
            [(121 * MIN, line(121 * MIN).text().to_string())],
            "{n}"
        );
        let intents = ran
            .rows
            .iter()
            .filter(|r| r.0 == INTENT && r.3.contains(ROOM))
            .count();
        assert_eq!(intents, 1, "{n}: no intent was booked for the held write");
        assert_eq!(
            ran.writes()
                .iter()
                .filter(|w| w.starts_with("send "))
                .count(),
            2,
            "{n}: the nudge and one room line"
        );
    }
    let ran = room_run("write-room-free", Occupancy::Prompt, &[121], &|_, _, _| {});
    assert_eq!(room_said(&ran), ["typed"], "{:#?}", ran.noted);
    assert_eq!(
        rooms_typed(&ran),
        [(120 * MIN, line(120 * MIN).text().to_string())]
    );
}

#[test]
fn a_stale_reread_refuses_the_line() {
    use crate::supervise::facts::FACT_FRESH_MS;
    let stale: [AtRoom<'_>; 2] = [
        &|h, at| *h.reread.lock().expect("reread") = Some(room_at(at - FACT_FRESH_MS - 1)),
        &|h, _| *h.pace.lock().expect("pace") = Some(PaceGrant::Local),
    ];
    for (n, old) in stale.into_iter().enumerate() {
        let ran = room_run(
            &format!("write-room-stale-{n}"),
            Occupancy::Prompt,
            &[121],
            &|h, i, at| {
                *h.reread.lock().expect("reread") = None;
                *h.pace.lock().expect("pace") = None;
                if i == ROOM_DUE {
                    old(h, at);
                }
            },
        );
        assert_eq!(
            room_said(&ran),
            ["paced why=stale", "typed"],
            "{n}: {:#?}",
            ran.noted
        );
        assert_eq!(rooms_typed(&ran).len(), 1, "{n}");
        assert_eq!(rooms_typed(&ran)[0].0, 121 * MIN, "{n}");
    }
    let ran = room_run("write-room-edge", Occupancy::Prompt, &[121], &|h, i, at| {
        *h.reread.lock().expect("reread") = (i == ROOM_DUE).then(|| room_at(at - FACT_FRESH_MS));
    });
    assert_eq!(room_said(&ran), ["typed"], "{:#?}", ran.noted);
    assert_eq!(rooms_typed(&ran)[0].0, 120 * MIN);
}

#[test]
fn the_typed_numbers_are_the_reread_boards() {
    let reread = |h: &DoorHost, i: usize, at: i64| {
        *h.reread.lock().expect("reread") = (i == ROOM_DUE).then(|| OccupancyReading {
            slots_free_ms: Some(25 * 60_000),
            avail_mib: Some(64 * 1024),
            ..room_with(at, 2)
        });
    };
    observe_types_nothing(&|occupancy| room_run("write-room-numbers", occupancy, &[], &reread));
    let ran = room_run("write-room-numbers", Occupancy::Prompt, &[], &reread);
    let typed = templates::hold_room(120 * MIN, 2, 3, 25 * 60_000, 64 * 1024);
    let decided = templates::hold_room(120 * MIN, 1, 3, 20 * 60_000, 96 * 1024);
    assert_ne!(typed.text(), decided.text(), "the control: other numbers");
    assert_eq!(rooms_typed(&ran), [(120 * MIN, typed.text().to_string())]);
    let sent = ran.writes();
    assert!(
        sent.iter()
            .any(|w| w.starts_with("send ") && w.ends_with(typed.text())),
        "{sent:#?}"
    );
    assert!(
        !sent.iter().any(|w| w.contains(decided.text())),
        "{sent:#?}"
    );
    assert!(
        ran.calls.contains(&format!("inform {}", typed.text())),
        "{:#?}",
        ran.calls
    );
}

#[test]
fn an_older_builds_bare_grant_types_the_line_as_decided() {
    let reread = |grant: bool| {
        move |h: &DoorHost, i: usize, at: i64| {
            *h.reread.lock().expect("reread") = (i == ROOM_DUE).then(|| room_with(at, 2));
            *h.pace.lock().expect("pace") = grant.then_some(PaceGrant::Granted);
        }
    };
    let decided = templates::hold_room(120 * MIN, 1, 3, 20 * 60_000, 96 * 1024);
    let ran = room_run("write-room-bare", Occupancy::Prompt, &[], &reread(true));
    assert_eq!(rooms_typed(&ran), [(120 * MIN, decided.text().to_string())]);
    let ran = room_run("write-room-reread", Occupancy::Prompt, &[], &reread(false));
    let typed = templates::hold_room(120 * MIN, 2, 3, 20 * 60_000, 96 * 1024);
    assert_eq!(rooms_typed(&ran), [(120 * MIN, typed.text().to_string())]);
}

fn typed_lines(ran: &Ran) -> Vec<(String, String)> {
    ran.rows
        .iter()
        .filter(|(rule, decision, _, _)| rule == HOLD && decision == "typed")
        .map(|(_, _, _, reason)| {
            let word = |key: &str| {
                reason
                    .split(' ')
                    .find_map(|w| w.strip_prefix(key))
                    .unwrap_or("")
                    .to_string()
            };
            (word("rule="), word("held_ms="))
        })
        .collect()
}

#[test]
fn a_persons_exchange_over_a_three_hour_shell_types_nothing() {
    let at_s: Vec<i64> = (0..=18).map(|i| i * 10 * 60).collect();
    let run = |occupancy, left: bool| {
        let host = Arc::new(DoorHost::default());
        *host.board.lock().expect("board") = room_at(T0);
        Script {
            host,
            ..Script::new("write-exchange-3h", occupancy, &at_s)
        }
        .run(&mut |s, i| {
            if left && !s.turn_end.left_to_person() {
                let mut reading =
                    s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
                reading.left_record = true;
                s.turn_end.observe(&reading, s.fx.mono());
                assert!(s.turn_end.left_to_person());
            }
            if !left && i == 4 {
                s.took(7, Some(&policy(occupancy)), &mut Heard::default());
            }
        })
    };
    let ran = run(Occupancy::Prompt, true);
    assert_eq!(
        wrote(&ran.noted),
        [said(NUDGE, "held:exchange"), said(ROOM, "held:exchange")],
        "{:#?}",
        ran.noted
    );
    assert!(ran.writes().is_empty(), "{:?}", ran.writes());
    assert!(ran.rows.is_empty(), "{:?}", ran.rows);
    assert!(!ran.requests().iter().any(|r| r == "status"));
    assert!(!ran.calls.iter().any(|c| c.starts_with("occupancy_pace")));
    assert_eq!(
        held_rows_of(&ran.ledger),
        [held_by(NUDGE, "exchange"), held_by(ROOM, "exchange")]
    );
    assert!(
        !ran.noted
            .iter()
            .any(|l| l.starts_with("OCCUPANCY ") && l.contains(" hold closed")),
        "the shell's hold stood three hours: {:#?}",
        ran.noted
    );
    observe_types_nothing(&|occupancy| run(occupancy, true));
    let control = run(Occupancy::Prompt, false);
    assert_eq!(
        typed_lines(&control),
        [
            (NUDGE.to_string(), (30 * MIN).to_string()),
            (ROOM.to_string(), (120 * MIN).to_string()),
        ],
        "{:#?}",
        control.noted
    );
}

#[test]
fn a_crash_after_the_intent_is_retold_once() {
    let first = Script::new("write-crash-1", Occupancy::Prompt, &[0, 31 * 60]).run(&mut nothing);
    assert_eq!(wrote(&first.noted), [said(NUDGE, "typed")]);
    let typed_row = |l: &&str| {
        l.contains("\"rule_id\":\"fact-hold@v1\"") && l.contains("\"decision\":\"typed\"")
    };
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
        let booked: Vec<(String, String)> = words
            .strip_prefix("held:")
            .map(|guard| held_by(NUDGE, guard))
            .into_iter()
            .collect();
        assert_eq!(held_rows_of(&ran.ledger), booked, "{tag}: booked once");
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

const HOLDER: &str = "aterm-harness@1";

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

/// The reads of a held session: its first, and one a minute from `from_min` to `to_min`.
fn minutely(from_min: i64, to_min: i64) -> Vec<i64> {
    std::iter::once(0)
        .chain((from_min..=to_min).map(|m| m * 60))
        .collect()
}

/// The told words' `ready_idle` lines, each with the wall it was told at.
fn ready_idles(ran: &Ran) -> Vec<(i64, Option<SaidReadyIdle>)> {
    ran.said.iter().map(|s| (s.at_ms, s.ready_idle)).collect()
}

/// The first told word that names a line ready-idle.
fn first_ready_idle(ran: &Ran) -> Option<&OccupancySaid> {
    ran.said.iter().find(|s| s.ready_idle.is_some())
}

#[test]
fn a_due_line_held_k_looks_is_said_ready_idle_under_prompt_alone() {
    let run = |occupancy| {
        let mut script = Script::new("write-k-land", occupancy, &minutely(31, 42));
        script.policy.autopilot = Autopilot::Land;
        script.run(&mut nothing)
    };
    let ran = run(Occupancy::Prompt);
    assert_eq!(wrote(&ran.noted), [said(NUDGE, "held:autopilot")]);
    let first = T0 + 31 * MIN;
    let raised = SaidReadyIdle {
        rule: RuleId::WaitNudge,
        guard: Some(Guard::Autopilot),
        since_ms: first,
    };
    let word = first_ready_idle(&ran).expect("raised");
    assert_eq!(
        word.at_ms,
        first + crate::supervise::occupancy::due::K_LOOKS * MIN,
        "at its K-th look: {:#?}",
        ready_idles(&ran)
    );
    assert_eq!(word.ready_idle, Some(raised));
    assert_eq!(word.ready_idle_under_prompt(), Some(raised));
    assert_eq!(
        ran.said.last().and_then(|s| s.ready_idle),
        Some(raised),
        "it stands while the line does"
    );
    assert!(ran.writes().is_empty(), "{:?}", ran.writes());
    // The clock runs in every mode; only `prompt`'s word names the line for the tab's Entry.
    let observe = run(Occupancy::Observe);
    assert_eq!(
        first_ready_idle(&observe).and_then(|s| s.ready_idle),
        Some(raised)
    );
    assert!(
        observe
            .said
            .iter()
            .all(|s| s.ready_idle_under_prompt().is_none()),
        "{:#?}",
        observe.said
    );
    assert!(run(Occupancy::Off).said.is_empty(), "off tells nothing");
    observe_types_nothing(&run);
}

#[test]
fn a_persons_hand_holds_the_ready_idle_line_and_a_handover_raises_it_at_once() {
    let run = |handover: Option<usize>| {
        Script {
            set: &|m| m.human_ms = Some(1_000),
            ..Script::new("write-k-person", Occupancy::Prompt, &minutely(31, 40))
        }
        .run(&mut |s, i| {
            if i == 0 {
                s.set_occupancy_k_looks(3);
            }
            if Some(i) == handover {
                s.ctl.m.human_ms = Some(60 * MIN.unsigned_abs());
                s.ctl.m.hold = "1";
            }
        })
    };
    let held = run(None);
    assert_eq!(wrote(&held.noted), [said(NUDGE, "held:person")]);
    assert!(
        first_ready_idle(&held).is_none(),
        "nine looks under a person's hand: {:#?}",
        ready_idles(&held)
    );
    let handed = run(Some(7));
    assert_eq!(
        wrote(&handed.noted),
        [said(NUDGE, "held:person"), said(NUDGE, "held:halted")]
    );
    let word = first_ready_idle(&handed).expect("raised at the handover");
    assert_eq!(word.at_ms, T0 + 37 * MIN, "{:#?}", ready_idles(&handed));
    assert_eq!(
        word.ready_idle,
        Some(SaidReadyIdle {
            rule: RuleId::WaitNudge,
            guard: Some(Guard::Halted),
            since_ms: T0 + 31 * MIN,
        }),
        "its clock from its first look, not from the halt"
    );
    assert!(handed.writes().is_empty(), "{:?}", handed.writes());
}

#[test]
fn a_line_the_write_holds_k_looks_is_ready_idle_until_it_is_typed() {
    let run = |typed: bool| {
        let host = Arc::new(DoorHost::default());
        host.taskless.store(true, Ordering::SeqCst);
        let free = Arc::clone(&host);
        Script {
            host,
            ..Script::new("write-k-taskless", Occupancy::Prompt, &minutely(31, 35))
        }
        .run(&mut |s, i| {
            if i == 0 {
                s.set_occupancy_k_looks(3);
            }
            if typed && i == 5 {
                free.taskless.store(false, Ordering::SeqCst);
            }
        })
    };
    let ran = run(true);
    assert_eq!(
        wrote(&ran.noted),
        [said(NUDGE, "held:taskless"), said(NUDGE, "typed")]
    );
    let raised = Some(SaidReadyIdle {
        rule: RuleId::WaitNudge,
        guard: Some(Guard::Taskless),
        since_ms: T0 + 31 * MIN,
    });
    let word = first_ready_idle(&ran).expect("raised");
    assert_eq!(
        (word.at_ms, word.ready_idle),
        (T0 + 34 * MIN, raised),
        "{:#?}",
        ready_idles(&ran)
    );
    let last = ran.said.last().expect("told");
    assert_eq!(
        (last.at_ms, last.ready_idle),
        (T0 + 35 * MIN, None),
        "typed: no longer due"
    );
    assert_eq!(ran.writes().len(), 2, "{:?}", ran.writes());
    let held = run(false);
    assert_eq!(held.said.last().and_then(|s| s.ready_idle), raised);
}

#[test]
fn a_successor_says_the_carried_line_ready_idle_when_its_predecessor_would_have() {
    let land = |tag: &'static str, at_s: &[i64], seed: Option<(String, i64)>| {
        let mut script = Script {
            seed,
            ..Script::new(tag, Occupancy::Prompt, at_s)
        };
        script.policy.autopilot = Autopilot::Land;
        script.run(&mut nothing)
    };
    let first = land("write-k-first", &minutely(31, 36), None);
    assert_eq!(wrote(&first.noted), [said(NUDGE, "held:autopilot")]);
    assert!(first_ready_idle(&first).is_none(), "five looks");
    assert_eq!(held_rows_of(&first.ledger), [held_by(NUDGE, "autopilot")]);
    // Each row at the wall of the read that wrote it: the hold's since plus its `held_ms`.
    let body: String = first
        .ledger
        .lines()
        .map(|line| {
            let held_ms = line
                .split(['"', ' '])
                .find_map(|w| w.strip_prefix("held_ms=")?.parse::<i64>().ok())
                .expect("each row's held_ms");
            stamped(line, T0 + held_ms)
        })
        .collect();
    let successor = || {
        let at_s: Vec<i64> = (0..=5).map(|m| m * 60).collect();
        land(
            "write-k-successor",
            &at_s,
            Some((body.clone(), T0 + 37 * MIN)),
        )
    };
    let carried = successor();
    let word = first_ready_idle(&carried).expect("raised");
    assert_eq!(
        word.at_ms,
        T0 + 41 * MIN,
        "ten looks from the predecessor's first: {:#?}",
        ready_idles(&carried)
    );
    assert_eq!(
        word.ready_idle,
        Some(SaidReadyIdle {
            rule: RuleId::WaitNudge,
            guard: Some(Guard::Autopilot),
            since_ms: T0 + 31 * MIN,
        })
    );
    let unseeded = fault::with(Fault::KUnseeded, successor);
    assert!(
        first_ready_idle(&unseeded).is_none(),
        "a clock from the successor's first look raises at 47 min: {:#?}",
        ready_idles(&unseeded)
    );
}

/// A PERSON AT THE K-TH LOOK HOLDS THE RAISE (the slice-2 plan's D2, S15): a line the write holds
/// taskless for K looks, whose K-th look's write finds a person's keys, is said ready-idle at no
/// look: the break leaves its look to the write, whose `status` read the person shows in, and no
/// word names the line while the person holds it. NEGATIVE CONTROL: the person arriving a look
/// past the K-th finds the line raised there, taskless, standing on, held now by the person.
#[test]
fn a_person_at_the_kth_look_holds_the_raise() {
    let run = |arrives: usize| {
        let host = Arc::new(DoorHost::default());
        host.taskless.store(true, Ordering::SeqCst);
        Script {
            host,
            ..Script::new("write-k-person-at-k", Occupancy::Prompt, &minutely(31, 36))
        }
        .run(&mut |s, i| {
            if i == 0 {
                s.set_occupancy_k_looks(3);
            }
            if i == arrives {
                s.ctl.m.human_ms = Some(1_000);
            }
        })
    };
    let at_k = run(4);
    assert_eq!(
        wrote(&at_k.noted),
        [said(NUDGE, "held:taskless"), said(NUDGE, "held:person")]
    );
    assert!(
        first_ready_idle(&at_k).is_none(),
        "a person at its K-th look: {:#?}",
        ready_idles(&at_k)
    );
    let raised = SaidReadyIdle {
        rule: RuleId::WaitNudge,
        guard: Some(Guard::Taskless),
        since_ms: T0 + 31 * MIN,
    };
    let after = run(5);
    let word = first_ready_idle(&after).expect("raised at its K-th look");
    assert_eq!(
        (word.at_ms, word.ready_idle),
        (T0 + 34 * MIN, Some(raised)),
        "{:#?}",
        ready_idles(&after)
    );
    assert_eq!(
        after.said.last().and_then(|s| s.ready_idle),
        Some(SaidReadyIdle {
            guard: Some(Guard::Person),
            ..raised
        })
    );
    assert!(at_k.writes().is_empty(), "{:?}", at_k.writes());
    assert!(after.writes().is_empty(), "{:?}", after.writes());
}

/// A SUCCESSOR COUNTS A CRASHED LINE FROM ITS INTENT, AND A TYPED ONE NOT AT ALL: a nudge whose
/// write crashed after its intent row (its typed row lost), carried by a successor whose
/// autopilot holds it, is said ready-idle K looks from that row; the same nudge typed, its typed
/// row kept beside the same intent row, is the hold's nudge spent, and no word names a line.
/// NEGATIVE CONTROL: `KUnseeded` counts the crashed line from the successor's first look, ten
/// looks on.
#[test]
fn a_successor_counts_a_crashed_line_from_its_intent_and_a_typed_one_not_at_all() {
    let first = Script::new("write-k-intent-1", Occupancy::Prompt, &[0, 31 * 60]).run(&mut nothing);
    assert_eq!(wrote(&first.noted), [said(NUDGE, "typed")]);
    let typed_row = |l: &&str| {
        l.contains("\"rule_id\":\"fact-hold@v1\"") && l.contains("\"decision\":\"typed\"")
    };
    assert!(first.ledger.lines().any(|l| typed_row(&l)));
    let crashed: String = first
        .ledger
        .lines()
        .filter(|l| !typed_row(l))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(crashed.contains(INTENT));
    let successor = |tag: &'static str, body: &str| {
        let at_s: Vec<i64> = (0..=12).map(|m| m * 60).collect();
        let mut script = Script {
            seed: Some((stamped(body, T0 + 31 * MIN), T0 + 35 * MIN)),
            ..Script::new(tag, Occupancy::Prompt, &at_s)
        };
        script.policy.autopilot = Autopilot::Land;
        script.run(&mut nothing)
    };
    let carried = successor("write-k-intent-crashed", &crashed);
    assert_eq!(wrote(&carried.noted), [said(NUDGE, "held:autopilot")]);
    let raised = SaidReadyIdle {
        rule: RuleId::WaitNudge,
        guard: Some(Guard::Autopilot),
        since_ms: T0 + 31 * MIN,
    };
    let word = first_ready_idle(&carried).expect("raised");
    assert_eq!(
        (word.at_ms, word.ready_idle),
        (T0 + 41 * MIN, Some(raised)),
        "ten looks from its intent: {:#?}",
        ready_idles(&carried)
    );
    let typed = successor("write-k-intent-typed", &first.ledger);
    assert!(wrote(&typed.noted).is_empty(), "{:#?}", typed.noted);
    assert!(
        first_ready_idle(&typed).is_none(),
        "a typed line stands typed: {:#?}",
        ready_idles(&typed)
    );
    assert!(carried.writes().is_empty() && typed.writes().is_empty());
    let unseeded = fault::with(Fault::KUnseeded, || {
        successor("write-k-intent-unseeded", &crashed)
    });
    assert_eq!(
        first_ready_idle(&unseeded).map(|s| (s.at_ms, s.ready_idle)),
        Some((
            T0 + 45 * MIN,
            Some(SaidReadyIdle {
                since_ms: T0 + 35 * MIN,
                ..raised
            })
        )),
        "{:#?}",
        ready_idles(&unseeded)
    );
}

/// OF TWO LINES STANDING RAISED, THE WORD NAMES THE ONE DUE LONGEST: a person's exchange holds the
/// nudge from 30 minutes and the room line from two hours, each past K looks by the last read (at
/// three hours), and every word from the nudge's raise on names the nudge, from its first look:
/// none moves to the room line at its own raise.
#[test]
fn of_two_lines_raised_the_word_names_the_one_due_longest() {
    let at_s: Vec<i64> = (0..=18).map(|i| i * 10 * 60).collect();
    let host = Arc::new(DoorHost::default());
    *host.board.lock().expect("board") = room_at(T0);
    let ran = Script {
        host,
        ..Script::new("write-k-longest", Occupancy::Prompt, &at_s)
    }
    .run(&mut |s, i| {
        if i == 0 {
            s.set_occupancy_k_looks(3);
        }
        if !s.turn_end.left_to_person() {
            let mut reading = s.turn_end_reading(&screen_at(90, ended(SAID)), None, &hosted(30));
            reading.left_record = true;
            s.turn_end.observe(&reading, s.fx.mono());
        }
    });
    assert_eq!(
        wrote(&ran.noted),
        [said(NUDGE, "held:exchange"), said(ROOM, "held:exchange")]
    );
    let nudge = SaidReadyIdle {
        rule: RuleId::WaitNudge,
        guard: Some(Guard::Exchange),
        since_ms: T0 + 30 * MIN,
    };
    let raised: Vec<(i64, Option<SaidReadyIdle>)> = ready_idles(&ran)
        .into_iter()
        .filter(|(_, idle)| idle.is_some())
        .collect();
    assert_eq!(raised.first(), Some(&(T0 + 40 * MIN, Some(nudge))));
    assert!(
        raised.iter().all(|(_, idle)| *idle == Some(nudge)),
        "{raised:#?}"
    );
    let both = ran
        .said
        .iter()
        .find(|s| {
            s.rules.iter().all(|r| {
                r.verdict == crate::supervise::occupancy::SaidVerdict::Held(Guard::Exchange)
            })
        })
        .expect("both lines held");
    assert_eq!(
        both.at_ms,
        T0 + 120 * MIN,
        "the room line due from two hours"
    );
}

fn live_turn() -> Vec<String> {
    let mut r = rows(&["⏺ Working.", "", "✻ Synthesizing… (18s)", ""]);
    r.extend(composer("  esc to interrupt"));
    r
}

/// A managed worker's breaks every ten minutes to 130, its nudge put past the run (the setup's
/// wait a day) and the board's room standing: the room line is due from 120 minutes on. At
/// `close_at` a live turn not the harness's is drawn, and closes the hold.
fn managed_room(
    tag: &str,
    occupancy: Occupancy,
    autopilot: Autopilot,
    close_at: Option<usize>,
) -> Ran {
    let at_s: Vec<i64> = (0..=13).map(|m| m * 10 * 60).collect();
    let host = Arc::new(DoorHost::default());
    *host.board.lock().expect("board") = room_at(T0);
    let drawn = |i| {
        if close_at == Some(i) {
            live_turn()
        } else {
            waiting_break()
        }
    };
    let mut script = Script {
        drawn: &drawn,
        host,
        ..Script::new(tag, occupancy, &at_s)
    };
    script.policy.autopilot = autopilot;
    let cfg = script.policy.clone();
    script.run(&mut |s, i| {
        s.turn_end.set_managed(true);
        s.setup.occupancy_nudge_after = Duration::from_secs(24 * 3600);
        if close_at == Some(i) {
            s.hold_live(90, Some(&cfg), &mut Heard::default());
        }
    })
}

fn task_facts(ran: &Ran) -> Vec<String> {
    ran.calls
        .iter()
        .filter(|c| c.starts_with("task_fact "))
        .cloned()
        .collect()
}

fn task_kinds(ran: &Ran) -> Vec<(String, String)> {
    task_rows(&ran.ledger)
        .into_iter()
        .map(|(rule, decision, _, _)| (rule, decision))
        .collect()
}

fn row(rule: &str, decision: &str) -> (String, String) {
    (rule.to_string(), decision.to_string())
}

/// THE MANAGER DOOR (the slice-2 plan's D1, W3): under `prompt` a managed Cruise worker's due
/// room line, held by the managed guard alone, is noted once on its task record — its intent
/// row, one `task_fact` with the episode's word, its `noted` row — and never typed, however many
/// breaks stand past due. NEGATIVE CONTROLS: `NoteAndType` (the managed guard dropped from the
/// break's guards) sends the line as well as noting it, and `NoteEachBreak` notes the episode at
/// every break past due.
#[test]
fn a_managed_cruise_workers_due_room_line_is_noted_once_and_never_typed() {
    let ran = managed_room(
        "write-task-room",
        Occupancy::Prompt,
        Autopilot::Cruise,
        None,
    );
    assert_eq!(task_facts(&ran), [format!("task_fact {ROOM}#1")]);
    assert!(ran.writes().is_empty(), "{:?}", ran.writes());
    assert!(
        !ran.calls.iter().any(|c| c.starts_with("occupancy_pace")),
        "a note draws no pace: {:?}",
        ran.calls
    );
    assert_eq!(room_said(&ran), ["held:managed"], "{:#?}", ran.noted);
    assert!(ran.rows.is_empty(), "no line's rows: {:?}", ran.rows);
    assert_eq!(
        task_kinds(&ran),
        [row(INTENT, "waiting"), row(HOLD, "waiting")]
    );
    let rows = task_rows(&ran.ledger);
    assert_eq!(rows[0].2, "", "the intent names no word");
    assert_eq!(
        rows[1].2,
        format!("{ROOM}#1"),
        "the outcome row names the word"
    );
    assert!(rows[0].3.ends_with(" door=task"), "{rows:?}");
    assert!(rows[1].3.ends_with(" door=task noted"), "{rows:?}");
    let episode = |reason: &str| {
        reason
            .split(' ')
            .find(|w| w.starts_with("episode="))
            .map(str::to_string)
    };
    assert!(episode(&rows[0].3).is_some());
    assert_eq!(episode(&rows[0].3), episode(&rows[1].3));
    assert!(
        ran.noted.iter().any(|l| l.starts_with("OCCUPANCY ")
            && l.contains(&format!(" rule={ROOM} "))
            && l.ends_with(" door=task noted")),
        "{:#?}",
        ran.noted
    );
    observe_types_nothing(&|occupancy| {
        managed_room("write-task-room-obs", occupancy, Autopilot::Cruise, None)
    });

    let typed = fault::with(Fault::NoteAndType, || {
        managed_room(
            "write-task-and-type",
            Occupancy::Prompt,
            Autopilot::Cruise,
            None,
        )
    });
    assert_eq!(task_facts(&typed), [format!("task_fact {ROOM}#1")]);
    assert!(
        typed.writes().iter().any(|w| w.starts_with("send ")),
        "CONTROL: a note that keeps no guard types the line too: {:?}",
        typed.writes()
    );
    let each = fault::with(Fault::NoteEachBreak, || {
        managed_room(
            "write-task-each",
            Occupancy::Prompt,
            Autopilot::Cruise,
            None,
        )
    });
    assert_eq!(
        task_facts(&each),
        [format!("task_fact {ROOM}#1"), format!("task_fact {ROOM}#1")],
        "CONTROL: noted again at the next break past due"
    );
}

/// The hold's close clears the word the door noted (`task_fact(None)`) and books its row; a
/// clear the host fenced is owed and made at the next step, and under no grant none is asked.
#[test]
fn the_holds_close_clears_the_word_the_door_noted() {
    let ran = managed_room(
        "write-task-close",
        Occupancy::Prompt,
        Autopilot::Cruise,
        Some(13),
    );
    assert_eq!(
        task_facts(&ran),
        [format!("task_fact {ROOM}#1"), "task_fact -".to_string()]
    );
    assert_eq!(
        task_kinds(&ran),
        [
            row(INTENT, "waiting"),
            row(HOLD, "waiting"),
            row(HOLD, "waiting")
        ]
    );
    let rows = task_rows(&ran.ledger);
    assert_eq!(
        (rows[2].2.as_str(), rows[2].3.as_str()),
        (
            "hold-room@v1#1",
            "rule=hold-room@v1 fact=hold-room@v1#1 door=task cleared"
        )
    );
    assert!(ran.writes().is_empty(), "{:?}", ran.writes());
    let unnoted = managed_room(
        "write-task-close-observe",
        Occupancy::Observe,
        Autopilot::Cruise,
        Some(13),
    );
    assert_eq!(task_facts(&unnoted), Vec::<String>::new());
    assert_eq!(
        task_kinds(&unnoted),
        [row(
            crate::supervise::occupancy::rules::WOULD_RULE,
            "waiting"
        )],
        "observe books the would-be note, and no clear"
    );
}

/// Under Land a managed worker's due nudge is held by the autopilot's guard too, so the task
/// door notes nothing: no line would have gone but for the manager. NEGATIVE CONTROL:
/// `NoteOnManagedAlone` (the managed guard's verdict alone asked) notes it.
#[test]
fn a_land_workers_managed_nudge_is_never_noted() {
    let run = |tag: &str| {
        let mut script = Script::new(tag, Occupancy::Prompt, &[0, 31 * 60, 32 * 60]);
        script.policy.autopilot = Autopilot::Land;
        script.run(&mut |s, _| s.turn_end.set_managed(true))
    };
    let ran = run("write-task-land");
    assert_eq!(wrote(&ran.noted), [said(NUDGE, "held:managed")]);
    assert_eq!(task_facts(&ran), Vec::<String>::new());
    assert_eq!(task_rows(&ran.ledger), Vec::new());
    assert!(ran.writes().is_empty());
    let alone = fault::with(Fault::NoteOnManagedAlone, || run("write-task-land-alone"));
    assert_eq!(
        task_facts(&alone),
        [format!("task_fact {NUDGE}#1")],
        "CONTROL: the managed guard's verdict alone notes the Land worker's line"
    );
}

/// T-obs2 for the task door: under `observe` a managed Cruise worker's due nudge books one
/// would-be note (`fact-would@v1 … door=task`) however many breaks stand past due, and asks the
/// host nothing; under `off`, nothing at all. NEGATIVE CONTROL: `NoteUnderObserve` asks it.
#[test]
fn under_observe_the_task_door_books_a_would_row_and_asks_nothing() {
    let run = |tag: &str, occupancy| {
        Script::new(tag, occupancy, &[0, 31 * 60, 32 * 60])
            .run(&mut |s, _| s.turn_end.set_managed(true))
    };
    let would = crate::supervise::occupancy::rules::WOULD_RULE;
    let ran = run("write-task-observe", Occupancy::Observe);
    assert_eq!(task_facts(&ran), Vec::<String>::new());
    assert!(ran.writes().is_empty());
    let rows = task_rows(&ran.ledger);
    assert_eq!(
        rows.iter()
            .map(|(r, d, c, _)| (r.as_str(), d.as_str(), c.as_str()))
            .collect::<Vec<_>>(),
        [(would, "waiting", "")]
    );
    assert!(rows[0].3.starts_with(&format!("rule={NUDGE} ")), "{rows:?}");
    assert!(rows[0].3.ends_with(" door=task"), "{rows:?}");
    assert!(
        ran.rows.is_empty(),
        "the typed door's would-be rows hold none: {:?}",
        ran.rows
    );
    assert_eq!(
        crate::supervise::occupancy::rules::would_rows_in(&ran.ledger, Some("@s-1")),
        Vec::new(),
        "a successor's books count no note"
    );
    let off = run("write-task-off", Occupancy::Off);
    assert_eq!(task_rows(&off.ledger), Vec::new());
    assert_eq!(task_facts(&off), Vec::<String>::new());
    let asked = fault::with(Fault::NoteUnderObserve, || {
        run("write-task-observe-asked", Occupancy::Observe)
    });
    assert_eq!(
        task_facts(&asked),
        [format!("task_fact {NUDGE}#1")],
        "CONTROL: a door that asks under observe is seen"
    );
}

/// The host's answer that took no word is booked `skipped why=<word>`: `no-task` and `closed`
/// once an episode, `fenced` tried again at the next break.
#[test]
fn a_word_the_host_took_not_is_skipped_and_a_fenced_one_tried_again() {
    for (outcome, calls) in [
        (NoteOutcome::NoTask, 1),
        (NoteOutcome::Closed, 1),
        (NoteOutcome::Fenced, 2),
    ] {
        let tag = format!("write-task-{}", outcome.word());
        let host = Arc::new(DoorHost::default());
        *host.note.lock().expect("note") = Some(outcome);
        let ran = Script {
            host,
            ..Script::new(&tag, Occupancy::Prompt, &[0, 31 * 60, 32 * 60])
        }
        .run(&mut |s, _| s.turn_end.set_managed(true));
        assert_eq!(task_facts(&ran).len(), calls, "{tag}: {:?}", ran.calls);
        let skipped: Vec<String> = task_rows(&ran.ledger)
            .into_iter()
            .filter(|(rule, decision, _, _)| rule == HOLD && decision == "skipped")
            .map(|(_, _, _, reason)| reason)
            .collect();
        assert_eq!(skipped.len(), calls, "{tag}");
        assert!(
            skipped
                .iter()
                .all(|r| r.ends_with(&format!(" door=task why={}", outcome.word()))),
            "{tag}: {skipped:?}"
        );
        assert!(ran.writes().is_empty(), "{tag}");
        assert_eq!(
            crate::supervise::occupancy::door::seed_rows_in(&ran.ledger, Some("@s-1")),
            Vec::new(),
            "{tag}: a successor's facts take no note's intent or skip for a line's"
        );
    }
}

/// A word the record carries at the loop's first read of it (`task_seed`), with no hold open,
/// stands for no episode: under `prompt` it is cleared at once, its row booked; under `observe`
/// and `off` nothing is asked. With a hold open, the clear waits for that hold's close.
#[test]
fn a_standing_word_with_no_hold_open_is_cleared_at_the_seed_under_prompt_alone() {
    let word = FactWord::new(RuleId::HoldRoom, 1);
    let run = |tag: &str, occupancy, at: usize| {
        Script::new(tag, occupancy, &[0, 10 * 60]).run(&mut |s, i| {
            if i == at {
                s.door_task_seed(word, &policy(occupancy));
            }
        })
    };
    let ran = run("write-task-seed", Occupancy::Prompt, 0);
    assert_eq!(task_facts(&ran), ["task_fact -"]);
    assert_eq!(
        task_rows(&ran.ledger),
        [(
            HOLD.to_string(),
            "waiting".to_string(),
            "hold-room@v1#1".to_string(),
            "rule=hold-room@v1 fact=hold-room@v1#1 door=task cleared".to_string()
        )]
    );
    for occupancy in [Occupancy::Observe, Occupancy::Off] {
        let quiet = run("write-task-seed-quiet", occupancy, 0);
        assert_eq!(task_facts(&quiet), Vec::<String>::new(), "{occupancy:?}");
        assert_eq!(task_rows(&quiet.ledger), Vec::new(), "{occupancy:?}");
    }
    let open = run("write-task-seed-open", Occupancy::Prompt, 1);
    assert_eq!(
        task_facts(&open),
        Vec::<String>::new(),
        "the open hold's close clears it"
    );
}

/// A standing word seeded with a hold open is taken up as the hold's (`DoorWatch::task_word`):
/// that hold's close clears it under `prompt`, its row booked, and asks nothing under `observe`.
/// CONTROL: the same close with no word seeded asks no clear, so the seeded word alone made it.
#[test]
fn a_word_seeded_under_an_open_hold_is_cleared_at_that_holds_close() {
    let word = FactWord::new(RuleId::HoldRoom, 1);
    let run = |tag: &str, occupancy, seeded: bool| {
        let cfg = policy(occupancy);
        let drawn = |i| if i == 2 { live_turn() } else { break_screen() };
        Script {
            drawn: &drawn,
            ..Script::new(tag, occupancy, &[0, 10 * 60, 20 * 60])
        }
        .run(&mut |s, i| {
            if i == 1 && seeded {
                s.door_task_seed(word, &cfg);
            }
            if i == 2 {
                s.hold_live(90, Some(&cfg), &mut Heard::default());
            }
        })
    };
    let ran = run("write-task-seed-close", Occupancy::Prompt, true);
    assert_eq!(task_facts(&ran), ["task_fact -"], "{:?}", ran.calls);
    assert_eq!(
        task_rows(&ran.ledger),
        [(
            HOLD.to_string(),
            "waiting".to_string(),
            "hold-room@v1#1".to_string(),
            "rule=hold-room@v1 fact=hold-room@v1#1 door=task cleared".to_string()
        )]
    );
    let observed = run("write-task-seed-close-observe", Occupancy::Observe, true);
    assert_eq!(task_facts(&observed), Vec::<String>::new());
    assert_eq!(task_rows(&observed.ledger), Vec::new());
    let unseeded = run("write-task-seed-close-none", Occupancy::Prompt, false);
    assert_eq!(
        task_facts(&unseeded),
        Vec::<String>::new(),
        "CONTROL: a close with no word seeded asks no clear"
    );
}

/// A TAPE SLICE 2'S DOORS DID NOT RECORD HAS NO TASK DOOR (the slice-2 plan's F2 and D7): on a
/// replay whose header names a slice-1 build's doors, a line the managed guard alone holds is no
/// task door's (`door_task_due`), and a word the record carries at the seed is not cleared
/// (`door_task_seed`), so such a tape's loop asks `host.task_fact` nothing it never recorded.
/// NEGATIVE CONTROL: the same loop under a header with slice 2's doors in takes the line and asks
/// the clear (answered `fenced`, so it books no row).
#[test]
fn a_tape_without_slice_twos_doors_has_no_task_door() {
    use crate::supervise::occupancy::rules::{Guards, HoldSeen, Verdict};
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::TAPE_ID;
    const SLICE2: [&str; 5] = [
        "host.wakes",
        "host.wake_claim",
        "host.wake_release",
        "host.wake_told",
        "host.task_fact",
    ];
    let cfg = policy(Occupancy::Prompt);
    let held = [(RuleId::HoldRoom, Verdict::Held(Guard::Managed))];
    let guards = Guards {
        managed: true,
        ..Guards::clear(Autopilot::Cruise)
    };
    let hold = HoldSeen {
        since_ms: 0,
        declared: false,
    };
    let run = |slice2: bool| {
        let host = Arc::new(DoorHost::default());
        *host.note.lock().expect("note") = Some(NoteOutcome::Fenced);
        let mut m = Mock::new(Vec::new());
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        let start = s.fx.mono();
        s.hold_seed_from("", start, T0, false, &cfg, &Heard::default());
        let doors: Vec<String> = crate::supervise::run::host_door::HOST_DOORS
            .iter()
            .filter(|door| slice2 || !SLICE2.contains(door))
            .map(ToString::to_string)
            .collect();
        let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
        core.recorded_doors(doors);
        s.set_fx(Fx::replay(core));
        assert_eq!(s.fx.slice2_recorded(), slice2);
        let due = s.door_task_due(&held, &guards, hold).is_some();
        s.door_task_seed(FactWord::new(RuleId::HoldRoom, 1), &cfg);
        (due, host.calls())
    };
    assert_eq!(run(false), (false, Vec::new()));
    assert_eq!(
        run(true),
        (true, vec!["task_fact -".to_string()]),
        "CONTROL: slice 2's doors in, the door takes the line and asks the clear"
    );
}

/// A MANAGED WORKER'S LINE BEFORE THE LOOP'S FIRST FOLD (its first turn, or a loop that carried
/// an open hold; 2.4's safety review): the record no fold has read yet is read once, at the write
/// past the owner's grant (`Session::managed_at_write`), and the managed guard holds the due
/// nudge there; the next break notes it on the record, and nothing is typed. A read the server
/// turned away holds the line, and the next write reads again. Under `observe` and `off` the
/// record is not read. NEGATIVE CONTROLS: `ManagedUnread` (no read at the write) types the nudge
/// into the managed worker, and a session with no task record is typed it after the same one
/// read.
#[test]
fn a_managed_workers_line_before_its_first_fold_is_held_by_the_record_read_at_the_write() {
    let working = Some((
        1,
        "state=working from=ctl since_ms=0 result_bytes=0".to_string(),
    ));
    let run_lost = |tag: &str, occupancy, task: &Option<(u64, String)>, lost: usize| {
        let set = |m: &mut Mock| {
            m.task.clone_from(task);
            m.task_lost = lost;
        };
        Script {
            set: &set,
            ..Script::new(tag, occupancy, &[0, 31 * 60, 32 * 60])
        }
        .run(&mut |s, _| s.tasks_tracked = true)
    };
    let run =
        |tag: &str, occupancy, task: &Option<(u64, String)>| run_lost(tag, occupancy, task, 0);
    let reads = |ran: &Ran| ran.requests().iter().filter(|r| *r == "task").count();
    let ran = run("write-managed-unread", Occupancy::Prompt, &working);
    assert!(ran.writes().is_empty(), "{:?}", ran.writes());
    assert_eq!(reads(&ran), 1, "{:?}", ran.requests());
    assert_eq!(task_facts(&ran), [format!("task_fact {NUDGE}#1")]);
    assert!(ran.rows.is_empty(), "no line's rows: {:?}", ran.rows);
    observe_types_nothing(&|occupancy| {
        let ran = run("write-managed-unread-observe", occupancy, &working);
        assert_eq!(reads(&ran), 0, "{occupancy:?}: {:?}", ran.requests());
        ran
    });
    let typed = fault::with(Fault::ManagedUnread, || {
        run("write-managed-unread-typed", Occupancy::Prompt, &working)
    });
    assert_eq!(reads(&typed), 0);
    assert!(
        typed.writes().iter().any(|w| w.starts_with("send ")),
        "CONTROL: a write that reads no record types into the managed worker: {:?}",
        typed.writes()
    );
    let lost = run_lost("write-managed-lost", Occupancy::Prompt, &working, 1);
    assert!(lost.writes().is_empty(), "{:?}", lost.writes());
    assert_eq!(
        reads(&lost),
        2,
        "read again at the next write: {:?}",
        lost.requests()
    );
    assert_eq!(task_facts(&lost), Vec::<String>::new());
    let unmanaged = run("write-managed-none", Occupancy::Prompt, &None);
    assert_eq!(reads(&unmanaged), 1);
    assert!(
        unmanaged.writes().iter().any(|w| w.starts_with("send ")),
        "CONTROL: a session with no task record is typed its line: {:?}",
        unmanaged.writes()
    );
}

#[path = "facts_loop_tests.rs"]
mod facts_loop;
