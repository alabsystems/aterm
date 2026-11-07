// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
fn inputs(root: &Path) -> Inputs {
    Inputs {
        prefix: root.join("store"),
        repo: root.join("repo"),
        path: "/usr/bin:/bin".into(),
        cargo_home: root.join("cargo"),
        overrides: vec![],
    }
}
fn lock() -> trust_lock::TrustLock {
    trust_lock::TrustLock {
        build: 7,
        tree_root: "0".repeat(64),
        compiler_commit: "1".repeat(40),
        receipt: "SEED-MINT sha256:".to_owned() + &"2".repeat(64),
        stock_toolchain: "1.95.0".into(),
    }
}
#[test]
fn expired_deadline_never_creates_lease_or_probes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let inputs = inputs(root.path());
    let result = select(
        &inputs,
        Some(&lock()),
        None,
        Options {
            deadline: Some(Instant::now()),
            lease_wait: Duration::ZERO,
        },
        &|_, _| panic!("expired selection must not fetch signed authority"),
        &|_, _| panic!("expired selection must not execute"),
    );
    assert!(matches!(result, Err(Error::Deadline)));
    assert!(!inputs.prefix.exists());
}
#[test]
fn nonblocking_busy_never_fetches_or_executes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let inputs = inputs(root.path());
    let subject = crate::lease::Subject::build("trust", 7).unwrap();
    std::fs::create_dir_all(&inputs.prefix).unwrap();
    let held = crate::lease::reclaim(&inputs.prefix, &subject);
    assert!(matches!(&held, crate::lease::Reclaim::Clear(_)));
    let result = select(
        &inputs,
        Some(&lock()),
        None,
        Options {
            deadline: Some(Instant::now() + Duration::from_secs(1)),
            lease_wait: Duration::ZERO,
        },
        &|_, _| panic!("busy selection must not fetch"),
        &|_, _| panic!("busy selection must not execute"),
    );
    assert!(matches!(result, Err(Error::Busy(_))));
    drop(held);
}
