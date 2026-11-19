// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_exact_native_ancestors_supply_an_inherited_ceiling() {
    let parent = Holder {
        pid: 42,
        birth_us: 100,
    };
    let outside = Holder {
        pid: 43,
        birth_us: 101,
    };
    let held = HeldSlots {
        holders: vec![Held::verified(parent, 4), Held::verified(outside, 3)],
    };
    assert_eq!(held.budget(&[parent]), (3, Some(4)));
    assert_eq!(held.budget(&[]), (7, None));
    assert_eq!(
        held.budget(&[Holder {
            pid: parent.pid,
            birth_us: 99
        }]),
        (7, None),
        "reused PID is not ancestry"
    );
    let unknown = HeldSlots {
        holders: vec![Held::unmeasured(None, 14)],
    };
    assert_eq!(
        unknown.budget(&[parent]),
        (14, None),
        "unknown never inherits"
    );
    let nested = HeldSlots {
        holders: vec![Held::verified(parent, 4), Held::verified(outside, 2)],
    };
    assert_eq!(
        nested.budget(&[parent, outside]),
        (0, Some(2)),
        "nested ceilings are not additive"
    );
}

#[test]
fn distinct_same_owner_leases_form_one_ceiling_shared_by_siblings_and_outsiders() {
    let owner = Holder {
        pid: 42,
        birth_us: 100,
    };
    let child1 = Holder {
        pid: 43,
        birth_us: 101,
    };
    let child2 = Holder {
        pid: 44,
        birth_us: 102,
    };
    let old = HeldSlots {
        holders: vec![
            Held::verified(owner, 1),
            Held::verified(owner, 1),
            Held::verified(owner, 2),
        ],
    };
    assert_eq!(
        old.budget(&[child1, owner]),
        (0, Some(4)),
        "distinct reservations from one native scheduler are additive"
    );
    let first = vec![(vec![child1, owner], 2)];
    assert_eq!(old.budget_with(&[child2, owner], &first), (0, Some(2)));
    assert_eq!(
        old.budget_with(&[], &first),
        (2, None),
        "old remaining2 + independently counted child2 = held4"
    );
    let both = vec![(vec![child1, owner], 2), (vec![child2, owner], 2)];
    assert_eq!(
        old.budget_with(&[owner], &both),
        (0, Some(0)),
        "siblings cannot each receive the aggregate ceiling"
    );
    assert_eq!(
        old.budget_with(&[], &both),
        (0, None),
        "outside accounting counts the four held ledger cores once"
    );
    let unknown = HeldSlots {
        holders: vec![old.holders[0].clone(), Held::unmeasured(None, 14)],
    };
    assert_eq!(
        unknown.budget(&[owner]),
        (14, Some(1)),
        "unknown reservations never join a known owner group"
    );
}

#[test]
fn different_native_ancestor_ceilings_and_reused_pids_never_collapse() {
    let grandparent = Holder {
        pid: 40,
        birth_us: 90,
    };
    let parent = Holder {
        pid: 42,
        birth_us: 100,
    };
    let child = Holder {
        pid: 43,
        birth_us: 101,
    };
    let sibling = Holder {
        pid: 44,
        birth_us: 102,
    };
    let old = HeldSlots {
        holders: vec![
            Held::verified(grandparent, 8),
            Held::verified(parent, 2),
            Held::verified(parent, 2),
        ],
    };
    let first = vec![(vec![sibling, grandparent], 6)];
    assert_eq!(
        old.budget_with(&[child, parent, grandparent], &first),
        (0, Some(2)),
        "the different grandparent ceiling still binds"
    );
    let both = vec![
        (vec![sibling, grandparent], 6),
        (vec![child, parent, grandparent], 2),
    ];
    assert_eq!(old.budget_with(&[parent, grandparent], &both), (0, Some(0)));
    assert_eq!(
        old.budget_with(&[], &both),
        (4, None),
        "nearest-owner credit is used once: remaining4 + ledger8 = original12"
    );
    let reused = Holder {
        pid: parent.pid,
        birth_us: parent.birth_us + 1,
    };
    let different = HeldSlots {
        holders: vec![Held::verified(parent, 4), Held::verified(reused, 2)],
    };
    assert_eq!(
        different.budget(&[parent]),
        (2, Some(4)),
        "numeric PID equality cannot aggregate another birth"
    );
}

#[test]
fn verified_owner_without_a_recorded_width_backs_one_nested_core() {
    let owner = Holder {
        pid: 42,
        birth_us: 100,
    };
    let child = Holder {
        pid: 43,
        birth_us: 101,
    };
    let old = HeldSlots {
        holders: vec![Held::unmeasured(Some(owner), 36)],
    };
    assert_eq!(old.budget(&[]), (36, None));
    assert_eq!(old.budget(&[owner]), (0, Some(1)));
    let first = vec![(vec![child, owner], 1)];
    assert_eq!(old.budget_with(&[], &first), (35, None));
    assert_eq!(old.budget_with(&[owner], &first), (0, Some(0)));
    let mut repeated = old;
    repeated.holders.push(Held::unmeasured(Some(owner), 36));
    assert_eq!(repeated.budget(&[owner]), (0, Some(1)));
}
