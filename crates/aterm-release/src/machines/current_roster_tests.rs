// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client::{PublicRosterPair, PublicRosterRead};
use aterm_update_core::roster::{Machine, SUPPORTED_SCHEMA};

fn candidate(seq: u64, revoked: bool, expired: bool) -> (String, Candidate) {
    let seed = atpkg_keys::master::parse_master(&"2".repeat(52))
        .unwrap()
        .seed();
    let roster = Roster {
        schema: SUPPORTED_SCHEMA,
        roster_seq: seq,
        valid_until: if expired {
            "2020-01-01T00:00:00Z"
        } else {
            "9999-12-31T00:00:00Z"
        }
        .into(),
        machines: vec![Machine {
            id: "m3".into(),
            pubkey: aterm_codec::base64::encode(&[0x42; 32]).unwrap(),
            added_at: "2019-01-01T00:00:00Z".into(),
            not_after: None,
        }],
        revoked: if revoked { vec!["m3".into()] } else { vec![] },
    };
    let bytes = roster.to_toml().unwrap().into_bytes();
    let sig = seed.sign(&bytes).unwrap();
    let master = seed.pubkey_b64().unwrap();
    let admitted = admit_candidate(&[&master], bytes, sig).unwrap();
    (master, admitted)
}

fn write_pair(path: &Path, c: &Candidate) {
    std::fs::write(path, &c.bytes).unwrap();
    std::fs::write(machines::RosterDocument::signature_path(path), &c.sig).unwrap();
}

fn public(c: &Candidate) -> PublicRosterRead {
    PublicRosterRead::Pair(PublicRosterPair {
        roster: c.bytes.clone(),
        signature: c.sig.clone(),
    })
}

#[test]
fn newer_public_denial_is_selected_before_signer_authorization() {
    for (revoked, expired) in [(true, false), (false, true)] {
        let home = aterm_tempfile::tempdir().unwrap();
        let path = home.path().join("roster.toml");
        let (master, old) = candidate(3, false, false);
        let (_, new) = candidate(4, revoked, expired);
        write_pair(&path, &old);
        let key = aterm_codec::base64::encode(&[0x42; 32]).unwrap();
        assert!(
            machines::authorize_cut(&[&master], old.bytes.clone(), &old.sig, &key, 1_793_000_000)
                .is_ok()
        );
        let selected = resolve(home.path(), &[&path], &[&master], Ok(None), |_| {
            Ok(public(&new))
        })
        .unwrap();
        assert_eq!(selected.selected.bytes, new.bytes);
        assert!(
            machines::authorize_cut(
                &[&master],
                selected.selected.bytes,
                &selected.selected.sig,
                &key,
                1_793_000_000
            )
            .is_err()
        );
    }
}

#[test]
fn retained_floor_and_unavailable_public_source_never_fall_back_to_local_grant() {
    let home = aterm_tempfile::tempdir().unwrap();
    let path = home.path().join("roster.toml");
    let (master, old) = candidate(3, false, false);
    write_pair(&path, &old);
    assert!(
        resolve(home.path(), &[&path], &[&master], Ok(None), |_| Ok(
            PublicRosterRead::Absent
        ))
        .is_ok()
    );
    assert!(
        resolve(home.path(), &[&path], &[&master], Ok(None), |_| Err(
            aterm_secrets::Error::new("synthetic unavailable source")
        ))
        .is_err()
    );
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    aterm_uds::private_file::ensure_private_dir(&paths.dir).unwrap();
    aterm_uds::private_file::write_private(&paths.dir, "release-ring-roster-floor", b"5\n")
        .unwrap();
    assert!(
        resolve(home.path(), &[&path], &[&master], Ok(None), |_| Ok(
            PublicRosterRead::Absent
        ))
        .is_err()
    );
    let (_, newer) = candidate(5, false, false);
    assert_eq!(
        resolve(home.path(), &[&path], &[&master], Ok(None), |_| Ok(public(
            &newer
        )))
        .unwrap()
        .selected
        .bytes,
        newer.bytes
    );
}

#[test]
fn lower_generation_forks_are_not_hidden_by_newer_candidate_order() {
    let (_, newest) = candidate(5, false, false);
    let (_, a) = candidate(3, false, false);
    let (_, b) = candidate(3, true, false);
    for candidates in [
        vec![newest.clone(), a.clone(), b.clone()],
        vec![b.clone(), a.clone(), newest.clone()],
    ] {
        assert!(select_all(candidates).is_err());
    }
    assert_eq!(
        select_all(vec![newest.clone(), a.clone(), a])
            .unwrap()
            .unwrap()
            .bytes,
        newest.bytes
    );
}

#[test]
fn local_pair_inspection_distinguishes_absence_from_unavailable_or_malformed() {
    let home = aterm_tempfile::tempdir().unwrap();
    let path = home.path().join("roster.toml");
    let (master, good) = candidate(3, false, false);
    assert!(
        read_local_candidate_with(&path, &[&master])
            .unwrap()
            .is_none()
    );
    assert!(
        read_local_candidate_using(&path, &[&master], |_| Err(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )))
        .is_err()
    );
    std::fs::write(&path, &good.bytes).unwrap();
    assert!(read_local_candidate_with(&path, &[&master]).is_err());
    write_pair(&path, &good);
    assert_eq!(
        read_local_candidate_with(&path, &[&master])
            .unwrap()
            .unwrap()
            .bytes,
        good.bytes
    );
    std::fs::write(&path, b"malformed").unwrap();
    assert!(read_local_candidate_with(&path, &[&master]).is_err());
}

#[test]
fn a_new_public_grant_requires_the_actual_canonical_engine_pair_before_positive_use() {
    let home = aterm_tempfile::tempdir().unwrap();
    let canonical = home.path().join(atpkg_keys::provision::RELEASE_ROSTER_REL);
    let (master, old) = candidate(3, false, false);
    crate::provision::publish_proven_pair(&canonical, &old.bytes, &old.sig).unwrap();
    let (_, new) = candidate(5, false, false);
    let selected = resolve(home.path(), &[&canonical], &[&master], Ok(None), |_| {
        Ok(public(&new))
    })
    .unwrap();
    let key = aterm_codec::base64::encode(&[0x42; 32]).unwrap();
    assert!(
        machines::authorize_cut(
            &[&master],
            selected.selected.bytes.clone(),
            &selected.selected.sig,
            &key,
            1_793_000_000
        )
        .is_ok()
    );
    assert!(selected.require_canonical(home.path(), &[&master]).is_err());
    assert_eq!(std::fs::read(&canonical).unwrap(), old.bytes);
    crate::provision::publish_proven_pair(&canonical, &new.bytes, &new.sig).unwrap();
    selected.require_canonical(home.path(), &[&master]).unwrap();
}

/// The kept pair as the vault places it: two plain files, with no roster lock beside them.
fn place_as_the_vault_does(path: &Path, c: &Candidate) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_pair(path, c);
}

fn lock_of(path: &Path) -> std::path::PathBuf {
    atpkg_keys::provision::roster_lock_path(path.to_str().unwrap()).into()
}

#[test]
fn a_pair_no_roster_writer_has_locked_is_read_without_creating_its_lock() {
    let home = aterm_tempfile::tempdir().unwrap();
    let canonical = home.path().join(atpkg_keys::provision::RELEASE_ROSTER_REL);
    let (master, old) = candidate(3, false, false);
    let (_, new) = candidate(5, false, false);
    place_as_the_vault_does(&canonical, &old);
    let selected = resolve(home.path(), &[&canonical], &[&master], Ok(None), |_| {
        Ok(public(&new))
    })
    .unwrap();
    assert!(
        selected.require_canonical(home.path(), &[&master]).is_err(),
        "an unlocked pair older than the selected one is still refused"
    );
    place_as_the_vault_does(&canonical, &new);
    selected.require_canonical(home.path(), &[&master]).unwrap();
    assert!(
        !lock_of(&canonical).exists(),
        "the read created no lock: a check writes nothing"
    );
    // Once a writer has locked the pair, it is read under that lock.
    crate::provision::publish_proven_pair(&canonical, &new.bytes, &new.sig).unwrap();
    assert!(lock_of(&canonical).is_file());
    selected.require_canonical(home.path(), &[&master]).unwrap();
    assert!(
        require_public_pair(&canonical, &old, &[&master]).is_err(),
        "a locked pair other than the selected one is refused"
    );
}

#[test]
fn a_pair_whose_lock_exists_is_read_only_through_that_lock() {
    let home = aterm_tempfile::tempdir().unwrap();
    let canonical = home.path().join(atpkg_keys::provision::RELEASE_ROSTER_REL);
    let (master, pair) = candidate(5, false, false);
    place_as_the_vault_does(&canonical, &pair);
    // A lock that is present but cannot be claimed: a symlink, which the claim never follows.
    let elsewhere = home.path().join("elsewhere");
    std::fs::write(&elsewhere, b"").unwrap();
    std::os::unix::fs::symlink(&elsewhere, lock_of(&canonical)).unwrap();
    let refused = require_public_pair(&canonical, &pair, &[&master]).unwrap_err();
    assert!(
        refused.to_string().contains("without creating it"),
        "the matching pair was not read past its lock: {refused}"
    );
}

#[test]
fn a_pair_a_writer_locks_while_it_is_read_unlocked_is_read_again_under_the_lock() {
    let (master, old) = candidate(3, false, false);
    let (_, new) = candidate(5, false, false);
    let publish = |path: &Path, c: &Candidate| {
        crate::provision::publish_proven_pair(path, &c.bytes, &c.sig).unwrap();
    };
    // The unlocked read saw the old pair; the writer's new one is what stands.
    let home = aterm_tempfile::tempdir().unwrap();
    let canonical = home.path().join(atpkg_keys::provision::RELEASE_ROSTER_REL);
    place_as_the_vault_does(&canonical, &old);
    require_public_pair_around(&canonical, &new, &[&master], || publish(&canonical, &new)).unwrap();
    // The unlocked read saw the selected pair; the writer replaced it, so the read is refused.
    let home = aterm_tempfile::tempdir().unwrap();
    let canonical = home.path().join(atpkg_keys::provision::RELEASE_ROSTER_REL);
    place_as_the_vault_does(&canonical, &new);
    let refused =
        require_public_pair_around(&canonical, &new, &[&master], || publish(&canonical, &old))
            .unwrap_err();
    assert!(
        refused.to_string().contains("changed during provisioning"),
        "{refused}"
    );
}

#[test]
fn a_pair_beside_a_pending_transaction_is_refused_locked_or_not() {
    let home = aterm_tempfile::tempdir().unwrap();
    let canonical = home.path().join(atpkg_keys::provision::RELEASE_ROSTER_REL);
    let (master, pair) = candidate(5, false, false);
    place_as_the_vault_does(&canonical, &pair);
    std::fs::create_dir(atpkg_keys::provision::roster_txn_path(
        canonical.to_str().unwrap(),
    ))
    .unwrap();
    let refused = require_public_pair(&canonical, &pair, &[&master]).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("interrupted roster transaction"),
        "{refused}"
    );
    assert!(!lock_of(&canonical).exists());
    // Locked, the same pending transaction refuses too.
    std::fs::write(lock_of(&canonical), b"").unwrap();
    let refused = require_public_pair(&canonical, &pair, &[&master]).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("interrupted roster transaction"),
        "{refused}"
    );
}
