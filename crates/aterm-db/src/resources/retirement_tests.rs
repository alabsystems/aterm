// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::resources::dispatch::{Admission, Invoked};
use aterm_secrets::{
    client,
    commons::{
        constraints::{Fact, Frontier},
        journal::Journal,
        node::Edit,
        roster::Grant,
    },
};

use std::collections::BTreeSet;

const OPERATION: [u8; 32] = [78; 32];

fn cutoff(lab: &Lab, fact: Fact) {
    let bootstrap = lab.bootstrap(0);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let identity = client::machine_identity(&lab.paths[0]).unwrap();
    let journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let draft = admin
        .prepare_commons_rotation(
            &journal,
            (0..2)
                .map(|n| {
                    Grant::recorded(
                        format!("m{}", n + 1),
                        lab.bootstrap(n).request().unwrap().binding().clone(),
                        Role::Writer,
                    )
                })
                .collect(),
        )
        .unwrap();
    let payload = draft.open(&identity).unwrap();
    let mut facts = draft.facts().to_vec();
    facts.push(fact);
    let update = admin
        .seal_commons_restricted(
            bootstrap.trust().network,
            Edit::Update {
                parent: draft.parents()[0],
                hosts: &["m1".into(), "m2".into()],
                payload: payload.as_slice(),
            },
            &facts,
        )
        .unwrap();
    drop(journal);
    for n in 0..2 {
        let bootstrap = lab.bootstrap(n);
        Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())
            .unwrap()
            .ingest(update.bytes())
            .unwrap();
    }
}

struct Fixture {
    _lab: Lab,
    a: Resources,
    b: Resources,
    /// The adapter's finished call, which the one `Record` step on this world spends.
    invoked: Option<Invoked>,
    before: Snapshot,
    actors: [astream_wire::sync::Actor; 2],
    allocation: Allocation,
}

impl Fixture {
    /// This world in a lab of its own, for a step that must not see the steps run on this one.
    /// The copy holds no finished adapter call: only `Record` spends one.
    fn fork(&self) -> Self {
        let lab = self._lab.fork();
        Self {
            a: resources(&lab, 0, &self.allocation),
            b: resources(&lab, 1, &self.allocation),
            _lab: lab,
            invoked: None,
            before: self.before.clone(),
            actors: self.actors,
            allocation: self.allocation.clone(),
        }
    }
}

/// Two writers' world up to its first repair, the same for every environment of one lab kind
/// (log-backed or not): both resource tables enrolled, B's first reservation repaired across, and
/// a catalog-authored debit of A's.
struct Template {
    lab: Lab,
    allocation: Allocation,
    actors: [astream_wire::sync::Actor; 2],
    scope: Scope,
    log_backed: bool,
}

fn template(log_backed: bool) -> Template {
    let lab = if log_backed {
        Lab::with_foundation(&[Role::Writer, Role::Writer])
    } else {
        Lab::new(&[Role::Writer, Role::Writer])
    };
    if log_backed {
        client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
        let records = client::load_records(&lab.paths[0]).unwrap().encode();
        client::save_records(&lab.paths[1], &records).unwrap();
    }
    for n in 0..2 {
        assert_eq!(lab.bootstrap(n).is_log_backed().unwrap(), log_backed);
    }
    let actors = std::array::from_fn(|n| lab.bootstrap(n).request().unwrap().binding().actor());
    let allocation = allocation(&lab, &[3, 3]);
    let a = resources(&lab, 0, &allocation);
    let b = resources(&lab, 1, &allocation);
    let scope = a.initialize().unwrap();
    b.initialize().unwrap();
    let peer = b.reserve([1; 16], 1).unwrap();
    assert_ne!(peer.receipt.id, [0; 32]);
    rounds(&lab);
    // Existing installations have catalog-authored debits. Preserve their
    // cross-writer parents, rather than testing only newly authored history.
    raw(
        &lab,
        &allocation,
        ledger::Debit {
            allocation: allocation.id(),
            request: [1; 16],
            cost: 1,
            cumulative: 1,
        },
    );
    Template {
        lab,
        allocation,
        actors,
        scope,
        log_backed,
    }
}

/// Environment `mode`'s world, on a copy of `template`: A's adapter call finished but not yet
/// recorded, B's second reservation repaired across, then the mode's retirement.
fn fixture(template: &Template, mode: i64) -> Fixture {
    assert_eq!(template.log_backed, matches!(mode, 2 | 4));
    let lab = template.lab.fork();
    let (allocation, actors, scope) = (&template.allocation, template.actors, template.scope);
    let a = resources(&lab, 0, allocation);
    let b = resources(&lab, 1, allocation);
    let Admission::Fresh(permit) = a.admit([2; 16], 1, OPERATION).unwrap() else {
        panic!("fresh intent");
    };
    let answer = Observation::new(Outcome::Completed, [79; 32]).unwrap();
    // The adapter finished before retirement; only its observation is pending.
    let invoked = permit.invoke(|| Some(answer));
    b.reserve([2; 16], 1).unwrap();
    rounds(&lab);
    let before = a.snapshot().unwrap();
    assert_eq!(before.used.values().sum::<u64>(), 4);
    match mode {
        1 => lab.rotate(&[Role::Writer, Role::Reader]),
        2 => {
            client::remove_member_in_log(&lab.paths[0], actors[1]).unwrap();
            let records = client::load_records(&lab.paths[0]).unwrap().encode();
            client::save_records(&lab.paths[1], &records).unwrap();
        }
        3..=6 => cutoff(
            &lab,
            Fact::Cutoff {
                table: scope.digest(),
                actor: lab
                    .bootstrap(usize::from(mode != 5))
                    .request()
                    .unwrap()
                    .binding()
                    .actor(),
                through: (mode == 6).then_some(Frontier {
                    sequence: 1,
                    digest: [99; 32],
                }),
            },
        ),
        _ => panic!("unknown retirement mode"),
    }
    // This is the old append policy as an executable negative control.
    if matches!(mode, 1 | 3 | 4) {
        a.with_store(false, |session, store, _| {
            assert!(store.commit_catalog(session, b"must not append").is_err());
            Ok(())
        })
        .unwrap();
    }
    Fixture {
        _lab: lab,
        a,
        b,
        invoked: Some(invoked),
        before,
        actors,
        allocation: allocation.clone(),
    }
}

/// One model step on `world`, whose mode the action names at stage 0 and the state names after.
/// The refinement anchors on `Store::commit_liability` project through this function by name.
fn retirement_transition(
    world: &mut Fixture,
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mode = if state["stage"] == 0 {
        match action {
            "LegacyPeer" => 1,
            "LogPeer" => 2,
            "PeerCutoff" => 3,
            "LogCutoff" => 4,
            "LocalCutoff" => 5,
            "MissingFrontier" => 6,
            _ => panic!("unknown retirement environment"),
        }
    } else {
        state["mode"]
    };
    let Fixture {
        _lab: lab,
        a,
        b,
        invoked,
        before,
        actors,
        allocation: _,
    } = world;
    let mut next = state.clone();
    next.insert("mode", mode);
    next.insert("stage", state["stage"] + 1);
    let original = a.attempt([2; 16]);
    if state["stage"] > 0 {
        let (kind, accepted, observed) = match action {
            "Reserve" => {
                let result = a.reserve([3; 16], 1);
                if let Ok(result) = &result {
                    assert!(result.fresh);
                    let retry = a.reserve([3; 16], 1).unwrap();
                    assert!(!retry.fresh);
                    assert_eq!(retry.receipt, result.receipt);
                    assert!(a.reserve([4; 16], 1).is_err());
                }
                (1, result.is_ok(), false)
            }
            "Dispatch" => {
                let answer = Observation::new(Outcome::Rejected, [80; 32]).unwrap();
                let result = a.dispatch_once([3; 16], 1, OPERATION, || Some(answer));
                if let Ok(result) = &result {
                    assert!(result.invoked);
                    let retry = a
                        .dispatch_once([3; 16], 1, OPERATION, || panic!("duplicate entry"))
                        .unwrap();
                    assert!(!retry.invoked);
                    assert_eq!(retry.receipt, result.receipt);
                    assert_eq!(retry.observation, result.observation);
                    assert!(a.reserve([4; 16], 1).is_err());
                }
                (
                    2,
                    result.is_ok(),
                    result.is_ok_and(|r| r.observation.confirmed() == Some(answer)),
                )
            }
            "Record" => {
                let result = a.record(invoked.take().expect("one Record per world"));
                if let Ok(Some(original)) = &original {
                    assert_eq!(result.receipt, original.receipt);
                }
                let recorded = result.observation.confirmed().is_some();
                (3, recorded, recorded)
            }
            _ => panic!("unknown retirement append"),
        };
        next.insert("kind", kind);
        next.insert("accepted", i64::from(accepted));
        next.insert("observed", i64::from(observed));
        if accepted {
            a.with_store(false, |session, store, _| {
                let records = store.liability_evidence(session)?;
                let foreign = records
                    .iter()
                    .filter(|r| r.header().actor == session.actor() && r.header().sequence > 2)
                    .any(|r| r.header().parents.iter().copied().ne(r.header().previous));
                next.insert("foreign", i64::from(foreign));
                Ok(())
            })
            .unwrap();
        }
    }
    // Retired B still has one unspent unit; refusal must not be exhaustion.
    if mode <= 4 {
        assert!(b.reserve([3; 16], 1).is_err());
    }
    let snapshot = match a.snapshot() {
        Ok(snapshot) => snapshot,
        Err(_) => {
            assert_eq!(mode, 6, "only unresolved evidence denies a balance");
            before.clone()
        }
    };
    next.insert("a", snapshot.used[&actors[0]] as i64);
    next.insert("b", snapshot.used[&actors[1]] as i64);
    if state["stage"] > 0 && !matches!(mode, 2 | 6) {
        rounds(lab);
        assert_eq!(a.snapshot().unwrap(), b.snapshot().unwrap());
    }
    next
}

#[test]
fn retirement_appends_conform_and_catalog_negative_control_is_detected() {
    let model = aterm_models_db::resource_retirement_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let enabled = |state: &aterm_spec::interp::State| {
        model
            .actions
            .iter()
            .filter(|action| model.action_enabled(action.name, state))
            .map(|action| action.name)
            .collect::<Vec<_>>()
    };
    let sole = |action: &str, state: &aterm_spec::interp::State| {
        model.successors(action, state).into_iter().next().unwrap()
    };
    let init = model.init_state();
    let environments: Vec<_> = enabled(&init)
        .into_iter()
        .map(|environment| (environment, sole(environment, &init)))
        .collect();
    // The walk below is two levels deep and takes each action's first successor. Every step the
    // model reaches must be one of its steps, so a new action, a deeper one or a second successor
    // fails here, not silently.
    let steps: BTreeSet<_> = aterm_spec::interp::reachable_states(&model)
        .iter()
        .flat_map(|state| {
            enabled(state)
                .into_iter()
                .map(|action| (state.clone(), action))
        })
        .collect();
    let walked: BTreeSet<_> = environments
        .iter()
        .flat_map(|(environment, seeded)| {
            enabled(seeded)
                .into_iter()
                .map(|action| (seeded.clone(), action))
                .chain([(init.clone(), *environment)])
        })
        .collect();
    assert_eq!(walked, steps);
    assert_eq!(steps.len(), 24, "6 environments, 3 appends in each");
    // Each environment runs on its own copy of its lab kind's template, in its own temporary home,
    // on the lane's places, and a step reads the world it runs on. An accepted append changes that
    // world. In modes 1-4 `Record` only adds the observation of a debit the fixture already
    // counted, so `Reserve` follows it on its world with the same budget, and `Dispatch` runs first
    // on a copy of the world taken as the fixture finished, so a first append on a retired frontier
    // stays held. Modes 5 and 6 refuse all three on one world, so a refusal that latched could
    // stand in for a later one's.
    // GAP: a `Reserve` that succeeds only because a `Record` ran before it is seen only if
    // `Dispatch` shares the defect, and the foreign-parents check on the `Reserve` step also reads
    // `Record`'s append.
    let templates = [template(false), template(true)];
    aterm_spec::fs_lane::each(&environments, |(environment, seeded)| {
        let mode = seeded["mode"];
        let check = |world: &mut Fixture, state: &aterm_spec::interp::State, action: &str| {
            let next = retirement_transition(world, state, action);
            assert!(
                model.successors(action, state).contains(&next),
                "{action}: {state:?} -> {next:?}"
            );
        };
        let mut world = fixture(&templates[usize::from(matches!(mode, 2 | 4))], mode);
        let dispatch = (mode <= 4).then(|| world.fork());
        check(&mut world, &init, environment);
        for action in ["Record", "Reserve"] {
            check(&mut world, seeded, action);
        }
        let mut world = dispatch.unwrap_or(world);
        check(&mut world, seeded, "Dispatch");
    });
    // Conformance is the walk's; this asks only the model.
    let mut blocked = sole("Reserve", &sole("LegacyPeer", &init));
    blocked.insert("accepted", 0);
    assert!(!model.check_invariant("EligibleProgress", &blocked));
}
