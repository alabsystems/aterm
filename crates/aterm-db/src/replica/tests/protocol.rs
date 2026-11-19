// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What each database protocol lets table repair do ([`Tables`]): `atdb/9` reconciles a table's
//! conflict proofs by range, `atdb/7` and `atdb/8` trade them as one list, and a peer on an
//! earlier protocol is refused table repair.

use super::*;
use std::os::unix::net::UnixStream;

/// A peer that offers no database protocol past `atdb/6` (aterm 0.202.0 and older) is refused
/// table repair: the round moves no record, and both sides' failure is a line that names the
/// protocol the peer offered and the builds that speak it, with the cause that says the peer's
/// protocol cannot complete it. The control repair before it still completes.
/// NEGATIVE CONTROL: the same two Macs on this build's protocol repair the table in one round.
#[test]
fn a_peer_offering_below_atdb_7_is_refused_table_repair_with_a_line_naming_its_build() {
    let records = 5;
    let p = Pair::new(false, records);
    let (older, this) = (p.endpoint(0, true).duplex(), p.endpoint(1, true));
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&older, &this, &mut ra, &mut rb);
    assert!(
        ra.actor().is_some() && rb.actor().is_some(),
        "the control repair completes"
    );
    assert!(ra.application().is_none() && rb.application().is_none());
    assert_eq!(p.count(1), 0, "no record moved");
    for repair in [&ra, &rb] {
        let failure = repair
            .application_error()
            .expect("table repair is refused on both sides");
        assert_eq!(
            failure.cause,
            aterm_types::network_state::RepairCause::NeedsUpdate,
            "{failure}"
        );
        assert_eq!(failure.text, refused_below_atdb_7("atdb/6"), "{failure}");
        assert!(
            failure.text.contains("atdb/6") && failure.text.contains("aterm 0.202.0 and older"),
            "{failure}"
        );
    }

    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    p.round(&p.endpoint(0, true), &this, &mut ra, &mut rb);
    assert!(
        ra.application_error().is_none() && rb.application_error().is_none(),
        "{:?} {:?}",
        ra.application_error(),
        rb.application_error()
    );
    assert_eq!(p.count(1), records);
}

/// `count` proof keys of distinct bindings, the `n`th's actor and binding taken from `seed` and
/// `n`: a regular (non-fork) proof, kind byte 0.
fn proof_keys(seed: u8, count: usize) -> BTreeSet<ProofKey> {
    (0..count)
        .map(|n| {
            let mut key = [0; 81];
            let digest = Sha256::digest([&[seed][..], &(n as u64).to_le_bytes()].concat());
            key[..32].copy_from_slice(&digest);
            key[32..40].copy_from_slice(&(n as u64).to_le_bytes());
            key[49..81].copy_from_slice(&digest);
            key
        })
        .collect()
}

/// Run `exchange` on both ends of a socket pair, `a` initiating: what each side learned of the
/// other's proofs and the bytes both wrote.
fn traded<T: Send>(
    a: &BTreeSet<ProofKey>,
    b: &BTreeSet<ProofKey>,
    exchange: fn(&mut Counted<UnixStream>, Side, &BTreeSet<ProofKey>) -> Option<T>,
) -> (T, T, usize) {
    let (left, right) = UnixStream::pair().unwrap();
    for stream in [&left, &right] {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
    }
    thread::scope(|threads| {
        let worker = threads.spawn(move || {
            let mut stream = Counted {
                inner: right,
                written: 0,
            };
            let learned = exchange(&mut stream, Side::Responder, b);
            (learned, stream.written)
        });
        let mut stream = Counted {
            inner: left,
            written: 0,
        };
        let learned = exchange(&mut stream, Side::Initiator, a);
        let (other, written) = worker.join().unwrap();
        (
            learned.expect("both inventories are available"),
            other.expect("both inventories are available"),
            stream.written + written,
        )
    })
}

/// Every key of `local` the peer lacks, as the round selects them batch by batch
/// ([`lacked_proofs`]), until the turn comes back round.
fn every_lacked(local: &BTreeSet<ProofKey>, remote: &PeerProofs) -> BTreeSet<ProofKey> {
    let mut all = BTreeSet::new();
    let mut after = None;
    loop {
        let batch = lacked_proofs(local, remote, after);
        let Some(last) = batch.last().copied() else {
            return all;
        };
        if !batch
            .into_iter()
            .fold(false, |new, key| all.insert(key) || new)
        {
            return all;
        }
        after = Some(last);
    }
}

/// Two tables' conflict-proof inventories of 20,000 proofs each reconcile by range on `atdb/9`:
/// each side learns exactly which of its proofs the other lacks (here 25 one way, 9 the other,
/// and not the one whose binding the peer already holds a fork proof of), in under 16 KiB both
/// ways together, where `atdb/7` and `atdb/8` send each side's whole list (20,000 keys of 81
/// bytes, 1.6 MB each way). No count bounds the inventories: 25,000 proofs, past the 20,480 a
/// whole list carries, reconcile as well.
/// NEGATIVE CONTROL: the whole-list exchange of the same inventories selects the same proofs and
/// writes more than 3 MB, and refuses an inventory past its list's bound.
#[test]
fn the_conflict_proof_inventory_of_20000_proofs_reconciles_by_range() {
    let shared = proof_keys(1, 20_000 - 25);
    let only_a = proof_keys(2, 25);
    let only_b = proof_keys(3, 8);
    // `b` holds the fork proof of one binding `a` alone holds a proof of: that proof is not sent.
    let forked = *only_a.first().unwrap();
    let a: BTreeSet<ProofKey> = shared.union(&only_a).copied().collect();
    let mut b: BTreeSet<ProofKey> = shared.union(&only_b).copied().collect();
    b.insert(fork_of(&forked));
    b.remove(shared.first().unwrap());
    assert_eq!((a.len(), b.len()), (20_000, 19_983));

    let lacked_by_b: BTreeSet<ProofKey> = a
        .iter()
        .filter(|key| !b.contains(*key) && !b.contains(&fork_of(key)))
        .copied()
        .collect();
    let lacked_by_a: BTreeSet<ProofKey> = b
        .iter()
        .filter(|key| !a.contains(*key) && !a.contains(&fork_of(key)))
        .copied()
        .collect();
    assert_eq!(
        (lacked_by_b.len(), lacked_by_a.len()),
        (25, 9),
        "24 of a's own and a shared one b lacks; b's 8 and its fork proof"
    );

    let (of_b, of_a, ranges) = traded(&a, &b, |stream, side, local| {
        reconcile_proofs(stream, side, Some(local), || Ok(()))
            .unwrap()
            .remote
            .map(PeerProofs::Reconciled)
    });
    assert_eq!(every_lacked(&a, &of_b), lacked_by_b);
    assert_eq!(every_lacked(&b, &of_a), lacked_by_a);
    let PeerProofs::Reconciled(ids) = &of_b else {
        panic!("a reconciled inventory")
    };
    assert_eq!(
        *ids,
        b.iter().map(proof_id).collect::<BTreeSet<_>>(),
        "a learns b's exact inventory"
    );
    assert!(ranges < 16 * 1024, "{ranges} bytes by range");

    let (listed_b, listed_a, lists) = traded(&a, &b, |stream, side, local| {
        exchange_proof_list(stream, side, Some(local), || Ok(()))
            .unwrap()
            .map(PeerProofs::Listed)
    });
    assert_eq!(every_lacked(&a, &listed_b), lacked_by_b);
    assert_eq!(every_lacked(&b, &listed_a), lacked_by_a);
    assert!(lists > 3_000_000, "{lists} bytes as whole lists");

    // Past the whole list's bound: by range it reconciles, as a list it is refused.
    let large = proof_keys(4, 25_000);
    let smaller: BTreeSet<ProofKey> = large.iter().skip(3).copied().collect();
    let (of_smaller, _, _) = traded(&large, &smaller, |stream, side, local| {
        reconcile_proofs(stream, side, Some(local), || Ok(()))
            .unwrap()
            .remote
            .map(PeerProofs::Reconciled)
    });
    assert_eq!(
        every_lacked(&large, &of_smaller),
        large.iter().take(3).copied().collect()
    );
    assert!(
        exchange_proof_list(
            &mut Cursor::new(Vec::new()),
            Side::Initiator,
            Some(&large),
            || Ok(())
        )
        .is_err(),
        "a whole list past {LISTED_PROOFS} keys is refused"
    );
}
