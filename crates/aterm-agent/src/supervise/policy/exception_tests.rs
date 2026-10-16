// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The exceptions: a person is asked for only what only a person can do, with
//! the one action, and a wait is never one.

use super::*;
use crate::supervise::phase::Phase;
use crate::supervise::policy::turn_end::{
    Composer, TurnEndAction, TurnEndReading, decide_turn_end,
};
use aterm_phase::prompt::fixtures::{
    COMPACT_ERROR_500_MEASURED, COMPACT_FAILED_1_MEASURED, COMPACT_FAILED_2_MEASURED,
    COMPACT_FAILED_3_MEASURED, CONTEXT_WALL_MEASURED, END_529, screen,
};

const MIN: Duration = Duration::from_secs(60);

fn t0() -> Instant {
    Instant::now() + Duration::from_secs(10 * 3600)
}

fn cfg() -> SupervisorConfig {
    SupervisorConfig::default()
}

fn walled(kind: WallKind, message: &str) -> TurnEndReading {
    TurnEndReading {
        phase: Phase::Idle,
        authoritative: true,
        survey: false,
        wall: Some(kind),
        wall_message: message.to_string(),
        reset_at: None,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some("Suites are running.".to_string()),
        worked: Some(3 * MIN),
        rules: None,
        pending_input: false,
        interrupted: false,
        restartable: true,
        resume: None,
        upgrading: false,
        taskless: false,
        person: None,
        reach: Default::default(),
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        reset_clock: None,
        program: aterm_phase::Program::Claude,
        goal: None,
        model_field: None,
        limits: Default::default(),
        sandbox_fell: None,
        thread_model: None,
        upgrade_goal: false,
        account: None,
        autopilot: crate::supervise::Autopilot::Cruise,
        login_label: None,
        login_superseded: false,
        login_notice: None,
    }
}

fn seen(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) {
    st.observe(r, now);
}

/// A lost login is an exception only once its `/login` has been typed (or the
/// owner switched the retries off): before, the supervisor is still acting.
#[test]
fn a_lost_login_after_its_login_is_an_exception_with_its_action() {
    let t = t0();
    let r = walled(WallKind::Auth, "Login expired · Please run /login");
    let mut st = TurnEndState::default();
    seen(&mut st, &r, t);
    assert_eq!(
        exception_for(&st, &r, &cfg(), t),
        None,
        "the /login is still to be typed"
    );
    let a = crate::supervise::policy::turn_end::decide_turn_end(&st, &r, &cfg(), t);
    st.acted(&a, &r, t);
    let e = exception_for(&st, &r, &cfg(), t + MIN).expect("an exception");
    assert_eq!((e.kind, e.when), (ExceptionKind::Login, Urgency::Now));
    assert!(e.action.contains("browser"), "{e:?}");
    let off = SupervisorConfig {
        retry_api_errors: false,
        ..cfg()
    };
    let st = TurnEndState::default();
    assert!(exception_for(&st, &r, &off, t).is_some_and(|e| e.kind == ExceptionKind::Login));
}

/// A spend or credit limit no reset ends says so once, with the action the
/// vendor's own words call for: the admin's, not "add credits", for a seat or
/// an organization that cannot buy them.
#[test]
fn a_spend_wall_with_no_reset_says_so_once_and_the_action_is_the_strings() {
    let t = t0();
    let st = TurnEndState::default();
    let spend = |m: &str| exception_for(&st, &walled(WallKind::Spend, m), &cfg(), t).expect(m);
    let credits = spend("You're out of extra usage · add funds to continue");
    assert_eq!(credits.kind, ExceptionKind::Spend);
    assert!(
        credits.action.to_lowercase().contains("add usage credits"),
        "{credits:?}"
    );
    for (message, wants) in [
        ("Your seat type doesn't include usage credits", "admin"),
        (
            "Your usage allocation has been disabled by your admin",
            "admin",
        ),
        ("Your group's usage limit is set to $0", "admin"),
        ("This service is disabled for your org", "admin"),
        (
            "Your organization has disabled Claude subscription access · Sign in again",
            "admin",
        ),
        ("Extra usage is required for long context", "extra usage"),
    ] {
        let e = spend(message);
        assert!(e.action.to_lowercase().contains(wants), "{message}: {e:?}");
        assert!(
            !e.action.to_lowercase().contains("add usage credits"),
            "{message}: {e:?}"
        );
        assert_ne!(e.why, credits.why, "{message}");
    }
    // Negative control: with a reset named, it is a wait.
    let mut r = walled(
        WallKind::Spend,
        "Monthly spend limit reached · resets Oct 1",
    );
    r.reset_at = Some(t + 90 * MIN);
    assert_eq!(exception_for(&st, &r, &cfg(), t), None);
}

/// A Codex screen whose last turn ended on the `■` row `wall`, as the policy
/// reads it (the Codex reader: its wall kind, its program).
fn codex_ended_on(wall: &str) -> TurnEndReading {
    let rows: Vec<String> = [
        "› go on",
        "",
        &format!("■ {wall}"),
        "",
        "› Ask Codex to do anything",
        "",
        "  gpt · /x",
    ]
    .iter()
    .map(|r| r.to_string())
    .collect();
    let reading = aterm_phase::read(Some("codex"), &rows, None);
    assert_eq!(reading.program, aterm_phase::Program::Codex);
    TurnEndReading::of(&reading, &rows, false, Some(3 * MIN), None, None)
}

/// The state after a wall in `r` answered every continuation typed after
/// its reset (`r.reset_at`, which is `t`), one try past
/// [`FUTILE_AFTER_TRIES`].
fn tried_past_the_reset(
    r: &mut TurnEndReading,
    cfg: &SupervisorConfig,
    t: Instant,
) -> TurnEndState {
    let mut st = TurnEndState::default();
    let mut now = t + MIN;
    let mut tries = 0;
    while tries < FUTILE_AFTER_TRIES + 1 {
        r.worked = Some(if tries == 0 {
            3 * MIN
        } else {
            Duration::from_secs(4)
        });
        st.observe(r, now);
        match decide_turn_end(&st, r, cfg, now) {
            a @ TurnEndAction::Type { .. } => {
                st.acted(&a, r, now);
                tries += 1;
                now += Duration::from_secs(5);
            }
            TurnEndAction::WaitUntil { until, .. } => now = until,
            other => panic!("{other:?}"),
        }
    }
    st
}

/// A CODEX SESSION'S WALLS GET CODEX'S WORDS, never Claude Code's: its spend
/// walls (its reader's spend-cap row, and the two the 0.162.1 lab measured,
/// `docs/measured/real-claude-lab-2026-10-15.md` M2) send the person to
/// Codex's own notice, never to claude.ai or "usage credits"; a usage limit
/// futile past its reset to Codex's `/status`; a lost login to `codex login`,
/// never the page `/login` opened (nothing is typed into Codex). Any other
/// program's words name no vendor. NEGATIVE CONTROL: the same walls in a
/// Claude Code session keep Claude Code's words.
#[test]
fn a_codex_sessions_walls_get_codexs_words() {
    use aterm_phase::Program;
    let t = t0();
    let claudes = ["claude.ai", "usage credits", "/login", "Claude"];
    let names_none = |e: &Exception, words: &[&str]| {
        words
            .iter()
            .all(|w| !e.why.contains(w) && !e.action.contains(w))
    };
    for notice in [
        "You hit your spend cap set in your workspace. Increase your spend cap to continue.",
        "Your workspace is out of credits. Ask your workspace owner to refill in order to continue.",
        "You hit your spend cap set by the owner of your workspace. Ask an owner to increase your \
         spend cap to continue.",
    ] {
        let r = codex_ended_on(notice);
        assert_eq!(r.wall, Some(WallKind::Spend), "{notice}");
        let st = TurnEndState::default();
        let e = exception_for(&st, &r, &cfg(), t).expect(notice);
        assert_eq!((e.kind, e.when), (ExceptionKind::Spend, Urgency::Later));
        assert!(e.action.contains("Codex's notice"), "{e:?}");
        assert!(names_none(&e, &claudes), "{notice}: {e:?}");
        let claude = TurnEndReading {
            program: Program::Claude,
            ..r.clone()
        };
        let control = exception_for(&st, &claude, &cfg(), t).expect(notice);
        assert_eq!(control.action, "Add usage credits in claude.ai settings.");
    }

    let mut usage = walled(
        WallKind::UsageSession,
        "You've hit your usage limit. Try again at 12:56 PM.",
    );
    usage.reset_at = Some(t);
    let mut c = cfg();
    c.model_fallback = None;
    for (program, wants) in [
        (Program::Codex, "Run /status in this tab"),
        (Program::Generic, "the program's account"),
        (Program::Claude, "claude.ai/settings/usage"),
    ] {
        let mut r = TurnEndReading {
            program,
            ..usage.clone()
        };
        let st = tried_past_the_reset(&mut r, &c, t);
        let e = exception_for(&st, &r, &c, t + 61 * MIN).expect("futile");
        assert_eq!(e.kind, ExceptionKind::Futile, "{program:?}");
        assert!(e.action.contains(wants), "{program:?}: {e:?}");
    }

    let off = SupervisorConfig {
        retry_api_errors: false,
        ..cfg()
    };
    let lost = codex_ended_on(
        "Your access token could not be refreshed. Please log out and sign in again.",
    );
    assert_eq!(lost.wall, Some(WallKind::Auth));
    let e = exception_for(&TurnEndState::default(), &lost, &off, t).expect("login");
    assert_eq!(
        (e.kind, e.action.as_str(), e.clears, e.escalated),
        (
            ExceptionKind::Login,
            "Sign in to Codex again with codex login.",
            "the agent working",
            true
        )
    );
    assert!(names_none(&e, &claudes), "{e:?}");
    let claude = TurnEndReading {
        program: Program::Claude,
        ..lost.clone()
    };
    let control = exception_for(&TurnEndState::default(), &claude, &off, t).expect("login");
    assert!(control.action.contains("/login"), "{control:?}");

    // Any other program: no vendor's words at all.
    let vendors = [
        "claude.ai",
        "usage credits",
        "/login",
        "Claude",
        "Codex",
        "codex",
    ];
    let generic = |kind, message: &str| TurnEndReading {
        program: Program::Generic,
        ..walled(kind, message)
    };
    let st = TurnEndState::default();
    for e in [
        exception_for(&st, &generic(WallKind::Spend, "out of credits"), &cfg(), t),
        exception_for(&st, &generic(WallKind::Auth, "Not logged in"), &off, t),
    ] {
        let e = e.expect("an exception");
        assert!(names_none(&e, &vendors), "{e:?}");
    }
}

/// `Goal cleared after an unrecoverable error (<label>)` names its cause.
#[test]
fn a_cleared_goal_names_its_cause() {
    let t = t0();
    let st = TurnEndState::default();
    let r = walled(
        WallKind::Auth,
        "Login expired · Please run /login — Goal cleared after an unrecoverable error (authentication failed): \"x\". Run /goal again to continue.",
    );
    let e = exception_for(&st, &r, &cfg(), t).expect("an exception");
    assert_eq!(e.kind, ExceptionKind::GoalCleared);
    assert!(e.why.contains("authentication failed"), "{e:?}");
    assert!(e.action.contains("/goal"), "{e:?}");
}

/// A bucket no reset ends is an exception only when the tab has not switched
/// off it an hour on, and says why. Negative controls: switched, under an hour,
/// and a bucket whose reset is named.
#[test]
fn a_bucket_no_reset_ends_is_an_exception_only_while_the_tab_cannot_switch() {
    let t = t0();
    let fable = "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.";
    let kind = WallKind::ModelBucket { consent: false };
    let r = walled(kind, fable);
    let mut st = TurnEndState::default();
    seen(&mut st, &r, t);
    // Less than an hour: the supervisor is still asking the host.
    assert_eq!(exception_for(&st, &r, &cfg(), t + 59 * MIN), None);
    // An hour on, the fallback still deferred: the jobs hold it.
    let held = exception_for(&st, &r, &cfg(), t + 61 * MIN).expect("held");
    assert_eq!(
        (held.kind, held.when),
        (ExceptionKind::Bucket, Urgency::Later)
    );
    assert!(
        held.why.contains("running jobs") && held.why.contains("opus"),
        "{held:?}"
    );
    assert!(
        held.action.contains("/model") && held.action.contains("opus"),
        "{held:?}"
    );
    // No other model to fall back on: the other words, the other action.
    let none = SupervisorConfig {
        model_fallback: None,
        ..cfg()
    };
    let e = exception_for(&st, &r, &none, t + 61 * MIN).expect("no fallback");
    assert!(e.why.contains("no other model"), "{e:?}");
    assert!(e.action.contains("another model"), "{e:?}");
    // The bucket is the fallback's model, or the host cannot relaunch: the same.
    let mut opus = walled(kind, "You've reached your Opus limit · run /usage-credits");
    opus.restartable = false;
    let mut st2 = TurnEndState::default();
    seen(&mut st2, &opus, t);
    assert!(
        exception_for(&st2, &opus, &cfg(), t + 61 * MIN)
            .is_some_and(|e| e.why.contains("no other model"))
    );
    // Switched: the tab works on the fallback, nothing to ask.
    let mut switched = TurnEndState::default();
    seen(&mut switched, &r, t);
    switched.restarted(
        &crate::supervise::policy::turn_end::Restart::Model {
            to: "opus".to_string(),
        },
        true,
        &r,
        t,
    );
    assert_eq!(exception_for(&switched, &r, &cfg(), t + 61 * MIN), None);
    // A reset named is a wait.
    let mut named = walled(kind, "You've reached your Opus limit · resets 4pm");
    named.reset_at = Some(t + 300 * MIN);
    let mut st3 = TurnEndState::default();
    seen(&mut st3, &named, t);
    assert_eq!(exception_for(&st3, &named, &cfg(), t + 120 * MIN), None);
}

/// A limit that keeps answering the continuation an hour past its reset is
/// futile, a bucket with a named reset too. Two tries, or under an hour, are not.
#[test]
fn a_wall_that_answers_three_tries_an_hour_past_its_reset_is_futile() {
    use crate::supervise::policy::turn_end::{TurnEndAction, decide_turn_end};
    let t = t0();
    for kind in [
        WallKind::UsageWeekly,
        WallKind::ModelBucket { consent: false },
    ] {
        let mut cfg = cfg();
        cfg.model_fallback = None;
        let mut r = walled(kind, "You've hit your weekly limit · resets 4pm");
        // The reset came at `t`; the wall answers each continuation at once.
        r.reset_at = Some(t);
        let mut st = TurnEndState::default();
        let mut now = t + MIN;
        let mut tries = 0;
        let mut futile_after = Vec::new();
        while tries < FUTILE_AFTER_TRIES + 1 {
            r.worked = Some(if tries == 0 {
                3 * MIN
            } else {
                Duration::from_secs(4)
            });
            st.observe(&r, now);
            match decide_turn_end(&st, &r, &cfg, now) {
                a @ TurnEndAction::Type { .. } => {
                    st.acted(&a, &r, now);
                    tries += 1;
                    futile_after.push(exception_for(&st, &r, &cfg, now).map(|e| e.kind));
                    now += Duration::from_secs(5);
                }
                TurnEndAction::WaitUntil { until, .. } => now = until,
                other => panic!("{kind:?}: {other:?}"),
            }
        }
        assert_eq!(
            futile_after,
            [None, None, None, Some(ExceptionKind::Futile)],
            "{kind:?}: three tries by 41 minutes past the reset are not yet an hour"
        );
        let early = exception_for(&st, &r, &cfg, t + 30 * MIN);
        assert_eq!(early, None, "{kind:?}: less than an hour past the reset");
        assert!(
            exception_for(&st, &r, &cfg, t + 61 * MIN)
                .is_some_and(|e| e.kind == ExceptionKind::Futile),
            "{kind:?}"
        );
        // Two tries are not enough.
        let mut two = TurnEndState::default();
        r.worked = Some(3 * MIN);
        two.observe(&r, t + MIN);
        let a = decide_turn_end(&two, &r, &cfg, t + MIN);
        two.acted(&a, &r, t + MIN);
        assert_eq!(
            exception_for(&two, &r, &cfg, t + 90 * MIN),
            None,
            "{kind:?}"
        );
    }
}

/// A real Claude Code screen as the policy reads it.
fn read_screen(fixture: &str, worked: Option<Duration>) -> TurnEndReading {
    let rows = screen(fixture);
    let reading = aterm_phase::read(Some("claude"), &rows, Some(2));
    TurnEndReading::of(&reading, &rows, false, worked, None, None)
}

/// The exception after each answer to a wall as the loop meets it: `wall`, an
/// act typed at it on the ladder, the next of `answers` drawn, and again.
fn answered(wall: &str, answers: &[&str]) -> Vec<Option<Exception>> {
    let (mut st, mut now) = (TurnEndState::default(), t0());
    let mut r = read_screen(wall, Some(3 * MIN));
    let mut told = Vec::new();
    for answer in answers {
        loop {
            st.observe(&r, now);
            match decide_turn_end(&st, &r, &cfg(), now) {
                a @ (TurnEndAction::Type { .. } | TurnEndAction::TypeCommand { .. }) => {
                    st.acted(&a, &r, now);
                    break;
                }
                TurnEndAction::WaitUntil { until, .. } => now = until,
                other => panic!("{other:?}"),
            }
        }
        now += Duration::from_secs(5);
        r = read_screen(answer, None);
        st.observe(&r, now);
        told.push(exception_for(&st, &r, &cfg(), now));
    }
    told
}

/// A full context Claude Code says `/compact` cannot reduce, three `/compact`s
/// running, is a person's; a request error on a later rung does not end it.
/// Negative controls: two compacts, three request errors (Claude Code never
/// said it), and three tries at an outage.
#[test]
fn a_full_context_three_compacts_leave_full_is_an_exception_with_the_way_back() {
    let told = answered(
        CONTEXT_WALL_MEASURED,
        &[
            COMPACT_FAILED_1_MEASURED,
            COMPACT_FAILED_2_MEASURED,
            COMPACT_FAILED_3_MEASURED,
            COMPACT_FAILED_3_MEASURED,
        ],
    );
    assert_eq!(told[..2], [None, None], "one and two compacts are a ladder");
    // The third failure raises it and the fourth leaves it standing.
    assert_eq!(
        told[2..],
        [Some(Exception::context()), Some(Exception::context())]
    );
    let e = told[2].as_ref().expect("raised");
    assert_eq!((e.kind, e.when), (ExceptionKind::Context, Urgency::Later));
    let between = [
        COMPACT_FAILED_1_MEASURED,
        COMPACT_FAILED_2_MEASURED,
        COMPACT_FAILED_3_MEASURED,
        COMPACT_ERROR_500_MEASURED,
        COMPACT_FAILED_3_MEASURED,
    ];
    assert_eq!(
        answered(CONTEXT_WALL_MEASURED, &between)[2..],
        std::array::from_fn::<_, 3, _>(|_| Some(Exception::context())),
        "a request error between exhausted rungs leaves it standing"
    );
    let errors = [COMPACT_ERROR_500_MEASURED; 3];
    assert_eq!(answered(CONTEXT_WALL_MEASURED, &errors)[2], None);
    assert_eq!(answered(END_529, &[END_529; 3])[2], None);
}

/// A wait is never an exception: a reset ahead, an API outage, the vendor's
/// own countdown, a full context.
#[test]
fn a_wait_is_never_an_exception() {
    let t = t0();
    let st = TurnEndState::default();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets 4pm",
    );
    weekly.reset_at = Some(t + 200 * MIN);
    assert_eq!(exception_for(&st, &weekly, &cfg(), t), None);
    weekly.resumes_by_itself = true;
    assert_eq!(exception_for(&st, &weekly, &cfg(), t), None);
    for kind in [WallKind::Overloaded, WallKind::Context] {
        assert_eq!(
            exception_for(&st, &walled(kind, "529"), &cfg(), t),
            None,
            "{kind:?}"
        );
    }
}

/// Every text fits the digest's caps and lint (the module doc's rules).
#[test]
fn the_exception_texts_fit_the_digests_caps_and_lint() {
    let t = t0();
    let mut st = TurnEndState::default();
    let mut all: Vec<Exception> = Vec::new();
    for (needle, _, _) in ACCOUNT_LIMITS {
        let m = if needle.is_empty() {
            "You're out of usage credits"
        } else {
            needle
        };
        all.push(exception_for(&st, &walled(WallKind::Spend, m), &cfg(), t).expect(m));
    }
    let bucket = walled(
        WallKind::ModelBucket { consent: false },
        "You've reached your Sonnet limit · run /usage-credits",
    );
    seen(&mut st, &bucket, t);
    for c in [
        cfg(),
        SupervisorConfig {
            model_fallback: None,
            ..cfg()
        },
    ] {
        all.push(exception_for(&st, &bucket, &c, t + 2 * 60 * MIN).expect("bucket"));
    }
    let cleared = walled(
        WallKind::Auth,
        "Login expired · Please run /login — Goal cleared after an unrecoverable error (organization verification required): \"x\".",
    );
    all.push(exception_for(&TurnEndState::default(), &cleared, &cfg(), t).expect("cleared"));
    let auth = walled(WallKind::Auth, "Login expired · Please run /login");
    let off = SupervisorConfig {
        retry_api_errors: false,
        ..cfg()
    };
    all.push(exception_for(&TurnEndState::default(), &auth, &off, t).expect("login"));
    // A renewable login's end not waited at: to renew, and, renewed, over a
    // draft and with nothing to relaunch the tab.
    {
        let renewable = TurnEndReading {
            login_label: Some("m7-a".to_string()),
            ..auth.clone()
        };
        let superseded = TurnEndReading {
            login_superseded: true,
            ..renewable.clone()
        };
        let drafted = TurnEndReading {
            composer: Composer::Typed,
            ..superseded.clone()
        };
        let no_relaunch = SupervisorConfig {
            relaunch: false,
            ..cfg()
        };
        let st = TurnEndState::default();
        for (r, c) in [
            (&renewable, &no_relaunch),
            (&drafted, &cfg()),
            (&superseded, &no_relaunch),
        ] {
            all.push(exception_for(&st, r, c, t).expect("renewable"));
        }
    }
    // A lost login after a move, the host advising nothing at all.
    {
        use crate::supervise::policy::turn_end::{AccountAt, Restart};
        let mut st = TurnEndState::default();
        let to = Restart::Account {
            to: "work-2".to_string(),
            at: AccountAt::Wall,
        };
        st.restarted(&to, true, &auth, t);
        st.host_typed(t);
        st.observe(&auth, t + MIN);
        all.push(exception_for(&st, &auth, &cfg(), t + MIN).expect("no account to move to"));
    }
    // A usage limit that answered every continuation an hour past its reset.
    {
        use crate::supervise::policy::turn_end::{TurnEndAction, decide_turn_end};
        let mut r = walled(
            WallKind::UsageWeekly,
            "You've hit your weekly limit · resets 4pm",
        );
        r.reset_at = Some(t);
        let mut st = TurnEndState::default();
        let mut now = t + MIN;
        let mut tries = 0;
        while tries < FUTILE_AFTER_TRIES + 1 {
            r.worked = Some(if tries == 0 {
                3 * MIN
            } else {
                Duration::from_secs(4)
            });
            st.observe(&r, now);
            match decide_turn_end(&st, &r, &cfg(), now) {
                a @ TurnEndAction::Type { .. } => {
                    st.acted(&a, &r, now);
                    tries += 1;
                    now += Duration::from_secs(5);
                }
                TurnEndAction::WaitUntil { until, .. } => now = until,
                other => panic!("{other:?}"),
            }
        }
        all.push(exception_for(&st, &r, &cfg(), t + 61 * MIN).expect("futile"));
    }
    // A full context that three `/compact`s left full.
    let failed = [COMPACT_FAILED_1_MEASURED; 3];
    all.push(
        answered(CONTEXT_WALL_MEASURED, &failed)[2]
            .clone()
            .expect("context"),
    );
    // A spend wall, a lost login and a futile limit in a Codex session, and
    // in any other program's.
    for program in [aterm_phase::Program::Codex, aterm_phase::Program::Generic] {
        let st = TurnEndState::default();
        let spend = TurnEndReading {
            program,
            ..walled(WallKind::Spend, "Your workspace is out of credits.")
        };
        all.push(exception_for(&st, &spend, &cfg(), t).expect("spend"));
        let lost = TurnEndReading {
            program,
            ..auth.clone()
        };
        all.push(exception_for(&st, &lost, &off, t).expect("login"));
        let mut r = TurnEndReading {
            program,
            reset_at: Some(t),
            ..walled(
                WallKind::UsageWeekly,
                "You've hit your usage limit. Try again at 4:00 PM.",
            )
        };
        let st = tried_past_the_reset(&mut r, &cfg(), t);
        all.push(exception_for(&st, &r, &cfg(), t + 61 * MIN).expect("futile"));
    }
    // The canary's third rung (`wall_memory`), which no point reaches.
    all.push(Exception::stalled());
    all.push(Exception::dialog());
    assert!(all.len() >= 23, "{}", all.len());
    for e in &all {
        assert!(e.why.chars().count() <= WHY_MAX, "{e:?}");
        assert!(e.action.chars().count() <= ACTION_MAX, "{e:?}");
        assert!(
            !e.action.contains(" or ") && !e.action.contains("; ") && !e.action.contains('\n'),
            "{e:?}"
        );
    }
}
