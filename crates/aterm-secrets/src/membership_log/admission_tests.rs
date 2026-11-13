// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::SecretBytes;
use crate::age::Identity;
use crate::commons::binding::Request;
use crate::text::{Audience, Entry};
use zeroize::Zeroizing;

const NETWORK: [u8; 16] = [0x51; 16];
const AUTHORITY: [u8; 32] = [0x52; 32];

/// A synthetic Mac: its writer seed, incarnation, age identity, actor and entrant.
struct Mac {
    seed: [u8; 32],
    incarnation: [u8; 16],
    identity: Identity,
    actor: MemberId,
    entrant: Entrant,
}

impl Mac {
    fn new(tag: u8, label: &str) -> Self {
        let seed = [tag; 32];
        let incarnation = [tag; 16];
        let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let request = Request::create(
            NETWORK,
            AUTHORITY,
            [tag.wrapping_add(0x20); 16],
            incarnation,
            &identity,
            &seed,
        )
        .expect("request");
        let actor = request.binding().actor();
        Self {
            seed,
            incarnation,
            actor,
            entrant: Entrant {
                request,
                node_key: [tag.wrapping_add(0x80); 32],
                name: format!("mac-{tag}"),
                label: label.to_owned(),
            },
            identity,
        }
    }

    fn signer(&self) -> Signer<'_> {
        Signer {
            actor: &self.actor,
            seed: &self.seed,
            incarnation: self.incarnation,
        }
    }

    fn sign(&self, log: &mut Log, ms: u64, body: Body) -> Record {
        let record = log
            .draft(&self.actor, ms, body)
            .sign(&self.seed, self.incarnation)
            .expect("sign");
        log.insert(record.clone()).expect("insert");
        record
    }

    /// Mint an `[everyone]` epoch at `generation`, wrap it to each of `to`, and return its
    /// id and key.
    fn mint(
        &self,
        log: &mut Log,
        ms: u64,
        generation: u64,
        to: &[&Mac],
    ) -> (RecordId, Zeroizing<[u8; 32]>) {
        let (draft, key) = log
            .draft_epoch(&self.actor, ms, Audience::Everyone, generation)
            .expect("epoch");
        let epoch = draft.sign(&self.seed, self.incarnation).expect("sign");
        let id = *epoch.id();
        log.insert(epoch).expect("insert epoch");
        for mac in to {
            let wrap = log
                .draft_wrap(&self.actor, ms, id, &key, mac.identity.recipient())
                .expect("wrap")
                .sign(&self.seed, self.incarnation)
                .expect("sign");
            log.insert(wrap).expect("insert wrap");
        }
        (id, key)
    }

    fn put(&self, log: &mut Log, ms: u64, epoch: (RecordId, &[u8; 32]), name: &str, value: &[u8]) {
        let entry = Entry {
            name: name.to_owned(),
            value: SecretBytes::from(value),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let put = log
            .draft_put(&self.actor, ms, epoch.0, epoch.1, &entry)
            .expect("put")
            .sign(&self.seed, self.incarnation)
            .expect("sign");
        log.insert(put).expect("insert put");
    }

    fn reads(&self, log: &Log, name: &str) -> Option<Vec<u8>> {
        log.open_entries(&self.identity)
            .expect("open")?
            .into_iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.value.as_slice().to_vec())
    }

    fn opened(&self, log: &Log) -> OpenedAudienceEntries {
        log.open_audience_entries(&self.identity)
            .expect("open")
            .expect("not removed")
    }
}

/// `m1` founds, admits `m2`, mints `[everyone]` epoch 1 for both and puts `shared`.
fn two_members() -> (Log, Mac, Mac, (RecordId, Zeroizing<[u8; 32]>)) {
    let m1 = Mac::new(1, "m1");
    let m2 = Mac::new(2, "m2");
    let genesis = Record::found(&[9; 32], vec![m1.entrant.clone()], 1_000).expect("genesis");
    let mut log = Log::new(genesis).expect("log");
    m1.sign(
        &mut log,
        1_001,
        Body::Admit {
            entrant: m2.entrant.clone(),
            evidence: Evidence::Phrase,
        },
    );
    let epoch = m1.mint(&mut log, 1_002, 1, &[&m1, &m2]);
    m1.put(&mut log, 1_003, (epoch.0, &epoch.1), "shared", b"v1");
    (log, m1, m2, epoch)
}

/// A host list names the Macs it is for, so a seat it does not wrap is no gap of it. m1 and
/// m2 hold `[everyone]`. m1 mints an epoch of the host list `[mac-1]`, wrapped to itself
/// alone, and a newer `[everyone]` epoch wrapped to itself alone. Both are newest in their
/// audience and wrap no removed Mac, and m2 is a current seat. The `[everyone]` epoch is m2's
/// gap (the rule fires), the host list's epoch is not, and there is no `SeatAudience` of a
/// host list to ask `wrap_gaps` or an admission about.
///
/// NEGATIVE CONTROL: drop `is_wrap_gap`'s `SeatAudience::of` refusal. The host list's epoch
/// is then m2's gap, as a seat audience's would be, and the test fails "a host list's epoch is
/// no gap of a seat it does not name". `wrap_gaps`' `compile_fail` doctest holds the type:
/// make `wrap_gaps` generic over `A: Clone + Into<EpochAudience>` (each converted, host lists
/// dropped), so a host list can be passed again, and that doctest fails "Test compiled
/// successfully, but it's marked `compile_fail`".
#[test]
fn a_host_list_epoch_is_no_gap_of_a_seat_it_does_not_name() {
    let (mut log, m1, m2, _) = two_members();
    let hosts = EpochAudience::Hosts(vec!["mac-1".to_owned()]);
    let (draft, key) = log
        .draft_epoch(&m1.actor, 1_010, hosts.clone(), 1)
        .expect("host-list epoch");
    let listed = draft.sign(&m1.seed, m1.incarnation).expect("sign");
    let listed_id = *listed.id();
    log.insert(listed).expect("insert");
    let wrap = log
        .draft_wrap(&m1.actor, 1_010, listed_id, &key, m1.identity.recipient())
        .expect("wrap")
        .sign(&m1.seed, m1.incarnation)
        .expect("sign");
    log.insert(wrap).expect("insert wrap");
    let (everyone, _) = m1.mint(&mut log, 1_011, 2, &[&m1]);
    let opened = m1.opened(&log);

    assert!(
        opened.maximal_epochs[&hosts].contains(&listed_id)
            && opened.unwrapped_keys.contains_key(&listed_id),
        "CONTROL: the host list's epoch is newest there, and m1 opens it"
    );
    assert!(
        log.is_wrap_gap(&opened, &everyone, &m2.actor),
        "CONTROL: an [everyone] epoch that omits m2 is its gap"
    );
    assert!(
        !log.is_wrap_gap(&opened, &listed_id, &m2.actor),
        "a host list's epoch is no gap of a seat it does not name"
    );
    let gaps = log.wrap_gaps(&opened, &admission_audiences(&opened));
    assert_eq!(
        gaps.iter()
            .map(|gap| (gap.audience.clone(), gap.epoch, gap.member))
            .collect::<Vec<_>>(),
        vec![(EpochAudience::Everyone, everyone, m2.actor)],
        "the heal names m2's [everyone] gap and nothing of the host list"
    );
    assert_eq!(
        SeatAudience::of(&hosts),
        None,
        "no seat audience of a host list"
    );
    for seat in [SeatAudience::Everyone, SeatAudience::Commons] {
        assert_eq!(SeatAudience::of(&seat.epochs()), Some(seat));
    }
}

/// The Allowing member wraps every epoch that still seals a current entry, so a Mac it lets
/// in after a rotation reads what was sealed before it, with no admin locker taking part.
///
/// NEGATIVE CONTROL: make `admission_epochs` take only `maximal_epochs` (wrap only the newest
/// epoch): the Admit carries no `Wrap` of epoch 1, which seals `old-key`, and the first
/// assert (`signed.len()`) fails.
#[test]
fn a_mac_admitted_after_a_rotation_reads_an_entry_sealed_before_it() {
    let (mut log, m1, m2, epoch1) = two_members();
    m1.put(
        &mut log,
        1_004,
        (epoch1.0, &epoch1.1),
        "old-key",
        b"sealed-before",
    );
    // The rotation: epoch 2 for both members, and a new value under it. `old-key` and
    // `shared` stay sealed under epoch 1.
    let epoch2 = m1.mint(&mut log, 1_005, 2, &[&m1, &m2]);
    m1.put(
        &mut log,
        1_006,
        (epoch2.0, &epoch2.1),
        "new-key",
        b"sealed-after",
    );

    // m2, a plain member, lets m3 in.
    let m3 = Mac::new(3, "m3");
    let opened = m2.opened(&log);
    let signed = log
        .admit(
            &m2.signer(),
            m3.entrant.clone(),
            &opened,
            &[SeatAudience::Everyone],
            1_007,
        )
        .expect("m2 admits m3");
    assert_eq!(signed[0].kind(), super::super::Kind::Admit);
    assert_eq!(
        signed.len(),
        3,
        "an Admit and one Wrap per epoch that seals a current entry"
    );
    assert!(log.membership().contains(&m3.actor));

    assert_eq!(
        m3.reads(&log, "old-key").as_deref(),
        Some(b"sealed-before".as_slice())
    );
    assert_eq!(m3.reads(&log, "shared").as_deref(), Some(b"v1".as_slice()));
    assert_eq!(
        m3.reads(&log, "new-key").as_deref(),
        Some(b"sealed-after".as_slice())
    );
    // A Mac the log never admitted reads nothing.
    assert_eq!(Mac::new(4, "m4").reads(&log, "shared"), None);
}

/// An `Admit` and `Wrap`s signed by a key no record seated let no Mac in, even when that key
/// holds a stolen epoch key: the Mac it names reads nothing, and a store refuses the records.
///
/// NEGATIVE CONTROL: in `eval.rs` `Graph::authorized`, authorize every record
/// (`authorized[i] = true`): m3 shows as a member and the first `membership()` assert fails.
#[test]
fn an_admit_signed_by_a_non_member_is_inert() {
    let (mut log, m1, _m2, epoch1) = two_members();
    let outsider = Mac::new(9, "m9");
    let m3 = Mac::new(3, "m3");
    let admit = outsider.sign(
        &mut log,
        2_000,
        Body::Admit {
            entrant: m3.entrant.clone(),
            evidence: Evidence::Phrase,
        },
    );
    let wrap = log
        .draft_wrap(
            &outsider.actor,
            2_001,
            epoch1.0,
            &epoch1.1,
            m3.identity.recipient(),
        )
        .expect("wrap")
        .sign(&outsider.seed, outsider.incarnation)
        .expect("sign");
    log.insert(wrap.clone()).expect("held, not effective");
    assert!(!log.membership().contains(&m3.actor));
    assert_eq!(m3.reads(&log, "shared"), None);
    // A member's gap rule never wraps to it: it is no member.
    assert!(
        log.wrap_gaps(&m1.opened(&log), &[SeatAudience::Everyone])
            .iter()
            .all(|gap| gap.member != m3.actor)
    );
    // A store, which requires seats, refuses both outright.
    let mut store = super::super::Store::for_vault("team").expect("store");
    assert!(store.insert(log.genesis().clone()).expect("genesis"));
    for record in log.records().skip(1) {
        if record.id() != admit.id() && record.id() != wrap.id() {
            store.insert(record.clone()).expect("a member's record");
        }
    }
    assert!(
        store.insert(admit).is_err(),
        "a store held a non-member's Admit"
    );
}

/// One-mesh proofs P2, law 4 (no lockout): two concurrent heads that each wrap a Mac removed
/// after both, with no seat holding both, are each a gap of the other side's minter, so a
/// heal makes a carrier (a seat holding every newest epoch) that can re-key. Founders m1, m2
/// and m3 hold `[everyone]` epoch 1; m1 mints epoch 2a for m1 and m3 while m2 mints epoch 2b
/// for m2 and m3; then m1 removes m3. Both heads are due (each wraps m3) and no seat holds
/// both: m1's closer names m2's gap of 2a and m2's names m1's of 2b. Once m1 heals m2 into
/// 2a, m2 holds both heads, a carrier whose re-key wraps m1, and neither names a gap.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=shipped-heal-rule` heals no omission of an
/// epoch that wraps a removed Mac (the rule before PF2): neither side names a gap, no seat
/// comes to hold both heads, and the first assert fails.
#[test]
fn two_due_heads_with_no_carrier_heal_each_other_instead_of_locking_out() {
    let (m1, m2, m3) = (Mac::new(1, "m1"), Mac::new(2, "m2"), Mac::new(3, "m3"));
    let founders = vec![m1.entrant.clone(), m2.entrant.clone(), m3.entrant.clone()];
    let genesis = Record::found(&[9; 32], founders, 1_000).expect("genesis");
    let mut base = Log::new(genesis).expect("log");
    m1.mint(&mut base, 1_001, 1, &[&m1, &m2, &m3]);
    let mut log = base.clone();
    let (a, key_a) = m1.mint(&mut log, 1_100, 2, &[&m1, &m3]);
    let mut side_b = base;
    let (b, _) = m2.mint(&mut side_b, 1_100, 2, &[&m2, &m3]);
    log.merge(&side_b).expect("merge");
    let cutoff = *log.network();
    m1.sign(
        &mut log,
        1_200,
        Body::Remove {
            member: m3.actor,
            cutoff,
        },
    );
    let gaps_of = |log: &Log, mac: &Mac| -> BTreeSet<(RecordId, MemberId)> {
        log.wrap_gaps(&mac.opened(log), &[SeatAudience::Everyone])
            .into_iter()
            .map(|gap| (gap.epoch, gap.member))
            .collect()
    };
    assert_eq!(gaps_of(&log, &m1), BTreeSet::from([(a, m2.actor)]));
    assert_eq!(gaps_of(&log, &m2), BTreeSet::from([(b, m1.actor)]));

    let heal = log
        .draft_wrap(&m1.actor, 1_300, a, &key_a, m2.identity.recipient())
        .expect("wrap")
        .sign(&m1.seed, m1.incarnation)
        .expect("sign");
    log.insert(heal).expect("insert heal");
    assert!(gaps_of(&log, &m1).is_empty());
    assert!(
        gaps_of(&log, &m2).is_empty(),
        "m2 holds both heads, so it re-keys rather than heals"
    );
}

/// A Mac admitted on one side of a split, while the other side minted a new epoch for the
/// members it knew, reads the value put under that epoch once a member that holds it sees
/// the gap after the heal. An epoch minted after its minter saw the Mac's `Admit` and left
/// it out is a gap too (one-mesh M5: only a `Remove` revokes), and an explicit `Remove`
/// ends it.
///
/// NEGATIVE CONTROL: make `wrap_gaps` return no gap (drop the wrap-gap rule): no gap names
/// m3 after the heal, so nothing wraps epoch 2 to it, and the test fails at
/// `expect("the gap")`.
#[test]
fn a_mac_admitted_on_one_side_reads_a_value_put_under_an_epoch_minted_on_the_other() {
    let (base, m1, m2, _) = two_members();
    let m3 = Mac::new(3, "m3");

    // Side A: m1 admits m3.
    let mut side_a = base.clone();
    let opened = m1.opened(&side_a);
    side_a
        .admit(
            &m1.signer(),
            m3.entrant.clone(),
            &opened,
            &[SeatAudience::Everyone],
            3_000,
        )
        .expect("m1 admits m3");
    // Side B: m2 rotates `[everyone]` for the members it knows and puts a new value.
    let mut side_b = base.clone();
    let epoch2 = m2.mint(&mut side_b, 3_001, 2, &[&m1, &m2]);
    m2.put(
        &mut side_b,
        3_002,
        (epoch2.0, &epoch2.1),
        "b-value",
        b"put-on-b",
    );

    // The heal: a union, in either order.
    let mut healed = side_a.clone();
    healed.merge(&side_b).expect("merge");
    let mut other = side_b.clone();
    other.merge(&side_a).expect("merge");
    assert_eq!(healed.heads(), other.heads());
    assert!(healed.membership().contains(&m3.actor));
    assert_eq!(
        m3.reads(&healed, "b-value"),
        None,
        "epoch 2 was never wrapped to m3"
    );

    // Every holder sees the same gap and the same ranks.
    let gaps_m1 = healed.wrap_gaps(&m1.opened(&healed), &[SeatAudience::Everyone]);
    let gaps_m2 = healed.wrap_gaps(&m2.opened(&healed), &[SeatAudience::Everyone]);
    assert_eq!(gaps_m1, gaps_m2);
    let gap = gaps_m1
        .iter()
        .find(|gap| gap.epoch == epoch2.0 && gap.member == m3.actor)
        .cloned()
        .expect("the gap");
    assert_eq!(gap.opened_ms, 3_001);
    let r1 = healed
        .gap_rank(&m1.opened(&healed), &gap, &m1.actor)
        .expect("m1 holds it");
    let r2 = healed
        .gap_rank(&m2.opened(&healed), &gap, &m2.actor)
        .expect("m2 holds it");
    assert_eq!(BTreeSet::from([r1, r2]), BTreeSet::from([0, 1]));
    assert_eq!(healed.gap_rank(&m1.opened(&healed), &gap, &m3.actor), None);

    // The rank-0 holder closes it; m3 then reads the value put on the other side.
    let closer = if r1 == 0 { &m1 } else { &m2 };
    let opened = closer.opened(&healed);
    let key = &opened.unwrapped_keys[&gap.epoch];
    let wrap = healed
        .draft_wrap(&closer.actor, 3_100, gap.epoch, key, gap.recipient)
        .expect("wrap")
        .sign(&closer.seed, closer.incarnation)
        .expect("sign");
    healed.insert(wrap).expect("insert");
    assert_eq!(
        m3.reads(&healed, "b-value").as_deref(),
        Some(b"put-on-b".as_slice())
    );
    assert!(
        healed
            .wrap_gaps(&m1.opened(&healed), &[SeatAudience::Everyone])
            .is_empty(),
        "a closed gap is closed for every member"
    );

    // m2 now knows m3 and mints epoch 3 without it. That is no revocation: m3 holds a seat
    // no `Remove` names, so epoch 3's omission of it is a gap a holder closes.
    let epoch3 = m2.mint(&mut healed, 3_200, 3, &[&m1, &m2]);
    m2.put(
        &mut healed,
        3_201,
        (epoch3.0, &epoch3.1),
        "after",
        b"m3-omitted",
    );
    let entry_before = healed.membership().get(&m3.actor).expect("m3").entry;
    assert!(healed.left_out(&m1.opened(&healed), &EpochAudience::Everyone, &m3.actor));
    let omitted = |log: &Log| {
        log.wrap_gaps(&m1.opened(log), &[SeatAudience::Everyone])
            .into_iter()
            .filter(|gap| gap.epoch == epoch3.0)
            .map(|gap| gap.member)
            .collect::<Vec<_>>()
    };
    assert_eq!(omitted(&healed), vec![m3.actor], "epoch 3 omits m3: a gap");
    assert!(healed.is_wrap_gap(&m1.opened(&healed), &epoch3.0, &m3.actor));

    // Neither m3 signing a self-Admit after epoch 3 (with a lower RecordId than its original
    // Admit) nor another member signing a redundant later Admit of m3 moves m3's standing
    // entry, and the gap stays one gap.
    for signer in [&m3, &m1] {
        let later = (3_300_u64..)
            .map(|ms| {
                healed
                    .draft(
                        &signer.actor,
                        ms,
                        Body::Admit {
                            entrant: m3.entrant.clone(),
                            evidence: super::Evidence::Phrase,
                        },
                    )
                    .sign(&signer.seed, signer.incarnation)
                    .expect("sign")
            })
            .find(|r| *r.id() < entry_before)
            .expect("a later Admit with a lower RecordId");
        healed.insert(later).expect("insert");
    }
    assert_eq!(
        healed.membership().get(&m3.actor).expect("m3").entry,
        entry_before,
        "a self-Admit or redundant later Admit moved m3's standing entry"
    );
    assert_eq!(omitted(&healed), vec![m3.actor], "still one gap");

    // An explicit `Remove` is what revokes: m3 is no current seat, and no gap names it.
    let mut removed = healed.clone();
    m1.sign(
        &mut removed,
        3_400,
        Body::Remove {
            member: m3.actor,
            cutoff: entry_before,
        },
    );
    assert!(
        !removed
            .current_recipients(&EpochAudience::Everyone)
            .contains(&m3.identity.recipient())
    );
    assert!(
        omitted(&removed).is_empty(),
        "a removed Mac is never healed"
    );
    assert!(!removed.is_wrap_gap(&m1.opened(&removed), &epoch3.0, &m3.actor));

    // NEGATIVE CONTROL: 0.154.0's rule read epoch 3's omission as m3's revocation.
    OLD_GAP_RULE.with(|on| on.set(true));
    let old = omitted(&healed);
    OLD_GAP_RULE.with(|on| on.set(false));
    assert!(old.is_empty(), "the old rule heals no omission: {old:?}");
}

/// A label held at `m99999` (a Mac that asked under it, or a roster row above it) leaves the
/// next Mac a label an `Admit` can carry: the lowest one nothing holds, not `m100000`, which
/// `is_label` refuses and which failed every later Allow on that log.
///
/// NEGATIVE CONTROL: make `next_label` return the one above the highest whatever it is:
/// `"m100000"` comes back and the `is_label` assert fails.
#[test]
fn a_label_held_at_m99999_still_leaves_one_an_admit_can_carry() {
    let (log, _m1, _m2, _) = two_members();
    for high in ["m99999", "m100000"] {
        let taken = BTreeSet::from([high.to_owned()]);
        let next = log
            .next_label("Andrews-MacBook-Air", &taken, &BTreeSet::new())
            .expect("a free label");
        assert!(is_label(&next), "{high}: {next}");
        assert_eq!(next, "m3", "m1 founds and m2 is admitted");
    }
    let all: BTreeSet<String> = (1..=99_999_u32).map(|n| format!("m{n}")).collect();
    assert_eq!(
        log.next_label("m5", &all, &BTreeSet::new()),
        None,
        "every m<N> is held"
    );
}

/// C11 under F15: the allowing member keeps the label a Mac asks under when nothing holds it,
/// and otherwise gives the one above the highest label the log or the roster holds.
#[test]
fn the_allowing_member_assigns_the_next_free_label() {
    let (mut log, m1, _m2, _) = two_members();
    let none = BTreeSet::new();
    assert_eq!(
        log.next_label("m40", &none, &BTreeSet::new()).as_deref(),
        Some("m40")
    );
    assert_eq!(
        log.next_label("M40", &none, &BTreeSet::new()).as_deref(),
        Some("m40")
    );
    assert_eq!(
        log.next_label("m2", &none, &BTreeSet::new()).as_deref(),
        Some("m3"),
        "m2 is held by an Admit"
    );
    assert_eq!(
        log.next_label("Andrews-MacBook-Air", &none, &BTreeSet::new())
            .as_deref(),
        Some("m3")
    );
    let roster = BTreeSet::from(["m55".to_owned(), "m3".to_owned()]);
    assert_eq!(
        log.next_label("m3", &roster, &BTreeSet::new()).as_deref(),
        Some("m56")
    );
    assert_eq!(
        log.next_label("m40", &roster, &BTreeSet::new()).as_deref(),
        Some("m40")
    );
    // A sheet's number is counted toward the next label and holds none by itself.
    let sheet = BTreeSet::from(["m70".to_owned()]);
    assert_eq!(log.next_label("mac", &none, &sheet).as_deref(), Some("m71"));
    assert_eq!(log.next_label("m70", &none, &sheet).as_deref(), Some("m70"));
    // A removed Mac's label stays held.
    let m7 = Mac::new(7, "m7");
    let admit = m1.sign(
        &mut log,
        4_000,
        Body::Admit {
            entrant: m7.entrant.clone(),
            evidence: Evidence::Phrase,
        },
    );
    m1.sign(
        &mut log,
        4_001,
        Body::Remove {
            member: m7.actor,
            cutoff: *admit.id(),
        },
    );
    assert!(!log.membership().contains(&m7.actor));
    assert_eq!(
        log.next_label("m7", &none, &BTreeSet::new()).as_deref(),
        Some("m8")
    );
    // A removed Mac's own `Admit` (for another Mac or itself) claims no label and cannot
    // squat `m40` or `m99999`.
    let m99 = Mac::new(99, "m99999");
    m7.sign(
        &mut log,
        4_002,
        Body::Admit {
            entrant: m99.entrant,
            evidence: Evidence::Phrase,
        },
    );
    let mut self_m40 = m7.entrant.clone();
    self_m40.label = "m40".to_owned();
    m7.sign(
        &mut log,
        4_003,
        Body::Admit {
            entrant: self_m40,
            evidence: Evidence::Phrase,
        },
    );
    assert_eq!(
        log.next_label("m40", &none, &BTreeSet::new()).as_deref(),
        Some("m40")
    );
    assert_eq!(
        log.next_label("Andrews-MacBook-Air", &none, &BTreeSet::new())
            .as_deref(),
        Some("m8")
    );
}

/// `m1` founds and admits `m2`..`m<n>`, then mints `[everyone]` epoch 1 for every seat.
fn lab(n: u8) -> (Log, Vec<Mac>) {
    let macs: Vec<Mac> = (1..=n)
        .map(|tag| Mac::new(tag, &format!("m{tag}")))
        .collect();
    let genesis = Record::found(&[9; 32], vec![macs[0].entrant.clone()], 1_000).expect("genesis");
    let mut log = Log::new(genesis).expect("log");
    for (ms, mac) in (1_001..).zip(&macs[1..]) {
        macs[0].sign(
            &mut log,
            ms,
            Body::Admit {
                entrant: mac.entrant.clone(),
                evidence: Evidence::Phrase,
            },
        );
    }
    let all: Vec<&Mac> = macs.iter().collect();
    macs[0].mint(&mut log, 1_500, 1, &all);
    (log, macs)
}

/// When each healer first saw each gap, in rank steps: `(healer, epoch, recipient)`.
type Sight = std::collections::BTreeMap<(MemberId, RecordId, Recipient), usize>;

/// One rank step of the heal on partial views, by `close_wrap_gaps`' rule: each Mac of
/// `healers` looks at its own copy of `log` at once and signs each gap it holds a rank `k`
/// for once `step` is `k` steps past when it first saw that gap (`sight`); then every Mac
/// takes every other's records. With `unranked` every holder signs every gap it sees, the
/// closer's negative control. Returns the `Wrap`s signed.
fn heal_step(
    log: &mut Log,
    healers: &[&Mac],
    sight: &mut Sight,
    step: usize,
    unranked: bool,
) -> usize {
    let mut copies = Vec::new();
    let mut signed = 0;
    for healer in healers {
        let mut copy = log.clone();
        let opened = healer.opened(&copy);
        for gap in copy.wrap_gaps(&opened, &[SeatAudience::Everyone]) {
            let first = *sight
                .entry((healer.actor, gap.epoch, gap.recipient))
                .or_insert(step);
            let due = unranked
                || copy
                    .gap_rank(&opened, &gap, &healer.actor)
                    .is_some_and(|rank| step >= first + rank);
            if !due {
                continue;
            }
            let key = &opened.unwrapped_keys[&gap.epoch];
            let wrap = copy
                .draft_wrap(&healer.actor, 9_000, gap.epoch, key, gap.recipient)
                .expect("wrap")
                .sign(&healer.seed, healer.incarnation)
                .expect("sign");
            copy.insert(wrap).expect("insert");
            signed += 1;
        }
        copies.push(copy);
    }
    for copy in &copies {
        log.merge(copy).expect("merge");
    }
    signed
}

/// Heal step by step until no gap is open, at most `steps`; returns the `Wrap`s signed.
fn heal(log: &mut Log, healers: &[&Mac], steps: usize) -> usize {
    let mut sight = Sight::new();
    (0..steps)
        .map(|step| heal_step(log, healers, &mut sight, step, false))
        .sum()
}

/// The `Wrap`s of `epoch` the log holds, by recipient.
fn wraps_of(log: &Log, epoch: &RecordId) -> std::collections::BTreeMap<Recipient, usize> {
    let mut out = std::collections::BTreeMap::new();
    for record in log.records() {
        if let Body::Wrap {
            epoch: of,
            recipient,
            ..
        } = record.body()
            && of == epoch
        {
            *out.entry(*recipient).or_insert(0) += 1;
        }
    }
    out
}

/// The 2026-11-03 replay (one-mesh §2.1, M5). 88 seats hold `[everyone]` epoch 1; `m4`, holding
/// only a prefix of a batch, re-keys to epoch 2 wrapped to `m1`..`m3` and not even to itself,
/// and puts a value under it; `m1` then removes `m88` with an explicit `Remove`. Every holder
/// of epoch 2 names the same 84 gaps (every current seat it omits), each gap has one holder
/// ranked first, and a rank step at a time the holders heal it with one `Wrap` per gap, on
/// partial views, after which every current seat reads the value. A seat that does not hold
/// epoch 2 heals nothing, and the removed `m88` is never wrapped.
///
/// An epoch's key, and so its id and every gap's rank order, is fresh on each run, so the
/// replay runs on several.
///
/// NEGATIVE CONTROL: with `OLD_GAP_RULE` on (0.154.0: an omission the minter knew is a
/// revocation) no holder names a gap, and 85 seats stay out, as Commons epoch 66 did; with
/// the minter ranked among the holders though it holds no `Wrap` (0.154.0's holder set),
/// some run signs two `Wrap`s for a gap the minter ranked first.
#[test]
fn eighty_eight_seats_wrapped_to_three_heal_one_wrap_per_gap() {
    for _ in 0..6 {
        eighty_eight_replay();
    }
}

fn eighty_eight_replay() {
    let (mut log, macs) = lab(88);
    let wrapped: Vec<&Mac> = macs[..3].iter().collect();
    let epoch2 = macs[3].mint(&mut log, 2_000, 2, &wrapped);
    macs[3].put(&mut log, 2_001, (epoch2.0, &epoch2.1), "after", b"epoch-2");
    let removed = &macs[87];
    let cutoff = log.membership().get(&removed.actor).expect("m88").entry;
    macs[0].sign(
        &mut log,
        2_002,
        Body::Remove {
            member: removed.actor,
            cutoff,
        },
    );

    let current = log.current_recipients(&EpochAudience::Everyone);
    assert_eq!(current.len(), 87, "88 seats less one `Remove`");
    assert!(!current.contains(&removed.identity.recipient()));
    let omitted: BTreeSet<MemberId> = macs[3..87].iter().map(|mac| mac.actor).collect();
    for holder in &wrapped {
        let gaps = log.wrap_gaps(&holder.opened(&log), &[SeatAudience::Everyone]);
        assert_eq!(
            gaps.iter().map(|gap| gap.member).collect::<BTreeSet<_>>(),
            omitted,
            "every current seat epoch 2 omits is a gap, the minter's own included"
        );
        assert!(gaps.iter().all(|gap| gap.epoch == epoch2.0));
    }
    // A seat that does not hold epoch 2 heals nothing and ranks for nothing.
    let outside = &macs[10];
    let opened = outside.opened(&log);
    assert!(
        log.wrap_gaps(&opened, &[SeatAudience::Everyone]).is_empty(),
        "a non-holder names no gap it could close"
    );
    let any_gap = log.wrap_gaps(&macs[0].opened(&log), &[SeatAudience::Everyone])[0].clone();
    assert_eq!(log.gap_rank(&opened, &any_gap, &outside.actor), None);

    // NEGATIVE CONTROL: the old rule leaves the 85 out.
    OLD_GAP_RULE.with(|on| on.set(true));
    let old = log.wrap_gaps(&macs[0].opened(&log), &[SeatAudience::Everyone]);
    OLD_GAP_RULE.with(|on| on.set(false));
    assert!(old.is_empty(), "the old rule heals nothing: {old:?}");

    // The heal, a rank step at a time, every current seat looking: the minter `m4` cannot
    // open epoch 2, so it ranks for nothing until healed, and a seat healed ranks behind the
    // batch's holders, so the three holders' ranks hold still for the whole heal.
    let current_macs: Vec<&Mac> = macs[..87].iter().collect();
    let signed = heal(&mut log, &current_macs, 3);
    assert_eq!(signed, omitted.len(), "one `Wrap` per gap");
    let wraps = wraps_of(&log, &epoch2.0);
    assert!(wraps.values().all(|&n| n == 1), "one `Wrap` per recipient");
    assert!(!wraps.contains_key(&removed.identity.recipient()));
    for holder in &wrapped {
        assert!(
            log.wrap_gaps(&holder.opened(&log), &[SeatAudience::Everyone])
                .is_empty()
        );
    }
    for mac in &macs[..87] {
        assert_eq!(
            mac.reads(&log, "after").as_deref(),
            Some(b"epoch-2".as_slice()),
            "{} reads under the healed epoch",
            mac.entrant.label
        );
        assert!(!log.left_out(&macs[0].opened(&log), &EpochAudience::Everyone, &mac.actor));
    }
    assert_eq!(
        removed.reads(&log, "after"),
        None,
        "a removed Mac is never healed"
    );
}

/// N concurrent healers produce one heal per gap (one-mesh M5, item 6 of A1): 8 holders of
/// an epoch that omits 12 of 20 seats each look at the gaps at the same instant on their own
/// copy of the log; exactly one ranks first for each gap, so the step signs 12 `Wrap`s and
/// the next steps sign none.
///
/// NEGATIVE CONTROL: every holder ranked first (as `remove::UNRANKED` does to the closer)
/// signs 8 `Wrap`s per gap, 96 in all, past the log's one per (gap, epoch).
#[test]
fn eight_concurrent_healers_sign_one_wrap_per_gap() {
    let (mut log, macs) = lab(20);
    let holders: Vec<&Mac> = macs[..8].iter().collect();
    let epoch2 = macs[0].mint(&mut log, 2_000, 2, &holders);
    let mut unranked = log.clone();
    let everyone: Vec<&Mac> = macs.iter().collect();

    let mut sight = Sight::new();
    let first = heal_step(&mut log, &everyone, &mut sight, 0, false);
    assert_eq!(first, 12, "one holder ranks first for each gap");
    for step in 1..8 {
        assert_eq!(
            heal_step(&mut log, &everyone, &mut sight, step, false),
            0,
            "step {step}"
        );
    }
    let wraps = wraps_of(&log, &epoch2.0);
    assert_eq!(wraps.len(), 20);
    assert!(wraps.values().all(|&n| n == 1), "{wraps:?}");

    assert_eq!(
        heal_step(&mut unranked, &holders, &mut Sight::new(), 0, true),
        96
    );
    assert!(wraps_of(&unranked, &epoch2.0).values().any(|&n| n == 8));
}
