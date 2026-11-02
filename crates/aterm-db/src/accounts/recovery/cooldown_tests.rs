// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;
use std::cell::Cell;

#[test]
fn every_batch_cut_preserves_unattempted_requests_and_the_dispatch_limit() {
    let lab = Lab::new(&[Role::Writer]);
    let db = Recovery::at(lab.paths[0].clone());
    for n in 1..=6 {
        db.request_at([n; 32], [n + 10; 32], [n + 20; 16], NOW)
            .unwrap();
    }
    let requests = db.pending_at(NOW).unwrap();
    let start = Instant::now();
    let at = start + Duration::from_secs(60);
    for limit in [0, 1, 4] {
        for cut in 0..=limit {
            for deadline in [false, true] {
                let mut queue = Queue::default();
                assert!(queue.ready(requests.clone(), start, limit).is_empty());
                let calls = Cell::new(0);
                let mut attempted = std::collections::BTreeSet::new();
                let result = queue.dispatch(
                    requests.clone(),
                    at,
                    limit,
                    || {
                        if calls.get() == cut {
                            if deadline {
                                require_time(Instant::now())?;
                            }
                            return Ok(None);
                        }
                        Ok(Some(at))
                    },
                    |request| {
                        calls.set(calls.get() + 1);
                        attempted.insert(request.account());
                        request.account()[0] % 2 == 0
                    },
                );
                assert_eq!(calls.get(), cut);
                if deadline && cut < limit {
                    assert!(result.is_err());
                } else {
                    assert_eq!(result.unwrap(), (cut / 2, cut < limit));
                }
                let expected: std::collections::BTreeSet<_> = requests
                    .iter()
                    .map(Request::account)
                    .filter(|a| !attempted.contains(a))
                    .collect();
                let ready: std::collections::BTreeSet<_> = queue
                    .ready(
                        requests.clone(),
                        at + Duration::from_secs(1),
                        requests.len(),
                    )
                    .iter()
                    .map(Request::account)
                    .collect();
                assert_eq!(
                    ready, expected,
                    "limit {limit}, cut {cut}, deadline {deadline}"
                );
            }
        }
    }
}

struct Fixture {
    _lab: Lab,
    requests: Vec<Request>,
    queue: Queue,
    start: Instant,
    now: u64,
    calls: i64,
    issued: i64,
    first: i64,
    stopped: i64,
    error: i64,
}

impl Fixture {
    fn new() -> Self {
        let lab = Lab::new(&[Role::Writer]);
        let db = Recovery::at(lab.paths[0].clone());
        db.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
        db.request_at([4; 32], [5; 32], [6; 16], NOW).unwrap();
        let requests = db.pending_at(NOW).unwrap();
        let start = Instant::now();
        let mut queue = Queue::default();
        assert!(queue.ready(requests.clone(), start, 2).is_empty());
        Self {
            _lab: lab,
            requests,
            queue,
            start,
            now: 60,
            calls: 0,
            issued: 0,
            first: 0,
            stopped: 0,
            error: 0,
        }
    }

    fn run(&mut self, action: &str) -> i64 {
        let (phase, allowance, fails, succeeds, started) = match action {
            "Stop" => (1, 0, false, true, 60),
            "Deadline" => (2, 0, true, true, 60),
            "Partial" => (3, 1, false, true, 60),
            "Refused" => (4, 1, false, false, 60),
            "Delayed" => (5, 1, false, false, 90),
            "Complete" => (6, 2, false, true, 60),
            "Resume" => (7, 2, false, true, self.now + 1),
            "LateResume" => (10, 2, false, true, 150),
            "Rescan" => (8, 2, false, true, 61),
            "Boundary" => (9, 2, false, true, 120),
            _ => panic!("unknown recovery cooldown action {action}"),
        };
        // Delayed dispatch selected at 60 but starts its attempt at 90.
        let selected = if action == "Delayed" { 60 } else { started };
        let attempts = Cell::new(0);
        let first = Cell::new(0);
        let result = self.queue.dispatch(
            self.requests.clone(),
            self.start + Duration::from_secs(selected),
            2,
            || {
                if attempts.get() == allowance {
                    if fails {
                        // Exercise the same deadline guard as the issuer.
                        require_time(Instant::now())?;
                    }
                    return Ok(None);
                }
                Ok(Some(self.start + Duration::from_secs(started)))
            },
            |request| {
                if attempts.get() == 0 {
                    first.set(if request.account() == [1; 32] { 1 } else { 2 });
                }
                attempts.set(attempts.get() + 1);
                succeeds
            },
        );
        self.now = started;
        self.calls = attempts.get();
        self.first = first.get();
        self.issued = 0;
        self.stopped = 0;
        self.error = 0;
        match result {
            Ok((issued, stopped)) => {
                self.issued = issued as i64;
                self.stopped = i64::from(stopped);
            }
            Err(_) => self.error = 1,
        }
        phase
    }

    fn snapshot(&mut self, phase: i64) -> State {
        let due = |account| {
            self.queue
                .due
                .iter()
                .find(|((key, _, _), _)| *key == account)
                .unwrap()
                .1
                .duration_since(self.start)
                .as_secs() as i64
        };
        let a = due([1; 32]);
        let b = due([4; 32]);
        let ready = self
            .queue
            .ready(
                self.requests.clone(),
                self.start + Duration::from_secs(self.now),
                2,
            )
            .len() as i64;
        State::from([
            ("phase", phase),
            ("now", self.now as i64),
            ("a", a),
            ("b", b),
            ("calls", self.calls),
            ("issued", self.issued),
            ("first", self.first),
            ("stopped", self.stopped),
            ("error", self.error),
            ("ready", ready),
        ])
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut fixture = Fixture::new();
    match state["phase"] {
        0 => {}
        phase @ 1..=6 => {
            fixture.run(
                [
                    "Stop", "Deadline", "Partial", "Refused", "Delayed", "Complete",
                ][(phase - 1) as usize],
            );
        }
        8 => {
            fixture.run("Complete");
            fixture.run("Rescan");
        }
        _ => panic!("terminal recovery cooldown state"),
    }
    assert_eq!(fixture.snapshot(state["phase"]), *state);
    let phase = fixture.run(action);
    fixture.snapshot(phase)
}

#[test]
fn interrupted_batches_charge_only_attempts_and_resume_oldest_first() {
    let model = aterm_models_db::recovery_cooldown_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3, 4, 5, 6, 7]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                let mut unattempted_charge = observed.clone();
                unattempted_charge.insert("b", observed["b"] + 1);
                let mut phantom = observed.clone();
                phantom.insert("issued", observed["issued"] + 1);
                assert!(!model.successors(action.name, &state).contains(&phantom));
                assert!(
                    !model
                        .successors(action.name, &state)
                        .contains(&unattempted_charge)
                );
            }
        }
    }
}
