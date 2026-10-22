// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Tiered-scrollback attachment and byte budgets for the wasm engine, shared
//! with `aterm-gpu-web` via `#[path]`.
//!
//! [`tiered_terminal`] builds a terminal with `tiered_scrollback_defaults()`
//! and `set_compress_offload_active(true)` so ingest only stages scrolled-off
//! lines while `render()` (or [`WebTerminal::drain_scrollback_backlog`] when
//! rendering is throttled) promotes a bounded batch per frame via
//! `Terminal::trickle_lazy_bounded`.
//!
//! Each pane carries a per-pane budget ([`WebTerminal::set_scrollback_budget`])
//! and shares a module-global budget
//! ([`WebTerminal::set_scrollback_global_budget`]) under
//! `aterm_core::terminal::scrollback_shared_budget` (`min(per-pane, global /
//! live-panes)`). [`WebTerminal::tier_capabilities_json`] reports the RAM-only
//! LZ4 tier configuration.

use aterm_core::terminal::scrollback_shared_budget::{
    set_global_scrollback_budget, ScrollbackBudgetShare,
};
use aterm_core::terminal::{Terminal, TerminalBuilder};

use crate::WebTerminal;

/// Staged lines promoted into the compressed store per `render()` frame.
pub(crate) const RENDER_DRAIN_BATCH_LINES: usize = 2_048;

/// Build a wasm terminal with default tiered scrollback and frame-boundary
/// compression offload enabled.
#[inline]
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
#[inline]
pub(crate) fn register_budget_share(term: &Terminal) -> ScrollbackBudgetShare {
    let configured = term
        .scrollback()
        .map_or(0, aterm_core::scrollback::ScrollbackStorage::memory_budget);
    ScrollbackBudgetShare::register(configured)
}

impl WebTerminal {
    /// Frame-boundary scrollback maintenance, called by `render()`: apply a
    /// changed global-budget share, then promote one bounded batch of staged
    /// lines into the store (which enforces the byte budget as it grows).
    #[inline]
    pub(crate) fn drain_compress_backlog_on_render(&mut self) {
        if let Some(bytes) = self.budget_share.pending_effective() {
            // Enforcement errors surface as watermark pressure, never a panic
            // on the frame path.
            let _ = self.term.set_memory_budget(bytes);
        }
        if self.term.lazy_backlog_len() > 0 {
            self.term.trickle_lazy_bounded(RENDER_DRAIN_BATCH_LINES);
        }
    }
}

/// The JS-visible surface. Each crate expands it onto its own terminal
/// type, under its own export attribute, with `web_terminal_impl!`: the
/// attribute names the JS class after the impl's type, which this shared
/// file cannot spell.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
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
            /// docs. Like the frame drain it is a trickle opportunity: it skips while
            /// a flood is cutting and delivered a batch or more since the previous
            /// call, so a host looping until `0` promotes on its next call.
            pub fn drain_scrollback_backlog(&mut self, max_lines: u32) -> u32 {
                if let Some(bytes) = self.budget_share.pending_effective() {
                    let _ = self.term.set_memory_budget(bytes);
                }
                let batch = if max_lines == 0 {
                    RENDER_DRAIN_BATCH_LINES
                } else {
                    max_lines as usize
                };
                u32::try_from(self.term.trickle_lazy_bounded(batch)).unwrap_or(u32::MAX)
            }

            /// Lines currently staged for promotion (the compress backlog).
            pub fn scrollback_backlog_lines(&self) -> u32 {
                u32::try_from(self.term.lazy_backlog_len()).unwrap_or(u32::MAX)
            }

            /// Monotonic count of history lines lost to non-user-requested truncation:
            /// flood-backpressure staged-line drops, reflow-window cap drops, and
            /// memory-pressure store evictions. `f64` because a sustained flood can
            /// outgrow `u32` (exact to 2^53).
            pub fn scrollback_truncated_lines(&self) -> f64 {
                self.term.scrollback_truncated_lines() as f64
            }

            /// Current scrollback memory-pressure watermark: 0 = green, 1 = yellow
            /// (eager compression active), 2 = red (throttle recommended).
            pub fn scrollback_pressure(&self) -> u8 {
                match self.term.scrollback_pressure_level() {
                    aterm_core::scrollback::WatermarkLevel::Green => 0,
                    aterm_core::scrollback::WatermarkLevel::Yellow => 1,
                    _ => 2,
                }
            }

            /// BUILD-truth tier capabilities of this wasm module as one JSON object:
            /// `{"coldCodec":"lz4","diskSpill":bool}`. Constant per build —
            /// the host brands budget math/telemetry with it instead of assuming.
            pub fn tier_capabilities_json() -> String {
                render_tier_capabilities_json(aterm_core::scrollback::TierCapabilities::current())
            }
        }
    };
}
web_terminal_impl!(bind);

/// The pure JSON rendering, split out of the exported method so the WIRE SHAPE
/// can be pinned for every capability combination rather than only the one this
/// build happens to select. Not exported, so the wasm ABI is unchanged.
fn render_tier_capabilities_json(caps: aterm_core::scrollback::TierCapabilities) -> String {
    let codec = match caps.cold_codec {
        aterm_core::scrollback::ColdTierCodec::Lz4 => "lz4",
    };
    format!(
        "{{\"coldCodec\":\"{codec}\",\"diskSpill\":{}}}",
        caps.disk_spill
    )
}

/// This crate's own tests of the surface, from `src/scrollback_tiers_api_tests.rs` of the
/// crate compiling this shared file (the CPU and GPU hosts draw differently),
/// and the tests both hosts share, written once below it.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/scrollback_tiers_api_tests.rs"
    ));

    // THE TESTS BOTH HOSTS SHARE: each crate's file above names its `Host` and
    // its `term()` (the shipped constructor on the bundled face); these are
    // written once.

    #[test]
    fn ctor_attaches_tiered_store_with_the_unified_default_total() {
        let t = term();
        assert!(
            t.term.scrollback().is_some(),
            "E1: the web ctor must attach the tiered store"
        );
        assert_eq!(
            t.term.scrollback_line_limit(),
            Some(aterm_core::scrollback::DEFAULT_LINE_LIMIT),
            "the construction default round-trips as ONE total"
        );
    }

    /// The JSON wire contract the host glue parses: exact keys, lowercase codec
    /// tokens, JSON-lowercase booleans.
    ///
    /// Every combination is pinned against the pure renderer, so no arm can go
    /// dead, and the exported method is checked to report this build.
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
            Host::tier_capabilities_json(),
            render_tier_capabilities_json(TierCapabilities::current()),
            "the exported method must report the capabilities of THIS build"
        );
    }
}
