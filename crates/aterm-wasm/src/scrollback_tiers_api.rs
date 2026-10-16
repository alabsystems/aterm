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
//! and retention-accounted; under a sustained flood past the engine's staging
//! cap (what the store's memory budget has left, never under ~20k lines) the
//! OLDEST staged lines are dropped O(1) — the same deliberate
//! throughput-over-depth trade the native session makes, counted out of band
//! and marked in the history by one dim row at the cut. The per-frame drain
//! is a trickle OPPORTUNITY (`Terminal::trickle_lazy_bounded`): while the
//! flood is cutting and still delivering a batch or more per frame it skips,
//! since promoting the marker would only open a new cut behind it — one flood,
//! one marker.
//!
//! GLUE CONTRACT (for the embedding host): a pane that keeps processing while
//! its `render()` is throttled (hidden pane, occluded window) should call
//! [`WebTerminal::drain_scrollback_backlog`] on a coarse timer so retention
//! does not fall back to ring + staged cap; the call is a bounded no-op when
//! the backlog is empty.
//!
//! ## Budgets (per-pane + global)
//!
//! Every pane carries the store's per-pane byte budget
//! ([`WebTerminal::set_scrollback_budget`]); all panes of one wasm module
//! (one worker) additionally share the module-global budget
//! ([`WebTerminal::set_scrollback_global_budget`]) under the engine's
//! equal-share policy (`aterm_core::terminal::scrollback_shared_budget`,
//! spec-modeled): effective = `min(per-pane, global / live-panes)`, applied at
//! each pane's own render/drain points (bounded staleness, no cross-pane
//! locking).
//!
//! ## Build-truth tier capabilities
//!
//! This build's tier shape is what the CARGO FEATURES say, not what a caller
//! wishes: the wasm crates build `aterm-core` without `disk-tier` → RAM-only
//! store with LZ4 cold pages; native daemon builds may add the disk spill.
//! [`WebTerminal::tier_capabilities_json`] exposes that truth so a host
//! sizing budgets never assumes a disk spill on a RAM-only build.
//!
//! Shared with `aterm-gpu-web`, which mounts this file by `#[path]`;
//! [`WebTerminal`] is each crate's own terminal type.

use aterm_core::terminal::scrollback_shared_budget::{
    set_global_scrollback_budget, ScrollbackBudgetShare,
};
use aterm_core::terminal::{Terminal, TerminalBuilder};

use crate::WebTerminal;

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

impl WebTerminal {
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

            /// Monotonic count of history lines LOST to non-user-requested truncation
            /// (audit E10a): flood-backpressure staged-line drops the configured limit
            /// would have kept, reflow-window cap drops, and memory-pressure store
            /// evictions. The OUT-OF-BAND truncation signal: the host polls this (e.g.
            /// per frame settle) and surfaces the loss in its own chrome. A flood cut
            /// is ALSO marked in content, by one dim `— aterm dropped N lines here …`
            /// row where the lines are missing; store-pressure evictions take the
            /// oldest lines and leave no mark. `f64` because a sustained flood can
            /// outgrow `u32` (exact to 2^53).
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
/// crate compiling this shared file: the CPU and GPU hosts draw differently.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/scrollback_tiers_api_tests.rs"
    ));
}
