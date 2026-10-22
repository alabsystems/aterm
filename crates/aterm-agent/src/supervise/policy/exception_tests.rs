// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The exceptions: a person is asked for only what only a person can do, with
//! the one action, and a wait is never one.

use std::collections::BTreeSet;

use super::*;
use crate::supervise::phase::Phase;
use crate::supervise::policy::notice;
use crate::supervise::policy::turn_end::{
    Composer, RULE_LOGIN, TurnEndAction, TurnEndReading, Unmade, decide_turn_end,
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
        screen_mark: None,
        worked: Some(3 * MIN),
        rules: None,
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
        account: None,
        autopilot: crate::supervise::Autopilot::Cruise,
        login_label: None,
        login_superseded: false,
        login_notice: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        record_turn: None,
        credits: None,
        repo: None,
        login_screen: None,
        login_interrupted: false,
        login_at_wall: None,
        login_method: None,
        login_shared: None,
        roster: None,
    }
}

fn seen(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) {
    st.observe(r, now);
}

/// The id of the notice `e` carries, if any.
fn notice_id(e: Option<&Exception>) -> Option<&'static str> {
    e.and_then(|e| e.notice.as_ref()).map(|n| n.id)
}

/// Observes `r` at `now`, then what the point decides and the exception that
/// stands there.
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

/// A reading of a real 2.1.303 `/login` screen, the session's login a
/// subscription.
fn dialog(text: &str) -> TurnEndReading {
    let rows = screen(text);
    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
    TurnEndReading {
        login_method: Some(aterm_phase::LoginMethod::Subscription),
        ..TurnEndReading::of(&read, &rows, false, None, None, None)
    }
}

/// A state that saw the wall and typed `/login` into it at `t`.
fn typed_login(t: Instant) -> TurnEndState {
    let mut st = TurnEndState::default();
    let wall = walled(WallKind::Auth, "Login expired · Please run /login");
    let (a, _) = point(&mut st, &wall, &cfg(), t);
    assert_eq!(a.rule_id(), Some(RULE_LOGIN), "{a:?}");
    st.acted(&a, &wall, t);
    st
}

/// THE NOTICE SAYS WHAT THE POINT DECIDES (the review of 2026-10-19:
/// claims-login-ran, F5, F3''): a lost login's notice is chosen with the
/// point's own decision, so none says `/login` ran, or that a browser
/// opened, where nothing was typed. Nothing is told while aterm is still at
/// it — `/login` about to go, a person's keys waited out — and `/login` held
/// back is told with why: a draft, `retry_api_errors = false`, the hour's
/// typing budget, no task. The record's finish ends no task (only a landing
/// or a person's exchange does), so a wall read with it gets `/login`.
#[test]
fn a_lost_logins_notice_is_what_the_point_decides() {
    let t = t0();
    let wall = walled(WallKind::Auth, "Login expired · Please run /login");
    // `/login` about to go: nothing told.
    let mut st = TurnEndState::default();
    let (a, e) = point(&mut st, &wall, &cfg(), t);
    assert_eq!(a.rule_id(), Some(RULE_LOGIN), "{a:?}");
    assert_eq!(e, None, "aterm is at it");
    // A person's key after the wall was read: their grace waited out, nothing told.
    let typing = TurnEndReading {
        person: Some(Duration::from_secs(1)),
        worked: None,
        ..wall.clone()
    };
    let (a, e) = point(&mut st, &typing, &cfg(), t + Duration::from_secs(20));
    assert!(matches!(a, TurnEndAction::WaitUntil { .. }), "{a:?}");
    assert_eq!(e, None, "a wait with an end");
    // Held back: `LOGIN_NOT_RUN`, with why.
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
    // A draft holds it: a notice of its own, whose step clears the draft
    // first (the review of the login carry, 2026-10-20: told as
    // `LOGIN_NOT_RUN`, its step typed `/login` onto the draft). NEGATIVE
    // CONTROL: every other why is `LOGIN_NOT_RUN`'s, and its step is
    // another.
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
    // The hour's typing budget spent by a continuation the wall answered.
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

/// THE SIGN-IN IS TOLD WHERE IT IS THE OWNER'S (F6, LW-2, LW-4): the browser
/// hand-off of the `/login` aterm typed is the one step only they can take,
/// named for the roster row the session's login is; the picker aterm answers
/// is aterm's, one it does not answer the owner's; a `/login` left or stopped
/// is theirs to type again. Inside `/login`'s take window, the wall read with
/// no dialog yet is no sign-in left.
#[test]
fn the_sign_in_is_told_where_it_is_the_owners() {
    use aterm_phase::prompt::fixtures::{
        LOGIN_CONSOLE_MENU, LOGIN_ERROR, LOGIN_HANDOFF, LOGIN_INTERRUPTED, LOGIN_PICKER,
        LOGIN_SUCCESS,
    };
    let t = t0();
    let wall = walled(WallKind::Auth, "Login expired · Please run /login");
    let at = |st: &mut TurnEndState, r: &TurnEndReading, now| point(st, r, &cfg(), now).1;
    // The browser hand-off: the sign-in, the row named where there is one.
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
    let rostered = TurnEndReading {
        login_label: Some("work-2".to_string()),
        ..handoff.clone()
    };
    let e = at(&mut typed_login(t), &rostered, t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_SIGN_IN_AS.id),
        "{e:?}"
    );
    assert!(e.is_some_and(|e| e.action.contains("work-2")));
    // The picker: aterm's to answer with the subscription; with nothing
    // known of the method, the owner's.
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
    // Console's own menu behind it (the picker moved onto Console): how to
    // sign in is still the owner's choice, nothing stopped (the review of
    // the login carry, 2026-10-20: it was told as a sign-in stopped).
    let e = at(&mut typed_login(t), &dialog(LOGIN_CONSOLE_MENU), t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_CHOOSE.id),
        "{e:?}"
    );
    // Stopped on a state of the dialog's own.
    let e = at(&mut typed_login(t), &dialog(LOGIN_ERROR), t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_STOPPED.id),
        "{e:?}"
    );
    // Left: `Login interrupted`, or the wall back with no dialog past
    // `/login`'s take window — and not inside it.
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
    // On a roster row's login, the sign-in by hand names the row (the review
    // of the login carry, 2026-10-20: nothing at the hand-off of the owner's
    // own `/login` does). NEGATIVE CONTROL: the plain step above.
    let rostered_left = TurnEndReading {
        login_label: Some("work-2".to_string()),
        ..dialog(LOGIN_INTERRUPTED)
    };
    let e = at(&mut typed_login(t), &rostered_left, t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_CANCELLED.id),
        "{e:?}"
    );
    assert!(e.is_some_and(|e| e.action.contains("work-2")));
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
    // The login back: nothing told.
    assert_eq!(
        at(&mut typed_login(t), &dialog(LOGIN_SUCCESS), t + MIN),
        None
    );
}

/// A PERSON'S OWN `/login` (the review's F2, F3', voluntary-login-false-alarm,
/// LW-3): on a working session it is no lost login — nothing told, its picker
/// theirs; at a lost login they are going through it — nothing told — and
/// one they left is told as theirs (`login-left`, never "aterm ran /login"),
/// with no `/login` of aterm's after it, their next prompt answered by the
/// wall again included. NEGATIVE CONTROL: aterm's own `/login` left is told
/// as aterm's (`login-cancelled`).
#[test]
fn a_persons_own_login_is_theirs() {
    use aterm_phase::prompt::fixtures::{
        LOGIN_CONSOLE_MENU, LOGIN_HANDOFF, LOGIN_INTERRUPTED, LOGIN_PICKER,
    };
    let t = t0();
    let wall = walled(WallKind::Auth, "Login expired · Please run /login");
    // On a working session: no lost login, nothing told.
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
    // At a lost login, their dialog: nothing told, Console's menu they
    // chose included.
    for text in [LOGIN_PICKER, LOGIN_CONSOLE_MENU, LOGIN_HANDOFF] {
        let mut st = TurnEndState::default();
        st.observe(&wall, t);
        let (_, e) = point(&mut st, &dialog(text), &cfg(), t + Duration::from_secs(5));
        assert_eq!(e, None, "{text}");
    }
    // Left: theirs, and no `/login` after it.
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
    // NEGATIVE CONTROL: aterm's own, left.
    let mut st = typed_login(t);
    let (_, e) = point(&mut st, &dialog(LOGIN_INTERRUPTED), &cfg(), t + MIN);
    assert_eq!(
        notice_id(e.as_ref()),
        Some(notice::LOGIN_CANCELLED.id),
        "{e:?}"
    );
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

/// A CODEX SESSION'S WALLS GET WORDS THAT NAME NO VENDOR, never Claude Code's
/// (the Codex harness is retired, so no Codex step is said, and aterm reads no
/// Codex screen: the walls are built here): its spend walls (the words the
/// 0.162.1 lab measured, `docs/measured/real-claude-lab-2026-10-15.md` M2)
/// send the person to the program's own notice, never to claude.ai or "usage credits"; a usage limit
/// futile past its reset to the program's account; a lost login to its own
/// notice, never the page `/login` opened. Muse Code's get its own:
/// `/upgrade` past a reset, and a key wall's own remedy. NEGATIVE CONTROL:
/// the same walls in a Claude Code session keep Claude Code's words.
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
    // Muse Code's key wall names its own remedy: `META_API_KEY` beats
    // `muse login`. Its signed-out wall, naming none, is `muse login`.
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

/// `Goal cleared after an unrecoverable error (<label>)` names its cause —
/// but where the error is Claude Code's lost login, the sign-in is told
/// first: it is needed now, and nothing runs, the goal included, until it
/// is made.
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
    answered_from(read_screen(wall, Some(3 * MIN)), answers)
}

/// [`answered`] from the wall the loop first meets as `first`.
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

/// A FULL CONTEXT THAT IMAGES OR ATTACHMENTS FILL is told with the way back
/// its own notice names (ATERM-KI-1016-1, limits: "the image notice's context
/// wall reuses the context exception's wording (`go back a few messages`)
/// after three `/compact`s do not help"). Claude Code's notice that an image
/// is past the many-image size, or that images and attachments pushed the
/// request past the API's size, is the full context's wall (its own remedy,
/// `Run /compact`, is the act); where three `/compact`s fail, what fills the
/// context is no run of recent messages but the image or the attachments, so
/// the owner is told to go back to before them — read from the episode's own
/// notice, though the vendor's `Compaction failed` row stands in its place by
/// then. NEGATIVE CONTROL: the plain full context keeps its way back.
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
        // It is the notice's own way back when the notice itself stands.
        let r = walled(WallKind::Context, notice);
        assert_eq!(Exception::context_of(&r.wall_message, None), e, "{names}");
    }
    // NEGATIVE CONTROL: the plain full context.
    assert_eq!(answered(CONTEXT_WALL_MEASURED, &failed)[2], Some(plain));
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

/// A REQUEST THE API REFUSES the same way each time is told with the remedy
/// its notice names — by the typed kind Claude Code wrote with it where the
/// session's transcript has one, else by its words — and every other with
/// the tab's own: fixed aterm words, nothing of the notice quoted.
/// NEGATIVE CONTROL: a typed kind the table does not name falls to the
/// words, and words it does not name to the last row.
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
        // NEGATIVE CONTROLS: a typed kind no row names, words no row names.
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

/// The long context's entitlement as 2.1.300 words it is the account limit
/// the older words were, with its own why: told once, the action the same.
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

/// A multi-day weekly usage limit with no account rotation available raises
/// `notice::LIMIT_SIGN_IN` (`Now`, telling the owner to type `/login` in the
/// tab), while one with an account move or wait advised raises nothing, and
/// a short wait (<= 6 h) raises nothing.
#[test]
fn a_weekly_usage_limit_with_no_account_to_rotate_to_raises_limit_sign_in() {
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.resumes_by_itself = true;
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    let e = exception_for(&st, &weekly, &cfg(), t).expect("told");
    assert_eq!((e.kind, e.when), (ExceptionKind::Login, Urgency::Now));
    assert_eq!(e.notice.as_ref().map(|n| n.id), Some("limit-sign-in"));
    assert!(e.action.contains("/login"), "{e:?}");

    // When an account move or wait is advised, nothing is raised.
    let mut advised = weekly.clone();
    advised.account = Some(crate::supervise::policy::turn_end::AccountAdvice::Move {
        to: "work-2".to_string(),
    });
    assert_eq!(exception_for(&st, &advised, &cfg(), t), None);

    // While the owner is in `/login` at the weekly limit, the picker/browser/success
    // clears the notice, and a cancelled `/login` (`login_interrupted`) tells
    // the limit's own sign-in (below).
    let mut in_browser = weekly.clone();
    in_browser.wall = None;
    in_browser.login_screen = Some(LoginScreen::Browser);
    assert_eq!(exception_for(&st, &in_browser, &cfg(), t), None);
    // Console's own menu is the person's choice there too, nothing stopped;
    // an error in the dialog is (the login-carry review's
    // console-stopped-false, 2026-10-20).
    let in_console = TurnEndReading {
        login_screen: Some(LoginScreen::ConsoleMenu),
        ..in_browser.clone()
    };
    assert_eq!(exception_for(&st, &in_console, &cfg(), t), None);
    let failed = TurnEndReading {
        login_screen: Some(LoginScreen::Failed),
        ..in_browser.clone()
    };
    let e = exception_for(&st, &failed, &cfg(), t).expect("stopped");
    assert_eq!(e.notice.as_ref().map(|n| n.id), Some("login-stopped"));

    // A cancelled `/login` (`login_interrupted`) at the limit is told the
    // limit's own sign-in, to ANOTHER account: a lost login's "sign in
    // again" signs the same account in, whose limit still stands (the review
    // of the login carry, r5h-4).
    let mut interrupted = weekly;
    interrupted.wall = None;
    interrupted.login_interrupted = true;
    let left = exception_for(&st, &interrupted, &cfg(), t).expect("login left");
    assert_eq!(left.notice.as_ref().map(|n| n.id), Some("limit-sign-in"));
    assert!(left.action.contains("another account"), "{left:?}");
    // NEGATIVE CONTROL: a person's `/login` left at a LOST LOGIN is a lost
    // login's, told to sign in again.
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
    assert!(!e.action.contains("another account"), "{e:?}");
}

/// A WEEKLY LIMIT'S NOTICE SAYS WHEN THE TAB GOES ON BY ITSELF (the review
/// of the band, 2026-10-20): under `resume_limits` aterm continues the tab
/// past the reset whatever the owner does, so the notice's expectation names
/// the reset as the vendor's notice said it — whichever step the roster
/// gives. NEGATIVE CONTROLS: a notice that named no clock, and a limit
/// `resume_limits` does not continue past (the owner's own `/login` left at
/// it), tell the expectation they always had.
#[test]
fn a_weekly_limits_notice_names_the_reset_the_tab_goes_on_after() {
    use crate::supervise::limit::ResetClock;
    use crate::supervise::policy::turn_end::{HeldBy, RosterWord, SignInNext};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit \u{b7} resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.reset_clock = ResetClock::of(Some("Oct 23 at 8am (America/Los_Angeles)"));
    weekly.resumes_by_itself = true;
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    let expect = |r: &TurnEndReading, cfg: &SupervisorConfig| {
        let e = exception_for(&st, r, cfg, t).expect("told");
        let n = e.notice.expect("a notice");
        (n.id, n.expect)
    };
    let row = |held: Option<HeldBy>| RosterWord::Row {
        next: Some(SignInNext {
            label: "m8-2".to_string(),
            renew: true,
        }),
        ready: None,
        held,
    };
    let none = RosterWord::Row {
        next: None,
        ready: None,
        held: None,
    };
    for word in [
        None,
        Some(row(None)),
        Some(row(Some(HeldBy::Earlier))),
        Some(none),
        Some(RosterWord::Unread),
    ] {
        let r = TurnEndReading {
            roster: word.clone(),
            ..weekly.clone()
        };
        let (id, said) = expect(&r, &cfg());
        assert!(said.contains("on Oct 23 at 8am"), "{id}: {said:?}");
        // NEGATIVE CONTROL: a notice that named no clock.
        let unclocked = TurnEndReading {
            reset_clock: None,
            ..r.clone()
        };
        let (_, plain) = expect(&unclocked, &cfg());
        assert!(!plain.contains("Oct 23"), "{id}: {plain:?}");
        assert_ne!(plain, said, "{id}");
    }
    // NEGATIVE CONTROL: the person's own `/login` left at the limit, told
    // the roster's step: with `resume_limits` off nothing continues the tab
    // past the reset, and the notice says none.
    let off = SupervisorConfig {
        resume_limits: false,
        ..cfg()
    };
    let left = TurnEndReading {
        login_interrupted: true,
        roster: Some(RosterWord::Row {
            next: None,
            ready: None,
            held: None,
        }),
        ..weekly.clone()
    };
    let (id, said) = expect(&left, &cfg());
    assert!(said.contains("on Oct 23 at 8am"), "{id}: {said:?}");
    let (id, said) = expect(&left, &off);
    assert!(!said.contains("Oct 23"), "{id}: {said:?}");
}

/// A WEEKLY LIMIT ON A ROSTER ROW'S LOGIN NEVER ASKS FOR `/login` IN THE TAB
/// (the review's acct-1, acct-7): that would sign another account into the
/// row's own login, and every session on it would follow. The roster's word
/// chooses the step: another row ready to run now, by `accounts use` (acct3-5:
/// the roster report said no listed account could take over while one could),
/// another row to sign in by name (renewal for an email row, `accounts login`
/// without), another account added where no row is ready or left to sign in
/// (r8c-3), and the roster report where the roster or the state was unread,
/// which says why — a person's own
/// `/login` left at the limit included. A multi-day WAIT the
/// roster advises is told the sign-in by name too, where one stands (acct-4:
/// the tab waited silently for days). NEGATIVE CONTROLS: no roster word and
/// an unlisted login are `/login` in the tab as before; a multi-day wait with
/// no row to sign in, and a short one, tell nothing.
#[test]
fn a_weekly_limit_on_a_roster_row_names_the_rosters_step_never_login_in_the_tab() {
    use crate::supervise::policy::turn_end::{
        AccountAdvice, HeldBy, Ready, RosterWord, SignInNext,
    };
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.resumes_by_itself = true;
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    let with = |word: Option<RosterWord>| TurnEndReading {
        roster: word,
        ..weekly.clone()
    };
    let next = |renew: bool| RosterWord::Row {
        next: Some(SignInNext {
            label: "m8-2".to_string(),
            renew,
        }),
        ready: None,
        held: None,
    };
    let none = RosterWord::Row {
        next: None,
        ready: None,
        held: None,
    };
    let ready = |next: Option<SignInNext>| RosterWord::Row {
        next,
        ready: Some(Ready {
            label: "m8-3".to_string(),
        }),
        held: Some(HeldBy::Earlier),
    };
    for (word, id, step) in [
        (ready(None), "limit-use", "aterm harness accounts use m8-3"),
        (
            ready(Some(SignInNext {
                label: "m8-2".to_string(),
                renew: true,
            })),
            "limit-use",
            "aterm harness accounts use m8-3",
        ),
        (
            next(true),
            "limit-renew",
            "aterm harness accounts renew m8-2",
        ),
        (
            next(false),
            "limit-login",
            "aterm harness accounts login m8-2",
        ),
        (
            none.clone(),
            "limit-roster",
            "aterm harness accounts add <label>",
        ),
        (RosterWord::Unread, "limit-unread", "aterm harness accounts"),
    ] {
        let r = with(Some(word.clone()));
        let e = exception_for(&st, &r, &cfg(), t).expect("told");
        assert_eq!((e.kind, e.when), (ExceptionKind::Login, Urgency::Now));
        assert_eq!(e.notice.as_ref().map(|n| n.id), Some(id), "{word:?}");
        assert!(e.action.ends_with(step), "{word:?}: {e:?}");
        assert!(!e.action.contains("/login"), "{word:?}: {e:?}");
        // A person's own `/login` left there: the roster's step, not `/login`.
        let left = TurnEndReading {
            wall: None,
            login_interrupted: true,
            ..r.clone()
        };
        let e = exception_for(&st, &left, &cfg(), t).expect("told");
        assert_eq!(e.notice.as_ref().map(|n| n.id), Some(id), "{word:?}");
    }
    // A multi-day wait with a row to sign in: that row's step.
    let far = |word: Option<RosterWord>, days: u32| TurnEndReading {
        account: Some(AccountAdvice::Wait {
            until: t + days * 24 * 60 * MIN,
        }),
        ..with(word)
    };
    let e = exception_for(&st, &far(Some(next(true)), 6), &cfg(), t).expect("told");
    assert_eq!(e.notice.as_ref().map(|n| n.id), Some("limit-renew"));
    // NEGATIVE CONTROLS.
    for word in [None, Some(RosterWord::Unlisted)] {
        let e = exception_for(&st, &with(word.clone()), &cfg(), t).expect("told");
        assert_eq!(e.notice.as_ref().map(|n| n.id), Some("limit-sign-in"));
        assert!(e.action.contains("/login"), "{word:?}: {e:?}");
    }
    assert_eq!(exception_for(&st, &far(Some(none), 6), &cfg(), t), None);
    assert_eq!(exception_for(&st, &far(None, 6), &cfg(), t), None);
    assert_eq!(
        exception_for(&st, &far(Some(next(true)), 0), &cfg(), t),
        None,
        "a wait under six hours tells nothing"
    );
}

/// A SWITCH IS PROMISED ONLY WHERE ONE CAN FOLLOW (the review of the login
/// carry, r3h-2: under `accounts = "off"` the owner ran the `accounts use`
/// the notice asked for, which only pinned the account; the tab never moved,
/// and the notice asked again). Where the owner's aterm.toml keeps aterm from
/// switching the tab — `accounts = "off"`, `relaunch = false` as a window's
/// host reads it (the loop's config always says it on there, and the host
/// relaunches nothing: r4h-1), or `resume_limits = false` (at a person's
/// `/login` left at the limit, where the notice stands with it off) — a
/// roster row's limit with another row ready, or to sign in, is told the
/// line that lets the tab switch, naming that row: never `accounts use`, nor
/// a sign-in whose expectation is a switch. Where nothing here relaunches
/// the tab whatever aterm.toml says, the roster report. NEGATIVE CONTROLS:
/// the default switches tell `accounts use` and the sign-in by name as
/// before; a row with nothing to switch to is the roster report under every
/// switch.
#[test]
fn a_weekly_limit_promises_a_switch_only_where_one_can_follow() {
    use crate::harness::accounts::Mode;
    use crate::supervise::policy::turn_end::{HeldBy, Ready, RosterWord, SignInNext};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.resumes_by_itself = true;
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    let told = |r: &TurnEndReading, cfg: &SupervisorConfig| {
        let e = exception_for(&st, r, cfg, t).expect("told");
        let n = e.notice.expect("a notice");
        (n.id, e.action, n.did)
    };
    let ready = RosterWord::Row {
        next: None,
        ready: Some(Ready {
            label: "m8-3".to_string(),
        }),
        held: Some(HeldBy::Pin),
    };
    let next = RosterWord::Row {
        next: Some(SignInNext {
            label: "m8-2".to_string(),
            renew: true,
        }),
        ready: None,
        held: None,
    };
    // What a window's host reads where the owner's `relaunch = false` holds
    // the tab: its config's `relaunch` on, no restart, the switch off.
    let relaunch_off = |r: &TurnEndReading| TurnEndReading {
        restartable: false,
        relaunch_off: true,
        ..r.clone()
    };
    // The ready row is one the roster holds the tab off (the owner's pin):
    // with the lines set, the next step is named rather than a switch by
    // itself promised (r4h-4).
    // A row still to sign in: with the lines set its sign-in is the next
    // step, not a switch (r5h-2).
    for (word, row, promised, stays) in [
        (ready, "m8-3", "limit-use", "limit-stay-held"),
        (next, "m8-2", "limit-renew", "limit-stay-held"),
    ] {
        let at_wall = TurnEndReading {
            roster: Some(word.clone()),
            ..weekly.clone()
        };
        // A person's own `/login` left at the limit: told whatever
        // `resume_limits` says.
        let left = TurnEndReading {
            wall: None,
            login_interrupted: true,
            ..at_wall.clone()
        };
        let accounts_off = SupervisorConfig {
            accounts: Mode::Off,
            ..cfg()
        };
        let resume_off = SupervisorConfig {
            resume_limits: false,
            ..cfg()
        };
        for (r, cfg, line) in [
            (&at_wall, &accounts_off, notice::STAY_WHYS[0]),
            (&relaunch_off(&at_wall), &cfg(), notice::STAY_WHYS[1]),
            (&left, &resume_off, notice::STAY_WHYS[3]),
        ] {
            let (id, action, did) = told(r, cfg);
            assert_eq!(id, stays, "{word:?}, {line}");
            assert!(action.contains(line), "{word:?}: {action:?}");
            assert!(did.contains(row), "{word:?}: {did:?}");
        }
        // Nothing here relaunches the tab: the roster report, whatever
        // aterm.toml says.
        let unrestartable = TurnEndReading {
            restartable: false,
            ..at_wall.clone()
        };
        assert_eq!(told(&unrestartable, &cfg()).0, "limit-roster", "{word:?}");
        assert_eq!(
            told(
                &unrestartable,
                &SupervisorConfig {
                    accounts: Mode::Off,
                    ..cfg()
                }
            )
            .0,
            "limit-roster",
            "{word:?}"
        );
        // NEGATIVE CONTROLS: a switch can follow.
        assert_eq!(told(&at_wall, &cfg()).0, promised, "{word:?}");
        assert_eq!(told(&left, &cfg()).0, promised, "{word:?}");
    }
    // NEGATIVE CONTROL: nothing to switch to is the roster report, held or
    // not.
    let none = TurnEndReading {
        roster: Some(RosterWord::Row {
            next: None,
            ready: None,
            held: None,
        }),
        ..weekly.clone()
    };
    let accounts_off = SupervisorConfig {
        accounts: Mode::Off,
        ..cfg()
    };
    for (r, cfg) in [
        (none.clone(), cfg()),
        (none.clone(), accounts_off),
        (relaunch_off(&none), cfg()),
    ] {
        assert_eq!(told(&r, &cfg).0, "limit-roster");
    }
}

/// EVERY SETTING THAT HOLDS THE TAB IS NAMED, together (the review of the
/// login carry, r4h-2: named the first alone, the owner set it, and the next
/// notice named another — the first notice's promise false): with two or
/// three of `accounts = "off"`, `relaunch = false` and `resume_limits =
/// false` holding a roster row's limit beside a ready row, the step names
/// the line for each, and a clause the notice is raised with
/// ([`NoticeText::whys`]). NEGATIVE CONTROL: one setting off names its line
/// and no other.
#[test]
fn every_setting_that_holds_a_limits_tab_is_named() {
    use crate::harness::accounts::Mode;
    use crate::supervise::policy::turn_end::{Ready, RosterWord};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    // A person's own `/login` left at the limit: the one point told with
    // `resume_limits` off.
    let left = TurnEndReading {
        wall: None,
        login_interrupted: true,
        roster: Some(RosterWord::Row {
            next: None,
            ready: Some(Ready {
                label: "m8-3".to_string(),
            }),
            held: None,
        }),
        ..weekly.clone()
    };
    let lines = [
        "accounts = \"wall\"",
        "relaunch = true",
        "resume_limits = true",
    ];
    let mut told = BTreeSet::new();
    for bits in 1..8_u8 {
        let off = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0];
        let cfg = SupervisorConfig {
            accounts: if off[0] { Mode::Off } else { Mode::Early },
            resume_limits: !off[2],
            ..cfg()
        };
        let r = TurnEndReading {
            restartable: !off[1],
            relaunch_off: off[1],
            ..left.clone()
        };
        let e = exception_for(&st, &r, &cfg, t).expect("told");
        assert_eq!(
            e.notice.as_ref().map(|n| n.id),
            Some("limit-stay"),
            "{off:?}"
        );
        for (line, held) in lines.iter().zip(off) {
            assert_eq!(e.action.contains(line), held, "{off:?}: {}", e.action);
        }
        assert!(
            notice::STAY_WHYS.iter().any(|why| e.action.contains(why)),
            "{off:?}: {}",
            e.action
        );
        told.insert(e.action);
    }
    assert_eq!(told.len(), 7, "each combination in its own words: {told:?}");
}

/// THE ROSTER'S REPORTS PROMISE NO CONTINUATION WHERE aterm MAKES NONE (the
/// review of the login carry, r4h-3): at a person's `/login` left at a
/// weekly limit with `resume_limits = false`, nothing continues the tab past
/// its reset — no relaunch, no `keep going` — so a roster row's limit with
/// no row to take over, or a roster aterm could not read, is told an
/// expectation that says aterm does not continue the tab, whatever else
/// holds it (the reset never named: the row is drawn over). NEGATIVE
/// CONTROL: with `resume_limits` on, the reports say the session continues
/// by itself, as before.
#[test]
fn a_limits_report_promises_no_continuation_where_aterm_makes_none() {
    use crate::harness::accounts::Mode;
    use crate::supervise::policy::turn_end::RosterWord;
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    let none = RosterWord::Row {
        next: None,
        ready: None,
        held: None,
    };
    for (word, open, kept) in [
        (none, &notice::LIMIT_ROSTER, &notice::LIMIT_ROSTER_KEPT),
        (
            RosterWord::Unread,
            &notice::LIMIT_UNREAD,
            &notice::LIMIT_UNREAD_KEPT,
        ),
    ] {
        let left = TurnEndReading {
            wall: None,
            login_interrupted: true,
            roster: Some(word.clone()),
            ..weekly.clone()
        };
        let unrestartable = TurnEndReading {
            restartable: false,
            ..left.clone()
        };
        let resume = |on: bool, accounts: Mode| SupervisorConfig {
            resume_limits: on,
            accounts,
            ..cfg()
        };
        for (r, cfg) in [
            (&left, resume(false, Mode::Early)),
            (&left, resume(false, Mode::Off)),
            (&unrestartable, resume(false, Mode::Early)),
        ] {
            let e = exception_for(&st, r, &cfg, t).expect("told");
            let n = e.notice.expect("a notice");
            assert_eq!(n.id, kept.id, "{word:?}");
            assert_eq!(n.expect, kept.expect, "{word:?}");
            assert_ne!(n.expect, open.expect, "{word:?}");
        }
        // NEGATIVE CONTROL: aterm continues the tab past its reset.
        let e = exception_for(&st, &left, &resume(true, Mode::Early), t).expect("told");
        let n = e.notice.expect("a notice");
        assert_eq!(
            (n.id, n.expect.as_str()),
            (open.id, open.expect),
            "{word:?}"
        );
    }
}

/// WHERE NO OTHER ACCOUNT CAN TAKE OVER, THE STEP ADDS ONE (the review of the
/// login carry, r8c-3: the step was the roster report, which only says that
/// no account is next — the band carried neither something to do nor what to
/// expect): a roster row's weekly limit with no other row ready or left to
/// sign in, whatever holds the tab and whether aterm continues it past the
/// reset or not, is told the command that lists another account, and, where
/// aterm continues the tab, the reset it continues after. NEGATIVE CONTROL:
/// where aterm cannot tell which account is next (the roster refused, the
/// state unread), the step stays the report, which says why.
#[test]
fn where_no_other_account_can_take_over_the_step_adds_one() {
    use crate::harness::cli::{AccountsCmd, Cmd, parse};
    use crate::supervise::policy::turn_end::{HeldBy, RosterWord};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.resumes_by_itself = true;
    weekly.reset_clock = crate::supervise::limit::ResetClock::of(Some("Oct 23 at 8am"));
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    // The command the step runs, as `aterm harness` parses it.
    let command = |e: &Exception| -> Cmd {
        let n = e.notice.as_ref().expect("a notice");
        let step = n.command.as_deref().unwrap_or(&e.action);
        let at = step.find("aterm harness ").expect("a command");
        let args: Vec<String> = step[at + "aterm harness ".len()..]
            .split_whitespace()
            .map(str::to_string)
            .collect();
        parse(&args).expect("a command aterm harness takes").0
    };
    let left = TurnEndReading {
        wall: None,
        login_interrupted: true,
        ..weekly.clone()
    };
    let kept = SupervisorConfig {
        resume_limits: false,
        ..cfg()
    };
    let none = |held: Option<HeldBy>| RosterWord::Row {
        next: None,
        ready: None,
        held,
    };
    for (word, adds) in [
        (none(None), true),
        (none(Some(HeldBy::Earlier)), true),
        (RosterWord::Unread, false),
    ] {
        for (shown, under, resumes) in [(&weekly, cfg(), true), (&left, kept.clone(), false)] {
            let r = TurnEndReading {
                roster: Some(word.clone()),
                ..shown.clone()
            };
            let e = exception_for(&st, &r, &under, t).expect("told");
            let cmd = command(&e);
            assert_eq!(
                matches!(cmd, Cmd::Accounts(AccountsCmd::Add { .. })),
                adds,
                "{word:?}: {e:?}"
            );
            assert_eq!(
                matches!(cmd, Cmd::Accounts(AccountsCmd::Report { .. })),
                !adds,
                "{word:?}: {e:?}"
            );
            // When the tab goes on whatever the owner does.
            let n = e.notice.expect("a notice");
            assert_eq!(n.expect.contains("Oct 23"), resumes, "{word:?}: {n:?}");
        }
    }
}

/// A SWITCH BY ITSELF IS PROMISED ONLY WHERE THE ROSTER MAKES ONE (the review
/// of the login carry, r4h-4): where the roster's own advice holds the tab at
/// its limit even once another row is ready (`held`: a limit from before
/// this Claude Code under `accounts = "wall"`, the owner's pin, a limit
/// reset kept), a sign-in by name and the line that lets the tab switch are
/// told with an expectation that names the next step — the step and what
/// aterm did unchanged. NEGATIVE CONTROL: nothing holding the tab, the same
/// steps promise the switch by itself. (The line that lets the tab switch:
/// [`the_lines_that_let_a_tab_switch_promise_one_only_beside_a_ready_row`].)
#[test]
fn a_switch_by_itself_is_promised_only_where_the_roster_makes_one() {
    use crate::supervise::policy::turn_end::{HeldBy, RosterWord, SignInNext};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    let next = |renew: bool, held: Option<HeldBy>| RosterWord::Row {
        next: Some(SignInNext {
            label: "m8-2".to_string(),
            renew,
        }),
        ready: None,
        held,
    };
    for (renew, free, held_text) in [
        (true, &notice::LIMIT_RENEW, &notice::LIMIT_RENEW_HELD),
        (false, &notice::LIMIT_LOGIN, &notice::LIMIT_LOGIN_HELD),
    ] {
        let mut steps = BTreeSet::new();
        for held in [HeldBy::Earlier, HeldBy::Pin, HeldBy::Reset] {
            let r = TurnEndReading {
                roster: Some(next(renew, Some(held))),
                ..weekly.clone()
            };
            let e = exception_for(&st, &r, &cfg(), t).expect("told");
            let n = e.notice.expect("a notice");
            assert_eq!(n.id, held_text.id, "{held:?}");
            assert_eq!(n.expect, held_text.fill(Some("m8-2"), None).expect);
            assert_ne!(n.expect, free.fill(Some("m8-2"), None).expect);
            steps.insert((e.action, n.did));
        }
        // NEGATIVE CONTROL: nothing holds the tab.
        let r = TurnEndReading {
            roster: Some(next(renew, None)),
            ..weekly.clone()
        };
        let e = exception_for(&st, &r, &cfg(), t).expect("told");
        let n = e.notice.expect("a notice");
        assert_eq!(n.id, free.id);
        steps.insert((e.action, n.did));
        assert_eq!(
            steps.len(),
            1,
            "{}: the step and the did: {steps:?}",
            free.id
        );
    }
}

/// THE LINES THAT LET A TAB SWITCH PROMISE A SWITCH ONLY BESIDE A READY ROW
/// NOTHING HOLDS THE TAB OFF (the review of the login carry, r5h-2): under
/// `accounts = "off"`, with the only other row still to sign in, the owner
/// read "Set accounts = \"wall\"." and "aterm then switches the tab…" — set,
/// the next notice was that row's sign-in, no switch. Such a row, and a
/// ready one the roster holds the tab off, are told the lines with the next
/// step named (`limit-stay-held`), the reset known or not, and no
/// expectation of theirs says the tab switches. NEGATIVE CONTROL: a ready
/// row nothing holds the tab off is told the lines and the switch that
/// follows them (`limit-stay`).
#[test]
fn the_lines_that_let_a_tab_switch_promise_one_only_beside_a_ready_row() {
    use crate::harness::accounts::Mode;
    use crate::supervise::limit::ResetClock;
    use crate::supervise::policy::turn_end::{HeldBy, Ready, RosterWord, SignInNext};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.resumes_by_itself = true;
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    let off = SupervisorConfig {
        accounts: Mode::Off,
        ..cfg()
    };
    let told = |word: RosterWord, clock: bool| {
        let r = TurnEndReading {
            roster: Some(word),
            reset_clock: if clock {
                ResetClock::of(Some("Oct 23 at 8am (America/Los_Angeles)"))
            } else {
                None
            },
            ..weekly.clone()
        };
        let e = exception_for(&st, &r, &off, t).expect("told");
        assert!(e.action.contains(notice::STAY_WHYS[0]), "{e:?}");
        let n = e.notice.expect("a notice");
        (n.id, n.expect)
    };
    let next = |held: Option<HeldBy>| RosterWord::Row {
        next: Some(SignInNext {
            label: "m8-2".to_string(),
            renew: true,
        }),
        ready: None,
        held,
    };
    let ready = |held: Option<HeldBy>| RosterWord::Row {
        next: None,
        ready: Some(Ready {
            label: "m8-3".to_string(),
        }),
        held,
    };
    for clock in [true, false] {
        for word in [
            next(None),
            next(Some(HeldBy::Earlier)),
            ready(Some(HeldBy::Pin)),
        ] {
            let (id, expect) = told(word.clone(), clock);
            assert_eq!(id, notice::LIMIT_STAY_HELD.id, "{word:?} {clock}");
            assert!(!expect.contains("switches"), "{word:?}: {expect:?}");
            assert_eq!(expect.contains("Oct 23"), clock, "{expect:?}");
        }
        // NEGATIVE CONTROL: a ready row nothing holds the tab off.
        let (id, expect) = told(ready(None), clock);
        assert_eq!(id, notice::LIMIT_STAY.id, "{clock}");
        assert!(expect.contains("switches"), "{expect:?}");
    }
}

/// A READY ROW NOTHING HOLDS THE TAB OFF IS THE ROSTER'S OWN MOVE (the review
/// of the login carry, r5h-3): its `held` is `None` where the roster would
/// move the tab itself, and where the point was not asked about — an upgrade
/// holding the point — the owner was told "this Claude Code started after
/// the limit" (false: this process hit it) and the pin of every session that
/// the roster's own move makes needless. Told nothing there. A DRAFT in the
/// prompt is no such moment (r6h-1): while it stands the move is never asked,
/// and the tab waited silently for the reset, days away — told the draft's
/// notice, naming the ready row, for a person's draft, one over the
/// completion menu and aterm's own words alike; and its step sends only the
/// person's draft as it stands — aterm's own words are cleared, and a draft
/// under the menu, whose Enter runs the menu's entry, is reached by closing
/// the menu (r7h-4). A relaunch onto the row the host refused in a way a
/// second ask can pass (the latch) is the rotation failed, and named as such,
/// never as the limit from before this Claude Code. NEGATIVE CONTROLS: the
/// upgrade holding the point tells nothing, a draft or not; an empty prompt
/// read before the advice is the roster's own move and tells nothing either
/// (r7h-7); and a row the roster holds the tab off (the limit from before
/// this Claude Code) is still told `accounts use` naming that cause, over a
/// draft too.
#[test]
fn a_ready_row_nothing_holds_the_tab_off_is_the_rosters_own_move() {
    use crate::supervise::policy::turn_end::{HeldBy, Ready, RosterWord};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.resumes_by_itself = true;
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    let row = |held: Option<HeldBy>| RosterWord::Row {
        next: None,
        ready: Some(Ready {
            label: "m8-3".to_string(),
        }),
        held,
    };
    let draft = TurnEndReading {
        roster: Some(row(None)),
        composer: Composer::Typed,
        ..weekly.clone()
    };
    for (composer, text) in [
        (Composer::Typed, &notice::LIMIT_DRAFT),
        (Composer::Diverted, &notice::LIMIT_DRAFT_MENU),
        (Composer::Own, &notice::LIMIT_DRAFT_OWN),
    ] {
        let r = TurnEndReading {
            composer: composer.clone(),
            ..draft.clone()
        };
        let e = exception_for(&st, &r, &cfg(), t).expect("told");
        assert_eq!((e.kind, e.when), (ExceptionKind::Login, Urgency::Now));
        let n = e.notice.clone().expect("a notice");
        assert_eq!(n.id, text.id, "{composer:?}");
        assert!(n.did.contains("m8-3"), "{composer:?}: {:?}", n.did);
        assert!(n.expect.contains("switches"), "{:?}", n.expect);
        assert!(!e.action.contains("accounts use"), "{e:?}");
        match composer {
            Composer::Typed => assert!(e.action.starts_with("Send "), "{e:?}"),
            Composer::Diverted => {
                assert!(e.action.starts_with("Close "), "{e:?}");
                assert!(e.action.contains("menu"), "{e:?}");
            }
            Composer::Own => {
                assert!(e.action.starts_with("Clear aterm's "), "{e:?}");
                assert!(!e.action.contains("send"), "{e:?}");
            }
            _ => unreachable!("the three admitted draft states above"),
        }
        assert_eq!(aterm_messages::need::Step::new(&e.action).fault(), None);
        assert!(e.action.chars().count() <= aterm_messages::need::STEP_CAP);
        // The person's draft alone is sent as it stands.
        assert_eq!(
            e.action == notice::LIMIT_DRAFT.todo,
            composer == Composer::Typed,
            "{composer:?}: {e:?}"
        );
    }
    // NEGATIVE CONTROL: an upgrade holding the point asks nothing, and is no
    // draft's to answer.
    let upgrading = TurnEndReading {
        roster: Some(row(None)),
        upgrading: true,
        ..weekly.clone()
    };
    let upgrading_drafted = TurnEndReading {
        composer: Composer::Typed,
        ..upgrading.clone()
    };
    for r in [&upgrading, &upgrading_drafted] {
        assert_eq!(exception_for(&st, r, &cfg(), t), None, "{r:?}");
    }
    // NEGATIVE CONTROL: an empty prompt read before the advice.
    let empty = TurnEndReading {
        roster: Some(row(None)),
        ..weekly.clone()
    };
    assert_eq!(empty.composer, Composer::Empty);
    assert_eq!(exception_for(&st, &empty, &cfg(), t), None);
    // The host refused the relaunch onto the row in a way a second ask can
    // pass: the rotation failed, and the notice says so.
    let mut refused = st.clone();
    refused.seed_account_unmade(Unmade::Again, None);
    assert!(refused.account_refused());
    let e = exception_for(&refused, &draft, &cfg(), t).expect("told");
    let n = e.notice.expect("a notice");
    assert_eq!(n.id, notice::LIMIT_USE.id);
    assert!(n.did.contains(notice::WHY_USE_UNMADE), "{:?}", n.did);
    assert!(!n.did.contains(notice::WHY_USE_EARLIER), "{:?}", n.did);
    // NEGATIVE CONTROL: the roster holds the tab off the row.
    let held = TurnEndReading {
        roster: Some(row(Some(HeldBy::Earlier))),
        ..draft.clone()
    };
    let e = exception_for(&st, &held, &cfg(), t).expect("told");
    let n = e.notice.expect("a notice");
    assert_eq!(n.id, notice::LIMIT_USE.id);
    assert!(n.did.contains(notice::WHY_USE_EARLIER), "{:?}", n.did);
}

/// A RESTART REFUSED FOR THIS LAUNCH IS NEVER TOLD AS A MOVE `accounts use`
/// MAKES (the review of the login carry, r6h-2): the host refused the
/// relaunch onto the ready row with a word of this launch's — the tab's own
/// program is Claude Code (`refused:not-a-shell-job`), any other `refused:`
/// or `failed:` — so asked again it is refused the same way: `accounts use`
/// would pin every other session and leave this tab, and its "this tab
/// included" was false. Told the limit's notice of a restart this launch
/// cannot be given, naming the cause, with no `accounts use` and no switch
/// of this launch promised — whatever the roster says holds the tab, and the
/// reset named where it is known. NEGATIVE CONTROL: a refusal a second ask
/// can pass (`busy:…`, no word) is still `accounts use` naming the failed
/// restart.
#[test]
fn a_restart_refused_for_this_launch_is_never_told_as_a_move_accounts_use_makes() {
    use crate::supervise::limit::ResetClock;
    use crate::supervise::policy::turn_end::{AccountAt, HeldBy, Ready, Restart, RosterWord};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.reset_clock = ResetClock::of(Some("Oct 23 at 8am (America/Los_Angeles)"));
    weekly.resumes_by_itself = true;
    let why = Restart::Account {
        to: "m8-3".to_string(),
        at: AccountAt::Wall,
    };
    let told = |step: Option<&str>, held: Option<HeldBy>| {
        let mut st = TurnEndState::default();
        st.observe(&weekly, t);
        st.restart_unmade(&why, step, &weekly, t);
        assert!(st.account_refused(), "{step:?}");
        let r = TurnEndReading {
            roster: Some(RosterWord::Row {
                next: None,
                ready: Some(Ready {
                    label: "m8-3".to_string(),
                }),
                held,
            }),
            ..weekly.clone()
        };
        let e = exception_for(&st, &r, &cfg(), t).expect("told");
        let n = e.notice.clone().expect("a notice");
        (e, n)
    };
    for (step, cause) in [
        ("refused:not-a-shell-job", notice::WHY_UNMADE_NO_SHELL),
        ("refused:line", notice::WHY_USE_UNMADE),
        ("failed:account-not-applied", notice::WHY_USE_UNMADE),
    ] {
        for held in [None, Some(HeldBy::Earlier)] {
            let (e, n) = told(Some(step), held);
            assert_eq!(n.id, notice::LIMIT_UNMADE.id, "{step} {held:?}");
            assert!(n.did.contains(cause) && n.did.contains("m8-3"), "{n:?}");
            assert!(!e.action.contains("accounts use"), "{step}: {e:?}");
            assert!(!n.expect.contains("this tab included"), "{n:?}");
            assert!(n.expect.contains("on Oct 23 at 8am"), "{n:?}");
            assert_eq!(n.undo, None, "no pin to end: {n:?}");
        }
    }
    // NEGATIVE CONTROL: a refusal a second ask can pass.
    for step in [Some("busy:x"), None] {
        let (e, n) = told(step, None);
        assert_eq!(n.id, notice::LIMIT_USE.id, "{step:?}");
        assert!(n.did.contains(notice::WHY_USE_UNMADE), "{n:?}");
        assert!(e.action.ends_with("accounts use m8-3"), "{e:?}");
    }
}

/// A RESTART REFUSED FOR THIS LAUNCH PROMISES NO SWITCH AFTER ANY STEP (the
/// review of the login carry, r7h-2): with no row ready and one still to
/// sign in, that sign-in is the accounts' news, which ends the latch; the
/// move is asked, refused again with the same word, and the limit's notice
/// of this launch follows — so the sign-in's notice names the next step,
/// never a switch to the row. So too the lines of aterm.toml beside a ready
/// row. NEGATIVE CONTROLS: with no latch, or one a second ask can pass, the
/// sign-in and the lines promise the switch as before.
#[test]
fn a_restart_refused_for_this_launch_promises_no_switch_after_any_step() {
    use crate::harness::accounts::Mode;
    use crate::supervise::limit::ResetClock;
    use crate::supervise::policy::turn_end::{AccountAt, Ready, Restart, RosterWord, SignInNext};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.reset_clock = ResetClock::of(Some("Oct 23 at 8am (America/Los_Angeles)"));
    weekly.resumes_by_itself = true;
    let why = Restart::Account {
        to: "m8-3".to_string(),
        at: AccountAt::Wall,
    };
    // The state after the host's word for the move (`None`: no move asked).
    let state = |word: Option<Option<&str>>| {
        let mut st = TurnEndState::default();
        st.observe(&weekly, t);
        if let Some(step) = word {
            st.restart_unmade(&why, step, &weekly, t);
            assert!(st.account_refused(), "{step:?}");
        }
        st
    };
    let next = |renew: bool| RosterWord::Row {
        next: Some(SignInNext {
            label: "m8-2".to_string(),
            renew,
        }),
        ready: None,
        held: None,
    };
    let ready = RosterWord::Row {
        next: None,
        ready: Some(Ready {
            label: "m8-3".to_string(),
        }),
        held: None,
    };
    let off = SupervisorConfig {
        accounts: Mode::Off,
        ..cfg()
    };
    let told = |st: &TurnEndState, word: &RosterWord, cfg: &SupervisorConfig| {
        let r = TurnEndReading {
            roster: Some(word.clone()),
            ..weekly.clone()
        };
        let e = exception_for(st, &r, cfg, t).expect("told");
        e.notice.expect("a notice").id
    };
    for step in [
        "refused:not-a-shell-job",
        "refused:line",
        "failed:account-not-applied",
    ] {
        let st = state(Some(Some(step)));
        assert_eq!(
            told(&st, &next(true), &cfg()),
            notice::LIMIT_RENEW_HELD.id,
            "{step}"
        );
        assert_eq!(
            told(&st, &next(false), &cfg()),
            notice::LIMIT_LOGIN_HELD.id,
            "{step}"
        );
        assert_eq!(
            told(&st, &ready, &off),
            notice::LIMIT_STAY_HELD.id,
            "{step}"
        );
    }
    // NEGATIVE CONTROLS: no latch, and a refusal a second ask can pass.
    for word in [None, Some(Some("busy:x")), Some(None)] {
        let st = state(word);
        assert_eq!(
            told(&st, &next(true), &cfg()),
            notice::LIMIT_RENEW.id,
            "{word:?}"
        );
        assert_eq!(
            told(&st, &next(false), &cfg()),
            notice::LIMIT_LOGIN.id,
            "{word:?}"
        );
        assert_eq!(told(&st, &ready, &off), notice::LIMIT_STAY.id, "{word:?}");
    }
}

/// A `/login` LEFT AT A USAGE LIMIT NAMES THE RESET THE TAB GOES ON AFTER
/// (the review of the login carry, r6h-3): its limit's row drawn over, the
/// reading names no reset, but the limit's track still waits on the one its
/// notice named, and aterm continues the tab after it whether or not the
/// owner signs in — told so, at a weekly and at a session limit alike.
/// NEGATIVE CONTROLS: with `resume_limits` off aterm continues nothing and
/// the notice names no reset; and a reset already past names none.
#[test]
fn a_login_left_at_a_usage_limit_names_the_reset_the_tab_goes_on_after() {
    use crate::supervise::limit::ResetClock;
    let t = t0();
    for (kind, message, ahead) in [
        (
            WallKind::UsageWeekly,
            "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
            6 * 24 * 60 * MIN,
        ),
        (
            WallKind::UsageSession,
            "You've hit your session limit · resets 8am (America/Los_Angeles)",
            3 * 60 * MIN,
        ),
    ] {
        let mut wall = walled(kind, message);
        wall.reset_at = Some(t + ahead);
        wall.reset_clock = ResetClock::of(Some(if kind == WallKind::UsageWeekly {
            "Oct 23 at 8am (America/Los_Angeles)"
        } else {
            "8am (America/Los_Angeles)"
        }));
        wall.resumes_by_itself = true;
        let mut st = TurnEndState::default();
        st.observe(&wall, t);
        // The person's `/login`, left: its row drawn over the limit's.
        let left = TurnEndReading {
            wall: None,
            wall_message: String::new(),
            reset_at: None,
            reset_clock: None,
            login_interrupted: true,
            ..wall.clone()
        };
        let at = t + MIN;
        let e = exception_for(&st, &left, &cfg(), at).expect("told");
        let n = e.notice.expect("a notice");
        assert_eq!(n.id, notice::LIMIT_SIGN_IN.id, "{kind:?}");
        assert!(n.expect.contains("at 8am"), "{kind:?}: {:?}", n.expect);
        // NEGATIVE CONTROL: aterm continues nothing past the reset.
        let off = SupervisorConfig {
            resume_limits: false,
            ..cfg()
        };
        let e = exception_for(&st, &left, &off, at).expect("told");
        let n = e.notice.expect("a notice");
        assert!(!n.expect.contains("8am"), "{kind:?}: {:?}", n.expect);
        // NEGATIVE CONTROL: a reset already past.
        let late = t + ahead + MIN;
        if let Some(e) = exception_for(&st, &left, &cfg(), late) {
            let n = e.notice.expect("a notice");
            assert!(!n.expect.contains("8am"), "{kind:?}: {:?}", n.expect);
        }
    }
}

/// A ROW READY NOW THAT THE TAB WAS NOT MOVED TO IS TOLD WITH WHAT HELD THE
/// TAB (the review of the band, ox4-5: "aterm does not switch this tab by
/// itself" beside "ready to take over now" read as a fault, and named no
/// cause): `accounts use`'s notice says, in what aterm did, which of the
/// roster's reasons kept the tab where it is — the limit from before this
/// Claude Code started, the owner's pin, a limit reset kept — each in words
/// of its own, beside the row. NEGATIVE CONTROL: the step is the same
/// whatever the cause, and so is what follows it.
#[test]
fn a_ready_row_the_tab_stayed_off_says_what_held_it() {
    use crate::supervise::policy::turn_end::{HeldBy, Ready, RosterWord};
    let t = t0();
    let mut weekly = walled(
        WallKind::UsageWeekly,
        "You've hit your weekly limit · resets Oct 23 at 8am (America/Los_Angeles)",
    );
    weekly.reset_at = Some(t + 6 * 24 * 60 * MIN);
    weekly.resumes_by_itself = true;
    let mut st = TurnEndState::default();
    st.observe(&weekly, t);
    let mut dids = std::collections::BTreeSet::new();
    let mut steps = std::collections::BTreeSet::new();
    for held in [HeldBy::Earlier, HeldBy::Pin, HeldBy::Reset] {
        let r = TurnEndReading {
            roster: Some(RosterWord::Row {
                next: None,
                ready: Some(Ready {
                    label: "m8-3".to_string(),
                }),
                held: Some(held),
            }),
            ..weekly.clone()
        };
        let e = exception_for(&st, &r, &cfg(), t).expect("told");
        let n = e.notice.expect("a notice");
        assert_eq!(n.id, "limit-use", "{held:?}");
        assert!(
            n.did.contains(held.why()) && n.did.contains("m8-3"),
            "{held:?}: {:?}",
            n.did
        );
        assert!(notice::USE_WHYS.contains(&held.why()), "{held:?}");
        dids.insert(n.did);
        steps.insert((e.action, n.expect));
    }
    assert_eq!(dids.len(), 3, "each cause in its own words: {dids:?}");
    assert_eq!(steps.len(), 1, "the control: {steps:?}");
}
