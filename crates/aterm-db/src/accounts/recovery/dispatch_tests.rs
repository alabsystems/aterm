// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

struct Fixture {
    lab: Lab,
    admin: aterm_secrets::client::Admin,
    db: crate::accounts::activations::Activations,
    child: Digest,
    first: aterm_secrets::commons::accounts::Record,
    queue: Queue,
    issued: usize,
}

impl Fixture {
    fn new() -> Self {
        let (lab, admin, db, child, first) = prepared();
        Self {
            lab,
            admin,
            db,
            child,
            first,
            queue: Queue {
                cooldown: Duration::ZERO,
                ..Queue::default()
            },
            issued: 0,
        }
    }

    fn snapshot(&self, phase: i64) -> State {
        let view = self.db.inspect(self.child).unwrap();
        let generation = view["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["generation"].as_i64().unwrap())
            .max()
            .unwrap();
        State::from([
            ("phase", phase),
            ("issued", self.issued as i64),
            ("generation", generation),
            ("due", self.queue.due.len() as i64),
        ])
    }

    fn run(&mut self, action: &str) -> i64 {
        use crate::accounts::activations::State as AccountState;
        let requests = Recovery::at(self.lab.paths[0].clone());
        let phase = match action {
            "Issue" => {
                requests
                    .request(self.child, self.first.id(), [3; 16])
                    .unwrap();
                1
            }
            "Repeat" => 2,
            "Successor" => {
                let AccountState::Ready(current) = self.db.state(self.child).unwrap() else {
                    panic!("first recovery did not produce a successor")
                };
                requests.request(self.child, current, [4; 16]).unwrap();
                3
            }
            "Quiet" => 4,
            "Flood" => {
                for n in 10..18 {
                    requests
                        .request([n; 32], [n + 20; 32], [n + 40; 16])
                        .unwrap();
                }
                self.queue.capacity = 1;
                assert!(
                    self.queue
                        .ready(requests.pending().unwrap(), Instant::now(), 0)
                        .is_empty()
                );
                assert_eq!(
                    self.queue.due.len(),
                    1,
                    "a bogus notice already occupies the slot"
                );
                requests
                    .request(self.child, self.first.id(), [3; 16])
                    .unwrap();
                5
            }
            "Stop" | "StopDevice" => {
                let (account, parent) = if action == "Stop" {
                    let aterm_secrets::commons::accounts::Action::Prepare(terms) =
                        &self.first.request().action
                    else {
                        unreachable!()
                    };
                    let reference = terms.intent.unwrap();
                    (reference.account, reference.decision)
                } else {
                    (self.child, self.first.id())
                };
                let stop = self.db.stop_request(account, vec![parent]).unwrap();
                self.db.issue(&self.admin, [7; 16], stop).unwrap();
                requests
                    .request(self.child, self.first.id(), [3; 16])
                    .unwrap();
                if action == "Stop" { 6 } else { 8 }
            }
            "Unknown" => {
                let mut missing = self.first.id();
                missing[0] ^= 1;
                requests.request(self.child, missing, [3; 16]).unwrap();
                7
            }
            _ => panic!("unknown recovery dispatch action {action}"),
        };
        self.issued = self
            .queue
            .step(
                &self.lab.paths[0],
                &self.admin,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
        phase
    }
}

#[test]
fn recovery_filter_requires_the_preparations_exact_device() {
    let (lab, _, db, child, first) = prepared();
    let requests = Recovery::at(lab.paths[0].clone());
    requests.request(child, first.id(), [3; 16]).unwrap();
    let valid = requests.pending().unwrap().remove(0);
    let other = Lab::new(&[Role::Writer]);
    let mut foreign = valid.clone();
    foreign.requester = other.bootstrap(0).request().unwrap().binding().clone();
    let current = db.current_recoveries(&[foreign, valid.clone()]).unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].requester(), valid.requester());
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut fixture = Fixture::new();
    match state["phase"] {
        0 => {}
        phase @ 1..=3 => {
            fixture.run("Issue");
            if phase >= 2 {
                fixture.run("Repeat");
            }
            if phase >= 3 {
                fixture.run("Successor");
            }
        }
        _ => panic!("terminal recovery dispatch state"),
    }
    assert_eq!(fixture.snapshot(state["phase"]), *state);
    let phase = fixture.run(action);
    fixture.snapshot(phase)
}

#[test]
fn only_current_recovery_requests_dispatch_or_count_as_work() {
    let model = aterm_models_db::recovery_dispatch_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3, 4, 5]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 0);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let observed = transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&observed));
                let mut phantom = observed.clone();
                phantom.insert("issued", observed["issued"] + 1);
                assert!(!model.successors(action.name, &state).contains(&phantom));
            }
        }
    }
}
