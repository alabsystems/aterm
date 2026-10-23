// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The corpus of generator v1 (design §10.3): it is deterministic under a pinned digest, no item leaves the denominator, and every scenario is admitted inside its tables.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::MomentumTrace;
use super::corpus::{
    COOLS, CORPUS_DOMAIN, Corpus, CorpusSplit, END_AFTER_NS, FRAME_INTERVALS_NS, GENERATOR_VERSION,
    GOLDEN_TRACE, HELD_OUT_ITEMS, HELD_OUT_SEEDS, INTENSITIES, ItemBody, ItemKind, SplitMix,
    Stratum, TAUS_S, TRAIN_ITEMS, TRAIN_SEEDS, corpus_digest, golden_trace_digest, item_part,
    scenario, stratum_of,
};
use super::lane::{Exo, s0};
use aterm_aos::Digest;

/// The corpus digest generator v1 makes, pinned so that moving it is a reviewed source change.
const CORPUS_DIGEST: &str = "82c8c684de8990b3b8c421085c83ecdd6484208c2d7794d2dc140d2d7a406b57";

fn corpus() -> Corpus {
    Corpus::generate().expect("generator v1's scenarios are admitted")
}

/// The corpus is the same every time, its digest is the pinned one and the documented fold, and a flipped byte of any scenario, the version or the golden trace's digest moves it.
#[test]
fn the_corpus_is_deterministic_and_its_digest_is_stable() {
    let (a, b) = (corpus(), corpus());
    assert_eq!(a, b, "the corpus is a function of the source alone");
    assert_eq!(a.digest(), b.digest());
    assert_eq!(a.digest().hex(), CORPUS_DIGEST, "the pinned corpus digest");
    let golden = golden_trace_digest();
    let parts = a.parts();
    assert_eq!(
        parts.len(),
        TRAIN_ITEMS + HELD_OUT_ITEMS - 1,
        "one part per lane item"
    );
    assert_eq!(corpus_digest(&golden, &parts), a.digest());
    let version = GENERATOR_VERSION.to_le_bytes();
    let mut all: Vec<&[u8]> = vec![&version, &golden.0];
    all.extend(parts.iter().map(Vec::as_slice));
    assert_eq!(Digest::of(CORPUS_DOMAIN, &all), a.digest());

    let mut flipped = 0usize;
    for (i, item) in a
        .items()
        .iter()
        .filter(|i| i.scenario().is_some())
        .enumerate()
    {
        let encoding = item.scenario().expect("a lane item").encode();
        let n = encoding.len();
        for at in [0, 1, n / 3, n / 2, (2 * n) / 3, n - 2, n - 1] {
            let mut bytes = encoding.clone();
            bytes[at] ^= 0x01;
            let mut moved = parts.clone();
            moved[i] = item_part(item.kind(), item.split(), &bytes);
            assert_ne!(
                corpus_digest(&golden, &moved),
                a.digest(),
                "{}: byte {at} of {n}",
                item.id()
            );
            flipped += 1;
        }
    }
    assert_eq!(flipped, 7 * parts.len());
    let mut other = golden;
    other.0[31] ^= 1;
    assert_ne!(
        corpus_digest(&other, &parts),
        a.digest(),
        "the golden trace's digest"
    );
    assert_ne!(
        Digest::of(CORPUS_DOMAIN, &[&2u32.to_le_bytes(), &golden.0]),
        Digest::of(CORPUS_DOMAIN, &[&version, &golden.0]),
        "the version"
    );
}

/// No item leaves the denominator: 17 training and 9 held-out items with stable ids and kinds, and every stratum in both splits.
#[test]
fn no_item_leaves_the_denominator() {
    let c = corpus();
    assert_eq!(c.items().len(), TRAIN_ITEMS + HELD_OUT_ITEMS);
    assert_eq!((TRAIN_ITEMS, HELD_OUT_ITEMS), (17, 9));
    let train: Vec<_> = c.split(CorpusSplit::Train).collect();
    let held: Vec<_> = c.split(CorpusSplit::HeldOut).collect();
    assert_eq!((train.len(), held.len()), (17, 9));
    let mut want = vec!["golden".to_owned()];
    want.extend(TRAIN_SEEDS.iter().map(|s| format!("train-{s:02}")));
    want.push("s0".to_owned());
    want.extend(HELD_OUT_SEEDS.iter().map(|s| format!("held-{s}")));
    let ids: Vec<&str> = c.items().iter().map(|i| i.id()).collect();
    assert_eq!(ids, want.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(
        ids.iter().collect::<BTreeSet<_>>().len(),
        ids.len(),
        "ids are unique"
    );
    assert_eq!(train[0].kind(), ItemKind::GoldenTrace);
    assert_eq!(held[0].kind(), ItemKind::M3Story);
    assert_eq!(held[0].scenario(), Some(&s0()));
    for (items, seeds) in [
        (&train[1..], &TRAIN_SEEDS[..]),
        (&held[1..], &HELD_OUT_SEEDS[..]),
    ] {
        assert_eq!(items.len(), seeds.len());
        for (item, &seed) in items.iter().zip(seeds) {
            assert_eq!(
                item.kind(),
                ItemKind::Generated {
                    seed,
                    stratum: stratum_of(seed)
                }
            );
            assert_eq!(item.scenario(), Some(&scenario(seed).expect("admitted")));
        }
        let strata: BTreeSet<&str> = items
            .iter()
            .filter_map(|i| match i.kind() {
                ItemKind::Generated { stratum, .. } => Some(stratum.name()),
                _ => None,
            })
            .collect();
        assert_eq!(
            strata.len(),
            Stratum::ALL.len(),
            "every stratum in each split"
        );
    }
    let ItemBody::Trace(golden) = train[0].body() else {
        panic!("the golden item is a trace");
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/aos-momentum/aarch64-macos.trace");
    let on_disk = std::fs::read(&path).expect("the golden trace is checked in");
    assert_eq!(
        *golden,
        on_disk.as_slice(),
        "the compiled-in trace is the file"
    );
    assert_eq!(*golden, GOLDEN_TRACE);
    MomentumTrace::decode(golden).expect("the golden trace decodes");
}

/// Every scenario generator v1 draws over 300 seeds is admitted, fixed by its seed, and inside its tables.
#[test]
fn every_generated_scenario_is_admitted_and_inside_its_tables() {
    let mut seen_intervals = BTreeSet::new();
    let mut seen_taus = BTreeSet::new();
    for seed in (1..=200).chain(HELD_OUT_SEEDS).chain(1_000..1_092) {
        let s = scenario(seed).expect("admitted");
        assert_eq!(
            s,
            scenario(seed).expect("admitted"),
            "seed {seed}: fixed by the seed"
        );
        assert!(FRAME_INTERVALS_NS.contains(&s.frame_interval_ns()));
        seen_intervals.insert(s.frame_interval_ns());
        let cfg = s.start().cfg.config();
        assert!(TAUS_S.contains(&cfg.tau_s));
        seen_taus.insert(cfg.tau_s.to_bits());
        assert!(INTENSITIES.contains(&cfg.intensity));
        let keys: Vec<u64> = s
            .events()
            .iter()
            .filter_map(|&(at, e)| matches!(e, Exo::Key { .. }).then_some(at))
            .collect();
        assert!(
            (4..=96).contains(&keys.len()),
            "seed {seed}: {} keys",
            keys.len()
        );
        for &(_, e) in s.events() {
            if let Exo::Key { tau_bits } = e {
                assert_eq!(tau_bits, cfg.tau_s.to_bits(), "a key's τ is the ticks'");
            }
        }
        // A gap inside a burst is 1/16 s to 1/2 s; one between bursts is a cut or a park from the tables.
        let gaps: Vec<u64> = keys.windows(2).map(|w| w[1] - w[0]).collect();
        let between = gaps.iter().filter(|&&g| g > 500_000_000).count();
        assert!(between <= 3, "seed {seed}: at most four bursts");
        for &g in &gaps {
            let in_burst = (62_500_000..=500_000_000).contains(&g);
            let cut = COOLS.iter().flatten().any(|&c| c == g);
            assert!(in_burst || cut || g > 500_000_000, "seed {seed}: gap {g}");
        }
        let last_input = s.events().last().map_or(0, |e| e.0);
        assert_eq!(s.end_ns(), last_input + END_AFTER_NS);
        assert!(s.end_ns() >= keys.last().copied().unwrap_or(0) + END_AFTER_NS);
        match stratum_of(seed) {
            Stratum::MovingHue => {
                assert!(
                    !cfg.dark_theme,
                    "seed {seed}: the moving hue is a light page's"
                );
                assert!(!s.presentation_moves_only_at_keys());
            }
            Stratum::IntensityRamp => assert!(!s.presentation_moves_only_at_keys()),
            Stratum::ResetInCoolDown => {
                assert!(s.events().iter().any(|e| e.1 == Exo::Reset));
                assert!(s.presentation_moves_only_at_keys());
            }
            Stratum::Plain | Stratum::CaretAtKeys | Stratum::ConfigAtKeys => {
                assert!(s.presentation_moves_only_at_keys());
                assert!(s.events().iter().all(|e| e.1 != Exo::Reset));
            }
        }
    }
    assert_eq!(seen_intervals.len(), FRAME_INTERVALS_NS.len());
    assert_eq!(seen_taus.len(), TAUS_S.len());
}

/// The corpus generator's SplitMix is splitmix64 (its reference outputs from state 0), and the momentum fixtures' stream is the same one.
#[test]
fn split_mix_is_splitmix64() {
    let mut r = SplitMix::new(0);
    assert_eq!(r.next_u64(), 0xe220_a839_7b1d_cdaf);
    assert_eq!(r.next_u64(), 0x6e78_9e6a_a1b9_65f4);
    assert_eq!(r.next_u64(), 0x06c4_5d18_8009_454f);
    let mut a = SplitMix::new(0xC0FFEE);
    let mut b = super::test_fixtures::Rng::new(0xC0FFEE);
    for n in [1u64, 2, 3, 7, 1_000, u64::MAX] {
        assert_eq!(a.below(n), b.below(n));
    }
    assert_eq!(SplitMix::new(5).below(0), 0);
    assert_eq!(SplitMix::new(5).pick::<u8>(&[]), None);
}
