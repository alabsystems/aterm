// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{LOOK_AGAIN, TakeUp, Taken, look, take_up_with};
use aterm_cli::macos_doctor::PASS_RETRY_SCHEDULE;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Scripted reads, and what the looks did with them. Unscripted, no setup runs here.
struct Fake {
    here: VecDeque<bool>,
    resumable: VecDeque<Result<Option<u64>, String>>,
    at: Instant,
    did: Vec<String>,
}

impl Fake {
    fn new(resumable: impl IntoIterator<Item = Result<Option<u64>, String>>) -> Self {
        Self {
            here: VecDeque::new(),
            resumable: resumable.into_iter().collect(),
            at: Instant::now(),
            did: Vec::new(),
        }
    }
}

impl TakeUp for Fake {
    fn arm(&mut self) -> bool {
        true
    }
    fn wait(&mut self, within: Duration) {
        self.did.push(format!("wait {}", within.as_secs()));
    }
    fn setup_here(&mut self) -> bool {
        self.here.pop_front().unwrap_or(false)
    }
    fn resumable(&mut self) -> Result<Option<u64>, String> {
        self.resumable.pop_front().expect("the look reads no more")
    }
    fn resume(&mut self) {
        self.did.push("resume".into());
    }
    fn now(&mut self) -> Instant {
        self.at
    }
    fn say(&mut self, _: String) {}
}

/// A run another process reopens while this window is open and its own worker has returned
/// (a widened `aterm init` cut short) goes on here at the records' change, as a launch would
/// resume it, so its person stages run with no relaunch; once it is finished nothing more is
/// done. Before, nothing in an open window took it up: the login job ran its background
/// stages and the Permissions offers and the sign-outs waited for the next launch, which a
/// person who keeps aterm open never makes.
#[test]
fn a_run_reopened_under_an_open_window_goes_on_here() {
    let mut host = Fake::new([Ok(Some(7)), Ok(None), Ok(None)]);
    let mut taken = Taken::default();
    assert_eq!(look(&mut host, &mut taken), LOOK_AGAIN);
    assert_eq!(host.did, ["resume"]);
    assert_eq!(look(&mut host, &mut taken), LOOK_AGAIN);
    assert_eq!(host.did, ["resume"], "a finished run is not taken up again");

    // Drive the actual loop: an admission while a watch is installed must be read
    // before waiting. A replacement watch has the same obligation after its predecessor
    // fails, while an unavailable watch uses the existing retry pause.
    struct EndLoop;
    struct Loop {
        at: Instant,
        watching: bool,
        available: bool,
        arm_attempts: usize,
        admit_on_arm: Option<usize>,
        pending: Option<u64>,
        resumed: usize,
        busy_first: bool,
        lose_after_first: bool,
        waits: Vec<(Duration, usize)>,
        rounds: usize,
    }
    impl TakeUp for Loop {
        fn arm(&mut self) -> bool {
            if self.watching {
                return true;
            }
            self.arm_attempts += 1;
            if !self.available {
                return false;
            }
            self.watching = true;
            if self.admit_on_arm == Some(self.arm_attempts) {
                self.pending = Some(7);
            }
            true
        }
        fn wait(&mut self, within: Duration) {
            self.waits.push((within, self.resumed));
            self.at += within;
            if self.waits.len() == self.rounds {
                std::panic::panic_any(EndLoop);
            }
            if self.lose_after_first {
                self.watching = false;
            }
        }
        fn setup_here(&mut self) -> bool {
            self.busy_first && self.waits.is_empty()
        }
        fn resumable(&mut self) -> Result<Option<u64>, String> {
            Ok(self.pending.take())
        }
        fn resume(&mut self) {
            self.resumed += 1;
        }
        fn now(&mut self) -> Instant {
            self.at
        }
        fn say(&mut self, _: String) {}
    }
    for case in ["initial", "rearm", "missing", "healthy", "busy"] {
        let mut host = Loop {
            at: Instant::now(),
            watching: false,
            available: case != "missing",
            arm_attempts: 0,
            admit_on_arm: match case {
                "initial" | "busy" => Some(1),
                "rearm" => Some(2),
                _ => None,
            },
            pending: None,
            resumed: 0,
            busy_first: case == "busy",
            lose_after_first: case == "rearm",
            waits: Vec::new(),
            rounds: if case == "initial" { 1 } else { 2 },
        };
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            take_up_with(&mut host);
        }))
        .expect_err("the fixture ends after its bounded waits");
        assert!(
            ended.is::<EndLoop>(),
            "only the fixture's own end is expected"
        );
        let expected: &[usize] = match case {
            "initial" => &[1],
            "rearm" | "busy" => &[0, 1],
            _ => &[0, 0],
        };
        assert_eq!(
            host.waits
                .iter()
                .map(|(_, resumed)| *resumed)
                .collect::<Vec<_>>(),
            expected,
            "{case}: an admission is read before the next wait"
        );
        let wait = if case == "missing" {
            PASS_RETRY_SCHEDULE.after(1)
        } else {
            LOOK_AGAIN
        };
        assert!(
            host.waits.iter().all(|(within, _)| *within == wait),
            "{case}: no unbounded missing-watch wait or rapid polling"
        );
        assert_eq!(
            host.arm_attempts,
            if matches!(case, "rearm" | "missing") {
                2
            } else {
                1
            },
            "{case}: retain a healthy watch and rearm a failed one"
        );
    }
}

/// While setup is under way here (a startup, a run's worker running or parked) the window
/// reads nothing and starts nothing beside it; with no run owed, or records it cannot read,
/// it starts nothing either.
#[test]
fn nothing_goes_on_beside_setup_here_or_with_no_run_owed() {
    let mut host = Fake::new([Ok(None), Err("the records are being written".into())]);
    host.here = [true].into();
    let mut taken = Taken::default();
    for _ in 0..3 {
        assert_eq!(look(&mut host, &mut taken), LOOK_AGAIN);
    }
    assert!(host.did.is_empty(), "{:?}", host.did);
    assert!(host.resumable.is_empty(), "each look past setup here read");
}

/// A take-up that ends with the same run still owed waits the run's own pause before the next
/// one, longer after each in a row, however often the records change meanwhile (the login
/// job's own saves); a newer run goes on at once, and a take-up that finishes the run starts
/// the count afresh. Nothing spins, and nothing is given up.
#[test]
fn a_take_up_that_leaves_its_run_owed_waits_the_runs_pause() {
    let mut host = Fake::new([
        Ok(Some(7)),
        Ok(Some(7)),
        Ok(Some(7)),
        Ok(Some(7)),
        Ok(Some(7)),
        Ok(Some(8)),
        Ok(None),
    ]);
    let mut taken = Taken::default();
    let first = PASS_RETRY_SCHEDULE.after(1);
    assert_eq!(look(&mut host, &mut taken), first);
    assert_eq!(host.did, ["resume"]);
    // A change heard before the pause passed: nothing goes, the wait is what is left of it.
    host.at += first / 2;
    assert_eq!(look(&mut host, &mut taken), first - first / 2);
    assert_eq!(host.did, ["resume"]);
    // The pause passed: the run goes on here again, and the next pause is longer.
    host.at += first;
    assert_eq!(look(&mut host, &mut taken), PASS_RETRY_SCHEDULE.after(2));
    assert_eq!(host.did, ["resume", "resume"]);
    // A newer run goes at once, and finishing it starts the count afresh.
    assert_eq!(look(&mut host, &mut taken), LOOK_AGAIN);
    assert_eq!(host.did, ["resume", "resume", "resume"]);
    assert_eq!(taken.revision, None);
    assert_eq!(taken.tries, 0);
}
