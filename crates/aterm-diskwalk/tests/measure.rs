// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The walker's throughput and slice latency over a ~200k-entry scratch tree, on the real
//! clock. Ignored by default (it writes 200,000 files); run it with
//! `targo --unverified test --release -p aterm-diskwalk --test measure -- --ignored --nocapture`.
//!
//! It reports, per lister: entries per second, slices, and the wall time of one slice at
//! p50/p99/max. The tree was just written, so its metadata is warm in the cache: this measures
//! the walker, not the disk. It asserts only what the walk found; a number is a report, never a
//! budget.

use std::path::Path;
use std::time::{Duration, Instant};

use aterm_diskwalk::{Deny, ListerChoice, Policy, RootSpec, SLICE, Step, Walk};

const TOP: usize = 40;
const MID: usize = 10;
const FILES: usize = 500;

fn now() -> Instant {
    #[expect(
        clippy::disallowed_methods,
        reason = "the measurement reads the real clock: slice latency is what it reports"
    )]
    Instant::now()
}

/// `TOP × MID` folders of `FILES` files each: 200,000 files in 440 folders. Every tenth file
/// has a page of bytes, the rest are empty.
fn build(root: &Path) -> u64 {
    let page = vec![1u8; 4_096];
    let mut entries = 0u64;
    for t in 0..TOP {
        let top = root.join(format!("t{t:02}"));
        std::fs::create_dir(&top).expect("mkdir top");
        entries += 1;
        for m in 0..MID {
            let mid = top.join(format!("m{m:02}"));
            std::fs::create_dir(&mid).expect("mkdir mid");
            entries += 1;
            for f in 0..FILES {
                let bytes: &[u8] = if f % 10 == 0 { &page } else { &[] };
                std::fs::write(mid.join(format!("f{f:04}")), bytes).expect("write file");
                entries += 1;
            }
        }
    }
    entries
}

fn percentile(sorted: &[Duration], p: usize) -> Duration {
    let i = (sorted.len() * p / 100).min(sorted.len() - 1);
    sorted[i]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1_000.0
}

fn measure(root: &Path, lister: ListerChoice, expected: u64) {
    let mut policy = Policy::new(
        Deny::for_home(Path::new("/Users//diskwalk-measure-home"), &[], &[]),
        aterm_sysprobe::volumes::table(),
    );
    policy.lister = lister;
    let started = now();
    let mut walk = Walk::begin(&RootSpec::new(root), &policy).expect("begin");
    let mut clock = now;
    let mut slices = Vec::new();
    loop {
        let t = now();
        let step = walk.diskwalk_slice(&mut clock);
        slices.push(now() - t);
        if !matches!(step, Step::More { .. }) {
            assert_eq!(step, Step::Done);
            break;
        }
    }
    let total = now() - started;
    assert_eq!(walk.walked(), expected, "{lister:?} listed every entry");
    assert_eq!(
        walk.tree().root().subtree.files,
        u64::try_from(TOP * MID * FILES).expect("fits")
    );
    slices.sort();
    let rate = f64::from(u32::try_from(walk.walked()).expect("a scratch walk fits u32"))
        / total.as_secs_f64();
    println!(
        "measure {lister:?}: {} entries in {:.1} ms = {:.0} entries/s; {} slices of {} ms budget, \
         slice p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms; {} calls",
        walk.walked(),
        ms(total),
        rate,
        slices.len(),
        SLICE.as_millis(),
        ms(percentile(&slices, 50)),
        ms(percentile(&slices, 99)),
        ms(*slices.last().expect("at least one slice")),
        walk.calls(),
    );
}

#[test]
#[ignore = "writes 200,000 files; run on purpose with --ignored --nocapture"]
fn walk_a_200k_entry_tree_and_report() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path().join("tree");
    std::fs::create_dir(&root).expect("mkdir root");
    let built = now();
    let expected = build(&root);
    println!(
        "measure: built {expected} entries in {:.1} s",
        (now() - built).as_secs_f64()
    );
    // Each lister twice: the first pass also warms what the build left cold.
    for lister in [ListerChoice::Native, ListerChoice::Portable] {
        measure(&root, lister, expected);
        measure(&root, lister, expected);
    }
}
