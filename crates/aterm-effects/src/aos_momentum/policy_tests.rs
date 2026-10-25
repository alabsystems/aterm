// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The acceptance policy v1: its pin, baseline, corpus drift, bases, grid, budgets, and the
//! slack `NoStepSlept` leaves unjudged.

use std::collections::BTreeSet;
use std::time::Duration;

use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::NoEffects;
use aterm_aos::replay::{Run, replay};
use aterm_aos::{ArtifactKind, Digest};
use aterm_time::Instant;

use super::corpus::{
    Corpus, CorpusSplit, FRAME_INTERVALS_NS, GOLDEN_TRACE, GOLDEN_TRACE_TARGET, ItemKind, Stratum,
};
use super::fidelity::{LaneFidelity, PROBE_DELTA};
use super::lane::{Scenario, run};
use super::plan::{DEMO_PLAN, SHIPPED_PLAN, SchedulePlan, Split, Wake};
use super::policy::{
    AcceptancePolicy, BASES_V1, Baseline, BaselineField, Body, Budgets, CORPUS_FOLD,
    CORPUS_V1_DIGEST, Field, GRADE_CAP, GRID_SPLITS, GRID_V1, GRID_WAKES, HELD_OUT_COUNT, ITEMS_V1,
    ItemFacts, ItemPin, M4_POLICY_V1_DIGEST, MAX_DISTINCT, MAX_PROPOSAL_BYTES, MAX_ROUNDS,
    Measured, POLICY_NAME, PolicyRefusal, Replace, TRAIN_COUNT, Totals, measure,
};
use super::test_fixtures::{attached_for_lane, hosted};
use super::{
    DEMO_CODE_NAME, MomentumCode, MomentumEvent, MomentumHost, MomentumObservation, MomentumSchema,
    MomentumTrace, NoStrandedLight, SETTLE_MARGIN, SHIPPED_CODE_NAME, Sched,
    WAKE_NO_LATER_THAN_CLEAR, target_name,
};
use aterm_aos::evidence::EvidenceGrade;

fn corpus() -> Corpus {
    Corpus::generate().expect("generator v1's scenarios are admitted")
}

fn facts(c: &Corpus) -> Vec<ItemFacts<'_>> {
    c.items().iter().map(ItemFacts::of).collect()
}

fn pinned_rows() -> Vec<Measured> {
    ITEMS_V1
        .iter()
        .map(|p| Measured {
            id: p.id().to_owned(),
            baseline: p.baseline(),
            admitted: true,
        })
        .collect()
}

/// A baseline row as [`ITEMS_V1`] spells it: `T`, `P`, and `Z` grouped by thousands, the lane
/// digest by 16 bits, and the chain's hex.
fn pin_spelling(b: &Baseline) -> String {
    let n = |v: u64| {
        let d = v.to_string();
        let mut g = String::new();
        for (k, c) in d.chars().enumerate() {
            if k > 0 && (d.len() - k).is_multiple_of(3) {
                g.push('_');
            }
            g.push(c);
        }
        g
    };
    let lane = b.lane.map_or_else(
        || "None".to_owned(),
        |l| {
            let w = |k: u32| (l >> (16 * k)) & 0xffff;
            format!(
                "Some(0x{:04x}_{:04x}_{:04x}_{:04x})",
                w(3),
                w(2),
                w(1),
                w(0)
            )
        },
    );
    format!(
        "({}, {}, {}), {lane}, \"{}\"",
        n(b.ticks),
        n(b.presented),
        n(b.wasted),
        b.chain.hex()
    )
}

const ROWS: &str =
    "the measured rows are not the policy's items: a new policy version (design §10.4)";
const FAULT: &str = "the shipped code is not admitted on a lane row, which no pin can take: fix what broke its admission (design §10.4)";
const GOLDEN: &str = "the shipped code is not admitted on the golden trace, and no v1 re-pin can take that: a change meant to move its recorded head needs a new trace and a new policy version; otherwise fix what broke its admission (design §10.4)";
const VERSION: &str = "the fixed baseline moved: a new policy version (design §10.4)";
const REPIN: &str = "only lane digests or chains moved: a v1 re-pin may take the measured values (ITEMS_V1's doc; for s0's lane digest also M3_STORY_DIGEST in lane_tests.rs and the_baseline_is_pinned's copy), then the digest the_policy_is_pinned prints";

/// The remedy that comes first for the change from `pins` to `measured` in `check_baseline`'s
/// order of classes, or `None` when nothing moved.
fn verdict(measured: &[Measured], pins: &[Measured]) -> Option<&'static str> {
    let pairs = || measured.iter().zip(pins);
    let fixed = |b: &Baseline, q: &Baseline| {
        (b.ticks, b.presented, b.wasted) != (q.ticks, q.presented, q.wasted)
    };
    let lane = |m: &Measured| {
        ITEMS_V1
            .iter()
            .any(|p| p.id() == m.id && p.kind() != ItemKind::GoldenTrace)
    };
    if measured == pins {
        None
    } else if measured.len() != pins.len() || pairs().any(|(m, p)| m.id != p.id) {
        Some(ROWS)
    } else if pairs().any(|(m, _)| !m.admitted && lane(m)) {
        Some(FAULT)
    } else if pairs().any(|(m, _)| !m.admitted) {
        Some(GOLDEN)
    } else if pairs().any(|(m, p)| fixed(&m.baseline, &p.baseline)) {
        Some(VERSION)
    } else {
        Some(REPIN)
    }
}

const fn remedy(field: BaselineField) -> &'static str {
    match field {
        BaselineField::Row => ROWS,
        BaselineField::Admitted => FAULT,
        BaselineField::GoldenHead => GOLDEN,
        BaselineField::Ticks | BaselineField::Presented | BaselineField::Wasted => VERSION,
        BaselineField::LaneDigest | BaselineField::Chain => REPIN,
    }
}

/// `the_baseline_is_pinned`'s report: a line per differing, extra, or missing row, followed by
/// the first applicable remedy.
fn moved_rows(measured: &[Measured], pins: &[Measured]) -> Vec<String> {
    let moved = |m: &Measured, p: &Measured| {
        let (b, q) = (&m.baseline, &p.baseline);
        [
            (m.id != p.id, "id"),
            (!m.admitted, "admission"),
            (b.ticks != q.ticks, "T"),
            (b.presented != q.presented, "P"),
            (b.wasted != q.wasted, "Z"),
            (b.lane != q.lane, "lane digest"),
            (b.chain != q.chain, "chain"),
        ]
        .into_iter()
        .filter_map(|(differs, what)| differs.then_some(what))
        .collect::<Vec<_>>()
        .join(", ")
    };
    let mut lines: Vec<String> = measured
        .iter()
        .zip(pins)
        .filter(|(m, p)| m != p)
        .map(|(m, p)| {
            format!(
                "{}: measured {} (admitted {}), pinned {} as {}; moved: {}",
                m.id,
                pin_spelling(&m.baseline),
                m.admitted,
                pin_spelling(&p.baseline),
                p.id,
                moved(m, p)
            )
        })
        .collect();
    lines.extend(measured.iter().skip(pins.len()).map(|m| {
        format!(
            "{}: measured {} (admitted {}), no pin",
            m.id,
            pin_spelling(&m.baseline),
            m.admitted
        )
    }));
    lines.extend(pins.iter().skip(measured.len()).map(|p| {
        format!(
            "{}: not measured, pinned {}",
            p.id,
            pin_spelling(&p.baseline)
        )
    }));
    lines.extend(verdict(measured, pins).map(|v| format!("remedy: {v}")));
    lines
}

fn corpus_drift(item: &str) -> Result<(), PolicyRefusal> {
    Err(PolicyRefusal::CorpusDrift {
        item: item.to_owned(),
    })
}

fn law(v: &Verdict) -> Option<&'static str> {
    match v {
        Verdict::Pass => None,
        Verdict::Fail { law, .. } => Some(law),
    }
}

#[test]
fn the_policy_is_pinned() {
    let t0 = Instant::now();
    let v1 = AcceptancePolicy::v1();
    assert_eq!(
        v1,
        AcceptancePolicy::v1(),
        "the policy is a function of the source"
    );
    let id = v1.id();
    assert_eq!(id.kind, ArtifactKind::Policy);
    assert_eq!((id.name.as_str(), id.version), (POLICY_NAME, 1));
    assert_eq!(POLICY_NAME, "momentum-glow.m4-acceptance");
    println!("momentum-glow.m4-acceptance v1: {}", id.digest.hex());
    if target_name() == GOLDEN_TRACE_TARGET {
        assert_eq!(
            id.digest, M4_POLICY_V1_DIGEST,
            "the policy is pinned in source; re-pinning is a reviewed commit"
        );
        assert_eq!(v1.check_pin(), Ok(()));
        assert_eq!(AcceptancePolicy::pinned(t0), Ok(v1));
    } else {
        assert_eq!(
            AcceptancePolicy::pinned(t0),
            Err(PolicyRefusal::PolicyMutated {
                found: id.digest,
                pinned: M4_POLICY_V1_DIGEST,
            }),
            "another target's profile is another policy"
        );
    }
}

#[test]
fn a_mutated_policy_is_refused_field_by_field() {
    let v1 = AcceptancePolicy::v1();
    let mut seen = BTreeSet::from([v1.id().digest]);
    for field in Field::ALL {
        let m = AcceptancePolicy::mutated(field);
        let found = m.id().digest;
        assert_ne!(
            found,
            v1.id().digest,
            "{field:?} is bound by the descriptor"
        );
        assert!(seen.insert(found), "{field:?} moves the digest its own way");
        assert_eq!(
            m.check_pin(),
            Err(PolicyRefusal::PolicyMutated {
                found,
                pinned: M4_POLICY_V1_DIGEST,
            }),
            "{field:?}"
        );
    }
    assert_eq!(seen.len(), Field::ALL.len() + 1);
}

type Move = fn(&mut Measured);

#[test]
fn the_baseline_is_pinned() {
    let t0 = Instant::now();
    let c = corpus();
    let facts = facts(&c);
    let measured = measure(&facts, t0);
    assert_eq!(
        measure(&facts, t0 + Duration::from_secs(13)),
        measured,
        "the origin changes nothing"
    );
    let pins = pinned_rows();
    let moved = moved_rows(&measured, &pins);
    assert!(
        moved.is_empty(),
        "the measured baseline is not the pin ({} rows measured, {} pinned):\n{}",
        measured.len(),
        pins.len(),
        moved.join("\n")
    );
    let v1 = AcceptancePolicy::v1();
    assert_eq!(v1.check_baseline(&measured), Ok(()));

    for p in ITEMS_V1 {
        let b = p.baseline();
        assert_eq!(b.wasted, b.ticks - b.presented, "{}: Z = T - P", p.id());
        assert_eq!(
            b.lane.is_none(),
            p.kind() == ItemKind::GoldenTrace,
            "{}: only the golden trace runs no lane",
            p.id()
        );
    }
    let sum = |keep: &dyn Fn(&ItemPin) -> bool| {
        ITEMS_V1
            .iter()
            .filter(|p| keep(p))
            .fold((0u64, 0u64), |a, p| {
                (a.0 + p.baseline().ticks, a.1 + p.baseline().wasted)
            })
    };
    let generated_train =
        sum(&|p| p.split() == CorpusSplit::Train && matches!(p.kind(), ItemKind::Generated { .. }));
    assert_eq!(generated_train, (8_287, 2_151), "phase 2: train T, Z");
    assert_eq!(
        sum(&|p| p.split() == CorpusSplit::HeldOut),
        (6_685, 1_750),
        "phase 2: held-out T, Z"
    );
    let item = |id: &str| {
        ITEMS_V1
            .iter()
            .find(|p| p.id() == id)
            .expect("a pinned item")
            .baseline()
    };
    let s0 = item("s0");
    assert_eq!((s0.ticks, s0.presented, s0.wasted), (2_776, 2_480, 296));
    assert_eq!(
        s0.lane,
        Some(0xbf64_2e76_44df_7e95),
        "the M3 story's digest"
    );
    let golden = item("golden");
    assert_eq!(
        (golden.ticks, golden.presented, golden.wasted),
        (154, 130, 24)
    );
    let trace = MomentumTrace::decode(GOLDEN_TRACE).expect("the golden trace decodes");
    assert_eq!(golden.chain, trace.head_chain, "the recorded head's chain");
    assert_eq!(
        v1.totals(CorpusSplit::Train),
        Totals {
            ticks: 8_441,
            presented: 6_266,
            wasted: 2_175
        }
    );
    assert_eq!(
        v1.totals(CorpusSplit::HeldOut),
        Totals {
            ticks: 6_685,
            presented: 4_935,
            wasted: 1_750
        }
    );

    let k = ITEMS_V1
        .iter()
        .position(|p| p.id() == "s0")
        .expect("S0 is pinned");
    let moved: [(BaselineField, Move); 6] = [
        (BaselineField::Ticks, |m| m.baseline.ticks += 1),
        (BaselineField::Presented, |m| m.baseline.presented -= 1),
        (BaselineField::Wasted, |m| m.baseline.wasted += 1),
        (BaselineField::LaneDigest, |m| m.baseline.lane = None),
        (BaselineField::Chain, |m| m.baseline.chain.0[7] ^= 0x10),
        (BaselineField::Admitted, |m| m.admitted = false),
    ];
    for (field, change) in moved {
        let mut rows = measured.clone();
        change(&mut rows[k]);
        assert_eq!(
            v1.check_baseline(&rows),
            Err(PolicyRefusal::BaselineDrift {
                item: "s0".to_owned(),
                field
            }),
            "{}",
            field.name()
        );
    }
    let mut rows = measured.clone();
    rows[k].baseline.lane = None;
    rows[k].admitted = false;
    assert_eq!(
        v1.check_baseline(&rows),
        Err(PolicyRefusal::BaselineDrift {
            item: "s0".to_owned(),
            field: BaselineField::Admitted
        }),
        "admission is named before the re-pinnable lane digest"
    );
}

#[test]
fn a_drift_says_whether_a_re_pin_may_take_it() {
    use BaselineField::{Admitted, Chain, LaneDigest, Presented, Row, Ticks, Wasted};
    let moved = |name: &str, remedy: &str| {
        format!(
            "acceptance policy refused: the shipped baseline moved at s0 ({name}); {remedy} (`the_baseline_is_pinned` reports every moved row)"
        )
    };
    let repin = "no row moved anything but its lane digest or chain, so a v1 re-pin in a reviewed source commit may take the change";
    let version = "that is a new policy version";
    for (field, says) in [
        (
            Row,
            "acceptance policy refused: the measured rows are not the policy's items at s0; that is a new policy version (`the_baseline_is_pinned` reports every moved row)".to_owned(),
        ),
        (Ticks, moved("T", version)),
        (Presented, moved("P", version)),
        (Wasted, moved("Z", version)),
        (LaneDigest, moved("lane digest", repin)),
        (Chain, moved("chain", repin)),
        (
            Admitted,
            "acceptance policy refused: the shipped code is not admitted at s0, which no pin can take: fix what broke its admission (`the_baseline_is_pinned` reports every moved row)".to_owned(),
        ),
    ] {
        assert_eq!(field.repinnable(), matches!(field, LaneDigest | Chain), "{}", field.name());
        let refusal = PolicyRefusal::BaselineDrift {
            item: "s0".to_owned(),
            field,
        };
        assert_eq!(refusal.to_string(), says);
    }
    let golden = PolicyRefusal::BaselineDrift {
        item: "golden".to_owned(),
        field: BaselineField::GoldenHead,
    };
    assert!(
        !BaselineField::GoldenHead.repinnable(),
        "{}",
        BaselineField::GoldenHead.name()
    );
    assert_eq!(
        PolicyRefusal::CorpusDrift {
            item: "golden".to_owned()
        }
        .to_string(),
        "acceptance policy refused: the corpus moved at golden, which v1 binds: fix the change, or make a new policy version"
    );
    assert_eq!(
        golden.to_string(),
        "acceptance policy refused: the shipped code is not admitted at golden, the golden trace, and no v1 re-pin can take that: a change meant to move its recorded head needs a new trace and a new policy version; otherwise fix what broke its admission (`the_baseline_is_pinned` reports every moved row)"
    );
}

#[test]
fn the_baseline_report_names_each_moved_row_and_its_rule() {
    let pins = pinned_rows();
    let at = |id: &str| pins.iter().position(|p| p.id == id).expect("a pinned item");
    let chain = pins[at("s0")].baseline.chain;
    let row = Baseline {
        ticks: 1_234_567,
        presented: 2_480,
        wasted: 96,
        lane: Some(0x0123_4567_89ab_cdef),
        chain,
    };
    assert_eq!(
        pin_spelling(&row),
        format!(
            "(1_234_567, 2_480, 96), Some(0x0123_4567_89ab_cdef), \"{}\"",
            chain.hex()
        )
    );
    let row = Baseline {
        ticks: 154,
        presented: 130,
        wasted: 24,
        lane: None,
        chain,
    };
    assert_eq!(
        pin_spelling(&row),
        format!("(154, 130, 24), None, \"{}\"", chain.hex())
    );
    assert!(moved_rows(&pins, &pins).is_empty());
    let v1 = AcceptancePolicy::v1();
    assert_eq!(v1.check_baseline(&pins), Ok(()));

    assert_eq!(at("golden"), 0, "the cases' golden trace is row 0");
    type Change = fn(&mut Vec<Measured>, usize, usize);
    let cases: [(&str, Change, &str, &str, BaselineField); 19] = [
        (
            "lane",
            |r, s, _| r[s].baseline.lane = Some(1),
            REPIN,
            "s0",
            BaselineField::LaneDigest,
        ),
        (
            "chain",
            |r, s, _| r[s].baseline.chain.0[0] ^= 1,
            REPIN,
            "s0",
            BaselineField::Chain,
        ),
        (
            "T",
            |r, s, _| r[s].baseline.ticks += 1,
            VERSION,
            "s0",
            BaselineField::Ticks,
        ),
        (
            "Z",
            |r, s, _| r[s].baseline.wasted += 1,
            VERSION,
            "s0",
            BaselineField::Wasted,
        ),
        (
            "admission",
            |r, s, _| r[s].admitted = false,
            FAULT,
            "s0",
            BaselineField::Admitted,
        ),
        (
            "id",
            |r, s, _| r[s].id.push('x'),
            ROWS,
            "s0",
            BaselineField::Row,
        ),
        (
            "lane, then T",
            |r, s, h| {
                r[s].baseline.lane = Some(1);
                r[h].baseline.ticks += 1;
            },
            VERSION,
            "held-101",
            BaselineField::Ticks,
        ),
        (
            "T, then admission",
            |r, s, h| {
                r[s].baseline.ticks += 1;
                r[h].admitted = false;
            },
            FAULT,
            "held-101",
            BaselineField::Admitted,
        ),
        (
            "lane, then chain",
            |r, s, h| {
                r[s].baseline.lane = Some(1);
                r[h].baseline.chain.0[0] ^= 1;
            },
            REPIN,
            "s0",
            BaselineField::LaneDigest,
        ),
        (
            "unmeasured",
            |r, s, _| {
                r[s].baseline.ticks = 0;
                r[s].admitted = false;
            },
            FAULT,
            "s0",
            BaselineField::Admitted,
        ),
        (
            "T, then another id",
            |r, s, h| {
                r[s].baseline.ticks += 1;
                r[h].id.push('x');
            },
            ROWS,
            "held-101",
            BaselineField::Row,
        ),
        (
            "lane, and an extra row",
            |r, s, h| {
                r[s].baseline.lane = Some(1);
                let mut extra = r[h].clone();
                extra.id = "extra".to_owned();
                r.push(extra);
            },
            ROWS,
            "extra",
            BaselineField::Row,
        ),
        (
            "lane, and the last row missing",
            |r, s, _| {
                r[s].baseline.lane = Some(1);
                r.pop();
            },
            ROWS,
            "held-108",
            BaselineField::Row,
        ),
        (
            "admission, then another id",
            |r, s, h| {
                r[s].admitted = false;
                r[h].id.push('x');
            },
            ROWS,
            "held-101",
            BaselineField::Row,
        ),
        (
            "admission, and an extra row",
            |r, s, h| {
                r[s].admitted = false;
                let mut extra = r[h].clone();
                extra.id = "extra".to_owned();
                r.push(extra);
            },
            ROWS,
            "extra",
            BaselineField::Row,
        ),
        (
            "admission, and the last row missing",
            |r, s, _| {
                r[s].admitted = false;
                r.pop();
            },
            ROWS,
            "held-108",
            BaselineField::Row,
        ),
        (
            "golden's frames",
            |r, _, _| {
                r[0].admitted = false;
                r[0].baseline.ticks += 1;
                r[0].baseline.chain.0[0] ^= 1;
            },
            GOLDEN,
            "golden",
            BaselineField::GoldenHead,
        ),
        (
            "golden's admission, then a lane's",
            |r, s, _| {
                r[0].admitted = false;
                r[s].admitted = false;
            },
            FAULT,
            "s0",
            BaselineField::Admitted,
        ),
        (
            "golden's admission, then T",
            |r, _, h| {
                r[0].admitted = false;
                r[h].baseline.ticks += 1;
            },
            GOLDEN,
            "golden",
            BaselineField::GoldenHead,
        ),
    ];
    for (what, change, remedied, item, field) in cases {
        let mut rows = pins.clone();
        change(&mut rows, at("s0"), at("held-101"));
        assert_eq!(verdict(&rows, &pins), Some(remedied), "{what}");
        assert_eq!(
            v1.check_baseline(&rows),
            Err(PolicyRefusal::BaselineDrift {
                item: item.to_owned(),
                field
            }),
            "{what}"
        );
        assert_eq!(
            remedy(field),
            remedied,
            "{what}: the refusal's remedy is the report's"
        );
        let report = moved_rows(&rows, &pins);
        assert_eq!(
            report.last(),
            Some(&format!("remedy: {remedied}")),
            "{what}"
        );
    }

    let mut rows = pins.clone();
    rows[at("s0")].baseline.lane = Some(1);
    rows[at("held-101")].baseline.ticks += 1;
    let report = moved_rows(&rows, &pins);
    assert_eq!(report.len(), 3, "{report:?}");
    assert!(
        report[0].starts_with("s0: ") && report[0].ends_with("; moved: lane digest"),
        "{}",
        report[0]
    );
    assert!(
        report[1].starts_with("held-101: ") && report[1].ends_with("; moved: T"),
        "{}",
        report[1]
    );
    let mut rows = pins.clone();
    let s0 = &mut rows[at("s0")];
    s0.baseline.ticks = 0;
    s0.admitted = false;
    let report = moved_rows(&rows, &pins);
    assert!(
        report[0].contains("(admitted false)")
            && report[0].ends_with(" as s0; moved: admission, T"),
        "{}",
        report[0]
    );
    let mut rows = pins.clone();
    let s0 = &mut rows[at("s0")];
    s0.id.push('x');
    s0.admitted = false;
    s0.baseline.ticks += 1;
    s0.baseline.presented += 1;
    s0.baseline.wasted += 1;
    s0.baseline.lane = Some(1);
    s0.baseline.chain.0[0] ^= 1;
    let report = moved_rows(&rows, &pins);
    assert!(
        report[0].ends_with(" as s0; moved: id, admission, T, P, Z, lane digest, chain"),
        "{}",
        report[0]
    );
    let mut longer = pins.clone();
    longer.push(pins[at("s0")].clone());
    let report = moved_rows(&longer, &pins);
    assert_eq!(report.len(), 2);
    assert!(
        report[0].starts_with("s0: measured (2_776, 2_480, 296)") && report[0].ends_with("no pin"),
        "{}",
        report[0]
    );
    let report = moved_rows(&pins[..pins.len() - 1], &pins);
    assert_eq!(report.len(), 2);
    assert!(
        report[0].starts_with("held-108: not measured, pinned"),
        "{}",
        report[0]
    );
}

#[test]
fn a_damaged_golden_trace_is_corpus_drift() {
    let t0 = Instant::now();
    let c = corpus();
    let v1 = AcceptancePolicy::v1();
    let good = facts(&c);
    assert_eq!(v1.check_corpus(&good), Ok(()));
    assert_eq!(good[0].id, "golden");
    let n = GOLDEN_TRACE.len();
    let mut damaged: Vec<Vec<u8>> = [0, 9, n / 2, n - 1]
        .into_iter()
        .map(|at| {
            let mut b = GOLDEN_TRACE.to_vec();
            b[at] ^= 0x01;
            b
        })
        .collect();
    damaged.extend([n - 1, n / 2, 0].map(|len| GOLDEN_TRACE[..len].to_vec()));
    for bytes in &damaged {
        let mut f = good.clone();
        f[0].body = Body::Trace(bytes);
        assert_eq!(
            v1.check_corpus(&f),
            corpus_drift("golden"),
            "{} bytes",
            bytes.len()
        );
        assert!(MomentumTrace::decode(bytes).is_err(), "not a trace");
        let mut rows = pinned_rows();
        rows[0] = measure(&f[..1], t0).remove(0);
        assert_eq!(
            v1.check_baseline(&rows),
            Err(PolicyRefusal::BaselineDrift {
                item: "golden".to_owned(),
                field: BaselineField::GoldenHead
            }),
            "the baseline on a damaged trace is drift"
        );
    }
}

#[test]
fn a_changed_scenario_is_corpus_drift() {
    let c = corpus();
    let v1 = AcceptancePolicy::v1();
    let good = facts(&c);
    let lane: Vec<(usize, &Scenario)> = good
        .iter()
        .enumerate()
        .filter_map(|(k, f)| match f.body {
            Body::Lane(s) => Some((k, s)),
            Body::Trace(_) => None,
        })
        .collect();
    assert_eq!(lane.len(), 25);
    let later: Vec<Scenario> = lane
        .iter()
        .map(|(_, s)| {
            Scenario::new(
                s.frame_interval_ns(),
                *s.start(),
                s.events().to_vec(),
                s.end_ns() + 1,
            )
            .expect("admitted")
        })
        .collect();
    for (j, &(k, _)) in lane.iter().enumerate() {
        let id = good[k].id;
        let mut f = good.clone();
        f[k].body = Body::Lane(&later[j]);
        assert_eq!(v1.check_corpus(&f), corpus_drift(id), "{id}: a later end");
        let other = lane[(j + 1) % lane.len()].1;
        let mut f = good.clone();
        f[k].body = Body::Lane(other);
        assert_eq!(
            v1.check_corpus(&f),
            corpus_drift(id),
            "{id}: another scenario"
        );
    }
    let k = 3;
    let id = good[k].id;
    let mut f = good.clone();
    f[k].id = "train-99";
    assert_eq!(v1.check_corpus(&f), corpus_drift(id), "another id");
    let mut f = good.clone();
    f[k].kind = ItemKind::Generated {
        seed: 3,
        stratum: Stratum::Plain,
    };
    assert_eq!(v1.check_corpus(&f), corpus_drift(id), "another kind");
    let mut f = good.clone();
    f[k].split = CorpusSplit::HeldOut;
    assert_eq!(v1.check_corpus(&f), corpus_drift(id), "another split");
}

#[test]
fn every_item_counts() {
    let c = corpus();
    let v1 = AcceptancePolicy::v1();
    assert_eq!((TRAIN_COUNT, HELD_OUT_COUNT), (17, 9));
    assert_eq!(ITEMS_V1.len(), 26);
    assert_eq!(v1.items(), ITEMS_V1);
    let count = |split| ITEMS_V1.iter().filter(|p| p.split() == split).count();
    assert_eq!(
        (count(CorpusSplit::Train), count(CorpusSplit::HeldOut)),
        (17, 9)
    );
    assert_eq!(ITEMS_V1[0].kind(), ItemKind::GoldenTrace);
    let good = facts(&c);
    assert_eq!(good.len(), 26);
    for (f, p) in good.iter().zip(ITEMS_V1) {
        assert_eq!((f.id, f.kind, f.split), (p.id(), p.kind(), p.split()));
    }
    let rows = pinned_rows();
    assert_eq!(v1.check_baseline(&rows), Ok(()));
    for (k, p) in ITEMS_V1.iter().enumerate() {
        let mut f = good.clone();
        f.remove(k);
        assert_eq!(
            v1.check_corpus(&f),
            corpus_drift(p.id()),
            "{} dropped",
            p.id()
        );
        let mut r = rows.clone();
        r.remove(k);
        assert_eq!(
            v1.check_baseline(&r),
            Err(PolicyRefusal::BaselineDrift {
                item: p.id().to_owned(),
                field: BaselineField::Row
            }),
            "{} dropped from the baseline",
            p.id()
        );
    }
    let mut f = good.clone();
    f.push(good[4]);
    assert_eq!(
        v1.check_corpus(&f),
        corpus_drift(good[4].id),
        "an extra item"
    );
    let mut r = rows.clone();
    r.push(rows[4].clone());
    assert_eq!(
        v1.check_baseline(&r),
        Err(PolicyRefusal::BaselineDrift {
            item: rows[4].id.clone(),
            field: BaselineField::Row
        })
    );
    assert_eq!(v1.check_corpus(&[]), corpus_drift("golden"));
    assert_ne!(CORPUS_FOLD, "golden");
    assert_eq!(c.digest(), CORPUS_V1_DIGEST, "the corpus digest is the pin");
}

#[test]
fn shipped_and_the_demo_code_are_the_only_bases() {
    let v1 = AcceptancePolicy::v1();
    assert_eq!(
        BASES_V1,
        [
            (SHIPPED_CODE_NAME, Replace::Optimization),
            (DEMO_CODE_NAME, Replace::Repair)
        ]
    );
    assert_eq!(v1.bases(), &BASES_V1);
    assert_eq!(
        MomentumCode::ALL,
        &[
            MomentumCode::Shipped,
            MomentumCode::Safe,
            MomentumCode::DemoStrandedSettle
        ]
    );
    assert_eq!(
        v1.base(Sched::Prebuilt(MomentumCode::Shipped)),
        Some(Replace::Optimization)
    );
    assert_eq!(
        v1.base(Sched::Prebuilt(MomentumCode::Safe)),
        None,
        "the safe code is no base"
    );
    assert_eq!(
        v1.base(Sched::Prebuilt(MomentumCode::DemoStrandedSettle)),
        Some(Replace::Repair)
    );
    assert_eq!(MomentumCode::Shipped.artifact_name(), SHIPPED_CODE_NAME);
    assert_eq!(
        MomentumCode::DemoStrandedSettle.artifact_name(),
        DEMO_CODE_NAME
    );
    let mut plans: Vec<SchedulePlan> = v1.grid().to_vec();
    plans.extend([SHIPPED_PLAN, DEMO_PLAN]);
    for p in plans {
        assert_eq!(v1.base(Sched::Plan(p)), None, "{p:?} is no base");
    }
    assert_ne!(Replace::Optimization.tag(), Replace::Repair.tag());
    assert_ne!(Replace::Optimization.rule(), Replace::Repair.rule());
}

#[test]
fn the_grid_and_the_budgets_are_v1() {
    let v1 = AcceptancePolicy::v1();
    assert_eq!(GRID_V1.len(), 54);
    assert_eq!(GRID_SPLITS.len() * GRID_WAKES.len(), 54);
    let grid: Vec<SchedulePlan> = GRID_V1
        .iter()
        .map(|&(s, w)| SchedulePlan::new(s, w).expect("every grid plan decodes"))
        .collect();
    assert_eq!(v1.grid(), grid.as_slice());
    for (i, p) in grid.iter().enumerate() {
        assert_eq!(
            (p.split(), p.wake()),
            (GRID_SPLITS[i / 6], GRID_WAKES[i % 6])
        );
    }
    let digests: BTreeSet<Digest> = grid.iter().map(SchedulePlan::digest).collect();
    assert_eq!(digests.len(), 54);
    assert_eq!(grid[0], SHIPPED_PLAN);
    assert_eq!(grid[5], DEMO_PLAN);
    assert_eq!(
        v1.budgets(),
        Budgets {
            rounds: 4,
            distinct: 256,
            proposal_bytes: 64
        }
    );
    assert_eq!((MAX_ROUNDS, MAX_DISTINCT, MAX_PROPOSAL_BYTES), (4, 256, 64));
    assert_eq!(v1.grade_cap(), EvidenceGrade::Tested);
    assert_eq!(GRADE_CAP, EvidenceGrade::Tested);
}

/// `plan` attached for a whole lane run of `s`: lane fidelity's verdict, `NoStrandedLight`'s
/// verdict on the replayed log, the replay, and its inputs.
fn judged(
    t0: Instant,
    plan: SchedulePlan,
    s: &Scenario,
) -> (Verdict, Verdict, Run<MomentumSchema>, Vec<MomentumEvent>) {
    let mut h: MomentumHost = attached_for_lane(t0, Sched::Plan(plan));
    let r = run(&mut h, s, t0);
    let fidelity = LaneFidelity::new().judge(&r);
    let b = &hosted(&h).boundary;
    let checkpoint = b.checkpoint().expect("fixture: attached");
    let log = b.log().expect("fixture: attached");
    let start = checkpoint
        .restore(b.schema(), b.identity())
        .expect("the ancestor restores");
    let replayed = replay(
        b.schema(),
        &b.pair().code,
        start,
        log.inputs(),
        log.header().first_cursor,
        &mut NoEffects,
    );
    let verdict = NoStrandedLight::new().judge(b.schema(), log.inputs(), &replayed);
    (fidelity, verdict, replayed, log.inputs().to_vec())
}

/// Whether `v` fails `WakeNoLaterThanClear` at a settle wake after
/// `max(cold_at + margin, at + frame_interval)`.
fn wakes_past_the_clear(v: &Verdict, run: &Run<MomentumSchema>, inputs: &[MomentumEvent]) -> bool {
    let Verdict::Fail { cursor, law, .. } = v else {
        return false;
    };
    if *law != WAKE_NO_LATER_THAN_CLEAR {
        return false;
    }
    let Some(i) = run.observations.iter().position(|(c, _)| c == cursor) else {
        return false;
    };
    let margin = i128::try_from(SETTLE_MARGIN.as_nanos()).unwrap_or(i128::MAX);
    match (&run.observations[i].1, inputs.get(i)) {
        (
            MomentumObservation::Schedule {
                settling: true,
                cold_at,
                deadline: Some(d),
                ..
            },
            Some(MomentumEvent::Query { at, frame_interval }),
        ) => {
            let floor = at + i128::try_from(frame_interval.as_nanos()).unwrap_or(i128::MAX);
            *d > floor && cold_at.is_none_or(|c| *d > c + margin)
        }
        _ => false,
    }
}

#[test]
fn a_poll_just_past_a_frame_is_refused() {
    let t0 = Instant::now();
    let c = corpus();
    let delta = u64::try_from(PROBE_DELTA.as_nanos()).unwrap_or(0);
    let mut plans = Vec::new();
    for f in FRAME_INTERVALS_NS {
        let f_us = u32::try_from(f / 1_000).expect("a frame is under a second");
        for q in [f_us + 1, f_us + 2_000] {
            let q_ns = u64::from(q) * 1_000;
            assert!(
                q_ns > f && q_ns <= f + delta,
                "{q} us is in the slack of {f} ns"
            );
            for split in [Split::SettleAtStep { gap_us: 0 }, Split::Shipped] {
                let p = SchedulePlan::new(split, Wake::Poll { period_us: q }).expect("in range");
                plans.push((f, p));
            }
        }
    }
    let items: Vec<(&str, &Scenario)> = c
        .split(CorpusSplit::Train)
        .filter_map(|i| i.scenario().map(|s| (i.id(), s)))
        .collect();
    let jobs: Vec<(usize, usize)> = plans
        .iter()
        .enumerate()
        .flat_map(|(p, &(f, _))| {
            items
                .iter()
                .enumerate()
                .filter(move |(_, (_, s))| s.frame_interval_ns() == f)
                .map(move |(i, _)| (p, i))
        })
        .collect();
    let results = crate::test_pool::par_map(&jobs, |&(p, i)| {
        let (fidelity, verdict, replayed, inputs) = judged(t0, plans[p].1, items[i].1);
        let past = wakes_past_the_clear(&verdict, &replayed, &inputs);
        assert!(
            past || law(&verdict) != Some(WAKE_NO_LATER_THAN_CLEAR),
            "{:?} {}: a WakeNoLaterThanClear failure is a wake past the clear",
            plans[p].1,
            items[i].0
        );
        (p, i, fidelity.passed(), past)
    });
    assert_eq!(results.len(), jobs.len());
    for (p, &(f, plan)) in plans.iter().enumerate() {
        let rows: Vec<_> = results.iter().filter(|r| r.0 == p).collect();
        assert!(
            !rows.is_empty(),
            "{f} ns: fixture: training items at the interval"
        );
        let refused: Vec<&str> = rows.iter().filter(|r| r.3).map(|r| items[r.1].0).collect();
        println!(
            "{:?} {:?} at {f} ns: lane fidelity {}/{}, WakeNoLaterThanClear past the clear on {refused:?}",
            plan.split(),
            plan.wake(),
            rows.iter().filter(|r| r.2).count(),
            rows.len()
        );
        assert!(
            rows.iter().all(|r| r.2),
            "{plan:?}: the slack is unjudged by lane fidelity"
        );
        assert!(
            !refused.is_empty(),
            "{plan:?}: NoStrandedLight refuses a poll past a frame on a training item at {f} ns"
        );
    }
}

#[test]
fn an_unmeasured_row_is_no_pin() {
    let zero = Baseline {
        ticks: 0,
        presented: 0,
        wasted: 0,
        lane: None,
        chain: Digest([0; 32]),
    };
    assert!(ITEMS_V1.iter().all(|p| p.baseline() != zero));
    assert!(ITEMS_V1.iter().all(|p| p.content() != Digest([0; 32])));
}
