// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One minter per due re-key: lab Macs on an injected clock, each Mac's vault passes
//! ([`vault_pass_at`]: the ranked gap closer, then the ranked tender) run one
//! [`log_entries::GAP_STEP_MS`] apart while the records move between them as causal prefixes of
//! each minter's batch, as the exchange moves a few records a round.

use super::*;
use crate::client::log_entries::{GAP_STEP_MS, REMOVER_GRACE_MS};
use crate::membership_log::{
    Turn,
    rekey::{MINTERS_CARRY, PREFIX_BLIND, SEATLESS_FLY, TWINS_DUE, UNDECIDED_BLIND, UNSETTLED_OK},
};

/// Six lab Macs: `m22`, `m7` and four `m7` lets in, each holding every record; their actors and
/// age recipients, in that order.
struct Six {
    macs: TwoMacs,
    all: Vec<Paths>,
    actors: Vec<MemberId>,
    recipients: Vec<Recipient>,
}

fn six() -> Six {
    six_with(false)
}

/// [`six`], with `m7` an admin too where `m7_admin`.
fn six_with(m7_admin: bool) -> Six {
    let macs = two_macs_with(
        &[(Audience::Everyone, "shared", b"v1".as_slice())],
        m7_admin,
    );
    let mut all = vec![macs.m22.clone(), macs.m7.clone()];
    for name in ["m40", "m41", "m42", "m43"] {
        let (paths, _, _) = let_in_by(&macs, &macs.m7, name, name);
        all.push(paths);
    }
    let wire = load_records(&macs.m7).unwrap().encode();
    for paths in &all {
        save_records(paths, &wire).unwrap();
    }
    let actors = all.iter().map(|paths| member_key(paths).1).collect();
    let recipients = all
        .iter()
        .map(|paths| read_identity(paths).unwrap().unwrap().recipient())
        .collect();
    Six {
        macs,
        all,
        actors,
        recipients,
    }
}

fn ids(paths: &Paths) -> BTreeSet<RecordId> {
    log_of(paths).records().map(|record| *record.id()).collect()
}

/// The records `paths` holds that `before` does not name, in its log's order: what it signed
/// since, each after the one it signed before.
fn signed_since(paths: &Paths, before: &BTreeSet<RecordId>) -> Vec<Record> {
    log_of(paths)
        .records()
        .filter(|record| !before.contains(record.id()))
        .cloned()
        .collect()
}

/// How much of `chain` (one Mac's removal batches, in its signing order) another Mac holds at
/// step `k`: the `Remove` alone, then through the first `Epoch` and one of its `Wrap`s, then
/// half its `Wrap`s, then all of it.
fn prefix(chain: &[Record], k: u64) -> &[Record] {
    let epoch = chain
        .iter()
        .position(|record| matches!(record.body(), Body::Epoch { .. }))
        .expect("a removal batch mints an epoch");
    let wraps = chain[epoch + 1..]
        .iter()
        .take_while(|record| matches!(record.body(), Body::Wrap { .. }))
        .count();
    let len = match k {
        0 => 1,
        1 => epoch + 2,
        2 => epoch + 1 + (wraps / 2).max(1),
        _ => chain.len(),
    };
    &chain[..len.min(chain.len())]
}

/// Every `Epoch` of `signed`, counted by audience.
fn epochs_by_audience(signed: &[Record]) -> BTreeMap<EpochAudience, usize> {
    let mut out = BTreeMap::new();
    for record in signed {
        if let Body::Epoch { audience, .. } = record.body() {
            *out.entry(audience.clone()).or_insert(0) += 1;
        }
    }
    out
}

/// Each `(epoch, recipient)` that two counting-or-not `Wrap`s of `log` both hand on.
fn duplicate_wraps(log: &Log) -> Vec<(RecordId, Recipient)> {
    let mut seen = BTreeSet::new();
    log.records()
        .filter_map(|record| match record.body() {
            Body::Wrap {
                epoch, recipient, ..
            } => (!seen.insert((*epoch, *recipient))).then_some((*epoch, *recipient)),
            _ => None,
        })
        .collect()
}

/// The recipients the `Wrap`s of `epoch` in `log` hand it to.
fn wrapped_by(log: &Log, epoch: &RecordId) -> BTreeSet<Recipient> {
    log.records()
        .filter_map(|record| match record.body() {
            Body::Wrap {
                epoch: of,
                recipient,
                ..
            } if of == epoch => Some(*recipient),
            _ => None,
        })
        .collect()
}

/// `paths` re-keys `audience` on its own, as a rotation signs it: one higher `Epoch`, its
/// `Wrap`s to each member `to` keeps, in recipient order, and the `shared` entry, the Commons
/// control descriptor or a host list's first entry resealed under it, all at one `authored_ms`.
/// Returns the batch, kept in `paths`' records.
fn rekey(paths: &Paths, audience: &EpochAudience, to: impl Fn(&Recipient) -> bool) -> Vec<Record> {
    let recipients: BTreeSet<Recipient> = log_of(paths)
        .membership()
        .members()
        .values()
        .map(|member| member.binding.recipient())
        .filter(|recipient| to(recipient))
        .collect();
    rekey_to(paths, audience, &recipients)
}

/// [`rekey`] wrapped to `recipients`, a Mac with no seat among them or not.
fn rekey_to(
    paths: &Paths,
    audience: &EpochAudience,
    recipients: &BTreeSet<Recipient>,
) -> Vec<Record> {
    let identity = read_identity(paths).unwrap().unwrap();
    let (bootstrap, actor, incarnation) = member_key(paths);
    let mut log = log_of(paths);
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let generation = opened.max_generation[audience] + 1;
    let recipients = recipients.clone();
    let records = bootstrap
        .device()
        .with_seed(|seed| -> crate::Result<Vec<Record>> {
            let (draft, key) = log.draft_epoch(&actor, 1000, audience.clone(), generation)?;
            let epoch = draft.sign(seed, incarnation)?;
            let id = *epoch.id();
            log.insert(epoch.clone())?;
            let mut records = vec![epoch];
            for recipient in recipients {
                let wrap = log
                    .draft_wrap(&actor, 1000, id, &key, recipient)?
                    .sign(seed, incarnation)?;
                log.insert(wrap.clone())?;
                records.push(wrap);
            }
            let put = match audience {
                EpochAudience::Commons => {
                    let control = opened
                        .commons_control
                        .as_ref()
                        .expect("a control descriptor");
                    log.draft_commons_put(&actor, 1000, id, &key, control.as_slice())?
                }
                EpochAudience::Everyone => {
                    let entry = &opened.by_audience[&Audience::Everyone]["shared"];
                    log.draft_put(&actor, 1000, id, &key, entry)?
                }
                EpochAudience::Hosts(hosts) => {
                    let entry = opened.by_audience[&Audience::Hosts(hosts.clone())]
                        .values()
                        .next()
                        .expect("the list seals an entry");
                    log.draft_put(&actor, 1000, id, &key, entry)?
                }
            };
            let put = put.sign(seed, incarnation)?;
            log.insert(put.clone())?;
            records.push(put);
            Ok(records)
        })
        .unwrap();
    save_records(paths, &encode_records(&records)).unwrap();
    records
}

/// The `[everyone]` entry `shared` as the Mac at `paths` opens it from its records.
fn shared(paths: &Paths) -> Option<Vec<u8>> {
    let identity = read_identity(paths).unwrap().unwrap();
    let opened = log_of(paths).open_audience_entries(&identity).unwrap()?;
    opened
        .by_audience
        .get(&Audience::Everyone)?
        .get("shared")
        .map(|entry| entry.value.as_slice().to_vec())
}

/// The recipients of `lab`, sorted, and the index in `lab.all` of the Mac holding each.
fn in_order(lab: &Six) -> (Vec<Recipient>, Vec<usize>) {
    let mut order: Vec<usize> = (0..lab.all.len()).collect();
    order.sort_by_key(|&i| lab.recipients[i]);
    (order.iter().map(|&i| lab.recipients[i]).collect(), order)
}

/// Every Mac of `all` takes `from`'s records.
fn carry(all: &[Paths], from: usize) {
    let wire = load_records(&all[from]).unwrap().encode();
    for (i, paths) in all.iter().enumerate() {
        if i != from {
            save_records(paths, &wire).unwrap();
        }
    }
}

/// The first Mac of `all` takes every other's records, and its log.
fn union(all: &[Paths]) -> Log {
    for i in 1..all.len() {
        save_records(&all[0], &load_records(&all[i]).unwrap().encode()).unwrap();
    }
    log_of(&all[0])
}

/// The burst: on one side m22 removes m42 and then m43, on the other m7 removes m41. Each step
/// `k` (0..=15) every Mac holds a longer prefix of the other side's removal batches
/// ([`prefix`]), and each Mac's vault passes run at `t0 + k` steps; what a pass signs reaches
/// every other Mac with its whole store at once. Returns the lab, what the passes signed, and
/// the union of the records.
fn burst(unranked: bool) -> (Six, Vec<Record>, Log) {
    let lab = six();
    let base = ids(&lab.all[0]);
    test_remove_unposted(&lab.all[0], &join_code(&lab.recipients[4])).expect("m22 removes m42");
    test_remove_unposted(&lab.all[0], &join_code(&lab.recipients[5])).expect("m22 removes m43");
    test_remove_unposted(&lab.all[1], &join_code(&lab.recipients[3])).expect("m7 removes m41");
    let chains = [
        (0, signed_since(&lab.all[0], &base)),
        (1, signed_since(&lab.all[1], &base)),
    ];
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    remove::UNRANKED.with(|cell| cell.set(unranked));
    let mut signed = Vec::new();
    for k in 0..16 {
        for (minter, chain) in &chains {
            let wire = encode_records(prefix(chain, k));
            for (i, paths) in lab.all.iter().enumerate() {
                if i != *minter {
                    save_records(paths, &wire).unwrap();
                }
            }
        }
        for i in 0..lab.all.len() {
            let mine = vault_pass_at(&lab.all[i], t0 + k * GAP_STEP_MS).unwrap();
            if !mine.is_empty() {
                carry(&lab.all, i);
                signed.extend(mine);
            }
        }
    }
    remove::UNRANKED.with(|cell| cell.set(false));
    let union = union(&lab.all);
    (lab, signed, union)
}

/// The burst ([`burst`]) re-keys each audience once: one higher `Epoch` of `[everyone]` and of
/// Commons, both by one of the two removers (the first ranked; m40, which waits the grace,
/// finds them covered), and nothing is due after. (The burst opens no wrap gap, so no `Wrap`
/// two holders could both sign: [`a_gap_on_a_batch_that_settles_late_is_closed_once`] holds
/// that.)
///
/// NEGATIVE CONTROL: with `remove::UNRANKED` (every Mac's turn first, at once, as every Mac's
/// exchange-time repair once minted) the same burst mints two or more epochs of an audience.
#[test]
fn a_three_removal_burst_delivered_as_prefixes_mints_one_epoch_per_due_by_a_remover() {
    let (lab, signed, union) = burst(false);
    let by_audience = epochs_by_audience(&signed);
    assert_eq!(
        by_audience,
        BTreeMap::from([(EpochAudience::Everyone, 1), (EpochAudience::Commons, 1)]),
        "one epoch per due: {by_audience:?}"
    );
    let removers = BTreeSet::from([lab.actors[0], lab.actors[1]]);
    for record in &signed {
        if matches!(record.body(), Body::Epoch { .. }) {
            assert!(removers.contains(record.actor()), "a remover mints");
        }
    }
    assert!(union.rekey_dues().is_empty(), "{:?}", union.rekey_dues());
    lab.macs.server.stop();

    let (lab, signed, _) = burst(true);
    let by_audience = epochs_by_audience(&signed);
    assert!(
        by_audience.values().any(|&n| n >= 2),
        "CONTROL: unranked, the burst mints two or more per due: {by_audience:?}"
    );
    lab.macs.server.stop();
}

/// m22 removes m41 and m7 takes the whole batch, while m40 holds only the `Remove`. Steps
/// 0..=15: m40, no remover, waits [`REMOVER_GRACE_MS`] and its rank, then mints its own
/// rotation of each audience once. At step 16 every Mac takes every record: the two rotations
/// wrap one recipient set and no removed Mac, secure twins, so steps 16..=31 sign nothing.
/// Returns what the two phases signed.
fn withheld() -> (Vec<Record>, Vec<Record>, MemberId) {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m40, actor40, _) = let_in_by(&macs, &macs.m7, "m40", "m40");
    let (m41, _, rec41) = let_in_by(&macs, &macs.m7, "m41", "m41");
    let all = vec![macs.m22.clone(), macs.m7.clone(), m40, m41];
    carry(&all, 1);
    let base = ids(&all[0]);
    test_remove_unposted(&all[0], &join_code(&rec41)).expect("m22 removes m41");
    let chain = signed_since(&all[0], &base);
    save_records(&all[1], &encode_records(&chain)).unwrap();
    save_records(&all[2], &encode_records(&chain[..1])).unwrap();
    let seated = &all[..3];
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let mut before = Vec::new();
    for k in 0..16 {
        for paths in seated {
            before.extend(vault_pass_at(paths, t0 + k * GAP_STEP_MS).unwrap());
        }
    }
    for i in 0..seated.len() {
        carry(seated, i);
    }
    let mut after = Vec::new();
    for k in 16..32 {
        for i in 0..seated.len() {
            let mine = vault_pass_at(&seated[i], t0 + k * GAP_STEP_MS).unwrap();
            if !mine.is_empty() {
                carry(seated, i);
                after.extend(mine);
            }
        }
    }
    macs.server.stop();
    (before, after, actor40)
}

/// A rotation withheld from m40 past the grace costs one extra epoch of each audience, m40's,
/// and then quiescence ([`withheld`]).
///
/// NEGATIVE CONTROL: with `TWINS_DUE` (no newest epochs are secure twins), the heal finds two
/// newest epochs of each audience due a coalesce, and one follows.
#[test]
fn a_rotation_withheld_past_the_grace_mints_once_more_and_then_rests() {
    let (before, after, actor40) = withheld();
    assert_eq!(
        epochs_by_audience(&before),
        BTreeMap::from([(EpochAudience::Everyone, 1), (EpochAudience::Commons, 1)]),
        "one extra epoch per audience"
    );
    assert!(
        before
            .iter()
            .filter(|record| matches!(record.body(), Body::Epoch { .. }))
            .all(|record| *record.actor() == actor40),
        "the Mac the rotation was withheld from mints it"
    );
    assert!(after.is_empty(), "quiescence after the heal: {after:?}");

    TWINS_DUE.with(|cell| cell.set(true));
    let (_, after, _) = withheld();
    TWINS_DUE.with(|cell| cell.set(false));
    assert!(
        !epochs_by_audience(&after).is_empty(),
        "CONTROL: without the twins rule a coalesce follows the heal"
    );
}

/// A, the Mac of `r5`, removes `r4`'s: the batch it signs.
fn a_removes_r4(a: &Paths, r: &[Recipient]) -> Vec<Record> {
    let base = ids(a);
    test_remove_unposted(a, &join_code(&r[4])).expect("A removes r4");
    signed_since(a, &base)
}

/// What X holds of A's removal batch `chain`: the `Remove`, the `[everyone]` `Epoch` and its
/// first three `Wrap`s, to `r0`, `r1` and `r2`.
fn a_prefix<'a>(chain: &'a [Record], r: &[Recipient]) -> &'a [Record] {
    let epoch = chain
        .iter()
        .position(|record| matches!(record.body(), Body::Epoch { .. }))
        .unwrap();
    let held = &chain[..epoch + 4];
    assert_eq!(
        held[epoch + 1..]
            .iter()
            .filter_map(|record| match record.body() {
                Body::Wrap { recipient, .. } => Some(*recipient),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![r[0], r[1], r[2]],
        "CONTROL: a batch wraps in recipient order"
    );
    held
}

/// An audience can remain due while a newly arrived epoch changes its settledness.
/// Authoring must reconsider the scheduling observation before minting.
///
/// NEGATIVE CONTROL: the scheduling observation permits the `[everyone]` mint and the audience
/// is still due under the lock, so a look that signed from that observation would mint; with
/// `remove::STALE_AUTHORING` (the signing look checks no rank, grace or settledness and
/// re-keys every audience still due) the same look mints `[everyone]` over the prefix.
#[test]
fn authoring_rechecks_a_new_prefix_after_the_scheduling_observation() {
    let lab = six();
    let (r, order) = in_order(&lab);
    let (x, a) = (&lab.all[order[0]], &lab.all[order[5]]);
    test_remove_unposted(x, &join_code(&r[1])).unwrap();
    let chain = a_removes_r4(a, &r);
    save_records(x, &encode_records(&chain[..1])).unwrap();
    let me = member_key(x).1;
    let t0 = aterm_time::unix_millis().unwrap();
    let (first, _) = ready_rekeys(&log_of(x), &me, &TendMark::default(), t0);
    let now = t0 + 100 * GAP_STEP_MS;
    let (mark, ready) = ready_rekeys(&log_of(x), &me, &first, now);
    assert!(
        ready.contains(&EpochAudience::Everyone),
        "the old observation permits a mint"
    );

    // The transaction sees a new head with only part of its minter's Wrap batch.
    save_records(x, &encode_records(a_prefix(&chain, &r))).unwrap();
    assert!(
        log_of(x)
            .rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone)
    );
    let identity = read_identity(x).unwrap().unwrap();
    let minted = tend_ready_epochs(x, now, &identity, &mark).unwrap();
    assert!(!epochs_by_audience(&minted).contains_key(&EpochAudience::Everyone));

    STALE_AUTHORING.with(|cell| cell.set(true));
    let stale = tend_ready_epochs(x, now, &identity, &mark);
    STALE_AUTHORING.with(|cell| cell.set(false));
    assert!(
        epochs_by_audience(&stale.unwrap()).contains_key(&EpochAudience::Everyone),
        "CONTROL: trusting the scheduling observation mints over the prefix"
    );
    lab.macs.server.stop();
}

/// The m4 shape. Of six Macs in recipient order `r0..r5`, `r0`'s Mac X removes `r1`'s and
/// `r5`'s Mac A removes `r4`'s, each on its own side. X holds A's `Remove`, A's `[everyone]`
/// `Epoch` and its first three `Wrap`s (to `r0`, `r1` and `r2`, in recipient order) from step
/// 0, and the rest of A's batches from step `rest` if one is given. X's passes run at steps
/// 0..=15. Returns the step of X's first signed record, the recipients its first
/// `[everyone]` epoch wraps, and the recipients.
fn m4_shape(rest: Option<u64>) -> (Option<u64>, BTreeSet<Recipient>, Vec<Recipient>) {
    let (first, wraps, r, _) = m4_shape_with(rest, false);
    (first, wraps.unwrap_or_default(), r)
}

/// [`m4_shape`], and where `seatless` first m22 (an admin) lets a `[machines]`-only key S into
/// `[everyone]`, which sorts above every lab Mac, so both removals carry it and A's batch wraps
/// it last. Returns the step of X's first signed record, the recipients of its first
/// `[everyone]` epoch if it signs one, the recipients, and S.
fn m4_shape_with(
    rest: Option<u64>,
    seatless: bool,
) -> (
    Option<u64>,
    Option<BTreeSet<Recipient>>,
    Vec<Recipient>,
    Option<Recipient>,
) {
    let lab = six();
    let (r, order) = in_order(&lab);
    let seatless = seatless.then(|| {
        let key = key_above(&r[5]).recipient();
        let admin = admin_open(&lab.all[0]).expect("m22 is an admin");
        let mut doc = admin.doc.clone();
        doc.add_machine("m99", &key.to_bech32()).unwrap();
        admin_publish_approved(&lab.all[0], &admin, &doc, &[key.to_bech32()])
            .expect("m22 lets S in");
        carry(&lab.all, 0);
        key
    });
    let (x, a) = (&lab.all[order[0]], &lab.all[order[5]]);
    test_remove_unposted(x, &join_code(&r[1])).expect("X removes r1");
    let chain = a_removes_r4(a, &r);
    if let Some(key) = &seatless {
        let epoch = chain
            .iter()
            .find(|record| matches!(record.body(), Body::Epoch { .. }))
            .unwrap();
        assert!(
            wrapped_by(&log_of(a), epoch.id()).contains(key),
            "CONTROL: A's batch wraps S"
        );
    }
    save_records(x, &encode_records(a_prefix(&chain, &r))).unwrap();
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let mut first = None;
    let mut wraps = None;
    for k in 0..16 {
        if rest == Some(k) {
            save_records(x, &encode_records(&chain)).unwrap();
        }
        let signed = vault_pass_at(x, t0 + k * GAP_STEP_MS).unwrap();
        if first.is_none() && !signed.is_empty() {
            first = Some(k);
        }
        // The removers' ranks are a hash of their keys, so the `[everyone]` re-key may come a
        // step after the Commons one.
        if wraps.is_none()
            && let Some(minted) = signed.iter().find(|record| {
                matches!(
                    record.body(),
                    Body::Epoch {
                        audience: EpochAudience::Everyone,
                        ..
                    }
                )
            })
        {
            wraps = Some(wrapped_by(&log_of(x), minted.id()));
        }
    }
    lab.macs.server.stop();
    (first, wraps, r, seatless)
}

/// The m4 shape ([`m4_shape`]): X signs nothing while it holds only a prefix of A's batch, and
/// once the batch settles its one higher `[everyone]` epoch wraps every Mac neither removal
/// names. A minter that falls silent after its last `Wrap` holds the audience only
/// [`log_entries::SETTLE_GRACE_MS`] from first sight, and what X mints then still carries `r3`
/// and `r5`, whose `Wrap`s the rest of A's batch may still bring ([`Log::in_flight`]).
///
/// NEGATIVE CONTROLS: with `UNSETTLED_OK` (every epoch reads as settled), X mints
/// at its first turn from the prefix, reads `r3` and `r5`, whose `Wrap`s it lacks, as left out
/// on purpose, and its new epoch wraps only `r0` and `r2`; with `PREFIX_BLIND` (no batch may
/// wrap more than this Mac holds) the mint after the grace narrows the same way.
#[test]
fn nothing_is_signed_from_an_epoch_held_as_a_prefix_until_its_batch_settles() {
    let (first, wraps, r) = m4_shape(Some(5));
    let first = first.expect("X re-keys once the batch settles");
    assert!((5..=6).contains(&first), "signed at step {first}");
    assert_eq!(wraps, BTreeSet::from([r[0], r[2], r[3], r[5]]));

    let (silent, carried, r) = m4_shape(None);
    let silent = silent.expect("a silent minter does not hold the audience forever");
    let grace = log_entries::SETTLE_GRACE_MS / GAP_STEP_MS;
    assert!(silent >= grace, "signed at step {silent}, before the grace");
    assert_eq!(
        carried,
        BTreeSet::from([r[0], r[2], r[3], r[5]]),
        "the mint after the grace carries the Macs A's batch may still wrap"
    );

    PREFIX_BLIND.with(|cell| cell.set(true));
    let (_, blind, r) = m4_shape(None);
    PREFIX_BLIND.with(|cell| cell.set(false));
    assert_eq!(
        blind,
        BTreeSet::from([r[0], r[2]]),
        "CONTROL: prefix-blind, the mint after the grace narrows"
    );

    UNSETTLED_OK.with(|cell| cell.set(true));
    let (early, narrowed, r) = m4_shape(Some(5));
    UNSETTLED_OK.with(|cell| cell.set(false));
    assert!(
        early.is_some_and(|k| k <= 1),
        "CONTROL: settledness off, X mints from the prefix: {early:?}"
    );
    assert_eq!(
        narrowed,
        BTreeSet::from([r[0], r[2]]),
        "CONTROL: settledness off narrows the new epoch"
    );
}

/// F16's review with the grace: m7 removes m40, which a ring `[m22 m40]` m7 is not in seals
/// to, and m22 holds only m7's `Remove`. For `[everyone]` and Commons m7, the remover, carries
/// its own rotation, so m22 waits the grace; for the ring no remover can carry it, so its
/// holder m22 ranks first with no grace and rotates it at its first pass, and nothing else.
///
/// NEGATIVE CONTROL: the audiences the remover can carry, under the same removal and on the
/// same pass, wait the grace: m22 mints no `[everyone]` or Commons epoch before it.
#[test]
fn a_remover_that_cannot_open_an_audience_leaves_its_holders_to_rank_with_no_grace() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_m40, _, rec40) = let_in(&macs, "m40", "m40");
    sync(m22, None).expect("m22 syncs m40's admission");
    let ring = Audience::Hosts(vec!["m22".to_owned(), "m40".to_owned()]);
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&ring, "ring-cred", b"ring-v1").expect("ring");
    admin_publish(m22, &admin, &doc).expect("publish the ring");
    let lists = EpochAudience::from(&ring);
    sync(m7, None).expect("m7 syncs");
    let base = ids(m7);
    test_remove_unposted(m7, &join_code(&rec40)).expect("m7 removes m40");
    let chain = signed_since(m7, &base);
    save_records(m22, &encode_records(&chain[..1])).unwrap();

    let log = log_of(m22);
    let membership = log.membership();
    let (_, actor22, _) = member_key(m22);
    let turns: BTreeMap<EpochAudience, Option<Turn>> = log
        .rekey_dues()
        .into_iter()
        .map(|due| (due.audience.clone(), due.turn(&membership, &actor22)))
        .collect();
    assert_eq!(
        turns.get(&lists),
        Some(&Some(Turn {
            rank: 0,
            after_grace: false
        })),
        "{turns:?}"
    );
    for audience in [EpochAudience::Everyone, EpochAudience::Commons] {
        assert!(
            turns
                .get(&audience)
                .is_some_and(|turn| turn.is_some_and(|turn| turn.after_grace)),
            "CONTROL: m22 waits the grace for {audience:?}: {turns:?}"
        );
    }

    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let signed = vault_pass_at(m22, t0).unwrap();
    assert_eq!(
        epochs_by_audience(&signed),
        BTreeMap::from([(lists.clone(), 1)]),
        "m22 rotates the ring at once and nothing else"
    );
    assert!(!log_of(m22).wraps_maximal_audience_to(&lists, &rec40));
    let mut waited = Vec::new();
    let steps = REMOVER_GRACE_MS / GAP_STEP_MS;
    for k in 1..steps {
        waited.extend(vault_pass_at(m22, t0 + k * GAP_STEP_MS).unwrap());
    }
    assert_eq!(
        epochs_by_audience(&waited),
        BTreeMap::new(),
        "CONTROL: m22 waits the grace for the audiences m7 carries"
    );
    macs.server.stop();
}

/// m22 mints `n` successive Commons epochs on its own, each wrapped to every member and
/// sealing the control descriptor, as `n` rotations would.
fn mint_commons_history(paths: &Paths, n: usize) -> Vec<RecordId> {
    let identity = read_identity(paths).unwrap().unwrap();
    let (bootstrap, actor, incarnation) = member_key(paths);
    let mut log = log_of(paths);
    let control = log.open_commons_control(&identity).unwrap().unwrap();
    let mut minted = Vec::new();
    let records = bootstrap
        .device()
        .with_seed(|seed| -> crate::Result<Vec<Record>> {
            let mut records = Vec::new();
            for _ in 0..n {
                let opened = log.open_audience_entries(&identity)?.unwrap();
                let generation = opened.max_generation[&EpochAudience::Commons] + 1;
                let (draft, key) =
                    log.draft_epoch(&actor, 1000, EpochAudience::Commons, generation)?;
                let epoch = draft.sign(seed, incarnation)?;
                let id = *epoch.id();
                log.insert(epoch.clone())?;
                records.push(epoch);
                let recipients: BTreeSet<Recipient> = log
                    .membership()
                    .members()
                    .values()
                    .map(|member| member.binding.recipient())
                    .collect();
                for recipient in recipients {
                    let wrap = log
                        .draft_wrap(&actor, 1000, id, &key, recipient)?
                        .sign(seed, incarnation)?;
                    log.insert(wrap.clone())?;
                    records.push(wrap);
                }
                let put = log
                    .draft_commons_put(&actor, 1000, id, &key, control.as_slice())?
                    .sign(seed, incarnation)?;
                log.insert(put.clone())?;
                records.push(put);
                minted.push(id);
            }
            Ok(records)
        })
        .unwrap();
    save_records(paths, &encode_records(&records)).unwrap();
    minted
}

/// ACE1 enrollment on m22 of a Mac, m10, that m7 let in on its own side while m22 rotated
/// Commons 20 times: what m22's enrollment signs. Returns the lab, m22's log before it, the
/// records it signed, m10's home, and the 20 epochs.
fn enroll_after_history() -> (TwoMacs, Log, Vec<Record>, Paths, Vec<RecordId>) {
    use crate::commons::binding::Pending;
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let trust = member_key(m22).0.trust();
    let m10 = Paths::for_home(&macs.root.path().join("m10"));
    configure_log_member(&m10, &format!("{}#{}", macs.url, macs.signer), "m10").unwrap();
    let device = Device::open(&m10).unwrap().unwrap();
    let request = device.request(trust.network, trust.authority).unwrap();
    admit_member_in_log(
        m7,
        crate::membership_log::Entrant {
            request: request.clone(),
            node_key: [0xaa; 32],
            name: "m10".to_owned(),
            label: "m10".to_owned(),
        },
        crate::membership_log::Evidence::Phrase,
    )
    .unwrap();
    let history = mint_commons_history(m22, 20);
    save_records(m22, &load_records(m7).unwrap().encode()).unwrap();
    save_records(&m10, &load_records(m22).unwrap().encode()).unwrap();
    let before = log_of(m22);
    let (mut pending, challenge) =
        Pending::issue(&request, trust.network, trust.authority).unwrap();
    let answer = device.answer(&request, &challenge).unwrap();
    let possession = pending.verify(&answer).unwrap();
    let held = ids(m22);
    enroll_commons_member_in_log(m22, trust, request, possession).expect("enrolled");
    let signed = signed_since(m22, &held);
    save_records(&m10, &load_records(m22).unwrap().encode()).unwrap();
    (macs, before, signed, m10, history)
}

/// ACE1 enrollment after 20 Commons rotations wraps the Mac the admission epochs alone (with an
/// `Admit`, at most one more record), and the Mac reads a row sealed under an older epoch
/// through the history handoff (ACHG1) a holder issues it on demand: the key it opens is the
/// epoch's.
///
/// NEGATIVE CONTROL: with `log_entries::HISTORY_BACKFILL` (every Commons epoch the signer holds
/// wrapped, as the backfill once did) the same enrollment signs 20 or more records.
#[test]
fn enrollment_after_twenty_rotations_wraps_the_admission_epochs_and_history_comes_by_handoff() {
    let (macs, before, signed, m10, history) = enroll_after_history();
    let identity22 = read_identity(&macs.m22).unwrap().unwrap();
    let opened = before.open_audience_entries(&identity22).unwrap().unwrap();
    let admission =
        Log::admission_epochs(&opened, &[EpochAudience::Everyone, EpochAudience::Commons]);
    assert!(
        signed.len() <= 1 + admission.len(),
        "{} signed for {} admission epochs",
        signed.len(),
        admission.len()
    );
    assert!(
        !signed.is_empty(),
        "CONTROL: the Mac lacked the newest epoch"
    );

    // An old Commons epoch is not wrapped to the Mac: it asks a holder for its key.
    let old = history[4];
    let identity10 = read_identity(&m10).unwrap().unwrap();
    let log10 = log_of(&m10);
    assert!(
        log10
            .unwrap_commons_epoch(&old, &identity10)
            .unwrap()
            .is_none(),
        "no backfilled wrap"
    );
    let b22 = Bootstrap::required(&macs.m22).unwrap();
    let b10 = Bootstrap::required(&m10).unwrap();
    let trust = b22.trust();
    let journal =
        crate::commons::journal::Journal::open_existing(b10.journal_dir(), trust).unwrap();
    let access = b10
        .access_in(&journal)
        .unwrap()
        .expect("m10 observes Commons");
    assert!(
        b10.historical(&journal, &access, old).is_err(),
        "CONTROL: m10 opens no old epoch before a handoff"
    );
    drop(journal);
    let journal =
        crate::commons::journal::Journal::open_existing(b22.journal_dir(), trust).unwrap();
    let access = b22.access_in(&journal).unwrap().unwrap();
    let handoff = b22
        .history_handoff(&journal, &access, old, b10.request().unwrap().binding())
        .expect("m22 issues the handoff of a log-era epoch");
    drop(journal);
    let journal =
        crate::commons::journal::Journal::open_existing(b10.journal_dir(), trust).unwrap();
    let access = b10.access_in(&journal).unwrap().unwrap();
    b10.import_history(&journal, &access, handoff.bytes())
        .expect("m10 imports it");
    let key = b10
        .historical(&journal, &access, old)
        .expect("m10 opens the old epoch through the handoff")
        .with_key(&journal, |key| *key)
        .unwrap();
    drop(journal);
    let expected = log_of(&macs.m22)
        .unwrap_commons_epoch(&old, &identity22)
        .unwrap()
        .unwrap();
    assert_eq!(key, *expected, "the handoff carries the epoch's own key");
    macs.server.stop();

    log_entries::HISTORY_BACKFILL.with(|cell| cell.set(true));
    let (macs, _, signed, _, _) = enroll_after_history();
    log_entries::HISTORY_BACKFILL.with(|cell| cell.set(false));
    assert!(
        signed.len() >= 20,
        "CONTROL: the old backfill signs {} records",
        signed.len()
    );
    macs.server.stop();
}

/// The m4 shape the other way round: X takes A's removal batch only through its `Wrap` to `r2`
/// ([`a_prefix`]) first, and then removes `r1`. Returns the recipients X's removal rotation of
/// `[everyone]` wraps, and the recipients.
fn removal_over_prefix() -> (BTreeSet<Recipient>, Vec<Recipient>) {
    let lab = six();
    let (r, order) = in_order(&lab);
    let (x, a) = (&lab.all[order[0]], &lab.all[order[5]]);
    let chain = a_removes_r4(a, &r);
    save_records(x, &encode_records(a_prefix(&chain, &r))).unwrap();
    let held = ids(x);
    test_remove_unposted(x, &join_code(&r[1])).expect("X removes r1");
    let signed = signed_since(x, &held);
    let minted = signed
        .iter()
        .find(|record| {
            matches!(
                record.body(),
                Body::Epoch {
                    audience: EpochAudience::Everyone,
                    ..
                }
            )
        })
        .expect("X rotates [everyone]");
    let wraps = wrapped_by(&log_of(x), minted.id());
    lab.macs.server.stop();
    (wraps, r)
}

/// A removal signed over a prefix of another Mac's batch ([`removal_over_prefix`]) carries the
/// Macs the rest of that batch may still wrap: X's rotation wraps `r3` and `r5` as well as `r0`
/// and `r2`.
///
/// NEGATIVE CONTROL: with `PREFIX_BLIND` (no batch may wrap more than this Mac holds) X reads
/// the `Wrap`s to `r3` and `r5` it lacks as left out on purpose and wraps only `r0` and `r2`.
#[test]
fn a_removal_over_a_prefix_of_another_batch_carries_the_macs_it_may_still_wrap() {
    let (wraps, r) = removal_over_prefix();
    assert_eq!(wraps, BTreeSet::from([r[0], r[2], r[3], r[5]]));

    PREFIX_BLIND.with(|cell| cell.set(true));
    let (narrowed, r) = removal_over_prefix();
    PREFIX_BLIND.with(|cell| cell.set(false));
    assert_eq!(
        narrowed,
        BTreeSet::from([r[0], r[2]]),
        "CONTROL: prefix-blind, the rotation narrows"
    );
}

/// The ring's shape. Of six Macs in recipient order `r0..r5`, m22 (an admin) publishes a ring
/// of `r0`'s X, `r1`'s, `r3`'s and A, `r5`'s Mac. A re-keys the ring ([`rekey`]: its `Epoch`,
/// `Wrap`s to `r0`, `r1`, `r3` and `r5`, then the ring's entry), X holds that batch only
/// through its `Wrap` to `r1`, and then removes `r1`'s Mac, which rotates the ring in its own
/// batch. Returns the recipients X's rotation of the ring wraps, and the recipients.
fn ring_removal_over_prefix() -> (BTreeSet<Recipient>, Vec<Recipient>) {
    const NAMES: [&str; 6] = ["m22", "m7", "m40", "m41", "m42", "m43"];
    let lab = six();
    let (r, order) = in_order(&lab);
    let places = [0, 1, 3, 5];
    let mut hosts: Vec<String> = places.iter().map(|&k| NAMES[order[k]].to_owned()).collect();
    hosts.sort();
    let ring = Audience::Hosts(hosts);
    let admin = admin_open(&lab.all[0]).expect("m22 is an admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&ring, "ring-cred", b"ring-v1").expect("ring");
    admin_publish(&lab.all[0], &admin, &doc).expect("m22 publishes the ring");
    carry(&lab.all, 0);
    let list = EpochAudience::from(&ring);
    let in_ring: BTreeSet<Recipient> = places.iter().map(|&k| r[k]).collect();
    let (x, a) = (&lab.all[order[0]], &lab.all[order[5]]);
    let batch = rekey(a, &list, |recipient| in_ring.contains(recipient));
    let held = &batch[..3];
    assert_eq!(
        held[1..]
            .iter()
            .filter_map(|record| match record.body() {
                Body::Wrap { recipient, .. } => Some(*recipient),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![r[0], r[1]],
        "CONTROL: X holds A's ring batch through its Wrap to r1"
    );
    save_records(x, &encode_records(held)).unwrap();
    let before = ids(x);
    test_remove_unposted(x, &join_code(&r[1])).expect("X removes r1");
    let signed = signed_since(x, &before);
    let minted = signed
        .iter()
        .find(|record| matches!(record.body(), Body::Epoch { audience, .. } if *audience == list))
        .expect("X's removal rotates the ring");
    let wraps = wrapped_by(&log_of(x), minted.id());
    lab.macs.server.stop();
    (wraps, r)
}

/// A removal over a prefix of another Mac's rotation of a host list ([`ring_removal_over_prefix`])
/// carries the hosts the rest of that batch may still wrap: X's rotation of the ring wraps `r3`
/// and `r5` as well as `r0`, so neither loses the ring once A's batch arrives.
///
/// NEGATIVE CONTROL: with `log_entries::HOSTS_FLIGHT_BLIND` (a host list carries only the
/// `Wrap`s its newest epochs hold, as before) X reads the `Wrap`s to `r3` and `r5` it lacks as
/// left out on purpose and wraps the ring to `r0` alone.
#[test]
fn a_removal_over_a_prefix_of_a_host_list_rotation_carries_the_hosts_it_may_still_wrap() {
    let (wraps, r) = ring_removal_over_prefix();
    assert_eq!(wraps, BTreeSet::from([r[0], r[3], r[5]]));

    log_entries::HOSTS_FLIGHT_BLIND.with(|cell| cell.set(true));
    let (narrowed, r) = ring_removal_over_prefix();
    log_entries::HOSTS_FLIGHT_BLIND.with(|cell| cell.set(false));
    assert_eq!(
        narrowed,
        BTreeSet::from([r[0]]),
        "CONTROL: blind to the ring's flight, the rotation narrows"
    );
}

/// The first key, of secrets of `0x5a` bytes led by a counter, whose recipient sorts above
/// `bound`. The lab Macs' keys are random, so no fixed set of keys lies above every draw of them
/// (the 255 single-byte secrets once tried missed about one fixture in 450); the counter runs on
/// until one does, which only a bound at the very top of the key space could keep from ending.
fn key_above(bound: &Recipient) -> Identity {
    (0u64..)
        .map(|n| {
            let mut secret = [0x5a; 32];
            secret[..8].copy_from_slice(&n.to_le_bytes());
            Identity::from_secret_bytes(secret)
        })
        .find(|identity| identity.recipient() > *bound)
        .expect("a counter past u64 is never reached")
}

/// The retirement's shape for a Mac with no seat. Of six Macs in recipient order `r0..r5`, X,
/// the lower of the two admins m22 and m7, lets a `[machines]`-only key S into `[everyone]` (S
/// sorts above every lab Mac), with an entry `extra`. A (`r5`'s Mac) then re-keys `[everyone]`
/// to the six members alone ([`rekey`]), as a retirement of S signs it: S has no seat, so no
/// `Remove` names it and the missing `Wrap` is its only record. X holds that batch only through
/// its own `Wrap` and deletes `extra`, which mints a fresh epoch. Returns what `[everyone]` has
/// in flight on X before the delete, the recipients X's fresh epoch wraps, S, and the lab Macs
/// above X in recipient order.
fn seatless_retired_over_prefix() -> (
    BTreeSet<Recipient>,
    BTreeSet<Recipient>,
    Recipient,
    BTreeSet<Recipient>,
) {
    let lab = six_with(true);
    let (r, order) = in_order(&lab);
    let x_at = usize::from(lab.recipients[1] < lab.recipients[0]);
    let x = &lab.all[x_at];
    let mine = order.iter().position(|&i| i == x_at).unwrap();
    let seatless = key_above(&r[5]).recipient();
    let admin = admin_open(x).expect("X is an admin");
    let mut doc = admin.doc.clone();
    doc.add_machine("m99", &seatless.to_bech32()).unwrap();
    doc.set_entry(&Audience::Everyone, "extra", b"e1")
        .expect("edit");
    admin_publish_approved(x, &admin, &doc, &[seatless.to_bech32()]).expect("X lets S in");
    assert!(
        log_of(x).wraps_maximal_audience_to(&EpochAudience::Everyone, &seatless),
        "CONTROL: S is in [everyone]"
    );
    carry(&lab.all, x_at);
    let batch = rekey(&lab.all[order[5]], &EpochAudience::Everyone, |_| true);
    assert!(
        !wrapped_by(&log_of(&lab.all[order[5]]), batch[0].id()).contains(&seatless),
        "CONTROL: A's re-key leaves S out"
    );
    save_records(x, &encode_records(&batch[..mine + 2])).unwrap();
    let flight = log_of(x).in_flight(&EpochAudience::Everyone, &r[mine]);
    let before = ids(x);
    let admin = admin_open(x).expect("X is an admin");
    let mut doc = admin.doc.clone();
    assert!(
        doc.unset_entry(&Audience::Everyone, "extra")
            .expect("delete"),
        "CONTROL: X reads extra"
    );
    admin_publish(x, &admin, &doc).expect("X deletes extra");
    let signed = signed_since(x, &before);
    let minted = signed
        .iter()
        .find(|record| {
            matches!(
                record.body(),
                Body::Epoch {
                    audience: EpochAudience::Everyone,
                    ..
                }
            )
        })
        .expect("a delete from [everyone] mints a fresh epoch");
    let wraps = wrapped_by(&log_of(x), minted.id());
    lab.macs.server.stop();
    (
        flight,
        wraps,
        seatless,
        r[mine + 1..].iter().copied().collect(),
    )
}

/// A Mac with no seat that a retirement left out stays out over a prefix of the retiring batch
/// ([`seatless_retired_over_prefix`]): its `Wrap` is never in flight, so X's fresh epoch leaves
/// S out, while the seated Macs above X, whose `Wrap`s are still on their way, are carried.
///
/// NEGATIVE CONTROL: with `SEATLESS_FLY` (a recipient no seat binds is in flight too) S reads as
/// still arriving, X keeps it in its document and its fresh epoch wraps S: the revoked key reads
/// `[everyone]` again.
#[test]
fn a_seatless_mac_a_retirement_left_out_stays_out_over_a_prefix_of_the_batch() {
    let (flight, wraps, seatless, above) = seatless_retired_over_prefix();
    assert!(
        !flight.contains(&seatless),
        "S is not in flight: {flight:?}"
    );
    assert!(!wraps.contains(&seatless), "X's fresh epoch leaves S out");
    assert!(
        !above.is_empty() && above.is_subset(&flight) && above.is_subset(&wraps),
        "the seated Macs above X are in flight and carried: {flight:?}"
    );

    SEATLESS_FLY.with(|cell| cell.set(true));
    let (flight, wraps, seatless, _) = seatless_retired_over_prefix();
    SEATLESS_FLY.with(|cell| cell.set(false));
    assert!(flight.contains(&seatless), "CONTROL: S reads as in flight");
    assert!(
        wraps.contains(&seatless),
        "CONTROL: with S in flight, X's fresh epoch wraps the revoked key"
    );
}

/// The writer's shape: X, of m22 and m7 (both admins) the one with the lower recipient, holds
/// A's (`r5`'s) `[everyone]` re-key ([`rekey`]) only through its own `Wrap`, and edits `shared`
/// to `v2`. Then every Mac takes A's and X's records. Returns what X's edit signed and, for each
/// Mac above X in recipient order, whether it reads `v2`.
fn write_over_prefix() -> (Vec<Record>, Vec<bool>) {
    let lab = six_with(true);
    let (r, order) = in_order(&lab);
    let x_at = usize::from(lab.recipients[1] < lab.recipients[0]);
    let x = &lab.all[x_at];
    let mine = order.iter().position(|&i| i == x_at).unwrap();
    let batch = rekey(&lab.all[order[5]], &EpochAudience::Everyone, |_| true);
    let held = &batch[..mine + 2];
    assert_eq!(
        held[1..]
            .iter()
            .filter_map(|record| match record.body() {
                Body::Wrap { recipient, .. } => Some(*recipient),
                _ => None,
            })
            .collect::<Vec<_>>(),
        r[..=mine].to_vec(),
        "CONTROL: X holds A's batch through its own Wrap"
    );
    save_records(x, &encode_records(held)).unwrap();
    let before = ids(x);
    let admin = admin_open(x).expect("X is an admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"v2")
        .expect("edit");
    admin_publish(x, &admin, &doc).expect("X publishes the edit");
    let signed = signed_since(x, &before);
    carry(&lab.all, order[5]);
    carry(&lab.all, x_at);
    let reads = order[mine + 1..]
        .iter()
        .map(|&i| shared(&lab.all[i]).as_deref() == Some(b"v2".as_slice()))
        .collect();
    lab.macs.server.stop();
    (signed, reads)
}

/// A writer that holds a prefix of another Mac's re-key ([`write_over_prefix`]) seals under that
/// epoch, whose batch may wrap no Mac outside the edit's plan, and mints none: every Mac reads
/// the edit once the batch arrives. A writer made to mint (`log_entries::FRESH_OVER_PREFIX`)
/// carries the Macs the batch may still wrap, so they read it too.
///
/// NEGATIVE CONTROL: with `FRESH_OVER_PREFIX` and `PREFIX_BLIND` (a writer mints over an
/// unsettled epoch from what it holds alone, as before), the fresh epoch leaves every Mac above
/// X out and none of them reads the edit.
#[test]
fn a_writer_over_a_prefix_of_a_rekey_seals_under_it_and_every_mac_reads_the_edit() {
    let (signed, reads) = write_over_prefix();
    assert_eq!(
        epochs_by_audience(&signed),
        BTreeMap::new(),
        "X mints no epoch"
    );
    assert!(!reads.is_empty(), "CONTROL: a Mac sorts above X");
    assert!(
        reads.iter().all(|&read| read),
        "every Mac reads the edit: {reads:?}"
    );

    log_entries::FRESH_OVER_PREFIX.with(|cell| cell.set(true));
    let (signed, reads) = write_over_prefix();
    assert!(
        epochs_by_audience(&signed).contains_key(&EpochAudience::Everyone),
        "CONTROL: X mints over the prefix"
    );
    assert!(
        reads.iter().all(|&read| read),
        "the fresh epoch carries the Macs the batch may still wrap: {reads:?}"
    );
    PREFIX_BLIND.with(|cell| cell.set(true));
    let (_, reads) = write_over_prefix();
    PREFIX_BLIND.with(|cell| cell.set(false));
    log_entries::FRESH_OVER_PREFIX.with(|cell| cell.set(false));
    assert!(
        reads.iter().all(|&read| !read),
        "CONTROL: prefix-blind, the fresh epoch leaves the Macs above X out: {reads:?}"
    );
}

/// m22 removes m42 and m40 holds only the `Remove` at `t0`, so its clock for both audiences
/// starts there. At `t0` + 30 steps m40 takes m22's whole batch and with it m7's `Remove` of
/// m43, which m7 signed once it held m22's batch: the audiences stay due, on m7's removal now.
/// Returns what m40's passes at `t0` and at `t0` + 30 steps signed.
fn second_removal() -> (Vec<Record>, Vec<Record>) {
    let lab = six();
    let (m22, m7, m40) = (&lab.all[0], &lab.all[1], &lab.all[2]);
    let base = ids(m22);
    test_remove_unposted(m22, &join_code(&lab.recipients[4])).expect("m22 removes m42");
    let first = signed_since(m22, &base);
    save_records(m40, &encode_records(&first[..1])).unwrap();
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let early = vault_pass_at(m40, t0).unwrap();
    save_records(m7, &encode_records(&first)).unwrap();
    let held = ids(m7);
    test_remove_unposted(m7, &join_code(&lab.recipients[5])).expect("m7 removes m43");
    let second = signed_since(m7, &held);
    save_records(m40, &encode_records(&first)).unwrap();
    save_records(m40, &encode_records(&second[..1])).unwrap();
    let late = vault_pass_at(m40, t0 + 30 * GAP_STEP_MS).unwrap();
    lab.macs.server.stop();
    (early, late)
}

/// A removal that finds the audience due already still gets its grace ([`second_removal`]):
/// m40, no remover, waits [`REMOVER_GRACE_MS`] from when it first saw m7's removal, for m7's
/// rotation, and signs nothing at either pass.
///
/// NEGATIVE CONTROL: with `remove::GRACE_FROM_DUE` (the grace counts from the audience's
/// clock, started on m22's removal 30 steps before) m40 re-keys `[everyone]` at `t0` + 30 steps,
/// ahead of the remover.
#[test]
fn a_removal_that_finds_the_audience_due_still_gets_its_grace() {
    let (early, late) = second_removal();
    assert!(
        early.is_empty(),
        "m40 waits the grace on m22's removal: {early:?}"
    );
    assert!(
        late.is_empty(),
        "m40 waits the grace on m7's removal: {late:?}"
    );

    remove::GRACE_FROM_DUE.with(|cell| cell.set(true));
    let (_, late) = second_removal();
    remove::GRACE_FROM_DUE.with(|cell| cell.set(false));
    assert!(
        epochs_by_audience(&late).contains_key(&EpochAudience::Everyone),
        "CONTROL: a grace from the audience's clock lets m40 mint at once: {late:?}"
    );
}

/// Three concurrent Commons re-keys ([`rekey`]): m22's and m40's wrap every Mac, m7's leaves m43
/// out, so the audience is due a re-key covering them, which its carriers rank for with no
/// grace. B, a carrier ranked `k` >= 1, holds m22's and m7's at `t0`, and m40's, which arrives
/// settled, at `t0` + 30 seconds; its passes run then and at each step to `t0` + `k` steps.
/// Returns `k` and what each pass signed.
fn settled_head(moves: bool) -> (usize, Vec<Vec<Record>>) {
    let lab = six();
    let third = rekey(&lab.all[2], &EpochAudience::Commons, |_| true);
    let first = rekey(&lab.all[0], &EpochAudience::Commons, |_| true);
    let left_out = lab.recipients[5];
    let second = rekey(&lab.all[1], &EpochAudience::Commons, |r| *r != left_out);
    let turn_of = |log: &Log, actor: &MemberId| {
        log.rekey_dues()
            .into_iter()
            .find(|due| due.audience == EpochAudience::Commons)
            .and_then(|due| due.turn(&log.membership(), actor))
    };
    let mut probe = log_of(&lab.all[3]);
    for record in first.iter().chain(&second) {
        probe.insert(record.clone()).unwrap();
    }
    let (b, turn) = [3, 4]
        .into_iter()
        .find_map(|i| {
            turn_of(&probe, &lab.actors[i])
                .filter(|turn| turn.rank >= 1)
                .map(|turn| (i, turn))
        })
        .expect("a carrier ranked after the first");
    assert!(
        !turn.after_grace,
        "CONTROL: a covering re-key waits no grace"
    );
    let actor = lab.actors[b];
    let b = &lab.all[b];
    save_records(b, &encode_records(first.iter().chain(&second))).unwrap();
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    remove::HEADS_MOVE_RANKS.with(|cell| cell.set(moves));
    let mut passes = vec![vault_pass_at(b, t0).unwrap()];
    save_records(b, &encode_records(&third)).unwrap();
    assert_eq!(
        turn_of(&log_of(b), &actor),
        Some(turn),
        "CONTROL: the third head keeps B's rank"
    );
    passes.push(vault_pass_at(b, t0 + 30_000).unwrap());
    for step in 1..=turn.rank {
        let at = t0 + u64::try_from(step).unwrap() * GAP_STEP_MS;
        passes.push(vault_pass_at(b, at).unwrap());
    }
    remove::HEADS_MOVE_RANKS.with(|cell| cell.set(false));
    lab.macs.server.stop();
    (turn.rank, passes)
}

/// A head that arrives settled moves no rank ([`settled_head`]): B signs nothing before its
/// rank's step and mints the one covering Commons epoch at it.
///
/// NEGATIVE CONTROL: with `remove::HEADS_MOVE_RANKS` (a head moves the ranks to when this Mac
/// first saw it) the third head puts B's step 30 seconds later, and B signs nothing at it.
#[test]
fn a_head_that_arrives_settled_moves_no_rank() {
    let (k, passes) = settled_head(false);
    let (last, waited) = passes.split_last().unwrap();
    assert!(
        waited.iter().all(Vec::is_empty),
        "B waits its rank: {waited:?}"
    );
    assert_eq!(
        epochs_by_audience(last),
        BTreeMap::from([(EpochAudience::Commons, 1)]),
        "B mints at its rank's step {k}"
    );

    let (_, passes) = settled_head(true);
    assert!(
        passes.iter().all(Vec::is_empty),
        "CONTROL: a head that moves the ranks puts B past its step: {passes:?}"
    );
}

/// m22, an admin outside the ring `[m40 m7]` it publishes, wraps the ring's epoch to m7 and m40
/// alone and then removes m40, while m7 holds only the `Remove` and its vault moves to a path.
/// Returns m7's turn for the ring's re-key, the same under `MINTERS_CARRY` (a minter carries its
/// epoch without a `Wrap`), and whether the ring still reaches m40 after m7's next `sync`.
fn outsider_ring() -> (Option<Turn>, Option<Turn>, bool) {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (_m40, _, rec40) = let_in(&macs, "m40", "m40");
    sync(m22, None).expect("m22 syncs m40's admission");
    let ring = Audience::Hosts(vec!["m40".to_owned(), "m7".to_owned()]);
    let admin = admin_open(m22).expect("admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&ring, "ring-cred", b"ring-v1").expect("ring");
    admin_publish(m22, &admin, &doc).expect("publish the ring");
    let lists = EpochAudience::from(&ring);
    let log = log_of(m22);
    let epoch = log
        .records()
        .find(|record| matches!(record.body(), Body::Epoch { audience, .. } if *audience == lists))
        .map(|record| *record.id())
        .expect("the ring's epoch");
    let rec22 = read_identity(m22).unwrap().unwrap().recipient();
    assert!(
        !wrapped_by(&log, &epoch).contains(&rec22),
        "CONTROL: m22 holds no key of the ring"
    );
    sync(m7, None).expect("m7 syncs");
    let base = ids(m22);
    test_remove_unposted(m22, &join_code(&rec40)).expect("m22 removes m40");
    let chain = signed_since(m22, &base);
    save_records(m7, &encode_records(&chain[..1])).unwrap();
    let (_, actor7, _) = member_key(m7);
    let turn = || {
        let log = log_of(m7);
        log.rekey_dues()
            .into_iter()
            .find(|due| due.audience == lists)
            .and_then(|due| due.turn(&log.membership(), &actor7))
    };
    let plain = turn();
    MINTERS_CARRY.with(|cell| cell.set(true));
    let carried = turn();
    MINTERS_CARRY.with(|cell| cell.set(false));
    let mut config7 = config(m7).unwrap().unwrap();
    config7.source = Source::Path(macs.root.path().join("no-envelope.vault"));
    save_config(m7, &config7).unwrap();
    let mut state7 = load_state(m7).unwrap();
    state7.source_identity = Some(source_identity(&config7));
    save_state(m7, &state7).unwrap();
    let _ = sync(m7, None);
    let still = log_of(m7).wraps_maximal_audience_to(&lists, &rec40);
    macs.server.stop();
    (plain, carried, still)
}

/// A minter that wrapped its epoch only to others holds no key of it, so it carries none of its
/// re-keys ([`outsider_ring`]): with no remover that can carry the ring, m7 ranks first with no
/// grace, and its vault pass runs at a `sync` of a vault at a path too, whose Commons exchange
/// signs nothing, so the ring's rotation reaches m40 no more.
///
/// NEGATIVE CONTROLS: with `MINTERS_CARRY` m7 waits the grace for a rotation m22 cannot sign;
/// with `URL_ONLY_PASS` (only a vault at a URL runs the vault pass) the `sync` leaves the ring on
/// m40.
#[test]
fn an_outsider_minter_carries_no_host_list_and_a_vault_at_a_path_rotates_it() {
    let (plain, carried, still) = outsider_ring();
    assert_eq!(
        plain,
        Some(Turn {
            rank: 0,
            after_grace: false
        }),
        "m7 ranks first with no grace"
    );
    assert!(
        carried.is_some_and(|turn| turn.after_grace),
        "CONTROL: a minter that carries makes m7 wait the grace: {carried:?}"
    );
    assert!(!still, "m7's sync rotates the ring off m40");

    crate::client::URL_ONLY_PASS.with(|cell| cell.set(true));
    let (_, _, still) = outsider_ring();
    crate::client::URL_ONLY_PASS.with(|cell| cell.set(false));
    assert!(
        still,
        "CONTROL: a vault at a path that runs no pass leaves the ring on m40"
    );
}

/// m7 lets in m44 on its own side, and m22, which never hears of it, re-keys Commons
/// ([`rekey`]). m7 and m40..m43 take m22's `Epoch` and `Wrap`s but never its last record, so the
/// batch never settles there, and m22 runs no pass. Each step every holder's vault pass runs,
/// and what one signs reaches the others at the end of the step. Returns how many `Wrap`s hand
/// m22's epoch to m44, and every `(epoch, recipient)` two `Wrap`s hand on.
fn late_settle() -> (usize, Vec<(RecordId, Recipient)>) {
    let lab = six();
    let (_m44, _, rec44) = let_in_by(&lab.macs, &lab.macs.m7, "m44", "m44");
    let wire = load_records(&lab.all[1]).unwrap().encode();
    for paths in &lab.all[2..] {
        save_records(paths, &wire).unwrap();
    }
    let batch = rekey(&lab.all[0], &EpochAudience::Commons, |_| true);
    let epoch = *batch[0].id();
    assert!(
        !wrapped_by(&log_of(&lab.all[0]), &epoch).contains(&rec44),
        "CONTROL: m22 never heard of m44"
    );
    let holders = &lab.all[1..];
    let prefix = encode_records(&batch[..batch.len() - 1]);
    for paths in holders {
        save_records(paths, &prefix).unwrap();
    }
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    for k in 0..24 {
        let signers: Vec<usize> = (0..holders.len())
            .filter(|&i| {
                !vault_pass_at(&holders[i], t0 + k * GAP_STEP_MS)
                    .unwrap()
                    .is_empty()
            })
            .collect();
        for i in signers {
            carry(holders, i);
        }
    }
    let union = union(holders);
    let closed = union
        .records()
        .filter(|record| {
            matches!(
                record.body(),
                Body::Wrap { epoch: of, recipient, .. } if *of == epoch && *recipient == rec44
            )
        })
        .count();
    lab.macs.server.stop();
    (closed, duplicate_wraps(&union))
}

/// A wrap gap on a batch that settles late, or never, is closed once ([`late_settle`]): after
/// [`log_entries::SETTLE_GRACE_MS`] the holders' ranks count a step apart from when the grace let
/// the gap go, so one holder signs before the next looks, and no two `Wrap`s hand one epoch to
/// one Mac.
///
/// NEGATIVE CONTROLS: with `GAPS_FROM_FIRST_SIGHT` (ranks counted from first sight, so the
/// grace puts every rank due at once) or `remove::UNRANKED` (every holder first) two or more
/// holders sign m44's `Wrap` in one step.
#[test]
fn a_gap_on_a_batch_that_settles_late_is_closed_once() {
    let (closed, duplicates) = late_settle();
    assert_eq!(closed, 1, "one holder hands m44 the epoch");
    assert_eq!(duplicates, Vec::new(), "no duplicate wraps");

    log_entries::GAPS_FROM_FIRST_SIGHT.with(|cell| cell.set(true));
    let (from_first_sight, _) = late_settle();
    log_entries::GAPS_FROM_FIRST_SIGHT.with(|cell| cell.set(false));
    assert!(
        from_first_sight >= 2,
        "CONTROL: ranks from first sight close the gap {from_first_sight} times"
    );

    remove::UNRANKED.with(|cell| cell.set(true));
    let (unranked, _) = late_settle();
    remove::UNRANKED.with(|cell| cell.set(false));
    assert!(
        unranked >= 2,
        "CONTROL: unranked holders close the gap {unranked} times"
    );
}

/// The m4 shape with a Mac S that has no seat ([`m4_shape_with`]): A's batch wraps S last, so
/// X's prefix lacks its `Wrap`, which only the rest of the batch says is coming or left out on
/// purpose (a retirement). X waits for the batch [`log_entries::SETTLE_GRACE_MS`] from first
/// sight, as for any prefix: where it arrives within the grace, X's one higher epoch wraps S
/// with the seated Macs. Past the grace X re-keys over the prefix, carries the seated Macs in
/// flight and leaves S out, as a removal's own rotation over it would: no record marks a
/// batch's end, so a whole one reads as a prefix until its minter signs again
/// ([`a_batch_that_ends_in_wraps_holds_a_removal_only_the_settle_grace`]).
///
/// NEGATIVE CONTROL: with `remove::UNDECIDED_HOLDS` (the grace does not let the audience go
/// while S is undecided, as before) X mints no `[everyone]` epoch at any step.
#[test]
fn a_mac_with_no_seat_holds_the_rekey_only_the_settle_grace() {
    let grace = log_entries::SETTLE_GRACE_MS / GAP_STEP_MS;
    let (first, settled, r, seatless) = m4_shape_with(Some(5), true);
    let seatless = seatless.unwrap();
    let settled = settled.expect("X re-keys once the batch settles");
    assert!(first.is_some_and(|k| k < grace), "signed at {first:?}");
    assert_eq!(
        settled,
        BTreeSet::from([r[0], r[2], r[3], r[5], seatless]),
        "X's epoch after the batch wraps S and every Mac neither removal names"
    );

    let (silent, past, r, seatless) = m4_shape_with(None, true);
    let past = past.expect("a silent minter does not hold the audience forever");
    assert!(
        silent >= Some(grace),
        "signed at {silent:?}, before the grace"
    );
    assert!(
        !past.contains(&seatless.unwrap()) && past.contains(&r[3]) && past.contains(&r[5]),
        "past the grace X's epoch carries the seated Macs and leaves S out: {past:?}"
    );

    remove::UNDECIDED_HOLDS.with(|cell| cell.set(true));
    let (_, held, _, _) = m4_shape_with(None, true);
    remove::UNDECIDED_HOLDS.with(|cell| cell.set(false));
    assert_eq!(
        held, None,
        "CONTROL: an undecided hold mints no [everyone] epoch"
    );
}

/// A retirement whose batch ends in `Wrap`s, and a removal after it. Of six Macs in recipient
/// order `r0..r5`, X, the lower of the two admins m22 and m7, lets a `[machines]`-only key S
/// into `[everyone]` (S sorts above every lab Mac). A (`r5`'s Mac) re-keys `[everyone]` to the
/// six members alone ([`rekey`]), as a retirement of S signs it, and X takes all of that batch
/// but its last record: the epoch and every `Wrap`, which is the whole of a batch whose last
/// audience its minter does not open whole (a removal's Commons rotation then ends in its
/// `Wrap`s), and nothing tells X it is whole. Z, the lowest Mac but X and A, then removes T, the
/// next, and X takes only Z's `Remove`. X's passes run at steps `0..=steps`. Returns the
/// recipients of X's first `[everyone]` epoch if it signs one, the recipients, S, and T's.
fn removal_after_wrapped_retirement(
    steps: u64,
) -> (
    Option<BTreeSet<Recipient>>,
    Vec<Recipient>,
    Recipient,
    Recipient,
) {
    let lab = six_with(true);
    let (r, order) = in_order(&lab);
    let x_at = usize::from(lab.recipients[1] < lab.recipients[0]);
    let x = &lab.all[x_at];
    let mine = order.iter().position(|&i| i == x_at).unwrap();
    let seatless = key_above(&r[5]).recipient();
    let admin = admin_open(x).expect("X is an admin");
    let mut doc = admin.doc.clone();
    doc.add_machine("m99", &seatless.to_bech32()).unwrap();
    admin_publish_approved(x, &admin, &doc, &[seatless.to_bech32()]).expect("X lets S in");
    carry(&lab.all, x_at);
    let batch = rekey(&lab.all[order[5]], &EpochAudience::Everyone, |_| true);
    assert!(
        !wrapped_by(&log_of(&lab.all[order[5]]), batch[0].id()).contains(&seatless),
        "CONTROL: A's re-key leaves S out"
    );
    save_records(x, &encode_records(&batch[..batch.len() - 1])).unwrap();
    assert!(
        log_of(x)
            .undecided(&EpochAudience::Everyone, &r[mine])
            .contains(&seatless),
        "CONTROL: X cannot tell A's whole batch from a prefix"
    );
    let others: Vec<usize> = (0..5).filter(|&k| k != mine).collect();
    let (z, t) = (&lab.all[order[others[0]]], others[1]);
    let base = ids(z);
    test_remove_unposted(z, &join_code(&r[t])).expect("Z removes T");
    let chain = signed_since(z, &base);
    assert!(matches!(chain[0].body(), Body::Remove { .. }));
    save_records(x, &encode_records(&chain[..1])).unwrap();
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let mut wraps = None;
    for k in 0..=steps {
        let signed = vault_pass_at(x, t0 + k * GAP_STEP_MS).unwrap();
        if wraps.is_none()
            && let Some(minted) = signed.iter().find(|record| {
                matches!(
                    record.body(),
                    Body::Epoch {
                        audience: EpochAudience::Everyone,
                        ..
                    }
                )
            })
        {
            wraps = Some(wrapped_by(&log_of(x), minted.id()));
        }
    }
    lab.macs.server.stop();
    let removed = r[t];
    (wraps, r, seatless, removed)
}

/// A whole batch that ends in `Wrap`s and left a Mac with no seat out reads as a prefix for good
/// ([`removal_after_wrapped_retirement`]), yet holds a removal's re-key only the settle grace:
/// once the grace, Z's and X's rank have passed, X rotates `[everyone]` without T and without S.
///
/// NEGATIVE CONTROL: with `remove::UNDECIDED_HOLDS` (the grace does not let the audience go
/// while S is undecided, as before) X signs no `[everyone]` epoch at any step, and T keeps the
/// newest key until A signs again.
#[test]
fn a_batch_that_ends_in_wraps_holds_a_removal_only_the_settle_grace() {
    let steps = 4 * log_entries::SETTLE_GRACE_MS / GAP_STEP_MS;
    let (wraps, r, seatless, removed) = removal_after_wrapped_retirement(steps);
    let wraps = wraps.expect("X rotates [everyone] past every grace");
    let kept: BTreeSet<Recipient> = r.iter().copied().filter(|r| *r != removed).collect();
    assert_eq!(wraps, kept, "X's epoch leaves T and S out");
    assert!(!wraps.contains(&seatless));

    remove::UNDECIDED_HOLDS.with(|cell| cell.set(true));
    let (held, _, _, _) = removal_after_wrapped_retirement(steps);
    remove::UNDECIDED_HOLDS.with(|cell| cell.set(false));
    assert_eq!(
        held, None,
        "CONTROL: an undecided hold never lets [everyone] go"
    );
}

/// The writer's shape for a Mac with no seat that a re-key keeps. Of six Macs in recipient order
/// `r0..r5`, X, the lower of the two admins, lets a `[machines]`-only key S into `[everyone]`
/// (S sorts above every lab Mac). A (`r5`'s Mac) re-keys `[everyone]` to the six members and S
/// ([`rekey_to`]); X holds that batch only through its own `Wrap` and edits `shared` to `v2`.
/// Then X takes A's whole batch. Returns what X's edit signed, whether X's document kept S
/// over the prefix, whether S reads `v2`, and whether the log then leaves S out of
/// `[everyone]`.
fn seatless_kept_over_prefix() -> (Vec<Record>, bool, bool, bool) {
    let lab = six_with(true);
    let (r, order) = in_order(&lab);
    let x_at = usize::from(lab.recipients[1] < lab.recipients[0]);
    let x = &lab.all[x_at];
    let mine = order.iter().position(|&i| i == x_at).unwrap();
    let identity = key_above(&r[5]);
    let seatless = identity.recipient();
    let admin = admin_open(x).expect("X is an admin");
    let mut doc = admin.doc.clone();
    doc.add_machine("m99", &seatless.to_bech32()).unwrap();
    admin_publish_approved(x, &admin, &doc, &[seatless.to_bech32()]).expect("X lets S in");
    carry(&lab.all, x_at);
    let a = &lab.all[order[5]];
    let mut to: BTreeSet<Recipient> = r.iter().copied().collect();
    to.insert(seatless);
    let batch = rekey_to(a, &EpochAudience::Everyone, &to);
    assert!(
        wrapped_by(&log_of(a), batch[0].id()).contains(&seatless),
        "CONTROL: A's re-key keeps S"
    );
    save_records(x, &encode_records(&batch[..mine + 2])).unwrap();
    let before = ids(x);
    let admin = admin_open(x).expect("X is an admin");
    let kept = admin
        .doc
        .machines()
        .iter()
        .any(|machine| machine.recipient == seatless.to_bech32());
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"v2")
        .expect("edit");
    admin_publish(x, &admin, &doc).expect("X publishes the edit");
    let signed = signed_since(x, &before);
    save_records(x, &encode_records(&batch)).unwrap();
    let log = log_of(x);
    let reads = log
        .open_audience_entries(&identity)
        .unwrap()
        .and_then(|opened| {
            opened
                .by_audience
                .get(&Audience::Everyone)
                .and_then(|entries| entries.get("shared"))
                .map(|entry| entry.value.as_slice() == b"v2")
        })
        .unwrap_or(false);
    let left = log.audience_left_out_recipient(&EpochAudience::Everyone, &seatless);
    lab.macs.server.stop();
    (signed, kept, reads, left)
}

/// A writer over a prefix of a re-key that may still wrap a Mac with no seat
/// ([`seatless_kept_over_prefix`]) leaves that Mac to the batch: its document keeps S, it seals
/// under A's epoch and mints none, and once A's batch arrives S reads the edit and is not left
/// out.
///
/// NEGATIVE CONTROL: with `UNDECIDED_BLIND` (S's missing `Wrap` reads as left out on purpose, as
/// before) X drops S from its document and mints a fresh epoch without it, which is the newest
/// once A's batch arrives: S reads nothing of the edit and is left out for good.
#[test]
fn a_writer_leaves_a_mac_with_no_seat_to_the_batch_that_may_wrap_it() {
    let (signed, kept, reads, left) = seatless_kept_over_prefix();
    assert!(kept, "X's document keeps S over the prefix");
    assert_eq!(
        epochs_by_audience(&signed),
        BTreeMap::new(),
        "X mints no epoch"
    );
    assert!(reads, "S reads the edit once the batch arrives");
    assert!(!left, "S is not left out");

    UNDECIDED_BLIND.with(|cell| cell.set(true));
    let (signed, kept, reads, left) = seatless_kept_over_prefix();
    UNDECIDED_BLIND.with(|cell| cell.set(false));
    assert!(!kept, "CONTROL: X's document drops S");
    assert!(
        epochs_by_audience(&signed).contains_key(&EpochAudience::Everyone),
        "CONTROL: X mints a fresh epoch"
    );
    assert!(!reads && left, "CONTROL: S is left out for good");
}

/// An approval over a prefix of a re-key. Of six Macs in recipient order `r0..r5`, A (`r5`'s
/// Mac) re-keys `[everyone]` to the six members ([`rekey`]); X, the lower of the two admins,
/// holds that batch only through its own `Wrap`, and approves a `[machines]`-only key N with an
/// edit of `shared` to `v2`. Then X takes A's whole batch. Returns what X's approval signed and
/// whether N reads `v2`.
fn approval_over_prefix() -> (Vec<Record>, bool) {
    let lab = six_with(true);
    let (r, order) = in_order(&lab);
    let x_at = usize::from(lab.recipients[1] < lab.recipients[0]);
    let x = &lab.all[x_at];
    let mine = order.iter().position(|&i| i == x_at).unwrap();
    let batch = rekey(&lab.all[order[5]], &EpochAudience::Everyone, |_| true);
    save_records(x, &encode_records(&batch[..mine + 2])).unwrap();
    assert!(
        !log_of(x).unsettled(&[*batch[0].id()]).is_empty(),
        "CONTROL: X holds a prefix"
    );
    let identity = Identity::from_secret_bytes([0x77; 32]);
    let newcomer = identity.recipient();
    assert!(!r.contains(&newcomer));
    let before = ids(x);
    let admin = admin_open(x).expect("X is an admin");
    let mut doc = admin.doc.clone();
    doc.add_machine("m98", &newcomer.to_bech32()).unwrap();
    doc.set_entry(&Audience::Everyone, "shared", b"v2")
        .expect("edit");
    admin_publish_approved(x, &admin, &doc, &[newcomer.to_bech32()]).expect("X lets N in");
    let signed = signed_since(x, &before);
    save_records(x, &encode_records(&batch)).unwrap();
    let reads = log_of(x)
        .open_audience_entries(&identity)
        .unwrap()
        .and_then(|opened| {
            opened
                .by_audience
                .get(&Audience::Everyone)
                .and_then(|entries| entries.get("shared"))
                .map(|entry| entry.value.as_slice() == b"v2")
        })
        .unwrap_or(false);
    lab.macs.server.stop();
    (signed, reads)
}

/// A writer that seals under another Mac's epoch held as a prefix ([`approval_over_prefix`])
/// wraps it to each Mac of its plan the rest of that batch cannot bring: N, new to the plan,
/// reads the approval's edit.
///
/// NEGATIVE CONTROL: with `log_entries::PREFIX_GAPS_SKIPPED` (no `Wrap` of an epoch whose batch
/// may still be arriving, as before) N holds no key of the epoch the edit is sealed under and
/// cannot read it.
#[test]
fn an_approval_over_a_prefix_wraps_the_new_mac_the_batch_cannot_bring() {
    let (signed, reads) = approval_over_prefix();
    assert!(
        !epochs_by_audience(&signed).contains_key(&EpochAudience::Everyone),
        "CONTROL: X seals under A's epoch"
    );
    assert!(reads, "N reads the edit");

    log_entries::PREFIX_GAPS_SKIPPED.with(|cell| cell.set(true));
    let (_, reads) = approval_over_prefix();
    log_entries::PREFIX_GAPS_SKIPPED.with(|cell| cell.set(false));
    assert!(!reads, "CONTROL: N cannot read what the edit sealed");
}

/// The ring's shape over a prefix of `[everyone]`. Of six Macs in recipient order `r0..r5`, m22
/// (an admin) publishes a ring of `r0`'s X, `r1`'s, `r3`'s and A, `r5`'s Mac. A removes `r4`'s
/// (not in the ring), X holds A's batch only through its `[everyone]` `Wrap` to `r2`
/// ([`a_prefix`]), and then removes `r1`'s Mac, which rotates `[everyone]` and then the ring in
/// its own batch. Returns the recipients X's rotation of the ring wraps, and the recipients.
fn ring_over_everyone_prefix() -> (BTreeSet<Recipient>, Vec<Recipient>) {
    const NAMES: [&str; 6] = ["m22", "m7", "m40", "m41", "m42", "m43"];
    let lab = six();
    let (r, order) = in_order(&lab);
    let mut hosts: Vec<String> = [0, 1, 3, 5]
        .iter()
        .map(|&k| NAMES[order[k]].to_owned())
        .collect();
    hosts.sort();
    let ring = Audience::Hosts(hosts);
    let admin = admin_open(&lab.all[0]).expect("m22 is an admin");
    let mut doc = admin.doc.clone();
    doc.set_entry(&ring, "ring-cred", b"ring-v1").expect("ring");
    admin_publish(&lab.all[0], &admin, &doc).expect("m22 publishes the ring");
    carry(&lab.all, 0);
    let list = EpochAudience::from(&ring);
    let (x, a) = (&lab.all[order[0]], &lab.all[order[5]]);
    let chain = a_removes_r4(a, &r);
    save_records(x, &encode_records(a_prefix(&chain, &r))).unwrap();
    let before = ids(x);
    test_remove_unposted(x, &join_code(&r[1])).expect("X removes r1");
    let signed = signed_since(x, &before);
    let minted = signed
        .iter()
        .find(|record| matches!(record.body(), Body::Epoch { audience, .. } if *audience == list))
        .expect("X's removal rotates the ring");
    let wraps = wrapped_by(&log_of(x), minted.id());
    lab.macs.server.stop();
    (wraps, r)
}

/// A removal over a prefix of another Mac's `[everyone]` rotation ([`ring_over_everyone_prefix`])
/// reads what is in flight once, before its first mint: its ring rotation, minted after its own
/// `[everyone]` epoch, still carries `r3` and `r5`, whose `[everyone]` `Wrap`s A's batch may
/// still bring.
///
/// NEGATIVE CONTROL: with `log_entries::FLIGHT_AFTER_MINT` (the ring reads what is in flight
/// from the log after X's own `[everyone]` epoch, where nothing is) X reads `r3` and `r5` as left
/// out of `[everyone]` and wraps the ring to `r0` alone.
#[test]
fn a_removal_reads_the_flight_before_its_first_mint_for_every_audience() {
    let (wraps, r) = ring_over_everyone_prefix();
    assert_eq!(wraps, BTreeSet::from([r[0], r[3], r[5]]));

    log_entries::FLIGHT_AFTER_MINT.with(|cell| cell.set(true));
    let (narrowed, r) = ring_over_everyone_prefix();
    log_entries::FLIGHT_AFTER_MINT.with(|cell| cell.set(false));
    assert_eq!(
        narrowed,
        BTreeSet::from([r[0]]),
        "CONTROL: read after the mint, the ring narrows"
    );
}

/// The m4 shape ([`m4_shape`]) on one tender pass well past every grace, which re-keys
/// `[everyone]` and Commons together. Returns the recipients of the Commons epoch it mints, and
/// the recipients.
fn m4_one_pass() -> (BTreeSet<Recipient>, Vec<Recipient>) {
    let lab = six();
    let (r, order) = in_order(&lab);
    let (x, a) = (&lab.all[order[0]], &lab.all[order[5]]);
    test_remove_unposted(x, &join_code(&r[1])).expect("X removes r1");
    let chain = a_removes_r4(a, &r);
    save_records(x, &encode_records(a_prefix(&chain, &r))).unwrap();
    let me = member_key(x).1;
    let t0 = aterm_time::unix_millis().unwrap();
    let (first, _) = ready_rekeys(&log_of(x), &me, &TendMark::default(), t0);
    let now = t0 + 100 * GAP_STEP_MS;
    let (mark, ready) = ready_rekeys(&log_of(x), &me, &first, now);
    assert!(
        ready.contains(&EpochAudience::Everyone) && ready.contains(&EpochAudience::Commons),
        "CONTROL: both audiences are ready on one pass: {ready:?}"
    );
    let identity = read_identity(x).unwrap().unwrap();
    let minted = tend_ready_epochs(x, now, &identity, &mark).unwrap();
    let commons = minted
        .iter()
        .find(|record| {
            matches!(
                record.body(),
                Body::Epoch {
                    audience: EpochAudience::Commons,
                    ..
                }
            )
        })
        .expect("the pass re-keys Commons");
    let wraps = wrapped_by(&log_of(x), commons.id());
    lab.macs.server.stop();
    (wraps, r)
}

/// A tender pass that re-keys `[everyone]` and then Commons ([`m4_one_pass`]) reads what is in
/// flight before its first mint: the Commons epoch carries `r3` and `r5`, whose `[everyone]`
/// `Wrap`s A's batch may still bring.
///
/// NEGATIVE CONTROL: with `log_entries::FLIGHT_AFTER_MINT` Commons reads what is in flight after
/// the pass's own `[everyone]` epoch, reads `r3` and `r5` as left out of `[everyone]`, and its
/// epoch wraps only `r0` and `r2`.
#[test]
fn a_tender_pass_reads_the_flight_before_its_first_mint_for_every_audience() {
    let (wraps, r) = m4_one_pass();
    assert_eq!(wraps, BTreeSet::from([r[0], r[2], r[3], r[5]]));

    log_entries::FLIGHT_AFTER_MINT.with(|cell| cell.set(true));
    let (narrowed, r) = m4_one_pass();
    log_entries::FLIGHT_AFTER_MINT.with(|cell| cell.set(false));
    assert_eq!(
        narrowed,
        BTreeSet::from([r[0], r[2]]),
        "CONTROL: read after the mint, Commons narrows"
    );
}

/// A host list's shape over a prefix of `[everyone]` with a Mac with no seat. Of six Macs in
/// recipient order `r0..r5`, m22 (an admin) lets a `[machines]`-only key S into `[everyone]` as
/// `m99` (S sorts above every lab Mac) and publishes a ring of `r0`'s Mac X, `r4`'s and S. A
/// (`r5`'s Mac, not in the ring) removes `r4`'s: its batch wraps `[everyone]` to S last and
/// cannot rotate the ring, so the ring's one carrier, X, ranks first with no grace. X holds A's
/// batch only through its `[everyone]` `Wrap` to `r2` ([`a_prefix`]) from step 0, and the rest of
/// it from step `rest` if one is given. X's passes run at steps `0..=15`. Returns the step X
/// re-keys the ring at, the recipients its epoch wraps, whether the ring's newest epochs wrap S
/// after the last step, and S.
fn ring_over_seatless_prefix(
    rest: Option<u64>,
) -> (Option<u64>, BTreeSet<Recipient>, bool, Recipient) {
    const NAMES: [&str; 6] = ["m22", "m7", "m40", "m41", "m42", "m43"];
    let lab = six();
    let (r, order) = in_order(&lab);
    let seatless = key_above(&r[5]).recipient();
    let mut hosts = vec![
        NAMES[order[0]].to_owned(),
        NAMES[order[4]].to_owned(),
        "m99".to_owned(),
    ];
    hosts.sort();
    let ring = Audience::Hosts(hosts);
    let admin = admin_open(&lab.all[0]).expect("m22 is an admin");
    let mut doc = admin.doc.clone();
    doc.add_machine("m99", &seatless.to_bech32()).unwrap();
    doc.set_entry(&ring, "ring-cred", b"ring-v1").expect("ring");
    admin_publish_approved(&lab.all[0], &admin, &doc, &[seatless.to_bech32()])
        .expect("m22 lets S in and publishes the ring");
    carry(&lab.all, 0);
    let list = EpochAudience::from(&ring);
    assert!(
        log_of(&lab.all[0]).wraps_maximal_audience_to(&list, &seatless),
        "CONTROL: the ring wraps S"
    );
    let (x, a) = (&lab.all[order[0]], &lab.all[order[5]]);
    let chain = a_removes_r4(a, &r);
    let epoch = chain
        .iter()
        .find(|record| matches!(record.body(), Body::Epoch { .. }))
        .unwrap();
    assert!(
        wrapped_by(&log_of(a), epoch.id()).contains(&seatless),
        "CONTROL: A's batch wraps S"
    );
    assert!(
        !chain.iter().any(
            |record| matches!(record.body(), Body::Epoch { audience, .. } if *audience == list)
        ),
        "CONTROL: A cannot rotate the ring"
    );
    save_records(x, &encode_records(a_prefix(&chain, &r))).unwrap();
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let mut at = None;
    let mut wraps = BTreeSet::new();
    for k in 0..16 {
        if rest == Some(k) {
            save_records(x, &encode_records(&chain)).unwrap();
        }
        let signed = vault_pass_at(x, t0 + k * GAP_STEP_MS).unwrap();
        if at.is_none()
            && let Some(minted) = signed.iter().find(
                |record| matches!(record.body(), Body::Epoch { audience, .. } if *audience == list),
            )
        {
            at = Some(k);
            wraps = wrapped_by(&log_of(x), minted.id());
        }
    }
    let kept = log_of(x).wraps_maximal_audience_to(&list, &seatless);
    lab.macs.server.stop();
    (at, wraps, kept, seatless)
}

/// A re-key of a host list reads `[everyone]`'s leave-out too, so it waits for `[everyone]`'s
/// batch while that may still decide a Mac with no seat the list wraps
/// ([`ring_over_seatless_prefix`]): X re-keys the ring once A's batch arrives, and its epoch
/// keeps S. A batch that never settles holds the ring only the settle grace, after which X's
/// epoch leaves S out, as a removal's own rotation over the prefix would.
///
/// NEGATIVE CONTROL: with `remove::EVERYONE_UNREAD` (a re-key waits on its own audience's
/// epochs alone, as before) X re-keys the ring from the prefix at its first pass, reads S as
/// left out of `[everyone]`, and S is out of the ring for good once A's batch arrives.
#[test]
fn a_host_list_rekey_waits_for_the_everyone_batch_that_decides_a_mac_with_no_seat() {
    let (at, wraps, kept, seatless) = ring_over_seatless_prefix(Some(3));
    assert_eq!(at, Some(3), "X re-keys the ring once A's batch arrives");
    assert!(wraps.contains(&seatless) && kept, "X's ring epoch keeps S");

    let grace = log_entries::SETTLE_GRACE_MS / GAP_STEP_MS;
    let (late, narrowed, _, seatless) = ring_over_seatless_prefix(None);
    assert_eq!(
        late,
        Some(grace),
        "a batch that never settles holds the ring the grace"
    );
    assert!(
        !narrowed.contains(&seatless),
        "past the grace X leaves S out"
    );

    remove::EVERYONE_UNREAD.with(|cell| cell.set(true));
    let (early, blind, kept, seatless) = ring_over_seatless_prefix(Some(3));
    remove::EVERYONE_UNREAD.with(|cell| cell.set(false));
    assert_eq!(
        early,
        Some(0),
        "CONTROL: X re-keys the ring from the prefix at once"
    );
    assert!(
        !blind.contains(&seatless) && !kept,
        "CONTROL: S is out of the ring for good"
    );
}
