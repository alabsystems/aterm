// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::supervise::approvals::{RULE_TAP_RESET, RULE_WALL_WAIT};
use crate::supervise::policy::turn_end::decide_turn_end;
use crate::supervise::run::turn_end_loop::resume_jitter;
use crate::supervise::tap_clock::{TapWindow, UsageWindow};

type Answers = Vec<(UsageWindow, Option<TapWindow>)>;

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
    let notice = reset.map_or_else(
        || "  ⎿  You've hit your session limit".to_string(),
        |r| format!("  ⎿  You've hit your session limit · resets {r}"),
    );
    let mut r = rows(&[
        "❯ carry on with the task",
        &notice,
        "     /upgrade to increase your usage limit.",
        "",
        "✻ Cooked for 0s · done 8:54 AM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
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

fn successor<'a>(m: &'a mut Mock, ledger: &Path) -> Session<'a, Mock> {
    let mut s = tapped(m, Some(ledger.to_path_buf()));
    s.seed_wall_memory(crate::supervise::approvals::open_wall(ledger, Some(SID)));
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
