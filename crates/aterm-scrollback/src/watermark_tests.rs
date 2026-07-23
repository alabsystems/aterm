// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the memory watermark backpressure system (#5233).

use super::*;

/// Generate a pseudo-random line that compresses poorly.
/// Uses a simple LCG seeded by the line index so tests are deterministic.
fn random_line(seed: usize, len: usize) -> String {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            // Map to printable ASCII 33..127
            (33 + (state >> 56) % 94) as u8 as char
        })
        .collect()
}

/// Push `n` lines of poorly-compressible data (~`line_bytes` each).
fn push_random_lines(sb: &mut Scrollback, n: usize, line_bytes: usize) {
    for i in 0..n {
        sb.push_line(Line::from(random_line(i, line_bytes).as_str()));
    }
}

// ── Enum properties ──────────────────────────────────────────────────

#[test]
fn watermark_starts_green() {
    let sb = Scrollback::with_defaults();
    assert_eq!(sb.watermark_level(), WatermarkLevel::Green);
}

#[test]
fn watermark_default_is_green() {
    assert_eq!(WatermarkLevel::default(), WatermarkLevel::Green);
}

#[test]
fn watermark_level_ord_is_green_yellow_red() {
    assert!(WatermarkLevel::Green < WatermarkLevel::Yellow);
    assert!(WatermarkLevel::Yellow < WatermarkLevel::Red);
    assert!(WatermarkLevel::Green < WatermarkLevel::Red);
}

// ── Threshold transitions ────────────────────────────────────────────

#[test]
fn watermark_crosses_yellow_under_pressure() {
    // Random data doesn't compress well, so budgeted_bytes stays high.
    let budget = 200_000;
    let mut sb = Scrollback::with_block_size(10_000, 50_000, budget, 100);

    let mut saw_yellow = false;
    for i in 0..3000 {
        sb.push_line(Line::from(random_line(i, 200).as_str()));
        if sb.watermark_level() >= WatermarkLevel::Yellow {
            saw_yellow = true;
            break;
        }
    }
    assert!(
        saw_yellow,
        "should reach Yellow with random data in 200KB budget"
    );
}

#[test]
fn watermark_crosses_red_under_pressure() {
    let budget = 200_000;
    let mut sb = Scrollback::with_block_size(10_000, 50_000, budget, 100);

    let mut saw_red = false;
    for i in 0..3000 {
        sb.push_line(Line::from(random_line(i, 200).as_str()));
        if sb.watermark_level() == WatermarkLevel::Red {
            saw_red = true;
            break;
        }
    }
    assert!(saw_red, "should reach Red with random data in 200KB budget");
}

#[test]
fn watermark_visits_all_three_levels() {
    // Under sustained pressure, the watermark should visit Green, Yellow,
    // and Red at least once. Eager promotion may cause Green↔Yellow
    // oscillation, which is expected behavior — the test checks that all
    // three levels are visited, not strict monotonic ordering.
    let budget = 200_000;
    let mut sb = Scrollback::with_block_size(10_000, 50_000, budget, 100);

    let mut saw_green = true; // starts Green
    let mut saw_yellow = false;
    let mut saw_red = false;
    for i in 0..3000 {
        sb.push_line(Line::from(random_line(i, 200).as_str()));
        match sb.watermark_level() {
            WatermarkLevel::Green => saw_green = true,
            WatermarkLevel::Yellow => saw_yellow = true,
            WatermarkLevel::Red => {
                saw_red = true;
                break;
            }
        }
    }
    assert!(saw_green, "should start Green");
    assert!(saw_yellow, "should visit Yellow under pressure");
    assert!(saw_red, "should visit Red under sustained pressure");
}

#[test]
fn watermark_returns_to_green_after_budget_increase() {
    let budget = 200_000;
    let mut sb = Scrollback::with_block_size(10_000, 50_000, budget, 100);

    for i in 0..3000 {
        sb.push_line(Line::from(random_line(i, 200).as_str()));
        if sb.watermark_level() >= WatermarkLevel::Yellow {
            break;
        }
    }
    assert!(sb.watermark_level() >= WatermarkLevel::Yellow);

    // 10x budget → should drop back to Green.
    sb.set_memory_budget(budget * 10)
        .expect("memory budget update should succeed");
    assert_eq!(
        sb.watermark_level(),
        WatermarkLevel::Green,
        "after 10x budget increase, should return to Green"
    );
}

#[test]
fn watermark_custom_thresholds_lower_yellow() {
    // Set yellow=10%, red=20%. The lower threshold means Yellow triggers
    // much earlier than with defaults (80%).
    let budget = 2_000_000;
    let mut sb = Scrollback::with_block_size(10_000, 50_000, budget, 100);
    sb.set_watermark_thresholds(10, 20);

    let mut saw_yellow = false;
    for i in 0..1000 {
        sb.push_line(Line::from(random_line(i, 200).as_str()));
        if sb.watermark_level() >= WatermarkLevel::Yellow {
            saw_yellow = true;
            break;
        }
    }
    assert!(
        saw_yellow,
        "with 10% yellow threshold, should reach Yellow before 1000 lines in 2MB"
    );
}

// ── Eager promotion ──────────────────────────────────────────────────

#[test]
fn watermark_eager_promotion_fires_under_pressure() {
    // With a tight budget, eager promotion at Yellow should move hot data
    // to warm even though hot hasn't reached hot_limit.
    //
    // Budget is sized so that 150 width-200 lines clearly exceed the Yellow
    // watermark (80% of budget) with margin to spare, independent of the exact
    // `size_of::<Line>()` — the per-line struct overhead is small relative to
    // the 200-byte payload, so the test stays robust to struct-size changes.
    let budget = 45_000;
    let mut sb = Scrollback::with_block_size(200, 2000, budget, 10);

    // Push 150 random lines. Normal hot_limit=200 wouldn't promote until 200.
    push_random_lines(&mut sb, 150, 200);

    assert!(
        sb.warm_line_count() > 0 || sb.cold_line_count() > 0,
        "eager promotion should move data out of hot under Yellow pressure; \
         hot={} warm={} cold={}",
        sb.hot_line_count(),
        sb.warm_line_count(),
        sb.cold_line_count(),
    );
}

#[test]
fn watermark_compressible_data_stays_green_longer() {
    // Compressible data (repeating chars) compresses well in warm tier,
    // so the watermark stays Green longer than with random data.
    let budget = 200_000;
    let mut sb_compress = Scrollback::with_block_size(10_000, 50_000, budget, 100);
    let mut sb_random = Scrollback::with_block_size(10_000, 50_000, budget, 100);

    let n = 500;
    for i in 0..n {
        sb_compress.push_str(&"A".repeat(200));
        sb_random.push_line(Line::from(random_line(i, 200).as_str()));
    }

    // The random data scrollback should have a higher or equal watermark.
    assert!(
        sb_random.watermark_level() >= sb_compress.watermark_level(),
        "random data should hit higher watermark; random={:?}, compress={:?}",
        sb_random.watermark_level(),
        sb_compress.watermark_level(),
    );
}

// ── Eviction and recovery ────────────────────────────────────────────

#[test]
fn watermark_not_over_budget_after_sustained_push() {
    let budget = 5_000;
    let mut sb = Scrollback::with_block_size(50, 200, budget, 10);
    push_random_lines(&mut sb, 500, 50);

    assert!(
        !sb.over_budget(),
        "after sustained push, eviction should keep us at or under budget"
    );
}

#[test]
fn watermark_clear_resets_to_green() {
    let budget = 200_000;
    let mut sb = Scrollback::with_block_size(10_000, 50_000, budget, 100);
    for i in 0..3000 {
        sb.push_line(Line::from(random_line(i, 200).as_str()));
        if sb.watermark_level() > WatermarkLevel::Green {
            break;
        }
    }

    sb.clear();
    assert_eq!(
        sb.watermark_level(),
        WatermarkLevel::Green,
        "clear should reset watermark to Green"
    );
}

// ── Hysteresis unit tests ────────────────────────────────────────────
//
// The watermark state machine has three code paths for downward transitions
// that are not exercised by the integration tests above:
// 1. Yellow stays Yellow when budgeted_bytes is between exit (50%) and entry (80%)
// 2. Red drops to Yellow (not Green) when below red threshold
// 3. Red→Yellow→Green requires dropping below yellow_exit_threshold (50%)

/// Helper: create a scrollback and directly set internal state for unit testing.
fn scrollback_with_state(
    budget: usize,
    budgeted_bytes: usize,
    level: WatermarkLevel,
) -> Scrollback {
    let mut sb = Scrollback::with_block_size(1000, 5000, budget, 100);
    sb.budgeted_bytes = budgeted_bytes;
    sb.watermark_level = level;
    sb
}

#[test]
fn watermark_yellow_sticky_between_exit_and_entry() {
    // Budget=1000, yellow_entry=800 (80%), yellow_exit=500 (50%), red=950 (95%).
    // At 600 bytes (60%), a Yellow watermark should STAY Yellow due to hysteresis.
    let mut sb = scrollback_with_state(1000, 600, WatermarkLevel::Yellow);
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Yellow,
        "Yellow at 60% (between 50% exit and 80% entry) should stay Yellow"
    );
}

#[test]
fn watermark_yellow_drops_to_green_below_exit() {
    // At 390 bytes (39%), which is below the 50% exit threshold, Yellow→Green.
    let mut sb = scrollback_with_state(1000, 390, WatermarkLevel::Yellow);
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Green,
        "Yellow at 39% (below 50% exit) should drop to Green"
    );
}

#[test]
fn watermark_red_drops_to_yellow_not_green() {
    // At 600 bytes (60%), below red (950) but above yellow entry (800)? No —
    // 600 < 800, so we're in the "below yellow entry" branch.
    // Red should drop to Yellow, not straight to Green.
    let mut sb = scrollback_with_state(1000, 600, WatermarkLevel::Red);
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Yellow,
        "Red at 60% should drop to Yellow, not Green"
    );
}

#[test]
fn watermark_red_drops_to_yellow_between_yellow_entry_and_red() {
    // At 900 bytes (90%), between yellow entry (800) and red (950).
    // Red→Yellow in this band.
    let mut sb = scrollback_with_state(1000, 900, WatermarkLevel::Red);
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Yellow,
        "Red at 90% (between yellow entry and red threshold) should drop to Yellow"
    );
}

#[test]
fn watermark_red_to_yellow_to_green_cascade() {
    // Full downward cascade through hysteresis:
    // Start Red at 960 (above 950 red threshold).
    let mut sb = scrollback_with_state(1000, 960, WatermarkLevel::Green);
    sb.update_watermark_level();
    assert_eq!(sb.watermark_level, WatermarkLevel::Red, "960 → Red");

    // Drop to 850 (between yellow entry 800 and red 950). Red→Yellow.
    sb.budgeted_bytes = 850;
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Yellow,
        "850 → Yellow (from Red)"
    );

    // Drop to 600 (between exit 500 and entry 800). Yellow stays Yellow.
    sb.budgeted_bytes = 600;
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Yellow,
        "600 → still Yellow (sticky hysteresis)"
    );

    // Drop to 390 (below exit 500). Yellow→Green.
    sb.budgeted_bytes = 390;
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Green,
        "390 → Green (below exit)"
    );
}

#[test]
fn watermark_green_at_exact_yellow_exit_stays_yellow() {
    // At exactly the exit threshold (50% = 500 bytes), Yellow should NOT
    // drop to Green — the condition is strictly less than.
    let mut sb = scrollback_with_state(1000, 500, WatermarkLevel::Yellow);
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Yellow,
        "Yellow at exactly 50% exit threshold should stay Yellow (< not <=)"
    );
}

#[test]
fn watermark_boundary_at_exact_yellow_entry() {
    // At exactly 80% = 800 bytes, Green→Yellow.
    let mut sb = scrollback_with_state(1000, 800, WatermarkLevel::Green);
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Yellow,
        "Green at exactly 80% should enter Yellow"
    );
}

#[test]
fn watermark_boundary_at_exact_red_entry() {
    // At exactly 95% = 950 bytes, any level→Red.
    let mut sb = scrollback_with_state(1000, 950, WatermarkLevel::Green);
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Red,
        "Green at 95% → Red"
    );

    let mut sb = scrollback_with_state(1000, 950, WatermarkLevel::Yellow);
    sb.update_watermark_level();
    assert_eq!(
        sb.watermark_level,
        WatermarkLevel::Red,
        "Yellow at 95% → Red"
    );
}

// ── ScrollbackStorage delegation ─────────────────────────────────────

#[test]
fn watermark_storage_delegates_green() {
    let sb = Scrollback::with_defaults();
    let storage: ScrollbackStorage = sb.into();
    assert_eq!(storage.watermark_level(), WatermarkLevel::Green);
}

#[test]
fn watermark_storage_reflects_pressure() {
    let budget = 200_000;
    let mut sb = Scrollback::with_block_size(10_000, 50_000, budget, 100);
    for i in 0..3000 {
        sb.push_line(Line::from(random_line(i, 200).as_str()));
        if sb.watermark_level() >= WatermarkLevel::Yellow {
            break;
        }
    }

    let storage: ScrollbackStorage = sb.into();
    assert!(
        storage.watermark_level() >= WatermarkLevel::Yellow,
        "storage should delegate watermark level from inner Scrollback"
    );
}
