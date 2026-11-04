// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

struct Fixture {
    queue: PreparationQueue,
    start: Instant,
    picked: i64,
}
impl Fixture {
    fn new() -> Self {
        Self {
            queue: PreparationQueue::default(),
            start: Instant::now(),
            picked: 0,
        }
    }
    fn run(&mut self, action: &str) -> i64 {
        let (phase, home, ids, seconds) = match action {
            "Initial" => (1, "home", vec![[2; 32], [1; 32], [2; 32]], 10),
            "Sparse" => (2, "home", vec![[1; 32], [2; 32]], 70),
            "Late" => (3, "home", vec![[1; 32], [2; 32]], 200),
            "Prune" => (4, "home", vec![[2; 32]], 70),
            "OtherHome" => (5, "other", vec![[1; 32]], 11),
            "Grow" => (6, "home", vec![[1; 32], [2; 32]], 11),
            "Cooldown" => (7, "home", vec![[1; 32]], 69),
            "Boundary" => (8, "home", vec![[1; 32]], 70),
            "Future" => (9, "home", vec![[1; 32]], 9),
            "Empty" => (10, "home", Vec::new(), 70),
            "Disappear" => (11, "home", Vec::new(), 11),
            "Reappear" => (12, "home", vec![[1; 32]], 12),
            _ => panic!("unknown native preparation action {action}"),
        };
        self.picked = self
            .queue
            .pick(
                Path::new(home),
                &ids,
                self.start + Duration::from_secs(seconds),
            )
            .map_or(0, |id| i64::from(id[0]));
        phase
    }
    fn snapshot(&self, phase: i64) -> State {
        let at = |home: &str, id| {
            self.queue
                .seen
                .get(&(PathBuf::from(home), [id; 32]))
                .map_or(0, |seen| {
                    seen.at.duration_since(self.start).as_secs() as i64
                })
        };
        State::from([
            ("phase", phase),
            ("picked", self.picked),
            ("count", self.queue.seen.len() as i64),
            ("a", at("home", 1)),
            ("b", at("home", 2)),
            ("other", at("other", 1)),
        ])
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut f = Fixture::new();
    match state["phase"] {
        0 => {}
        1 => {
            f.run("Initial");
        }
        2 => {
            f.run("Initial");
            f.run("Sparse");
        }
        7 => {
            f.run("Initial");
            f.run("Cooldown");
        }
        11 => {
            f.run("Initial");
            f.run("Disappear");
        }
        _ => panic!("terminal native preparation state"),
    }
    assert_eq!(f.snapshot(state["phase"]), *state);
    let phase = f.run(action);
    f.snapshot(phase)
}

#[test]
fn native_preparation_queue_conforms_and_catches_wrong_selection() {
    let model = aterm_models_harness::native_preparation_queue_model();
    assert_eq!(model.mutant_dials(), (1..=10).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                let mut wrong = observed.clone();
                wrong.insert("picked", observed["picked"] + 1);
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
}

#[test]
fn retired_candidates_release_history_without_erasing_another_homes_cooldown() {
    let mut q = PreparationQueue::default();
    let start = Instant::now();
    let first = Path::new("first");
    let second = Path::new("second");
    assert_eq!(q.pick(first, &[[1; 32]], start), Some([1; 32]));
    assert_eq!(q.pick(second, &[[2; 32]], start), Some([2; 32]));
    assert_eq!(
        q.pick(first, &[[1; 32]], start + Duration::from_secs(1)),
        None
    );
    for n in 3..=20 {
        let at = start + COOLDOWN * u32::from(n - 2);
        assert_eq!(q.pick(first, &[[n; 32]], at), Some([n; 32]));
        assert_eq!(q.seen.len(), 2);
        assert_eq!(
            q.seen
                .get(&(second.to_owned(), [2; 32]))
                .map(|seen| seen.at),
            Some(start)
        );
    }
    assert_eq!(q.pick(first, &[], start + COOLDOWN * 19), None);
    assert_eq!(q.seen.len(), 1);
}

#[test]
fn a_large_queue_keeps_mature_ties_ordered_and_admits_every_account() {
    let mut q = PreparationQueue::default();
    let home = Path::new("large-queue");
    let start = Instant::now();
    let mut candidates: Vec<_> = (1..=96).map(|n| [n; 32]).collect();
    candidates.reverse();
    for n in 1..=96 {
        assert_eq!(q.pick(home, &candidates, start), Some([n; 32]));
    }
    assert_eq!(q.seen.len(), 96);
    assert_eq!(q.pick(home, &candidates, start), None);
    let mature = start + COOLDOWN;
    assert_eq!(q.pick(home, &candidates, mature), Some([1; 32]));
    assert_eq!(q.pick(home, &candidates, mature), Some([2; 32]));
    assert_eq!(q.seen.len(), 96);
}

#[test]
fn sparse_maintenance_turns_do_not_starve_later_native_accounts() {
    let start = Instant::now();
    let home = Path::new("native-queue-test");
    let mut queue = PreparationQueue::default();
    let candidates = [[1; 32], [2; 32]];
    let picked: Vec<_> = [0, 60, 120, 180]
        .into_iter()
        .map(|seconds| queue.pick(home, &candidates, start + Duration::from_secs(seconds)))
        .collect();
    assert_eq!(
        picked,
        vec![Some([1; 32]), Some([2; 32]), Some([1; 32]), Some([2; 32])]
    );
}

#[test]
fn native_revisions_replace_one_slot_only_after_admission() {
    let start = Instant::now();
    let home = Path::new("revision");
    let mut queue = PreparationQueue::default();
    let first = BTreeMap::from([([1; 32], [1; 32])]);
    assert_eq!(
        queue.pick_revisions_if(home, &first, start, |_, _| true),
        Some(([1; 32], [1; 32]))
    );
    let changed = BTreeMap::from([([1; 32], [2; 32])]);
    let next = start + Duration::from_secs(1);
    assert_eq!(
        queue.pick_revisions_if(home, &changed, next, |_, _| false),
        None
    );
    let prior = queue.seen.get(&(home.to_owned(), [1; 32])).unwrap();
    assert_eq!(prior.revision, [1; 32]);
    assert_eq!(prior.at, start);
    assert_eq!(
        queue.pick_revisions_if(home, &changed, next, |_, _| true),
        Some(([1; 32], [2; 32]))
    );
    assert_eq!(queue.seen.len(), 1);
    assert_eq!(
        queue.pick_revisions_if(home, &changed, next, |_, _| true),
        None
    );
    let newer = BTreeMap::from([([1; 32], [3; 32])]);
    assert_eq!(
        queue.pick_revisions_if(home, &newer, start, |_, _| true),
        None,
        "a future clock remains held"
    );
    for revision in 3..=20 {
        let entries = BTreeMap::from([([1; 32], [revision; 32])]);
        assert!(
            queue
                .pick_revisions_if(home, &entries, next, |_, _| true)
                .is_some()
        );
        assert_eq!(queue.seen.len(), 1);
    }
    let another = BTreeMap::from([([2; 32], [1; 32])]);
    assert_eq!(
        queue.pick_revisions_if(home, &another, next, |_, _| true),
        Some(([2; 32], [1; 32]))
    );
    assert_eq!(
        queue.pick_revisions_if(Path::new("other"), &changed, next, |_, _| true),
        Some(([1; 32], [2; 32]))
    );
    assert_eq!(queue.seen.len(), 3);
}

struct AdmissionFixture {
    queue: PreparationQueue,
    start: Instant,
    picked: i64,
    offered: i64,
}

impl AdmissionFixture {
    fn step(&mut self, action: &str) -> i64 {
        let (phase, seconds, ids) = match action {
            "Reject" => (1, 10, vec![[2; 32], [1; 32], [2; 32]]),
            "Retry" => (2, 11, vec![[1; 32], [2; 32]]),
            "Skip" => (3, 10, vec![[3; 32], [2; 32], [1; 32], [2; 32]]),
            "Cooling" => (4, 12, vec![[1; 32]]),
            "Grow" => (5, 11, vec![[1; 32]]),
            _ => panic!("unknown admission action {action}"),
        };
        self.offered = 0;
        self.picked = self
            .queue
            .pick_if(
                Path::new("admission"),
                &ids,
                self.start + Duration::from_secs(seconds),
                |id| {
                    self.offered += 1;
                    match action {
                        "Reject" => false,
                        "Skip" => id == [2; 32],
                        _ => true,
                    }
                },
            )
            .map_or(0, |id| i64::from(id[0]));
        phase
    }

    fn snapshot(&self, phase: i64) -> State {
        let at = |id| {
            self.queue
                .seen
                .get(&(PathBuf::from("admission"), [id; 32]))
                .map_or(0, |seen| {
                    seen.at.duration_since(self.start).as_secs() as i64
                })
        };
        State::from([
            ("phase", phase),
            ("picked", self.picked),
            ("offered", self.offered),
            ("count", self.queue.seen.len() as i64),
            ("a", at(1)),
            ("b", at(2)),
        ])
    }
}

pub(crate) fn admission_transition(state: &State, action: &str) -> State {
    let mut f = AdmissionFixture {
        queue: PreparationQueue::default(),
        start: Instant::now(),
        picked: 0,
        offered: 0,
    };
    match state["phase"] {
        0 => {}
        1 => {
            f.step("Reject");
        }
        2 => {
            f.step("Reject");
            f.step("Retry");
        }
        3 => {
            f.step("Skip");
        }
        _ => panic!("terminal admission state"),
    }
    assert_eq!(f.snapshot(state["phase"]), *state);
    let phase = f.step(action);
    f.snapshot(phase)
}

#[test]
fn native_admission_conforms_and_catches_charging_a_denied_candidate() {
    let model = aterm_models_harness::native_preparation_admission_model();
    assert_eq!(model.mutant_dials(), (1..=6).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = admission_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                let mut wrong = observed.clone();
                wrong.insert("count", observed["count"] + 1);
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
}
