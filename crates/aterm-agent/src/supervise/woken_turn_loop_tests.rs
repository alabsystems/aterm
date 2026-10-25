// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A turn the Mac slept through in the vendor's API retry (`super::super::super::woken_turn`):
//! the loop presses Esc on it once per wake, where the wake is a full one, the host measured the
//! API up since it and the margin passed, the composer holds no draft and the row is Claude
//! Code's network or no-response retry. Every negative is the positive run with one condition
//! taken away, so each run that presses nothing has a control that presses.

use super::*;
use crate::supervise::fx::ScriptedClock;
use crate::supervise::journal::UNSTUCK;
use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, Reach};
use crate::supervise::run::woken_turn::WOKEN_MARGIN;

/// How long the Mac slept with the turn in the vendor's retry.
const SLEPT_S: i64 = 30 * 60;

/// The host's measure of the API after the wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Net {
    /// Measured up since the first ask after the wake.
    Up,
    /// Measured down.
    Down,
    /// Measured up since before the Mac slept: a measure the wake made stale.
    UpBeforeTheWake,
}

/// A host whose measure of the API answers as [`Net`] once the Mac woke (the scripted clock's
/// wall past the sleep), and [`Reach::Unknown`] before.
#[derive(Debug)]
struct WokeHost {
    clock: Arc<ScriptedClock>,
    net: Net,
    /// The loop's clock when the host was made, before the run.
    born: Instant,
    /// The first ask after the wake, on the loop's clock.
    since: std::sync::Mutex<Option<Instant>>,
    asks: std::sync::atomic::AtomicUsize,
}

impl WokeHost {
    fn new(clock: &Arc<ScriptedClock>, net: Net) -> Arc<Self> {
        Arc::new(Self {
            clock: Arc::clone(clock),
            net,
            born: clock.mono(),
            since: std::sync::Mutex::new(None),
            asks: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn since(&self) -> Option<Instant> {
        *self.since.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl IdleHost for WokeHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn reach(&self) -> Reach {
        if self.clock.peek_wall_ms() < (TEST_NOW + SLEPT_S) * 1000 {
            return Reach::Unknown;
        }
        self.asks.fetch_add(1, Ordering::SeqCst);
        let since = *self
            .since
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(|| self.clock.mono());
        match self.net {
            Net::Up => Reach::Up { since },
            Net::Down => Reach::Down { since },
            Net::UpBeforeTheWake => Reach::Up { since: self.born },
        }
    }
}

/// One run: a busy turn, the Mac sleeping as the retry row is first read, the row held until a
/// key and the screens after it.
#[derive(Clone)]
struct Case {
    retry: Vec<String>,
    /// The retry row stands until a key: a press takes it away.
    held: bool,
    after: Vec<Vec<String>>,
    /// The first screen after the key stands until the loop types a turn.
    after_turn: bool,
    slept_s: i64,
    net: Net,
    full_wake: Option<bool>,
    human_seq: Option<u64>,
    cell_attrs: &'static str,
    retry_api_errors: bool,
    autopilot: Option<&'static str>,
    grace: u32,
    /// The watch's own limit.
    max_s: u64,
}

impl Case {
    fn on(retry: Vec<String>) -> Self {
        Self {
            retry,
            held: true,
            after: vec![busy_screen(), ended(STOP)],
            after_turn: false,
            slept_s: SLEPT_S,
            net: Net::Up,
            full_wake: Some(true),
            human_seq: Some(0),
            cell_attrs: "dim",
            retry_api_errors: true,
            autopilot: None,
            grace: 120,
            max_s: 30,
        }
    }
}

struct Woke {
    lines: Vec<String>,
    /// Every Esc sent, and when on the loop's clock.
    escs: Vec<(String, Instant)>,
    host: Arc<WokeHost>,
    waits: Vec<String>,
}

impl Woke {
    fn unstuck(&self) -> Vec<&String> {
        self.lines
            .iter()
            .filter(|l| l.starts_with(&format!("{UNSTUCK} ")))
            .collect()
    }

    /// The journal's WAITING notes of the woken turn.
    fn waiting(&self) -> Vec<&String> {
        self.waits
            .iter()
            .filter(|w| w.contains("slept in the vendor's API retry"))
            .collect()
    }
}

fn woken_run(tag: &str, case: Case) -> Woke {
    let (dir, path) = journal_file(&format!("te-woken-{tag}"));
    let mut screens = vec![busy_screen(), case.retry.clone()];
    screens.extend(case.after.iter().cloned());
    let mut m = Mock::new(screens);
    if case.held {
        m.key_releases = Some(1);
    }
    if case.after_turn {
        m.turn_releases = Some(2);
    }
    if case.slept_s > 0 {
        m.sleeps_at.insert(1, case.slept_s);
    }
    m.human_seq = case.human_seq;
    m.cell_attrs = case.cell_attrs;
    m.autopilot = case.autopilot;
    m.vanish_after = Some(u32::try_from(2 * case.max_s).unwrap_or(u32::MAX));
    m.stall_sleep = Some(Duration::from_secs(1));
    let clock = virtual_clock();
    clock.set_full_wake(case.full_wake);
    let host = WokeHost::new(&clock, case.net);
    let mut opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..hosted(case.max_s)
    };
    opts.policy.retry_api_errors = case.retry_api_errors;
    opts.policy.human_grace_s = case.grace;
    let run = record_watch_on(&mut m, &opts, clock, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_millis(1),
            short_backoff: Duration::ZERO,
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    if run.unreplayable.is_none() {
        crate::supervise::tape::testing::assert_replays(
            &run.tape,
            &run.header,
            &run.sent,
            &run.lines,
        );
    }
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let waits = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.clone())
        .collect();
    let escs = m
        .key_times
        .iter()
        .filter(|(line, _)| line.ends_with(" esc"))
        .cloned()
        .collect();
    Woke {
        lines: run.lines,
        escs,
        host,
        waits,
    }
}

/// Claude Code 2.1.283's measured retry with a message queued behind it: `Retrying in 13s`,
/// the composer's placeholder `Press up to edit queued messages`.
fn queued() -> Vec<String> {
    use aterm_phase::prompt::fixtures::{API_RETRYING_QUEUED_MEASURED, screen};
    screen(API_RETRYING_QUEUED_MEASURED)
}

/// [`queued`] with its retry row read as `row`.
fn queued_with(row: &str) -> Vec<String> {
    queued()
        .into_iter()
        .map(|r| {
            if r.starts_with("✻ Can't reach the API server") {
                row.to_string()
            } else {
                r
            }
        })
        .collect()
}

/// The turn Claude Code stopped at the Esc, nothing queued.
fn stopped() -> Vec<String> {
    let mut stopped = rows(&[
        "⏺ Working.",
        "  ⎿  Interrupted · What should Claude do instead?",
        "",
    ]);
    stopped.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    stopped
}

#[test]
fn a_turn_the_mac_slept_through_in_the_vendors_retry_is_pressed_esc_once_the_api_is_up() {
    let rows = queued();
    assert_eq!(
        worker_phase(&rows),
        Phase::Busy,
        "PRECONDITION: a busy read"
    );
    let guard = aterm_phase::api_retry_row(&rows)
        .expect("PRECONDITION: the retry row")
        .guard;
    let w = woken_run("up", Case::on(rows));
    assert_eq!(
        w.escs.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
        [format!("key if-human=0 if={guard} esc")],
        "{:#?}",
        w.lines
    );
    let since = w.host.since().expect("the host was asked after the wake");
    let (_, at) = w.escs[0];
    assert!(
        at.saturating_duration_since(since) >= WOKEN_MARGIN,
        "no Esc inside the margin: {:?}",
        at.saturating_duration_since(since)
    );
    assert_eq!(w.unstuck().len(), 1, "{:#?}", w.lines);
    assert!(
        w.unstuck()[0].contains("Can't reach the API server"),
        "{:#?}",
        w.unstuck()
    );
    assert!(w.waiting().is_empty(), "{:#?}", w.lines);
    assert!(
        w.lines.iter().any(|l| l.contains("I need your decision")),
        "the queued message ran on to the turn's end: {:#?}",
        w.lines
    );
}

/// Each run with one condition of the press taken away, beside the positive it differs from.
#[test]
fn a_woken_turn_is_pressed_nothing_where_any_condition_fails() {
    let pressed = |tag: &str, case: Case| {
        let w = woken_run(tag, case);
        (w.escs.len(), w.unstuck().len(), w.waiting().len(), w.lines)
    };
    let (escs, unstuck, waiting, lines) = pressed("control", Case::on(queued()));
    assert_eq!(
        (escs, unstuck, waiting),
        (1, 1, 0),
        "the control: {lines:#?}"
    );
    let cases: [(&str, Case, usize); 9] = [
        (
            "no-sleep",
            Case {
                slept_s: 0,
                ..Case::on(queued())
            },
            0,
        ),
        (
            "down",
            Case {
                net: Net::Down,
                ..Case::on(queued())
            },
            1,
        ),
        (
            "up-before-the-wake",
            Case {
                net: Net::UpBeforeTheWake,
                ..Case::on(queued())
            },
            1,
        ),
        (
            "dark-wake",
            Case {
                full_wake: Some(false),
                ..Case::on(queued())
            },
            0,
        ),
        (
            "wake-unread",
            Case {
                full_wake: None,
                ..Case::on(queued())
            },
            0,
        ),
        (
            "no-person-fence",
            Case {
                human_seq: None,
                ..Case::on(queued())
            },
            0,
        ),
        (
            "retry-off",
            Case {
                retry_api_errors: false,
                ..Case::on(queued())
            },
            0,
        ),
        (
            "autopilot-off",
            Case {
                autopilot: Some("off"),
                ..Case::on(queued())
            },
            0,
        ),
        (
            "placeholder-read-typed",
            Case {
                cell_attrs: "none",
                ..Case::on(queued())
            },
            0,
        ),
    ];
    for (tag, case, waits) in cases {
        let (escs, unstuck, waiting, lines) = pressed(tag, case);
        assert_eq!((escs, unstuck, waiting), (0, 0, waits), "{tag}: {lines:#?}");
    }
}

#[test]
fn a_draft_in_the_composer_is_never_pressed_away() {
    use aterm_phase::prompt::fixtures::{
        API_RETRYING_DRAFT_MEASURED, API_RETRYING_QUEUED_DRAFT_MEASURED, screen,
    };
    for (name, fixture) in [
        ("draft", API_RETRYING_DRAFT_MEASURED),
        ("queued-and-draft", API_RETRYING_QUEUED_DRAFT_MEASURED),
    ] {
        let rows = screen(fixture);
        assert_eq!(worker_phase(&rows), Phase::Busy, "PRECONDITION {name}");
        let typed = woken_run(
            &format!("{name}-typed"),
            Case {
                cell_attrs: "none",
                ..Case::on(rows.clone())
            },
        );
        assert!(typed.escs.is_empty(), "{name}: {:#?}", typed.lines);
        let dim = woken_run(&format!("{name}-dim"), Case::on(rows));
        assert_eq!(dim.escs.len(), 1, "{name}, the control: {:#?}", dim.lines);
    }
}

#[test]
fn the_vendors_own_waits_and_a_short_countdown_are_pressed_nothing() {
    use aterm_phase::prompt::fixtures::{API_RETRYING_MEASURED, screen};
    for row in [
        "✻ Usage limit reached · Retrying in 13s (9:50pm) · attempt 6/10",
        "✻ Your request is queued · next try in 13s · attempt 6",
        "✻ Auto mode check unavailable · next try in 13s · attempt 6/10",
        "✻ Connection refused — a firewall or proxy may be blocking it (ECONNREFUSED) · Retrying \
         in 13s · attempt 6/10",
    ] {
        let rows = queued_with(row);
        assert_eq!(
            worker_phase(&rows),
            Phase::Busy,
            "PRECONDITION, a busy read: {row}"
        );
        let w = woken_run("vendor-wait", Case::on(rows));
        assert!(w.escs.is_empty(), "{row}: {:#?}", w.lines);
        assert!(w.unstuck().is_empty(), "{row}: {:#?}", w.lines);
    }
    let short = screen(API_RETRYING_MEASURED);
    let r = aterm_phase::api_retry_row(&short).expect("PRECONDITION: the retry row");
    assert_eq!(r.next_try_s, Some(2), "PRECONDITION");
    let w = woken_run("short", Case::on(short.clone()));
    assert!(
        w.escs.is_empty(),
        "2 s to the vendor's next try: {:#?}",
        w.lines
    );
    let five: Vec<String> = short
        .iter()
        .map(|l| l.replace("Retrying in 2s", "Retrying in 5s"))
        .collect();
    assert_ne!(five, short, "PRECONDITION: the countdown moved");
    let w = woken_run("five", Case::on(five));
    assert_eq!(w.escs.len(), 1, "the control, 5 s: {:#?}", w.lines);
}

#[test]
fn a_try_that_went_out_after_the_api_came_up_is_not_cut_short() {
    let old = queued_with(
        "✻ No response from the API after 1m 5s · retrying, waiting up to 3m · attempt 2/10",
    );
    assert_eq!(worker_phase(&old), Phase::Busy, "PRECONDITION");
    let w = woken_run("no-response-old", Case::on(old));
    assert_eq!(
        w.escs.len(),
        1,
        "65 s waited, sent before the API came up: {:#?}",
        w.lines
    );
    let young = queued_with(
        "✻ No response from the API after 8s · retrying, waiting up to 3m · attempt 2/10",
    );
    let w = woken_run("no-response-young", Case::on(young));
    assert!(
        w.escs.is_empty(),
        "8 s waited, sent since the API came up: {:#?}",
        w.lines
    );
}

#[test]
fn the_wakes_one_esc_is_never_pressed_twice() {
    let w = woken_run(
        "unheard",
        Case {
            held: false,
            after: Vec::new(),
            ..Case::on(queued())
        },
    );
    assert_eq!(
        (w.escs.len(), w.unstuck().len()),
        (1, 1),
        "the row stands past the Esc, the wake's one press spent: {:#?}",
        w.lines
    );
}

#[test]
fn the_point_the_harnesss_esc_stopped_is_no_persons_esc() {
    let persons = woken_run(
        "persons-esc",
        Case {
            slept_s: 0,
            after: vec![stopped(), busy_screen(), ended(STOP)],
            after_turn: true,
            held: false,
            ..Case::on(busy_screen())
        },
    );
    assert!(
        persons
            .waits
            .iter()
            .any(|w| w.ends_with("a person stopped the turn with Esc")),
        "the control, a person's Esc: {:#?}\n{:#?}",
        persons.waits,
        persons.lines
    );
    // The wake's settle holds the point first (`the Mac woke: deciding again`), past the Esc.
    let ours = woken_run(
        "harness-esc",
        Case {
            after: vec![stopped(), busy_screen(), ended(STOP)],
            after_turn: true,
            max_s: 300,
            ..Case::on(queued())
        },
    );
    assert_eq!(ours.escs.len(), 1, "{:#?}", ours.lines);
    assert!(
        !ours
            .waits
            .iter()
            .any(|w| w.contains("a person stopped the turn")),
        "{:#?}\n{:#?}",
        ours.waits,
        ours.lines
    );
    assert!(
        ours.lines
            .iter()
            .any(|l| l.starts_with("CONTINUED seq=") && l.contains("rule=continue@v1")),
        "the point is the turn's end, continued: {:#?}",
        ours.lines
    );
}

#[test]
fn the_vendors_continuation_the_harnesss_esc_stopped_gets_the_limits_resume() {
    let decide = |marker: Option<bool>| {
        let mut m = Mock::new(vec![stopped()]);
        let opts = hosted(30);
        let mut s = virtual_session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        if let Some(resume) = marker {
            s.turn_end.woken_esc(resume);
        }
        let point = point_at(&s, stopped(), 102);
        s.turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision")
    };
    let held = decide(None);
    assert!(
        matches!(&held, TurnEndAction::WaitUntil { why, .. } if why.ends_with("a person stopped the turn with Esc")),
        "the control, a person's Esc: {held:?}"
    );
    let resumed = decide(Some(true));
    assert_eq!(resumed.rule_id(), Some(RULE_LIMIT_RESUME), "{resumed:?}");
    let continued = decide(Some(false));
    assert_eq!(continued.rule_id(), Some(RULE_CONTINUE), "{continued:?}");
}

/// The busy read's own steps, the reader named: Claude Code's presses, another program's never
/// (its screen is read by its own reader, which knows no such row).
#[test]
fn only_claude_codes_retry_row_is_pressed() {
    let press = |reader: &'static dyn ScreenReader| {
        let mut m = Mock::new(vec![queued()]);
        m.human_seq = Some(0);
        let clock = virtual_clock();
        clock.set_full_wake(Some(true));
        m.run_on(&clock);
        let host = WokeHost::new(&clock, Net::Up);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut heard = Heard::default();
        let mut s = session(&mut m, None);
        s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
        s.set_clock(TEST_NOW, PDT);
        let screen = s.read_screen().expect("a read");
        s.running.busy(s.fx.mono());
        s.watch_sleep();
        clock.slept(SLEPT_S);
        s.watch_sleep();
        assert!(
            s.woken.is_some(),
            "PRECONDITION: the wake with the turn running"
        );
        s.unstick_woken_turn(&screen, reader, &opts, &mut heard)
            .expect("the first read after the wake");
        clock.skip(WOKEN_MARGIN);
        s.unstick_woken_turn(&screen, reader, &opts, &mut heard)
            .expect("the read past the margin");
        let pressed = s.woken.is_some_and(|w| w.pressed());
        let stands = s.turn_end.woken_esc_stands();
        drop(s);
        (m.presses().len(), pressed, stands, heard.said)
    };
    let (presses, pressed, stands, said) = press(&aterm_phase::ClaudeReader);
    assert_eq!(
        (presses, pressed, stands),
        (1, true, true),
        "the control: {said:#?}"
    );
    for program in [
        Program::Codex,
        Program::Grok,
        Program::Kimi,
        Program::Agy,
        Program::Muse,
        Program::Gemini,
        Program::Generic,
    ] {
        let (presses, pressed, stands, said) = press(program.reader());
        assert_eq!(
            (presses, pressed, stands),
            (0, false, false),
            "{program:?}: {said:#?}"
        );
    }
}

/// The harness's mark on the point its Esc stops lasts from the press while the retry row still
/// reads live, for [`WOKEN_MARGIN`]: a person's Esc on a retry row read later is a person's.
#[test]
fn the_harnesss_mark_lasts_only_until_claude_code_could_take_the_key() {
    let mut m = Mock::new(vec![queued()]);
    m.human_seq = Some(0);
    let clock = virtual_clock();
    clock.set_full_wake(Some(true));
    m.run_on(&clock);
    let host = WokeHost::new(&clock, Net::Up);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut heard = Heard::default();
    let mut s = session(&mut m, None);
    s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
    s.set_clock(TEST_NOW, PDT);
    let screen = s.read_screen().expect("a read");
    s.running.busy(s.fx.mono());
    s.watch_sleep();
    clock.slept(SLEPT_S);
    s.watch_sleep();
    let mut read = |s: &mut Session<'_, Mock>| {
        s.turn_end.took();
        s.unstick_woken_turn(&screen, &aterm_phase::ClaudeReader, &opts, &mut heard)
            .expect("a busy read");
        s.turn_end.woken_esc_stands()
    };
    assert!(!read(&mut s), "the API not yet up for the margin");
    clock.skip(WOKEN_MARGIN);
    assert!(read(&mut s), "the press");
    clock.skip(Duration::from_secs(1));
    assert!(
        read(&mut s),
        "the row still live a second after the press: the press's own"
    );
    clock.skip(WOKEN_MARGIN);
    assert!(
        !read(&mut s),
        "the row a margin after the press: no longer the press's"
    );
    drop(s);
    assert_eq!(m.presses().len(), 1, "one Esc for the wake");
}
