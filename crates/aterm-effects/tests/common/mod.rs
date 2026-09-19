// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared support for this crate's release-only perf gates
//! (`tests/cursor_bench.rs` and `tests/rainbow_kitty_v2_frame_cost.rs`): the
//! ONE budget-scale knob both read, and its pure parse. Each consuming binary
//! declares `mod common;` (the standard integration-test share: aterm-gpu
//! and aterm-toml carry a `tests/common/mod.rs`, aterm-render and aterm-spec a
//! `tests/common/` directory). The parse is pinned in exactly one binary,
//! `cursor_bench`, which has no counting allocator;
//! `rainbow_kitty_v2_frame_cost` counts every thread's allocations, so it
//! keeps its one non-ignored test. `tests/rain_bench.rs` (the matrix-rain
//! gates) keeps its own bars and does not read this knob.

/// The knob's name — one name across both gates, so a host's scale is set
/// once for every absolute wall-clock bound in the two binaries.
pub const BUDGET_SCALE_VAR: &str = "RK_FRAME_COST_BUDGET_SCALE";

/// `RK_FRAME_COST_BUDGET_SCALE=x` (a positive float, default `1.0`) multiplies
/// the ABSOLUTE wall-clock budgets of the two release-only perf gates —
/// §18's p50 400 µs / p90 600 µs pair in `rainbow_kitty_v2_frame_cost` (held
/// by the engine alone and through the seam), and the four p90/median bounds
/// in `cursor_bench` (500 / 500 / 1 000 / 2 000 µs) — and nothing else. §18
/// (docs/design/RAINBOW-KITTY-V2.md) says only that its pair is for "the
/// reference machine", which it does not name, and no x86_64 measurement is
/// on record, so the knob is a precaution for a host that has none, not a fix
/// for a known red: it widens (or tightens) absolute numbers only. Nothing
/// structural is scaled — not the frame-cost gate's zero-alloc, idle → zero
/// and cap laws, not cursor_bench's quad caps and shares, not the
/// fixture-emits checks: a red there is a finding about the engine, not host
/// noise.
pub fn budget_scale() -> f64 {
    parse_budget_scale(std::env::var(BUDGET_SCALE_VAR).ok().as_deref())
}

/// The scale's parse, kept pure so it can be pinned without touching the
/// process environment: unset, empty, unparsable, non-finite and non-positive
/// spellings all mean `1.0` — a typo widens nothing. The floor that keeps a
/// tiny scale from asserting `p50 ≤ 0 µs` lives in [`scaled_budget_us`].
pub fn parse_budget_scale(raw: Option<&str>) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0)
}

/// The absolute budget on this host: the reference number times the scale,
/// rounded to the microsecond and floored at 1 µs — at `1.0` the reference
/// number itself, exactly; and no scale, however small (`1e-9` rounds 400 µs
/// to 0), turns a gate into `p50 ≤ 0 µs`.
pub fn scaled_budget_us(reference_us: u64, scale: f64) -> u64 {
    (reference_us as f64 * scale).round().max(1.0) as u64
}
