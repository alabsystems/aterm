// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `NativeUpdateHistoryCarry`
//! (`aterm_spec::derive::native_update_history_carry_model`): a self-update
//! carries every history line of a tab, or counts it.
//!
//! Every path of the machine — the export, up to two lines of output and a
//! rewrap in any order, the park, a sidecar corrupted on disk or not, a
//! scrollback the successor's pane clears before the import or not, and the
//! successor's import — is driven through the REAL code on real engines:
//! `HistoryExporter` writes the sidecar from a live `Terminal`, the park is
//! `Terminal::checkpoint_carry` (at the machine's `Cap`) plus
//! [`capture_head`], the join is [`stamp_manifest`] (which calls [`join`]),
//! the successor's take-before-proof is [`incoming`], its adopt is the real
//! `spawn::hydrate_adopted_engine` (the restore, the keys reserved and the
//! claim kept), and its import is [`run_imports`]. The environment is real
//! too: output is `Terminal::process`, the rewrap `Terminal::resize` one
//! column narrower, the corruption one flipped byte in the named sidecar, and
//! the clear an `ESC [3J` the adopted engine processes. The export runs
//! without FOLLOWING (the fork lane's shape): the machine's `Export` is where
//! the export's last look ended, and the launched lane's following — which
//! only moves that point later — is pinned by
//! `a_following_export_takes_what_lands_after_its_first_pass`.
//!
//! Projection, after every step, onto the machine's variables: `exp` — the
//! export's fenced line count; `total` — the parent's history at the park;
//! `carried` — [`carried_history`] of the checkpoint; `take` — what the join
//! named; `dropped` — the record's `history_dropped`; `held` — the
//! successor's history, counted after the import; `failed` — the report's
//! `failed_lines`. Where nothing was lost the successor's history is also
//! compared with the parent's LINE FOR LINE — a count alone could hide the
//! wrong lines.
//!
//! NEGATIVE CONTROL: the machine's `Buggy = 1` member (a fallback that counts
//! nothing, a sidecar named over a moved history, a failed sidecar dropped
//! without a count, a cleared history put back) is replayed along the same
//! real paths, and on each of its four defect paths the real code lands
//! somewhere the buggy machine does not — so a pass here is never vacuous.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_core::terminal::{Terminal, TerminalCheckpoint};
use aterm_spec::derive::Model;

use super::*;
use crate::session_store::{SessionHandoff, SessionRecord};

const ROWS: u16 = 4;
const COLS: u16 = 40;
const NONCE: &str = "0123456789abcdef0123456789abcdef";

type State = BTreeMap<&'static str, i64>;

/// The real system along one path.
struct Real {
    dir: aterm_tempfile::TempDir,
    parent: Arc<Mutex<Terminal>>,
    cap: usize,
    late: usize,
    results: Option<ExportResults>,
    exp: i64,
    park: Option<(TerminalCheckpoint, SessionHandoff, i64)>,
    /// The successor's adopted engine and what it brought, from its first
    /// step after the park that needs it (the clear, or the settle).
    successor: Option<(Arc<Mutex<Terminal>>, AdoptedHistory)>,
    settled: Option<(Vec<String>, ImportReport)>,
}

/// One of the machine's constants, by name.
fn constant(model: &Model, name: &str) -> i64 {
    model
        .consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("the machine declares `{name}`"))
}

fn history(t: &Terminal) -> Vec<String> {
    let grid = t.main_grid();
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

fn record() -> SessionRecord {
    SessionRecord {
        local_id: 1,
        sid: "s-0001".to_string(),
        parent: None,
        state: "alive".to_string(),
        title: String::new(),
        screen: None,
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        questions: None,
        control: None,
        frozen_path: false,
        identity: None,
        topics: Vec::new(),
        fg_holder: None,
        rekey: false,
        loader: false,
        history: None,
        history_dropped: 0,
        history_withheld: false,
        history_lost: 0,
        hold: None,
        supervisor: None,
        claim_known: false,
        attention_owners: Vec::new(),
        viewport_from_bottom: None,
        fabric: Default::default(),
        timeline_id: 0,
    }
}

impl Real {
    /// A live session whose history holds exactly the machine's `Deep` lines.
    fn new(model: &Model) -> Self {
        let deep = usize::try_from(constant(model, "Deep")).expect("a count");
        let mut t = Terminal::new(ROWS, COLS);
        for i in 0..usize::from(ROWS) - 1 + deep {
            t.process(format!("old{i}\r\n").as_bytes());
        }
        assert_eq!(history(&t).len(), deep, "the rig starts at `Deep` lines");
        Self {
            dir: aterm_tempfile::tempdir().expect("a scratch dir"),
            parent: Arc::new(Mutex::new(t)),
            cap: usize::try_from(constant(model, "Cap")).expect("a count"),
            late: 0,
            results: None,
            exp: 0,
            park: None,
            successor: None,
            settled: None,
        }
    }

    /// The successor's take-before-proof and adopt, once: the sidecar opened
    /// (and unlinked) by [`incoming`], the engine hydrated by the real seam.
    fn successor(&mut self) -> &mut (Arc<Mutex<Terminal>>, AdoptedHistory) {
        if self.successor.is_none() {
            let (checkpoint, manifest, _) = self.park.as_ref().expect("parked");
            let mut remaining = MAX_AGGREGATE_BYTES;
            let mut adopted = incoming(
                &manifest.sessions[0],
                false,
                &self.named(),
                self.dir.path(),
                &mut remaining,
            );
            let term = Arc::new(Mutex::new(Terminal::new(checkpoint.rows, checkpoint.cols)));
            crate::spawn::hydrate_adopted_engine(
                &term,
                Some(checkpoint),
                None,
                None,
                None,
                1,
                &mut adopted,
            );
            self.successor = Some((term, adopted));
        }
        self.successor.as_mut().expect("adopted")
    }

    fn named(&self) -> std::path::PathBuf {
        self.dir
            .path()
            .join(format!("seamless-{}-{NONCE}.s1.hist", std::process::id()))
    }

    /// Drive one action through the real code.
    fn step(&mut self, action: &str) {
        match action {
            "Export" => {
                let results = HistoryExporter::start(
                    self.dir.path().to_path_buf(),
                    vec![(1, Arc::clone(&self.parent))],
                    false,
                )
                .expect("the export worker starts")
                .finish(Duration::from_secs(30));
                self.exp = results
                    .exports
                    .iter()
                    .map(|e| i64::try_from(e.facts.lines).expect("a count"))
                    .sum();
                self.results = Some(results);
            }
            "Output" => {
                self.parent
                    .lock()
                    .unwrap()
                    .process(format!("late{}\r\n", self.late).as_bytes());
                self.late += 1;
            }
            "Rewrap" => self.parent.lock().unwrap().resize(ROWS, COLS - 1),
            "Park" => {
                let (checkpoint, head, total) = {
                    let t = self.parent.lock().unwrap();
                    (
                        t.checkpoint_carry(self.cap).expect("a Ground parser"),
                        capture_head(1, &t),
                        i64::try_from(history(&t).len()).expect("a count"),
                    )
                };
                let mut manifest = SessionHandoff {
                    schema: SessionHandoff::SCHEMA,
                    sessions: vec![record()],
                    window: None,
                    connections: Vec::new(),
                    next_turn_id: None,
                    outgoing_build: None,
                    held: Vec::new(),
                    roster_seq: None,
                    fabric_attached: Vec::new(),
                };
                stamp_manifest(
                    &mut manifest,
                    &[(1, checkpoint.clone())],
                    &[head],
                    self.results.take().expect("exported"),
                    self.dir.path(),
                    NONCE,
                );
                self.park = Some((checkpoint, manifest, total));
            }
            "Corrupt" => {
                let path = self.named();
                let mut bytes = std::fs::read(&path).expect("the named sidecar");
                let at = bytes.len() - 1;
                bytes[at] ^= 0x01;
                std::fs::write(&path, bytes).expect("rewrite the sidecar");
            }
            "Clear" => {
                let (term, _) = self.successor();
                term.lock().unwrap().process(b"\x1b[3J");
            }
            "Settle" => {
                self.successor();
                let (successor, adopted) = self.successor.take().expect("adopted");
                let report = run_imports(vec![ImportJob {
                    session: 1,
                    term: Arc::clone(&successor),
                    history: adopted,
                }])
                .remove(0);
                let held = history(&successor.lock().unwrap());
                self.settled = Some((held, report));
            }
            other => panic!("the rig has no step for {other}"),
        }
    }
}

/// THE PROJECTION: the real system's facts, under the machine's names, for
/// every variable the steps so far have defined.
fn project(real: &Real) -> State {
    let mut s = State::new();
    s.insert("exp", real.exp);
    if let Some((checkpoint, manifest, total)) = &real.park {
        let record = &manifest.sessions[0];
        s.insert("total", *total);
        s.insert(
            "carried",
            i64::try_from(carried_history(checkpoint)).expect("a count"),
        );
        s.insert(
            "take",
            record
                .history
                .as_deref()
                .and_then(parse_stamp)
                .map_or(0, |(_, _, take)| i64::try_from(take).expect("a count")),
        );
        s.insert(
            "dropped",
            i64::try_from(record.history_dropped).expect("a count"),
        );
    }
    if let Some((held, report)) = &real.settled {
        s.insert("held", i64::try_from(held.len()).expect("a count"));
        s.insert(
            "failed",
            i64::try_from(report.failed_lines).expect("a count"),
        );
    }
    s
}

/// Every path from the initial state to a settled one (the stutter left out).
fn paths(model: &Model) -> Vec<Vec<&'static str>> {
    let mut out = Vec::new();
    let mut stack = vec![(model.init_state(), Vec::new())];
    while let Some((state, path)) = stack.pop() {
        if state["phase"] == 3 {
            out.push(path);
            continue;
        }
        for action in &model.actions {
            if action.name == "Settled" {
                continue;
            }
            let mut next = state.clone();
            if model.fire(action.name, &mut next) {
                let mut longer = path.clone();
                longer.push(action.name);
                stack.push((next, longer));
            }
        }
    }
    out
}

/// Run `path` on the real system and on `model`, and return where each
/// disagrees (empty: the real code IS the machine along this path).
fn divergence(model: &Model, path: &[&'static str]) -> Vec<String> {
    let mut real = Real::new(model);
    let mut state = model.init_state();
    let mut out = Vec::new();
    for action in path {
        assert!(model.fire(action, &mut state), "{action} on {state:?}");
        real.step(action);
        for (var, got) in project(&real) {
            if state[var] != got {
                out.push(format!(
                    "after {action}: {var} real={got} model={}",
                    state[var]
                ));
            }
        }
    }
    // Where nothing was lost (and nothing cleared), the successor holds the
    // parent's history itself, not merely as many lines.
    if let Some((held, report)) = &real.settled
        && report.lost() == 0
        && report.failed.is_none()
        && !report.cleared
    {
        let parent = history(&real.parent.lock().unwrap());
        if *held != parent {
            out.push(format!("successor {held:?} is not the parent's {parent:?}"));
        }
    }
    out
}

/// TIER-1: every path of the committed machine, on the real code.
#[test]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Output",
    reason = "The environment: program output reaching the outgoing grid between the export \
              and the park. Tier-1 drives it as real `Terminal::process` bytes; the carry's \
              own seams (`join`, the export, the import) are what it is checked against."
)]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Rewrap",
    reason = "The environment: a width reflow (or a clear or reset) moving the history under \
              the export's fence. Tier-1 drives a real `Terminal::resize`; the join's fence \
              check is what refuses it."
)]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Corrupt",
    reason = "The environment: a sidecar that reaches the successor with other bytes than its \
              stamp. Tier-1 flips a byte of the real named file; the import's length and sha \
              check is what refuses it."
)]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Clear",
    reason = "The environment: the adopted shell clearing its scrollback (ED3, a reset) after \
              Commit, before the import lands. Tier-1 feeds a real `ESC [3J` to the adopted \
              engine; the import's claim check is what refuses to put the history back."
)]
#[aterm_spec::spec_unmodeled(
    machine = "NativeUpdateHistoryCarry",
    action = "Settled",
    reason = "Stutter: the settled session stays settled (the machine's deadlock guard); no \
              shipping code runs for it."
)]
fn the_real_history_carry_is_the_derived_machine() {
    let model = aterm_spec::derive::native_update_history_carry_model();
    // Every interleaving of up to two lines of output and a rewrap (nine); on
    // the two that name a sidecar, a corruption, a clear, both, or neither
    // (four each), and on the seven that name none a clear or not (two each):
    // twenty-two.
    let all = paths(&model);
    assert_eq!(all.len(), 22, "the machine's paths: {all:?}");
    let mut failures = Vec::new();
    for path in &all {
        let diverged = divergence(&model, path);
        if !diverged.is_empty() {
            failures.push(format!("{path:?}: {}", diverged.join("; ")));
        }
    }
    assert!(
        failures.is_empty(),
        "the real carry left the machine on {} path(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// NEGATIVE CONTROL: the `Buggy = 1` machine's four defect paths each end
/// where the real code does not — the real carry counts the fallback, refuses
/// the moved history, counts the failed sidecar, and leaves a cleared
/// scrollback cleared.
#[test]
fn the_buggy_machine_is_not_the_real_history_carry() {
    let buggy =
        aterm_spec::interp::with_buggy(&aterm_spec::derive::native_update_history_carry_model(), 1);
    for (path, what) in [
        (
            &["Export", "Output", "Output", "Park", "Settle"][..],
            "the silent fallback",
        ),
        (
            &["Export", "Rewrap", "Park", "Settle"][..],
            "the moved history",
        ),
        (
            &["Export", "Park", "Corrupt", "Settle"][..],
            "the uncounted failed sidecar",
        ),
        (
            &["Export", "Park", "Clear", "Settle"][..],
            "the resurrected history",
        ),
    ] {
        assert!(
            !divergence(&buggy, path).is_empty(),
            "{what}: the real code must not land where the buggy machine does"
        );
    }
}
