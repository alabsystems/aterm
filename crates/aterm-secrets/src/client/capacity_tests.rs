// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! P6, the membership log cannot wedge (`docs/design/one-mesh-proofs/P6-log-capacity.md`, and
//! the proof plan's review, which corrects it): a removal reserves the room, in records and in
//! bytes, of every re-key a held `Remove` owes that some Mac can sign, and a heal and the
//! tender's re-keys no removal made due keep that room free, so the re-key fits when its ranked
//! minter signs it. A proof admission and a person's other writes weigh only the log's hard bound
//! (P6's Calm premises F3 and F4). Each test runs the shipped code against a log a few records or
//! bytes from its bound, with the budget scaled ([`keyed_budget`],
//! [`crate::membership_log::RecordBudgets::within_bytes`]), and the rule before it as its
//! control.

use super::*;

/// The lab of P6's capacity tests ([`host_list_lab`] of the ring, `[m22 m40]`): m22, the ring's
/// one Mac but m40, is its carrier.
struct RingLab {
    macs: TwoMacs,
    rec40: Recipient,
    rec41: Recipient,
    ring: EpochAudience,
}

fn ring_lab(value: &[u8]) -> RingLab {
    host_list_lab(&["m22", "m40"], value)
}

/// m22 and m7, then m40 and m41, whom m7 lets in; m22 puts `ring-cred` (`value`) in the host
/// list of `hosts`, which m7 is not in, so m7 never opens or mints it. m22 need not be in it: an
/// admin writing a host list it is not in holds no key of it.
fn host_list_lab(hosts: &[&str], value: &[u8]) -> RingLab {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (_m40, _, rec40) = let_in(&macs, "m40", "m40");
    let (_m41, _, rec41) = let_in(&macs, "m41", "m41");
    sync(&macs.m22, None).expect("m22 syncs the admissions");
    let hosts = Audience::Hosts(hosts.iter().map(|host| (*host).to_owned()).collect());
    let admin = admin_open(&macs.m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&hosts, "ring-cred", value).expect("ring");
    admin_publish(&macs.m22, &admin, &doc).expect("publish the ring");
    sync(&macs.m7, None).expect("m7 syncs");
    let ring = EpochAudience::from(&hosts);
    let identity = read_identity(&macs.m7).unwrap().unwrap();
    let opened = log_of(&macs.m7)
        .open_audience_entries(&identity)
        .unwrap()
        .unwrap();
    assert!(
        !log_entries::opens_newest(&opened, &ring),
        "CONTROL: m7 does not open the ring"
    );
    RingLab {
        macs,
        rec40,
        rec41,
        ring,
    }
}

/// The bound of one re-key of the ring over m7's log ([`log_entries::rekey_bound`]): no seat is
/// carried into a host list, and every recipient the log removed, with `also`, is gone.
fn ring_bound(m7: &Paths, ring: &EpochAudience, also: &[Recipient]) -> log_entries::RekeyCost {
    let log = log_of(m7);
    let identity = read_identity(m7).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let (_, me, _) = member_key(m7);
    let flight = log_entries::Flight::of_batch(&log, &opened, &me);
    let mut gone = log.removed_recipients();
    gone.extend(also.iter().copied());
    log_entries::rekey_bound(&log, &opened, &flight, ring, &BTreeSet::new(), &gone, None)
}

/// The rotation of `ring` a held `Remove` made due, as the log at `paths` reads it, if any.
fn ring_due(paths: &Paths, ring: &EpochAudience) -> Option<crate::membership_log::RekeyDue> {
    log_of(paths)
        .rekey_dues()
        .into_iter()
        .find(|due| due.audience == *ring && due.rotation)
}

/// The fill numbers (held, batch, worst, limit) of m7's removal of m41, in a keyed budget of what
/// m7's log holds, which refuses it; `own_target_only` sets `remove::FILL_OWN_TARGET_ONLY` and
/// `uncarried` `log_entries::OWED_UNCARRIED`.
fn m41_fill(lab: &RingLab, own_target_only: bool, uncarried: bool) -> Vec<usize> {
    let m7 = &lab.macs.m7;
    let keyed = log_of(m7).counts().keyed;
    remove::FILL_OWN_TARGET_ONLY.with(|cell| cell.set(own_target_only));
    log_entries::OWED_UNCARRIED.with(|cell| cell.set(uncarried));
    let refused = keyed_budget(keyed).within(|| remove_member(m7, &join_code(&lab.rec41), false));
    remove::FILL_OWN_TARGET_ONLY.with(|cell| cell.set(false));
    log_entries::OWED_UNCARRIED.with(|cell| cell.set(false));
    numbers_of(&refused.expect_err("the removal refuses").to_string())
}

/// P6 F1: a removal reserves the re-keys every held `Remove` made due, not only its own target's.
/// m7 removes m40, which leaves the ring due a rotation m7 cannot mint (m22 owes it); m7's next
/// removal, of m41 (in `[everyone]` and Commons, which m7 mints, and not in the ring), reserves
/// that rotation and its coalesce too: its fill's worst case passes its batch by twice the
/// ring's re-key, in a budget that refuses it.
///
/// NEGATIVE CONTROL: `remove::FILL_OWN_TARGET_ONLY` (the fill counts only audiences that wrap its
/// own target, as shipped before P6), run in the test: the worst case equals the batch, and the
/// ring's owed re-keys are uncounted.
#[test]
fn a_removal_reserves_the_rekeys_an_earlier_removal_made_due() {
    let lab = ring_lab(b"ring-v1");
    let m7 = &lab.macs.m7;
    remove_member(m7, &join_code(&lab.rec40), false).expect("m7 removes m40");
    assert!(
        ring_due(m7, &lab.ring).is_some_and(|due| !due.carriers.is_empty()),
        "CONTROL: the ring is due the rotation m7 cannot mint, and m22 carries it"
    );
    let ring = ring_bound(m7, &lab.ring, &[lab.rec41]).keyed;
    assert!(ring > 0);
    let shipped = m41_fill(&lab, true, false);
    let [_, batch, worst, _] = shipped.as_slice() else {
        panic!("the fill's four numbers: {shipped:?}");
    };
    assert_eq!(
        worst, batch,
        "CONTROL: a fill of its own target's audiences leaves the ring out: {shipped:?}"
    );
    let reserved = m41_fill(&lab, false, false);
    let [_, batch, worst, _] = reserved.as_slice() else {
        panic!("the fill's four numbers: {reserved:?}");
    };
    assert_eq!(
        worst - batch,
        2 * ring,
        "two re-keys of the ring m40's removal made due: {reserved:?}, the ring's bound {ring}"
    );
    lab.macs.server.stop();
}

/// The lab of P6's heal tests ([`host_list_lab`] of `hosts` and `value`): m7 removes m40, which
/// leaves the list due a rotation; then m22 re-keys `[everyone]` without m41, a current seat: a
/// gap m22 and m7 each hold the epoch of and can heal, from the same records. Returns the lab,
/// m7's keyed records and what the list's re-keys would owe (twice its bound).
fn heal_lab(hosts: &[&str], value: &[u8]) -> (RingLab, usize, log_entries::RekeyCost) {
    let lab = host_list_lab(hosts, value);
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    remove_member(m7, &join_code(&lab.rec40), false).expect("m7 removes m40");
    save_records(m22, &load_records(m7).unwrap().encode()).unwrap();
    let (rec40, rec41) = (lab.rec40, lab.rec41);
    convergence::rekey(m22, &EpochAudience::Everyone, |r| {
        *r != rec40 && *r != rec41
    });
    save_records(m7, &load_records(m22).unwrap().encode()).unwrap();
    let log = log_of(m7);
    let identity = read_identity(m7).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let audiences = crate::membership_log::admission_audiences(&opened);
    let gaps = log.wrap_gaps(&opened, &audiences);
    assert!(
        gaps.iter().any(|gap| gap.recipient == rec41),
        "CONTROL: m41 is a gap of m22's epoch: {gaps:?}"
    );
    assert!(
        ring_due(m7, &lab.ring).is_some(),
        "CONTROL: the list is still due"
    );
    let need = ring_bound(m7, &lab.ring, &[]).times(2);
    let keyed = log.counts().keyed;
    (lab, keyed, need)
}

/// The gap closer at `paths` at `at`, in a keyed budget of `limit` and a byte bound of `bytes`
/// where each is given. Returns what it signed.
fn look(paths: &Paths, at: u64, limit: Option<usize>, bytes: Option<usize>) -> Vec<Record> {
    let close = || log_entries::close_wrap_gaps(paths, at).expect("the gap closer");
    let keyed = || match limit {
        Some(limit) => keyed_budget(limit).within(close),
        None => close(),
    };
    match bytes {
        Some(bytes) => crate::membership_log::RecordBudgets::within_bytes(bytes, keyed),
        None => keyed(),
    }
}

/// m7's gap closer in a keyed budget of `limit` and a byte bound of `bytes` where each is given,
/// `ignores_need` setting `log_entries::HEAL_IGNORES_NEED`: a first look at `t0` (where a first
/// rank heals at once) and one 40 steps on, when every rank is due. Returns what both signed.
fn heal(
    m7: &Paths,
    t0: u64,
    limit: Option<usize>,
    bytes: Option<usize>,
    ignores_need: bool,
) -> Vec<Record> {
    log_entries::HEAL_IGNORES_NEED.with(|cell| cell.set(ignores_need));
    let mut signed = look(m7, t0, limit, bytes);
    signed.extend(look(m7, t0 + 40 * log_entries::GAP_STEP_MS, limit, bytes));
    log_entries::HEAL_IGNORES_NEED.with(|cell| cell.set(false));
    signed
}

/// Whether `signed` is the one `Wrap` that heals m41's gap.
fn heals_m41(signed: &[Record], rec41: Recipient) -> bool {
    matches!(signed, [record] if matches!(record.body(),
        Body::Wrap { recipient, .. } if *recipient == rec41))
}

/// P6 F4: a heal keeps free the room of the re-keys a held `Remove` made due ([`heal_lab`]). With
/// the log's keyed records and the ring's owed re-keys filling the budget exactly, m7's gap closer
/// signs nothing, and the gap stays open; once a record lands (one record of budget more stands
/// in for the room the ring's rotation would leave) it signs the one `Wrap`, to m41.
///
/// NEGATIVE CONTROL: `log_entries::HEAL_IGNORES_NEED` (the heal weighs only the hard bound, as
/// shipped before P6), run in the test on a lab of its own: at the exact fill the heal signs its
/// `Wrap` into the room the ring's rotation needs.
#[test]
fn a_heal_keeps_the_room_a_removal_made_due() {
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let (lab, keyed, need) = heal_lab(&["m22", "m40"], b"ring-v1");
    let full = Some(keyed + need.keyed);
    assert_eq!(
        heal(&lab.macs.m7, t0, full, None, true).len(),
        1,
        "CONTROL: a heal that weighs only the hard bound signs into the reserve"
    );
    lab.macs.server.stop();

    let (lab, keyed, need) = heal_lab(&["m22", "m40"], b"ring-v1");
    let m7 = &lab.macs.m7;
    assert!(
        heal(m7, t0, full, None, false).is_empty(),
        "the heal keeps the {} records the ring's re-keys owe free at {keyed} of {}",
        need.keyed,
        keyed + need.keyed
    );
    // A record lands, as the ring's rotation would: the closer looks again.
    log_entries::gap_mark_after_a_record(m7);
    let signed = heal(m7, t0, Some(keyed + need.keyed + 1), None, false);
    assert!(
        heals_m41(&signed, lab.rec41),
        "CONTROL: one record more of budget and the heal signs m41's Wrap: {signed:?}"
    );
    lab.macs.server.stop();
}

/// P6 F4 in bytes: a heal keeps free the bytes the ring's owed re-keys re-put. The ring holds a
/// 256 KiB value; with the log's bytes and twice the ring's re-key bytes filling the byte bound
/// exactly (records to spare), m7's gap closer signs nothing; with one `Wrap`'s most bytes more
/// it signs m41's.
///
/// NEGATIVE CONTROL: `log_entries::HEAL_IGNORES_NEED`, run in the test on a lab of its own: at the
/// exact byte fill the heal signs into the ring's bytes.
#[test]
fn a_heal_keeps_the_bytes_a_removal_made_due() {
    let value = vec![b'r'; 256 * 1024];
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let (lab, _, need) = heal_lab(&["m22", "m40"], &value);
    let m7 = &lab.macs.m7;
    assert!(
        need.bytes >= 2 * value.len(),
        "the ring re-puts its value twice: {need:?}"
    );
    let full = Some(log_of(m7).held_bytes() + need.bytes);
    assert_eq!(
        heal(m7, t0, None, full, true).len(),
        1,
        "CONTROL: a heal that weighs only the hard bound signs into the ring's bytes"
    );
    lab.macs.server.stop();

    let (lab, _, need) = heal_lab(&["m22", "m40"], &value);
    let m7 = &lab.macs.m7;
    let full = log_of(m7).held_bytes() + need.bytes;
    assert!(
        heal(m7, t0, None, Some(full), false).is_empty(),
        "the heal keeps the ring's {} bytes free",
        need.bytes
    );
    log_entries::gap_mark_after_a_record(m7);
    let wrap = crate::membership_log::Kind::Wrap.most_bytes();
    let signed = heal(m7, t0, None, Some(full + wrap), false);
    assert!(
        heals_m41(&signed, lab.rec41),
        "CONTROL: a Wrap's most bytes more and the heal signs m41's: {signed:?}"
    );
    lab.macs.server.stop();
}

/// P6, a heal held for room: its gap ranks again from the look that finds room, as a gap whose
/// batch has not settled waits. m22 and m7 both hold m22's `[everyone]` epoch, which m41 lacks
/// ([`heal_lab`]); both look at `t0` with the ring's room owed, and hold the heal. Forty steps
/// on, a record lands and each has room for one `Wrap`: the first rank heals, and the second,
/// whose step from that look has not passed, signs nothing.
///
/// NEGATIVE CONTROL: `log_entries::HEAL_HOLD_KEEPS_RANKS` (a held gap keeps its ranks from first
/// sight, as the first F4 fix did), run in the test on a lab of its own: every rank passed during
/// the hold, and both Macs sign the same `Wrap`.
#[test]
fn a_heal_held_for_room_ranks_again_when_the_room_returns() {
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let later = t0 + 40 * log_entries::GAP_STEP_MS;
    let signed = |keeps_ranks: bool| {
        let (lab, keyed, need) = heal_lab(&["m22", "m40"], b"ring-v1");
        let macs = [&lab.macs.m22, &lab.macs.m7];
        log_entries::HEAL_HOLD_KEEPS_RANKS.with(|cell| cell.set(keeps_ranks));
        for paths in macs {
            assert!(
                look(paths, t0, Some(keyed + need.keyed), None).is_empty(),
                "the heal is held for the ring's room"
            );
        }
        let mut signed = Vec::new();
        for paths in macs {
            log_entries::gap_mark_after_a_record(paths);
            signed.extend(look(paths, later, Some(keyed + need.keyed + 1), None));
        }
        log_entries::HEAL_HOLD_KEEPS_RANKS.with(|cell| cell.set(false));
        lab.macs.server.stop();
        signed
    };
    assert_eq!(
        signed(true).len(),
        2,
        "CONTROL: ranks kept through the hold are all due once the room returns"
    );
    let healed = signed(false);
    assert_eq!(healed.len(), 1, "one holder heals m41: {healed:?}");
}

/// P6, a due no Mac can sign owes no room. m22 writes `ring-cred` to `[m40]`, a host list it is
/// not in; once m7 removes m40, `[m40]` is due a rotation nobody carries. m7's next removal (of
/// m41) reserves nothing for it, and m7's heal signs m41's `Wrap` with the log's keyed records
/// and the list's would-be re-keys filling the budget.
///
/// NEGATIVE CONTROL: `log_entries::OWED_UNCARRIED` (an uncarried due owes room, as the first F1
/// and F4 fixes had it), run in the test: the removal's worst case passes its batch by twice the
/// list's re-key, and the heal is held for a re-key no Mac will sign.
#[test]
fn a_due_no_mac_carries_owes_no_room() {
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let lab = host_list_lab(&["m40"], b"orphan-v1");
    let m7 = &lab.macs.m7;
    remove_member(m7, &join_code(&lab.rec40), false).expect("m7 removes m40");
    assert!(
        ring_due(m7, &lab.ring).is_some_and(|due| due.carriers.is_empty()),
        "CONTROL: [m40] is due a rotation no Mac carries"
    );
    let list = ring_bound(m7, &lab.ring, &[lab.rec41]).keyed;
    assert!(list > 0);
    let uncarried = m41_fill(&lab, false, true);
    let [_, batch, worst, _] = uncarried.as_slice() else {
        panic!("the fill's four numbers: {uncarried:?}");
    };
    assert_eq!(
        worst - batch,
        2 * list,
        "CONTROL: a fill that counts uncarried dues reserves [m40]'s re-keys: {uncarried:?}"
    );
    let carried = m41_fill(&lab, false, false);
    let [_, batch, worst, _] = carried.as_slice() else {
        panic!("the fill's four numbers: {carried:?}");
    };
    assert_eq!(worst, batch, "nothing is reserved for [m40]: {carried:?}");
    lab.macs.server.stop();

    let (lab, keyed, need) = heal_lab(&["m40"], b"orphan-v1");
    log_entries::OWED_UNCARRIED.with(|cell| cell.set(true));
    let held = heal(&lab.macs.m7, t0, Some(keyed + need.keyed), None, false);
    log_entries::OWED_UNCARRIED.with(|cell| cell.set(false));
    assert!(
        held.is_empty(),
        "CONTROL: the heal is held for [m40]: {held:?}"
    );
    lab.macs.server.stop();

    let (lab, keyed, need) = heal_lab(&["m40"], b"orphan-v1");
    let signed = heal(&lab.macs.m7, t0, Some(keyed + need.keyed), None, false);
    assert!(
        heals_m41(&signed, lab.rec41),
        "the heal signs m41's Wrap: {signed:?}"
    );
    lab.macs.server.stop();
}

/// P6 F2 and F6: the tender's re-key no removal made due keeps the owed room free, in bytes as in
/// records. m7 removes m40, which leaves the ring (256 KiB of value) due a rotation m22 owes;
/// then m22 re-keys `[everyone]` without m41 and m7 re-keys it without m40 alone, each from the
/// same log: two heads no removal left, a coalesce m7 carries. With the log's bytes and the
/// ring's owed bytes filling the byte bound exactly, m7's tender signs no coalesce, and both
/// heads stay.
///
/// NEGATIVE CONTROL: `remove::AUTOMATIC_IGNORES_OWED` (the 8,500 stop alone, as before P6), run in
/// the test on a lab of its own: m7 coalesces `[everyone]` into the bytes the ring's rotation
/// needs.
#[test]
fn an_automatic_rekey_keeps_the_owed_room() {
    let value = vec![b'r'; 256 * 1024];
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let heads = |paths: &Paths| {
        let identity = read_identity(paths).unwrap().unwrap();
        let opened = log_of(paths)
            .open_audience_entries(&identity)
            .unwrap()
            .unwrap();
        opened.maximal_epochs[&EpochAudience::Everyone].len()
    };
    let run = |ignores_owed: bool| {
        let lab = ring_lab(&value);
        let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
        remove_member(m7, &join_code(&lab.rec40), false).expect("m7 removes m40");
        save_records(m22, &load_records(m7).unwrap().encode()).unwrap();
        let (rec40, rec41) = (lab.rec40, lab.rec41);
        convergence::rekey(m22, &EpochAudience::Everyone, |r| {
            *r != rec40 && *r != rec41
        });
        convergence::rekey(m7, &EpochAudience::Everyone, |r| *r != rec40);
        save_records(m7, &load_records(m22).unwrap().encode()).unwrap();
        assert_eq!(heads(m7), 2, "CONTROL: two [everyone] heads");
        let dues = log_of(m7).rekey_dues();
        assert!(
            dues.iter()
                .any(|due| due.audience == EpochAudience::Everyone
                    && due.coverage
                    && !due.rotation),
            "CONTROL: [everyone] is due a coalesce no removal made: {dues:?}"
        );
        let need = ring_bound(m7, &lab.ring, &[]).times(2);
        let full = log_of(m7).held_bytes() + need.bytes;
        remove::AUTOMATIC_IGNORES_OWED.with(|cell| cell.set(ignores_owed));
        crate::membership_log::RecordBudgets::within_bytes(full, || {
            tend_epochs_at(m7, t0).expect("the tender's first look");
            tend_epochs_at(m7, t0 + 40 * log_entries::GAP_STEP_MS).expect("the tender");
        });
        remove::AUTOMATIC_IGNORES_OWED.with(|cell| cell.set(false));
        let left = full.saturating_sub(log_of(m7).held_bytes());
        let after = (heads(m7), left >= need.bytes);
        lab.macs.server.stop();
        after
    };
    assert_eq!(
        run(true),
        (1, false),
        "CONTROL: the stop alone lets the coalesce take the ring's bytes"
    );
    assert_eq!(
        run(false),
        (2, true),
        "the coalesce waits, and the ring's bytes stay free"
    );
}

/// P6 F6: a removal reserves its re-keys' bytes as well as their records. m22's ring holds a
/// 256 KiB value, so the rotation m7's removal of m40 leaves m22 to sign re-puts it; with the
/// log's bytes 64 KiB below the byte bound (and records to spare), m7's removal refuses with the
/// byte fill, and signs nothing. With 1 MiB more of byte bound it removes m40.
///
/// NEGATIVE CONTROL: `remove::FILL_IGNORES_BYTES` (the fill weighs records only, as shipped
/// before P6), run in the test: the removal is signed, and m22's rotation of the ring then
/// passes the byte bound and is refused, so the ring stays due, wrapping m40: the wedge.
#[test]
fn a_removal_reserves_its_rekeys_bytes() {
    let value = vec![b'r'; 256 * 1024];
    let t0 = aterm_time::unix_millis().unwrap_or(0);

    let lab = ring_lab(&value);
    let m7 = &lab.macs.m7;
    let log = log_of(m7);
    let bound = log.held_bytes() + 64 * 1024;
    let ids = convergence::ids(m7);
    let error = crate::membership_log::RecordBudgets::within_bytes(bound, || {
        remove_member(m7, &join_code(&lab.rec40), false)
    })
    .expect_err("the removal refuses");
    let text = error.to_string();
    assert!(text.contains("record bytes"), "{text}");
    let fill = numbers_of(&text);
    let [held, batch, worst, limit] = fill.as_slice() else {
        panic!("the fill's four numbers: {fill:?}");
    };
    assert_eq!((*held, *limit), (log.held_bytes(), bound), "{fill:?}");
    assert!(
        *worst >= 2 * value.len() && held + batch + worst > bound,
        "the worst case re-puts the ring's value twice: {fill:?}"
    );
    assert_eq!(convergence::ids(m7), ids, "nothing is signed");
    crate::membership_log::RecordBudgets::within_bytes(bound + 1024 * 1024, || {
        remove_member(m7, &join_code(&lab.rec40), false)
    })
    .expect("CONTROL: with room for the ring's re-puts m7 removes m40");
    lab.macs.server.stop();

    let lab = ring_lab(&value);
    let (m22, m7) = (&lab.macs.m22, &lab.macs.m7);
    let bound = log_of(m7).held_bytes() + 64 * 1024;
    remove::FILL_IGNORES_BYTES.with(|cell| cell.set(true));
    let signed = crate::membership_log::RecordBudgets::within_bytes(bound, || {
        remove_member(m7, &join_code(&lab.rec40), false)
    });
    remove::FILL_IGNORES_BYTES.with(|cell| cell.set(false));
    signed.expect("CONTROL: a fill of records alone signs the removal");
    save_records(m22, &load_records(m7).unwrap().encode()).unwrap();
    let refusals: Vec<String> = crate::membership_log::RecordBudgets::within_bytes(bound, || {
        [
            tend_epochs_at(m22, t0),
            tend_epochs_at(m22, t0 + 40 * log_entries::GAP_STEP_MS),
        ]
    })
    .iter()
    .filter_map(|tend| tend.as_ref().err().map(ToString::to_string))
    .collect();
    assert!(
        refusals
            .iter()
            .any(|refusal| refusal.contains("record bytes")),
        "CONTROL: m22's rotation of the ring is refused at the byte bound: {refusals:?}"
    );
    assert!(
        ring_due(m22, &lab.ring).is_some(),
        "CONTROL: the ring's rotation passes the byte bound and stays due"
    );
    assert!(
        log_of(m22).wraps_maximal_audience_to(&lab.ring, &lab.rec40),
        "CONTROL: the ring's newest epoch still wraps removed m40"
    );
    lab.macs.server.stop();
}
