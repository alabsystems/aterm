// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The provenance bundle (design §10.7): what it names, its bytes, the forgeries it refuses, its publication and its selection's provenance.

use aterm_aos::evidence::TESTED_ASSUMPTIONS;
use aterm_aos::{ArtifactId, ArtifactKind, Digest};

use super::bundle::{
    BUNDLE_DOMAIN, BUNDLE_GRADE, BUNDLE_MAGIC, BundleField, BundleRefusal, CandidateBundle,
    PLAN_ASSUMPTIONS, Publication, PublishedHead, RowVerdict, SelectionProvenance, UNDECLARED,
    UncheckedBundle,
};
use super::corpus::{CorpusSplit, ItemKind};
use super::fidelity::{LaneFidelity, NO_STEP_SLEPT, ORACLE};
use super::lane::LANE_MODEL;
use super::m4_tests::{cold_only, evaluator, gap0, index_of, plan, searched};
use super::plan::{PLAN_INTERPRETER, Split, Wake};
use super::policy::{ITEMS_V1, MAX_ROUNDS};
use super::search::{GridProposer, Mode, Proposer, ProposerId};
use super::{MomentumCode, NoStrandedLight, SHIPPED_CODE_NAME, profile_artifact, schema_artifact};

/// The grid search's bundle, as its parts.
fn accepted() -> UncheckedBundle {
    searched().admitted.bundle().parts().clone()
}

/// `parts`, resealed and verified.
fn verify(parts: &UncheckedBundle) -> Result<CandidateBundle, BundleRefusal> {
    CandidateBundle::verify(&parts.encode(), evaluator())
}

fn forged(field: BundleField, row: Option<&str>) -> Result<CandidateBundle, BundleRefusal> {
    Err(BundleRefusal::Forged {
        field,
        row: row.map(str::to_owned),
    })
}

/// An accepted bundle holds every part plan §5 lists (design §10.7), each as the evaluator computed it, with one row per corpus item in policy order, no publication and no timings.
#[test]
fn an_accepted_bundle_names_every_plan_s5_part() {
    let ev = evaluator();
    let b = accepted();
    let p = gap0();
    assert_eq!(&b.policy, ev.policy().id());
    assert_eq!(
        b.base,
        MomentumCode::Shipped.artifact(super::test_fixtures::BUILD)
    );
    assert_eq!(b.candidate, p.artifact(super::test_fixtures::BUILD));
    assert_eq!(b.plan, p.encode());
    assert_eq!(b.proposer, GridProposer::new().id());
    assert!(b.round < MAX_ROUNDS);
    assert!(b.index < 54);
    assert_eq!(b.stranded_light, NoStrandedLight::artifact());
    assert_eq!(b.lane_fidelity, LaneFidelity::artifact());
    assert_eq!(
        (b.interpreter.as_str(), b.lane.as_str(), b.oracle.as_str()),
        (PLAN_INTERPRETER, LANE_MODEL, ORACLE)
    );
    assert_eq!(b.corpus, ev.corpus_digest());
    assert_eq!(b.schema, schema_artifact());
    assert_eq!(b.profile, profile_artifact());
    assert_eq!(b.build, super::test_fixtures::BUILD);
    assert_eq!(b.toolchain, UNDECLARED);
    assert_eq!(b.rows.len(), ITEMS_V1.len());
    for (r, pin) in b.rows.iter().zip(ITEMS_V1) {
        assert_eq!(
            (r.item.as_str(), r.split, r.kind),
            (pin.id(), pin.split(), pin.kind())
        );
        assert_eq!(r.ticks, r.presented + r.wasted);
        assert!(r.run.passed() && r.stranded_light.passed());
        if r.kind == ItemKind::GoldenTrace {
            assert_eq!(
                (&r.lane_fidelity, r.lane, r.firings),
                (&RowVerdict::NotRun, None, None),
                "the golden trace runs no lane"
            );
        } else {
            assert!(r.lane_fidelity.passed() && r.lane.is_some() && r.firings.is_some());
        }
    }
    let s = &b.selection;
    assert_eq!(s.mode, Mode::Search);
    assert_eq!(s.held_out_looks, 1);
    assert_eq!(
        (s.stats.proposed, s.stats.distinct, s.stats.admitted),
        (54, 54, 18)
    );
    assert_eq!(s.train.baseline, ev.policy().totals(CorpusSplit::Train));
    assert_eq!(
        s.held_out.baseline,
        ev.policy().totals(CorpusSplit::HeldOut)
    );
    assert!(s.useful);
    assert_eq!(s.gain_ppm, s.held_out.gain_ppm());
    assert_eq!(s.s0_gain_ppm, s.s0.gain_ppm());
    assert_eq!(b.publication, None);
    assert!(b.timings.is_empty());
    assert_eq!(b.grade, BUNDLE_GRADE);
    assert_eq!(b.tested_assumptions, TESTED_ASSUMPTIONS);
    assert_eq!(b.plan_assumptions, PLAN_ASSUMPTIONS);
}

/// A bundle round-trips through its bytes and verifies to itself, and the decode refuses every proper prefix, every flipped byte and an appended byte.
#[test]
fn a_bundle_round_trips_and_every_cut_or_flipped_byte_is_refused() {
    let ev = evaluator();
    let bundle = searched().admitted.bundle();
    let bytes = bundle.encode();
    println!("the bundle is {} bytes", bytes.len());
    assert_eq!(bytes[..BUNDLE_MAGIC.len()], BUNDLE_MAGIC);
    let n = bytes.len();
    let (body, seal) = bytes.split_at(n - 32);
    assert_eq!(seal, Digest::of(BUNDLE_DOMAIN, &[body]).0);
    let decoded = UncheckedBundle::decode(&bytes).expect("it decodes");
    assert_eq!(&decoded, bundle.parts());
    assert_eq!(decoded.encode(), bytes);
    let verified = CandidateBundle::verify(&bytes, ev).expect("it verifies");
    assert_eq!(&verified, bundle);
    let cuts: Vec<usize> = (0..n).collect();
    let refused = crate::test_pool::par_map(&cuts, |&k| {
        let cut = UncheckedBundle::decode(&bytes[..k]).err();
        let mut flipped = bytes.clone();
        flipped[k] ^= 0x01;
        let flip = UncheckedBundle::decode(&flipped).err();
        (k, cut, flip)
    });
    for (k, cut, flip) in refused {
        assert!(cut.is_some(), "the first {k} bytes are refused");
        assert!(flip.is_some(), "byte {k} flipped is refused");
        if k >= BUNDLE_MAGIC.len() {
            assert!(
                matches!(flip, Some(BundleRefusal::Seal { .. })),
                "byte {k}: {flip:?}"
            );
        }
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(matches!(
        UncheckedBundle::decode(&longer),
        Err(BundleRefusal::Seal { .. })
    ));
    let mut format = bytes;
    format[8] = 2;
    assert_eq!(
        UncheckedBundle::decode(&format),
        Err(BundleRefusal::Format { found: 2 })
    );
    assert_eq!(
        UncheckedBundle::decode(b"AOSCAND"),
        Err(BundleRefusal::Magic)
    );
}

/// A bundle resealed over altered evidence (rows, verdicts, identities, build, plan, sums, selection, grade, assumptions or timings) is refused as forged, naming the field and the row.
#[test]
fn a_resealed_bundle_with_altered_evidence_is_forged() {
    let ev = evaluator();
    let good = accepted();
    let t1 = index_of("train-01");
    let ks: Vec<usize> = (0..ITEMS_V1.len()).collect();
    let cold = cold_only();
    let cold_rows = crate::test_pool::par_map(&ks, |&k| ev.row_at(cold, k).expect("an item"));
    type Alter = Box<dyn Fn(&mut UncheckedBundle) + Sync>;
    let cases: Vec<(&str, Alter, Result<CandidateBundle, BundleRefusal>)> = vec![
        (
            "a lower T",
            Box::new(move |b| b.rows[t1].ticks -= 1),
            forged(BundleField::Ticks, Some("train-01")),
        ),
        (
            "a lower T, with P and the sums made to agree",
            Box::new(move |b| {
                b.rows[t1].ticks -= 1;
                b.rows[t1].presented -= 1;
                b.selection.train.candidate.ticks -= 1;
                b.selection.train.candidate.presented -= 1;
            }),
            forged(BundleField::Ticks, Some("train-01")),
        ),
        (
            "a failure made a pass (the cold-only plan's real rows, NoStepSlept flipped)",
            Box::new(move |b| {
                b.plan = cold.encode().to_vec();
                b.candidate = cold.artifact(&b.build);
                b.rows.clone_from(&cold_rows);
                for r in &mut b.rows {
                    if matches!(r.lane_fidelity, RowVerdict::Fail { .. }) {
                        r.lane_fidelity = RowVerdict::Pass;
                    }
                }
            }),
            forged(BundleField::LaneFidelityVerdict, Some("train-01")),
        ),
        (
            "a dropped row",
            Box::new(|b| {
                b.rows.remove(5);
            }),
            forged(BundleField::Row, Some(ITEMS_V1[5].id())),
        ),
        (
            "two rows swapped",
            Box::new(|b| b.rows.swap(1, 2)),
            forged(BundleField::Row, Some(ITEMS_V1[1].id())),
        ),
        (
            "an extra row",
            Box::new(|b| {
                let r = b.rows[1].clone();
                b.rows.push(r);
            }),
            forged(BundleField::Row, Some("train-01")),
        ),
        (
            "another policy digest",
            Box::new(|b| b.policy.digest.0[0] ^= 1),
            forged(BundleField::Policy, None),
        ),
        (
            "another build",
            Box::new(|b| "another-build".clone_into(&mut b.build)),
            forged(BundleField::Build, None),
        ),
        (
            "another build, its identities recomputed",
            Box::new(|b| {
                "another-build".clone_into(&mut b.build);
                b.base = MomentumCode::Shipped.artifact("another-build");
                b.candidate = gap0().artifact("another-build");
            }),
            forged(BundleField::Build, None),
        ),
        (
            "another candidate identity",
            Box::new(|b| b.candidate.digest.0[0] ^= 1),
            forged(BundleField::Candidate, None),
        ),
        (
            "a moved chain",
            Box::new(move |b| b.rows[t1].chain.0[31] ^= 1),
            forged(BundleField::Chain, Some("train-01")),
        ),
        (
            "a moved lane digest",
            Box::new(move |b| b.rows[t1].lane = b.rows[t1].lane.map(|l| l ^ 1)),
            forged(BundleField::LaneDigest, Some("train-01")),
        ),
        (
            "another count of guard firings",
            Box::new(move |b| b.rows[t1].firings = b.rows[t1].firings.map(|f| f + 1)),
            forged(BundleField::Firings, Some("train-01")),
        ),
        (
            "a lower sum",
            Box::new(|b| b.selection.held_out.candidate.ticks -= 1),
            forged(BundleField::Sums, None),
        ),
        (
            "another baseline",
            Box::new(|b| b.selection.held_out.baseline.ticks += 1),
            forged(BundleField::Sums, None),
        ),
        (
            "useful denied",
            Box::new(|b| b.selection.useful = false),
            forged(BundleField::Useful, None),
        ),
        (
            "a larger gain",
            Box::new(|b| b.selection.gain_ppm += 1),
            forged(BundleField::Gain, None),
        ),
        (
            "a second held-out look",
            Box::new(|b| b.selection.held_out_looks = 2),
            forged(BundleField::Stats, None),
        ),
        (
            "fewer rounds asked",
            Box::new(|b| b.selection.stats.rounds = 1),
            forged(BundleField::Stats, None),
        ),
        (
            "on demand, claiming a search's counts",
            Box::new(|b| b.selection.mode = Mode::OnDemand),
            forged(BundleField::Proposer, None),
        ),
        (
            "another grade",
            Box::new(|b| "Proved".clone_into(&mut b.grade)),
            forged(BundleField::Grade, None),
        ),
        (
            "an assumption dropped",
            Box::new(|b| {
                b.plan_assumptions.pop();
            }),
            forged(BundleField::Assumptions, None),
        ),
        (
            "another base",
            Box::new(|b| b.base = MomentumCode::DemoStrandedSettle.artifact(&b.build)),
            forged(BundleField::Base, None),
        ),
        (
            "an unlabelled timing",
            Box::new(|b| {
                b.timings.push(super::bundle::Timing {
                    label: String::new(),
                    measured_ns: 1,
                });
            }),
            forged(BundleField::Timings, None),
        ),
        (
            "another plan over the same evidence",
            Box::new(|b| {
                let other = plan(Split::SettleAtStep { gap_us: 8_333 }, Wake::NextStep);
                b.plan = other.encode().to_vec();
                b.candidate = other.artifact(&b.build);
            }),
            forged(BundleField::LaneDigest, Some("train-01")),
        ),
    ];
    let results = crate::test_pool::par_map(&cases, |(what, alter, _)| {
        let mut b = good.clone();
        alter(&mut b);
        (*what, CandidateBundle::verify(&b.encode(), ev))
    });
    for ((what, got), (_, _, want)) in results.iter().zip(&cases) {
        println!("{what}: {:?}", got.as_ref().err());
        assert_eq!(got, want, "{what}");
    }
}

/// A refused candidate is no bundle: the cold-only plan's real rows are forged at the first lane item, resealed as they are or with their failures made passes.
#[test]
fn a_refused_candidate_is_no_bundle() {
    let ev = evaluator();
    let p = cold_only();
    let ks: Vec<usize> = (0..ITEMS_V1.len()).collect();
    let real = crate::test_pool::par_map(&ks, |&k| ev.row_at(p, k).expect("an item"));
    assert!(
        matches!(&real[index_of("train-01")].lane_fidelity, RowVerdict::Fail { law, .. } if law == NO_STEP_SLEPT)
    );
    let mut b = accepted();
    b.plan = p.encode().to_vec();
    b.candidate = p.artifact(&b.build);
    b.rows.clone_from(&real);
    assert_eq!(
        verify(&b),
        forged(BundleField::LaneFidelityVerdict, Some("train-01")),
        "an honest failure is not an admission"
    );
    for r in &mut b.rows {
        if matches!(r.lane_fidelity, RowVerdict::Fail { .. }) {
            r.lane_fidelity = RowVerdict::Pass;
        }
    }
    assert_eq!(
        verify(&b),
        forged(BundleField::LaneFidelityVerdict, Some("train-01")),
        "a failure made a pass is re-evaluated"
    );
    assert!(real.iter().all(|r| r.stranded_light.passed()));
}

/// The publication part is checked for consistency only: one naming another candidate, a failing candidate verdict, no later revision or a range past the head is forged.
#[test]
fn the_publication_is_checked_for_consistency_only() {
    let mut b = accepted();
    let pubn = Publication {
        head: PublishedHead {
            identity: Digest([3; 32]),
            revision: 4,
            branch: 0,
            state: Digest([5; 32]),
            input_cursor: 900,
            effect_cursor: 0,
        },
        revision: 5,
        live_code: MomentumCode::Shipped.artifact(&b.build),
        live_verdict: RowVerdict::Pass,
        trace: Digest([6; 32]),
        from: 0,
        to: 900,
        log_epoch: 1,
        ancestor: Digest([7; 32]),
        candidate_code: b.candidate.clone(),
        candidate_verdict: RowVerdict::Pass,
        candidate_chain: Digest([8; 32]),
        candidate_state: Digest([9; 32]),
    };
    type Alter = fn(&mut Publication);
    let cases: [(&str, Option<Alter>); 5] = [
        ("consistent", None),
        (
            "another candidate",
            Some(|p| p.candidate_code = p.live_code.clone()),
        ),
        (
            "a failing candidate",
            Some(|p| p.candidate_verdict = RowVerdict::fail(super::HOT_HAS_WAKE, 3)),
        ),
        ("no new revision", Some(|p| p.revision = p.head.revision)),
        ("past the head", Some(|p| p.to = p.head.input_cursor + 1)),
    ];
    let results = crate::test_pool::par_map(&cases, |(what, alter)| {
        let mut b = b.clone();
        let mut p = pubn.clone();
        if let Some(alter) = alter {
            alter(&mut p);
        }
        b.publication = Some(p);
        (*what, verify(&b))
    });
    for (what, got) in results {
        if what == "consistent" {
            assert!(got.is_ok(), "{what}: {got:?}");
        } else {
            assert_eq!(got, forged(BundleField::Publication, None), "{what}");
        }
    }
    b.publication = Some(pubn);
    let bytes = b.encode();
    assert_eq!(
        UncheckedBundle::decode(&bytes)
            .expect("it decodes")
            .publication,
        b.publication
    );
}

/// A publication of `b`'s candidate over the live code `live`, both judged passing, at a later revision over the head's range.
fn publication_over(b: &UncheckedBundle, live: ArtifactId) -> Publication {
    Publication {
        head: PublishedHead {
            identity: Digest([3; 32]),
            revision: 4,
            branch: 0,
            state: Digest([5; 32]),
            input_cursor: 900,
            effect_cursor: 0,
        },
        revision: 5,
        live_code: live,
        live_verdict: RowVerdict::Pass,
        trace: Digest([6; 32]),
        from: 0,
        to: 900,
        log_epoch: 1,
        ancestor: Digest([7; 32]),
        candidate_code: b.candidate.clone(),
        candidate_verdict: RowVerdict::Pass,
        candidate_chain: Digest([8; 32]),
        candidate_state: Digest([9; 32]),
    }
}

/// The publication's live code must be a base the policy names, as compiled into the bundle's build, and meet that base's rule: a useful result over the shipped code, any admitted one over the demonstration fault.
#[test]
fn the_publication_replaces_a_base_of_the_policy_by_its_rule() {
    let ev = evaluator();
    let useful = accepted();
    let cadence = ev
        .admit_one(&plan(Split::Shipped, Wake::Cadence).encode())
        .expect("the cadence plan is admitted")
        .bundle()
        .parts()
        .clone();
    assert!(useful.selection.useful && !cadence.selection.useful);
    let build = useful.build.clone();
    let shipped = MomentumCode::Shipped.artifact(&build);
    let demo = MomentumCode::DemoStrandedSettle.artifact(&build);
    let cases: Vec<(&str, &UncheckedBundle, ArtifactId, bool)> = vec![
        (
            "a useful plan over the shipped code",
            &useful,
            shipped.clone(),
            true,
        ),
        (
            "a useful plan over the demonstration fault",
            &useful,
            demo.clone(),
            true,
        ),
        (
            "a not-useful plan over the demonstration fault",
            &cadence,
            demo,
            true,
        ),
        (
            "a not-useful plan over the shipped code",
            &cadence,
            shipped,
            false,
        ),
        (
            "the shipped code of another build",
            &useful,
            MomentumCode::Shipped.artifact("another-build"),
            false,
        ),
        (
            "the shipped name at another version and digest",
            &useful,
            ArtifactId::new(ArtifactKind::Code, SHIPPED_CODE_NAME, 99, b"junk"),
            false,
        ),
        (
            "the shipped name with another descriptor",
            &useful,
            ArtifactId::new(
                ArtifactKind::Code,
                SHIPPED_CODE_NAME,
                1,
                b"another descriptor",
            ),
            false,
        ),
        (
            "the shipped name of another kind",
            &useful,
            ArtifactId::new(ArtifactKind::Policy, SHIPPED_CODE_NAME, 1, b"terms"),
            false,
        ),
        (
            "the demonstration fault of another build",
            &cadence,
            MomentumCode::DemoStrandedSettle.artifact("another-build"),
            false,
        ),
    ];
    let results = crate::test_pool::par_map(&cases, |(what, b, live, _)| {
        let mut b = (*b).clone();
        b.publication = Some(publication_over(&b, live.clone()));
        (*what, verify(&b))
    });
    for ((what, got), (_, _, _, ok)) in results.into_iter().zip(&cases) {
        println!("{what}: {:?}", got.as_ref().err());
        if *ok {
            assert!(got.is_ok(), "{what}: {got:?}");
        } else {
            assert_eq!(got, forged(BundleField::Publication, None), "{what}");
        }
    }
}

/// A search credited to the grid must be the grid's own selection, which `verify` re-runs, while another proposer's selection verifies as declared.
#[test]
fn a_selection_credited_to_the_grid_is_the_grids() {
    let ev = evaluator();
    let grid = accepted();
    let relabel = |b: &mut UncheckedBundle, from: &UncheckedBundle| {
        b.selection.mode = from.selection.mode;
        b.selection.stats = from.selection.stats;
        b.proposer = from.proposer.clone();
        b.round = from.round;
        b.index = from.index;
    };
    let mut honest = ev
        .admit_one(&gap0().encode())
        .expect("the winner is admitted")
        .bundle()
        .parts()
        .clone();
    relabel(&mut honest, &grid);
    assert_eq!(honest.encode(), grid.encode(), "the grid's own bundle");
    let rederived = verify(&honest).expect("the grid's bundle verifies");
    assert_eq!(
        rederived.selection_provenance(),
        SelectionProvenance::Rederived
    );
    assert_eq!(SelectionProvenance::Rederived.name(), "rederived");

    let runner_up = plan(Split::SettleAtStep { gap_us: 8_333 }, Wake::NextStep);
    let other = ev
        .admit_one(&runner_up.encode())
        .expect("the runner-up is admitted")
        .bundle()
        .parts()
        .clone();
    assert!(other.selection.train.beats(), "fixture: it beats training");
    let on_demand = verify(&other).expect("an on-demand bundle verifies");
    assert_eq!(
        on_demand.selection_provenance(),
        SelectionProvenance::Declared
    );
    let mut credited = other.clone();
    relabel(&mut credited, &grid);
    let mut index = grid.clone();
    index.index += 1;
    let mut round = grid.clone();
    round.round = 1;
    let mut stats = grid.clone();
    stats.selection.stats.admitted -= 1;
    let cases = [
        (
            "another plan credited to the grid",
            credited,
            forged(BundleField::Proposer, None),
        ),
        ("another index", index, forged(BundleField::Proposer, None)),
        ("another round", round, forged(BundleField::Proposer, None)),
        ("other counts", stats, forged(BundleField::Stats, None)),
    ];
    let results = crate::test_pool::par_map(&cases, |(what, b, _)| (*what, verify(b)));
    for ((what, got), (_, _, want)) in results.iter().zip(&cases) {
        println!("{what}: {:?}", got.as_ref().err());
        assert_eq!(got, want, "{what}");
    }

    let mut declared = other;
    declared.selection.mode = Mode::Search;
    declared.selection.stats = grid.selection.stats;
    declared.proposer = ProposerId::new("someone", 7, b"anything");
    declared.round = 2;
    declared.index = 40;
    let declared = verify(&declared).expect("another proposer's search is declared data");
    assert_eq!(
        declared.selection_provenance(),
        SelectionProvenance::Declared
    );
    assert_eq!(SelectionProvenance::Declared.name(), "declared");
}
