// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age;
use std::sync::OnceLock;

#[path = "payload_tests.rs"]
mod payload_tests;

const FOUNDING_SEED: [u8; 32] = [99; 32];

/// One Mac, its keys all derived from its tag.
#[derive(Clone, Copy, Debug)]
struct Mac(u8);

const A: Mac = Mac(1);
const B: Mac = Mac(2);
const C: Mac = Mac(3);
const D: Mac = Mac(4);
const E: Mac = Mac(5);
const S: Mac = Mac(6);
const X: Mac = Mac(7);
const Y: Mac = Mac(8);
const Z: Mac = Mac(9);
const W: Mac = Mac(10);
const V: Mac = Mac(11);
const U: Mac = Mac(12);
const P: Mac = Mac(13);
const R: Mac = Mac(14);

impl Mac {
    fn seed(self) -> [u8; 32] {
        [self.0; 32]
    }
    fn incarnation(self) -> [u8; 16] {
        [self.0; 16]
    }
    fn identity(self) -> age::Identity {
        age::Identity::from_secret_bytes([self.0 ^ 0x55; 32])
    }
    fn request(self) -> Request {
        Request::create(
            [2; 16],
            [3; 32],
            [self.0; 16],
            self.incarnation(),
            &self.identity(),
            &self.seed(),
        )
        .unwrap()
    }
    fn id(self) -> MemberId {
        self.request().binding().actor()
    }
    fn entrant(self, label: &str) -> Entrant {
        Entrant {
            request: self.request(),
            node_key: [self.0; 32],
            name: format!("mac-{}", self.0),
            label: label.to_owned(),
        }
    }
    /// The label this Mac takes when another Mac claims `label` too.
    fn suffixed(self, label: &str) -> String {
        let digest = Sha256::digest(self.request().binding().bytes());
        format!("{label}-{:02x}{:02x}", digest[0], digest[1])
    }
}

fn found(founders: &[(Mac, &str)]) -> Log {
    let founders = founders
        .iter()
        .map(|(mac, label)| mac.entrant(label))
        .collect();
    Log::new(Record::found(&FOUNDING_SEED, founders, 1).unwrap()).unwrap()
}

/// A record by `by` that follows every record `log` holds.
fn sign(log: &Log, by: Mac, body: Body) -> Record {
    log.draft(&by.id(), 7, body)
        .sign(&by.seed(), by.incarnation())
        .unwrap()
}
fn admit(log: &Log, by: Mac, who: Mac, label: &str) -> Record {
    let body = Body::Admit {
        entrant: who.entrant(label),
        evidence: Evidence::Phrase,
    };
    sign(log, by, body)
}
fn remove(log: &Log, by: Mac, who: Mac, cutoff: &RecordId) -> Record {
    let body = Body::Remove {
        member: who.id(),
        cutoff: *cutoff,
    };
    sign(log, by, body)
}
trait Keep {
    /// Hold `record`, and hand it back.
    fn keep(&mut self, record: Record) -> Record;
}
impl Keep for Log {
    fn keep(&mut self, record: Record) -> Record {
        assert_eq!(self.insert(record.clone()).unwrap(), Inserted::Held);
        record
    }
}
fn heal(one: &mut Log, two: &mut Log) {
    one.merge(two).unwrap();
    two.merge(one).unwrap();
}
fn ids(macs: &[Mac]) -> BTreeSet<MemberId> {
    macs.iter().map(|m| m.id()).collect()
}
fn members(log: &Log) -> BTreeSet<MemberId> {
    log.membership().members().keys().copied().collect()
}

#[test]
fn membership_tracks_landed_removals_while_unchanged_clones_keep_their_own_prefix() {
    let mut original = found(&[(A, "m1"), (B, "m2")]);
    assert_eq!(members(&original), ids(&[A, B]));
    let mut sender = original.clone();
    let renamed = sender.keep(sign(
        &sender,
        B,
        Body::Name {
            name: "renamed".to_owned(),
        },
    ));
    let removed = sender.keep(remove(&sender, A, B, renamed.id()));
    assert_eq!(members(&sender), ids(&[A]));
    assert_eq!(members(&original), ids(&[A, B]));

    let mut receiver = original.clone();
    assert_eq!(receiver.insert(removed).unwrap(), Inserted::Waiting);
    assert_eq!(members(&receiver), ids(&[A, B]));
    assert_eq!(receiver.insert(renamed).unwrap(), Inserted::Held);
    assert_eq!(receiver.waiting(), 0);
    assert_eq!(members(&receiver), ids(&[A]));
    assert_eq!(receiver.membership(), eval::evaluate(&receiver.order));
    assert_eq!(members(&original), ids(&[A, B]));

    original.merge(&receiver).unwrap();
    assert_eq!(original.membership(), sender.membership());
    assert_eq!(members(&original), ids(&[A]));
}

/// A real fleet node identity, from its tag: `Mac`'s `[n; 32]` node keys are no Ed25519 keys.
fn node_pair(tag: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[tag ^ 0xa5; 32]).unwrap()
}
fn node_key(tag: u8) -> [u8; 32] {
    node_pair(tag).public_key().as_ref().try_into().unwrap()
}
/// A `Node` record by `by` that binds node key `tag`, its proof signed by that key.
fn node(log: &Log, by: Mac, tag: u8) -> Record {
    let proof = node_pair(tag).sign(&node_proof_message(log.network(), &by.id()));
    let body = Body::Node {
        node_key: node_key(tag),
        proof: proof.as_ref().try_into().unwrap(),
    };
    sign(log, by, body)
}

#[test]
fn a_record_round_trips_and_every_byte_is_signed() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let genesis = lab.genesis();
    assert_eq!(Record::decode(genesis.bytes()).unwrap().id(), genesis.id());
    let record = admit(&lab, A, X, "m3");
    assert_eq!(Record::decode(record.bytes()).unwrap().id(), record.id());
    assert_eq!(record.network(), lab.network());
    assert_eq!(record.sync_header().id, *record.id());
    for at in 0..record.bytes().len() {
        let mut wire = record.bytes().to_vec();
        wire[at] ^= 0x10;
        assert!(Record::decode(&wire).is_err(), "byte {at} is unsigned");
    }
    let mut longer = record.bytes().to_vec();
    longer.push(0);
    assert!(Record::decode(&longer).is_err());
}

#[test]
fn malformed_records_are_refused_before_any_log_sees_them() {
    let lab = found(&[(A, "m1")]);
    let draft = |body: Body| lab.draft(&A.id(), 7, body);
    let name = |n: &str| Body::Name { name: n.to_owned() };
    let mut no_parent = draft(name("a"));
    no_parent.parents.clear();
    assert!(no_parent.sign(&A.seed(), A.incarnation()).is_err());
    let mut no_sequence = draft(name("a"));
    no_sequence.sequence = 0;
    assert!(no_sequence.sign(&A.seed(), A.incarnation()).is_err());
    let mut lost_previous = draft(name("a"));
    lost_previous.sequence = 2;
    assert!(lost_previous.sign(&A.seed(), A.incarnation()).is_err());
    for bad in ["", "a\nb", &"n".repeat(MAX_NAME + 1)] {
        assert!(draft(name(bad)).sign(&A.seed(), A.incarnation()).is_err());
    }
    for bad in ["m0", "m01", "x4", "m", "m123456", "m4-3fa2"] {
        let body = Body::Admit {
            entrant: X.entrant(bad),
            evidence: Evidence::Manual,
        };
        assert!(
            draft(body).sign(&A.seed(), A.incarnation()).is_err(),
            "{bad}"
        );
    }
    assert!(is_label("m40") && is_label("m99999"));
    let twins = vec![A.entrant("m1"), A.entrant("m2")];
    assert!(Record::found(&FOUNDING_SEED, twins, 1).is_err());
    let one_label = vec![A.entrant("m1"), B.entrant("m1")];
    assert!(Record::found(&FOUNDING_SEED, one_label, 1).is_err());
    let genesis = lab.genesis().body().clone();
    assert!(draft(genesis).sign(&A.seed(), A.incarnation()).is_err());
}

#[test]
fn a_record_waits_for_its_parent_and_lands_when_it_arrives() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let mut first = lab.clone();
    let parent = first.keep(admit(&first, A, X, "m3"));
    let child = admit(&first, B, Y, "m4");
    let mut later = lab.clone();
    assert_eq!(later.insert(child.clone()).unwrap(), Inserted::Waiting);
    assert_eq!(later.insert(child.clone()).unwrap(), Inserted::Waiting);
    assert_eq!(later.waiting(), 1);
    assert!(!members(&later).contains(&Y.id()));
    assert_eq!(later.insert(parent).unwrap(), Inserted::Held);
    assert_eq!(later.waiting(), 0);
    assert_eq!(later.insert(child.clone()).unwrap(), Inserted::Known);
    assert_eq!(members(&later), ids(&[A, B, X, Y]));
    assert_eq!(later.heads(), vec![*child.id()]);
}

#[test]
fn records_of_another_network_or_out_of_order_are_refused() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let other = found(&[(A, "m1")]);
    assert!(lab.clone().insert(admit(&other, A, X, "m2")).is_err());
    assert!(lab.clone().insert(other.genesis().clone()).is_err());
    assert_eq!(
        lab.clone().insert(lab.genesis().clone()).unwrap(),
        Inserted::Known
    );
    assert!(lab.clone().merge(&other).is_err());

    // A removal's cutoff is a record in its own past.
    let mut one = lab.clone();
    let mut two = lab.clone();
    let elsewhere = two.keep(admit(&two, B, X, "m3"));
    let early = remove(&one, A, B, elsewhere.id());
    assert!(one.insert(early).is_err());

    // A previous is its signer's own record at the sequence before.
    let mine = one.keep(admit(&one, A, Y, "m4"));
    let mut forged = one.draft(&B.id(), 7, Body::Name { name: "b".into() });
    forged.sequence = 2;
    forged.previous = Some(*mine.id());
    let forged = forged.sign(&B.seed(), B.incarnation()).unwrap();
    assert!(one.insert(forged).is_err());
}

#[test]
fn every_insertion_order_gives_the_same_log_and_membership() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    let mut one = lab.clone();
    let mut two = lab.clone();
    let ax = one.keep(admit(&one, A, X, "m4"));
    let rc = one.keep(remove(&one, A, C, lab.network()));
    let by = two.keep(admit(&two, B, Y, "m4"));
    let cz = two.keep(admit(&two, C, Z, "m5"));
    let named = two.keep(sign(&two, Y, Body::Name { name: "why".into() }));
    let bound = two.keep(node(&two, Y, 1));
    let records = [ax, rc, by, cz, named, bound];
    let mut want: Option<(Membership, Vec<RecordId>)> = None;
    let mut order: Vec<usize> = (0..records.len()).collect();
    let mut seen = 0;
    // Every permutation, by Heap's algorithm.
    let mut counters = vec![0; order.len()];
    let mut at = 0;
    loop {
        let mut log = lab.clone();
        for &i in &order {
            log.insert(records[i].clone()).unwrap();
        }
        let got = (log.membership(), log.heads());
        match &want {
            None => want = Some(got),
            Some(want) => assert_eq!(&got, want, "order {order:?}"),
        }
        seen += 1;
        while at < order.len() && counters[at] >= at {
            counters[at] = 0;
            at += 1;
        }
        if at == order.len() {
            break;
        }
        if at % 2 == 0 {
            order.swap(0, at);
        } else {
            order.swap(counters[at], at);
        }
        counters[at] += 1;
        at = 0;
    }
    assert_eq!(seen, 720);
    let (membership, _) = want.unwrap();
    assert_eq!(
        membership
            .members()
            .keys()
            .copied()
            .collect::<BTreeSet<_>>(),
        ids(&[A, B, X, Y])
    );
    assert_eq!(membership.get(&Y.id()).unwrap().name, "why");
    assert_eq!(membership.get(&Y.id()).unwrap().node_key, Some(node_key(1)));
    assert_eq!(membership.node(&node_key(1)), NodeStanding::Member(Y.id()));
}

#[test]
fn two_islands_each_admit_one_mac_and_the_union_agrees_with_no_publish() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let mut one = lab.clone();
    let mut two = lab.clone();
    one.keep(admit(&one, A, X, "m3"));
    two.keep(admit(&two, B, Y, "m4"));
    assert_eq!(members(&one), ids(&[A, B, X]));
    assert_eq!(members(&two), ids(&[A, B, Y]));
    heal(&mut one, &mut two);
    // The union is the genesis and the two admissions: no record was made to merge them.
    assert_eq!(one.records().count(), 3);
    assert_eq!(one.heads(), two.heads());
    assert_eq!(one.membership(), two.membership());
    assert_eq!(members(&one), ids(&[A, B, X, Y]));
    let x = one.membership().get(&X.id()).cloned().unwrap();
    assert_eq!((x.label.as_str(), x.admitted_by), ("m3", Some(A.id())));
}

#[test]
fn remove_beats_a_concurrent_admit() {
    let lab = found(&[(B, "m1"), (C, "m2"), (D, "m3")]);
    let mut one = lab.clone();
    let mut two = lab.clone();
    // X's request reaches C and B; D removes X once it sees B's admission, not C's.
    one.keep(admit(&one, C, X, "m4"));
    let entry = two.keep(admit(&two, B, X, "m4"));
    two.keep(remove(&two, D, X, entry.id()));
    assert!(members(&one).contains(&X.id()));
    heal(&mut one, &mut two);
    for log in [&one, &two] {
        let membership = log.membership();
        assert_eq!(members(log), ids(&[B, C, D]));
        assert_eq!(membership.removal(&X.id()).unwrap().by, ids(&[D]));
    }
}

#[test]
fn an_admit_that_follows_a_remove_restores_the_mac() {
    let mut lab = found(&[(B, "m1"), (D, "m2")]);
    let entry = lab.keep(admit(&lab, B, X, "m3"));
    lab.keep(remove(&lab, D, X, entry.id()));
    assert!(!members(&lab).contains(&X.id()));
    let mut again = lab.clone();
    let restore = again.keep(admit(&again, B, X, "m7"));
    let membership = again.membership();
    let x = membership.get(&X.id()).unwrap();
    assert_eq!(x.entry, *restore.id());
    // The label is the one its first admission claimed.
    assert_eq!(x.label, "m3");
    // A removal concurrent with the restoring admission still wins.
    let mut other = lab.clone();
    other.keep(remove(&other, B, X, entry.id()));
    heal(&mut again, &mut other);
    assert!(!members(&again).contains(&X.id()));
}

#[test]
fn mutual_removes_remove_both() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    let mut one = lab.clone();
    let mut two = lab.clone();
    one.keep(remove(&one, A, B, lab.network()));
    two.keep(remove(&two, B, A, lab.network()));
    assert_eq!(members(&one), ids(&[A, C]));
    assert_eq!(members(&two), ids(&[B, C]));
    heal(&mut one, &mut two);
    assert_eq!(members(&one), ids(&[C]));
    assert_eq!(one.membership(), two.membership());
}

#[test]
fn a_removed_members_later_admit_is_inert() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let mut one = lab.clone();
    let mut two = lab.clone();
    // B lets Y in, and A sees it before it removes B: Y keeps its place.
    let by = one.keep(admit(&one, B, Y, "m3"));
    two.merge(&one).unwrap();
    one.keep(remove(&one, A, B, by.id()));
    // B, not yet aware, lets Z in after the cutoff, and after hearing of it lets W in.
    two.keep(admit(&two, B, Z, "m4"));
    assert!(members(&two).contains(&Z.id()));
    heal(&mut one, &mut two);
    assert_eq!(members(&one), ids(&[A, Y]));
    two.keep(admit(&two, B, W, "m5"));
    assert_eq!(members(&two), ids(&[A, Y]));
}

#[test]
fn a_mass_remove_by_one_member_is_undone_by_a_quorum_remove() {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4"), (E, "m5")]);
    let cutoff = *lab.network();
    let mut removals = Vec::new();
    for who in [B, C, D, E] {
        removals.push(lab.keep(remove(&lab, A, who, &cutoff)));
    }
    assert_eq!(members(&lab), ids(&[A]));
    // Each honest Mac signs its share after it heard of its removal: as a plain removal it is
    // inert, its signer out in its past.
    let mut shares = Vec::new();
    for who in [B, C, D] {
        shares.push(remove(&lab, who, A, &cutoff));
    }
    let mut log = lab.clone();
    log.keep(shares[0].clone());
    log.keep(shares[1].clone());
    // Two of the four members at the cutoff is not more than half.
    assert_eq!(members(&log), ids(&[A]));
    assert!(log.membership().quorums().is_empty());
    log.keep(shares[2].clone());
    let membership = log.membership();
    assert_eq!(members(&log), ids(&[B, C, D, E]));
    let removal = membership.removal(&A.id()).unwrap();
    assert!(removal.quorum);
    assert_eq!(removal.by, ids(&[B, C, D]));
    let removed: BTreeSet<RecordId> = removals.iter().map(|r| *r.id()).collect();
    assert_eq!(*membership.voided(), removed);
    assert_eq!(membership.quorums()[0].cutoff, cutoff);
}

#[test]
fn a_lone_removed_mac_cannot_void_its_removal() {
    for founders in [
        &[(B, "m1"), (S, "m2")][..],
        &[(A, "m1"), (B, "m2"), (S, "m3")],
    ] {
        let mut lab = found(founders);
        let cutoff = *lab.network();
        lab.keep(remove(&lab, B, S, &cutoff));
        // S heard of it, and alone signs a quorum remove of B from before it.
        lab.keep(remove(&lab, S, B, &cutoff));
        let membership = lab.membership();
        assert!(membership.contains(&B.id()) && !membership.contains(&S.id()));
        assert!(membership.quorums().is_empty());
        assert_eq!(membership.removal(&S.id()).unwrap().by, ids(&[B]));
    }
}

#[test]
fn macs_the_actor_admitted_after_the_cutoff_never_vote() {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4")]);
    let genesis = *lab.network();
    for (who, label) in [(X, "m5"), (Y, "m6"), (Z, "m7")] {
        lab.keep(admit(&lab, A, who, label));
    }
    for who in [B, C, D] {
        lab.keep(remove(&lab, A, who, &genesis));
    }
    assert_eq!(members(&lab), ids(&[A, X, Y, Z]));
    // B and C undo A from the genesis, B first.
    let share = lab.keep(remove(&lab, B, A, &genesis));
    lab.keep(remove(&lab, C, A, &genesis));
    // A and the Macs it let in answer with a quorum of their own against B from B's share,
    // where they were every member B had not seen go.
    let mut counter = lab.clone();
    for who in [A, X, Y, Z] {
        counter.keep(remove(&counter, who, B, share.id()));
    }
    for log in [&lab, &counter] {
        let membership = log.membership();
        assert_eq!(members(log), ids(&[B, C, D]));
        let out: Vec<MemberId> = membership.quorums().iter().map(|q| q.member).collect();
        assert_eq!(out, vec![A.id()]);
    }
}

#[test]
fn mutual_quorum_removes_remove_both() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4")]);
    let cutoff = *lab.network();
    // Each quorum of three voters needs two shares, and each counts a share by the member the
    // other one takes out: judged with the other's effects, neither would stand.
    let mut one = lab.clone();
    let mut two = lab.clone();
    for who in [B, C] {
        one.keep(remove(&one, who, A, &cutoff));
    }
    for who in [A, D] {
        two.keep(remove(&two, who, B, &cutoff));
    }
    heal(&mut one, &mut two);
    let membership = one.membership();
    assert_eq!(members(&one), ids(&[C, D]));
    let out: BTreeSet<MemberId> = membership.quorums().iter().map(|q| q.member).collect();
    assert_eq!(out, ids(&[A, B]));
    assert_eq!(membership, two.membership());
}

#[test]
fn a_cutoff_is_a_record_of_the_mac_it_removes_or_the_one_that_let_it_in() {
    let mut lab = found(&[(A, "m1"), (B, "m2")]);
    let genesis = *lab.network();
    let entry = lab.keep(admit(&lab, A, X, "m3"));
    let named = lab.keep(sign(&lab, B, Body::Name { name: "b".into() }));
    for (who, cutoff) in [(B, &genesis), (X, entry.id()), (B, named.id())] {
        assert!(lab.clone().insert(remove(&lab, A, who, cutoff)).is_ok());
    }
    for (who, cutoff) in [(X, &genesis), (X, named.id()), (B, entry.id())] {
        assert!(lab.clone().insert(remove(&lab, A, who, cutoff)).is_err());
    }
}

#[test]
fn the_undo_of_a_removed_mac_defeats_the_quorum_its_branch_made() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (S, "m4")]);
    let genesis = *lab.network();
    let mut honest = lab.clone();
    honest.keep(remove(&honest, B, S, &genesis));
    // S, on a branch that leaves its removal out, lets two Macs of its own in, and C sees them.
    let mut branch = lab.clone();
    let let_in = branch.keep(admit(&branch, S, X, "m5"));
    branch.keep(admit(&branch, S, Y, "m6"));
    let seen = branch.keep(sign(&branch, C, Body::Name { name: "c".into() }));
    // A cutoff of S's own choosing, which would leave C's view out, is refused.
    assert!(
        branch
            .clone()
            .insert(remove(&branch, S, C, let_in.id()))
            .is_err()
    );
    for who in [S, X, Y] {
        branch.keep(remove(&branch, who, C, seen.id()));
    }
    heal(&mut honest, &mut branch);
    // At C's record S, X and Y were three of five members, so their quorum holds until the
    // members undo S from its entry, which voids the admissions that gave them their votes.
    let quorum = |log: &Log| -> Vec<MemberId> {
        log.membership()
            .quorums()
            .iter()
            .map(|q| q.member)
            .collect()
    };
    assert_eq!(quorum(&honest), vec![C.id()]);
    for who in [A, B, C] {
        honest.keep(remove(&honest, who, S, &genesis));
    }
    assert_eq!(quorum(&honest), vec![S.id()]);
    assert_eq!(members(&honest), ids(&[A, B, C]));
    // A lab of two: S and one Mac of its own against the member that removed it.
    let lab = found(&[(B, "m1"), (S, "m2")]);
    let genesis = *lab.network();
    let mut honest = lab.clone();
    honest.keep(remove(&honest, B, S, &genesis));
    let mut branch = lab.clone();
    branch.keep(admit(&branch, S, X, "m3"));
    for who in [S, X] {
        branch.keep(remove(&branch, who, B, &genesis));
    }
    heal(&mut honest, &mut branch);
    let membership = honest.membership();
    assert!(membership.quorums().is_empty());
    assert!(!membership.contains(&S.id()) && !membership.contains(&X.id()));
}

#[test]
fn an_admit_no_member_signed_is_no_cutoff() {
    let mut lab = found(&[(A, "m1"), (S, "m2"), (X, "m3")]);
    // Z, never a member, signs an early admission of B, before A lets C, D and B in.
    let forged = lab.keep(sign(
        &lab,
        Z,
        Body::Admit {
            entrant: B.entrant("m9"),
            evidence: Evidence::Manual,
        },
    ));
    for (who, label) in [(C, "m4"), (D, "m5"), (B, "m6")] {
        lab.keep(admit(&lab, A, who, label));
    }
    // S and X, two of the three Macs in at the forged record, remove B from it.
    for who in [S, X] {
        lab.keep(remove(&lab, who, B, forged.id()));
    }
    let membership = lab.membership();
    assert!(membership.quorums().is_empty());
    assert!(!membership.removal(&B.id()).unwrap().quorum);
}

#[test]
fn macs_the_member_let_in_after_the_cutoff_never_carry_a_counter_quorum() {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    let genesis = *lab.network();
    for (who, label) in [(X, "m4"), (Y, "m5"), (Z, "m6")] {
        lab.keep(admit(&lab, A, who, label));
    }
    // B signs with A's admissions in view, then A removes B and C, and they undo A.
    let seen = lab.keep(sign(&lab, B, Body::Name { name: "b".into() }));
    for who in [B, C] {
        lab.keep(remove(&lab, A, who, &genesis));
    }
    for who in [B, C] {
        lab.keep(remove(&lab, who, A, &genesis));
    }
    // A and the Macs it let in answer against B from B's record, where they were four of five.
    for who in [A, X, Y, Z] {
        lab.keep(remove(&lab, who, B, seen.id()));
    }
    let membership = lab.membership();
    let out: Vec<MemberId> = membership.quorums().iter().map(|q| q.member).collect();
    assert_eq!(out, vec![A.id()]);
    assert_eq!(members(&lab), ids(&[B, C]));
}

#[test]
fn a_remove_signed_after_the_cutoff_takes_no_vote_away() {
    let mut lab = found(&[(A, "m1"), (S, "m2")]);
    let genesis = *lab.network();
    let b_in = lab.keep(admit(&lab, A, B, "m3"));
    let c_in = lab.keep(admit(&lab, A, C, "m4"));
    // S lets P in, then removes A, B and C.
    let good = lab.keep(admit(&lab, S, P, "m5"));
    lab.keep(remove(&lab, S, A, c_in.id()));
    lab.keep(remove(&lab, S, B, b_in.id()));
    lab.keep(remove(&lab, S, C, c_in.id()));
    for who in [A, B, C] {
        lab.keep(remove(&lab, who, S, good.id()));
    }
    // P removes A from the genesis afterwards, which would cut A's admissions of B and C.
    lab.keep(remove(&lab, P, A, &genesis));
    for who in [A, B, C] {
        lab.keep(remove(&lab, who, P, good.id()));
    }
    let membership = lab.membership();
    let out: BTreeSet<MemberId> = membership.quorums().iter().map(|q| q.member).collect();
    assert_eq!(out, ids(&[S, P]));
    assert_eq!(members(&lab), ids(&[A, B, C]));
}

#[test]
fn a_quorum_keeps_what_its_member_signed_before_the_cutoff() {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (D, "m3"), (E, "m4")]);
    let genesis = *lab.network();
    // D lets C in and removes E, a stolen Mac; then D, stolen in turn, removes everyone else.
    let let_in = lab.keep(admit(&lab, D, C, "m5"));
    let last_good = lab.keep(remove(&lab, D, E, &genesis));
    for who in [A, B] {
        lab.keep(remove(&lab, D, who, &genesis));
    }
    lab.keep(remove(&lab, D, C, let_in.id()));
    assert_eq!(members(&lab), ids(&[D]));
    for who in [A, B, C] {
        lab.keep(remove(&lab, who, D, last_good.id()));
    }
    let membership = lab.membership();
    assert_eq!(members(&lab), ids(&[A, B, C]));
    assert_eq!(membership.get(&C.id()).unwrap().admitted_by, Some(D.id()));
    assert_eq!(membership.removal(&E.id()).unwrap().by, ids(&[D]));
    assert!(membership.removal(&D.id()).unwrap().quorum);
}

#[test]
fn mutual_quorum_removes_remove_both_whichever_cutoff_comes_first() {
    let lab = found(&[(A, "m1"), (B, "m2"), (D, "m3"), (X, "m4")]);
    let genesis = *lab.network();
    // D signs a name, then removes A and B from the genesis, and X joins its removal of A.
    let mut one = lab.clone();
    let last_good = one.keep(sign(&one, D, Body::Name { name: "d".into() }));
    for who in [A, B] {
        one.keep(remove(&one, D, who, &genesis));
    }
    let mut two = one.clone();
    one.keep(remove(&one, X, A, &genesis));
    // A and B undo D from its name, the cutoff after D's.
    for who in [A, B] {
        two.keep(remove(&two, who, D, last_good.id()));
    }
    heal(&mut one, &mut two);
    let membership = one.membership();
    let out: BTreeSet<MemberId> = membership.quorums().iter().map(|q| q.member).collect();
    assert_eq!(out, ids(&[A, D]));
    assert_eq!(members(&one), ids(&[B, X]));
    assert_eq!(membership, two.membership());
}

#[test]
fn two_removers_that_shield_each_other_are_both_undone() {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4"), (E, "m5")]);
    let genesis = *lab.network();
    // D and E, both stolen, remove every other member, so each honest share follows a
    // removal of its signer by the remover it does not name.
    for by in [D, E] {
        for who in [A, B, C] {
            lab.keep(remove(&lab, by, who, &genesis));
        }
    }
    assert_eq!(members(&lab), ids(&[D, E]));
    for who in [A, B, C] {
        for rogue in [D, E] {
            lab.keep(remove(&lab, who, rogue, &genesis));
        }
    }
    assert_eq!(members(&lab), ids(&[A, B, C]));
    assert_eq!(lab.membership().quorums().len(), 2);
}

/// Every Mac the actor removed signs its share of the Undo at [`Log::undo_cutoff`], as F16's
/// "Undo m7's removals" row does.
fn undo_shares(log: &mut Log, actor: Mac, signers: &[Mac]) {
    let cutoff = log.undo_cutoff(&actor.id()).expect("an undo cutoff");
    for &who in signers {
        log.keep(remove(log, who, actor, &cutoff));
    }
}

/// F16's Undo against a stolen Mac that first lets Macs of its own in where nobody sees them,
/// more of them than the lab has members, and then removes every other member: every honest Mac
/// computes one cutoff, before the hidden `Admit`s, so those Macs never vote, the honest shares
/// are a quorum, and the lab is back with the Sybils out.
///
/// NEGATIVE CONTROL: in `Graph::undo_cutoff`, push only `Remove`s into `acts` (an `Admit` no act):
/// the cutoff falls after the hidden `Admit`s, X, Y, Z and W outvote B, C and D, and the
/// `"the lab is back"` assert fails.
#[test]
fn the_undo_cutoff_falls_before_admits_nobody_saw_so_their_macs_never_vote() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4")]);
    let genesis = *lab.network();
    // B signs once in the open: A's record before its branch is the genesis.
    let mut log = lab.clone();
    log.keep(sign(&log, B, Body::Name { name: "b".into() }));
    let mut branch = log.clone();
    for (who, label) in [(X, "m5"), (Y, "m6"), (Z, "m7"), (W, "m8")] {
        branch.keep(admit(&branch, A, who, label));
    }
    for who in [B, C, D] {
        branch.keep(remove(&branch, A, who, &genesis));
    }
    heal(&mut log, &mut branch);
    assert_eq!(members(&log), ids(&[A, X, Y, Z, W]));
    assert_eq!(log.undo_cutoff(&A.id()), Some(genesis));
    let undo = log.undo(&A.id()).expect("an undo");
    assert_eq!(undo.voters, ids(&[B, C, D]));
    assert_eq!(undo.needed(), 2);
    undo_shares(&mut log, A, &[B, C, D]);
    let undo = log.undo(&A.id()).expect("an undo");
    assert_eq!(undo.signed, ids(&[B, C, D]));
    assert!(undo.took_effect && undo.needed() == 0);
    assert_eq!(members(&log), ids(&[B, C, D]), "the lab is back");
    assert!(log.membership().removal(&A.id()).unwrap().quorum);
}

/// An act the lab carried on from stands: A removed E, a stolen Mac, and B built on that before
/// A, stolen in turn, removed B, C and D. The Undo's cutoff is A's removal of E, so the quorum
/// brings B, C and D back and keeps E out.
///
/// NEGATIVE CONTROL: in `Graph::undo_cutoff`, never push to `acknowledged`: the cutoff falls
/// before A's removal of E, the quorum voids it, and the `"E stays out"` assert fails.
#[test]
fn the_undo_keeps_a_removal_the_lab_carried_on_from() {
    let mut log = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4"), (E, "m5")]);
    let genesis = *log.network();
    let removed_e = log.keep(remove(&log, A, E, &genesis));
    log.keep(sign(&log, B, Body::Name { name: "b".into() }));
    for who in [B, C, D] {
        log.keep(remove(&log, A, who, &genesis));
    }
    assert_eq!(members(&log), ids(&[A]));
    assert_eq!(log.undo_cutoff(&A.id()), Some(*removed_e.id()));
    undo_shares(&mut log, A, &[B, C, D]);
    assert_eq!(members(&log), ids(&[B, C, D]), "E stays out");
    assert!(!log.membership().removal(&E.id()).unwrap().quorum);
}

/// A Mac whose removals nobody contests has no Undo: one that removed nothing, and one whose
/// one removal the lab built on.
#[test]
fn a_member_with_no_contested_act_has_no_undo() {
    let mut log = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    assert_eq!(log.undo_cutoff(&A.id()), None);
    let genesis = *log.network();
    log.keep(remove(&log, A, C, &genesis));
    log.keep(sign(&log, B, Body::Name { name: "b".into() }));
    assert_eq!(log.undo_cutoff(&A.id()), None);
    assert!(log.undo(&A.id()).is_none());
}

/// F16's review: a Mac the actor let in through a Mac it let in is no more independent than
/// one it let in itself. A stolen A lets X in where nobody sees it, X lets Y in, A removes B, C
/// and D, and Y signs: Y's record is no acknowledgment of A's acts, so the cutoff is before X's
/// admission, B, C and D vote, and their quorum leaves A, X and Y out.
///
/// NEGATIVE CONTROL: in `Graph::undo_cutoff`, drop the loop that adds the Macs those let in
/// (`let_in` holds A's own entrants alone): Y's record acknowledges every act of A,
/// `undo_cutoff` is `None`, and the `"an undo cutoff"` expect fails.
#[test]
fn a_mac_let_in_by_a_mac_the_actor_let_in_never_acknowledges_its_acts() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4")]);
    let genesis = *lab.network();
    let mut log = lab.clone();
    log.keep(sign(&log, B, Body::Name { name: "b".into() }));
    let mut branch = log.clone();
    branch.keep(admit(&branch, A, X, "m5"));
    branch.keep(admit(&branch, X, Y, "m6"));
    for who in [B, C, D] {
        branch.keep(remove(&branch, A, who, &genesis));
    }
    branch.keep(sign(&branch, Y, Body::Name { name: "y".into() }));
    heal(&mut log, &mut branch);
    assert_eq!(members(&log), ids(&[A, X, Y]));
    undo_shares(&mut log, A, &[B, C, D]);
    assert_eq!(log.undo_cutoff(&A.id()), Some(genesis));
    assert_eq!(members(&log), ids(&[B, C, D]), "the lab is back");
}

/// F16's review: removing the remover contests it, so neither that `Remove` nor what its signer
/// signs after it acknowledges the remover's acts. A removes B, C and D; E, still in, removes A
/// and signs on. The Undo of A still stands before A's removals, and B, C and D's shares bring
/// them back.
///
/// NEGATIVE CONTROL: in `Graph::undo_cutoff`, leave `contests` empty: E's records acknowledge
/// A's removals, `undo_cutoff` is `None`, and the `"an undo cutoff"` expect fails.
#[test]
fn removing_the_remover_leaves_its_removals_to_undo() {
    let mut log = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4"), (E, "m5")]);
    let genesis = *log.network();
    let mut last = genesis;
    for who in [B, C, D] {
        last = *log.keep(remove(&log, A, who, &genesis)).id();
    }
    log.keep(remove(&log, E, A, &last));
    log.keep(sign(&log, E, Body::Name { name: "e".into() }));
    assert_eq!(members(&log), ids(&[E]));
    undo_shares(&mut log, A, &[B, C, D]);
    assert_eq!(log.undo_cutoff(&A.id()), Some(genesis));
    assert_eq!(members(&log), ids(&[B, C, D, E]));
}

/// F16's review, the split: on one side A removes S while on the other S removes C and D.
/// Removals are never cut off (two Macs that remove each other are both out), so after the heal
/// C and D are out, and whatever A's side signs next (an epoch the coverage rule mints, here a
/// name) holds A's removal of S in its past: it contests S, so C and D keep their Undo of S, and
/// a quorum with A brings them back.
///
/// NEGATIVE CONTROL: as above, leave `contests` empty: A's record acknowledges S's removals and
/// the `"an undo"` expect fails.
#[test]
fn a_record_after_a_removal_of_the_remover_leaves_its_split_removals_to_undo() {
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4"), (S, "m5")]);
    let genesis = *lab.network();
    let (mut one, mut two) = (lab.clone(), lab);
    one.keep(remove(&one, A, S, &genesis));
    for who in [C, D] {
        two.keep(remove(&two, S, who, &genesis));
    }
    heal(&mut one, &mut two);
    one.keep(sign(&one, A, Body::Name { name: "a".into() }));
    assert_eq!(members(&one), ids(&[A, B]));
    let undo = one.undo(&S.id()).expect("an undo");
    assert_eq!(undo.voters, ids(&[A, B, C, D]));
    undo_shares(&mut one, S, &[A, C, D]);
    assert_eq!(members(&one), ids(&[A, B, C, D]));
}

/// F16's review: a stolen Mac that lets one member stay to carry on from its first removals and
/// then removes that one too cannot leave a lab no quorum can bring back. The acknowledgment that
/// would walk the cutoff past the first removals leaves one voter, so it is set aside. In one
/// batch, every Mac comes back; removed one at a time, each removal carried on from by the Macs
/// still in, the last two come back, and they allow the rest again.
///
/// NEGATIVE CONTROL: in `Graph::undo_cutoff`, return `Some(settled)` at once (no step back): the
/// cutoff is A's removal of D, where E alone votes, and the first `undo_cutoff` assert fails.
#[test]
fn a_mac_left_to_carry_on_and_then_removed_still_brings_the_lab_back() {
    // In one batch: A removes B, C and D, E signs on, A removes E.
    let mut log = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4"), (E, "m5")]);
    let genesis = *log.network();
    for who in [B, C, D] {
        log.keep(remove(&log, A, who, &genesis));
    }
    log.keep(sign(&log, E, Body::Name { name: "e".into() }));
    log.keep(remove(&log, A, E, &genesis));
    assert_eq!(members(&log), ids(&[A]));
    assert_eq!(log.undo_cutoff(&A.id()), Some(genesis));
    let undo = log.undo(&A.id()).expect("an undo");
    assert_eq!(undo.voters, ids(&[B, C, D, E]));
    undo_shares(&mut log, A, &[B, C, D]);
    assert_eq!(members(&log), ids(&[B, C, D, E]), "every Mac comes back");

    // One at a time, each removal carried on from by the Macs still in.
    let mut log = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4"), (E, "m5")]);
    let genesis = *log.network();
    let mut by_c = genesis;
    for (who, rest) in [(B, &[C, D, E][..]), (C, &[D, E]), (D, &[E])] {
        let removal = log.keep(remove(&log, A, who, &genesis));
        if who.0 == C.0 {
            by_c = *removal.id();
        }
        for &on in rest {
            log.keep(sign(&log, on, Body::Name { name: "on".into() }));
        }
    }
    log.keep(remove(&log, A, E, &genesis));
    assert_eq!(members(&log), ids(&[A]));
    assert_eq!(log.undo_cutoff(&A.id()), Some(by_c));
    assert_eq!(log.undo(&A.id()).expect("an undo").voters, ids(&[D, E]));
    undo_shares(&mut log, A, &[D, E]);
    assert_eq!(members(&log), ids(&[D, E]), "the last two are back");
    log.keep(admit(&log, D, B, "m2"));
    log.keep(admit(&log, E, C, "m3"));
    assert_eq!(
        members(&log),
        ids(&[B, C, D, E]),
        "and allow the rest again"
    );
}

#[test]
fn two_admissions_of_one_mac_agree_in_either_order() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let by_a = admit(&lab, A, X, "m3");
    let mut entrant = X.entrant("m3");
    entrant.node_key = [42; 32];
    let by_b = sign(
        &lab,
        B,
        Body::Admit {
            entrant,
            evidence: Evidence::Manual,
        },
    );
    let mut one = lab.clone();
    one.keep(by_a.clone());
    one.keep(by_b.clone());
    let mut two = lab.clone();
    two.keep(by_b.clone());
    two.keep(by_a.clone());
    let membership = one.membership();
    assert_eq!(membership, two.membership());
    let first = by_a.id().min(by_b.id());
    assert_eq!(membership.get(&X.id()).unwrap().entry, *first);
}

#[test]
fn a_draft_past_the_parent_limit_follows_the_lowest_heads() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let mut log = lab.clone();
    for n in 0..=MAX_PARENTS {
        log.keep(sign(
            &lab,
            A,
            Body::Name {
                name: format!("n{n}"),
            },
        ));
    }
    assert_eq!(log.heads().len(), MAX_PARENTS + 1);
    let next = log.keep(sign(&log, B, Body::Name { name: "b".into() }));
    assert_eq!(next.parents().len(), MAX_PARENTS);
    assert_eq!(log.heads().len(), 2);
    let last = log.keep(sign(&log, B, Body::Name { name: "c".into() }));
    assert_eq!(log.heads(), vec![*last.id()]);
}

#[test]
fn a_claim_that_follows_a_label_does_not_relabel_its_holder() {
    let mut lab = found(&[(B, "m1"), (C, "m2")]);
    lab.keep(admit(&lab, B, X, "m3"));
    // C, with X's m3 in view, still lets Y in as m3, and as m1.
    lab.keep(admit(&lab, C, Y, "m3"));
    lab.keep(admit(&lab, C, Z, "m1"));
    let membership = lab.membership();
    assert_eq!(membership.label(&X.id()), Some("m3"));
    assert_eq!(membership.label(&B.id()), Some("m1"));
    assert_eq!(membership.label(&Y.id()), Some(Y.suffixed("m3").as_str()));
    assert_eq!(membership.label(&Z.id()), Some(Z.suffixed("m1").as_str()));
}

#[test]
fn a_removed_macs_admission_claims_no_label() {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let genesis = *lab.network();
    lab.keep(remove(&lab, A, X, &genesis));
    let mut one = lab.clone();
    let mut two = lab.clone();
    // X, with its removal in view, lets V in as m4 while B lets U in as m4.
    one.keep(admit(&one, X, V, "m4"));
    two.keep(admit(&two, B, U, "m4"));
    heal(&mut one, &mut two);
    let membership = one.membership();
    assert!(!membership.contains(&V.id()));
    assert_eq!(membership.label(&U.id()), Some("m4"));
}

#[test]
fn a_label_clash_after_a_heal_suffixes_every_claimant() {
    let lab = found(&[(B, "m1"), (C, "m2"), (D, "m3")]);
    let mut one = lab.clone();
    let mut two = lab.clone();
    one.keep(admit(&one, B, X, "m4"));
    two.keep(admit(&two, C, Y, "m4"));
    assert_eq!(one.membership().label(&X.id()), Some("m4"));
    assert_eq!(two.membership().label(&Y.id()), Some("m4"));
    heal(&mut one, &mut two);
    for log in [&one, &two] {
        let membership = log.membership();
        assert_eq!(membership.label(&X.id()), Some(X.suffixed("m4").as_str()));
        assert_eq!(membership.label(&Y.id()), Some(Y.suffixed("m4").as_str()));
        assert_eq!(membership.label(&B.id()), Some("m1"));
    }
    assert_ne!(X.suffixed("m4"), Y.suffixed("m4"));
}

#[test]
fn a_late_concurrent_admit_does_not_relabel_a_settled_clash_loser() {
    let lab = found(&[(B, "m1"), (C, "m2"), (D, "m3")]);
    let mut one = lab.clone();
    let mut two = lab.clone();
    let mut away = lab.clone();
    one.keep(admit(&one, B, X, "m4"));
    two.keep(admit(&two, C, Y, "m4"));
    // W settles on a bare m5 on the island that heals.
    one.keep(admit(&one, B, W, "m5"));
    heal(&mut one, &mut two);
    let settled = one.membership();
    assert_eq!(settled.label(&W.id()), Some("m5"));
    // Days later a Mac that was away rejoins with its own m4 and m5 admissions.
    away.keep(admit(&away, D, Z, "m4"));
    away.keep(admit(&away, D, V, "m5"));
    heal(&mut one, &mut away);
    let late = one.membership();
    for mac in [X, Y] {
        assert_eq!(late.label(&mac.id()), settled.label(&mac.id()));
    }
    assert_eq!(late.label(&Z.id()), Some(Z.suffixed("m4").as_str()));
    // Only the Mac that still held the bare label moves, and only this once.
    assert_eq!(late.label(&W.id()), Some(W.suffixed("m5").as_str()));
    assert_eq!(late.label(&V.id()), Some(V.suffixed("m5").as_str()));
    assert_eq!(late, away.membership());
}

#[test]
fn a_mac_let_in_by_a_member_a_quorum_restored_still_shows_its_clash() {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4")]);
    let cutoff = *lab.network();
    for who in [B, C, D] {
        lab.keep(remove(&lab, A, who, &cutoff));
    }
    for who in [B, C] {
        lab.keep(remove(&lab, who, A, &cutoff));
    }
    let mut one = lab.clone();
    let mut two = lab.clone();
    // B, whose removal the quorum voided, lets Y in as m5 while C lets Z in as m5.
    one.keep(admit(&one, B, Y, "m5"));
    two.keep(admit(&two, C, Z, "m5"));
    heal(&mut one, &mut two);
    let membership = one.membership();
    assert_eq!(members(&one), ids(&[B, C, D, Y, Z]));
    assert_eq!(membership.label(&Y.id()), Some(Y.suffixed("m5").as_str()));
    assert_eq!(membership.label(&Z.id()), Some(Z.suffixed("m5").as_str()));
}

#[test]
fn a_name_renames_its_signer_and_a_removed_members_name_is_inert() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let mut one = lab.clone();
    let mut two = lab.clone();
    one.keep(sign(
        &one,
        A,
        Body::Name {
            name: "first".into(),
        },
    ));
    one.keep(sign(
        &one,
        A,
        Body::Name {
            name: "second".into(),
        },
    ));
    one.keep(remove(&one, A, B, lab.network()));
    two.keep(sign(
        &two,
        B,
        Body::Name {
            name: "late".into(),
        },
    ));
    heal(&mut one, &mut two);
    let membership = one.membership();
    assert_eq!(membership.get(&A.id()).unwrap().name, "second");
    assert!(!membership.contains(&B.id()));
}

#[test]
fn a_node_record_round_trips_and_a_forged_proof_is_refused() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let other = found(&[(A, "m1")]);
    let record = node(&lab, A, 1);
    let decoded = Record::decode(record.bytes()).unwrap();
    assert_eq!((decoded.id(), decoded.kind()), (record.id(), Kind::Node));
    let Body::Node { node_key: key, .. } = decoded.body() else {
        panic!("not a node record");
    };
    assert_eq!(*key, node_key(1));
    // A proof by another key, for another member, or for another network is no proof.
    let forged = |by: &Ed25519KeyPair, network: &RecordId, actor: &MemberId| {
        let proof = by.sign(&node_proof_message(network, actor));
        let body = Body::Node {
            node_key: node_key(1),
            proof: proof.as_ref().try_into().unwrap(),
        };
        lab.draft(&A.id(), 7, body).sign(&A.seed(), A.incarnation())
    };
    let refusals = [
        forged(&node_pair(2), lab.network(), &A.id()),
        forged(&node_pair(1), lab.network(), &B.id()),
        forged(&node_pair(1), other.network(), &A.id()),
    ];
    for refused in refusals {
        let error = refused.unwrap_err().to_string();
        assert_eq!(error, "a node record's proof is not its node key's");
    }
    assert!(forged(&node_pair(1), lab.network(), &A.id()).is_ok());
}

/// A record's signer key of small order signs nothing: not the identity's forgery, which ring
/// alone takes for every message, and not the zero key's (a point of order 4, which the record's
/// shape once refused by name) for a record whose signed bytes ring alone takes it for. Ring is
/// shown to take each forgery, so neither arm passes by ring refusing it.
///
/// NEGATIVE CONTROL: `ed25519::verifies` through ring's `UnparsedPublicKey`: each forgery decodes
/// as a record of its key.
#[test]
fn a_small_order_key_signs_no_membership_record() {
    use crate::ed25519::{FORGED, SMALL_ORDER_KEY, ZERO_KEY, ring_takes, zero_key_forgery};
    let lab = found(&[(A, "m1"), (B, "m2")]);
    // MAGIC, VERSION, the network and the kind come before the actor; the signature ends it.
    let actor = MAGIC.len() + 1 + 32 + 1;
    // A's record renaming it `name`, its signer key replaced by `key` and its signature by the
    // one `forge` makes of its signed bytes.
    let forged = |name: String, key: [u8; 32], forge: &dyn Fn(&[u8]) -> Option<[u8; 64]>| {
        let mut wire = sign(&lab, A, Body::Name { name }).bytes().to_vec();
        wire[actor..actor + 32].copy_from_slice(&key);
        let signed = wire.len() - 64;
        let signature = forge(&signed_message(&wire[..signed]))?;
        wire[signed..].copy_from_slice(&signature);
        Some(wire)
    };
    let identity =
        |message: &[u8]| ring_takes(&SMALL_ORDER_KEY, message, &FORGED).then_some(FORGED);
    let identity = forged("renamed".into(), SMALL_ORDER_KEY, &identity)
        .expect("ring alone takes the identity's forgery");
    let zero = (0..64)
        .find_map(|n| forged(format!("renamed-{n}"), ZERO_KEY, &zero_key_forgery))
        .expect("ring alone takes the zero key's forgery of one of 64 records");
    for (key, wire) in [("identity", identity), ("zero", zero)] {
        let error = Record::decode(&wire).map(|_| ()).unwrap_err().to_string();
        assert_eq!(error, "membership record signature failed", "{key}");
    }
}

/// A Node record whose node key is of small order binds nothing: not the identity's forgery, and
/// not the zero key's (which the proof check once refused by name) on a network whose proof
/// message ring alone takes it for. Ring is shown to take each proof, so neither arm passes by
/// ring refusing it.
///
/// NEGATIVE CONTROL: `ed25519::verifies` through ring's `UnparsedPublicKey`: each forgery is
/// taken as its node key's proof and the record signs.
#[test]
fn a_small_order_node_key_proves_no_node_record() {
    use crate::ed25519::{FORGED, SMALL_ORDER_KEY, ZERO_KEY, ring_takes, zero_key_forgery};
    let lab = found(&[(A, "m1"), (B, "m2")]);
    assert!(
        ring_takes(
            &SMALL_ORDER_KEY,
            &node_proof_message(lab.network(), &A.id()),
            &FORGED
        ),
        "ring alone takes the identity's forgery"
    );
    // The proof signs the network, so another founding time is another message.
    let (zero_lab, zero_proof) = (2..66)
        .find_map(|authored_ms| {
            let founders = vec![A.entrant("m1"), B.entrant("m2")];
            let genesis = Record::found(&FOUNDING_SEED, founders, authored_ms).unwrap();
            let lab = Log::new(genesis).unwrap();
            let proof = zero_key_forgery(&node_proof_message(lab.network(), &A.id()))?;
            Some((lab, proof))
        })
        .expect("ring alone takes the zero key's forgery on one of 64 networks");
    for (lab, node_key, proof) in [
        (&lab, SMALL_ORDER_KEY, FORGED),
        (&zero_lab, ZERO_KEY, zero_proof),
    ] {
        let body = Body::Node { node_key, proof };
        let refused = lab.draft(&A.id(), 7, body).sign(&A.seed(), A.incarnation());
        let error = refused.map(|_| ()).unwrap_err().to_string();
        assert_eq!(
            error, "a node record's proof is not its node key's",
            "{node_key:02x?}"
        );
    }
}

#[test]
fn a_founder_listed_with_no_node_key_is_bound_by_its_own_node_record() {
    let founders: Vec<Founder> = [(A, "m1"), (B, "m2")]
        .into_iter()
        .map(|(mac, label)| Founder {
            node_key: None,
            ..Founder::from(mac.entrant(label))
        })
        .collect();
    let root = Root::of(A.request().binding());
    let genesis = Record::genesis(&FOUNDING_SEED, root, founders, 1).unwrap();
    let mut lab = Log::new(genesis).unwrap();
    assert_eq!(lab.membership().get(&A.id()).unwrap().node_key, None);
    assert!(lab.membership().nodes().is_empty());
    lab.keep(node(&lab, A, 1));
    let membership = lab.membership();
    assert_eq!(membership.get(&A.id()).unwrap().node_key, Some(node_key(1)));
    assert_eq!(membership.get(&B.id()).unwrap().node_key, None);
    assert_eq!(membership.node(&node_key(1)), NodeStanding::Member(A.id()));
    assert_eq!(membership.node(&node_key(2)), NodeStanding::Unknown);
}

#[test]
fn an_admit_with_zero_node_key_leaves_node_key_unbound_until_node_record() {
    let founders = vec![
        Founder::from(A.entrant("m1")),
        Founder {
            recipient: X.request().binding().recipient(),
            binding: None,
            node_key: None,
            name: "mac-x".to_owned(),
            label: "m2".to_owned(),
        },
    ];
    let root = Root::of(A.request().binding());
    let genesis = Record::genesis(&FOUNDING_SEED, root, founders, 1).unwrap();
    let mut lab = Log::new(genesis).unwrap();
    for (mac, label, evidence) in [(X, "m2", Evidence::Listed), (Y, "m3", Evidence::Manual)] {
        let mut entrant = mac.entrant(label);
        entrant.node_key = [0; 32];
        lab.keep(sign(&lab, A, Body::Admit { entrant, evidence }));
    }
    let membership = lab.membership();
    assert_eq!(membership.get(&X.id()).unwrap().node_key, None);
    assert_eq!(membership.get(&Y.id()).unwrap().node_key, None);
    assert_eq!(membership.node(&[0; 32]), NodeStanding::Unknown);
    assert!(!membership.nodes().contains_key(&[0; 32]));

    lab.keep(node(&lab, X, 1));
    let membership = lab.membership();
    assert_eq!(membership.get(&X.id()).unwrap().node_key, Some(node_key(1)));
    assert_eq!(membership.get(&Y.id()).unwrap().node_key, None);
    assert_eq!(membership.node(&node_key(1)), NodeStanding::Member(X.id()));
    assert_eq!(membership.node(&[0; 32]), NodeStanding::Unknown);
}

#[test]
fn the_latest_node_record_wins_and_the_key_it_replaced_is_retired() {
    let mut lab = found(&[(A, "m1"), (B, "m2")]);
    let entry_key = [A.0; 32];
    assert_eq!(
        lab.membership().node(&entry_key),
        NodeStanding::Member(A.id())
    );
    lab.keep(node(&lab, A, 1));
    lab.keep(node(&lab, A, 2));
    let membership = lab.membership();
    assert_eq!(membership.get(&A.id()).unwrap().node_key, Some(node_key(2)));
    assert_eq!(membership.node(&node_key(2)), NodeStanding::Member(A.id()));
    for old in [node_key(1), entry_key] {
        assert_eq!(membership.node(&old), NodeStanding::Retired(A.id()));
    }
}

#[test]
fn a_removed_macs_node_keys_read_removed_even_when_a_member_claims_one() {
    let mut lab = found(&[(A, "m1"), (B, "m2")]);
    let mut entrant = X.entrant("m3");
    entrant.node_key = node_key(5);
    let body = Body::Admit {
        entrant,
        evidence: Evidence::Phrase,
    };
    lab.keep(sign(&lab, A, body));
    let rotated = lab.keep(node(&lab, X, 6));
    // B, holding X's first node key, claims it as its own, and A removes X.
    lab.keep(node(&lab, B, 5));
    lab.keep(remove(&lab, A, X, rotated.id()));
    let membership = lab.membership();
    assert!(membership.removal(&X.id()).is_some());
    assert_eq!(membership.get(&B.id()).unwrap().node_key, Some(node_key(5)));
    for key in [node_key(5), node_key(6)] {
        assert_eq!(membership.node(&key), NodeStanding::Removed(X.id()));
    }
}

#[test]
fn two_members_that_claim_one_node_key_read_contested() {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    lab.keep(node(&lab, A, 7));
    assert_eq!(
        lab.membership().node(&node_key(7)),
        NodeStanding::Member(A.id())
    );
    lab.keep(node(&lab, B, 7));
    let membership = lab.membership();
    assert_eq!(membership.node(&node_key(7)), NodeStanding::Contested);
    assert_eq!(membership.node(&[C.0; 32]), NodeStanding::Member(C.id()));
}

#[test]
fn an_inert_admit_that_carries_a_members_node_key_binds_nothing() {
    let mut lab = found(&[(A, "m1"), (B, "m2")]);
    // Z, never a member, lets P in with A's node key, and B removes P from that admission.
    let mut entrant = P.entrant("m3");
    entrant.node_key = [A.0; 32];
    let body = Body::Admit {
        entrant,
        evidence: Evidence::Manual,
    };
    let inert = lab.keep(sign(&lab, Z, body));
    lab.keep(remove(&lab, B, P, inert.id()));
    let membership = lab.membership();
    assert!(membership.removal(&P.id()).is_some());
    assert_eq!(membership.node(&[A.0; 32]), NodeStanding::Member(A.id()));
}

#[test]
fn a_removed_mac_admit_carrying_a_members_node_key_does_not_poison_it() {
    for case in [
        "before plain removal cutoff",
        "sybil admit-and-remove before plain removal cutoff",
        "backdated self-admit",
        "after plain removal",
        "after quorum removal",
        "backdated sybil admit-and-remove",
        "backdated replay of independently removed mac",
        "honest member removes sybil carrying member node key",
        "redundant re-admit of removed mac carrying member node key",
    ] {
        let mut lab = found(&[(A, "m1"), (B, "m2")]);
        if case == "backdated replay of independently removed mac" {
            let admit_y = lab.keep(admit(&lab, A, Y, "m4"));
            lab.keep(remove(&lab, A, Y, admit_y.id()));
        }
        let let_in = lab.keep(admit(&lab, B, X, "m3"));
        let mut removal_cutoff = *let_in.id();
        if case == "before plain removal cutoff" {
            let mut entrant = X.entrant("m3");
            entrant.node_key = [A.0; 32];
            let own = lab.keep(sign(
                &lab,
                X,
                Body::Admit {
                    entrant,
                    evidence: Evidence::Manual,
                },
            ));
            removal_cutoff = *own.id();
        } else if case == "sybil admit-and-remove before plain removal cutoff" {
            let mut entrant = P.entrant("m99");
            entrant.node_key = [A.0; 32];
            let admit_p = lab.keep(sign(
                &lab,
                X,
                Body::Admit {
                    entrant,
                    evidence: Evidence::Manual,
                },
            ));
            let rem_p = lab.keep(remove(&lab, X, P, admit_p.id()));
            removal_cutoff = *rem_p.id();
        } else if case == "honest member removes sybil carrying member node key" {
            let mut entrant = P.entrant("m99");
            entrant.node_key = [A.0; 32];
            let admit_p = lab.keep(sign(
                &lab,
                X,
                Body::Admit {
                    entrant,
                    evidence: Evidence::Manual,
                },
            ));
            // Even while P is admitted and after A removes P, A's node key stays Member(A).
            assert_eq!(
                lab.membership().node(&[A.0; 32]),
                NodeStanding::Member(A.id()),
                "{case} (before removing P)"
            );
            let _rem_p = lab.keep(remove(&lab, A, P, admit_p.id()));
            assert_eq!(
                lab.membership().node(&[A.0; 32]),
                NodeStanding::Member(A.id()),
                "{case} (after A removes P, while X is still active)"
            );
            removal_cutoff = *admit_p.id();
        } else if case == "redundant re-admit of removed mac carrying member node key" {
            let admit_y = lab.keep(admit(&lab, A, Y, "m4"));
            let mut entrant = Y.entrant("m4");
            entrant.node_key = [A.0; 32];
            let readmit_y = lab.keep(sign(
                &lab,
                X,
                Body::Admit {
                    entrant,
                    evidence: Evidence::Manual,
                },
            ));
            let _rem_y = lab.keep(remove(&lab, A, Y, admit_y.id()));
            removal_cutoff = *readmit_y.id();
        }

        let removers: &[Mac] = if case == "after quorum removal" {
            &[A, B]
        } else {
            &[A]
        };
        for &by in removers {
            lab.keep(remove(&lab, by, X, &removal_cutoff));
        }

        match case {
            "before plain removal cutoff"
            | "sybil admit-and-remove before plain removal cutoff"
            | "honest member removes sybil carrying member node key"
            | "redundant re-admit of removed mac carrying member node key" => {}
            "backdated sybil admit-and-remove" => {
                let mut entrant = P.entrant("m99");
                entrant.node_key = [A.0; 32];
                let admit_p = Draft {
                    network: *lab.network(),
                    sequence: 1,
                    previous: None,
                    parents: vec![*let_in.id()],
                    authored_ms: 9,
                    body: Body::Admit {
                        entrant,
                        evidence: Evidence::Manual,
                    },
                }
                .sign(&X.seed(), X.incarnation())
                .unwrap();
                let admit_p_id = *admit_p.id();
                lab.keep(admit_p);
                let rem_p = Draft {
                    network: *lab.network(),
                    sequence: 2,
                    previous: Some(admit_p_id),
                    parents: vec![admit_p_id],
                    authored_ms: 10,
                    body: Body::Remove {
                        member: P.id(),
                        cutoff: admit_p_id,
                    },
                }
                .sign(&X.seed(), X.incarnation())
                .unwrap();
                lab.keep(rem_p);
            }
            "backdated replay of independently removed mac" => {
                let mut entrant = Y.entrant("m4");
                entrant.node_key = [A.0; 32];
                let replay = Draft {
                    network: *lab.network(),
                    sequence: 1,
                    previous: None,
                    parents: vec![*let_in.id()],
                    authored_ms: 9,
                    body: Body::Admit {
                        entrant,
                        evidence: Evidence::Manual,
                    },
                }
                .sign(&X.seed(), X.incarnation())
                .unwrap();
                lab.keep(replay);
            }
            _ => {
                let mut entrant = X.entrant("m3");
                entrant.node_key = [A.0; 32];
                let body = Body::Admit {
                    entrant,
                    evidence: Evidence::Manual,
                };
                let own = if case == "backdated self-admit" {
                    Draft {
                        network: *lab.network(),
                        sequence: 1,
                        previous: None,
                        parents: vec![*let_in.id()],
                        authored_ms: 9,
                        body,
                    }
                    .sign(&X.seed(), X.incarnation())
                    .unwrap()
                } else {
                    sign(&lab, X, body)
                };
                lab.keep(own);
            }
        }

        let membership = lab.membership();
        assert!(!membership.contains(&X.id()), "{case}");
        assert_eq!(
            membership.node(&[A.0; 32]),
            NodeStanding::Member(A.id()),
            "{case}"
        );
        assert_eq!(
            membership.node(&[B.0; 32]),
            NodeStanding::Member(B.id()),
            "{case}"
        );
        assert_eq!(
            membership.node(&[X.0; 32]),
            NodeStanding::Removed(X.id()),
            "{case}"
        );
    }
}

/// A log that holds every kind of record, and its records after the genesis, each after its
/// parents.
fn every_kind() -> (Log, Vec<Record>) {
    let mut lab = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
    let records = vec![
        lab.keep(admit(&lab, A, X, "m4")),
        lab.keep(node(&lab, X, 1)),
        lab.keep(sign(&lab, B, Body::Name { name: "bee".into() })),
        lab.keep(remove(&lab, A, C, lab.network())),
        lab.keep(admit(&lab, X, Y, "m5")),
    ];
    (lab, records)
}

/// The record store F13 keeps (`Store`, the secrets directory's `records`, which F17's fleet
/// admission reads) carries a `Node` record as it carries any other: fed in either order, its
/// genesis last, it reads back the log's membership, node keys and heads, and so does its own
/// encoding read again.
/// NEGATIVE CONTROL: a record code the store cannot read refuses the stream.
#[test]
fn the_record_store_carries_every_kind_and_reads_back_the_membership() {
    let (lab, records) = every_kind();
    assert!(records.iter().any(|r| r.kind() == Kind::Node));
    let want = (lab.membership(), lab.heads());
    for order in [records.clone(), records.iter().rev().cloned().collect()] {
        let mut store = Store::with_compiled(Some(*lab.network()));
        store.ingest(&encode_records(&order)).unwrap();
        assert!(store.log().is_none(), "no genesis, no log");
        store.ingest(lab.genesis().bytes()).unwrap();
        let log = store.log().expect("a log once its genesis came");
        assert_eq!((log.membership(), log.heads()), want);
        let mut again = Store::with_compiled(Some(*lab.network()));
        again.ingest(&store.encode()).unwrap();
        let log = again.log().expect("the encoding reads back");
        assert_eq!((log.membership(), log.heads()), want);
    }
}

#[test]
fn quorum_candidates_cost_one_authorization_pass_each_not_one_per_pair() {
    // A removes X. On a branch that never holds that removal, X lets in Y, Z and W, W signs
    // `N` names, and X, Y and Z each sign a removal of W at every one of them: `N` quorum
    // `Remove`s that each hold, which any member's sync stores for good.
    const N: usize = 24;
    let lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let mut ours = lab.clone();
    let mut theirs = lab.clone();
    let removal = remove(&ours, A, X, lab.network());
    ours.keep(removal);
    for (who, label) in [(Y, "m4"), (Z, "m5"), (W, "m6")] {
        let entry = admit(&theirs, X, who, label);
        theirs.keep(entry);
    }
    let mut names = Vec::with_capacity(N);
    for i in 0..N {
        let body = Body::Name {
            name: format!("w-{i}"),
        };
        let name = sign(&theirs, W, body);
        names.push(*theirs.keep(name).id());
    }
    for cutoff in &names {
        for by in [X, Y, Z] {
            let share = remove(&theirs, by, W, cutoff);
            theirs.keep(share);
        }
    }
    heal(&mut ours, &mut theirs);

    let membership = ours.membership();
    assert_eq!(members(&ours), ids(&[A, B]));
    assert_eq!(membership.quorums().len(), N);
    // One pass weighs every candidate apart, one per candidate another is weighed beside, and
    // one each for the quorums that take effect and for none: never one per pair.
    let records: Vec<Record> = ours.records().cloned().collect();
    let passes = super::eval::authorization_passes(&records);
    assert!(
        passes <= N + 2,
        "{passes} authorization passes for {N} quorum candidates"
    );
}

/// §4.2's Sybil quorum flood: A removes X on our branch, while on an isolated branch X admits
/// Y, Z and W, W signs `n` records (removing A and then alternating Sybil admissions and removals
/// every 8 steps, so `n / 8` distinct cutoffs change membership-record voiding while X, Y and Z
/// stay a majority at every cutoff), and X, Y and Z sign quorum removals of W at every cutoff
/// (`5 + 4n` records, `n` quorum candidates).
fn section_4_2_flood(n: usize) -> Log {
    let lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let mut ours = lab.clone();
    let mut theirs = lab.clone();
    ours.keep(remove(&ours, A, X, lab.network()));
    for (who, label) in [(Y, "m4"), (Z, "m5"), (W, "m6")] {
        let entry = admit(&theirs, X, who, label);
        theirs.keep(entry);
    }
    let mut cutoffs = Vec::with_capacity(n);
    let mut active_sybil: Option<(Mac, RecordId)> = None;
    for i in 0..n {
        let rec = if i % 8 == 7 {
            let step = i / 8;
            if step == 0 {
                remove(&theirs, W, A, lab.network())
            } else if let Some((sybil, admit_id)) = active_sybil.take() {
                remove(&theirs, W, sybil, &admit_id)
            } else {
                let tag = 100 + (step / 2);
                let sybil = Mac(u8::try_from(tag).unwrap());
                let entry = admit(&theirs, W, sybil, &format!("m{tag}"));
                let kept = *theirs.keep(entry).id();
                active_sybil = Some((sybil, kept));
                cutoffs.push(kept);
                continue;
            }
        } else {
            sign(
                &theirs,
                W,
                Body::Name {
                    name: format!("w-{i}"),
                },
            )
        };
        cutoffs.push(*theirs.keep(rec).id());
    }
    for cutoff in &cutoffs {
        for by in [X, Y, Z] {
            let share = remove(&theirs, by, W, cutoff);
            theirs.keep(share);
        }
    }
    heal(&mut ours, &mut theirs);
    ours
}

#[test]
fn a_thousand_record_sybil_quorum_flood_evaluates_without_cubic_passes() {
    // §4.2's 1,030-record shape (1 + 1 + 3 + 256 + 768 = 1,029 records, 256 quorum candidates).
    const N: usize = 256;
    let ours = section_4_2_flood(N);
    assert_eq!(ours.records().count(), 1029);

    let membership = ours.membership();
    assert_eq!(members(&ours), ids(&[A, B]));
    assert_eq!(membership.quorums().len(), N);
    let records: Vec<Record> = ours.records().cloned().collect();
    let passes = super::eval::authorization_passes(&records);
    // Only the 32 cutoffs after which W admitted another Sybil produce distinct membership-void
    // sets (`<= 34` passes total instead of `258`).
    assert!(
        passes <= (N / 8) + 2,
        "{passes} authorization passes for {N} candidates with {} membership cutoffs",
        N / 8
    );
    // Weighing the 256 candidates in pairs re-checks only what each one's voided records reach:
    // 11,917 `is_member_upto` calls where the pair loop ran 704,154.
    // NEGATIVE CONTROL: the walk re-checks every actor at every step that differs from plain:
    // 704,154 calls, and the bound fails.
    let cost = super::eval::weigh_cost(&records);
    eprintln!(
        "flood: {} records, {N} candidates, {passes} authorization passes, {} is_member_upto calls",
        records.len(),
        cost.evaluations
    );
    assert!(
        cost.evaluations <= 1029 * 1029 / 8,
        "{} is_member_upto calls weighing {N} candidates",
        cost.evaluations
    );
}

#[cfg(unix)]
mod everyone_key;
mod fold;
mod reach;
mod readers;
mod record_budgets;
mod seat_removed;
pub(super) mod signed;
pub(super) mod weigh;

/// A `[everyone]` entry.
fn everyone(name: &str, value: &[u8]) -> crate::text::Entry {
    crate::text::Entry {
        name: name.to_owned(),
        value: crate::SecretBytes::from(value),
        audience: crate::text::Audience::Everyone,
        if_absent: false,
        anchor: None,
    }
}

/// Sign `draft` as `by`, hold it in `log`, and hand it back.
fn signed_by(log: &mut Log, by: Mac, draft: Draft) -> Record {
    log.keep(draft.sign(&by.seed(), by.incarnation()).unwrap())
}

/// F21's split, as records: the genesis (A, R and Y, with `E0` for `[everyone]` wrapped to all
/// three and `S=v0` under it), A's removal of R at the genesis, and on a log that lacks that
/// removal R's epoch `E` (wrapped to all three, with `T` under it) and then Y's `S=v1` under `E`.
struct KeyedLab {
    genesis: Log,
    removal: Record,
    /// `E`, R's `Wrap`s of it to A, R and Y, and R's `Put` of `T`, in that order.
    minted: Vec<Record>,
    edit: Record,
    e0: RecordId,
    epoch: RecordId,
    key: zeroize::Zeroizing<[u8; 32]>,
}

impl KeyedLab {
    /// The genesis and then `records`, each held in turn.
    fn with(&self, records: &[&Record]) -> Log {
        let mut log = self.genesis.clone();
        for record in records {
            log.keep((*record).clone());
        }
        log
    }
    fn minted(&self) -> Vec<&Record> {
        self.minted.iter().collect()
    }
}

fn keyed_lab() -> KeyedLab {
    let mut lab = found(&[(A, "m1"), (R, "m2"), (Y, "m3")]);
    let (draft, key0) = lab
        .draft_epoch(&A.id(), 7, EpochAudience::Everyone, 1)
        .unwrap();
    let e0 = *signed_by(&mut lab, A, draft).id();
    for mac in [A, R, Y] {
        let wrap = lab
            .draft_wrap(&A.id(), 7, e0, &key0, mac.identity().recipient())
            .unwrap();
        signed_by(&mut lab, A, wrap);
    }
    let put = lab
        .draft_put(&A.id(), 7, e0, &key0, &everyone("S", b"v0"))
        .unwrap();
    signed_by(&mut lab, A, put);
    let removal = remove(&lab, A, R, lab.network());
    let mut island = lab.clone();
    let (draft, key) = island
        .draft_epoch(&R.id(), 7, EpochAudience::Everyone, 2)
        .unwrap();
    let minted_epoch = signed_by(&mut island, R, draft);
    let epoch = *minted_epoch.id();
    let mut minted = vec![minted_epoch];
    for mac in [A, R, Y] {
        let wrap = island
            .draft_wrap(&R.id(), 7, epoch, &key, mac.identity().recipient())
            .unwrap();
        minted.push(signed_by(&mut island, R, wrap));
    }
    let put = island
        .draft_put(&R.id(), 7, epoch, &key, &everyone("T", b"t"))
        .unwrap();
    minted.push(signed_by(&mut island, R, put));
    let edit = island
        .draft_put(&Y.id(), 7, epoch, &key, &everyone("S", b"v1"))
        .unwrap();
    let edit = signed_by(&mut island, Y, edit);
    KeyedLab {
        genesis: lab,
        removal,
        minted,
        edit,
        e0,
        epoch,
        key,
    }
}

/// The dependents of `epoch` in `log` (F21 rule 1), by id.
fn dependents_of(log: &Log, epoch: &RecordId) -> Vec<RecordId> {
    log.effective()
        .dependents(log.held[epoch])
        .iter()
        .map(|&d| *log.order[d].id())
        .collect()
}

/// Original keyed-epoch history, then the late Undo which changes its evaluation.
#[cfg(unix)]
pub(in crate::membership_log) fn history_reader_fixture() -> (Log, Log, RecordId) {
    let lab = keyed_lab();
    let before = lab.with(&[&[&lab.removal][..], &lab.minted()[..], &[&lab.edit][..]].concat());
    let mut after = before.clone();
    undo_shares(&mut after, A, &[R, Y]);
    (before, after, lab.epoch)
}

/// F21's rule 1, clause by clause, over [`keyed_lab`]'s records and variations of them.
///
/// - Every clause holds: `E` is keyed and Y's `Put` is its one dependent. R's `Wrap`s of `E`, all
///   in that `Put`'s past, open it, though none counts; R's own `Put` is no dependent, and `E0`,
///   which counts, is not keyed.
/// - counts(d): when A also removes Y at the genesis, Y's `Put` is cut and is no dependent.
/// - !counts(e): without A's removal `E` counts, and nothing depends on it.
/// - plain(e): an epoch Z signs, where R let Z in after R held its own removal, was signed by no
///   member, so Y's write under it is no dependent.
/// - writer(d) ≠ writer(e): R's next incarnation (R's writer key, another incarnation), let in by
///   Y on R's side, writes under `E`, and is no dependent.
/// - Good faith: Y's write under `E` once it holds A's removal is no dependent, while Y's own
///   removal of R at R's `Put` of `T`, whose cutoff reaches `E`, leaves Y's next write one.
///
/// NEGATIVE CONTROLS, one clause dropped from `Graph::depends` (or `Graph::dependents`) at a
/// time, each failing its own assertion:
/// - counts(d) ("a cut write is no dependent");
/// - !counts(e) ("an epoch that counts is not keyed");
/// - plain(e) ("an epoch no member signed is not keyed");
/// - the writer clause ("R's next incarnation is R's writer");
/// - good faith ("a write signed once its signer held the removal is no dependent").
#[test]
fn an_epoch_is_keyed_only_by_an_honest_write_from_another_writer_that_never_saw_its_cut() {
    let lab = keyed_lab();
    let minted = lab.minted();

    let healed = lab.with(&[&[&lab.removal][..], &minted[..], &[&lab.edit][..]].concat());
    let effective = healed.effective();
    let at = |id: &RecordId| healed.held[id];
    assert!(effective.keyed(at(&lab.epoch)), "E is keyed");
    assert_eq!(
        dependents_of(&healed, &lab.epoch),
        [*lab.edit.id()],
        "Y's Put is E's one dependent, R's own is none"
    );
    for wrap in &lab.minted[1..4] {
        let w = at(wrap.id());
        assert!(
            !effective.counts(w) && effective.opens_wrap(w),
            "R's Wrap in the dependent's past opens E"
        );
    }
    assert!(effective.counts(at(&lab.e0)) && !effective.keyed(at(&lab.e0)));

    let mut log = lab.with(&[&lab.removal]);
    let cut_y = remove(&log, A, Y, log.network());
    log.keep(cut_y);
    for record in minted.iter().chain([&&lab.edit]) {
        log.keep((*record).clone());
    }
    assert!(!log.effective().counts(log.held[lab.edit.id()]));
    assert!(
        dependents_of(&log, &lab.epoch).is_empty(),
        "a cut write is no dependent"
    );

    let log = lab.with(&[&minted[..], &[&lab.edit][..]].concat());
    assert!(log.effective().counts(log.held[&lab.epoch]));
    assert!(
        dependents_of(&log, &lab.epoch).is_empty(),
        "an epoch that counts is not keyed"
    );

    let mut log = lab.with(&[&lab.removal]);
    let let_in = admit(&log, R, Z, "m4");
    log.keep(let_in);
    let (draft, key) = log
        .draft_epoch(&Z.id(), 7, EpochAudience::Everyone, 2)
        .unwrap();
    let unsigned = *signed_by(&mut log, Z, draft).id();
    let put = log
        .draft_put(&Y.id(), 7, unsigned, &key, &everyone("S", b"v2"))
        .unwrap();
    let put = *signed_by(&mut log, Y, put).id();
    assert!(!log.effective().counts(log.held[&unsigned]));
    assert!(log.effective().counts(log.held[&put]));
    assert!(
        dependents_of(&log, &unsigned).is_empty(),
        "an epoch no member signed is not keyed"
    );

    let mut island = lab.with(&minted);
    let next = Entrant {
        request: Request::create(
            [2; 16],
            [3; 32],
            [R.0; 16],
            [0xEE; 16],
            &age::Identity::from_secret_bytes([0xEE; 32]),
            &R.seed(),
        )
        .unwrap(),
        node_key: [0xEE; 32],
        name: "mac-14".to_owned(),
        label: "m4".to_owned(),
    };
    let next_id = next.member();
    assert!(next_id[..32] == R.id()[..32] && next_id != R.id());
    let let_in = sign(
        &island,
        Y,
        Body::Admit {
            entrant: next,
            evidence: Evidence::Phrase,
        },
    );
    island.keep(let_in);
    let put = island
        .draft_put(&next_id, 7, lab.epoch, &lab.key, &everyone("S", b"v3"))
        .unwrap();
    let put = *island.keep(put.sign(&R.seed(), [0xEE; 16]).unwrap()).id();
    island.keep(lab.removal.clone());
    assert!(
        island.effective().counts(island.held[&put]),
        "the next incarnation's write counts"
    );
    assert!(
        dependents_of(&island, &lab.epoch).is_empty(),
        "R's next incarnation is R's writer"
    );

    let mut log = lab.with(&[&[&lab.removal][..], &minted[..]].concat());
    let late = log
        .draft_put(&Y.id(), 7, lab.epoch, &lab.key, &everyone("S", b"v4"))
        .unwrap();
    let late = *signed_by(&mut log, Y, late).id();
    assert!(log.effective().counts(log.held[&late]));
    assert!(
        dependents_of(&log, &lab.epoch).is_empty(),
        "a write signed once its signer held the removal is no dependent"
    );

    let mut island = lab.with(&minted);
    let accepted = remove(&island, Y, R, lab.minted[4].id());
    island.keep(accepted);
    let put = island
        .draft_put(&Y.id(), 7, lab.epoch, &lab.key, &everyone("S", b"v5"))
        .unwrap();
    let put = signed_by(&mut island, Y, put);
    island.keep(lab.removal.clone());
    assert_eq!(
        dependents_of(&island, &lab.epoch),
        [*put.id()],
        "a removal whose cutoff reaches E does not cut it there"
    );
}

/// One `Membership` scenario, built for real: its genesis, the records each of the model's
/// three record slots stands for, and which Macs `u`, `v`, `w` and `x` read.
struct Scenario {
    genesis: Log,
    slots: [Vec<Record>; 3],
    reads: Vec<Mac>,
    labels: bool,
    /// Scenario 7: the epoch R mints past its cutoff, read through `reads[0]`'s entries.
    keyed: Option<RecordId>,
}

/// Scenario 7: [`keyed_lab`]'s removal, R's epoch and Y's write, as the model's three slots.
fn keyed_write() -> Scenario {
    let lab = keyed_lab();
    Scenario {
        genesis: lab.genesis,
        slots: [vec![lab.removal], lab.minted, vec![lab.edit]],
        reads: vec![Y],
        labels: false,
        keyed: Some(lab.epoch),
    }
}

/// The scenarios of `aterm_models_swarm::membership_model`, by `sc` (index `sc - 1`).
fn scenarios() -> &'static [Scenario; 7] {
    static SCENARIOS: OnceLock<[Scenario; 7]> = OnceLock::new();
    SCENARIOS.get_or_init(|| {
        let alone = |log: &Log, record: Record| {
            let mut log = log.clone();
            log.keep(record);
            log
        };
        // 1: three Macs claim m5 on islands that never saw each other's admission.
        let lab = found(&[(B, "m1"), (C, "m2"), (D, "m3")]);
        let labels = Scenario {
            slots: [
                vec![admit(&lab, B, X, "m5")],
                vec![admit(&lab, C, Y, "m5")],
                vec![admit(&lab, D, Z, "m5")],
            ],
            genesis: lab,
            reads: vec![X, Y, Z],
            labels: true,
            keyed: None,
        };
        // 2: C lets X in, while B lets it in and D removes it.
        let lab = found(&[(B, "m1"), (C, "m2"), (D, "m3")]);
        let entry = admit(&lab, B, X, "m4");
        let removal = remove(&alone(&lab, entry.clone()), D, X, entry.id());
        let remove_vs_admit = Scenario {
            slots: [
                vec![admit(&lab, C, X, "m4")],
                vec![entry, removal],
                Vec::new(),
            ],
            genesis: lab,
            reads: vec![X],
            labels: false,
            keyed: None,
        };
        // 3: A and B remove each other.
        let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3")]);
        let mutual = Scenario {
            slots: [
                vec![remove(&lab, A, B, lab.network())],
                vec![remove(&lab, B, A, lab.network())],
                Vec::new(),
            ],
            genesis: lab,
            reads: vec![A, B],
            labels: false,
            keyed: None,
        };
        // 4: A removes B, while B, unaware, lets Z in.
        let lab = found(&[(A, "m1"), (B, "m2")]);
        let cutoff = Scenario {
            slots: [
                vec![remove(&lab, A, B, lab.network())],
                vec![admit(&lab, B, Z, "m3")],
                Vec::new(),
            ],
            genesis: lab,
            reads: vec![B, Z],
            labels: false,
            keyed: None,
        };
        // 5: A removes B, C and D; B and C each sign a quorum remove of A from the genesis.
        let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4")]);
        let rb = remove(&lab, A, B, lab.network());
        let after_rb = alone(&lab, rb.clone());
        let rc = remove(&after_rb, A, C, lab.network());
        let after_rc = alone(&after_rb, rc.clone());
        let rd = remove(&after_rc, A, D, lab.network());
        let after_rd = alone(&after_rc, rd.clone());
        let mass_remove = Scenario {
            slots: [
                vec![rb, rc, rd],
                vec![remove(&after_rd, B, A, lab.network())],
                vec![remove(&after_rd, C, A, lab.network())],
            ],
            genesis: lab,
            reads: vec![A, B, C, D],
            labels: false,
            keyed: None,
        };
        // 6: B removes S; S, having heard, alone signs a quorum remove of B.
        let lab = found(&[(B, "m1"), (S, "m2")]);
        let rs = remove(&lab, B, S, lab.network());
        let share = remove(&alone(&lab, rs.clone()), S, B, lab.network());
        let lone_share = Scenario {
            slots: [vec![rs], vec![share], Vec::new()],
            genesis: lab,
            reads: vec![B, S],
            labels: false,
            keyed: None,
        };
        [
            labels,
            remove_vs_admit,
            mutual,
            cutoff,
            mass_remove,
            lone_share,
            keyed_write(),
        ]
    })
}

/// Island `island`'s log in `state`: the scenario's genesis and the slots it holds, inserted
/// last slot first and each slot's records last first, so a record often waits for its parent.
fn island_log(state: &aterm_spec::interp::State, island: char) -> Log {
    let get = |k: &str| *state.get(k).unwrap_or(&0);
    let scenario = &scenarios()[usize::try_from(get("sc") - 1).unwrap()];
    let mut log = scenario.genesis.clone();
    for slot in (0..3).rev() {
        if get(&format!("{island}{}", slot + 1)) == 1 {
            for record in scenario.slots[slot].iter().rev() {
                log.insert(record.clone()).unwrap();
            }
        }
    }
    assert_eq!(log.waiting(), 0);
    log
}

/// Scenario 7's outputs on `log`, read through `reader`: `S` (1 for `v0`, 2 for `v1`), whether
/// `T` is there, whether `epoch` is newest or winning in `[everyone]`, and 0.
fn keyed_outputs(log: &Log, reader: Mac, epoch: &RecordId) -> [i64; 4] {
    let opened = log
        .open_audience_entries(&reader.identity())
        .unwrap()
        .expect("Y reads");
    let everyone = opened.by_audience.get(&crate::text::Audience::Everyone);
    let value = |name: &str| {
        everyone
            .and_then(|m| m.get(name))
            .map(|e| e.value.as_slice())
    };
    let s = match value("S") {
        Some(b"v0") => 1,
        Some(b"v1") => 2,
        other => panic!("unexpected S {other:?}"),
    };
    let newest = opened
        .maximal_epochs
        .get(&EpochAudience::Everyone)
        .is_some_and(|ids| ids.contains(epoch))
        || opened
            .winning_epochs
            .get(&EpochAudience::Everyone)
            .is_some_and(|ids| ids.contains(epoch));
    [s, i64::from(value("T").is_some()), i64::from(newest), 0]
}

/// What island `island` (`'1'` or `'2'`) evaluates in `state`, written into `next`: the real
/// evaluator's membership or labels, and each label change counted.
fn project(state: &aterm_spec::interp::State, island: char, next: &mut aterm_spec::interp::State) {
    let get = |k: &str| *state.get(k).unwrap_or(&0);
    let scenario = &scenarios()[usize::try_from(get("sc") - 1).unwrap()];
    let holds = if island == '1' { 'a' } else { 'b' };
    if let Some(epoch) = &scenario.keyed {
        let outputs = keyed_outputs(&island_log(state, holds), scenario.reads[0], epoch);
        for (out, value) in ["u", "v", "w", "x"].into_iter().zip(outputs) {
            next.insert(var(&format!("{out}{island}")), value);
        }
        return;
    }
    let membership = island_log(state, holds).membership();
    for (at, out) in ["u", "v", "w", "x"].into_iter().enumerate() {
        let output = format!("{out}{island}");
        let value = match scenario.reads.get(at) {
            None => 0,
            Some(mac) if !scenario.labels => i64::from(membership.contains(&mac.id())),
            Some(mac) => match membership.label(&mac.id()) {
                None => 0,
                Some("m5") => 1,
                Some(label) if label == mac.suffixed("m5") => 2,
                Some(label) => panic!("unexpected label {label}"),
            },
        };
        if scenario.labels && at < 3 {
            let was = get(&output);
            let count = format!("c{out}{island}");
            let changes = get(&count) + i64::from(was > 0 && value != was);
            next.insert(var(&count), changes);
        }
        next.insert(var(&output), value);
    }
}

/// The model's variable this test writes, by name.
fn var(name: &str) -> &'static str {
    const VARS: [&str; 20] = [
        "a1", "a2", "a3", "b1", "b2", "b3", "u1", "v1", "w1", "x1", "u2", "v2", "w2", "x2", "cu1",
        "cv1", "cw1", "cu2", "cv2", "cw2",
    ];
    VARS.into_iter()
        .find(|v| *v == name)
        .unwrap_or_else(|| panic!("no model variable {name}"))
}

/// Replay one abstract `Membership` transition through the real log and evaluator: a scenario
/// action evaluates the genesis on both islands, `Sign<k>` and `Deliver<k>` insert slot `k`'s
/// records into the island's real log, and `Evaluate<i>` runs the real evaluator over it.
pub(crate) fn membership_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let scenario = match action {
        "Labels" => Some(1),
        "RemoveVsAdmit" => Some(2),
        "Mutual" => Some(3),
        "Cutoff" => Some(4),
        "MassRemove" => Some(5),
        "LoneShare" => Some(6),
        "KeyedWrite" => Some(7),
        _ => None,
    };
    if let Some(sc) = scenario {
        next.insert("sc", sc);
        let fresh = next.clone();
        project(&fresh, '1', &mut next);
        project(&fresh, '2', &mut next);
        return next;
    }
    let (slot, island) = match action {
        "Sign1" => (1, 'a'),
        "Sign2" => (2, 'b'),
        "Sign3" => (3, 'b'),
        "Deliver1" => (1, 'b'),
        "Deliver2" => (2, 'a'),
        "Deliver3" => (3, 'a'),
        "Evaluate1" | "Evaluate2" => {
            let island = if action == "Evaluate1" { '1' } else { '2' };
            next.insert(if island == '1' { "d1" } else { "d2" }, 0);
            project(state, island, &mut next);
            return next;
        }
        other => panic!("unexpected enabled action in Buggy=0 model: {other}"),
    };
    let flag = var(&format!("{island}{slot}"));
    let mut log = island_log(state, island);
    let scenario = &scenarios()[usize::try_from(state["sc"] - 1).unwrap()];
    for record in &scenario.slots[slot - 1] {
        assert_eq!(log.insert(record.clone()).unwrap(), Inserted::Held);
    }
    next.insert(flag, 1);
    next.insert(if island == 'a' { "d1" } else { "d2" }, 1);
    next
}

/// Every transition the real evaluator makes from a reachable state of
/// `aterm_models_swarm::membership_model` is one the model allows, and each `Buggy` dial breaks
/// an invariant.
///
/// NEGATIVE CONTROLS (F21, scenario 7): with `Log::unwrap_keyed_epochs` opening nothing this
/// fails "conformance mismatch on Evaluate2 from {.. "sc": 7, "u2": 2 ..}" (the real evaluator
/// reads `S=v0`, `u2` 1, where the model allows only 2); with `HonestWriteOnACutEpochStands` made
/// vacuous (`sc <= 7 ||`) it fails "Buggy=8 breaks no invariant".
#[test]
fn membership_model_conforms_across_reachable_transitions_and_catches_mutants() {
    let model = aterm_models_swarm::membership_model();
    aterm_spec::interp::prove_and_catch(&model);
    for dial in model.mutant_dials() {
        let caught =
            aterm_spec::interp::violated_invariants(&aterm_spec::interp::with_buggy(&model, dial));
        assert!(!caught.is_empty(), "Buggy={dial} breaks no invariant");
    }

    let reachable = aterm_spec::interp::reachable_states(&model);
    assert!(
        reachable.len() >= 50,
        "expected a non-trivial state space, got {}",
        reachable.len()
    );
    let fired = aterm_spec::interp::fired_actions(&model);
    assert_eq!(fired.len(), model.actions.len(), "fired actions: {fired:?}");

    for state in &reachable {
        for action in &model.actions {
            if model.action_enabled(action.name, state) {
                let next = membership_transition(state, action.name);
                assert!(
                    model.successors(action.name, state).contains(&next),
                    "conformance mismatch on {} from {state:?}: {next:?}",
                    action.name
                );
            }
        }
    }

    // Negative control: an evaluator that kept A in after A and B removed each other is no
    // successor the model allows, and breaks `MutualRemovesRemoveBoth`.
    let mut split = model.init_state();
    for (var, value) in [("sc", 3), ("a1", 1), ("a2", 1), ("b1", 1), ("d1", 1)] {
        split.insert(var, value);
    }
    let mut kept = membership_transition(&split, "Evaluate1");
    assert_eq!((kept["u1"], kept["v1"]), (0, 0));
    kept.insert("u1", 1);
    assert!(!model.successors("Evaluate1", &split).contains(&kept));
    assert!(!model.check_invariant("MutualRemovesRemoveBoth", &kept));

    // Negative control (F21): island 1 holds A's removal of R, R's epoch and Y's write under it.
    // The real evaluator reads Y's `S=v1`; main's, where a cut epoch opens for nobody, read
    // `S=v0`, which is no successor the model allows and breaks `HonestWriteOnACutEpochStands`.
    let mut healed = model.init_state();
    for (var, value) in [
        ("sc", 7),
        ("a1", 1),
        ("a2", 1),
        ("a3", 1),
        ("d1", 1),
        ("u1", 1),
    ] {
        healed.insert(var, value);
    }
    let mut reverted = membership_transition(&healed, "Evaluate1");
    assert_eq!((reverted["u1"], reverted["v1"], reverted["w1"]), (2, 0, 0));
    reverted.insert("u1", 1);
    assert!(!model.successors("Evaluate1", &healed).contains(&reverted));
    assert!(!model.check_invariant("HonestWriteOnACutEpochStands", &reverted));
}

/// P1's revocation answers by causality, never by the order records arrived in. X is admitted by
/// A, and A then removes it. Concurrently B admits X2: another install (another writer key and
/// incarnation) of X's age identity, so its seat binds X's recipient. X2's `Admit` does not have
/// the `Remove` of X in its causal past, so it does not restore that recipient: it stays revoked
/// in each of the six arrival orders, and every order evaluates one membership. The rule before
/// PF1 kept the recipient when X2's entry had a higher index than X's seat, which only the
/// arrival order decided.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=revocation-arrival-order` reads the arrival
/// order as the causal order: the orders in which X2's `Admit` lands after the `Remove` restore
/// the recipient, and the asserts fail.
#[test]
fn a_concurrent_admit_of_a_removed_macs_key_keeps_it_revoked_in_every_arrival_order() {
    let lab = found(&[(A, "m1"), (B, "m2")]);
    let twin = Entrant {
        request: Request::create(
            [2; 16],
            [3; 32],
            [X.0; 16],
            [0x70; 16],
            &X.identity(),
            &[0x70; 32],
        )
        .unwrap(),
        node_key: [0x70; 32],
        name: "mac-x2".to_owned(),
        label: "m4".to_owned(),
    };
    let twin_actor = twin.request.binding().actor();
    assert_ne!(twin_actor, X.id());
    let mut one = lab.clone();
    let ax = one.keep(admit(&one, A, X, "m3"));
    let rx = one.keep(remove(&one, A, X, ax.id()));
    let mut two = lab.clone();
    let bx2 = two.keep(sign(
        &two,
        B,
        Body::Admit {
            entrant: twin,
            evidence: Evidence::Phrase,
        },
    ));
    let records = [ax, rx, bx2];
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let recipient = X.identity().recipient();
    let mut want: Option<Membership> = None;
    for order in orders {
        let mut log = lab.clone();
        for i in order {
            log.insert(records[i].clone()).unwrap();
        }
        let membership = log.membership();
        assert!(
            membership.contains(&twin_actor),
            "order {order:?}: X2 is a member"
        );
        assert!(
            membership.removed_recipients().contains(&recipient),
            "order {order:?}: X2's concurrent Admit does not restore X's recipient"
        );
        match &want {
            None => want = Some(membership),
            Some(want) => assert_eq!(&membership, want, "order {order:?}"),
        }
    }
}
