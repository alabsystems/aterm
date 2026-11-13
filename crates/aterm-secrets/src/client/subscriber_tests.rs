// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::{Config, State, Stored, save_config, save_state};
use crate::transport::Source;
use std::os::unix::fs::PermissionsExt;

fn configured(home: &std::path::Path) -> Paths {
    let paths = Paths::for_home(home);
    let signer = crate::b64::decode_padded(crate::ALAB_SIGNER).unwrap();
    save_config(
        &paths,
        &Config {
            source: Source::Url("https://example.invalid/vault".into()),
            mirrors: Vec::new(),
            name: crate::ALAB_VAULT.into(),
            signer: Some(signer.try_into().unwrap()),
            followed: Vec::new(),
            host: "member".into(),
        },
    )
    .unwrap();
    paths
}

#[test]
fn absent_subscriber_admission_reads_no_strict_materialization_lock() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = configured(root.path());
    let lock = paths.lock_file();
    std::fs::write(&lock, b"old lock").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(super::super::materialization_ready(&paths).is_err());
    assert!(configured_for_recovery(&paths).unwrap());
    let repaired = after_held(HeldMend::NotHeld, || {
        assert!(configured_for_recovery(&paths)?);
        Ok(HeldMend::Moved("installed".into(), HeldMoved::Missing))
    })
    .unwrap();
    assert_eq!(
        repaired,
        HeldMend::Moved("installed".into(), HeldMoved::Missing)
    );
    assert_eq!(
        std::fs::metadata(&lock).unwrap().permissions().mode() & 0o7777,
        0o644
    );
    assert_eq!(std::fs::read(&lock).unwrap(), b"old lock");

    save_state(
        &paths,
        &State {
            membership: Stored::Revoked,
            ..State::default()
        },
    )
    .unwrap();
    assert!(!configured_for_recovery(&paths).unwrap());
    save_state(&paths, &State::default()).unwrap();
    crate::fsx::write_owned_in_place(
        root.path().join(".config/aterm/aterm.toml"),
        "[swarm]\nenabled = false\n",
    );
    assert!(!configured_for_recovery(&paths).unwrap());
}

#[test]
fn an_existing_held_decision_never_falls_through_to_absent_recovery() {
    for kept in [
        HeldMend::Kept(HeldKept::Pinned),
        HeldMend::Kept(HeldKept::Reads),
        HeldMend::Unread("busy".into()),
        HeldMend::Moved("already moved".into(), HeldMoved::ManagedRelease),
    ] {
        assert_eq!(
            after_held(kept.clone(), || panic!("must preserve held decision")).unwrap(),
            kept
        );
    }
}
