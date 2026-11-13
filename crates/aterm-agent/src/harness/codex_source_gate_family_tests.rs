// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
fn fixture(home: &Path, grant: &str, version: u64) -> Fixture {
    // Kernel-lock controls only; never a qualified peer, native grant or census.
    let gate = SourceWorkLease::for_home(home).unwrap().identity().unwrap();
    let caller = Caller {
        sid: "gate-fixture".into(),
        pid: std::process::id(),
        birth: 1,
    };
    Fixture {
        identity: Identity {
            family: family_key(&caller, &grant).unwrap(),
            caller,
            version,
            gate,
        },
        live: true.into(),
    }
}

#[test]
fn borrowed_family_reenters_without_relocking_and_other_grants_remain_live() {
    let home = aterm_tempfile::tempdir().unwrap();
    let a = fixture(home.path(), "grant-a", 1);
    let alias = fixture(home.path(), "grant-a", 9);
    let b = fixture(home.path(), "grant-b", 1);
    let work = FamilyWork::acquire(FamilySource::Fixture(&a)).unwrap();
    assert!(FamilyExclusive::acquire(FamilySource::Fixture(&alias)).is_err());
    // A copy of the descriptor (as a child forked before exec holds) must
    // not keep the family: the guard unlocks before it closes.
    let retained = work.held.file.try_clone().unwrap();
    drop(work);
    let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&a))
        .expect("the family is free once its guard drops, beside a retained copy");
    drop(retained);
    assert!(FamilyWork::acquire(FamilySource::Fixture(&alias)).is_err());
    let first = exclusive.borrow().unwrap();
    let second = exclusive.borrow().unwrap();
    first.current().unwrap();
    second.current().unwrap();
    assert_eq!(first.version(), 1);
    assert!(first.matches(FamilySource::Fixture(&alias)).is_err());
    assert!(first.matches(FamilySource::Fixture(&b)).is_err());
    let independent = FamilyWork::acquire(FamilySource::Fixture(&b)).unwrap();
    independent.current().unwrap();
    assert!(PublicationLock::for_home(home.path()).is_err());
}
#[test]
fn same_family_handoff_retains_shared_kernel_custody() {
    let home = aterm_tempfile::tempdir().unwrap();
    let source = fixture(home.path(), "grant-a", 1);
    let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&source)).unwrap();
    let FamilyExclusive {
        global: _global,
        held,
        ..
    } = exclusive;
    let held = OwnedFamily {
        held,
        mode: FAMILY_EXCLUSIVE.into(),
    };
    assert!(FamilyWork::acquire(FamilySource::Fixture(&source)).is_err());
    held.share_after_admission().unwrap();
    held.share_after_admission().unwrap();
    assert!(held.exclusive().is_err());
    held.borrowed().unwrap();
    let work = FamilyWork::acquire(FamilySource::Fixture(&source)).unwrap();
    assert!(FamilyExclusive::acquire(FamilySource::Fixture(&source)).is_err());
    drop(work);
    assert!(FamilyExclusive::acquire(FamilySource::Fixture(&source)).is_err());
    drop(held);
    let _exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&source)).unwrap();
}

#[test]
fn replaced_family_gate_during_handoff_never_reenables_borrowers() {
    let home = aterm_tempfile::tempdir().unwrap();
    let source = fixture(home.path(), "grant-a", 1);
    let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&source)).unwrap();
    let FamilyExclusive {
        global: _global,
        held,
        ..
    } = exclusive;
    let held = OwnedFamily {
        held,
        mode: FAMILY_EXCLUSIVE.into(),
    };
    std::fs::rename(&held.held.path, held.held.path.with_extension("old")).unwrap();
    private_file::write_private_new(
        held.held.path.parent().unwrap(),
        "gate",
        b"replacement",
        |_| Ok(()),
    )
    .unwrap();
    assert!(held.share_after_admission().is_err());
    assert!(held.borrowed().is_err());
    assert!(held.exclusive().is_err());
    assert_eq!(held.mode.load(std::sync::atomic::Ordering::Acquire), 0);
}

#[test]
fn replaced_source_family_cannot_restore_after_failed_upgrade() {
    let home = aterm_tempfile::tempdir().unwrap();
    let source = fixture(home.path(), "grant-b", 1);
    let original = FamilyExclusive::acquire(FamilySource::Fixture(&source)).unwrap();
    let FamilyExclusive {
        global: _global,
        held,
        ..
    } = original;
    let held = OwnedFamily {
        held,
        mode: FAMILY_EXCLUSIVE.into(),
    };
    held.share_after_admission().unwrap();
    std::fs::rename(&held.held.path, held.held.path.with_extension("old")).unwrap();
    private_file::write_private_new(
        held.held.path.parent().unwrap(),
        "gate",
        b"replacement",
        |_| Ok(()),
    )
    .unwrap();
    assert!(held.exclusive_after_census().is_err());
    assert_eq!(held.mode.load(std::sync::atomic::Ordering::Acquire), 0);
    assert!(held.share_after_admission().is_err());
    assert!(held.borrowed().is_err());
    assert_eq!(held.mode.load(std::sync::atomic::Ordering::Acquire), 0);
}

#[test]
fn competing_fd_blocks_upgrade_and_only_real_shared_reacquisition_restores_borrow() {
    // Physical lock-state controls only; no fabricated census/cancel/ACK.
    let home = aterm_tempfile::tempdir().unwrap();
    let source = fixture(home.path(), "grant-b", 1);
    let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&source)).unwrap();
    let FamilyExclusive {
        global: _global,
        held,
        ..
    } = exclusive;
    let held = OwnedFamily {
        held,
        mode: FAMILY_EXCLUSIVE.into(),
    };
    held.share_after_admission().unwrap();
    let competing = FamilyWork::acquire(FamilySource::Fixture(&source)).unwrap();
    assert_eq!(held.exclusive_after_census(), Err(BUSY));
    assert_eq!(held.mode.load(std::sync::atomic::Ordering::Acquire), 0);
    assert!(held.borrowed().is_err());
    // An OS failure is never interpreted as still owning the old SH.
    held.share_after_admission().unwrap();
    held.borrowed().unwrap();
    assert!(FamilyExclusive::acquire(FamilySource::Fixture(&source)).is_err());
    drop(competing);
    held.exclusive_after_census().unwrap();
    assert!(FamilyWork::acquire(FamilySource::Fixture(&source)).is_err());
}

#[test]
fn repeated_family_epochs_do_not_keep_old_exclusive_or_revive_old_handles() {
    let home = aterm_tempfile::tempdir().unwrap();
    let b = fixture(home.path(), "grant-b", 1);
    let c = fixture(home.path(), "grant-c", 1);
    let first = FamilyExclusive::acquire(FamilySource::Fixture(&b)).unwrap();
    let FamilyExclusive {
        global: _global_b,
        held,
        ..
    } = first;
    let old_b = OwnedFamily {
        held,
        mode: FAMILY_EXCLUSIVE.into(),
    };
    old_b.share_after_admission().unwrap();
    old_b.exclusive_after_census().unwrap();
    let next = FamilyExclusive::acquire(FamilySource::Fixture(&c)).unwrap();
    let FamilyExclusive {
        global: _global_c,
        held,
        ..
    } = next;
    let old_c = OwnedFamily {
        held,
        mode: FAMILY_EXCLUSIVE.into(),
    };
    old_c.share_after_admission().unwrap();
    old_b.release_after_effect().unwrap();
    assert!(old_b.borrowed().is_err());
    assert!(old_b.share_after_admission().is_err());
    assert!(old_b.exclusive_after_census().is_err());
    old_c.exclusive_after_census().unwrap();
    let final_b = FamilyExclusive::acquire(FamilySource::Fixture(&b)).unwrap();
    let FamilyExclusive {
        global: _global_b2,
        held,
        ..
    } = final_b;
    let current_b = OwnedFamily {
        held,
        mode: FAMILY_EXCLUSIVE.into(),
    };
    current_b.share_after_admission().unwrap();
    old_c.release_after_effect().unwrap();
    // Exact historical release cannot unlock the new epoch's distinct FD.
    old_b.release_after_effect().unwrap();
    assert!(FamilyExclusive::acquire(FamilySource::Fixture(&b)).is_err());
    current_b.borrowed().unwrap();
    drop(current_b);
    let _exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&b)).unwrap();
}

#[test]
fn root_version_liveness_and_replaced_gate_cannot_inherit_a_borrow() {
    let home = aterm_tempfile::tempdir().unwrap();
    let other = aterm_tempfile::tempdir().unwrap();
    let a = fixture(home.path(), "grant-a", 1);
    let foreign = fixture(other.path(), "grant-a", 1);
    let exclusive = FamilyExclusive::acquire(FamilySource::Fixture(&a)).unwrap();
    let independent = FamilyExclusive::acquire(FamilySource::Fixture(&foreign)).unwrap();
    independent.current().unwrap();
    let borrowed = exclusive.borrow().unwrap();
    assert!(borrowed.matches(FamilySource::Fixture(&foreign)).is_err());
    a.live.store(false, std::sync::atomic::Ordering::Release);
    assert!(borrowed.current().is_err());
    a.live.store(true, std::sync::atomic::Ordering::Release);
    let old = exclusive.held.path.with_extension("old");
    std::fs::rename(&exclusive.held.path, old).unwrap();
    private_file::write_private_new(
        exclusive.held.path.parent().unwrap(),
        "gate",
        b"replacement",
        |_| Ok(()),
    )
    .unwrap();
    assert!(borrowed.current().is_err());
}
