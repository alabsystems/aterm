// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Tiered-scrollback attachment and byte budgets for the wasm engine.

use aterm_core::terminal::scrollback_shared_budget::{
    set_global_scrollback_budget, ScrollbackBudgetShare,
};
use aterm_core::terminal::{Terminal, TerminalBuilder};

use crate::WebTerminal;

/// Staged lines promoted into the compressed store per `render()` frame.
pub(crate) const RENDER_DRAIN_BATCH_LINES: usize = 2_048;

/// Build a wasm terminal with default tiered scrollback and frame-boundary compression offload
/// enabled.
#[inline]
pub(crate) fn tiered_terminal(rows: u16, cols: u16) -> Terminal {
    let mut term = TerminalBuilder::new()
        .size(rows, cols)
        .tiered_scrollback_defaults()
        .build();
    term.set_compress_offload_active(true);
    // The facade drains the OSC app-event queue (`take_osc_events`).
    term.set_osc_event_polling(true);
    term
}

/// Register the freshly built pane in the module-global budget.
#[inline]
pub(crate) fn register_budget_share(term: &Terminal) -> ScrollbackBudgetShare {
    let configured = term
        .scrollback()
        .map_or(0, aterm_core::scrollback::ScrollbackStorage::memory_budget);
    ScrollbackBudgetShare::register(configured)
}

impl WebTerminal {
    /// Apply any pending global or per-pane scrollback-budget share to `self.term`.
    #[inline]
    pub(crate) fn apply_pending_scrollback_budget(&mut self) {
        if let Some(bytes) = self.budget_share.pending_effective() {
            let _ = self.term.set_memory_budget(bytes);
        }
    }

    /// Frame-boundary scrollback maintenance, called by `render()`.
    #[inline]
    pub(crate) fn drain_compress_backlog_on_render(&mut self) {
        self.apply_pending_scrollback_budget();
        if self.term.lazy_backlog_len() > 0 {
            self.term.trickle_lazy_bounded(RENDER_DRAIN_BATCH_LINES);
        }
    }
}

/// The JS-visible surface.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Set this pane's scrollback byte budget.
            pub fn set_scrollback_budget(&mut self, bytes: u32) {
                self.budget_share.set_configured(bytes as usize);
                self.apply_pending_scrollback_budget();
            }

            /// Set the MODULE-GLOBAL scrollback budget shared by every pane of this wasm
            /// module/worker.
            pub fn set_scrollback_global_budget(bytes: u32) {
                set_global_scrollback_budget(bytes as usize);
            }

            /// This pane's EFFECTIVE scrollback budget in bytes.
            pub fn scrollback_budget_effective(&self) -> u32 {
                u32::try_from(self.budget_share.effective()).unwrap_or(u32::MAX)
            }

            /// Bytes currently held by the tiered scrollback store.
            pub fn scrollback_memory_used(&self) -> u32 {
                let used = self.term.scrollback().map_or(
                    0,
                    aterm_core::scrollback::ScrollbackStorage::total_memory_used,
                );
                u32::try_from(used).unwrap_or(u32::MAX)
            }

            /// Promote up to `max_lines` staged lines into the compressed store
            /// (`0` = the render-frame batch size) and apply any pending global-share
            /// change.
            pub fn drain_scrollback_backlog(&mut self, max_lines: u32) -> u32 {
                self.apply_pending_scrollback_budget();
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

            /// Monotonic count of history lines lost to non-user-requested truncation.
            pub fn scrollback_truncated_lines(&self) -> f64 {
                self.term.scrollback_truncated_lines() as f64
            }

            /// Current scrollback memory-pressure watermark.
            pub fn scrollback_pressure(&self) -> u8 {
                match self.term.scrollback_pressure_level() {
                    aterm_core::scrollback::WatermarkLevel::Green => 0,
                    aterm_core::scrollback::WatermarkLevel::Yellow => 1,
                    _ => 2,
                }
            }

            /// BUILD-truth tier capabilities of this wasm module as one JSON object.
            pub fn tier_capabilities_json() -> String {
                render_tier_capabilities_json(aterm_core::scrollback::TierCapabilities::current())
            }
        }
    };
}
web_terminal_impl!(bind);

/// The pure JSON rendering.
fn render_tier_capabilities_json(caps: aterm_core::scrollback::TierCapabilities) -> String {
    let codec = match caps.cold_codec {
        aterm_core::scrollback::ColdTierCodec::Lz4 => "lz4",
    };
    format!(
        "{{\"coldCodec\":\"{codec}\",\"diskSpill\":{}}}",
        caps.disk_spill
    )
}

/// This crate's own tests of the surface.
#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "scrollback_tiers_api_own_tests.rs"]
mod tests;
