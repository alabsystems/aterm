// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `shell_rekey::typed` against `aterm_models_gui::typed_rekey_model`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use aterm_core::terminal::{ShellIntegrationNonce, Terminal};
use aterm_models_gui::typed_rekey_model;
use aterm_session::SessionId;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::verify::validate_transition_tiered;

use crate::shell_rekey::typed;

const OWN: [u8; 32] = [0x0D; 32];

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

    fn step(&mut self, dir: &std::path::Path, action: &str) -> bool {
        match action {
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

    fn verifies(&self, key: [u8; 32]) -> bool {
        let mut t = self.term.lock().unwrap();
        let before = t.shell_integration_dropped_count();
        t.process(format!("\x1b]133;A;id={}\x07", ShellIntegrationNonce(key).to_hex()).as_bytes());
        t.shell_integration_dropped_count() == before
    }

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

const SCHEDULES: &[&[&str]] = &[
    &["Lose", "Issue", "Read", "Settle"],
    &["Lose", "Issue", "Settle", "Read"],
    &["Keep", "Issue"],
    &["Keep", "Upgrade", "Read", "Settle"],
    &["Keep", "Upgrade", "Settle", "Read"],
];

fn first_key(path: &std::path::Path) -> Option<[u8; 32]> {
    let text = std::fs::read_to_string(path).expect("the key file");
    ShellIntegrationNonce::from_hex(text.lines().next().unwrap_or("")).map(|k| k.0)
}

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
            !model.action_enabled(schedule[k], &before) && after == before
        };
        verdicts.push((label, conforms));
    }
    verdicts
}

#[test]
fn the_real_typed_rekey_conforms_to_the_model() {
    let model = typed_rekey_model();
    for schedule in SCHEDULES {
        for (label, conforms) in replay(&model, schedule, "committed") {
            assert!(conforms, "{label} does not conform to TypedRekey");
        }
    }
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
    let upgrade = replay(&buggy, &["Keep", "Upgrade", "Read"], "buggy-upgrade");
    assert!(
        upgrade
            .iter()
            .any(|(l, ok)| l.ends_with("step Upgrade") && !ok),
        "{upgrade:?}"
    );
}
