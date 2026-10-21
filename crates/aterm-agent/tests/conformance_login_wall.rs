// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `HarnessLoginWall` (aterm-spec `harness_login_wall_model`): on every
//! reachable state of the model, the real code (the screen reader `aterm_phase`,
//! `upgrade::transcript_login`, `upgrade::rearmed` over `upgrade::notices_received`,
//! `upgrade::clock_held`/`clock_held_until`, `upgrade::next_step`/`announce_asks`,
//! and `decide_turn_end`) is handed what the state stands for and takes exactly the
//! step the model's guards allow.
//!
//! A state stands for a screen (`Login expired` where `wall`, `Login interrupted`
//! where dismissed, `Login successful` where `back`, else a turn that ended after
//! real work), a transcript (the latest notice answered by the wall's
//! `authentication_failed` row or by the model, the person's `/login`, the agent's
//! READY, a turn the wall answered last) and clocks. The real facts are checked
//! against the state, and the supervisor and the upgrade act exactly where
//! `TypeLogin`, `Continue`, `Announce`, `GiveUp`, `Restart` and `Elapse` are enabled.
//!
//! A renewable login: a `rostered` state's readings carry the session's renewable
//! login (`login_label`), and a `renewed` one the host's advice to move onto it.
//! There the real decider types `/login` as anywhere until a renewal stands, then
//! relaunches (`Relaunch`) and follows (`Follow`) exactly where the model does, and
//! the real `aterm harness accounts renew`, run through a recording host, checks
//! the new login in its staging home alone, never the idle one it replaces, as the
//! model's `Renew` does.
//!
//! The lost login carried (the owner's ruling of 2026-10-19): the real exception
//! tells the owner now, not escalated, exactly where `TellNow` is enabled, and the
//! real supervisor never rests at a wall with nothing typed and nobody told
//! (`Rest`); a `done` state's wall answered the owner's own prompt to a finished
//! task (the policy seeded done, the wall read with no work and the person's key
//! just before it), where the real decider types `/login` as anywhere; and for a
//! `picker` state the real gate (`TurnEndState::login_typed_at`: the picker
//! behind a `/login` this loop typed) and picker policy
//! (`policy::login::pick_method` over the measured 2.1.303 picker) choose
//! exactly where `PickMethod` is enabled: the method the session's login is,
//! only when `known`, and never on a person's own picker. A `left` state is a
//! person's own `/login` left at the lost login (`Login interrupted` with no
//! `/login` of the loop's), and, with the wall back, their next prompt answered
//! by it: the real decider types no `/login` there.
//!
//! One sign-in per login: an `elsewhere` state is another tab of the host on
//! the same login, as the host's latch hands it to the reading at the wall —
//! its sign-in standing (`LoginShared::Carried`, `elsewhere` 1) or the login
//! back there (`LoginShared::Back`, 2). The real decider types no `/login`
//! while that tab carries it (a wait with an end: no rest, the owner told
//! there), and the turn it delivers again once the login is back
//! (`login-back@v1`) is the model's `Continue` at its wall. So is the turn of
//! a tab whose own `/login` carried the sign-in, or a person's left there,
//! when the login comes back in the other tab (`BackElsewhere`): the host is
//! asked at that `/login`'s `Login interrupted` too, no notice stands, a task
//! the wall reopened is delivered all the same, and with a renewal standing
//! the tab moves onto the renewed login (`Relaunch`) as a tab that waited
//! does. The dialog of aterm's `/login` is drawn where it stands (`track` and
//! `wall`: the measured 2.1.303 picker while `picker` is 1, else the browser
//! hand-off), so the decider, the exception and the picker gate
//! ([`picker_gate`], over the host's word) are checked there: with the login
//! back elsewhere the owner is told to close it (`TellClose`), never to sign
//! in, nothing is typed into it and its picker is answered by nobody; so too
//! with a renewal made while that `/login` was up, which is followed once it
//! is left (`Relaunch` at its `Login interrupted`). The host's advice is
//! handed to the decider only where the real loop asks for it
//! (`TurnEndState::account_point`, after the point is folded in).
//!
//! Negative controls: the model with any one defect switched on (`Buggy`, and each
//! knob) disagrees with the real code somewhere.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::align::Captured;
use aterm_agent::harness::cli::{AccountsCmd, AccountsHost, Env, RenewBrowser, run_accounts};
use aterm_agent::harness::upgrade::{
    self, Agent, Facts, GAVE_UP, MAX_ASKS, Phase, REASK_S, Step, Version,
};
use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::policy::approval::Decision;
use aterm_agent::supervise::policy::exception::{Exception, Urgency, exception_for};
use aterm_agent::supervise::policy::login::{
    LoginShared, PickerGate, pick_method, picker_gate, renewal_followed,
};
use aterm_agent::supervise::policy::notice::{LOGIN_BACK_DIALOG, LOGIN_RENEWED_DIALOG};
use aterm_agent::supervise::policy::outlook::ExceptionKind;
use aterm_agent::supervise::policy::turn_end::{
    AccountAdvice, AccountAt, RULE_CONTINUE, RULE_LOGIN, RULE_LOGIN_BACK, Restart, TurnEndAction,
    TurnEndReading, TurnEndState, decide_turn_end,
};
use aterm_json::Value;
use aterm_models_harness::harness_login_wall_model;
use aterm_phase::prompt::fixtures::{LOGIN_EXPIRED, LOGIN_HANDOFF, LOGIN_PICKER, screen};
use aterm_phase::{LoginMethod, LoginScreen};
use aterm_spec::derive::Model;
use aterm_spec::interp::Resolved;

type S = BTreeMap<&'static str, i64>;

/// The recorded conversation, target and round salt.
const SESSION: &str = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
const SALT: u64 = 1_790_373_062;
/// The walk's clock: a unix second well after every stamp it writes.
const NOW: u64 = 1_790_600_000;

fn to() -> Version {
    Version::parse("2.1.283").expect("to")
}

/// Every state `m` reaches by [`Model::fire`] (each action's first successor),
/// invariants unchecked, in breadth-first order. Walked resolved
/// ([`Resolved::successors`] is [`Model::successors`] case for case, in the same
/// order, without a string-keyed environment per step): walked by name, it was
/// 1.3 s of the test's 17.7 s at a load of 20-40.
fn reachable(m: &Model) -> Vec<S> {
    let r = Resolved::new(m).expect("the login wall model resolves");
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([r.init()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in 0..m.actions.len() {
            if let Some(next) = r.successors(a, &s).into_iter().next() {
                queue.push_back(next);
            }
        }
        out.push(r.state(&s));
    }
    out
}

// ---------------------------------------------------------------- the screen

/// The recorded screen with its wall row replaced by `tail`.
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

/// A person's own `/login`, left with Esc under the wall.
const LEFT: [&str; 4] = [WALL_ROW, "", "❯ /login", "  ⎿  Login interrupted"];

/// The recorded screen with the dialog of the `/login` aterm typed at the wall
/// standing under its row, in the composer's place: the measured 2.1.303
/// method picker while `picker` is 1 (the picker of the `/login` aterm typed,
/// unanswered), else its browser hand-off — each as the measured screen draws
/// it under its own prompt's.
fn with_dialog(picker: bool) -> Vec<String> {
    let dialog = screen(if picker { LOGIN_PICKER } else { LOGIN_HANDOFF });
    let typed = dialog
        .iter()
        .position(|r| r.trim_end() == "❯ /login")
        .expect("the measured dialog's /login row");
    let mut r = screen(LOGIN_EXPIRED);
    let at = r
        .iter()
        .position(|row| row.starts_with("⏺ Login expired"))
        .expect("the wall row");
    r.truncate(at + 1);
    r.push(String::new());
    r.extend(dialog[typed..].iter().cloned());
    r
}

/// Whether the screen of state `s` is the dialog of aterm's `/login`, standing
/// under the wall's row ([`with_dialog`]).
fn dialog_up(s: &S) -> bool {
    s["wall"] == 1 && s["track"] == 1 && !(s["left"] == 1 && s["back"] == 0)
}

/// The screen state `s` stands for.
fn screen_of(s: &S) -> Vec<String> {
    if s["left"] == 1 && s["back"] == 0 {
        if s["wall"] == 1 {
            let mut tail = LEFT.to_vec();
            tail.extend(["", "❯ go on", "", WALL_ROW]);
            with_tail(&tail)
        } else {
            with_tail(&LEFT)
        }
    } else if dialog_up(s) {
        with_dialog(s["picker"] == 1)
    } else if s["wall"] == 1 {
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

/// A transcript row as Claude Code 2.1.281 writes one, in the fields the readers
/// take (the shape of `upgrade_login_wall_tests.rs`'s recorded rows).
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
    // lifted by the person's `/login` (`back`; one of its own when the rows hold
    // none), or by a turn the model answered since.
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
    /// Where an announced upgrade's window has not run out (`phase` 1,
    /// `window` 0): whether the real clock lets it run out ([`window_runs`]).
    runs: Option<bool>,
}

/// The variables [`upgrade_at`] reads (through the screen, the transcript and
/// the phase); the others are the model's own records or the supervisor's.
const UPGRADE_VARS: [&str; 11] = [
    "asks", "back", "left", "phase", "picker", "ready", "stood", "track", "unread", "wall",
    "window",
];

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
        runs: (s["phase"] == 1 && s["window"] == 0).then(|| window_runs(m, s)),
    }
}

/// The model's action a real upgrade step is.
fn upgrade_action(step: &Step) -> Option<&'static str> {
    match step {
        Step::Announce => Some("Announce"),
        Step::GiveUp => Some("GiveUp"),
        // A taskless conversation's restart without a notice (`Step::Fresh`)
        // starts the new build all the same.
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

/// The wall's first read in state `s`: over a finished task (`done`), the turn
/// the owner's prompt began, read never busy — no work, the person's key a
/// moment before it.
fn first_wall(s: &S, st: &mut TurnEndState) -> TurnEndReading {
    if s["done"] == 1 {
        assert!(st.seed_task_done(), "a task done at {s:?}");
        TurnEndReading {
            person: Some(Duration::from_secs(2)),
            ..reading(&with_tail(&[WALL_ROW]), None)
        }
    } else {
        reading(&with_tail(&[WALL_ROW]), Some(Duration::from_secs(180)))
    }
}

/// The renewable login a rostered state's session runs on.
const LABEL: &str = "m7-a";

/// The tab that carries the login's sign-in in an `elsewhere` state.
const PEER: &str = "@2";

/// `r` as the window's host hands it over for state `s`, the loop's books `st`
/// as they stood before it. At a lost login — its row, or the `/login` dialog
/// or `Login interrupted` of the loss the books track — the other tab's
/// sign-in of it (`elsewhere`: carried there, or the login back there), as the
/// host's latch answers the loop's ask. On a rostered login:
/// the session's renewable label and a host that relaunches; once renewed, the
/// process on the superseded login and the advice to move onto the renewed one
/// (what `accounts::advise` gives at a lost login or a turn end). Elsewhere no
/// host keeps a roster for it. The advice is the host's answer to the loop's
/// ask, so it is set only where the loop asks ([`advised`]).
fn hosted(s: &S, st: &TurnEndState, renewed: bool, r: TurnEndReading) -> TurnEndReading {
    let by = PEER.to_string();
    let asked = r.wall == Some(aterm_phase::WallKind::Auth)
        || (st.login_track().is_some() && (r.login_screen.is_some() || r.login_interrupted));
    let login_shared = match s["elsewhere"] {
        _ if !asked => None,
        1 => Some(LoginShared::Carried { by }),
        2 => Some(LoginShared::Back { by }),
        _ => None,
    };
    let r = TurnEndReading { login_shared, ..r };
    if s["rostered"] == 0 {
        return r;
    }
    TurnEndReading {
        login_label: Some(LABEL.to_string()),
        login_superseded: renewed,
        restartable: true,
        ..r
    }
}

/// `r` with the host's advice, as the loop hands it to the decider: asked
/// once the point is folded into `st`, and only where the loop asks
/// ([`TurnEndState::account_point`]) — once renewed, the move onto the
/// renewed login (what `accounts::advise` gives at a lost login or a turn
/// end); else nothing.
fn advised(st: &TurnEndState, renewed: bool, r: TurnEndReading) -> TurnEndReading {
    let asked = st.account_point(&r, &SupervisorConfig::default()).is_some();
    TurnEndReading {
        account: (renewed && asked).then(|| AccountAdvice::Move {
            to: LABEL.to_string(),
        }),
        ..r
    }
}

/// The approval loop's picker gate at `r` ([`picker_gate`]): behind the
/// `/login` the books `st` typed, the host's word on that login's sign-in,
/// and a renewal the tab follows ([`renewal_followed`]).
fn gate_at(st: &TurnEndState, r: &TurnEndReading) -> PickerGate {
    picker_gate(
        st.login_typed_at().is_some(),
        r.login_shared.as_ref(),
        renewal_followed(
            r.login_superseded,
            &SupervisorConfig::default(),
            r.restartable,
        ),
    )
}

/// State `s` as it stood at the loss's earlier reads ([`supervisor_at`]): the
/// login back in another tab came after them — after this tab's `/login`
/// (`BackElsewhere`), or after the person's own was left under the other
/// tab's sign-in (`OtherBack`).
fn earlier(s: &S) -> S {
    let mut then = s.clone();
    if then["elsewhere"] == 2 {
        then.insert("elsewhere", if s["track"] == 1 { 0 } else { 1 });
    }
    then
}

/// What the real supervisor decides at the point state `s` stands for, the
/// exception it raises there, and whose a picker read there is (the approval
/// loop's gate, [`picker_gate`]: behind the `/login` it typed, by
/// `login_typed_at`, and the host's word on that login's sign-in). Its policy is
/// fresh, except where the state's lost-login track stands (it typed `/login`
/// at the wall's first point), where a person's own `/login` was left there
/// (`left`), and where the wall came over a finished task (the task seeded
/// done).
fn supervisor_at(s: &S) -> (TurnEndAction, Option<Exception>, PickerGate) {
    let cfg = SupervisorConfig::default();
    let t0 = Instant::now() + Duration::from_secs(10 * 3600);
    let mut st = TurnEndState::default();
    let long = Some(Duration::from_secs(180));
    // The instant the state's own reading is a minute after: the wall's, or
    // the `/login` typed at it once the wait it decided there ended.
    let mut from = t0;
    let then = earlier(s);
    let worked = if s["track"] == 1 {
        let first = first_wall(s, &mut st);
        let first = hosted(&then, &st, false, first);
        st.observe(&first, t0);
        let (a, first, at) = settled_at(&st, &first, &cfg, t0);
        assert!(
            matches!(&a, TurnEndAction::TypeCommand { rule_id, .. } if *rule_id == RULE_LOGIN),
            "{a:?}"
        );
        st.acted(&a, &first, at);
        from = at;
        None
    } else if s["left"] == 1 && s["back"] == 0 {
        // The wall read, then the person's own `/login` left under it.
        let first = first_wall(s, &mut st);
        let first = hosted(&then, &st, false, first);
        st.observe(&first, t0);
        let gone = hosted(&then, &st, false, reading(&with_tail(&LEFT), None));
        st.observe(&gone, t0 + Duration::from_secs(30));
        assert!(st.login_cancelled(), "the person's /login left at {s:?}");
        None
    } else if s["done"] == 1 && s["wall"] == 1 {
        // The wall over the finished task, read first now.
        let r = first_wall(s, &mut st);
        let r = hosted(s, &st, s["renewed"] == 1, r);
        st.observe(&r, t0);
        let r = advised(&st, s["renewed"] == 1, r);
        return (
            settled(&st, &r, &cfg, t0),
            exception_for(&st, &r, &cfg, t0),
            gate_at(&st, &r),
        );
    } else {
        long
    };
    let now = from + Duration::from_secs(60);
    let r = hosted(s, &st, s["renewed"] == 1, reading(&screen_of(s), worked));
    st.observe(&r, now);
    let r = advised(&st, s["renewed"] == 1, r);
    (
        decide_turn_end(&st, &r, &cfg, now),
        exception_for(&st, &r, &cfg, now),
        gate_at(&st, &r),
    )
}

/// What the real decider does at `r` once a wait it decides there ends: the
/// point decided again at the wait's end, nothing more read (a person's key
/// that holds an act ages as the wait does). A wait with an end is no rest:
/// the step it ends in is the model's.
fn settled(
    st: &TurnEndState,
    r: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    settled_at(st, r, cfg, now).0
}

/// [`settled`], with the reading and the instant it was decided at.
fn settled_at(
    st: &TurnEndState,
    r: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> (TurnEndAction, TurnEndReading, Instant) {
    match decide_turn_end(st, r, cfg, now) {
        TurnEndAction::WaitUntil { until, .. } if until > now => {
            let waited = until - now;
            let aged = TurnEndReading {
                person: r.person.map(|ago| ago + waited),
                ..r.clone()
            };
            (decide_turn_end(st, &aged, cfg, until), aged, until)
        }
        decided => (decided, r.clone(), now),
    }
}

/// The model's action a real turn-end decision is.
fn supervisor_action(a: &TurnEndAction) -> Option<&'static str> {
    match a {
        TurnEndAction::TypeCommand { rule_id, .. } if *rule_id == RULE_LOGIN => Some("TypeLogin"),
        TurnEndAction::Type { rule_id, .. }
            if *rule_id == RULE_CONTINUE || *rule_id == RULE_LOGIN_BACK =>
        {
            Some("Continue")
        }
        TurnEndAction::Restart {
            why: Restart::Account { to, at },
            ..
        } if to == LABEL => Some(match at {
            AccountAt::Wall => "Relaunch",
            AccountAt::Ahead => "Follow",
        }),
        _ => None,
    }
}

/// The owner told now: the login exception, not escalated (the host's ask on
/// the glass), with the owner's four-part notice — the notice to close a
/// dialog whose login came back elsewhere ([`LOGIN_BACK_DIALOG`]) or was
/// renewed ([`LOGIN_RENEWED_DIALOG`]): `TellClose`; or any other (`TellNow`).
fn tells_now(e: Option<&Exception>) -> Option<&'static str> {
    e.filter(|e| e.kind == ExceptionKind::Login && e.when == Urgency::Now && !e.escalated)
        .and_then(|e| e.notice.as_ref())
        .map(|n| {
            if n.id == LOGIN_BACK_DIALOG.id || n.id == LOGIN_RENEWED_DIALOG.id {
                "TellClose"
            } else {
                "TellNow"
            }
        })
}

/// Whether the real picker policy chooses at the measured picker for a state
/// whose method is `known` (or not).
fn picks(known: bool) -> bool {
    let rows = screen(LOGIN_PICKER);
    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
    let prompt = read.prompt.expect("the picker is a prompt");
    let Some(LoginScreen::Picker(picker)) = aterm_phase::login_screen(&rows) else {
        panic!("the measured picker reads as the picker");
    };
    let method = known.then_some(LoginMethod::Subscription);
    matches!(
        pick_method(&prompt, &picker, method),
        Decision::Approve { .. }
    )
}

/// The variables [`supervisor_at`] reads (through the screen and the host's
/// readings); the others are the model's own records or the upgrade's.
const SUPERVISOR_VARS: [&str; 9] = [
    "back",
    "done",
    "elsewhere",
    "left",
    "picker",
    "renewed",
    "rostered",
    "track",
    "wall",
];

/// What the real supervisor decides at the look a state stands for: its
/// decision (`sup`: the model's action it is, or a rest), its exception (`ask`:
/// the owner told now, `TellNow` or `TellClose`), and the picker gate (`carry`:
/// the picker is aterm's to answer).
struct Supervised {
    decided: TurnEndAction,
    exception: Option<Exception>,
    sup: Option<&'static str>,
    ask: Option<&'static str>,
    carry: bool,
}

impl Supervised {
    fn at(s: &S) -> Self {
        let (decided, exception, gate) = supervisor_at(s);
        let carry = gate == PickerGate::Carried;
        let ask = tells_now(exception.as_ref());
        // At a wall nothing typed at, nobody told and no wait with an end: a
        // rest.
        let waits = matches!(decided, TurnEndAction::WaitUntil { .. });
        let sup = supervisor_action(&decided).or((s["wall"] == 1
            && s["track"] == 0
            && ask.is_none()
            && !waits)
            .then_some("Rest"));
        Self {
            decided,
            exception,
            sup,
            ask,
            carry,
        }
    }
}

/// The real code's decisions, kept by the variables each side reads
/// ([`UPGRADE_VARS`], [`SUPERVISOR_VARS`]) and handed only those, so a read of
/// any other panics rather than going unkeyed. Each side is pure in them (a
/// fresh screen, transcript, phase and policy per look): the 11,034 states read
/// 232 upgrades and 75 supervisors. Read at every state, they were 4.5 s of the
/// walk's 17.7 s at a load of 20-40.
#[derive(Default)]
struct Looks {
    upgrades: BTreeMap<S, Upgrade>,
    supervisors: BTreeMap<S, Supervised>,
}

impl Looks {
    fn at(&mut self, m: &Model, s: &S) -> (&Upgrade, &Supervised) {
        let only = |names: &[&'static str]| -> S { names.iter().map(|&k| (k, s[k])).collect() };
        let up = self
            .upgrades
            .entry(only(&UPGRADE_VARS))
            .or_insert_with_key(|vars| upgrade_at(m, vars));
        let sv = self
            .supervisors
            .entry(only(&SUPERVISOR_VARS))
            .or_insert_with_key(Supervised::at);
        (up, sv)
    }
}

/// Whether the model `m` agrees at `s` with what the real code decided: the
/// upgrade and its clock, the supervisor, its exception, the picker gate
/// (`carry`) and policy (`picked`, by whether the method is known), and, where
/// the model's renewal is enabled, what the real renewal touched
/// (`renew_clean`: no check under any login but its new one, the live login
/// left as it was).
fn agrees(
    m: &Model,
    s: &S,
    (up, sv): (&Upgrade, &Supervised),
    renew_clean: bool,
    picked: [bool; 2],
) -> bool {
    let Supervised {
        sup, ask, carry, ..
    } = sv;
    let taken = upgrade_action(&up.step);
    let upgrade_ok = ["Announce", "GiveUp", "Restart"]
        .iter()
        .all(|a| m.action_enabled(a, s) == (taken == Some(*a)));
    let supervisor_ok = ["TypeLogin", "Continue", "Relaunch", "Follow", "Rest"]
        .iter()
        .all(|a| m.action_enabled(a, s) == (*sup == Some(*a)));
    let ask_ok = ["TellNow", "TellClose"]
        .iter()
        .all(|a| m.action_enabled(a, s) == (*ask == Some(*a)));
    let pick_ok = s["picker"] == 0
        || m.action_enabled("PickMethod", s) == (*carry && picked[usize::from(s["known"] == 1)]);
    let renew_ok = !m.action_enabled("Renew", s) || {
        let mut next = s.clone();
        m.fire("Renew", &mut next)
            && (next["login"] == s["login"] && next["checked"] == s["checked"]) == renew_clean
    };
    // The ask the notice goes as: the same ask again, and the bound reached.
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
    let clock_ok = up
        .runs
        .is_none_or(|runs| m.action_enabled("Elapse", s) == runs);
    upgrade_ok && supervisor_ok && ask_ok && pick_ok && renew_ok && asks_ok && clock_ok
}

/// A host for the real renewal that records what it runs: its sign-in writes
/// into the staging home the address it was hinted with (the row's: the browser
/// held that account), its check answers that home's record, and an
/// `auth status` under any other login is counted as a check under an idle
/// login.
#[derive(Default)]
struct Recording {
    flows: RefCell<Vec<String>>,
    checked: RefCell<Vec<String>>,
    idle_checks: RefCell<usize>,
}

impl AccountsHost for Recording {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _login: &str) -> Result<Captured, String> {
        *self.idle_checks.borrow_mut() += 1;
        Err("an idle login is never asked".to_string())
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _dir: Option<&str>) -> Result<i32, String> {
        Err("no plain sign-in".to_string())
    }
    fn now(&self) -> i64 {
        0
    }
    fn renew_flow(&self, home: &str, email: &str, _: &RenewBrowser) -> Result<i32, String> {
        self.flows.borrow_mut().push(home.to_string());
        std::fs::write(Path::new(home).join(".claude.json"), email).map_err(|e| e.to_string())?;
        Ok(0)
    }
    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        self.checked.borrow_mut().push(home.to_string());
        let email = std::fs::read_to_string(Path::new(home).join(".claude.json"))
            .map_err(|e| e.to_string())?;
        Ok(Captured {
            stdout: format!(
                r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{email}","subscriptionType":"max"}}"#
            )
            .into_bytes(),
            code: Some(0),
        })
    }
}

/// Runs the real `aterm harness accounts renew m7-a` once, over a temp machine
/// whose roster lists that login with an email, and returns whether it was
/// clean: one sign-in and one check, both in one new staging home under the
/// logins directory, no `auth status` under any other login, and the row bound
/// to that home.
fn real_renewal_is_clean() -> bool {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-conf-login-wall-renew-")
        .tempdir()
        .expect("temp dir");
    let root = tmp.path();
    std::fs::write(
        root.join("accounts.toml"),
        format!(
            "[[account]]\nlabel = \"{LABEL}\"\nclass = \"dedicated\"\nemail = \"a@example.com\"\n"
        ),
    )
    .expect("roster");
    let env = Env {
        state: root.join("state"),
        aterm_state: None,
        cwd: root.to_path_buf(),
        home: Some(root.join("home")),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        now: 1_790_600_000,
        sid: String::new(),
        sock: None,
        config: Some(root.join("aterm.toml")),
        claude_tmp: None,
        claude_running: None,
    };
    let host = Recording::default();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_accounts(
        &AccountsCmd::Renew {
            labels: vec![LABEL.to_string()],
            due: false,
            on: None,
        },
        &env,
        &host,
        &mut out,
        &mut err,
    );
    let staging: PathBuf = env.accounts_store().logins_dir().join(format!("{LABEL}+1"));
    let staging = staging.to_string_lossy().into_owned();
    let bound = env
        .accounts_store()
        .load_logins(env.now)
        .ok()
        .and_then(|(logins, _)| logins.get(LABEL).and_then(|b| b.dir.clone()));
    format!("{code:?}") == format!("{:?}", std::process::ExitCode::SUCCESS)
        && *host.flows.borrow() == [staging.clone()]
        && *host.checked.borrow() == [staging.clone()]
        && *host.idle_checks.borrow() == 0
        && bound.as_deref() == Some(staging.as_str())
}

/// The model with one defect switched on.
fn defective() -> Vec<(&'static str, Model)> {
    let model = harness_login_wall_model();
    [
        ("Buggy", aterm_spec::interp::with_buggy(&model, 1)),
        ("Buggy2", aterm_spec::interp::with_buggy(&model, 2)),
        ("Buggy3", aterm_spec::interp::with_buggy(&model, 3)),
        ("Buggy4", aterm_spec::interp::with_buggy(&model, 4)),
        ("Buggy5", aterm_spec::interp::with_buggy(&model, 5)),
        ("Buggy6", aterm_spec::interp::with_buggy(&model, 6)),
        ("Buggy7", aterm_spec::interp::with_buggy(&model, 7)),
    ]
    .into_iter()
    .chain(
        [
            "NoGate",
            "NoRefund",
            "ClockAtWall",
            "NoSee",
            "NoHold",
            "WaitOnRoster",
            "NoFollow",
            "CheckIdle",
            "NoDoneWall",
            "GraceHolds",
            "Relogin",
            "GuessMethod",
            "ReloginOverLeft",
            "PickAnyPicker",
            "NoLatch",
            "NoCarrierBack",
            "SignInOverBack",
            "PickAfterBack",
            "NoFollowAfterLogin",
            "SignInOverRenewed",
        ]
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
    // Each defect is caught by one state where it disagrees: a defective model
    // is judged until then (each judged at every state was 11.3 s of the walk's
    // 17.7 s at a load of 20-40).
    let mut uncaught = defective();
    let mut taken = BTreeSet::new();
    let renew_clean = real_renewal_is_clean();
    let picked = [picks(false), picks(true)];
    assert_eq!(picked, [false, true], "the method known, and only then");
    assert!(
        renew_clean,
        "the real renewal checks its staging home alone"
    );
    let mut looks = Looks::default();
    for s in &states {
        let env = model.eval_env(s);
        for inv in &model.invariants {
            assert!(model.check_invariant_in(inv, &env), "{} at {s:?}", inv.name);
        }
        let (up, sv) = looks.at(&model, s);
        // The real facts are the state's (the wall's row read above the
        // dialog of aterm's `/login` too).
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
        assert!(
            agrees(&model, s, (up, sv), renew_clean, picked),
            "at {s:?}: the real upgrade took {:?} (asks {}, about {:?}), the supervisor \
             {:?} ({:?}), its exception {:?}",
            up.step,
            up.asks,
            up.asked_about,
            sv.decided,
            sv.sup,
            sv.exception
        );
        taken.extend(upgrade_action(&up.step));
        taken.extend(sv.sup);
        taken.extend(sv.ask);
        if s["picker"] != 0 && sv.carry && picked[usize::from(s["known"] == 1)] {
            taken.insert("PickMethod");
        }
        if model.action_enabled("Renew", s) {
            taken.insert("Renew");
        }
        uncaught.retain(|(_, m)| agrees(m, s, (up, sv), renew_clean, picked));
    }
    assert_eq!(
        taken,
        BTreeSet::from([
            "Announce",
            "GiveUp",
            "Restart",
            "TypeLogin",
            "Continue",
            "Relaunch",
            "Follow",
            "TellNow",
            "TellClose",
            "PickMethod",
            "Renew",
        ]),
        "every decision of the real code is taken somewhere"
    );
    let uncaught: Vec<&str> = uncaught.iter().map(|(name, _)| *name).collect();
    assert!(
        uncaught.is_empty(),
        "the real code has these defects: {uncaught:?}"
    );
}
