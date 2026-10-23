// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Proposers and budgets (design §10.2): the grid and file proposers, the evaluator's budgets, and a hostile proposer that gains nothing.

use super::m4::{CandidateRefusal, NoOutcome, PLAN_REFUSED};
use super::m4_tests::{evaluator, gap0, searched};
use super::plan::{PLAN_DOMAIN, PLAN_LEN, PlanRefusal, SHIPPED_PLAN, SchedulePlan};
use super::policy::{GRID_V1, MAX_DISTINCT, MAX_PROPOSAL_BYTES, MAX_ROUNDS};
use super::search::{
    FILE_PROPOSER, FileProposer, GRID_PROPOSER, GridProposer, Mode, PROPOSER_DOMAIN, Proposer,
    ProposerId, SearchStats, TrainFeedback, candidate_id,
};
use aterm_aos::Digest;

/// The grid proposer proposes the policy's 54 plans in [`GRID_V1`]'s order in round 0 and nothing after, whatever it is shown, and its identity is configured by those bytes.
#[test]
fn the_grid_proposer_is_the_policy_grid_once() {
    let plans = GridProposer::plans();
    assert_eq!(plans.len(), 54);
    for (bytes, &(split, wake)) in plans.iter().zip(&GRID_V1) {
        let p = SchedulePlan::decode(bytes).expect("a grid plan decodes");
        assert_eq!((p.split(), p.wake()), (split, wake));
        assert_eq!(candidate_id(bytes), p.digest(), "a plan's id is its digest");
    }
    assert_eq!(
        plans,
        evaluator()
            .policy()
            .grid()
            .iter()
            .map(|p| p.encode().to_vec())
            .collect::<Vec<_>>()
    );
    let mut g = GridProposer::new();
    assert_eq!(g.propose(0, &[]), plans);
    let shown = [TrainFeedback::refused(Digest([7; 32]), "NoStepSlept")];
    for round in 1..=8 {
        assert!(g.propose(round, &shown).is_empty());
    }
    assert_eq!(g.propose(0, &shown), plans, "it ignores the feedback");
    let id = g.id();
    assert_eq!((id.name.as_str(), id.version), (GRID_PROPOSER, 1));
    assert_eq!(id.config, Digest::of(PROPOSER_DOMAIN, &[&plans.concat()]));
    assert_eq!(id, GridProposer::new().id());
}

/// The file proposer reads hex lines as their bytes and any other line as a candidate the evaluator refuses at decode, so it never fails.
#[test]
fn the_file_proposer_parses_hex_lines_and_never_fails() {
    let hex =
        |p: SchedulePlan| -> String { p.encode().iter().map(|b| format!("{b:02x}")).collect() };
    let text = format!(
        "# Aos M4 candidates\n\n{}\n  {}  # the winner, upper case\nnot hex at all\nabc\n{} # again\n",
        hex(SHIPPED_PLAN),
        hex(gap0()).to_uppercase(),
        hex(SHIPPED_PLAN),
    );
    let mut f = FileProposer::from_text(&text);
    let c = f.candidates().to_vec();
    assert_eq!(c.len(), 5);
    assert_eq!(c[0], SHIPPED_PLAN.encode());
    assert_eq!(c[1], gap0().encode());
    assert_eq!(c[2], b"not hex at all");
    assert_eq!(c[3], b"abc", "an odd number of digits is not hex");
    assert_eq!(c[4], c[0]);
    let id = f.id();
    assert_eq!((id.name.as_str(), id.version), (FILE_PROPOSER, 1));
    assert_eq!(id.config, Digest::of(PROPOSER_DOMAIN, &[text.as_bytes()]));
    assert_ne!(id, FileProposer::from_text("").id());
    assert_eq!(f.propose(0, &[]).len(), 5);
    assert!(f.propose(1, &[]).is_empty());
    let ev = evaluator();
    for bad in [&c[2], &c[3]] {
        let r = ev.admit_one(bad).expect_err("refused");
        assert!(
            matches!(r, CandidateRefusal::Decode(PlanRefusal::Length { .. })),
            "{r:?}"
        );
        assert_eq!(r.law(), PLAN_REFUSED);
    }
    let twelve = FileProposer::from_text("twelve bytes");
    let r = ev.admit_one(&twelve.candidates()[0]).expect_err("refused");
    assert_eq!(
        r,
        CandidateRefusal::Decode(PlanRefusal::Format { found: b't' })
    );
    assert!(
        FileProposer::from_text("# only a comment\n\n   \n")
            .candidates()
            .is_empty()
    );
}

/// A proposer that offers `per_round` in every round it is asked, and records the rounds.
struct Greedy {
    per_round: Vec<Vec<Vec<u8>>>,
    asked: Vec<u8>,
}

impl Proposer for Greedy {
    fn id(&self) -> ProposerId {
        ProposerId::new("greedy", 1, b"hostile")
    }

    fn propose(&mut self, round: u8, _feedback: &[TrainFeedback]) -> Vec<Vec<u8>> {
        self.asked.push(round);
        self.per_round
            .get(usize::from(round))
            .cloned()
            .unwrap_or_else(|| vec![SHIPPED_PLAN.encode().to_vec()])
    }
}

/// `n` distinct 12-byte strings no plan decoder accepts (format byte 0).
fn junk(n: usize, salt: u8) -> Vec<Vec<u8>> {
    (0..n)
        .map(|i| {
            let mut b = vec![0u8; PLAN_LEN];
            b[1] = salt;
            b[4..12].copy_from_slice(&u64::try_from(i).expect("small").to_le_bytes());
            b
        })
        .collect()
}

/// The evaluator owns the budgets: oversized proposals are dropped, duplicates skipped, judging stops at 256 distinct candidates even for a grid plan, and only four rounds are asked.
#[test]
fn the_budgets_are_the_evaluators() {
    let ev = evaluator();
    let mut r0 = vec![vec![1u8; MAX_PROPOSAL_BYTES + 1], vec![0u8; 1 << 20]];
    r0.extend(junk(200, 1));
    r0.extend(junk(10, 1));
    let mut r1 = junk(100, 2);
    r1.push(vec![9u8; MAX_PROPOSAL_BYTES]);
    let r2 = GridProposer::plans();
    let mut g = Greedy {
        per_round: vec![r0, r1, r2, Vec::new(), junk(5, 3)],
        asked: Vec::new(),
    };
    let r = ev.select(&mut g).expect_err("no outcome");
    assert_eq!(
        g.asked,
        (0..MAX_ROUNDS).collect::<Vec<_>>(),
        "four rounds, no more"
    );
    assert_eq!(
        r,
        NoOutcome::NoneAdmitted {
            stats: SearchStats {
                rounds: MAX_ROUNDS,
                proposed: 212 + 101 + 54,
                oversized: 2,
                duplicates: 10,
                over_budget: 101 + 54 - 56,
                distinct: u64::try_from(MAX_DISTINCT).expect("small"),
                admitted: 0,
            }
        }
    );
}

/// A proposer is shown one training row per judged candidate, and none for an oversized proposal or a duplicate.
#[test]
fn feedback_is_one_training_row_per_judged_candidate() {
    struct Watch {
        shown: Vec<Vec<TrainFeedback>>,
    }
    impl Proposer for Watch {
        fn id(&self) -> ProposerId {
            ProposerId::new("watch", 1, &[])
        }
        fn propose(&mut self, round: u8, feedback: &[TrainFeedback]) -> Vec<Vec<u8>> {
            self.shown.push(feedback.to_vec());
            match round {
                0 => vec![
                    vec![0u8; MAX_PROPOSAL_BYTES + 1],
                    b"not a plan".to_vec(),
                    b"not a plan".to_vec(),
                ],
                1 => vec![super::plan::DEMO_PLAN.encode().to_vec()],
                _ => Vec::new(),
            }
        }
    }
    let mut w = Watch { shown: Vec::new() };
    let r = evaluator().select(&mut w).expect_err("no outcome");
    assert!(matches!(r, NoOutcome::NoneAdmitted { .. }), "{r:?}");
    assert_eq!(w.shown.len(), usize::from(MAX_ROUNDS));
    assert!(w.shown[0].is_empty());
    assert_eq!(
        w.shown[1].len(),
        1,
        "the oversized one and the duplicate get no row"
    );
    assert_eq!(w.shown[1][0].plan(), candidate_id(b"not a plan"));
    assert_eq!(
        (
            w.shown[1][0].is_admitted(),
            w.shown[1][0].law(),
            w.shown[1][0].ticks()
        ),
        (false, Some(PLAN_REFUSED), None)
    );
    let demo = &w.shown[2][1];
    assert_eq!(demo.plan(), super::plan::DEMO_PLAN.digest());
    assert_eq!(demo.law(), Some(super::HOT_HAS_WAKE));
    assert_eq!(w.shown[3], w.shown[2]);
    assert_eq!(
        candidate_id(&SHIPPED_PLAN.encode()),
        Digest::of(PLAN_DOMAIN, &[&SHIPPED_PLAN.encode()])
    );
}

/// A hostile proposer that reorders, pads and repeats the grid and chases its feedback gains nothing: the evaluator selects the grid's winner with the grid's rows, sums and gain.
#[test]
fn a_hostile_proposer_gains_nothing() {
    struct Hostile {
        rounds: Vec<Vec<Vec<u8>>>,
    }
    impl Proposer for Hostile {
        fn id(&self) -> ProposerId {
            ProposerId::new("hostile", 7, b"reads what it can")
        }
        fn propose(&mut self, round: u8, feedback: &[TrainFeedback]) -> Vec<Vec<u8>> {
            let mut out = vec![vec![0xffu8; MAX_PROPOSAL_BYTES + 1], b"#".repeat(12)];
            for earlier in &self.rounds[..usize::from(round)] {
                out.extend(earlier.iter().cloned());
            }
            out.extend(self.rounds[usize::from(round)].iter().cloned());
            // The best it was told about, by the only number it is shown.
            if let Some(best) = feedback
                .iter()
                .filter(|f| f.is_admitted())
                .min_by_key(|f| f.ticks())
            {
                let grid = GridProposer::plans();
                if let Some(p) = grid.iter().find(|p| candidate_id(p) == best.plan()) {
                    out.push(p.clone());
                }
            }
            out
        }
    }
    let mut backwards = GridProposer::plans();
    backwards.reverse();
    let rounds: Vec<Vec<Vec<u8>>> = backwards.chunks(14).map(<[Vec<u8>]>::to_vec).collect();
    assert_eq!(rounds.len(), usize::from(MAX_ROUNDS));
    let mut h = Hostile { rounds };
    let ev = evaluator();
    let hostile = ev.search(&mut h).expect("an outcome");
    let grid = &searched().admitted;
    assert_eq!(hostile.plan(), grid.plan());
    assert_eq!(hostile.useful(), grid.useful());
    assert_eq!(hostile.gain(), grid.gain());
    let (p, q) = (hostile.bundle().parts(), grid.bundle().parts());
    assert_eq!(p.rows, q.rows);
    assert_eq!(
        (p.selection.train, p.selection.held_out, p.selection.s0),
        (q.selection.train, q.selection.held_out, q.selection.s0)
    );
    assert_eq!(p.selection.mode, Mode::Search);
    assert_eq!(p.proposer.name, "hostile");
    let st = p.selection.stats;
    assert_eq!(
        (st.distinct, st.admitted),
        (55, 18),
        "54 plans and one malformed string"
    );
    assert_eq!(st.oversized, 4);
    assert_eq!(st.over_budget, 0);
    assert_eq!(
        st.proposed,
        st.oversized + st.duplicates + st.over_budget + st.distinct
    );
}
