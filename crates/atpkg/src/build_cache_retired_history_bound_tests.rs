// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private-store proof's time bounds are hang detectors, each its own step's: a history read's
//! counts from that git run's start, a caller's end refuses a run before it starts, and a walk's
//! counts from that walk's start. None is a budget over a proof's steps together, which a loaded
//! Mac outlasts while every step keeps working (RL-21's lands 5 and 7 on m86).

use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant};

use super::super::registry::{Budget, Directory};
use super::super::{Answer, Git, READ_FOR};
use super::{Read, Snapshot};

/// A git that runs nothing: it records each history read's deadline and answers it read.
struct Recorded {
    until: Option<Instant>,
    deadlines: RefCell<Vec<Instant>>,
}

impl Recorded {
    fn until(until: Option<Instant>) -> Self {
        Self {
            until,
            deadlines: RefCell::new(Vec::new()),
        }
    }
}

impl Git for Recorded {
    fn run(&self, _: &Path, args: &[&str]) -> Answer {
        unreachable!("a history proof reads through read_history, never git {args:?}")
    }

    fn read_history(
        &self,
        _: &Path,
        _: &[&str],
        _: &[u8],
        _: Option<&Path>,
        deadline: Instant,
    ) -> Answer {
        self.deadlines.borrow_mut().push(deadline);
        Answer {
            code: Some(0),
            out: String::new(),
            err: String::new(),
        }
    }

    fn until(&self) -> Option<Instant> {
        self.until
    }
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("retired-bound-")
        .tempdir()
        .unwrap()
}

#[test]
fn each_history_read_is_bounded_from_its_own_start() {
    let scratch = scratch();
    let git = Recorded::until(None);
    let read = Read {
        git: &git,
        checkout: scratch.path(),
    };
    let mut started = Vec::new();
    for _ in 0..3 {
        started.push(Instant::now());
        read.git(scratch.path(), &["rev-parse"], &[], None).unwrap();
    }
    let deadlines = git.deadlines.borrow();
    assert_eq!(deadlines.len(), started.len());
    for (run, (start, deadline)) in started.iter().zip(deadlines.iter()).enumerate() {
        assert!(
            *deadline >= *start + READ_FOR,
            "run {run}'s bound counts from its own start, not the proof's"
        );
    }
}

#[test]
fn a_callers_end_bounds_each_run_and_refuses_one_past_it() {
    let scratch = scratch();
    let soon = Instant::now() + Duration::from_secs(5);
    let git = Recorded::until(Some(soon));
    let read = Read {
        git: &git,
        checkout: scratch.path(),
    };
    read.git(scratch.path(), &["rev-parse"], &[], None).unwrap();
    assert_eq!(
        *git.deadlines.borrow(),
        [soon],
        "the caller's end comes first"
    );

    let past = Recorded::until(Some(Instant::now()));
    let read = Read {
        git: &past,
        checkout: scratch.path(),
    };
    let refused = read
        .git(scratch.path(), &["rev-parse"], &[], None)
        .unwrap_err();
    assert!(refused.contains("reached its time bound"), "{refused}");
    assert!(
        past.deadlines.borrow().is_empty(),
        "no run starts past the caller's end"
    );
}

#[test]
fn each_walk_is_bounded_from_its_own_start() {
    let scratch = scratch();
    std::fs::create_dir(scratch.path().join("refs")).unwrap();
    std::fs::write(scratch.path().join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let store = Directory::root(scratch.path())
        .unwrap()
        .expect("the store is there");
    // Longer than any walk's bound: the proof's earlier walks and git runs took this long.
    let earlier = Instant::now()
        .checked_sub(Duration::from_secs(120))
        .expect("this clock runs two minutes back");
    let mut budget = Budget::started(earlier);
    assert!(
        budget.check(0).is_err(),
        "CONTROL: a walk two minutes old is past its bound"
    );
    let snapshot =
        Snapshot::read(&store, false, &mut budget).expect("the next walk's clock is its own");
    assert!(snapshot.files.contains_key(Path::new("HEAD")));
}
