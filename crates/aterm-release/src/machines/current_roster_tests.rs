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
