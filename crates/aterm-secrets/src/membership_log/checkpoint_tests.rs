// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault checkpoint as a pure function of the log (tracker FL-148b): its cut, its keep set,
//! its finality, the share rule, and the `VaultCheckpoint` model they refine.

use super::*;
use crate::age;
use crate::commons::binding::Request;
use crate::membership_log::{Draft, Entrant, Inserted};
use crate::text::{Audience, Entry};
use aterm_spec::interp::State;
use aterm_spec::xref::StepEvidence;
use zeroize::Zeroizing;

/// One Mac, its keys all derived from its tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::membership_log) struct Mac(pub(in crate::membership_log) u8);

pub(in crate::membership_log) const A: Mac = Mac(1);
pub(in crate::membership_log) const B: Mac = Mac(2);
pub(in crate::membership_log) const C: Mac = Mac(3);

impl Mac {
    pub(in crate::membership_log) fn seed(self) -> [u8; 32] {
        [self.0; 32]
    }
    pub(in crate::membership_log) fn incarnation(self) -> [u8; 16] {
        [self.0; 16]
    }
    pub(in crate::membership_log) fn identity(self) -> age::Identity {
        age::Identity::from_secret_bytes([self.0 ^ 0x55; 32])
    }
    pub(in crate::membership_log) fn id(self) -> MemberId {
        self.request().binding().actor()
    }
    pub(in crate::membership_log) fn request(self) -> Request {
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
    pub(in crate::membership_log) fn entrant(self) -> Entrant {
        Entrant {
            request: self.request(),
            node_key: [self.0; 32],
            name: format!("mac-{}", self.0),
            label: format!("m{}", self.0),
        }
    }
}

/// A log founded by `macs`, its genesis authored at 1 ms.
pub(in crate::membership_log) fn found(macs: &[Mac]) -> Log {
    let founders = macs.iter().map(|m| m.entrant()).collect();
    Log::new(Record::found(&FOUNDER, founders, 1).unwrap()).unwrap()
}

/// The seed whose key signs every genesis [`found`] makes.
pub(in crate::membership_log) const FOUNDER: [u8; 32] = [99; 32];

/// Hold `record` in `log`, and hand it back.
pub(in crate::membership_log) fn hold(log: &mut Log, record: Record) -> Record {
    assert_eq!(log.insert(record.clone()).unwrap(), Inserted::Held);
    record
}

/// Hold the record `$record` makes from `$log` in `$log`, and hand it back.
macro_rules! held {
    ($log:ident, $record:expr $(,)?) => {{
        let record = $record;
        hold(&mut $log, record)
    }};
}

/// `by`'s draft of `body` at `at`, following every head of `log`.
pub(in crate::membership_log) fn draft(log: &Log, by: Mac, at: u64, body: Body) -> Draft {
    log.draft(&by.id(), at, body)
}

/// `draft`, following `parents` (and `by`'s previous record) alone.
pub(in crate::membership_log) fn draft_after(
    log: &Log,
    by: Mac,
    at: u64,
    parents: &[RecordId],
    body: Body,
) -> Draft {
    let mut out = draft(log, by, at, body);
    out.parents = out
        .previous
        .into_iter()
        .chain(parents.iter().copied())
        .collect();
    out
}

/// `draft` signed as record version 1.
pub(in crate::membership_log) fn one(draft: Draft, by: Mac) -> Record {
    draft.sign(&by.seed(), by.incarnation()).unwrap()
}

/// `draft` signed as record version 2, stating `statement` (a `Put`'s or `Delete`'s).
pub(in crate::membership_log) fn two(
    draft: Draft,
    by: Mac,
    statement: Option<&Statement>,
) -> Record {
    draft
        .sign_two(&by.seed(), by.incarnation(), statement)
        .unwrap()
}

pub(in crate::membership_log) fn name(text: &str) -> Body {
    Body::Name {
        name: text.to_owned(),
    }
}

/// A statement superseding `ops`, carrying `carries`.
pub(in crate::membership_log) fn supersedes(
    ops: &[&Record],
    carries: Option<&Record>,
) -> Statement {
    let mut supersedes: Vec<RecordId> = ops.iter().map(|r| *r.id()).collect();
    supersedes.sort_unstable();
    Statement {
        supersedes,
        carries: carries.map(|r| *r.id()),
    }
}

/// A fresh epoch of `audience` by `by` at `at`, wrapped to each of `to`: its id and key.
pub(in crate::membership_log) fn epoch(
    log: &mut Log,
    by: Mac,
    at: u64,
    audience: EpochAudience,
    generation: u64,
    to: &[Mac],
) -> (RecordId, Zeroizing<[u8; 32]>) {
    let (draft, key) = log.draft_epoch(&by.id(), at, audience, generation).unwrap();
    let id = *hold(log, one(draft, by)).id();
    for mac in to {
        let wrap = log
            .draft_wrap(&by.id(), at, id, &key, mac.identity().recipient())
            .unwrap();
        hold(log, one(wrap, by));
    }
    (id, key)
}

/// A `Put` of opaque bytes under `epoch`: a frame is all the checkpoint reads of it.
pub(in crate::membership_log) fn opaque_put(
    audience: EpochAudience,
    epoch: RecordId,
    tag: u8,
) -> Body {
    Body::Put {
        audience,
        epoch,
        salt: [tag; 32],
        ciphertext: vec![tag; 64].into(),
    }
}

/// The checkpoint's day in these tests: day 100 of the clock.
pub(in crate::membership_log) const T: u64 = 100 * DAY_MS;
pub(in crate::membership_log) const HOUR: u64 = 60 * 60 * 1000;

/// What two proposers that hold the same records below the cut, other records above it and
/// different audience keys propose, one body each, as bytes.
struct TwoProposers {
    p: Log,
    q: Log,
    now: u64,
    /// The cut's one head.
    head: RecordId,
    /// The `Put` the prune drops.
    superseded: RecordId,
    /// Each proposer's audience epoch.
    epochs: (RecordId, RecordId),
}

fn two_proposers() -> TwoProposers {
    let mut base = found(&[A, B, C]);
    let t0 = T - 3 * DAY_MS;
    let m1 = EpochAudience::Hosts(vec!["m1".to_owned()]);
    let m2 = EpochAudience::Hosts(vec!["m2".to_owned()]);
    let (ea, ka) = epoch(&mut base, A, t0, m1, 1, &[A]);
    let (eb, kb) = epoch(&mut base, B, t0 + HOUR, m2.clone(), 1, &[B]);
    let entry = |audience: &str, name: &str, value: &[u8]| Entry {
        name: name.to_owned(),
        value: crate::SecretBytes::from(value),
        audience: Audience::Hosts(vec![audience.to_owned()]),
        if_absent: false,
        anchor: None,
    };
    let first = base
        .draft_put(
            &A.id(),
            t0 + 2 * HOUR,
            ea,
            &ka,
            &entry("m1", "a.key", b"one"),
        )
        .unwrap();
    let first = held!(base, one(first, A));
    let second = base
        .draft_put(
            &A.id(),
            t0 + 3 * HOUR,
            ea,
            &ka,
            &entry("m1", "a.key", b"two"),
        )
        .unwrap();
    held!(base, two(second, A, Some(&supersedes(&[&first], None))));
    let theirs = base
        .draft_put(&B.id(), t0 + 4 * HOUR, eb, &kb, &entry("m2", "b.key", b"x"))
        .unwrap();
    held!(base, two(theirs, B, Some(&Statement::default())));
    held!(base, one(draft(&base, B, t0 + 5 * HOUR, name("b")), B));
    let heads = base.heads();
    assert_eq!(heads.len(), 1, "the base is one line");
    let head = heads[0];
    // Both: A's record before the day that B's newest record before it has not seen.
    let unseen = one(draft_after(&base, A, T - HOUR, &[head], name("unseen")), A);
    let unseen = held!(base, unseen);
    // P: each seat's records after the day, B's following only the cut.
    let mut p = base.clone();
    held!(
        p,
        one(draft_after(&p, A, T + HOUR, &[*unseen.id()], name("p")), A)
    );
    held!(
        p,
        one(draft_after(&p, B, T + 2 * HOUR, &[head], name("p")), B)
    );
    // Q: other records after the day, B's following A's (and so `unseen`), and a `Put` B made
    // there.
    let mut q = base.clone();
    let aq = held!(q, one(draft_after(&q, A, T + HOUR, &[head], name("q")), A));
    held!(
        q,
        one(draft_after(&q, B, T + 3 * HOUR, &[*aq.id()], name("q")), B)
    );
    let later = q
        .draft_put(&B.id(), T + 4 * HOUR, eb, &kb, &entry("m2", "b.key", b"y"))
        .unwrap();
    held!(q, two(later, B, Some(&Statement::default())));
    TwoProposers {
        p,
        q,
        now: T + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR,
        head,
        superseded: *first.id(),
        epochs: (ea, eb),
    }
}

/// Each proposer's body, in its one encoding.
fn bodies(two: &TwoProposers) -> (Vec<u8>, Vec<u8>) {
    let p = propose(&two.p, two.now).expect("P proposes");
    let q = propose(&two.q, two.now).expect("Q proposes");
    (encode_body(&p.body), encode_body(&q.body))
}

/// Two Macs that hold different audience keys (A opens `[m1]`'s epoch and not `[m2]`'s, B the
/// other way) and different records above the cut (both hold a record of A's before the day that
/// B's newest record before it has not seen; P each seat's records after the day, B's following
/// only the cut; Q other records after it, B's following A's and a `Put` among them) propose
/// byte-identical bodies: the cut is what each recent seat's newest record before the day saw,
/// and the keep set, the state and the dropped digest read the cut's frames alone. The prune
/// drops A's first `Put` of `a.key`, which A's own version-2 `Put` superseded, and nothing else.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail:
/// - `Mutant::NewestOfAnyDate` (the cut is bounded by each seat's newest record of any date):
///   Q's B record after the day saw `unseen` and P's did not, so the heads part: ``assertion
///   `left == right` failed: two proposers write one body`` with P's `[.., 0, 0, 1, 156, 58, 251,
///   ..]` against Q's `[.., 0, 0, 1, 127, 96, 240, ..]`.
/// - `Mutant::WholeLog` (the keep set and the state read the proposer's whole log): the heads
///   agree and the state digests part, the same assertion with P's `[.., 107, 246, 189, 157, ..]`
///   against Q's `[.., 107, 34, 2, 190, ..]`.
/// - `Mutant::NoSeenBound` (the cut takes every record before its day): both cut alike, at
///   `unseen`, which B never saw: ``assertion `left == right` failed`` on `(predecessor, cut_ms,
///   heads)`, `left`'s head `[134, 142, 195, ..]` against the base's head `[14, 240, 188, ..]`.
#[test]
fn vault_checkpoint_two_proposers_write_one_body() {
    let two_logs = two_proposers();
    assert_eq!(cut_day(two_logs.now), Some(T));
    assert_eq!(cut_day(two_logs.now - HOUR - 1), Some(T - DAY_MS));
    let (ea, eb) = two_logs.epochs;
    let keys_a = two_logs.p.unwrap_epochs(&A.identity()).unwrap();
    let keys_b = two_logs.q.unwrap_epochs(&B.identity()).unwrap();
    assert!(keys_a.contains_key(&ea) && !keys_a.contains_key(&eb));
    assert!(keys_b.contains_key(&eb) && !keys_b.contains_key(&ea));

    let (p, q) = bodies(&two_logs);
    assert_eq!(p, q, "two proposers write one body");
    let plan = propose(&two_logs.p, two_logs.now).unwrap();
    let Body::Checkpoint {
        predecessor,
        cut_ms,
        heads,
        dropped,
        ..
    } = &plan.body
    else {
        panic!("a checkpoint");
    };
    assert_eq!(
        (predecessor, *cut_ms, heads.as_slice()),
        (&None, T, &[two_logs.head][..])
    );
    assert_eq!(plan.dropped, vec![two_logs.superseded]);
    assert_eq!(*dropped, dropped_digest(&plan.stubs));
    // The body recomputes at its heads, on either Mac.
    assert_eq!(
        encode_body(&plan_at(&two_logs.q, T, &[two_logs.head]).unwrap().body),
        p
    );

    for mutant in [Mutant::NewestOfAnyDate, Mutant::WholeLog] {
        MUTANT.set(Some(mutant));
        let (p, q) = bodies(&two_logs);
        MUTANT.set(None);
        assert_ne!(p, q, "{mutant:?} writes two bodies");
    }
    MUTANT.set(Some(Mutant::NoSeenBound));
    let unbounded = propose(&two_logs.q, two_logs.now).unwrap();
    MUTANT.set(None);
    assert_ne!(
        encode_body(&unbounded.body),
        encode_body(&plan.body),
        "NoSeenBound cuts past what B saw"
    );
}

/// A clock that makes `T` the proposer's day.
pub(in crate::membership_log) const NOW: u64 = T + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;

/// `log`'s proposal at [`NOW`], in its one encoding.
fn proposed(log: &Log) -> Vec<u8> {
    encode_body(&propose(log, NOW).expect("a proposal").body)
}

/// `body`'s heads.
fn heads_of(body: &Body) -> Vec<RecordId> {
    cut_of(body).map(|(_, heads)| heads.to_vec()).unwrap()
}

/// A seat that forked (A's two records at sequence 2, one following its own first record alone
/// and one following B's too) bounds the cut by what both its newest records saw, so two Macs
/// that took the branches in either order propose one body, and the cut holds neither branch.
///
/// NEGATIVE CONTROL: `Mutant::OneNewest` (one newest record of each seat, the last of its highest
/// sequence in the order the log took them), run from the test's first line and seen to fail
/// with ``assertion `left == right` failed: either order cuts alike``, P's body `[.., 0, 0, 1,
/// 147, 39, 0, ..]` against Q's `[.., 0, 0, 1, 125, 245, 43, ..]`.
#[test]
fn vault_checkpoint_a_forked_seat_cuts_alike_in_either_order() {
    let mut base = found(&[A, B]);
    let a1 = held!(base, one(draft(&base, A, T - 10 * DAY_MS, name("a1")), A));
    let b1 = held!(base, one(draft(&base, B, T - 9 * DAY_MS, name("b1")), B));
    let x = one(draft_after(&base, A, T - 8 * DAY_MS, &[], name("x")), A);
    let y = one(
        draft_after(&base, A, T - 7 * DAY_MS, &[*b1.id()], name("y")),
        A,
    );
    assert_eq!((x.sequence(), y.sequence()), (2, 2), "a fork");
    let mut p = base.clone();
    hold(&mut p, x.clone());
    hold(&mut p, y.clone());
    let mut q = base.clone();
    hold(&mut q, y);
    hold(&mut q, x);
    assert_eq!(proposed(&p), proposed(&q), "either order cuts alike");
    let plan = propose(&p, NOW).unwrap();
    assert_eq!(heads_of(&plan.body), vec![*a1.id()]);

    MUTANT.set(Some(Mutant::OneNewest));
    let (p, q) = (proposed(&p), proposed(&q));
    MUTANT.set(None);
    assert_ne!(p, q, "OneNewest cuts by the order the log took a fork");
}

/// A record signed after the day moves no cut: P holds A's record after the day, following C's,
/// and Q does not; both bound the cut by A's newest record before the day, which saw neither B's
/// nor C's, and propose one body.
///
/// NEGATIVE CONTROL: `Mutant::NewestOfAnyDate` (each seat's newest record of any date bounds the
/// cut), run from the test's first line and seen to fail with ``assertion `left == right` failed:
/// a record after the day moves no cut``, P's body `[.., 0, 0, 1, 92, 92, 164, ..]` against Q's
/// `[.., 0, 0, 1, 209, 59, 31, ..]`.
#[test]
fn vault_checkpoint_a_record_after_the_day_moves_no_cut() {
    let mut q = found(&[A, B, C]);
    let a1 = held!(q, one(draft(&q, A, T - 10 * DAY_MS, name("a1")), A));
    let c1 = held!(q, one(draft(&q, C, T - 9 * DAY_MS, name("c1")), C));
    held!(q, one(draft(&q, B, T - 8 * DAY_MS, name("b1")), B));
    let mut p = q.clone();
    held!(
        p,
        one(draft_after(&p, A, T + HOUR, &[*c1.id()], name("a2")), A)
    );
    assert_eq!(
        proposed(&p),
        proposed(&q),
        "a record after the day moves no cut"
    );
    assert_eq!(heads_of(&propose(&q, NOW).unwrap().body), vec![*a1.id()]);

    MUTANT.set(Some(Mutant::NewestOfAnyDate));
    let (p, q) = (proposed(&p), proposed(&q));
    MUTANT.set(None);
    assert_ne!(p, q, "NewestOfAnyDate cuts by a record after the day");
}

/// A record signed with a clock a year fast holds back no cut: B's, C's and A's later records,
/// signed before the day, follow it, so it is placed before the day with them, and the cut is
/// what every recent seat's newest record saw, it among them.
///
/// NEGATIVE CONTROL: `Mutant::ClosedBeforeDay` (the records before the day are only those
/// authored before it whose parents are too), run from the test's first line and seen to fail
/// with `a proposal`: nothing follows the genesis before the day, no seat is recent, and nothing
/// is proposed.
#[test]
fn vault_checkpoint_a_record_signed_a_year_ahead_holds_back_no_cut() {
    let mut log = found(&[A, B, C]);
    let ahead = held!(log, one(draft(&log, A, T + 365 * DAY_MS, name("ahead")), A));
    let b1 = held!(log, one(draft(&log, B, T - 5 * DAY_MS, name("b1")), B));
    held!(log, one(draft(&log, C, T - 4 * DAY_MS, name("c1")), C));
    held!(log, one(draft(&log, A, T - 3 * DAY_MS, name("a2")), A));
    let plan = propose(&log, NOW).expect("a proposal");
    assert_eq!(heads_of(&plan.body), vec![*b1.id()]);
    assert!(plan.keep.contains(ahead.id()));

    MUTANT.set(Some(Mutant::ClosedBeforeDay));
    let held_back = propose(&log, NOW);
    MUTANT.set(None);
    assert!(held_back.is_none(), "ClosedBeforeDay holds every cut back");
}

/// Who names A's op of `[m2]` and B's (`a.one`'s and `b.two`'s, say) in a version-2 statement.
#[derive(Clone, Copy, Debug)]
enum Namer {
    /// A, in a `Put` of `[everyone]`.
    OtherAudience,
    /// A, in a `Put` of `[m2]` it holds no key of.
    NoKey,
    /// A, holding `[m2]`'s key, in a `Put` concurrent with the ops it names (a fork of its own).
    OutsideItsPast,
    /// A, holding `[m2]`'s key, in a `Put` that follows them.
    Holder,
}

/// Whether a prune drops A's op of `[m2]` once `namer` names it and B's; B's op stays whoever
/// names it, since only B's own newer op supersedes it.
fn drops_by(namer: Namer) -> bool {
    let m2 = EpochAudience::Hosts(vec!["m2".to_owned()]);
    let everyone = EpochAudience::Everyone;
    let mut log = found(&[A, B]);
    let t = T - 5 * DAY_MS;
    let holds = matches!(namer, Namer::OutsideItsPast | Namer::Holder);
    let to: &[Mac] = if holds { &[B, A] } else { &[B] };
    let (e2, _) = epoch(&mut log, B, t, m2.clone(), 1, to);
    let (ea, _) = epoch(&mut log, A, t + 1, everyone.clone(), 1, &[A, B]);
    let before = log.clone();
    let a1 = held!(
        log,
        one(draft(&log, A, t + 2, opaque_put(m2.clone(), e2, 1)), A)
    );
    let b2 = held!(
        log,
        one(draft(&log, B, t + 3, opaque_put(m2.clone(), e2, 2)), B)
    );
    let body = match namer {
        Namer::OtherAudience => opaque_put(everyone, ea, 3),
        _ => opaque_put(m2, e2, 3),
    };
    let from = if matches!(namer, Namer::OutsideItsPast) {
        &before
    } else {
        &log
    };
    let named = two(
        draft(from, A, t + 4, body),
        A,
        Some(&supersedes(&[&a1, &b2], None)),
    );
    hold(&mut log, named);
    let plan = plan_at(&log, T, &log.heads()).unwrap();
    assert!(plan.keep.contains(b2.id()), "B's op stays");
    plan.dropped.contains(a1.id())
}

/// A name counts only where the frames show it could be true. A's statement that its own op of
/// `[m2]` stands replaced drops it only where A writes in `[m2]`, holds its key by a wrap in its
/// past, and follows the op; not from `[everyone]`, not from a `[m2]` frame A holds no key of,
/// and not from an op that never saw it. B's op it names stays in every case.
///
/// NEGATIVE CONTROL: `Mutant::AnyName` (every name counts), run from the test's first line and
/// seen to fail with `assertion failed: !drops_by(Namer::OtherAudience)`: A, in `[everyone]`,
/// drops an op of a `[m2]` entry it cannot open.
#[test]
fn vault_checkpoint_a_name_counts_only_where_its_namer_could_write_it() {
    assert!(!drops_by(Namer::OtherAudience));
    assert!(!drops_by(Namer::NoKey));
    assert!(!drops_by(Namer::OutsideItsPast));
    assert!(drops_by(Namer::Holder));

    MUTANT.set(Some(Mutant::AnyName));
    let dropped = [Namer::OtherAudience, Namer::NoKey, Namer::OutsideItsPast].map(drops_by);
    MUTANT.set(None);
    assert_eq!(dropped, [true; 3], "AnyName counts every name");
}

/// A checkpoint within a cut is final only by [`judge`]'s rule: A, the cut's one recent seat,
/// shares a body whose cut is no whole day, or whose state is not the cut's, and a later body
/// names neither as its predecessor; the honest body it shares, it names.
///
/// NEGATIVE CONTROL: `Mutant::SharesAlone` (a checkpoint within a cut is final by its shares
/// alone), run from the test's first line and seen to fail with ``assertion `left == right`
/// failed``, `left` `Some([58, 62, 240, ..])`, the body whose cut is no whole day, and `right`
/// `None`.
#[test]
fn vault_checkpoint_a_body_judge_refuses_is_no_predecessor() {
    let mut log = found(&[A, B, C]);
    let a1 = held!(log, one(draft(&log, A, T - 3 * DAY_MS, name("a1")), A));
    let honest = plan_at(&log, T - 2 * DAY_MS, &[*a1.id()]).unwrap().body;
    let Body::Checkpoint {
        predecessor,
        heads,
        state,
        dropped,
        ..
    } = honest.clone()
    else {
        panic!("a checkpoint");
    };
    let forge = |cut_ms: u64, state: [u8; 32]| Body::Checkpoint {
        predecessor,
        cut_ms,
        heads: heads.clone(),
        state,
        dropped,
    };
    let off_day = forge(T - 2 * DAY_MS - HOUR, state);
    let off_state = forge(T - 2 * DAY_MS, [0x77; 32]);
    let predecessor = |body: &Body| {
        let mut log = log.clone();
        let share = two(draft(&log, A, T - DAY_MS, body.clone()), A, None);
        hold(&mut log, share);
        plan_at(&log, T, &log.heads()).unwrap().predecessor()
    };
    assert_eq!(predecessor(&honest), checkpoint_id(&honest));
    assert_eq!(predecessor(&off_day), None);
    assert_eq!(predecessor(&off_state), None);

    MUTANT.set(Some(Mutant::SharesAlone));
    let named = predecessor(&off_day);
    MUTANT.set(None);
    assert_eq!(
        named,
        checkpoint_id(&off_day),
        "SharesAlone follows a body judge refuses"
    );
}

/// A share signed past its signer's cutoff counts for nothing: C, recent at the cut, shares the
/// body before it learns that B removed it, so A's share is one of two recent seats', and the
/// body is final neither to [`judge`] nor within a later cut.
///
/// NEGATIVE CONTROL: `Mutant::AnyShare` (a share counts whether or not its record does), run from
/// the test's first line and seen to fail with ``assertion `left == right` failed``, `left`
/// counting C's share and the body final.
#[test]
fn vault_checkpoint_a_share_signed_past_its_cutoff_counts_for_nothing() {
    let mut log = found(&[A, B, C]);
    held!(log, one(draft(&log, A, T - 4 * DAY_MS, name("a1")), A));
    let c1 = held!(log, one(draft(&log, C, T - 3 * DAY_MS, name("c1")), C));
    let body = plan_at(&log, T - 2 * DAY_MS, &[*c1.id()]).unwrap().body;
    let c_share = two(draft(&log, C, T - DAY_MS, body.clone()), C, None);
    let removal = Body::Remove {
        member: C.id(),
        cutoff: *c1.id(),
    };
    held!(log, one(draft(&log, B, T - DAY_MS, removal), B));
    hold(&mut log, c_share);
    let a_share = two(draft(&log, A, T - DAY_MS + HOUR, body.clone()), A, None);
    hold(&mut log, a_share);
    let judged = judge(&log, &body).unwrap();
    assert_eq!(
        (&judged.recent, &judged.signed, judged.is_final),
        (&seats(&[A, C]), &seats(&[A]), false)
    );
    let later = plan_at(&log, T, &log.heads()).unwrap();
    assert_eq!(later.predecessor(), None);

    MUTANT.set(Some(Mutant::AnyShare));
    let (judged, later) = (
        judge(&log, &body).unwrap(),
        plan_at(&log, T, &log.heads()).unwrap(),
    );
    MUTANT.set(None);
    assert!(judged.is_final, "AnyShare counts a cut-off share");
    assert_eq!(later.predecessor(), checkpoint_id(&body));
}

/// Three cuts of one log: cut 1 (day `D`) heads `b1`; cut 2 (`D` + 1) heads `a2`, which follows
/// it; cut 3 (`D` + 2) heads `b3`, which follows cut 1 and not `a2`. A and B signed records
/// within 30 days before each cut; C signed none.
struct Cuts {
    base: Log,
    /// Each cut's body, as the base computes it.
    bodies: [Body; 3],
    /// When the seats sign their shares.
    shared_ms: u64,
}

pub(in crate::membership_log) const D: u64 = 100 * DAY_MS;

fn cuts() -> Cuts {
    let mut base = found(&[A, B, C]);
    let a1 = held!(base, one(draft(&base, A, D - 3 * HOUR, name("a1")), A));
    let b1 = held!(
        base,
        one(
            draft_after(&base, B, D - 2 * HOUR, &[*a1.id()], name("b1")),
            B
        ),
    );
    let a2 = held!(
        base,
        one(
            draft_after(&base, A, D + 2 * HOUR, &[*b1.id()], name("a2")),
            A
        ),
    );
    let b3 = held!(
        base,
        one(
            draft_after(&base, B, D + DAY_MS + 2 * HOUR, &[*b1.id()], name("b3")),
            B,
        ),
    );
    let at = |cut: u64, head: &Record| plan_at(&base, cut, &[*head.id()]).unwrap().body;
    let bodies = [at(D, &b1), at(D + DAY_MS, &a2), at(D + 2 * DAY_MS, &b3)];
    Cuts {
        base,
        bodies,
        shared_ms: D + 3 * DAY_MS,
    }
}

/// The day of the split's cuts: a month after `D`.
pub(in crate::membership_log) const SPLIT: u64 = D + 31 * DAY_MS;

/// The `VaultCheckpoint` model's cuts: cut 1 (day `D`) heads `b1`, A and B signing within 30
/// days before it; cuts 2 ([`SPLIT`]) and 3 (the day after) head `a2` and `b3`, each signed the
/// day before its cut and following `b1` and not each other. Cut 2's past holds no record of B's within 30 days
/// before it and cut 3's none of A's, so A alone is recent at cut 2 and B alone at cut 3: the
/// two sides of a split.
fn split_cuts() -> Cuts {
    let mut base = found(&[A, B, C]);
    let a1 = held!(base, one(draft(&base, A, D - 3 * HOUR, name("a1")), A));
    let b1 = held!(
        base,
        one(
            draft_after(&base, B, D - 2 * HOUR, &[*a1.id()], name("b1")),
            B
        ),
    );
    let a2 = held!(
        base,
        one(
            draft_after(&base, A, SPLIT - DAY_MS, &[*b1.id()], name("a2")),
            A
        ),
    );
    let b3 = held!(
        base,
        one(
            draft_after(&base, B, SPLIT + HOUR, &[*b1.id()], name("b3")),
            B,
        ),
    );
    let at = |cut: u64, head: &Record| plan_at(&base, cut, &[*head.id()]).unwrap().body;
    let bodies = [at(D, &b1), at(SPLIT, &a2), at(SPLIT + DAY_MS, &b3)];
    Cuts {
        base,
        bodies,
        shared_ms: SPLIT + DAY_MS + HOUR,
    }
}

/// `seat`'s share of `body`, signed at `at` and held in `log`.
pub(in crate::membership_log) fn share_at(log: &mut Log, seat: Mac, body: &Body, at: u64) {
    let record = two(draft(log, seat, at, body.clone()), seat, None);
    hold(log, record);
}

/// `seat`'s share of `body`, signed when [`cuts`]' seats share, held in `log`.
fn share(log: &mut Log, seat: Mac, body: &Body) {
    share_at(log, seat, body, D + 3 * DAY_MS);
}

pub(in crate::membership_log) fn seats(macs: &[Mac]) -> BTreeSet<MemberId> {
    macs.iter().map(|m| m.id()).collect()
}

/// A checkpoint is final with more than half of the recent seats' shares and no fewer: A's
/// share and C's (C signed nothing within 30 days before the cut) are one of two recent seats,
/// and only B's makes it final. A body this Mac computes otherwise at the same heads is final for
/// no count of shares.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail:
/// - `Mutant::HalfIsFinal` (half the recent seats suffice) finalizes A's and C's shares, one of
///   two recent seats: `assertion failed: !judged.is_final`.
/// - `Mutant::EverySeatRecent` (every seat at the cut counts) counts C among the recent seats:
///   ``assertion `left == right` failed`` with `left` naming three recent seats and `right` two;
///   past that check, A's and C's shares, two of three, finalize the body (the test's own
///   assertion below).
#[test]
fn vault_checkpoint_a_minority_is_never_final() {
    let cuts = cuts();
    let body = &cuts.bodies[1];
    let judged_with = |macs: &[Mac], body: &Body| {
        let mut log = cuts.base.clone();
        for mac in macs {
            share(&mut log, *mac, body);
        }
        judge(&log, body).unwrap()
    };
    let judged = judged_with(&[A, C], body);
    assert_eq!(
        (judged.recomputed, &judged.recent, &judged.signed),
        (true, &seats(&[A, B]), &seats(&[A, C]))
    );
    assert!(!judged.is_final);
    assert!(judged_with(&[A, B], body).is_final);
    assert!(!judged_with(&[A], body).is_final);
    let Body::Checkpoint {
        predecessor,
        cut_ms,
        heads,
        dropped,
        ..
    } = body.clone()
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
    let judged = judged_with(&[A, B, C], &forged);
    assert_eq!(
        (judged.recomputed, judged.is_final, &judged.signed),
        (false, false, &seats(&[A, B, C]))
    );

    for (mutant, macs) in [
        (Mutant::HalfIsFinal, &[A][..]),
        (Mutant::EverySeatRecent, &[A, C][..]),
    ] {
        MUTANT.set(Some(mutant));
        let judged = judged_with(macs, body);
        MUTANT.set(None);
        assert!(judged.is_final, "{mutant:?} finalizes a minority's body");
    }
}

/// A seat that shared cut 2 shares neither cut 3, which omits it, nor cut 1, which it contains
/// but which would shrink its shares; a seat that shared nothing shares either, and the one that
/// shared cut 2 shares a later cut that contains it. No seat shares a body it computes otherwise.
///
/// NEGATIVE CONTROL: `Mutant::ByDayAlone` (a seat shares any cut later by its day than each it
/// shared) lets A share cut 3 beside cut 2, run from the test's first line and seen to fail with
/// `assertion failed: !may_share(&log, &A.id(), &cuts.bodies[2]).unwrap()`.
#[test]
fn vault_checkpoint_a_seat_never_shares_heads_that_omit_ones_it_shared() {
    let cuts = cuts();
    let mut log = cuts.base.clone();
    share(&mut log, A, &cuts.bodies[1]);
    assert!(!may_share(&log, &A.id(), &cuts.bodies[2]).unwrap());
    assert!(!may_share(&log, &A.id(), &cuts.bodies[0]).unwrap());
    assert!(may_share(&log, &A.id(), &cuts.bodies[1]).unwrap());
    for body in &cuts.bodies {
        assert!(may_share(&log, &C.id(), body).unwrap());
    }
    // A later cut holding both sides: A shares it.
    let mut later = cuts.base.clone();
    let tip = cuts.base.heads();
    let joined = held!(
        later,
        one(
            draft_after(&later, C, D + 2 * DAY_MS + 3 * HOUR, &tip, name("both")),
            C,
        ),
    );
    let body = plan_at(&later, D + 3 * DAY_MS, &[*joined.id()])
        .unwrap()
        .body;
    share(&mut later, A, &cuts.bodies[1]);
    assert!(may_share(&later, &A.id(), &body).unwrap());
    let Body::Checkpoint {
        cut_ms,
        heads,
        state,
        dropped,
        ..
    } = cuts.bodies[2].clone()
    else {
        panic!("a checkpoint");
    };
    let forged = Body::Checkpoint {
        predecessor: Some([0x55; 32]),
        cut_ms,
        heads,
        state,
        dropped,
    };
    assert!(!may_share(&cuts.base, &C.id(), &forged).unwrap());

    MUTANT.set(Some(Mutant::ByDayAlone));
    let shares = may_share(&log, &A.id(), &cuts.bodies[2]).unwrap();
    MUTANT.set(None);
    assert!(shares, "ByDayAlone shares a cut that omits one shared");
}

/// What a prune at a cut keeps, read from frames: a signer's older ops its own newer op
/// superseded go, another signer's ops a statement names stay (only their own signer's newer op
/// supersedes them, whatever another names of their register), an op no version-2 op names
/// stays (a version-1 op among them), the value a kept op carries stays, every epoch stays with
/// its wraps (a record outside the cut may be sealed under any), and an op under an epoch a
/// seat is not yet wrapped to stays. Every weighed record, `Name` and `Node` stays.
///
/// NEGATIVE CONTROL: `Mutant::CrossSigner` (FL-148b's rule: a name supersedes another signer's
/// op where a newer op of that signer is among the names), run from the test's first line and
/// seen to fail with `B's older op stays: A's name of it supersedes nothing`: A's statement
/// drops B's older op.
#[test]
fn vault_checkpoint_keeps_what_the_frames_say_the_state_needs() {
    let everyone = EpochAudience::Everyone;
    let build = |wrap_c: bool| {
        let mut log = found(&[A, B, C]);
        let t = T - 5 * DAY_MS;
        let all: &[Mac] = if wrap_c { &[A, B, C] } else { &[A, B] };
        let (old, _) = epoch(&mut log, A, t, everyone.clone(), 1, &[A, B, C]);
        let p1 = held!(
            log,
            one(
                draft(&log, A, t + 1, opaque_put(everyone.clone(), old, 1)),
                A
            ),
        );
        let p2 = held!(
            log,
            one(
                draft(&log, B, t + 2, opaque_put(everyone.clone(), old, 2)),
                B
            ),
        );
        let p3 = held!(
            log,
            one(
                draft(&log, B, t + 3, opaque_put(everyone.clone(), old, 3)),
                B
            ),
        );
        let alone = held!(
            log,
            one(
                draft(&log, B, t + 4, opaque_put(everyone.clone(), old, 4)),
                B
            ),
        );
        let (new, _) = epoch(&mut log, A, t + 5, everyone.clone(), 2, all);
        let restate = two(
            draft(&log, A, t + 6, opaque_put(everyone.clone(), new, 5)),
            A,
            Some(&supersedes(&[&p1, &p2, &p3], Some(&p3))),
        );
        let restate = held!(log, restate);
        let q1 = held!(
            log,
            two(
                draft(&log, A, t + 7, opaque_put(everyone.clone(), new, 6)),
                A,
                Some(&Statement::default()),
            ),
        );
        let q2 = two(
            draft(&log, A, t + 8, opaque_put(everyone.clone(), new, 7)),
            A,
            Some(&supersedes(&[&q1], None)),
        );
        let q2 = held!(log, q2);
        let plan = plan_at(&log, T, &log.heads()).unwrap();
        (log, plan, [p1, p2, p3, alone, restate, q1, q2], old, new)
    };
    let (log, plan, [p1, p2, p3, alone, restate, q1, q2], old, new) = build(true);
    let kept = |r: &Record| plan.keep.contains(r.id());
    // A's own restate superseded its p1; B's p2 and p3 only B's own newer op supersedes, and
    // p3 is carried.
    assert!(!kept(&p1));
    assert!(
        kept(&p2),
        "B's older op stays: A's name of it supersedes nothing"
    );
    assert!(kept(&p3) && kept(&restate) && kept(&alone));
    assert!(!kept(&q1) && kept(&q2));
    assert!(plan.keep.contains(&new));
    // The first epoch: p3 is under it and kept, so it and its wraps stay.
    assert!(plan.keep.contains(&old));
    for record in log.records() {
        match record.kind() {
            Kind::Genesis | Kind::Admit | Kind::Remove | Kind::Name | Kind::Node => {
                assert!(kept(record));
            }
            Kind::Epoch | Kind::Wrap => assert!(kept(record), "every epoch and wrap"),
            _ => {}
        }
    }
    let expected: Vec<RecordId> = {
        let mut ids = vec![*p1.id(), *q1.id()];
        ids.sort_unstable();
        ids
    };
    assert_eq!(plan.dropped, expected);
    // C holds no wrap of the newest epoch: every op under it stays, q1 among them.
    let (_, owed, [.., q1, _], _, _) = build(false);
    assert!(owed.keep.contains(q1.id()));

    // A names both of B's ops of the register: both stay, B's older one too.
    let mut log = found(&[A, B]);
    let t = T - 5 * DAY_MS;
    let (e, _) = epoch(&mut log, A, t, everyone.clone(), 1, &[A, B]);
    let b_old = held!(
        log,
        one(draft(&log, B, t + 1, opaque_put(everyone.clone(), e, 1)), B),
    );
    let b_newest = held!(
        log,
        one(draft(&log, B, t + 2, opaque_put(everyone.clone(), e, 2)), B),
    );
    let a_write = two(
        draft(&log, A, t + 3, opaque_put(everyone.clone(), e, 3)),
        A,
        Some(&supersedes(&[&b_old, &b_newest], None)),
    );
    held!(log, a_write);
    let keep_of = |log: &Log| plan_at(log, T, &log.heads()).unwrap().keep;
    let plan_keep = keep_of(&log);
    assert!(plan_keep.contains(b_old.id()) && plan_keep.contains(b_newest.id()));
    MUTANT.set(Some(Mutant::CrossSigner));
    let mutated = keep_of(&log);
    MUTANT.set(None);
    assert!(
        !mutated.contains(b_old.id()) && mutated.contains(b_newest.id()),
        "CrossSigner drops B's older op by A's name of it"
    );
}

/// The seats and their order in the model.
const MODEL_SEATS: [(Mac, &str); 3] = [(A, "a"), (B, "b"), (C, "c")];

/// The model variable of `seat`'s share of cut `n` (1 to 3).
fn share_var(seat: &str, n: usize) -> &'static str {
    match (seat, n) {
        ("a", 1) => "a1",
        ("a", 2) => "a2",
        ("a", 3) => "a3",
        ("b", 1) => "b1",
        ("b", 2) => "b2",
        ("b", 3) => "b3",
        ("c", 1) => "c1",
        ("c", 2) => "c2",
        ("c", 3) => "c3",
        _ => panic!("no share {seat}{n}"),
    }
}

/// The model variable of the cut `seat` shared last.
fn last_var(seat: &str) -> &'static str {
    match seat {
        "a" => "la",
        "b" => "lb",
        "c" => "lc",
        _ => panic!("no seat {seat}"),
    }
}

/// The real log of a `VaultCheckpoint` state: the cuts' base and each share the state holds.
fn model_log(cuts: &Cuts, state: &State) -> Log {
    let mut log = cuts.base.clone();
    for (mac, seat) in MODEL_SEATS {
        for n in 1..=3 {
            if state[share_var(seat, n)] == 1 {
                share_at(&mut log, mac, &cuts.bodies[n - 1], cuts.shared_ms);
            }
        }
    }
    log
}

/// What a prune at a later cut that holds every share finds: the cut (1 to 3) of the newest
/// final checkpoint its body names as its predecessor (0 for none), and how many checkpoint
/// records it keeps.
fn kept_shares(cuts: &Cuts, log: &Log) -> (i64, i64) {
    let mut log = log.clone();
    let tip_ms = cuts.shared_ms + DAY_MS;
    let tip = held!(log, one(draft(&log, A, tip_ms, name("tip")), A));
    let plan = plan_at(&log, tip_ms / DAY_MS * DAY_MS + DAY_MS, &[*tip.id()]).unwrap();
    let newest = plan.predecessor().map_or(0, |id| {
        1 + cuts
            .bodies
            .iter()
            .position(|body| checkpoint_id(body) == Some(id))
            .expect("the predecessor is one of the cuts")
    });
    let kept = log
        .records()
        .filter(|r| r.kind() == Kind::Checkpoint && plan.keep.contains(r.id()))
        .count();
    (i64::try_from(newest).unwrap(), i64::try_from(kept).unwrap())
}

/// The `VaultCheckpoint` state the real code reaches by `action` from `state`, where the code
/// takes it: `ShareXn` where [`may_share`] lets seat X share cut `n`, `Judge` as [`judge`] finds
/// each body, `Prune` as [`plan_at`] keeps the share records at a later cut. `None` where
/// [`may_share`] refuses. The anchors' projection.
fn model_transition(
    cuts: &Cuts,
    state: &State,
    action: &'static str,
    ev: &mut StepEvidence,
) -> Option<State> {
    let log = model_log(cuts, state);
    let mut next = state.clone();
    if let Some(share) = action.strip_prefix("Share") {
        let seat = &share[..1].to_lowercase();
        let n: usize = share[1..].parse().unwrap();
        let (mac, _) = MODEL_SEATS.iter().find(|(_, s)| s == seat).unwrap();
        let body = &cuts.bodies[n - 1];
        if !ev.step(action, || may_share(&log, &mac.id(), body).unwrap()) {
            return None;
        }
        next.insert(share_var(seat, n), 1);
        next.insert(last_var(seat), i64::try_from(n).unwrap());
        return Some(next);
    }
    match action {
        "Judge" => {
            next.insert("judged", 1);
            for (n, var) in ["f1", "f2", "f3"].into_iter().enumerate() {
                let body = &cuts.bodies[n];
                let judged = ev.step(action, || judge(&log, body).unwrap());
                next.insert(var, i64::from(judged.is_final));
            }
        }
        "Prune" => {
            next.insert("judged", 2);
            let (newest, kept) = ev.step(action, || kept_shares(cuts, &log));
            next.insert("nw", newest);
            next.insert("kept", kept);
        }
        other => panic!("not a VaultCheckpoint action: {other}"),
    }
    Some(next)
}

/// Every move the model reaches against the real code: from each reachable state, each share
/// [`may_share`] allows is one the model allows and none other (and leaves the state the model
/// gives), and each `Judge` and `Prune` reads what [`judge`] and [`plan_at`] find. `Err` names the
/// first move where they part.
fn conforms(
    model: &aterm_spec::derive::Model,
    cuts: &Cuts,
) -> std::result::Result<StepEvidence, String> {
    let mut ev = StepEvidence::new("VaultCheckpoint");
    for state in aterm_spec::interp::reachable_states(model) {
        for action in &model.actions {
            let enabled = model.action_enabled(action.name, &state);
            let share = action.name.strip_prefix("Share");
            let unshared = share.is_some_and(|s| {
                let seat = &s[..1].to_lowercase();
                state[share_var(seat, s[1..].parse().unwrap())] == 0
            });
            if share.is_some() && (state["judged"] != 0 || !unshared) {
                if enabled {
                    return Err(format!("{} enabled from {state:?}", action.name));
                }
                continue;
            }
            if share.is_none() && !enabled {
                continue;
            }
            let real = model_transition(cuts, &state, action.name, &mut ev);
            let allowed = model.successors(action.name, &state);
            match real {
                None if enabled => {
                    return Err(format!(
                        "{} refused by the code from {state:?}",
                        action.name
                    ));
                }
                None => {}
                Some(next) if !allowed.contains(&next) => {
                    return Err(format!(
                        "{} from {state:?}: the code reaches {next:?}, the model {allowed:?}",
                        action.name
                    ));
                }
                Some(_) => {}
            }
        }
    }
    Ok(ev)
}

/// `VaultCheckpoint` holds its laws, each mutant breaks the law it stands for (1 and 4
/// `SharesNeverShrink`, 1 `ChainAndSharesBounded`, 2 and 3 `FinalOnlyWithMajority`, 5
/// `NewestNestsEveryFinal` and `PruneKeepsWhatTheNewestOmits`), and the real checkpoint refines it: from every state the
/// model reaches, over three real cuts ([`split_cuts`]: cut 1, and cuts 2 and 3 each containing it
/// and not each other, each with its own recent seats) and the real shares of the state,
/// [`may_share`] allows exactly the shares the model allows, [`judge`] finds final exactly the
/// bodies the model does, and a prune at a later cut names the newest final checkpoint and keeps
/// the share records the model does. Each step runs inside its evidence window, and each anchored
/// function is entered by its own action's windows alone.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail with `called
/// Result::unwrap() on an Err value`, which the test asserts:
/// - `Mutant::ByDayAlone`: the code lets a seat that shared cut 2 share cut 3, which the model
///   refuses: `"ShareA3 from {.. \"a2\": 1, \"a3\": 0, .. \"la\": 2 ..}: the code reaches
///   {.. \"a2\": 1, \"a3\": 1, .. \"la\": 3 ..}, the model []"`.
/// - `Mutant::NewestByDay`: with both sides' cuts final, the code names one of them the newest
///   and drops the other's share, where the model names none and keeps both: `"Prune from {..
///   \"f2\": 1, \"f3\": 1, \"judged\": 1 ..}: the code reaches {.. \"kept\": 1 ..
///   \"nw\": 3}, the model [{.. \"kept\": 2 .. \"nw\": 0}]"`.
#[test]
fn vault_checkpoint_conforms_to_its_model() {
    use aterm_spec::interp::{violated_invariants, with_buggy};
    let model = aterm_models_swarm::vault_checkpoint_model();
    assert!(violated_invariants(&model).is_empty());
    aterm_spec::interp::prove_and_catch(&model);
    for (buggy, law) in [
        (1, "SharesNeverShrink"),
        (1, "ChainAndSharesBounded"),
        (2, "FinalOnlyWithMajority"),
        (3, "FinalOnlyWithMajority"),
        (4, "SharesNeverShrink"),
        (5, "NewestNestsEveryFinal"),
        (5, "PruneKeepsWhatTheNewestOmits"),
    ] {
        assert!(
            violated_invariants(&with_buggy(&model, buggy)).contains(law),
            "mutant {buggy} breaks {law}"
        );
    }
    let cuts = split_cuts();
    let ev = conforms(&model, &cuts).unwrap();
    let audit = ev.audit();
    assert!(audit.is_complete(), "{}", audit.summary());

    for (mutant, action) in [
        (Mutant::ByDayAlone, "ShareA3 from"),
        (Mutant::NewestByDay, "Prune from"),
    ] {
        MUTANT.set(Some(mutant));
        let mutated = conforms(&model, &cuts);
        MUTANT.set(None);
        let Err(why) = mutated else {
            panic!("{mutant:?} conforms");
        };
        assert!(why.starts_with(action), "{mutant:?}: {why}");
    }
}

/// The two sides of a split each finalize their own cut, A alone recent at cut 2 and B alone at
/// cut 3; neither contains the other, so a later body names no predecessor and its prune keeps
/// both sides' shares. With cut 1 final too and only one side's cut, that cut is the newest and
/// cut 1's shares go.
///
/// NEGATIVE CONTROL: `Mutant::NewestByDay` (the newest final checkpoint is the latest by its
/// day, nested or not), run from the test's first line and seen to fail with ``assertion
/// `left == right` failed``, `left: (3, 1)` (cut 3 the newest, cut 2's share dropped) against
/// `right: (0, 2)`.
#[test]
fn vault_checkpoint_two_sides_of_a_split_name_no_newest() {
    let cuts = split_cuts();
    let with = |shares: &[(Mac, usize)]| {
        let mut log = cuts.base.clone();
        for &(mac, n) in shares {
            share_at(&mut log, mac, &cuts.bodies[n - 1], cuts.shared_ms);
        }
        kept_shares(&cuts, &log)
    };
    let split = [(A, 2), (B, 3)];
    for (n, mac) in [(1, A), (2, B)] {
        let judged = judge(&cuts.base, &cuts.bodies[n]).unwrap();
        assert_eq!(judged.recent, seats(&[mac]));
    }
    assert_eq!(with(&split), (0, 2));
    assert_eq!(with(&[(A, 1), (B, 1), (A, 2)]), (2, 1));

    MUTANT.set(Some(Mutant::NewestByDay));
    let named = with(&split);
    MUTANT.set(None);
    assert_ne!(named.0, 0, "NewestByDay names a side");
}

/// A cut is a whole day, and its heads are the newest records of their past, each authored
/// before it: a cut a millisecond past a day, a cut before a head's own time, and heads one of
/// which follows another are refused.
#[test]
fn vault_checkpoint_a_cut_is_a_whole_day_before_its_records() {
    let cuts = cuts();
    let heads = cuts.base.heads();
    assert!(plan_at(&cuts.base, D + 2 * DAY_MS + 1, &heads).is_err());
    assert!(
        plan_at(&cuts.base, D + DAY_MS, &heads).is_err(),
        "b3 is later"
    );
    assert!(plan_at(&cuts.base, D + 2 * DAY_MS, &heads).is_ok());
    let tops: Vec<RecordId> = cuts.base.records().map(|r| *r.id()).collect();
    assert!(
        plan_at(&cuts.base, D + 2 * DAY_MS, &tops).is_err(),
        "heads are the newest"
    );
}
