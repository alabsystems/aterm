// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for `ForegroundHandback` kernel edges against [`FgCutter`].

use std::collections::{BTreeMap, HashSet, VecDeque};

use aterm_models_gui::foreground_handback_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;

use super::{FgCutter, FgEdge, ProcSeen};
use crate::holder_image::EdgeKind;

type State = BTreeMap<&'static str, i64>;

const SHELL: i32 = 4100;
const JOB: i32 = 4200;

const PROJECTED: [&str; 4] = ["run", "carries", "early", "mixed"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Writer {
    Shell,
    Old,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rewrite {
    None,
    CutAtPoll,
    LayAtCut,
}

#[derive(Clone, Debug)]
struct ExecRig {
    cutter: FgCutter,
    fg: i32,
    execd: bool,
    pending: bool,
    job_queue: VecDeque<Writer>,
    shell_queue: usize,
    batch: Option<Vec<Writer>>,
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
            "Die" | "Stop" | "Parse" | "Sweep" => {}
            other => panic!("the exec rig has no step for `{other}`"),
        }
    }
}

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
