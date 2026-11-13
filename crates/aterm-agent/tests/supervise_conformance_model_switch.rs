// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorModelSwitch` (aterm-spec `supervisor_model_switch_model`):
//! the real turn-end decider (`decide_turn_end`, with `TurnEndState`'s `observe`,
//! `acted` and `restarted`) driven along every reachable state of the model, so the
//! answer to Fable's own limit (the move onto Opus asked for until the host can make
//! it, the continuation first at the reset, a return to the launch's own model only at
//! a named reset that was ahead when the move was made or as a `/usage` sample of Fable's
//! week reads it where the notice named none, and nothing at all for a launch that names
//! another model) is the model's, state for state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    BackBy, Composer, FABLE_FALLBACK, LaunchModel, ModelSwitch, RULE_LIMIT_RESUME, Restart,
    Sampled, TurnEndAction, TurnEndReading, TurnEndState, TurnEndTiming, decide_turn_end,
    fable_rung,
};
use aterm_models_harness::supervisor_model_switch_model;
use aterm_phase::WallKind;
use aterm_spec::derive::Model;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

const MIN: Duration = Duration::from_secs(60);
const REAL_WORK: Duration = Duration::from_secs(30 * 60);
const WALL: &str =
    "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.";
/// A launch's own Fable id (`launch` 2).
const FABLE_ID: &str = "claude-fable-5-2";
/// Another model a launch names (`launch` 3): never moved.
const OTHER_ID: &str = "claude-sonnet-4-6";

/// What the model's `launch` stands for, as the host reads the launch. `mislabel` reads a launch
/// that names another model as one that names none: the negative control's broken bind.
fn launch_of(st: &State, mislabel: bool) -> LaunchModel {
    match st["launch"] {
        2 => LaunchModel::Named(FABLE_ID.to_string()),
        3 if !mislabel => LaunchModel::Named(OTHER_ID.to_string()),
        1 | 3 => LaunchModel::Unnamed,
        _ => LaunchModel::Unread,
    }
}

/// The `--model` the return puts back for the model's `launch`.
fn back_to(st: &State) -> Option<String> {
    (st["launch"] == 2).then(|| FABLE_ID.to_string())
}

/// The real policy's clocks, scaled so one model tick is one minute and the reset
/// is acted on at the reset.
fn timing() -> TurnEndTiming {
    TurnEndTiming {
        reset_grace: Duration::ZERO,
        reset_jitter: Duration::ZERO,
        window: MIN * 24 * 60,
        ..TurnEndTiming::default()
    }
}

fn base_reading() -> TurnEndReading {
    TurnEndReading {
        launch_model: LaunchModel::Unnamed,
        phase: Phase::Idle,
        authoritative: true,
        survey: false,
        wall: None,
        wall_message: String::new(),
        reset_at: None,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some("Slice done.".to_string()),
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
        worked: Some(REAL_WORK),
        rules: None,
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
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        chosen_in_transcript: None,
        conversation: aterm_agent::supervise::policy::turn_end::Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
        person: None,
        key: None,
        reach: Default::default(),
        witness: None,
        program: aterm_phase::Program::Claude,
        seat: Default::default(),
        account: None,
        route: None,
        autopilot: aterm_agent::supervise::Autopilot::Cruise,
        login_label: None,
        autopilot_by_person: false,
        login_superseded: false,
        login_notice: None,
        login_screen: None,
        login_interrupted: false,
        login_at_wall: None,
        login_method: None,
        login_shared: None,
        login_pin: aterm_agent::supervise::policy::login::LoginPin::Picker,
        login_moved: false,
        roster: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        ran_tools: false,
        tools_unread: false,
        record_turn: None,
        credits: None,
        repo: aterm_agent::harness::repo_work::RepoRead::Clean,
        leftover: aterm_agent::harness::leftover::LeftRead::NotAsked,
        credentials: aterm_agent::supervise::policy::credential_need::CredentialRead::NotAsked,
        fact: None,
        wake: None,
    }
}

/// The point the model state stands for: the wall, on the bucket's model,
/// with the reset the notice named (`named`) `ResetAt` ticks after it
/// appeared; else an idle point (the fallback at work, or the bucket's model
/// with its limit lifted).
fn reading(
    m: &Model,
    st: &State,
    appeared: Instant,
    worked: Option<Duration>,
    mislabel: bool,
) -> TurnEndReading {
    let walled = st["bucket"] == 1 && st["on"] == 0;
    if !walled {
        return TurnEndReading {
            worked,
            launch_model: launch_of(st, mislabel),
            ..base_reading()
        };
    }
    let reset =
        (st["named"] == 1).then(|| appeared + MIN * u32::try_from(cnst(m, "ResetAt")).unwrap());
    TurnEndReading {
        wall: Some(WallKind::ModelBucket { consent: false }),
        wall_message: WALL.to_string(),
        reset_at: reset,
        worked,
        launch_model: launch_of(st, mislabel),
        ..base_reading()
    }
}

#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    appeared: Instant,
    /// The strict walk's: a mirror the real switch disagrees with fails it, never prunes it.
    strict: bool,
    /// When the last return made on a sample's room left was made.
    last_room: Option<Instant>,
}

/// The instant the rung of `roomed` returns made on room left ends after the last one, as the
/// real switch paces the next ([`fable_rung`]); `roomed` 0 is no rung.
fn rung_end(real: &Real, roomed: i64) -> Instant {
    let n = u32::try_from(roomed.max(1) - 1).unwrap();
    real.last_room.map_or(real.now, |last| last + fable_rung(n))
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// The continuation typed at the reset.
    Continue,
    /// The fallback asked of the host.
    AskFallback,
    /// The return asked of the host.
    Return,
    /// Anything else: a wait, nothing, the point's own act.
    Other,
}

/// The verdict of `a` at `st`: a move asked onto anything but [`FABLE_FALLBACK`], or a return to
/// anything but the launch's own model, is no verdict the model names.
fn verdict(a: &TurnEndAction, st: &State) -> Option<Verdict> {
    Some(match a {
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_LIMIT_RESUME => Verdict::Continue,
        TurnEndAction::Restart {
            why: Restart::Model { to },
            ..
        } if to == FABLE_FALLBACK => Verdict::AskFallback,
        TurnEndAction::Restart {
            why: Restart::ModelBack { to },
            ..
        } if *to == back_to(st) => Verdict::Return,
        TurnEndAction::Restart {
            why: Restart::Model { .. } | Restart::ModelBack { .. },
            ..
        } => return None,
        _ => Verdict::Other,
    })
}

fn expected(m: &Model, st: &State) -> Verdict {
    if st["bucket"] == 1 && st["on"] == 0 {
        if m.action_enabled("Continue", st) {
            Verdict::Continue
        } else if m.action_enabled("AskFallback", st) {
            Verdict::AskFallback
        } else {
            Verdict::Other
        }
    } else if st["on"] == 1
        && ((st["back"] == 1 && st["clock"] >= cnst(m, "ResetAt")) || st["back"] == 2)
    {
        Verdict::Return
    } else {
        Verdict::Other
    }
}

fn mirror(
    m: &Model,
    action: &str,
    before: &State,
    after: &State,
    real: &mut Real,
    cfg: &SupervisorConfig,
    mislabel: bool,
) -> bool {
    let now = real.now;
    match action {
        "JobsStart" | "JobsEnd" | "LaunchUnnamed" | "LaunchFable" | "LaunchOther" => {}
        "BucketLifts" | "BucketLiftsUnnamed" => {
            if before["on"] == 0 {
                let r = reading(m, after, real.appeared, Some(REAL_WORK), mislabel);
                real.st.observe(&r, now);
            }
        }
        "Tick" => real.now += MIN,
        "Works" => {
            let r = reading(m, after, real.appeared, Some(REAL_WORK), mislabel);
            real.st.observe(&r, now);
        }
        "BucketHitsNamed" | "BucketHitsUnnamed" => {
            real.appeared = now;
            let r = reading(m, after, now, Some(REAL_WORK), mislabel);
            real.st.observe(&r, now);
        }
        "Continue" => {
            let r = reading(m, before, real.appeared, None, mislabel);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, cfg, now);
            if verdict(&a, before) != Some(Verdict::Continue) {
                return false;
            }
            real.st.acted(&a, &r, now);
            let again = reading(
                m,
                after,
                real.appeared,
                Some(Duration::from_secs(3)),
                mislabel,
            );
            real.st.observe(&again, now);
        }
        "AskFallback" => {
            let r = reading(m, before, real.appeared, None, mislabel);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, cfg, now);
            let TurnEndAction::Restart { why, .. } = &a else {
                return false;
            };
            if before["jobs"] == 0 {
                real.st.restarted(why, true, &r, now);
            }
        }
        "Return" => {
            let r = reading(m, before, real.appeared, None, mislabel);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, cfg, now);
            let TurnEndAction::Restart { why, .. } = &a else {
                return false;
            };
            if !matches!(why, Restart::ModelBack { .. }) {
                return false;
            }
            real.st.restarted(why, true, &r, now);
            if before["back"] == 2 {
                real.last_room = Some(now);
            }
            if after["bucket"] == 1 {
                real.appeared = now;
                let again = reading(m, after, now, Some(Duration::from_secs(3)), mislabel);
                real.st.observe(&again, now);
            }
        }
        // A `/usage` sample of Fable's week, told the switch as the loop tells it: a row with room
        // left (right or wrong: the bucket may stand), or one at its limit naming the reset
        // `ResetAt` ticks after the wall the model's clock counts from. In the strict walk the real switch must time the return
        // as the model does (a room-left return at once once its rung has passed, else at the
        // rung's end; the spent row's at its reset): a disagreement fails it, never prunes it.
        "SampleRoom" | "SampleSpent" => {
            let (read, want) = if action == "SampleRoom" {
                let at = if after["back"] == 2 {
                    now
                } else {
                    rung_end(real, before["roomed"])
                };
                (Some((40, None)), Sampled::Room { at })
            } else {
                // `ResetAt` ticks after the wall the model's `clock` counts from.
                let ahead = cnst(m, "ResetAt") - before["clock"];
                let reset = now + MIN * u32::try_from(ahead).unwrap();
                (Some((100, Some(reset))), Sampled::Reset { at: reset })
            };
            let got = real.st.model_sampled(read, now);
            if got != Some(want) {
                assert!(
                    !real.strict,
                    "{action} at {before:?}: {got:?}, not {want:?}"
                );
                return false;
            }
        }
        // The `Buggy = 2` model's room-left row returned on at once inside its rung: told the
        // real switch as any room-left row is, so the walk judges the real answer, not the
        // model's wish.
        "SampleUnpaced" => {
            let _ = real.st.model_sampled(Some((40, None)), now);
        }
        // The rung since the last return made on room left passes; the wall's clock moves with
        // it, so a reset it names stays the same ticks ahead.
        "RungPasses" => {
            let to = rung_end(real, before["roomed"]).max(now);
            real.appeared += to - now;
            real.now = to;
        }
        // The last return made on room left held the longest rung.
        "RoomHolds" => {
            if let Some(last) = real.last_room {
                real.now = real.now.max(last + fable_rung(u32::MAX));
            }
        }
        "GuessedReturn" | "GivesUp" | "ReturnBlocks" | "MovesAnother" => {}
        other => panic!("an action this bind does not mirror: {other}"),
    }
    true
}

fn walk(m: &Model, strict: bool, mislabel: bool) -> (usize, Vec<State>) {
    let cfg = SupervisorConfig::default();
    let base = Instant::now() + Duration::from_secs(3600);
    let init = Real {
        st: TurnEndState::new(timing()),
        now: base,
        appeared: base,
        strict,
        last_room: None,
    };
    let mut seen: BTreeSet<Vec<i64>> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    let mut disagree = Vec::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(crate::common::key(m, &st)) {
            continue;
        }
        if strict {
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
            }
        }
        let mut at = real.st.clone();
        let here = reading(m, &st, real.appeared, None, mislabel);
        at.observe(&here, real.now);
        let got = verdict(&decide_turn_end(&at, &here, &cfg, real.now), &st);
        let want = Some(expected(m, &st));
        if got != want {
            assert!(
                !strict,
                "the real decider disagrees at {st:?}: {got:?}, not {want:?}"
            );
            disagree.push(st.clone());
        }
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            if mirror(m, action.name, &st, &next, &mut r, &cfg, mislabel) {
                queue.push_back((next, r));
            }
        }
    }
    (seen.len(), disagree)
}

#[test]
fn tier1_the_real_decider_switches_and_returns_exactly_where_the_model_allows() {
    let m = supervisor_model_switch_model();
    let (states, _) = walk(&m, true, false);
    assert!(states > 50, "{states}");
}

/// Negative controls: the `Buggy = 1` model (a return guessed at a switch nothing
/// scheduled one for) and the `Buggy = 2` one (a return made on a row with room left inside the
/// rung since the last such return, which a row misread while the bucket stands turns into a
/// `NoPingPong`) are not the real decider, the latter judged by what the real switch answers that
/// row; and
/// a switch seeded with a guessed half hour's `back_at` makes the real decider return where
/// the committed switch owes none until a sample times it.
#[test]
fn the_guessed_return_and_the_old_half_hour_disagree_with_the_real_code() {
    let m = supervisor_model_switch_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let (_, off) = walk(&buggy, false, false);
    assert!(
        off.iter().any(|st| st["on"] == 0 && st["flips"] >= 2),
        "no disagreement after a guessed return: {} states",
        off.len()
    );
    let unpaced = aterm_spec::interp::with_buggy(&m, 2);
    assert_eq!(
        aterm_spec::interp::violated_invariants(&unpaced),
        std::collections::BTreeSet::from(["NoPingPong"]),
        "a room-left row returned on inside its rung ping-pongs on a misread bucket"
    );
    let (_, off) = walk(&unpaced, false, false);
    assert!(
        off.iter()
            .any(|st| st["on"] == 1 && st["back"] == 2 && st["roomed"] >= 1 && st["paced"] == 0),
        "no disagreement at a return owed inside its rung: {} states",
        off.len()
    );
    let (_, mislabelled) = walk(&m, false, true);
    assert!(
        mislabelled
            .iter()
            .any(|st| st["launch"] == 3 && st["bucket"] == 1 && st["on"] == 0),
        "a bind that reads another model's launch as none moves it, which the model forbids: {} states",
        mislabelled.len()
    );

    let cfg = SupervisorConfig::default();
    let t = Instant::now() + Duration::from_secs(3600);
    let mut st = TurnEndState::new(timing());
    st.seed_model(ModelSwitch {
        launch: None,
        from: Some("fable".to_string()),
        to: FABLE_FALLBACK.to_string(),
        back_at: Some(t + 30 * MIN),
        back_by: Some(BackBy::Notice),
        sample_at: None,
        samples: 0,
        proc: None,
        unproven: false,
    });
    let idle = base_reading();
    st.observe(&idle, t + 31 * MIN);
    let a = decide_turn_end(&st, &idle, &cfg, t + 31 * MIN);
    assert!(
        matches!(
            &a,
            TurnEndAction::Restart {
                why: Restart::ModelBack { to: None },
                ..
            }
        ),
        "the guessed half hour returns: {a:?}"
    );
    let mut committed = TurnEndState::new(timing());
    let wall = TurnEndReading {
        wall: Some(WallKind::ModelBucket { consent: false }),
        wall_message: WALL.to_string(),
        ..base_reading()
    };
    committed.observe(&wall, t);
    let a = decide_turn_end(&committed, &wall, &cfg, t);
    let TurnEndAction::Restart { why, .. } = &a else {
        panic!("the fallback is asked for: {a:?}");
    };
    committed.restarted(why, true, &wall, t);
    assert_eq!(committed.model_switch().map(|s| s.back_at), Some(None));
    let later = t + 24 * 60 * MIN;
    committed.observe(&idle, later);
    assert!(!matches!(
        decide_turn_end(&committed, &idle, &cfg, later),
        TurnEndAction::Restart {
            why: Restart::ModelBack { .. },
            ..
        }
    ));
    assert!(
        committed.model_sample_due(later),
        "a sample is owed instead"
    );
}
