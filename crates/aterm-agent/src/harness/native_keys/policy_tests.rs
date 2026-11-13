// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn assignment() -> Assignment {
    Assignment {
        subject: Subject {
            scope: Scope::Intent,
            account: [1; 32],
            revision: [2; 32],
            provider: Provider::Google,
        },
        warm: vec!["m117".into()],
        automatic: None,
    }
}

#[test]
fn native_policy_binds_revision_provider_and_exact_member() {
    let mut policy = Policy::default();
    let row = assignment();
    policy.set(row.clone()).unwrap();
    assert!(policy.permits(row.subject, "m117"));
    assert!(!policy.permits(row.subject, "M117.local"));
    assert!(!policy.permits(
        Subject {
            revision: [3; 32],
            ..row.subject
        },
        "m117"
    ));
    assert!(!policy.permits(
        Subject {
            provider: Provider::Meta,
            ..row.subject
        },
        "m117"
    ));
    assert_eq!(Policy::parse(&policy.text()).unwrap(), policy);
    let mut held = row;
    held.warm.clear();
    policy.set(held.clone()).unwrap();
    assert!(!policy.permits(held.subject, "m117"));
}

#[test]
fn native_policy_refuses_aliases_duplicates_and_extra_slots() {
    for warm in [vec!["M1"], vec!["m1", "m1"], vec!["m1", "m2", "m3", "m4"]] {
        let mut row = assignment();
        row.warm = warm.into_iter().map(str::to_owned).collect();
        assert!(row.validate().is_err());
    }
    let mut p = Policy::default();
    p.set(assignment()).unwrap();
    let raw = p.text();
    assert!(Policy::parse(&(raw.clone() + raw.trim_start_matches("version = 1\n"))).is_err());
    assert!(Policy::parse(&(raw + "arbitrary = true\n")).is_err());
}

#[test]
fn automatic_policy_is_explicit_delegation_and_keeps_fixed_semantics() {
    let mut fixed = Policy::default();
    fixed.set(assignment()).unwrap();
    assert!(fixed.automatic(Provider::Google).unwrap().is_none());
    assert!(!fixed.text().contains("allocation"));
    let mut row = assignment();
    row.warm.clear();
    row.automatic = Some(Allocator {
        actor: [7; 48],
        machine: [8; 16],
        grant: [9; 16],
    });
    let mut policy = Policy::default();
    policy.set(row.clone()).unwrap();
    assert!(!policy.permits(row.subject, "m117"));
    assert_eq!(Policy::parse(&policy.text()).unwrap(), policy);
    let mut other = row.clone();
    other.subject.account = [9; 32];
    assert!(policy.set(other).is_err());
    let mut other = row.clone();
    other.subject.scope = Scope::Preparation;
    assert!(policy.set(other).is_err());
    let mut duplicate = row.clone();
    duplicate.subject.revision = [10; 32];
    policy.set(duplicate.clone()).unwrap();
    assert_eq!(policy.assignments, vec![duplicate]);
    row.automatic.as_mut().unwrap().actor = [0; 48];
    assert!(row.validate().is_err());
    assert!(Policy::parse(&policy.text().replace("automatic", "fixed")).is_err());
}
