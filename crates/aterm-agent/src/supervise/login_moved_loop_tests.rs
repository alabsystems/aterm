// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A login moved off the account a standing limit was read under
//! ([`TurnEndReading::login_moved`]): the limit's reset binds no longer, and
//! one resume goes at once.

use super::*;
use crate::harness::profile::AccountKey;
use crate::supervise::approvals::{RULE_LOGIN_MOVED, RULE_WALL_WAIT};
use crate::supervise::policy::turn_end::{RULE_LIMIT_RESUME, decide_turn_end};
use crate::supervise::run::turn_end_loop::LOGIN_LOOK_EVERY;

/// A host whose session's profile names the account the test sets
/// ([`IdleHost::login_account`]), every ask counted. It takes no step and
/// owns no turn end.
#[derive(Debug, Default)]
struct LoginHost {
    account: std::sync::Mutex<Option<AccountKey>>,
    asks: std::sync::atomic::AtomicUsize,
    /// The asks after which [`Self::later`] replaces the account (a `/login`
    /// elsewhere while the loop waits).
    after: std::sync::atomic::AtomicUsize,
    later: std::sync::Mutex<Option<AccountKey>>,
}

impl LoginHost {
    fn on(account: Option<AccountKey>) -> Arc<Self> {
        let h = Arc::new(Self::default());
        h.set(account);
        h
    }

    fn set(&self, account: Option<AccountKey>) {
        *self.account.lock().unwrap() = account;
    }

    fn asks(&self) -> usize {
        self.asks.load(Ordering::SeqCst)
    }
}

impl IdleHost for LoginHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn login_account(&self) -> Option<AccountKey> {
        let n = self.asks.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.after.load(Ordering::SeqCst)
            && let Some(later) = self.later.lock().unwrap().take()
        {
            *self.account.lock().unwrap() = Some(later);
        }
        *self.account.lock().unwrap()
    }
}

const SID: &str = "@s-1";

fn key(id: &str) -> AccountKey {
    AccountKey::claude(id).expect("a key")
}

/// Claude Code at its weekly limit, the notice's reset six days out.
fn weekly_wall() -> Vec<String> {
    let mut r = rows(&[
        "❯ carry on with the task",
        "  ⎿  You've hit your weekly limit · resets Sep 23 at 8am (America/Los_Angeles)",
        "     /upgrade to increase your usage limit.",
        "",
        "✻ Cooked for 0s · done 8:54 AM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// Claude Code at a limit under its own countdown to the reset
/// (`/rate-limit-options`' wait), as measured on 2.1.300: it goes on by itself.
fn countdown_wall() -> Vec<String> {
    include_str!(
        "../../../aterm-phase/src/fixtures/claude-2.1.300-wall-sonnet-bucket-auto-continue-armed.txt"
    )
    .lines()
    .filter(|line| !line.starts_with("# "))
    .map(str::to_owned)
    .collect()
}

fn logged<'a>(m: &'a mut Mock, ledger: Option<PathBuf>) -> Session<'a, Mock> {
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

fn opts_with(host: &Arc<LoginHost>) -> SuperviseOpts {
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
    decide_after(s, rows, None, opts)
}

/// [`decide`] at a point a turn of `worked` led to.
fn decide_after(
    s: &mut Session<'_, Mock>,
    rows: Vec<String>,
    worked: Option<Duration>,
    opts: &SuperviseOpts,
) -> (TurnEndReading, TurnEndAction) {
    let screen = read_screen(rows);
    let r = s.turn_end_reading(&screen, worked, opts);
    let now = Instant::now();
    s.observe_point(&r, now, screen.seq);
    let a = decide_turn_end(&s.turn_end, &r, &opts.policy, now);
    (r, a)
}

fn waits(a: &TurnEndAction) -> bool {
    matches!(a, TurnEndAction::WaitUntil { .. })
}

/// A login moved off the limit's account resumes the limit at once, once:
/// the read that sees the move drops the notice's reset and owes one
/// resume, the ledger says from which account to which, and the resume's
/// answer (the limit again, now the new account's) is waited on as any
/// limit is. A login that stays, or one the host cannot read, waits to the
/// notice's reset.
#[test]
fn a_login_moved_off_the_limits_account_resumes_it_at_once_and_once() {
    let (dir, ledger) = ledger_at("login-moved");
    let host = LoginHost::on(Some(key("u-first")));
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = logged(&mut m, Some(ledger.clone()));
    let (r, a) = decide(&mut s, weekly_wall(), &opts);
    assert_eq!(r.wall, Some(aterm_phase::WallKind::UsageWeekly), "{r:#?}");
    assert!(!r.login_moved);
    assert!(waits(&a), "the notice's reset first: {a:?}");
    let (r, a) = decide(&mut s, weekly_wall(), &opts);
    assert!(!r.login_moved && waits(&a), "the same login waits: {a:?}");

    host.set(Some(key("u-second")));
    let (r, a) = decide(&mut s, weekly_wall(), &opts);
    assert!(r.login_moved, "{r:#?}");
    assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{a:?}");
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let moved: Vec<&str> = rows
        .lines()
        .filter(|l| l.contains(RULE_LOGIN_MOVED))
        .collect();
    assert_eq!(moved.len(), 1, "{rows}");
    assert!(
        moved[0].contains(&format!("from={}; to={}", key("u-first"), key("u-second"))),
        "{rows}"
    );
    assert!(
        rows.lines()
            .any(|l| l.contains(RULE_WALL_WAIT)
                && l.contains(&format!("; login={};", key("u-first")))),
        "the limit's row names the account it was read under: {rows}"
    );
    s.turn_end.acted(&a, &r, Instant::now());

    let (r, a) = decide(&mut s, weekly_wall(), &opts);
    assert!(
        !r.login_moved,
        "the resume's answer is the new account's limit"
    );
    assert!(waits(&a), "one resume a move: {a:?}");
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    assert_eq!(
        rows.lines()
            .filter(|l| l.contains(RULE_LOGIN_MOVED))
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);

    for (case, then) in [("stays", Some(key("u-first"))), ("unread", None)] {
        let host = LoginHost::on(Some(key("u-first")));
        let opts = opts_with(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = logged(&mut m, None);
        let _ = decide(&mut s, weekly_wall(), &opts);
        host.set(then);
        let (r, a) = decide(&mut s, weekly_wall(), &opts);
        assert!(!r.login_moved, "{case}");
        assert!(waits(&a), "{case}: {a:?}");
    }
}

/// Claude Code's own countdown to the reset is an old login's too: a moved
/// login resumes the limit at once, where the countdown alone resumes nothing.
#[test]
fn a_moved_login_resumes_a_limit_claude_code_counts_down_itself() {
    for moves in [false, true] {
        let host = LoginHost::on(Some(key("u-first")));
        let opts = opts_with(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = logged(&mut m, None);
        let (r, a) = decide(&mut s, countdown_wall(), &opts);
        assert!(r.resumes_by_itself, "{r:#?}");
        assert_ne!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{a:?}");
        if moves {
            host.set(Some(key("u-second")));
        }
        let (r, a) = decide(&mut s, countdown_wall(), &opts);
        assert_eq!(r.login_moved, moves);
        assert_eq!(a.rule_id() == Some(RULE_LIMIT_RESUME), moves, "{a:?}");
    }
}

/// While a wait on the limit stands, the profile is read every
/// [`LOGIN_LOOK_EVERY`] and a moved login wakes the loop; between reads it is
/// not asked, and with no wait standing or no account kept it is not read.
#[test]
fn the_wait_wakes_on_a_moved_login_reading_the_profile_at_a_pace() {
    let host = LoginHost::on(Some(key("u-first")));
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = logged(&mut m, None);
    assert!(!s.login_edge(&opts), "no wait stands");
    let (_, a) = decide(&mut s, weekly_wall(), &opts);
    let TurnEndAction::WaitUntil { until, why, wait } = a else {
        panic!("a wait: {a:?}");
    };
    s.turn_end_due = Some(Due {
        at: until,
        unix_ms: s.unix_of(until) * 1000,
        why,
        wait,
    });
    let asked = host.asks();
    assert!(!s.login_edge(&opts));
    assert_eq!(host.asks(), asked + 1);
    host.set(Some(key("u-second")));
    assert!(
        !s.login_edge(&opts),
        "read again only after {LOGIN_LOOK_EVERY:?}"
    );
    assert_eq!(host.asks(), asked + 1);
    s.login_looked_at = None;
    assert!(s.login_edge(&opts), "the moved login wakes the wait");
    s.wall_login = None;
    s.login_looked_at = None;
    assert!(!s.login_edge(&opts), "no account kept");
}

/// The whole loop: waiting at the limit, a `/login` elsewhere (the host's
/// account changes after its first asks) is read during the wait and the
/// limit resumed at once with one typed line; with no move, nothing is typed.
#[test]
fn a_login_moved_during_the_wait_resumes_the_limit_from_the_loop() {
    let run = |moves: bool| {
        let host = LoginHost::on(Some(key("u-first")));
        host.after.store(1, Ordering::SeqCst);
        if moves {
            *host.later.lock().unwrap() = Some(key("u-second"));
        }
        let (dir, ledger) = ledger_at(&format!("login-moved-loop-{moves}"));
        let mut m = Mock::new(vec![busy_screen(), weekly_wall()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.vanish_after = Some(40);
        let opts = SuperviseOpts {
            ledger: Some(ledger.clone()),
            ..opts_with(&host)
        };
        let mut out: Vec<u8> = Vec::new();
        {
            let mut s = logged(&mut m, Some(ledger.clone()));
            s.login_looked_at = None;
            let _ = s.watch(&opts, &mut out);
        }
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let typed = rows
            .lines()
            .filter(|l| l.contains("\"decision\":\"typed\""))
            .count();
        (typed, rows, host.asks())
    };
    let (typed, rows, asks) = run(true);
    assert!(asks >= 2, "read at the decision and in the wait: {asks}");
    assert_eq!(typed, 1, "{rows}");
    assert!(rows.lines().any(|l| l.contains(RULE_LOGIN_MOVED)), "{rows}");
    assert!(
        rows.lines()
            .any(|l| l.contains("\"decision\":\"typed\"") && l.contains(RULE_LIMIT_RESUME)),
        "{rows}"
    );
    let (typed, rows, _) = run(false);
    assert_eq!(typed, 0, "{rows}");
    assert!(!rows.contains(RULE_LOGIN_MOVED), "{rows}");
}

/// A loop that takes the limit over from the ledger keeps the account its
/// row names, and a login moved off it since resumes the limit; an older
/// row naming none takes the first read's account as the limit's.
#[test]
fn a_successor_keeps_the_limits_account_from_its_row() {
    let (dir, ledger) = ledger_at("login-moved-seed");
    let host = LoginHost::on(Some(key("u-first")));
    let opts = opts_with(&host);
    {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = logged(&mut m, Some(ledger.clone()));
        let (_, a) = decide(&mut s, weekly_wall(), &opts);
        assert!(waits(&a), "{a:?}");
    }
    let open = crate::supervise::approvals::open_wall(&ledger, Some(SID)).expect("an open wall");
    assert_eq!(open.login, Some(key("u-first")));
    let _ = std::fs::remove_dir_all(&dir);
    for (row, moves) in [(Some(key("u-first")), true), (None, false)] {
        host.set(Some(key("u-second")));
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = logged(&mut m, None);
        s.seed_wall_memory(Some(crate::supervise::approvals::OpenWall {
            login: row,
            ..open.clone()
        }));
        let (r, a) = decide(&mut s, weekly_wall(), &opts);
        assert_eq!(r.login_moved, moves, "{row:?}");
        assert_eq!(
            a.rule_id() == Some(RULE_LIMIT_RESUME),
            moves,
            "{row:?}: {a:?}"
        );
    }
}

/// A replay of a tape whose build had no `host.login_account` never asks
/// it: the decision and the wait read no account, and nothing moves.
#[test]
fn a_replay_without_the_door_reads_no_account() {
    use crate::supervise::fx::Fx;
    use crate::supervise::tape::header::VERBATIM;
    use crate::supervise::tape::replay::ReplayCore;
    use crate::supervise::tape::testing::TAPE_ID;
    let core = ReplayCore::start(Vec::new(), VERBATIM, VERBATIM, TAPE_ID, None);
    core.recorded_doors(vec!["host.wants".to_owned()]);
    let host = LoginHost::on(Some(key("u-first")));
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = logged(&mut m, None);
    s.fx = Fx::replay(Arc::clone(&core));
    s.seed_login(Some(key("u-second")));
    assert!(!s.login_moved_at(
        opts.idle_host.as_ref(),
        Some(aterm_phase::WallKind::UsageWeekly),
        Program::Claude,
        super::super::wall_confirm::Check::Decide,
    ));
    s.turn_end_due = Some(Due {
        at: Instant::now(),
        unix_ms: TEST_NOW * 1000,
        why: "a wait".to_owned(),
        wait: WaitKind::Other,
    });
    assert!(!s.login_edge(&opts));
    assert_eq!(host.asks(), 0);
    assert!(core.diverged().is_none());
}

/// A limit the policy holds with nothing decided (the same reply again after
/// its tries) is read for a moved login too: the edge stands with no wait
/// due, and wakes once for each account it reads, the policy then resuming
/// the limit. With nothing held and no wait due it reads nothing.
#[test]
fn a_held_limit_wakes_on_a_moved_login_once_for_each_account() {
    let host = LoginHost::on(Some(key("u-first")));
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = logged(&mut m, None);
    let _ = decide(&mut s, weekly_wall(), &opts);
    s.turn_end_due = None;
    s.nothing_decided = true;
    s.login_looked_at = None;
    assert!(!s.login_edge(&opts), "the same login");
    host.set(Some(key("u-second")));
    s.login_looked_at = None;
    assert!(s.login_edge(&opts), "a held limit wakes on the move");
    s.login_looked_at = None;
    assert!(!s.login_edge(&opts), "once for the account it read");
    let (r, a) = decide(&mut s, weekly_wall(), &opts);
    assert!(r.login_moved, "{r:#?}");
    assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{a:?}");
    s.nothing_decided = false;
    host.set(Some(key("u-third")));
    s.login_looked_at = None;
    let asked = host.asks();
    assert!(!s.login_edge(&opts), "nothing held, no wait due");
    assert_eq!(host.asks(), asked, "not read");
}

/// A limit the policy still waits on with its notice gone from the screen is
/// the old login's all the same: a moved login read then is delivered and
/// taken up, its resume owed (typed once the limit's conversation is read:
/// `a_login_moved_off_a_held_limit_the_screen_left_resumes_it`), and the
/// edge wakes no more for it.
#[test]
fn a_moved_login_off_a_limit_the_screen_left_resumes_it_once() {
    let (dir, ledger) = ledger_at("login-moved-off-screen");
    let host = LoginHost::on(Some(key("u-first")));
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = logged(&mut m, Some(ledger.clone()));
    let (_, a) = decide(&mut s, weekly_wall(), &opts);
    assert!(waits(&a), "{a:?}");
    let (r, a) = decide(&mut s, notice_gone(), &opts);
    assert!(
        r.wall.is_none() && s.turn_end.waits_on_named_reset(),
        "{r:#?}"
    );
    assert!(waits(&a), "still the notice's reset: {a:?}");
    host.set(Some(key("u-second")));
    let (r, _) = decide(&mut s, notice_gone(), &opts);
    assert!(r.login_moved, "{r:#?}");
    assert!(s.turn_end.login_resume_owed());
    assert_eq!(s.wall_login_account(), Some(key("u-first")), "owed");
    let _ = decide(&mut s, notice_gone(), &opts);
    assert!(s.turn_end.login_resume_owed(), "still held");
    assert_eq!(
        s.wall_login_account(),
        Some(key("u-first")),
        "kept past the next point off the screen"
    );
    let open = crate::supervise::approvals::open_wall(&ledger, Some(SID));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        open.and_then(|o| o.login),
        Some(key("u-first")),
        "the limit's row stays open, naming the account before the move"
    );
    s.turn_end_due = Some(Due {
        at: Instant::now(),
        unix_ms: TEST_NOW * 1000,
        why: "a wait".to_owned(),
        wait: WaitKind::Other,
    });
    s.login_looked_at = None;
    assert!(!s.login_edge(&opts), "taken: the edge wakes no more");
}

/// Claude Code idle after its weekly limit, the notice gone from the screen
/// (a local command's answer below it, no turn served).
fn notice_gone() -> Vec<String> {
    let mut r = rows(&["❯ /status", "  ⎿  Status dialog dismissed", ""]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// A move the policy took whose resume is still untyped leaves the limit's
/// row naming the account before the move, so a successor loop owes the
/// resume too; once the resume is acted on the row names the new account.
#[test]
fn a_resume_owed_but_untyped_is_owed_by_a_successor_too() {
    let (dir, ledger) = ledger_at("login-moved-owed");
    let host = LoginHost::on(Some(key("u-first")));
    let opts = opts_with(&host);
    {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = logged(&mut m, Some(ledger.clone()));
        let _ = decide(&mut s, weekly_wall(), &opts);
        host.set(Some(key("u-second")));
        let (r, a) = decide(&mut s, weekly_wall(), &opts);
        assert!(r.login_moved, "{r:#?}");
        assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{a:?}");
        let (_, a) = decide(&mut s, weekly_wall(), &opts);
        assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "untyped: {a:?}");
        assert_eq!(s.wall_login_account(), Some(key("u-first")));
    }
    let open = crate::supervise::approvals::open_wall(&ledger, Some(SID)).expect("an open wall");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        open.login,
        Some(key("u-first")),
        "the row names the account before the move"
    );
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = logged(&mut m, None);
    s.seed_wall_memory(Some(open));
    let (r, a) = decide(&mut s, weekly_wall(), &opts);
    assert!(r.login_moved, "{r:#?}");
    assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{a:?}");
    s.turn_end.acted(&a, &r, Instant::now());
    let _ = decide(&mut s, weekly_wall(), &opts);
    assert_eq!(s.wall_login_account(), Some(key("u-second")));
}

/// A served turn that ends the limit ends its login too: the resume typed at
/// the limit's reset served, the point after it, held or waited on, reads no
/// profile however long it stands.
#[test]
fn a_served_turn_that_ends_the_limit_reads_no_profile_after() {
    let host = LoginHost::on(Some(key("u-first")));
    let opts = opts_with(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = logged(&mut m, None);
    let (r, _) = decide(&mut s, weekly_wall(), &opts);
    assert!(s.turn_end.waits_on_named_reset());
    let resume = TurnEndAction::Type {
        text: "keep going".to_string(),
        rule_id: RULE_LIMIT_RESUME,
    };
    s.turn_end.acted(&resume, &r, Instant::now());
    assert!(
        s.turn_end.waits_on_named_reset(),
        "the reset stands until a turn is served"
    );
    let _ = decide_after(&mut s, idle_screen(), Some(Duration::from_secs(90)), &opts);
    assert_eq!(s.wall_login, None, "the limit's login ended with it");
    s.turn_end_due = None;
    s.nothing_decided = true;
    s.login_looked_at = None;
    host.set(Some(key("u-third")));
    let asked = host.asks();
    assert!(!s.login_edge(&opts));
    assert_eq!(host.asks(), asked, "not read");
}
