// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `HarnessUsageTap` (aterm-spec `harness_usage_tap_model`): on every
//! reachable state of the model, the real code (`tap::gate` at a relaunch, the
//! silence rule at the new process's first point via `tap::Silence::step` or
//! `tap::Silence::untapped`, `tap::Fresh::admit` and `accounts::note_reading` at the
//! host's read, `tap::Silence::step` at a decided point 30 s on) is handed what the
//! state stands for and takes exactly the model's step.
//!
//! A state stands for: the switch (`armed`: the gate is handed this aterm's path, or
//! none); the session's own status line (`userline`); the machine's stored login
//! (`cred`, which the gate must take alike in both its forms: the default login and
//! the login a renewal bound the machine's one account to); a process (`pid`) with
//! `api` API time and the five-hour figure `figs` (as `10 + 10·figs` percent); the
//! reading last admitted (`lpid`, `lapi`, `lfigs`), replayed into a fresh `Fresh`;
//! the figure on record (`noted`); the silence rule's strikes, replayed into a fresh
//! `Silence`; and the window's line (`said`: a `Heard::Silent` tells it, a
//! `Heard::Again` ends it). Compared: every variable in [`DECIDED`]. Attribution
//! beyond the stored login is the model's `cred`, as handed.
//!
//! Negative controls: an `admit` that takes every reading (it records the idle
//! repeat), and a silence rule that starts over on a new process without a word (the
//! window's mark outlives its process), each disagree with the model somewhere.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use aterm_agent::harness::accounts;
use aterm_agent::harness::netwatch::Source;
use aterm_agent::harness::tap::{self, Fresh, Heard, Reading, Silence, Window};
use aterm_models_harness::harness_usage_tap_model;
use aterm_spec::derive::Model;

type S = BTreeMap<&'static str, i64>;

const EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
/// The five-hour window's reset every reading names.
const RESET: i64 = 1_792_062_396;
/// The host's clock at a read.
const NOW: i64 = 1_792_050_000;

/// What the real code decides; every other variable is the model's own.
const DECIDED: [&str; 8] = [
    "tapped", "noted", "lpid", "lapi", "lfigs", "strike", "silent", "said",
];

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

fn pct(figs: i64) -> u32 {
    u32::try_from(10 + 10 * figs).expect("a percent")
}

fn reading(pid: i64, api: i64, figs: i64) -> Reading {
    Reading {
        at: NOW,
        sid: "s-7a9".to_string(),
        pid: u32::try_from(1000 + pid).expect("a pid"),
        session: None,
        login: None,
        home: None,
        api_ms: u64::try_from(100 * api).expect("api"),
        five_hour: Some(Window {
            pct: pct(figs),
            resets_at: RESET,
        }),
        seven_day: None,
        warm: None,
        cache_expires_at: None,
        version: None,
        dur_us: 0,
    }
}

/// The login a renewal bound the machine's one account to.
const RENEWED: &str = "/Users//example/.aterm/logins/main.1";

/// The real gate's word on a relaunch from `s`, which must match on the default
/// login and on the renewed one.
fn gate_taps(s: &S) -> bool {
    let argv = vec!["claude".to_string(), "--verbose".to_string()];
    let user = if s["userline"] == 1 {
        r#"{"statusLine":{"type":"command","command":"echo mine"}}"#
    } else {
        r#"{"permissions":{"allow":["Bash(ls)"]}}"#
    };
    let settings = vec![
        ("managed".to_string(), Source::Missing),
        ("user".to_string(), Source::Text(user.to_string())),
    ];
    let taps = [None, Some(RENEWED)].map(|login| {
        tap::gate(&tap::GateFacts {
            exe: (s["armed"] == 1).then(|| Path::new(EXE)),
            stored_login: s["cred"] == 1,
            login,
            lone_login: login,
            home: None,
            argv: &argv,
            settings: &settings,
            profile_store: false,
        })
        .is_some()
    });
    assert_eq!(taps[0], taps[1], "both forms of the own login: {s:?}");
    taps[0]
}

/// The real host's read from `s`, under `admit`: the decided variables after.
fn read_by(s: &S, admit: &dyn Fn(&mut Fresh, &Reading) -> bool) -> S {
    let mut fresh = Fresh::default();
    if s["lpid"] > 0 {
        assert!(admit(
            &mut fresh,
            &reading(s["lpid"], s["lapi"], s["lfigs"])
        ));
    }
    let mut out = s.clone();
    if !admit(&mut fresh, &reading(s["pid"], s["api"], s["figs"])) {
        return out;
    }
    out.insert("lpid", s["pid"]);
    out.insert("lapi", s["api"]);
    out.insert("lfigs", s["figs"]);
    if s["cred"] == 1 {
        let mut state = accounts::State::default();
        let held = accounts::Sample {
            session_pct: Some(pct(s["noted"])),
            session_reset: Some(RESET),
            at: NOW - 3600,
            ..accounts::Sample::default()
        };
        let _ = accounts::note_reading(&mut state, "main", held, NOW - 3600);
        let _ = accounts::note_reading(
            &mut state,
            "main",
            reading(s["pid"], s["api"], s["figs"]).sample(NOW),
            NOW,
        );
        let recorded = state.accounts["main"]
            .sample
            .as_ref()
            .and_then(|sample| sample.session_pct)
            .expect("a figure");
        out.insert("noted", (i64::from(recorded) - 10) / 10);
    }
    out
}

/// The real process of `s` (`1000 + pid`).
fn pid_of(s: &S) -> u32 {
    u32::try_from(1000 + s["pid"]).expect("a pid")
}

/// The real silence rule as `s` stands: its strikes, each a stale point 30 s
/// apart on `s`'s process, and the clock of its next point.
fn silence_of(s: &S) -> (Silence, i64) {
    let mut silence = Silence::default();
    let strikes = if s["silent"] == 1 { 2 } else { s["strike"] };
    for k in 0..strikes {
        let _ = silence.step(30 * k, pid_of(s), 60, true, None);
    }
    (silence, 30 * (strikes + 1))
}

/// The decided variables of the silence rule after a point at `at` on
/// process `pid` that said `heard`: `silent`, the strike (one stands where
/// one more stale point would mark it, and while it is marked: the strike
/// that marked it is kept until it is heard), and the window's line.
fn marked(out: &mut S, silence: &Silence, at: i64, pid: u32, heard: Heard) {
    let mut probe = silence.clone();
    let next = probe.step(at + 30, pid, 60, true, None);
    let strike = silence.silent() || heard == Heard::Silent || next == Heard::Silent;
    let said = match heard {
        Heard::Silent => 1,
        Heard::Again => 0,
        Heard::Same => out["said"],
    };
    out.insert("silent", i64::from(silence.silent()));
    out.insert("strike", i64::from(strike));
    out.insert("said", said);
}

/// The real silence rule's step from `s`: the decided variables after.
fn tick_by(s: &S) -> S {
    let mut out = s.clone();
    out.insert("file", 0);
    if s["tapped"] == 0 {
        // The host reads no untapped process.
        return out;
    }
    let (mut silence, at) = silence_of(s);
    let age = (s["file"] == 1).then_some(0);
    let heard = silence.step(at, pid_of(s), 60, s["served"] == 1, age);
    marked(&mut out, &silence, at, pid_of(s), heard);
    out
}

/// What the host's silence rule does at the first point of the process a
/// relaunch brought up (`pid`, carrying the tap or not), as `first`.
type First = dyn Fn(&mut Silence, i64, u32, bool) -> Heard;

/// The real host's first point of a new process: the rule's step on a
/// tapped one (up a moment, no turn served, no file yet), its
/// [`Silence::untapped`] on one that carries none.
fn first_point(silence: &mut Silence, at: i64, pid: u32, tapped: bool) -> Heard {
    if tapped {
        silence.step(at, pid, 0, false, None)
    } else {
        silence.untapped(pid)
    }
}

/// The real relaunch from `s`, its first point under `first`: the decided
/// variables after.
fn launch_by(s: &S, first: &First) -> S {
    let mut out = s.clone();
    let tapped = gate_taps(s);
    out.insert("tapped", i64::from(tapped));
    let (mut silence, at) = silence_of(s);
    let pid = u32::try_from(1000 + s["pid"] + 1).expect("a pid");
    let heard = first(&mut silence, at, pid, tapped);
    marked(&mut out, &silence, at, pid, heard);
    out
}

/// The disagreements of the real code (under `admit`) with model `m`, on
/// every state `m` reaches: `(action, state, real, model)`.
fn disagreements(
    m: &Model,
    admit: &dyn Fn(&mut Fresh, &Reading) -> bool,
    first: &First,
) -> Vec<(&'static str, S, S, S)> {
    let mut out = Vec::new();
    for s in reachable(m) {
        for action in ["Launch", "Read", "Tick30"] {
            let mut model = s.clone();
            if !m.fire(action, &mut model) {
                continue;
            }
            let real = match action {
                "Launch" => launch_by(&s, first),
                "Read" => read_by(&s, admit),
                _ => tick_by(&s),
            };
            if DECIDED.iter().any(|v| real[v] != model[v]) {
                out.push((action, s.clone(), real, model));
            }
        }
    }
    out
}

/// The real code takes the model's step on every reachable state, and both negative
/// controls disagree somewhere.
#[test]
fn the_real_tap_takes_the_models_steps_on_every_reachable_state() {
    let m = harness_usage_tap_model();
    let states = reachable(&m);
    assert!(
        states.len() > 100,
        "a walk worth the name: {}",
        states.len()
    );
    let real = |f: &mut Fresh, r: &Reading| f.admit(r);
    let wrong = disagreements(&m, &real, &first_point);
    assert!(wrong.is_empty(), "{:?}", wrong.first());
    // Every decided step is reached with each outcome.
    let reads = |m: &Model, pick: &dyn Fn(&S, &S) -> bool| {
        reachable(m).iter().any(|s| {
            let mut n = s.clone();
            m.fire("Read", &mut n) && pick(s, &n)
        })
    };
    assert!(reads(&m, &|s, n| n["noted"] > s["noted"]), "a record");
    assert!(reads(&m, &|s, n| n == s), "a stale read left alone");
    assert!(
        reachable(&m).iter().any(|s| s["silent"] == 1),
        "a silent tap"
    );
    assert!(
        reachable(&m).iter().any(|s| s["tapped"] == 1),
        "a tapped line"
    );
    let ends = |tapped: i64| {
        reachable(&m).iter().any(|s| {
            let mut n = s.clone();
            s["said"] == 1 && m.fire("Launch", &mut n) && n["said"] == 0 && n["tapped"] == tapped
        })
    };
    assert!(
        ends(1) && ends(0),
        "a told mark ended by a relaunch, both ways"
    );
    // Negative controls: the always-fresh admit; the rule started over on a new
    // process without a word.
    let always = |_: &mut Fresh, _: &Reading| true;
    let caught = disagreements(&m, &always, &first_point);
    assert!(
        caught.iter().any(|(action, ..)| *action == "Read"),
        "the always-fresh admit must disagree with the model"
    );
    let unsaid = |silence: &mut Silence, _: i64, _: u32, _: bool| {
        *silence = Silence::default();
        Heard::Same
    };
    let caught = disagreements(&m, &real, &unsaid);
    assert!(
        caught
            .iter()
            .any(|(action, s, real, _)| *action == "Launch" && s["said"] == 1 && real["said"] == 1),
        "a first point that ends the mark without a word must disagree with the model"
    );
}
