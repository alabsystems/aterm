// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age;
use std::sync::OnceLock;

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

impl Mac {
    fn seed(self) -> [u8; 32] {
        [self.0; 32]
    }
    fn incarnation(self) -> [u8; 16] {
        [self.0; 16]
    }
    fn request(self) -> Request {
        let identity = age::Identity::from_secret_bytes([self.0 ^ 0x55; 32]);
        Request::create(
            [2; 16],
            [3; 32],
            [self.0; 16],
            self.incarnation(),
            &identity,
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
            | "sybil admit-and-remove before plain removal cutoff" => {}
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

#[test]
fn a_thousand_record_sybil_quorum_flood_evaluates_without_cubic_passes() {
    // §4.2's 1,030-record shape: A removes X on our branch, while on an isolated branch X admits
    // Y, Z and W, W signs 256 records (removing A and then alternating Sybil admissions and
    // removals every 8 steps so 32 distinct cutoffs change membership-record voiding while X, Y
    // and Z stay a majority at every cutoff), and X, Y and Z sign quorum removals of W at every
    // cutoff (1 + 1 + 3 + 256 + 768 = 1,029 records, 256 quorum candidates).
    const N: usize = 256;
    let lab = found(&[(A, "m1"), (B, "m2"), (X, "m3")]);
    let mut ours = lab.clone();
    let mut theirs = lab.clone();
    ours.keep(remove(&ours, A, X, lab.network()));
    for (who, label) in [(Y, "m4"), (Z, "m5"), (W, "m6")] {
        let entry = admit(&theirs, X, who, label);
        theirs.keep(entry);
    }
    let mut cutoffs = Vec::with_capacity(N);
    let mut active_sybil: Option<(Mac, RecordId)> = None;
    for i in 0..N {
        let rec = if i % 8 == 7 {
            let step = i / 8;
            if step == 0 {
                remove(&theirs, W, A, lab.network())
            } else if let Some((sybil, admit_id)) = active_sybil.take() {
                remove(&theirs, W, sybil, &admit_id)
            } else {
                let n = 100 + (step / 2);
                let sybil = Mac(u8::try_from(n).unwrap());
                let entry = admit(&theirs, W, sybil, &format!("m{n}"));
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
}

/// One `Membership` scenario, built for real: its genesis, the records each of the model's
/// three record slots stands for, and which Macs `u`, `v`, `w` and `x` read.
struct Scenario {
    genesis: Log,
    slots: [Vec<Record>; 3],
    reads: Vec<Mac>,
    labels: bool,
}

/// The scenarios of `aterm_models_swarm::membership_model`, by `sc` (index `sc - 1`).
fn scenarios() -> &'static [Scenario; 6] {
    static SCENARIOS: OnceLock<[Scenario; 6]> = OnceLock::new();
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
        };
        [
            labels,
            remove_vs_admit,
            mutual,
            cutoff,
            mass_remove,
            lone_share,
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

/// What island `island` (`'1'` or `'2'`) evaluates in `state`, written into `next`: the real
/// evaluator's membership or labels, and each label change counted.
fn project(state: &aterm_spec::interp::State, island: char, next: &mut aterm_spec::interp::State) {
    let get = |k: &str| *state.get(k).unwrap_or(&0);
    let scenario = &scenarios()[usize::try_from(get("sc") - 1).unwrap()];
    let holds = if island == '1' { 'a' } else { 'b' };
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
}
