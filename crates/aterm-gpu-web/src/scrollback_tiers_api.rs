// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Tiered-scrollback attachment + byte budgets for the GPU wasm engine
//! (audit E1) — the GPU sibling of `aterm-wasm/src/scrollback_tiers_api.rs`,
//! which carries the full design rationale (frame-boundary compression per
//! SCROLL-1, the glue drain contract for render-throttled panes, the
//! equal-share global budget policy, build-truth capabilities). Kept
//! behavior-identical: the two web crates are separate wasm modules, so each
//! carries its own thin glue over the SHARED engine policy in
//! `aterm_core::terminal::scrollback_shared_budget` +
//! `TerminalBuilder::tiered_scrollback_defaults`.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use aterm_core::terminal::scrollback_shared_budget::{
    set_global_scrollback_budget, ScrollbackBudgetShare,
};
use aterm_core::terminal::{Terminal, TerminalBuilder};

use crate::AtermGpuTerminal;

/// Staged lines promoted (LZ4) into the store per rendered frame — same
/// sizing rationale as the CPU sibling (~120k lines/s at 60 fps, ~2k-line
/// bounded per-frame spike).
pub(crate) const RENDER_DRAIN_BATCH_LINES: usize = 2_048;

/// Build the wasm engine's terminal: the ENGINE-DEFAULT tiered store behind
/// the default hot ring, ingest-path compression handed to the frame boundary.
pub(crate) fn tiered_terminal(rows: u16, cols: u16) -> Terminal {
    let mut term = TerminalBuilder::new()
        .size(rows, cols)
        .tiered_scrollback_defaults()
        .build();
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

impl AtermGpuTerminal {
    /// Frame-boundary scrollback maintenance, called by `render` /
    /// `render_offscreen`: apply a changed global-budget share, then promote
    /// one bounded batch of staged lines into the store.
    // Why: both callers are wasm-only (they drive the GPU); native builds keep
    // the drain reachable via `drain_scrollback_backlog`.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn drain_compress_backlog_on_render(&mut self) {
        if let Some(bytes) = self.budget_share.pending_effective() {
            let _ = self.term.set_memory_budget(bytes);
        }
        if self.term.lazy_backlog_len() > 0 {
            self.term.drain_lazy_bounded(RENDER_DRAIN_BATCH_LINES);
        }
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermGpuTerminal {
    /// Set this pane's scrollback byte budget (the tiered store evicts oldest
    /// history to stay inside it). The module-global budget can only lower
    /// the effective value, never raise it past this.
    pub fn set_scrollback_budget(&mut self, bytes: u32) {
        self.budget_share.set_configured(bytes as usize);
        if let Some(effective) = self.budget_share.pending_effective() {
            let _ = self.term.set_memory_budget(effective);
        }
    }

    /// Set the MODULE-GLOBAL scrollback budget shared by every pane of this
    /// wasm module/worker (`0` = unlimited, the default).
    pub fn set_scrollback_global_budget(bytes: u32) {
        set_global_scrollback_budget(bytes as usize);
    }

    /// This pane's EFFECTIVE scrollback budget in bytes (per-pane budget
    /// after the module-global equal-share cap).
    pub fn scrollback_budget_effective(&self) -> u32 {
        u32::try_from(self.budget_share.effective()).unwrap_or(u32::MAX)
    }

    /// Bytes currently held by the tiered scrollback store (hot + warm + cold,
    /// including caches/overhead).
    pub fn scrollback_memory_used(&self) -> u32 {
        let used = self.term.scrollback().map_or(
            0,
            aterm_core::scrollback::ScrollbackStorage::total_memory_used,
        );
        u32::try_from(used).unwrap_or(u32::MAX)
    }

    /// Promote up to `max_lines` staged lines into the compressed store
    /// (`0` = the render-frame batch size). Returns the lines STILL staged.
    /// For hosts draining a pane whose render is throttled.
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
    /// (audit E10a, out-of-band — no sentinel content). See the aterm-wasm
    /// twin for the full contract.
    pub fn scrollback_truncated_lines(&self) -> f64 {
        self.term.scrollback_truncated_lines() as f64
    }

    /// Current scrollback memory-pressure watermark: 0 green / 1 yellow /
    /// 2 red (audit E10a co-land — pressure observable before loss begins).
    pub fn scrollback_pressure(&self) -> u8 {
        match self.term.scrollback_pressure_level() {
            aterm_core::scrollback::WatermarkLevel::Green => 0,
            aterm_core::scrollback::WatermarkLevel::Yellow => 1,
            _ => 2,
        }
    }

    /// BUILD-truth tier capabilities of this wasm module as one JSON object:
    /// `{"coldCodec":"lz4"|"zstd","diskSpill":bool}`.
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

    #[test]
    fn ctor_attaches_tiered_store_with_the_unified_default_total() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(6, 40, 14.0) else {
            return;
        };
        assert!(
            t.term.scrollback().is_some(),
            "E1: the GPU wasm ctor must attach the tiered store"
        );
        assert_eq!(
            t.term.scrollback_line_limit(),
            Some(aterm_core::scrollback::DEFAULT_LINE_LIMIT),
            "the construction default round-trips as ONE total"
        );
        // Ingest stages; the drain export promotes (the offscreen/GPU render
        // paths call the same hook).
        let mut buf = Vec::new();
        for i in 0..4_000 {
            buf.extend_from_slice(format!("gpu tier line {i}\r\n").as_bytes());
        }
        t.process(&buf);
        assert!(t.scrollback_backlog_lines() > 0, "flood ingest stages");
        while t.drain_scrollback_backlog(0) > 0 {}
        assert!(
            t.term
                .scrollback()
                .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count)
                > 0,
            "drained lines landed in the tiered store"
        );
    }

    /// The JSON wire contract the host glue parses: exact keys, lowercase codec
    /// tokens, JSON-lowercase booleans.
    ///
    /// This is a NATIVE test binary, and cargo unifies features per build — under
    /// `cargo test --workspace` the members that take `aterm-core` default-featured
    /// pull `disk-tier` (hence `zstd`) onto the single `aterm-scrollback` unit, so
    /// `TierCapabilities::current()` legitimately reads zstd/true here and lz4/false
    /// under `-p aterm-gpu-web`. A hard-coded literal therefore pinned the
    /// INVOCATION, not the contract, and had to be wrong for one of them.
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
            AtermGpuTerminal::tier_capabilities_json(),
            render_tier_capabilities_json(TierCapabilities::current()),
            "the exported method must report the capabilities of THIS build"
        );
    }
}
