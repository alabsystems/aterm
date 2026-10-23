// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Occupancy hold loop tests (`run/occupancy_loop.rs`).

use super::*;
use crate::supervise::config::Occupancy;
use crate::supervise::journal::JournalRecord;
use crate::supervise::policy::turn_end::{RULE_COMPACT, Then};
use crate::supervise::run::occupancy_loop::CARRY_GAP_MS;

const T0: i64 = 1_760_000_000_000;

/// A minute, in the clock's milliseconds.
const MIN: i64 = 60_000;

/// Claude Code's break screen whose last words declare the wait.
fn waiting_break() -> Vec<String> {
    let mut r = rows(&[
        "⏺ WAITING: the mint, about 6 h; nothing else is ready.",
        "",
        "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
        "",
    ]);
    r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
    r
}

/// An `OCCUPANCY` record: its seq, the measure, and the change's first word.
type Said = (Option<u64>, String, String);

/// A `fact-hold@v1` row: its decision and its reason's words.
type HoldRow = (String, Vec<String>);

/// The `OCCUPANCY` records of `noted`.
fn occupancy(noted: &[String]) -> Vec<Said> {
    noted
        .iter()
        .map(|line| JournalRecord::of_line(0, None, line, None))
        .filter(|r| r.kind == "occupancy")
        .map(|r| {
            let change = r.summary.split(' ').next().unwrap_or("").to_string();
            (r.seq, r.phase, change)
        })
        .collect()
}

/// `(seq, word)` as an `OCCUPANCY` record of the hold.
fn change(seq: u64, word: &str) -> Said {
    (Some(seq), "hold".to_string(), word.to_string())
}

/// The `fact-hold@v1` rows of the ledger at `path`.
fn hold_rows(path: &Path) -> Vec<HoldRow> {
    let rule = crate::supervise::facts::FactKind::Hold.rule();
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|v| v.get("rule_id").and_then(aterm_json::Value::as_str) == Some(rule))
        .map(|v| {
            let text = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let words = text("reason").split(' ').map(str::to_string).collect();
            (text("decision"), words)
        })
        .collect()
}

/// The reasons of `rows`, each its words; every row `waiting` (no act follows one).
fn reasons(rows: &[HoldRow]) -> Vec<Vec<String>> {
    assert!(rows.iter().all(|(d, _)| d == "waiting"), "{rows:?}");
    rows.iter().map(|(_, w)| w.clone()).collect()
}

/// A reason's words, as [`reasons`] gives them.
fn said(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The `held_ms` a row's words name.
fn held_ms(words: &[String]) -> i64 {
    words
        .iter()
        .find_map(|w| w.strip_prefix("held_ms=")?.parse().ok())
        .unwrap_or_else(|| panic!("no held_ms in {words:?}"))
}

/// A session on scripted time at `T0`, its ledger at `path`, the host `host` and the break's
/// settle `settle`.
fn hold_session<'m>(
    m: &'m mut Mock,
    path: &Path,
    host: Option<Arc<dyn IdleHost>>,
    settle: Duration,
) -> (Session<'m, Mock>, Arc<crate::supervise::fx::ScriptedClock>) {
    let clock = Arc::new(crate::supervise::fx::ScriptedClock::default());
    clock.stand_mono();
    clock.set_at_ms(T0);
    let mut s = session(m, Some("@s-1".to_string()));
    s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
    s.ledger = approvals::Ledger::open(Some(path), Some("@s-1"), &mut Vec::new());
    s.stall_host = host;
    s.setup.background_settle = settle;
    (s, clock)
}

/// `clock` moved on `ms`, its wall time with it.
fn skip(clock: &crate::supervise::fx::ScriptedClock, at_ms: &mut i64, ms: i64) {
    clock.skip(Duration::from_millis(u64::try_from(ms).expect("forward")));
    *at_ms += ms;
    clock.set_at_ms(*at_ms);
}

/// One run of a break that stands 11 min (its first read inside the settle), then the agent's
/// own wake: the `OCCUPANCY` records, the hold rows and the requests the session made.
fn one_hold(
    tag: &str,
    hosted: bool,
    policy: Option<SupervisorConfig>,
    attended: bool,
) -> (Vec<Said>, Vec<HoldRow>, Vec<String>) {
    let (dir, path) = ledger_at(tag);
    let mut m = Mock::new(Vec::new());
    let host = hosted.then(|| Arc::new(Withholds::default()) as Arc<dyn IdleHost>);
    let settle = crate::supervise::run::background_settle();
    let noted = {
        let (mut s, clock) = hold_session(&mut m, &path, host, settle);
        let mut heard = Heard::default();
        let mut person = Attended::default();
        let review: &mut dyn Review = if attended { &mut person } else { &mut heard };
        let cfg = policy.as_ref();
        let mut since = None;
        let mut read = |s: &mut Session<'_, Mock>, seq: u64, review: &mut dyn Review| {
            s.host_steps_in_background(&screen_at(seq, break_screen()), &mut since, cfg, review)
                .expect("the break's step");
        };
        read(&mut s, 1, review);
        clock.skip(settle);
        read(&mut s, 2, review);
        clock.skip(Duration::from_secs(11 * 60));
        clock.set_at_ms(T0 + 11 * MIN);
        read(&mut s, 3, review);
        s.took(4, cfg, review);
        if attended {
            person.noted.clone()
        } else {
            heard.noted.clone()
        }
    };
    let rows = hold_rows(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (occupancy(&noted), rows, m.requests)
}

/// UNDER OBSERVE, ON A HOSTED UNATTENDED LOOP, A HOLD IS JOURNALED ONCE A CHANGE AND BOOKED ONCE
/// IT HAS STOOD 10 MIN: opened at the settled read (none inside the settle), its open row at the
/// read past 10 min, closed at the agent's own wake with its close row — `waiting` rows that
/// type nothing; and the session made exactly the requests it makes with occupancy off.
#[test]
fn a_hold_is_said_once_a_change_on_a_hosted_unattended_loop_only() {
    let observe = SupervisorConfig::default();
    assert_eq!(observe.occupancy, Occupancy::Observe);
    let (noted, rows, requests) = one_hold("occ-observe", true, Some(observe.clone()), false);
    assert_eq!(noted, [change(2, "open"), change(4, "closed")]);
    assert_eq!(
        reasons(&rows),
        [
            said(&["hold=open", "held_ms=680000"]),
            said(&["hold=closed", "why=foreign", "held_ms=680000"]),
        ]
    );
    let off = SupervisorConfig {
        occupancy: Occupancy::Off,
        ..observe.clone()
    };
    for (tag, hosted, policy, attended) in [
        ("occ-off", true, Some(off), false),
        ("occ-unhosted", false, Some(observe.clone()), false),
        ("occ-attended", true, Some(observe.clone()), true),
        ("occ-no-policy", true, None, false),
    ] {
        let (noted, rows, quiet) = one_hold(tag, hosted, policy, attended);
        assert!(noted.is_empty(), "{tag}: {noted:?}");
        assert!(rows.is_empty(), "{tag}: {rows:?}");
        if tag == "occ-off" {
            assert_eq!(
                requests, quiet,
                "observe asks the server nothing off does not"
            );
        }
    }
}

/// A DECIDED POINT, through the loop's own executor (F5): nothing decided while the background
/// work still shows keeps the hold; nothing decided, or a wait, with no background work on the
/// screen closes it.
#[test]
fn a_point_keeps_the_hold_only_over_the_work() {
    let (dir, path) = ledger_at("occ-point");
    let mut m = Mock::new(vec![ended(SAID)]);
    let host = Arc::new(Withholds::default()) as Arc<dyn IdleHost>;
    let (mut s, _clock) = hold_session(&mut m, &path, Some(host), Duration::ZERO);
    let opts = hosted(30);
    let mut heard = Heard::default();
    let mut since = None;
    let cfg = Some(&opts.policy);
    s.host_steps_in_background(&screen_at(1, break_screen()), &mut since, cfg, &mut heard)
        .expect("the break's step");
    for seq in 2..5 {
        let point = s.turn_of(screen_at(seq, break_screen()));
        s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
            .expect("the point's act");
    }
    assert_eq!(
        occupancy(&heard.noted),
        [change(1, "open")],
        "kept over the work"
    );
    let point = s.turn_of(screen_at(5, ended(SAID)));
    s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
        .expect("the point's act");
    s.host_steps_in_background(&screen_at(6, break_screen()), &mut since, cfg, &mut heard)
        .expect("the break's step");
    let point = s.turn_of(screen_at(7, ended(SAID)));
    s.turn_end_execute(&point, wait_a_while(), &opts, &[], &mut heard)
        .expect("the point's act");
    assert_eq!(
        occupancy(&heard.noted),
        [
            change(1, "open"),
            change(5, "closed"),
            change(6, "open"),
            change(7, "closed"),
        ]
    );
    drop(s);
    assert!(hold_rows(&path).is_empty(), "no hold stood 10 min");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A wait the policy decides at a point.
fn wait_a_while() -> TurnEndAction {
    TurnEndAction::WaitUntil {
        until: Instant::now() + Duration::from_secs(600),
        why: "a person typing".to_string(),
        wait: WaitKind::Other,
    }
}

/// WHAT SHOWS THE AGENT'S BACKGROUND WORK AT A POINT (F5, the review of 1a's D2, M8 and M13):
/// a hold opened at a break, then one point decided on `rows` (its seq 2, the transcript's wait
/// kept for it where `transcript`) with `action`: whether the hold still stands.
fn kept_at_point(rows: Vec<String>, transcript: bool, action: TurnEndAction) -> bool {
    let (dir, path) = ledger_at("occ-shows");
    let mut m = Mock::new(Vec::new());
    let host = Arc::new(Withholds::default()) as Arc<dyn IdleHost>;
    let (mut s, _clock) = hold_session(&mut m, &path, Some(host), Duration::ZERO);
    let opts = hosted(30);
    let mut heard = Heard::default();
    let mut since = None;
    s.host_steps_in_background(
        &screen_at(1, break_screen()),
        &mut since,
        Some(&opts.policy),
        &mut heard,
    )
    .expect("the break's step");
    if transcript {
        s.transcript_wait_seq = Some(2);
    }
    let point = Turn {
        phase: Phase::Idle,
        screen: screen_at(2, rows),
        timed_out: false,
    };
    s.turn_end_execute(&point, action, &opts, &[], &mut heard)
        .expect("the point's act");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    let said = occupancy(&heard.noted);
    assert_eq!(said.first(), Some(&change(1, "open")), "{said:?}");
    said.len() == 1
}

/// THE BACKGROUND WORK A POINT KEEPS THE HOLD OVER is the loop's own reading of a break: the
/// screen reader's background wait — a shell left running, or a dynamic workflow with no shell
/// counted — or the session's transcript's wait kept for that very screen (BG-1); and a wait the
/// policy decides over it keeps the hold as nothing decided does.
#[test]
fn a_point_reads_the_background_work_as_a_break_does() {
    let nothing = || TurnEndAction::Nothing;
    let escalate = || TurnEndAction::Escalate {
        reason: "a person's decision".to_string(),
    };
    assert!(kept_at_point(break_screen(), false, nothing()));
    assert!(kept_at_point(break_screen(), false, wait_a_while()));
    let workflow = waiting_on(&[
        "⏺ Second-pass reviewers are out and the build is running.",
        "",
        "✻ Waiting for 1 dynamic workflow to finish",
        "",
    ]);
    assert!(aterm_phase::phase::background_wait(&workflow).is_some());
    assert_eq!(aterm_phase::phase::shells_running(&workflow), Some(0));
    assert!(kept_at_point(workflow, false, nothing()));
    assert!(kept_at_point(ended(SAID), true, nothing()));
    assert!(!kept_at_point(ended(SAID), false, nothing()), "control");
    let mut begun = break_screen();
    let status = begun
        .iter()
        .position(|r| r.starts_with("✻ Crunched"))
        .expect("the status row");
    begun.insert(status + 2, "❯ what is the build doing?".to_string());
    assert_eq!(aterm_phase::phase::background_wait(&begun), None);
    assert_eq!(aterm_phase::phase::shells_running(&begun), Some(1));
    assert!(!kept_at_point(begun, false, nothing()));
    assert!(!kept_at_point(break_screen(), false, escalate()));
}

/// THE ANSWER TO THE HARNESS'S OWN LINE KEEPS THE HOLD, AND ITS `WAITING:` DECLARES IT: the host
/// types its line at a settled break, the agent's live turn answers it — read as the loop reads
/// one (`Session::took`, which reads the edge before it marks the act taken: read after, the
/// answer is the agent's own wake, the review of 1a's M22) — and the next break's last words
/// start `WAITING:`: declared, and booked once the hold stood 10 min.
#[test]
fn the_answer_to_a_harness_line_keeps_the_hold_and_declares_it() {
    let run = |host: Arc<dyn IdleHost>, tag: &str| {
        let (dir, path) = ledger_at(tag);
        let mut m = Mock::new(Vec::new());
        let noted = {
            let (mut s, clock) = hold_session(&mut m, &path, Some(host), Duration::ZERO);
            let opts = hosted(30);
            let cfg = Some(&opts.policy);
            let mut heard = Heard::default();
            let mut since = None;
            s.host_steps_in_background(&screen_at(1, break_screen()), &mut since, cfg, &mut heard)
                .expect("the break's step");
            s.took(2, cfg, &mut heard);
            clock.skip(Duration::from_secs(11 * 60));
            clock.set_at_ms(T0 + 11 * MIN);
            s.host_steps_in_background(&screen_at(3, waiting_break()), &mut since, cfg, &mut heard)
                .expect("the break's step");
            heard.noted
        };
        let rows = hold_rows(&path);
        let _ = std::fs::remove_dir_all(&dir);
        (occupancy(&noted), rows)
    };
    let typed = Arc::new(BackgroundHost::default());
    let (noted, rows) = run(Arc::clone(&typed) as Arc<dyn IdleHost>, "occ-declared");
    assert!(
        typed.backgrounds.load(Ordering::SeqCst) > 0,
        "the host typed its line"
    );
    assert_eq!(noted, [change(1, "open"), change(3, "declared")]);
    assert_eq!(
        reasons(&rows),
        [said(&["hold=open", "held_ms=660000", "declared"])]
    );
    let (noted, rows) = run(Arc::new(Withholds::default()), "occ-undeclared");
    assert_eq!(
        noted,
        [change(1, "open"), change(2, "closed"), change(3, "open")]
    );
    assert!(
        rows.iter().all(|(_, w)| !w.iter().any(|w| w == "declared")),
        "{rows:?}"
    );
}

/// A COMMAND'S CONTINUATION STILL OWED IS NOTHING TYPED (the review of 1a's M12): the policy
/// typed a command whose continuation follows at the next point, its turn was read and taken,
/// and the hold opened at the break after it: the agent's own wake there is no answer of the
/// harness's, and closes the hold.
#[test]
fn a_commands_owed_continuation_is_no_turn_the_harness_typed() {
    let run = |taken_first: bool| {
        let (dir, path) = ledger_at("occ-command");
        let mut m = Mock::new(Vec::new());
        let host = Arc::new(Withholds::default()) as Arc<dyn IdleHost>;
        let (mut s, _clock) = hold_session(&mut m, &path, Some(host), Duration::ZERO);
        let opts = hosted(30);
        let cfg = Some(&opts.policy);
        let mut heard = Heard::default();
        let reading = s.turn_end_reading(&screen_at(1, ended(SAID)), None, &opts);
        s.turn_end.acted(
            &TurnEndAction::TypeCommand {
                command: "/compact".to_string(),
                rule_id: RULE_COMPACT,
                then: Then::Continue,
            },
            &reading,
            Instant::now(),
        );
        if taken_first {
            s.took(2, cfg, &mut heard);
        }
        assert!(s.turn_end.act_untaken(), "the continuation is owed");
        let mut since = None;
        s.host_steps_in_background(&screen_at(3, break_screen()), &mut since, cfg, &mut heard)
            .expect("the break's step");
        s.took(4, cfg, &mut heard);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
        occupancy(&heard.noted)
    };
    assert_eq!(run(true), [change(3, "open"), change(4, "closed")]);
    assert_eq!(run(false), [change(3, "open")], "control");
}

/// A host that steps at an idle point once `wanting`: the live upgrade's notice typed
/// (`typed`), or a step that moved nothing and typed nothing.
#[derive(Debug)]
struct StepsAtIdle {
    typed: bool,
    wanting: AtomicBool,
}

impl IdleHost for StepsAtIdle {
    fn wants(&self) -> bool {
        self.wanting.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.wanting.store(false, Ordering::SeqCst);
        Some(HostStep {
            line: "upgrade step=announced:1".to_string(),
            moved: self.typed,
            typed: self
                .typed
                .then_some(crate::supervise::policy::turn_end::HostTurn::Notice),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
}

/// THE HOST'S STEP AT AN IDLE POINT IS THAT POINT'S ACT (the review of 1a's M14): a hold booked
/// at 11 min, then an idle point where the host types its notice: the hold closes there
/// (`why=point`).
#[test]
fn the_hosts_step_at_an_idle_point_closes_the_hold() {
    let run = |typed: bool| {
        let (dir, path) = ledger_at("occ-idle-step");
        let host = Arc::new(StepsAtIdle {
            typed,
            wanting: AtomicBool::new(false),
        });
        let mut m = Mock::new(vec![idle_screen()]);
        m.cursor_on_caret = true;
        {
            let (mut s, clock) = hold_session(
                &mut m,
                &path,
                Some(Arc::clone(&host) as Arc<dyn IdleHost>),
                Duration::ZERO,
            );
            let opts = SuperviseOpts {
                idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
                ..hosted(30)
            };
            let cfg = Some(&opts.policy);
            let mut heard = Heard::default();
            let mut since = None;
            let mut at_ms = T0;
            s.host_steps_in_background(&screen_at(1, break_screen()), &mut since, cfg, &mut heard)
                .expect("the break's step");
            skip(&clock, &mut at_ms, 11 * MIN);
            s.host_steps_in_background(&screen_at(2, break_screen()), &mut since, cfg, &mut heard)
                .expect("the break's step");
            let screen = s.screen().expect("the point");
            let point = s.turn_of(screen);
            let _ = s
                .turn_end_at_point(&point, Some(Duration::from_secs(6)), &opts)
                .expect("the decision");
            host.wanting.store(true, Ordering::SeqCst);
            let stepped = s
                .host_steps_here(&point, &opts, &mut false, &mut heard)
                .expect("the host's step");
            assert!(stepped, "the host stepped: {:#?}", heard.noted);
        }
        let rows = hold_rows(&path);
        let _ = std::fs::remove_dir_all(&dir);
        reasons(&rows)
    };
    let booked = said(&["hold=open", "held_ms=660000"]);
    assert_eq!(
        run(true),
        [
            booked.clone(),
            said(&["hold=closed", "why=point", "held_ms=660000"])
        ]
    );
    assert_eq!(run(false), [booked], "control");
}

/// A TURN A LIMIT RESET'S END READS RUNNING IS READ AT ITS EDGE TOO (the review of 1a's D3):
/// the step's end paid by the worker already at work (`Session::reset_at_work`) takes that turn
/// through `Session::took`, so a hold booked before it closes there as the agent's own
/// (`why=foreign`).
#[test]
fn a_turn_a_limit_resets_end_reads_running_closes_the_hold() {
    let run = |owed: bool| {
        let (dir, path) = ledger_at("occ-reset");
        let mut m = Mock::new(Vec::new());
        {
            let host = Arc::new(Withholds::default()) as Arc<dyn IdleHost>;
            let (mut s, clock) = hold_session(&mut m, &path, Some(host), Duration::ZERO);
            let opts = hosted(30);
            assert!(!opts.policy.limit_reset, "the step's end is told to nobody");
            let cfg = Some(&opts.policy);
            let mut heard = Heard::default();
            let mut since = None;
            let mut at_ms = T0;
            s.host_steps_in_background(&screen_at(1, break_screen()), &mut since, cfg, &mut heard)
                .expect("the break's step");
            skip(&clock, &mut at_ms, 11 * MIN);
            s.host_steps_in_background(&screen_at(2, break_screen()), &mut since, cfg, &mut heard)
                .expect("the break's step");
            if owed {
                let now = s.fx.mono();
                s.reset = ResetStep::Owed(Box::new(ResetOwed {
                    go: None,
                    since: now,
                    keyed: now,
                    entered: false,
                    enters: 0,
                    kept: None,
                    tries: 0,
                    at: now,
                    told: false,
                    seen: None,
                    offer: None,
                    hold: None,
                    shown: None,
                    yes_said: false,
                    skips: 0,
                    agains: 0,
                    stale: false,
                }));
            }
            s.reset_at_work(&screen_at(3, busy_screen()), true, &opts, &mut heard)
                .expect("the step's end");
            assert_eq!(s.reset, ResetStep::Idle);
        }
        let rows = hold_rows(&path);
        let _ = std::fs::remove_dir_all(&dir);
        reasons(&rows)
    };
    let booked = said(&["hold=open", "held_ms=660000"]);
    assert_eq!(
        run(true),
        [
            booked.clone(),
            said(&["hold=closed", "why=foreign", "held_ms=660000"])
        ]
    );
    assert_eq!(run(false), [booked], "control");
}

/// A host whose agent's process started at `launched` (unix ms; [`IdleHost::launch`]).
#[derive(Debug)]
struct Launched(Option<i64>);

impl IdleHost for Launched {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn launch(&self) -> Option<i64> {
        self.0
    }
}

/// A predecessor's hold, from `T0` and booked at 11 min, then `declared` by the agent's answer
/// to a host's line where asked, and booked again as the loop ended a minute later
/// ([`Session::hold_left`]) where `ends`: the ledger's text.
fn predecessor(tag: &str, declared: bool, ends: bool) -> String {
    let (dir, path) = ledger_at(tag);
    let mut m = Mock::new(Vec::new());
    {
        let host: Arc<dyn IdleHost> = if declared {
            Arc::new(BackgroundHost::default())
        } else {
            Arc::new(Withholds::default())
        };
        let (mut s, clock) = hold_session(&mut m, &path, Some(host), Duration::ZERO);
        let opts = hosted(30);
        let cfg = Some(&opts.policy);
        let mut heard = Heard::default();
        let mut since = None;
        let mut at_ms = T0;
        s.host_steps_in_background(&screen_at(1, break_screen()), &mut since, cfg, &mut heard)
            .expect("the break's step");
        if declared {
            s.took(2, cfg, &mut heard);
        }
        skip(&clock, &mut at_ms, 11 * MIN);
        let last = if declared {
            waiting_break()
        } else {
            break_screen()
        };
        s.host_steps_in_background(&screen_at(3, last), &mut since, cfg, &mut heard)
            .expect("the break's step");
        if ends {
            skip(&clock, &mut at_ms, MIN);
            s.hold_left(cfg, &heard);
        }
    }
    let body = std::fs::read_to_string(&path).expect("the predecessor's ledger");
    let _ = std::fs::remove_dir_all(&dir);
    body
}

/// What a successor makes of the ledger text `body`: started with its wall clock at `now_ms`,
/// Off lifted since where `lifted_off`, its agent launched at `launched`, it reads a break and
/// then the agent's own wake, and ends where `ends`.
fn successor(
    body: &str,
    now_ms: i64,
    lifted_off: bool,
    launched: Option<i64>,
    ends: bool,
) -> Vec<Vec<String>> {
    let (dir, path) = ledger_at("occ-successor");
    std::fs::write(&path, body).expect("the ledger carried");
    let before = hold_rows(&path).len();
    let mut m = Mock::new(Vec::new());
    {
        let host = Arc::new(Launched(launched)) as Arc<dyn IdleHost>;
        let (mut s, clock) = hold_session(&mut m, &path, Some(host), Duration::ZERO);
        clock.set_at_ms(now_ms);
        let opts = hosted(30);
        let cfg = Some(&opts.policy);
        let mut heard = Heard::default();
        let start = s.fx.mono();
        s.hold_seed_from(body, start, now_ms, lifted_off, &opts.policy, &heard);
        let mut since = None;
        s.host_steps_in_background(&screen_at(1, break_screen()), &mut since, cfg, &mut heard)
            .expect("the break's step");
        if ends {
            s.hold_left(cfg, &heard);
        } else {
            s.took(2, cfg, &mut heard);
        }
    }
    let rows = hold_rows(&path).split_off(before);
    let _ = std::fs::remove_dir_all(&dir);
    reasons(&rows)
}

/// A SUCCESSOR CARRIES THE HOLD ONLY FROM A LOOP THAT WATCHED IT UNTIL JUST BEFORE IT BEGAN (the
/// red-team's F3; the review of 1a's D1).
#[test]
fn a_successor_carries_the_hold_only_from_a_loop_that_watched_it_till_just_now() {
    let body = predecessor("occ-seed", false, false);
    let row = crate::supervise::occupancy::hold::seed_row_in(&body, Some("@s-1"))
        .expect("the booked hold is open");
    assert_eq!(row.held_ms, 11 * MIN);
    let carried = [said(&["hold=closed", "why=foreign", "held_ms=900000"])];
    let stale = [said(&["hold=closed", "why=stale", "held_ms=660000"])];
    let soon = row.row_ms + 4 * MIN;
    for launched in [None, Some(row.row_ms - 60 * MIN)] {
        assert_eq!(
            successor(&body, soon, false, launched, false),
            carried,
            "{launched:?}"
        );
    }
    assert_eq!(
        successor(&body, row.row_ms + CARRY_GAP_MS, false, None, false),
        [said(&["hold=closed", "why=foreign", "held_ms=960000"])],
        "at the gap itself"
    );
    let day = 24 * 60 * MIN;
    assert_eq!(
        successor(&body, row.row_ms + day, false, None, false),
        stale
    );
    assert_eq!(
        successor(&body, row.row_ms + CARRY_GAP_MS + 1, false, None, false),
        stale
    );
    let typed = approvals::Row {
        rule_id: RULE_CONTINUE,
        outcome: approvals::Outcome::Typed,
        command: "keep going",
        reason: "-",
        box_seq: 9,
    }
    .to_json(row.row_ms + MIN, Some("@s-1"));
    let followed = format!("{body}{typed}\n");
    assert_eq!(successor(&followed, soon, false, None, false), stale);
    assert_eq!(successor(&body, soon, true, None, false), stale);
    assert_eq!(
        successor(&body, soon, false, Some(row.row_ms + MIN), false),
        stale
    );
}

/// A LOOP THAT ENDS WITH A BOOKED HOLD OPEN BOOKS IT AGAIN, declared as it stands (the review of
/// 1a's M16), so a successor that begins within the carry's gap of the END carries it: the
/// predecessor's hold, declared and booked at 11 min, is booked again as the loop ends a minute
/// later, held to the last read that saw it (so the `since` it gives errs late, never early); a
/// successor 4 min after that end carries it declared, and its own end books it so, held 15
/// min.
#[test]
fn a_loop_that_ends_with_the_hold_open_books_it_for_its_successor() {
    let body = predecessor("occ-left", true, true);
    let rows: Vec<Vec<String>> = body
        .lines()
        .filter(|l| l.contains("fact-hold@v1"))
        .map(|l| {
            let v = aterm_json::from_str::<aterm_json::Value>(l).expect("a row");
            let reason = v.get("reason").and_then(aterm_json::Value::as_str);
            reason
                .unwrap_or("")
                .split(' ')
                .map(str::to_string)
                .collect()
        })
        .collect();
    let booked = said(&["hold=open", "held_ms=660000", "declared"]);
    assert_eq!(
        rows,
        [booked.clone(), booked],
        "booked, then booked at the end"
    );
    let end_ms = crate::supervise::occupancy::hold::seed_row_in(&body, Some("@s-1"))
        .expect("the end's row")
        .row_ms;
    assert_eq!(
        successor(&body, end_ms + 4 * MIN, false, None, true),
        [said(&["hold=open", "held_ms=900000", "declared"])]
    );
    let (dir, path) = ledger_at("occ-left-short");
    let mut m = Mock::new(Vec::new());
    {
        let host = Arc::new(Withholds::default()) as Arc<dyn IdleHost>;
        let (mut s, clock) = hold_session(&mut m, &path, Some(host), Duration::ZERO);
        let opts = hosted(30);
        let cfg = Some(&opts.policy);
        let mut heard = Heard::default();
        let mut since = None;
        let mut at_ms = T0;
        s.host_steps_in_background(&screen_at(1, break_screen()), &mut since, cfg, &mut heard)
            .expect("the break's step");
        skip(&clock, &mut at_ms, 5 * MIN);
        s.host_steps_in_background(&screen_at(2, break_screen()), &mut since, cfg, &mut heard)
            .expect("the break's step");
        s.hold_left(cfg, &heard);
        assert!(hold_rows(&path).is_empty(), "not booked yet");
        skip(&clock, &mut at_ms, 6 * MIN);
        s.host_steps_in_background(&screen_at(3, break_screen()), &mut since, cfg, &mut heard)
            .expect("the break's step");
        s.took(4, cfg, &mut heard);
        s.hold_left(cfg, &heard);
    }
    let heads: Vec<String> = reasons(&hold_rows(&path))
        .into_iter()
        .map(|w| w[0].clone())
        .collect();
    assert_eq!(
        heads,
        ["hold=open", "hold=closed"],
        "nothing after the close"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE HOSTED LOOP CARRIES THE HOLD ACROSS ITS RESTARTS, end to end (`Session::run_hosted`,
/// stopped by its host; the review of 1a's M29): its start seeds the hold from the ledger
/// (`drive_loop`'s `hold_seed_from`), and its end books the hold it leaves open
/// (`drive`'s `hold_left`).
#[test]
fn the_hosted_loop_carries_the_hold_across_its_restarts() {
    let run = |age_ms: i64, tag: &str| {
        let (dir, path) = ledger_at(tag);
        let row = approvals::Row {
            rule_id: crate::supervise::facts::FactKind::Hold.rule(),
            outcome: approvals::Outcome::Waiting,
            command: "",
            reason: "hold=open held_ms=660000",
            box_seq: 1,
        }
        .to_json(crate::supervise::journal::unix_ms() - age_ms, Some("@s-1"));
        std::fs::write(&path, format!("{row}\n")).expect("the predecessor's row");
        let opts = SuperviseOpts {
            idle_host: Some(Arc::new(Withholds::default()) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        for screen in [break_screen(), busy_screen()] {
            let mut m = Mock::new(vec![screen]);
            let (r, lines) = host_stops(&mut m, Some("@s-1"), path.clone(), &opts, false);
            assert_eq!(r, Ok(()), "{lines:#?}");
        }
        let rows = reasons(&hold_rows(&path));
        let _ = std::fs::remove_dir_all(&dir);
        rows
    };
    let rows = run(MIN, "occ-hosted");
    let heads: Vec<&str> = rows.iter().map(|w| w[0].as_str()).collect();
    assert_eq!(
        heads,
        ["hold=open", "hold=open", "hold=closed"],
        "the predecessor's, the first loop's end, the wake: {rows:?}"
    );
    assert!(rows[2].iter().any(|w| w == "why=foreign"), "{rows:?}");
    for words in &rows[1..] {
        assert!(held_ms(words) >= 11 * MIN + MIN, "carried: {rows:?}");
    }
    assert_eq!(
        run(6 * MIN, "occ-hosted-stale"),
        [
            said(&["hold=open", "held_ms=660000"]),
            said(&["hold=closed", "why=stale", "held_ms=660000"]),
        ]
    );
}

#[path = "occupancy_door_tests.rs"]
mod door;

#[path = "occupancy_write_tests.rs"]
mod write;
