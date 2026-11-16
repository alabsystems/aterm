// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const S: u64 = 1_000_000_000;
const MODULUS: u64 = 1 << 32;

fn sample(rx: u64, tx: u64, packets: u64) -> ByteSample {
    ByteSample {
        index: 16,
        counter: ByteCounter::DarwinModulo32,
        rx,
        tx,
        rx_packets: packets,
        tx_packets: packets,
        rate_bps: Some(10_000_000_000),
    }
}

#[test]
fn bounded_modulo_wrap_counts_traffic_without_reconstructing_a_lifetime_total() {
    let before = sample(MODULUS - 2048, MODULUS - 1024, 100);
    let now = sample(4096, 1024, 110);
    assert_eq!(
        byte_delta(before, now, S),
        Ok(ByteDelta { rx: 6144, tx: 2048 })
    );
    assert_eq!(now.rx, 4096, "the reading remains a modulo residue");
    assert_eq!(now.counter.bits(), Some(32));
    assert_eq!(now.counter.quantum(), Some(1024));
}

#[test]
fn forward_residues_do_not_prove_how_many_wraps_occurred() {
    let before = sample(1024, 0, 100);
    let now = sample(2048, 1024, 110);
    for seconds in [4, 45, 60, 90] {
        assert_eq!(
            byte_delta(before, now, seconds * S),
            Err(DeltaError::AmbiguousWrap)
        );
    }
    assert!(byte_delta(before, now, S).is_ok());
    assert_eq!(
        byte_delta(
            before,
            ByteSample {
                rate_bps: None,
                ..now
            },
            S
        ),
        Err(DeltaError::AmbiguousWrap)
    );
    assert_eq!(
        byte_delta(
            before,
            ByteSample {
                rate_bps: Some(1_000_000_000),
                ..now
            },
            S
        ),
        Err(DeltaError::AmbiguousWrap)
    );
}

#[test]
fn full_width_packets_and_capacity_reject_a_reset_disguised_as_a_wrap() {
    let before = sample(2_000_000 * 1024, 2048, 10_000);
    let now = sample(1024, 3072, 9_000);
    assert_eq!(byte_delta(before, now, S), Err(DeltaError::Discontinuity));
    let now = ByteSample {
        rx_packets: 10_001,
        tx_packets: 10_001,
        ..now
    };
    assert_eq!(
        byte_delta(before, now, S),
        Err(DeltaError::Discontinuity),
        "the modulo candidate exceeds the capacity bound"
    );
    assert_eq!(
        byte_delta(sample(0, 0, 5), sample(1024, 0, 5), S),
        Err(DeltaError::Discontinuity),
        "bytes changed without packet movement"
    );
}

#[test]
fn idle_uncapacitated_links_do_not_hide_an_active_interfaces_rates() {
    let idle = ByteSample {
        rate_bps: None,
        ..sample(0, 0, 0)
    };
    assert_eq!(
        byte_delta(idle, idle, 60 * S),
        Ok(ByteDelta { rx: 0, tx: 0 })
    );
    let moving = ByteSample {
        rx_packets: 1,
        rx: 1024,
        ..idle
    };
    assert_eq!(byte_delta(idle, moving, S), Err(DeltaError::AmbiguousWrap));
}

#[test]
fn counter_provenance_and_valid_window_are_required() {
    let before = sample(0, 0, 0);
    let now = sample(1024, 1024, 1);
    assert_eq!(byte_delta(before, now, 0), Err(DeltaError::InvalidInterval));
    assert_eq!(
        byte_delta(before, now, MAX_WINDOW_NS + 1),
        Err(DeltaError::InvalidInterval)
    );
    assert_eq!(
        byte_delta(before, ByteSample { index: 17, ..now }, S),
        Err(DeltaError::SourceChanged)
    );
    assert_eq!(
        byte_delta(
            before,
            ByteSample {
                counter: ByteCounter::Cumulative64,
                ..now
            },
            S
        ),
        Err(DeltaError::SourceChanged)
    );
    assert_eq!(
        byte_delta(ByteSample::default(), ByteSample::default(), S),
        Err(DeltaError::UnknownCounter)
    );
    for invalid in [1, MODULUS] {
        assert_eq!(
            byte_delta(before, ByteSample { rx: invalid, ..now }, S),
            Err(DeltaError::UnknownCounter)
        );
    }
}

#[test]
fn cumulative64_keeps_full_width_and_rejects_regression() {
    let before = ByteSample {
        counter: ByteCounter::Cumulative64,
        rx: 4 * MODULUS,
        ..sample(0, 0, 10)
    };
    let now = ByteSample {
        rx: 7 * MODULUS,
        rx_packets: 20,
        tx_packets: 20,
        ..before
    };
    assert_eq!(
        byte_delta(before, now, 60 * S),
        Ok(ByteDelta {
            rx: 3 * MODULUS,
            tx: 0
        })
    );
    assert_eq!(byte_delta(now, before, S), Err(DeltaError::Discontinuity));
}
