// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Tiered-scrollback attachment + byte budgets for the wasm engine (audit E1).
//!
//! The wasm ctor was ring-only: deep history was raw uncompressed cells
//! (~640 B/line at 80 cols, content-independent) and the crate's LZ4 tiers,
//! DeferredLine lazy promotion, and off-thread-style history reflow all sat
//! dormant in the shipped renderer. The ctor now builds the ENGINE-DEFAULT
//! tiered terminal (`TerminalBuilder::tiered_scrollback_defaults`): a
//! 1000-line hot ring in front of the hot/warm(/cold) compressed store, ONE
//! total retention limit across ring + staged + store, and the store's byte
//! budget enforced by eviction.
//!
//! ## Compression stays off the ingest path (SCROLL-1)
//!
//! Inline LZ4 promotion on the PTY-drain path collapsed native cat-flood
//! throughput 193 → 59 MB/s (the SCROLL-1 regression), which is why native
//! sessions hand the lazy backlog to a worker thread. wasm has no worker, but
//! it has a frame boundary: the ctor arms `set_compress_offload_active(true)`
//! (ingest only STAGES scrolled-off lines — O(cells) snapshot, no codec) and
//! `render()` drains a bounded batch per frame. Staged lines remain readable
//! and retention-accounted; under a sustained flood past the engine's ~20k
//! staged-line cap the OLDEST staged lines are dropped O(1) — the same
//! deliberate throughput-over-depth trade the native session makes (surfaced
//! out-of-band, audit E10a).
//!
//! GLUE CONTRACT (for the embedding host): a pane that keeps processing while
//! its `render()` is throttled (hidden pane, occluded window) should call
//! [`AtermTerminal::drain_scrollback_backlog`] on a coarse timer so retention
//! does not fall back to ring + staged cap; the call is a bounded no-op when
//! the backlog is empty.
//!
//! ## Budgets (per-pane + global)
//!
//! Every pane carries the store's per-pane byte budget
//! ([`AtermTerminal::set_scrollback_budget`]); all panes of one wasm module
//! (one worker) additionally share the module-global budget
//! ([`AtermTerminal::set_scrollback_global_budget`]) under the engine's
//! equal-share policy (`aterm_core::terminal::scrollback_shared_budget`,
//! spec-modeled): effective = `min(per-pane, global / live-panes)`, applied at
//! each pane's own render/drain points (bounded staleness, no cross-pane
//! locking).
//!
//! ## Build-truth tier capabilities
//!
//! This build's cold-tier codec is what the CARGO FEATURES say, not what a
//! caller wishes: the wasm crates build `aterm-core` with default features →
//! LZ4-only store, no disk spill; native daemon builds may opt into
//! zstd/disk. [`AtermTerminal::tier_capabilities_json`] exposes that truth so
//! a host sizing budgets never assumes zstd ratios on an LZ4 build.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use aterm_core::terminal::scrollback_shared_budget::{
    set_global_scrollback_budget, ScrollbackBudgetShare,
};
use aterm_core::terminal::{Terminal, TerminalBuilder};

use crate::AtermTerminal;

/// Staged lines promoted (LZ4) into the store per `render()` frame. At 60 fps
/// this drains ~120k lines/s — far above interactive output rates, so the
/// backlog only accumulates during genuine floods — while bounding the
/// per-frame promotion spike to ~2k lines (≈ the reflow pump's budget class).
pub(crate) const RENDER_DRAIN_BATCH_LINES: usize = 2_048;

/// Build the wasm engine's terminal: the ENGINE-DEFAULT tiered store behind
/// the default hot ring, with ingest-path compression handed to the frame
/// boundary (see module docs).
pub(crate) fn tiered_terminal(rows: u16, cols: u16) -> Terminal {
    let mut term = TerminalBuilder::new()
        .size(rows, cols)
        .tiered_scrollback_defaults()
        .build();
    // Ingest stages; render() promotes. Without this the feeding call pays
    // inline LZ4 every ~1000 scrolled lines (the SCROLL-1 collapse).
    term.set_compress_offload_active(true);
    term
}

/// Register the freshly built pane in the module-global budget, configured at
/// the store's own construction-default byte budget.
pub(crate) fn register_budget_share(term: &Terminal) -> ScrollbackBudgetShare {
    let configured = term
        .scrollback()
        .map_or(0, aterm_core::scrollback::ScrollbackStorage::memory_budget);
    ScrollbackBudgetShare::register(configured)
}

impl AtermTerminal {
    /// Frame-boundary scrollback maintenance, called by `render()`: apply a
    /// changed global-budget share, then promote one bounded batch of staged
    /// lines into the store (which enforces the byte budget as it grows).
    pub(crate) fn drain_compress_backlog_on_render(&mut self) {
        if let Some(bytes) = self.budget_share.pending_effective() {
            // Enforcement errors surface as watermark pressure, never a panic
            // on the frame path.
            let _ = self.term.set_memory_budget(bytes);
        }
        if self.term.lazy_backlog_len() > 0 {
            self.term.drain_lazy_bounded(RENDER_DRAIN_BATCH_LINES);
        }
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermTerminal {
    /// Set this pane's scrollback byte budget (the tiered store evicts oldest
    /// history to stay inside it). The module-global budget can only lower
    /// the effective value, never raise it past this. `0` clamps to the
    /// engine's 1-byte floor (retain ~nothing) — pass the real budget.
    pub fn set_scrollback_budget(&mut self, bytes: u32) {
        self.budget_share.set_configured(bytes as usize);
        if let Some(effective) = self.budget_share.pending_effective() {
            let _ = self.term.set_memory_budget(effective);
        }
    }

    /// Set the MODULE-GLOBAL scrollback budget shared by every pane of this
    /// wasm module/worker (`0` = unlimited, the default): each pane's
    /// effective budget becomes `min(its own budget, global / live panes)`,
    /// applied as each pane is next rendered/drained.
    pub fn set_scrollback_global_budget(bytes: u32) {
        set_global_scrollback_budget(bytes as usize);
    }

    /// This pane's EFFECTIVE scrollback budget in bytes (per-pane budget
    /// after the module-global equal-share cap).
    pub fn scrollback_budget_effective(&self) -> u32 {
        u32::try_from(self.budget_share.effective()).unwrap_or(u32::MAX)
    }

    /// Bytes currently held by the tiered scrollback store (hot + warm + cold,
    /// including caches/overhead). Staged-but-unpromoted lines are not yet
    /// counted; `drain_scrollback_backlog` settles them.
    pub fn scrollback_memory_used(&self) -> u32 {
        let used = self.term.scrollback().map_or(
            0,
            aterm_core::scrollback::ScrollbackStorage::total_memory_used,
        );
        u32::try_from(used).unwrap_or(u32::MAX)
    }

    /// Promote up to `max_lines` staged lines into the compressed store
    /// (`0` = the render-frame batch size) and apply any pending global-share
    /// change. Returns the lines STILL staged. For hosts draining a pane
    /// whose `render()` is throttled — see the glue contract in the module
    /// docs.
    pub fn drain_scrollback_backlog(&mut self, max_lines: u32) -> u32 {
        if let Some(bytes) = self.budget_share.pending_effective() {
            let _ = self.term.set_memory_budget(bytes);
        }
        let batch = if max_lines == 0 {
            RENDER_DRAIN_BATCH_LINES
        } else {
            max_lines as usize
        };
        u32::try_from(self.term.drain_lazy_bounded(batch)).unwrap_or(u32::MAX)
    }

    /// Lines currently staged for promotion (the compress backlog).
    pub fn scrollback_backlog_lines(&self) -> u32 {
        u32::try_from(self.term.lazy_backlog_len()).unwrap_or(u32::MAX)
    }

    /// Monotonic count of history lines LOST to non-user-requested truncation
    /// (audit E10a): flood-backpressure staged-line drops, reflow-window cap
    /// drops, and memory-pressure store evictions. The OUT-OF-BAND truncation
    /// signal — the engine never injects a sentinel line into content; the
    /// host polls this (e.g. per frame settle) and surfaces the loss in its
    /// own chrome. `f64` because a sustained flood can outgrow `u32` (exact
    /// to 2^53).
    pub fn scrollback_truncated_lines(&self) -> f64 {
        self.term.scrollback_truncated_lines() as f64
    }

    /// Current scrollback memory-pressure watermark: 0 = green, 1 = yellow
    /// (eager compression active), 2 = red (throttle recommended) — the
    /// store's budget watermark, co-landed with the truncation counter
    /// (audit E10a) so budget pressure is observable before loss begins.
    pub fn scrollback_pressure(&self) -> u8 {
        match self.term.scrollback_pressure_level() {
            aterm_core::scrollback::WatermarkLevel::Green => 0,
            aterm_core::scrollback::WatermarkLevel::Yellow => 1,
            _ => 2,
        }
    }

    /// BUILD-truth tier capabilities of this wasm module as one JSON object:
    /// `{"coldCodec":"lz4"|"zstd","diskSpill":bool}`. Constant per build —
    /// the host brands budget math/telemetry with it instead of assuming.
    pub fn tier_capabilities_json() -> String {
        render_tier_capabilities_json(aterm_core::scrollback::TierCapabilities::current())
    }
}

/// The pure JSON rendering, split out of the exported method so the WIRE SHAPE
/// can be pinned for every capability combination rather than only the one this
/// build happens to select. Not exported, so the wasm ABI is unchanged.
fn render_tier_capabilities_json(caps: aterm_core::scrollback::TierCapabilities) -> String {
    let codec = match caps.cold_codec {
        aterm_core::scrollback::ColdTierCodec::Lz4 => "lz4",
        aterm_core::scrollback::ColdTierCodec::Zstd => "zstd",
    };
    format!(
        "{{\"coldCodec\":\"{codec}\",\"diskSpill\":{}}}",
        caps.disk_spill
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn term() -> Option<AtermTerminal> {
        AtermTerminal::new_from_system(6, 40, 14.0)
    }

    fn feed_lines(t: &mut AtermTerminal, n: usize) {
        let mut buf = Vec::new();
        for i in 0..n {
            buf.extend_from_slice(format!("tier line {i}\r\n").as_bytes());
        }
        t.process(&buf);
    }

    #[test]
    fn ctor_attaches_tiered_store_with_the_unified_default_total() {
        let Some(t) = term() else { return };
        assert!(
            t.term.scrollback().is_some(),
            "E1: the wasm ctor must attach the tiered store"
        );
        assert_eq!(
            t.term.scrollback_line_limit(),
            Some(aterm_core::scrollback::DEFAULT_LINE_LIMIT),
            "the construction default round-trips as ONE total"
        );
    }

    #[test]
    fn ingest_stages_and_render_promotes() {
        let Some(mut t) = term() else { return };
        // Well past the ring cap: scroll-off must STAGE (no inline promote).
        feed_lines(&mut t, 4_000);
        assert!(
            t.scrollback_backlog_lines() > 0,
            "flood ingest stages scrolled-off lines instead of compressing inline"
        );
        // Frames drain bounded batches until the backlog settles.
        for _ in 0..16 {
            t.render();
        }
        assert_eq!(
            t.scrollback_backlog_lines(),
            0,
            "render() drains the backlog"
        );
        assert!(
            t.term
                .scrollback()
                .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count)
                > 0,
            "drained lines landed in the tiered store"
        );
    }

    #[test]
    fn set_scrollback_limit_caps_the_total_across_tiers() {
        let Some(mut t) = term() else { return };
        t.set_scrollback_limit(1_500);
        feed_lines(&mut t, 5_000);
        while t.drain_scrollback_backlog(0) > 0 {}
        assert_eq!(
            t.term.scrollback_line_limit(),
            Some(1_500),
            "wasm export takes the ONE total"
        );
        assert_eq!(
            t.term.grid().scrollback_lines(),
            1_500,
            "ring + staged + store together honor the total"
        );
    }

    #[test]
    fn pane_budget_evicts_and_reports() {
        let Some(mut t) = term() else { return };
        feed_lines(&mut t, 3_000);
        while t.drain_scrollback_backlog(0) > 0 {}
        let before = t
            .term
            .scrollback()
            .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count);
        assert!(before > 0);
        // A tiny budget must evict most of the store, not error or grow.
        t.set_scrollback_budget(4_096);
        let after = t
            .term
            .scrollback()
            .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count);
        assert!(
            after < before,
            "budget shrink evicts oldest store lines ({before} -> {after})"
        );
        assert!(t.scrollback_budget_effective() <= 4_096);
    }

    #[test]
    fn global_budget_caps_the_effective_share() {
        let Some(mut t) = term() else { return };
        // Robust under parallel tests (other live panes shift the divisor):
        // the effective share can never exceed the global cap itself, and
        // restoring global=0 restores the configured budget.
        AtermTerminal::set_scrollback_global_budget(8_192);
        assert!(
            t.scrollback_budget_effective() <= 8_192,
            "global cap bounds the effective share"
        );
        t.render(); // applies the share on the frame path without panicking
        AtermTerminal::set_scrollback_global_budget(0);
        assert_eq!(
            t.scrollback_budget_effective() as usize,
            t.budget_share.configured().max(1),
            "unset global restores the configured per-pane budget"
        );
    }

    #[test]
    fn truncation_and_pressure_are_observable_out_of_band() {
        let Some(mut t) = term() else { return };
        assert_eq!(t.scrollback_truncated_lines(), 0.0);
        // Squeeze the pane budget, then keep feeding + draining: the store
        // must evict under pressure and the loss must surface in the counter
        // (E10a) — never as sentinel lines in content.
        t.set_scrollback_budget(4_096);
        feed_lines(&mut t, 3_000);
        while t.drain_scrollback_backlog(0) > 0 {}
        assert!(
            t.scrollback_truncated_lines() > 0.0,
            "budget-pressure evictions register out-of-band"
        );
        assert!(t.scrollback_pressure() <= 2, "pressure is the 0/1/2 scale");
        let oldest = t.row_text(0);
        if let Some(text) = oldest {
            assert!(
                text.is_empty() || text.starts_with("tier line"),
                "no sentinel content injected (got {text:?})"
            );
        }
    }

    /// The JSON wire contract the host glue parses: exact keys, lowercase codec
    /// tokens, JSON-lowercase booleans.
    ///
    /// This is a NATIVE test binary, and cargo unifies features per build — under
    /// `cargo test --workspace` the members that take `aterm-core` default-featured
    /// pull `disk-tier` (hence `zstd`) onto the single `aterm-scrollback` unit, so
    /// `TierCapabilities::current()` legitimately reads zstd/true here and lz4/false
    /// under `-p aterm-wasm`. A hard-coded literal therefore pinned the INVOCATION,
    /// not the contract. (The old comment here also had it backwards: this crate
    /// takes `aterm-core` with `default-features = false`.)
    ///
    /// So pin every combination against the pure renderer — no arm can go dead —
    /// and separately assert the exported method reports THIS build faithfully. The
    /// shipped module's lz4/no-spill shape is enforced structurally instead: the
    /// wasm builds are `-p`-scoped (`xtask gate web`, `tools/wasm-bench/run.sh`) and
    /// `disk-tier` drags in libc mmap + zstd-sys C, which cannot target
    /// wasm32-unknown-unknown — so it fails to BUILD long before any assertion.
    #[test]
    fn tier_capabilities_json_pins_the_wire_contract() {
        use aterm_core::scrollback::{ColdTierCodec, TierCapabilities};
        let json = |cold_codec, disk_spill| {
            render_tier_capabilities_json(TierCapabilities {
                cold_codec,
                disk_spill,
            })
        };
        assert_eq!(
            json(ColdTierCodec::Lz4, false),
            "{\"coldCodec\":\"lz4\",\"diskSpill\":false}"
        );
        assert_eq!(
            json(ColdTierCodec::Lz4, true),
            "{\"coldCodec\":\"lz4\",\"diskSpill\":true}"
        );
        assert_eq!(
            json(ColdTierCodec::Zstd, false),
            "{\"coldCodec\":\"zstd\",\"diskSpill\":false}"
        );
        assert_eq!(
            json(ColdTierCodec::Zstd, true),
            "{\"coldCodec\":\"zstd\",\"diskSpill\":true}"
        );
        assert_eq!(
            AtermTerminal::tier_capabilities_json(),
            render_tier_capabilities_json(TierCapabilities::current()),
            "the exported method must report the capabilities of THIS build"
        );
    }
}
