// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Mac whose acknowledged membership history is established under one vault (A) and that is
//! then configured for another (B), by each path that does that: `aterm secrets join` (`join`),
//! a member's bundle after A let it go (`aterm members join` with a person's phrase: its log,
//! `write_from_log_bundle`, or its `[everyone]` files, `write_from_bundle`), and the log-member
//! seam the labs use (`configure_log_member`). Once B's records are here, every authority read
//! of this Mac reads B's log, as a Mac with no history before reads it. A bundle of the vault this
//! Mac is in or waits for keeps the floor it holds of it, and one that history refuses under the
//! bundle's own configuration writes nothing. A proof with nobody's Allow moves no Mac
//! (`readmit_tests.rs`, `a_proof_never_moves_a_mac_to_another_vault`).

use super::*;
use crate::client::log_entries_tests::{TwoMacs, hand, join_host, newcomer, two_macs};
use crate::commons::bootstrap::{Bootstrap, Device};

fn network_of(store: &crate::membership_log::Store) -> crate::membership_log::RecordId {
    *store.log().expect("a log").network()
}

/// `admin`'s vault lets `mac` in under `host` by its envelope, as `aterm secrets approve` does.
fn approve(admin: &Paths, host: &str, mac: &Paths) {
    let recipient = read_identity(mac)
        .unwrap()
        .expect("an identity")
        .recipient()
        .to_bech32();
    let opened = admin_open(admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.add_machine(host, &recipient).unwrap();
    admin_publish_approved(admin, &opened, &doc, std::slice::from_ref(&recipient)).unwrap();
}

/// Whether `mac`'s two authority reads of its store read `vault`'s log.
fn reads_log_of(mac: &Paths, vault: &TwoMacs) -> Result<(), String> {
    let network = network_of(&load_records(&vault.m22).unwrap());
    let acknowledged =
        load_acknowledged_records(mac).map_err(|e| format!("load_acknowledged_records: {e}"))?;
    if acknowledged.log().map(|log| *log.network()) != Some(network) {
        return Err("load_acknowledged_records reads another log or none".into());
    }
    let read = load_acknowledged_and_ahead(mac)
        .map_err(|e| format!("load_acknowledged_and_ahead: {e}"))?;
    if read.acknowledged.log().map(|log| *log.network()) != Some(network) {
        return Err("load_acknowledged_and_ahead reads another log or none".into());
    }
    Ok(())
}

/// The value of `shared` as `aterm secrets get` reads it on `mac`.
fn shared(mac: &Paths) -> Option<Vec<u8>> {
    let config = configured(mac).unwrap();
    read_verified_entry(mac, &config, "shared")
        .ok()
        .flatten()
        .map(|entry| entry.value().as_slice().to_vec())
}

/// A, with `m7` in it and its history acknowledged; B, another vault of the same name whose
/// admin is `m22`.
fn two_vaults() -> (TwoMacs, TwoMacs) {
    let a = two_macs(&[(Audience::Everyone, "shared", b"a".as_slice())]);
    let b = two_macs(&[(Audience::Everyone, "shared", b"b".as_slice())]);
    assert_eq!(
        reads_log_of(&a.m7, &a),
        Ok(()),
        "PRECONDITION: m7 reads A's log"
    );
    assert!(
        retained_history::files(&a.m7)[0].is_file(),
        "PRECONDITION: m7's acknowledged history is established"
    );
    (a, b)
}

/// `aterm secrets join` of B on a Mac in A: the join starts B afresh, and once B's records are
/// here every authority read reads B's log and `aterm secrets get` reads B's value.
#[test]
fn a_join_elsewhere_reads_the_log_of_the_vault_joined() {
    let _answering = ServersAnswering::set();
    let (a, b) = two_vaults();
    let mac = &a.m7;
    approve(&b.m22, "m30", mac);
    join_host(mac, &format!("{}#{}", b.url, b.signer), "m30");
    sync_records(mac, None).unwrap();
    sync(mac, None).unwrap();
    assert_eq!(reads_log_of(mac, &b), Ok(()), "after the join of B");
    assert_eq!(shared(mac).as_deref(), Some(b"b".as_slice()));
    a.server.stop();
    b.server.stop();
}

/// `aterm members join` on a Mac A let go: a member of B lets it in through B's log
/// (`write_from_log_bundle`), as a member's desk hands it. The write acknowledges B's records as
/// B's first history, under its own lock, so every authority read reads B's log and `aterm secrets
/// get` reads B's value from it at once; the join's pass then applies B's version, which is no
/// rollback of the one this Mac held of A.
///
/// NEGATIVE CONTROL: keep the state of the vault left (skip both `afresh` steps of
/// `write_log_bundle`): fails "B's log bundle, written" with `membership history awaits its first
/// durable acknowledgement`, A's records digest standing for B's. Keep only A's cached envelope
/// (drop its removal in `write_log_bundle`): fails "B's value, read from its log".
#[test]
fn a_mac_a_vault_let_go_reads_the_log_of_the_vault_a_member_lets_it_into() {
    let _answering = ServersAnswering::set();
    let (a, b) = two_vaults();
    let mac = &a.m7;
    let admin = admin_open(&a.m22).unwrap();
    admin_revoke(&a.m22, &admin, "m7", false).unwrap();
    let _ = sync(mac, None);
    let state = load_state(mac).unwrap();
    assert_eq!(
        state.membership,
        Stored::Revoked,
        "PRECONDITION: A let m7 go"
    );
    assert!(
        state.records_digest.is_some(),
        "PRECONDITION: the state holds the digest of the log m7 applied in A"
    );
    let device = Device::open(mac).unwrap().expect("a device");
    let member = Bootstrap::required(&b.m7).unwrap();
    let trust = member.trust();
    let request = device.request(trust.network, trust.authority).unwrap();
    let allowed = member
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request.binding(), "m30", seed))
        .unwrap();
    let (bundle, logged) = hand(&b.m7, &request, &allowed, true);
    assert!(logged, "PRECONDITION: B's member hands its log");
    write_from_log_bundle(mac, &bundle).unwrap();
    assert_eq!(reads_log_of(mac, &b), Ok(()), "B's log bundle, written");
    assert_eq!(
        shared(mac).as_deref(),
        Some(b"b".as_slice()),
        "B's value, read from its log"
    );
    let applied = sync(mac, None).map(|_| ()).map_err(|e| e.to_string());
    assert_eq!(applied, Ok(()), "B's version applies after the bundle");
    assert_eq!(reads_log_of(mac, &b), Ok(()), "after the join's pass");
    assert_eq!(shared(mac).as_deref(), Some(b"b".as_slice()));
    a.server.stop();
    b.server.stop();
}

/// `aterm members join` on a Mac A let go, where B's member hands `[everyone]` files rather than
/// its log (a member whose log path does not apply, or a joiner that reads no log:
/// `write_from_bundle`): the write starts B afresh, so once B's admin lets this Mac in, B's
/// version applies, which is no rollback of the one this Mac held of A, and every authority read
/// reads B's log once its records are here.
///
/// NEGATIVE CONTROL: keep the state of the vault left in `write_from_bundle` (skip its `afresh`
/// step): fails "B's version applies once B lets this Mac in" with the refusal of B's version at
/// the sequence this Mac held of A: here a fork (`a different envelope with seq 6, which this
/// machine already holds`), as B's version and the one held of A are both seq 6; where A's is
/// higher, a rollback.
#[test]
fn a_mac_a_vault_let_go_reads_the_vault_a_member_hands_its_files_of() {
    let _answering = ServersAnswering::set();
    let (a, b) = two_vaults();
    let mac = &a.m7;
    let admin = admin_open(&a.m22).unwrap();
    admin_revoke(&a.m22, &admin, "m7", false).unwrap();
    let _ = sync(mac, None);
    assert_eq!(
        load_state(mac).unwrap().membership,
        Stored::Revoked,
        "PRECONDITION: A let m7 go"
    );
    let device = Device::open(mac).unwrap().expect("a device");
    let member = Bootstrap::required(&b.m7).unwrap();
    let trust = member.trust();
    let request = device.request(trust.network, trust.authority).unwrap();
    let allowed = member
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request.binding(), "m30", seed))
        .unwrap();
    let (bundle, logged) = hand(&b.m7, &request, &allowed, false);
    assert!(!logged, "PRECONDITION: B's member hands its files");
    write_from_bundle(mac, &bundle).unwrap();
    assert_eq!(
        load_state(mac).unwrap().membership,
        Stored::Pending,
        "PRECONDITION: the bundle leaves this Mac waiting to be let in"
    );
    approve(&b.m22, "m30", mac);
    let applied = sync(mac, None).map(|_| ()).map_err(|e| e.to_string());
    assert_eq!(
        applied,
        Ok(()),
        "B's version applies once B lets this Mac in"
    );
    sync_records(mac, None).unwrap();
    assert_eq!(reads_log_of(mac, &b), Ok(()), "after B's records");
    assert_eq!(shared(mac).as_deref(), Some(b"b".as_slice()));
    a.server.stop();
    b.server.stop();
}

/// The labs' log-member seam on a Mac in A, pointed at B: once B's records are here, both
/// authority reads read B's log.
///
/// NEGATIVE CONTROL: keep the state of the vault left in `configure_log_member` (skip its
/// `moves_vault` step): fails "after B's records" with `membership history awaits its first
/// durable acknowledgement`, as nothing here applies a records digest of B's.
#[test]
fn a_log_member_configured_elsewhere_reads_the_log_of_the_vault_configured() {
    let _answering = ServersAnswering::set();
    let (a, b) = two_vaults();
    let mac = &a.m7;
    configure_log_member(mac, &format!("{}#{}", b.url, b.signer), "m30").unwrap();
    assert!(
        sync_records(mac, None).unwrap() > 0,
        "PRECONDITION: B's records"
    );
    assert_eq!(reads_log_of(mac, &b), Ok(()), "after B's records");
    a.server.stop();
    b.server.stop();
}

/// The vault this Mac is in, configured again (the seam, as a rejoin of the same vault does): the
/// state keeps that vault's floor, its version, envelope digest and records digest, beside its
/// cached envelope, and the history this Mac acknowledged still refuses an older snapshot of that
/// vault's records put back.
///
/// NEGATIVE CONTROL: take every configuration for another vault (`same_vault` returns `false`):
/// fails "the same vault keeps its floor".
#[test]
fn the_vault_this_mac_is_in_configured_again_keeps_its_floor_and_refuses_a_rollback() {
    let _answering = ServersAnswering::set();
    let a = two_macs(&[(Audience::Everyone, "shared", b"a".as_slice())]);
    let mac = &a.m7;
    let old = load_records(mac).unwrap();
    let admin = admin_open(&a.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"a2").unwrap();
    admin_publish(&a.m22, &admin, &doc).unwrap();
    sync(mac, None).unwrap();
    let heads = |store: &crate::membership_log::Store| store.log().expect("a log").heads();
    assert_ne!(
        heads(&load_acknowledged_records(mac).unwrap()),
        heads(&old),
        "PRECONDITION: m7 acknowledged a newer log of A"
    );
    let before = load_state(mac).unwrap();
    assert!(
        before.seq > 0 && before.digest.is_some() && before.records_digest.is_some(),
        "PRECONDITION: m7 applied A"
    );
    configure_log_member(mac, &format!("{}#{}", a.url, a.signer), "m7").unwrap();
    let after = load_state(mac).unwrap();
    assert_eq!(
        (after.seq, after.digest, after.records_digest),
        (before.seq, before.digest, before.records_digest),
        "the same vault keeps its floor"
    );
    assert!(mac.cache_file().is_file(), "and its cached envelope");
    fsx::write_atomic(&mac.records_data_file().unwrap(), &old.encode(), 0o600).unwrap();
    assert_eq!(
        load_acknowledged_records(mac).err().map(|e| e.kind()),
        Some(crate::ErrorKind::MembershipHistoryUnavailable),
        "an older snapshot of the same vault put back is refused"
    );
    a.server.stop();
}

/// A Mac that waits to be let into A, its acknowledged history of A established, handed a bundle
/// of A under another vault name (a member's bundle names the vault, and A's genesis binds no
/// name), its log (`write_from_log_bundle`) or its `[everyone]` files (`write_from_bundle`): the
/// history this Mac acknowledged holds A's log as A's, so it refuses that log under the other
/// name, and every check the write runs refuses the bundle before anything is written. The
/// joiner's own check (`accepts_bundle`) refuses it as the write does; the configuration, the
/// version, both digests and the cached envelope stay; and the same bundle under A's own name is
/// then taken, so nothing was left refusing this Mac's later reads.
///
/// NEGATIVE CONTROL: drop the history's check of the records under the bundle's configuration
/// (`validate_configured`) from `checked_log_bundle`: fails "m30: the joiner refuses it as the
/// write does"; from `checked_files_bundle`: fails "m31: the joiner refuses it as the write does".
#[test]
fn a_bundle_of_the_vault_this_mac_waits_for_under_another_name_writes_nothing() {
    let _answering = ServersAnswering::set();
    let a = two_macs(&[(Audience::Everyone, "shared", b"a".as_slice())]);
    for (log, name) in [(true, "m30"), (false, "m31")] {
        let (mac, request, allowed) = newcomer(a.root.path(), &a.m7, name);
        join_host(&mac, &format!("{}#{}", a.url, a.signer), name);
        sync_records(&mac, None).unwrap();
        assert!(
            retained_history::files(&mac)[0].is_file(),
            "PRECONDITION: {name}'s acknowledged history is established"
        );
        let state = load_state(&mac).unwrap();
        assert_eq!(
            state.membership,
            Stored::Pending,
            "PRECONDITION: {name} waits to be let into A"
        );
        assert!(
            state.seq > 0 && state.digest.is_some(),
            "PRECONDITION: {name} applied A's version"
        );
        let floor = (state.seq, state.digest, state.records_digest);
        let cache = std::fs::read(mac.cache_file()).unwrap();
        let config_before = config(&mac).unwrap();
        let (mut bundle, logged) = hand(&a.m7, &request, &allowed, log);
        assert_eq!(logged, log, "PRECONDITION: {name}'s bundle");
        let vault = std::mem::replace(&mut bundle.vault, "other".to_owned());
        let write = |bundle: &crate::members::Bundle| {
            if log {
                write_from_log_bundle(&mac, bundle).map(drop)
            } else {
                write_from_bundle(&mac, bundle).map(drop)
            }
        };
        assert!(
            accepts_bundle(&mac, &bundle).is_err(),
            "{name}: the joiner refuses it as the write does"
        );
        assert!(write(&bundle).is_err(), "{name}: the write refuses it");
        let after = load_state(&mac).unwrap();
        assert_eq!(
            (after.seq, after.digest, after.records_digest),
            floor,
            "{name}: the refused bundle leaves the floor"
        );
        assert_eq!(
            std::fs::read(mac.cache_file()).ok(),
            Some(cache),
            "{name}: and the cached envelope"
        );
        assert!(
            config(&mac).unwrap() == config_before,
            "{name}: and the configuration"
        );
        bundle.vault = vault;
        assert_eq!(
            write(&bundle).map_err(|e| e.to_string()),
            Ok(()),
            "{name}: the bundle under A's own name"
        );
        assert_eq!(
            reads_log_of(&mac, &a),
            Ok(()),
            "{name}: reads A's log after it"
        );
    }
    a.server.stop();
}

/// A bundle that moves this Mac to a vault its acknowledged history has no room for is refused as
/// the write will refuse it, before the write has saved the new configuration and forgotten what
/// this Mac held. The history judges such a move as the replacement it will be (`afresh`), which
/// asks for room where the check of an ordinary write stops at no applied frontier.
///
/// NEGATIVE CONTROL (2026-11-05): with `validate_configured` passing `false` for `afresh`, the
/// joiner takes a bundle the write then refuses, the write having gone part way: `accepted Ok(()),
/// written Err("retained membership context capacity exhausted"), same configuration false, same
/// floor and envelope false`.
#[test]
fn a_move_the_history_has_no_room_for_is_refused_before_anything_is_written() {
    let _answering = ServersAnswering::set();
    let (a, b) = two_vaults();
    let mac = &a.m7;
    retained_history::fill_contexts(mac);
    let admin = admin_open(&a.m22).unwrap();
    admin_revoke(&a.m22, &admin, "m7", false).unwrap();
    let _ = sync(mac, None);
    assert_eq!(
        load_state(mac).unwrap().membership,
        Stored::Revoked,
        "PRECONDITION: A let m7 go"
    );
    let device = Device::open(mac).unwrap().expect("a device");
    let member = Bootstrap::required(&b.m7).unwrap();
    let trust = member.trust();
    let request = device.request(trust.network, trust.authority).unwrap();
    let allowed = member
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request.binding(), "m30", seed))
        .unwrap();
    let (bundle, logged) = hand(&b.m7, &request, &allowed, true);
    assert!(logged, "PRECONDITION: B's member hands its log");

    let held = |mac: &Paths| {
        let state = load_state(mac).unwrap();
        (
            state.seq,
            state.digest,
            state.records_digest,
            std::fs::read(mac.cache_file()).unwrap(),
        )
    };
    let config_before = configured(mac).unwrap();
    let before = held(mac);
    let accepted = accepts_bundle(mac, &bundle).map_err(|e| e.to_string());
    let written = write_from_log_bundle(mac, &bundle)
        .map(drop)
        .map_err(|e| e.to_string());
    let same_config = configured(mac).unwrap() == config_before;
    let same_held = held(mac) == before;
    let no_room = |result: &Result<(), String>| {
        result
            .as_ref()
            .is_err_and(|error| error.contains("context capacity exhausted"))
    };
    assert!(
        no_room(&accepted) && no_room(&written) && same_config && same_held,
        "accepted {accepted:?}, written {written:?}, same configuration {same_config}, same floor \
         and envelope {same_held}"
    );
    a.server.stop();
    b.server.stop();
}

/// A member's bundle of the vault this Mac waits to be let into, by its log
/// (`write_from_log_bundle`) and by its `[everyone]` files (`write_from_bundle`): the same vault
/// keeps the version and the envelope digest this Mac applied of it, and its cached envelope, so
/// an older version of that vault does not apply.
///
/// NEGATIVE CONTROL: take every bundle for another vault (`afresh` always true in
/// `checked_log_bundle` and `checked_files_bundle`): fails "m30: the same vault keeps its floor".
#[test]
fn a_bundle_of_the_vault_this_mac_waits_for_keeps_its_floor() {
    let _answering = ServersAnswering::set();
    let a = two_macs(&[(Audience::Everyone, "shared", b"a".as_slice())]);
    let older = std::fs::read(a.m22.cache_file()).unwrap();
    let admin = admin_open(&a.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"a2").unwrap();
    admin_publish(&a.m22, &admin, &doc).unwrap();
    for (log, name) in [(true, "m30"), (false, "m31")] {
        let (mac, request, allowed) = newcomer(a.root.path(), &a.m7, name);
        join_host(&mac, &format!("{}#{}", a.url, a.signer), name);
        let state = load_state(&mac).unwrap();
        assert_eq!(
            state.membership,
            Stored::Pending,
            "PRECONDITION: {name} waits to be let into A"
        );
        assert!(
            state.seq > 0 && state.digest.is_some(),
            "PRECONDITION: {name} applied A's newer version"
        );
        let floor = (state.seq, state.digest);
        let cache = std::fs::read(mac.cache_file()).unwrap();
        let (bundle, logged) = hand(&a.m7, &request, &allowed, log);
        assert_eq!(logged, log, "PRECONDITION: {name}'s bundle");
        if log {
            write_from_log_bundle(&mac, &bundle).unwrap();
        } else {
            write_from_bundle(&mac, &bundle).unwrap();
        }
        let after = load_state(&mac).unwrap();
        assert_eq!(
            (after.seq, after.digest),
            floor,
            "{name}: the same vault keeps its floor"
        );
        assert_eq!(
            std::fs::read(mac.cache_file()).ok(),
            Some(cache),
            "{name}: and its cached envelope"
        );
        let outcome = sync_with(&mac, older.clone())
            .map(|outcome| format!("{outcome:?}"))
            .map_err(|e| e.to_string());
        let kept = load_state(&mac).unwrap();
        assert_eq!(
            (kept.seq, kept.digest),
            floor,
            "{name}: an older version of the vault does not apply: {outcome:?}"
        );
    }
    a.server.stop();
}

/// A member that has not followed its vault's signer rotation yet hands its `[everyone]` files to
/// a Mac that joined at the rotated key and waits to be let in: the bundle pins the key the vault
/// rotated away from, which the Mac's line followed, so it is the same vault's. The version, the
/// envelope digest and the cached envelope stay, and the vault's version from before the rotation
/// does not apply.
///
/// NEGATIVE CONTROL: take every configuration for another vault (`same_vault` returns `false`):
/// fails "the same vault keeps its floor".
#[test]
fn a_lagging_members_files_of_the_vault_this_mac_waits_for_keep_its_floor() {
    let _answering = ServersAnswering::set();
    let a = two_macs(&[(Audience::Everyone, "shared", b"a".as_slice())]);
    let older = std::fs::read(a.m22.cache_file()).unwrap();
    let admin = admin_open(&a.m22).unwrap();
    admin_rotate(&a.m22, &admin, &admin.doc.clone()).unwrap();
    let rotated = config(&a.m22).unwrap().unwrap().signer.unwrap();
    let (mac, request, allowed) = newcomer(a.root.path(), &a.m7, "m30");
    join_host(
        &mac,
        &format!("{}#{}", a.url, crate::b64::encode_padded(&rotated)),
        "m30",
    );
    let state = load_state(&mac).unwrap();
    assert_eq!(
        state.membership,
        Stored::Pending,
        "PRECONDITION: m30 waits to be let into A"
    );
    let (bundle, logged) = hand(&a.m7, &request, &allowed, false);
    assert!(!logged, "PRECONDITION: m7 hands its files");
    assert!(
        bundle.signer != rotated && !bundle.followed.contains(&rotated),
        "PRECONDITION: m7's bundle pins the key the vault rotated away from"
    );
    let floor = (state.seq, state.digest);
    let cache = std::fs::read(mac.cache_file()).unwrap();
    write_from_bundle(&mac, &bundle).unwrap();
    let after = load_state(&mac).unwrap();
    assert_eq!(
        (after.seq, after.digest),
        floor,
        "the same vault keeps its floor"
    );
    assert_eq!(
        std::fs::read(mac.cache_file()).ok(),
        Some(cache),
        "and its cached envelope"
    );
    let outcome = sync_with(&mac, older)
        .map(|outcome| format!("{outcome:?}"))
        .map_err(|e| e.to_string());
    let kept = load_state(&mac).unwrap();
    assert_eq!(
        (kept.seq, kept.digest),
        floor,
        "the version from before the rotation does not apply: {outcome:?}"
    );
    a.server.stop();
}
