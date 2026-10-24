// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The M6 model-admission policy v1 (design §13.7, stage A): its pin, its terms, its featurizer, and the sets its sampler draws.

use std::collections::{BTreeMap, BTreeSet};

use aterm_aos::Digest;

use super::corpus::SplitMix;
use super::m6_policy::{
    BASE_DOMAIN, CONTROL_DOMAIN, CONTROL_ID_DOMAIN, FEATURES, Field, M6_POLICY_V1_DIGEST,
    M6_SETS_V1_DIGEST, MAX_DRAWS, PLANS_FILE_V1, POOL_DOMAIN, POOL_PLANS, POOLS, PoolId,
    PoolRefusal, RANKER_POLICY_NAME, RANKER_POLICY_VERSION, RankerPolicy, RankerRefusal,
    SETS_DOMAIN, SIDE_DOMAIN, SetId, SetTag, Side, TIE_DOMAIN, TRAIN_POOLS, TRAINER_DOMAIN,
    VERSION_DOMAIN, control_id, control_key, draw, draw_with, features, fill, high_mass, one_in,
    pool_seed, set_pools, set_pools_at, sets_digest, sets_v1, side, skipped, tie_key, trainer_seed,
};
use super::plan::{MAX_GAP_US, MAX_PERIOD_US, MIN_PERIOD_US, SchedulePlan, Split, Wake};
use super::policy::{self, AcceptancePolicy, GRID_V1, M4_POLICY_V1_DIGEST};

fn plan(split: Split, wake: Wake) -> SchedulePlan {
    SchedulePlan::new(split, wake).expect("a plan in range")
}

fn settle(gap_us: u32, wake: Wake) -> SchedulePlan {
    plan(Split::SettleAtStep { gap_us }, wake)
}

fn poll(period_us: u32) -> Wake {
    Wake::Poll { period_us }
}

const FREE_WAKES: [Wake; 4] = [Wake::NextStep, Wake::ColdOnly, Wake::Cadence, Wake::Never];

/// `[lo, hi]` of gap bucket `b` in `1..=20`, from the rule's text.
fn gap_edges(b: u32) -> (u32, u32) {
    (1 << (b - 1), ((1u32 << b) - 1).min(MAX_GAP_US))
}

/// `[lo, hi]` of period bucket `c` in `9..=19`, from the rule's text.
fn period_edges(c: u32) -> (u32, u32) {
    (
        (1u32 << c).max(MIN_PERIOD_US),
        ((1u32 << (c + 1)) - 1).min(MAX_PERIOD_US),
    )
}

/// The choices `m6-pools v1` makes to draw `p`, each with the bound it is drawn below, from the rule's text: split 0 is `Shipped`, and the wakes are in tag order.
fn path(p: &SchedulePlan) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    match p.split() {
        Split::Shipped => out.push((0, 2)),
        Split::SettleAtStep { gap_us: 0 } => out.extend([(1, 2), (0, 21)]),
        Split::SettleAtStep { gap_us } => {
            let b = gap_us.ilog2() + 1;
            let (lo, hi) = gap_edges(b);
            out.extend([
                (1, 2),
                (u64::from(b), 21),
                (u64::from(gap_us - lo), u64::from(hi - lo) + 1),
            ]);
        }
    }
    match p.wake() {
        Wake::NextStep => out.push((0, 5)),
        Wake::ColdOnly => out.push((1, 5)),
        Wake::Poll { period_us } => {
            let c = period_us.ilog2();
            let (lo, hi) = period_edges(c);
            out.extend([
                (2, 5),
                (u64::from(c - 9), 11),
                (u64::from(period_us - lo), u64::from(hi - lo) + 1),
            ]);
        }
        Wake::Cadence => out.push((3, 5)),
        Wake::Never => out.push((4, 5)),
    }
    out
}

/// What [`draw_with`] draws from `path`'s choices, checking that it asks each choice's bound in order and asks for every choice.
fn replay(path: &[(u64, u64)]) -> SchedulePlan {
    let mut next = path.iter();
    let p = draw_with(&mut |m| {
        let &(c, bound) = next
            .next()
            .expect("draw asks no more choices than the path holds");
        assert_eq!(m, bound, "draw asks the rule's bound");
        assert!(c < m, "a path's choice is in range");
        c
    })
    .expect("a path's plan is in range");
    assert!(next.next().is_none(), "draw asks every choice of the path");
    p
}

/// Every set v1 can open, with its pools, drawn once for the tests that read them.
fn drawn() -> &'static BTreeMap<SetId, Vec<Vec<SchedulePlan>>> {
    static SETS: std::sync::OnceLock<BTreeMap<SetId, Vec<Vec<SchedulePlan>>>> =
        std::sync::OnceLock::new();
    SETS.get_or_init(|| {
        sets_v1()
            .into_iter()
            .map(|set| (set, set_pools(set).expect("every v1 pool fills")))
            .collect()
    })
}

/// v1's digest is the pin; a mismatch prints the digest found, which only a reviewed commit holding no learner code may pin.
#[test]
fn the_m6_policy_is_pinned() {
    let found = RankerPolicy::v1().id().digest;
    assert_eq!(
        found,
        M6_POLICY_V1_DIGEST,
        "the M6 policy digest is {}",
        found.hex()
    );
    assert_eq!(RankerPolicy::v1().pin(), Ok(()));
}

/// The sets v1 draws are the pinned ones; a mismatch prints the digest found.
#[test]
fn the_sets_are_pinned() {
    let found = sets_digest().expect("every v1 pool fills");
    assert_eq!(
        found,
        M6_SETS_V1_DIGEST,
        "the M6 sets digest is {}",
        found.hex()
    );
}

/// [`RankerPolicy::pinned`] returns v1 on this tree.
#[test]
fn the_pinned_policy_builds() {
    assert_eq!(RankerPolicy::pinned(), Ok(RankerPolicy::v1()));
}

/// Changing any term changes the digest, and the pin refuses it as mutated.
#[test]
fn every_term_is_bound() {
    let v1 = RankerPolicy::v1().id().digest;
    let mut seen = BTreeSet::from([v1]);
    for field in Field::all() {
        let p = RankerPolicy::mutated(field);
        assert!(
            seen.insert(p.id().digest),
            "{field:?} changed no digest, or the same as another field"
        );
        assert_eq!(
            p.pin(),
            Err(RankerRefusal::PolicyMutated {
                found: p.id().digest,
                pinned: M6_POLICY_V1_DIGEST,
            }),
            "{field:?}"
        );
    }
}

/// The descriptor binds M4's pinned digest, and a moved M4 policy is refused by name.
#[test]
fn a_moved_m4_policy_is_refused() {
    assert_eq!(AcceptancePolicy::v1().id().digest, M4_POLICY_V1_DIGEST);
    let moved = AcceptancePolicy::mutated(policy::Field::MaxGap).id().digest;
    assert_ne!(moved, M4_POLICY_V1_DIGEST);
    assert_eq!(
        RankerPolicy::pinned_with(RankerPolicy::v1(), moved, || Ok(M6_SETS_V1_DIGEST)),
        Err(RankerRefusal::AcceptanceMoved {
            bound: M4_POLICY_V1_DIGEST,
            found: moved,
        })
    );
}

/// [`RankerPolicy::pinned`] refuses a mutated policy first, then a moved M4 policy, then a pool that does not fill or moved sets, and hands its checks M4's v1 digest now and the sets drawn now.
#[test]
fn the_pin_refuses_in_order() {
    let moved = AcceptancePolicy::mutated(policy::Field::MaxGap).id().digest;
    let mut sets = M6_SETS_V1_DIGEST;
    sets.0[0] ^= 1;
    let pool = PoolId {
        set: SetId {
            tag: SetTag::HeldOut,
            index: 1,
        },
        pool: 0,
    };
    let exhausted = PoolRefusal::PoolExhausted { pool };
    let mutated = RankerPolicy::mutated(Field::Epochs);
    let found = mutated.id().digest;
    assert_eq!(
        RankerPolicy::pinned_with(mutated, moved, || Err(exhausted)),
        Err(RankerRefusal::PolicyMutated {
            found,
            pinned: M6_POLICY_V1_DIGEST,
        })
    );
    assert_eq!(
        RankerPolicy::pinned_with(RankerPolicy::v1(), moved, || Err(exhausted)),
        Err(RankerRefusal::AcceptanceMoved {
            bound: M4_POLICY_V1_DIGEST,
            found: moved,
        })
    );
    assert_eq!(
        RankerPolicy::pinned_with(RankerPolicy::v1(), M4_POLICY_V1_DIGEST, || Err(exhausted)),
        Err(RankerRefusal::Pool(exhausted))
    );
    assert_eq!(
        RankerPolicy::pinned_with(RankerPolicy::v1(), M4_POLICY_V1_DIGEST, || Ok(sets)),
        Err(RankerRefusal::SetsMoved { found: sets })
    );
    assert_eq!(
        RankerPolicy::pinned_with(RankerPolicy::v1(), M4_POLICY_V1_DIGEST, || Ok(
            M6_SETS_V1_DIGEST
        )),
        Ok(RankerPolicy::v1())
    );
    let src: String = include_str!("m6_policy.rs").split_whitespace().collect();
    assert!(
        src.contains(
            "Self::pinned_from(Self::v1(),AcceptancePolicy::v1().id().digest,sets_digest)"
        ),
        "pinned() checks against M4's v1 now and the sets drawn now, not a constant"
    );
}

/// Every plan has three distinct features in range, by the rule's classes, at every bucket edge.
#[test]
fn the_featurizer_follows_its_rule() {
    let mut gaps = vec![0, 1, 2, 3, 511, 512, MAX_GAP_US];
    gaps.extend((1..20).flat_map(|b| [(1 << b) - 1, 1 << b]));
    let mut periods = vec![MIN_PERIOD_US, 1_023, MAX_PERIOD_US];
    periods.extend((10..20).flat_map(|c| [(1 << c) - 1, 1 << c]));
    let mut wakes: Vec<Wake> = FREE_WAKES.to_vec();
    wakes.extend(periods.iter().map(|&p| poll(p)));
    let mut splits = vec![Split::Shipped];
    splits.extend(gaps.iter().map(|&gap_us| Split::SettleAtStep { gap_us }));
    let mut used = BTreeSet::new();
    for &split in &splits {
        for &wake in &wakes {
            let Ok(p) = SchedulePlan::new(split, wake) else {
                continue;
            };
            let [s, w, x] = features(&p);
            let s_want = match split {
                Split::Shipped => 0,
                Split::SettleAtStep { gap_us: 0 } => 1,
                Split::SettleAtStep { gap_us } => 2 + gap_us.ilog2() as usize,
            };
            let w_want = match wake {
                Wake::NextStep => 0,
                Wake::ColdOnly => 1,
                Wake::Cadence => 2,
                Wake::Never => 3,
                Wake::Poll { period_us } => period_us.ilog2() as usize - 5,
            };
            assert_eq!([s, w, x], [s_want, 22 + w_want, 37 + 15 * s_want + w_want]);
            assert!(s < 22 && (22..37).contains(&w) && (37..FEATURES).contains(&x));
            used.extend([s, w, x]);
        }
    }
    assert_eq!(used.len(), FEATURES, "every weight is reachable");
}

/// The gap and period buckets partition their ranges, and `one_in` is the product of the call sequence's choices, uniform within a bucket.
#[test]
fn the_mass_function_follows_the_call_sequence() {
    assert_eq!(one_in(&plan(Split::Shipped, Wake::NextStep)), 2 * 5);
    assert_eq!(one_in(&settle(0, Wake::Never)), 2 * 21 * 5);
    assert_eq!(one_in(&settle(1, Wake::Cadence)), 2 * 21 * 5);
    assert_eq!(one_in(&settle(2, Wake::Cadence)), 2 * 21 * 2 * 5);
    assert_eq!(one_in(&settle(511, Wake::ColdOnly)), 2 * 21 * 256 * 5);
    assert_eq!(
        one_in(&settle(MAX_GAP_US, Wake::NextStep)),
        2 * 21 * u64::from(MAX_GAP_US - (1 << 19) + 1) * 5
    );
    assert_eq!(one_in(&plan(Split::Shipped, poll(1_000))), 2 * 5 * 11 * 24);
    assert_eq!(
        one_in(&plan(Split::Shipped, poll(1_024))),
        2 * 5 * 11 * 1_024
    );
    assert_eq!(
        one_in(&plan(Split::Shipped, poll(MAX_PERIOD_US))),
        2 * 5 * 11 * u64::from(MAX_PERIOD_US - (1 << 19) + 1)
    );
    // The probabilities of every gap, and of every period, sum to one: each bucket's width cancels its own.
    let gap_sum: u64 = (1..=20)
        .map(|b: u32| {
            let lo = 1u32 << (b - 1);
            let p = settle(lo, Wake::NextStep);
            let width = one_in(&p) / (2 * 21 * 5);
            let hi = (lo + u32::try_from(width).unwrap() - 1).min(MAX_GAP_US);
            assert_eq!(
                one_in(&settle(hi, Wake::NextStep)),
                one_in(&p),
                "bucket {b}"
            );
            if b < 20 {
                assert_eq!(hi + 1, 1 << b, "bucket {b} ends where {} begins", b + 1);
            } else {
                assert_eq!(hi, MAX_GAP_US);
            }
            width
        })
        .sum();
    assert_eq!(gap_sum, u64::from(MAX_GAP_US));
    let period_sum: u64 = (9..=19)
        .map(|c: u32| {
            let lo = (1u32 << c).max(MIN_PERIOD_US);
            one_in(&plan(Split::Shipped, poll(lo))) / (2 * 5 * 11)
        })
        .sum();
    assert_eq!(period_sum, u64::from(MAX_PERIOD_US - MIN_PERIOD_US + 1));
}

/// Over a fixed stream, each of the 330 (split class, wake class) cells is drawn at the rate the rule gives it, within four standard deviations: a split class is half the draws for `Shipped` and one of 21 gap choices of the other half, a wake class one of five wakes or one of `Poll`'s 11 period buckets.
#[test]
fn draws_match_the_mass_function() {
    let mut r = SplitMix::new(0x6d36_2d6d_6173_7321);
    let n = 400_000u64;
    let mut cells = [0u64; 22 * 15];
    for _ in 0..n {
        let [s, w, _] = features(&draw(&mut r).expect("a draw is in range"));
        cells[15 * s + w - 22] += 1;
    }
    for s in 0..22 {
        for w in 0..15 {
            let split = if s == 0 { 2.0 } else { 2.0 * 21.0 };
            let wake = if w < 4 { 5.0 } else { 5.0 * 11.0 };
            let expected = n as f64 / (split * wake);
            let got = cells[15 * s + w] as f64;
            assert!(
                got > 0.0 && (got - expected).abs() <= 4.0 * expected.sqrt() + 1.0,
                "split class {s}, wake class {w}: {got} draws against {expected:.1} expected"
            );
        }
    }
}

/// High mass is exactly the design's list, beyond the grid's own plans.
#[test]
fn the_high_mass_plans_are_the_listed_ones() {
    for &wake in &FREE_WAKES {
        assert!(high_mass(&plan(Split::Shipped, wake)));
        for gap in 0..=2_048 {
            assert_eq!(
                high_mass(&settle(gap, wake)),
                gap <= 511,
                "gap {gap} {wake:?}"
            );
        }
    }
    for period in MIN_PERIOD_US..=4_096 {
        let listed = period <= 1_023;
        assert_eq!(high_mass(&plan(Split::Shipped, poll(period))), listed);
        for gap in 0..=8 {
            assert_eq!(
                high_mass(&settle(gap, poll(period))),
                listed && gap <= 1,
                "gap {gap} poll {period}"
            );
        }
    }
    // The grid's parameter-free plans are high-mass and in the grid both.
    for &wake in &FREE_WAKES {
        for split in [Split::Shipped, Split::SettleAtStep { gap_us: 0 }] {
            assert!(GRID_V1.contains(&(split, wake)));
        }
    }
}

/// The sampler is a function of its seed with full support: every plan has a path of in-range choices, read from the rule's text, that the draw takes to it, and the product of the path's bounds is its `one_in`. A pool takes the first distinct draws its set accepts, so every side-0 plan can enter each train pool, and every plan of side 1, 2 or 3 its sets do not skip each of theirs.
#[test]
fn the_sampler_is_deterministic_with_full_support() {
    let set = SetId {
        tag: SetTag::PracticeHeldOut,
        index: 1,
    };
    assert_eq!(set_pools(set), set_pools(set));
    // The buckets partition the grammar's gaps and periods.
    assert_eq!(gap_edges(1).0, 1);
    assert_eq!(gap_edges(20).1, MAX_GAP_US);
    assert!((1..20).all(|b| gap_edges(b).1 + 1 == gap_edges(b + 1).0));
    assert_eq!(period_edges(9).0, MIN_PERIOD_US);
    assert_eq!(period_edges(19).1, MAX_PERIOD_US);
    assert!((9..19).all(|c| period_edges(c).1 + 1 == period_edges(c + 1).0));
    // The least choice everywhere draws `Shipped` with `NextStep`, the greatest the largest gap with `Never`.
    assert_eq!(
        draw_with(&mut |_| 0),
        Ok(plan(Split::Shipped, Wake::NextStep))
    );
    assert_eq!(
        draw_with(&mut |m| m - 1),
        Ok(settle(MAX_GAP_US, Wake::Never))
    );
    // Every bucket edge with every wake class's edges, and every plan of a stream.
    let mut splits = vec![Split::Shipped, Split::SettleAtStep { gap_us: 0 }];
    for b in 1..=20 {
        let (lo, hi) = gap_edges(b);
        splits.extend([lo, hi].map(|gap_us| Split::SettleAtStep { gap_us }));
    }
    let mut wakes = FREE_WAKES.to_vec();
    for c in 9..=19 {
        let (lo, hi) = period_edges(c);
        wakes.extend([lo, hi].map(poll));
    }
    let mut plans: Vec<SchedulePlan> = splits
        .iter()
        .flat_map(|&s| wakes.iter().map(move |&w| plan(s, w)))
        .collect();
    let mut r = SplitMix::new(7);
    for _ in 0..10_000 {
        plans.push(draw(&mut r).expect("every draw is in range"));
    }
    for p in plans {
        let path = path(&p);
        assert_eq!(replay(&path), p, "the rule's path draws {p:?}");
        assert_eq!(
            path.iter().map(|&(_, m)| m).product::<u64>(),
            one_in(&p),
            "{p:?}"
        );
    }
}

/// A pool is the first N distinct plans of its draws that its set accepts: train pools skip nothing, so they hold high-mass plans; every other set skips; practice-report j also skips practice-held-out j.
#[test]
fn pools_are_the_first_plans_their_set_accepts() {
    let looks = &drawn()[&SetId {
        tag: SetTag::PracticeHeldOut,
        index: 0,
    }];
    for (&set, pools) in drawn() {
        for (p, pool) in pools.iter().enumerate() {
            let id = PoolId {
                set,
                pool: u32::try_from(p).expect("a pool index"),
            };
            let want = fill(&id, |plan| {
                side(plan) == set.tag.side()
                    && (set.tag == SetTag::Train || !skipped(plan))
                    && (set.tag != SetTag::PracticeReport
                        || !looks.iter().flatten().any(|q| q == plan))
            })
            .expect("every v1 pool fills");
            assert_eq!(pool, &want, "{set:?} pool {p}");
        }
    }
    let train = &drawn()[&SetId {
        tag: SetTag::Train,
        index: 0,
    }];
    assert!(
        train.iter().flatten().any(high_mass),
        "the train set skips no high-mass plan"
    );
}

/// Bumping the policy version moves every pool.
#[test]
fn a_new_version_draws_fresh_pools() {
    for (&set, pools) in drawn() {
        let next = set_pools_at(RANKER_POLICY_VERSION + 1, set).expect("every pool fills");
        for (p, (a, b)) in pools.iter().zip(&next).enumerate() {
            assert_ne!(a, b, "{set:?} pool {p} did not move");
        }
    }
}

/// Each set has its count of pools of N distinct plans of its own side; sets of different sides share no plan, and practice-report 0 shares none with practice-held-out 0, by id alone.
#[test]
fn the_sets_are_disjoint_by_side() {
    let mut by_side: BTreeMap<Side, BTreeSet<Digest>> = BTreeMap::new();
    for (&set, pools) in drawn() {
        let count = if set.tag == SetTag::Train {
            TRAIN_POOLS
        } else {
            POOLS
        };
        assert_eq!(pools.len(), count, "{set:?}");
        for pool in pools {
            assert_eq!(pool.len(), POOL_PLANS);
            let ids: BTreeSet<Digest> = pool.iter().map(SchedulePlan::digest).collect();
            assert_eq!(ids.len(), POOL_PLANS, "a pool's plans are distinct");
            for p in pool {
                assert_eq!(side(p), set.tag.side(), "{set:?}");
            }
            by_side.entry(set.tag.side()).or_default().extend(ids);
        }
    }
    let sides: Vec<&BTreeSet<Digest>> = by_side.values().collect();
    for (a, x) in sides.iter().enumerate() {
        for y in &sides[a + 1..] {
            assert!(x.is_disjoint(y));
        }
    }
    let ids = |tag| -> BTreeSet<Digest> {
        drawn()[&SetId { tag, index: 0 }]
            .iter()
            .flatten()
            .map(SchedulePlan::digest)
            .collect()
    };
    assert!(ids(SetTag::PracticeHeldOut).is_disjoint(&ids(SetTag::PracticeReport)));
}

/// No held-out, report or practice pool holds a grid plan, a plans-file plan or a high-mass plan.
#[test]
fn no_skipped_plan_is_drawn() {
    let table: BTreeSet<Digest> = GRID_V1
        .iter()
        .chain(&PLANS_FILE_V1)
        .map(|&(s, w)| plan(s, w).digest())
        .collect();
    assert_eq!(table.len(), 54 + 5, "three plans-file lines are grid plans");
    for (set, pools) in drawn() {
        if set.tag == SetTag::Train {
            continue;
        }
        for p in pools.iter().flatten() {
            assert!(
                !table.contains(&p.digest()) && !high_mass(p) && !skipped(p),
                "{set:?} {p:?}"
            );
        }
    }
}

/// No plan is in more than 8 of a held-out, report or practice set's pools.
#[test]
fn no_plan_dominates_a_set() {
    for (set, pools) in drawn() {
        if set.tag == SetTag::Train {
            continue;
        }
        let mut n: BTreeMap<Digest, usize> = BTreeMap::new();
        for p in pools.iter().flatten() {
            *n.entry(p.digest()).or_default() += 1;
        }
        let most = n.values().copied().max().unwrap_or(0);
        assert!(most <= 8, "{set:?}: a plan is in {most} pools");
    }
}

/// A pool fills well inside its draw budget.
#[test]
fn pools_fill_inside_their_budget() {
    let id = PoolId {
        set: SetId {
            tag: SetTag::HeldOut,
            index: 1,
        },
        pool: 0,
    };
    let mut r = SplitMix::new(super::m6_policy::pool_seed(&id));
    let (mut taken, mut draws) = (BTreeSet::new(), 0);
    while taken.len() < POOL_PLANS {
        let p = draw(&mut r).expect("a draw is in range");
        draws += 1;
        if side(&p) == Side::HeldOut && !skipped(&p) {
            taken.insert(p.encode());
        }
    }
    assert!(draws < MAX_DRAWS / 8, "{draws} draws");
}

/// `u64::from_le_bytes` of the first 8 bytes of `Digest::of(domain, parts)`.
fn key(domain: &[u8], parts: &[&[u8]]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&Digest::of(domain, parts).0[..8]);
    u64::from_le_bytes(b)
}

/// The tie and control keys, the pool seed, the trainer's seed and the control's id are the encoding rule's digests of their inputs, and differ when the set's tag, its index, the pool or the plan does.
#[test]
fn keys_bind_their_inputs() {
    let a = PoolId {
        set: SetId {
            tag: SetTag::HeldOut,
            index: 1,
        },
        pool: 0,
    };
    let pools = [
        a,
        PoolId { pool: 1, ..a },
        PoolId {
            set: SetId {
                tag: SetTag::Report,
                index: 1,
            },
            ..a
        },
        PoolId {
            set: SetId {
                tag: SetTag::HeldOut,
                index: 2,
            },
            ..a
        },
    ];
    let name = RANKER_POLICY_NAME.as_bytes();
    let version = RANKER_POLICY_VERSION.to_le_bytes();
    let mut keys = BTreeSet::new();
    for pool in pools {
        let (tag, i, p) = (
            [pool.set.tag.tag()],
            pool.set.index.to_le_bytes(),
            pool.pool.to_le_bytes(),
        );
        assert_eq!(
            pool_seed(&pool),
            key(POOL_DOMAIN, &[name, &version, &tag, &i, &p])
        );
        for x in [settle(600, Wake::NextStep), settle(601, Wake::NextStep)] {
            let parts: [&[u8]; 6] = [name, &version, &tag, &i, &p, &x.encode()];
            assert_eq!(tie_key(&pool, &x), key(TIE_DOMAIN, &parts));
            assert_eq!(control_key(&pool, &x), key(CONTROL_DOMAIN, &parts));
            assert!(keys.insert(tie_key(&pool, &x)) && keys.insert(control_key(&pool, &x)));
        }
    }
    assert_eq!(trainer_seed(), key(TRAINER_DOMAIN, &[name, &version]));
    assert_eq!(
        control_id(),
        Digest::of(CONTROL_ID_DOMAIN, &[name, &version])
    );
}

/// The nine domains are distinct and each ends in its only NUL, so no M6 digest, the control's id among them, can equal another kind's (a version's id included).
#[test]
fn the_domains_are_distinct() {
    let domains = [
        POOL_DOMAIN,
        SIDE_DOMAIN,
        TIE_DOMAIN,
        CONTROL_DOMAIN,
        CONTROL_ID_DOMAIN,
        TRAINER_DOMAIN,
        VERSION_DOMAIN,
        BASE_DOMAIN,
        SETS_DOMAIN,
    ];
    assert_eq!(domains.iter().collect::<BTreeSet<_>>().len(), domains.len());
    for d in domains {
        assert_eq!(
            d.iter().position(|&b| b == 0),
            Some(d.len() - 1),
            "{}",
            String::from_utf8_lossy(d)
        );
    }
}

/// The numeric literals of `src`'s code, outside comments, strings and character literals.
fn numbers(src: &str) -> Vec<&str> {
    let b = src.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let (mut i, mut out) = (0, Vec::new());
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'\'' if b.get(i + 1) == Some(&b'\\') => {
                i += 2;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
                i += 1;
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 3,
            c if c.is_ascii_digit() && (i == 0 || !(ident(b[i - 1]) || b[i - 1] == b'.')) => {
                let start = i;
                while i < b.len()
                    && (ident(b[i])
                        || (b[i] == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)))
                {
                    i += 1;
                }
                out.push(&src[start..i]);
            }
            _ => i += 1,
        }
    }
    out
}

/// Whether a numeric literal is a float: a fraction, an exponent or a float suffix, in a literal that is not hex, octal or binary.
fn is_float(n: &str) -> bool {
    if ["0x", "0o", "0b"].iter().any(|p| n.starts_with(p)) {
        return false;
    }
    let ints = [
        "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
    ];
    let n = ints.iter().find_map(|s| n.strip_suffix(s)).unwrap_or(n);
    n.contains('.')
        || n.contains(['e', 'E'])
        || n.ends_with(concat!("f", "32"))
        || n.ends_with(concat!("f", "64"))
}

/// The policy's arithmetic is integer: its source names no float type and holds no float literal.
#[test]
fn the_policy_has_no_float() {
    let src = include_str!("m6_policy.rs");
    for banned in [concat!("f", "32"), concat!("f", "64")] {
        let hit = src
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|word| word == banned);
        assert!(!hit, "m6_policy.rs names {banned}");
    }
    let floats: Vec<&str> = numbers(src).into_iter().filter(|n| is_float(n)).collect();
    assert!(
        floats.is_empty(),
        "m6_policy.rs holds float literals {floats:?}"
    );
    // The scan's own check: it finds a fraction, an exponent and a float suffix, and nothing in a comment, a string, a range, a tuple field or an integer literal.
    let probe = concat!(
        "let a = 1.5; let b = 2e6; let c = 3f64; let d = 0.5_f32;\n",
        "// 0.25 in a comment\n let s = \"0.75\"; let q = '1';\n",
        "let r = 0..8; let t = x.0.1; let h = 0x1e; let u = 7usize; let v = 1_000u64;"
    );
    let found: Vec<&str> = numbers(probe).into_iter().filter(|n| is_float(n)).collect();
    assert_eq!(found, ["1.5", "2e6", "3f64", "0.5_f32"]);
}
