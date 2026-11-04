// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One minter per due re-key: lab Macs on an injected clock, each Mac's vault passes
//! ([`test_tend_at`]: the ranked gap closer, then the ranked tender) run one
//! [`log_entries::GAP_STEP_MS`] apart while the records move between them as causal prefixes of
//! each minter's batch, as the exchange moves a few records a round.

use super::*;
use crate::client::log_entries::{GAP_STEP_MS, REMOVER_GRACE_MS};
use crate::membership_log::{Turn, rekey::TWINS_DUE};

/// Six lab Macs: `m22`, `m7` and four `m7` lets in, each holding every record; their actors and
/// age recipients, in that order.
struct Six {
    macs: TwoMacs,
    all: Vec<Paths>,
    actors: Vec<MemberId>,
    recipients: Vec<Recipient>,
}

fn six() -> Six {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
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
            let mine = test_tend_at(&lab.all[i], t0 + k * GAP_STEP_MS).unwrap();
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
/// finds them covered), no two `Wrap`s hand one epoch to one Mac, and nothing is due after.
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
    assert_eq!(duplicate_wraps(&union), Vec::new(), "no duplicate wraps");
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
            before.extend(test_tend_at(paths, t0 + k * GAP_STEP_MS).unwrap());
        }
    }
    for i in 0..seated.len() {
        carry(seated, i);
    }
    let mut after = Vec::new();
    for k in 16..32 {
        for i in 0..seated.len() {
            let mine = test_tend_at(&seated[i], t0 + k * GAP_STEP_MS).unwrap();
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

/// An audience can remain due while a newly arrived epoch changes its settledness.
/// Authoring must reconsider the scheduling observation before minting.
#[test]
fn authoring_rechecks_a_new_prefix_after_the_scheduling_observation() {
    let lab = six();
    let mut order: Vec<usize> = (0..6).collect();
    order.sort_by_key(|&i| lab.recipients[i]);
    let r: Vec<Recipient> = order.iter().map(|&i| lab.recipients[i]).collect();
    let (x, a) = (&lab.all[order[0]], &lab.all[order[5]]);
    let base = ids(a);
    test_remove_unposted(x, &join_code(&r[1])).unwrap();
    test_remove_unposted(a, &join_code(&r[4])).unwrap();
    let chain = signed_since(a, &base);
    let epoch = chain
        .iter()
        .position(|record| matches!(record.body(), Body::Epoch { .. }))
        .unwrap();
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
    save_records(x, &encode_records(&chain[..epoch + 4])).unwrap();
    assert!(
        log_of(x)
            .rekey_dues()
            .iter()
            .any(|due| due.audience == EpochAudience::Everyone)
    );
    let identity = read_identity(x).unwrap().unwrap();
    let minted = tend_ready_epochs(x, now, &identity, &mark).unwrap();
    assert!(!epochs_by_audience(&minted).contains_key(&EpochAudience::Everyone));
    lab.macs.server.stop();
}

/// The m4 shape. Of six Macs in recipient order `r0..r5`, `r0`'s Mac X removes `r1`'s and
/// `r5`'s Mac A removes `r4`'s, each on its own side. X holds A's `Remove`, A's `[everyone]`
/// `Epoch` and its first three `Wrap`s (to `r0`, `r1` and `r2`, in recipient order) from step
/// 0, and the rest of A's batches from step `rest` if one is given. X's passes run at steps
/// 0..=15. Returns the step of X's first signed record, the recipients its first
/// `[everyone]` epoch wraps, and the recipients.
fn m4_shape(rest: Option<u64>) -> (Option<u64>, BTreeSet<Recipient>, Vec<Recipient>) {
    let lab = six();
    let mut order: Vec<usize> = (0..6).collect();
    order.sort_by_key(|&i| lab.recipients[i]);
    let r: Vec<Recipient> = order.iter().map(|&i| lab.recipients[i]).collect();
    let (x, a) = (&lab.all[order[0]], &lab.all[order[5]]);
    let base = ids(a);
    test_remove_unposted(x, &join_code(&r[1])).expect("X removes r1");
    test_remove_unposted(a, &join_code(&r[4])).expect("A removes r4");
    let chain = signed_since(a, &base);
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
    save_records(x, &encode_records(held)).unwrap();
    let t0 = aterm_time::unix_millis().unwrap_or(0);
    let mut first = None;
    let mut wraps = BTreeSet::new();
    for k in 0..16 {
        if rest == Some(k) {
            save_records(x, &encode_records(&chain)).unwrap();
        }
        let signed = test_tend_at(x, t0 + k * GAP_STEP_MS).unwrap();
        if first.is_none() && !signed.is_empty() {
            first = Some(k);
            let log = log_of(x);
            if let Some(minted) = signed.iter().find(|record| {
                matches!(
                    record.body(),
                    Body::Epoch {
                        audience: EpochAudience::Everyone,
                        ..
                    }
                )
            }) {
                wraps = wrapped_by(&log, minted.id());
            }
        }
    }
    lab.macs.server.stop();
    (first, wraps, r)
}

/// The m4 shape ([`m4_shape`]): X signs nothing while it holds only a prefix of A's batch, and
/// once the batch settles its one higher `[everyone]` epoch wraps every Mac neither removal
/// names. A minter that falls silent after its last `Wrap` holds the audience only
/// [`log_entries::SETTLE_GRACE_MS`] from first sight.
///
/// NEGATIVE CONTROL: with `log_entries::UNSETTLED_OK` (every epoch reads as settled), X mints
/// at its first turn from the prefix, reads `r3` and `r5`, whose `Wrap`s it lacks, as left out
/// on purpose, and its new epoch wraps only `r0` and `r2`.
#[test]
fn nothing_is_signed_from_an_epoch_held_as_a_prefix_until_its_batch_settles() {
    let (first, wraps, r) = m4_shape(Some(5));
    let first = first.expect("X re-keys once the batch settles");
    assert!((5..=6).contains(&first), "signed at step {first}");
    assert_eq!(wraps, BTreeSet::from([r[0], r[2], r[3], r[5]]));

    let (silent, _, _) = m4_shape(None);
    let silent = silent.expect("a silent minter does not hold the audience forever");
    let grace = log_entries::SETTLE_GRACE_MS / GAP_STEP_MS;
    assert!(silent >= grace, "signed at step {silent}, before the grace");

    log_entries::UNSETTLED_OK.with(|cell| cell.set(true));
    let (early, narrowed, r) = m4_shape(Some(5));
    log_entries::UNSETTLED_OK.with(|cell| cell.set(false));
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
    let signed = test_tend_at(m22, t0).unwrap();
    assert_eq!(
        epochs_by_audience(&signed),
        BTreeMap::from([(lists.clone(), 1)]),
        "m22 rotates the ring at once and nothing else"
    );
    assert!(!log_of(m22).wraps_maximal_audience_to(&lists, &rec40));
    let mut waited = Vec::new();
    let steps = REMOVER_GRACE_MS / GAP_STEP_MS;
    for k in 1..steps {
        waited.extend(test_tend_at(m22, t0 + k * GAP_STEP_MS).unwrap());
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
