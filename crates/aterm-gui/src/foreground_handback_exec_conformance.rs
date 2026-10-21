// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `ForegroundHandback`'s kernel edges
//! (`aterm_models_gui::foreground_handback_model`'s `Exec` and `ObserveExec`,
//! card engine §3.3): the real [`FgCutter`] driven through every reachable
//! state of the model.
//!
//! The rig keeps what the gather knows and the kernel does: the bytes the
//! batch has read, each marked with the image that wrote it (the shell's, the
//! job's before its `exec`, or after it), the job bytes still queued in the
//! tty, the scripted `tcgetpgrp`, and whether the `exec`'s `NOTE_EXEC` is
//! pending unheard. Each model action is the gather's own call on the cutter:
//! `Park` is the pre-park [`FgCutter::sample`], `ObserveExec` a poll that heard
//! the watch ([`FgCutter::proc_seen`], then [`FgCutter::proc_drained`], as
//! `ExecEdges::heard` makes them), `Deliver` [`FgCutter::finish`] (or
//! [`FgCutter::take_edge`] for a byte-free batch) and `Restart` a new cutter
//! seeded with the carried holder and a new, quiet watch. A read is the
//! master's: after a park it is a poll that found the watch quiet.
//!
//! Projection ([`project_exec`]), after every transition out of every
//! reachable model state (each reached by the first path that finds it):
//!
//! * `run`     — [`FgCutter::holder`] is the job;
//! * `carries` — the batch carries its edge (the model's `edge` or `xedge`);
//! * `early`   — an Exec edge's `at` has a byte of the new image before it, or
//!   a batch already delivered held one unheard;
//! * `mixed`   — an Exec edge's `lay` has a byte of the old image read before
//!   the event at or after it.
//!
//! NEGATIVE CONTROLS: the same walk with the edge the cutter made rewritten
//! to cut at the offset of the poll that heard it lands, state for state,
//! where the `Buggy = 2` replay does, and reaches `early = 1`, which the
//! committed model rejects (`ExecCutBlind`); rewritten to lay the new run at
//! the cut, it lands where `Buggy = 3` does and reaches `mixed = 1`
//! (`ExecSpanClean`).

use std::collections::{BTreeMap, HashSet, VecDeque};

use aterm_models_gui::foreground_handback_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;

use super::{FgCutter, FgEdge, ProcSeen};
use crate::holder_image::EdgeKind;

type State = BTreeMap<&'static str, i64>;

const SHELL: i32 = 4100;
const JOB: i32 = 4200;

/// The model variables the cutter projects onto.
const PROJECTED: [&str; 4] = ["run", "carries", "early", "mixed"];

/// Who wrote a byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Writer {
    Shell,
    /// The job before its `exec`.
    Old,
    /// The job's new image.
    New,
}

/// How the walk rewrites the edge the real cutter made (a negative control).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rewrite {
    /// The cutter's own edge.
    None,
    /// Cut at the offset of the poll that heard the event (`Buggy = 2`).
    CutAtPoll,
    /// Lay the new holder's run at the cut (`Buggy = 3`).
    LayAtCut,
}

#[derive(Clone, Debug)]
struct ExecRig {
    cutter: FgCutter,
    /// The scripted `tcgetpgrp(master)`.
    fg: i32,
    /// The job has `exec`'d: what it writes now is the new image's.
    execd: bool,
    /// The `exec`'s `NOTE_EXEC` is pending, unheard.
    pending: bool,
    job_queue: VecDeque<Writer>,
    shell_queue: usize,
    /// The open batch's bytes, in read order; `None` before its first read.
    batch: Option<Vec<Writer>>,
    /// A delivered batch held a byte of the new image while its `exec` was
    /// still unheard (the model's `nc` carried across `Deliver`).
    unheard_new: bool,
    early: bool,
    mixed: bool,
    rewrite: Rewrite,
}

impl ExecRig {
    fn new(rewrite: Rewrite) -> Self {
        Self {
            cutter: FgCutter::new(SHELL),
            fg: SHELL,
            execd: false,
            pending: false,
            job_queue: VecDeque::new(),
            shell_queue: 0,
            batch: None,
            unheard_new: false,
            early: false,
            mixed: false,
            rewrite,
        }
    }

    /// The open batch, begun at the poll that started it.
    fn open(&mut self) -> &mut Vec<Writer> {
        if self.batch.is_none() {
            self.cutter.begin();
        }
        self.batch.get_or_insert_with(Vec::new)
    }

    fn filled(&self) -> usize {
        self.batch.as_ref().map_or(0, Vec::len)
    }

    fn read(&mut self, writer: Writer) {
        self.open().push(writer);
    }

    /// A poll heard the watch after `filled` bytes: the gather's drain.
    fn observe(&mut self) {
        self.open();
        let filled = self.filled();
        self.pending = false;
        let ev = ProcSeen {
            pid: JOB,
            pgid: JOB,
            fg: self.fg,
        };
        let edge = self.cutter.proc_seen(ev, filled, |_, _| true);
        self.cutter.proc_drained(filled);
        if edge {
            let real = self.cutter.edge.expect("proc_seen said it carries an edge");
            let e = match self.rewrite {
                Rewrite::None => real,
                Rewrite::CutAtPoll => FgEdge {
                    at: real.lay,
                    ..real
                },
                Rewrite::LayAtCut => FgEdge {
                    lay: real.at,
                    ..real
                },
            };
            self.cutter.edge = Some(e);
            if e.kind == EdgeKind::Exec {
                let batch = self.batch.as_deref().unwrap_or_default();
                self.early |= self.unheard_new || batch[..e.at].contains(&Writer::New);
                self.mixed |= batch[e.lay..].contains(&Writer::Old);
            }
        } else if self.filled() == 0 {
            // A watch heard before any byte with no edge: the next poll
            // begins the batch again.
            self.batch = None;
        }
        self.unheard_new = false;
    }

    fn deliver(&mut self) {
        let fg = self.fg;
        let filled = self.filled();
        let edge = if filled == 0 {
            self.cutter.take_edge()
        } else {
            self.cutter.finish(|| fg, filled)
        };
        let batch = self.batch.take().unwrap_or_default();
        // `ExecEdges::absorb`: a sampled edge to the job hears its `exec` with it.
        let absorbed =
            self.pending && edge.is_some_and(|e| e.kind == EdgeKind::Group && e.to == JOB);
        if absorbed {
            self.pending = false;
        }
        self.unheard_new = self.pending && batch.contains(&Writer::New);
    }

    fn step(&mut self, action: &str) {
        match action {
            "Launch" | "OneShot" | "Resume" => self.fg = JOB,
            "Adopt" => {
                self.fg = JOB;
                self.cutter = FgCutter::new(JOB);
            }
            "JobWrite" => {
                self.job_queue
                    .push_back(if self.execd { Writer::New } else { Writer::Old })
            }
            "Exec" => {
                self.execd = true;
                self.pending = true;
            }
            "Reclaim" => self.fg = SHELL,
            "ShellWrite" => self.shell_queue += 1,
            "ReadJob" => {
                let w = self
                    .job_queue
                    .pop_front()
                    .expect("the model reads a queued byte");
                self.read(w);
            }
            "ReadShell" => {
                self.shell_queue -= 1;
                self.read(Writer::Shell);
            }
            "Park" => {
                let (fg, filled) = (self.fg, self.filled());
                self.cutter.sample(fg, filled);
            }
            "ObserveExec" => self.observe(),
            "Deliver" => self.deliver(),
            "Restart" => {
                self.cutter = FgCutter::new(self.cutter.holder());
                self.pending = false;
            }
            // The job's death and stop, the parse stage and the status sweep
            // make no call on the cutter.
            "Die" | "Stop" | "Parse" | "Sweep" => {}
            other => panic!("the exec rig has no step for `{other}`"),
        }
    }
}

/// The real cutter's projection.
fn project_exec(rig: &ExecRig) -> State {
    [
        ("run", i64::from(rig.cutter.holder() == JOB)),
        ("carries", i64::from(rig.cutter.carries_edge())),
        ("early", i64::from(rig.early)),
        ("mixed", i64::from(rig.mixed)),
    ]
    .into_iter()
    .collect()
}

fn projected(s: &State) -> State {
    let mut out: State = PROJECTED
        .iter()
        .filter(|k| **k != "carries")
        .map(|k| (*k, s[k]))
        .collect();
    out.insert("carries", i64::from(s["edge"] + s["xedge"] > 0));
    out
}

/// Walk every transition of `model` reachable from its initial state, each
/// source state by the first path that reaches it, driving a rig that
/// rewrites its edges with `rewrite` along the way; the projection must equal
/// the model's after every action. Returns the states visited.
fn walk(model: &Model, rewrite: Rewrite) -> Vec<State> {
    let actions: Vec<&'static str> = model.actions.iter().map(|a| a.name).collect();
    let init: State = model.init_state();
    let rig = ExecRig::new(rewrite);
    assert_eq!(project_exec(&rig), projected(&init), "at the start");
    let mut seen: HashSet<Vec<(&'static str, i64)>> = HashSet::new();
    seen.insert(init.clone().into_iter().collect());
    let mut stack = vec![(init, rig, Vec::<&str>::new())];
    let mut visited = Vec::new();
    while let Some((state, rig, path)) = stack.pop() {
        for action in &actions {
            for next in model.successors(action, &state) {
                let mut r = rig.clone();
                r.step(action);
                let mut p = path.clone();
                p.push(action);
                assert_eq!(
                    project_exec(&r),
                    projected(&next),
                    "after {p:?}: the real cutter (left) and the model (right) disagree"
                );
                if seen.insert(next.clone().into_iter().collect()) {
                    stack.push((next, r, p));
                }
            }
        }
        visited.push(state);
    }
    visited
}

/// Every reachable state of the committed model, the real cutter in step:
/// an Exec edge cut at the last clean poll and laid where it was heard never
/// hands a byte to the wrong image.
#[test]
fn foreground_handback_exec_the_cutter_refines_every_reachable_state() {
    let model = foreground_handback_model();
    let states = walk(&model, Rewrite::None);
    assert!(
        states.iter().any(|s| s["img"] == 1 && s["xedge"] == 1),
        "the walk reaches an Exec edge"
    );
    assert!(
        states.iter().all(|s| s["early"] == 0 && s["mixed"] == 0),
        "the committed model is clean"
    );
}

/// Negative control: cutting at the poll that heard the `exec` (`Buggy = 2`)
/// is the replay's state for state, and it hands the new image's first bytes
/// to the old one.
#[test]
fn foreground_handback_exec_negative_control_cutting_at_the_poll_is_caught() {
    let committed = foreground_handback_model();
    let states = walk(&interp::with_buggy(&committed, 2), Rewrite::CutAtPoll);
    let caught = states
        .iter()
        .find(|s| s["early"] == 1)
        .expect("a caught state");
    assert!(
        !committed.check_invariant("ExecCutBlind", caught),
        "the committed model rejects it"
    );
}

/// Negative control: laying the new image's run at the cut (`Buggy = 3`) is
/// the replay's state for state, and it hands the old image's last bytes to
/// the new one.
#[test]
fn foreground_handback_exec_negative_control_laying_at_the_cut_is_caught() {
    let committed = foreground_handback_model();
    let states = walk(&interp::with_buggy(&committed, 3), Rewrite::LayAtCut);
    let caught = states
        .iter()
        .find(|s| s["mixed"] == 1)
        .expect("a caught state");
    assert!(
        !committed.check_invariant("ExecSpanClean", caught),
        "the committed model rejects it"
    );
}
