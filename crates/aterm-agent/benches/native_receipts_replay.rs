// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Authenticated receipt replay, excluding file I/O and synthetic corpus construction.

#[cfg(all(unix, debug_assertions))]
fn main() {
    use aterm_agent::harness::native_receipts::testing::{ReplayCorpus, ReplayFormat};
    use std::hint::black_box;
    use std::time::Instant;

    const RUNS: usize = 5;
    for format in [
        ReplayFormat::LegacyClosed,
        ReplayFormat::IssuerOpen,
        ReplayFormat::RecipientOpen,
        ReplayFormat::MixedClosed,
    ] {
        for operations in [128, 1024, 4096] {
            let corpus = ReplayCorpus::new(format, operations).expect("bounded synthetic history");
            let expected = corpus.replay().expect("warm replay");
            assert_eq!(expected.operations, operations);
            let reserved = match format {
                ReplayFormat::IssuerOpen => operations * 2,
                ReplayFormat::RecipientOpen => operations * 3,
                ReplayFormat::LegacyClosed | ReplayFormat::MixedClosed => 0,
            };
            assert_eq!(expected.reserved_records, reserved);
            let mut elapsed = Vec::with_capacity(RUNS);
            for _ in 0..RUNS {
                let start = Instant::now();
                let actual = black_box(corpus.replay().expect("authenticated replay"));
                elapsed.push(start.elapsed());
                assert_eq!(actual, expected);
            }
            elapsed.sort_unstable();
            println!(
                "{format:?} operations={operations} records={} reserved={} reserved_bytes={} runs={RUNS} min_ms={:.3} median_ms={:.3} max_ms={:.3}",
                expected.records,
                expected.reserved_records,
                expected.reserved_bytes,
                elapsed[0].as_secs_f64() * 1000.0,
                elapsed[RUNS / 2].as_secs_f64() * 1000.0,
                elapsed[RUNS - 1].as_secs_f64() * 1000.0,
            );
        }
    }
}

#[cfg(not(all(unix, debug_assertions)))]
fn main() {
    eprintln!("native receipt replay requires Unix and --profile opt-test");
    std::process::exit(2);
}
