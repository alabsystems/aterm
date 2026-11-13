// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn manifest() -> Manifest {
    Manifest {
        version: 1,
        digest: "a".repeat(64),
        root: "root".into(),
        turn: "original".into(),
        members: vec![
            Member {
                thread: "root".into(),
                parent: None,
                turn: Some("original".into()),
            },
            Member {
                thread: "active".into(),
                parent: Some("root".into()),
                turn: Some("child-turn".into()),
            },
            Member {
                thread: "idle".into(),
                parent: Some("active".into()),
                turn: None,
            },
        ],
    }
}

fn valid(manifest: &Manifest) -> bool {
    Manifest::decode(
        &aterm_json::to_string(manifest).unwrap(),
        "root",
        "original",
    )
    .is_some()
}

#[test]
fn source_tree_requires_exact_origin_and_ordered_unique_members() {
    assert!(valid(&manifest()));
    let mut changed = manifest();
    changed.members.swap(1, 2);
    assert!(!valid(&changed));
    let mut changed = manifest();
    changed.members.push(changed.members[1].clone());
    assert!(!valid(&changed));
    let mut changed = manifest();
    changed.members[1].parent = Some("elsewhere".into());
    assert!(!valid(&changed));
    let mut changed = manifest();
    changed.members[0].turn = None;
    assert!(!valid(&changed));
    let mut changed = manifest();
    changed.digest = "A".repeat(64);
    assert!(!valid(&changed));
    let mut changed = manifest();
    changed.version = 2;
    assert!(!valid(&changed));
}

#[test]
fn member_admission_is_capsule_bound_and_never_invents_an_idle_turn() {
    let original = manifest();
    let mut ledger = Admission::new(original.clone());
    assert!(ledger.matches(&original));
    assert_eq!(
        ledger.admit(&"b".repeat(64), "active", Some("child-turn")),
        None
    );
    assert_eq!(
        ledger.admit(&original.digest, "active", Some("other")),
        None
    );
    assert_eq!(
        ledger.admit(&original.digest, "missing", Some("child-turn")),
        None
    );
    assert_eq!(
        ledger.admit(&original.digest, "root", Some("original")),
        None
    );
    assert_eq!(
        ledger.admit(&original.digest, "active", Some("child-turn")),
        Some(true)
    );
    assert_eq!(
        ledger.admit(&original.digest, "active", Some("child-turn")),
        Some(false)
    );
    assert_eq!(
        ledger.admit(&original.digest, "idle", Some("invented")),
        None
    );
    assert_eq!(ledger.admit(&original.digest, "idle", None), Some(true));
    assert_eq!(ledger.admit(&original.digest, "idle", None), Some(false));
    let mut changed = original;
    changed.members[1].turn = Some("replacement".into());
    assert!(!ledger.matches(&changed));
}
