// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding for the harness core's bounded limit-recovery ladder
//! (`harness::limits`, design §11 item 7). The ledger ring (`harness::ring`)
//! and the grid spine's turn machine (`harness::observe`) had binds here too;
//! both subsystems were deleted with the second harness stack on 2026-09-23,
//! and their models and binds with them.
//!
//! A `ty`-green model on its own is a statement about the DESCRIPTION of the
//! code. These tests are what make it a statement about the code that
//! compiled: they drive the real classifier, table and engine against real
//! evidence, project what those produce onto the model's variables, and check
//! every observed transition against the same derived model checked at Tier
//! 0 — by the in-process interpreter always, and additionally by `ty trace
//! validate` wherever that binary is installed.
//!
//! The machine also carries FORGED successors the model must refuse, so a
//! green binding is never vacuous: a model that admits everything would pass
//! the positive half of every test here and fail every negative one.
//!
//! What is MEASURED and what is not. Measured: the engine's decisions for real
//! classifications at injected clock values. Not measured: any evidence the
//! design records as unconfirmed (§5.8.9) — a hook payload this suite hands in
//! is a value the classifier may receive, never one it has been seen to
//! receive.

use std::collections::BTreeMap;

use aterm_agent::harness::limits::{
    Action, ActionTable, Carry, Class, Classification, Decision, Evidence, Guards, LimitsConfig,
    Refusal, State as Engine, Step, TableError, Terminal, Verdict, WindowKind, classify, step,
};
use aterm_agent::harness::source::Source;
use aterm_spec::derive::{Model, harness_failure_recovery_model};
use aterm_spec::verify;

/// The model's variables, as the interpreter and `ty` both take them.
type Vars = BTreeMap<&'static str, i64>;

fn small(value: u64) -> i64 {
    i64::try_from(value).expect("a bounded test quantity")
}

/// Check one observed transition against the model's named action, on every
/// tier that is installed.
fn assert_transition(model: &Model, before: &Vars, after: &Vars, action: &str, label: &str) {
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        after,
        Some(action),
        label,
    );
    assert!(
        accepted,
        "the real {label} is not the model's `{action}`: {before:?} -> {after:?}\n{diagnostics}"
    );
}

/// A successor the real code did NOT produce must be refused as `action`.
///
/// `base` is what the real code DID produce; the forgery is that state with
/// `changes` applied, and it is offered as a successor of `before`. Keeping
/// the two apart is what makes the control a near-miss of the truth rather
/// than of the state before it.
fn assert_forged_rejected(
    model: &Model,
    before: &Vars,
    base: &Vars,
    action: &str,
    changes: &[(&'static str, i64)],
    label: &str,
) {
    let mut forged = base.clone();
    for (name, value) in changes {
        forged.insert(name, *value);
    }
    assert_ne!(
        &forged, base,
        "{label}: the forgery must differ from the truth"
    );
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        &forged,
        Some(action),
        label,
    );
    assert!(
        !accepted,
        "a forged `{action}` was admitted, so the bind is vacuous: {before:?} -> {forged:?}\n\
         {diagnostics}"
    );
}

// ---------------------------------------------------------------------------
// §11 item 7 — the limit-recovery ladder
// ---------------------------------------------------------------------------

/// Every clock value in this half is a literal; nothing reads a wall clock.
const NOW: i64 = 1_789_660_000;
const GEN: u64 = 25;
/// The reset the five-hour window reports, inside the shipped `max_wait_h`.
const RESET: i64 = NOW + 7200;

fn stop_failure(error: &str) -> Evidence {
    Evidence::StopFailure {
        error: error.to_string(),
        details: None,
    }
}

fn window(which: WindowKind, pct: f64, resets_at: Option<i64>) -> Evidence {
    Evidence::Window {
        which,
        used_pct: Some(pct),
        resets_at,
        source: Source::StatusLine,
        age_s: Some(8),
    }
}

fn classified(evidence: &[Evidence]) -> Classification {
    classify(evidence, NOW).unwrap_or_else(|| panic!("no classification for {evidence:?}"))
}

fn action_of(decided: &Step) -> Option<Action> {
    match &decided.decision {
        Decision::Act { action, .. } => Some(*action),
        _ => None,
    }
}

/// The action coding of the model's `act`: the closed vocabulary of §5.8.4.
fn act_code(action: Option<Action>) -> i64 {
    match action {
        None => 0,
        Some(Action::LetVendorRetry) => 1,
        Some(Action::Wait) => 2,
        Some(Action::Retry) => 3,
        Some(Action::SwitchModel | Action::SwitchAccount) => 4,
        Some(Action::Relogin) => 5,
        Some(Action::Escalate) => 6,
        Some(other) => panic!("{other} is outside the ladder this bind drives"),
    }
}

/// The real engine, projected onto the model's variables.
struct RecoveryBound {
    cfg: LimitsConfig,
    guards: Guards,
    model: Model,
    engine: Option<Engine>,
    /// The last action the ENGINE decided in the live generation (its own
    /// output, not this test's opinion); a settled generation has none.
    act: i64,
    now: i64,
    rows: u64,
    vars: Vars,
    transitions: usize,
}

impl RecoveryBound {
    fn new(cfg: LimitsConfig, guards: Guards) -> Self {
        let model = harness_failure_recovery_model();
        let bound = RecoveryBound {
            cfg,
            guards,
            vars: model.init_state(),
            model,
            engine: None,
            act: 0,
            now: NOW,
            rows: 1,
            transitions: 0,
        };
        assert_eq!(
            bound.project(),
            bound.vars,
            "no classification yet must be the model's Init"
        );
        bound
    }

    fn project(&self) -> Vars {
        let (class, phase, inflight, spent, dwell, generation) = match &self.engine {
            None => (0, 0, 0, 0, 0, 0),
            Some(engine) => {
                let phase = match engine.terminal {
                    Some(Terminal::Escalated) => 4,
                    // Settled or Generation: the generation is over and the
                    // host holds no live classification any more.
                    Some(_) => 0,
                    None if engine.in_flight.is_some() => 2,
                    None => 1,
                };
                // Cross-checks: the two phases that ARE readable off the real
                // state must agree with the phase this projection reports.
                assert_eq!(
                    phase == 2,
                    engine.in_flight.is_some(),
                    "phase 2 is exactly one action awaiting its verdict"
                );
                assert_eq!(
                    phase == 4,
                    engine.terminal == Some(Terminal::Escalated),
                    "phase 4 is exactly the escalated terminal"
                );
                let class = Class::ALL
                    .iter()
                    .position(|candidate| *candidate == engine.class)
                    .expect("every class is in Class::ALL");
                let dwell = engine
                    .last_switch_at
                    .is_some_and(|at| self.now < at.saturating_add(small(self.cfg.min_dwell_s)));
                (
                    i64::try_from(class).expect("eight classes"),
                    phase,
                    i64::from(engine.in_flight.is_some()),
                    i64::from(engine.budget.used(self.cfg.budget, self.now)),
                    i64::from(dwell),
                    small(engine.generation.saturating_sub(GEN)),
                )
            }
        };
        BTreeMap::from([
            ("class", class),
            ("phase", phase),
            ("inflight", inflight),
            ("act", if phase == 0 { 0 } else { self.act }),
            ("spent", spent),
            ("dwell", dwell),
            ("gen", generation),
            // The three witnesses the shipping engine never sets. That it
            // never does is the claim; the forged successors below are the
            // control.
            ("stale", 0),
            ("auto", 0),
            ("silent", 0),
            ("broke", 0),
        ])
    }

    fn settle(&mut self, action: &str) {
        let after = self.project();
        let label = format!("recovery step {} ({action})", self.transitions);
        let before = self.vars.clone();
        assert_transition(&self.model, &before, &after, action, &label);
        self.vars = after;
        self.transitions += 1;
    }

    /// The real code changed nothing the model names — a refusal, or evidence
    /// folded into a live classification.
    fn assert_unchanged(&self, why: &str) {
        assert_eq!(self.project(), self.vars, "{why} must change nothing");
    }

    fn forged_rejected(&self, action: &str, changes: &[(&'static str, i64)]) {
        assert_forged_rejected(
            &self.model,
            &self.vars,
            &self.vars,
            action,
            changes,
            "recovery ladder forgery",
        );
    }

    /// Fold one event into the live generation. The engine's own `act` is
    /// cleared where the class moved, because the decision the old row made
    /// went with the row.
    fn observe(&mut self, event: &aterm_agent::harness::limits::Event, action: &str) {
        let before = self
            .engine
            .as_ref()
            .map(|engine| engine.class)
            .expect("a classified generation");
        let engine = self.engine.as_mut().expect("a classified generation");
        engine.observe(event, self.now);
        if engine.class != before {
            self.act = 0;
        }
        self.settle(action);
    }

    /// The host classifies and builds the state for one generation.
    fn classify_as(&mut self, c: &Classification, action: &str) {
        let carry = self
            .engine
            .as_ref()
            .map_or_else(Carry::default, Engine::carry);
        let generation = self.engine.as_ref().map_or(GEN, |engine| engine.generation);
        self.engine = Some(Engine::new(c, self.now, generation, carry));
        self.act = 0;
        self.settle(action);
    }

    /// Ask the engine for one decision. Returns what it decided, unrecorded.
    fn decide(&self) -> Step {
        let engine = self.engine.as_ref().expect("a classified generation");
        step(&self.cfg, engine, &self.guards, self.now, engine.generation)
    }

    /// Ask, then record what it decided, then check the model admits it.
    fn act_now(&mut self, expected: Action, action: &str) -> Step {
        let decided = self.decide();
        assert_eq!(
            action_of(&decided),
            Some(expected),
            "the engine decided {:?}, not {expected}",
            decided.decision
        );
        let engine = self.engine.as_mut().expect("a classified generation");
        engine.commit(&decided, self.rows, self.now);
        self.rows += 1;
        self.act = act_code(Some(expected));
        self.settle(action);
        decided
    }

    fn verdict(&mut self, verdict: Verdict) {
        let engine = self.engine.as_mut().expect("a classified generation");
        engine.verdict(verdict, self.now);
        self.settle("Verdict");
    }

    fn advance_to(&mut self, now: i64, action: Option<&str>) {
        assert!(now >= self.now, "the injected clock only moves forward");
        self.now = now;
        match action {
            Some(action) => self.settle(action),
            None => self.assert_unchanged("time passing with no timer due"),
        }
    }
}

fn level_three_guards(cfg: &LimitsConfig) -> Guards {
    Guards::from_config(cfg, false)
}

#[test]
fn harness_recovery_real_five_hour_ladder_matches_the_model() {
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);

    // A real pair: the hook value plus an exhausted readable window. Nothing
    // above L1 may run on a single source, so the pair is what unlocks the
    // switch below.
    let five_hour = classified(&[
        stop_failure("rate_limit"),
        window(WindowKind::FiveHour, 96.0, Some(RESET)),
    ]);
    assert_eq!(five_hour.class, Class::Session5hLimit);
    assert!(!five_hour.unpaired, "StopFailure + window is a pair");
    bound.classify_as(&five_hour, "ClassifySession5h");

    // The row is [switch-account, switch-model, wait, retry, escalate]:
    // switch-account is L4 and this level is 3, so it is skipped, journaled.
    let switched = bound.act_now(Action::SwitchModel, "StartSwitch");
    assert!(
        switched
            .skipped
            .iter()
            .any(|k| k.action == Action::SwitchAccount && k.why.contains("level 4")),
        "the L4 candidate must be skipped and journaled: {:?}",
        switched.skipped
    );
    assert_eq!(bound.vars["inflight"], 1);

    // A second automatic action, while one awaits its verdict, is REFUSED and
    // queued. This is the model's `SecondActionWhileInFlight` at Buggy = 0.
    assert!(matches!(
        bound.decide().decision,
        Decision::Refused {
            why: Refusal::InFlight
        }
    ));
    bound.assert_unchanged("a refused second action");
    bound.settle("SecondActionWhileInFlight");
    // NEGATIVE CONTROL: the defect this law exists to refuse.
    bound.forged_rejected("SecondActionWhileInFlight", &[("inflight", 2)]);

    bound.verdict(Verdict::Executed);
    assert_eq!(bound.vars["spent"], 1, "the switch spent one budget slot");
    assert_eq!(bound.vars["dwell"], 1, "and started the dwell");

    // The ladder walks on from the candidate after the one that ran: `wait`
    // keyed on the window's own reset, then `retry` once that wait has ended.
    bound.act_now(Action::Wait, "ArmWait");
    assert!(
        matches!(bound.decide().decision, Decision::Wait { until } if until == RESET + 60),
        "a retry before its wait has ended is a wait, not an action"
    );

    // Time alone carries the dwell out: nothing else the model names moves.
    bound.advance_to(RESET + 120, Some("DwellElapses"));
    bound.act_now(Action::Retry, "StartRetry");
    bound.verdict(Verdict::Refused);
    assert_eq!(
        bound.vars["spent"], 1,
        "a retry is not a switch: the shared budget is untouched"
    );

    // The last candidate is `escalate`, and it too is conditional: while the
    // window's reset is inside `max_wait_h` and the budget has room, the
    // engine observes rather than calling a human. Nothing is committed, so
    // nothing the model names changes.
    let exhausted = bound.decide();
    assert!(matches!(
        exhausted.decision,
        Decision::Refused {
            why: Refusal::Exhausted
        }
    ));
    assert!(
        exhausted
            .skipped
            .iter()
            .any(|k| k.action == Action::Escalate && k.why.contains("observing")),
        "the skipped escalate must say why: {:?}",
        exhausted.skipped
    );
    bound.assert_unchanged("an exhausted ladder");
}

#[test]
fn harness_recovery_real_stale_generation_and_handover_match_the_model() {
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);

    let five_hour = classified(&[
        stop_failure("rate_limit"),
        window(WindowKind::FiveHour, 96.0, Some(RESET)),
    ]);
    bound.classify_as(&five_hour, "ClassifySession5h");

    // A request bound to a generation the state does not have is refused
    // before anything else is read — before the table, the timers or a guard.
    let engine = bound.engine.as_ref().expect("classified");
    let stale = step(
        &bound.cfg,
        engine,
        &bound.guards,
        bound.now,
        engine.generation + 1,
    );
    assert!(matches!(
        stale.decision,
        Decision::Refused {
            why: Refusal::StaleGeneration
        }
    ));
    assert!(stale.skipped.is_empty(), "nothing was even considered");
    bound.assert_unchanged("a stale-generation request");
    bound.settle("StaleGenerationRequest");
    // NEGATIVE CONTROL: acting for a generation the state no longer has.
    bound.forged_rejected("StaleGenerationRequest", &[("stale", 1)]);

    // The generation changes under the state: whatever was pending is
    // dropped, and the budget and dwell carry into the next one.
    bound.act_now(Action::SwitchModel, "StartSwitch");
    assert_eq!(bound.vars["inflight"], 1);
    let engine = bound.engine.as_mut().expect("classified");
    engine.observe(
        &aterm_agent::harness::limits::Event::GenerationChanged(GEN + 1),
        bound.now,
    );
    bound.act = 0;
    bound.settle("GenerationChanged");
    assert_eq!(bound.vars["inflight"], 0, "the pending action was dropped");
    assert_eq!(bound.vars["gen"], 1);

    // `unknown` is the fail-closed class: its row is `["escalate"]` and the
    // table REFUSES any attempt to give it a retry or a switch.
    let unknown = classified(&[stop_failure("invalid_request")]);
    assert_eq!(unknown.class, Class::Unknown);
    assert!(unknown.unpaired, "unknown is never a pair");
    bound.classify_as(&unknown, "ClassifyUnknown");

    let mut table = ActionTable::default();
    assert_eq!(table.get(Class::Unknown), [Action::Escalate]);
    assert!(matches!(
        table.set_names(Class::Unknown, &["retry", "escalate"]),
        Err(TableError::RetryNotAllowed {
            class: Class::Unknown
        })
    ));
    assert!(matches!(
        table.set_names(Class::Unknown, &["switch-model", "escalate"]),
        Err(TableError::SwitchNotAllowed {
            class: Class::Unknown,
            action: Action::SwitchModel
        })
    ));
    assert!(matches!(
        table.set_names(Class::NetworkOffline, &["switch-account", "escalate"]),
        Err(TableError::SwitchNotAllowed {
            class: Class::NetworkOffline,
            action: Action::SwitchAccount
        })
    ));
    assert_eq!(
        table.get(Class::Unknown),
        [Action::Escalate],
        "a refused row leaves the table unchanged"
    );
    // NEGATIVE CONTROLS: the two rows the table just refused, forged as
    // transitions of the live `unknown` generation.
    bound.forged_rejected("RetryInUnknownClass", &[("act", 3)]);
    bound.forged_rejected("SwitchInForbiddenClass", &[("act", 4)]);

    // Escalation waits for the third `unknown` inside the window, so the
    // engine's first answer is a wait, not an action.
    assert!(matches!(bound.decide().decision, Decision::Wait { .. }));
    bound.assert_unchanged("a wait for the escalate-after budget");
    let engine = bound.engine.as_mut().expect("classified");
    for at in [bound.now + 10, bound.now + 20] {
        engine.observe(
            &aterm_agent::harness::limits::Event::Reclassified(unknown.clone()),
            at,
        );
    }
    bound.assert_unchanged("more evidence for the same class");
    bound.act_now(Action::Escalate, "Escalate");
}

#[test]
fn harness_recovery_real_pinned_rows_for_spend_billing_and_auth_match_the_model() {
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);

    // Money is a human's call: the row is exactly `["escalate"]`, and the
    // table refuses anything else for it.
    let spend = classified(&[
        stop_failure("billing_error"),
        window(WindowKind::SpendLimit, 99.0, None),
    ]);
    assert_eq!(spend.class, Class::SpendBilling);
    assert!(!spend.unpaired, "hook plus spend window is a pair");
    bound.classify_as(&spend, "ClassifySpendBilling");

    let mut table = ActionTable::default();
    assert_eq!(table.get(Class::SpendBilling), [Action::Escalate]);
    assert!(matches!(
        table.set_names(Class::SpendBilling, &["wait", "escalate"]),
        Err(TableError::Pinned {
            class: Class::SpendBilling,
            action: Action::Wait
        })
    ));
    // NEGATIVE CONTROL: an automatic wait armed for a billing failure.
    bound.forged_rejected("ActOutsideThePinnedRow", &[("act", 2)]);

    bound.act_now(Action::Escalate, "Escalate");
    assert!(matches!(
        bound.decide().decision,
        Decision::Refused {
            why: Refusal::Terminal(Terminal::Escalated)
        }
    ));
    bound.assert_unchanged("a refused step on an escalated billing failure");
    bound.settle("ResumeEscalated");
    bound.forged_rejected("ResumeEscalated", &[("phase", 1), ("auto", 1)]);

    // Auth, on ONE source. CHANGED 2026-09-22 with the narrowed two-source
    // rule (§5.8.2): this block used to assert that the lone hook skipped
    // `relogin` "display-only" and escalated instead. `relogin` opens the
    // VENDOR's own sign-in door for a human to walk through (§5.8.10) — it
    // spends no money, no allowance and no account — so one source reaches
    // it, and the model admits the same transition it admits for a pair.
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);
    let auth = classified(&[stop_failure("authentication_failed")]);
    assert_eq!(auth.class, Class::Auth);
    assert!(auth.unpaired, "a hook value alone is one source");
    bound.classify_as(&auth, "ClassifyAuth");
    assert!(
        bound.decide().skipped.is_empty(),
        "nothing on the auth row is withheld from one source: {:?}",
        bound.decide().skipped
    );
    bound.act_now(Action::Relogin, "StartRelogin");
    bound.verdict(Verdict::Executed);

    // The paired half of the same row: `relogin` and nothing else, and the
    // pinned row refuses every other member whatever the evidence says.
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);
    let paired = Classification {
        unpaired: false,
        ..auth
    };
    bound.classify_as(&paired, "ClassifyAuth");
    assert!(matches!(
        table.set_names(Class::Auth, &["switch-model", "escalate"]),
        Err(TableError::Pinned {
            class: Class::Auth,
            action: Action::SwitchModel
        })
    ));
    // NEGATIVE CONTROL: an account or model switch to answer a login failure.
    bound.forged_rejected("ActOutsideThePinnedRow", &[("act", 4)]);

    bound.act_now(Action::Relogin, "StartRelogin");
    assert_eq!(bound.vars["inflight"], 1);
    bound.verdict(Verdict::Executed);
    assert_eq!(
        bound.vars["spent"], 0,
        "a relogin spends the relogin budget, never the switch budget"
    );
}

/// The model's two class-change laws, on the real engine: a hook value the
/// engine cannot place forces `unknown` and its ladder starts over, and a
/// generation reclassified as another class adopts that class's row. Both
/// mutants are offered as forged successors, so neither law is a ghost.
#[test]
fn harness_recovery_a_class_change_moves_the_ladder_cursor_with_it() {
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);

    let five_hour = classified(&[
        stop_failure("rate_limit"),
        window(WindowKind::FiveHour, 96.0, Some(RESET)),
    ]);
    bound.classify_as(&five_hour, "ClassifySession5h");

    // Walk the five-hour ladder one whole step, so the cursor is NOT at zero
    // when the class moves under it.
    bound.act_now(Action::SwitchModel, "StartSwitch");
    bound.verdict(Verdict::Executed);
    let advanced = bound.engine.as_ref().expect("classified").step;
    assert_eq!(advanced, 2, "the cursor is past the candidate that ran");

    // A `StopFailure.error` outside the closed list forces `unknown` — and
    // the cursor goes with the class. `unknown`'s row is ONE entry long, so
    // a cursor of 2 would walk it past its end for ever.
    bound.observe(
        &aterm_agent::harness::limits::Event::Evidence(stop_failure("a_renamed_hook_value")),
        "UnplaceableHookValue",
    );
    let engine = bound.engine.as_ref().expect("classified");
    assert_eq!(engine.class, Class::Unknown);
    assert_eq!(engine.step, 0, "the new row is entered at its beginning");
    // NEGATIVE CONTROL: the class flips and the ladder is left behind.
    bound.forged_rejected("UnplaceableHookValue", &[("silent", 1)]);

    // Two more inside the window make the burst, and the row's one candidate
    // — `escalate` — is reached. That is the whole point of the reset.
    for at in [bound.now + 1, bound.now + 2] {
        let engine = bound.engine.as_mut().expect("classified");
        engine.observe(
            &aterm_agent::harness::limits::Event::Evidence(stop_failure("a_renamed_hook_value")),
            at,
        );
    }
    bound.assert_unchanged("more unplaceable evidence for the same class");
    bound.advance_to(bound.now + 3, None);
    bound.act_now(Action::Escalate, "Escalate");

    // The other half: a live generation reclassified as `spend-billing`
    // adopts the pinned row instead of letting the old one decide.
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);
    bound.classify_as(&five_hour, "ClassifySession5h");
    bound.act_now(Action::SwitchModel, "StartSwitch");
    bound.verdict(Verdict::Executed);

    let spend = classified(&[
        stop_failure("billing_error"),
        window(WindowKind::SpendLimit, 99.0, None),
    ]);
    assert_eq!(spend.class, Class::SpendBilling);
    bound.observe(
        &aterm_agent::harness::limits::Event::Reclassified(spend),
        "ReclassifiedAsSpendBilling",
    );
    let engine = bound.engine.as_ref().expect("classified");
    assert_eq!(engine.class, Class::SpendBilling);
    assert_eq!(engine.step, 0);
    // NEGATIVE CONTROL: the class moves and the old row's decision stands —
    // a model switch left standing for a generation whose latest evidence is
    // `billing_error`.
    bound.forged_rejected("ReclassifiedAsSpendBilling", &[("act", 4)]);

    // Money is a human's call whatever the previous class was doing.
    bound.act_now(Action::Escalate, "Escalate");
}
