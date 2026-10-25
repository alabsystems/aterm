// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `HarnessLoginWall` (aterm-spec `harness_login_wall_model`).

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
    LoginShared, PickerGate, RULE_LOGIN_BACK_OUT, pick_method, picker_gate, renewal_followed,
};
use aterm_agent::supervise::policy::notice::LOGIN_BACK_DIALOG;
use aterm_agent::supervise::policy::outlook::ExceptionKind;
use aterm_agent::supervise::policy::turn_end::{
    AccountAdvice, AccountAt, Composer, RULE_CONTINUE, RULE_LOGIN, RULE_LOGIN_BACK, Restart,
    TurnEndAction, TurnEndReading, TurnEndState, decide_turn_end,
};
use aterm_json::Value;
use aterm_models_harness::harness_login_wall_model;
use aterm_phase::prompt::fixtures::{LOGIN_EXPIRED, LOGIN_HANDOFF, LOGIN_PICKER, screen};
use aterm_phase::{LoginMethod, LoginScreen};
use aterm_spec::derive::Model;
use aterm_spec::interp::Resolved;

type S = BTreeMap<&'static str, i64>;

const SESSION: &str = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
const SALT: u64 = 1_790_373_062;
const NOW: u64 = 1_790_600_000;

fn to() -> Version {
    Version::parse("2.1.283").expect("to")
}

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

const LEFT: [&str; 4] = [WALL_ROW, "", "❯ /login", "  ⎿  Login interrupted"];

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

fn dialog_up(s: &S) -> bool {
    s["wall"] == 1 && s["track"] == 1 && !(s["left"] == 1 && s["back"] == 0)
}

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

fn wall(at: u64) -> String {
    row(
        "assistant",
        at,
        r#""message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Login expired · Please run /login"}]},"error":"authentication_failed","isApiErrorMessage":true"#,
    )
}

fn completion(at: u64) -> String {
    user(
        at,
        "<task-notification>\n<status>completed</status>\n</task-notification>",
    )
}

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

fn lifted_at(s: &S) -> u64 {
    if s["window"] == 1 {
        NOW - REASK_S - 30
    } else {
        NOW - 30
    }
}

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

fn idle() -> Facts {
    Facts {
        status: "idle".to_string(),
        status_age_s: 3_600,
        composer_empty: true,
        quiet_s: 3_600,
        ..Facts::default()
    }
}

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
    let mut phase = upgrade::clock_held(&phase_of(m, s), &f, NOW);
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

fn upgrade_action(step: &Step) -> Option<&'static str> {
    match step {
        Step::Announce => Some("Announce"),
        Step::GiveUp => Some("GiveUp"),
        Step::Terminate | Step::Fresh => Some("Restart"),
        Step::Wait(_) | Step::Void(_) | Step::Rearm => None,
    }
}

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

fn reading(rows: &[String], worked: Option<Duration>) -> TurnEndReading {
    let r = aterm_phase::read(Some("claude"), rows, Some(2));
    TurnEndReading::of(&r, rows, false, worked, None, None)
}

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

const LABEL: &str = "m7-a";

const PEER: &str = "@2";

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

fn advised(st: &TurnEndState, renewed: bool, r: TurnEndReading) -> TurnEndReading {
    let asked = st.account_point(&r, &SupervisorConfig::default()).is_some();
    TurnEndReading {
        account: (renewed && asked).then(|| AccountAdvice::Move {
            to: LABEL.to_string(),
        }),
        ..r
    }
}

fn gate_at(st: &TurnEndState, r: &TurnEndReading) -> PickerGate {
    picker_gate(
        st.login_typed_at().is_some(),
        st.login_back_from(r.login_shared.as_ref()),
        r.login_label.is_some(),
        renewal_followed(
            r.login_superseded,
            &SupervisorConfig::default(),
            r.restartable,
        ),
    )
}

/// The state the tab walled in: another tab's sign-in was still carried there before it came
/// back, and a `/login` aterm typed was typed before the roster knew the login's row (a roster
/// row's lost login types none: `RenewAtLogin`, `EnrolAtLogin`).
fn earlier(s: &S) -> S {
    let mut then = s.clone();
    if then["elsewhere"] == 2 {
        then.insert("elsewhere", if s["track"] == 1 { 0 } else { 1 });
    }
    if then["track"] == 1 {
        then.insert("rostered", 0);
    }
    then
}

fn supervisor_at(s: &S) -> (TurnEndAction, Option<Exception>, PickerGate) {
    supervisor_drawn(s, &|r| r, false)
}

/// [`supervisor_at`] with each reading the wall's state is decided on drawn by `drawn`, and,
/// with `moved`, the tab relaunched onto the row by a move made at a wall before it (the wall the
/// state reads is then the moved-to process's own), each answer of that move a point gives told
/// before the point is decided, as the loop tells it (`account_tells`).
fn supervisor_drawn(
    s: &S,
    drawn: &dyn Fn(TurnEndReading) -> TurnEndReading,
    moved: bool,
) -> (TurnEndAction, Option<Exception>, PickerGate) {
    let cfg = SupervisorConfig::default();
    let t0 = Instant::now() + Duration::from_secs(10 * 3600);
    let mut st = TurnEndState::default();
    if moved {
        let move_to = Restart::Account {
            to: LABEL.to_string(),
            at: AccountAt::Wall,
        };
        let before = reading(&with_tail(&[WALL_ROW]), None);
        st.restarted(&move_to, true, &before, t0 - Duration::from_secs(60));
    }
    let tell = |st: &mut TurnEndState, r: &TurnEndReading| {
        if let Some(answer) = st.account_answer(r) {
            st.account_told(answer);
        }
    };
    let long = Some(Duration::from_secs(180));
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
        let first = first_wall(s, &mut st);
        let first = hosted(&then, &st, false, first);
        st.observe(&first, t0);
        let gone = hosted(&then, &st, false, reading(&with_tail(&LEFT), None));
        st.observe(&gone, t0 + Duration::from_secs(30));
        assert!(st.login_cancelled(), "the person's /login left at {s:?}");
        None
    } else if s["done"] == 1 && s["wall"] == 1 {
        let r = first_wall(s, &mut st);
        let r = drawn(hosted(s, &st, s["renewed"] == 1, r));
        tell(&mut st, &r);
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
    let r = drawn(hosted(
        s,
        &st,
        s["renewed"] == 1,
        reading(&screen_of(s), worked),
    ));
    tell(&mut st, &r);
    st.observe(&r, now);
    let r = advised(&st, s["renewed"] == 1, r);
    (
        decide_turn_end(&st, &r, &cfg, now),
        exception_for(&st, &r, &cfg, now),
        gate_at(&st, &r),
    )
}

fn settled(
    st: &TurnEndState,
    r: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    settled_at(st, r, cfg, now).0
}

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
        TurnEndAction::Cancel { rule_id } if *rule_id == RULE_LOGIN_BACK_OUT => Some("BackOut"),
        _ => None,
    }
}

fn tells_now(e: Option<&Exception>) -> Option<&'static str> {
    e.filter(|e| e.kind == ExceptionKind::Login && e.when == Urgency::Now && !e.escalated)
        .and_then(|e| e.notice.as_ref())
        .map(|n| {
            if n.id == LOGIN_BACK_DIALOG.id {
                "TellClose"
            } else {
                "TellNow"
            }
        })
}

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
        // The picker is a box: the approval loop, not the turn end, backs out of it.
        let backs = s["picker"] == 1 && dialog_up(s) && gate.backs_out();
        let ask = tells_now(exception.as_ref());
        let waits = matches!(decided, TurnEndAction::WaitUntil { .. });
        let sup = supervisor_action(&decided)
            .or(backs.then_some("BackOut"))
            .or((s["wall"] == 1 && s["track"] == 0 && ask.is_none() && !waits).then_some("Rest"));
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
/// any other panics rather than going unkeyed.
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
    let supervisor_ok = [
        "TypeLogin",
        "Continue",
        "Relaunch",
        "Follow",
        "Rest",
        "BackOut",
    ]
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
        agy_conversation: None,
        codex_home: None,
        now: 1_790_600_000,
        sid: String::new(),
        sock: None,
        config: Some(root.join("aterm.toml")),
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
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
        ("Buggy8", aterm_spec::interp::with_buggy(&model, 8)),
    ]
    .into_iter()
    .chain(
        [
            "NoGate",
            "NoRefund",
            "ClockAtWall",
            "NoSee",
            "NoHold",
            "LoginOnRoster",
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
            "SignInAsRow",
            "AskToClose",
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
            "BackOut",
            "TellNow",
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

/// A roster row's wall drawn as a sign-in menu in place of the composer
/// (`TurnEndReading::at_sign_in_menu`: no point, so nothing is typed into it), bound to the model
/// where the composer's wall is, but for the typed acts a menu cannot take: no `/login`
/// (`NoRosterRowSignedInByAPerson`), no person told (no `TellNow` or `TellClose` is enabled
/// there) and no badge, and the relaunch onto the renewed row exactly where `Relaunch` is
/// enabled, which the host's advice, asked at the menu, makes. Each state is decided twice: on
/// a tab that made no move, and on one relaunched onto the row by a move made at a wall, whose
/// menu is the moved-to login's own (`Relaunch` then `Expire` in the model), never the wall the
/// session left. Not where the login is back in another tab (`elsewhere` = 2), whose act is the
/// composer's typed continuation.
#[test]
fn a_roster_rows_sign_in_menu_conforms_to_the_login_wall_model() {
    let model = harness_login_wall_model();
    let menu = |r: TurnEndReading| TurnEndReading {
        composer: Composer::Absent,
        ..r
    };
    let mut seen = BTreeSet::new();
    let mut taken = BTreeSet::new();
    for s in reachable(&model) {
        if !(s["rostered"] == 1
            && s["wall"] == 1
            && s["track"] == 0
            && s["left"] == 0
            && s["elsewhere"] != 2)
        {
            continue;
        }
        let only: S = SUPERVISOR_VARS.iter().map(|&k| (k, s[k])).collect();
        if !seen.insert(only.clone()) {
            continue;
        }
        for act in ["TypeLogin", "TellNow", "TellClose"] {
            assert!(!model.action_enabled(act, &s), "{act} at {s:?}");
        }
        for moved in [false, true] {
            let (decided, exception, _) = supervisor_drawn(&only, &menu, moved);
            assert!(
                !matches!(
                    decided,
                    TurnEndAction::Escalate { .. }
                        | TurnEndAction::Type { .. }
                        | TurnEndAction::TypeCommand { .. }
                ),
                "at {only:?}, moved {moved}: {decided:?}"
            );
            assert_eq!(
                tells_now(exception.as_ref()),
                None,
                "at {only:?}, moved {moved}: {exception:?}"
            );
            let relaunched = supervisor_action(&decided) == Some("Relaunch");
            assert_eq!(
                relaunched,
                model.action_enabled("Relaunch", &s),
                "at {only:?}, moved {moved}: {decided:?}"
            );
            taken.insert((moved, if relaunched { "Relaunch" } else { "waits" }));
        }
    }
    assert_eq!(
        taken,
        BTreeSet::from([
            (false, "Relaunch"),
            (false, "waits"),
            (true, "Relaunch"),
            (true, "waits"),
        ]),
        "the menu both waits and relaunches somewhere, with a move made before it and without"
    );
}

/// A person told to sign a roster row in at the dialog of the `/login` aterm typed before the
/// roster named the row (the `SignInAsRow` knob: the notice aterm raised there before it backed
/// out of that dialog by itself) breaks `NoRosterRowSignedInByAPerson`, which the model as
/// written keeps.
#[test]
fn a_person_told_to_sign_a_roster_row_in_breaks_its_law() {
    let model = harness_login_wall_model();
    let law = "NoRosterRowSignedInByAPerson";
    assert!(
        !aterm_spec::interp::violated_invariants(&model).contains(law),
        "the model as written"
    );
    let told = aterm_spec::interp::with_consts(&model, &[("SignInAsRow", 1)]);
    assert!(
        aterm_spec::interp::violated_invariants(&told).contains(law),
        "the knob that tells the person"
    );
}

/// A person told to close the dialog of the `/login` aterm typed, the login back in another tab
/// (Buggy=8 and the `AskToClose` knob: the notice aterm raised there before it closed that dialog
/// by itself) breaks `NoPersonClosesAtermsDialog`, which the model as written keeps; in the model as written
/// aterm's own back-out closes that dialog wherever it stands over a login back elsewhere.
#[test]
fn a_person_told_to_close_aterms_dialog_breaks_its_law() {
    let model = harness_login_wall_model();
    let law = "NoPersonClosesAtermsDialog";
    assert!(
        !aterm_spec::interp::violated_invariants(&model).contains(law),
        "the model as written"
    );
    for (mutant, told) in [
        (
            "the knob",
            aterm_spec::interp::with_consts(&model, &[("AskToClose", 1)]),
        ),
        ("Buggy=8", aterm_spec::interp::with_buggy(&model, 8)),
    ] {
        assert!(
            aterm_spec::interp::violated_invariants(&told).contains(law),
            "{mutant} that tells the person"
        );
    }
    let back_elsewhere = reachable(&model)
        .into_iter()
        .filter(|s| dialog_up(s) && s["elsewhere"] == 2 && s["back"] == 0)
        .collect::<Vec<_>>();
    assert!(
        !back_elsewhere.is_empty(),
        "aterm's dialog over a login back elsewhere is reachable"
    );
    for s in &back_elsewhere {
        assert!(model.action_enabled("BackOut", s), "{s:?}");
        assert!(!model.action_enabled("TellClose", s), "{s:?}");
    }
}
