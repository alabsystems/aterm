// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 for `HarnessLoginWall` (aterm-spec `harness_login_wall_model`): on
//! EVERY reachable state of the model, the REAL code — the screen reader
//! (`aterm_phase`), the upgrade's transcript fold (`upgrade::transcript_login`),
//! its give-up take-back (`upgrade::rearmed` over `upgrade::notices_received`),
//! its clocks (`upgrade::clock_held`, `upgrade::clock_held_until`), its reducer
//! (`upgrade::next_step`, `upgrade::announce_asks`) and the supervisor's
//! turn-end decider (`decide_turn_end`, with the `TurnEndState` the loop keeps)
//! — is handed what the state stands for and takes exactly the step the
//! model's guards allow.
//!
//! What a state stands for: its SCREEN — the incident's `⏺ Login expired ·
//! Please run /login` row where `wall`, the `/login` dialog's `Login
//! interrupted` under it where the row was dismissed, the person's `Login
//! successful` where `back`, else a turn that ended after three minutes of
//! work; its TRANSCRIPT — the upgrade's latest notice (the round's, for a
//! give-up), each answered by the login wall's `authentication_failed` row
//! where `unread` or by the model, the person's `/login` where `back`, the
//! agent's READY where `ready`, and a turn the wall answered last where
//! `stood`; and its clocks — an announced upgrade's window run out where
//! `window`, the login's lift recent or long past to match. The real facts
//! are read back from those (the screen's wall, the transcript's, a notice's
//! fate), every one checked against the state, and the decisions compared:
//! the supervisor types `/login` exactly where `TypeLogin` is enabled and a
//! continuation exactly where `Continue` is; the upgrade announces exactly
//! where `Announce` is (and to the ask the model's `Announce` makes: the same
//! ask, or the bound reached, alike), gives up exactly where `GiveUp` is,
//! takes the restart exactly where `Restart` is, and lets its window run out
//! exactly where `Elapse` may.
//!
//! NEGATIVE CONTROLS: the model with any ONE defect switched on — the
//! incident's code (`Buggy`), and each knob — disagrees with the real code
//! somewhere, so a green walk is not vacuous.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::upgrade::{
    self, Agent, Facts, GAVE_UP, MAX_ASKS, Phase, REASK_S, Step, Version,
};
use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::policy::turn_end::{
    RULE_CONTINUE, RULE_LOGIN, TurnEndAction, TurnEndReading, TurnEndState, decide_turn_end,
};
use aterm_json::Value;
use aterm_phase::prompt::fixtures::{LOGIN_EXPIRED, screen};
use aterm_spec::derive::{Model, harness_login_wall_model};

type S = BTreeMap<&'static str, i64>;

/// The incident's conversation, target and round salt.
const SESSION: &str = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
const SALT: u64 = 1_790_373_062;
/// The walk's clock: a unix second well after every stamp it writes.
const NOW: u64 = 1_790_600_000;

fn to() -> Version {
    Version::parse("2.1.283").expect("to")
}

/// Every state `m` reaches, invariants unchecked.
fn reachable(m: &Model) -> Vec<S> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            let mut next = s.clone();
            if m.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        out.push(s);
    }
    out
}

// ---------------------------------------------------------------- the screen

/// The incident's screen with its wall row replaced by `tail`.
fn with_tail(tail: &[&str]) -> Vec<String> {
    let mut r = screen(LOGIN_EXPIRED);
    let at = r
        .iter()
        .position(|row| row.starts_with("⏺ Login expired"))
        .expect("the wall row");
    r.splice(at..=at, tail.iter().map(|s| (*s).to_string()));
    r
}

const WALL_ROW: &str = "⏺ Login expired · Please run /login";

/// The screen state `s` stands for.
fn screen_of(s: &S) -> Vec<String> {
    if s["wall"] == 1 {
        with_tail(&[WALL_ROW])
    } else if s["back"] == 1 {
        with_tail(&[WALL_ROW, "", "❯ /login", "  ⎿  Login successful"])
    } else if s["track"] == 1 {
        with_tail(&[WALL_ROW, "", "❯ /login", "  ⎿  Login interrupted"])
    } else {
        with_tail(&[
            "⏺ Stage done: the suites are green.",
            "",
            "✻ Worked for 3m 2s · done 9:31 PM",
        ])
    }
}

// ---------------------------------------------------------------- the transcript

/// A transcript row as Claude Code 2.1.281 writes one, in the fields the
/// readers take (the measured rows' shape: `upgrade_login_wall_tests.rs`).
fn row(kind: &str, at: u64, body: &str) -> String {
    let stamp = aterm_agent::harness::usage::rfc3339_utc(i64::try_from(at).expect("at"));
    format!(
        r#"{{"isSidechain":false,"type":"{kind}",{body},"timestamp":"{stamp}","sessionId":"{SESSION}","version":"2.1.281"}}"#
    )
}

fn user(at: u64, text: &str) -> String {
    let text = aterm_json::to_string(&Value::from(text)).expect("json");
    row(
        "user",
        at,
        &format!(r#""message":{{"role":"user","content":{text}}}"#),
    )
}

fn agent(at: u64, text: &str) -> String {
    let text = aterm_json::to_string(&Value::from(text)).expect("json");
    row(
        "assistant",
        at,
        &format!(
            r#""message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{text}}}]}}"#
        ),
    )
}

/// The login wall's row: `<synthetic>`, `isApiErrorMessage`,
/// `authentication_failed`.
fn wall(at: u64) -> String {
    row(
        "assistant",
        at,
        r#""message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Login expired · Please run /login"}]},"error":"authentication_failed","isApiErrorMessage":true"#,
    )
}

/// A turn nobody typed (a background completion): no direction.
fn completion(at: u64) -> String {
    user(
        at,
        "<task-notification>\n<status>completed</status>\n</task-notification>",
    )
}

/// The ask `k`'s marker.
fn marker(k: u32) -> String {
    upgrade::ready_marker(SESSION, &to(), SALT + u64::from(k))
}

fn notice(at: u64, k: u32) -> String {
    user(
        at,
        &upgrade::prepare_prompt(
            &Version::parse("2.1.281").expect("from"),
            &to(),
            upgrade::Source::Managed,
            &marker(k),
        ),
    )
}

/// The real ask of the model's `asks` (the bound scaled: the model's
/// `MaxAsks` is the real `MAX_ASKS`, the ask before it the one before).
fn real_asks(m: &Model, asks: i64) -> u32 {
    let bound = m
        .consts
        .iter()
        .find(|(n, _)| *n == "MaxAsks")
        .map(|(_, v)| *v)
        .expect("MaxAsks");
    match asks {
        0 => 0,
        a if a >= bound => MAX_ASKS,
        a => MAX_ASKS - u32::try_from(bound - a).expect("asks"),
    }
}

/// When the lift the state knows of was: long past where the window has run
/// out since, else a moment ago.
fn lifted_at(s: &S) -> u64 {
    if s["window"] == 1 {
        NOW - REASK_S - 30
    } else {
        NOW - 30
    }
}

/// The transcript state `s` stands for, and the latest notice's marker.
fn transcript_of(m: &Model, s: &S) -> (String, String) {
    let mut rows = Vec::new();
    let asks = real_asks(m, s["asks"]);
    let latest = match s["phase"] {
        1 => {
            let at = NOW - 2 * REASK_S;
            rows.push(notice(at, asks));
            rows.push(if s["unread"] == 1 {
                wall(at + 1)
            } else {
                agent(at + 5, "Winding down; the suites still run.")
            });
            marker(asks)
        }
        2 => {
            // The round: every notice the wall's (a give-up taken back), or
            // every one read (a give-up of this build's).
            let at = NOW - 3 * REASK_S;
            for k in 1..=MAX_ASKS {
                let t = at + u64::from(k) * 60;
                rows.push(notice(t, k));
                rows.push(if s["unread"] == 1 {
                    wall(t + 1)
                } else {
                    agent(t + 5, "Not yet: the suites still run.")
                });
            }
            marker(MAX_ASKS)
        }
        _ => String::new(),
    };
    // A wall in the rows so far is history unless the state says it stands:
    // lifted by the person's `/login` (`back`), or by a turn the model
    // answered since. The person's `/login` lifts a wall — one of its own
    // when the rows hold none.
    let walled_before = rows.iter().any(|r| r.contains("authentication_failed"));
    let t = lifted_at(s);
    if s["back"] == 1 {
        if !walled_before {
            rows.push(completion(t - 20));
            rows.push(wall(t - 19));
        }
        rows.push(user(t, "<command-name>/login</command-name>"));
        rows.push(user(
            t,
            "<local-command-stdout>Login successful</local-command-stdout>",
        ));
    } else if walled_before && s["stood"] == 0 {
        rows.push(user(t - 5, "continue"));
        rows.push(agent(t, "Back at it: the suites are running."));
    }
    if s["ready"] == 1 {
        rows.push(agent(NOW - 20, &format!("All committed.\n\n{latest}")));
    }
    let walled_last = rows
        .last()
        .is_some_and(|r| r.contains("authentication_failed"));
    if s["stood"] == 1 && !walled_last {
        rows.push(completion(NOW - 10));
        rows.push(wall(NOW - 9));
    }
    (rows.join("\n") + "\n", latest)
}

// ---------------------------------------------------------------- the real side

/// A settled, idle Claude at an empty composer, nobody at it.
fn idle() -> Facts {
    Facts {
        status: "idle".to_string(),
        status_age_s: 3_600,
        composer_empty: true,
        quiet_s: 3_600,
        ..Facts::default()
    }
}

/// The real phase state `s` stands for.
fn phase_of(m: &Model, s: &S) -> Phase {
    match s["phase"] {
        0 => Phase::Pending,
        1 => Phase::Announced {
            at_s: if s["window"] == 1 {
                NOW - REASK_S - 1
            } else {
                NOW - 60
            },
            asks: real_asks(m, s["asks"]),
        },
        2 => Phase::Failed(GAVE_UP.to_string()),
        _ => Phase::Done,
    }
}

/// What the real upgrade decides at a look at state `s` — the driver's
/// fold, step by step — and the real facts it read.
struct Upgrade {
    step: Step,
    /// The ask the notice goes as ([`upgrade::announce_asks`]).
    asks: u32,
    /// The phase it was asked about (a give-up taken back, or its own).
    asked_about: Phase,
    login: bool,
    screen_wall: bool,
    stands: bool,
    undelivered: bool,
}

fn upgrade_at(m: &Model, s: &S) -> Upgrade {
    let rows = screen_of(s);
    let (tail, marker) = transcript_of(m, s);
    let screen_wall = upgrade::login_wall(Agent::Claude, &rows);
    let mut f = Facts {
        login: screen_wall,
        ..idle()
    };
    // `look`: the clock held at a look that finds the wall on the screen.
    let mut phase = upgrade::clock_held(&phase_of(m, s), &f, NOW);
    // `login_facts`: the transcript's word, the lift, the notice's fate.
    let said = upgrade::transcript_login(&phase, &marker, &tail);
    if said.stands {
        f.login = true;
        phase = upgrade::clock_held(&phase, &f, NOW);
    }
    if let Some(lifted) = said.lifted_at {
        phase = upgrade::clock_held_until(&phase, lifted);
    }
    let gave_up = matches!(&phase, Phase::Failed(w) if w == GAVE_UP);
    f.undelivered = if gave_up {
        upgrade::notice_fate(&tail, &marker) == Some(true)
    } else {
        said.undelivered == Some(true)
    };
    let undelivered = f.undelivered;
    let rearmed = gave_up
        .then(|| {
            let round = upgrade::round_markers(SESSION, &to(), SALT);
            upgrade::rearmed(
                &phase,
                f.undelivered,
                upgrade::notices_received(&tail, &round),
            )
        })
        .flatten();
    let ready = upgrade::transcript_has_ready(&tail, &marker);
    let asked_about = match rearmed {
        Some(p) if !ready => {
            f.undelivered = false;
            p
        }
        _ => phase,
    };
    Upgrade {
        step: upgrade::next_step(&asked_about, &f, ready, NOW),
        asks: upgrade::announce_asks(&asked_about, f.undelivered),
        asked_about,
        login: f.login,
        screen_wall,
        stands: said.stands,
        undelivered,
    }
}

/// The model's action a real upgrade step is.
fn upgrade_action(step: &Step) -> Option<&'static str> {
    match step {
        Step::Announce => Some("Announce"),
        Step::GiveUp => Some("GiveUp"),
        // A taskless conversation's restart without a notice (main's
        // `Step::Fresh`) starts the new build all the same.
        Step::Terminate | Step::Fresh => Some("Restart"),
        // A stopped round's new one (`Step::Rearm`) is the never-strands
        // model's: no round here has rested `RETRY_S` (`Facts::failed_s` 0).
        Step::Wait(_) | Step::Void(_) | Step::Rearm => None,
    }
}

/// Whether the real clock lets an announced upgrade's window run out at the
/// look `s` stands for: a look a full window on still finds the notice where
/// it was typed.
fn window_runs(m: &Model, s: &S) -> bool {
    let rows = screen_of(s);
    let (tail, marker) = transcript_of(m, s);
    let f = Facts {
        login: upgrade::login_wall(Agent::Claude, &rows)
            || upgrade::transcript_login(&phase_of(m, s), &marker, &tail).stands,
        ..idle()
    };
    let t0 = NOW - 60;
    let asked = Phase::Announced {
        at_s: t0,
        asks: real_asks(m, s["asks"]),
    };
    matches!(
        upgrade::clock_held(&asked, &f, t0 + REASK_S),
        Phase::Announced { at_s, .. } if at_s == t0
    )
}

/// The reading of a real screen, `worked` the busy work since the last point.
fn reading(rows: &[String], worked: Option<Duration>) -> TurnEndReading {
    let r = aterm_phase::read(Some("claude"), rows, Some(2));
    TurnEndReading::of(&r, rows, false, worked, None, None)
}

/// What the real supervisor decides at the point state `s` stands for: a
/// fresh policy, or — where the state's lost-login track stands — the one
/// that typed `/login` at the wall's first point.
fn supervisor_at(s: &S) -> TurnEndAction {
    let cfg = SupervisorConfig::default();
    let t0 = Instant::now() + Duration::from_secs(10 * 3600);
    let mut st = TurnEndState::default();
    let long = Some(Duration::from_secs(180));
    let worked = if s["track"] == 1 {
        let first = reading(&with_tail(&[WALL_ROW]), long);
        st.observe(&first, t0);
        let a = decide_turn_end(&st, &first, &cfg, t0);
        assert!(
            matches!(&a, TurnEndAction::TypeCommand { rule_id, .. } if *rule_id == RULE_LOGIN),
            "{a:?}"
        );
        st.acted(&a, &first, t0);
        None
    } else {
        long
    };
    let now = t0 + Duration::from_secs(60);
    let r = reading(&screen_of(s), worked);
    st.observe(&r, now);
    decide_turn_end(&st, &r, &cfg, now)
}

/// The model's action a real turn-end decision is.
fn supervisor_action(a: &TurnEndAction) -> Option<&'static str> {
    match a {
        TurnEndAction::TypeCommand { rule_id, .. } if *rule_id == RULE_LOGIN => Some("TypeLogin"),
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_CONTINUE => Some("Continue"),
        _ => None,
    }
}

/// Whether the model `m` agrees at `s` with what the real code decided.
fn agrees(m: &Model, s: &S, up: &Upgrade, sup: Option<&'static str>) -> bool {
    let taken = upgrade_action(&up.step);
    let upgrade_ok = ["Announce", "GiveUp", "Restart"]
        .iter()
        .all(|a| m.action_enabled(a, s) == (taken == Some(*a)));
    let supervisor_ok = ["TypeLogin", "Continue"]
        .iter()
        .all(|a| m.action_enabled(a, s) == (sup == Some(*a)));
    // The ask the notice goes as: the same ask again, and the bound reached,
    // alike.
    let asks_ok = taken != Some("Announce") || {
        let mut next = s.clone();
        let fired = m.fire("Announce", &mut next);
        let before = match &up.asked_about {
            Phase::Announced { asks, .. } => *asks,
            _ => 0,
        };
        fired
            && (next["asks"] == s["asks"]) == (up.asks == before && before > 0)
            && (real_asks(m, next["asks"]) == MAX_ASKS) == (up.asks >= MAX_ASKS)
    };
    let clock_ok = !(s["phase"] == 1 && s["window"] == 0)
        || m.action_enabled("Elapse", s) == window_runs(m, s);
    upgrade_ok && supervisor_ok && asks_ok && clock_ok
}

/// The model with one defect switched on.
fn defective() -> Vec<(&'static str, Model)> {
    let model = harness_login_wall_model();
    std::iter::once(("Buggy", aterm_spec::interp::with_buggy(&model, 1)))
        .chain(
            ["NoGate", "NoRefund", "ClockAtWall", "NoSee", "NoHold"]
                .into_iter()
                .map(|knob| (knob, aterm_spec::interp::with_consts(&model, &[(knob, 1)]))),
        )
        .collect()
}

#[test]
fn the_real_code_conforms_to_the_login_wall_model() {
    let model = harness_login_wall_model();
    let states = reachable(&model);
    assert!(states.len() > 100, "{} states", states.len());
    let defective = defective();
    let mut disagree: BTreeMap<&str, usize> = defective.iter().map(|(n, _)| (*n, 0)).collect();
    let mut taken = BTreeSet::new();
    for s in &states {
        for inv in &model.invariants {
            assert!(model.check_invariant(inv.name, s), "{} at {s:?}", inv.name);
        }
        let up = upgrade_at(&model, s);
        // The real facts are the state's.
        assert_eq!(up.screen_wall, s["wall"] == 1, "the screen's wall at {s:?}");
        assert_eq!(up.stands, s["stood"] == 1, "the transcript's wall at {s:?}");
        assert_eq!(
            up.login,
            s["wall"] == 1 || s["stood"] == 1,
            "the login fact at {s:?}"
        );
        if s["phase"] == 1 || s["phase"] == 2 {
            assert_eq!(
                up.undelivered,
                s["unread"] == 1,
                "the notice's fate at {s:?}"
            );
        }
        let sup = supervisor_action(&supervisor_at(s));
        assert!(
            agrees(&model, s, &up, sup),
            "at {s:?}: the real upgrade took {:?} (asks {}, about {:?}), the supervisor {sup:?}",
            up.step,
            up.asks,
            up.asked_about
        );
        taken.extend(upgrade_action(&up.step));
        taken.extend(sup);
        for (name, m) in &defective {
            if !agrees(m, s, &up, sup) {
                *disagree.get_mut(name).expect("a knob") += 1;
            }
        }
    }
    assert_eq!(
        taken,
        BTreeSet::from(["Announce", "GiveUp", "Restart", "TypeLogin", "Continue"]),
        "every decision of the real code is taken somewhere"
    );
    for (name, n) in &disagree {
        assert!(
            *n > 0,
            "the real code has the `{name}` defect: {disagree:?}"
        );
    }
}
