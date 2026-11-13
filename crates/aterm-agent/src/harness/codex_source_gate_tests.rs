// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A child parked between fork and exec holds a copy of every descriptor: a gate claim let go
/// by a close alone would stay held in it. Exclusive and shared claims both let go at once.
#[test]
fn a_dropped_claim_is_free_while_a_forked_child_holds_its_descriptor() {
    let home = aterm_tempfile::tempdir().unwrap();
    let publication = PublicationLock::for_home(home.path()).unwrap();
    let parked = aterm_uds::fork_window::Parked::park();
    drop(publication);
    let work = SourceWorkLease::for_home(home.path())
        .expect("an exclusive claim dropped beside a forked child is let go");
    drop(work);
    assert!(
        PublicationLock::for_home(home.path()).is_ok(),
        "a shared claim dropped beside a forked child is let go"
    );
    assert!(parked.release().success());
}

#[test]
fn captured_gate_probes_never_create_a_missing_inode() {
    let home = aterm_tempfile::tempdir().unwrap();
    let work = SourceWorkLease::for_home(home.path()).unwrap();
    let identity = work.identity().unwrap();
    let namespace = work.namespace().unwrap();
    let gate = identity.path.clone();
    let displaced = gate.with_extension("held-original");
    std::fs::rename(&gate, &displaced).unwrap();

    assert!(!identity.current());
    assert!(!gate.exists(), "identity inspection must not create a gate");
    assert!(!identity.matches_home(home.path()));
    assert!(
        !gate.exists(),
        "namespace comparison must not create a gate"
    );
    assert!(existing_namespace_for_home(home.path()).is_err());
    assert!(!gate.exists(), "namespace lookup must not create a gate");
    assert!(SourceWorkLease::for_existing_home(home.path()).is_err());
    assert!(
        !gate.exists(),
        "maintenance admission must not create a gate"
    );
    assert!(identity.shared().is_err());
    assert!(
        !gate.exists(),
        "captured shared acquisition must not create a gate"
    );
    assert!(work.current().is_err());
    assert!(namespace.directory().is_err());
    assert!(!gate.exists(), "held read probes must not create a gate");

    std::fs::rename(&displaced, &gate).unwrap();
    assert!(identity.current());
    let existing = SourceWorkLease::for_existing_home(home.path()).unwrap();
    drop(work);
    assert!(
        PublicationLock::try_for_home(home.path())
            .unwrap()
            .is_none()
    );
    drop(existing);
    let publication = PublicationLock::for_home(home.path()).unwrap();
    assert!(publication.current().is_ok());
}

#[test]
fn publication_excludes_real_live_source_work_and_replaced_gate_is_refused() {
    let home = aterm_tempfile::tempdir().unwrap();
    let first = SourceWorkLease::for_home(home.path()).unwrap();
    let second = SourceWorkLease::for_home(home.path()).unwrap();
    assert!(PublicationLock::for_home(home.path()).is_err());
    drop(first);
    assert!(PublicationLock::for_home(home.path()).is_err());
    drop(second);
    let publication = PublicationLock::for_home(home.path()).unwrap();
    assert!(SourceWorkLease::for_home(home.path()).is_err());
    publication.current().unwrap();
    let path = publication.0.path.clone();
    std::fs::rename(&path, path.with_extension("held")).unwrap();
    private_file::write_private_new(path.parent().unwrap(), "gate", b"replacement", |_| Ok(()))
        .unwrap();
    assert!(publication.current().is_err());
}

#[cfg(unix)]
#[test]
fn namespace_join_checks_existing_gate_and_accepts_canonical_home_alias() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let other = root.path().join("other");
    let missing = root.path().join("missing-gate");
    for directory in [&home, &other, &missing] {
        private_file::create_private_dir(directory).unwrap();
    }
    let work = SourceWorkLease::for_home(&home).unwrap();
    let identity = work.identity().unwrap();
    assert!(identity.matches_home(&home));
    assert!(identity.current());
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    assert!(identity.matches_home(&alias));
    drop(work);
    let _publication = PublicationLock::for_home(&home).unwrap();
    // Namespace validation performs no hidden shared acquisition, even
    // while this actual gate is exclusively held.
    assert!(identity.matches_home(&home));
    assert!(
        identity.current(),
        "captured metadata remains valid after READY released its lease; no shared reacquisition under exclusive publication"
    );
    // A metadata comparison cannot manufacture a missing namespace.
    assert!(!identity.matches_home(&missing));
    assert!(!missing.join(".aterm").exists());
    let _other = SourceWorkLease::for_home(&other).unwrap();
    assert!(!identity.matches_home(&other));
    // Even the same path is insufficient after kernel identity changes.
    let gate = identity.path.clone();
    std::fs::rename(&gate, gate.with_extension("old")).unwrap();
    private_file::write_private_new(gate.parent().unwrap(), "gate", b"replacement", |_| Ok(()))
        .unwrap();
    assert!(!identity.matches_home(&home));
    assert!(!identity.matches_home(&alias));
    assert!(
        !identity.current(),
        "a READY snapshot cannot survive physical gate replacement"
    );
}
