// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication of an admitted plan through the wrapper with injected instants.

use aterm_aos::contract::Verdict;
use aterm_aos::effects::NoEffects;
use aterm_aos::publish::{Head, PublishRefusal};
use aterm_aos::replay::replay;
use aterm_aos::task::state_digest;
use aterm_aos::{ArtifactId, ArtifactKind, Digest};
use aterm_time::Instant;

use super::bundle::{
    BUNDLE_DOMAIN, BundleField, BundleRefusal, CandidateBundle, Publication, PublishedHead,
    RowVerdict, UncheckedBundle,
};
use super::m4::{Admitted, Evaluator};
use super::m4_tests::{evaluator, gap0, plan};
use super::plan::{PLAN_NAME, SchedulePlan, Split, Wake};
use super::policy::M4_POLICY_V1_DIGEST;
use super::publish::PlanPublishRefusal;
use super::test_fixtures::{
    BUILD, LANE, TAU, attached, attached_for_lane, burst, cfg, hosted, hosted_mut, judged_log, ms,
    run_lane, strand,
};
use super::{HOT_HAS_WAKE, MomentumCode, MomentumEvent, MomentumHost, PrebuiltMomentum, Sched};

/// The grid winner, admitted once: an admission of one plan on one evaluator is the same bundle
/// every time (`same_policy_same_winner_same_bundle_digest`).
fn winner() -> Admitted {
    static WINNER: std::sync::OnceLock<Admitted> = std::sync::OnceLock::new();
    WINNER
        .get_or_init(|| {
            evaluator()
                .admit_one(&gap0().encode())
                .expect("the grid winner is admitted")
        })
        .clone()
}

fn cadence() -> SchedulePlan {
    plan(Split::Shipped, Wake::Cadence)
}

fn typed_and_paused(t0: Instant, sched: Sched) -> (MomentumHost, Instant, Head) {
    let mut host = match sched {
        Sched::Prebuilt(code) => attached(t0, code),
        Sched::Plan(_) => attached_for_lane(t0, sched),
    };
    let mut seen = Vec::new();
    let (last, _) = burst(&mut host, t0, 12, &cfg(true), &mut seen);
    let head = host
        .aos_pause(last, LANE)
        .expect("a running attached glow pauses");
    assert!(host.aos_status().hot, "fixture: the glow is hot");
    (host, last, head)
}

fn queued_two(t0: Instant) -> (MomentumHost, Instant, Head) {
    let (mut host, last, head) = typed_and_paused(t0, Sched::Prebuilt(MomentumCode::Shipped));
    host.on_key(last + ms(30), TAU);
    host.on_key(last + ms(60), TAU);
    assert_eq!(
        hosted(&host).boundary.queued(),
        2,
        "fixture: two keys queued"
    );
    (host, last + ms(60), head)
}

fn untouched(
    host: &MomentumHost,
    head: &Head,
    queued: usize,
    sched: Sched,
    (replays, publications): (u64, u64),
    why: &str,
) {
    let b = &hosted(host).boundary;
    assert!(host.aos_paused(), "{why}: still paused");
    assert_eq!(b.paused_head(), Some(head), "{why}: the same paused head");
    assert_eq!(b.queued(), queued, "{why}: the queue intact");
    assert_eq!(host.aos_sched(), sched, "{why}: the live code unchanged");
    let st = b.stats();
    assert_eq!(
        (st.replays, st.publications),
        (replays, publications),
        "{why}: the counters"
    );
}

#[test]
fn an_admitted_plan_publishes_once() {
    let t0 = Instant::now();
    let (mut host, at, head) = queued_two(t0);
    let before = host.aos_status().stats;
    let published = host
        .aos_publish(winner())
        .expect("the grid winner publishes over the shipped code");
    assert!(published.revision() > head.revision, "a new revision");
    assert_eq!(published.delivered().len(), 2);
    assert!(
        published.delivered().iter().all(Result::is_ok),
        "each queued key is admitted by the new pair"
    );
    assert_eq!(host.aos_sched(), Sched::Plan(gap0()));
    assert_eq!(host.aos_code(), None, "a plan is no prebuilt code");
    assert!(!host.aos_paused());
    let s = host.aos_status();
    assert_eq!(s.code, PLAN_NAME);
    assert_eq!(s.revision, published.revision().0);
    assert_eq!(
        (s.stats.replays, s.stats.publications),
        (before.replays + 2, before.publications + 1)
    );
    let log = hosted(&host).boundary.log().expect("attached").inputs();
    assert_eq!(
        log.len(),
        2,
        "the new log holds the delivered queue and nothing else"
    );
    assert!(
        log.iter().all(|e| matches!(e, MomentumEvent::Key { .. })),
        "{log:?}"
    );
    assert_eq!(
        judged_log(&host),
        (Verdict::Pass, true),
        "the new log, replayed from the new checkpoint, reproduces the live head"
    );

    let c = cfg(true);
    let mut seen = Vec::new();
    let parked = run_lane(&mut host, at, LANE, (4, 21), &c, &mut seen);
    assert!(!parked.stranded, "the published plan clears the glow");
    assert!(!host.aos_status().hot);
    assert_eq!(judged_log(&host), (Verdict::Pass, true));

    assert_eq!(
        host.aos_publish(winner()).err(),
        Some(PlanPublishRefusal::NotPaused)
    );
    let again = host
        .aos_pause(parked.at, LANE)
        .expect("the published pair pauses");
    let counted = host.aos_status().stats;
    assert_eq!(
        host.aos_publish(winner()).err(),
        Some(PlanPublishRefusal::BaseNotInPolicy { live: PLAN_NAME })
    );
    untouched(
        &host,
        &again,
        0,
        Sched::Plan(gap0()),
        (counted.replays, counted.publications),
        "a second result",
    );
    assert_eq!(counted.publications, 1);
}

#[test]
fn a_stale_winner_is_refused() {
    type Between = fn(&mut MomentumHost, Instant);
    let input: Between = |h, at| {
        let _ = h.aos_resume();
        h.on_key(at + ms(30), TAU);
        h.aos_pause(at + ms(40), LANE).expect("pauses");
    };
    let restore: Between = |h, at| {
        let ancestor = hosted(h).boundary.checkpoint().expect("attached").clone();
        assert_eq!(
            hosted_mut(h).boundary.restore(&ancestor).err(),
            Some(PublishRefusal::AlreadyPaused),
            "a restore is refused while paused"
        );
        let _ = h.aos_resume();
        hosted_mut(h)
            .boundary
            .restore(&ancestor)
            .expect("the ancestor restores once running");
        h.aos_pause(at + ms(40), LANE).expect("pauses");
    };
    let rival_repair: Between = |h, at| {
        h.aos_repair(MomentumCode::Shipped)
            .expect("the shipped code commits over its own head");
        h.aos_pause(at + ms(40), LANE).expect("pauses");
    };
    let rival_plan: Between = |h, at| {
        h.aos_publish(winner()).expect("the rival publishes first");
        h.aos_pause(at + ms(40), LANE).expect("pauses");
    };
    let cases = [
        ("an input", input, Sched::Prebuilt(MomentumCode::Shipped), 0),
        (
            "a restore",
            restore,
            Sched::Prebuilt(MomentumCode::Shipped),
            0,
        ),
        (
            "a rival repair",
            rival_repair,
            Sched::Prebuilt(MomentumCode::Shipped),
            1,
        ),
        ("a rival plan", rival_plan, Sched::Plan(gap0()), 1),
    ];
    let t0 = Instant::now();
    let results = crate::test_pool::par_map(&cases, |&(why, between, live, publications)| {
        let (mut host, at, head) = typed_and_paused(t0, Sched::Prebuilt(MomentumCode::Shipped));
        let got = host.aos_publish_after(winner(), |h| between(h, at));
        let live_head = hosted(&host)
            .boundary
            .paused_head()
            .expect("re-paused")
            .clone();
        assert_eq!(
            got.err(),
            Some(PlanPublishRefusal::Boundary(PublishRefusal::StaleHead {
                expected: Box::new(head.clone()),
                live: Box::new(live_head.clone()),
            })),
            "{why}"
        );
        assert_ne!(live_head, head, "{why}: the head moved");
        assert!(host.aos_paused(), "{why}: still paused");
        assert_eq!(
            host.aos_sched(),
            live,
            "{why}: the live code as the rival left it"
        );
        assert_eq!(
            host.aos_status().stats.publications,
            publications,
            "{why}: nothing more published"
        );
        let later = (live == Sched::Plan(gap0())).then(|| host.aos_publish(winner()).err());
        (why, later)
    });
    for (why, later) in results {
        if let Some(later) = later {
            assert_eq!(
                later,
                Some(PlanPublishRefusal::BaseNotInPolicy { live: PLAN_NAME }),
                "{why}: a later winner finds the rival's live plan"
            );
        }
    }
}

#[test]
fn publishing_over_an_unnamed_base_is_refused() {
    let t0 = Instant::now();
    let live = Sched::Plan(cadence());
    let (mut host, _, head) = typed_and_paused(t0, live);
    let other = ArtifactId::new(ArtifactKind::Policy, "another-policy", 1, b"terms");
    let not_cadence = evaluator()
        .admit_one(&cadence().encode())
        .expect("the cadence plan is admitted");
    for (why, admitted, want) in [
        (
            "the useful winner",
            winner(),
            PlanPublishRefusal::BaseNotInPolicy { live: PLAN_NAME },
        ),
        (
            "a plan that is not useful",
            not_cadence,
            PlanPublishRefusal::BaseNotInPolicy { live: PLAN_NAME },
        ),
        (
            "a result of another policy",
            winner().under_policy(other.clone()),
            PlanPublishRefusal::PolicyMismatch {
                admitted: other.digest,
                pinned: M4_POLICY_V1_DIGEST,
            },
        ),
    ] {
        assert_eq!(host.aos_publish(admitted).err(), Some(want), "{why}");
        untouched(&host, &head, 0, live, (0, 0), why);
    }
}

#[test]
fn publishing_over_the_safe_code_is_refused_base_not_in_policy() {
    let t0 = Instant::now();
    let live = Sched::Prebuilt(MomentumCode::Safe);
    let (mut host, _, head) = typed_and_paused(t0, live);
    assert_eq!(
        host.aos_publish(winner()).err(),
        Some(PlanPublishRefusal::BaseNotInPolicy { live: "safe" })
    );
    untouched(&host, &head, 0, live, (0, 0), "over the safe code");
    let (mut shipped, _, _) = typed_and_paused(t0, Sched::Prebuilt(MomentumCode::Shipped));
    assert!(
        shipped.aos_publish(winner()).is_ok(),
        "negative control: the same winner publishes over the shipped code"
    );
}

#[test]
fn each_refusal_before_the_boundary_is_named_and_changes_nothing() {
    let t0 = Instant::now();
    let mut direct = MomentumHost::default();
    assert_eq!(
        direct.aos_publish(winner()).err(),
        Some(PlanPublishRefusal::NotAttached)
    );
    assert!(!direct.aos_attached(), "nothing was attached");

    let mut running = attached(t0, MomentumCode::Shipped);
    let mut seen = Vec::new();
    let _ = burst(&mut running, t0, 4, &cfg(true), &mut seen);
    assert_eq!(
        running.aos_publish(winner()).err(),
        Some(PlanPublishRefusal::NotPaused)
    );
    assert!(!running.aos_paused(), "publication never pauses on its own");
    assert_eq!(running.aos_status().stats.replays, 0);

    let (mut host, _, head) = queued_two(t0);
    let shipped = Sched::Prebuilt(MomentumCode::Shipped);
    let other = ArtifactId::new(ArtifactKind::Policy, "another-policy", 1, b"terms");
    assert_eq!(
        host.aos_publish(winner().under_policy(other.clone())).err(),
        Some(PlanPublishRefusal::PolicyMismatch {
            admitted: other.digest,
            pinned: M4_POLICY_V1_DIGEST,
        })
    );
    untouched(&host, &head, 2, shipped, (0, 0), "another policy");

    let foreign = Evaluator::v1("another-build", t0)
        .expect("the pinned policy v1")
        .admit_one(&gap0().encode())
        .expect("admitted in its own build");
    assert_eq!(
        host.aos_publish(foreign).err(),
        Some(PlanPublishRefusal::ForeignBuild {
            admitted: "another-build".to_owned(),
            host: BUILD.to_owned(),
        })
    );
    untouched(&host, &head, 2, shipped, (0, 0), "another build");
}

#[test]
fn a_not_useful_plan_repairs_the_demo_fault_but_cannot_replace_shipped() {
    let t0 = Instant::now();
    let admitted = || {
        evaluator()
            .admit_one(&cadence().encode())
            .expect("the cadence plan is admitted")
    };
    let over_shipped = admitted();
    assert!(!over_shipped.useful(), "fixture: not useful");
    let gain_ppm = over_shipped.gain().ppm();
    let shipped = Sched::Prebuilt(MomentumCode::Shipped);
    let (mut host, _, head) = typed_and_paused(t0, shipped);
    assert_eq!(
        host.aos_publish(over_shipped).err(),
        Some(PlanPublishRefusal::NotUseful {
            plan: cadence().digest(),
            gain_ppm,
        })
    );
    untouched(&host, &head, 0, shipped, (0, 0), "not useful over shipped");

    for dark in [true, false] {
        let (mut demo, now, cell) =
            strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, dark);
        demo.aos_pause(now, LANE).expect("the stranded glow pauses");
        let published = demo
            .aos_publish(admitted())
            .expect("the cadence plan repairs the demonstration fault");
        let p = published.publication().expect("filled");
        assert!(
            matches!(&p.live_verdict, RowVerdict::Fail { law, .. } if law == HOT_HAS_WAKE),
            "dark={dark}: the live code fails the corrected contract: {:?}",
            p.live_verdict
        );
        assert_eq!(p.candidate_verdict, RowVerdict::Pass);
        assert_eq!(
            p.live_code,
            MomentumCode::DemoStrandedSettle.artifact(BUILD)
        );
        assert_eq!(demo.aos_sched(), Sched::Plan(cadence()));
        let bytes = published.bundle().encode();
        assert_eq!(
            CandidateBundle::verify(&bytes, evaluator()).as_ref(),
            Ok(published.bundle()),
            "dark={dark}: the repair's bundle verifies"
        );
        assert!(demo.settling(), "the accepted state is the settling glow");
        let mut seen = Vec::new();
        let cleared = run_lane(&mut demo, now, LANE, cell, &cfg(dark), &mut seen);
        assert!(
            !cleared.stranded,
            "dark={dark}: the repaired lane parks cold"
        );
    }
}

/// Where an interrupted commit leaves the pair.
#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Installed {
    Old,
    New,
    Mixed,
}

/// A commit stopped after any of its primitive stores leaves the complete old pair or the complete new one, never a mix, still paused with both queued keys.
#[cfg(debug_assertions)]
#[test]
fn an_interrupted_plan_commit_leaves_one_complete_pair() {
    let t0 = Instant::now();
    let (mut reference, _, head) = queued_two(t0);
    let done = reference
        .aos_publish(winner())
        .expect("the uninterrupted publication");
    let new_state = done.publication().expect("filled").candidate_state;
    let (old_code, new_code) = (
        MomentumCode::Shipped.artifact(BUILD),
        gap0().artifact(BUILD),
    );
    let ks: Vec<usize> = (0..=8).collect();
    let results = crate::test_pool::par_map(&ks, |&k| {
        let (mut host, _, h) = queued_two(t0);
        assert_eq!(
            h, head,
            "fixture: the run is a function of its pinned instants"
        );
        let old_len = hosted(&host).boundary.log().expect("attached").len();
        match host.aos_publish_stopping_after(winner(), k) {
            None => {
                let hd = hosted(&host);
                let pair = hd.boundary.pair();
                let state = state_digest(hd.boundary.schema(), &pair.state);
                let at = if pair.code.id == old_code
                    && pair.head.revision == head.revision
                    && state == head.state_digest
                {
                    Installed::Old
                } else if pair.code.id == new_code
                    && pair.head.revision == done.revision()
                    && state == new_state
                {
                    Installed::New
                } else {
                    Installed::Mixed
                };
                assert!(host.aos_paused(), "k = {k}: still paused");
                assert_eq!(hd.boundary.queued(), 2, "k = {k}: the queue whole");
                let delivered = host.aos_resume();
                assert_eq!(delivered.len(), 2, "k = {k}");
                assert!(delivered.iter().all(Result::is_ok), "k = {k}");
                let log = hosted(&host).boundary.log().expect("attached").inputs();
                let want = if at == Installed::Old { old_len + 2 } else { 2 };
                assert_eq!(log.len(), want, "k = {k}: delivered once into its own log");
                assert!(
                    log[log.len() - 2..]
                        .iter()
                        .all(|e| matches!(e, MomentumEvent::Key { .. })),
                    "k = {k}"
                );
                Some(at)
            }
            Some(got) => {
                let p = got.unwrap_or_else(|r| panic!("k = {k}: the commit ran to the end: {r}"));
                assert_eq!(p.revision(), done.revision(), "k = {k}");
                assert_eq!(p.delivered().len(), 2, "k = {k}");
                assert_eq!(p.bundle(), done.bundle(), "k = {k}: the same bundle");
                assert!(!host.aos_paused(), "k = {k}");
                assert_eq!(host.aos_sched(), Sched::Plan(gap0()), "k = {k}");
                None
            }
        }
    });
    let stopped: Vec<Installed> = results.iter().flatten().copied().collect();
    assert_eq!(
        stopped,
        vec![
            Installed::Old,
            Installed::Old,
            Installed::New,
            Installed::New
        ]
    );
    assert!(
        results[4..].iter().all(Option::is_none),
        "k >= 4 runs the commit to the end"
    );
}

#[test]
fn a_published_plan_records_its_witness_and_revision() {
    let t0 = Instant::now();
    let (mut host, _, head) = typed_and_paused(t0, Sched::Prebuilt(MomentumCode::Shipped));
    let b = &hosted(&host).boundary;
    let ancestor = b.checkpoint().expect("attached").clone();
    let log = b.log().expect("attached");
    let (from, to) = (log.header().first_cursor, log.end_cursor());
    let trace = log
        .trace_digest(b.schema(), from, to)
        .expect("the live log's range");
    let log_epoch = log.header().log_epoch;
    let start = ancestor
        .restore(b.schema(), b.identity())
        .expect("the ancestor restores");
    let planned = replay(
        b.schema(),
        &PrebuiltMomentum::planned(gap0(), BUILD),
        start,
        log.inputs(),
        from,
        &mut NoEffects,
    );
    let admitted = winner();
    let computed = admitted.bundle().parts().clone();
    let published = host.aos_publish(admitted).expect("publishes");
    let p = published.publication().expect("filled");
    assert_eq!(
        p.head,
        PublishedHead {
            identity: head.identity,
            revision: head.revision.0,
            branch: head.branch.0,
            state: head.state_digest,
            input_cursor: head.input_cursor,
            effect_cursor: head.effect_cursor,
        }
    );
    assert_eq!(p.revision, published.revision().0);
    assert_eq!(p.revision, host.aos_status().revision);
    assert_eq!(p.live_code, MomentumCode::Shipped.artifact(BUILD));
    assert_eq!(p.live_verdict, RowVerdict::Pass);
    assert_eq!(p.trace, trace);
    assert_eq!((p.from, p.to), (from, to));
    assert_eq!(p.to, head.input_cursor);
    assert_eq!(p.log_epoch, log_epoch);
    assert_eq!(p.ancestor, ancestor.digest());
    assert_eq!(p.candidate_code, gap0().artifact(BUILD));
    assert_eq!(p.candidate_code, computed.candidate);
    assert_eq!(p.candidate_verdict, RowVerdict::Pass);
    assert_eq!(p.candidate_chain, planned.observation_chain);
    assert_eq!(p.candidate_state, planned.state_digest);
    let hd = hosted(&host);
    assert_eq!(
        p.candidate_state,
        state_digest(hd.boundary.schema(), hd.state())
    );
    assert_eq!(
        hd.boundary
            .checkpoint()
            .expect("attached")
            .provenance()
            .parent,
        Some(ancestor.digest())
    );
    let mut rest = published.bundle().parts().clone();
    rest.publication = None;
    assert_eq!(rest, computed, "only the publication part is filled");
}

#[test]
fn a_published_bundle_verifies_and_a_forged_publication_is_refused() {
    let t0 = Instant::now();
    let (mut host, _, _) = typed_and_paused(t0, Sched::Prebuilt(MomentumCode::Shipped));
    let published = host.aos_publish(winner()).expect("publishes");
    let bytes = published.bundle().encode();
    let verified = CandidateBundle::verify(&bytes, evaluator()).expect("the bundle verifies");
    assert_eq!(&verified, published.bundle());
    assert_eq!(
        published.bundle().digest(),
        Digest::of(BUNDLE_DOMAIN, &[&bytes]),
        "a bundle is named by the digest of its bytes"
    );
    assert_eq!(
        UncheckedBundle::decode(&bytes)
            .expect("decodes")
            .publication
            .as_ref(),
        published.publication()
    );
    type Alter = fn(&mut Publication);
    let cases: [(&str, Alter); 10] = [
        ("the live code off the bases", |p| {
            p.live_code = ArtifactId::new(ArtifactKind::Code, "momentum-glow.other", 1, b"x");
        }),
        ("the plan as its own live code", |p| {
            p.live_code = p.candidate_code.clone();
        }),
        ("the shipped code of another build", |p| {
            p.live_code = MomentumCode::Shipped.artifact("another-build");
        }),
        ("the shipped name at another version and digest", |p| {
            p.live_code = ArtifactId::new(ArtifactKind::Code, "momentum-glow.shipped", 99, b"junk");
        }),
        ("another candidate", |p| {
            p.candidate_code = MomentumCode::Shipped.artifact(BUILD);
        }),
        ("a live verdict that never ran", |p| {
            p.live_verdict = RowVerdict::NotRun;
        }),
        ("a failing candidate", |p| {
            p.candidate_verdict = RowVerdict::fail(HOT_HAS_WAKE, 3);
        }),
        ("no new revision", |p| p.revision = p.head.revision),
        ("a reversed range", |p| p.from = p.to + 1),
        ("a range short of the head", |p| {
            p.to = p.head.input_cursor - 1
        }),
    ];
    let parts = published.bundle().parts().clone();
    let results = crate::test_pool::par_map(&cases, |&(why, alter)| {
        let mut forged = parts.clone();
        if let Some(p) = forged.publication.as_mut() {
            alter(p);
        }
        (why, CandidateBundle::verify(&forged.encode(), evaluator()))
    });
    for (why, got) in results {
        assert_eq!(
            got,
            Err(BundleRefusal::Forged {
                field: BundleField::Publication,
                row: None,
            }),
            "{why}"
        );
    }
    let not_useful = evaluator()
        .admit_one(&cadence().encode())
        .expect("the cadence plan is admitted");
    assert!(!not_useful.useful(), "fixture: not useful");
    let mut moved = not_useful.bundle().parts().clone();
    let mut p = published.publication().expect("filled").clone();
    p.candidate_code = moved.candidate.clone();
    moved.publication = Some(p);
    assert_eq!(
        CandidateBundle::verify(&moved.encode(), evaluator()),
        Err(BundleRefusal::Forged {
            field: BundleField::Publication,
            row: None,
        }),
        "a not-useful plan published over the shipped code"
    );
}
