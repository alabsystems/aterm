// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the typed re-key (`shell_rekey::typed`) to
//! `aterm_spec::derive::typed_rekey_model` (`TypedRekey`).
//!
//! Every schedule of the model is replayed on the SHIPPING code: a real engine
//! (lost nonce, or a working one), the real `typed::issue_in` writing a real
//! file in a scratch control dir — or, for `Upgrade` (2026-09-26), the real
//! `typed::issue_upgrading_in` for a working tab whose shell predates loaders —
//! the shell's typed line (read the file's first line into the key the shell
//! signs with, remove it — what `aterm_shell_integration::typed_rekey` runs,
//! and the older sweeps' key-only line too), and the real
//! `typed::settle_issued`. Each step is projected onto the model and checked as
//! a transition the committed model admits (interpreter, and `ty` wherever
//! installed); a step the real code REFUSES (a healthy tab's issue) must be
//! one the model's guard refuses too.
//!
//! `eng` — which key the engine verifies — is read by signing a mark with each
//! key and watching the drop counter. A verified mark ends a pending key's way
//! back, so the read is the LAST thing done to a replay: every projection
//! replays its prefix afresh.
//!
//! NEGATIVE CONTROL: the same real steps against `Buggy = 1` — the re-key
//! channel's semantics on a hookless shell, posture-blind, and an upgrade whose
//! first line is empty — must be rejected (the settle that takes an unread key
//! back, file and all; the upgrade that writes the shell's own key) and must
//! disagree with the real refusal of a healthy tab.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use aterm_core::terminal::{ShellIntegrationNonce, Terminal};
use aterm_session::SessionId;
use aterm_spec::derive::{Model, typed_rekey_model};
use aterm_spec::interp::{self, State};
use aterm_spec::verify::validate_transition_tiered;

use crate::shell_rekey::typed;

/// The key the shell was spawned with (and, in a working tab, the engine's).
const OWN: [u8; 32] = [0x0D; 32];

/// One replay: the engine, the shell's key, and the typed key's file.
struct Replay {
    term: Mutex<Terminal>,
    working: bool,
    sid: SessionId,
    /// The key the file's first line holds (`None`: an empty line), the file,
    /// and whether it named a loader.
    typed: Option<(Option<[u8; 32]>, PathBuf, bool)>,
    /// The key the shell took from the file's first line, when the line ran.
    shell_took: Option<Option<[u8; 32]>>,
    settled: bool,
}

impl Replay {
    fn run(dir: &std::path::Path, sid: &SessionId, prefix: &[&str]) -> Self {
        // Integrated from the start (marks require a key), holding none: the
        // model's `eng = 0` before `Lose`/`Keep` says which tab this is.
        let mut term = Terminal::new(24, 80);
        term.set_require_shell_integration_nonce(true);
        let mut r = Replay {
            term: Mutex::new(term),
            working: false,
            sid: sid.clone(),
            typed: None,
            shell_took: None,
            settled: false,
        };
        for action in prefix {
            r.step(dir, action);
        }
        r
    }

    /// Drive one action on the shipping code; `false` when the code refused it.
    fn step(&mut self, dir: &std::path::Path, action: &str) -> bool {
        match action {
            // The lost nonce is the engine as built; a working tab verifies
            // the key its shell was spawned with.
            "Lose" => true,
            "Keep" => {
                self.term.lock().unwrap().authorize_shell_integration(OWN);
                self.working = true;
                true
            }
            "Issue" => match typed::issue_in(dir, &self.term, &self.sid) {
                Ok((path, _)) => {
                    self.typed = Some((first_key(&path), path, false));
                    true
                }
                Err(_) => false,
            },
            // The typed UPGRADE of a shell from before loaders.
            "Upgrade" => {
                let up = typed::Upgrade {
                    folder: dir.join("shell-integration").join("0123456789abcdef"),
                    pointer: dir.join("integration").join(self.sid.as_str()),
                };
                match typed::issue_upgrading_in(dir, &self.term, &self.sid, Some(&up)) {
                    Ok((path, _)) => {
                        let lines = std::fs::read_to_string(&path).expect("the file");
                        let names_loader = lines.lines().nth(1).is_some();
                        self.typed = Some((first_key(&path), path, names_loader));
                        true
                    }
                    Err(_) => false,
                }
            }
            // The typed line: `read` the file's first line into the shell's
            // globals, `rm` it.
            "Read" => match &self.typed {
                Some((key, path, _)) if path.exists() => {
                    std::fs::remove_file(path).expect("rm");
                    self.shell_took = Some(*key);
                    true
                }
                _ => false,
            },
            "Settle" => {
                let settled = typed::settle_issued(&self.term, &self.sid, None);
                self.settled |= settled != typed::Settled::None;
                settled != typed::Settled::None
            }
            other => panic!("no such action {other}"),
        }
    }

    /// Whether a mark signed with `key` verifies now (drop counter unmoved).
    fn verifies(&self, key: [u8; 32]) -> bool {
        let mut t = self.term.lock().unwrap();
        let before = t.shell_integration_dropped_count();
        t.process(format!("\x1b]133;A;id={}\x07", ShellIntegrationNonce(key).to_hex()).as_bytes());
        t.shell_integration_dropped_count() == before
    }

    /// The model's state, read off the shipping code. Consumes the replay: the
    /// probe marks end a pending key's way back.
    fn project(self, phase: bool) -> State {
        let typed_key = self.typed.as_ref().and_then(|(k, _, _)| *k);
        let file = self.typed.as_ref().is_some_and(|(_, p, _)| p.exists());
        let eng = if self.verifies(OWN) {
            1
        } else if typed_key.is_some_and(|k| k != OWN && self.verifies(k)) {
            2
        } else {
            0
        };
        // A key as the model numbers it: none, the shell's own, the fresh one.
        let number = |key: Option<[u8; 32]>| match key {
            None => 0,
            Some(k) if k == OWN => 1,
            Some(_) => 2,
        };
        let mut s: State = BTreeMap::new();
        s.insert("phase", i64::from(phase));
        s.insert("working", i64::from(self.working));
        s.insert("sh", self.shell_took.map_or(1, number));
        s.insert("eng", eng);
        s.insert("prior", i64::from(self.typed.is_some() && self.working));
        s.insert("issued", i64::from(self.typed.is_some()));
        s.insert("file", i64::from(file));
        s.insert("settled", i64::from(self.settled));
        s.insert(
            "fkey",
            self.typed.as_ref().map_or(0, |(k, _, _)| number(*k)),
        );
        s.insert(
            "upg",
            i64::from(self.typed.as_ref().is_some_and(|(_, _, upg)| *upg)),
        );
        let _ = typed::settle_issued(&self.term, &self.sid, None);
        s
    }
}

/// Every schedule the model reaches, and the ones the real code refuses.
const SCHEDULES: &[&[&str]] = &[
    &["Lose", "Issue", "Read", "Settle"],
    &["Lose", "Issue", "Settle", "Read"],
    &["Keep", "Issue"],
    &["Keep", "Upgrade", "Read", "Settle"],
    &["Keep", "Upgrade", "Settle", "Read"],
];

/// The key a one-use file's FIRST line holds — what every typed line reads as
/// the shell's key — or `None` for an empty (or unreadable) line.
fn first_key(path: &std::path::Path) -> Option<[u8; 32]> {
    let text = std::fs::read_to_string(path).expect("the key file");
    ShellIntegrationNonce::from_hex(text.lines().next().unwrap_or("")).map(|k| k.0)
}

/// Replay `schedule` step by step against `model`; the refused steps and the
/// admitted ones, as `(label, conforms)`.
fn replay(model: &Model, schedule: &[&str], tag: &str) -> Vec<(String, bool)> {
    let dir = aterm_tempfile::Builder::new()
        .prefix("rk-conf")
        .tempdir()
        .expect("scratch");
    let sid = SessionId::new(format!("s-c0{:04x}", tag.len() * 16 + schedule.len()));
    let mut verdicts = Vec::new();
    for k in 0..schedule.len() {
        let before = Replay::run(dir.path(), &sid, &schedule[..k]).project(k > 0);
        let mut live = Replay::run(dir.path(), &sid, &schedule[..k]);
        let drove = live.step(dir.path(), schedule[k]);
        let after = live.project(true);
        let label = format!("{tag} {schedule:?} step {}", schedule[k]);
        let conforms = if drove {
            validate_transition_tiered(model, &[], &before, &after, Some(schedule[k]), &label).0
        } else {
            // Refused by the code: refused by the model's guard, nothing moved.
            !model.action_enabled(schedule[k], &before) && after == before
        };
        verdicts.push((label, conforms));
    }
    verdicts
}

/// The shipping typed re-key refines `TypedRekey` on every schedule: the heal
/// (the shell reads the key; the settle keeps it; shell and engine agree), the
/// take-back (settled before the line: the file removed, the lost nonce
/// restored, the late line finding nothing), and the healthy tab refused.
#[test]
fn the_real_typed_rekey_conforms_to_the_model() {
    let model = typed_rekey_model();
    for schedule in SCHEDULES {
        for (label, conforms) in replay(&model, schedule, "committed") {
            assert!(conforms, "{label} does not conform to TypedRekey");
        }
    }
    // Every model state the schedules end in satisfies every invariant.
    let dir = aterm_tempfile::Builder::new()
        .prefix("rk-conf-end")
        .tempdir()
        .expect("scratch");
    for (n, schedule) in SCHEDULES.iter().enumerate() {
        let end = Replay::run(
            dir.path(),
            &SessionId::new(format!("s-c1{n:04x}")),
            schedule,
        )
        .project(true);
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &end),
                "{schedule:?}: {} on {end:?}",
                invariant.name
            );
        }
    }
}

/// NEGATIVE CONTROL: `Buggy = 1` rejects the real settle that takes an unread
/// key back, and its posture-blind `Issue` disagrees with the real refusal of a
/// healthy tab — so the conformance above cannot pass vacuously.
#[test]
fn the_no_way_back_design_rejects_the_real_steps() {
    let buggy = interp::with_buggy(&typed_rekey_model(), 1);
    let take_back = replay(&buggy, &["Lose", "Issue", "Settle", "Read"], "buggy");
    assert!(
        take_back
            .iter()
            .any(|(l, ok)| l.ends_with("step Settle") && !ok),
        "{take_back:?}"
    );
    let healthy = replay(&buggy, &["Keep", "Issue"], "buggy-healthy");
    assert!(
        healthy
            .iter()
            .any(|(l, ok)| l.ends_with("step Issue") && !ok),
        "{healthy:?}"
    );
    // The upgrade whose first line is empty: the real one writes the shell's own
    // key, and the step is rejected there.
    let upgrade = replay(&buggy, &["Keep", "Upgrade", "Read"], "buggy-upgrade");
    assert!(
        upgrade
            .iter()
            .any(|(l, ok)| l.ends_with("step Upgrade") && !ok),
        "{upgrade:?}"
    );
}
