// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! PolicyEngine hot-path benchmark for #7998.
//!
//! This bench measures the per-evaluation cost of `PolicyEngine::evaluate`
//! across the three built-in profiles. It is the bench gate referenced by
//! #7998's acceptance criteria and is the authoritative source of the
//! ">5% overhead is a regression" guardrail for the policy handoff on
//! the PTY -> handler path.
//!
//! Run with:
//!
//! ```bash
//! cargo bench --package aterm-policy --bench policy_engine_hotpath
//! ```
//!
//! The bench ids cover every major (profile, sequence) pair we expect to
//! dominate in the wild:
//!
//!   * `policy/<profile>/osc52_set`    — clipboard write from a PTY
//!   * `policy/<profile>/osc52_query`  — clipboard read query
//!   * `policy/<profile>/osc4_query`   — palette color query
//!   * `policy/<profile>/osc9`         — Terminal-style notification
//!   * `policy/<profile>/osc_unknown`  — unknown major (exercises the
//!     default / wildcard path)
//!   * `policy/<profile>/csi_window_op` — CSI t 20 (window op)
//!
//! The comparison baseline for the <=2% regression gate is
//! `cargo bench --baseline main` on this bench file.

use aterm_policy::{OriginTag, engine::PolicyEngine, profiles, selector::DispatchedSequence};
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_profile(c: &mut Criterion, profile_name: &str, eng: PolicyEngine) {
    // OSC 52 set — clipboard write.
    let seq = DispatchedSequence::osc(52, [String::from("c"), String::from("SGVsbG8=")]);
    c.bench_function(&format!("policy/{profile_name}/osc52_set"), |b| {
        b.iter(|| {
            black_box(eng.evaluate(black_box(&seq), black_box(OriginTag::User)));
        });
    });

    // OSC 52 query.
    let seq = DispatchedSequence::osc(52, [String::from("c"), String::from("?")]);
    c.bench_function(&format!("policy/{profile_name}/osc52_query"), |b| {
        b.iter(|| {
            black_box(eng.evaluate(black_box(&seq), black_box(OriginTag::Host)));
        });
    });

    // OSC 4 query — palette color.
    let seq = DispatchedSequence::osc(4, [String::from("3"), String::from("?")]);
    c.bench_function(&format!("policy/{profile_name}/osc4_query"), |b| {
        b.iter(|| {
            black_box(eng.evaluate(black_box(&seq), black_box(OriginTag::PtySafe)));
        });
    });

    // OSC 9 — Terminal notification.
    let seq = DispatchedSequence::osc(9, [String::from("build done")]);
    c.bench_function(&format!("policy/{profile_name}/osc9"), |b| {
        b.iter(|| {
            black_box(eng.evaluate(black_box(&seq), black_box(OriginTag::User)));
        });
    });

    // OSC 1337 — unknown major, exercises default / wildcard.
    let seq = DispatchedSequence::osc(1337, [String::from("leet")]);
    c.bench_function(&format!("policy/{profile_name}/osc_unknown"), |b| {
        b.iter(|| {
            black_box(eng.evaluate(black_box(&seq), black_box(OriginTag::Pty)));
        });
    });

    // CSI 20 t — window op.
    let seq = DispatchedSequence::csi(Some(20), 't', core::iter::empty::<String>());
    c.bench_function(&format!("policy/{profile_name}/csi_window_op"), |b| {
        b.iter(|| {
            black_box(eng.evaluate(black_box(&seq), black_box(OriginTag::Host)));
        });
    });
}

fn bench_permissive(c: &mut Criterion) {
    bench_profile(c, "permissive", PolicyEngine::new(profiles::permissive()));
}

fn bench_standard(c: &mut Criterion) {
    bench_profile(c, "standard", PolicyEngine::new(profiles::standard()));
}

fn bench_hardened(c: &mut Criterion) {
    bench_profile(c, "hardened", PolicyEngine::new(profiles::hardened()));
}

fn bench_construction(c: &mut Criterion) {
    // Engine construction cost — called once at session start and on
    // policy hot-swap. We measure it here so the #7998 gate sees any
    // regression in rule precompilation.
    c.bench_function("policy/construct/hardened", |b| {
        b.iter(|| {
            black_box(PolicyEngine::new(profiles::hardened()));
        });
    });
    c.bench_function("policy/construct/standard", |b| {
        b.iter(|| {
            black_box(PolicyEngine::new(profiles::standard()));
        });
    });
    c.bench_function("policy/construct/permissive", |b| {
        b.iter(|| {
            black_box(PolicyEngine::new(profiles::permissive()));
        });
    });
}

criterion_group!(
    benches,
    bench_permissive,
    bench_standard,
    bench_hardened,
    bench_construction,
);
criterion_main!(benches);
