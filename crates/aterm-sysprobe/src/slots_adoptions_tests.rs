// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

fn fixture() -> (aterm_tempfile::TempDir, PathBuf, PathBuf, Value) {
    let temp = aterm_tempfile::tempdir().unwrap();
    let pool = std::fs::canonicalize(temp.path()).unwrap();
    let slot = pool.join("slot-0");
    std::fs::write(&slot, b"").unwrap();
    let root = pool.join("adopted");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pid = std::process::id();
    let p = crate::kproc::record_read(pid).unwrap().unwrap();
    let value = aterm_json::json!({
        "schema": 1, "adopted": true, "pid": pid,
        "identity": {
            "pid": pid, "uid": p.uid,
            "birth": format!("darwin:{}:{}", p.start_s, p.start_us),
            "executable": "the original executable may exec"
        },
        "pool": pool, "worktree": null, "leases": [slot]
    });
    (temp, root, slot, value)
}

fn publish(root: &Path, value: &Value) {
    let path = root.join("claim.json");
    std::fs::write(&path, aterm_json::to_vec(value).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn judged(pool: &Path, root: Option<&Path>) -> super::super::SlotsRead {
    let mut book = super::super::SlotBook::default();
    let machine = super::super::Machine::default();
    let _ = book.judge(
        super::super::observe(pool, root, 0, 0).unwrap(),
        &machine,
        &mut |_| false,
    );
    book.judge(
        super::super::observe(pool, root, 1_000, 0).unwrap(),
        &machine,
        &mut |_| false,
    )
}

fn counts(read: super::super::SlotsRead) -> super::super::SlotCounts {
    match read {
        super::super::SlotsRead::Read { counts, .. } => counts,
        other => panic!("unexpected read: {other:?}"),
    }
}

#[test]
fn a_live_adoption_holds_an_unlocked_slot_and_its_birth_ends_the_claim() {
    let (temp, root, _, mut value) = fixture();
    publish(&root, &value);
    assert_eq!(counts(judged(temp.path(), Some(&root))).held, 1);
    let p = crate::kproc::record_read(std::process::id())
        .unwrap()
        .unwrap();
    let holder = Holder {
        pid: p.pid,
        birth_us: birth_us(&p),
    };
    assert_eq!(holds(&root, temp.path(), holder), Some(true));
    // Negative control: an ordinary empty-slot observation misses this real claim.
    assert_eq!(
        counts(judged(temp.path(), Some(&root.join("missing")))).free,
        1
    );
    let pid = std::process::id();
    let p = crate::kproc::record_read(pid).unwrap().unwrap();
    value
        .as_object_mut()
        .unwrap()
        .get_mut("identity")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(
            "birth".into(),
            Value::from(format!("darwin:{}:{}", p.start_s + 1, p.start_us)),
        );
    publish(&root, &value);
    assert_eq!(counts(judged(temp.path(), Some(&root))).free, 1);
    assert_eq!(holds(&root, temp.path(), holder), Some(false));
}

#[test]
fn missing_custody_malformed_claims_and_symlinks_never_prove_free_capacity() {
    let (temp, root, _, mut value) = fixture();
    assert_eq!(counts(judged(temp.path(), None)).unknown, 1);
    value
        .as_object_mut()
        .unwrap()
        .insert("leases".into(), Value::from("not an array"));
    publish(&root, &value);
    assert_eq!(counts(judged(temp.path(), Some(&root))).unknown, 1);
    std::fs::remove_file(root.join("claim.json")).unwrap();
    std::os::unix::fs::symlink(temp.path().join("absent"), root.join("claim.json")).unwrap();
    assert_eq!(counts(judged(temp.path(), Some(&root))).unknown, 1);
}

#[test]
fn conflicting_holders_and_unavailable_identity_remain_unknown() {
    let a = Holder {
        pid: 2,
        birth_us: 1,
    };
    let b = Holder {
        pid: 3,
        birth_us: 2,
    };
    assert_eq!(combine(&Sight::Sighted(a), Claim::Held(b)), Sight::Unproven);
    let (_temp, _root, _slot, value) = fixture();
    let uid = crate::kproc::record_read(std::process::id())
        .unwrap()
        .unwrap()
        .uid;
    let result = claims(&value, uid, &mut |_, _| Some(Claim::Unknown)).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].1, Claim::Unknown);
    assert!(claims(&value, uid, &mut |_, _| None).unwrap().is_empty());
}

#[test]
fn the_canonical_registry_can_claim_a_different_pool_and_requires_private_files() {
    let (pool, root, _, value) = fixture();
    let registry = aterm_tempfile::tempdir().unwrap();
    let moved = registry.path().join("adopted");
    std::fs::rename(root, &moved).unwrap();
    publish(&moved, &value);
    assert_eq!(counts(judged(pool.path(), Some(&moved))).held, 1);
    let p = crate::kproc::record_read(std::process::id())
        .unwrap()
        .unwrap();
    assert_eq!(
        holds(
            &moved,
            registry.path(),
            Holder {
                pid: p.pid,
                birth_us: birth_us(&p)
            }
        ),
        Some(false)
    );
    std::fs::set_permissions(
        moved.join("claim.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_eq!(counts(judged(pool.path(), Some(&moved))).unknown, 1);
}

#[test]
fn replacing_a_claim_after_its_read_refuses_its_old_contents() {
    let (_pool, root, _, value) = fixture();
    publish(&root, &value);
    let path = root.join("claim.json");
    let uid = crate::kproc::record_read(std::process::id())
        .unwrap()
        .unwrap()
        .uid;
    assert!(
        record_with(&path, uid, &mut || {
            let replacement = root.join("replacement");
            std::fs::write(&replacement, aterm_json::to_vec(&value).unwrap()).unwrap();
            std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::rename(replacement, &path).unwrap();
        })
        .is_none()
    );
}
