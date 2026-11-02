// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::supervise::approvals::{RULE_TAP_RESET, RULE_WALL_WAIT, RULE_WALL_WAIT_END};
use crate::supervise::policy::turn_end::decide_turn_end;
use crate::supervise::run::turn_end_loop::resume_jitter;
use crate::supervise::tap_clock::{TapWindow, UsageWindow};

type Answers = Vec<(UsageWindow, Option<TapWindow>)>;

#[test]
fn a_bare_footer_cannot_carry_a_tap_across_conflicting_named_zones() {
    let host = TapHost::answering(&[(UsageWindow::FiveHour, None)]);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, None);
    let opts = opts_with(&host);
    let (first, _) = decide(&mut s, session_wall(Some("4pm (UTC)")), &opts);
    let notice = reset_of(&s, &first).expect("the notice is placed");
    let carried = notice + 120;
    s.seed_tap(
        "usage-session|4pm (UTC)".to_string(),
        aterm_phase::WallKind::UsageSession,
        "You've hit your session limit",
        carried,
        TEST_NOW,
    );
    let (bare, _) = decide(&mut s, session_wall(Some("4pm")), &opts);
    assert_eq!(reset_of(&s, &bare), Some(carried));
    let (changed, _) = decide(
        &mut s,
        session_wall(Some("4pm (America/Los_Angeles)")),
        &opts,
    );
    assert_eq!(reset_of(&s, &changed), Some(notice + 7 * 3600));
    assert_eq!(s.tap_unix(), None, "the old tap is no longer carried");
}

/// A host whose usage tap answers what the test sets ([`IdleHost::tap_window`]),
/// one answer per window, every ask counted. It takes no step and owns no turn
/// end. Its transcript is [`Self::tail`] (`None`: none to read).
#[derive(Debug, Default)]
struct TapHost {
    answers: std::sync::Mutex<Answers>,
    asks: std::sync::atomic::AtomicUsize,
    /// The asks after which [`Self::later`] replaces the answers (the tap's
    /// run that comes while the loop waits).
    after: std::sync::atomic::AtomicUsize,
    later: std::sync::Mutex<Option<Answers>>,
    /// The session's transcript ([`IdleHost::transcript_tail`]).
    tail: Option<String>,
    /// The limit row the transcript stands at ([`IdleHost::standing_limit`]).
    standing: Option<crate::harness::upgrade::StandingLimit>,
    /// Each reset the loop's limit episode told ([`IdleHost::limit_wall`]).
    walls: std::sync::Mutex<Vec<Option<i64>>>,
    /// The account the session's profile names ([`IdleHost::login_account`]).
    account: std::sync::Mutex<Option<crate::harness::profile::AccountKey>>,
}

impl TapHost {
    fn answering(answers: &[(UsageWindow, Option<TapWindow>)]) -> Arc<Self> {
        Self::reading_tail(answers, None)
    }

    fn reading_tail(answers: &[(UsageWindow, Option<TapWindow>)], tail: Option<&str>) -> Arc<Self> {
        let h = Arc::new(Self {
            tail: tail.map(str::to_owned),
            ..Self::default()
        });
        h.set(answers);
        h
    }

    fn set(&self, answers: &[(UsageWindow, Option<TapWindow>)]) {
        *self.answers.lock().unwrap() = answers.to_vec();
    }

    fn asks(&self) -> usize {
        self.asks.load(Ordering::SeqCst)
    }
}

impl IdleHost for TapHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn tap_window(&self, window: UsageWindow) -> Option<TapWindow> {
        let n = self.asks.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.after.load(Ordering::SeqCst)
            && let Some(later) = self.later.lock().unwrap().take()
        {
            *self.answers.lock().unwrap() = later;
        }
        self.answers
            .lock()
            .unwrap()
            .iter()
            .find(|(w, _)| *w == window)
            .and_then(|(_, a)| *a)
    }
    fn transcript_tail(&self) -> Option<String> {
        self.tail.clone()
    }
    fn standing_limit(&self) -> Option<crate::harness::upgrade::StandingLimit> {
        self.standing.clone()
    }
    fn limit_wall(&self, reset_unix: Option<i64>) {
        self.walls.lock().unwrap().push(reset_unix);
    }
    fn login_account(&self) -> Option<crate::harness::profile::AccountKey> {
        *self.account.lock().unwrap()
    }
}

const SID: &str = "@s-1";

fn reading(pct: u32, resets_at: i64, read_at: i64) -> Option<TapWindow> {
    Some(TapWindow {
        figure: Some(crate::harness::tap::Window {
            pct,
            resets_at,
            pct_x10: Some(pct * 10),
        }),
        read_at,
    })
}

fn session_wall(reset: Option<&str>) -> Vec<String> {
    usage_wall(&reset.map_or_else(
        || "You've hit your session limit".to_string(),
        |r| format!("You've hit your session limit · resets {r}"),
    ))
}

fn tapped<'a>(m: &'a mut Mock, ledger: Option<PathBuf>) -> Session<'a, Mock> {
    let mut s = session(m, Some(SID.to_string()));
    s.set_clock(TEST_NOW, PDT);
    s.set_mono_standing();
    s.set_zone_lookup(test_zones);
    s.ledger = crate::supervise::approvals::Ledger::open(
        ledger.as_deref(),
        Some(SID),
        &mut std::io::sink(),
    );
    s.set_approval_ledger(ledger);
    s
}

/// [`tapped`] on a virtual clock: the monotonic and wall clocks and every row's stamp start at [`TEST_NOW`] and move only on the loop's sleeps.
fn tapped_virtual<'a>(m: &'a mut Mock, ledger: Option<PathBuf>) -> Session<'a, Mock> {
    let mut s = tapped(m, ledger);
    s.set_fx(Fx::over(ClockSource::Virtual(Arc::new(
        crate::supervise::fx::VirtualClock::new(Instant::now(), TEST_NOW * 1000),
    ))));
    s
}

fn opts_with(host: &Arc<TapHost>) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..hosted(30)
    }
}

fn decide(
    s: &mut Session<'_, Mock>,
    rows: Vec<String>,
    opts: &SuperviseOpts,
) -> (TurnEndReading, TurnEndAction) {
    let screen = read_screen(rows);
    let r = s.turn_end_reading(&screen, None, opts);
    let now = Instant::now();
    s.observe_point(&r, now, screen.seq);
    let a = decide_turn_end(&s.turn_end, &r, &opts.policy, now);
    (r, a)
}

fn until_unix(s: &Session<'_, Mock>, a: &TurnEndAction) -> i64 {
    match a {
        TurnEndAction::WaitUntil { until, .. } => s.unix_of(*until),
        other => panic!("a wait: {other:?}"),
    }
}

fn why(a: &TurnEndAction) -> &str {
    match a {
        TurnEndAction::WaitUntil { why, .. } => why,
        other => panic!("a wait: {other:?}"),
    }
}

#[test]
fn a_cut_session_walls_wait_is_timed_by_the_taps_reset() {
    use crate::supervise::fx::Fx;
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::host::ReplayHost;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::TAPE_ID;
    let reset = TEST_NOW + 2 * 3600 + 17;
    let five = UsageWindow::FiveHour;
    let (dir, ledger) = ledger_at("tap-cut");
    let host = TapHost::answering(&[(five, reading(100, reset, TEST_NOW))]);
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, Some(ledger.clone()));
    let (r, a) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(r.wall, Some(aterm_phase::WallKind::UsageSession), "{r:#?}");
    let t = s.turn_end.timing.clone();
    let jitter = resume_jitter(SID, reset, t.reset_jitter);
    assert_eq!(r.jitter, jitter, "the session's share of the tap's reset");
    assert!(jitter > Duration::ZERO, "a share to see: {jitter:?}");
    let grace = i64::try_from(t.reset_grace.as_secs()).unwrap();
    let share = i64::try_from(jitter.as_secs()).unwrap();
    assert_eq!(until_unix(&s, &a), reset + grace + share, "{a:?}");
    assert_eq!(why(&a), "usage-session resets");
    assert_eq!(host.asks(), 1, "one window asked, once");
    let again = s.turn_end_reread(&read_screen(session_wall(None)), &opts);
    assert_eq!(again.reset_at.map(|at| s.unix_of(at)), Some(reset));
    assert_eq!(host.asks(), 1);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    assert!(
        rows.lines().any(|l| l.contains(RULE_WALL_WAIT)
            && l.contains("reset_unix=-;")
            && l.contains(&format!("; tap_unix={reset};"))),
        "the wall's row carries the tap's reset, apart from the notice's none: {rows}"
    );
    assert!(!rows.contains(RULE_TAP_RESET), "nothing disagreed: {rows}");
    let _ = std::fs::remove_dir_all(&dir);

    let backoff = i64::try_from(t.limit_backoff.last().unwrap().as_secs()).unwrap();
    let cases: Vec<(&str, Option<Answers>)> = vec![
        ("no host", None),
        ("nothing read", Some(vec![(five, None)])),
        (
            "written before the wall was read",
            Some(vec![(five, reading(100, reset, TEST_NOW - 1))]),
        ),
        (
            "the seven-day window full",
            Some(vec![(UsageWindow::SevenDay, reading(100, reset, TEST_NOW))]),
        ),
        (
            "under 100",
            Some(vec![(five, reading(99, reset, TEST_NOW))]),
        ),
    ];
    for (case, answers) in cases {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = tapped(&mut m, None);
        let opts = match &answers {
            Some(a) => opts_with(&TapHost::answering(a)),
            None => hosted(30),
        };
        let (r, a) = decide(&mut s, session_wall(None), &opts);
        assert_eq!(r.reset_at, None, "{case}");
        assert_eq!(until_unix(&s, &a), TEST_NOW + backoff, "{case}: {a:?}");
    }
    let host = TapHost::answering(&[(five, reading(100, reset, TEST_NOW))]);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, None);
    let codex = s.tap_reset(
        opts_with(&host).idle_host.as_ref(),
        aterm_phase::WallKind::UsageSession,
        "You've hit your usage limit",
        None,
        Program::Codex,
        super::super::wall_confirm::Check::Decide,
        1,
    );
    assert_eq!((codex, host.asks()), (None, 0));
    for doors in [Vec::new(), vec!["host.wants".to_owned()]] {
        let core = ReplayCore::start(
            vec![astream_effects::EffectRecord::Clock(
                u64::try_from(TEST_NOW).unwrap() * 1_000_000_000,
            )],
            VERBATIM,
            VERBATIM,
            TAPE_ID,
            None,
        );
        core.recorded_doors(doors);
        let host = TapHost::answering(&[(five, reading(100, reset, TEST_NOW))]);
        let opts = opts_with(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = tapped(&mut m, None);
        s.fx = Fx::replay(Arc::clone(&core));
        let seed = |s: &mut Session<'_, Mock>| {
            s.seed_tap(
                "an older wall".to_owned(),
                aterm_phase::WallKind::UsageSession,
                "You've hit your session limit",
                reset,
                TEST_NOW,
            );
        };
        seed(&mut s);
        assert_eq!(
            s.tap_reset(
                opts.idle_host.as_ref(),
                aterm_phase::WallKind::UsageSession,
                "You've hit your session limit",
                None,
                Program::Claude,
                super::super::wall_confirm::Check::Decide,
                1,
            ),
            None
        );
        assert!(s.wall_tap.is_none());
        seed(&mut s);
        s.turn_end_due = Some(Due {
            at: Instant::now(),
            unix_ms: TEST_NOW * 1000,
            why: "the older loop's wall wait".to_owned(),
            wait: WaitKind::Other,
        });
        assert!(!s.tap_edge(&opts, 1));
        assert_eq!(host.asks(), 0);
        assert_eq!(core.left(), 1, "the old loop's clock was not consumed");
        assert!(core.diverged().is_none());
    }
    let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
    core.recorded_doors(vec!["host.tap_window".to_owned()]);
    let replay = ReplayHost::new(Fx::replay(Arc::clone(&core)));
    let missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        IdleHost::tap_window(&replay, five)
    }));
    assert!(missing.is_err());
    assert!(core.diverged().is_some());
}

#[test]
fn the_notice_and_the_tap_take_the_taps_reset_or_the_later_with_a_ledger_row() {
    let five = UsageWindow::FiveHour;
    let words = "11:35am (America/Los_Angeles)";
    let run = |offset: i64| {
        let (dir, ledger) = ledger_at(&format!("tap-both-{offset}"));
        let host = TapHost::answering(&[(five, None)]);
        let opts = opts_with(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = tapped(&mut m, Some(ledger.clone()));
        let (_, first) = decide(&mut s, session_wall(Some(words)), &opts);
        let notice = s.wall_reset.as_ref().and_then(|(_, u)| *u).expect("placed");
        assert!(notice > TEST_NOW, "{}", notice - TEST_NOW);
        let t = s.turn_end.timing.clone();
        let grace = i64::try_from(t.reset_grace.as_secs()).unwrap();
        let span = t.reset_jitter;
        let share = move |at| i64::try_from(resume_jitter(SID, at, span).as_secs()).unwrap();
        assert_eq!(until_unix(&s, &first), notice + grace + share(notice));
        host.set(&[(five, reading(100, notice + offset, TEST_NOW + 9))]);
        let (_, a) = decide(&mut s, session_wall(Some(words)), &opts);
        let (_, again) = decide(&mut s, session_wall(Some(words)), &opts);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let taps: Vec<&str> = rows
            .lines()
            .filter(|l| l.contains(RULE_TAP_RESET))
            .collect();
        let waited = until_unix(&s, &a);
        assert_eq!(waited, until_unix(&s, &again));
        (notice, waited - grace, share, taps.len(), rows)
    };
    let (notice, at, share, taps, rows) = run(45);
    assert_eq!(at, notice + 45 + share(notice + 45));
    assert_eq!(taps, 0, "{rows}");
    let (notice, at, share, taps, _) = run(-limit::CLOCK_SLACK_S);
    assert_eq!(
        at,
        notice - limit::CLOCK_SLACK_S + share(notice - limit::CLOCK_SLACK_S)
    );
    assert_eq!(taps, 0, "at the slack");
    let (notice, at, share, taps, rows) = run(20 * 60);
    assert_eq!(at, notice + 20 * 60 + share(notice + 20 * 60));
    assert_eq!(taps, 1, "{rows}");
    assert!(
        rows.contains(&format!("notice_unix={notice}; tap_unix={}", notice + 1200))
            && rows.contains("\"decision\":\"waiting\""),
        "{rows}"
    );
    let (notice, at, share, taps, rows) = run(-20 * 60);
    assert_eq!(at, notice + share(notice));
    assert_eq!(taps, 1, "{rows}");
}

#[test]
fn a_tap_showing_the_window_rolled_over_ends_a_wait_with_no_reset() {
    let five = UsageWindow::FiveHour;
    let week = UsageWindow::SevenDay;
    let run = |first: u32| {
        let host = TapHost::answering(&[
            (five, reading(first, TEST_NOW - 3, TEST_NOW)),
            (week, reading(40, TEST_NOW + 86_400, TEST_NOW)),
        ]);
        let opts = opts_with(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = tapped(&mut m, None);
        let (_, a) = decide(&mut s, session_wall(None), &opts);
        let t = s.turn_end.timing.clone();
        let backoff = i64::try_from(t.limit_backoff.last().unwrap().as_secs()).unwrap();
        assert_eq!(until_unix(&s, &a), TEST_NOW + backoff, "the back-off first");
        host.set(&[
            (
                five,
                Some(TapWindow {
                    figure: None,
                    read_at: TEST_NOW + 2,
                }),
            ),
            (week, reading(40, TEST_NOW + 86_400, TEST_NOW + 2)),
        ]);
        let (_, a) = decide(&mut s, session_wall(None), &opts);
        let grace = i64::try_from(t.reset_grace.as_secs()).unwrap();
        (until_unix(&s, &a), grace, backoff, t.reset_jitter)
    };
    let (until, grace, _, span) = run(100);
    let share = i64::try_from(resume_jitter(SID, TEST_NOW + 2, span).as_secs()).unwrap();
    assert_eq!(until, TEST_NOW + 2 + grace + share);
    let (until, _, backoff, _) = run(97);
    assert_eq!(until, TEST_NOW + backoff);
}

#[test]
fn the_taps_reading_during_the_wait_decides_the_wall_again() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 3 * 3600;
    let run = |reads: bool| {
        let host = TapHost::answering(&[(five, None)]);
        host.after.store(1, Ordering::SeqCst);
        if reads {
            *host.later.lock().unwrap() = Some(vec![(five, reading(100, reset, TEST_NOW + 1))]);
        }
        let (dir, path) = journal_file(&format!("tap-edge-{reads}"));
        let mut m = Mock::new(vec![busy_screen(), session_wall(None)]);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.vanish_after = Some(30);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..opts_with(&host)
        };
        let mut out: Vec<u8> = Vec::new();
        {
            let mut s = tapped(&mut m, None);
            let _ = s.watch(&opts, &mut out);
        }
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let waits: Vec<String> = records
            .into_iter()
            .filter(|r| r.kind == "waiting")
            .map(|r| r.summary)
            .collect();
        (waits, count(&m, "turn"), host.asks())
    };
    let stamp = |unix: i64| {
        let (y, mo, d) = limit::civil(unix.div_euclid(86_400));
        let s = unix.rem_euclid(86_400);
        format!(
            "until={y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z usage-session resets",
            s / 3600,
            s % 3600 / 60,
            s % 60
        )
    };
    let t = TurnEndTiming::default();
    let backoff = i64::try_from(t.limit_backoff.last().unwrap().as_secs()).unwrap();
    let grace = i64::try_from(t.reset_grace.as_secs()).unwrap();
    let share = i64::try_from(resume_jitter(SID, reset, t.reset_jitter).as_secs()).unwrap();
    let (waits, typed, asks) = run(true);
    assert_eq!(typed, 0, "{waits:#?}");
    assert!(asks >= 2, "asked at the decision and at a step: {asks}");
    assert_eq!(
        waits.first().map(String::as_str),
        Some(stamp(TEST_NOW + backoff).as_str()),
        "{waits:#?}"
    );
    assert!(
        waits.iter().any(|w| *w == stamp(reset + grace + share)),
        "decided again at the tap's reset: {waits:#?}"
    );
    let (waits, typed, _) = run(false);
    assert_eq!(typed, 0);
    assert!(
        !waits.is_empty() && waits.iter().all(|w| *w == stamp(TEST_NOW + backoff)),
        "{waits:#?}"
    );
}

/// Claude Code idle at its session limit, the notice gone from the screen (a local command's
/// answer below it, no turn served).
fn wall_scrolled_off() -> Vec<String> {
    let mut r = rows(&["❯ /status", "  ⎿  Status dialog dismissed", ""]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// The wall's notice `wall` shows ([`usage_wall`], [`session_wall`]).
fn wall_said(wall: &[String]) -> String {
    wall.iter()
        .find_map(|r| r.trim_start().strip_prefix("⎿  "))
        .expect("the notice's row")
        .to_owned()
}

/// A transcript that stands at the limit notice `said`, the turn's prompt `carry on with the task`
/// above it.
fn limit_tail(said: &str) -> String {
    countdown_tail().replace(
        "You've hit your session limit · resets 11:35am (America/Los_Angeles)",
        said,
    )
}

/// A cut session wall the tap had not timed at its decision keeps its tap clock once its notice
/// leaves the screen. The tap's reading during the wait moves the wait, and the point it decides
/// again at, the notice gone by then, waits to the tap's reset (its grace and the session's share
/// after it), where the clock left with the notice and the limit's resume was typed into a limit
/// the tap had just timed. A tap that reads nothing leaves that point to the resume, as ever.
#[test]
fn a_cut_walls_tap_clock_outlasts_its_notice() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 2 * 3600 + 17;
    let run = |later: Option<TapWindow>| {
        let wall = session_wall(None);
        let host = TapHost::reading_tail(&[(five, None)], Some(&limit_tail(&wall_said(&wall))));
        let opts = opts_with(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = tapped(&mut m, None);
        let (r, a) = decide(&mut s, wall, &opts);
        assert!(r.wall.is_some() && r.reset_at.is_none(), "{r:#?}");
        let at_wall = until_unix(&s, &a);
        host.set(&[(five, later)]);
        s.turn_end_due = Some(Due {
            at: Instant::now(),
            unix_ms: TEST_NOW * 1000,
            why: "the wall's wait".to_owned(),
            wait: WaitKind::Other,
        });
        let moved = s.tap_edge(&opts, 1);
        let (r, a) = decide(&mut s, wall_scrolled_off(), &opts);
        assert!(r.wall.is_none(), "{r:#?}");
        let waited = match &a {
            TurnEndAction::WaitUntil { until, .. } => Some(s.unix_of(*until)),
            _ => None,
        };
        (at_wall, moved, a, s.turn_end.timing.clone(), waited)
    };
    let (at_wall, moved, _, t, after) = run(reading(100, reset, TEST_NOW + 1));
    let backoff = i64::try_from(t.limit_backoff.last().unwrap().as_secs()).unwrap();
    let grace = i64::try_from(t.reset_grace.as_secs()).unwrap();
    let share = i64::try_from(resume_jitter(SID, reset, t.reset_jitter).as_secs()).unwrap();
    assert_eq!(at_wall, TEST_NOW + backoff, "nothing read at the decision");
    assert!(moved, "the tap's reading moves the wait");
    assert_eq!(after, Some(reset + grace + share), "the tap's reset");

    let (_, moved, a, _, _) = run(None);
    assert!(!moved);
    assert_eq!(
        a.rule_id(),
        Some(crate::supervise::policy::turn_end::RULE_LIMIT_RESUME),
        "{a:?}"
    );
}

/// A cut wall the tap reads only while the loop waits on it moves the open limit episode to its
/// reset as a notice's new reset does: the host is told it and the budget is extended to it once
/// (`EXTEND until=<reset + grace>`), where the episode kept the reset it opened with, none, and
/// said nothing. A tap that reads nothing tells and extends nothing.
#[test]
fn a_tap_reading_during_the_wait_extends_the_limits_episode_to_its_reset() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 3 * 3600;
    let run = |reads: bool| {
        let host = TapHost::answering(&[(five, None)]);
        host.after.store(1, Ordering::SeqCst);
        if reads {
            *host.later.lock().unwrap() = Some(vec![(five, reading(100, reset, TEST_NOW + 1))]);
        }
        let mut m = Mock::new(vec![busy_screen(), session_wall(None)]);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.vanish_after = Some(30);
        let opts = opts_with(&host);
        let mut out: Vec<u8> = Vec::new();
        {
            let mut s = tapped(&mut m, None);
            let _ = s.watch(&opts, &mut out);
        }
        let extends: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .filter(|l| l.starts_with("EXTEND "))
            .map(str::to_owned)
            .collect();
        let walls = host.walls.lock().unwrap().clone();
        (extends, walls)
    };
    let grace = i64::try_from(RESET_GRACE.as_secs()).unwrap();
    let until = utc_stamp(u64::try_from(reset + grace).unwrap());
    let (extends, walls) = run(true);
    assert_eq!(extends, [format!("EXTEND until={until} reset=-")]);
    assert_eq!(walls, [None, Some(reset)], "the episode opened with none");
    let (extends, walls) = run(false);
    assert!(extends.is_empty(), "{extends:#?}");
    assert_eq!(walls, [None]);
}

/// A cut wall the tap read nothing of at its decision, whose notice leaves the screen before the
/// wait's next poll: the decision on the screen with the notice gone reads the tap itself, waits to
/// the reset it reads (its grace and the session's share after it) and writes the wall's row with
/// it, where it typed the limit's resume into a limit the tap had just timed.
#[test]
fn a_decision_with_the_notice_gone_reads_the_tap_the_wait_had_not() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 2 * 3600 + 17;
    let (dir, ledger) = ledger_at("tap-gone-decided");
    let wall = session_wall(None);
    let host = TapHost::reading_tail(&[(five, None)], Some(&limit_tail(&wall_said(&wall))));
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, Some(ledger.clone()));
    let (r, _) = decide(&mut s, wall, &opts);
    assert!(r.wall.is_some() && r.reset_at.is_none(), "{r:#?}");
    host.set(&[(five, reading(100, reset, TEST_NOW + 1))]);
    let asked = host.asks();
    let (r, a) = decide(&mut s, wall_scrolled_off(), &opts);
    assert!(r.wall.is_none(), "{r:#?}");
    assert_eq!(host.asks(), asked + 1, "the decision read the tap");
    let t = s.turn_end.timing.clone();
    let grace = i64::try_from(t.reset_grace.as_secs()).unwrap();
    let share = i64::try_from(resume_jitter(SID, reset, t.reset_jitter).as_secs()).unwrap();
    assert!(
        matches!(a, TurnEndAction::WaitUntil { .. }),
        "no resume into the timed limit: {a:?}"
    );
    assert_eq!(until_unix(&s, &a), reset + grace + share);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        rows.lines()
            .any(|l| l.contains(RULE_WALL_WAIT) && l.contains(&format!("; tap_unix={reset};"))),
        "the wall's row carries the tap's reset: {rows}"
    );
}

/// The loop itself at a cut wall whose notice leaves the screen right after the tap's reading
/// moved the wait: the point the tap's edge decides again is gone by then, and the point the loop
/// decides next, the notice gone, waits to the tap's reset instead of typing the limit's resume.
/// A tap that reads nothing leaves the wall's back-off standing, nothing typed.
#[test]
fn a_notice_gone_before_the_point_the_tap_moved_waits_to_the_taps_reset() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 3 * 3600;
    let run = |reads: bool| {
        let said = wall_said(&session_wall(None));
        let host = TapHost::reading_tail(&[(five, None)], Some(&limit_tail(&said)));
        host.after.store(1, Ordering::SeqCst);
        if reads {
            *host.later.lock().unwrap() = Some(vec![(five, reading(100, reset, TEST_NOW + 1))]);
        }
        let (dir, path) = journal_file(&format!("tap-gone-{reads}"));
        let mut m = Mock::new(vec![busy_screen(), session_wall(None), wall_scrolled_off()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.vanish_after = Some(30);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..opts_with(&host)
        };
        let mut out: Vec<u8> = Vec::new();
        {
            let mut s = tapped(&mut m, None);
            let _ = s.watch(&opts, &mut out);
        }
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let waits: Vec<String> = records
            .into_iter()
            .filter(|r| r.kind == "waiting")
            .map(|r| r.summary)
            .collect();
        (waits, count(&m, "turn"))
    };
    let t = TurnEndTiming::default();
    let grace = i64::try_from(t.reset_grace.as_secs()).unwrap();
    let share = i64::try_from(resume_jitter(SID, reset, t.reset_jitter).as_secs()).unwrap();
    let backoff = i64::try_from(t.limit_backoff.last().unwrap().as_secs()).unwrap();
    let wait = |unix: i64| {
        format!(
            "until={} usage-session resets",
            utc_stamp(u64::try_from(unix).unwrap())
        )
    };
    let (waits, typed) = run(true);
    assert_eq!(typed, 0, "{waits:#?}");
    assert!(
        waits.contains(&wait(reset + grace + share)),
        "waited to the tap's reset with the notice gone: {waits:#?}"
    );
    let (waits, typed) = run(false);
    assert_eq!(typed, 0, "{waits:#?}");
    assert_eq!(waits, [wait(TEST_NOW + backoff)]);
}

/// A tap reading a decision folds while the wall's limit episode is open moves the episode to the
/// tap's reset, not only one the tap's own edge sees move during the wait: the wall's point decided
/// anew ([`Session::limit_after`]), or decided again by another edge (`turn_end_now`) and looked at
/// as the next wait starts. The host is told the reset and the budget is extended to it, where the
/// episode kept the cut notice's none. A tap that reads nothing moves nothing.
#[test]
fn a_tap_reading_a_decision_folds_retimes_the_open_limit_episode() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 3 * 3600;
    let run = |again: bool, reads: bool| {
        let host = TapHost::answering(&[(five, None)]);
        let opts = opts_with(&host);
        let mut m = Mock::new(vec![session_wall(None)]);
        let (held, moved, deadline) = {
            let mut s = tapped(&mut m, None);
            s.stall_host = opts.idle_host.clone();
            let mut review = Noted::default();
            let started = Instant::now();
            let mut deadline = started;
            let screen = s.screen().expect("the wall");
            let seen = s.point_of(screen);
            s.turn_end_looked(&seen, None, &opts, &mut review)
                .expect("decided");
            s.limit_after(&seen, &mut deadline, true, &mut review)
                .expect("opened");
            let held = s.limit.as_ref().map(|ep| ep.reset_unix);
            if reads {
                host.set(&[(five, reading(100, reset, TEST_NOW + 1))]);
            }
            if again {
                s.turn_end_now(&seen, &opts, &[], &mut review)
                    .expect("decided again");
                s.stop = Some(Arc::new(AtomicBool::new(true)));
                s.wait_for_next(seen, &opts, &[], &mut deadline, &mut review)
                    .expect("the wait starts");
            } else {
                let screen = s.screen().expect("the wall");
                let seen = s.point_of(screen);
                s.turn_end_looked(&seen, None, &opts, &mut review)
                    .expect("decided");
                s.limit_after(&seen, &mut deadline, true, &mut review)
                    .expect("kept");
            }
            let moved = s.limit.as_ref().map(|ep| ep.reset_unix);
            (held, moved, deadline.saturating_duration_since(started))
        };
        (held, moved, deadline, host.walls.lock().unwrap().clone())
    };
    let hours = |d: Duration| d.as_secs() / 3600;
    for again in [false, true] {
        let (held, moved, deadline, walls) = run(again, true);
        assert_eq!(
            held,
            Some(None),
            "again={again}: the cut wall's episode opens at none"
        );
        assert_eq!(moved, Some(Some(reset)), "again={again}: the tap's reset");
        assert_eq!(walls, [None, Some(reset)], "again={again}");
        assert_eq!(
            hours(deadline),
            3,
            "again={again}: extended to it: {deadline:?}"
        );
        let (_, moved, deadline, walls) = run(again, false);
        assert_eq!(moved, Some(None), "again={again}: nothing read");
        assert_eq!(
            (walls, deadline),
            (vec![None], Duration::ZERO),
            "again={again}"
        );
    }
}

/// A cut wall keeps the account it was met under once its notice leaves the screen, as it keeps
/// its tap clock: a login moved to another account before the tap times the limit is a move once
/// the tap names its reset, where the account was forgotten with the notice and the new one taken
/// for the limit's own.
#[test]
fn a_cut_walls_login_outlasts_its_notice_with_its_tap_clock() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 2 * 3600 + 17;
    let key = |id: &str| crate::harness::profile::AccountKey::claude(id).expect("a key");
    let run = |moves: bool| {
        let wall = session_wall(None);
        let host = TapHost::reading_tail(&[(five, None)], Some(&limit_tail(&wall_said(&wall))));
        *host.account.lock().unwrap() = Some(key("account-a"));
        let opts = opts_with(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = tapped(&mut m, None);
        let (r, _) = decide(&mut s, wall, &opts);
        assert!(r.wall.is_some() && !r.login_moved, "{r:#?}");
        let (r, _) = decide(&mut s, wall_scrolled_off(), &opts);
        assert!(
            r.wall.is_none() && !s.turn_end.waits_on_named_reset(),
            "{r:#?}"
        );
        if moves {
            *host.account.lock().unwrap() = Some(key("account-b"));
        }
        host.set(&[(five, reading(100, reset, TEST_NOW + 1))]);
        let _ = decide(&mut s, wall_scrolled_off(), &opts);
        assert!(s.turn_end.waits_on_named_reset(), "the tap named the reset");
        let (r, _) = decide(&mut s, wall_scrolled_off(), &opts);
        r.login_moved
    };
    assert!(run(true), "the move is the limit's");
    assert!(!run(false), "no move");
}

fn countdown_wall(cancelled: bool) -> Vec<String> {
    let mut r = rows(&[
        "❯ carry on with the task",
        "  ⎿  You've hit your session limit · resets 11:35am (America/Los_Angeles)",
        "     /upgrade to increase your usage limit.",
        "",
        "⏺ Usage limit reached · continuing automatically at 11:35am · esc to cancel",
        "",
        "✻ Baked for 0s · done 8:54 AM",
        "",
    ]);
    if cancelled {
        r.extend(rows(&[
            "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
            "",
        ]));
    }
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

fn countdown_tail() -> String {
    let at = aterm_types::rfc3339::format_rfc3339(
        u64::try_from(TEST_NOW - 30).expect("after the epoch"),
    );
    [
        format!(
            r#"{{"parentUuid":null,"isSidechain":false,"type":"user","message":{{"role":"user","content":"carry on with the task"}},"uuid":"u1","timestamp":"{at}","sessionId":"c-1"}}"#
        ),
        format!(
            r#"{{"parentUuid":"u1","isSidechain":false,"type":"assistant","uuid":"n1","timestamp":"{at}","message":{{"model":"<synthetic>","role":"assistant","content":[{{"type":"text","text":"You've hit your session limit · resets 11:35am (America/Los_Angeles)"}}]}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"sessionId":"c-1"}}"#
        ),
        format!(
            r#"{{"parentUuid":"n1","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":40,"timestamp":"{at}","uuid":"d1","sessionId":"c-1"}}"#
        ),
    ]
    .join("\n")
        + "\n"
}

#[test]
fn a_left_walls_wait_is_retimed_by_the_tap_read_while_it_waits() {
    let five = UsageWindow::FiveHour;
    let n = TEST_NOW + 2 * 3600 + 40 * 60;
    let journal = |path: &Path| -> Vec<String> {
        let (records, _) = journal_records(path);
        records
            .into_iter()
            .filter(|r| r.kind == "waiting")
            .map(|r| r.summary)
            .collect()
    };
    let run = |offset: Option<i64>| {
        let tag = offset.map_or_else(|| "none".to_string(), |o| o.to_string());
        let (ldir, ledger) = ledger_at(&format!("tap-left-{tag}"));
        let tail = countdown_tail();
        let host = TapHost::reading_tail(&[(five, None)], Some(&tail));
        host.after.store(2, Ordering::SeqCst);
        if let Some(offset) = offset {
            *host.later.lock().unwrap() =
                Some(vec![(five, reading(100, n + offset, TEST_NOW + 1))]);
        }
        let (dir, path) = journal_file(&format!("tap-left-{tag}"));
        let mut m = Mock::new(vec![
            busy_screen(),
            countdown_wall(false),
            countdown_wall(true),
        ]);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.vanish_after = Some(40);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..opts_with(&host)
        };
        let mut out: Vec<u8> = Vec::new();
        let (notice, limit) = {
            let mut s = tapped_virtual(&mut m, Some(ledger.clone()));
            let _ = s.watch(&opts, &mut out);
            (
                s.wall_reset.as_ref().and_then(|(_, u)| *u),
                s.turn_end.waited_limit().map(|w| s.unix_of(w.at)),
            )
        };
        let waits = journal(&path);
        let typed = count(&m, "turn");
        let _ = std::fs::remove_dir_all(&dir);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let open = crate::supervise::approvals::open_wall(&ledger, Some(SID));
        let (dir, path) = journal_file(&format!("tap-left-next-{tag}"));
        let mut m = Mock::new(vec![countdown_wall(true)]);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.vanish_after = Some(20);
        let quiet = TapHost::reading_tail(&[(five, None)], Some(&tail));
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..opts_with(&quiet)
        };
        {
            let mut s = tapped_virtual(&mut m, Some(ledger.clone()));
            let _ = s.watch(&opts, &mut out);
        }
        let next = journal(&path);
        let next_typed = count(&m, "turn");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&ldir);
        (waits, typed, (notice, limit), rows, open, next, next_typed)
    };
    let stamp = |unix: i64, why: &str| {
        let (y, mo, d) = limit::civil(unix.div_euclid(86_400));
        let s = unix.rem_euclid(86_400);
        format!(
            "until={y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z {why}",
            s / 3600,
            s % 3600 / 60,
            s % 60
        )
    };
    let t = TurnEndTiming::default();
    let grace = i64::try_from(t.reset_grace.as_secs()).unwrap();
    let auto = i64::try_from(t.auto_resume_grace.as_secs()).unwrap();
    let share = |at| i64::try_from(resume_jitter(SID, at, t.reset_jitter).as_secs()).unwrap();
    let resets = |at| stamp(at + grace + share(at), "usage-session resets");
    let taps = |rows: &str| {
        rows.lines()
            .filter(|l| l.contains(&format!("\"rule_id\":\"{RULE_TAP_RESET}\"")))
            .count()
    };
    for offset in [28, 421] {
        let (waits, typed, (notice, limit), rows, open, next, next_typed) = run(Some(offset));
        assert_eq!(notice, Some(n), "11:35am");
        assert_eq!(limit, Some(n + offset), "{offset}: the limit's reset");
        assert_eq!(typed, 0, "{waits:#?}");
        assert!(
            waits
                .iter()
                .all(|w| *w != stamp(n + offset + auto, "usage-session goes on by itself")),
            "the cancel is kept: {waits:#?}"
        );
        assert!(
            waits.contains(&resets(n)),
            "the cancel: the notice's reset: {waits:#?}"
        );
        assert_eq!(
            waits.last(),
            Some(&resets(n + offset)),
            "{offset}: the tap's reset: {waits:#?}"
        );
        assert!(
            rows.contains(&format!(
                "reset_unix={n}; reset=11:35am; kind=usage-session; tap_unix={};",
                n + offset
            )),
            "{offset}: {rows}"
        );
        assert!(open.is_some(), "{offset}: {rows}");
        if offset > limit::CLOCK_SLACK_S {
            assert_eq!(taps(&rows), 1, "{rows}");
            assert!(
                rows.contains(&format!("notice_unix={n}; tap_unix={}", n + offset)),
                "{rows}"
            );
        } else {
            assert_eq!(taps(&rows), 0, "within the slack: {rows}");
        }
        assert_eq!(next_typed, 0, "{next:#?}");
        assert_eq!(
            next.last(),
            Some(&resets(n + offset)),
            "{offset}: the successor: {next:#?}"
        );
    }
    let (waits, typed, (_, limit), rows, open, next, next_typed) = run(None);
    assert_eq!(typed, 0);
    assert_eq!(limit, Some(n), "the notice's reset");
    assert_eq!(waits.last(), Some(&resets(n)), "{waits:#?}");
    assert_eq!(open.and_then(|o| o.reset_unix), Some(n), "{rows}");
    assert!(!rows.contains("tap_unix="), "{rows}");
    assert_eq!(taps(&rows), 0, "{rows}");
    assert_eq!(next_typed, 0, "{next:#?}");
    assert_eq!(next.last(), Some(&resets(n)), "{next:#?}");
}

#[test]
fn a_wall_met_again_is_read_afresh_by_the_tap() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 2 * 3600;
    let later = reset + 3600;
    let rolled = reading(3, later + 4 * 3600, later);
    let host = TapHost::answering(&[(five, reading(100, reset, TEST_NOW))]);
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, None);
    let (r, _) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(r.reset_at.map(|at| s.unix_of(at)), Some(reset));
    let (r, _) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(r.reset_at.map(|at| s.unix_of(at)), Some(reset), "kept");
    s.set_clock(later, PDT);
    host.set(&[(five, rolled)]);
    let screen = read_screen(session_wall(None));
    let r = s.turn_end_reading(&screen, Some(Duration::from_secs(600)), &opts);
    let now = Instant::now();
    s.observe_point(&r, now, screen.seq);
    let a = decide_turn_end(&s.turn_end, &r, &opts.policy, now);
    assert_eq!(r.reset_at, None, "the new answer's reset is unknown");
    assert!(
        matches!(a, TurnEndAction::WaitUntil { .. }),
        "the back-off, not the past reset: {a:?}"
    );
    let host = TapHost::answering(&[(five, reading(100, reset, TEST_NOW))]);
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, None);
    let _ = decide(&mut s, session_wall(None), &opts);
    s.set_clock(later, PDT);
    let (r, a) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(r.reset_at.map(|at| s.unix_of(at)), Some(reset));
    assert!(matches!(a, TurnEndAction::Type { .. }), "continued: {a:?}");
    s.turn_end.acted(&a, &r, Instant::now());
    host.set(&[(five, rolled)]);
    let (r, _) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(
        r.reset_at, None,
        "the stale reset is no reset of the answer"
    );
}

/// A wall that answers the loop's own act, read by the footer's `11:35am` and then, the footer
/// gone, by the turn's notice `11:35am (America/Los_Angeles)`, is one answer of the wall: the
/// tap's timing of it stands before the tap writes again, and the notice's `wall-wait@v1` row
/// names the tap's reset. CONTROL: a re-read naming another clock time is a new answer, waited
/// to the notice's reset.
#[test]
fn a_walls_answer_read_again_with_its_zone_named_keeps_the_taps_timing() {
    use crate::supervise::policy::turn_end::RULE_LAND;
    let five = UsageWindow::FiveHour;
    let ww = |rows: &str| -> Vec<String> {
        rows.lines()
            .filter(|l| l.contains(RULE_WALL_WAIT))
            .map(str::to_owned)
            .collect()
    };
    let run = |again: &str, tag: &str| {
        let (dir, ledger) = ledger_at(tag);
        let host = TapHost::answering(&[(five, None)]);
        let opts = opts_with(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = tapped(&mut m, Some(ledger.clone()));
        let (r, _) = decide(&mut s, session_wall(Some("11:35am")), &opts);
        let n = s.wall_reset.as_ref().and_then(|(_, u)| *u).expect("placed");
        let land = TurnEndAction::Type {
            text: "land it".to_owned(),
            rule_id: RULE_LAND,
        };
        s.turn_end.acted(&land, &r, Instant::now());
        assert!(s.turn_end.untaken_act().is_some(), "the act is untaken");
        host.set(&[(five, reading(100, n + 120, TEST_NOW))]);
        let (r, _) = decide(&mut s, session_wall(Some("11:35am")), &opts);
        assert_eq!(reset_of(&s, &r), Some(n + 120), "the tap's, in the slack");
        let before = ww(&std::fs::read_to_string(&ledger).unwrap_or_default()).len();
        s.set_clock(TEST_NOW + 30, PDT);
        let (r, _) = decide(&mut s, session_wall(Some(again)), &opts);
        let got = reset_of(&s, &r);
        let notice = s.wall_reset.as_ref().and_then(|(_, u)| *u);
        drop(s);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let after = ww(&rows).split_off(before);
        (n, got, notice, after, rows)
    };
    let (n, got, _, after, rows) = run("11:35am (America/Los_Angeles)", "tap-answer-zone");
    assert_eq!(got, Some(n + 120), "the tap's timing stands: {rows}");
    let tapped = format!("; tap_unix={};", n + 120);
    assert!(
        !after.is_empty() && after.iter().all(|l| l.contains(&tapped)),
        "the notice's row names the tap's reset: {after:#?}"
    );
    let (n, got, notice, _, rows) = run("12:35pm (America/Los_Angeles)", "tap-answer-other");
    assert_ne!(notice, Some(n), "another clock time: {rows}");
    assert_eq!(got, notice, "a new answer: the notice's reset: {rows}");
}

#[test]
fn a_spend_wall_is_never_timed_by_the_tap() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 2 * 3600;
    let spend = {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've hit your monthly spend limit.",
            "",
            "✻ Worked for 3m 2s · done 4:51 PM",
            "",
        ]);
        r.extend(composer("  ? for shortcuts"));
        r
    };
    let host = TapHost::answering(&[(five, reading(100, reset, TEST_NOW))]);
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, None);
    let (r, a) = decide(&mut s, spend, &opts);
    assert_eq!(r.wall, Some(aterm_phase::WallKind::Spend), "{r:#?}");
    assert_eq!((r.reset_at, host.asks()), (None, 0));
    assert!(
        matches!(&a, TurnEndAction::Escalate { reason } if reason.contains("only usage credits")),
        "{a:?}"
    );
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, None);
    let (r, _) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(r.reset_at.map(|at| s.unix_of(at)), Some(reset));
}

fn standing_idle() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Running the suites before the release notes.",
        "",
        "⏺ Auto mode lets Claude handle permission prompts automatically — Claude checks \
         each tool call for risky actions and prompt injection",
        "  before executing. Ideal for long-running tasks. Shift+Tab to change mode.",
        "",
    ]);
    r.extend(composer(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
    ));
    r
}

fn weekly_standing(until: i64, answer: Option<TapWindow>) -> Arc<TapHost> {
    let host = Arc::new(TapHost {
        standing: Some(crate::harness::upgrade::StandingLimit {
            text: "You've hit your weekly limit · resets Sep 18 at 8pm (America/Los_Angeles)"
                .to_string(),
            until: u64::try_from(until).ok(),
            window: Some("seven_day".to_string()),
            at: u64::try_from(TEST_NOW - 3600).ok(),
            row: Some("row-weekly".to_string()),
        }),
        ..TapHost::default()
    });
    host.set(&[(UsageWindow::SevenDay, answer)]);
    host
}

#[test]
fn a_transcript_limit_is_timed_by_the_tap_against_its_rows_reset() {
    let until = TEST_NOW + 30 * 3600;
    let tap = until + 45 * 60;
    let idle = standing_idle();
    let run = |answer: Option<TapWindow>| {
        let (dir, ledger) = ledger_at(&format!("tap-standing-{}", answer.is_some()));
        let opts = opts_with(&weekly_standing(until, answer));
        let mut m = Mock::new(vec![idle.clone()]);
        let mut s = tapped(&mut m, Some(ledger.clone()));
        let (r, a) = decide(&mut s, idle.clone(), &opts);
        assert_eq!(r.wall, Some(aterm_phase::WallKind::UsageWeekly), "{r:#?}");
        let at = r.reset_at.map(|at| s.unix_of(at));
        let waited = until_unix(&s, &a);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (at, r.jitter, waited, rows)
    };
    let t = TurnEndTiming::default();
    let grace = i64::try_from(t.reset_grace.as_secs()).unwrap();
    let share = |at| resume_jitter(SID, at, t.reset_jitter);
    let secs = |d: Duration| i64::try_from(d.as_secs()).unwrap();
    let wall_row = |rows: &str, tapped: &str| {
        rows.lines().any(|l| {
            l.contains(RULE_WALL_WAIT)
                && l.contains(&format!("reset_unix={until};"))
                && l.contains(&format!("kind=usage-weekly{tapped}; message="))
        })
    };
    let (at, jitter, waited, rows) = run(reading(100, tap, TEST_NOW));
    assert_eq!(at, Some(tap), "the later: {rows}");
    assert_eq!(jitter, share(tap));
    assert_eq!(waited, tap + grace + secs(share(tap)));
    assert!(wall_row(&rows, &format!("; tap_unix={tap}")), "{rows}");
    assert!(
        rows.contains(&format!("notice_unix={until}; tap_unix={tap}")),
        "{rows}"
    );
    let (at, jitter, waited, rows) = run(None);
    assert_eq!(at, Some(until));
    assert_eq!(jitter, share(until));
    assert_eq!(waited, until + grace + secs(share(until)));
    assert!(wall_row(&rows, ""), "{rows}");
    assert!(!rows.contains(RULE_TAP_RESET), "{rows}");
}

fn reset_of(s: &Session<'_, Mock>, r: &TurnEndReading) -> Option<i64> {
    r.reset_at.map(|at| s.unix_of(at))
}

fn cut_wall_written(tag: &str, read: Option<TapWindow>) -> (PathBuf, PathBuf) {
    let (dir, ledger) = ledger_at(tag);
    let host = TapHost::answering(&[(UsageWindow::FiveHour, read)]);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, Some(ledger.clone()));
    let _ = decide(&mut s, session_wall(None), &opts_with(&host));
    (dir, ledger)
}

/// A loop that takes the session over from the loop that wrote `ledger`, seeded off it as a
/// loop's start seeds itself.
fn successor<'a>(m: &'a mut Mock, ledger: &Path) -> Session<'a, Mock> {
    let mut s = tapped(m, Some(ledger.to_path_buf()));
    let now_ms = s.now_unix_ms();
    let open = s.seed_from_ledger(ledger, now_ms, false);
    s.seed_wall_memory(open);
    s
}

#[test]
fn a_successor_reads_a_cut_wall_met_again_afresh() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 2 * 3600;
    let (dir, ledger) = cut_wall_written("tap-next-cut", reading(100, reset, TEST_NOW));
    let host = TapHost::answering(&[(five, None)]);
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = successor(&mut m, &ledger);
    let (r, _) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(reset_of(&s, &r), Some(reset), "the tap's reset, carried");
    let later = reset + 3600;
    s.set_clock(later, PDT);
    let (r, a) = decide(&mut s, session_wall(None), &opts);
    assert!(matches!(a, TurnEndAction::Type { .. }), "continued: {a:?}");
    s.turn_end.acted(&a, &r, Instant::now());
    host.set(&[(five, reading(3, later + 4 * 3600, later))]);
    let (r, a) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(r.reset_at, None, "the past reset is no reset of the answer");
    assert!(
        matches!(a, TurnEndAction::WaitUntil { .. }),
        "the back-off: {a:?}"
    );
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    let (dir, ledger) = cut_wall_written("tap-next-cut-none", None);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = successor(&mut m, &ledger);
    let quiet = opts_with(&TapHost::answering(&[(five, None)]));
    let (r, a) = decide(&mut s, session_wall(None), &quiet);
    assert_eq!(r.reset_at, None);
    let backoff = TurnEndTiming::default()
        .limit_backoff
        .last()
        .unwrap()
        .as_secs();
    let backoff = i64::try_from(backoff).unwrap();
    assert_eq!(until_unix(&s, &a), TEST_NOW + backoff, "{a:?}");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_successors_own_tap_reading_at_a_cut_wall_is_taken_alone() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 2 * 3600;
    let run = |read: Option<TapWindow>| {
        let tag = format!("tap-next-own-{}", read.is_some());
        let (dir, ledger) = cut_wall_written(&tag, reading(100, reset, TEST_NOW));
        let host = TapHost::answering(&[(five, read)]);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = successor(&mut m, &ledger);
        let (r, _) = decide(&mut s, session_wall(None), &opts_with(&host));
        let got = reset_of(&s, &r);
        drop(s);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (got, rows)
    };
    let (got, rows) = run(reading(100, reset + 1200, TEST_NOW));
    assert!(
        !rows.contains(RULE_TAP_RESET),
        "no notice named one: {rows}"
    );
    assert_eq!(got, Some(reset + 1200));
    let (got, _) = run(None);
    assert_eq!(got, Some(reset));
}

#[test]
fn a_successor_sets_its_own_tap_against_the_notices_reset() {
    let five = UsageWindow::FiveHour;
    let words = "11:35am (America/Los_Angeles)";
    let run = |tap: Option<i64>| {
        let (dir, ledger) = ledger_at(&format!("tap-next-named-{}", tap.is_some()));
        let n = {
            let host = TapHost::answering(&[(five, None)]);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut s = tapped(&mut m, Some(ledger.clone()));
            let _ = decide(&mut s, session_wall(Some(words)), &opts_with(&host));
            let n = s.wall_reset.as_ref().and_then(|(_, u)| *u).expect("placed");
            if let Some(off) = tap {
                host.set(&[(five, reading(100, n + off, TEST_NOW))]);
                let (r, _) = decide(&mut s, session_wall(Some(words)), &opts_with(&host));
                assert_eq!(
                    reset_of(&s, &r),
                    Some(n + off),
                    "inside the slack: the tap's"
                );
            }
            n
        };
        let host = TapHost::answering(&[(five, reading(100, n - 500, TEST_NOW))]);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = successor(&mut m, &ledger);
        let (r, _) = decide(&mut s, session_wall(Some(words)), &opts_with(&host));
        let got = reset_of(&s, &r);
        drop(s);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (n, got, rows)
    };
    let (n, got, rows) = run(Some(-240));
    assert_eq!(got, Some(n), "the later, the notice's: {rows}");
    assert!(
        rows.contains(&format!("notice_unix={n}; tap_unix={}", n - 500)),
        "{rows}"
    );
    let (n, got, _) = run(None);
    assert_eq!(got, Some(n));
}

/// A successor whose tap reads the pair the loop before it wrote down last writes no second
/// tap-reset row, that pair being one a second disagreement wrote after the wall's last
/// wall-wait row (the waited reset, the notice's, did not move). CONTROL: a pair other than the
/// last is written again.
#[test]
fn a_successor_writes_no_second_tap_reset_row_for_the_pair_the_loop_before_wrote() {
    let five = UsageWindow::FiveHour;
    let words = "11:35am (America/Los_Angeles)";
    let tap_row = format!("\"rule_id\":\"{RULE_TAP_RESET}\"");
    let run = |before: &[i64], again: i64| {
        let tag = format!("tap-next-said-{}-{}", before.len(), -again);
        let (dir, ledger) = ledger_at(&tag);
        let n = {
            let host = TapHost::answering(&[(five, None)]);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut s = tapped(&mut m, Some(ledger.clone()));
            let _ = decide(&mut s, session_wall(Some(words)), &opts_with(&host));
            let n = s.wall_reset.as_ref().and_then(|(_, u)| *u).expect("placed");
            for off in before {
                host.set(&[(five, reading(100, n + off, TEST_NOW))]);
                let (r, _) = decide(&mut s, session_wall(Some(words)), &opts_with(&host));
                assert_eq!(reset_of(&s, &r), Some(n), "the later, the notice's");
            }
            n
        };
        let written = std::fs::read_to_string(&ledger).unwrap_or_default();
        let host = TapHost::answering(&[(five, reading(100, n + again, TEST_NOW))]);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = successor(&mut m, &ledger);
        let (r, _) = decide(&mut s, session_wall(Some(words)), &opts_with(&host));
        let got = reset_of(&s, &r);
        drop(s);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let taps = rows.lines().filter(|l| l.contains(&tap_row)).count();
        assert_eq!(got, Some(n), "{rows}");
        (n, written, taps, rows)
    };
    let (_, _, taps, rows) = run(&[-500], -500);
    assert_eq!(taps, 1, "the pair the loop before wrote: {rows}");
    let (n, _, taps, rows) = run(&[-500], -900);
    assert_eq!(taps, 2, "a new pair: {rows}");
    assert!(
        rows.contains(&format!("notice_unix={n}; tap_unix={}", n - 900)),
        "{rows}"
    );
    let (n, written, taps, rows) = run(&[-500, -900], -900);
    let lines: Vec<&str> = written.lines().collect();
    let last = |rule: &str| {
        lines
            .iter()
            .rposition(|l| l.contains(&format!("\"rule_id\":\"{rule}\"")))
    };
    assert!(
        last(RULE_TAP_RESET) > last(RULE_WALL_WAIT),
        "the second pair's row came after the wall's last row: {written}"
    );
    assert!(
        written.contains(&format!("notice_unix={n}; tap_unix={}", n - 900)),
        "{written}"
    );
    assert_eq!(taps, 2, "the second pair, the last written: {rows}");
    let (_, _, taps, rows) = run(&[-500, -900], -500);
    assert_eq!(taps, 3, "the first pair again, after another: {rows}");
}

/// A Claude Code turn that ended on the wall `notice` names.
fn usage_wall(notice: &str) -> Vec<String> {
    let mut r = rows(&[
        "❯ carry on with the task",
        &format!("  ⎿  {notice}"),
        "     /upgrade to increase your usage limit.",
        "",
        "✻ Cooked for 0s · done 8:54 AM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// One point of a run [`tap_reset_rows`] drives: a decision at a wall's screen with the tap's
/// answers, or an act the loop typed.
#[derive(Clone)]
enum Point {
    Wall(Vec<String>, Answers),
    Typed,
}

/// The tap-reset rows a run of `points` writes, a loop that takes the session over starting
/// before each point `restarts` names, with the run's ledger.
fn tap_reset_rows(tag: &str, points: &[Point], restarts: &[usize]) -> (usize, String) {
    let (dir, ledger) = ledger_at(tag);
    let host = TapHost::answering(&[]);
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut at = 0;
    while at < points.len() {
        let mut s = if at == 0 {
            tapped(&mut m, Some(ledger.clone()))
        } else {
            successor(&mut m, &ledger)
        };
        loop {
            match &points[at] {
                Point::Wall(screen, answers) => {
                    host.set(answers);
                    let _ = decide(&mut s, screen.clone(), &opts);
                }
                Point::Typed => s.ledger_row(
                    crate::supervise::policy::turn_end::RULE_CONTINUE,
                    crate::supervise::approvals::Outcome::Typed,
                    "carry on",
                    "the turn-end policy (an act typed at the wall)",
                    1,
                ),
            }
            at += 1;
            if at == points.len() || restarts.contains(&at) {
                break;
            }
        }
    }
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let taps = rows
        .lines()
        .filter(|l| l.contains(&format!("\"rule_id\":\"{RULE_TAP_RESET}\"")))
        .count();
    (taps, rows)
}

/// A loop that takes the session over at any point, or at every point, writes the tap-reset
/// rows one loop writes: one per pair at its wall until an act is typed, through a second
/// disagreement that moves no waited reset, a notice of another kind naming the same reset, a
/// typed act, and a wall of another class met (whose wall-wait-end row the point writes after
/// the new wall's pair). CONTROL: each run with no restart writes the rows named.
#[test]
fn a_successor_at_any_point_writes_the_tap_reset_rows_one_loop_writes() {
    use aterm_phase::WallKind;
    let five = UsageWindow::FiveHour;
    let week = UsageWindow::SevenDay;
    let words = "11:35am (America/Los_Angeles)";
    let session = usage_wall(&format!("You've hit your session limit · resets {words}"));
    let weekly = usage_wall(&format!("You've hit your weekly limit · resets {words}"));
    let bucket = usage_wall("You've hit your Opus limit");
    let n = {
        let host = TapHost::answering(&[(five, None)]);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = tapped(&mut m, None);
        let (r, _) = decide(&mut s, session.clone(), &opts_with(&host));
        assert_eq!(r.wall, Some(WallKind::UsageSession), "{r:#?}");
        let n = s.wall_reset.as_ref().and_then(|(_, u)| *u).expect("placed");
        let (r, _) = decide(&mut s, weekly.clone(), &opts_with(&host));
        assert_eq!(r.wall, Some(WallKind::UsageWeekly), "{r:#?}");
        let weekly_n = s.wall_reset.as_ref().and_then(|(_, u)| *u);
        assert_eq!(weekly_n, Some(n), "the weekly notice names the same reset");
        n
    };
    let tap = |w: UsageWindow, off: Option<i64>| -> Answers {
        vec![(w, off.and_then(|off| reading(100, n + off, TEST_NOW)))]
    };
    let at = |screen: &Vec<String>, w, off| Point::Wall(screen.clone(), tap(w, off));
    let pair = |off: i64| format!("notice_unix={n}; tap_unix={}", n + off);
    let scenarios: Vec<(&str, Vec<Point>, Vec<String>)> = vec![
        (
            "second",
            vec![
                at(&session, five, None),
                at(&session, five, Some(-500)),
                at(&session, five, Some(-900)),
                at(&session, five, Some(-900)),
            ],
            vec![pair(-500), pair(-900)],
        ),
        (
            "kind",
            vec![
                at(&session, five, None),
                at(&session, five, Some(-500)),
                at(&weekly, week, Some(100)),
                at(&weekly, week, Some(-500)),
                at(&weekly, week, Some(-500)),
            ],
            vec![pair(-500), pair(-500)],
        ),
        (
            "typed",
            vec![
                at(&session, five, None),
                at(&session, five, Some(-500)),
                Point::Typed,
                at(&session, five, Some(-500)),
                at(&session, five, Some(-500)),
            ],
            vec![pair(-500), pair(-500)],
        ),
        (
            "class",
            vec![
                Point::Wall(bucket.clone(), Vec::new()),
                at(&session, five, Some(-500)),
                at(&session, five, Some(-500)),
            ],
            vec![pair(-500)],
        ),
    ];
    for (name, points, said) in scenarios {
        let (taps, rows) = tap_reset_rows(&format!("tap-any-{name}"), &points, &[]);
        let written: Vec<&str> = rows
            .lines()
            .filter(|l| l.contains(&format!("\"rule_id\":\"{RULE_TAP_RESET}\"")))
            .collect();
        assert_eq!(taps, said.len(), "{name}: {rows}");
        for (row, pair) in written.iter().zip(&said) {
            assert!(row.contains(pair.as_str()), "{name}: {pair} in {row}");
        }
        let every: Vec<usize> = (1..points.len()).collect();
        let mut restarts: Vec<Vec<usize>> = every.iter().map(|k| vec![*k]).collect();
        restarts.push(every);
        for at in restarts {
            let tag = format!("tap-any-{name}-{at:?}").replace([' ', ',', '[', ']'], "");
            let (got, rows) = tap_reset_rows(&tag, &points, &at);
            assert_eq!(got, said.len(), "{name}, restarted before {at:?}: {rows}");
        }
    }
    let (_, rows) = tap_reset_rows(
        "tap-any-class-shape",
        &[
            Point::Wall(bucket, Vec::new()),
            at(&session, five, Some(-500)),
        ],
        &[],
    );
    let order: Vec<&str> = rows
        .lines()
        .filter_map(|l| {
            [RULE_TAP_RESET, RULE_WALL_WAIT_END, RULE_WALL_WAIT]
                .into_iter()
                .find(|r| l.contains(&format!("\"rule_id\":\"{r}\"")))
        })
        .collect();
    assert!(
        order.ends_with(&[
            RULE_TAP_RESET,
            RULE_WALL_WAIT,
            RULE_WALL_WAIT_END,
            RULE_WALL_WAIT
        ]),
        "the new wall's pair before the point's end of the old track: {order:?} {rows}"
    );
}

/// The pair a successor holds, as the ledger's rows fold it: kept through a wall-wait-end row
/// (a point ends the old track after the new wall's pair), named by the wall row after it (not
/// the open wall before it), and ended by a typed act alone (a fact row is none). CONTROL: no
/// pair, none held.
#[test]
fn the_pair_a_successor_holds_is_folded_off_the_rows_in_their_order() {
    use crate::supervise::approvals::{
        LIMIT_ENDED, Outcome, Row, TapSaid, tap_reset_reason, tap_said_in, wall_wait_reason_of,
    };
    use aterm_phase::WallKind;
    let n = TEST_NOW + 7200;
    let row = |rule_id: &str, outcome: Outcome, reason: &str| {
        Row {
            rule_id,
            outcome,
            command: "",
            reason,
            box_seq: 1,
        }
        .to_json(TEST_NOW * 1000, Some(SID))
    };
    let wall = |kind: WallKind| {
        let reason =
            wall_wait_reason_of(Some(n), Some(n), None, Some("11:35am"), kind, "the limit");
        row(RULE_WALL_WAIT, Outcome::Waiting, &reason)
    };
    let tap = row(
        RULE_TAP_RESET,
        Outcome::Waiting,
        &tap_reset_reason(n, n - 500, n),
    );
    let end = row(RULE_WALL_WAIT_END, Outcome::Waiting, LIMIT_ENDED);
    let typed = row("continue@v1", Outcome::Typed, "carry on");
    let fact = row("fact-hold@v1", Outcome::Typed, "a fact");
    let session = wall(WallKind::UsageSession);
    let weekly = wall(WallKind::UsageWeekly);
    let held = |rows: &[&String]| {
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        tap_said_in(&body, Some(SID))
    };
    let at = |key: Option<&str>| {
        Some(TapSaid {
            pair: (n, n - 500),
            key: key.map(str::to_owned),
        })
    };
    let session_key = Some("usage-session|11:35am");
    assert_eq!(held(&[&tap, &session, &end, &session]), at(session_key));
    assert_eq!(held(&[&weekly, &tap, &session]), at(session_key));
    assert_eq!(held(&[&session, &tap]), at(None), "the open wall's");
    assert_eq!(
        held(&[&tap, &session, &end, &weekly]),
        at(session_key),
        "the session wall's, which a weekly tap's key is not"
    );
    assert_eq!(held(&[&tap, &session, &fact, &session]), at(session_key));
    assert_eq!(held(&[&tap, &typed, &session]), None);
    assert_eq!(held(&[&session, &tap, &session, &typed]), None);
    assert_eq!(held(&[&session, &end, &session]), None);
}

#[test]
fn a_successor_waits_to_the_taps_reset_over_a_transcript_limits_row() {
    let until = TEST_NOW + 30 * 3600;
    let tap = until + 45 * 60;
    let idle = standing_idle();
    let run = |answer: Option<TapWindow>| {
        let (dir, ledger) = ledger_at(&format!("tap-next-standing-{}", answer.is_some()));
        {
            let mut m = Mock::new(vec![idle.clone()]);
            let mut s = tapped(&mut m, Some(ledger.clone()));
            let opts = opts_with(&weekly_standing(until, answer));
            let _ = decide(&mut s, idle.clone(), &opts);
        }
        let mut m = Mock::new(vec![idle.clone()]);
        let mut s = successor(&mut m, &ledger);
        let opts = opts_with(&weekly_standing(until, None));
        let (r, _) = decide(&mut s, idle.clone(), &opts);
        assert_eq!(r.wall, Some(aterm_phase::WallKind::UsageWeekly), "{r:#?}");
        let got = reset_of(&s, &r);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
        got
    };
    assert_eq!(
        run(reading(100, tap, TEST_NOW)),
        Some(tap),
        "the tap's, later"
    );
    assert_eq!(run(None), Some(until), "the row's");
}

/// A successor that reads the wall's reset from another of its rows (Claude Code's footer names
/// `10:58pm`, the turn's notice `10:58pm (America/Los_Angeles)`, the countdown's Esc taking the
/// footer away) still waits to the tap's reset the loop before it timed, before its own tap
/// reads: the same reset with its zone named is no new answer of the wall. CONTROL: a notice
/// naming another clock time drops the carried reset.
#[test]
fn a_successor_keeps_the_taps_reset_where_the_notice_names_its_zone() {
    let five = UsageWindow::FiveHour;
    let run = |seen: &str, tag: &str| {
        let (dir, ledger) = ledger_at(tag);
        let (n, tap) = {
            let host = TapHost::answering(&[(five, None)]);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut s = tapped(&mut m, Some(ledger.clone()));
            let _ = decide(&mut s, session_wall(Some("11:35am")), &opts_with(&host));
            let n = s.wall_reset.as_ref().and_then(|(_, u)| *u).expect("placed");
            host.set(&[(five, reading(100, n + 120, TEST_NOW))]);
            let (r, _) = decide(&mut s, session_wall(Some("11:35am")), &opts_with(&host));
            assert_eq!(reset_of(&s, &r), Some(n + 120), "the tap's, in the slack");
            (n, n + 120)
        };
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        assert!(
            rows.lines()
                .any(|l| l.contains(RULE_WALL_WAIT) && l.contains(&format!("; tap_unix={tap};"))),
            "{rows}"
        );
        let first = rows.lines().filter(|l| l.contains(RULE_WALL_WAIT)).count();
        let host = TapHost::answering(&[(five, None)]);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = successor(&mut m, &ledger);
        let (r, _) = decide(&mut s, session_wall(Some(seen)), &opts_with(&host));
        let got = reset_of(&s, &r);
        let jitter = r.jitter;
        drop(s);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let theirs: Vec<String> = rows
            .lines()
            .filter(|l| l.contains(RULE_WALL_WAIT))
            .skip(first)
            .map(str::to_owned)
            .collect();
        (n, tap, got, jitter, theirs)
    };
    let (_, tap, got, jitter, theirs) = run("11:35am (America/Los_Angeles)", "tap-next-zone");
    assert_eq!(got, Some(tap), "the carried tap's reset: {theirs:#?}");
    assert_eq!(
        jitter,
        resume_jitter(SID, tap, TurnEndTiming::default().reset_jitter),
        "the share of the tap's reset"
    );
    assert!(
        !theirs.is_empty()
            && theirs
                .iter()
                .all(|l| l.contains(&format!("; tap_unix={tap};"))),
        "the successor writes the notice's row, naming the tap's reset: {theirs:#?}"
    );
    let (n, _, got, _, _) = run("12:35pm (America/Los_Angeles)", "tap-next-zone-other");
    assert_ne!(got, Some(n + 120), "another reset: the carried one dropped");
    use crate::supervise::run::turn_end_loop::same_reset;
    let s = "usage-session|";
    for (a, b) in [
        ("Oct 20 at 9am", "Oct 20 at 9AM (Etc/GMT+5)"),
        ("11pm", "11:00pm (America/Los_Angeles)"),
        ("10:58pm PDT", "10:58pm (America/Los_Angeles)"),
        ("10:58pm", "10:58pm PDT"),
        ("in 2h", "in 2h"),
    ] {
        assert!(
            same_reset(&format!("{s}{a}"), &format!("{s}{b}")),
            "{a} | {b}"
        );
    }
    for (a, b) in [
        ("11:35am", "in 2h (12:00pm)"),
        ("11:35am", "11:35am (in 2 hours)"),
        ("11:35am", "11:35am (3pm)"),
        ("11:35am", "11:36am (America/Los_Angeles)"),
        ("11:35am", "Oct 20 at 11:35am"),
        ("10:58pm (UTC)", "10:58pm (America/Los_Angeles)"),
        ("11:35am (America/Los_Angeles)", "11:35am (Europe/London)"),
        ("in 2h (3pm)", "in 2h (4pm)"),
    ] {
        assert!(
            !same_reset(&format!("{s}{a}"), &format!("{s}{b}")),
            "{a} | {b}"
        );
    }
    assert!(!same_reset(
        "usage-session|11:35am",
        "usage-weekly|11:35am (America/Los_Angeles)"
    ));
}
