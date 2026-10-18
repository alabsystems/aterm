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

// ── Threshold transitions ────────────────────────────────────────────

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

// ── Hysteresis ───────────────────────────────────────────────────────

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

/// The hysteresis state machine at budget 1000: Yellow enters at 800 (80%)
/// and exits only BELOW 500 (50%, strictly less), Red enters at 950 (95%),
/// and Red steps down through Yellow, never straight to Green.
#[test]
fn watermark_hysteresis_transitions() {
    use WatermarkLevel::{Green, Red, Yellow};
    for (from, bytes, want) in [
        (Yellow, 600, Yellow), // sticky between exit and entry
        (Yellow, 500, Yellow), // exactly at exit: `<`, not `<=`
        (Yellow, 390, Green),  // below exit
        (Red, 900, Yellow),    // between yellow entry and red
        (Red, 600, Yellow),    // below yellow entry: still Yellow, not Green
        (Green, 800, Yellow),  // exactly at yellow entry
        (Green, 950, Red),     // exactly at red entry
        (Yellow, 950, Red),
        (Green, 960, Red),
    ] {
        let mut sb = scrollback_with_state(1000, bytes, from);
        sb.update_watermark_level();
        assert_eq!(sb.watermark_level, want, "{from:?} at {bytes}");
    }
}

/// THE THRESHOLDS ARE BOUNDED FOR EVERY BUDGET, and a fresh scrollback
/// starts Green: for every budget 0..=100_000, the yellow threshold never
/// exceeds the budget nor the red one, both are positive exactly from a
/// budget of 2 (80% and 95% of 1 truncate to 0, and a zero budget is zero),
/// and zero bytes against positive thresholds stays Green from Green through
/// the real (hysteretic) `recompute_watermark`.
#[test]
fn thresholds_are_bounded_for_every_budget() {
    for budget in 0..=100_000usize {
        let yellow = threshold_bytes(DEFAULT_YELLOW_PERCENT, budget);
        let red = threshold_bytes(DEFAULT_RED_PERCENT, budget);
        let exit = threshold_bytes(YELLOW_EXIT_PERCENT, budget);
        assert!(yellow <= budget && yellow <= red, "budget {budget}");
        assert_eq!(yellow == 0, budget <= 1, "budget {budget}");
        assert_eq!(red == 0, budget <= 1, "budget {budget}");
        if budget >= 2 {
            assert_eq!(
                recompute_watermark(WatermarkLevel::Green, 0, red, yellow, exit),
                WatermarkLevel::Green,
                "budget {budget}"
            );
        }
    }
}
