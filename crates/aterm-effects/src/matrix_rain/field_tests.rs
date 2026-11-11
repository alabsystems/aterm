// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const fn params(rows: u32, tick_ms: u32, speed: u32, trail: u32) -> FieldParams {
    let (mq, dq, sq) = FieldParams::quanta(tick_ms, 133);
    FieldParams {
        seed32: rain_hash32(0xC0FF_EE00),
        rows,
        tick_ms,
        speed,
        trail,
        mq,
        dq,
        sq,
    }
}

/// Verify `c * p * tick_ms >= 1000` across clamped configs and viewport heights.
#[test]
fn flash_floor_holds_at_all_clamped_configs() {
    for rows in [3u32, 4, 5, 6, 7, 8, 50, 120] {
        for fps in [12u32, 30, 60] {
            for speed in [1u32, 5, 10] {
                for trail in [1u32, 5, 10] {
                    let fp = params(rows, 1000 / fps, speed, trail);
                    for col in 0..200 {
                        let cp = col_params(&fp, col);
                        assert!(cp.p >= 2, "a head steps at most every other tick");
                        assert!(
                            cp.c * cp.p * fp.tick_ms >= 1000,
                            "flash floor broken: rows={rows} fps={fps} speed={speed} \
                             trail={trail} col={col} c={} p={} tick_ms={}",
                            cp.c,
                            cp.p,
                            fp.tick_ms
                        );
                    }
                }
            }
        }
    }
}

/// Verify that 1–2-row viewports clamp cleanly to a 3-row virtual cycle.
#[test]
fn tiny_viewports_never_panic_and_keep_invariants() {
    for rows in [1u32, 2, 3] {
        for speed in [1u32, 5, 10] {
            for trail in [1u32, 5, 10] {
                let fp = params(rows, 83, speed, trail);
                for col in 0..100 {
                    let cp = col_params(&fp, col);
                    assert_eq!(cp.l, 3);
                    assert!(cp.p >= 2);
                    assert!(
                        cp.c * cp.p * fp.tick_ms >= 1000,
                        "flash floor broken at rows={rows}"
                    );
                }
            }
        }
    }
}

/// Verify that `dq` is a multiple of `mq` at or above the 350 ms floor.
#[test]
fn dither_quantum_rides_the_mutation_grid() {
    for tick_ms in [16u32, 33, 83] {
        for mutation_ms in [80u32, 133, 500, 2000] {
            let (mq, dq, _) = FieldParams::quanta(tick_ms, mutation_ms);
            assert_eq!(dq % mq, 0, "dq must be a multiple of mq");
            assert!(
                dq * tick_ms + tick_ms / 2 >= 350,
                "dq below the 350ms floor"
            );
        }
    }
}

#[test]
fn density_ring_looks_up_cycle_start() {
    let mut ring = DensityRing::default();
    assert_eq!(ring.at(100), 0, "empty ring is density 0");
    ring.push(10, 42);
    ring.push(50, 126);
    ring.push(90, 63);
    assert_eq!(ring.at(9), 42, "older than all → oldest retained");
    assert_eq!(ring.at(10), 42);
    assert_eq!(ring.at(60), 126);
    assert_eq!(ring.at(1000), 63);
    for i in 0..12u64 {
        ring.push(100 + i, i as u8);
    }
    assert_eq!(ring.at(u64::MAX), 11);
    assert_eq!(ring.at(0), 4, "evicted history resolves to the oldest kept");
}

#[test]
fn extreme_u32_inputs_never_panic() {
    assert_eq!(bits(0xDEAD_BEEF, 0, 32), 0xDEAD_BEEF);
    assert_eq!(bits(0xDEAD_BEEF, 16, 8), 0);
    assert_eq!(bits(0xDEAD_BEEF, 32, 64), 0);
    assert_eq!(ticks_of(u32::MAX, 2), u32::MAX / 2 + 1);
    let (mq, dq, sq) = FieldParams::quanta(1, u32::MAX);
    let fp = FieldParams {
        seed32: u32::MAX,
        rows: u32::MAX,
        tick_ms: u32::MAX,
        speed: u32::MAX,
        trail: u32::MAX,
        mq,
        dq,
        sq,
    };
    let cp = col_params(&fp, u32::MAX);
    assert!((2..=16).contains(&cp.p));
    assert!(u64::from(cp.phi) < u64::from(cp.c) * u64::from(cp.p));
}
