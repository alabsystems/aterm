// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The usage tap times the wait at a usage wall, in the loop (accounts v2 S10a;
// `tap_clock.rs`, `Session::tap_reset`, `Session::tap_edge`): the reset the
// policy is handed, the jitter it is spread by, the ledger's rows, and the
// wait decided again when the tap's reading comes. Each behaviour has its
// negative control. Included from `turn_end_loop_tests.rs`, so its helpers and
// `run_tests.rs`'s are in scope.

use super::*;
use crate::supervise::approvals::{RULE_TAP_RESET, RULE_WALL_WAIT};
use crate::supervise::policy::turn_end::decide_turn_end;
use crate::supervise::run::turn_end_loop::resume_jitter;
use crate::supervise::tap_clock::{TapWindow, UsageWindow};

/// What a [`TapHost`] answers: a reading, or none, for each window.
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

    /// [`Self::answering`], the session's transcript `tail`.
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

/// A window read at `read_at`, `pct` used, resetting at `resets_at`.
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

/// The five-hour wall a turn ended on (Claude Code 2.1.296's notice, the
/// countdown cancelled), its reset as `reset` words it — `None`: the notice
/// cut before its reset, as a narrow pane cuts it.
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

/// A session at [`TEST_NOW`] under `host`, the default clocks (150 s of
/// grace, a minute of jitter), its ledger at `ledger`.
fn tapped<'a>(m: &'a mut Mock, ledger: Option<PathBuf>) -> Session<'a, Mock> {
    let mut s = session(m, Some(SID.to_string()));
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    // As a loop's start opens it (`Session::watch`).
    s.ledger = crate::supervise::approvals::Ledger::open(
        ledger.as_deref(),
        Some(SID),
        &mut std::io::sink(),
    );
    s.set_approval_ledger(ledger);
    s
}

fn opts_with(host: &Arc<TapHost>) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..hosted(30)
    }
}

/// One decided point at `rows`: read, folded into the policy, decided.
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

/// Where a wait goes (unix seconds), on the session's clock.
fn until_unix(s: &Session<'_, Mock>, a: &TurnEndAction) -> i64 {
    match a {
        TurnEndAction::WaitUntil { until, .. } => s.unix_of(*until),
        other => panic!("a wait: {other:?}"),
    }
}

/// The policy's own word for what it waits on.
fn why(a: &TurnEndAction) -> &str {
    match a {
        TurnEndAction::WaitUntil { why, .. } => why,
        other => panic!("a wait: {other:?}"),
    }
}

/// THE NOTICE CUT, THE TAP'S RESET: a five-hour wall whose notice names no
/// reset is waited to the tap's — its reset plus the grace plus the session's
/// jitter, the jitter its share of THAT reset — where the policy alone waits
/// on its 30-minute back-off. The ledger's wall row carries the tap's reset
/// for the loop that follows. NEGATIVE CONTROLS, each the back-off: no host;
/// the host's tap reading nothing; a reading the tap wrote before the loop
/// first read the wall; a full SEVEN-day window at a five-hour wall; a
/// window under 100 %; and a Codex's wall, whose tap is never asked.
#[test]
fn a_cut_session_walls_wait_is_timed_by_the_taps_reset() {
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
    // The reread for the books asks nothing and keeps the decision's reset.
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

    // NEGATIVE CONTROLS: the back-off from the wall's first read.
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
    // A Codex's usage wall: its tap is never asked.
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
}

/// THE NOTICE AND THE TAP TOGETHER: within six minutes of each other, the
/// tap's reset (to the second) and no row; further apart, the LATER — the
/// tap's or the notice's — and one `tap-reset@v1` ledger row for that pair,
/// never a second one at the next read, never an alert. The tap's reading
/// here comes after the first decision (its run after the wall was read),
/// and the wall's first read is kept.
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
    // Within the slack: the tap's, no row.
    let (notice, at, share, taps, rows) = run(45);
    assert_eq!(at, notice + 45 + share(notice + 45));
    assert_eq!(taps, 0, "{rows}");
    let (notice, at, share, taps, _) = run(-limit::CLOCK_SLACK_S);
    assert_eq!(
        at,
        notice - limit::CLOCK_SLACK_S + share(notice - limit::CLOCK_SLACK_S)
    );
    assert_eq!(taps, 0, "at the slack");
    // The tap later by 20 minutes: the tap's, one row.
    let (notice, at, share, taps, rows) = run(20 * 60);
    assert_eq!(at, notice + 20 * 60 + share(notice + 20 * 60));
    assert_eq!(taps, 1, "{rows}");
    assert!(
        rows.contains(&format!("notice_unix={notice}; tap_unix={}", notice + 1200))
            && rows.contains("\"decision\":\"waiting\""),
        "{rows}"
    );
    // The notice later by 20 minutes: the notice's, one row.
    let (notice, at, share, taps, rows) = run(-20 * 60);
    assert_eq!(at, notice + share(notice));
    assert_eq!(taps, 1, "{rows}");
}

/// NO RESET AT ALL, AND THE WINDOW ROLLED OVER: the notice cut, the tap's
/// reading full but its reset already behind (read after it passed, before
/// Claude Code dropped the window), so it times nothing and the back-off
/// stands; its next run shows the window dropped (E1f: its run 1.4 s after
/// the reset), and the wait ends there, plus the grace and the jitter.
/// NEGATIVE CONTROL: a window that never read full, then dropped, ends
/// nothing.
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
    // NEGATIVE CONTROL.
    let (until, _, backoff, _) = run(97);
    assert_eq!(until, TEST_NOW + backoff);
}

/// THE TAP'S READING COMES WHILE THE LOOP WAITS: the wall's first decision
/// finds no reading since the wall was read (the tap's run 0.4 s after the
/// 429 came before it) and waits on the back-off; the tap's next run, read at
/// a step of that wait, decides the point again at once, and the journal's
/// wait is the tap's reset plus the grace and the jitter — nothing typed.
/// NEGATIVE CONTROL: a host whose tap never reads leaves the back-off.
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
    // NEGATIVE CONTROL.
    let (waits, typed, _) = run(false);
    assert_eq!(typed, 0);
    assert!(
        !waits.is_empty() && waits.iter().all(|w| *w == stamp(TEST_NOW + backoff)),
        "{waits:#?}"
    );
}

/// The five-hour wall of [`session_wall`] (`resets 11:35am`) with the vendor's
/// countdown armed under it, or — `cancelled` — Esc on that countdown: the
/// cancel the last thing said, which the reader reads as an idle screen
/// with no wall (the lab's screen on Claude Code 2.1.301).
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

/// The session's transcript at [`countdown_wall`]: the turn the five-hour
/// limit answered (the vendor's notice row, its 429's `rate_limit`), written
/// as the turn ended, in conversation `c-1` — what a Claude Code host reads,
/// and what a limit whose notice left the screen is acted on by under a host
/// (`TurnEndState`'s `limit-conversation-unread` holds one no read named).
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

/// THE NOTICE LEFT, THEN THE TAP READ (the lab's run on Claude Code 2.1.301:
/// Esc on the vendor's countdown, the tap's figures first on its 10-second
/// refresh): the wall is read armed, then cancelled, which reads as no wall,
/// and the policy waits on the reset the notice named plus the grace and the
/// jitter; the tap's reading at a step of that wait decides the point again
/// and the wait is the tap's reset plus the grace and the session's share of
/// THAT reset — the cancel kept, never the countdown's ten minutes. Its
/// books follow the reset waited to though no read shows the wall again:
/// the ledger's `wall-wait@v1` row carries the tap's reset, a reset more
/// than the slack past the notice's (the lab's 421 s) has its `tap-reset@v1`
/// row, and a loop that takes the session over (an update, a restart) —
/// which has no tap clock of its own — waits to the tap's reset, never the
/// notice's minute, before which it would type into the standing limit.
/// Both hosts read the session's transcript ([`countdown_tail`]), as a
/// Claude Code's host does. NEGATIVE CONTROL: with no reading, the notice's
/// reset stands, in the wait, the ledger and the loop that follows.
#[test]
fn a_left_walls_wait_is_retimed_by_the_tap_read_while_it_waits() {
    let five = UsageWindow::FiveHour;
    // The notice's `11:35am`, as the loop places it.
    let n = TEST_NOW + 2 * 3600 + 40 * 60;
    let journal = |path: &Path| -> Vec<String> {
        let (records, _) = journal_records(path);
        records
            .into_iter()
            .filter(|r| r.kind == "waiting")
            .map(|r| r.summary)
            .collect()
    };
    // `offset`: how far past the notice's minute the tap's reading names the
    // reset; `None`, no reading at all.
    let run = |offset: Option<i64>| {
        let tag = offset.map_or_else(|| "none".to_string(), |o| o.to_string());
        let (ldir, ledger) = ledger_at(&format!("tap-left-{tag}"));
        let tail = countdown_tail();
        let host = TapHost::reading_tail(&[(five, None)], Some(&tail));
        host.after.store(2, Ordering::SeqCst);
        if let Some(offset) = offset {
            // The reading the tap writes once the loop has read the wall.
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
            let mut s = tapped(&mut m, Some(ledger.clone()));
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
        // THE LOOP THAT TAKES OVER, on the cancelled screen, its host's tap
        // reading nothing.
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
            let mut s = tapped(&mut m, Some(ledger.clone()));
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
        // The policy's limit is reset at the tap's reset from then on, for
        // all that reads it (the vendor's word that it went on, the clock it
        // binds), not only the wait.
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
        // The books: the wall's row carries the reset waited to, the tap's,
        // apart from the notice's placement.
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
        // The loop that takes over waits to the tap's reset, typing nothing.
        assert_eq!(next_typed, 0, "{next:#?}");
        assert_eq!(
            next.last(),
            Some(&resets(n + offset)),
            "{offset}: the successor: {next:#?}"
        );
    }
    // NEGATIVE CONTROL.
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

/// A WALL MET AGAIN IS READ AFRESH BY THE TAP: the tap's clock is the wall's
/// at one answer of it, kept from the loop's first read of that answer until
/// a turn the loop saw run ends on the wall again, or the wall answers an act
/// of the loop's — where the notice's reset is placed afresh too
/// (`Session::wall_reset_at`). The cut five-hour wall the tap timed to `R`;
/// past `R`, a turn runs and meets the same words (Claude Code's own
/// continuation at the reset, another limit binding), the tap's run since
/// showing the window rolled over: no reset is known, and the point waits
/// the back-off — never the past `R`, acted on at once. So too where the
/// wall answers the loop's own continuation typed past `R`: the stale `R` is
/// no reset of that answer. NEGATIVE CONTROL: the wall read again with
/// nothing between keeps `R`.
#[test]
fn a_wall_met_again_is_read_afresh_by_the_tap() {
    let five = UsageWindow::FiveHour;
    let reset = TEST_NOW + 2 * 3600;
    let later = reset + 3600;
    let rolled = reading(3, later + 4 * 3600, later);
    // A turn that ran and met the same wall again.
    let host = TapHost::answering(&[(five, reading(100, reset, TEST_NOW))]);
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, None);
    let (r, _) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(r.reset_at.map(|at| s.unix_of(at)), Some(reset));
    // NEGATIVE CONTROL: read again, nothing between.
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
    // The wall answers the loop's own continuation.
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

/// A SPEND LIMIT IS NOT THE TAP'S TO TIME: only usage credits lift it, or a
/// reset its notice names (R-9), so a reset the tap read would change its
/// act, not only its time — under `[harness] usage_credits = false` the
/// escalation that types nothing, whichever way the tap's first run fell
/// against the loop's first read. Its tap is never asked, and the act is the
/// same with the five-hour window full. NEGATIVE CONTROL: the five-hour
/// wall, the same reading, is timed by it.
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
    // NEGATIVE CONTROL.
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, None);
    let (r, _) = decide(&mut s, session_wall(None), &opts);
    assert_eq!(r.reset_at.map(|at| s.unix_of(at)), Some(reset));
}

/// A relaunched Claude Code's screen that draws no limit: its conversation
/// redrawn under the vendor's own paragraph ([`weekly_standing`]'s transcript
/// stands at the limit).
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

/// A host whose transcript stands at a weekly limit its row resets at
/// `until`, the tap answering `answer` for the seven-day window.
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

/// A LIMIT ONLY THE TRANSCRIPT SHOWS, TIMED BY THE TAP: its notice's reset
/// is its row's (`quotaLimits.resetsAt`), and the tap's reading is set
/// against THAT, as against any notice's — the later of the two further
/// apart than the slack, with its `tap-reset@v1` row — so the wait, the
/// session's share of the reset and the wall's ledger row name one reset.
/// NEGATIVE CONTROL: the tap reading nothing, all three name the row's.
#[test]
fn a_transcript_limit_is_timed_by_the_tap_against_its_rows_reset() {
    // The row's reset, away from where its words would be placed.
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
    // NEGATIVE CONTROL.
    let (at, jitter, waited, rows) = run(None);
    assert_eq!(at, Some(until));
    assert_eq!(jitter, share(until));
    assert_eq!(waited, until + grace + secs(share(until)));
    assert!(wall_row(&rows, ""), "{rows}");
    assert!(!rows.contains(RULE_TAP_RESET), "{rows}");
}

/// The reset `r` names, unix seconds on `s`'s clock.
fn reset_of(s: &Session<'_, Mock>, r: &TurnEndReading) -> Option<i64> {
    r.reset_at.map(|at| s.unix_of(at))
}

/// THE FIRST LOOP AT A CUT five-hour wall, its tap answering `read`, its
/// books in a ledger of their own (`tag`): the directory and the ledger.
fn cut_wall_written(tag: &str, read: Option<TapWindow>) -> (PathBuf, PathBuf) {
    let (dir, ledger) = ledger_at(tag);
    let host = TapHost::answering(&[(UsageWindow::FiveHour, read)]);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = tapped(&mut m, Some(ledger.clone()));
    let _ = decide(&mut s, session_wall(None), &opts_with(&host));
    (dir, ledger)
}

/// THE LOOP THAT TAKES THE SESSION OVER from the books at `ledger`
/// (`Session::seed_wall_memory`, as a loop's start seeds it).
fn successor<'a>(m: &'a mut Mock, ledger: &Path) -> Session<'a, Mock> {
    let mut s = tapped(m, Some(ledger.to_path_buf()));
    s.seed_wall_memory(crate::supervise::approvals::open_wall(ledger, Some(SID)));
    s
}

/// A LOOP THAT TAKES THE SESSION OVER AT A CUT WALL THE TAP TIMED READS IT
/// MET AGAIN AFRESH: the wall's row keeps the notice's placement (none, the
/// notice cut) apart from the tap's reset `R` (`reset_unix=-`, `tap_unix=R`),
/// so the successor — no tap clock of its own — waits to `R`, and past it,
/// where the wall answers the successor's own continuation, no reset is
/// known: the back-off, as in one loop
/// ([`a_wall_met_again_is_read_afresh_by_the_tap`]), never the past `R`
/// taken for the notice's and typed into again. NEGATIVE CONTROL: a first
/// loop whose tap read nothing leaves the successor the back-off at once.
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
    // NEGATIVE CONTROL.
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

/// THE SUCCESSOR'S OWN TAP AT A CUT WALL IS TAKEN ALONE: the notice named
/// no reset, so the successor's own reading of one — 20 minutes past the
/// reset the first loop's tap carried — is the reset waited to, set against
/// nothing: no `tap-reset@v1` row names a notice's reset no notice named.
/// NEGATIVE CONTROL: its tap reading nothing, the carried reset stands.
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
    // NEGATIVE CONTROL.
    let (got, _) = run(None);
    assert_eq!(got, Some(reset));
}

/// THE SUCCESSOR SETS ITS OWN TAP AGAINST THE NOTICE'S RESET, NOT THE ONE IT
/// CARRIES: the notice's `11:35am` placed at `n`, the first loop's tap inside
/// the slack (`n - 240`, waited to); the successor's own reading 500 s from
/// `n` — further than the slack from the notice's reset, though inside it
/// from the carried `n - 240` — waits to the later, the notice's, with its
/// `tap-reset@v1` row. NEGATIVE CONTROL: no tap before, the same.
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
    // NEGATIVE CONTROL.
    let (n, got, _) = run(None);
    assert_eq!(got, Some(n));
}

/// A SUCCESSOR AT A LIMIT ONLY THE TRANSCRIPT SHOWS WAITS TO THE TAP'S
/// RESET OVER THE ROW'S: the row resets at `until`, the first loop's tap 45
/// minutes later (the later, waited to); the relaunched process's tap reads
/// nothing, and the successor waits to the tap's reset, never the row's
/// earlier one, before which it would type into the standing limit.
/// NEGATIVE CONTROL: a first loop whose tap read nothing, the row's.
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
    // NEGATIVE CONTROL.
    assert_eq!(run(None), Some(until), "the row's");
}
