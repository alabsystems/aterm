// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_directory_created_during_resolution_is_canonicalized_again() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    for nested in [false, true] {
        let parent = scratch.path().join(if nested { "nested" } else { "leaf" });
        let path = if nested {
            parent.join("ledger")
        } else {
            parent.clone()
        };
        let mut created = false;
        let actual = canonical_candidate_with(&path, |probe| {
            if !created && probe == parent {
                created = true;
                std::fs::create_dir(&parent).unwrap();
                return Err(io::ErrorKind::NotFound.into());
            }
            probe.canonicalize()
        })
        .unwrap();
        assert!(created);
        let expected = if nested {
            parent.canonicalize().unwrap().join("ledger")
        } else {
            parent.canonicalize().unwrap()
        };
        assert_eq!(actual, expected);
    }
}

#[test]
fn an_unresolved_entry_never_becomes_an_absent_ledger() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("ledger");
    let mut created = false;
    assert!(
        canonical_candidate_with(&path, |probe| {
            assert!(!created);
            created = true;
            std::os::unix::fs::symlink(scratch.path().join("absent"), &path).unwrap();
            assert_eq!(probe, path);
            Err(io::ErrorKind::NotFound.into())
        })
        .is_err()
    );
    assert!(created);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let mut reads = 0;
    assert!(
        canonical_candidate_with(&path, |_| {
            reads += 1;
            Err(io::ErrorKind::NotFound.into())
        })
        .is_err()
    );
    assert_eq!(reads, 128, "directory churn must stay bounded");
}

fn prepare() -> (aterm_tempfile::TempDir, Namespace) {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    std::fs::create_dir(&home).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let namespace = Namespace::create_private(scratch.path(), &home).unwrap();
    (scratch, namespace)
}

#[test]
fn old_reader_hits_a_file_and_late_predecessor_writes_block() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    fd::mkdirat(
        directory(&scratch.path().canonicalize().unwrap()).unwrap(),
        "home",
        Mode::RWXU,
    )
    .unwrap();
    let mut held_old = None;
    let namespace = Namespace::prepare_private(scratch.path(), &home, |old| {
        held_old = Some(old.try_clone()?);
        Ok(())
    })
    .unwrap();
    assert!(std::fs::read_dir(namespace.fence()).is_err());
    assert_eq!(
        Namespace::resolve(&namespace.fence()).unwrap(),
        Some(namespace.clone())
    );
    assert_eq!(
        Namespace::resolve(&namespace.backend()).unwrap(),
        Some(namespace.clone())
    );
    let old = held_old.unwrap();
    let held_identity = Inode::read(&old, true).unwrap();
    assert_eq!(held_identity, namespace.identities[3]);
    // This descriptor still reaches the old inode after the directory-name fence.
    let _late = create(&old, "late.claim").unwrap();
    assert_eq!(
        namespace.validate().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    fd::unlinkat(&old, "late.claim", fd::AtFlags::empty()).unwrap();
    namespace.validate().unwrap();
}

#[test]
fn replacement_missing_header_and_incomplete_ready_never_mean_empty() {
    let (_scratch, namespace) = prepare();
    let backend = directory(&namespace.backend()).unwrap();
    fd::renameat_with(&backend, READY, &backend, PENDING, RenameFlags::NOREPLACE).unwrap();
    assert!(Namespace::resolve(&namespace.backend()).is_err());
    fd::renameat_with(&backend, PENDING, &backend, READY, RenameFlags::NOREPLACE).unwrap();
    namespace.validate().unwrap();
    let container = directory(&namespace.root).unwrap();
    fd::renameat_with(
        &container,
        HEADER,
        &container,
        "saved-header",
        RenameFlags::NOREPLACE,
    )
    .unwrap();
    assert!(Namespace::resolve(&namespace.backend()).is_err());
    fd::renameat_with(
        &container,
        "saved-header",
        &container,
        HEADER,
        RenameFlags::NOREPLACE,
    )
    .unwrap();
    namespace.validate().unwrap();
    fd::renameat_with(
        &container,
        BACKEND,
        &container,
        "saved-backend",
        RenameFlags::NOREPLACE,
    )
    .unwrap();
    fd::mkdirat(&container, BACKEND, Mode::RWXU).unwrap();
    assert!(Namespace::resolve(&namespace.backend()).is_err());
}

#[test]
fn headerless_fence_and_pre_header_preparation_cannot_fall_back() {
    let (scratch, namespace) = prepare();
    let container = directory(&namespace.root).unwrap();
    fd::renameat_with(
        &container,
        HEADER,
        &container,
        "saved-header",
        RenameFlags::NOREPLACE,
    )
    .unwrap();
    assert!(
        Namespace::resolve(&namespace.fence()).is_err(),
        "a missing header must not return the ordinary-ledger result for the fence"
    );
    fd::renameat_with(
        &container,
        OLD,
        &container,
        "saved-fence",
        RenameFlags::NOREPLACE,
    )
    .unwrap();
    assert!(
        Namespace::resolve(&namespace.fence()).is_err(),
        "a missing fence cannot authorize an empty replacement beside the old backend"
    );

    let partial = namespace
        .home
        .join(".capacity-cohort-00000000000000000000000000000001.noindex");
    fd::mkdirat(
        directory(&namespace.home).unwrap(),
        partial.file_name().unwrap(),
        Mode::RWXU,
    )
    .unwrap();
    let partial_fd = directory(&partial).unwrap();
    fd::mkdirat(&partial_fd, OLD, Mode::RWXU).unwrap();
    fd::mkdirat(&partial_fd, BACKEND, Mode::RWXU).unwrap();
    assert!(
        Namespace::resolve(&partial.join(OLD)).is_err(),
        "pre-header interruption is protocol evidence, not an empty v1 ledger"
    );
    assert_eq!(
        Namespace::resolve(&scratch.path().join("ordinary")).unwrap(),
        None
    );
}

#[test]
fn nested_and_aliased_v3_paths_cannot_become_ordinary_ledgers() {
    let (scratch, namespace) = prepare();
    let key = "0000000000001-1-1.00000000000000000000000000000001.noindex";
    let claim = namespace.active().join(key);
    std::fs::create_dir(&claim).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&claim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let alias = scratch.path().join("ordinary-name");
    std::os::unix::fs::symlink(&claim, &alias).unwrap();
    assert_eq!(Namespace::resolve(&alias).unwrap(), Some(namespace.clone()));
    assert!(Namespace::resolve(&alias.join("unknown/one/two/three/four/five/six")).is_err());
    assert!(Namespace::resolve(&namespace.root).is_err());
    assert!(Namespace::resolve(&namespace.retired().join(key).join("claim.noindex")).is_err());
    let dangling = scratch.path().join("arbitrary-dangling-name");
    std::os::unix::fs::symlink(claim.join("absent"), &dangling).unwrap();
    assert!(
        Namespace::resolve(&dangling).is_err(),
        "an unresolved alias cannot authorize a later v1 walk"
    );
    let container = directory(&namespace.root).unwrap();
    fd::renameat_with(
        &container,
        HEADER,
        &container,
        "saved-header",
        RenameFlags::NOREPLACE,
    )
    .unwrap();
    assert!(
        Namespace::resolve(&alias).is_err(),
        "an alias must retain the missing-header refusal"
    );
}
