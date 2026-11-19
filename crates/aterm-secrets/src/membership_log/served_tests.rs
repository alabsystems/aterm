// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serving a log rooted at a final checkpoint (tracker FL-148c): a Mac behind the cut catches up
//! from a pruned peer, a record that follows only what the other side of a split dropped is
//! placed once that side's root is taken, and a forged stub, a stub no root dropped, a root that
//! is not final and a body the cut does not compute are each refused with nothing taken.

use super::*;
use crate::membership_log::checkpoint::tests::{
    A, B, C, HOUR, Mac, NOW, SPLIT, T, draft, epoch, found, hold, name, one, share_at, supersedes,
    two,
};
use crate::membership_log::checkpoint::{DAY_MS, MIN_AGE_MS, propose};
use crate::membership_log::rooted::tests::{
    Evaluation, SEEDS, assert_evaluates_as, carry_of_dropped, common, entry, evaluated,
    random_vault, side_b, sleeper, store_of,
};
use crate::membership_log::store::{REPLY_CAP, read_reply};
use crate::membership_log::{
    EpochAudience, Kind, MAX_HELD_RECORDS, Reading, Statement, Store, budgets, decode_records,
    decode_reply, read_records,
};

/// The sleeper's vault ([`sleeper`]) a fortnight on: A and B, the recent seats at its cut,
/// shared its checkpoint, so it is final; the peer is pruned there, dropping A's first `Put`,
/// which C's record after the cut follows alone. The Mac behind the cut holds the genesis and
/// the epoch alone.
struct Behind {
    whole: Log,
    peer: Log,
    behind: Log,
    body: Body,
    slept: RecordId,
    dropped: RecordId,
}

fn behind() -> Behind {
    let slept = sleeper();
    let mut whole = slept.l2.clone();
    let at = T + MIN_AGE_MS + HOUR;
    share_at(&mut whole, A, &slept.body, at);
    share_at(&mut whole, B, &slept.body, at);
    let peer = whole.adopt(&slept.body).unwrap();
    let mut behind = Log::new(whole.genesis().clone()).unwrap();
    for record in whole
        .records()
        .filter(|r| matches!(r.kind(), Kind::Epoch | Kind::Wrap))
    {
        hold(&mut behind, record.clone());
    }
    Behind {
        whole,
        peer,
        behind,
        body: slept.body,
        slept: slept.slept,
        dropped: slept.dropped,
    }
}

/// [`behind`]'s vault as a test of another module meets it (a Mac's records pass, a join): a
/// peer pruned at a final checkpoint, and what a Mac behind its cut holds.
pub(crate) struct PrunedVault {
    /// The seed whose key signed the vault's genesis.
    pub(crate) founder: [u8; 32],
    /// The pruned peer's store, reading format 2.
    pub(crate) peer: Store,
    /// What the Mac behind the cut holds, the genesis first.
    pub(crate) behind: Vec<Record>,
    /// The checkpoint the peer is rooted at.
    pub(crate) root: RecordId,
    /// The sleeper's record, which follows the dropped one alone.
    pub(crate) slept: RecordId,
    /// The record the peer's prune dropped.
    pub(crate) dropped: RecordId,
    /// A record a seat signs after the prune, which the peer does not hold yet.
    pub(crate) next: Record,
    /// A record seat A signs after `next`, which follows `next` alone: what waits at a holder
    /// of the peer's store until `next` comes.
    pub(crate) after_next: Record,
    /// A record seat B signed on what the Mac behind the cut holds, which the peer never
    /// took: what another writer of that Mac's records file appends to it.
    pub(crate) stray: Record,
}

/// [`PrunedVault`] of [`behind`].
pub(crate) fn pruned_vault() -> PrunedVault {
    let world = behind();
    let next = one(
        draft(&world.peer, A, T + MIN_AGE_MS + 2 * HOUR, name("next")),
        A,
    );
    let mut with_next = world.peer.clone();
    hold(&mut with_next, next.clone());
    let after_next = one(
        draft(
            &with_next,
            A,
            T + MIN_AGE_MS + 2 * HOUR + 1,
            name("after next"),
        ),
        A,
    );
    let stray = one(draft(&world.behind, B, T + DAY_MS, name("stray")), B);
    let mut records = vec![world.behind.genesis().clone()];
    records.extend(
        world
            .behind
            .records()
            .filter(|record| record.kind() != Kind::Genesis)
            .cloned(),
    );
    PrunedVault {
        founder: crate::membership_log::checkpoint::tests::FOUNDER,
        peer: store_of(world.peer.clone()).read_at(Reading::Format(crate::crossing::FORMAT_TWO)),
        behind: records,
        root: checkpoint_id(&world.body).unwrap(),
        slept: world.slept,
        dropped: world.dropped,
        next,
        after_next,
        stray,
    }
}

impl PrunedVault {
    /// Seat A, a member of the pruned peer, lets the Mac of `request` in under `name`, after
    /// the cut: the peer then holds an `Admit` of it and a `Wrap` of the vault's `[everyone]`
    /// epoch key to it. Returns the `Admit`'s id and the `Allowed` A signs for the new Mac's
    /// bundle. Seat C shares the checkpoint first: the cut holds a version-1 record's stub, so
    /// a Mac behind it takes the root only on the shares of more than half of the log's seats
    /// ([`super::served`]), which the new Mac's seat makes four.
    pub(crate) fn admit(
        &mut self,
        request: &crate::commons::binding::Request,
        name: &str,
    ) -> (RecordId, crate::members::Allowed) {
        let at = T + MIN_AGE_MS + 3 * HOUR;
        let log = self.peer.log().expect("a log");
        let body = log.newest_root().expect("a root").body.clone();
        let share = two(draft(log, C, at - 1, body), C, None);
        assert_eq!(self.peer.ingest_records(vec![share]).unwrap(), 1);
        let log = self.peer.log().expect("a log");
        let admit = one(
            draft(
                log,
                A,
                at,
                Body::Admit {
                    entrant: crate::membership_log::Entrant {
                        request: request.clone(),
                        node_key: *request.binding().writer(),
                        name: name.to_owned(),
                        label: name.to_owned(),
                    },
                    evidence: crate::membership_log::Evidence::Phrase,
                },
            ),
            A,
        );
        assert_eq!(self.peer.ingest_records(vec![admit.clone()]).unwrap(), 1);
        let log = self.peer.log().expect("a log");
        let keys = log.unwrap_epochs(&A.identity()).expect("A's epoch keys");
        let (epoch, key) = keys.iter().next().expect("the vault's one epoch");
        assert_eq!(keys.len(), 1);
        let wrap = one(
            log.draft_wrap(&A.id(), at + 1, *epoch, key, request.binding().recipient())
                .expect("a wrap"),
            A,
        );
        assert_eq!(self.peer.ingest_records(vec![wrap]).unwrap(), 1);
        let allowed =
            crate::members::Allowed::sign(request.binding(), name, &A.seed()).expect("A's Allowed");
        (*admit.id(), allowed)
    }
}

/// What a store's log holds, as two logs that took the same records and roots hold it alike:
/// its records, its stubs, its roots, the earlier roots it keeps
/// beside them and its heads digest.
pub(in crate::membership_log) fn holding(log: &Log) -> String {
    let rooting = log.rooting();
    let ids = |roots: &[crate::membership_log::rooted::Root]| {
        roots.iter().map(|root| root.id).collect::<Vec<_>>()
    };
    format!(
        "records {:?}\nstubs {:?}\nroots {:?}\nearlier {:?}\nheads {:?}\n",
        log.records().map(|r| *r.id()).collect::<BTreeSet<_>>(),
        rooting.map(|r| r.stubs.clone()),
        rooting.map(|r| ids(&r.roots)),
        rooting.map(|r| ids(&r.earlier)),
        log.heads_digest(),
    )
}

/// `peer`'s reply to a pull by `receiver`, through its bytes.
fn reply(peer: &Store, receiver: &Store) -> (Vec<Record>, Option<Served>, Vec<u8>) {
    let missing = peer.missing_served(&receiver.have(), REPLY_CAP);
    let bytes = read_reply(&missing).unwrap();
    let (records, served) = decode_reply(&bytes, MAX_HELD_RECORDS, Reading::Follow).unwrap();
    (records, served, bytes)
}

/// A Mac behind a final cut catches up from a peer pruned there: the peer serves, beside the
/// records it lacks, its root and the stub of the record its prune dropped; the Mac reads the
/// cut through that stub, finds the root final by the shares the reply brought, prunes there
/// itself and then holds what the peer holds (the sleeper's record that follows only the
/// dropped `Put` among it) and evaluates as the whole log. Asked again it is served nothing. A
/// Mac with no records at all (a joiner) catches up the same way. Records alone, as a peer
/// served them before this slice, leave the sleeper's record waiting on the dropped one, and a
/// reader that reads records alone refuses the served frame rather than take what would wait.
///
/// NEGATIVE CONTROLS, each a change to the code run against this test and seen to fail:
/// - [`Store::missing_served`] serving no root (as before this slice): ``a pruned peer serves its
///   root to a Mac behind its cut``.
/// - [`Log::named_cuts`] answering that no pull names the cut: ``assertion failed:
///   again.records.is_empty() && again.served.is_none()``, the root served again to a Mac that
///   took it.
#[test]
fn vault_checkpoint_a_mac_behind_the_cut_catches_up_from_a_pruned_peer() {
    let world = behind();
    let peer = store_of(world.peer.clone());
    let mut receiver = store_of(world.behind.clone());

    let (records, served, bytes) = reply(&peer, &receiver);
    let served = served.expect("a pruned peer serves its root to a Mac behind its cut");
    assert_eq!(served.roots, vec![checkpoint_id(&world.body).unwrap()]);
    assert_eq!(
        served.stubs.keys().copied().collect::<Vec<_>>(),
        vec![world.dropped]
    );
    assert!(
        decode_records(&bytes).is_err(),
        "a reader of records alone refuses the served frame"
    );

    let mut records_alone = receiver.clone();
    records_alone.ingest_records(records.clone()).unwrap();
    assert!(
        !records_alone.holds(&world.slept),
        "records alone leave the sleeper's record waiting on the dropped one"
    );

    let added = receiver.ingest_served(records, Some(&served)).unwrap();
    assert!(added > 0);
    let log = receiver.log().unwrap();
    assert_eq!(log.newest_root().map(|r| r.id), checkpoint_id(&world.body));
    assert!(
        log.get(&world.slept).is_some(),
        "the sleeper's record is placed"
    );
    assert!(log.is_dropped(&world.dropped) && log.get(&world.dropped).is_none());
    assert_eq!(holding(log), holding(&world.peer));
    assert_eq!(log.heads_digest(), world.whole.heads_digest());
    assert_eq!(evaluated(log), evaluated(&world.whole));

    let again = peer.missing_served(&receiver.have(), REPLY_CAP);
    assert!(again.records.is_empty() && again.served.is_none());

    // A joiner, which holds nothing, catches up the same way.
    let mut joiner = Store::with_compiled(None);
    let (records, served, _) = reply(&peer, &joiner);
    joiner.ingest_served(records, served.as_ref()).unwrap();
    assert_eq!(holding(joiner.log().unwrap()), holding(&world.peer));
}

/// A Mac that holds the cut takes the root as [`Log::adopt`] does: it recomputes it and reads no
/// stub of the peer's, so a forged one changes nothing, and the stub it keeps is made from its
/// own record. A whole log serves byte for byte what it served before: its records' stream alone.
///
/// NEGATIVE CONTROL: [`Log::named_cuts`] answering that no pull names the cut, seen to fail with
/// ``assertion failed: peer.missing_served(&receiver.have(), REPLY_CAP).served.is_none()``.
#[test]
fn vault_checkpoint_a_mac_that_holds_the_cut_takes_no_stub_of_the_peers() {
    let world = behind();
    let peer = store_of(world.peer.clone());
    let mut holder = store_of(world.whole.clone());
    let missing = peer.missing_served(&[], REPLY_CAP);
    let mut served = missing.served.clone().unwrap();
    for stub in served.stubs.values_mut() {
        stub.authored_ms += DAY_MS;
    }
    holder
        .ingest_served(missing.records, Some(&served))
        .unwrap();
    assert_eq!(holding(holder.log().unwrap()), holding(&world.peer));

    let whole = store_of(world.whole.clone());
    let missing = whole.missing_served(&[], REPLY_CAP);
    assert!(missing.served.is_none());
    assert_eq!(
        read_reply(&missing).unwrap(),
        read_records(&missing.records).unwrap()
    );
    // A reader of records alone is served records alone by a rooted peer too, whatever its
    // `have` shows: it would refuse the frame.
    let alone = peer.missing_within(&[], REPLY_CAP);
    assert!(alone.served.is_none());
    assert_eq!(
        read_reply(&alone).unwrap(),
        read_records(&alone.records).unwrap()
    );
    // A peer whose pull names the root's cut is served no root.
    let receiver = store_of(world.whole);
    assert!(
        peer.missing_served(&receiver.have(), REPLY_CAP)
            .served
            .is_none()
    );
}

/// A split ([`side_b`]): a Mac pruned at A's side's final cut takes B's records from a Mac pruned
/// at B's, and B's `b3`, which follows only `b2`, which B's prune dropped, waits. Served B's root
/// with its stub, it reads B's cut through it and A's own stub of `a0`, takes B's root, places
/// `b3` and every record after it, and reads what the whole log reads. B's side is B and C, who
/// both shared B's checkpoint: a root read through stubs is taken on the shares of more than half
/// of the seats too ([`vault_checkpoint_one_seat_cannot_certify_the_stubs_it_serves`]), so with
/// B's share alone the Mac keeps waiting and takes nothing.
#[test]
fn vault_checkpoint_a_record_after_the_other_sides_drop_is_placed_once_its_root_is_taken() {
    let common = common();
    let put = &common.put;
    let mut side_a = common.log.clone();
    let a1 = two(
        put(&side_a, A, SPLIT - DAY_MS, "a.key", b"a1"),
        A,
        Some(&Statement {
            supersedes: vec![*common.a0.id()],
            carries: None,
        }),
    );
    let a1 = hold(&mut side_a, a1);
    let cut_a = checkpoint::plan_at(&side_a, SPLIT, &[*a1.id()]).unwrap();
    share_at(&mut side_a, A, &cut_a.body, SPLIT + 2 * DAY_MS);
    let side = side_b(&common);
    let x = side_a.adopt(&cut_a.body).unwrap();
    let alone = store_of(side.log.adopt(&side.body).unwrap());
    let mut side_log = side.log.clone();
    share_at(&mut side_log, C, &side.body, SPLIT + 3 * DAY_MS);
    let y = side_log.adopt(&side.body).unwrap();
    let mut whole = side_a.clone();
    whole.merge(&side_log).unwrap();

    let mut receiver = store_of(x);
    let (records, served, _) = reply(&alone, &receiver);
    let mut refused = receiver.clone();
    assert!(
        refused
            .ingest_served(records, Some(&served.unwrap()))
            .is_err(),
        "B's root on B's share alone, one seat of three, is refused"
    );
    assert_eq!(refused.inventory(), receiver.inventory());

    let peer = store_of(y);
    let (records, served, _) = reply(&peer, &receiver);
    let served = served.expect("B's side serves its root to A's");
    let mut records_alone = receiver.clone();
    records_alone.ingest_records(records.clone()).unwrap();
    assert!(!records_alone.holds(side.b3.id()), "b3 waits for b2");

    receiver.ingest_served(records, Some(&served)).unwrap();
    let log = receiver.log().unwrap();
    assert_eq!(log.rooting().unwrap().roots.len(), 2);
    assert!(log.get(side.b3.id()).is_some(), "b3 is placed by b2's stub");
    assert!(log.is_dropped(side.b2.id()) && log.is_dropped(common.a0.id()));
    let mut expected: BTreeSet<RecordId> = whole.records().map(|r| *r.id()).collect();
    expected.remove(common.a0.id());
    expected.remove(side.b2.id());
    assert_eq!(
        log.records().map(|r| *r.id()).collect::<BTreeSet<_>>(),
        expected
    );
    assert_eq!(evaluated(log), evaluated(&whole));
}

/// A receiver's store before and after a refused take: unchanged.
fn unchanged(before: &Store, after: &Store) -> bool {
    before.inventory() == after.inventory()
        && before.heads_digest() == after.heads_digest()
        && after.log().is_some_and(|log| log.rooting().is_none())
}

/// Whether the Mac behind the cut takes `served` and the peer's records, forged as `forge` says.
fn takes(forge: impl Fn(&mut Behind, &mut Vec<Record>, &mut Served)) -> bool {
    let mut world = behind();
    let peer = store_of(world.peer.clone());
    let receiver = store_of(world.behind.clone());
    let missing = peer.missing_served(&receiver.have(), REPLY_CAP);
    let mut records = missing.records;
    let mut served = missing.served.unwrap();
    forge(&mut world, &mut records, &mut served);
    let mut taking = receiver.clone();
    let taken = taking.ingest_served(records, Some(&served)).is_ok();
    assert!(
        taken || unchanged(&receiver, &taking),
        "a refused take takes nothing"
    );
    taken
}

/// A served stub with one field forged (the dropped `Put`'s clock) is refused, and nothing taken:
/// the root's `dropped` digest hashes each stub's fields, so the stubs read compute another body.
///
/// NEGATIVE CONTROL: `Mutant::IdsOnly` (the digest names the dropped ids alone, as FL-148b's did),
/// run from the test's first line (the whole vault built under it) and seen to fail with
/// ``assertion failed: !takes(clock)``: the forged stub is taken.
#[test]
fn vault_checkpoint_a_forged_stub_field_is_refused() {
    let clock = |_: &mut Behind, _: &mut Vec<Record>, served: &mut Served| {
        for stub in served.stubs.values_mut() {
            stub.authored_ms += HOUR;
        }
    };
    assert!(takes(|_, _, _| {}), "the honest reply is taken");
    assert!(!takes(clock));

    MUTANT.set(Some(Mutant::IdsOnly));
    let taken = takes(clock);
    MUTANT.set(None);
    assert!(taken, "IdsOnly takes the forged stub");
}

/// A served stub set other than the one the root dropped is refused, and nothing taken: a stub
/// of a record no root dropped (a made-up id the peer names as dropped), and a dropped record's
/// stub left out (the cut is then not held).
///
/// NEGATIVE CONTROL: `Mutant::Uncovered` (a stub no root taken drops is read too), run from the
/// test's first line and seen to fail with ``assertion failed: !takes(extra)``: the made-up stub
/// is taken.
#[test]
fn vault_checkpoint_a_stub_no_root_dropped_is_refused() {
    let extra = |_: &mut Behind, _: &mut Vec<Record>, served: &mut Served| {
        let mut made_up = served.stubs.values().next().unwrap().clone();
        made_up.parents = vec![*made_up.parents.first().unwrap()];
        served.stubs.insert([0xEE; 32], made_up);
    };
    assert!(!takes(extra));
    assert!(!takes(|_, _, served| served.stubs.clear()));

    MUTANT.set(Some(Mutant::Uncovered));
    let taken = takes(extra);
    MUTANT.set(None);
    assert!(taken, "Uncovered takes a stub no root dropped");
}

/// A root that is not final by its shares is refused, and nothing taken: B's share left out of
/// the reply leaves A's, one of the two recent seats. So is a body the cut does not compute
/// (another state digest), though A and B both signed it.
///
/// NEGATIVE CONTROL: `Mutant::Unjudged` (a root read through the stubs is taken unjudged), run from
/// the test's first line and seen to fail with ``assertion failed: !takes(one_share)``; the test's
/// own last lines hold that it takes the forged state too.
#[test]
fn vault_checkpoint_a_root_not_final_by_its_shares_is_refused() {
    let one_share = |world: &mut Behind, records: &mut Vec<Record>, _: &mut Served| {
        let id = checkpoint_id(&world.body);
        records.retain(|r| checkpoint_id(r.body()) != id || r.actor() != &B.id());
    };
    let forged_state = |world: &mut Behind, records: &mut Vec<Record>, served: &mut Served| {
        let Body::Checkpoint {
            predecessor,
            cut_ms,
            heads,
            dropped,
            ..
        } = world.body.clone()
        else {
            panic!("a checkpoint");
        };
        let forged = Body::Checkpoint {
            predecessor,
            cut_ms,
            heads,
            state: [0x77; 32],
            dropped,
        };
        let mut log = world.peer.clone();
        let at = T + MIN_AGE_MS + 2 * HOUR;
        share_at(&mut log, A, &forged, at);
        share_at(&mut log, B, &forged, at);
        records.extend(
            log.records()
                .filter(|r| checkpoint_id(r.body()) == checkpoint_id(&forged))
                .cloned(),
        );
        served.roots = vec![checkpoint_id(&forged).unwrap()];
    };
    assert!(!takes(one_share));
    assert!(!takes(forged_state));

    MUTANT.set(Some(Mutant::Unjudged));
    let (one, forged) = (takes(one_share), takes(forged_state));
    MUTANT.set(None);
    assert!(one, "Unjudged takes a root one recent seat of two shared");
    assert!(forged, "Unjudged takes a body the cut does not compute");
}

/// A served frame reads back as it was written; a frame out of order, naming no root, or
/// trailing bytes is refused.
#[test]
fn vault_checkpoint_a_served_frame_reads_back() {
    let world = behind();
    let served = world.peer.served().unwrap();
    let bytes = served.encode();
    assert_eq!(bytes.len(), served.wire_len());
    let read = Served::decode(&bytes, Reading::Follow).unwrap();
    assert_eq!(read, served);
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(Served::decode(&trailing, Reading::Follow).is_err());
    let none = Served {
        network: served.network,
        roots: Vec::new(),
        stubs: served.stubs.clone(),
    };
    assert!(Served::decode(&none.encode(), Reading::Follow).is_err());
    let twice = Served {
        network: served.network,
        roots: vec![served.roots[0], served.roots[0]],
        stubs: served.stubs.clone(),
    };
    assert!(Served::decode(&twice.encode(), Reading::Follow).is_err());
}

/// A rooted log's inventory, which set-reconciling peers compare (the mesh's, aterm-db's records
/// exchange), names each record its root dropped as known, as the whole log of the same history
/// does, so the two agree and a whole peer sends back none of them; the ids it offers name held
/// records alone, and a cursor at a dropped record starts again at genesis.
///
/// NEGATIVE CONTROL: [`Store::complete_inventory`] naming held records alone, as before this
/// slice, seen to fail with ``assertion `left == right` failed`` on the two inventories.
#[test]
fn vault_checkpoint_a_rooted_inventory_reads_a_dropped_record_as_known() {
    let world = behind();
    let rooted = store_of(world.peer.clone());
    let whole = store_of(world.whole.clone());
    assert_eq!(rooted.complete_inventory(), whole.complete_inventory());
    assert!(rooted.complete_inventory().contains(&world.dropped));
    let offered = rooted.complete_missing(&BTreeSet::new(), None, usize::MAX);
    assert!(!offered.contains(&world.dropped));
    assert_eq!(
        rooted.complete_missing(&BTreeSet::new(), Some(&world.dropped), usize::MAX),
        offered,
        "a cursor at a dropped record starts again at genesis"
    );
}

/// The legacy fence counts the records a root dropped (tracker FL-148D). A rooted log tells a
/// set-reconciling peer of every id it knows, the dropped ones among them
/// ([`Store::complete_inventory`]), and a build before the fence reads no more than
/// [`LEGACY_RECORDS`] ids of an inventory (aterm-db's legacy exchange, `MAX_NODES`). So what
/// the fence counts ([`Log::known_count`]) is the size of that inventory: a rooted log within
/// the legacy count of held records and past it with its stubs has crossed
/// ([`crate::crossing::crossed`]) and states a fenced bound, with no mark stored, while the
/// whole log of as many held records has not. A whole log's count is its held count.
///
/// NEGATIVE CONTROL: `rooted::Mutant::FenceHeldAlone` (the fence counts held records alone, as
/// before this slice), run from the test's first line and seen to fail with ``assertion `left
/// == right` failed: the fence counts what the inventory names``, `left: 11`, `right: 12`.
#[cfg(unix)]
#[test]
fn vault_checkpoint_the_legacy_fence_counts_the_records_a_root_dropped() {
    use crate::crossing::{FormatMark, Mark, crossed, past_legacy, stated};
    use crate::membership_log::LEGACY_RECORDS;

    let world = behind();
    assert_eq!(world.whole.known_count(), world.whole.held_count());
    assert_eq!(
        world.peer.known_count(),
        store_of(world.peer.clone()).complete_inventory().len(),
        "the fence counts what the inventory names"
    );
    assert_eq!(world.peer.known_count(), world.whole.held_count());

    // The same log had its roots dropped as many records as a legacy build takes.
    let mut log = world.peer.clone();
    let rooting = std::sync::Arc::make_mut(log.rooting.as_mut().unwrap());
    let stub = rooting.stubs.values().next().unwrap().clone();
    for i in 0..LEGACY_RECORDS {
        let mut id = [0xAB; 32];
        id[..8].copy_from_slice(&(i as u64).to_be_bytes());
        rooting.stubs.insert(id, stub.clone());
    }
    let store = store_of(log);
    let log = store.log().unwrap();
    assert!(!past_legacy(log.held_count()));
    let inventory = store.complete_inventory();
    assert!(
        inventory.len() > LEGACY_RECORDS,
        "more ids than a build before the fence reads of an inventory"
    );
    assert_eq!(log.known_count(), inventory.len());

    let root = aterm_tempfile::tempdir().unwrap();
    let mark = Mark::at(root.path().join("records.crossed"));
    let format = FormatMark::at(root.path().join("records.format"));
    assert!(crossed(&mark, &store).unwrap());
    assert!(stated(&mark, &format, &store).unwrap().fenced());
    let whole = store_of(world.whole.clone());
    assert!(!crossed(&mark, &whole).unwrap());
    assert!(!stated(&mark, &format, &whole).unwrap().fenced());
    assert!(!mark.stored().unwrap(), "the count alone, no mark stored");
}

/// A served reply passes what its store holds by its frame's head at most (tracker FL-148D).
/// The reply is the held records its puller lacks and the frame, which is the stubs' bytes
/// and a head of [`Served::head_len`] bytes; the store counts its records' and its stubs'
/// bytes in its own ([`Log::total_bytes`]), which stay within its capacity. So a joiner's
/// reply, every held record and the frame, is exactly the store's bytes and that head, a
/// puller that holds some of them is sent no more, and the head is at most
/// [`MAX_SERVED_HEAD`]: while a store's capacity is one reply's, a served reply passes
/// [`REPLY_CAP`] only from a store within that head of full
/// ([`Store::missing_served`], rooted.rs "Not yet").
///
/// NEGATIVE CONTROL: `rooted::Mutant::FreeStubs` (the stubs count against no budget, so a
/// store's bytes do not bound its reply), run from the test's first line and seen to fail with
/// ``assertion `left == right` failed: a joiner's reply is its store's bytes and the frame's
/// head``, `left: 5485`, `right: 5283`: the reply's bytes past the store's by its stubs'.
#[test]
fn vault_checkpoint_a_served_reply_passes_its_store_by_its_frames_head_at_most() {
    assert_eq!(MAX_SERVED_HEAD, 42 + 32 * MAX_SERVED_ROOTS);
    assert_eq!(
        Reading::Follow.capacity(),
        REPLY_CAP,
        "a store holds one reply: its served reply passes the reply's bound by the head alone"
    );
    let once = behind();
    let twice = twice_pruned(None);
    let mut roots = BTreeSet::new();
    for (peer, behind) in [
        (once.peer.clone(), once.behind.clone()),
        (twice.pruned(), twice.behind.clone()),
    ] {
        let store = store_of(peer);
        let log = store.log().unwrap();
        assert_eq!(log.waiting(), 0);
        assert!(log.total_bytes() <= log.capacity());

        let joiner = store.missing_served(&[], REPLY_CAP);
        let frame = joiner
            .served
            .as_ref()
            .expect("a joiner is served the roots");
        let head = Served::head_len(frame.roots.len());
        assert!(head <= MAX_SERVED_HEAD);
        roots.insert(frame.roots.len());
        assert_eq!(
            read_reply(&joiner).unwrap().len(),
            log.total_bytes() + head,
            "a joiner's reply is its store's bytes and the frame's head"
        );

        let (records, served, bytes) = reply(&store, &store_of(behind));
        assert!(served.is_some() && !records.is_empty());
        assert!(bytes.len() <= log.total_bytes() + head);
    }
    assert_eq!(roots.len(), 2, "one root and two");
}

/// A log holds its bytes to the capacity of the store it is the log of (tracker FL-148D): a
/// store gives its log its format when it makes it and when it is read at another, and every
/// log made from that one carries it, a pruned log and a projection to heads among them (a
/// cut's log too, which computes the same checkpoint), so none falls back to format 1's capacity where its store's is larger (tracker
/// FL-249). A log no store made is format 1's.
///
/// NEGATIVE CONTROL: [`Log::rebuilt_with`] not carrying the format (a pruned log made as
/// format 1's), seen to fail with ``assertion `left == right` failed: a pruned log carries
/// it``, `left: Format(1)`, `right: Follow`.
#[test]
fn vault_checkpoint_a_log_holds_its_bytes_to_its_stores_capacity() {
    let world = behind();
    assert_eq!(world.whole.stored_at, Reading::ONE);
    assert_eq!(world.whole.capacity(), Reading::ONE.capacity());
    assert_eq!(world.whole.fill(None).bound, world.whole.capacity());

    let mut store = Store::with_compiled(None).read_at(Reading::Follow);
    assert_eq!(store.capacity(), Reading::Follow.capacity());
    store.insert(world.whole.genesis().clone()).unwrap();
    assert_eq!(store.log().unwrap().stored_at, Reading::Follow);
    let one = store.read_at(Reading::ONE);
    assert_eq!(one.log().unwrap().stored_at, Reading::ONE);

    let mut whole = world.whole.clone();
    whole.stored_at = Reading::Follow;
    let pruned = whole.adopt(&world.body).unwrap();
    assert_eq!(pruned.stored_at, Reading::Follow, "a pruned log carries it");
    assert_eq!(
        whole.project_heads(&whole.heads()).unwrap().stored_at,
        Reading::Follow,
        "a projection carries it"
    );
    let Body::Checkpoint { cut_ms, heads, .. } = &world.body else {
        panic!("a checkpoint");
    };
    assert_eq!(
        checkpoint_id(&checkpoint::plan_at(&whole, *cut_ms, heads).unwrap().body).unwrap(),
        checkpoint_id(&world.body).unwrap(),
        "the cut's log, made with the format, computes the same body"
    );
}

/// One seat cannot certify the stubs it serves. A, B and C hold a vault in which B's `Put` `x` is
/// B's one record in the month before day `T`. A Mac behind the cut is served, by A, every record
/// but `x`, the stub of `x` relabelled as A's own (A's signer, A's next sequence and previous), A's
/// `h` after it naming it superseded, and A's checkpoint at `h`, which drops `x`, shared by A
/// alone. Read through that stub, the cut's recent seats are A alone, so the checkpoint is final
/// by the recent rule, though the whole log refuses `h` (its previous is B's record). Read through
/// stubs, a root's shares are its recent seats' and the stubs name who is recent; so a root taken
/// through them also needs the shares of more than half of the seats, which every Mac holds whole
/// (a prune keeps every weighed record). A's one share of three seats is refused, and nothing
/// taken.
///
/// NEGATIVE CONTROL: `Mutant::RecentAlone` (the recent rule alone, as the first cut of this slice
/// judged a root read through stubs), run from the test's first line and seen to fail with
/// ``assertion failed: !takes``: the Mac takes A's root, drops B's `x` as A's, and is then shut
/// out of B's next record.
#[test]
fn vault_checkpoint_one_seat_cannot_certify_the_stubs_it_serves() {
    let mut whole = found(&[A, B, C]);
    let (e, key) = epoch(
        &mut whole,
        A,
        T - 5 * DAY_MS,
        EpochAudience::Everyone,
        1,
        &[A, B, C],
    );
    let put = |log: &Log, by: Mac, at: u64, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, "b.key", value),
        )
        .unwrap()
    };
    let behind = whole.clone();
    let x = one(put(&whole, B, T - 4 * DAY_MS, b"bee"), B);
    let x = hold(&mut whole, x);

    // A's view: `x` as A's own record at A's next sequence.
    let next = draft(&behind, A, T - 4 * DAY_MS, Body::Name { name: "x".into() });
    let mut forged = whole
        .stub_for(x.id(), &BTreeMap::new())
        .expect("a stub of x");
    forged.actor = A.id();
    forged.sequence = next.sequence;
    forged.previous = next.previous;
    let mut fake = behind
        .with_stubs(&BTreeMap::from([(*x.id(), forged)]))
        .unwrap();
    let h = two(
        put(&fake, A, T - 3 * DAY_MS, b"aye"),
        A,
        Some(&Statement {
            supersedes: vec![*x.id()],
            carries: None,
        }),
    );
    assert_eq!(h.previous(), Some(x.id()));
    assert!(
        whole.clone().insert(h.clone()).is_err(),
        "the whole log refuses h"
    );
    let h = hold(&mut fake, h);
    let plan = checkpoint::plan_at(&fake, T, &[*h.id()]).unwrap();
    assert_eq!(plan.dropped, vec![*x.id()]);
    share_at(&mut fake, A, &plan.body, T + MIN_AGE_MS + HOUR);
    let judgment = checkpoint::judge(&fake, &plan.body).unwrap();
    assert!(judgment.is_final && judgment.recent.len() == 1);

    let held: BTreeSet<RecordId> = behind.records().map(|r| *r.id()).collect();
    let records: Vec<Record> = fake
        .records()
        .filter(|r| !held.contains(r.id()))
        .cloned()
        .collect();
    let served = Served {
        network: *fake.network(),
        roots: vec![checkpoint_id(&plan.body).unwrap()],
        stubs: plan.stubs.clone(),
    };
    let receiver = store_of(behind);
    let mut taking = receiver.clone();
    let takes = taking.ingest_served(records, Some(&served)).is_ok();
    if takes {
        let log = taking.log().unwrap();
        assert!(log.is_dropped(x.id()));
        assert_eq!(log.rooting().unwrap().stubs[x.id()].actor, A.id());
    } else {
        assert!(
            unchanged(&receiver, &taking),
            "a refused take takes nothing"
        );
    }
    assert!(!takes);
}

/// A Mac behind the cut catches up from a pruned peer whatever the reply cap: a served root's
/// reply is not cut at it ([`Store::missing_served`]), since the Mac takes the root and the
/// records all or none, and a prefix capped short of the root's settling shares would be refused
/// and served again, the same, on every pull. A reply with no root is capped as before.
///
/// NEGATIVE CONTROL: `Mutant::CappedServed` (the reply cut at the cap, its frame counted first, as
/// the first cut of this slice served it), run from the test's first line and seen to fail with
/// ``assertion failed: catches_up(cap)``: with a cap of one byte every pull refuses "a served root
/// has no share among the records", and one byte short of the whole reply every pull refuses "a
/// served root is not final by its shares", for ever.
#[test]
fn vault_checkpoint_a_capped_pull_behind_the_cut_catches_up() {
    let catches_up = |cap: usize| {
        let world = behind();
        let peer = store_of(world.peer.clone());
        let mut receiver = store_of(world.behind.clone());
        for _ in 0..4 {
            let missing = peer.missing_served(&receiver.have(), cap);
            let bytes = read_reply(&missing).unwrap();
            let (records, served) =
                decode_reply(&bytes, MAX_HELD_RECORDS, Reading::Follow).unwrap();
            if receiver.ingest_served(records, served.as_ref()).is_err() {
                return false;
            }
            if holding(receiver.log().unwrap()) == holding(&world.peer) {
                return true;
            }
        }
        false
    };
    let world = behind();
    let whole_reply = read_reply(
        &store_of(world.peer.clone()).missing_served(&store_of(world.behind).have(), REPLY_CAP),
    )
    .unwrap()
    .len();
    for cap in [1, whole_reply - 1, REPLY_CAP] {
        assert!(catches_up(cap), "cap {cap}");
    }

    MUTANT.set(Some(Mutant::CappedServed));
    let capped = (catches_up(1), catches_up(whole_reply - 1));
    MUTANT.set(None);
    assert_eq!(capped, (false, false), "CappedServed livelocks");
}

/// A pull whose `have` names a record a root of the store dropped is not asked to post it back
/// ([`Store::lacks_any`]): a whole Mac's `have` names records behind the cut (a sleeper's, one
/// step back from its newest), and a post of a dropped record is refused as known, so the ask
/// would come again on every pull. A record the store neither holds nor dropped is asked for.
///
/// NEGATIVE CONTROL: `Mutant::HeldHave` (lacked where the store does not hold it, dropped or not,
/// as the server read it), run from the test's first line and seen to fail with ``assertion
/// failed: !rooted.lacks_any(&[world.dropped])``.
#[test]
fn vault_checkpoint_a_pull_naming_a_dropped_record_is_not_asked_to_post_it() {
    let world = behind();
    let rooted = store_of(world.peer.clone());
    assert!(rooted.lacks_any(&[[0xEE; 32]]));
    assert!(!rooted.lacks_any(&[]));
    assert!(!rooted.lacks_any(&[world.dropped]));
    assert!(!rooted.lacks_any(&store_of(world.whole).have()));

    MUTANT.set(Some(Mutant::HeldHave));
    let lacked = rooted.lacks_any(&[world.dropped]);
    MUTANT.set(None);
    assert!(lacked, "HeldHave asks for the dropped record");
}

/// A served frame holds as many stubs as its bytes do: stubs count against a log's bytes and no
/// count, so a log that dropped more records than it may hold whole serves them all, and a frame
/// counting more stubs than its bytes can hold is refused before any is read.
///
/// NEGATIVE CONTROL: [`Served::decode`] refusing a frame of more than [`MAX_HELD_RECORDS`] stubs,
/// as the first cut of this slice did, seen to fail with ``called `Result::unwrap()` on an `Err`
/// value: Error("a served frame holds more stubs than a log")``.
#[test]
fn vault_checkpoint_a_served_frame_holds_more_stubs_than_a_log_holds_records() {
    let world = behind();
    let mut served = world.peer.served().unwrap();
    let stub = served.stubs.values().next().unwrap().clone();
    for i in 0..=MAX_HELD_RECORDS {
        let mut id = [0xAB; 32];
        id[..8].copy_from_slice(&(i as u64).to_be_bytes());
        served.stubs.insert(id, stub.clone());
    }
    let bytes = served.encode();
    assert_eq!(
        Served::decode(&bytes, Reading::Follow).unwrap().stubs.len(),
        served.stubs.len()
    );
    let count_at = SERVED_MAGIC.len() + 32 + 2 + served.roots.len() * 32;
    let mut overcounted = bytes;
    overcounted[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(Served::decode(&overcounted, Reading::Follow).is_err());
}

/// A vault of `seats` in which `live` alone signed in the month before day `T`: A writes `k`
/// (version 1 where `v1_first`, else version 2), deletes it, deletes it again over that
/// `Delete`, and once more over both, each `Delete` version 2 and naming what it replaces, and
/// each other live seat writes `b.key` twice in version 2, the second superseding the first. The
/// checkpoint at `T` drops each seat's superseded ops, A's middle `Delete` among them, whose stub
/// carries A's first `Delete` (`rep`); every live seat shares it, so it is final by its recent
/// seats, which are `live`. The peer is pruned there; `behind` holds the genesis and the epoch.
struct Minority {
    whole: Log,
    peer: Log,
    behind: Log,
    body: Body,
    plan: checkpoint::Plan,
    /// A's first `Put`, its first `Delete` and its middle one.
    p: RecordId,
    d0: RecordId,
    d1: RecordId,
    live: Vec<Mac>,
}

fn minority(seats: &[Mac], live: &[Mac], v1_first: bool) -> Minority {
    let mut log = found(seats);
    let t0 = T - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, seats);
    let behind = log.clone();
    let put = |log: &Log, by: Mac, at: u64, name: &str, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, name, value),
        )
        .unwrap()
    };
    let delete = |log: &Log, at: u64| log.draft_delete(&A.id(), at, e, &key, "k").unwrap();
    let p = put(&log, A, t0 + HOUR, "k", b"v");
    let p = if v1_first {
        one(p, A)
    } else {
        two(p, A, Some(&Statement::default()))
    };
    let p = hold(&mut log, p);
    let d0 = two(
        delete(&log, t0 + 2 * HOUR),
        A,
        Some(&supersedes(&[&p], None)),
    );
    let d0 = hold(&mut log, d0);
    let d1 = two(
        delete(&log, t0 + 3 * HOUR),
        A,
        Some(&supersedes(&[&p, &d0], None)),
    );
    let d1 = hold(&mut log, d1);
    let d2 = two(
        delete(&log, t0 + 4 * HOUR),
        A,
        Some(&supersedes(&[&p, &d0, &d1], None)),
    );
    hold(&mut log, d2);
    for (k, &mac) in live.iter().filter(|mac| **mac != A).enumerate() {
        let at = t0 + (5 + 2 * k as u64) * HOUR;
        let q1 = two(
            put(&log, mac, at, "b.key", b"one"),
            mac,
            Some(&Statement::default()),
        );
        let q1 = hold(&mut log, q1);
        let q2 = two(
            put(&log, mac, at + HOUR, "b.key", b"two"),
            mac,
            Some(&supersedes(&[&q1], None)),
        );
        hold(&mut log, q2);
    }
    let plan = propose(&log, NOW).expect("a proposal");
    assert!(plan.dropped.contains(p.id()) && plan.dropped.contains(d1.id()));
    for &mac in live {
        share_at(&mut log, mac, &plan.body, T + MIN_AGE_MS + HOUR);
    }
    let peer = log.adopt(&plan.body).unwrap();
    assert_eq!(peer.stub(d1.id()).unwrap().carries, Some(*d0.id()));
    Minority {
        whole: log,
        peer,
        behind,
        body: plan.body.clone(),
        plan,
        p: *p.id(),
        d0: *d0.id(),
        d1: *d1.id(),
        live: live.to_vec(),
    }
}

/// The minority side of a split: A alone of A, B and C heard in the month before `T`.
fn split_side(v1_first: bool) -> Minority {
    minority(&[A, B, C], &[A], v1_first)
}

/// Dead seats outnumbering live ones: A and B of five seats.
fn dead_seats() -> Minority {
    minority(&[A, B, C, Mac(4), Mac(5)], &[A, B], false)
}

/// Whether the Mac `receiver` takes `peer`'s reply, forged as `forge` says; a refused take takes
/// nothing.
fn takes_reply(
    peer: &Log,
    receiver: &Log,
    forge: impl FnOnce(&mut Vec<Record>, &mut Served),
) -> bool {
    let peer = store_of(peer.clone());
    let receiver = store_of(receiver.clone());
    let missing = peer.missing_served(&receiver.have(), REPLY_CAP);
    let mut records = missing.records;
    let mut served = missing.served.expect("the pruned peer serves its root");
    forge(&mut records, &mut served);
    let mut taking = receiver.clone();
    let taken = taking.ingest_served(records, Some(&served)).is_ok();
    assert!(
        taken || unchanged(&receiver, &taking),
        "a refused take takes nothing"
    );
    taken
}

/// The reply a forger that holds every live seat's key would send the Mac behind the minority
/// cut with the stubs `forge` alters: the checkpoint the forged stubs compute at the root's
/// heads, shared by every live seat in place of the root. `None` where they compute none.
fn resigned(
    world: &Minority,
    forge: impl FnOnce(&mut BTreeMap<RecordId, Stub>),
) -> Option<(Vec<Record>, Served)> {
    let mut stubs = world.plan.stubs.clone();
    forge(&mut stubs);
    let mut fake = world.behind.with_stubs(&stubs).ok()?;
    let root = checkpoint_id(&world.body);
    for record in world.peer.records() {
        if checkpoint_id(record.body()) != root && !fake.knows(record.id()) {
            fake.insert(record.clone()).ok()?;
        }
    }
    let Body::Checkpoint { cut_ms, heads, .. } = &world.body else {
        return None;
    };
    let plan = checkpoint::plan_at(&fake, *cut_ms, heads).ok()?;
    for &mac in &world.live {
        share_at(&mut fake, mac, &plan.body, T + MIN_AGE_MS + 2 * HOUR);
    }
    let held: BTreeSet<RecordId> = world.behind.records().map(|r| *r.id()).collect();
    let records = fake
        .records()
        .filter(|r| !held.contains(r.id()))
        .cloned()
        .collect();
    let served = Served {
        network: *fake.network(),
        roots: vec![checkpoint_id(&plan.body)?],
        stubs: plan.stubs,
    };
    Some((records, served))
}

/// Whether the Mac behind the minority cut takes the forger's reply (`resigned`).
fn takes_resigned(world: &Minority, reply: Option<(Vec<Record>, Served)>) -> bool {
    let Some((records, served)) = reply else {
        return false;
    };
    let receiver = store_of(world.behind.clone());
    let mut taking = receiver.clone();
    let taken = taking.ingest_served(records, Some(&served)).is_ok();
    assert!(
        taken || unchanged(&receiver, &taking),
        "a refused take takes nothing"
    );
    taken
}

/// A Mac behind a cut final on the minority side of a split, or in a vault whose dead seats
/// outnumber its live ones, catches up from a peer pruned there: every record the cut dropped is
/// version 2, so every stub it reads checks itself against its id (its frame, signed by its
/// signer), and the recent seats it reads off them are its signers' own; it takes the root on its
/// recent seats' shares, as a Mac that holds the cut does, prunes there, and evaluates as the
/// whole log. On the split, the Mac also holds the other side's records (B's and C's after the
/// split), and evaluates as the union. A cut whose prune drops a version-1 record (A's first
/// `Put`) keeps FL-148c's rule: its stub's fields stand on the shares' word, so the root also
/// needs the shares of more than half of all seats, and A's one of three is refused.
///
/// NEGATIVE CONTROL: `Mutant::AllSeats` (every root read through stubs needs more than half of
/// all seats, as FL-148c's rule), run from the test's first line and seen to fail with
/// ``assertion failed: catches_up(&split_side(false), true)``: the minority side's root, on
/// A's one share of three seats, is refused.
#[test]
fn vault_checkpoint_a_mac_behind_a_minority_cut_catches_up_from_self_checking_stubs() {
    let catches_up = |world: &Minority, split: bool| {
        let mut receiver = world.behind.clone();
        let mut union = world.whole.clone();
        if split {
            for mac in [B, C] {
                let record = one(draft(&receiver, mac, T + DAY_MS, name("other")), mac);
                hold(&mut receiver, record.clone());
                hold(&mut union, record);
            }
        }
        let peer = store_of(world.peer.clone());
        let mut store = store_of(receiver);
        let (records, served, _) = reply(&peer, &store);
        let before = store.clone();
        if store.ingest_served(records, served.as_ref()).is_err() {
            assert!(unchanged(&before, &store), "a refused take takes nothing");
            return false;
        }
        let log = store.log().unwrap();
        assert_eq!(log.newest_root().map(|r| r.id), checkpoint_id(&world.body));
        for id in &world.plan.dropped {
            assert!(log.is_dropped(id) && log.get(id).is_none());
        }
        assert_eq!(evaluated(log), evaluated(&union));
        true
    };
    for world in [split_side(false), dead_seats()] {
        assert!(
            world
                .peer
                .served()
                .unwrap()
                .stubs
                .values()
                .all(|stub| stub.frame.is_some()),
            "every stub is a version-2 record's"
        );
    }
    assert!(catches_up(&split_side(false), true));
    assert!(catches_up(&dead_seats(), false));

    let v1 = split_side(true);
    assert!(
        v1.peer.served().unwrap().stubs[&v1.p].frame.is_none(),
        "a version-1 record's stub has no frame"
    );
    assert!(
        !catches_up(&v1, false),
        "a version-1 drop keeps all seats' rule"
    );

    MUTANT.set(Some(Mutant::AllSeats));
    let (split, dead) = (
        catches_up(&split_side(false), true),
        catches_up(&dead_seats(), false),
    );
    MUTANT.set(None);
    assert!(!split && !dead, "AllSeats takes no minority root");
}

/// A forged version-2 record's stub is refused, and nothing taken, though its root is final by
/// its recent seats alone (A, the one live seat of the minority side): each field of A's dropped
/// `Put` altered (its signer, sequence, previous record, clock, kind, what it carries, its epoch,
/// what it names as replaced, its parents) and A's middle `Delete`'s `carries`, as served and as
/// a forger holding A's key re-signs them (the checkpoint the forged stubs compute, shared by A);
/// a frame of another id; a version-2 record's stub served as version 1, a share's among them,
/// and a version-1 record's served with a frame; a frame altered on the wire; and B's version-2
/// `Put` relabelled
/// as A's own and placed behind A's next record, which makes A the cut's one recent seat.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail:
/// - `Mutant::UncheckedFrames` (a stub's fields not checked against its frame), with ``a served
///   stub's sequence forged``: the digest hashes a version-2 stub's frame, not its fields, so a
///   field altered beside an honest frame is taken unless it is checked against it.
/// - `Mutant::DeleteRepsRead` (a `Delete`'s `carries` read as served), with ``a re-signed
///   stub's a Delete's rep forged``: the digest binds it, so only a forger's re-signed reply
///   passes it.
/// - [`Stub::checked`] reading a frameless share's stub as a version-1 record's, with ``a
///   share's stub with no frame``.
#[test]
fn vault_checkpoint_a_forged_version_two_stub_is_refused() {
    let world = split_side(false);
    let (p, d0, d1) = (world.p, world.d0, world.d1);
    assert!(takes_reply(&world.peer, &world.behind, |_, _| {}));
    assert!(takes_resigned(&world, resigned(&world, |_| {})));

    type Forge = Box<dyn Fn(&mut Stub)>;
    let fields: Vec<(&str, RecordId, Forge)> = vec![
        ("signer", p, Box::new(|s| s.actor = B.id())),
        ("sequence", p, Box::new(|s| s.sequence += 1)),
        ("previous", p, Box::new(move |s| s.previous = Some(d0))),
        ("clock", p, Box::new(|s| s.authored_ms += HOUR)),
        ("kind", p, Box::new(|s| s.kind = Kind::Delete)),
        ("carries", p, Box::new(move |s| s.carries = Some(d0))),
        ("epoch", p, Box::new(|s| s.epoch = Some([0x22; 32]))),
        ("supersedes", p, Box::new(move |s| s.supersedes = vec![d0])),
        ("parents", p, Box::new(move |s| s.parents = vec![d0])),
        ("a Delete's rep", d1, Box::new(move |s| s.carries = Some(p))),
    ];
    for (field, id, forge) in &fields {
        assert!(
            !takes_reply(&world.peer, &world.behind, |_, served| {
                forge(served.stubs.get_mut(id).unwrap());
            }),
            "a served stub's {field} forged"
        );
        assert!(
            !takes_resigned(
                &world,
                resigned(&world, |stubs| forge(stubs.get_mut(id).unwrap()))
            ),
            "a re-signed stub's {field} forged"
        );
    }
    let frame_of = |id: RecordId| world.plan.stubs[&id].frame.clone();
    assert!(
        !takes_reply(&world.peer, &world.behind, |_, served| {
            served.stubs.get_mut(&p).unwrap().frame = frame_of(d0);
        }),
        "a frame of another id"
    );
    assert!(
        !takes_reply(&world.peer, &world.behind, |_, served| {
            served.stubs.get_mut(&p).unwrap().frame = None;
        }),
        "a version-2 record's stub served as version 1"
    );
    assert!(
        !takes_resigned(
            &world,
            resigned(&world, |stubs| stubs.get_mut(&p).unwrap().frame = None)
        ),
        "a version-2 record's stub re-signed as version 1"
    );
    let v1 = split_side(true);
    assert!(
        !takes_reply(&v1.peer, &v1.behind, |_, served| {
            served.stubs.get_mut(&v1.p).unwrap().frame = v1.plan.stubs[&v1.d0].frame.clone();
        }),
        "a version-1 record's stub served with a frame"
    );
    // A share's stub (A's share of the root, as a later prune drops it) served with no frame,
    // as though a version-1 record's: no version-1 record is a share.
    let share = world
        .whole
        .records()
        .find(|r| checkpoint_id(r.body()) == checkpoint_id(&world.body))
        .map(|r| *r.id())
        .unwrap();
    let share_stub = world.whole.stub_for(&share, &BTreeMap::new()).unwrap();
    let network = *world.whole.network();
    assert!(share_stub.checked(&share, &network).is_ok());
    let unframed = Stub {
        frame: None,
        ..share_stub
    };
    assert!(
        unframed.checked(&share, &network).is_err(),
        "a share's stub with no frame"
    );
    // On the wire: the frame reads back as it was written, and one altered byte of a frame
    // (the first stub's signer) reads as a record of another id that no signer signed, refused
    // where a receiver reads its stub (the frame's signature is verified there, once).
    let served = world.peer.served().unwrap();
    let bytes = served.encode();
    assert_eq!(bytes.len(), served.wire_len());
    assert_eq!(Served::decode(&bytes, Reading::Follow).unwrap(), served);
    let first = SERVED_MAGIC.len() + 32 + 2 + served.roots.len() * 32 + 4;
    let mut altered = bytes;
    altered[first + 1 + 1 + 3] ^= 1;
    assert!(
        Served::decode(&altered, Reading::Follow).map_or(true, |read| read
            .stubs
            .iter()
            .any(|(id, stub)| stub.checked(id, &network).is_err())),
        "an altered frame is refused"
    );

    let clock = |stubs: &mut BTreeMap<RecordId, Stub>| {
        stubs.get_mut(&p).unwrap().authored_ms += HOUR;
    };
    let rep = |stubs: &mut BTreeMap<RecordId, Stub>| {
        stubs.get_mut(&d1).unwrap().carries = Some(p);
    };
    assert!(!relabelled());

    MUTANT.set(Some(Mutant::UncheckedFrames));
    let (unchecked, relabel) = (
        takes_resigned(&world, resigned(&world, clock)),
        relabelled(),
    );
    MUTANT.set(Some(Mutant::DeleteRepsRead));
    let read = takes_resigned(&world, resigned(&world, rep));
    MUTANT.set(None);
    assert!(unchecked, "UncheckedFrames takes a forged clock");
    assert!(relabel, "UncheckedFrames takes a relabelled stub");
    assert!(read, "DeleteRepsRead takes a forged rep");
}

/// [`vault_checkpoint_one_seat_cannot_certify_the_stubs_it_serves`] with B's `Put` `x` version
/// 2: A serves `x`'s stub relabelled as its own (its frame B's), placed behind A's next record,
/// and the checkpoint that drops it, shared by A alone, the one recent seat of a cut read
/// through that stub. Whether the Mac behind the cut takes it.
fn relabelled() -> bool {
    let mut whole = found(&[A, B, C]);
    let (e, key) = epoch(
        &mut whole,
        A,
        T - 5 * DAY_MS,
        EpochAudience::Everyone,
        1,
        &[A, B, C],
    );
    let put = |log: &Log, by: Mac, at: u64, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, "b.key", value),
        )
        .unwrap()
    };
    let behind = whole.clone();
    let x = two(
        put(&whole, B, T - 4 * DAY_MS, b"bee"),
        B,
        Some(&Statement::default()),
    );
    let x = hold(&mut whole, x);
    let next = draft(&behind, A, T - 4 * DAY_MS, Body::Name { name: "x".into() });
    let mut forged = whole
        .stub_for(x.id(), &BTreeMap::new())
        .expect("a stub of x");
    assert!(forged.frame.is_some());
    forged.actor = A.id();
    forged.sequence = next.sequence;
    forged.previous = next.previous;
    let mut fake = behind
        .with_stubs(&BTreeMap::from([(*x.id(), forged)]))
        .unwrap();
    let h = two(
        put(&fake, A, T - 3 * DAY_MS, b"aye"),
        A,
        Some(&Statement {
            supersedes: vec![*x.id()],
            carries: None,
        }),
    );
    let h = hold(&mut fake, h);
    let plan = checkpoint::plan_at(&fake, T, &[*h.id()]).unwrap();
    assert_eq!(plan.dropped, vec![*x.id()]);
    share_at(&mut fake, A, &plan.body, T + MIN_AGE_MS + HOUR);
    let held: BTreeSet<RecordId> = behind.records().map(|r| *r.id()).collect();
    let records: Vec<Record> = fake
        .records()
        .filter(|r| !held.contains(r.id()))
        .cloned()
        .collect();
    let served = Served {
        network: *fake.network(),
        roots: vec![checkpoint_id(&plan.body).unwrap()],
        stubs: plan.stubs.clone(),
    };
    let receiver = store_of(behind);
    let mut taking = receiver.clone();
    let takes = taking.ingest_served(records, Some(&served)).is_ok();
    assert!(takes || unchanged(&receiver, &taking));
    takes
}

/// A vault pruned twice ([`twice_pruned`]).
struct Twice {
    /// Every Mac's records, every share among them.
    whole: Log,
    /// The first checkpoint and the next, whose cut nests the first's.
    first: Body,
    next: Body,
    /// C's reseal after the first cut, of the reseal the first dropped.
    carry: RecordId,
    /// A's first `Put`, which the first drops and the next keeps (the dropped reseal carries it).
    first_put: RecordId,
    /// A's record after the first cut.
    after_a: RecordId,
    /// When all three shared the next checkpoint.
    shared_next: u64,
    /// A Mac that holds the genesis, the epoch and its wraps: behind both cuts.
    behind: Log,
}

/// [`carry_of_dropped`]'s vault run on. Its first checkpoint drops A's reseal of `a.key` and the
/// first `Put` it carries, and C's reseal after that cut carries the dropped reseal; A, B and C
/// each write after it, and the next checkpoint (at `T` + 5 days, before the first's shares)
/// keeps both in the whole log. The first checkpoint is at `first_day` (the day it was proposed
/// at where `None`) with the proposal's heads; A and B, its recent seats, share it a fortnight
/// after `T`, and all three share the next a fortnight after its day.
fn twice_pruned(first_day: Option<u64>) -> Twice {
    let (mut whole, plan, carry, first_put) = carry_of_dropped(false);
    let Body::Checkpoint { cut_ms, heads, .. } = &plan.body else {
        panic!("a checkpoint");
    };
    let first = checkpoint::plan_at(&whole, first_day.unwrap_or(*cut_ms), heads)
        .unwrap()
        .body;
    let mut after_a = None;
    for (at, mac) in [(2, A), (3, B), (4, C)] {
        let record = one(draft(&whole, mac, T + at * DAY_MS, name("after")), mac);
        let record = hold(&mut whole, record);
        if mac == A {
            after_a = Some(*record.id());
        }
    }
    let shared = T + MIN_AGE_MS + HOUR;
    share_at(&mut whole, A, &first, shared);
    share_at(&mut whole, B, &first, shared);
    let now = T + 5 * DAY_MS + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;
    let next = propose(&whole, now).expect("a next proposal");
    assert!(
        !next.dropped.contains(&first_put),
        "the whole log's next prune keeps what the first dropped"
    );
    for mac in [A, B, C] {
        share_at(&mut whole, mac, &next.body, now + HOUR);
    }
    let mut behind = Log::new(whole.genesis().clone()).unwrap();
    for record in whole
        .records()
        .filter(|r| matches!(r.kind(), Kind::Epoch | Kind::Wrap))
    {
        hold(&mut behind, record.clone());
    }
    Twice {
        whole,
        first,
        next: next.body,
        carry,
        first_put,
        after_a: after_a.expect("A wrote after the cut"),
        shared_next: now + HOUR,
        behind,
    }
}

impl Twice {
    /// The whole log pruned at the first checkpoint and then the next.
    fn pruned(&self) -> Log {
        self.whole
            .adopt(&self.first)
            .unwrap()
            .adopt(&self.next)
            .unwrap()
    }

    /// Both checkpoints' ids, ascending, as a frame names them.
    fn roots(&self) -> Vec<RecordId> {
        let mut roots = vec![
            checkpoint_id(&self.first).unwrap(),
            checkpoint_id(&self.next).unwrap(),
        ];
        roots.sort_unstable();
        roots
    }
}

/// The store of a Mac that holds `behind` after it takes `peer`'s reply to its pull.
pub(in crate::membership_log) fn catch_up(peer: &Log, behind: &Log) -> Result<Store> {
    let peer = store_of(peer.clone());
    let mut receiver = store_of(behind.clone());
    let (records, served, _) = reply(&peer, &receiver);
    receiver
        .ingest_served(records, served.as_ref())
        .map(|_| receiver)
}

/// A log pruned twice serves its roots ([`twice_pruned`]): the log pruned at both keeps A's first
/// `Put` and the dropped reseal as stubs no current root dropped. It keeps the first root beside
/// the second, whose cut nests it, and serves both; a Mac behind both cuts takes them as the log
/// did, the first and then the second, reads those stubs by the first root's digest, holds what
/// the log holds (its roots and the earlier roots beside them) and evaluates as the whole log,
/// and then serves both roots itself, from which a joiner catches up. A reply that turns a
/// record no root dropped into a stub (C's reseal, which the second cut keeps whole) is refused:
/// only a root's dropped set makes a stub.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail:
/// - `rooted::Mutant::NoEarlierRoots` (a prune keeps no root it nests, as FL-148c did), with
///   ``assertion `left == right` failed``, `left` `[]`: no earlier root kept (and, built under
///   it in the test, a log that serves no root).
/// - `Mutant::Uncovered` (a stub no root dropped is read too), with ``assertion failed:
///   !suppressed``: the reply that stubs C's reseal is taken.
#[test]
fn vault_checkpoint_a_twice_pruned_log_serves_its_roots() {
    let world = twice_pruned(None);
    let twice = world.pruned();
    let rooting = twice.rooting().unwrap();
    assert_eq!(
        rooting.roots.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![checkpoint_id(&world.next).unwrap()]
    );
    assert_eq!(
        rooting.earlier.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![checkpoint_id(&world.first).unwrap()]
    );
    assert!(twice.is_dropped(&world.first_put));
    assert_eq!(
        twice.served().map(|s| s.roots),
        Some(world.roots()),
        "a twice-pruned log serves its roots"
    );

    let receiver = catch_up(&twice, &world.behind).unwrap();
    let log = receiver.log().unwrap();
    assert_eq!(holding(log), holding(&twice));
    assert_eq!(evaluated(log), evaluated(&world.whole));
    assert_eq!(log.served().map(|s| s.roots), Some(world.roots()));
    let joiner = catch_up(log, &world.behind).unwrap();
    assert_eq!(holding(joiner.log().unwrap()), holding(&twice));

    let stub_carry = |records: &mut Vec<Record>, served: &mut Served| {
        records.retain(|r| r.id() != &world.carry);
        let stub = world
            .whole
            .stub_for(&world.carry, &BTreeMap::new())
            .expect("a stub of C's reseal");
        served.stubs.insert(world.carry, stub);
    };
    let suppressed = takes_reply(&twice, &world.behind, stub_carry);
    assert!(!suppressed);

    MUTANT.set(Some(Mutant::Uncovered));
    let uncovered = takes_reply(&twice, &world.behind, stub_carry);
    MUTANT.set(None);
    crate::membership_log::rooted::MUTANT
        .set(Some(crate::membership_log::rooted::Mutant::NoEarlierRoots));
    let forgetful = world.pruned().served();
    crate::membership_log::rooted::MUTANT.set(None);
    assert!(uncovered, "Uncovered takes a stub no root dropped");
    assert!(forgetful.is_none(), "NoEarlierRoots serves no root");
}

/// A Mac takes served roots in any order: a cut's day says nothing of what it nests. With the
/// first checkpoint of [`twice_pruned`] at `T` + 6 days (its heads the same), the next, at `T` +
/// 5 days, nests it, so a Mac behind both cuts, which takes them by their days, prunes at the
/// next first and then at the first, which it nests beside the next, as the log pruned at the
/// first and then at the next keeps it: the Mac holds what that log holds and evaluates as the
/// whole log.
///
/// NEGATIVE CONTROL: `rooted::Mutant::NestedAsRoot` (a prune at a root the log's roots nest
/// keeps it among them), run from the test's first line and seen to fail with ``the Mac holds
/// the roots its peer holds``: the Mac's roots are both checkpoints, the second nesting the
/// first, and nothing is kept beside them, where its peer's root is the next and the first is
/// kept beside it.
#[test]
fn vault_checkpoint_served_roots_are_taken_in_any_order() {
    let world = twice_pruned(Some(T + 6 * DAY_MS));
    let day = |body: &Body| match body {
        Body::Checkpoint { cut_ms, .. } => *cut_ms,
        _ => panic!("a checkpoint"),
    };
    assert!(day(&world.first) > day(&world.next));
    let twice = world.pruned();
    let rooting = twice.rooting().unwrap();
    assert_eq!(
        rooting.roots.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![checkpoint_id(&world.next).unwrap()]
    );
    assert_eq!(twice.served().map(|s| s.roots), Some(world.roots()));
    let taken = |world: &Twice| {
        catch_up(&twice, &world.behind)
            .map(|receiver| receiver.log().map(|log| (holding(log), evaluated(log))))
    };
    let (held, read) = taken(&world).unwrap().unwrap();
    crate::membership_log::rooted::MUTANT
        .set(Some(crate::membership_log::rooted::Mutant::NestedAsRoot));
    let nested_as_root = taken(&world);
    crate::membership_log::rooted::MUTANT.set(None);
    assert_eq!(
        held,
        holding(&twice),
        "the Mac holds the roots its peer holds"
    );
    assert_eq!(read, evaluated(&world.whole));
    assert!(
        nested_as_root.is_err()
            || nested_as_root.is_ok_and(|taken| taken.is_none_or(|(h, _)| h != holding(&twice))),
        "NestedAsRoot holds otherwise"
    );
}

/// An earlier root whose finality a later `Remove` undid is still taken, where a served root
/// that nests it is final. In [`twice_pruned`], B removes A at A's record after the first cut,
/// written after every share: A's shares count no more, so the first checkpoint, which A and B
/// alone of its recent seats shared, is no longer final, though it stays settled, as finality
/// never was for good; the next, which B and C also shared, stays final. The log pruned at both
/// keeps and serves the first beside the next, and a Mac behind both cuts takes the first as its
/// peer holds it, settled (the next nests it), holds what that log holds and evaluates as the
/// whole log.
///
/// NEGATIVE CONTROL: `Mutant::NestedFinal` (a nested root judged as the newest is, final by the
/// shares that still count), run from the test's first line and seen to fail with ``called
/// `Result::unwrap()` on an `Err` value: Error("a served root is not final by its shares",
/// Other)``: the Mac refuses the reply, as it would every reply of that peer until a later
/// prune's cut holds the first settled.
#[test]
fn vault_checkpoint_an_earlier_root_whose_finality_lapsed_is_taken() {
    let world = twice_pruned(None);
    let mut whole = world.whole.clone();
    let mut twice = world.pruned();
    let remove = one(
        draft(
            &whole,
            B,
            world.shared_next + HOUR,
            Body::Remove {
                member: A.id(),
                cutoff: world.after_a,
            },
        ),
        B,
    );
    hold(&mut whole, remove.clone());
    hold(&mut twice, remove);
    let first = checkpoint::judge(&whole, &world.first).unwrap();
    assert!(!first.is_final && first.is_settled, "{first:?}");
    assert!(checkpoint::judge(&whole, &world.next).unwrap().is_final);
    assert_eq!(twice.served().map(|s| s.roots), Some(world.roots()));

    let receiver = catch_up(&twice, &world.behind).unwrap();
    let log = receiver.log().unwrap();
    assert_eq!(holding(log), holding(&twice));
    assert_eq!(evaluated(log), evaluated(&whole));

    MUTANT.set(Some(Mutant::NestedFinal));
    let refused = catch_up(&twice, &world.behind).err().map(|e| e.to_string());
    MUTANT.set(None);
    assert_eq!(
        refused.as_deref(),
        Some("a served root is not final by its shares"),
        "NestedFinal refuses the lapsed earlier root"
    );
}

/// A reader of format 1 refuses a served frame that holds a version-2 record's stub, as it refuses
/// that record whole ([`decode_records`]): what a format-1 node takes, and could serve on, is
/// decided where the bytes are read (tracker FL-148e). A reader of format 2 reads it.
///
/// NEGATIVE CONTROL: `Mutant::StubsFollow` (a frame's stubs read as [`Reading::Follow`] reads
/// them, as this slice's first cut read them), run from the test's first line and seen to fail
/// with ``a format-1 reader refuses a version-2 record's stub``.
#[test]
fn vault_checkpoint_a_format_one_reader_refuses_version_two_stubs() {
    let world = split_side(false);
    let served = world.peer.served().unwrap();
    assert!(served.stubs.values().all(|stub| stub.frame.is_some()));
    let bytes = served.encode();
    let reply = [read_records(&[]).unwrap(), bytes.clone()].concat();
    let two = Reading::Format(crate::crossing::FORMAT_TWO);
    assert_eq!(Served::decode(&bytes, two).unwrap(), served);
    assert!(decode_reply(&reply, MAX_HELD_RECORDS, two).is_ok());
    let record = world.whole.get(&world.p).unwrap();
    assert!(decode_records(&read_records(&[record.clone()]).unwrap()).is_err());

    let refused = |reading: Reading| {
        Served::decode(&bytes, reading).is_err()
            && decode_reply(&reply, MAX_HELD_RECORDS, reading).is_err()
    };
    let one = refused(Reading::ONE);
    MUTANT.set(Some(Mutant::StubsFollow));
    let follows = refused(Reading::ONE);
    MUTANT.set(None);
    assert!(one, "a format-1 reader refuses a version-2 record's stub");
    assert!(
        !follows,
        "StubsFollow reads a version-2 record's stub at format 1"
    );
}

/// The law [`vault_checkpoint_twice_pruned_logs_serve_their_roots_over_random_dags`] asserts of
/// one vault (`seed` and `case` name it): `whole` pruned at `first` and then at `next`, whose cut nests the
/// first's, serves its roots, and a Mac behind both cuts (the genesis alone) and a Mac whole at
/// the first cut each catch up from it, hold what it holds and evaluate as `whole`. Whether the
/// twice-pruned log keeps the first root beside the second; `None` where `first` is not final in
/// `whole` or `next` is not.
fn serves_twice_pruned(
    seed: u64,
    case: &str,
    whole: &Log,
    first: &Body,
    next: &Body,
) -> Option<bool> {
    let (Ok(once), Ok(nested)) = (whole.adopt(first), checkpoint::judge(whole, next)) else {
        return None;
    };
    if !nested.is_final {
        return None;
    }
    let twice = once.adopt(next).unwrap();
    let earlier = !twice.rooting().unwrap().earlier.is_empty();
    assert!(
        twice.served().is_some(),
        "seed {seed} ({case}): a twice-pruned log serves its roots"
    );
    let Body::Checkpoint { heads, .. } = first else {
        panic!("a checkpoint");
    };
    let mut at_first = Log::new(whole.genesis().clone()).unwrap();
    for record in whole.records().skip(1) {
        if heads.contains(record.id()) || whole.reaches(heads, record.id()) {
            hold(&mut at_first, record.clone());
        }
    }
    let evaluation = Evaluation::of(whole);
    for behind in [Log::new(whole.genesis().clone()).unwrap(), at_first] {
        let receiver = catch_up(&twice, &behind)
            .unwrap_or_else(|e| panic!("seed {seed} ({case}): a Mac behind catches up: {e}"));
        let log = receiver.log().unwrap();
        assert_eq!(
            holding(log),
            holding(&twice),
            "seed {seed} ({case}): the Mac holds what its peer holds"
        );
        assert_evaluates_as(seed, case, log, &evaluation);
    }
    Some(earlier)
}

/// Over random vaults, a log pruned twice where the first checkpoint's shares follow the second
/// cut (so the second does not hold the first settled) keeps the first root beside the second
/// wherever a stub of its drops is one the second's whole log keeps, serves its roots, and a Mac
/// behind both cuts (the genesis alone) and a Mac whole at the first cut each catch up from it:
/// each holds what the log holds and evaluates as the whole log ([`serves_twice_pruned`]).
/// (`World::settle` signs the first's shares just after its cut, so the random-DAG prune test's
/// second cut always holds it settled and keeps no earlier root.)
///
/// Record ids hold random salts, so which seeds keep an earlier root differs from run to run
/// (over 32 seeds none did in about one run in eight, seen as ``assertion failed: earlier > 0
/// && caught_up == 2 * twice_pruned``, ``19 0 38``; over 128, about ten). So the law runs first
/// on [`twice_pruned`]'s vault, whose records are fixed and whose twice-pruned log always keeps
/// its first root beside the next: the law is never met without an earlier root, and its
/// control fails there in every run, whatever the random seeds keep. Decided over seeding the
/// salts, which come from the OS CSPRNG in the code under test: a test hook there would sit in
/// the one entropy path every sealed entry takes.
///
/// NEGATIVE CONTROL: `rooted::Mutant::NoEarlierRoots` (a prune keeps no root it nests), run from
/// the test's first line and seen to fail with ``seed 0 (twice_pruned's vault): a twice-pruned
/// log serves its roots``.
#[test]
fn vault_checkpoint_twice_pruned_logs_serve_their_roots_over_random_dags() {
    let fixed = twice_pruned(None);
    assert_eq!(
        serves_twice_pruned(
            0,
            "twice_pruned's vault",
            &fixed.whole,
            &fixed.first,
            &fixed.next
        ),
        Some(true),
        "twice_pruned's vault keeps its first root beside the next"
    );
    let (mut twice_pruned, mut earlier) = (0usize, 0usize);
    for seed in 1..=SEEDS {
        let mut world = random_vault(seed, 100, 30);
        let Some(first) = propose(&world.whole(), NOW) else {
            continue;
        };
        world.run(seed, 150);
        let now = world.clock + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;
        let Some(next) = propose(&world.whole(), now) else {
            continue;
        };
        let Body::Checkpoint {
            cut_ms: next_day, ..
        } = &next.body
        else {
            panic!("a checkpoint");
        };
        // The first checkpoint's shares follow the next cut, which so does not hold it settled.
        world.clock = world.clock.max(next_day + HOUR);
        world.settle(&first.body);
        world.settle(&next.body);
        let whole = world.whole();
        if let Some(kept) = serves_twice_pruned(seed, "random", &whole, &first.body, &next.body) {
            twice_pruned += 1;
            earlier += usize::from(kept);
        }
    }
    assert!(twice_pruned > 0, "some seed is pruned twice");
    eprintln!("pruned twice: {twice_pruned}, an earlier root kept: {earlier}");
}
