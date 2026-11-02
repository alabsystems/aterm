// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::release::probe::{
    EffectError, Git, Invocation, Node,
    model::{Artifact, Repository, Subject},
};
use std::sync::Arc;

struct NoGit;
impl Git for NoGit {
    fn node(&self) -> Result<Node> {
        panic!("no external identity in recovery seam")
    }
    fn fetch_head(&self, _: &Invocation<'_>) -> Result<String> {
        panic!("no fetch")
    }
    fn tag(&self, _: &Invocation<'_>, _: &str) -> Result<Option<String>> {
        panic!("no tag")
    }
    fn create_tag(
        &self,
        _: &Invocation<'_>,
        _: &str,
        _: &str,
    ) -> std::result::Result<(), EffectError> {
        panic!("no create")
    }
    fn delete_tag(
        &self,
        _: &Invocation<'_>,
        _: &str,
        _: &str,
    ) -> std::result::Result<(), EffectError> {
        panic!("no delete")
    }
}
fn fixture() -> (Native, Journal) {
    let native = Native::new(
        Scope {
            paths: crate::client::Paths::for_home(Path::new("/unused")),
            git: Arc::new(NoGit),
        },
        PathBuf::from("/unused"),
    );
    let repos = (0..2)
        .map(|i| {
            let mut r = Repository::new(format!("fixture-{i}"));
            r.commit = Some("ab".repeat(20));
            r.principal = Some("writer".into());
            r.tag_owned = true;
            r.release = Some(i + 1);
            r.asset = Some(i + 10);
            r.verified = true;
            r
        })
        .collect();
    let journal = Journal {
        nonce: "12".repeat(16),
        subject: Subject {
            uid: 501,
            node: "23".repeat(16),
            member: "34".repeat(16),
            writer: "45".repeat(32),
            incarnation: "56".repeat(16),
            network: "67".repeat(16),
            authority: "78".repeat(32),
            genesis: "89".repeat(32),
            vault: "9a".repeat(32),
        },
        repositories: repos,
        created_ms: 1,
        updated_ms: 1,
        artifact: Some(Artifact {
            sha256: "ab".repeat(32),
            executable_sha256: "cd".repeat(32),
            bytes: 100,
            signing_identity: "ef".repeat(20),
            team: aterm_update_core::pins::APPLE_TEAM_ID.into(),
            executed: true,
        }),
        submission: Some("01234567-89ab-cdef-0123-456789abcdef".into()),
        apple_accepted: true,
        pending: None,
        cleanup_only: false,
    };
    (native, journal)
}
fn readback() -> Outcome {
    Outcome::Applied(Receipt::Readback {
        sha256: "ab".repeat(32),
        bytes: 100,
    })
}

#[test]
fn the_same_native_reacquires_every_asset_before_the_first_cleanup_effect() {
    let (native, journal) = fixture();
    let events = RefCell::new(Vec::new());
    native
        .cleanup_with(
            &journal,
            |index| {
                events.borrow_mut().push(format!("read-{index}"));
                Ok(Some(readback()))
            },
            || {
                assert_eq!(*events.borrow(), ["read-0", "read-1"]);
                events.borrow_mut().push("delete-0".into());
                Ok(Outcome::Applied(Receipt::AssetAbsent))
            },
        )
        .unwrap();
    native
        .cleanup_with(
            &journal,
            |_| panic!("live witness is retained in this Native"),
            || {
                events.borrow_mut().push("delete-1".into());
                Ok(Outcome::Applied(Receipt::AssetAbsent))
            },
        )
        .unwrap();
    assert_eq!(
        *events.borrow(),
        ["read-0", "read-1", "delete-0", "delete-1"]
    );
}

#[test]
fn a_missing_previous_asset_never_becomes_fresh_and_bad_bytes_stop_deletion() {
    let (native, mut journal) = fixture();
    journal.repositories[0].asset_absent = true;
    let deleted = std::cell::Cell::new(false);
    native
        .cleanup_with(
            &journal,
            |index| Ok((index == 1).then(readback)),
            || {
                deleted.set(true);
                Ok(Outcome::Applied(Receipt::AssetAbsent))
            },
        )
        .unwrap();
    assert!(deleted.get());
    assert!(!native.fresh_uploads.borrow().contains(&0));
    assert!(native.fresh_uploads.borrow().contains(&1));
    let (other, journal) = fixture();
    assert!(
        other
            .cleanup_with(
                &journal,
                |_| Ok(Some(Outcome::Applied(Receipt::Readback {
                    sha256: "ff".repeat(32),
                    bytes: 100
                }))),
                || panic!("unverified data cannot be deleted")
            )
            .is_err()
    );
    assert!(other.fresh_uploads.borrow().is_empty());
}
