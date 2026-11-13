// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SetupRestartHost, run_setup_restart};
use aterm_types::macos_setup::{Plan, Selection};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

#[derive(Default)]
struct State {
    alive: Vec<&'static str>,
    drops: Vec<&'static str>,
    accepted: usize,
    refused: usize,
}

struct Lease {
    name: &'static str,
    state: Rc<RefCell<State>>,
}

impl Lease {
    fn new(name: &'static str, state: &Rc<RefCell<State>>) -> Self {
        state.borrow_mut().alive.push(name);
        Self {
            name,
            state: Rc::clone(state),
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.state.borrow_mut();
        state.alive.retain(|name| *name != self.name);
        state.drops.push(self.name);
    }
}

struct Password {
    secret: aterm_secrets::SecretBytes,
    _lease: Lease,
}

struct Receipt {
    lease: Lease,
    accepted: bool,
}

impl Drop for Receipt {
    fn drop(&mut self) {
        if !self.accepted {
            self.lease.state.borrow_mut().refused += 1;
        }
    }
}

struct Host {
    state: Rc<RefCell<State>>,
    outcomes: VecDeque<Admission>,
    pending: bool,
    plans: Vec<Plan>,
    claims: usize,
    /// Who holds the claim at each look before it is free.
    held: VecDeque<Holder>,
    /// The last look found this process's startup coordinator holding it.
    startup_holding: bool,
    awaits: usize,
    /// The waits for another setup begun, and how many go by before a later choice
    /// overtakes this one (`None`: none does).
    queues: usize,
    overtaken_after: Option<usize>,
    backoffs: usize,
    dispatches: Vec<u64>,
    failures: Vec<String>,
    /// The slot came back: the plan was not admitted.
    slot_back: bool,
}

#[derive(Clone, Copy)]
enum Holder {
    /// This process's startup coordinator ([`super::StartupClaim`]).
    Startup,
    /// Another aterm process, or `aterm doctor`.
    Elsewhere,
}

enum Admission {
    Pending(&'static str),
    Rejected(&'static str),
    Admitted(u64),
    Superseded,
    /// Another setup held the records past the choice's wait.
    Busy,
}

const BUSY: &str = "another setup holds the records";

impl SetupRestartHost for Host {
    type Claim = Lease;
    type Busy = Lease;
    type Password = Password;
    type Receipt = Receipt;

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        self.claims += 1;
        assert!(
            !self.state.borrow().alive.contains(&"claim"),
            "retain the original automatic claim"
        );
        if let Some(holder) = self.held.pop_front() {
            self.startup_holding = matches!(holder, Holder::Startup);
            return Ok(None);
        }
        Ok(Some(Lease::new("claim", &self.state)))
    }

    fn startup_holds_claim(&self) -> bool {
        self.startup_holding
    }

    fn await_claim(&mut self) {
        // The choice keeps its slot, receipt and password while it waits.
        assert_eq!(self.state.borrow().alive.len(), 3);
        self.awaits += 1;
    }

    fn persist(&mut self, plan: &Plan) -> Result<Option<u64>, String> {
        assert_eq!(self.state.borrow().alive.len(), 4);
        self.plans.push(plan.clone());
        let outcome = self
            .outcomes
            .pop_front()
            .expect("scripted admission attempt");
        self.pending = matches!(outcome, Admission::Pending(_));
        match outcome {
            Admission::Pending(why) | Admission::Rejected(why) => Err(why.into()),
            Admission::Busy => Err(BUSY.into()),
            Admission::Admitted(revision) => Ok(Some(revision)),
            Admission::Superseded => Ok(None),
        }
    }

    fn pending(&self) -> bool {
        self.pending
    }

    fn backoff(&mut self) {
        assert!(self.pending);
        assert_eq!(self.state.borrow().alive.len(), 4);
        assert_eq!(self.state.borrow().accepted, 0);
        assert_eq!(self.state.borrow().refused, 0);
        self.backoffs += 1;
    }

    fn admitted(
        &mut self,
        revision: u64,
        claim: Self::Claim,
        busy: Self::Busy,
        password: Self::Password,
        mut receipt: Self::Receipt,
    ) {
        assert_eq!(self.state.borrow().alive.len(), 4);
        assert_eq!(password.secret.as_slice(), b"retained password");
        self.dispatches.push(revision);
        receipt.accepted = true;
        self.state.borrow_mut().accepted += 1;
        drop(claim);
        drop(busy);
        drop(password);
        drop(receipt);
    }

    fn failure(&mut self, why: &str) {
        self.failures.push(why.into());
    }

    fn held_elsewhere(&self, why: &str) -> bool {
        why == BUSY
    }

    fn queue(&mut self) -> bool {
        // The choice keeps its slot, receipt and password while it waits.
        assert!(self.state.borrow().alive.len() >= 3);
        assert_eq!(self.state.borrow().refused, 0);
        self.queues += 1;
        self.overtaken_after
            .is_none_or(|after| self.queues <= after)
    }
}

fn run(plan: &Plan, outcomes: Vec<Admission>) -> Host {
    let host = run_held(plan, Vec::new(), outcomes);
    assert_eq!(host.state.borrow().drops[0], "claim");
    host
}

/// [`run`] where the claim is held by `held`, look by look, before it is free.
fn run_held(plan: &Plan, held: Vec<Holder>, outcomes: Vec<Admission>) -> Host {
    run_overtaken(plan, held, outcomes, None)
}

/// [`run_held`], where a later choice overtakes this one after `overtaken_after` waits for
/// another setup.
fn run_overtaken(
    plan: &Plan,
    held: Vec<Holder>,
    outcomes: Vec<Admission>,
    overtaken_after: Option<usize>,
) -> Host {
    let state = Rc::new(RefCell::new(State::default()));
    let password = Password {
        secret: aterm_secrets::SecretBytes::from(b"retained password".as_slice()),
        _lease: Lease::new("password", &state),
    };
    let receipt = Receipt {
        lease: Lease::new("receipt", &state),
        accepted: false,
    };
    let busy = Lease::new("busy", &state);
    let mut host = Host {
        state,
        outcomes: outcomes.into(),
        pending: false,
        plans: Vec::new(),
        claims: 0,
        held: held.into(),
        startup_holding: false,
        awaits: 0,
        queues: 0,
        overtaken_after,
        backoffs: 0,
        dispatches: Vec::new(),
        failures: Vec::new(),
        slot_back: false,
    };
    let slot = run_setup_restart(&mut host, plan, password, receipt, busy);
    // A refusal's receipt is dropped before its slot comes back.
    assert_eq!(slot.is_some(), host.state.borrow().alive == ["busy"]);
    host.slot_back = slot.is_some();
    drop(slot);
    assert!(host.state.borrow().alive.is_empty());
    host
}

/// A choice that meets another setup is never dropped (System State design §3.3). This
/// process's startup coordinator, holding the claim while it reads the plan in force, lets
/// go by itself: the choice waits for it. Another process's setup, or `aterm doctor`,
/// holding the claim or the records past the choice's wait: the choice waits for it too,
/// keeping its slot, receipt and password, says so, and is applied once it finishes, with
/// no failure. A later choice overtaking it lets it go, with no failure either.
#[test]
fn a_choice_waits_for_another_setup_and_is_applied_when_it_finishes() {
    let host = run_held(
        &Plan::default(),
        vec![Holder::Startup, Holder::Startup],
        vec![Admission::Admitted(5)],
    );
    assert_eq!(host.claims, 3);
    assert_eq!((host.awaits, host.queues), (2, 0));
    assert_eq!(host.dispatches, [5]);
    assert!(host.failures.is_empty());
    assert_eq!(host.state.borrow().accepted, 1);
    assert!(!host.slot_back);

    let host = run_held(
        &Plan::default(),
        vec![Holder::Elsewhere, Holder::Elsewhere],
        vec![Admission::Admitted(6)],
    );
    assert_eq!((host.claims, host.awaits, host.queues), (3, 0, 2));
    assert_eq!(host.dispatches, [6]);
    assert!(host.failures.is_empty());
    assert_eq!(host.state.borrow().accepted, 1);
    assert!(!host.slot_back);

    let plan = Plan::default();
    let host = run(&plan, vec![Admission::Busy, Admission::Admitted(7)]);
    assert_eq!((host.claims, host.queues, host.backoffs), (1, 1, 0));
    assert_eq!(host.plans, vec![plan.clone(); 2]);
    assert_eq!(host.dispatches, [7]);
    assert!(host.failures.is_empty());
    assert_eq!(host.state.borrow().accepted, 1);

    for (held, outcomes) in [
        (vec![Holder::Elsewhere, Holder::Elsewhere], Vec::new()),
        (Vec::new(), vec![Admission::Busy, Admission::Busy]),
    ] {
        let host = run_overtaken(&plan, held, outcomes, Some(1));
        assert_eq!(host.queues, 2);
        assert!(host.dispatches.is_empty());
        assert!(host.failures.is_empty(), "a choice let go fails nothing");
        assert_eq!(host.state.borrow().refused, 1);
        assert!(host.slot_back, "a choice let go gives its slot back");
    }
}

#[test]
fn manual_final_admission_retains_the_original_choice_until_confirmation() {
    let plan = Plan {
        vault_url: "https://vault.example/reviewed".into(),
        ..Plan::default()
    };
    let host = run(
        &plan,
        vec![
            Admission::Pending("published final admission could not flush"),
            Admission::Pending("the same final admission is still unconfirmed"),
            Admission::Admitted(8),
        ],
    );
    assert_eq!(host.dispatches, [8]);
    assert_eq!(host.plans, vec![plan; 3]);
    assert_eq!(host.claims, 1);
    assert_eq!(host.backoffs, 2);
    assert_eq!(host.failures.len(), 2);
    assert_eq!(host.state.borrow().accepted, 1);
    assert!(!host.slot_back);
    assert_eq!(host.state.borrow().refused, 0);
}

#[test]
fn rejected_manual_admission_does_not_accept_an_older_same_plan() {
    let host = run(
        &Plan::default(),
        vec![Admission::Rejected("pre-final write refused")],
    );
    assert!(host.dispatches.is_empty());
    assert_eq!(host.plans.len(), 1);
    assert_eq!(host.backoffs, 0);
    assert_eq!(host.state.borrow().accepted, 0);
    assert_eq!(host.state.borrow().refused, 1);
    assert!(host.slot_back);
}

#[test]
fn a_superseded_manual_attempt_never_dispatches_or_accepts_its_receipt() {
    for pending in [false, true] {
        let mut outcomes = Vec::new();
        if pending {
            outcomes.push(Admission::Pending("the final admission needs confirmation"));
        }
        outcomes.push(Admission::Superseded);
        let host = run(&Plan::default(), outcomes);
        assert!(host.dispatches.is_empty());
        assert_eq!(host.plans.len(), 1 + usize::from(pending));
        assert_eq!(host.failures.len(), usize::from(pending));
        assert_eq!(host.backoffs, usize::from(pending));
        assert_eq!(host.state.borrow().accepted, 0);
        assert_eq!(host.state.borrow().refused, 1);
        assert!(host.slot_back);
    }
}

#[test]
fn a_confirmed_empty_manual_plan_acknowledges_its_exact_revision() {
    let plan = Plan {
        selection: Selection::none(),
        ..Plan::default()
    };
    let host = run(
        &plan,
        vec![
            Admission::Pending("empty admission was published before its flush failed"),
            Admission::Admitted(12),
        ],
    );
    assert_eq!(host.dispatches, [12]);
    assert_eq!(host.plans, [plan.clone(), plan]);
    assert_eq!(host.backoffs, 1);
    assert_eq!(host.state.borrow().accepted, 1);
    assert!(!host.slot_back);
    assert_eq!(host.state.borrow().refused, 0);
}
