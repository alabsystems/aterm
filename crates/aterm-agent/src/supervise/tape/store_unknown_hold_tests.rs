// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::ownerlock::Owner;

fn tape(path: &Path, bytes: &[u8]) {
    let mut file = create(path).expect("exclusive owned tape");
    file.write_all(bytes).expect("owned bytes");
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
        .expect("fixed mtime");
}

#[test]
fn an_unknown_tape_hold_does_not_authorize_retention_removal() {
    let guard = aterm_tempfile::tempdir().expect("exclusive fixture");
    let root = guard
        .path()
        .canonicalize()
        .expect("owned canonical fixture")
        .join("tapes");
    custody_tests::private_dir(&root);
    custody_tests::private_dir(&root.join("s-1"));
    let store = custody::Store::open(&root, false).expect("admitted held store");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
    let dead = root.join("s-1/dead.atape");
    tape(&dead, b"free");
    let removed = sweep_held_probing(&store, now, 0, Duration::ZERO, |_| Owner::Dead)
        .expect("ordinary free lock");
    assert_eq!(removed.as_slice(), std::slice::from_ref(&dead));
    assert!(!dead.exists());
    let live = root.join("s-1/live.atape");
    tape(&live, b"held");
    assert!(
        sweep_held_probing(&store, now, 0, Duration::ZERO, |_| Owner::Live)
            .expect("ordinary held lock")
            .is_empty()
    );
    assert_eq!(std::fs::read(&live).expect("live retained"), b"held");
    let unknown = root.join("s-1/unknown.atape");
    tape(&unknown, b"no observation");
    let mut asked = Vec::new();
    let gone = sweep_held_probing(&store, now, 0, Duration::ZERO, |t| {
        asked.push(t.stored.path.clone());
        if t.stored.path == live {
            Owner::Live
        } else {
            Owner::Unknown
        }
    })
    .expect("complete inventory");
    assert!(asked.contains(&live) && asked.contains(&unknown));
    assert!(
        gone.is_empty() && std::fs::read(&unknown).is_ok_and(|b| b == b"no observation"),
        "an unknown tape hold must not authorize retention removal"
    );
}
