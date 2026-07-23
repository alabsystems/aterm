// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default-off env knobs for throughput MEASUREMENT (the cat-flood drain
//! investigation, docs/measured/arena/ISSUE-cat-flood-vs-ghostty-tip.md).
//! Bench instruments only — never product features. Every knob unset leaves
//! behavior byte-identical; each is read ONCE (`OnceLock`) at spawn/startup,
//! never per-chunk in a hot loop.

use std::sync::OnceLock;

/// `ATERM_FLOOD_QUIET=1`: skip SPAWNING the default-on effect/audio threads
/// (nyan-sprite loader worker, trail-audio worker and thus its CoreAudio
/// AudioQueue) so a flood measurement sees zero unrelated thread wakeups.
pub(crate) fn flood_quiet() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("ATERM_FLOOD_QUIET").is_ok_and(|v| v == "1"))
}

/// `ATERM_GATHER_SINK=drop`: the PTY gather thread counts + discards each
/// drained batch (no hand-off to the parse stage) — measures the pure
/// kernel→gather in-process drain ceiling.
// Call sites live in the unix gather/parse loops only.
#[cfg(unix)]
pub(crate) fn gather_sink_drop() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("ATERM_GATHER_SINK").is_ok_and(|v| v == "drop"))
}

/// `ATERM_PARSE_SINK=drop`: the parse stage recycles Data batches through the
/// normal free channel but skips engine ingest — isolates gather+channel+
/// recycle cost with parsing removed.
// Call sites live in the unix gather/parse loops only.
#[cfg(unix)]
pub(crate) fn parse_sink_drop() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("ATERM_PARSE_SINK").is_ok_and(|v| v == "drop"))
}

/// `ATERM_CAST_TAP=off`: skip the per-batch cast/byte-fanout tap (Arc alloc +
/// burst copy + writer wake) — prices the always-on recording tap. Bench only:
/// `cast` recordings and `bytes` subscribers see NOTHING while set (product
/// auto-gating is unsafe — the cast ring is an always-armed retro-capture
/// consumer; see aterm-gui cast.rs invariants).
pub(crate) fn cast_tap_off() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("ATERM_CAST_TAP").is_ok_and(|v| v == "off"))
}
