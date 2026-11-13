// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Fable limit's move onto Opus and its return at the reset, at the turn-end policy.

use super::*;
use crate::supervise::policy::exception::{Exception, Urgency, exception_for};
use crate::supervise::policy::outlook::ExceptionKind;
use aterm_phase::prompt::fixtures::{
    RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300, WALL_FABLE_NO_RESET_2_1_301, WALL_FABLE_RESET_2_1_301,
    WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300, screen,
};

const MIN: Duration = Duration::from_secs(60);

const JOB: Duration = Duration::from_secs(12 * 60);

const GRACE: Duration = Duration::from_secs(150);

const FABLE_NOTICE: &str =
    "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.";

fn cfg() -> SupervisorConfig {
    SupervisorConfig::default()
}

fn t0() -> Instant {
    Instant::now() + Duration::from_secs(10 * 3600)
}

/// A plain turn end of a Claude Code session whose launch named no `--model`: the launch the
/// fallback may move.
fn idle(said: &str, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        launch_model: LaunchModel::Unnamed,
        phase: if said.trim_end().ends_with('?') {
            Phase::Question
        } else {
            Phase::Idle
        },
        authoritative: true,
        survey: false,
        wall: None,
        wall_message: String::new(),
        reset_at: None,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some(said.to_string()),
        said_cut: false,
        opens_cut: false,
        turn_said: None,
        said_lines: None,
        turn_lines: None,
        turn_breaks: Default::default(),
        opened_by: None,
        message_drawn: false,
        submits: None,
        inputs_before: None,
        screen_mark: None,
        worked,
        rules: None,
        fact: None,
        wake: None,
        pending_input: false,
        interrupted: false,
        fault: None,
        restartable: true,
        relaunch_off: false,
        resume: None,
        upgrading: false,
        taskless: false,
        finished_record: None,
        left_record: false,
        left_work: None,
        person: None,
        key: None,
        reach: Default::default(),
        witness: None,
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        chosen_in_transcript: None,
        conversation: Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
        program: Program::Claude,
        seat: Default::default(),
        account: None,
        route: None,
        autopilot: Autopilot::Cruise,
        login_label: None,
        autopilot_by_person: false,
        login_superseded: false,
        login_notice: None,
        login_screen: None,
        login_interrupted: false,
        login_at_wall: None,
        login_method: None,
        login_shared: None,
        login_pin: crate::supervise::policy::login::LoginPin::Picker,
        login_moved: false,
        roster: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        ran_tools: worked.is_some_and(|w| w >= MIN_WORK),
        tools_unread: false,
        record_turn: None,
        credits: None,
        repo: crate::harness::repo_work::RepoRead::Clean,
        leftover: crate::harness::leftover::LeftRead::NotAsked,
    }
}

fn walled(kind: WallKind, message: &str, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        wall: Some(kind),
        wall_message: message.to_string(),
        ran_tools: false,
        ..idle("Suites are running.", worked)
    }
}

/// A model bucket in `message`'s words, its reset at `reset`.
fn bucket_of(message: &str, reset: Option<Instant>) -> TurnEndReading {
    TurnEndReading {
        reset_at: reset,
        ..walled(
            WallKind::ModelBucket { consent: false },
            message,
            Some(3 * MIN),
        )
    }
}

/// Fable's own bucket, its reset at `reset`.
fn fable(reset: Option<Instant>) -> TurnEndReading {
    bucket_of(FABLE_NOTICE, reset)
}

/// `r` launched as `launch` says.
fn launched(r: &TurnEndReading, launch: LaunchModel) -> TurnEndReading {
    TurnEndReading {
        launch_model: launch,
        ..r.clone()
    }
}

fn at_under(
    st: &mut TurnEndState,
    r: &TurnEndReading,
    now: Instant,
    cfg: &SupervisorConfig,
) -> TurnEndAction {
    st.observe(r, now);
    decide_turn_end(st, r, cfg, now)
}

fn at(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> TurnEndAction {
    at_under(st, r, now, &cfg())
}

/// The decision at `r`, a restart it asks made, and the act booked.
fn act(st: &mut TurnEndState, r: &TurnEndReading, now: Instant) -> TurnEndAction {
    let a = at(st, r, now);
    if let TurnEndAction::Restart { why, .. } = &a {
        st.restarted(why, true, r, now);
    }
    st.acted(&a, r, now);
    a
}

fn restarts(a: &TurnEndAction, want: &Restart, rule: &str) -> bool {
    matches!(a, TurnEndAction::Restart { why, rule_id, .. } if why == want && *rule_id == rule)
}

fn to_opus() -> Restart {
    Restart::Model {
        to: FABLE_FALLBACK.to_string(),
    }
}

fn falls_back(a: &TurnEndAction) -> bool {
    restarts(a, &to_opus(), RULE_MODEL_FALLBACK)
}

/// Any move onto a model, to whichever model.
fn any_model_move(a: &TurnEndAction) -> bool {
    match a {
        TurnEndAction::Restart {
            why: Restart::Model { .. },
            ..
        } => true,
        TurnEndAction::Restart { otherwise, .. } => any_model_move(otherwise),
        _ => false,
    }
}

fn restores(a: &TurnEndAction) -> bool {
    matches!(
        a,
        TurnEndAction::Restart {
            why: Restart::ModelBack { .. },
            rule_id: RULE_MODEL_RESTORE,
            ..
        }
    )
}

fn typed(rule: &'static str) -> TurnEndAction {
    let text = match rule {
        RULE_LIMIT_RESUME => cleared_text(Cleared::UsageLimit, Clearing::Measured).to_string(),
        RULE_CONSENT => cleared_text(Cleared::Credits, Clearing::Measured).to_string(),
        _ => continuation_text(&cfg(), None),
    };
    TurnEndAction::Type {
        text,
        rule_id: rule,
    }
}

fn waits_until(a: &TurnEndAction, until: Instant) -> bool {
    matches!(a, TurnEndAction::WaitUntil { until: u, .. } if *u == until)
}

fn is_escalate(a: &TurnEndAction, needle: &str) -> bool {
    matches!(a, TurnEndAction::Escalate { reason } if reason.contains(needle))
}

fn otherwise_of(a: &TurnEndAction) -> &TurnEndAction {
    match a {
        TurnEndAction::Restart { otherwise, .. } => otherwise,
        other => panic!("no restart: {other:?}"),
    }
}

fn advised(r: &TurnEndReading, to: &str) -> TurnEndReading {
    TurnEndReading {
        account: Some(AccountAdvice::Move {
            to: to.to_string(),
            yields: None,
        }),
        ..r.clone()
    }
}

fn advised_wait(r: &TurnEndReading, until: Instant) -> TurnEndReading {
    TurnEndReading {
        account: Some(AccountAdvice::Wait { until }),
        ..r.clone()
    }
}

fn to_account(to: &str, at: AccountAt) -> Restart {
    Restart::Account {
        to: to.to_string(),
        at,
    }
}

fn session_wall(t: Instant) -> TurnEndReading {
    let mut r = walled(
        WallKind::UsageSession,
        "You've hit your session limit · resets 3pm",
        Some(3 * MIN),
    );
    r.reset_at = Some(t + 60 * MIN);
    r
}

fn on_credits(r: &TurnEndReading, reset: Option<Instant>) -> TurnEndReading {
    TurnEndReading {
        credits: Some(CreditsCarry {
            reset_at: reset,
            ..CreditsCarry::default()
        }),
        ..r.clone()
    }
}

fn on_model_credits(r: &TurnEndReading, model: &str, reset: Option<Instant>) -> TurnEndReading {
    TurnEndReading {
        credits: Some(CreditsCarry {
            reset_at: reset,
            model: Some(model.to_string()),
            ..CreditsCarry::default()
        }),
        ..r.clone()
    }
}

fn credits_on() -> SupervisorConfig {
    SupervisorConfig {
        usage_credits: true,
        ..cfg()
    }
}

/// A screen of Claude Code's, read as the loop reads it, from a launch that named no model.
fn captured_rows(rows: &[String]) -> TurnEndReading {
    let reading = aterm_phase::read(Some("claude"), rows, Some(2));
    TurnEndReading {
        launch_model: LaunchModel::Unnamed,
        ..TurnEndReading::of(&reading, rows, false, None, None, None)
    }
}

fn captured(text: &str, reset: Option<Instant>, jitter: Duration) -> TurnEndReading {
    TurnEndReading {
        reset_at: reset,
        jitter,
        ..captured_rows(&screen(text))
    }
}

/// The measured Sonnet bucket's armed countdown (Claude Code 2.1.300) with its notice naming
/// Fable: the same screen at Fable's own limit.
fn fable_armed_rows() -> Vec<String> {
    screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300)
        .into_iter()
        .map(|r| r.replace("your Sonnet limit", "your Fable limit"))
        .collect()
}

fn fable_armed(reset: Instant) -> TurnEndReading {
    TurnEndReading {
        reset_at: Some(reset),
        ..captured_rows(&fable_armed_rows())
    }
}

fn resumed() -> TurnEndReading {
    captured(
        RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
        None,
        Duration::ZERO,
    )
}

fn switch(from: &str, launch: Option<&str>, back_at: Option<Instant>) -> ModelSwitch {
    ModelSwitch {
        from: Some(from.to_string()),
        to: FABLE_FALLBACK.to_string(),
        launch: launch.map(str::to_string),
        back_at,
        proc: None,
        unproven: false,
    }
}

/// THE ONE PAIR (owner, 2026-11-11): Fable's own limit on a Claude Code session whose launch
/// names no model, or names Fable, moves to [`FABLE_FALLBACK`]; nothing else moves. NEGATIVE
/// CONTROLS: every other bucket, a launch that names another model, a launch nobody read, and
/// every other program.
#[test]
fn fallback_for_moves_only_a_claude_fable_session_launched_on_no_model_or_fable() {
    let base = idle("Stage 2 done.", Some(JOB));
    let named = |m: &str| LaunchModel::Named(m.to_string());
    let mut wrong = Vec::new();
    let mut check = |program: Program, launch: LaunchModel, from: Option<&str>, want: bool| {
        let r = TurnEndReading {
            program,
            launch_model: launch.clone(),
            ..base.clone()
        };
        let got = fallback_for(&r, from);
        if got != want.then_some(FABLE_FALLBACK) {
            wrong.push(format!("{program:?} {launch:?} {from:?}: {got:?}"));
        }
    };
    for launch in [
        LaunchModel::Unnamed,
        named("fable"),
        named("Fable"),
        named("claude-fable-5"),
        named("claude-fable-5-1"),
    ] {
        check(Program::Claude, launch.clone(), Some("fable"), true);
        for other in [Some("sonnet"), Some("opus"), Some("haiku"), None] {
            check(Program::Claude, launch.clone(), other, false);
        }
        for program in [
            Program::Codex,
            Program::Agy,
            Program::Muse,
            Program::Grok,
            Program::Kimi,
            Program::Generic,
        ] {
            check(program, launch.clone(), Some("fable"), false);
        }
    }
    for launch in [
        LaunchModel::Unread,
        named("claude-sonnet-4-6"),
        named("sonnet"),
        named("opus"),
        named(FABLE_FALLBACK),
        named("claude-fabled-5"),
    ] {
        check(Program::Claude, launch, Some("fable"), false);
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// What a launch says of its model: [`is_fable`] reads an alias or a `claude-fable-…` id,
/// [`LaunchModel::admits_fallback`] admits a launch naming none or Fable and never one unread,
/// and [`LaunchModel::back_to`] is the launch's own `--model` (none where it named none).
#[test]
fn a_launchs_model_admits_the_fallback_and_names_its_return() {
    for m in ["fable", "FABLE", "claude-fable-5", "claude-fable-5-1"] {
        assert!(is_fable(m), "{m}");
    }
    for m in [
        "opus",
        "sonnet",
        "claude-opus-5-5",
        "claude-sonnet-4-6",
        "fable-5",
        "claude-fabled-5",
        "",
    ] {
        assert!(!is_fable(m), "{m}");
    }
    assert_eq!(LaunchModel::default(), LaunchModel::Unread);
    assert!(!LaunchModel::Unread.admits_fallback());
    assert!(LaunchModel::Unnamed.admits_fallback());
    assert!(LaunchModel::Named("claude-fable-5".to_string()).admits_fallback());
    assert!(!LaunchModel::Named("claude-sonnet-4-6".to_string()).admits_fallback());
    assert_eq!(LaunchModel::Unread.back_to(), None);
    assert_eq!(LaunchModel::Unnamed.back_to(), None);
    assert_eq!(
        LaunchModel::Named("fable".to_string()).back_to(),
        Some("fable".to_string())
    );
    let argv = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        LaunchModel::of_argv(&argv(&["claude"])),
        LaunchModel::Unnamed
    );
    assert_eq!(
        LaunchModel::of_argv(&argv(&["claude", "--model", "claude-sonnet-4-6"])),
        LaunchModel::Named("claude-sonnet-4-6".to_string())
    );
    assert_eq!(
        LaunchModel::of_argv(&argv(&["claude", "--model", "sonnet", "--model=fable"])),
        LaunchModel::Named("fable".to_string()),
        "the last --model"
    );
}

/// The kinds of the two moves: the move onto Opus is a move, its return a move put off to a
/// quiet moment.
#[test]
fn the_fallback_and_its_return_are_moves_and_the_return_waits_for_a_quiet_moment() {
    assert_eq!(to_opus().word(), Restart::MODEL);
    assert_eq!(Restart::ModelBack { to: None }.word(), Restart::MODEL_BACK);
    assert!(Restart::MOVES.contains(&Restart::MODEL));
    assert!(Restart::MOVES.contains(&Restart::MODEL_BACK));
    assert!(Restart::put_off(Restart::MODEL_BACK));
    assert!(
        !Restart::put_off(Restart::MODEL),
        "the fallback goes at once"
    );
    assert_eq!(RULE_MODEL_FALLBACK, "model-fallback@v1");
    assert_eq!(RULE_MODEL_RESTORE, "model-restore@v1");
}

#[test]
fn a_fable_limit_relaunches_on_opus_and_back_at_its_reset_never_by_model() {
    let t = t0();
    let reset = t + 120 * MIN;
    let r = fable(Some(reset));
    let mut st = TurnEndState::default();
    let a = act(&mut st, &r, t);
    assert!(falls_back(&a), "{a:?}");
    assert!(
        waits_until(otherwise_of(&a), reset + GRACE),
        "unmade, the reset is waited out: {a:?}"
    );
    assert_eq!(st.model_switch(), Some(&switch("fable", None, Some(reset))));
    let later = idle("Stage 3 done.", Some(30 * MIN));
    let a = at(&mut st, &later, reset + MIN);
    assert!(
        restarts(&a, &Restart::ModelBack { to: None }, RULE_MODEL_RESTORE),
        "the return at the reset, to the launch's own (no) --model: {a:?}"
    );
    assert_eq!(otherwise_of(&a), &typed(RULE_CONTINUE));
    let mut early = st.clone();
    assert!(
        !restores(&at(&mut early, &later, reset - MIN)),
        "not before the reset"
    );
    st.restarted(&Restart::ModelBack { to: None }, true, &later, reset + MIN);
    assert_eq!(st.model_switch(), None, "the return closes the switch");

    let named = launched(&r, LaunchModel::Named("claude-fable-5".to_string()));
    let mut st = TurnEndState::default();
    assert!(falls_back(&act(&mut st, &named, t)));
    assert_eq!(
        st.model_switch(),
        Some(&switch("fable", Some("claude-fable-5"), Some(reset)))
    );
    let a = at(&mut st, &later, reset + MIN);
    assert!(
        restarts(
            &a,
            &Restart::ModelBack {
                to: Some("claude-fable-5".to_string())
            },
            RULE_MODEL_RESTORE
        ),
        "the launch's own --model goes back on the line: {a:?}"
    );

    let mut st = TurnEndState::default();
    act(&mut st, &fable(None), t);
    assert_eq!(
        st.model_switch(),
        Some(&switch("fable", None, None)),
        "no reset named: no return scheduled"
    );
    let a = at(&mut st, &later, t + 10 * 24 * 60 * MIN);
    assert!(!restores(&a), "{a:?}");

    let bare = TurnEndReading {
        restartable: false,
        ..r.clone()
    };
    let mut st = TurnEndState::default();
    assert!(waits_until(&at(&mut st, &bare, t), reset + GRACE));
    let mut st = TurnEndState::default();
    let a = at(&mut st, &r, t);
    let TurnEndAction::Restart { why, .. } = &a else {
        panic!("{a:?}")
    };
    st.restarted(why, false, &r, t);
    assert!(st.fallback_refused());
    assert!(waits_until(&at(&mut st, &r, t + MIN), reset + GRACE));
    assert_eq!(st.model_switch(), None, "nothing switched");
}

/// NEGATIVE CONTROLS of the narrowing: every bucket but Fable's, a launch naming another
/// `--model`, a launch nobody read and another program's Fable limit are waited out to the reset
/// exactly as with no fallback at all, and no switch is opened.
#[test]
fn every_other_bucket_launch_or_program_waits_its_reset_out() {
    let t = t0();
    let reset = t + 120 * MIN;
    let unread = launched(&fable(Some(reset)), LaunchModel::Unread);
    let mut st = TurnEndState::default();
    let plain = at(&mut st, &unread, t);
    assert!(waits_until(&plain, reset + GRACE), "{plain:?}");
    let cases = [
        (
            "a Sonnet bucket",
            bucket_of("You've hit your Sonnet limit · resets 3pm", Some(reset)),
        ),
        (
            "an Opus bucket",
            bucket_of("You've reached your Opus limit · resets 3pm", Some(reset)),
        ),
        ("a launch nobody read", unread.clone()),
        (
            "a launch naming Sonnet",
            launched(
                &fable(Some(reset)),
                LaunchModel::Named("claude-sonnet-4-6".to_string()),
            ),
        ),
        (
            "a launch naming Opus",
            launched(&fable(Some(reset)), LaunchModel::Named("opus".to_string())),
        ),
    ];
    for (name, r) in cases {
        let mut st = TurnEndState::default();
        let a = act(&mut st, &r, t);
        assert_eq!(a, plain, "{name}");
        assert_eq!(st.model_switch(), None, "{name}");
    }
    for program in [Program::Codex, Program::Agy] {
        let r = TurnEndReading {
            program,
            ..fable(Some(reset))
        };
        let mut st = TurnEndState::default();
        let a = act(&mut st, &r, t);
        assert!(!any_model_move(&a), "{program:?}: {a:?}");
        assert_eq!(st.model_switch(), None, "{program:?}");
    }
    let r = fable(Some(reset));
    let mut st = TurnEndState::default();
    assert!(
        falls_back(&act(&mut st, &r, t)),
        "CONTROL: the same wall, a launch that named no model"
    );
}

#[test]
fn a_fable_bucket_under_the_generic_banner_relaunches_on_the_fallback() {
    let t = t0();
    let reset = t + 2 * 24 * 60 * MIN;
    let r = TurnEndReading {
        restartable: true,
        ..fable_armed(reset)
    };
    assert_eq!(
        r.wall,
        Some(WallKind::ModelBucket { consent: false }),
        "{r:#?}"
    );
    let mut st = TurnEndState::default();
    let a = act(&mut st, &r, t);
    assert!(falls_back(&a), "{a:?}");
    assert_eq!(st.model_switch(), Some(&switch("fable", None, Some(reset))));
    let goes_on = |a: &TurnEndAction| {
        matches!(a, TurnEndAction::WaitUntil { until, why, .. }
            if *until == reset + 10 * MIN && why == "model-bucket goes on by itself")
    };
    let a = at(
        &mut TurnEndState::default(),
        &launched(&r, LaunchModel::Unread),
        t,
    );
    assert!(goes_on(&a), "a launch nobody read: {a:?}");
    let sonnet = TurnEndReading {
        restartable: true,
        reset_at: Some(reset),
        ..captured_rows(&screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300))
    };
    let a = at(&mut TurnEndState::default(), &sonnet, t);
    assert!(goes_on(&a), "the measured Sonnet bucket: {a:?}");
}

#[test]
fn the_measured_fable_walls_relaunch_on_opus_with_and_without_a_reset() {
    let t = t0();
    let reset = t + 120 * MIN;
    for (name, text, at_reset) in [
        ("a reset", WALL_FABLE_RESET_2_1_301, Some(reset)),
        ("no reset", WALL_FABLE_NO_RESET_2_1_301, None),
    ] {
        let r = TurnEndReading {
            restartable: true,
            worked: Some(3 * MIN),
            ..captured(text, at_reset, Duration::ZERO)
        };
        assert!(
            matches!(r.wall, Some(WallKind::ModelBucket { .. })),
            "{name}: {r:#?}"
        );
        assert_eq!(bucket_model(&r.wall_message).as_deref(), Some(FABLE));
        let mut st = TurnEndState::default();
        let a = act(&mut st, &r, t);
        assert!(falls_back(&a), "{name}: {a:?}");
        assert_eq!(
            st.model_switch().map(|m| m.back_at),
            Some(at_reset),
            "{name}"
        );
        let mut st = TurnEndState::default();
        let a = act(&mut st, &launched(&r, LaunchModel::Unread), t);
        assert!(!any_model_move(&a), "{name}, unread: {a:?}");
    }
}

#[test]
fn a_buckets_words_under_a_call_run_no_model_fallback() {
    let t = t0();
    let reset = t + 2 * 24 * 60 * MIN;
    let rows = fable_armed_rows();
    let turn = rows
        .iter()
        .position(|r| r.starts_with("❯ carry on with the task"))
        .expect("the turn's row");
    let mut quoted = rows.clone();
    quoted[turn] = "⏺ Bash(cat limits.log)".to_string();
    let mut later = rows.clone();
    later.splice(
        turn..=turn,
        [
            "⏺ Bash(cat limits.log)".to_string(),
            "  ⎿  Pasted earlier:".to_string(),
        ],
    );
    for (name, rows, want) in [
        ("measured", &rows, WallKind::ModelBucket { consent: false }),
        ("quoted", &quoted, WallKind::UsageSession),
        ("later", &later, WallKind::UsageSession),
    ] {
        let r = TurnEndReading {
            restartable: true,
            reset_at: Some(reset),
            ..captured_rows(rows)
        };
        assert_eq!(r.wall, Some(want), "{name}: {r:#?}");
        let a = act(&mut TurnEndState::default(), &r, t);
        assert_eq!(falls_back(&a), name == "measured", "{name}: {a:?}");
        if name != "measured" {
            assert!(
                matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.starts_with("usage-session")),
                "{name}: {a:?}"
            );
        }
    }
}

/// A bucket whose countdown left (a `/resume`, Esc) is waited to its reset where no relaunch
/// moved off it; after the fallback's relaunch the session is on Opus and the continue policy
/// goes on.
#[test]
fn a_bucket_no_relaunch_moved_off_is_waited_on_when_its_countdown_ends() {
    let t = t0();
    let reset = t + 2 * 24 * 60 * MIN;
    let jitter = Duration::from_secs(17);
    let armed = TurnEndReading {
        jitter,
        ..fable_armed(reset)
    };
    let resumed = resumed();
    // The countdown cancelled with Esc: its banner gone, Claude Code's row saying so above the
    // composer's frame.
    let mut rows: Vec<String> = fable_armed_rows()
        .into_iter()
        .filter(|r| !r.contains("Usage limit reached") && !r.contains("Continuing automatically"))
        .collect();
    let top = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the composer's frame");
    rows.splice(
        top..top,
        [
            "⏺ Automatic continue cancelled · /rate-limit-options to re-arm".to_string(),
            String::new(),
        ],
    );
    let cancelled = captured_rows(&rows);
    assert_eq!(
        (resumed.wall, cancelled.wall, armed.resumes_by_itself),
        (None, None, true)
    );
    let due = reset + GRACE + jitter;
    for (what, launch, restartable) in [
        ("no host to relaunch", LaunchModel::Unnamed, false),
        ("a launch nobody read", LaunchModel::Unread, true),
        (
            "a launch naming Sonnet",
            LaunchModel::Named("claude-sonnet-4-6".to_string()),
            true,
        ),
    ] {
        for (left, screen) in [
            ("a /resume", &resumed),
            ("Esc on the countdown", &cancelled),
        ] {
            let on = |r: &TurnEndReading| TurnEndReading {
                restartable,
                launch_model: launch.clone(),
                ..r.clone()
            };
            let mut st = TurnEndState::default();
            let at_wall = at(&mut st, &on(&armed), t);
            assert!(
                matches!(&at_wall, TurnEndAction::WaitUntil { until, why, .. }
                    if *until == reset + 10 * MIN && why == "model-bucket goes on by itself"),
                "{what}: the countdown at its word: {at_wall:?}"
            );
            for now in [t + MIN, due - Duration::from_secs(1)] {
                let a = at(&mut st, &on(screen), now);
                assert!(
                    matches!(&a, TurnEndAction::WaitUntil { until, why, .. }
                        if *until == due && why == "model-bucket resets"),
                    "{what}, {left}: the bucket's reset: {a:?}"
                );
                assert!(st.waits_on_named_reset(), "{what}, {left}");
            }
            assert_eq!(
                at(&mut st, &on(screen), due),
                typed(RULE_LIMIT_RESUME),
                "{what}, {left}"
            );
        }
    }
    let mut st = TurnEndState::default();
    let relaunch = TurnEndReading {
        restartable: true,
        ..armed.clone()
    };
    assert!(falls_back(&act(&mut st, &relaunch, t)), "the relaunch");
    let a = at(&mut st, &resumed, t + MIN);
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("short turn")),
        "the continue policy's back-off: {a:?}"
    );
    assert!(!st.waits_on_named_reset());
}

#[test]
fn a_relaunch_where_the_buckets_notice_left_knows_the_bucket() {
    let t = t0();
    let reset = t + 2 * 24 * 60 * MIN;
    let armed = fable_armed(reset);
    let resumed = TurnEndReading {
        restartable: true,
        ..resumed()
    };
    let switched = switch("fable", None, Some(reset));
    let mut st = TurnEndState::default();
    let a = at(&mut st, &armed, t);
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { why, .. } if why == "model-bucket goes on by itself"),
        "no host to relaunch: {a:?}"
    );
    let a = act(&mut st, &resumed, t + MIN);
    assert!(falls_back(&a), "the bucket's own rule where it left: {a:?}");
    assert_eq!(st.model_switch(), Some(&switched));
    let mut st = TurnEndState::default();
    let a = act(
        &mut st,
        &TurnEndReading {
            restartable: true,
            ..armed.clone()
        },
        t,
    );
    assert!(falls_back(&a), "{a:?}");
    assert_eq!(st.model_switch(), Some(&switched));
    let mut st = TurnEndState::default();
    at(&mut st, &launched(&armed, LaunchModel::Unread), t);
    let a = act(&mut st, &launched(&resumed, LaunchModel::Unread), t + MIN);
    assert!(!any_model_move(&a), "CONTROL: a launch nobody read: {a:?}");
}

#[test]
fn a_buckets_notice_written_before_the_relaunched_process_started_is_the_one_it_left() {
    let t = t0();
    let reset = t + 2 * 24 * 60 * MIN;
    let armed = TurnEndReading {
        restartable: true,
        ..fable_armed(reset)
    };
    let asked = 1_792_100_000;
    let left = |written: i64, started: &[i64]| -> bool {
        let mut st = TurnEndState::default();
        let a = act(&mut st, &armed, t);
        assert!(falls_back(&a), "{a:?}");
        st.fallback_made_at(asked);
        assert!(st.fallback_unasked());
        for &at in started {
            st.agent_started_unix(at);
        }
        st.notice_written_at(Some(written));
        st.bucket_left(&armed)
    };
    let old = asked - 86_400;
    assert!(left(asked + 300, &[old, asked + 900]), "the redraw");
    assert!(
        !left(asked + 960, &[old, asked + 900]),
        "written by the relaunched process"
    );
    assert!(!left(asked + 300, &[]), "no start named");
    assert!(!left(asked + 300, &[old]), "the old process's start");
    assert!(
        left(asked + 300, &[asked + 900, asked + 3600]),
        "a later process moves nothing"
    );
    assert!(
        !left(asked + 1000, &[asked + 900, asked + 3600]),
        "nor reads the first one's answer as its redraw"
    );
}

#[test]
fn a_buckets_notice_nothing_dates_is_read_by_the_fallbacks_latch_alone() {
    let t = t0();
    let reset = t + 2 * 24 * 60 * MIN;
    let armed = TurnEndReading {
        restartable: true,
        ..fable_armed(reset)
    };
    let mut st = TurnEndState::default();
    let a = act(&mut st, &armed, t);
    assert!(falls_back(&a), "{a:?}");
    let asked = 1_792_100_000;
    st.fallback_made_at(asked);
    st.agent_started_unix(asked + 900);
    st.notice_written_at(None);
    assert!(st.bucket_left(&armed), "the redraw: the latch alone");
    let sonnet = TurnEndReading {
        restartable: true,
        reset_at: Some(reset),
        ..captured_rows(&screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300))
    };
    assert!(
        !st.bucket_left(&sonnet),
        "CONTROL: another model's bucket is not the one left"
    );
    let ran = TurnEndReading {
        worked: Some(3 * MIN),
        ..resumed()
    };
    let _ = at(&mut st, &ran, t + 10 * MIN);
    st.notice_written_at(None);
    assert!(!st.bucket_left(&armed), "a turn asked since: the wall");
}

#[test]
fn a_bucket_the_fallbacks_relaunch_left_holds_no_point_for_an_unread_conversation() {
    let t = t0();
    let reset = t + 2 * 24 * 60 * MIN;
    let unread = |r: TurnEndReading| TurnEndReading {
        conversation: Conversation::Unknown,
        restartable: true,
        ..r
    };
    let armed = unread(fable_armed(reset));
    let resumed = unread(resumed());
    let looks = |a: &TurnEndAction| {
        matches!(a, TurnEndAction::WaitUntil { why, .. }
            if why == "the remembered limit's conversation is unread; no act on its account")
    };
    let relaunched = |made: bool| {
        let mut st = TurnEndState::default();
        let a = at(&mut st, &armed, t);
        assert!(falls_back(&a), "{a:?}");
        st.restarted(&to_opus(), made, &armed, t);
        st
    };
    let continues = |st: &TurnEndState, r: &TurnEndReading, a: &TurnEndAction| {
        matches!(a, TurnEndAction::WaitUntil { until, .. }
            if !looks(a) && decide_turn_end(st, r, &cfg(), *until) == typed(RULE_CONTINUE))
    };
    for (name, r) in [("the redraw", &armed), ("no redraw", &resumed)] {
        let mut st = relaunched(true);
        let a = at(&mut st, r, t + MIN);
        assert!(continues(&st, r, &a), "{name}: {a:?}");
    }
    let mut st = relaunched(false);
    let a = at(&mut st, &resumed, t + MIN);
    assert!(looks(&a), "the relaunch not made: {a:?}");
    let mut st = relaunched(true);
    let a = act(&mut st, &resumed, t + MIN);
    let a = match a {
        TurnEndAction::WaitUntil { until, .. } => act(&mut st, &resumed, until),
        other => other,
    };
    assert_eq!(a, typed(RULE_CONTINUE), "a turn asked on the fallback");
    let _ = at(&mut st, &armed, t + 5 * MIN);
    let a = at(&mut st, &resumed, t + 6 * MIN);
    assert!(looks(&a), "the bucket answering again, then left: {a:?}");
}

#[test]
fn a_bucket_waited_on_is_taken_up_by_the_loop_that_follows() {
    let t = t0();
    let reset = t + 2 * 24 * 60 * MIN;
    let resumed = TurnEndReading {
        restartable: true,
        ..resumed()
    };
    let words = |model: &str| {
        format!(
            "You've hit your {model} limit · resets Oct 17 at 4:20pm (America/Los_Angeles) — ⚠ \
             Usage limit reached · limit resets Oct 17 at 4:20pm"
        )
    };
    let seeded = |model: &str| {
        let mut st = TurnEndState::default();
        st.seed_named_reset(
            WallKind::ModelBucket { consent: false },
            &words(model),
            reset,
            Duration::ZERO,
            false,
            ResetClock::of(Some("Oct 17 at 4:20pm")),
            t,
        );
        assert!(st.waits_on_named_reset());
        st
    };
    let mut st = seeded("Fable");
    let a = at(&mut st, &resumed, t + MIN);
    assert!(falls_back(&a), "{a:?}");
    for (name, model, r) in [
        ("an Opus bucket", "Opus", resumed.clone()),
        ("a Sonnet bucket", "Sonnet", resumed.clone()),
        (
            "a launch nobody read",
            "Fable",
            launched(&resumed, LaunchModel::Unread),
        ),
    ] {
        let mut st = seeded(model);
        let a = at(&mut st, &r, t + MIN);
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { until, why, .. }
                if *until == reset + GRACE && why == "model-bucket resets"),
            "{name}: {a:?}"
        );
    }
}

#[test]
fn a_model_switch_is_its_relaunched_process_and_another_programs_bucket_falls_back() {
    /// What the switch is to the process named: it rides it (its bucket waits, `back_at`
    /// restores), it holds its return (its bucket waits, nothing restores yet), or it is over
    /// (its bucket falls back, nothing restores).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Want {
        Rides,
        Holds,
        Over,
    }
    let t = t0();
    let reset = t + 2 * 24 * 60 * MIN;
    let a = SwitchProc {
        pid: 4100,
        start: 1_000,
        conversation: Some("conv-a".to_string()),
    };
    let switched = |proc: Option<SwitchProc>| {
        let mut st = TurnEndState::default();
        st.seed_model(ModelSwitch {
            proc,
            ..switch("fable", None, Some(reset))
        });
        st
    };
    // The bucket the switch left, met in the tab after the host names the process.
    let bucket = TurnEndReading {
        restartable: true,
        ..fable_armed(reset)
    };
    let named = |pid, start, conversation: Option<&str>| SwitchProc {
        pid,
        start,
        conversation: conversation.map(str::to_string),
    };
    // What the loop saw before the host named the process: nothing, a step of the host's that
    // started one, or its own restart, made (a memory restart's, a lost line's) or not.
    let nothing: &dyn Fn(&mut TurnEndState) = &|_: &mut TurnEndState| {};
    let host_step: &dyn Fn(&mut TurnEndState) = &|st: &mut TurnEndState| st.harness_started();
    let restart = |why: Restart, made: bool| {
        move |st: &mut TurnEndState| {
            st.restarted(&why, made, &idle("Fixed the parser.", Some(JOB)), t);
        }
    };
    let memory_restart = restart(Restart::Memory, true);
    let memory_restart: &dyn Fn(&mut TurnEndState) = &memory_restart;
    let lost_line = Restart::InputLost {
        of: (Some(a.pid), Some(a.start)),
        line: crate::supervise::custody::LineAsk {
            at_ms: 900_000,
            nth: 0,
        },
    };
    let input_lost_restart = restart(lost_line, true);
    let input_lost_restart: &dyn Fn(&mut TurnEndState) = &input_lost_restart;
    let memory_unmade = restart(Restart::Memory, false);
    let memory_unmade: &dyn Fn(&mut TurnEndState) = &memory_unmade;
    let b = named(4200, 2_000, Some("conv-b"));
    let p3 = |conversation| named(4300, 3_000, Some(conversation));
    let mut wrong = Vec::new();
    let rows = vec![
        (
            "the person's fresh program, B",
            Some(a.clone()),
            nothing,
            vec![b.clone()],
            false,
            Some(SwitchScope::Closed),
            Want::Over,
        ),
        (
            "another program under the switch's pid, reused",
            Some(a.clone()),
            nothing,
            vec![named(4100, 5_000, Some("conv-b"))],
            false,
            Some(SwitchScope::Closed),
            Want::Over,
        ),
        (
            "another process whose conversation the host does not name yet",
            Some(a.clone()),
            nothing,
            vec![named(4200, 2_000, None)],
            false,
            None,
            Want::Holds,
        ),
        (
            "a relaunch the host still owes, on a conversation the switch never named",
            Some(a.clone()),
            nothing,
            vec![p3("conv-z")],
            true,
            Some(SwitchScope::Moved(p3("conv-z"))),
            Want::Rides,
        ),
        (
            "a process a step of the host's started, afresh on a new conversation",
            Some(a.clone()),
            host_step,
            vec![p3("conv-n")],
            false,
            Some(SwitchScope::Moved(p3("conv-n"))),
            Want::Rides,
        ),
        (
            "a process this loop's memory restart started, afresh on a new conversation",
            Some(a.clone()),
            memory_restart,
            vec![p3("conv-n")],
            false,
            Some(SwitchScope::Moved(p3("conv-n"))),
            Want::Rides,
        ),
        (
            "a process this loop's lost-input restart started, afresh on a new conversation",
            Some(a.clone()),
            input_lost_restart,
            vec![p3("conv-n")],
            false,
            Some(SwitchScope::Moved(p3("conv-n"))),
            Want::Rides,
        ),
        (
            "control: a memory restart the host did not make, then B",
            Some(a.clone()),
            memory_unmade,
            vec![b.clone()],
            false,
            Some(SwitchScope::Closed),
            Want::Over,
        ),
        (
            "a process a step of the host's started, named first without its conversation",
            Some(a.clone()),
            host_step,
            vec![named(4300, 3_000, None), p3("conv-n")],
            false,
            Some(SwitchScope::Moved(p3("conv-n"))),
            Want::Rides,
        ),
        (
            "control: a step of the host's spent on the switch's own process, then B",
            Some(a.clone()),
            host_step,
            vec![a.clone(), b.clone()],
            false,
            Some(SwitchScope::Closed),
            Want::Over,
        ),
        (
            "control: the relaunched process",
            Some(a.clone()),
            nothing,
            vec![a.clone()],
            false,
            None,
            Want::Rides,
        ),
        (
            "control: the relaunched process after a /clear",
            Some(a.clone()),
            nothing,
            vec![named(4100, 1_000, Some("conv-a2"))],
            false,
            Some(SwitchScope::Moved(named(4100, 1_000, Some("conv-a2")))),
            Want::Rides,
        ),
        (
            "control: a relaunch that resumed A",
            Some(a.clone()),
            nothing,
            vec![p3("conv-a")],
            false,
            Some(SwitchScope::Moved(p3("conv-a"))),
            Want::Rides,
        ),
        (
            "control: a switch no process was named for, as an older build wrote it",
            None,
            nothing,
            vec![b.clone()],
            false,
            Some(SwitchScope::Moved(b.clone())),
            Want::Rides,
        ),
        (
            "control: a host that names a conversation for neither process",
            Some(named(4100, 1_000, None)),
            nothing,
            vec![named(4200, 2_000, None)],
            false,
            Some(SwitchScope::Moved(named(4200, 2_000, None))),
            Want::Rides,
        ),
    ];
    for (case, proc, before, told, owed, scope, want) in rows {
        let tell = |st: &mut TurnEndState| {
            before(st);
            told.iter()
                .map(|now| {
                    st.agent_process(now.pid, now.start, now.conversation.as_deref(), || owed)
                })
                .last()
                .flatten()
        };
        let mut st = switched(proc.clone());
        let scoped = tell(&mut st);
        let open = st.model_switch().is_some();
        let at_bucket = at(&mut st, &bucket, t);
        let mut st = switched(proc);
        tell(&mut st);
        let back = at(&mut st, &idle("Fixed the parser.", Some(JOB)), reset);
        let got = match (open, falls_back(&at_bucket), restores(&back)) {
            (true, false, true) => Some(Want::Rides),
            (true, false, false) => Some(Want::Holds),
            (false, true, false) => Some(Want::Over),
            _ => None,
        };
        if scoped != scope || got != Some(want) {
            wrong.push(format!(
                "{case}: told {scoped:?}, open {open}, at the bucket {at_bucket:?}, at back_at {back:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_fable_bucket_under_consent_falls_back_unless_credits_carry_it_and_opus_waits_out_its_own() {
    let t = t0();
    let consent = walled(
        WallKind::ModelBucket { consent: true },
        "Fable limit reached · continuing on Sonnet uses usage credits, and the prompt to confirm",
        Some(3 * MIN),
    );
    let fallback = |a: &TurnEndAction| {
        matches!(
            a,
            TurnEndAction::Restart {
                rule_id: RULE_MODEL_FALLBACK,
                ..
            }
        )
    };
    let mut st = TurnEndState::default();
    let a = at(&mut st, &consent, t);
    assert!(fallback(&a), "{a:?}");
    let mut st = TurnEndState::default();
    let a = at(&mut st, &launched(&consent, LaunchModel::Unread), t);
    assert!(
        is_escalate(&a, "usage_credits` is off"),
        "CONTROL: a launch nobody read: {a:?}"
    );
    let credits = credits_on();
    let mut st = TurnEndState::default();
    let a = at_under(&mut st, &consent, t, &credits);
    assert_eq!(a, typed(RULE_CONSENT));
    st.acted(&a, &consent, t);
    let again = walled(
        WallKind::ModelBucket { consent: true },
        "Fable limit reached · continuing on Sonnet uses usage credits, and the prompt to confirm",
        Some(Duration::from_secs(5)),
    );
    let a = at_under(&mut st, &again, t + MIN, &credits);
    assert!(fallback(&a), "the consent answered once: {a:?}");
    let no_continue = SupervisorConfig {
        continue_policy: false,
        ..credits.clone()
    };
    let mut st = TurnEndState::default();
    let a = at_under(&mut st, &consent, t, &no_continue);
    assert!(fallback(&a), "{a:?}");
    let r = walled(
        WallKind::ModelBucket { consent: false },
        "You've reached your Fable limit.",
        Some(3 * MIN),
    );
    let mut st = TurnEndState::default();
    assert!(fallback(&act(&mut st, &r, t)));
    act(&mut st, &idle("", None), t + MIN);
    let opus = bucket_of("You've reached your Opus limit.", Some(t + 90 * MIN));
    let a = at(&mut st, &opus, t + 5 * MIN);
    assert!(
        waits_until(&a, t + 90 * MIN + GRACE),
        "Opus's own limit, on the fallback, is waited out: {a:?}"
    );
    let mut st = TurnEndState::default();
    let odd = walled(
        WallKind::ModelBucket { consent: false },
        "Nimbus requires usage credits.",
        Some(3 * MIN),
    );
    let a = act(&mut st, &odd, t);
    assert!(!any_model_move(&a), "a bucket naming no model: {a:?}");
    assert_eq!(st.model_switch(), None);
}

#[test]
fn a_fable_bucket_moves_account_first_and_the_fallback_is_its_otherwise() {
    let t = t0();
    let r = fable(Some(t + 120 * MIN));
    let fallback = at(&mut TurnEndState::default(), &r, t);
    assert!(falls_back(&fallback), "{fallback:?}");
    let moved = at(&mut TurnEndState::default(), &advised(&r, "work-2"), t);
    assert!(
        restarts(&moved, &to_account("work-2", AccountAt::Wall), RULE_ACCOUNT),
        "{moved:?}"
    );
    assert_eq!(otherwise_of(&moved), &fallback);
    assert_eq!(
        at(
            &mut TurnEndState::default(),
            &advised_wait(&r, t + 30 * MIN),
            t
        ),
        fallback,
        "another account's later window yields to the move onto Opus now"
    );
    let consent = walled(
        WallKind::ModelBucket { consent: true },
        "Fable limit reached · continuing on Sonnet uses usage credits, and the prompt to confirm",
        Some(3 * MIN),
    );
    let moved = at(
        &mut TurnEndState::default(),
        &advised(&consent, "work-2"),
        t,
    );
    assert!(
        restarts(&moved, &to_account("work-2", AccountAt::Wall), RULE_ACCOUNT),
        "{moved:?}"
    );
    assert!(falls_back(otherwise_of(&moved)), "{moved:?}");
    assert_eq!(
        at_under(
            &mut TurnEndState::default(),
            &advised(&consent, "work-2"),
            t,
            &credits_on()
        ),
        typed(RULE_CONSENT)
    );
}

/// AN ACCOUNT MOVE MADE KEEPS THE SWITCH AND ITS RETURN: the account's relaunch keeps the line's
/// `--model`, so the session is still on Opus and the return at the Fable reset is still owed.
/// NEGATIVE CONTROL: an account move not made leaves the switch as it was too.
#[test]
fn an_account_move_made_keeps_the_switch_and_its_return() {
    let t = t0();
    let reset = t + 120 * MIN;
    let mut st = TurnEndState::default();
    act(&mut st, &fable(Some(reset)), t);
    let switched = st.model_switch().cloned().expect("the switch");
    let wall = advised(&session_wall(t + 10 * MIN), "work-2");
    let a = act(&mut st, &wall, t + 10 * MIN);
    assert!(
        restarts(&a, &to_account("work-2", AccountAt::Wall), RULE_ACCOUNT),
        "{a:?}"
    );
    assert_eq!(st.account_move().map(|m| m.to.as_str()), Some("work-2"));
    assert_eq!(st.wall_attempts(WallKind::UsageSession), None);
    assert_eq!(
        st.model_switch(),
        Some(&switched),
        "the switch rides the move"
    );
    assert!(
        !st.fallback_unasked(),
        "the account's relaunch is no longer the fallback's"
    );
    // The account's relaunch is owed its own continuation first; the return waits for a quiet
    // moment after it.
    let resumed_on = idle("Resumed: carrying on.", Some(Duration::from_secs(2)));
    let a = act(&mut st, &resumed_on, t + 11 * MIN);
    assert!(!restores(&a), "{a:?}");
    let a = at(&mut st, &idle("Stage 3 done.", Some(30 * MIN)), reset + MIN);
    assert!(
        restarts(&a, &Restart::ModelBack { to: None }, RULE_MODEL_RESTORE),
        "the return is still owed: {a:?}"
    );
    let mut st = TurnEndState::default();
    act(&mut st, &fable(Some(reset)), t);
    let a = at(&mut st, &wall, t + 10 * MIN);
    let TurnEndAction::Restart { why, .. } = &a else {
        panic!("{a:?}")
    };
    st.restarted(why, false, &wall, t + 10 * MIN);
    assert_eq!(st.model_switch(), Some(&switched));
}

#[test]
fn an_account_move_is_asked_ahead_of_the_models_return() {
    let t = t0();
    let mut st = TurnEndState::default();
    act(&mut st, &fable(Some(t + 120 * MIN)), t);
    let later = idle("Stage 3 done.", Some(30 * MIN));
    let mut plain = st.clone();
    assert!(restarts(
        &at(&mut plain, &later, t + 130 * MIN),
        &Restart::ModelBack { to: None },
        RULE_MODEL_RESTORE
    ));
    let a = act(&mut st, &advised(&later, "work-2"), t + 130 * MIN);
    assert!(
        restarts(&a, &to_account("work-2", AccountAt::Ahead), RULE_ACCOUNT),
        "{a:?}"
    );
    assert!(
        st.model_switch().is_some(),
        "the account move keeps the switch"
    );
    let a = at(&mut st, &later, t + 140 * MIN);
    assert!(
        restarts(&a, &Restart::ModelBack { to: None }, RULE_MODEL_RESTORE),
        "the return after the account move: {a:?}"
    );
}

#[test]
fn a_fable_bucket_past_its_reset_is_probed_and_falls_back_before_the_jitters_share() {
    let t = t0();
    let jitter = Duration::from_secs(45);
    let passed = t - GRACE - Duration::from_secs(10);
    let plain = fable(Some(passed));
    let mut st = TurnEndState::default();
    assert_eq!(
        at(&mut st, &plain, t),
        typed(RULE_LIMIT_RESUME),
        "no share: the probe goes at once"
    );
    let shared = TurnEndReading {
        jitter,
        ..plain.clone()
    };
    let mut st = TurnEndState::default();
    let a = at(&mut st, &shared, t);
    assert!(
        falls_back(&a),
        "before the share the probe is not yet due, so the fallback is asked: {a:?}"
    );
    let mut st = TurnEndState::default();
    assert_eq!(
        at(&mut st, &shared, t + Duration::from_secs(11 + 45)),
        typed(RULE_LIMIT_RESUME),
        "past the share the probe goes"
    );
}

#[test]
fn a_models_usage_credits_relaunch_onto_the_fallback_or_hold_until_the_process_changes() {
    let t = t0();
    let reset = t + 90 * MIN;
    let end = idle("Stage 2 done.", Some(JOB));
    let named = on_model_credits(&end, "fable", Some(reset));
    let mut st = TurnEndState::default();
    let a = act(&mut st, &named, t);
    assert!(falls_back(&a), "{a:?}");
    let wait = otherwise_of(&a);
    assert!(waits_until(wait, reset + GRACE), "{a:?}");
    assert!(
        matches!(wait, TurnEndAction::WaitUntil { why, .. } if why.contains("fable's requests")),
        "{a:?}"
    );
    let switched = st.model_switch().expect("the switch made");
    assert_eq!(
        (switched.from.as_deref(), switched.back_at),
        (Some("fable"), Some(reset))
    );
    let unnamed = on_model_credits(&end, "fable", None);
    let a = at(&mut TurnEndState::default(), &unnamed, t);
    assert!(falls_back(&a), "{a:?}");
    assert!(
        is_escalate(otherwise_of(&a), CREDITS_HOLD_MODEL_TOLD),
        "{a:?}"
    );
    assert_eq!(
        at_under(&mut TurnEndState::default(), &named, t, &credits_on()),
        typed(RULE_CONTINUE),
        "credits on: carried"
    );
    for (name, r) in [
        (
            "Sonnet's credits",
            on_model_credits(&end, "sonnet", Some(reset)),
        ),
        (
            "a launch naming Sonnet",
            launched(&named, LaunchModel::Named("claude-sonnet-4-6".to_string())),
        ),
        (
            "a launch nobody read",
            launched(&named, LaunchModel::Unread),
        ),
    ] {
        let mut st = TurnEndState::default();
        let a = act(&mut st, &r, t);
        assert!(waits_until(&a, reset + GRACE), "{name}: {a:?}");
        assert_eq!(st.model_switch(), None, "{name}");
    }
    let unread = launched(&unnamed, LaunchModel::Unread);
    let unread_end = launched(&end, LaunchModel::Unread);
    let mut st = TurnEndState::default();
    let a = at(&mut st, &unread, t);
    assert!(is_escalate(&a, CREDITS_HOLD_MODEL_TOLD), "{a:?}");
    st.credits_told();
    assert_eq!(at(&mut st, &unread, t + MIN), TurnEndAction::Nothing);
    assert_eq!(
        at(&mut st, &unread_end, t + 10 * MIN),
        TurnEndAction::Nothing
    );
    assert_eq!(
        at(&mut st, &unread_end, t + 400 * MIN),
        TurnEndAction::Nothing
    );
    st.agent_started(t + 401 * MIN);
    assert_eq!(
        at(&mut st, &unread_end, t + 402 * MIN),
        typed(RULE_CONTINUE)
    );
    let mut st = TurnEndState::default();
    st.credits_told();
    at(&mut st, &on_credits(&end, None), t);
    let bound = TurnEndTiming::default().credits_bound;
    let a = at(&mut st, &end, t + MIN);
    assert!(
        waits_until(&a, t + bound),
        "credits naming no model move nothing: {a:?}"
    );
    assert_eq!(at(&mut st, &end, t + bound), typed(RULE_CONTINUE));
}

#[test]
fn a_wall_only_usage_credits_lift_falls_back_where_it_names_fable() {
    let t = t0();
    let spend_named = "You're out of usage credits. Run /usage-credits to keep using Fable 5.1 \
                       or /model to switch models.";
    let spend = |message: &str| walled(WallKind::Spend, message, Some(3 * MIN));
    let decided = |r: &TurnEndReading| {
        let mut st = TurnEndState::default();
        st.observe(r, t);
        decide_turn_end(&st, r, &cfg(), t)
    };
    let a = decided(&spend(spend_named));
    assert!(falls_back(&a), "{a:?}");
    assert!(
        is_escalate(otherwise_of(&a), "usage_credits` is off"),
        "{a:?}"
    );
    let a = decided(&bucket_of("Fable 5.1 requires usage credits", None));
    assert!(falls_back(&a), "{a:?}");
    assert!(
        is_escalate(otherwise_of(&a), "usage_credits` is off"),
        "{a:?}"
    );
    for (name, r) in [
        (
            "a spend naming no model",
            spend("You're out of usage credits"),
        ),
        (
            "a launch nobody read",
            launched(&spend(spend_named), LaunchModel::Unread),
        ),
        (
            "a spend naming Sonnet",
            spend("You're out of usage credits. Run /usage-credits to keep using Sonnet 4.6."),
        ),
    ] {
        let a = decided(&r);
        assert!(is_escalate(&a, "usage_credits` is off"), "{name}: {a:?}");
    }
}

/// A told account move's first point after a relaunch of the fallback or its return is that
/// relaunch's start-up, as after a memory restart; a relaunch not made changes nothing.
#[test]
fn a_told_moves_first_point_after_a_model_relaunch_is_its_start_up() {
    let t = t0();
    let drawn = idle("Resumed: carrying on.", Some(Duration::from_secs(2)));
    let worked = idle("Carried on: stage 3.", Some(4 * MIN));
    for why in [to_opus(), Restart::ModelBack { to: None }] {
        for told in [true, false] {
            let seeded = || {
                let mut st = TurnEndState::default();
                if told {
                    st.seed_account(
                        "work-2".to_string(),
                        AccountAt::Wall,
                        90 * MIN,
                        false,
                        true,
                        true,
                        t,
                    );
                } else {
                    st.seed_account(
                        "work-2".to_string(),
                        AccountAt::Wall,
                        MIN,
                        true,
                        false,
                        false,
                        t,
                    );
                }
                st.observe(&drawn, t);
                st
            };
            let mut st = seeded();
            st.restarted(&why, true, &drawn, t + MIN);
            assert_eq!(
                st.account_answer(&drawn),
                None,
                "{why:?} told={told}: its start-up"
            );
            st.observe(&drawn, t + 2 * MIN);
            assert!(st.account_move().is_some(), "{why:?} told={told}");
            assert_eq!(
                st.account_answer(&worked),
                Some(AccountAnswer::Served),
                "{why:?} told={told}: the point after it"
            );
            let mut st = seeded();
            st.restarted(&why, false, &drawn, t + MIN);
            assert_eq!(
                st.account_answer(&drawn),
                Some(AccountAnswer::Served),
                "{why:?} told={told}: not made"
            );
        }
    }
}

/// A Fable bucket no reset ends is an exception only while the tab cannot switch: where the
/// fallback is the session's, it says the running jobs keep it from switching to Opus; where it
/// is not (another bucket, a launch nobody read, no host), it says the limit does not end by
/// itself. A switch made, or a reset named, raises none.
#[test]
fn a_bucket_no_reset_ends_is_an_exception_only_while_the_tab_cannot_switch() {
    let t = t0();
    let r = fable(None);
    let mut st = TurnEndState::default();
    st.observe(&r, t);
    assert_eq!(exception_for(&st, &r, &cfg(), t + 59 * MIN), None);
    let held = exception_for(&st, &r, &cfg(), t + 61 * MIN).expect("held");
    assert_eq!(
        (held.kind, held.when),
        (ExceptionKind::Bucket, Urgency::Later)
    );
    assert!(
        held.why.contains("running jobs") && held.why.contains("Opus"),
        "{held:?}"
    );
    assert!(
        held.action.contains("/model") && held.action.contains("Opus"),
        "{held:?}"
    );
    let generic = |e: Option<Exception>, name: &str| {
        let e = e.unwrap_or_else(|| panic!("{name}: an exception"));
        assert!(e.why.contains("does not end by itself"), "{name}: {e:?}");
        assert!(e.action.contains("another model"), "{name}: {e:?}");
    };
    generic(
        exception_for(
            &st,
            &launched(&r, LaunchModel::Unread),
            &cfg(),
            t + 61 * MIN,
        ),
        "a launch nobody read",
    );
    let bare = TurnEndReading {
        restartable: false,
        ..r.clone()
    };
    generic(
        exception_for(&st, &bare, &cfg(), t + 61 * MIN),
        "no host to relaunch",
    );
    let mut refused = TurnEndState::default();
    refused.observe(&r, t);
    refused.restarted(&to_opus(), false, &r, t);
    generic(
        exception_for(&refused, &r, &cfg(), t + 61 * MIN),
        "a fallback refused",
    );
    let opus = bucket_of("You've reached your Opus limit · run /usage-credits", None);
    let mut st2 = TurnEndState::default();
    st2.observe(&opus, t);
    generic(
        exception_for(&st2, &opus, &cfg(), t + 61 * MIN),
        "an Opus bucket",
    );
    let mut switched = TurnEndState::default();
    switched.observe(&r, t);
    switched.restarted(&to_opus(), true, &r, t);
    assert_eq!(exception_for(&switched, &r, &cfg(), t + 61 * MIN), None);
    let named = bucket_of(
        "You've reached your Fable limit · resets 4pm",
        Some(t + 300 * MIN),
    );
    let mut st3 = TurnEndState::default();
    st3.observe(&named, t);
    assert_eq!(exception_for(&st3, &named, &cfg(), t + 120 * MIN), None);
}

/// A wake's fact never rides a relaunch of the fallback or its return: the act is left as it is.
/// NEGATIVE CONTROL: the continuation alone carries it.
#[test]
fn a_wakes_fact_leaves_the_fallbacks_relaunch_and_its_return_as_they_are() {
    use crate::supervise::config::Occupancy;
    use crate::supervise::occupancy::templates;
    use crate::supervise::occupancy::wakes_seen::{AtomKind, Declarer};
    let line = templates::wake(2, Declarer::Agent, &[AtomKind::Pid], Some((23, 40)), None);
    let grant = Occupancy::Prompt.grant().expect("prompt grants");
    let ride = RideFact::new(grant, &line, 2, 0xfeed_0002, 0);
    let st = TurnEndState::default();
    let cont = typed(RULE_CONTINUE);
    for (why, rule_id) in [
        (to_opus(), RULE_MODEL_FALLBACK),
        (Restart::ModelBack { to: None }, RULE_MODEL_RESTORE),
        (
            Restart::ModelBack {
                to: Some("claude-fable-5".to_string()),
            },
            RULE_MODEL_RESTORE,
        ),
    ] {
        let relaunch = TurnEndAction::Restart {
            why,
            rule_id,
            otherwise: Box::new(cont.clone()),
        };
        assert!(!fact_may_ride(&st, &relaunch), "{relaunch:?}");
        assert!(!carries_fact(&relaunch, ride.fact()), "{relaunch:?}");
        assert_eq!(
            fact_rides(&st, relaunch.clone(), Some(ride.fact()), &mut NoTrace),
            relaunch,
            "a relaunch is left as it is"
        );
    }
    assert!(
        carries_fact(
            &fact_rides(&st, cont.clone(), Some(ride.fact()), &mut NoTrace),
            ride.fact()
        ),
        "CONTROL: the continuation alone carries it"
    );
}
