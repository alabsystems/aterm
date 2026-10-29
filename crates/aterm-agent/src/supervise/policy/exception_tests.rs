// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor exception policy tests.

use super::*;
use crate::supervise::phase::Phase;
use crate::supervise::policy::notice;
use crate::supervise::policy::turn_end::{
    Composer, RULE_LOGIN, TurnEndAction, TurnEndReading, decide_turn_end,
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
        said_cut: false,
        opens_cut: false,
        turn_said: None,
        opened_by: None,
        message_drawn: false,
        submits: None,
        inputs_before: None,
        screen_mark: None,
        worked: Some(3 * MIN),
        rules: None,
        fact: None,
        wake: None,
        pending_input: false,
        interrupted: false,
        restartable: true,
        relaunch_off: false,
        resume: None,
        upgrading: false,
        taskless: false,
        finished_record: None,
        left_record: false,
        left_work: None,
        person: None,
        reach: Default::default(),
        login_back: false,
        auto_continue: None,
        since_limit: None,
        background: false,
        served_in_transcript: false,
        chosen_in_transcript: None,
        conversation: crate::supervise::policy::turn_end::Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
        program: aterm_phase::Program::Claude,
        seat: Default::default(),
        account: None,
        autopilot: crate::supervise::Autopilot::Cruise,
        login_label: None,
        login_superseded: false,
        login_notice: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        ran_tools: false,
        tools_unread: false,
        record_turn: None,
        credits: None,
        repo: crate::harness::repo_work::RepoRead::Clean,
        login_screen: None,
        login_interrupted: false,
        login_at_wall: None,
        login_method: None,
        login_shared: None,
        login_moved: false,
        roster: None,
    }
}

fn seen(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) {
    st.observe(r, now);
}

fn notice_id(e: Option<&Exception>) -> Option<&'static str> {
    e.and_then(|e| e.notice.as_ref()).map(|n| n.id)
}

fn point(
    st: &mut TurnEndState,
    r: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> (TurnEndAction, Option<Exception>) {
    st.observe(r, now);
    (
        decide_turn_end(st, r, cfg, now),
        exception_for(st, r, cfg, now),
    )
}

fn dialog(text: &str) -> TurnEndReading {
    let rows = screen(text);
    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
    TurnEndReading {
        login_method: Some(aterm_phase::LoginMethod::Subscription),
        ..TurnEndReading::of(&read, &rows, false, None, None, None)
    }
}

fn typed_login(t: Instant) -> TurnEndState {
    let mut st = TurnEndState::default();
    let wall = walled(WallKind::Auth, "Login expired · Please run /login");
    let (a, _) = point(&mut st, &wall, &cfg(), t);
    assert_eq!(a.rule_id(), Some(RULE_LOGIN), "{a:?}");
    st.acted(&a, &wall, t);
    st
}

#[test]
fn a_lost_logins_notice_is_what_the_point_decides() {
    let t = t0();
    let wall = walled(WallKind::Auth, "Login expired · Please run /login");
    let mut st = TurnEndState::default();
    let (a, e) = point(&mut st, &wall, &cfg(), t);
    assert_eq!(a.rule_id(), Some(RULE_LOGIN), "{a:?}");
    assert_eq!(e, None, "aterm is at it");
    let typing = TurnEndReading {
        person: Some(Duration::from_secs(1)),
        worked: None,
        ..wall.clone()
    };
    let (a, e) = point(&mut st, &typing, &cfg(), t + Duration::from_secs(20));
    assert!(matches!(a, TurnEndAction::WaitUntil { .. }), "{a:?}");
    assert_eq!(e, None, "a wait with an end");
    let not_run = |st: &mut TurnEndState, r: &TurnEndReading, cfg: &SupervisorConfig, why| {
        let (a, e) = point(st, r, cfg, t + MIN);
        assert!(
            matches!(a, TurnEndAction::Escalate { .. } | TurnEndAction::Nothing),
            "{a:?}"
        );
        assert_eq!(
            notice_id(e.as_ref()),
            Some(notice::LOGIN_NOT_RUN.id),
            "{e:?}"
        );
        let e = e.expect("told");
        assert!(e.notice.is_some_and(|n| n.did.contains(why)), "{why}");
    };
    let drafted = TurnEndReading {
        composer: Composer::Typed,
        ..wall.clone()
    };
    let (a, e) = point(&mut TurnEndState::default(), &drafted, &cfg(), t + MIN);
    assert!(matches!(a, TurnEndAction::Escalate { .. }), "{a:?}");
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_NOT_RUN_DRAFT.id),
        "{e:?}"
    );
    assert_ne!(notice::LOGIN_NOT_RUN_DRAFT.todo, notice::LOGIN_NOT_RUN.todo);
    let off = SupervisorConfig {
        retry_api_errors: false,
        ..cfg()
    };
    not_run(
        &mut TurnEndState::default(),
        &wall,
        &off,
        notice::WHY_RETRY_OFF,
    );
    let taskless = TurnEndReading {
        taskless: true,
        ..wall.clone()
    };
    not_run(
        &mut TurnEndState::default(),
        &taskless,
        &cfg(),
        notice::WHY_NO_TASK,
    );
    let finished = TurnEndReading {
        finished_record: Some(true),
        ..wall.clone()
    };
    let (a, e) = point(&mut TurnEndState::default(), &finished, &cfg(), t);
    assert_eq!(a.rule_id(), Some(RULE_LOGIN), "{a:?}");
    assert_eq!(e, None, "aterm is at it");
    let budget = SupervisorConfig {
        continue_per_hour: 1,
        ..cfg()
    };
    let mut st = TurnEndState::default();
    let ended = TurnEndReading {
        wall: None,
        wall_message: String::new(),
        ..wall.clone()
    };
    let (a, _) = point(&mut st, &ended, &budget, t);
    assert!(a.rule_id().is_some(), "a continuation: {a:?}");
    st.acted(&a, &ended, t);
    let met = TurnEndReading {
        worked: None,
        ..wall.clone()
    };
    st.observe(&met, t + Duration::from_secs(5));
    not_run(&mut st, &met, &budget, notice::WHY_BUDGET);
}

#[test]
fn the_sign_in_is_told_where_it_is_the_owners() {
    use aterm_phase::prompt::fixtures::{
        LOGIN_CONSOLE_MENU, LOGIN_ERROR, LOGIN_HANDOFF, LOGIN_INTERRUPTED, LOGIN_PICKER,
        LOGIN_SUCCESS,
    };
    let t = t0();
    let wall = walled(WallKind::Auth, "Login expired · Please run /login");
    let at = |st: &mut TurnEndState, r: &TurnEndReading, now| point(st, r, &cfg(), now).1;
    let handoff = dialog(LOGIN_HANDOFF);
    assert_eq!(
        handoff.login_screen,
        Some(aterm_phase::LoginScreen::Browser)
    );
    let e = at(&mut typed_login(t), &handoff, t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_SIGN_IN.id),
        "{e:?}"
    );
    // Over a roster row's login aterm backs out of the dialog of its own `/login` and signs the
    // row in by itself: nobody is told to sign in there.
    let rostered = TurnEndReading {
        login_label: Some("work-2".to_string()),
        ..handoff.clone()
    };
    let (a, e) = point(&mut typed_login(t), &rostered, &cfg(), t + MIN);
    assert_eq!(e, None, "{a:?}");
    assert!(matches!(a, TurnEndAction::Cancel { .. }), "{a:?}");
    let picker = dialog(LOGIN_PICKER);
    assert_eq!(at(&mut typed_login(t), &picker, t + MIN), None);
    let unknown = TurnEndReading {
        login_method: None,
        ..picker.clone()
    };
    let e = at(&mut typed_login(t), &unknown, t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_CHOOSE.id),
        "{e:?}"
    );
    let e = at(&mut typed_login(t), &dialog(LOGIN_CONSOLE_MENU), t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_CHOOSE.id),
        "{e:?}"
    );
    let e = at(&mut typed_login(t), &dialog(LOGIN_ERROR), t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_STOPPED.id),
        "{e:?}"
    );
    let e = at(&mut typed_login(t), &dialog(LOGIN_INTERRUPTED), t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_CANCELLED.id),
        "{e:?}"
    );
    assert_eq!(
        e.map(|e| e.action),
        Some(notice::LOGIN_CANCELLED.todo.to_string())
    );
    let rostered_left = TurnEndReading {
        login_label: Some("work-2".to_string()),
        ..dialog(LOGIN_INTERRUPTED)
    };
    let (a, e) = point(&mut typed_login(t), &rostered_left, &cfg(), t + MIN);
    assert_eq!(e, None, "the row's own sign-in waited on: {a:?}");
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("work-2")),
        "{a:?}"
    );
    let again = TurnEndReading {
        worked: None,
        ..wall.clone()
    };
    let mut st = typed_login(t);
    assert_eq!(
        at(&mut st, &again, t + Duration::from_secs(5)),
        None,
        "the dialog to come"
    );
    let e = at(&mut st, &again, t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_CANCELLED.id),
        "{e:?}"
    );
    assert_eq!(
        at(&mut typed_login(t), &dialog(LOGIN_SUCCESS), t + MIN),
        None
    );
}

#[test]
fn a_persons_own_login_is_theirs() {
    use aterm_phase::prompt::fixtures::{
        LOGIN_CONSOLE_MENU, LOGIN_HANDOFF, LOGIN_INTERRUPTED, LOGIN_PICKER,
    };
    let t = t0();
    let wall = walled(WallKind::Auth, "Login expired · Please run /login");
    for text in [LOGIN_PICKER, LOGIN_CONSOLE_MENU, LOGIN_HANDOFF] {
        let mut st = TurnEndState::default();
        let r = TurnEndReading {
            wall: None,
            ..dialog(text)
        };
        let (a, e) = point(&mut st, &r, &cfg(), t);
        assert_eq!(e, None, "{text}");
        assert_eq!(a.rule_id(), None, "{text}: {a:?}");
    }
    for text in [LOGIN_PICKER, LOGIN_CONSOLE_MENU, LOGIN_HANDOFF] {
        let mut st = TurnEndState::default();
        st.observe(&wall, t);
        let (_, e) = point(&mut st, &dialog(text), &cfg(), t + Duration::from_secs(5));
        assert_eq!(e, None, "{text}");
    }
    let mut st = TurnEndState::default();
    st.observe(&wall, t);
    let (a, e) = point(&mut st, &dialog(LOGIN_INTERRUPTED), &cfg(), t + MIN);
    assert_eq!(a, TurnEndAction::Nothing);
    assert_eq!(notice_id(e.as_ref()), Some(notice::LOGIN_LEFT.id), "{e:?}");
    let next = TurnEndReading {
        worked: None,
        ..wall.clone()
    };
    for k in 2..=4 {
        let (a, e) = point(&mut st, &next, &cfg(), t + k * MIN);
        assert_eq!(a.rule_id(), None, "no /login over their cancel: {a:?}");
        assert_eq!(notice_id(e.as_ref()), Some(notice::LOGIN_LEFT.id), "{e:?}");
    }
    let mut st = typed_login(t);
    let (_, e) = point(&mut st, &dialog(LOGIN_INTERRUPTED), &cfg(), t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_CANCELLED.id),
        "{e:?}"
    );
}

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
    let mut r = walled(
        WallKind::Spend,
        "Monthly spend limit reached · resets Oct 1",
    );
    r.reset_at = Some(t + 90 * MIN);
    assert_eq!(exception_for(&st, &r, &cfg(), t), None);
}

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
            // The same answer to each of the ladder's tries holds it after them.
            TurnEndAction::Nothing if tries == FUTILE_AFTER_TRIES => break,
            other => panic!("{other:?}"),
        }
    }
    st
}

#[test]
fn a_codex_sessions_walls_get_words_that_name_no_vendor() {
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
        let r = TurnEndReading {
            program: Program::Codex,
            ..walled(WallKind::Spend, notice)
        };
        let st = TurnEndState::default();
        let e = exception_for(&st, &r, &cfg(), t).expect(notice);
        assert_eq!((e.kind, e.when), (ExceptionKind::Spend, Urgency::Later));
        assert!(e.action.contains("the program's notice"), "{e:?}");
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
        (Program::Codex, "the program's account"),
        (
            Program::Muse,
            "Run /upgrade in this tab for increased limits",
        ),
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
    let lost = TurnEndReading {
        program: Program::Codex,
        ..walled(
            WallKind::Auth,
            "Your access token could not be refreshed. Please log out and sign in again.",
        )
    };
    let e = exception_for(&TurnEndState::default(), &lost, &off, t).expect("login");
    assert_eq!(
        (e.kind, e.action.as_str(), e.clears, e.escalated),
        (
            ExceptionKind::Login,
            "Sign the program in again as its notice in this tab says.",
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
    for (message, wants) in [
        (
            "your API key from META_API_KEY was rejected — update or unset it",
            "As Muse Code says, update or unset META_API_KEY.",
        ),
        (
            "Log in with browser · Enter to choose",
            "Sign in to Muse Code again with muse login in a shell.",
        ),
    ] {
        let muse = TurnEndReading {
            program: Program::Muse,
            ..walled(WallKind::Auth, message)
        };
        let e = exception_for(&TurnEndState::default(), &muse, &off, t).expect("login");
        assert_eq!((e.kind, e.action.as_str()), (ExceptionKind::Login, wants));
    }

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

#[test]
fn a_cleared_goal_names_its_cause() {
    let t = t0();
    let st = TurnEndState::default();
    let r = walled(
        WallKind::ApiError {
            code: Some(500),
            retryable: true,
            cause: aterm_phase::ApiCause::Server,
        },
        "API Error: 500 — Goal cleared after an unrecoverable error (server error): \"x\". Run /goal again to continue.",
    );
    let e = exception_for(&st, &r, &cfg(), t).expect("an exception");
    assert_eq!(e.kind, ExceptionKind::GoalCleared);
    assert!(e.why.contains("server error"), "{e:?}");
    assert!(e.action.contains("/goal"), "{e:?}");
    let login = walled(
        WallKind::Auth,
        "Login expired · Please run /login — Goal cleared after an unrecoverable error (authentication failed): \"x\". Run /goal again to continue.",
    );
    assert_eq!(
        exception_for(&st, &login, &cfg(), t),
        None,
        "aterm runs /login first"
    );
    let off = SupervisorConfig {
        retry_api_errors: false,
        ..cfg()
    };
    let e = exception_for(&st, &login, &off, t).expect("an exception");
    assert_eq!((e.kind, e.when), (ExceptionKind::Login, Urgency::Now));
    assert!(e.notice.is_some(), "{e:?}");
}

#[test]
fn a_bucket_no_reset_ends_is_an_exception_only_while_the_tab_cannot_switch() {
    let t = t0();
    let fable = "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.";
    let kind = WallKind::ModelBucket { consent: false };
    let r = walled(kind, fable);
    let mut st = TurnEndState::default();
    seen(&mut st, &r, t);
    assert_eq!(exception_for(&st, &r, &cfg(), t + 59 * MIN), None);
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
    let none = SupervisorConfig {
        model_fallback: None,
        ..cfg()
    };
    let e = exception_for(&st, &r, &none, t + 61 * MIN).expect("no fallback");
    assert!(e.why.contains("no other model"), "{e:?}");
    assert!(e.action.contains("another model"), "{e:?}");
    let mut opus = walled(kind, "You've reached your Opus limit · run /usage-credits");
    opus.restartable = false;
    let mut st2 = TurnEndState::default();
    seen(&mut st2, &opus, t);
    assert!(
        exception_for(&st2, &opus, &cfg(), t + 61 * MIN)
            .is_some_and(|e| e.why.contains("no other model"))
    );
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
    let mut named = walled(kind, "You've reached your Opus limit · resets 4pm");
    named.reset_at = Some(t + 300 * MIN);
    let mut st3 = TurnEndState::default();
    seen(&mut st3, &named, t);
    assert_eq!(exception_for(&st3, &named, &cfg(), t + 120 * MIN), None);
}

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
        let said = "You've hit your weekly limit · resets 4pm";
        let mut r = walled(kind, said);
        r.reset_at = Some(t);
        let mut st = TurnEndState::default();
        let mut now = t + MIN;
        let mut tries = 0;
        let mut futile_after = Vec::new();
        // Each try answered in its own words: no reply comes again, and the hour bounds the word.
        let words = ["", " · later", " · try later", " · still"];
        let mut worked = Some(3 * MIN);
        while tries < FUTILE_AFTER_TRIES + 1 {
            // A turn is read once, at the point it ended.
            r.worked = worked.take();
            r.wall_message = format!("{said}{}", words[tries]);
            st.observe(&r, now);
            match decide_turn_end(&st, &r, &cfg, now) {
                a @ TurnEndAction::Type { .. } => {
                    st.acted(&a, &r, now);
                    tries += 1;
                    futile_after.push(exception_for(&st, &r, &cfg, now).map(|e| e.kind));
                    now += Duration::from_secs(5);
                    worked = Some(Duration::from_secs(4));
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
        // The same answer to each holds the ladder after its tries, and says so at once.
        let mut same = walled(kind, said);
        same.reset_at = Some(t);
        let held = tried_past_the_reset(&mut same, &cfg, t);
        assert_eq!(
            decide_turn_end(&held, &same, &cfg, t + 45 * MIN),
            TurnEndAction::Nothing,
            "{kind:?}"
        );
        assert_eq!(
            exception_for(&held, &same, &cfg, t + 45 * MIN),
            Some(Exception::limit_repeats(same.program, true)),
            "{kind:?}"
        );
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

fn read_screen(fixture: &str, worked: Option<Duration>) -> TurnEndReading {
    let rows = screen(fixture);
    let reading = aterm_phase::read(Some("claude"), &rows, Some(2));
    TurnEndReading::of(&reading, &rows, false, worked, None, None)
}

fn answered(wall: &str, answers: &[&str]) -> Vec<Option<Exception>> {
    answered_from(read_screen(wall, Some(3 * MIN)), answers)
}

fn answered_from(first: TurnEndReading, answers: &[&str]) -> Vec<Option<Exception>> {
    let (mut st, mut now) = (TurnEndState::default(), t0());
    let mut r = first;
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

#[test]
fn a_full_context_images_fill_is_told_with_the_way_back_its_notice_names() {
    const IMAGES: &str = "An image in the conversation exceeds the dimension limit for many-image requests (2000px). Run /compact to remove old images from context, or start a new session.";
    const ATTACHMENTS: &str = "Request too large (max 32 MB). Accumulated images and attachments in the conversation pushed the request over the limit. Run /compact, or double press esc to go back and remove attachments.";
    let failed = [
        COMPACT_FAILED_1_MEASURED,
        COMPACT_FAILED_2_MEASURED,
        COMPACT_FAILED_3_MEASURED,
    ];
    let plain = Exception::context();
    for (notice, names) in [(IMAGES, "image"), (ATTACHMENTS, "attachment")] {
        let told = answered_from(walled(WallKind::Context, notice), &failed);
        assert_eq!(told[..2], [None, None], "{names}: a ladder first");
        let e = told[2].clone().expect("raised");
        assert_eq!((e.kind, e.when), (ExceptionKind::Context, Urgency::Later));
        assert!(
            e.action != plain.action && e.action.contains(names) && e.why.contains(names),
            "{names}: {e:?}"
        );
        let r = walled(WallKind::Context, notice);
        assert_eq!(Exception::context_of(&r.wall_message, None), e, "{names}");
    }
    assert_eq!(answered(CONTEXT_WALL_MEASURED, &failed)[2], Some(plain));
}

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

#[test]
fn a_refused_request_is_told_with_the_remedy_its_notice_names() {
    for (typed, message, wants) in [
        (
            Some("tool_history_mismatch"),
            "API Error: 400 due to tool use concurrency issues.",
            "/rewind",
        ),
        (
            None,
            "API Error: 400 due to tool use concurrency issues. Run /rewind to recover the conversation.",
            "/rewind",
        ),
        (
            Some("claude_code_version_too_old"),
            "API Error: Claude Code 2.1.200 does not support this model; version 2.1.250 or newer is required. Run 'claude update', or update the Claude desktop app, then try again.",
            "aterm pkg update claude",
        ),
        (
            Some("model_not_found"),
            "There's an issue with the selected model (x). It may not exist or you may not have access to it. Run /model to pick a different model.",
            "/model",
        ),
        (
            Some("safety_monitor_blocked"),
            "API Error: Claude can't help with this. Start a new session to continue.",
            "rephrase",
        ),
        (
            None,
            "API Error: Opus 4.1's safeguards flagged this message.",
            "rephrase",
        ),
        (
            Some("account_on_hold"),
            "Your account is on hold and can't use Claude Code. View details or appeal: https://claude.ai/restricted",
            "claude.ai/restricted",
        ),
        (
            Some("effort_requires_thinking"),
            "API Error: Effort 'max' isn't available with thinking turned off on this model · run /effort high to continue",
            "/effort high",
        ),
        (
            Some("no_such_kind"),
            "API Error: 400 due to tool use concurrency issues. Run /rewind to recover the conversation.",
            "/rewind",
        ),
        (
            Some("no_such_kind"),
            "API Error: 418 I'm a teapot",
            "last error asks",
        ),
        (None, "API Error: 404 Not Found", "last error asks"),
    ] {
        let e = Exception::refused(typed, message);
        assert_eq!((e.kind, e.when), (ExceptionKind::Refused, Urgency::Later));
        assert!(e.action.contains(wants), "{typed:?} {message}: {e:?}");
        assert!(
            !e.action.contains("teapot") && !e.action.contains("API Error"),
            "{e:?}"
        );
    }
}

#[test]
fn the_long_contexts_usage_credits_are_an_account_limit() {
    let t = t0();
    let st = TurnEndState::default();
    let new = walled(
        WallKind::Spend,
        "API Error: Usage credits required for 1M context · run /model to switch to standard context",
    );
    let e = exception_for(&st, &new, &cfg(), t).expect("told");
    assert_eq!(e.kind, ExceptionKind::Spend);
    assert!(e.why.contains("Long context"), "{e:?}");
    let old = exception_for(
        &st,
        &walled(
            WallKind::Spend,
            "Extra usage is required for long context · run /extra-usage",
        ),
        &cfg(),
        t,
    )
    .expect("told");
    assert_ne!(e.why, old.why);
}

#[test]
fn a_weekly_usage_limit_does_not_raise_a_nag_exception() {
    use crate::supervise::policy::turn_end::{Ready, RosterWord};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.resumes_by_itself = true;
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    assert_eq!(exception_for(&st, &weekly, &cfg(), t), None);

    let mut drafted = weekly.clone();
    drafted.composer = Composer::Typed;
    drafted.roster = Some(RosterWord::Row {
        next: None,
        ready: Some(Ready {
            label: "m8-3".to_string(),
        }),
        held: None,
    });
    assert_eq!(exception_for(&st, &drafted, &cfg(), t), None);
    assert!(draft_holds_limit_switch(&st, &drafted, &cfg()));

    let mut in_browser = weekly.clone();
    in_browser.wall = None;
    in_browser.login_screen = Some(LoginScreen::Browser);
    assert_eq!(exception_for(&st, &in_browser, &cfg(), t), None);

    let failed = TurnEndReading {
        login_screen: Some(LoginScreen::Failed),
        ..in_browser
    };
    let e = exception_for(&st, &failed, &cfg(), t).expect("stopped");
    assert_eq!(e.notice.as_ref().map(|n| n.id), Some("login-stopped"));

    let mut interrupted = weekly;
    interrupted.wall = None;
    interrupted.login_interrupted = true;
    assert_eq!(exception_for(&st, &interrupted, &cfg(), t), None);

    let lost = walled(WallKind::Auth, "Login expired · Please run /login");
    let mut st = TurnEndState::default();
    st.observe(&lost, t);
    let left_lost = TurnEndReading {
        wall: None,
        login_interrupted: true,
        ..lost
    };
    let e = exception_for(&st, &left_lost, &cfg(), t + MIN).expect("told");
    assert_eq!(e.notice.as_ref().map(|n| n.id), Some("login-left"), "{e:?}");
}

#[test]
fn an_api_errors_wall_held_after_its_notice_left_is_told_stalled_not_a_limit() {
    let kind = WallKind::ApiError {
        code: None,
        retryable: true,
        cause: aterm_phase::ApiCause::CutOff,
    };
    let mut r = walled(
        kind,
        "API Error: Your computer went to sleep mid-response. The response above may be \
         incomplete.",
    );
    let mut st = TurnEndState::default();
    let mut now = t0();
    let mut tries = 0;
    // Each retry answered by the same error at once, until the ladder holds.
    loop {
        let (a, _) = point(&mut st, &r, &cfg(), now);
        match a {
            TurnEndAction::WaitUntil { until, .. } => {
                now = until;
                r.worked = None;
            }
            TurnEndAction::Nothing => break,
            a => {
                st.acted(&a, &r, now);
                tries += 1;
                now += Duration::from_secs(5);
                r.worked = Some(Duration::from_secs(4));
            }
        }
        assert!(tries <= FUTILE_AFTER_TRIES, "{tries} tries");
    }
    assert_eq!(tries, FUTILE_AFTER_TRIES);
    // The wall read once more where it holds: the last answer settled to it.
    r.worked = None;
    let (a, told) = point(&mut st, &r, &cfg(), now + MIN);
    assert_eq!(a, TurnEndAction::Nothing);
    assert_eq!(
        told,
        Some(Exception::repeats(crate::harness::acts::HeldBy::SameReply))
    );
    let left = TurnEndReading {
        wall: None,
        wall_message: String::new(),
        ..r
    };
    let (a, told) = point(&mut st, &left, &cfg(), now + 2 * MIN);
    assert_eq!(a, TurnEndAction::Nothing, "its track still holds");
    assert_eq!(
        told,
        Some(Exception::repeats(crate::harness::acts::HeldBy::SameReply)),
        "an API error's track is no limit"
    );
}

/// AN OUTAGE WALL RAISES NOTHING (v6 §14.17.5.2): a server error answered the same at each of
/// its tries is waited out on its ladder for hours, never held and never told, at the wall and
/// once its notice left the screen.
#[test]
fn an_outage_walls_same_answers_raise_nothing_however_long_it_stands() {
    let kind = WallKind::ApiError {
        code: Some(500),
        retryable: true,
        cause: aterm_phase::ApiCause::Server,
    };
    let mut r = walled(kind, "API Error: 500 Internal server error");
    let mut st = TurnEndState::default();
    let start = t0();
    let mut now = start;
    let mut tries = 0;
    while now < start + 6 * 60 * MIN {
        let (a, told) = point(&mut st, &r, &cfg(), now);
        assert_eq!(told, None, "after {tries} tries");
        match a {
            TurnEndAction::WaitUntil { until, .. } => {
                now = until;
                r.worked = None;
            }
            a => {
                assert!(a.rule_id().is_some(), "{a:?}");
                st.acted(&a, &r, now);
                tries += 1;
                now += Duration::from_secs(5);
                r.worked = Some(Duration::from_secs(4));
            }
        }
    }
    assert!(tries > 2 * FUTILE_AFTER_TRIES, "it kept trying: {tries}");
    assert_eq!(st.held_by(), None, "{:?}", st.replies());
    let left = TurnEndReading {
        wall: None,
        wall_message: String::new(),
        worked: None,
        ..r
    };
    let (_, told) = point(&mut st, &left, &cfg(), now + MIN);
    assert_eq!(told, None, "its notice left the screen");
}
