// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance test for `HarnessUsageTap`.
#![cfg(unix)]

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::Path;

use aterm_agent::harness::accounts;
use aterm_agent::harness::netwatch::Source;
use aterm_agent::harness::tap::{self, Beside, Fresh, Heard, Reading, Silence, Window};
use aterm_models_harness::harness_usage_tap_model;
use aterm_spec::derive::Model;

type S = BTreeMap<&'static str, i64>;

const EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
const RESET: i64 = 1_792_062_396;
const NOW: i64 = 1_792_050_000;

const DECIDED: [&str; 10] = [
    "tapped",
    "displaced",
    "noted",
    "epoch",
    "lpid",
    "lapi",
    "lfigs",
    "strike",
    "silent",
    "said",
];

/// The owner's own statusLine command in the user's `settings.json` when `userline` is 1.
const OWNER_LINE: &str = "echo mine";

fn reachable(m: &Model) -> Vec<S> {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(crate::common::key(m, &s)) {
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
            pct_x10: None,
        }),
        seven_day: None,
        warm: None,
        cache_expires_at: None,
        version: None,
        model: None,
        ctx_left: None,
        dur_us: 0,
    }
}

const RENEWED: &str = "/Users//example/.aterm/logins/main.1";

/// The value the real gate puts on a launch in state `s`.
fn real_gate(s: &S) -> Option<String> {
    let argv = vec!["claude".to_string(), "--verbose".to_string()];
    let user = if s["userline"] == 1 {
        format!(r#"{{"statusLine":{{"type":"command","command":"{OWNER_LINE}"}}}}"#)
    } else {
        r#"{"permissions":{"allow":["Bash(ls)"]}}"#.to_string()
    };
    let settings = vec![
        ("managed".to_string(), Source::Missing),
        ("user".to_string(), Source::Text(user)),
    ];
    let values = [None, Some(RENEWED)].map(|login| {
        tap::gate(&tap::GateFacts {
            exe: (s["armed"] == 1).then(|| Path::new(EXE)),
            stored_login: s["cred"] == 1,
            login,
            lone_login: login,
            home: None,
            argv: &argv,
            settings: &settings,
            user_settings: Some("user"),
            profile_store: false,
        })
    });
    assert_eq!(values[0], values[1], "both forms of the own login: {s:?}");
    values[0].clone()
}

type Gate = dyn Fn(&S) -> Option<String>;

/// What a launch's value does in state `s`: whether it taps, and whether it put the plain tap
/// over the owner's own statusLine instead of carrying it (`tap::carrying_of`).
fn gate_taps(s: &S, gate: &Gate) -> (bool, bool) {
    let value = gate(s);
    let displaced = s["userline"] == 1
        && value
            .as_deref()
            .is_some_and(|value| tap::carrying_of(value) != Some(true));
    (value.is_some(), displaced)
}

type BesideBy = dyn Fn(&Fresh, &Reading) -> Beside;

fn read_by(s: &S, admit: &dyn Fn(&mut Fresh, &Reading) -> bool, beside: &BesideBy) -> S {
    let mut fresh = Fresh::default();
    if s["lpid"] > 0 {
        assert!(admit(
            &mut fresh,
            &reading(s["lpid"], s["lapi"], s["lfigs"])
        ));
    }
    let mut out = s.clone();
    let now = reading(s["pid"], s["api"], s["figs"]);
    let said = beside(&fresh, &now);
    if !admit(&mut fresh, &now) {
        return out;
    }
    out.insert("lpid", s["pid"]);
    out.insert("lapi", s["api"]);
    out.insert("lfigs", s["figs"]);
    if s["cred"] == 1 {
        let mut state = accounts::State::default();
        let held = accounts::TapRead {
            five_hour: Some(accounts::WindowRead {
                pct_x10: pct(s["noted"]) * 10,
                reset: RESET,
                refilled: None,
            }),
            at: NOW - 3600,
            seen_before: true,
            ..accounts::TapRead::default()
        };
        let _ = accounts::note_reading(&mut state, "main", &held, NOW - 3600);
        if s["epoch"] == 1 {
            let account = state.accounts.get_mut("main").expect("held");
            let fig = account
                .windows
                .get_mut(accounts::SESSION_LIMIT)
                .expect("a figure");
            fig.epoch_at = Some(NOW - 3600);
        }
        let _ = accounts::note_reading(&mut state, "main", &now.tap_read(said, None), NOW);
        let fig = &state.accounts["main"].windows[accounts::SESSION_LIMIT];
        out.insert("noted", (i64::from(fig.pct_x10) / 10 - 10) / 10);
        out.insert("epoch", i64::from(fig.epoch_at.is_some()));
    }
    out
}

fn pid_of(s: &S) -> u32 {
    u32::try_from(1000 + s["pid"]).expect("a pid")
}

fn silence_of(s: &S) -> (Silence, i64) {
    let mut silence = Silence::default();
    let strikes = if s["silent"] == 1 { 2 } else { s["strike"] };
    for k in 0..strikes {
        let _ = silence.step(30 * k, pid_of(s), 60, true, None);
    }
    (silence, 30 * (strikes + 1))
}

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

fn tick_by(s: &S) -> S {
    let mut out = s.clone();
    out.insert("file", 0);
    if s["tapped"] == 0 {
        return out;
    }
    let (mut silence, at) = silence_of(s);
    let age = (s["file"] == 1).then_some(0);
    let heard = silence.step(at, pid_of(s), 60, s["served"] == 1, age);
    marked(&mut out, &silence, at, pid_of(s), heard);
    out
}

type First = dyn Fn(&mut Silence, i64, u32, bool) -> Heard;

fn first_point(silence: &mut Silence, at: i64, pid: u32, tapped: bool) -> Heard {
    if tapped {
        silence.step(at, pid, 0, false, None)
    } else {
        silence.untapped(pid)
    }
}

fn launch_by(s: &S, first: &First, gate: &Gate) -> S {
    let mut out = s.clone();
    let (tapped, displaced) = gate_taps(s, gate);
    out.insert("tapped", i64::from(tapped));
    out.insert("displaced", i64::from(displaced || s["displaced"] == 1));
    let (mut silence, at) = silence_of(s);
    let pid = u32::try_from(1000 + s["pid"] + 1).expect("a pid");
    let heard = first(&mut silence, at, pid, tapped);
    marked(&mut out, &silence, at, pid, heard);
    out
}

fn disagreements(
    m: &Model,
    states: &[S],
    admit: &dyn Fn(&mut Fresh, &Reading) -> bool,
    beside: &BesideBy,
    first: &First,
    gate: &Gate,
) -> Vec<(&'static str, S, S, S)> {
    let mut out = Vec::new();
    for s in states {
        for action in ["Launch", "Read", "Tick30"] {
            let mut model = s.clone();
            if !m.fire(action, &mut model) {
                continue;
            }
            let real = match action {
                "Launch" => launch_by(s, first, gate),
                "Read" => read_by(s, admit, beside),
                _ => tick_by(s),
            };
            if DECIDED.iter().any(|v| real[v] != model[v]) {
                out.push((action, s.clone(), real, model));
            }
        }
    }
    out
}

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
    let beside = |f: &Fresh, r: &Reading| f.beside(r);
    let wrong = disagreements(&m, &states, &real, &beside, &first_point, &real_gate);
    assert!(wrong.is_empty(), "{:?}", wrong.first());
    let reads = |m: &Model, pick: &dyn Fn(&S, &S) -> bool| {
        states.iter().any(|s| {
            let mut n = s.clone();
            m.fire("Read", &mut n) && pick(s, &n)
        })
    };
    assert!(reads(&m, &|s, n| n["noted"] > s["noted"]), "a record");
    assert!(reads(&m, &|s, n| n == s), "a stale read left alone");
    assert!(
        reads(&m, &|s, n| n["epoch"] > s["epoch"]
            && n["noted"] < s["noted"]),
        "a refill seen: the epoch opened, the lower figure taken"
    );
    assert!(
        reads(&m, &|s, n| s["epoch"] == 1
            && n["lpid"] != s["lpid"]
            && n["noted"] == s["noted"]),
        "a first reading under the epoch says nothing"
    );
    assert!(states.iter().any(|s| s["silent"] == 1), "a silent tap");
    assert!(states.iter().any(|s| s["tapped"] == 1), "a tapped line");
    let ends = |tapped: i64| {
        states.iter().any(|s| {
            let mut n = s.clone();
            s["said"] == 1 && m.fire("Launch", &mut n) && n["said"] == 0 && n["tapped"] == tapped
        })
    };
    assert!(
        ends(1) && ends(0),
        "a told mark ended by a relaunch, both ways"
    );
    let always = |_: &mut Fresh, _: &Reading| true;
    let caught = disagreements(&m, &states, &always, &beside, &first_point, &real_gate);
    assert!(
        caught.iter().any(|(action, ..)| *action == "Read"),
        "the always-fresh admit must disagree with the model"
    );
    let blind = |_: &Fresh, _: &Reading| Beside::default();
    let caught = disagreements(&m, &states, &real, &blind, &first_point, &real_gate);
    assert!(
        caught
            .iter()
            .any(|(action, s, real, _)| *action == "Read" && real["noted"] > s["figs"]),
        "a freshness rule blind to a refill must disagree with the model"
    );
    let unsaid = |silence: &mut Silence, _: i64, _: u32, _: bool| {
        *silence = Silence::default();
        Heard::Same
    };
    let caught = disagreements(&m, &states, &real, &beside, &unsaid, &real_gate);
    assert!(
        caught
            .iter()
            .any(|(action, s, real, _)| *action == "Launch" && s["said"] == 1 && real["said"] == 1),
        "a first point that ends the mark without a word must disagree with the model"
    );
    assert!(
        states
            .iter()
            .any(|s| s["userline"] == 1 && s["tapped"] == 1),
        "a line tapped beside the owner's own statusLine (R-12)"
    );
    let blind_to_chain = |s: &S| {
        (s["armed"] == 1 && s["cred"] == 1)
            .then(|| tap::settings_value(Path::new(EXE)))
            .flatten()
    };
    let caught = disagreements(&m, &states, &real, &beside, &first_point, &blind_to_chain);
    assert!(
        caught.iter().any(|(action, s, real, _)| *action == "Launch"
            && s["userline"] == 1
            && real["displaced"] == 1),
        "a gate that puts the plain tap over the owner's statusLine must disagree with the model"
    );
}
