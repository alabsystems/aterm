// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
#[test]
fn capabilities_accept_valid_legacy_and_cohort_namespaces() {
    use std::os::unix::fs::DirBuilderExt as _;
    let world = aterm_tempfile::tempdir().unwrap();
    let legacy = aterm_capacity::Ledger::at(world.path().join("legacy"));
    assert!(!legacy.cohort_aware().unwrap());
    let answer = super::capabilities_at(&legacy).unwrap();
    assert_eq!(answer["ledger"], aterm_json::json!(legacy.dir()));

    let home = world.path().join("home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let namespace =
        aterm_capacity::cohort_namespace::Namespace::create_private(world.path(), &home).unwrap();
    let cohort = aterm_capacity::Ledger::at(namespace.fence());
    assert!(cohort.cohort_aware().unwrap());
    let answer = super::capabilities_at(&cohort).unwrap();
    assert_eq!(answer["ledger"], aterm_json::json!(cohort.dir()));
}

#[cfg(unix)]
#[test]
fn capabilities_refuse_a_stored_or_later_namespace_failure() {
    use std::os::unix::fs::DirBuilderExt as _;
    let world = aterm_tempfile::tempdir().unwrap();
    let malformed = world
        .path()
        .join(".capacity-cohort-unready.noindex/capacity");
    std::fs::create_dir_all(&malformed).unwrap();
    let refused = aterm_capacity::Ledger::at(malformed);
    assert!(refused.cohort_aware().is_err());
    assert!(super::capabilities_at(&refused).is_err());

    let home = world.path().join("home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let namespace =
        aterm_capacity::cohort_namespace::Namespace::create_private(world.path(), &home).unwrap();
    let cohort = aterm_capacity::Ledger::at(namespace.fence());
    assert!(super::capabilities_at(&cohort).is_ok());
    std::fs::remove_file(namespace.fence()).unwrap();
    assert!(cohort.cohort_aware().is_err());
    assert!(super::capabilities_at(&cohort).is_err());
}
