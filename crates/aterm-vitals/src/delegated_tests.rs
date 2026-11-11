// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn p(pid: u32, start: u64, parent: u32) -> Process {
    Process {
        key: ProcessKey { pid, start },
        parent,
    }
}

fn job(root: ProcessKey, session: &str) -> Job {
    Job {
        root,
        session: session.into(),
        ai: "codex".into(),
        label: "build".into(),
    }
}

#[test]
fn two_jobs_under_one_daemon_never_claim_the_daemon_or_siblings() {
    let mut ledger = Ledger::default();
    ledger.register(job(p(20, 2, 10).key, "a")).unwrap();
    ledger.register(job(p(30, 3, 10).key, "b")).unwrap();
    let bindings = ledger.observe(&[
        p(10, 1, 1),
        p(20, 2, 10),
        p(21, 4, 20),
        p(30, 3, 10),
        p(31, 5, 30),
        p(40, 6, 10),
    ]);
    assert_eq!(bindings.len(), 4);
    assert_eq!(bindings.iter().filter(|b| b.root.pid == 20).count(), 2);
    assert_eq!(bindings.iter().filter(|b| b.root.pid == 30).count(), 2);
}

#[test]
fn detached_members_survive_parent_exit_but_not_pid_reuse() {
    let mut ledger = Ledger::default();
    let root = p(20, 2, 10).key;
    ledger.register(job(root, "a")).unwrap();
    ledger.observe(&[p(20, 2, 10), p(21, 3, 20)]);
    assert_eq!(
        ledger.observe(&[p(21, 3, 1)]),
        vec![Binding {
            process: p(21, 3, 1).key,
            root
        }]
    );
    assert!(ledger.observe(&[p(21, 9, 1)]).is_empty());
    assert!(ledger.jobs().is_empty());
}

#[test]
fn unreadable_prior_keys_stay_dormant_and_cannot_bridge_new_ancestry() {
    let mut ledger = Ledger::default();
    let root = p(20, 2, 10).key;
    let member = p(21, 3, 20).key;
    ledger.register(job(root, "a")).unwrap();
    ledger.observe(&[p(20, 2, 10), p(21, 3, 20)]);
    let unavailable = HashSet::from([member, p(99, 9, 1).key]);
    let retained = vec![Binding {
        process: member,
        root,
    }];
    assert_eq!(
        ledger.observe_with_unavailable(&[p(22, 4, 21)], &unavailable),
        retained
    );
    assert_eq!(ledger.jobs().len(), 1);
    assert_eq!(
        ledger.observe_with_unavailable(&[p(21, 9, 1), p(23, 10, 21)], &unavailable),
        retained
    );
    assert_eq!(ledger.observe(&[p(21, 3, 1), p(22, 4, 21)]).len(), 2);
    assert!(ledger.observe(&[p(21, 9, 1), p(23, 10, 21)]).is_empty());
    assert!(ledger.is_empty());
}

#[test]
fn an_unreadable_registered_root_is_retained_without_inventing_members() {
    let mut ledger = Ledger::default();
    let root = p(20, 2, 10).key;
    ledger.register(job(root, "a")).unwrap();
    assert!(
        ledger
            .observe_with_unavailable(&[p(21, 3, 20)], &HashSet::from([root]))
            .is_empty()
    );
    assert_eq!(ledger.jobs().len(), 1);
    assert_eq!(ledger.observe(&[p(20, 2, 10), p(21, 3, 20)]).len(), 2);
    assert!(ledger.observe(&[]).is_empty());
    assert!(ledger.is_empty());
}

#[test]
fn children_of_an_observed_detached_member_inherit_its_job() {
    let mut ledger = Ledger::default();
    let root = p(20, 2, 10).key;
    ledger.register(job(root, "a")).unwrap();
    ledger.observe(&[p(20, 2, 10), p(21, 3, 20)]);
    let bindings = ledger.observe(&[p(21, 3, 1), p(22, 4, 21)]);
    assert_eq!(bindings.len(), 2);
    assert!(bindings.iter().all(|binding| binding.root == root));
}

#[test]
fn nested_registration_wins_once_and_newer_parent_never_splices_a_chain() {
    let mut ledger = Ledger::default();
    let outer = p(20, 2, 1).key;
    let inner = p(21, 3, 20).key;
    ledger.register(job(outer, "a")).unwrap();
    ledger.observe(&[p(20, 2, 1), p(21, 3, 20), p(22, 4, 21)]);
    ledger.register(job(inner, "b")).unwrap();
    let bindings = ledger.observe(&[p(20, 2, 1), p(21, 3, 20), p(22, 4, 21), p(23, 1, 20)]);
    assert_eq!(
        bindings.iter().find(|b| b.process.pid == 22).unwrap().root,
        inner
    );
    assert!(!bindings.iter().any(|b| b.process.pid == 23));
    assert_eq!(
        ledger.register(job(inner, "c")),
        Err(RegisterError::Conflict)
    );
}

#[test]
fn carry_requires_a_registered_job_and_exact_live_member() {
    let root = p(20, 2, 1).key;
    let member = Binding {
        process: p(21, 3, 1).key,
        root,
    };
    let mut restored = Ledger::default();
    assert!(!restored.restore(member));
    restored.register(job(root, "a")).unwrap();
    assert!(restored.restore(member));
    assert_eq!(restored.observe(&[p(21, 3, 1)]), vec![member]);
    assert!(restored.observe(&[p(21, 4, 1)]).is_empty());
}
