// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fault-injection and safety tests for AOS publication and checkpoint restore.

use aterm_aos::checkpoint::{Checkpoint, Provenance, RestoreRefusal};
use aterm_aos::log::IdentityMismatch;
use aterm_aos::publish::{Admit, Head, PublishRefusal};
use aterm_aos::task::Code;
use aterm_aos::wire::WireFault;
use aterm_aos::{ArtifactId, ArtifactKind, BranchId, Digest, Revision, TaskIdentity};

use crate::toy::{
    ToyBoundary, ToyInput, ToySchema, ToyState, Variant, attached, boundary_over, code, feed,
    identity,
};

use ToyInput::{Add, Reset, Tick};

#[derive(Clone, Debug, PartialEq, Eq)]
struct PairView {
    code: ArtifactId,
    identity: TaskIdentity,
    head: Head,
    state: ToyState,
    ancestor: Digest,
    log_epoch: u64,
}

fn view(b: &mut ToyBoundary) -> PairView {
    let head = b.head().clone();
    PairView {
        code: b.pair().code.id().clone(),
        identity: b.identity().clone(),
        head,
        state: b.pair().state.clone(),
        ancestor: b.checkpoint().unwrap().digest(),
        log_epoch: b.log().unwrap().header().log_epoch,
    }
}

#[cfg(debug_assertions)]
#[derive(Debug, PartialEq, Eq)]
enum Installed {
    Old,
    New,
    Mixed,
}

#[cfg(debug_assertions)]
fn classify(v: &PairView, old: &PairView, new: &PairView) -> Installed {
    if v == old {
        Installed::Old
    } else if v == new {
        Installed::New
    } else {
        Installed::Mixed
    }
}

fn paused_base() -> (ToyBoundary, Checkpoint) {
    let mut b = attached(Variant::Stranded, 4);
    let c0 = b.checkpoint().unwrap().clone();
    feed(&mut b, &[Add(3), Tick, Add(4), Tick]);
    b.pause().unwrap();
    assert_eq!(b.admit(&Add(5)), Admit::Queued);
    assert_eq!(b.admit(&Add(6)), Admit::Queued);
    (b, c0)
}

#[cfg(debug_assertions)]
fn new_view(base: &ToyBoundary, c0: &Checkpoint) -> PairView {
    use aterm_aos::task::state_digest;

    let schema = ToySchema::new();
    let probe = base.clone().prepare(code(Variant::Good)).unwrap();
    let state = probe.state().clone();
    let fresh = Checkpoint::capture(
        &schema,
        &state,
        Provenance {
            identity: identity(Variant::Good),
            log_epoch: 2,
            input_cursor: 4,
            effect_cursor: 0,
            branch: BranchId(0),
            revision: Revision(1),
            parent: Some(c0.digest()),
        },
    );
    let head = Head {
        identity: identity(Variant::Good).digest(),
        revision: Revision(1),
        branch: BranchId(0),
        state_digest: state_digest(&schema, &state),
        input_cursor: 4,
        effect_cursor: 0,
    };
    assert_eq!(head.state_digest, probe.witness().candidate_state_digest());
    PairView {
        code: code(Variant::Good).id().clone(),
        identity: identity(Variant::Good),
        head,
        state,
        ancestor: fresh.digest(),
        log_epoch: 2,
    }
}

#[cfg(debug_assertions)]
#[test]
fn a_commit_stopped_after_any_number_of_stores_leaves_the_complete_old_pair_or_the_complete_new_one()
 {
    let (mut base, c0) = paused_base();
    let old = view(&mut base);
    let new = new_view(&base, &c0);
    assert_ne!(old.state, new.state);

    let mut seen = Vec::new();
    for k in 0..=6 {
        let mut b = base.clone();
        let p = b.prepare(code(Variant::Good)).unwrap();
        match b.commit_stopping_after(p, k) {
            None => {
                let at = classify(&view(&mut b), &old, &new);
                assert_ne!(at, Installed::Mixed, "k = {k}");
                assert!(b.is_paused(), "k = {k}");
                assert_eq!(b.queued(), 2, "k = {k}");
                assert_eq!(b.reject().len(), 2, "k = {k}");
                let log = b.log().unwrap().inputs().to_vec();
                assert_eq!(&log[log.len() - 2..], &[Add(5), Add(6)][..], "k = {k}");
                let expect_len = if at == Installed::Old { 6 } else { 2 };
                assert_eq!(log.len(), expect_len, "k = {k}");
                seen.push(at);
            }
            Some(done) => {
                let commit = done.unwrap();
                assert_eq!(commit.revision, Revision(1), "k = {k}");
                assert_eq!(commit.delivered.len(), 2, "k = {k}");
                assert!(!b.is_paused(), "k = {k}");
                assert_eq!(b.checkpoint().unwrap().digest(), new.ancestor, "k = {k}");
                assert_eq!(b.pair().code.id(), &new.code, "k = {k}");
            }
        }
    }
    assert_eq!(
        seen,
        vec![
            Installed::Old,
            Installed::Old,
            Installed::New,
            Installed::New
        ]
    );
}

#[cfg(debug_assertions)]
#[test]
fn negative_control_new_state_under_old_code_classifies_as_mixed_and_cannot_commit() {
    let (mut base, c0) = paused_base();
    let old = view(&mut base);
    let new = new_view(&base, &c0);
    let p = base.prepare(code(Variant::Good)).unwrap();

    let mut b = base.clone();
    *b.pair_mut().1 = p.state().clone();
    let v = view(&mut b);
    assert_eq!(classify(&v, &old, &new), Installed::Mixed);
    assert_eq!(
        b.commit(p).err(),
        Some(PublishRefusal::StaleHead {
            expected: Box::new(old.head.clone()),
            live: Box::new(v.head)
        })
    );
}

#[cfg(debug_assertions)]
#[test]
fn a_refused_commit_is_refused_the_same_under_fault_injection_at_every_k() {
    let (base, _) = paused_base();
    for k in 0..=6 {
        let mut b = base.clone();
        let p = b.prepare(code(Variant::Good)).unwrap();
        b.reject();
        live_add(&mut b, 1);
        let h = b.pause().unwrap();
        let got = b.commit_stopping_after(p, k);
        assert!(
            matches!(got, Some(Err(PublishRefusal::StaleHead { ref live, .. })) if **live == h),
            "k = {k}: {got:?}"
        );
        assert_eq!(b.pair().code.id(), code(Variant::Stranded).id());
        assert_eq!(b.stats().publications, 0);
    }
}

#[cfg(debug_assertions)]
#[test]
fn an_unstopped_fault_injected_commit_equals_the_production_commit() {
    let (base, _) = paused_base();
    let mut a = base.clone();
    let mut b = base.clone();
    let pa = a.prepare(code(Variant::Good)).unwrap();
    let pb = b.prepare(code(Variant::Good)).unwrap();
    let ca = a.commit(pa).unwrap();
    let cb = b.commit_stopping_after(pb, usize::MAX).unwrap().unwrap();
    assert_eq!(ca.revision, cb.revision);
    assert_eq!(ca.witness, cb.witness);
    assert_eq!(ca.delivered, cb.delivered);
    assert_eq!(view(&mut a), view(&mut b));
    assert_eq!(a.stats(), b.stats());
}

fn live_add(b: &mut ToyBoundary, n: i64) {
    feed(b, &[Add(n)]);
}

fn keep(b: &ToyBoundary, chain: &mut Vec<(Checkpoint, Vec<u8>)>) {
    let c = b.checkpoint().unwrap().clone();
    let bytes = c.encode().unwrap();
    chain.push((c, bytes));
}

#[test]
fn a_restore_mints_a_new_branch_and_revision_so_a_candidate_from_before_it_is_stale_over_equal_state()
 {
    let mut b = attached(Variant::Stranded, 4);
    feed(&mut b, &[Add(1), Tick]);
    b.attach(2, 64).unwrap();
    let c1 = b.checkpoint().unwrap().clone();
    let h0 = b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    b.reject();
    let restored = b.restore(&c1).unwrap();
    assert_eq!(restored.state_digest, h0.state_digest);
    assert_eq!(restored.input_cursor, h0.input_cursor);
    assert_eq!(restored.identity, h0.identity);
    assert_eq!(
        (restored.revision, restored.branch),
        (Revision(1), BranchId(1))
    );
    let h1 = b.pause().unwrap();
    assert_eq!(h1, restored);
    assert_eq!(
        b.commit(p).err(),
        Some(PublishRefusal::StaleHead {
            expected: Box::new(h0),
            live: Box::new(h1)
        })
    );
    assert_eq!((b.stats().restores, b.stats().publications), (1, 0));
}

#[test]
fn restore_refuses_while_paused_or_mid_step_and_leaves_the_boundary_as_it_was() {
    let mut b = attached(Variant::Good, 4);
    let c0 = b.checkpoint().unwrap().clone();
    feed(&mut b, &[Add(1)]);
    let h = b.pause().unwrap();
    assert_eq!(b.restore(&c0).err(), Some(PublishRefusal::AlreadyPaused));
    assert_eq!(b.paused_head(), Some(&h));
    b.reject();
    assert_eq!(b.admit(&Add(2)), Admit::Run);
    assert_eq!(b.restore(&c0).err(), Some(PublishRefusal::MidStep));
    assert_eq!(b.head(), &h);
    b.withdraw();
    assert_eq!(b.stats().restores, 0);
    let r = b.restore(&c0).unwrap();
    assert_eq!(b.pair().state, ToyState::default());
    assert_eq!(
        (r.input_cursor, r.branch, r.revision),
        (0, BranchId(1), Revision(1))
    );
}

#[test]
fn a_checkpoint_of_another_identity_does_not_restore_even_one_from_before_a_repair() {
    let mut b = attached(Variant::Stranded, 4);
    let c0 = b.checkpoint().unwrap().clone();
    feed(&mut b, &[Add(1), Tick]);
    b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    b.commit(p).unwrap();
    let head = b.head().clone();

    assert_eq!(
        b.restore(&c0).err(),
        Some(PublishRefusal::Restore(RestoreRefusal::Identity(
            IdentityMismatch::Code
        )))
    );
    let foreign = Checkpoint::capture(
        b.schema(),
        &ToyState::default(),
        Provenance {
            identity: TaskIdentity {
                task: ArtifactId::new(ArtifactKind::Task, "another-task", 1, b""),
                ..identity(Variant::Good)
            },
            ..c0.provenance().clone()
        },
    );
    assert_eq!(
        b.restore(&foreign).err(),
        Some(PublishRefusal::Restore(RestoreRefusal::Identity(
            IdentityMismatch::Task
        )))
    );
    let unreadable = Checkpoint::capture(
        &ToySchema::lossy(),
        &ToyState::default(),
        Provenance {
            identity: identity(Variant::Good),
            ..c0.provenance().clone()
        },
    );
    assert_eq!(
        b.restore(&unreadable).err(),
        Some(PublishRefusal::Restore(RestoreRefusal::Wire(
            WireFault::Truncated {
                field: "toy.seen",
                need: 8,
                have: 7
            }
        )))
    );
    assert_eq!(b.head(), &head);
    assert_eq!(b.stats().restores, 0);
    let c1 = b.checkpoint().unwrap().clone();
    assert!(b.restore(&c1).is_ok());
}

#[test]
fn revisions_stay_distinct_across_commits_and_restores() {
    let mut b = attached(Variant::Stranded, 4);
    let mut heads = vec![b.head().clone()];

    feed(&mut b, &[Add(1), Tick]);
    b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    b.commit(p).unwrap();
    heads.push(b.head().clone());
    let c1 = b.checkpoint().unwrap().clone();

    feed(&mut b, &[Add(2)]);
    heads.push(b.restore(&c1).unwrap());

    feed(&mut b, &[Add(3), Tick]);
    b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    b.commit(p).unwrap();
    heads.push(b.head().clone());
    let c3 = b.checkpoint().unwrap().clone();
    heads.push(b.restore(&c3).unwrap());

    let revisions: Vec<u64> = heads.iter().map(|h| h.revision.0).collect();
    assert_eq!(revisions, vec![0, 1, 2, 3, 4]);
    let branches: Vec<u64> = heads.iter().map(|h| h.branch.0).collect();
    assert_eq!(branches, vec![0, 0, 1, 1, 2]);
    assert_eq!(heads[4].state_digest, heads[3].state_digest);
    for (i, a) in heads.iter().enumerate() {
        for x in &heads[i + 1..] {
            assert_ne!(a, x);
        }
    }
}

#[test]
fn checkpoints_are_never_rewritten_and_each_new_ancestor_names_the_one_before() {
    let mut b = attached(Variant::Stranded, 4);
    let schema = ToySchema::new();
    let mut chain: Vec<(Checkpoint, Vec<u8>)> = Vec::new();
    keep(&b, &mut chain);

    feed(&mut b, &[Add(1), Tick]);
    assert_eq!(b.checkpoint(), Some(&chain[0].0));

    b.attach(5, 64).unwrap();
    keep(&b, &mut chain);
    feed(&mut b, &[Add(2), Tick]);
    b.pause().unwrap();
    let p = b.prepare(code(Variant::Good)).unwrap();
    b.commit(p).unwrap();
    keep(&b, &mut chain);
    feed(&mut b, &[Add(3)]);
    b.attach(8, 64).unwrap();
    keep(&b, &mut chain);
    let own = chain[2].0.clone();
    b.restore(&own).unwrap();
    keep(&b, &mut chain);

    let parents: Vec<Option<Digest>> = chain.iter().map(|(c, _)| c.provenance().parent).collect();
    assert_eq!(
        parents,
        vec![
            None,
            Some(chain[0].0.digest()),
            Some(chain[1].0.digest()),
            Some(chain[2].0.digest()),
            Some(own.digest()),
        ]
    );
    let epochs: Vec<u64> = chain
        .iter()
        .map(|(c, _)| c.provenance().log_epoch)
        .collect();
    assert_eq!(epochs, vec![1, 5, 6, 8, 9]);
    for (c, bytes) in &chain {
        assert_eq!(&c.encode().unwrap(), bytes);
        assert_eq!(&Checkpoint::decode(bytes).unwrap(), c);
        assert!(c.restore(&schema, &c.provenance().identity).is_ok());
    }
}

#[test]
fn a_witness_from_another_boundary_with_an_equal_head_is_refused_by_its_trace() {
    let mut a = attached(Variant::Stranded, 4);
    let mut b = attached(Variant::Stranded, 4);
    feed(&mut a, &[Add(5), Reset]);
    feed(&mut b, &[Add(7), Reset]);
    let ha = a.pause().unwrap();
    let hb = b.pause().unwrap();
    assert_eq!(ha, hb, "the reset leaves both heads equal");
    assert_eq!(a.checkpoint(), b.checkpoint());
    let from_a = a.prepare(code(Variant::Good)).unwrap();
    assert_eq!(
        b.commit(from_a).err(),
        Some(PublishRefusal::WitnessMismatch("trace"))
    );
    assert!(b.is_paused());
    assert_eq!(b.pair().code.id(), code(Variant::Stranded).id());
    let own = b.prepare(code(Variant::Good)).unwrap();
    assert!(b.commit(own).is_ok());
}

#[test]
fn a_witness_over_another_ancestor_is_refused() {
    let mut a = attached(Variant::Stranded, 4);
    let mut b = boundary_over(
        Variant::Stranded,
        ToyState {
            total: 9,
            hot: true,
            seen: 4,
        },
        4,
    );
    b.attach(1, 64).unwrap();
    feed(&mut a, &[Reset]);
    feed(&mut b, &[Reset]);
    let ha = a.pause().unwrap();
    assert_eq!(b.pause().unwrap(), ha);
    assert_ne!(
        a.checkpoint().unwrap().digest(),
        b.checkpoint().unwrap().digest()
    );
    let from_a = a.prepare(code(Variant::Good)).unwrap();
    assert_eq!(
        b.commit(from_a).err(),
        Some(PublishRefusal::WitnessMismatch("ancestor"))
    );
    assert!(b.is_paused());
}

#[test]
fn a_prepare_reads_only_and_a_refused_commit_changes_nothing() {
    let (mut base, _) = paused_base();
    let before = view(&mut base);
    let p = base.prepare(code(Variant::Good)).unwrap();
    assert_eq!(view(&mut base), before);
    assert_eq!(base.queued(), 2);
    base.reject();
    live_add(&mut base, 1);
    base.pause().unwrap();
    let paused = view(&mut base);
    let stats = base.stats();
    assert!(base.commit(p).is_err());
    assert_eq!(view(&mut base), paused);
    assert_eq!(base.stats(), stats);
    assert!(base.is_paused());
}
