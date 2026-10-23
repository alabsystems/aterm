// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for BudgetedWalk (`aterm_models_misc::budgeted_walk_model`): the real
//! [`Walker`] over a scratch tree, every step projected and admitted by the model's `Next`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_diskwalk::{Deny, Policy, RootSpec, SLICE, Sliced, Step, Ticket, Walk, Walker};
use aterm_models_misc::budgeted_walk_model;
use aterm_spec::derive::Model;
use aterm_spec::{interp, verify};

type State = BTreeMap<&'static str, i64>;

/// What the test knows of the tree and of the slices it asked for: the walk's calls and entries
/// (from a whole walk of the same tree), how many calls a slice may make under its budget, and
/// the ghosts of the last slice.
#[derive(Default)]
struct Ghost {
    calls: u64,
    entries: u64,
    allowed: u64,
    refused: bool,
    over: bool,
    resumed_old: bool,
}

fn int(n: u64) -> i64 {
    i64::try_from(n).expect("a scratch walk's counts fit i64")
}

/// The projection the anchors on `Walker` name.
fn project(walker: &Walker, ghost: &Ghost) -> State {
    let walk = walker.walk();
    let calls = walk.map_or(0, Walk::calls);
    let walked = walk.map_or(0, Walk::walked);
    [
        (
            "generation",
            walker.ticket().map_or(0, |t| int(t.generation())),
        ),
        (
            "left",
            if walk.is_some() {
                int(ghost.calls) - int(calls)
            } else {
                0
            },
        ),
        ("done", i64::from(walk.is_some_and(Walk::is_done))),
        ("cancelled", i64::from(walk.is_some_and(Walk::is_cancelled))),
        ("armed", i64::from(walker.armed())),
        ("refused", i64::from(ghost.refused)),
        ("over", i64::from(ghost.over)),
        ("resumed_old", i64::from(ghost.resumed_old)),
        ("relisted", i64::from(walked > ghost.entries)),
    ]
    .into_iter()
    .collect()
}

/// The model with the real walk's call count.
struct Bound {
    model: Model,
    overrides: Vec<(&'static str, i64)>,
}

impl Bound {
    fn new(calls: u64) -> Self {
        Self::with(calls, 0)
    }

    /// The same, with the mutants on (`buggy = 1`).
    fn with(calls: u64, buggy: i64) -> Self {
        let overrides = vec![("Calls", int(calls)), ("Buggy", buggy)];
        Self {
            model: interp::with_consts(&budgeted_walk_model(), &overrides),
            overrides,
        }
    }

    /// Whether the model admits `prev -> observed` as `action`, both tiers.
    fn admits(&self, prev: &State, observed: &State, action: &str, label: &str) -> bool {
        verify::validate_transition_tiered(
            &self.model,
            &self.overrides,
            prev,
            observed,
            Some(action),
            label,
        )
        .0
    }

    /// Bind one real step: the model's `action` from `state` gives exactly what the walker
    /// shows, and both tiers admit it.
    fn bind(&self, state: &mut State, action: &str, observed: State) {
        let mut expected = state.clone();
        assert!(
            self.model.fire(action, &mut expected),
            "BudgetedWalk action {action} disabled at {state:?}"
        );
        assert_eq!(
            observed, expected,
            "the real walker diverged from BudgetedWalk after {action}"
        );
        assert!(
            self.admits(state, &observed, action, "BudgetedWalk Tier-1"),
            "the real step {action} is not admitted: {state:?} -> {observed:?}"
        );
        *state = observed;
    }
}

/// A clock that charges `step` per reading.
fn charged(step: Duration) -> impl FnMut() -> Instant {
    #[expect(
        clippy::disallowed_methods,
        reason = "an origin for the charged clock; only differences of its readings are used"
    )]
    let base = Instant::now();
    let mut reads = 0u32;
    move || {
        let t = base + step * reads;
        reads += 1;
        t
    }
}

fn write(path: &Path, bytes: usize) {
    std::fs::write(path, vec![3u8; bytes]).expect("write scratch file");
}

/// A scratch tree of a few dozen calls.
fn tree() -> (aterm_tempfile::TempDir, PathBuf) {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("root");
    for d in ["a/b/c", "a/d", "e", "f/g", "empty"] {
        std::fs::create_dir_all(root.join(d)).expect("mkdir");
    }
    for (i, d) in ["a", "a/b", "a/b/c", "a/d", "e", "f", "f/g"]
        .iter()
        .enumerate()
    {
        for j in 0..3 {
            write(&root.join(d).join(format!("f{j}.bin")), 1_000 * (i + j + 1));
        }
    }
    (tmp, root)
}

/// A policy for a scratch tree, under a home the tree is not in.
fn policy(slice: Duration) -> Policy {
    let mut policy = Policy::new(
        Deny::for_home(Path::new("/Users//diskwalk-conformance-home"), &[], &[]),
        aterm_sysprobe::volumes::table(),
    );
    policy.slice = slice;
    policy
}

/// What a whole walk of `root` makes: its calls and entries.
fn whole(root: &Path, policy: &Policy) -> (u64, u64) {
    let mut walk = Walk::begin(&RootSpec::new(root), policy).expect("begin");
    let mut clock = charged(Duration::from_micros(1));
    while let Step::More { .. } = walk.diskwalk_slice(&mut clock) {}
    assert!(walk.is_done());
    (walk.calls(), walk.walked())
}

/// One slice through the walker, its ghosts recorded the way a host sees them.
fn slice(
    walker: &mut Walker,
    ghost: &mut Ghost,
    ticket: Ticket,
    clock: &mut dyn FnMut() -> Instant,
) -> Sliced {
    let current = walker.ticket();
    let before = walker.walk().map_or(0, Walk::calls);
    let answer = walker.slice(ticket, clock);
    let made = walker.walk().map_or(0, Walk::calls) - before;
    ghost.refused = answer == Sliced::Stale;
    ghost.over = made > ghost.allowed;
    ghost.resumed_old = Some(ticket) != current && answer != Sliced::Stale;
    answer
}

#[test]
fn the_real_walker_refines_budgeted_walk() {
    let (_tmp, root) = tree();
    let policy = policy(SLICE);
    let (calls, entries) = whole(&root, &policy);
    assert!(calls > 8, "the tree takes several slices ({calls} calls)");
    let bound = Bound::new(calls);
    let mut ghost = Ghost {
        calls,
        entries,
        allowed: 1,
        ..Ghost::default()
    };
    let mut clock = charged(SLICE);
    let mut walker = Walker::new();
    let mut state = project(&walker, &ghost);

    let first = walker.start(&RootSpec::new(&root), &policy).expect("start");
    bound.bind(&mut state, "Start", project(&walker, &ghost));
    for _ in 0..3 {
        let answer = slice(&mut walker, &mut ghost, first, &mut clock);
        assert!(matches!(answer, Sliced::Step(Step::More { .. })));
        bound.bind(&mut state, "Slice", project(&walker, &ghost));
    }

    ghost.refused = false;
    let second = walker
        .start(&RootSpec::new(&root), &policy)
        .expect("restart");
    bound.bind(&mut state, "Start", project(&walker, &ghost));
    assert_eq!(
        slice(&mut walker, &mut ghost, first, &mut clock),
        Sliced::Stale
    );
    bound.bind(&mut state, "StaleSlice", project(&walker, &ghost));
    let answer = slice(&mut walker, &mut ghost, second, &mut clock);
    assert!(matches!(answer, Sliced::Step(Step::More { .. })));
    bound.bind(&mut state, "Slice", project(&walker, &ghost));
    walker.cancel();
    ghost.refused = false;
    bound.bind(&mut state, "Cancel", project(&walker, &ghost));

    let third = walker.start(&RootSpec::new(&root), &policy).expect("third");
    bound.bind(&mut state, "Start", project(&walker, &ghost));
    loop {
        match slice(&mut walker, &mut ghost, third, &mut clock) {
            Sliced::Step(Step::More { .. }) => {
                bound.bind(&mut state, "Slice", project(&walker, &ghost));
            }
            Sliced::Step(Step::Done) => {
                bound.bind(&mut state, "FinalSlice", project(&walker, &ghost));
                break;
            }
            other => panic!("a walk in flight answered {other:?}"),
        }
    }
    assert_eq!(state["left"], 0);
    assert_eq!(walker.walk().map(Walk::walked), Some(entries));

    assert_eq!(
        slice(&mut walker, &mut ghost, second, &mut clock),
        Sliced::Stale
    );
    bound.bind(&mut state, "StaleSlice", project(&walker, &ghost));
}

#[test]
fn a_slice_past_its_budget_is_rejected() {
    let (_tmp, root) = tree();
    let policy = policy(SLICE * 2);
    let (calls, entries) = whole(&root, &policy);
    let bound = Bound::new(calls);
    let mut ghost = Ghost {
        calls,
        entries,
        allowed: 1,
        ..Ghost::default()
    };
    let mut clock = charged(SLICE);
    let mut walker = Walker::new();
    let mut state = project(&walker, &ghost);
    let ticket = walker.start(&RootSpec::new(&root), &policy).expect("start");
    bound.bind(&mut state, "Start", project(&walker, &ghost));
    let answer = slice(&mut walker, &mut ghost, ticket, &mut clock);
    assert!(matches!(answer, Sliced::Step(Step::More { .. })));
    let observed = project(&walker, &ghost);
    assert_eq!(observed["over"], 1, "the slipped slice made two calls");
    for action in ["Slice", "FinalSlice", "StaleSlice"] {
        assert!(
            !bound.admits(
                &state,
                &observed,
                action,
                "BudgetedWalk over-budget control"
            ),
            "a slice past its budget was admitted as {action}"
        );
    }
    assert!(
        Bound::with(calls, 1).admits(&state, &observed, "OverBudget", "BudgetedWalk OverBudget"),
        "the slipped slice is exactly the OverBudget mutant's step"
    );
}

#[test]
fn a_superseded_ticket_that_walks_is_rejected() {
    let (_tmp, root) = tree();
    let policy = policy(SLICE);
    let (calls, entries) = whole(&root, &policy);
    let bound = Bound::new(calls);
    let mut ghost = Ghost {
        calls,
        entries,
        allowed: 1,
        ..Ghost::default()
    };
    let mut clock = charged(SLICE);
    let mut walker = Walker::new();
    let mut state = project(&walker, &ghost);
    let _first = walker.start(&RootSpec::new(&root), &policy).expect("start");
    bound.bind(&mut state, "Start", project(&walker, &ghost));
    let second = walker
        .start(&RootSpec::new(&root), &policy)
        .expect("restart");
    bound.bind(&mut state, "Start", project(&walker, &ghost));
    let answer = walker.slice(second, &mut clock);
    assert!(matches!(answer, Sliced::Step(Step::More { .. })));
    ghost.resumed_old = true;
    let observed = project(&walker, &ghost);
    for action in ["StaleSlice", "Slice"] {
        assert!(
            !bound.admits(
                &state,
                &observed,
                action,
                "BudgetedWalk stale-resume control"
            ),
            "a superseded ticket that walked was admitted as {action}"
        );
    }
    assert!(
        Bound::with(calls, 1).admits(
            &state,
            &observed,
            "ResumeOldGeneration",
            "BudgetedWalk ResumeOldGeneration"
        ),
        "the slipped slice is exactly the ResumeOldGeneration mutant's step"
    );
}
