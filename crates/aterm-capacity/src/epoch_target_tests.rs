// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn legacy_parent_alias_resolves_once_but_target_leaf_symlinks_refuse() {
    let world = aterm_tempfile::Builder::new()
        .prefix("epoch-target-alias-")
        .tempdir()
        .unwrap();
    let physical = world.path().canonicalize().unwrap();
    let alias = physical.join("parent-alias");
    std::os::unix::fs::symlink(&physical, &alias).unwrap();
    let requested = alias.join("nested/target");
    assert!(TargetLease::acquire(&requested).is_err());
    let resolved = resolve_legacy_target(&requested).unwrap();
    assert_eq!(resolved, physical.join("nested/target"));
    let lease = TargetLease::acquire(&resolved).unwrap();
    assert!(
        matches!(TargetLease::acquire(&resolve_legacy_target(&requested).unwrap()),
        Err(error) if error.kind()==io::ErrorKind::WouldBlock)
    );
    // Retargeting the caller's alias cannot change the pinned physical path.
    std::fs::remove_file(&alias).unwrap();
    let other = physical.join("other");
    std::fs::create_dir(&other).unwrap();
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    lease.check().unwrap();
    let leaf = physical.join("target-link");
    std::os::unix::fs::symlink(&resolved, &leaf).unwrap();
    assert!(resolve_legacy_target(&leaf).is_err());
}

#[test]
fn original_loan_keeps_target_exclusive_and_can_be_reused_after_drop() {
    let world = aterm_tempfile::Builder::new()
        .prefix("epoch-target-")
        .tempdir()
        .unwrap();
    let target = world.path().canonicalize().unwrap().join("nested/target");
    let mut lease = TargetLease::acquire(&target).unwrap();
    assert!(matches!(TargetLease::acquire(&target),Err(e) if e.kind()==io::ErrorKind::WouldBlock));
    let loan = lease.loan().unwrap();
    assert!(matches!(lease.loan(),Err(e) if e.kind()==io::ErrorKind::WouldBlock));
    loan.validate(&target).unwrap();
    drop(lease);
    assert!(matches!(TargetLease::acquire(&target),Err(e) if e.kind()==io::ErrorKind::WouldBlock));
    drop(loan);
    let mut next = TargetLease::acquire(&target).unwrap();
    let first = next.loan().unwrap();
    drop(first);
    next.loan().unwrap().validate(&target).unwrap();
}

#[test]
fn original_directory_or_lock_replacement_refuses_without_adopting() {
    let world = aterm_tempfile::Builder::new()
        .prefix("epoch-target-identity-")
        .tempdir()
        .unwrap();
    let target = world.path().canonicalize().unwrap().join("target");
    let mut lease = TargetLease::acquire(&target).unwrap();
    let loan = lease.loan().unwrap();
    std::fs::rename(target.join(LOCK), target.join("original-lock")).unwrap();
    std::fs::write(target.join(LOCK), b"foreign").unwrap();
    std::fs::set_permissions(target.join(LOCK), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(lease.check().is_err());
    assert!(loan.check().is_err());
    drop(loan);
    drop(lease);
    let lease = TargetLease::acquire(&target).unwrap();
    std::fs::rename(&target, world.path().join("original-target")).unwrap();
    std::fs::create_dir(&target).unwrap();
    assert!(lease.check().is_err());
}
