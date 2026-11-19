// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{
    Signing, WITNESS_AT, Witness, cold, floor, group, journal_replays, with_open_hook,
};
use super::with_before_custody;
use super::*;
use crate::{
    catalog::{Descriptor, Directory, Sighted},
    checkpoint::{self, History, Tip},
    crypto::Causal,
    documents::log::Log,
    table::{
        KeyVerdict, Verdict,
        conflict::Conflict,
        signatures::test_verified,
        tests::{Lab, seal},
    },
};
use astream_wire::sync::Actor;
use aterm_secrets::{
    client,
    commons::{
        bootstrap::Bootstrap,
        journal::{Journal, State},
        node::{Edit, Node},
        roster::{Grant, Role},
    },
};
use aterm_uds::private_file;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    os::unix::fs::FileExt as _,
    path::PathBuf,
    rc::Rc,
};

/// Mac 1's table `dir` of three records: Mac 0's `historical`, written before Mac 1 was admitted,
/// which Mac 1 reads only through a real history handoff from Mac 0; Mac 1's own `current`; and
/// the checkpoint Mac 1 signed at `current`, which its floor stands on, `current`'s payload kept
/// in its cold file. Answers the lab, the table's folder and scope.
fn table() -> (Lab, PathBuf, Scope) {
    table_holding(|store, session| store.commit_catalog(session, b"historical").unwrap().id)
}

/// [`table`], whose `historical` record is the one `write` has Mac 0's store hold and names.
fn table_holding(write: impl FnOnce(&mut Store, &Session<'_>) -> Digest) -> (Lab, PathBuf, Scope) {
    let lab = Lab::with_enrolled(&[Role::Writer, Role::Writer], 1);
    let (old, here) = (
        lab._home.path().join("table-a"),
        lab._home.path().join("table-b"),
    );
    let (scope, historical) = {
        let a = lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let mut store = Store::create(&session, &old).unwrap();
        let id = write(&mut store, &session);
        (
            store.scope(),
            store.wire(&session, &id).unwrap().unwrap().to_vec(),
        )
    };
    lab.admit(&[Role::Writer, Role::Writer]);
    let (a, b) = (lab.bootstrap(0), lab.bootstrap(1));
    hand_history(&a, &b, a.trust().genesis);
    let session = Session::open(&b).unwrap();
    let mut store = Store::join(&session, &here, scope).unwrap();
    let current = store.commit_catalog(&session, b"current").unwrap().id;
    store.ingest(&session, &historical).unwrap();
    // The rows are no typed table's, whose state a checkpoint digests: the store judges no
    // state, so a constant one stands.
    let body = {
        let log = Log::read_with(&session, &store, scope, |_| {
            Err(Error::new("a row is no document command"))
        })
        .unwrap();
        let authored = |id: &Digest| log.authored_ms(id);
        let state = |_: &BTreeSet<Digest>| Ok([0; 32]);
        let history = History {
            scope: scope.digest(),
            generation: scope.generation(),
            index: log.causal_index(),
            projection: log.projection(),
            authored: &authored,
            state: &state,
        };
        let frontier = BTreeMap::from([(
            session.actor(),
            Tip {
                sequence: 1,
                digest: current,
            },
        )]);
        let body = checkpoint::cut_at(&history, &frontier).unwrap();
        let computed = checkpoint::second(&history, &body).unwrap();
        store.sign_checkpoint(&session, &computed, &[]).unwrap();
        body
    };
    store.set_floor(&session, &body).unwrap();
    assert!(store.floor().is_some());
    drop((store, session));
    (lab, here, scope)
}

/// Mac `a`'s history handoff of the roster `source` made out to Mac `b`, and taken by it: `b`
/// then opens the records sealed under that roster, which it was no member of.
fn hand_history(a: &Bootstrap, b: &Bootstrap, source: Digest) {
    let handoff = {
        let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
        let access = a.access_in(&journal).unwrap().unwrap();
        a.history_handoff(&journal, &access, source, b.request().unwrap().binding())
            .unwrap()
    };
    let journal = Journal::open_existing(b.journal_dir(), b.trust()).unwrap();
    let access = b.access_in(&journal).unwrap().unwrap();
    b.import_history(&journal, &access, handoff.bytes())
        .unwrap();
}

/// What an open of the table now reads, as `read` asks it: the revision, or the refusal's text.
type Read = std::result::Result<Digest, String>;

/// The revision a full observing open of `dir` reads now in a fresh session of Mac 1.
fn opened(lab: &Lab, dir: &Path, scope: Scope) -> Read {
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).map_err(|error| error.to_string())?;
    Store::observe(&session, dir, scope)
        .and_then(|store| store.revision(&session))
        .map_err(|error| error.to_string())
}

/// What a read of the table `dir` takes now in a fresh session of Mac 1: each record's identifier
/// and the payload the read opened or took from the cold file, or the refusal's text.
fn payloads(
    lab: &Lab,
    dir: &Path,
    scope: Scope,
) -> std::result::Result<Vec<(Digest, Option<Vec<u8>>)>, String> {
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).map_err(|error| error.to_string())?;
    Store::observe(&session, dir, scope)
        .and_then(|store| store.metadata_evidence(&session))
        .map(|evidence| {
            evidence
                .into_iter()
                .map(|evidence| {
                    (
                        evidence.header.id,
                        evidence.record.map(|record| record.payload().to_vec()),
                    )
                })
                .collect()
        })
        .map_err(|error| error.to_string())
}

/// A sighting of `dir` from a full observing open in a fresh session of Mac 1.
fn sighted(lab: &Lab, dir: &Path, scope: Scope) -> Sighting {
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let store = Store::observe_if_exists_sighted(&session, dir, scope)
        .unwrap()
        .unwrap();
    let (revision, sighting) = store.sighted_revision(&session).unwrap();
    let sighting = sighting.expect("an observed primary store holding no conflict is sighted");
    assert_eq!(sighting.revision(), revision);
    sighting
}

/// Whether `sighting` stands for `dir` in a fresh session of Mac 1, and what that answer cost: the
/// opens that went past the claim, the whole-journal replays and the signatures checked.
fn stands(lab: &Lab, dir: &Path, scope: Scope, sighting: &Sighting) -> (bool, [usize; 3]) {
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let opens = Rc::new(Cell::new(0));
    let counted = Rc::clone(&opens);
    let (replays, verified) = (journal_replays(), test_verified());
    let stood = with_open_hook(
        dir,
        move || {
            counted.set(counted.get() + 1);
            Ok(())
        },
        || Store::sighting_stands(&session, dir, scope, sighting),
    );
    (
        stood,
        [
            opens.get(),
            journal_replays() - replays,
            test_verified() - verified,
        ],
    )
}

/// One input of a revision changed: the sighting taken before it does not stand, and an open
/// now reads another revision or refuses, never the sighting's.
#[track_caller]
fn moved(lab: &Lab, dir: &Path, scope: Scope, sighting: &Sighting, input: &str) -> Read {
    let (stood, _) = stands(lab, dir, scope, sighting);
    assert!(!stood, "{input}: the sighting stood for a changed store");
    let read = opened(lab, dir, scope);
    assert_ne!(
        read.as_ref().ok(),
        Some(&sighting.revision()),
        "{input}: an open read the sighting's revision"
    );
    read
}

/// Restored as it was: the sighting stands again, and an open reads its revision.
#[track_caller]
fn restored(lab: &Lab, dir: &Path, scope: Scope, sighting: &Sighting, input: &str) {
    let (stood, _) = stands(lab, dir, scope, sighting);
    assert!(
        stood,
        "{input}: the sighting does not stand for the store restored"
    );
    assert_eq!(opened(lab, dir, scope), Ok(sighting.revision()), "{input}");
}

/// Flip one bit of the acknowledged journal byte at `at` in place, its modification time kept:
/// the same file, length and checkpoint.
fn flip(dir: &Path, at: u64) {
    let path = dir.join("table.astream");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let mut byte = [0];
    file.read_exact_at(&mut byte, at).unwrap();
    file.write_all_at(&[byte[0] ^ 1], at).unwrap();
    file.set_modified(modified).unwrap();
    drop(file);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
}

/// The claim folder, the claim's name and its bytes as Mac 1 holds them for the table `scope`.
fn claim_of(lab: &Lab, scope: Scope) -> (PathBuf, String, Vec<u8>) {
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let dir = super::super::claim_dir(&session).unwrap();
    let name = super::super::claim_name(scope);
    let claim = private_file::read_private(&dir, &name, super::super::CLAIM_LEN).unwrap();
    (dir, name, claim)
}

/// Mac 1's table ([`table`]) and a sighting of it, whose revision a full open reads.
fn held() -> (Lab, PathBuf, Scope, Sighting) {
    let (lab, dir, scope) = table();
    let sighting = sighted(&lab, &dir, scope);
    assert_eq!(opened(&lab, &dir, scope), Ok(sighting.revision()));
    (lab, dir, scope, sighting)
}

/// A table's revision is read without opening every record (tracker LT-25C): a sighting of Mac
/// 1's table, taken by a full observing open, stands while the store does, and standing opens
/// no store, replays no journal and checks no signature; the revision it holds is the one a full
/// open reads, and a full open replays the journal. One acknowledged journal byte flipped, the
/// file's modification time kept, the sighting fails and an open refuses; flipped back, the
/// sighting stands again and an open reads its revision: it compares bytes, not times. Each
/// other input a revision names is changed alone by a test of its own below.
/// NEGATIVE CONTROL: with the journal read handed the kept prefix left out of
/// `Store::sighted_now` (its answer taken as `Committed::Unchanged`): `a journal byte: the
/// sighting stood for a changed store`.
#[test]
fn a_held_revision_is_read_without_opening_every_record() {
    let (lab, dir, scope, sighting) = held();
    for _ in 0..3 {
        assert_eq!(
            stands(&lab, &dir, scope, &sighting),
            (true, [0, 0, 0]),
            "a held revision opens no store, replays no journal and checks no signature"
        );
    }
    // A full open does all three where nothing is kept of the journal.
    let replays = journal_replays();
    assert!(opened(&lab, &dir, scope).is_ok());
    assert_eq!(journal_replays() - replays, 1);

    let length = std::fs::metadata(dir.join("table.astream")).unwrap().len();
    flip(&dir, length / 2);
    let refused = moved(&lab, &dir, scope, &sighting, "a journal byte");
    assert!(refused.is_err(), "{refused:?}");
    flip(&dir, length / 2);
    restored(&lab, &dir, scope, &sighting, "a journal byte");
}

/// The cold payloads sealed again, the same payloads under a new salt, move the revision: the
/// sighting fails, and an open reads another revision, of which a new sighting is taken.
/// NEGATIVE CONTROLS: with the cold file's digest left out of `Store::sighted_now`: `the cold
/// file: the sighting stood for a changed store`. With `Store::revision` naming none of the
/// store's files: `the cold file: an open read the sighting's revision`.
#[test]
fn a_sighting_fails_once_the_cold_file_changes() {
    let (lab, dir, scope, sighting) = held();
    {
        let bootstrap = lab.bootstrap(1);
        let session = Session::observe(&bootstrap).unwrap();
        let key = session.cold_key(&scope).unwrap();
        let (_, _, claim) = claim_of(&lab, scope);
        let bound = Capacity::default().journal().bytes;
        let payloads = cold::read(&dir, &claim, &key, bound).unwrap();
        assert_eq!(
            payloads.len(),
            1,
            "the floor's one ready record is kept cold"
        );
        cold::write(&dir, &claim, &key, &payloads).unwrap();
    }
    let resealed = moved(&lab, &dir, scope, &sighting, "the cold file").unwrap();
    let sighting = sighted(&lab, &dir, scope);
    assert_eq!(sighting.revision(), resealed);
}

/// Under a floor a read takes payloads from the cold file, so a revision names it and refuses
/// where a read of it refuses: the file made writable by other users, a read refuses, the
/// sighting fails and an open's revision refuses; its mode restored, the sighting stands again.
/// NEGATIVE CONTROL: with the cold file named by no revision and hashed by no sighting
/// (`Store::revision_read` handing `Store::files_into` `false`, `Store::sighted_now` skipping
/// it): `an other-writable cold file: the sighting stood for a changed store`.
#[test]
fn a_cold_file_a_read_cannot_take_refuses_a_revision_under_a_floor() {
    use std::os::unix::fs::PermissionsExt as _;
    let (lab, dir, scope, sighting) = held();
    let cold = dir.join("cold");
    let mode = |mode| std::fs::set_permissions(&cold, std::fs::Permissions::from_mode(mode));
    assert!(payloads(&lab, &dir, scope).is_ok());
    mode(0o666).unwrap();
    let read = payloads(&lab, &dir, scope);
    assert!(read.is_err(), "{read:?}");
    let refused = moved(&lab, &dir, scope, &sighting, "an other-writable cold file");
    assert!(refused.is_err(), "{refused:?}");
    mode(0o600).unwrap();
    restored(&lab, &dir, scope, &sighting, "an other-writable cold file");
}

/// A cold file no read takes is named by no revision and refuses none: a table that stands on no
/// floor takes no payload from the file beside it (`Store::cold_payloads`; the payloads a floor
/// write kept before a crash cut it short, which no floor covers), so whatever stands there, a
/// file other users can write, other bytes, a folder, a symlink or nothing, a read takes the same
/// payloads, an open reads the same revision, the sighting stands and a new one is taken.
/// NEGATIVE CONTROL: with `Store::revision` and `Store::sighted_now` hashing the cold file
/// whether or not a read takes it, as they did: `writable by other users: an open's revision`,
/// `left: Err("cold: mode 666 lets other users write it")`.
#[test]
fn a_cold_file_no_read_takes_moves_no_revision_and_refuses_none() {
    use std::os::unix::fs::PermissionsExt as _;
    let (lab, dir, scope) = table();
    let (claims, name, _) = claim_of(&lab, scope);
    private_file::remove_private(&claims, &format!("{name}.floor")).unwrap();
    let cold = dir.join("cold");
    assert!(cold.is_file(), "the payloads the floor kept are left");
    let sighting = sighted(&lab, &dir, scope);
    let read = payloads(&lab, &dir, scope).unwrap();
    assert_eq!(read.len(), 3);
    let unread = |what: &str| {
        assert_eq!(
            payloads(&lab, &dir, scope).as_ref(),
            Ok(&read),
            "{what}: a read"
        );
        assert_eq!(
            opened(&lab, &dir, scope),
            Ok(sighting.revision()),
            "{what}: an open's revision"
        );
        assert!(
            stands(&lab, &dir, scope, &sighting).0,
            "{what}: the sighting does not stand"
        );
        assert_eq!(
            sighted(&lab, &dir, scope).revision(),
            sighting.revision(),
            "{what}: a new sighting"
        );
    };
    unread("as the floor left it");
    std::fs::set_permissions(&cold, std::fs::Permissions::from_mode(0o666)).unwrap();
    unread("writable by other users");
    std::fs::set_permissions(&cold, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&cold, b"not this table's cold payloads").unwrap();
    unread("other bytes");
    std::fs::remove_file(&cold).unwrap();
    std::fs::create_dir(&cold).unwrap();
    unread("a folder");
    std::fs::remove_dir(&cold).unwrap();
    std::os::unix::fs::symlink(dir.join("table.astream"), &cold).unwrap();
    unread("a symlink");
    std::fs::remove_file(&cold).unwrap();
    unread("nothing");
}

/// A sighted open verifies what an observing open verifies, no check more and none fewer: its
/// replay's read batch checks again, at its closing, exactly the keys an observing open's does
/// (`crate::table::KEYS_REVALIDATED`), and both read one revision. The table holds one record
/// under the roster Mac 1 joined after and two under its own: an observing open's batch reuses,
/// and checks again, the second roster's key alone.
/// NEGATIVE CONTROL: with the sighted open recording each class's verdict by
/// `ReadBatch::class_verdict`, which counts as a second use of the first roster's key: `a sighted
/// open checked other keys again than an observing open`, `left: 2`, `right: 1`.
#[test]
fn a_sighted_open_checks_again_the_keys_an_observing_open_does() {
    let (lab, dir, scope) = table();
    let open = |sighted: bool| {
        let bootstrap = lab.bootstrap(1);
        let session = Session::observe(&bootstrap).unwrap();
        let (keys, replays) = (crate::table::KEYS_REVALIDATED.get(), journal_replays());
        let store = if sighted {
            Store::observe_if_exists_sighted(&session, &dir, scope)
                .unwrap()
                .unwrap()
        } else {
            Store::observe(&session, &dir, scope).unwrap()
        };
        assert_eq!(journal_replays() - replays, 1, "one replay of the journal");
        (
            crate::table::KEYS_REVALIDATED.get() - keys,
            store.revision(&session).unwrap(),
        )
    };
    let (observed, revision) = open(false);
    assert_eq!(
        observed, 1,
        "CONTROL: an observing open reuses one roster's key"
    );
    let (sighted, sighted_revision) = open(true);
    assert_eq!(
        sighted, observed,
        "a sighted open checked other keys again than an observing open"
    );
    assert_eq!(sighted_revision, revision);
}

/// The floor file gone, the open stands on no floor: the sighting fails and an open reads another
/// revision; written back, the sighting stands again.
/// NEGATIVE CONTROLS: with the floor's bytes left out of `Store::sighted_now`: `the floor: the
/// sighting stood for a changed store`. With `Store::revision` naming none of the store's files:
/// `the floor: an open read the sighting's revision`.
#[test]
fn a_sighting_fails_once_the_floor_changes() {
    let (lab, dir, scope, sighting) = held();
    let (claims, name, _) = claim_of(&lab, scope);
    let floor_bytes = floor::bytes(&claims, &name).unwrap().unwrap();
    private_file::remove_private(&claims, &format!("{name}.floor")).unwrap();
    moved(&lab, &dir, scope, &sighting, "the floor").unwrap();
    aterm_journal::write_private_durable(&claims, &format!("{name}.floor"), &floor_bytes).unwrap();
    restored(&lab, &dir, scope, &sighting, "the floor");
}

/// A group permit written past the acknowledged witness beside `scope`'s claim in the claim
/// folder `claims`, in place of the completed one or none that stands there: an observing open's
/// boundary then allows one group more, so it reads another revision of the same records.
/// Answers what puts the permit file back as it was, the store then standing byte for byte as
/// it did. For a reader above the store too, whose test must move a table's revision and move it
/// back (`crate::att::tests`).
pub(crate) fn permit_group(claims: &Path, scope: Scope) -> impl FnOnce() + 'static {
    let name = super::super::claim_name(scope);
    let claim = private_file::read_private(claims, &name, super::super::CLAIM_LEN).unwrap();
    let stood = group::bytes(claims, &name).unwrap();
    let acknowledged = Witness::decode(&claim[WITNESS_AT..]).unwrap();
    let end = acknowledged
        .advance(acknowledged.count, [7; 32], 1)
        .unwrap();
    group::prepare(claims, &name, &claim, acknowledged, end).unwrap();
    assert_ne!(group::bytes(claims, &name).unwrap(), stood);
    let claims = claims.to_path_buf();
    move || match &stood {
        Some(bytes) => {
            aterm_journal::write_private_durable(&claims, &group::name(&name), bytes).unwrap();
        }
        None => group::retire(&claims, &name).unwrap(),
    }
}

/// A group permit written past the acknowledged witness, so the open's boundary allows one group
/// more: the sighting fails and an open reads another revision; retired, it stands again.
/// NEGATIVE CONTROLS: with the group permit's bytes left out of `Store::sighted_now`: `the group
/// permit: the sighting stood for a changed store`. With `Store::revision` naming none of the
/// store's files: `the group permit: an open read the sighting's revision`.
#[test]
fn a_sighting_fails_once_the_group_permit_changes() {
    let (lab, dir, scope, sighting) = held();
    let (claims, _, _) = claim_of(&lab, scope);
    let put_back = permit_group(&claims, scope);
    moved(&lab, &dir, scope, &sighting, "the group permit").unwrap();
    put_back();
    restored(&lab, &dir, scope, &sighting, "the group permit");
}

/// The claim's receipt witness changed, custody that no longer witnesses the journal: the
/// sighting fails and an open refuses; changed back, the sighting stands again. Changed while the
/// sighting is judged, once every input compared alike, the store's own check, last, sees it.
/// NEGATIVE CONTROLS: with the claim left out of `Store::sighted_now`'s comparisons: `the claim:
/// the sighting stood for a changed store`. With its closing check reading the claim no more
/// (`Custody::admits` alone): `the sighting stood for a claim changed as it was judged`.
#[test]
fn a_sighting_fails_once_the_custody_claim_changes() {
    let (lab, dir, scope, sighting) = held();
    let (claims, name, claim) = claim_of(&lab, scope);
    let mut changed = claim.clone();
    changed[WITNESS_AT + 8] ^= 1;
    aterm_journal::write_private_durable(&claims, &name, &changed).unwrap();
    let refused = moved(&lab, &dir, scope, &sighting, "the claim");
    assert!(refused.is_err(), "{refused:?}");
    aterm_journal::write_private_durable(&claims, &name, &claim).unwrap();
    restored(&lab, &dir, scope, &sighting, "the claim");

    let (stood, _) = with_before_custody(
        {
            let (claims, name) = (claims.clone(), name.clone());
            move || aterm_journal::write_private_durable(&claims, &name, &changed).unwrap()
        },
        || stands(&lab, &dir, scope, &sighting),
    );
    assert!(
        !stood,
        "the sighting stood for a claim changed as it was judged"
    );
    aterm_journal::write_private_durable(&claims, &name, &claim).unwrap();
    restored(&lab, &dir, scope, &sighting, "the claim, judged");
}

/// Mac 1's history handoffs moved away, the control head unchanged: a record of a roster Mac 1
/// joined after then awaits membership. Answers what moves them back.
fn withhold_history(lab: &Lab) -> impl FnOnce() + 'static {
    let history = lab.paths[1].dir.join("commons/history");
    let aside = lab._home.path().join("held-history");
    std::fs::create_dir(&aside).unwrap();
    let handoffs: Vec<_> = std::fs::read_dir(&history)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.path(), aside.join(entry.file_name()))
        })
        .collect();
    assert!(
        !handoffs.is_empty(),
        "a real historical handoff is required"
    );
    for (from, to) in &handoffs {
        std::fs::rename(from, to).unwrap();
    }
    move || {
        for (from, to) in &handoffs {
            std::fs::rename(to, from).unwrap();
        }
        std::fs::remove_dir(&aside).unwrap();
    }
}

/// A class verdict changed, a record's payload state with it: Mac 0's history handoff moved away,
/// the control head unchanged, so the historical record awaits membership. The sighting fails and
/// an open reads another revision; moved back, the sighting stands again.
/// NEGATIVE CONTROL: with the class verdicts left out of `Store::sighted_now`: `a class verdict:
/// the sighting stood for a changed store`.
#[test]
fn a_sighting_fails_once_a_class_verdict_changes() {
    let (lab, dir, scope, sighting) = held();
    let put_back = withhold_history(&lab);
    moved(&lab, &dir, scope, &sighting, "a class verdict").unwrap();
    put_back();
    restored(&lab, &dir, scope, &sighting, "a class verdict");
}

/// A record appended: the sighting fails and an open reads another revision, of which a new
/// sighting is taken and stands.
/// NEGATIVE CONTROL: an append moves the claim's receipt witness and the journal, each compared,
/// so either alone holds it; with the claim, the journal read and the closing check's claim all
/// left out of `Store::sighted_now`: `an append: the sighting stood for a changed store`.
#[test]
fn a_sighting_fails_once_a_record_is_appended() {
    let (lab, dir, scope, sighting) = held();
    {
        let bootstrap = lab.bootstrap(1);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::open(&session, &dir, scope).unwrap();
        store.commit_catalog(&session, b"appended").unwrap();
    }
    let appended = moved(&lab, &dir, scope, &sighting, "an append").unwrap();
    let sighting = sighted(&lab, &dir, scope);
    assert_eq!(sighting.revision(), appended);
    assert!(stands(&lab, &dir, scope, &sighting).0);
}

/// The access observation changed by a membership change, no record written: the sighting fails
/// and an open reads another revision.
/// NEGATIVE CONTROL: with the access observation left out of `Store::sighted_now`: `the access
/// observation: the sighting stood for a changed store`.
#[test]
fn a_sighting_fails_once_membership_changes() {
    let (lab, dir, scope, sighting) = held();
    lab.admit(&[Role::Reader, Role::Writer]);
    moved(&lab, &dir, scope, &sighting, "the access observation").unwrap();
}

/// Each of the lab's two Macs' control journals given `node`.
fn ingest(lab: &Lab, node: &Node) {
    for n in 0..2 {
        let bootstrap = lab.bootstrap(n);
        Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())
            .unwrap()
            .ingest(node.bytes())
            .unwrap();
    }
}

/// Mac 1's table `dir` of one record, Mac 0's, sealed under a roster that left Mac 1 out and that
/// a control recovery can discard: two rotations made from one control head, the first, of Mac 0
/// alone, taken and the record written under it, the second of both Macs. Mac 1, let in again
/// above the first, took the record through Mac 0's history handoff of that roster, moved away
/// since: the record awaits membership. Answers the lab, the table's folder and scope, the
/// record, and the recovery, which selects the second rotation over the first: the record's
/// source is then discarded on the control head.
fn discardable() -> (Lab, PathBuf, Scope, Digest, impl FnOnce(&Lab)) {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (old, dir) = (
        lab._home.path().join("table-a"),
        lab._home.path().join("table-b"),
    );
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let (left, right) = {
        let (a, b) = (lab.bootstrap(0), lab.bootstrap(1));
        let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
        let grant = |label: &str, bootstrap: &Bootstrap| {
            Grant::recorded(
                label,
                bootstrap.request().unwrap().binding().clone(),
                Role::Writer,
            )
        };
        let alone = vec![grant("m1", &a)];
        let both = vec![grant("m1", &a), grant("m2", &b)];
        (
            admin.prepare_commons_rotation(&journal, alone).unwrap(),
            admin.prepare_commons_rotation(&journal, both).unwrap(),
        )
    };
    ingest(&lab, &left);
    let (scope, id, wire) = {
        let a = lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let mut store = Store::create(&session, &old).unwrap();
        let id = store
            .commit_catalog(&session, b"under the roster a recovery discards")
            .unwrap()
            .id;
        (
            store.scope(),
            id,
            store.wire(&session, &id).unwrap().unwrap().to_vec(),
        )
    };
    lab.rotate(&[Role::Writer, Role::Writer]);
    let (a, b) = (lab.bootstrap(0), lab.bootstrap(1));
    hand_history(&a, &b, left.id());
    {
        let session = Session::open(&b).unwrap();
        let mut store = Store::join(&session, &dir, scope).unwrap();
        store.ingest(&session, &wire).unwrap();
    }
    // The handoff stays away: the record awaits membership from here on.
    drop(withhold_history(&lab));
    let above = {
        let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
        match journal.state() {
            State::Ready { head, .. } => head,
            _ => panic!("the control journal is ready"),
        }
    };
    let network = a.trust().network;
    let recover = move |lab: &Lab| {
        let recovery = admin
            .seal_commons_restricted(
                network,
                Edit::Recover {
                    heads: &[above, right.id()],
                    selected: right.id(),
                },
                &[],
            )
            .unwrap();
        ingest(lab, &right);
        ingest(lab, &recovery);
        lab.rotate(&[Role::Writer, Role::Writer]);
    };
    (lab, dir, scope, id, recover)
}

/// Each class verdict `sighting` keeps, beside that class's verdict asked again now in a fresh
/// session of Mac 1, as a sighting asked to stand asks it (`Store::sighted_now`).
fn verdicts(lab: &Lab, scope: Scope, sighting: &Sighting) -> Vec<(Verdict, Verdict)> {
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    session
        .with_read_batch(scope, |batch| {
            sighting
                .classes
                .iter()
                .map(|(class, kept)| Ok((kept.clone(), batch.class_verdict(class)?)))
                .collect()
        })
        .unwrap()
}

/// The records an observing open of `dir` quarantines now in a fresh session of Mac 1.
fn quarantined(lab: &Lab, dir: &Path, scope: Scope) -> Vec<Digest> {
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let store = Store::observe(&session, dir, scope).unwrap();
    let quarantined = store.quarantined(&session).unwrap();
    quarantined.iter().map(|held| held.receipt.id).collect()
}

/// A class's source discarded on the control head, the other half of its verdict
/// (`Verdict::same`): a control recovery selects another rotation than the one the table's
/// record was sealed under, so an open quarantines the record. The record awaits membership
/// when it is sighted, and so it does once its roster is discarded, a roster outside the
/// selected history granting no key: the class's verdict asked again differs from the one the
/// sighting keeps by its discarded half alone. (A class whose key Mac 1 holds awaits it once
/// discarded, which the key half sees too.) The sighting fails, and an open reads another
/// revision, of which a new sighting is taken and stands.
/// NEGATIVE CONTROLS: with the discarded half left out of `Verdict::same`: `a discarded source:
/// the class's verdict is the one the sighting keeps`. A recovery moves the control head, which
/// the access observation names and a sighting compares first, so either holds the sighting; with
/// the access observation left out of `Store::sighted_now` too: `a discarded source: the sighting
/// stood for a changed store`.
#[test]
fn a_sighting_fails_once_a_class_source_is_discarded() {
    let (lab, dir, scope, id, recover) = discardable();
    let sighting = sighted(&lab, &dir, scope);
    assert_eq!(opened(&lab, &dir, scope), Ok(sighting.revision()));
    assert!(
        stands(&lab, &dir, scope, &sighting).0,
        "CONTROL: the sighting stands before the recovery"
    );
    assert_eq!(
        quarantined(&lab, &dir, scope),
        Vec::<Digest>::new(),
        "CONTROL: nothing is quarantined before the recovery"
    );
    let asked = verdicts(&lab, scope, &sighting);
    assert_eq!(asked.len(), 1, "the table's one record is of one class");
    for (kept, now) in &asked {
        assert!(
            !kept.discarded && matches!(kept.key, KeyVerdict::Awaiting),
            "CONTROL: the record, its source selected, awaits membership"
        );
        assert!(
            now.same(kept),
            "CONTROL: the class's verdict is the sighting's before the recovery"
        );
    }

    recover(&lab);
    assert_eq!(
        quarantined(&lab, &dir, scope),
        [id],
        "CONTROL: the recovery discards the record's source"
    );
    let discarded = moved(&lab, &dir, scope, &sighting, "a discarded source").unwrap();
    for (kept, now) in &verdicts(&lab, scope, &sighting) {
        assert!(
            now.discarded && matches!(now.key, KeyVerdict::Awaiting),
            "CONTROL: the class's source is discarded, its key verdict as it was"
        );
        assert!(
            !now.same(kept),
            "a discarded source: the class's verdict is the one the sighting keeps"
        );
    }
    let sighting = sighted(&lab, &dir, scope);
    assert_eq!(sighting.revision(), discarded);
    assert!(stands(&lab, &dir, scope, &sighting).0);
}

/// A store that holds a conflict certificate makes no sighting: whether a certificate is active
/// is no class's verdict, so no sighting names every input of such a store's revision. Mac 0
/// signs two operations at one sequence, and Mac 1's table takes the certificate of that fork:
/// the sighting taken before it fails and an open reads another revision, and the open that
/// would record a sighting, holding the certificate, reads that revision and makes none.
/// NEGATIVE CONTROL: with `self.conflicts.is_empty()` left out of `Store::sighted_revision`: `a
/// store holding a conflict certificate made a sighting`.
#[test]
fn a_store_holding_a_conflict_certificate_makes_no_sighting() {
    let (lab, dir, scope, sighting) = held();
    let certificate = {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let causal = || Causal {
            sequence: 1,
            previous: None,
            parents: &[],
        };
        let branches = [b"branch one", b"branch two"]
            .map(|payload| seal(&session, scope, causal(), payload).unwrap());
        Conflict::from_pair(&session, scope, &branches[0], &branches[1])
            .unwrap()
            .wire()
            .to_vec()
    };
    {
        let bootstrap = lab.bootstrap(1);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::open(&session, &dir, scope).unwrap();
        store.ingest_conflict(&session, &certificate).unwrap();
    }
    let certified = moved(&lab, &dir, scope, &sighting, "a conflict certificate").unwrap();
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let store = Store::observe_if_exists_sighted(&session, &dir, scope)
        .unwrap()
        .unwrap();
    assert_eq!(
        store.conflicts.len(),
        1,
        "CONTROL: the open holds the certificate"
    );
    let (revision, sighting) = store.sighted_revision(&session).unwrap();
    assert_eq!(revision, certified);
    assert!(
        sighting.is_none(),
        "a store holding a conflict certificate made a sighting"
    );
}

/// A sighting asked of another table does not stand. Two tables of Mac 1 in one home, each of
/// one record of the same payload, on no floor, their records of one class. In one session, each
/// table's sighting is asked of each table's folder with each table's scope: it stands for its
/// own folder and scope alone. Asked through the home's directory, which gives a scope its
/// folder, a table's own sighting answers it unchanged, and the other's has the table read, at
/// its own revision.
/// NEGATIVE CONTROLS: another table's folder and scope meet that table's claim, its key, under
/// which the class's verdict is another grant, and its journal, each compared, so any one holds
/// it; with the claim's comparison, the class verdicts and the journal read all left out of
/// `Store::sighted_now`: `a sighting of table 0 asked of table 1's folder and table 1's scope`,
/// `left: true`, `right: false`. One table's folder with the other's scope is refused where the
/// claim is judged to hold its data folder, before the comparisons (`Store::claimed`) and at the
/// closing check, and by the journal read, whose anchor is the claim's; with those three left
/// out: `a sighting of table 0 asked of table 1's folder and table 0's scope`, `left: true`,
/// `right: false`.
#[test]
fn a_sighting_asked_of_another_table_does_not_stand() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(1);
    let directory = Directory::new(&lab.paths[1], bootstrap.trust());
    let tables = ["one", "another"].map(|role| {
        let session = Session::open(&bootstrap).unwrap();
        let descriptor = Descriptor::application(bootstrap.trust(), "sighted", role, 1).unwrap();
        let scope = directory.publish(&session, &descriptor).unwrap();
        let dir = directory.path(scope).unwrap();
        Store::open(&session, &dir, scope)
            .unwrap()
            .commit_catalog(&session, b"a row")
            .unwrap();
        (dir, scope)
    });
    assert_ne!(tables[0], tables[1]);
    let sightings = tables
        .each_ref()
        .map(|(dir, scope)| sighted(&lab, dir, *scope));
    assert_ne!(sightings[0].revision(), sightings[1].revision());

    let session = Session::observe(&bootstrap).unwrap();
    for (of, sighting) in sightings.iter().enumerate() {
        for (folder, (dir, _)) in tables.iter().enumerate() {
            for (scoped, (_, scope)) in tables.iter().enumerate() {
                assert_eq!(
                    Store::sighting_stands(&session, dir, *scope, sighting),
                    (folder, scoped) == (of, of),
                    "a sighting of table {of} asked of table {folder}'s folder and table \
                     {scoped}'s scope"
                );
            }
        }
        for (asked, (_, scope)) in tables.iter().enumerate() {
            let read = directory
                .observe_table_sighted(&session, *scope, Some(sighting))
                .unwrap();
            match read {
                Sighted::Unchanged => assert_eq!(
                    asked, of,
                    "a sighting of table {of} answered table {asked} unchanged"
                ),
                Sighted::Opened(store) => {
                    assert_ne!(
                        asked, of,
                        "table {asked}'s own sighting did not answer it unchanged"
                    );
                    assert_eq!(
                        store.revision(&session).unwrap(),
                        sightings[asked].revision(),
                        "table {asked}, read past a sighting of table {of}"
                    );
                }
            }
        }
    }
}

/// A writer no Mac of the lab is: whom the cutoffs below cut.
const REMOVED: Actor = [9; 48];

/// The records an observing open of `dir` flags as removal cutoffs now in a fresh session of
/// Mac 1.
fn cutoffs(lab: &Lab, dir: &Path, scope: Scope) -> Vec<Digest> {
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let store = Store::observe(&session, dir, scope).unwrap();
    flagged(&store)
}

/// The records `store` flags as removal cutoffs.
fn flagged(store: &Store) -> Vec<Digest> {
    store
        .records
        .iter()
        .filter(|(_, stored)| stored.cut.is_some())
        .map(|(id, _)| *id)
        .collect()
}

/// A record's cutoff flag moves the revision, and a sighting made before it does not stand.
/// Signed by the store's own writer (`Store::sign_cutoff`), a cutoff is a record appended, which
/// an open flags: the sighting fails and an open reads another revision, of which a new sighting
/// is taken and stands. And the flag of a record the store already holds: the table's
/// historical record is a cutoff Mac 0 signed under the roster Mac 1 joined after, flagged while
/// Mac 1's history handoff opens it; the handoff moved away, the record awaits membership and
/// is flagged no more, so the sighting fails and an open reads another revision; moved back,
/// the sighting stands again.
/// NEGATIVE CONTROLS: a cutoff signed moves the claim's receipt witness and the journal, each
/// compared; with the claim, the journal read and the closing check's claim all left out of
/// `Store::sighted_now`: `a cutoff signed: the sighting stood for a changed store`. A flag
/// follows from its record's bytes and payload state, both named by a revision beside it, so no
/// change moves the flag alone: with the flag left out of `Store::revision` the test passes, by
/// the payload state; with the payload state left out it passes, by the flag; with both left out:
/// `a cutoff flag: an open read the sighting's revision`.
#[test]
fn a_sighting_fails_once_a_record_becomes_a_removal_cutoff() {
    let (lab, dir, scope) = table_holding(|store, session| {
        assert!(store.sign_cutoff(session, REMOVED, Signing::Fresh).unwrap());
        flagged(store)[0]
    });
    let sighting = sighted(&lab, &dir, scope);
    assert_eq!(opened(&lab, &dir, scope), Ok(sighting.revision()));
    let historical = cutoffs(&lab, &dir, scope);
    assert_eq!(
        historical.len(),
        1,
        "CONTROL: Mac 0's cutoff, opened through its history handoff, is flagged"
    );

    {
        let bootstrap = lab.bootstrap(1);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::open(&session, &dir, scope).unwrap();
        assert!(
            store
                .sign_cutoff(&session, REMOVED, Signing::Fresh)
                .unwrap()
        );
    }
    assert_eq!(
        cutoffs(&lab, &dir, scope).len(),
        2,
        "CONTROL: the cutoff signed is flagged"
    );
    let signed = moved(&lab, &dir, scope, &sighting, "a cutoff signed").unwrap();
    let sighting = sighted(&lab, &dir, scope);
    assert_eq!(sighting.revision(), signed);
    assert!(stands(&lab, &dir, scope, &sighting).0);

    let put_back = withhold_history(&lab);
    let awaiting = cutoffs(&lab, &dir, scope);
    assert_eq!(
        awaiting.len(),
        1,
        "CONTROL: Mac 0's cutoff, awaiting membership, is flagged no more"
    );
    assert!(!awaiting.contains(&historical[0]));
    moved(&lab, &dir, scope, &sighting, "a cutoff flag").unwrap();
    put_back();
    restored(&lab, &dir, scope, &sighting, "a cutoff flag");
}
