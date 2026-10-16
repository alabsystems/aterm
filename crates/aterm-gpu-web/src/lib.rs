// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
//
// `aterm-gpu-web` — the GPU rendering substrate for the Electron renderer.
//
// Sibling of `aterm-wasm`: the same engine front-end (`aterm-core`), but frames
// are drawn by `aterm-gpu` on WebGL2 straight into a `<canvas>` surface, with
// no CPU readback on the present path. WebGPU is refused.
//
// Init is async: a browser cannot block its main thread, so adapter and device
// acquisition is awaited (`wasm_bindgen_futures`); the surface comes from the
// `HtmlCanvasElement` via `aterm_gpu::web::CanvasTarget::Canvas`. `render` draws the grid via
// `present_input`; `render_offscreen` plus `rgba`/`width`/`height` read the
// framebuffer back so an e2e harness can pixel-compare GPU against CPU.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

// The terminal type the binding modules shared by aterm-wasm and
// aterm-gpu-web extend (aterm-gpu-web mounts them by `#[path]`).
use crate::AtermGpuTerminal as WebTerminal;

// Expands a shared module's `bind!` — its exported impl — onto this crate's
// terminal under `#[wasm_bindgen]` (aterm-wasm passes aterm-wasm-abi's
// `export`). wasm-bindgen names the JS class after the impl's type, a bare
// identifier, so the name is brought into the shared module here.
macro_rules! web_terminal_impl {
    ($bind:ident) => {
        use crate::AtermGpuTerminal;
        #[cfg(target_arch = "wasm32")]
        use wasm_bindgen::prelude::wasm_bindgen;
        $bind!(AtermGpuTerminal, wasm_bindgen);
    };
}

#[path = "../../aterm-wasm/src/effects_api.rs"]
mod effects_api;
#[path = "../../aterm-wasm/src/facade_api.rs"]
mod facade_api;
mod host;
#[path = "../../aterm-wasm/src/messages_api.rs"]
mod messages_api;
#[path = "../../aterm-wasm/src/notifications_api.rs"]
mod notifications_api;
#[path = "../../aterm-wasm/src/predict_api.rs"]
mod predict_api;
#[path = "../../aterm-wasm/src/scroll_input_api.rs"]
mod scroll_input_api;
#[path = "../../aterm-wasm/src/scrollback_tiers_api.rs"]
mod scrollback_tiers_api;

use aterm_core::grid::{PendingScrollbackReflow, ReflowStep};
use aterm_core::selection::SmartSelection;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{BlockText, ClipboardAccess, CursorStyle, MouseMode, Rgb, Terminal};
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::{RenderInput, Renderer, SpillBand, Theme};

// ---------------------------------------------------------------------------
// Cooperative (thread-free) width-reflow offload, as in `aterm-wasm`: `resize`
// detaches the tiered history in O(1) and the rewrap runs in later
// budget-bounded tasks. Each `pump_reflow` (host-called, or a render-grace /
// output-backlog safety net) advances one `reflow_step` slice, so no single
// event-loop task grows with session history.
// ---------------------------------------------------------------------------

/// Histories at or below this many lines are rewrapped inline by `resize`.
/// Matches aterm-gui's and aterm-wasm's `INLINE_REFLOW_MAX_LINES`.
const INLINE_REFLOW_MAX_LINES: usize = 20_000;

/// Safety net for a host that never pumps: after this many `render` /
/// `render_offscreen` calls with a stashed rewrap (~2 s at 60 fps), each frame
/// runs one budgeted step.
const REFLOW_PUMP_GRACE_RENDERS: u32 = 120;

/// Safety net for a host that never renders or pumps: past this lazy-buffer
/// backlog, every `process` call pumps one budgeted step, so the rewrap
/// converges in short tasks.
const REFLOW_BACKLOG_MAX_LINES: usize = 20_000;

/// Default per-pump rewrap budget in input history lines (tunable via
/// [`AtermGpuTerminal::pump_reflow_budget`]); a few milliseconds of work.
const REFLOW_STEP_BUDGET_LINES: usize = 2_000;

// GpuContext is used only by the wasm async init path (`init`); on the native
// target (a compile-verification surface only) it would be unused.
#[cfg(target_arch = "wasm32")]
use aterm_gpu::GpuContext;
#[cfg(any(target_arch = "wasm32", test))]
use aterm_gpu::SurfacePresentFailure;
use aterm_gpu::{GpuRenderer, GpuSurface, WindowGpu};

std::thread_local! {
    /// Host-registered font blobs, copied across the JS/wasm boundary once per
    /// module. Engines reference them by `u32` handle, so large face blobs are
    /// never re-copied (or left fragmenting linear memory) per pane.
    static REGISTERED_FONTS: std::cell::RefCell<Vec<std::sync::Arc<Vec<u8>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Register a font blob for handle-based reuse by every engine in this module.
/// Content-interned: identical bytes return a handle to one shared copy.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn register_font(bytes: &[u8]) -> u32 {
    let arc = aterm_render::intern_font_bytes_slice(bytes);
    REGISTERED_FONTS.with(|cell| {
        let mut store = cell.borrow_mut();
        if let Some(i) = store.iter().position(|a| std::sync::Arc::ptr_eq(a, &arc)) {
            return i as u32;
        }
        store.push(arc);
        (store.len() - 1) as u32
    })
}

fn registered_font(handle: u32) -> Result<std::sync::Arc<Vec<u8>>, String> {
    REGISTERED_FONTS.with(|cell| {
        cell.borrow()
            .get(handle as usize)
            .cloned()
            .ok_or_else(|| format!("unregistered font handle {handle}"))
    })
}

/// Construction is split to match the browser lifecycle:
///   1. [`AtermGpuTerminal::new`] — synchronous: engine grid plus a CPU face
///      from injected font bytes (cell metrics, glyph atlas). No GPU yet.
///   2. [`AtermGpuTerminal::init`] — async: acquire the GPU and configure the
///      canvas surface. If WebGL is unavailable the host can fall back to the
///      CPU path (`aterm-wasm`) without having built anything GPU-side.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct AtermGpuTerminal {
    term: Terminal,
    // CPU face: owns the glyph rasterizer + cell metrics. Reused for cols/rows
    // sizing here, and handed to the GPU renderer to build the glyph atlas.
    cpu: Renderer,
    rows: usize,
    cols: usize,
    theme: Theme,
    // Read only by the wasm GPU paths (`init` rebuilds the face from these).
    font_bytes: Vec<u8>,
    // GPU side: None until `init` succeeds. Once set, `render` presents on the GPU;
    // the host wires `render` into a requestAnimationFrame loop.
    gpu: Option<GpuState>,
    // Offscreen readback cache: the last `render_offscreen` frame as RGBA8
    // (width*height*4 bytes), for GPU-vs-CPU pixel comparison.
    #[cfg(target_arch = "wasm32")]
    rgba: Vec<u8>,
    #[cfg(target_arch = "wasm32")]
    fb_width: usize,
    #[cfg(target_arch = "wasm32")]
    fb_height: usize,
    // Built-in smart-selection rules (url/file_path/email/...) for link
    // detection via `smart_word_at`; reused across `link_at` calls.
    smart: SmartSelection,
    // Host-injected OS fallback faces (CJK/symbols + colour emoji), kept so
    // `init` can re-apply them to the fresh GPU CPU face it builds from
    // `font_bytes`. Interned Arc shared across panes. Empty until
    // `set_fallback_font` / `set_emoji_font`.
    fallback_font: Option<std::sync::Arc<Vec<u8>>>,
    // Additional fallback faces from `add_fallback_font` (most-preferred first),
    // re-applied by `init`. Interned Arcs shared across panes.
    fallback_chain_extra: Vec<std::sync::Arc<Vec<u8>>>,
    emoji_font: Option<std::sync::Arc<Vec<u8>>>,
    // Host-injected bold face, re-applied by `init` to the fresh GPU CPU face.
    // Interned Arc. None until `set_bold_font`.
    bold_font: Option<std::sync::Arc<Vec<u8>>>,
    // Host-injected symbol fallback face, re-applied by `init`. Interned Arc.
    // None until `set_symbol_font`.
    symbol_font: Option<std::sync::Arc<Vec<u8>>>,
    // Host-selected text shaping (ligatures + OpenType `font_features`), applied
    // to the CPU and live GPU faces and re-applied by `init` so a setting chosen
    // before init survives the face rebuild.
    text_shaping: aterm_render::TextShapingConfig,
    // Reused per-frame engine snapshot for `render` and `render_offscreen`,
    // refilled in place via `cell_frame_damage_scoped_into` (damaged rows only
    // when the continuity proof holds, full refill otherwise). Unread on the
    // native verification target.
    frame_scratch: RenderInput,
    // Test builds only: which arm the last `refill_frame_scratch` took
    // (`Scoped`, `Full` with the refusing clause, or `None` before the first).
    #[cfg(test)]
    last_refill: Option<aterm_core::render::FrameRefill>,
    // The shared visual-effects pipeline (cursor aurora/trail, sparkle words),
    // host-clocked via `advance_effects`. Off by default, in which case `apply`
    // only clears the empty overlay channels.
    pub(crate) effects: EffectsPipeline,
    // Theme cursor colour (0x00RRGGBB): the default the glow/trail colours
    // derive from when the host passes none (the native resolution).
    pub(crate) theme_cursor: u32,
    // Theme colours needed by the backend-neutral PHOSPHOR resolver.
    pub(crate) theme_fg: u32,
    pub(crate) theme_bg: u32,
    // Bounded OSC 9/99/777 notification queue, fed by engine callbacks and
    // drained by `take_notifications`.
    pub(crate) notifications: notifications_api::NotificationQueue,
    // The message band: the message engine's rows, composed above the grid by
    // the renderer's `band` module. With no committed row a frame is
    // byte-identical to a band-less one.
    pub(crate) band: messages_api::WebBand,
    // Sub-row scroll accumulator for fractional/pixel wheel deltas: whole rows
    // go to `scroll_display`, the residual presents as a band shift.
    pub(crate) scroll_input: scroll_input_api::ScrollInputState,
    // Predictive local echo (the shared `aterm-predict` state machine). Mode
    // Off by default, fully inert until `set_predictive_echo`.
    pub(crate) predict: aterm_predict::Predictor,
    // Scratch row for the predictive-echo reconcile probe: `Predictor::reconcile`
    // observes the same row once per retired guess, and `render_row_into`
    // refills this buffer instead of allocating and re-resolving each time.
    pub(crate) pred_row_scratch: Vec<aterm_core::terminal::RenderCell>,
    // Chrome-band spill rasterizer (cross-pane window-space effects export),
    // refreshed per `render`/`render_offscreen` from the CPU face's geometry.
    // Integer CPU math, identical on both engines; identity at 0/0 chrome.
    pub(crate) spill: SpillBand,
    // A deferred width-change scrollback rewrap: `Some` while the tiered
    // history is detached awaiting pumps. Carries its own progress and drops
    // with the engine.
    pending_reflow: Option<PendingScrollbackReflow>,
    // Render-call countdown for safety net #1 (armed to
    // REFLOW_PUMP_GRACE_RENDERS when a job is stashed).
    reflow_grace: u32,
    // Per-pump `reflow_step` budget in input lines (REFLOW_STEP_BUDGET_LINES
    // unless the host tuned it via `pump_reflow_budget`).
    reflow_budget: usize,
    // Single-slot cache of the last display row read cell by cell (`cell_text`
    // / `cell_is_wide`). A scrolled-back row is materialized from scrollback,
    // so without it a per-cell walk costs O(cols²) per row. Keyed so any write,
    // resize or scroll invalidates it.
    display_row_cache: std::cell::RefCell<GpuDisplayRowCache>,
    // This pane's membership in the module-global scrollback byte budget;
    // the share is applied at frame/drain boundaries.
    pub(crate) budget_share: aterm_core::terminal::scrollback_shared_budget::ScrollbackBudgetShare,
    // Host half of the frame-gate key: bumped by every wasm-layer mutator of
    // renderer-held or presentation state that the engine's damage epoch cannot
    // see. `open_frame` reads it, so a mutator that forgets to bump serves a
    // stale frame. Over-bumping costs one frame. Pinned by
    // `every_visual_mutator_bumps_the_host_visual_generation`.
    host_visual_gen: u64,
    // Gate key of the last frame built and presented. `None` means the canvas
    // cannot be assumed to hold any frame of ours: after construction, `init`,
    // a failed present, or an offscreen readback.
    last_frame_key: Option<FrameGateKey>,
    // `scroll_frac_px` of the last presented frame. Releasing a sub-row
    // residual moves every pixel without grid damage, so the gate renders one
    // more frame after a nonzero residual.
    last_present_frac: i32,
    // `last_render_skipped()` — the gate's two-sided reach witness for tests
    // and benches.
    last_render_gated: bool,
    // Last blink phase handed to the renderer, so re-asserting the same phase
    // does not force a render. `None` until first set.
    blink_phase_shadow: Option<bool>,
    // Shadow of the last hollow-cursor override — same de-dup contract.
    hollow_shadow: Option<bool>,
}

/// Frame-gate key: every input of a rendered frame that can change between
/// two `render()` calls at stable dims/config. Grid content folds into
/// `damage_epoch`, host visual state into `host_visual_gen`. An active effects
/// pipeline never skips, and the active->idle edge changes the key so the
/// cleared overlay channels get painted once. Identical to `aterm-wasm`'s key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FrameGateKey {
    damage_epoch: u64,
    host_visual_gen: u64,
    effects_active: bool,
}

/// The single-slot per-row display-cell cache backing `cell_text`/`cell_is_wide`.
#[derive(Default)]
struct GpuDisplayRowCache {
    /// `(alt_screen, content_gen, display_offset, row)` the cached cells were
    /// resolved at. Main and alt grids keep independent `content_gen` counters,
    /// so the alt-screen bit is needed to invalidate on a screen swap.
    key: Option<(bool, u64, usize, u16)>,
    /// `(grapheme, is_wide)` per display column of `key.row`.
    cells: Vec<(String, bool)>,
}

/// Keep the engine's implicit cells and OSC 110/111/112/117 reset baselines
/// aligned with the renderer theme. Sparse row tails and live selection colours
/// come from `Terminal`, so changing only the CPU/GPU renderer theme would be
/// overwritten on the next frame refill.
fn apply_terminal_theme_colors(term: &mut Terminal, fg: u32, bg: u32, cursor: u32, selection: u32) {
    let rgb = |color: u32| Rgb {
        r: ((color >> 16) & 0xff) as u8,
        g: ((color >> 8) & 0xff) as u8,
        b: (color & 0xff) as u8,
    };
    term.set_default_foreground(rgb(fg));
    term.set_default_background(rgb(bg));
    term.set_default_cursor_color(Some(rgb(cursor)));
    term.set_default_selection_background(Some(rgb(selection)));
}

/// The GPU half of the terminal, populated by [`AtermGpuTerminal::init`].
struct GpuState {
    renderer: GpuRenderer,
    surface: GpuSurface,
    // Per-window present state (prior-frame snapshot for the scissored
    // dirty-row present). Drives `present_input` and `render_input`.
    win: WindowGpu,
}

/// Convert the GPU crate's typed swapchain failure into the string error
/// exposed across wasm-bindgen; target-neutral so native tests can prove a
/// dropped present never becomes `Ok(())`.
#[cfg(any(target_arch = "wasm32", test))]
fn webgl_present_result(result: Result<(), SurfacePresentFailure>) -> Result<(), String> {
    result.map_err(|failure| format!("WebGL canvas present failed: {failure:?}"))
}

#[cfg(any(target_arch = "wasm32", test))]
impl AtermGpuTerminal {
    /// The CPU face `init` assembles the GPU renderer on: built from the
    /// injected font bytes at the live face's px, with no system font
    /// discovery on the web (E1), and carrying every face and setting the host
    /// applied before init, which a fresh face lacks. The band's parity test
    /// draws on this same face.
    fn gpu_face(&self) -> Result<Renderer, String> {
        let mut cpu = Renderer::from_bytes(&self.font_bytes, self.cpu.px(), self.theme)?;
        cpu.set_runtime_font_discovery(false);
        // Re-apply any fonts the host injected: the fresh face above is built
        // from `font_bytes` alone, so it lacks them otherwise.
        if let Some(bytes) = self.fallback_font.as_deref() {
            cpu.set_fallback_bytes(bytes)?;
        }
        // Re-append the rest of the chain IN ORDER after the reset base above, so
        // the multi-script coverage the host built before init survives the rebuild.
        for bytes in &self.fallback_chain_extra {
            cpu.add_fallback_bytes(bytes)?;
        }
        if let Some(arc) = self.emoji_font.as_ref() {
            // Retained field is ALREADY an interned Arc, so install the SHARED blob
            // directly. The `_bytes` twin would deep-copy the ~180MB colour face only
            // for its own `intern_font_bytes` to memcmp it against this very entry and
            // drop it — same validation, same `install_color_font`, same Arc installed,
            // minus one full alloc+memcpy+memcmp per pane (wasm memory never shrinks).
            cpu.set_color_font_arc(std::sync::Arc::clone(arc))?;
        }
        // Re-apply a bold face injected before init (the fresh face lacks it).
        if let Some(bytes) = self.bold_font.as_deref() {
            cpu.set_bold_font(bytes)?;
        }
        // Re-apply a symbol face injected before init (the fresh face lacks it).
        if let Some(bytes) = self.symbol_font.as_deref() {
            cpu.set_symbol_fallback_bytes(bytes)?;
        }
        // Read normalized settings from the live CPU face, including rejected
        // requests that left the last valid setting in place.
        cpu.set_line_height(self.cpu.line_height());
        // Re-apply the host's text shaping (ligatures + font_features): the fresh face
        // above starts at TextShapingConfig::default(), so a setting chosen before init
        // would otherwise be lost — same retention contract as the fallback/emoji faces.
        cpu.set_text_shaping(self.text_shaping.clone());
        // Re-apply a minimum-contrast floor set before init (the fresh face
        // defaults to off) — same retention contract as text shaping above.
        cpu.set_minimum_contrast(self.cpu.minimum_contrast());
        // Re-apply background/cursor opacity set before init (the fresh face
        // defaults to opaque) — same retention contract as min-contrast above.
        cpu.set_background_opacity(self.cpu.background_opacity());
        cpu.set_cursor_opacity(self.cpu.cursor_opacity());
        cpu.set_cursor_blink_phase(self.cpu.cursor_blink_phase());
        cpu.set_cursor_style_override(self.cpu.cursor_style_override());
        cpu.set_selection_fg(self.cpu.selection_fg());
        cpu.set_selection_inactive(self.cpu.selection_inactive());
        cpu.set_selection_inactive_bg(self.cpu.selection_inactive_bg());
        // Re-apply window-chrome set before init (set_chrome; the fresh face
        // defaults to the exact-fit frame) — init's frame_size then sizes the
        // swapchain to the padded frame. Same retention contract as above.
        cpu.set_pad(self.cpu.pad());
        cpu.set_head(self.cpu.head());
        Ok(cpu)
    }
}

impl AtermGpuTerminal {
    /// Frame gate: run the two frame-boundary pumps, then decide whether this
    /// tick draws. `Some(key)` = build and present, then record `key`; `None` =
    /// identical to the last presented frame, so skip build and present.
    ///
    /// The pumps run first so a reflow re-attach's full damage shows in the
    /// epoch. Gate terms:
    /// - `damage_epoch`: advances iff the grid changed since the last build.
    /// - `host_visual_gen`: bumped by every renderer/presentation mutator.
    /// - `effects_active`: an active pipeline never skips; the active->idle
    ///   edge buys one settle frame.
    /// - frac terms: a pending or just-released sub-row translate re-presents.
    /// - `pending_reflow`: always draws.
    ///
    /// Skipping is safe on WebGL because an untouched canvas keeps its last
    /// image (the key is dropped whenever it might not). A gated tick returns
    /// before `present_input`, so aterm-gpu's prior-frame bookkeeping
    /// (`WindowGpu::prev_input`, `PresentPrev`) still describes the frame on
    /// glass and the next scissored present diffs against it correctly.
    fn open_frame(&mut self) -> Option<FrameGateKey> {
        // Deferred-reflow safety net #1: a host that never calls `pump_reflow`
        // still re-attaches its rewrapped history within the grace window.
        self.pump_reflow_on_render_tick();
        // Frame-boundary scrollback maintenance: apply a pending global-budget
        // share, then promote one bounded staged batch (LZ4 stays off ingest).
        self.drain_compress_backlog_on_render();
        let key = FrameGateKey {
            damage_epoch: self.term.damage_epoch(),
            host_visual_gen: self.host_visual_gen,
            effects_active: self.effects.is_active(),
        };
        let cell_h = self.cpu.cell_size().1;
        if !key.effects_active
            && self.pending_reflow.is_none()
            && self.last_present_frac == 0
            && self.scroll_input.frac_px(cell_h) == 0
            && self.last_frame_key == Some(key)
        {
            self.last_render_gated = true;
            return None;
        }
        self.last_render_gated = false;
        Some(key)
    }

    /// Record the frame `build_frame` just produced as the one on glass. The
    /// epoch in `key` was latched before `take_damage`, which re-arms but never
    /// changes the epoch value, so an idle successor matches.
    fn record_presented_frame(&mut self, key: FrameGateKey) {
        self.last_frame_key = Some(key);
        self.last_present_frac = self.frame_scratch.scroll_frac_px;
    }

    /// Build one frame into the kept scratch; shared by `render`,
    /// `render_offscreen` and the native `render_headless` seam.
    ///
    /// Order (as in `aterm-wasm`): engine refill -> damage consume -> effects
    /// overlay channels -> sub-row scroll stamp -> chrome spill export. Spill
    /// runs before any GPU access so exports stay coherent when a present fails.
    /// The frame-boundary pumps live in `open_frame`.
    fn build_frame(&mut self) {
        // Refill the kept scratch in place, re-resolving only damaged rows when
        // the continuity proof holds. The refill consumes the damage session so
        // the next change advances the epoch `open_frame` compares; consuming
        // leaves the epoch value alone, so the effects pipeline's
        // `term.damage_epoch() == input.snapshot_seq` check still holds.
        // First take last frame's band rows off the scratch so a scoped refill
        // is possible; refused (full refill) when nothing was prepended.
        let _ = self.band.undo(&mut self.frame_scratch, self.rows);
        self.refill_frame_scratch();
        let rows = self.rows;
        // Fill the overlay channels (aurora/trail/sparkle); with every effect
        // off this only clears channels the reused scratch may carry.
        let (cw, ch) = self.cpu.cell_size();
        // The pipeline must know the band's height to read the pointer against
        // the grid where this frame draws it.
        self.hand_band_to_effects();
        self.effects
            .apply(&mut self.term, &mut self.frame_scratch, cw, ch);
        // Present the banked sub-row scroll residual as a band shift. Stamped
        // every frame so the kept scratch never carries a stale shift;
        // `compose_band_into_frame` then moves the grid band below the message
        // rows.
        self.scroll_input.stamp(&mut self.frame_scratch, rows, ch);
        // Prepend the message band's committed rows, translate window-space
        // streams down with them and hand their gutters to both faces.
        self.compose_band_into_frame();
        // Refresh the chrome-band spill export before any GPU access. The
        // fingerprint short-circuit makes a second same-frame pass free.
        self.spill.update(&self.cpu, &self.frame_scratch);
    }

    /// Refill every engine-owned frame channel and consume the frame's damage
    /// session through the engine's one damage consumer,
    /// [`Terminal::cell_frame_damage_scoped_into`]. Under its continuity proof
    /// only damaged rows re-resolve; otherwise it refills fully. The implicit
    /// background, cursor and selection colours are restamped every time so
    /// the snapshot stays coherent. The effects pipeline only closes a damage
    /// session still open, so it does not re-take this one.
    fn refill_frame_scratch(&mut self) {
        let refill =
            self.term
                .cell_frame_damage_scoped_into(&mut self.frame_scratch, self.rows, self.cols);
        #[cfg(test)]
        {
            self.last_refill = Some(refill);
        }
        #[cfg(not(test))]
        let _ = refill;
    }

    /// Record a host-visible visual change the engine's damage epoch cannot
    /// see (renderer-held or presentation state), reopening the frame gate for
    /// one frame. Unconditional callers simply buy one render.
    pub(crate) fn note_host_visual_change(&mut self) {
        self.host_visual_gen = self.host_visual_gen.wrapping_add(1);
    }

    /// Band host hook: the CPU face's cell, pad and grid-top metrics (the face
    /// the GPU renderer is built from, so both agree).
    pub(crate) fn band_metrics(&self) -> messages_api::BandMetrics {
        let (cell_w, cell_h) = self.cpu.cell_size();
        messages_api::BandMetrics {
            cell_w,
            cell_h,
            pad: self.cpu.pad(),
            grid_top: self.cpu.grid_top(),
        }
    }

    /// Set the band's gutters on the CPU face (read by spill and the offscreen
    /// path) and on the live GPU renderer's inner face.
    pub(crate) fn set_band_bleed(&mut self, bleed: Option<aterm_render::ChromeBleed>) {
        self.cpu.set_chrome_bleed(bleed);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_chrome_bleed(bleed);
        }
    }

    /// The rows the swapchain is sized for: the grid's and the band's above
    /// it (the frame grows by the band, as the CPU twin's does).
    fn surface_rows(&self) -> usize {
        self.rows + self.band.committed()
    }

    /// The band changed: reopen the frame gate for one frame and, when its row
    /// count moved, resize the swapchain to the new frame height.
    pub(crate) fn note_band_change(&mut self, rows_moved: bool) {
        self.note_host_visual_change();
        if rows_moved {
            self.size_swapchain();
        }
    }

    /// Size the live swapchain to the frame [`Self::build_frame`] composes:
    /// [`Self::surface_rows`] (grid plus band rows) at the renderer's pad and
    /// head. The one sizing site after init (`resize`, `set_chrome`, band row
    /// moves), so the surface is never shorter than the frame.
    fn size_swapchain(&mut self) {
        let rows = self.surface_rows();
        if let Some(gpu) = self.gpu.as_mut() {
            let (w, h) = gpu.renderer.frame_size(rows, self.cols);
            gpu.renderer
                .resize_surface(&mut gpu.win, &mut gpu.surface, w as u32, h as u32);
        }
    }

    /// Read one `(grapheme, is_wide)` display cell through the single-slot row
    /// cache, refreshed from [`display_row_grapheme_cells`] only when the key
    /// changes. `None` for an out-of-range row/col.
    fn with_display_row_cell<T>(
        &self,
        row: u16,
        col: u16,
        read: impl FnOnce(&(String, bool)) -> T,
    ) -> Option<T> {
        let key = (
            self.term.is_alternate_screen(),
            self.term.grid().content_gen(),
            self.term.grid().display_offset(),
            row,
        );
        let mut cache = self.display_row_cache.borrow_mut();
        if cache.key != Some(key) {
            match self.term.display_row_grapheme_cells(row as usize) {
                Some(cells) => {
                    cache.cells = cells;
                    cache.key = Some(key);
                }
                None => {
                    cache.key = None;
                    cache.cells.clear();
                    return None;
                }
            }
        }
        cache.cells.get(col as usize).map(read)
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermGpuTerminal {
    /// Build a `rows`x`cols` terminal. `font_bytes` (TTF/OTF) is supplied by
    /// the host; there is no font discovery on wasm. `px` is the cell font size;
    /// `fg`/`bg`/`cursor`/`selection` (0x00RRGGBB) seed the default theme.
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(
        rows: u16,
        cols: u16,
        font_bytes: &[u8],
        px: f32,
        fg: u32,
        bg: u32,
        cursor: u32,
        selection: u32,
    ) -> Result<AtermGpuTerminal, String> {
        #[cfg(target_arch = "wasm32")]
        aterm_wasm_abi::panic_hook::set_once(host::CONSOLE);
        // The wasm artifact is its own root, the binary of the web terminal: it
        // stamps its version here (XTVERSION reads it), and only on wasm32, so
        // the native compile of this library carries no build fact.
        #[cfg(target_arch = "wasm32")]
        aterm_types::version::install(aterm_types::version::BuildStamp {
            app_version: env!("CARGO_PKG_VERSION"),
            ..aterm_types::version::BuildStamp::UNSTAMPED
        });
        let theme = Theme {
            fg,
            bg,
            cursor,
            selection,
        };
        // Build the CPU face now (cheap, GPU-independent) so cell metrics are
        // available before WebGL init and the host can size the canvas.
        let mut cpu = Renderer::from_bytes(font_bytes, px, theme)?;
        // No filesystem on the web: a real miss surfaces via
        // `take_missing_font_classes` (E1 lazy fonts), never system discovery.
        cpu.set_runtime_font_discovery(false);
        // The engine clamps rows/cols to 1..=4096; store the clamped dims, since
        // the GPU framebuffer is sized from `self.rows`/`self.cols` and an
        // unclamped copy could request an oversized texture. The tiered store
        // is attached here; compression drains at the render frame boundary.
        let mut term = scrollback_tiers_api::tiered_terminal(rows, cols);
        let budget_share = scrollback_tiers_api::register_budget_share(&term);
        apply_terminal_theme_colors(&mut term, fg, bg, cursor, selection);
        // Poll-drain surface for OSC 9/99/777 (a web host has no callback
        // thread); authorization keeps the engine's fail-closed default.
        let notifications = notifications_api::wire_notification_queue(&mut term);
        let rows = term.grid().rows() as usize;
        let cols = term.grid().cols() as usize;
        Ok(Self {
            budget_share,
            term,
            cpu,
            rows,
            cols,
            theme,
            font_bytes: font_bytes.to_vec(),
            gpu: None,
            #[cfg(target_arch = "wasm32")]
            rgba: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            fb_width: 0,
            #[cfg(target_arch = "wasm32")]
            fb_height: 0,
            smart: SmartSelection::with_builtin_rules(),
            fallback_font: None,
            fallback_chain_extra: Vec::new(),
            emoji_font: None,
            bold_font: None,
            symbol_font: None,
            text_shaping: aterm_render::TextShapingConfig::default(),
            frame_scratch: RenderInput::empty(),
            #[cfg(test)]
            last_refill: None,
            effects: EffectsPipeline::new(),
            theme_cursor: cursor & 0x00FF_FFFF,
            theme_fg: fg & 0x00FF_FFFF,
            theme_bg: bg & 0x00FF_FFFF,
            notifications,
            band: messages_api::WebBand::new(aterm_time::Instant::now(), fg, bg, cursor),
            scroll_input: scroll_input_api::ScrollInputState::default(),
            predict: aterm_predict::Predictor::default(),
            pred_row_scratch: Vec::new(),
            spill: SpillBand::new(),
            pending_reflow: None,
            reflow_grace: 0,
            reflow_budget: REFLOW_STEP_BUDGET_LINES,
            display_row_cache: std::cell::RefCell::new(GpuDisplayRowCache::default()),
            host_visual_gen: 0,
            last_frame_key: None,
            last_present_frac: 0,
            last_render_gated: false,
            blink_phase_shadow: None,
            hollow_shadow: None,
        })
    }

    /// Inject a broad-coverage (CJK + symbols) fallback face from font bytes, so
    /// glyphs the primary face lacks render real shapes instead of `.notdef` tofu.
    /// Applies to the CPU face (metrics) and the live GPU face if `init` already
    /// ran; the bytes are also remembered so `init` re-applies them to the fresh
    /// GPU face it builds. Throws on a bad blob; the existing faces stay.
    pub fn set_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_fallback_bytes(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_fallback_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        // Borrowed-slice intern: allocates only for a genuinely new blob, so
        // panes injecting the same OS face copy nothing.
        self.fallback_font = Some(aterm_render::intern_font_bytes_slice(bytes));
        // set_* RESETS the chain, so any previously appended extras are gone.
        self.fallback_chain_extra.clear();
        Ok(())
    }

    /// Append another fallback face to the chain (unlike [`set_fallback_font`],
    /// which resets it), e.g. CJK then Arabic/Devanagari/Thai/Hebrew. Applies
    /// to the CPU face and the live GPU face; remembered so `init` re-applies
    /// the chain. Throws on a bad blob; the chain stays.
    pub fn add_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.add_fallback_bytes(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.add_fallback_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        // Borrowed intern (see `set_fallback_font`): no copy once the blob is known.
        self.fallback_chain_extra
            .push(aterm_render::intern_font_bytes_slice(bytes));
        Ok(())
    }

    /// Inject a colour-emoji (sbix) face from font bytes, driving the existing
    /// ColorEmoji colour path. Same wiring as [`set_fallback_font`]. Throws on a
    /// bad blob; the existing face stays.
    pub fn set_emoji_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
                                        // Intern once and install/retain the shared Arc. `set_color_font_arc`
                                        // runs the same validation as the byte setter, so a bad blob still
                                        // throws (after landing in the intern store, as with `register_font`).
        let arc = aterm_render::intern_font_bytes_slice(bytes);
        self.cpu.set_color_font_arc(arc.clone())?;
        if let Some(gpu) = self.gpu.as_mut() {
            // The live GPU face takes its own copy (no Arc setter on
            // GpuRenderer); rare, since fonts are usually seeded before `init`.
            gpu.renderer.set_emoji_font_bytes(bytes.to_vec())?;
            gpu.win.invalidate_present();
        }
        self.emoji_font = Some(arc);
        Ok(())
    }

    /// Inject a real bold weight of the primary family so SGR-bold cells use it
    /// instead of synthetic embolden. Applies to the CPU and live GPU faces;
    /// remembered for `init`. Throws on a bad blob; the existing weight stays.
    pub fn set_bold_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_bold_font(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_bold_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        // Borrowed intern (see `set_fallback_font`): no copy once the blob is known.
        self.bold_font = Some(aterm_render::intern_font_bytes_slice(bytes));
        Ok(())
    }

    /// Inject a symbol fallback face from font bytes (the byte-injection
    /// counterpart of the config `symbol_font`). Applies to the CPU and live
    /// GPU faces; remembered for `init`. Throws on a bad blob.
    pub fn set_symbol_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_symbol_fallback_bytes(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_symbol_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        // Borrowed intern (see `set_fallback_font`): no copy once the blob is known.
        self.symbol_font = Some(aterm_render::intern_font_bytes_slice(bytes));
        Ok(())
    }

    // ── registered-font (handle) variants ───────────────────────────────────
    // Take a `register_font` handle and store the shared Arc directly, so panes
    // injecting the same OS faces copy nothing across the JS/wasm boundary.

    /// [`AtermGpuTerminal::set_fallback_font`] from a registered handle.
    pub fn set_fallback_font_registered(&mut self, handle: u32) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        let bytes = registered_font(handle)?;
        self.cpu.set_fallback_bytes(&bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_fallback_font_bytes(&bytes)?;
            gpu.win.invalidate_present();
        }
        self.fallback_font = Some(bytes);
        // set_* RESETS the chain, so any previously appended extras are gone.
        self.fallback_chain_extra.clear();
        Ok(())
    }

    /// [`AtermGpuTerminal::add_fallback_font`] from a registered handle.
    pub fn add_fallback_font_registered(&mut self, handle: u32) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        let bytes = registered_font(handle)?;
        self.cpu.add_fallback_bytes(&bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.add_fallback_font_bytes(&bytes)?;
            gpu.win.invalidate_present();
        }
        self.fallback_chain_extra.push(bytes);
        Ok(())
    }

    /// [`AtermGpuTerminal::set_emoji_font`] from a registered handle. The CPU
    /// face gets the shared interned copy; a live GPU face still receives its
    /// own copy (rare: fonts are usually seeded before `init`).
    pub fn set_emoji_font_registered(&mut self, handle: u32) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        let bytes = registered_font(handle)?;
        self.cpu.set_color_font_arc(bytes.clone())?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_emoji_font_bytes(bytes.as_ref().clone())?;
            gpu.win.invalidate_present();
        }
        self.emoji_font = Some(bytes);
        Ok(())
    }

    /// [`AtermGpuTerminal::set_bold_font`] from a registered handle.
    pub fn set_bold_font_registered(&mut self, handle: u32) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        let bytes = registered_font(handle)?;
        self.cpu.set_bold_font(&bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_bold_font_bytes(&bytes)?;
            gpu.win.invalidate_present();
        }
        self.bold_font = Some(bytes);
        Ok(())
    }

    /// [`AtermGpuTerminal::set_symbol_font`] from a registered handle.
    pub fn set_symbol_font_registered(&mut self, handle: u32) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        let bytes = registered_font(handle)?;
        self.cpu.set_symbol_fallback_bytes(&bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_symbol_font_bytes(&bytes)?;
            gpu.win.invalidate_present();
        }
        self.symbol_font = Some(bytes);
        Ok(())
    }

    /// Swap the primary face (the host's `terminalFontFamily`) and
    /// re-rasterize on the CPU and live GPU faces. The bytes replace
    /// `font_bytes` so a later `init` uses them. The host re-reads cell metrics
    /// and resizes after. Throws on a bad blob; the existing face stays.
    pub fn set_primary_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_primary_font(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_primary_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        // Future `init` calls must build the GPU CPU face from the NEW family.
        self.font_bytes = bytes.to_vec();
        Ok(())
    }

    /// Scale the cell box height (`terminalLineHeight`) without changing glyph
    /// px, on the CPU and live GPU faces; remembered for `init`. The host
    /// re-reads `cell_height` and resizes after.
    pub fn set_line_height(&mut self, scale: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_line_height(scale);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_line_height(scale);
            gpu.win.invalidate_present();
        }
    }

    /// Programming ligatures on/off (`=>`, `!=`, `===` …), like the native
    /// `ligatures` knob. Applies to the CPU and live GPU faces; remembered for
    /// `init`. Preserves any configured `font_features`.
    pub fn set_ligatures(&mut self, on: bool) {
        self.note_host_visual_change(); // WF-1 gate
        self.text_shaping.ligature_mode = if on {
            aterm_render::LigatureMode::Enabled
        } else {
            aterm_render::LigatureMode::Disabled
        };
        self.cpu.set_text_shaping(self.text_shaping.clone());
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_text_shaping(self.text_shaping.clone());
            gpu.win.invalidate_present();
        }
    }

    /// OpenType font features for the primary face as a space-separated spec
    /// (`"+ss01 zero -calt"`), like the native `font_features` knob; blank
    /// clears. Applies to both faces; remembered for `init`. Preserves the
    /// ligature mode.
    pub fn set_font_features(&mut self, spec: &str) {
        self.note_host_visual_change(); // WF-1 gate
        let features = aterm_render::parse_font_features(spec);
        self.text_shaping.font_features = if features.is_empty() {
            Vec::new()
        } else {
            vec![aterm_render::FontFeatureSet {
                font_id: 0,
                features,
            }]
        };
        self.cpu.set_text_shaping(self.text_shaping.clone());
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_text_shaping(self.text_shaping.clone());
            gpu.win.invalidate_present();
        }
    }

    /// Set palette colour `index` (0–255; 0–15 are the ANSI colours) so
    /// SGR-indexed cells resolve through the host's theme. The palette lives on
    /// the shared grid, so both GPU and CPU paths see it.
    pub fn set_palette_color(&mut self, index: u8, r: u8, g: u8, b: u8) {
        self.note_host_visual_change(); // WF-1 gate
        self.term.set_palette_color_components(index, r, g, b);
        // The band borrows slots 1, 3, 4 and 6 (its error, warn and meter hues).
        self.band.set_palette_color(index, [r, g, b]);
    }

    /// Set the cursor blink phase on the live GPU renderer and the CPU face.
    /// Value-shadowed, so a host timer re-asserting the same phase does not
    /// defeat the frame gate.
    pub fn set_cursor_blink_phase(&mut self, on: bool) {
        if self.blink_phase_shadow != Some(on) {
            self.blink_phase_shadow = Some(on);
            self.note_host_visual_change();
        }
        self.cpu.set_cursor_blink_phase(on);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_cursor_blink_phase(on);
        }
    }

    /// Force a hollow (unfocused) cursor when `true`, or restore the terminal's
    /// DECSCUSR style when `false`, on both faces. Value-shadowed like the
    /// blink phase.
    pub fn set_cursor_hollow(&mut self, hollow: bool) {
        if self.hollow_shadow != Some(hollow) {
            self.hollow_shadow = Some(hollow);
            self.note_host_visual_change();
        }
        let style = if hollow {
            Some(CursorStyle::HollowBlock)
        } else {
            None
        };
        self.cpu.set_cursor_style_override(style);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_cursor_style_override(style);
        }
    }

    /// Drain the missing-font class bits (1 = text/mono fallback, 2 = colour
    /// emoji) accumulated since the last call. Misses land on the pre-init
    /// metrics face and on the GPU renderer's inner CPU face, so both are
    /// drained and OR-ed.
    pub fn take_missing_font_classes(&mut self) -> u8 {
        let mut classes = self.cpu.take_missing_font_classes();
        if let Some(gpu) = self.gpu.as_mut() {
            classes |= gpu.renderer.take_missing_font_classes();
        }
        classes
    }

    /// Seed the engine's DEFAULT foreground/background so OSC 10/11 colour-query
    /// replies report the host theme. RGB components, 0–255.
    pub fn set_default_foreground(&mut self, r: u8, g: u8, b: u8) {
        self.note_host_visual_change(); // WF-1 gate
        self.term.set_default_foreground(Rgb { r, g, b });
    }

    pub fn set_default_background(&mut self, r: u8, g: u8, b: u8) {
        self.note_host_visual_change(); // WF-1 gate
        self.term.set_default_background(Rgb { r, g, b });
    }

    /// Replace the default fg/bg/cursor/selection theme (0x00RRGGBB) on the GPU
    /// renderer and the CPU face without a device or face rebuild.
    pub fn set_theme(&mut self, fg: u32, bg: u32, cursor: u32, selection: u32) {
        self.note_host_visual_change(); // WF-1 gate
                                        // Keep the effects' derive-from-theme default in sync (glow/trail colours
                                        // passed as `None` follow the cursor colour, like the native app).
        self.theme_cursor = cursor & 0x00FF_FFFF;
        self.theme_fg = fg & 0x00FF_FFFF;
        self.theme_bg = bg & 0x00FF_FFFF;
        self.effects
            .set_matrix_rain_theme(self.theme_bg, self.theme_fg, self.theme_cursor);
        // The pet's contrast sample belongs to the old theme; drop it while the
        // pet's behaviour, position and breed carry on.
        self.effects.invalidate_companion_colors();
        let theme = Theme {
            fg,
            bg,
            cursor,
            selection,
        };
        apply_terminal_theme_colors(&mut self.term, fg, bg, cursor, selection);
        self.theme = theme;
        self.cpu.set_theme(theme);
        // The band's inks derive from the theme (the next frame repaints it).
        self.band.set_theme(fg, bg, cursor);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_theme(theme);
            // Force a full repaint: selection band, idle cursor and padding
            // border are theme-derived, so the dirty-row diff would miss them.
            gpu.win.invalidate_present();
        }
    }

    /// Explicit selected-text foreground (0x00RRGGBB), or `undefined` for the
    /// WCAG contrast-floor default. Set on both faces; forces a full present.
    pub fn set_selection_fg(&mut self, fg: Option<u32>) {
        self.note_host_visual_change(); // WF-1 gate
        self.term
            .set_default_selection_foreground(fg.map(|color| Rgb {
                r: ((color >> 16) & 0xff) as u8,
                g: ((color >> 8) & 0xff) as u8,
                b: (color & 0xff) as u8,
            }));
        self.cpu.set_selection_fg(fg);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_selection_fg(fg);
            gpu.win.invalidate_present();
        }
    }

    /// Per-cell minimum contrast ratio (xterm's `minimumContrastRatio`,
    /// 1..=21; `<= 1.0` is off, the default): each glyph fg is floored against
    /// its own cell bg. Cells with fg == bg (SGR 8 conceal) are never adjusted.
    /// Set on both faces; forces a full present.
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_minimum_contrast(ratio);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_minimum_contrast(ratio);
            gpu.win.invalidate_present();
        }
    }

    /// Default-background opacity (0..=1, like Ghostty's `background-opacity`;
    /// `1.0` is opaque and the default). Only pixels whose bg is the frame's
    /// default background go translucent. Set on both faces; forces a full
    /// present. On glass this needs an alpha-compositing canvas; the offscreen
    /// readback carries alpha either way.
    pub fn set_background_opacity(&mut self, opacity: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_background_opacity(opacity);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_background_opacity(opacity);
            gpu.win.invalidate_present();
        }
    }

    /// Window chrome for window-space effects: interior padding (`pad`, px per
    /// edge) plus a top rise band (`head`, px), laid out `[head][pad][grid][pad]`.
    /// The swapchain resizes to the padded frame (the host offsets the canvas by
    /// `-pad,-(pad+head)`) and effect emissions become window-absolute. Set on
    /// both faces; `0/0` (default) is byte-identical to the exact-fit frame.
    pub fn set_chrome(&mut self, pad: u16, head: u16) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_pad(pad as usize);
        self.cpu.set_head(head as usize);
        self.effects.set_chrome(pad, head);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_pad(pad as usize);
            gpu.renderer.set_head(head as usize);
        }
        // The padded frame holds the band's rows too.
        self.size_swapchain();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.win.invalidate_present();
        }
    }

    /// The chrome interior padding from [`Self::set_chrome`] (px; 0 = exact
    /// fit), read from the CPU face.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn chrome_pad(&self) -> u16 {
        self.cpu.pad() as u16
    }

    /// The chrome top head band set via [`Self::set_chrome`] (px; 0 = none).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn chrome_head(&self) -> u16 {
        self.cpu.head() as u16
    }

    /// Cursor-fill opacity (0..=1, like Ghostty's `cursor-opacity`; `1.0` is
    /// the default opaque fill with block cut-out). Below 1.0 the glyph shows
    /// through. Set on both faces; forces a full present.
    pub fn set_cursor_opacity(&mut self, opacity: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_cursor_opacity(opacity);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_cursor_opacity(opacity);
            gpu.win.invalidate_present();
        }
    }

    /// Mark the pane unfocused (`true`) or focused (`false`); unfocused paints
    /// the selection with the inactive bg (xterm
    /// `selectionInactiveBackground`). Set on both faces; forces a full present.
    pub fn set_selection_inactive(&mut self, inactive: bool) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_selection_inactive(inactive);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_selection_inactive(inactive);
            gpu.win.invalidate_present();
        }
    }

    /// Set the inactive (unfocused) selection bg (0x00RRGGBB), or `undefined` to
    /// derive it from the active selection bg blended toward the theme bg. Set on
    /// both the CPU fallback face and the live GPU renderer; forces a full present.
    pub fn set_selection_inactive_bg(&mut self, bg: Option<u32>) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_selection_inactive_bg(bg);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_selection_inactive_bg(bg);
            gpu.win.invalidate_present();
        }
    }

    /// Re-rasterize at a new cell font px (DPI change) on both faces; the GPU
    /// renderer also drops its atlas. The host re-reads cell metrics and
    /// resizes after.
    pub fn set_px(&mut self, px: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_px(px);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_px(px);
            gpu.win.invalidate_present();
        }
    }

    /// Resize the grid and, if the GPU is live, the swapchain.
    ///
    /// The visible grid, bounded ring and swapchain resize synchronously. A
    /// width change with a deep tiered history detaches it in O(1)
    /// (`resize_offloading_scrollback`) and rewraps it in later budgeted tasks
    /// (see [`Self::pump_reflow`]); histories up to `INLINE_REFLOW_MAX_LINES`
    /// rewrap inline.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        // A reconfigured swapchain's contents are undefined, so the canvas no
        // longer holds the frame `last_frame_key` describes; bump always, even
        // for a same-dims call.
        self.note_host_visual_change();
        if let Some(pending) = self.term.resize_offloading_scrollback(rows, cols) {
            if pending.line_count() <= INLINE_REFLOW_MAX_LINES {
                // Small-history fast path: rewrap now. A follow-up job cannot
                // arise on this single-threaded path, but one would wedge the
                // detach window if dropped, so route it to the pump.
                if let Some(follow) = self.term.finish_resize_offload(pending.reflow()) {
                    self.pending_reflow = Some(follow);
                    self.reflow_grace = REFLOW_PUMP_GRACE_RENDERS;
                }
            } else {
                // Stash for a later host turn. Overwriting a stashed job only
                // happens after a reset/erase re-created the store, and the
                // clear_gen guard would discard the old job anyway.
                self.pending_reflow = Some(pending);
                self.reflow_grace = REFLOW_PUMP_GRACE_RENDERS;
            }
        }
        // Re-sync to the clamped grid dims (1..=4096), not the raw args: the GPU
        // surface is sized from these fields.
        self.rows = self.term.grid().rows() as usize;
        self.cols = self.term.grid().cols() as usize;
        // The prediction coordinate space just changed: drop in-flight guesses
        // rather than ghost-paint them at stale coords (the native resize rule).
        self.predict.reset();
        // The swapchain holds the band's rows too (the frame grows by them).
        self.size_swapchain();
    }

    /// Whether the next `render` would draw: the exported frame gate, so a JS
    /// host can skip the wasm call and its own rAF work on a settled pane.
    ///
    /// `&mut self` because reading `Terminal::damage_epoch` latches the current
    /// damage session (idempotent). `true` may be spurious; `false` is
    /// authoritative: the following `render()` will skip.
    ///
    /// Host hazard: a host that stops calling `render` on settled panes also
    /// stops any liveness check inside its draw function. Chromium can evict
    /// the oldest WebGL context without firing `webglcontextlost`, so poll
    /// `isContextLost()` outside the draw path (docs/matrix-rain-design.md).
    pub fn needs_frame(&mut self) -> bool {
        let cell_h = self.cpu.cell_size().1;
        let key = FrameGateKey {
            damage_epoch: self.term.damage_epoch(),
            host_visual_gen: self.host_visual_gen,
            effects_active: self.effects.is_active(),
        };
        key.effects_active
            || self.pending_reflow.is_some()
            || self.last_present_frac != 0
            || self.scroll_input.frac_px(cell_h) != 0
            || self.last_frame_key != Some(key)
    }

    /// Render-tick safety net (see `REFLOW_PUMP_GRACE_RENDERS`), called by the
    /// wasm-only `render`/`render_offscreen` and directly by native tests.
    /// After the grace window, one budgeted step per frame.
    fn pump_reflow_on_render_tick(&mut self) {
        if self.pending_reflow.is_none() {
            return;
        }
        if self.reflow_grace > 0 {
            self.reflow_grace -= 1;
            return;
        }
        self.pump_reflow();
    }

    /// Output safety net (see `REFLOW_BACKLOG_MAX_LINES`), called by `process`:
    /// one budgeted step per call while staged output is past the cap.
    fn pump_reflow_on_output(&mut self) {
        if self.pending_reflow.is_some() && self.term.lazy_backlog_len() > REFLOW_BACKLOG_MAX_LINES
        {
            self.pump_reflow();
        }
    }

    /// Cell width in device pixels — the host computes cols = floor(canvasW / cellWidth).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cell_width(&self) -> usize {
        self.cpu.cell_size().0
    }

    /// Cell height in device pixels — the host computes rows = floor(canvasH / cellHeight).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cell_height(&self) -> usize {
        self.cpu.cell_size().1
    }

    /// True once [`AtermGpuTerminal::init`] has acquired a GPU + surface.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn gpu_ready(&self) -> bool {
        self.gpu.is_some()
    }

    /// The acquired GPU adapter name + backend, once initialized (else empty).
    /// Lets the host log which GPU/backend WebGL handed us.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn adapter_info(&self) -> String {
        match self.gpu.as_ref() {
            Some(gpu) => {
                let (name, backend) = gpu.renderer.adapter();
                format!("{name} ({backend})")
            }
            None => String::new(),
        }
    }
}

/// Metadata for a legacy-contract search ([`AtermGpuTerminal::search_meta`]).
/// Same shape as the aterm-wasm module's `SearchMeta` (each wasm module
/// exports its own copy of the boundary type).
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct SearchMeta {
    incomplete: bool,
    match_count: u32,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl SearchMeta {
    /// True when the results may be truncated: index eviction dropped old rows
    /// before they could be searched, or the engine's match cap was reached.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn incomplete(&self) -> bool {
        self.incomplete
    }

    /// Number of matches the paired [`AtermGpuTerminal::search`] call returns
    /// (its flat triplet array length / 3), after any cap.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn match_count(&self) -> u32 {
        self.match_count
    }
}

/// One slice of a budgeted search ([`AtermGpuTerminal::search_budgeted`]).
/// Same shape as the aterm-wasm module's `BudgetedSearchResult` (each wasm
/// module exports its own copy of the boundary type).
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct BudgetedSearchResult {
    matches: Vec<u32>,
    complete: bool,
    cursor: Option<u64>,
    search_id: Option<u64>,
    reset: bool,
    incomplete_index: bool,
    lowest_retained_line: u32,
    rows_fed: u32,
    total_rows: u32,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl BudgetedSearchResult {
    /// Stable match DELTA as flat `[abs_line, start_col, len]` triplets (same
    /// coordinate contract as [`AtermGpuTerminal::search`]); append across calls.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn matches(&self) -> Vec<u32> {
        self.matches.clone()
    }

    /// Whether every retained row has been scanned and every match delta has
    /// been delivered. Dense searches may finish scanning before this flips.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Token to resume with; `undefined` once complete.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// Stable identity for the logical search, including its completing step;
    /// `undefined` only for an empty/invalid query result.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn search_id(&self) -> Option<u64> {
        self.search_id
    }

    /// Whether this step starts a new logical result stream. Clear previously
    /// accumulated match deltas before appending this step when true.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn reset(&self) -> bool {
        self.reset
    }

    /// True when the results may be truncated (eviction or the match cap).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn incomplete_index(&self) -> bool {
        self.incomplete_index
    }

    /// Final oldest absolute line retained by the completed search index,
    /// stable from the first turn. A nonzero watermark distinguishes history
    /// eviction from match-cap-only truncation.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn lowest_retained_line(&self) -> u32 {
        self.lowest_retained_line
    }

    /// Rows scanned so far (progress numerator; restarts reset it).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn rows_fed(&self) -> u32 {
        self.rows_fed
    }

    /// Total rows this search will scan (progress denominator).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn total_rows(&self) -> u32 {
        self.total_rows
    }
}

impl AtermGpuTerminal {
    /// Expand an OSC-8 hyperlink to the span of contiguous cells sharing its
    /// link. Cells group by `id=` when present, else by URL. Returns
    /// `[start_col, end_col_exclusive)`. Only valid at display_offset 0.
    fn osc8_span(&self, row: u16, col: u16) -> (u16, u16) {
        let same = |c: u16| -> bool {
            let id_here = self.term.hyperlink_id_at(row, col);
            let id_there = self.term.hyperlink_id_at(row, c);
            if id_here.is_some() && id_there.is_some() {
                id_here == id_there
            } else {
                self.term.hyperlink_at(row, c) == self.term.hyperlink_at(row, col)
            }
        };

        let mut start = col;
        while start > 0 && same(start - 1) {
            start -= 1;
        }

        let cols = self.cols as u16;
        let mut end = col + 1;
        while end < cols && same(end) {
            end += 1;
        }

        (start, end)
    }
}

/// Stateless key encoder for worker-hosted engines: encode a DOM keyboard
/// event against a mode-bits snapshot instead of a live terminal.
///
/// The host mirrors [`AtermGpuTerminal::keyboard_mode_bits`] to the main
/// thread and encodes keydowns synchronously here, accepting one-frame
/// staleness (as it does for DECCKM via `is_app_cursor_mode`). Parameters
/// match [`AtermGpuTerminal::encode_key`], plus `mode_bits` (undefined bits
/// are truncated). With fresh bits the output equals the instance method's.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
#[must_use]
pub fn encode_key_with_mode(
    key: &str,
    mods: u8,
    event_type: u8,
    base_layout_key: Option<char>,
    mode_bits: u32,
) -> Option<Vec<u8>> {
    use aterm_types::keyboard::{encode_dom_key, KeyboardMode};
    let mode = KeyboardMode::from_bits_truncate(mode_bits as u16);
    encode_dom_key(key, mods, event_type, base_layout_key, mode)
}

/// A detected link under a cell: its text/URL, the half-open display-column span
/// it covers, and a `kind` discriminant (0=osc8, 1=url, 2=file_path, 3=other).
/// Same shape as aterm-wasm's `LinkHit`.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct LinkHit {
    url: String,
    start_col: u16,
    end_col: u16,
    kind: u8,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl LinkHit {
    /// The link's URL/target text.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Inclusive start display column of the link span.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn start_col(&self) -> u16 {
        self.start_col
    }

    /// Exclusive end display column of the link span.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn end_col(&self) -> u16 {
        self.end_col
    }

    /// Link kind: 0=osc8, 1=url, 2=file_path, 3=other.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn kind(&self) -> u8 {
        self.kind
    }
}

/// Selection bounds in display viewport cell coords (0 = top visible row),
/// inclusive of `start`, with `end` side-adjusted to match `selection_text`
/// and the painted highlight.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct SelectionRange {
    start_x: u16,
    start_y: u16,
    end_x: u16,
    end_y: u16,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl SelectionRange {
    /// Start column (display-relative).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn start_x(&self) -> u16 {
        self.start_x
    }

    /// Start row (display-relative, 0 = top visible row).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn start_y(&self) -> u16 {
        self.start_y
    }

    /// End column (display-relative, side-adjusted/inclusive).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn end_x(&self) -> u16 {
        self.end_x
    }

    /// End row (display-relative).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn end_y(&self) -> u16 {
        self.end_y
    }
}

/// JSON-escape `s` and wrap it in double quotes for `take_osc_events` /
/// `take_notifications`. Mirrors the aterm-wasm crate.
pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Classify a matched span: 1=url, 2=file_path, else 3=other (plain word).
fn classify(s: &str) -> u8 {
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("file://")
        || lower.starts_with("www.")
    {
        return 1;
    }
    if s.starts_with('/')
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with("~/")
        || (s.contains('/') && !s.contains("://"))
    {
        return 2;
    }
    3
}

// ---------------------------------------------------------------------------
// Async WebGL init + present, wasm32 only. Native callers drive aterm-gpu
// directly via `GpuRenderer::new` and a window surface.
// ---------------------------------------------------------------------------
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl AtermGpuTerminal {
    /// Acquire the GPU and create + configure a WebGL2 surface on `canvas`.
    ///
    /// The browser counterpart of aterm-gpu's `GpuRenderer::new` +
    /// `create_window_surface`, with every blocking step awaited and the
    /// surface created before the adapter (WebGL enumerates its adapter
    /// against the canvas): `GpuContext::for_canvas`,
    /// `GpuRenderer::from_parts`, then `configure_canvas_surface`.
    ///
    /// Returns `Err` (a JS string) if WebGL is unavailable or any step fails, so
    /// the host can fall back to the CPU `aterm-wasm` path.
    pub async fn init(&mut self, canvas: aterm_gpu::web::HtmlCanvasElement) -> Result<(), String> {
        self.init_with_target(aterm_gpu::web::CanvasTarget::Canvas(canvas))
            .await
    }

    /// Worker variant: create the WebGL2 surface on a transferred
    /// `OffscreenCanvas`, so rendering and present run off the main thread.
    /// Otherwise identical to `init`.
    pub async fn init_offscreen(
        &mut self,
        canvas: aterm_gpu::web::OffscreenCanvas,
    ) -> Result<(), String> {
        self.init_with_target(aterm_gpu::web::CanvasTarget::Offscreen(canvas))
            .await
    }

    /// Shared GPU bring-up for `init` and `init_offscreen`: context and
    /// pending canvas surface from `target`, then the renderer and swapchain
    /// config. Not `pub` because `CanvasTarget` is not a JS type.
    async fn init_with_target(
        &mut self,
        target: aterm_gpu::web::CanvasTarget,
    ) -> Result<(), String> {
        // WebGL can only acquire an adapter from a surface (the GL context lives
        // on the canvas), so aterm-gpu creates the surface first and hands it
        // back unconfigured. Its errors name the failed step.
        let (ctx, canvas_surface) = GpuContext::for_canvas(target).await?;

        // Assemble the portable GPU renderer on the acquired context, on the
        // face the host configured — this builds every pipeline.
        let renderer = GpuRenderer::from_parts(ctx, self.gpu_face()?, None, self.theme)?;

        // Configure the canvas swapchain (non-sRGB, sized to the grid).
        let (w, h) = renderer.frame_size(self.surface_rows(), self.cols);
        let surface = renderer
            .configure_canvas_surface(canvas_surface, w as u32, h as u32)
            .map_err(|e| format!("configure canvas surface failed: {e}"))?;

        self.gpu = Some(GpuState {
            renderer,
            surface,
            win: WindowGpu::new(),
        });
        // A fresh swapchain holds no frame of ours; the gate must not skip the
        // first present onto it.
        self.last_frame_key = None;
        self.last_render_gated = false;
        Ok(())
    }

    /// Present one frame on the GPU canvas. Errors (as JS strings) when WebGL
    /// is uninitialized or the surface reports a typed transient failure
    /// (`Reconfigured`, `Timeout`, `Occluded`, `Validation`). After
    /// `Reconfigured` the resize repair already ran; the host should keep its
    /// rAF loop alive and retry.
    ///
    /// Snapshots the engine, then aterm-gpu's `present_input` renders it
    /// offscreen (atlas upload + instanced bg/glyph/cursor quads) and blits it
    /// into the canvas swapchain.
    pub fn render(&mut self) -> Result<(), String> {
        // Frame gate: `None` means the canvas already holds an identical frame,
        // so neither the build nor the present runs.
        let Some(key) = self.open_frame() else {
            // A gated tick reports what an ungated one would: a missing `init`
            // is never `Ok(())`.
            return if self.gpu.is_some() {
                Ok(())
            } else {
                Err("render() before init()".to_string())
            };
        };
        // Engine-side half of the tick, shared with `render_offscreen` and
        // `render_headless`.
        self.build_frame();
        self.record_presented_frame(key);
        let gpu = self.gpu.as_mut().ok_or("render() before init()")?;
        // `invert == false`: the visual-bell flash is host-driven. No overlay
        // or tray on the canvas path.
        let presented = webgl_present_result(gpu.renderer.present_input(
            &mut gpu.win,
            &mut gpu.surface,
            &self.frame_scratch,
            false,
            None,
            None,
        ));
        if presented.is_err() {
            // The canvas did not receive this frame; drop the key so the
            // host's retry draws.
            self.last_frame_key = None;
        }
        presented
    }

    /// Render the current grid offscreen and read the pixels back into the
    /// internal RGBA8 buffer, for GPU-vs-CPU pixel comparison (a WebGL
    /// swapchain is not CPU-readable). Same contract as aterm-wasm's
    /// `render()`+`rgba()`: `Frame` pixels (0xTTRRGGBB, TT = transmittance, 0 =
    /// opaque) expanded to RGBA8. Errors if WebGL was not initialized.
    pub fn render_offscreen(&mut self) -> Result<(), String> {
        // Ungated: a harness asking for a readback gets one. The frame-boundary
        // pumps still run.
        self.pump_reflow_on_render_tick();
        self.drain_compress_backlog_on_render();
        // The same frame build as `render`, so the readback cannot diverge
        // from the canvas path.
        self.build_frame();
        // This frame went to the OFFSCREEN target; the CANVAS did not receive
        // it, so it must not stand in for a present. Leave the gate open.
        self.last_frame_key = None;
        self.last_render_gated = false;
        let gpu = self
            .gpu
            .as_mut()
            .ok_or("render_offscreen() before init()")?;
        // `None`: the web/canvas path has no settings-card tray (the P3 tray arg).
        let frame = gpu
            .renderer
            .render_input(&mut gpu.win, &self.frame_scratch, None);
        self.fb_width = frame.width;
        self.fb_height = frame.height;
        // Expand 0xTTRRGGBB (TT = 255 - alpha) to RGBA8 for ImageData. Alpha is
        // 0xff at default opacity, lower on default-bg pixels below it.
        self.rgba.clear();
        self.rgba.reserve(frame.pixels.len() * 4);
        for &p in &frame.pixels {
            self.rgba.push((p >> 16) as u8);
            self.rgba.push((p >> 8) as u8);
            self.rgba.push(p as u8);
            self.rgba.push(0xff - (p >> 24) as u8);
        }
        Ok(())
    }

    /// Width in pixels of the last [`render_offscreen`](Self::render_offscreen)
    /// framebuffer.
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> usize {
        self.fb_width
    }

    /// Height in pixels of the last [`render_offscreen`](Self::render_offscreen)
    /// framebuffer.
    #[wasm_bindgen(getter)]
    pub fn height(&self) -> usize {
        self.fb_height
    }

    /// Copy of the last [`render_offscreen`](Self::render_offscreen) RGBA8
    /// framebuffer (`width*height*4` bytes), ready for
    /// `ctx.putImageData(new ImageData(rgba, width, height), 0, 0)` or a pixel diff.
    pub fn rgba(&self) -> Vec<u8> {
        self.rgba.clone()
    }
}

// Native-only constructor for tests: builds the CPU face from the first
// available system monospace font (no JS to fetch one), with the same
// clamped-dims sync as `new`.
#[cfg(not(target_arch = "wasm32"))]
impl AtermGpuTerminal {
    /// The native half of `render`: the same frame gate and frame build, minus
    /// the WebGL present. Returns `true` when the frame was gated.
    ///
    /// Returns `true` when the frame was elided by the settled-frame gate.
    ///
    /// This exists so the gate and the per-tick frame build are measurable and
    /// testable OFF the browser: the crate's own native tests use it as the
    /// gate's reach witness. It is deliberately NOT a second implementation — it calls the
    /// same two helpers `render` calls, in the same order, so a divergence is
    /// impossible by construction. What it does NOT cover is the GL half
    /// (swapchain acquire + letterbox blit + submit + present), which the gate
    /// also skips; any number measured here is therefore a LOWER bound on what
    /// a settled browser tick saves.
    ///
    /// `cfg(not(wasm32))`, like `new_from_system` directly below and for the
    /// same reason: it adds ZERO surface to the module that actually ships —
    /// the wasm build does not contain it.
    pub fn render_headless(&mut self) -> bool {
        let Some(key) = self.open_frame() else {
            return true;
        };
        self.build_frame();
        // The native seam has no canvas, so "presented" means "built": that is
        // what makes the gate settle here exactly as it settles in the browser.
        self.record_presented_frame(key);
        false
    }

    pub fn new_from_system(rows: u16, cols: u16, px: f32) -> Option<AtermGpuTerminal> {
        let theme = Theme::default();
        let cpu = Renderer::from_system(px, theme)?;
        // Same tiered-store attachment as `new`.
        let mut term = scrollback_tiers_api::tiered_terminal(rows, cols);
        let budget_share = scrollback_tiers_api::register_budget_share(&term);
        apply_terminal_theme_colors(&mut term, theme.fg, theme.bg, theme.cursor, theme.selection);
        // Same notification wiring as `new` (fail-closed until authorized).
        let notifications = notifications_api::wire_notification_queue(&mut term);
        let rows = term.grid().rows() as usize;
        let cols = term.grid().cols() as usize;
        Some(Self {
            budget_share,
            term,
            cpu,
            rows,
            cols,
            theme,
            font_bytes: Vec::new(),
            gpu: None,
            #[cfg(target_arch = "wasm32")]
            rgba: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            fb_width: 0,
            #[cfg(target_arch = "wasm32")]
            fb_height: 0,
            smart: SmartSelection::with_builtin_rules(),
            fallback_font: None,
            fallback_chain_extra: Vec::new(),
            emoji_font: None,
            bold_font: None,
            symbol_font: None,
            text_shaping: aterm_render::TextShapingConfig::default(),
            frame_scratch: RenderInput::empty(),
            #[cfg(test)]
            last_refill: None,
            effects: EffectsPipeline::new(),
            theme_cursor: theme.cursor & 0x00FF_FFFF,
            theme_fg: theme.fg & 0x00FF_FFFF,
            theme_bg: theme.bg & 0x00FF_FFFF,
            notifications,
            band: messages_api::WebBand::new(
                aterm_time::Instant::now(),
                theme.fg,
                theme.bg,
                theme.cursor,
            ),
            scroll_input: scroll_input_api::ScrollInputState::default(),
            predict: aterm_predict::Predictor::default(),
            pred_row_scratch: Vec::new(),
            spill: SpillBand::new(),
            pending_reflow: None,
            reflow_grace: 0,
            reflow_budget: REFLOW_STEP_BUDGET_LINES,
            display_row_cache: std::cell::RefCell::new(GpuDisplayRowCache::default()),
            host_visual_gen: 0,
            last_frame_key: None,
            last_present_frac: 0,
            last_render_gated: false,
            blink_phase_shadow: None,
            hollow_shadow: None,
        })
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use aterm_core::render::FrameRefill;

    use super::*;

    /// The bundled face, injected as a JS host would. Paste tests use it so a
    /// security property is never skipped on a box with no system font.
    const BUNDLED_FACE: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

    /// A link under the pointer is read from the cells it is drawn in,
    /// whatever precedes it on the row (r16 selection-stride): after two
    /// ideographs, or two flag halves drawn into neighbouring cells out of
    /// order (two cells each, which a walk of the row's text pairs into one
    /// two-cell flag), the URL's columns are its own and its text is the
    /// URL. On origin/main the URL's text was sliced out of the row one
    /// column a character: after the ideographs it read `tps://example.com/a`,
    /// no link at all, and after the flag halves the link was answered in
    /// cells 2-22 and not at cell 24.
    #[test]
    fn a_link_after_wide_characters_is_read_from_its_cells() {
        let mut t = AtermGpuTerminal::new(4, 60, BUNDLED_FACE, 16.0, 0, 0, 0, 0)
            .expect("the bundled face parses");
        t.process("\u{65e5}\u{672c} https://example.com/a\r\n".as_bytes());
        t.process("\x1b[2;3H\u{1f1e7}\x1b[2;1H\u{1f1e6}\x1b[2;5Hhttps://example.com/b".as_bytes());
        for (row, col, url, start, end) in [
            (0, 5, "https://example.com/a", 5, 26),
            (0, 25, "https://example.com/a", 5, 26),
            (1, 4, "https://example.com/b", 4, 25),
            (1, 24, "https://example.com/b", 4, 25),
        ] {
            let hit = t
                .link_at(row, col)
                .unwrap_or_else(|| panic!("({row}, {col}) is over a URL"));
            assert_eq!(
                (hit.url(), hit.start_col(), hit.end_col(), hit.kind()),
                (url.to_string(), start, end, 1),
                "link at ({row}, {col})"
            );
        }
        assert!(t.link_at(1, 2).is_none(), "a flag half is no link");
    }

    /// `init` builds its GPU face from the host's current settings
    /// (`gpu_face`), not the constructor's px: a `set_px` before `init`, or a
    /// re-`init` after one, must draw at the size the cells were measured at.
    #[test]
    fn the_gpu_face_is_the_face_the_host_set() {
        let mut t = AtermGpuTerminal::new(8, 48, BUNDLED_FACE, 14.0, 0, 0, 0, 0)
            .expect("the bundled face parses");
        t.set_px(21.0);
        t.set_line_height(1.25);
        t.set_chrome(4, 2);
        // An unusable request keeps the last valid CPU geometry. Rebuilding
        // must retain that geometry too, rather than replay the rejected input.
        t.set_line_height(f32::INFINITY);
        let face = t.gpu_face().expect("the bundled face");
        assert_eq!(face.px(), 21.0);
        assert_eq!(face.cell_size(), t.cpu.cell_size());
        assert_eq!((face.pad(), face.head()), (t.cpu.pad(), t.cpu.head()));

        t.process(b"selected text\r\n\x1b[1 q");
        t.selection_start(0, 0);
        t.selection_extend(0, 8);
        t.selection_finish();
        t.set_selection_fg(Some(0x00ff_8040));
        t.set_selection_inactive_bg(Some(0x0012_3456));
        for (inactive, hollow, blink) in [
            (false, false, false),
            (false, true, true),
            (true, false, true),
        ] {
            t.set_selection_inactive(inactive);
            t.set_cursor_hollow(hollow);
            t.set_cursor_blink_phase(blink);
            let mut face = t.gpu_face().expect("the bundled face");
            let input = t.term.cell_frame(t.rows, t.cols);
            let expected = t.cpu.render_input(&input);
            let actual = face.render_input(&input);
            assert_eq!(
                (actual.width, actual.height),
                (expected.width, expected.height)
            );
            assert_eq!(
                actual.pixels, expected.pixels,
                "init must retain inactive={inactive}, hollow={hollow}, blink={blink}"
            );
        }
    }

    #[test]
    fn console_input_api_counts_commits_without_replaying_their_echoes() {
        use aterm_effects::kitty_pet::PetInputKind;

        let mut t = AtermGpuTerminal::new(8, 48, BUNDLED_FACE, 14.0, 0, 0, 0, 0)
            .expect("the bundled face parses");
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 0);
        let generation = t.host_visual_gen;
        assert!(!t.note_console_input("unknown"));
        assert_eq!(t.host_visual_gen, generation);
        t.note_keystroke();
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 0);
        assert!(t.note_typed_char('x'));
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 1);
        // Same-cell overwrite and subsequent unrelated output do not note input.
        t.process(b"x\r");
        t.process(b"program repaint\r");
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 1);

        // The handler reports an entire composition commit, never preedit or
        // one note per scalar. A zero-advance combining commit counts as well.
        assert!(t.note_console_input("text"));
        t.process("中文🙂".as_bytes());
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 2);
        assert!(!t.note_typed_char('\u{0301}'));
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 3);

        // Formatting is pure; admission owns the single paste event.
        let paste = t.format_paste("one pasted sentence");
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 3);
        let generation = t.host_visual_gen;
        assert!(t.note_console_input("paste"));
        assert!(
            t.host_visual_gen > generation,
            "no-caret-travel input must wake one frame"
        );
        t.process(&paste);
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 4);
        assert_eq!(
            t.effects.cursor_pet_console_input_kind(),
            Some(PetInputKind::Paste)
        );
        for (name, kind) in [
            ("navigate", PetInputKind::Navigate),
            ("delete", PetInputKind::Delete),
            ("submit", PetInputKind::Submit),
        ] {
            let before = t.effects.cursor_pet_console_input_seq();
            assert!(t.note_console_input(name));
            assert_eq!(t.effects.cursor_pet_console_input_seq(), before + 1);
            assert_eq!(t.effects.cursor_pet_console_input_kind(), Some(kind));
        }
    }

    /// The web export is the engine's paste formatter: sanitize-then-wrap makes
    /// a planted `ESC[201~` (or 8-bit `0x9B 2 0 1 ~`) inert before the closing
    /// guard, so it cannot end its own bracket.
    #[test]
    fn paste_export_wraps_only_sanitized_text_so_a_planted_terminator_stays_inert() {
        let mut t = AtermGpuTerminal::new(24, 80, BUNDLED_FACE, 14.0, 0, 0, 0, 0)
            .expect("the bundled face parses");

        // Mode OFF: no guards to escape, yet ESC/C1 are still stripped and a bare
        // LF is still folded to CR for the PTY.
        assert!(!t.bracketed_paste_mode());
        assert_eq!(
            t.format_paste("a\x1b[31mb\u{009d}c\nd"),
            b"a[31mbc\rd",
            "unbracketed paste is sanitized too"
        );

        t.process(b"\x1b[?2004h");
        assert!(t.bracketed_paste_mode());

        let out = t.format_paste("safe\x1b[201~rm -rf ~");
        assert_eq!(out, b"\x1b[200~safe[201~rm -rf ~\x1b[201~");
        assert_eq!(
            out.windows(6).filter(|w| *w == b"\x1b[201~").count(),
            1,
            "exactly one closing guard on the wire"
        );
        assert!(out.ends_with(b"\x1b[201~"), "and it is the last thing sent");

        assert_eq!(
            t.format_paste("a\u{009b}201~b"),
            b"\x1b[200~a201~b\x1b[201~",
            "the 8-bit CSI spelling of the terminator is stripped as well"
        );

        // The seam that keeps the two web facades and native from drifting: the
        // export delegates, it does not reimplement.
        let mut engine = Terminal::new(24, 80);
        engine.process(b"\x1b[?2004h");
        for probe in [
            "",
            "\t",
            "x\r\ny\nz",
            "safe\x1b[201~rm -rf ~",
            "a\u{009b}201~b",
            "a\u{0003}b\u{0004}c\u{001a}d\u{007f}e",
            "中文🙂",
        ] {
            assert_eq!(
                t.format_paste(probe),
                engine.format_paste(probe),
                "web export must equal Terminal::format_paste for {probe:?}"
            );
        }
    }

    /// The `effects_api` mutators' `note_host_visual_change` bumps must be live:
    /// an effects config change that fails to reopen the gate would serve a
    /// stale frame. Also pins that `advance_effects` does not bump, since hosts
    /// call it every rAF tick.
    #[test]
    fn effects_config_mutators_bump_the_host_visual_generation() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(6, 40, 14.0) else {
            eprintln!("no system font; skipping host-visual-gen smoke");
            return;
        };
        let start = t.host_visual_gen;
        t.advance_effects(16.0);
        assert_eq!(
            t.host_visual_gen, start,
            "advance_effects must not bump — it runs every rAF tick"
        );

        // One representative per mutator family: focus/visibility gates,
        // keystroke ignition, cursor wake, phosphor rain and sparkle words.
        let mut last = start;
        for (label, mutate) in [
            (
                "set_effects_focused",
                (|t: &mut AtermGpuTerminal| t.set_effects_focused(true))
                    as fn(&mut AtermGpuTerminal),
            ),
            ("set_effects_visibility", |t| {
                t.set_effects_visibility("hidden")
            }),
            ("note_keystroke", |t| t.note_keystroke()),
            ("note_typed_char", |t| {
                let _ = t.note_typed_char('x');
            }),
            ("set_cursor_glow", |t| {
                t.set_cursor_glow(true, "lumen", None, None, 300, 8, 1.0, 0.0, false)
            }),
            ("set_cursor_trail", |t| {
                t.set_cursor_trail(true, 300, 8, None)
            }),
            ("set_matrix_rain_enabled", |t| {
                t.set_matrix_rain_enabled(true)
            }),
            ("note_matrix_rain_bell", |t| t.note_matrix_rain_bell()),
            ("set_sparkle_words_enabled", |t| {
                t.set_sparkle_words_enabled(true)
            }),
            ("set_sparkle_reduced_motion", |t| {
                t.set_sparkle_reduced_motion(true)
            }),
        ] {
            mutate(&mut t);
            assert!(
                t.host_visual_gen > last,
                "{label} must bump the host-visual generation (hook is a no-op?)"
            );
            last = t.host_visual_gen;
        }
    }

    /// A screenful of content, settled: the next `render_headless` gates.
    fn settled(rows: u16, cols: u16) -> Option<AtermGpuTerminal> {
        let mut t = AtermGpuTerminal::new_from_system(rows, cols, 14.0)?;
        for r in 0..rows {
            t.process(format!("line {r} of selectable text\r\n").as_bytes());
        }
        t.render_headless();
        t.render_headless();
        assert!(t.last_render_skipped(), "setup must reach a settled frame");
        Some(t)
    }

    /// The frame gate, both sides: a settled tick skips the whole frame, and
    /// every class of change the damage epoch cannot see reopens it.
    #[test]
    fn frame_gate_skips_settled_ticks_and_reopens_on_every_change_class() {
        let Some(mut t) = settled(8, 40) else {
            eprintln!("no system font; skipping gpu-web frame-gate test");
            return;
        };
        assert!(!t.needs_frame(), "needs_frame must agree with the gate");

        // REOPEN 1: grid damage (echo).
        t.process(b"!");
        assert!(t.needs_frame(), "damage must reopen the gate");
        assert!(!t.render_headless(), "an echo tick must draw");
        assert!(t.render_headless(), "and then re-settle");

        // REOPEN 2: renderer-held blink phase (no grid damage), with the
        // value-shadow de-dup on the second, identical assertion.
        t.set_cursor_blink_phase(false);
        assert!(!t.render_headless(), "a blink flip must draw");
        t.set_cursor_blink_phase(false);
        assert!(
            t.render_headless(),
            "an idempotent blink re-assert must gate"
        );

        // REOPEN 3: selection (Terminal-held, no grid damage) — and its
        // pointer-cadence de-dup.
        t.selection_start(2, 1);
        t.selection_extend(2, 8);
        assert!(!t.render_headless(), "a selection change must draw");
        t.selection_extend(2, 8);
        assert!(
            t.render_headless(),
            "a cell-identical extend must be gated away"
        );
        t.selection_extend(2, 9);
        assert!(!t.render_headless(), "a one-cell move must draw");
        t.selection_clear();
        assert!(!t.render_headless(), "clearing a selection must draw");
        t.render_headless();

        // REOPEN 4: viewport scroll (display-offset damage -> epoch).
        t.scroll_lines(3);
        assert!(!t.render_headless(), "a viewport scroll must draw");
        t.render_headless();
        t.scroll_to_bottom();
        assert!(!t.render_headless(), "and scrolling back must draw");
        t.render_headless();

        // REOPEN 5: appearance-only change that moves no cell. This is the one
        // the engine's damage epoch is structurally blind to.
        t.set_theme(0x00FF_0000, 0x0000_2200, 0x00FF_FFFF, 0x0033_3366);
        assert!(!t.render_headless(), "a theme change must draw");
        t.render_headless();
        t.set_palette_color(1, 9, 9, 9);
        assert!(!t.render_headless(), "a palette change must draw");
        t.render_headless();

        // An active effects pipeline animates, so it never gates, and the
        // active->idle edge buys one settle frame. Matrix rain is the ambient
        // effect a web host can ignite without typed-move witnesses.
        t.set_matrix_rain_enabled(true);
        assert!(!t.render_headless(), "an ignited effect must draw");
        let mut saw_active = false;
        for i in 0u32..60 {
            // Real output now and then: rain holds still on a pane that is only
            // being read, where closing the gate is correct.
            if i.is_multiple_of(10) {
                t.process(b"x");
            }
            t.advance_effects(16.0);
            let active = t.effects.is_active();
            saw_active |= active;
            let gated = t.render_headless();
            if active {
                assert!(!gated, "an active effects pipeline must never gate");
                assert!(t.needs_frame(), "and needs_frame must say so");
            }
        }
        assert!(
            saw_active,
            "the rain session never went active — this case proved nothing"
        );
        // Off again: the pipeline must be able to settle back into the gate
        // rather than holding it open forever.
        t.set_matrix_rain_enabled(false);
        let mut settled = false;
        for _ in 0..10 {
            if t.render_headless() {
                settled = true;
                break;
            }
        }
        assert!(
            settled,
            "an idle effects pipeline must settle back into the gate"
        );
    }

    /// Mutator audit: every `pub fn` that changes what a frame looks like must
    /// bump `host_visual_gen`, or the gate serves a stale frame. Each entry is
    /// called on a settled terminal and must reopen the gate. Fallible entries
    /// get invalid arguments: the bump precedes validation.
    #[test]
    fn every_visual_mutator_bumps_the_host_visual_generation() {
        let Some(mut t) = settled(8, 40) else {
            eprintln!("no system font; skipping gpu-web mutator audit");
            return;
        };
        let mut last = t.host_visual_gen;
        for (label, mutate) in [
            (
                "set_fallback_font",
                (|t: &mut AtermGpuTerminal| {
                    let _ = t.set_fallback_font(b"");
                }) as fn(&mut AtermGpuTerminal),
            ),
            ("add_fallback_font", |t| {
                let _ = t.add_fallback_font(b"");
            }),
            ("set_emoji_font", |t| {
                let _ = t.set_emoji_font(b"");
            }),
            ("set_bold_font", |t| {
                let _ = t.set_bold_font(b"");
            }),
            ("set_symbol_font", |t| {
                let _ = t.set_symbol_font(b"");
            }),
            ("set_primary_font", |t| {
                let _ = t.set_primary_font(b"");
            }),
            ("set_fallback_font_registered", |t| {
                let _ = t.set_fallback_font_registered(0);
            }),
            ("add_fallback_font_registered", |t| {
                let _ = t.add_fallback_font_registered(0);
            }),
            ("set_emoji_font_registered", |t| {
                let _ = t.set_emoji_font_registered(0);
            }),
            ("set_bold_font_registered", |t| {
                let _ = t.set_bold_font_registered(0);
            }),
            ("set_symbol_font_registered", |t| {
                let _ = t.set_symbol_font_registered(0);
            }),
            ("set_line_height", |t| t.set_line_height(1.2)),
            ("set_ligatures", |t| t.set_ligatures(true)),
            ("set_font_features", |t| t.set_font_features("+liga")),
            ("set_palette_color", |t| t.set_palette_color(3, 1, 2, 3)),
            ("set_default_foreground", |t| {
                t.set_default_foreground(1, 2, 3)
            }),
            ("set_default_background", |t| {
                t.set_default_background(4, 5, 6)
            }),
            ("set_scrollback_limit", |t| t.set_scrollback_limit(500)),
            ("set_theme", |t| {
                t.set_theme(0x0011_2233, 0x0044_5566, 0x0077_8899, 0x000A_0B0C)
            }),
            ("set_selection_fg", |t| {
                t.set_selection_fg(Some(0x0012_3456))
            }),
            ("set_minimum_contrast", |t| t.set_minimum_contrast(4.5)),
            ("set_background_opacity", |t| t.set_background_opacity(0.8)),
            ("set_chrome", |t| t.set_chrome(4, 2)),
            ("set_cursor_opacity", |t| t.set_cursor_opacity(0.5)),
            ("set_selection_inactive", |t| t.set_selection_inactive(true)),
            ("set_selection_inactive_bg", |t| {
                t.set_selection_inactive_bg(Some(0x0020_2020))
            }),
            ("set_px", |t| t.set_px(15.0)),
            ("set_spill_include_veils", |t| {
                t.set_spill_include_veils(true)
            }),
            ("set_default_cursor_style", |t| {
                t.set_default_cursor_style(3)
            }),
            ("set_color_scheme", |t| t.set_color_scheme(true)),
            ("set_cursor_blink_phase", |t| {
                t.set_cursor_blink_phase(false)
            }),
            ("set_cursor_hollow", |t| t.set_cursor_hollow(true)),
            ("selection_start", |t| t.selection_start(2, 1)),
            ("selection_extend", |t| t.selection_extend(2, 7)),
            ("selection_finish", |t| t.selection_finish()),
            ("selection_word", |t| {
                let _ = t.selection_word(3, 2);
            }),
            ("selection_line", |t| {
                let _ = t.selection_line(4, 0);
            }),
            ("selection_clear", |t| t.selection_clear()),
            // Last, since it changes the grid: a resize reconfigures the
            // swapchain, so it reopens the gate even at unchanged dims.
            ("resize", |t| t.resize(8, 40)),
        ] {
            mutate(&mut t);
            assert!(
                t.host_visual_gen > last,
                "{label} must bump the host-visual generation, or the frame \
                 gate will serve a stale frame after it"
            );
            assert!(
                t.needs_frame(),
                "{label} must reopen the frame gate end to end"
            );
            last = t.host_visual_gen;
            t.render_headless();
        }
    }

    /// The gate never lets the canvas keep a frame it did not receive. Natively
    /// `render` always reports "before init()", and a gated tick must report
    /// the same; a never-presented terminal never gates.
    #[test]
    fn an_uninitialized_instance_never_gates_and_keeps_its_error() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(6, 40, 14.0) else {
            eprintln!("no system font; skipping uninitialized-gate test");
            return;
        };
        assert!(t.gpu.is_none(), "native build has no swapchain");
        assert!(t.needs_frame(), "a never-presented pane must need a frame");
        // `render_headless` settles the gate; a real host reaches the same
        // state through `render`, whose canvas then holds the frame.
        assert!(!t.render_headless(), "the first tick must build");
        assert!(t.render_headless(), "and the second must gate");

        // `render_offscreen` (wasm32-only) never calls `open_frame` and drops
        // `last_frame_key` afterwards, since its frame went to the readback
        // target.
    }

    #[test]
    fn host_theme_colors_are_the_dynamic_color_reset_baseline() {
        let mut term = Terminal::new(2, 4);
        apply_terminal_theme_colors(
            &mut term,
            0x0011_2233,
            0x0044_5566,
            0x0077_8899,
            0x000A_0B0C,
        );
        term.process(b"\x1b]10;rgb:aaaa/bbbb/cccc\x1b\\");
        term.process(b"\x1b]11;rgb:dddd/eeee/ffff\x1b\\");
        term.process(b"\x1b]12;rgb:1111/2222/3333\x1b\\");
        term.process(b"\x1b]17;rgb:0101/0202/0303\x1b\\");
        term.process(b"\x1b]19;rgb:0404/0505/0606\x1b\\");
        assert_eq!(term.default_foreground(), Rgb::new(0xaa, 0xbb, 0xcc));
        assert_eq!(term.default_background(), Rgb::new(0xdd, 0xee, 0xff));
        assert_eq!(
            term.selection_background(),
            Some(Rgb::new(0x01, 0x02, 0x03))
        );
        assert_eq!(
            term.selection_foreground(),
            Some(Rgb::new(0x04, 0x05, 0x06))
        );

        term.process(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]117\x07\x1b]119\x07");
        assert_eq!(term.default_foreground(), Rgb::new(0x11, 0x22, 0x33));
        assert_eq!(term.default_background(), Rgb::new(0x44, 0x55, 0x66));
        assert_eq!(term.cursor_color(), Some(Rgb::new(0x77, 0x88, 0x99)));
        assert_eq!(
            term.selection_background(),
            Some(Rgb::new(0x0a, 0x0b, 0x0c))
        );
        assert_eq!(term.selection_foreground(), None);
    }

    #[test]
    fn frame_scratch_tracks_live_sparse_blank_and_cursor_colors() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(3, 8, 16.0) else {
            return;
        };
        t.set_default_foreground(0x11, 0x22, 0x33);
        t.set_default_background(0x44, 0x55, 0x66);
        t.set_selection_fg(Some(0x0012_3456));
        t.process(b"X");

        t.refill_frame_scratch();
        assert!(
            t.frame_scratch.cells[0].len() < t.cols,
            "the regression requires an unmaterialized sparse row tail"
        );
        assert_eq!(t.frame_scratch.default_bg, 0x0044_5566);
        assert_eq!(t.frame_scratch.cells[0][0].bg, [0x44, 0x55, 0x66]);
        assert_eq!(
            t.frame_scratch.cursor_color, t.theme_cursor,
            "the configured OSC 12 baseline matches the host theme"
        );
        assert_eq!(t.frame_scratch.selection_bg, Theme::default().selection);
        assert_eq!(t.frame_scratch.selection_fg, 0x0012_3456);

        t.process(b"\x1b]11;rgb:7777/8888/9999\x1b\\");
        t.process(b"\x1b]12;rgb:aaaa/bbbb/cccc\x1b\\");
        t.process(b"\x1b]17;rgb:0101/0202/0303\x1b\\");
        t.process(b"\x1b]19;rgb:0404/0505/0606\x1b\\");
        t.refill_frame_scratch();
        assert_eq!(t.frame_scratch.default_bg, 0x0077_8899);
        assert_eq!(t.frame_scratch.cells[0][0].bg, [0x77, 0x88, 0x99]);
        assert_eq!(t.frame_scratch.cursor_color, 0x00aa_bbcc);
        assert_eq!(t.frame_scratch.selection_bg, 0x0001_0203);
        assert_eq!(t.frame_scratch.selection_fg, 0x0004_0506);

        t.process(b"\x1b[?5h");
        t.refill_frame_scratch();
        assert_eq!(
            t.frame_scratch.default_bg, 0x0011_2233,
            "DECSCNM makes the live default foreground the effective blank background"
        );
        assert_eq!(t.frame_scratch.cells[0][0].bg, [0x11, 0x22, 0x33]);

        t.process(b"\x1b]111\x07\x1b]112\x07\x1b]117\x07\x1b]119\x07\x1b[?5l");
        t.refill_frame_scratch();
        assert_eq!(
            t.frame_scratch.default_bg, 0x0044_5566,
            "OSC 111 restores the configured background"
        );
        assert_eq!(
            t.frame_scratch.cursor_color, t.theme_cursor,
            "OSC 112 restores the host-configured cursor baseline"
        );
        assert_eq!(t.frame_scratch.selection_bg, Theme::default().selection);
        assert_eq!(t.frame_scratch.selection_fg, 0x0012_3456);

        t.process(b"\x1b]21;cursor=\x07\x1b]10;#DEADBE\x07");
        t.refill_frame_scratch();
        assert_eq!(t.term.cursor_color(), None);
        assert_eq!(
            t.frame_scratch.cursor_color, 0x00DE_ADBE,
            "OSC 21 cursor= makes the embedded cursor follow live OSC 10"
        );
        t.process(b"\x1b]10;#1234AB\x07");
        t.refill_frame_scratch();
        assert_eq!(
            t.frame_scratch.cursor_color, 0x0012_34AB,
            "a later OSC 10 recolors the still-dynamic cursor"
        );
    }

    #[test]
    fn dynamic_cursor_osc10_changes_gpu_web_input_pixels() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(2, 4, 16.0) else {
            return;
        };
        t.process(b"\x1b]21;cursor=\x07\x1b]10;#21C365\x07\x1b[2 q");
        t.refill_frame_scratch();
        assert_eq!(t.term.cursor_color(), None);
        assert_eq!(t.frame_scratch.cursor_color, 0x0021_C365);
        let (cw, ch) = t.cpu.cell_size();
        let frame = t.cpu.render_input(&t.frame_scratch);
        assert_eq!(
            frame
                .pixels
                .iter()
                .filter(|&&pixel| pixel == 0x0021_C365)
                .count(),
            cw * ch,
            "the gpu-web RenderInput paints the dynamic OSC 10 cursor color"
        );

        t.process(b"\x1b]10;#BADA55\x07");
        t.refill_frame_scratch();
        assert_eq!(t.frame_scratch.cursor_color, 0x00BA_DA55);
        let frame = t.cpu.render_input(&t.frame_scratch);
        assert_eq!(
            frame
                .pixels
                .iter()
                .filter(|&&pixel| pixel == 0x00BA_DA55)
                .count(),
            cw * ch,
            "changing OSC 10 changes gpu-web cursor pixels while the cursor slot stays dynamic"
        );
    }

    #[test]
    fn webgl_present_result_propagates_every_surface_failure() {
        assert_eq!(webgl_present_result(Ok(())), Ok(()));
        for (failure, expected) in [
            (
                SurfacePresentFailure::Reconfigured,
                "WebGL canvas present failed: Reconfigured",
            ),
            (
                SurfacePresentFailure::Timeout,
                "WebGL canvas present failed: Timeout",
            ),
            (
                SurfacePresentFailure::Occluded,
                "WebGL canvas present failed: Occluded",
            ),
            (
                SurfacePresentFailure::Validation,
                "WebGL canvas present failed: Validation",
            ),
        ] {
            assert_eq!(webgl_present_result(Err(failure)).unwrap_err(), expected);
        }
    }

    /// `search_meta` agrees with `search` and carries the `incomplete` signal
    /// `search` drops.
    #[test]
    fn search_meta_agrees_with_search_and_is_silent_on_empty_or_bad_input() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.process(b"alpha beta\r\nalpha gamma\r\nno match here\r\n");
        let matches = t.search("alpha", true, false);
        let meta = t.search_meta("alpha", true, false);
        assert_eq!(meta.match_count() as usize, matches.len() / 3);
        assert_eq!(meta.match_count(), 2);
        assert!(!meta.incomplete());
        let empty = t.search_meta("", true, false);
        assert_eq!((empty.incomplete(), empty.match_count()), (false, 0));
        let bad = t.search_meta("(", true, true);
        assert_eq!((bad.incomplete(), bad.match_count()), (false, 0));
    }

    /// Construction and `resize` store the grid's clamped dims (1..=4096), not
    /// the raw u16 args, so the GPU framebuffer is never sized past the grid.
    #[test]
    fn oversized_dims_are_clamped_to_the_grid_bound() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(u16::MAX, u16::MAX, 16.0) else {
            // No usable system font in this environment; skip rather than fail.
            return;
        };
        assert!(
            t.rows <= 4096 && t.cols <= 4096,
            "ctor clamps to grid bound"
        );
        assert_eq!(t.rows, t.term.grid().rows() as usize, "ctor syncs to grid");
        assert_eq!(t.cols, t.term.grid().cols() as usize, "ctor syncs to grid");

        // Zero is clamped UP to 1 by the grid, not stored as 0.
        t.resize(0, 0);
        assert_eq!(t.rows, 1, "resize(0) → grid clamps rows to 1");
        assert_eq!(t.cols, 1, "resize(0) → grid clamps cols to 1");

        // And an oversized resize re-syncs to the clamped bound too.
        t.resize(u16::MAX, u16::MAX);
        assert!(
            t.rows <= 4096 && t.cols <= 4096,
            "resize clamps to grid bound"
        );
        assert_eq!(
            t.rows,
            t.term.grid().rows() as usize,
            "resize syncs to grid"
        );
        assert_eq!(
            t.cols,
            t.term.grid().cols() as usize,
            "resize syncs to grid"
        );
    }

    /// Shrinking retention invalidates the search index like any content
    /// change, so a search never returns just-evicted absolute rows.
    #[test]
    fn retention_shrink_search_returns_no_evicted_rows() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(5, 40, 16.0) else {
            // No usable system font in this environment; skip rather than fail.
            return;
        };
        for i in 0..300 {
            t.process(format!("needle-{i}\r\n").as_bytes());
        }
        assert_eq!(t.search("needle-", true, false).len() / 3, 300);

        // Shrink retention to 50 (evicting the oldest history immediately)
        // with NO intervening write: the very next search must reflect it.
        t.set_scrollback_limit(50);
        let hits = t.search("needle-", true, false);
        // 50 retained history lines + the 4 needle rows still on screen.
        assert_eq!(hits.len() / 3, 54, "evicted rows no longer match");
        // Absolute-row identity: needle-i sits at absolute row i; the oldest
        // survivor is needle-246 (300 written − 50 history − 4 on screen).
        assert_eq!(hits[0], 246, "the first match is the oldest RETAINED row");
    }

    /// `set_scrollback_limit` is one total bound across ring + staged + store:
    /// shrink evicts oldest immediately; grow widens the store share.
    #[test]
    fn set_scrollback_limit_governs_ring_retention() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(5, 40, 16.0) else {
            // No usable system font in this environment; skip rather than fail.
            return;
        };
        t.set_scrollback_limit(200);
        for i in 0..500 {
            t.process(format!("line-{i}\r\n").as_bytes());
        }
        t.scroll_to_top();
        assert_eq!(
            t.display_offset(),
            200,
            "a 200-line limit really caps retention (was a silent no-op)"
        );
        t.scroll_to_bottom();
        t.set_scrollback_limit(12_000);
        for i in 500..11_500 {
            t.process(format!("line-{i}\r\n").as_bytes());
        }
        t.scroll_to_top();
        assert_eq!(
            t.display_offset(),
            200 + 11_000,
            "retention grows past the old 10k ring cap"
        );
    }

    /// DECCKM, modifiers and Kitty negotiation flow through
    /// `Terminal::keyboard_mode()` as in aterm-wasm.
    #[test]
    fn encode_key_follows_the_live_keyboard_mode() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            // No usable system font in this environment; skip rather than fail.
            return;
        };
        assert_eq!(
            t.encode_key("ArrowUp", 0, 0, None).as_deref(),
            Some(&b"\x1b[A"[..])
        );
        assert_eq!(
            t.encode_key("ArrowUp", 1, 0, None).as_deref(),
            Some(&b"\x1b[1;2A"[..]),
            "Shift+ArrowUp must carry the modifier"
        );
        assert_eq!(
            t.encode_key("ArrowUp", 4, 0, None).as_deref(),
            Some(&b"\x1b[1;5A"[..]),
            "Ctrl+ArrowUp must carry the modifier"
        );
        t.process(b"\x1b[?1h");
        assert_eq!(
            t.encode_key("ArrowUp", 0, 0, None).as_deref(),
            Some(&b"\x1bOA"[..]),
            "DECCKM arrow must be SS3"
        );
        t.process(b"\x1b[?1l");
        // Releases without the Kitty protocol and unmappable DOM keys are None.
        assert!(t.encode_key("ArrowUp", 0, 2, None).is_none());
        assert!(t.encode_key("Shift", 0, 0, None).is_none());
        // Pushed kitty disambiguate: Shift+Enter becomes a distinct CSI-u report.
        t.process(b"\x1b[>1u");
        assert_eq!(
            t.encode_key("Enter", 1, 0, None).as_deref(),
            Some(&b"\x1b[13;2u"[..]),
            "Shift+Enter under pushed kitty disambiguate must be CSI-u"
        );
    }

    /// win32-input-mode is the host's to grant: conhost's `?9001h` changes
    /// nothing until the embedder says a ConPTY is behind the terminal
    /// (`set_win32_input_enabled`). Then Shift+Enter is the key-record pair
    /// conhost reads (down, up) instead of the legacy LF.
    #[test]
    fn a_conpty_host_opts_in_to_win32_input_records() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.process(b"\x1b[?9001h");
        assert_eq!(
            t.encode_key("Enter", 1, 0, None).as_deref(),
            Some(&[0x0a][..]),
            "program output alone grants nothing"
        );
        t.set_win32_input_enabled(true);
        t.process(b"\x1b[?9001h");
        let pair = b"\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_";
        assert_eq!(
            t.encode_key("Enter", 1, 0, None).as_deref(),
            Some(&pair[..])
        );
    }

    /// Mirrors `aterm-wasm`'s stateless-encoder test: worker-hosted engines
    /// mirror `keyboard_mode_bits` to the main thread and encode there.
    #[test]
    fn encode_key_with_mode_matches_the_instance_encoder() {
        // Stateless with mode_bits = 0: plain legacy CSI A, no instance needed.
        assert_eq!(
            encode_key_with_mode("ArrowUp", 0, 0, None, 0).as_deref(),
            Some(&b"\x1b[A"[..])
        );
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        assert_eq!(t.keyboard_mode_bits(), 0, "fresh terminal: no mode bits");
        // Bits mirrored from a terminal with DECCKM set encode SS3 arrows.
        t.process(b"\x1b[?1h");
        assert_eq!(
            encode_key_with_mode("ArrowUp", 0, 0, None, t.keyboard_mode_bits()).as_deref(),
            Some(&b"\x1bOA"[..]),
            "snapshot bits under DECCKM must be SS3"
        );
        t.process(b"\x1b[?1l");
        // Bits captured after a real kitty disambiguate push speak CSI-u.
        t.process(b"\x1b[>1u");
        assert_eq!(
            encode_key_with_mode("Enter", 1, 0, None, t.keyboard_mode_bits()).as_deref(),
            Some(&b"\x1b[13;2u"[..]),
            "snapshot bits under pushed kitty must be CSI-u Shift+Enter"
        );
        // With FRESH bits the stateless encoder is byte-identical to the
        // instance method (the one-frame-staleness contract's fixed point).
        for (key, mods, event_type) in [
            ("ArrowUp", 0u8, 0u8),
            ("ArrowUp", 1, 0),
            ("Enter", 0, 0),
            ("Enter", 1, 0),
            ("a", 4, 0),
            ("Shift", 0, 0),
            ("ArrowUp", 0, 2),
        ] {
            assert_eq!(
                t.encode_key(key, mods, event_type, None),
                encode_key_with_mode(key, mods, event_type, None, t.keyboard_mode_bits()),
                "instance and stateless encoders must agree on {key:?} mods={mods} type={event_type}"
            );
        }
    }

    /// `build_frame` re-resolves one row per echo with the damage-reading
    /// effects on: the refill consumes the damage session and the effects
    /// pipeline does not re-take it.
    #[test]
    fn build_frame_echo_refills_one_row_with_sparkle_and_rain_on() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(8, 40, 16.0) else {
            return;
        };
        t.set_sparkle_words_enabled(true);
        t.set_matrix_rain_enabled(true);
        t.process(b"$ ");
        t.build_frame();
        assert!(
            matches!(t.last_refill, Some(FrameRefill::Full { .. })),
            "the first fill is full by construction"
        );
        for (i, ch) in b"hello world".iter().enumerate() {
            t.process(std::slice::from_ref(ch));
            t.advance_effects(16.0);
            t.build_frame();
            assert_eq!(
                t.last_refill,
                Some(FrameRefill::Scoped { rows_refilled: 1 }),
                "echo #{i} must refill exactly its one row"
            );
        }
    }

    /// Every built frame's engine channels equal a full extraction across a
    /// mutation stream with the damage-reading effects on, and both refill
    /// arms are reached.
    #[test]
    fn build_frame_scoped_refill_matches_a_full_extraction_over_a_mutation_stream() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(8, 40, 16.0) else {
            return;
        };
        t.set_sparkle_words_enabled(true);
        t.set_matrix_rain_enabled(true);
        let steps: &[&[u8]] = &[
            b"$ ",
            b"e",
            b"cho hello cat",
            b"\r\n",
            b"\x1b[31mred\x1b[0m and plain",
            b"\x1b[2K\rerased and retyped",
            b"\x1b]11;#102030\x07",
            b"a line long enough to wrap past the forty column edge of this grid",
            b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n",
            b"\x1b[?1049h",
            b"alt screen text",
            b"\x1b[?1049l",
            b"x",
        ];
        let (mut scoped, mut full) = (0usize, 0usize);
        let mut check = |t: &mut AtermGpuTerminal, what: &str| {
            let (rows, cols) = (t.rows, t.cols);
            let mut oracle = t.frame_scratch.clone();
            t.term.cell_frame_into(&mut oracle, rows, cols);
            assert!(
                t.frame_scratch == oracle,
                "the built frame diverged from a full extraction after: {what}"
            );
            match t.last_refill {
                Some(FrameRefill::Scoped { .. }) => scoped += 1,
                Some(FrameRefill::Full { .. }) => full += 1,
                None => panic!("build_frame must refill"),
            }
        };
        for step in steps {
            t.process(step);
            t.advance_effects(16.0);
            t.build_frame();
            check(&mut t, &String::from_utf8_lossy(step));
        }
        t.scroll_lines(3);
        t.build_frame();
        check(&mut t, "history viewport");
        t.scroll_lines(-3);
        t.build_frame();
        check(&mut t, "back to the live bottom");
        t.resize(10, 30);
        t.build_frame();
        check(&mut t, "resize");
        t.process(b"after resize");
        t.build_frame();
        check(&mut t, "echo after resize");
        assert!(scoped >= 5, "the scoped arm must be reached ({scoped})");
        assert!(full >= 2, "the full arm must be reached ({full})");
    }

    /// Native stand-in for the effects render path (GPU init is wasm-only):
    /// refill the snapshot, apply the effects pipeline, refresh the spill band.
    fn fill_frame(t: &mut AtermGpuTerminal) {
        t.refill_frame_scratch();
        let (cw, ch) = t.cpu.cell_size();
        t.effects.apply(&mut t.term, &mut t.frame_scratch, cw, ch);
        t.spill.update(&t.cpu, &t.frame_scratch);
    }

    /// Snapshot equality across two engine instances.
    ///
    /// `RenderInput`'s `PartialEq` compares sprite atlases by `Arc` identity,
    /// which differs between instances even for identical content. Compare
    /// atlas values here, neutralize the identity difference, and let the real
    /// `PartialEq` judge every other field.
    fn same_snapshot_across_instances(a: &RenderInput, b: &RenderInput) -> bool {
        fn atlas_value_eq(
            x: &Option<std::sync::Arc<aterm_render::SceneAtlas>>,
            y: &Option<std::sync::Arc<aterm_render::SceneAtlas>>,
        ) -> bool {
            match (x, y) {
                (None, None) => true,
                (Some(x), Some(y)) => {
                    x.width == y.width
                        && x.height == y.height
                        && x.version == y.version
                        && x.rgba == y.rgba
                }
                _ => false,
            }
        }
        if !atlas_value_eq(&a.cat_atlas, &b.cat_atlas)
            || !atlas_value_eq(&a.free_atlas, &b.free_atlas)
            || !atlas_value_eq(&a.rain_atlas, &b.rain_atlas)
        {
            return false;
        }
        let mut aligned = b.clone();
        aligned.cat_atlas.clone_from(&a.cat_atlas);
        aligned.free_atlas.clone_from(&a.free_atlas);
        aligned.rain_atlas.clone_from(&a.rain_atlas);
        *a == aligned
    }

    /// Spill band on the GPU engine's CPU face: the water jump splash on the
    /// top row surfaces band bytes whose source-over composite onto the theme
    /// bg equals the CPU face's composed band exactly. Also pins strip sizing,
    /// pointer stability, idle rev/rect stillness and the 0/0-chrome identity.
    #[test]
    fn spill_exports_surface_band_content_on_the_cpu_face() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(12, 40, 16.0) else {
            return;
        };
        fill_frame(&mut t);
        assert_eq!(t.spill_len(), 0, "0/0 chrome: identity — no band, no bytes");
        assert_eq!(t.spill_rev(), 0);

        let (pad, head) = (12usize, 30usize);
        t.set_chrome(pad as u16, head as u16);
        t.set_cursor_glow(true, "water", None, None, 400, 64, 1.0, 2.0, true);
        // Seed the just-enabled engines' cursor anchor so the first witness
        // below has an owner to check against (apply is the anchor authority).
        fill_frame(&mut t);
        // The glow observes cursor motion frame to frame: keystrokes interleave
        // with frames, each witnessed through the committed-char seam as a JS
        // keydown handler would (unwitnessed motion stays dark). The row-0
        // splashes arc droplets into the head band.
        for (ch, bytes) in [('a', b"a"), ('b', b"b"), ('c', b"c")] {
            assert!(
                t.note_typed_char(ch),
                "a witnessed simple scalar must arm movement provenance"
            );
            t.process(bytes);
            t.advance_effects(30.0);
            fill_frame(&mut t);
        }
        // Find a frame with live band coverage (nonzero alpha), not merely a
        // rev tick (the erase after a droplet exits also ticks it), so parity
        // below is non-vacuous.
        let mut lit_frame = false;
        for _ in 0..40 {
            if t.spill_rev() > 0
                && t.spill_rect_count() > 0
                && t.spill
                    .rgba()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|px| px[3] != 0)
            {
                lit_frame = true;
                break;
            }
            t.advance_effects(16.0);
            fill_frame(&mut t);
        }
        let (w, h) = t.cpu.frame_size(t.rows, t.cols);
        let grid_h = h - 2 * pad - head;
        assert_eq!(
            t.spill_len(),
            (w * (pad + head) + w * pad + 2 * pad * grid_h) * 4,
            "spill buffer sized to the four band strips"
        );
        assert!(
            lit_frame,
            "witnessed splash droplets must reach the band live"
        );
        assert!(t.spill_rev() > 0, "splash droplets must reach the band");
        assert!(t.spill_rect_count() > 0, "band content must report rects");

        // Byte parity against the CPU face's composed frame.
        let input = t.frame_scratch.clone();
        let mut win = aterm_render::WindowCpu::new();
        let view = t.cpu.render_input_cached(&mut win, &input);
        assert_eq!((view.width(), view.height()), (w, h));
        let pixels = view.pixels();
        let bg = Theme::default().bg & 0x00FF_FFFF;
        let strips = [
            (0usize, 0usize, w, pad + head),
            (0, h - pad, w, pad),
            (0, pad + head, pad, grid_h),
            (w - pad, pad + head, pad, grid_h),
        ];
        let buf = t.spill.rgba();
        let mut lit = 0usize;
        let mut off = 0usize;
        for (sx, sy, sw, sh) in strips {
            for yy in 0..sh {
                for xx in 0..sw {
                    let px = &buf[(off + yy * sw + xx) * 4..][..4];
                    let (x, y) = (sx + xx, sy + yy);
                    let composed = if px[3] == 0 {
                        bg
                    } else {
                        lit += 1;
                        aterm_render::over_rgb(
                            bg,
                            (u32::from(px[0]) << 16) | (u32::from(px[1]) << 8) | u32::from(px[2]),
                            px[3],
                        )
                    };
                    assert_eq!(
                        composed,
                        pixels[y * w + x] & 0x00FF_FFFF,
                        "spill ∘ bg must equal the frame band at ({x},{y})"
                    );
                }
            }
            off += sw * sh;
        }
        assert!(lit > 0, "the splash must light band pixels (non-vacuous)");

        // An unwitnessed program leap-write goes dark: a denied relocation
        // retires the live wake, so droplets vanish and the band drains to zero.
        //
        // Two claims: the band goes dark immediately and stays dark (checked on
        // every frame of the wait), and the in-grid aurora drains to nothing on
        // its own configured envelope (~740 ms here), so the wait is 2 s and
        // the diagnostic reports how far the drain got.
        //
        // The caret's body is held out: the `water` style draws its block body
        // at the caret, which on the top row reaches the head band. Serious
        // mode stands bodies (and the warmed momentum halo) down, so the claim
        // below concerns the aurora alone.
        t.effects.set_serious(true);
        t.process(b"\x1b[1;30Hz");
        const DRAIN_FRAMES: usize = 125; // 2 s at the 16 ms step above
        let mut drained_at = None;
        let mut last_quads = usize::MAX;
        for i in 0..DRAIN_FRAMES {
            t.advance_effects(16.0);
            fill_frame(&mut t);
            let band_lit = t
                .spill
                .rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[3] != 0)
                .count();
            assert_eq!(
                band_lit, 0,
                "frame {i} after an unwitnessed leap put {band_lit} lit pixels in the \
                 band — cold program movement must not splash the chrome"
            );
            last_quads = t.frame_scratch.cursor_glow_add.len();
            if last_quads == 0 {
                drained_at = Some(i);
                break;
            }
        }
        assert!(
            drained_at.is_some(),
            "the earned aurora still had {last_quads} quads after {DRAIN_FRAMES} frames \
             ({} ms): an unwitnessed leap retires the wake and the remaining light must \
             fade on its own envelope, not persist",
            DRAIN_FRAMES * 16
        );

        // Pointer stability across an animating content frame.
        let ptr = t.spill_ptr();
        t.advance_effects(16.0);
        fill_frame(&mut t);
        assert_eq!(t.spill_ptr(), ptr, "same geometry ⇒ the export never moves");

        // Idle re-frame (no clock advance): rev and rects hold still.
        let rev = t.spill_rev();
        fill_frame(&mut t);
        assert_eq!(t.spill_rev(), rev, "idle re-frame must not tick the rev");
        assert_eq!(t.spill_rect_count(), 0);

        // 0/0 restore: the identity law again — bytes drop with the band.
        t.set_chrome(0, 0);
        fill_frame(&mut t);
        assert_eq!(t.spill_len(), 0);
        let rev = t.spill_rev();
        fill_frame(&mut t);
        assert_eq!(t.spill_rev(), rev, "steady 0/0 chrome stays still");
    }

    /// `set_chrome` before GPU init sticks on the CPU face (init re-applies it
    /// and sizes the swapchain to the padded frame); 0/0 restores exact-fit.
    #[test]
    fn set_chrome_sticks_pre_init_and_zero_restores() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        assert_eq!((t.cpu.pad(), t.cpu.head()), (0, 0), "exact-fit by default");
        t.set_chrome(8, 24);
        assert_eq!((t.cpu.pad(), t.cpu.head()), (8, 24));
        let (cw, ch) = t.cpu.cell_size();
        let (w, h) = t.cpu.frame_size(24, 80);
        assert_eq!(w, 80 * cw + 16, "frame gains 2*pad in width");
        assert_eq!(h, 24 * ch + 16 + 24, "frame gains 2*pad + head in height");
        t.set_chrome(0, 0);
        assert_eq!(t.cpu.frame_size(24, 80), (80 * cw, 24 * ch));
    }

    /// THE SWAPCHAIN HOLDS THE BAND (ruling 340): the frame `build_frame`
    /// composes is the grid's rows plus the band's, so every live sizing of
    /// the canvas surface must count them. Round 28's review found
    /// `set_chrome` sizing it from `self.rows` alone — with a committed band
    /// the surface came out a band short of the frame drawn into it. A
    /// headless test has no canvas surface to measure, so this is LEXICAL
    /// over the production source: one `resize_surface` call, inside
    /// `size_swapchain`, which sizes from `surface_rows()`; `resize`,
    /// `set_chrome` and `note_band_change` reach it; and init's own sizing
    /// passes `surface_rows()` too. Negative control run by hand: the old
    /// `frame_size(self.rows, …)` in `set_chrome` fails it.
    #[test]
    fn every_swapchain_sizing_counts_the_band_rows() {
        let src = include_str!("lib.rs");
        let cut = src
            .find(concat!(
                "#[cfg(all(test, not(target_arch = \"wasm32\")))]\n",
                "mod tests {"
            ))
            .expect("the test module");
        let prod = &src[..cut];
        let body = |sig: &str| -> &str {
            let at = prod.find(sig).unwrap_or_else(|| panic!("{sig} is gone"));
            let rest = &prod[at..];
            &rest[..rest.find("\n    }\n").expect("the fn's end")]
        };
        assert_eq!(
            prod.matches(".resize_surface(").count(),
            1,
            "one live sizing site"
        );
        let helper = body("fn size_swapchain(");
        assert!(
            helper.contains("let rows = self.surface_rows();"),
            "{helper}"
        );
        assert!(helper.contains("frame_size(rows, self.cols)"), "{helper}");
        assert!(helper.contains(".resize_surface("), "{helper}");
        for sig in [
            "pub fn resize(",
            "pub fn set_chrome(",
            "fn note_band_change(",
        ] {
            assert!(body(sig).contains("self.size_swapchain();"), "{sig}");
        }
        let sizings: Vec<&str> = prod
            .lines()
            .filter(|l| l.contains(".frame_size(") && !l.trim_start().starts_with("//"))
            .collect();
        assert_eq!(sizings.len(), 2, "the helper and init: {sizings:?}");
        for line in sizings {
            assert!(
                line.contains("frame_size(rows, self.cols)")
                    || line.contains("frame_size(self.surface_rows(), self.cols)"),
                "a sizing that forgets the band: {line}"
            );
        }
    }

    /// Defaults OFF leave every overlay channel of the snapshot empty — the
    /// GPU encode then draws the byte-identical pre-effects frame (the
    /// empty-overlay parity is gated by aterm-gpu's own CPU==GPU suite).
    #[test]
    fn effects_off_leaves_the_snapshot_overlays_empty() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.process(b"a cat said fuck\r\n");
        t.advance_effects(100.0);
        fill_frame(&mut t);
        let f = &t.frame_scratch;
        assert!(f.cursor_glow_add.is_empty(), "glow off by default");
        assert!(f.cursor_trail.is_empty(), "trail off by default");
        assert!(f.word_decorations.is_empty(), "sparkle off by default");
        assert!(f.ink.is_empty() && f.cat_quads.is_empty() && f.nova_add.is_empty());
        assert!(f.cat_atlas.is_none());
        assert!(!t.is_effects_active(), "nothing animates while off");
    }

    /// Two instances fed the same bytes and dt stream produce identical
    /// `RenderInput`s (cells and every overlay channel), and turning everything
    /// off restores the untouched-instance snapshot exactly.
    #[test]
    fn effects_snapshots_are_deterministic_and_restore_on_off() {
        let Some(mut a) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        let Some(mut b) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        let Some(mut plain) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        for t in [&mut a, &mut b] {
            t.set_sparkle_words_enabled(true);
            t.set_cursor_glow(
                true,
                "fire",
                Some(0x00FF_8844),
                None,
                400,
                24,
                0.9,
                0.8,
                true,
            );
        }
        for step in 0..30u32 {
            for t in [&mut a, &mut b, &mut plain] {
                t.process(b"kitty fuck x");
                t.advance_effects(33.0);
                fill_frame(t);
            }
            assert!(
                same_snapshot_across_instances(&a.frame_scratch, &b.frame_scratch),
                "same bytes + dt stream must produce identical snapshots (step {step})"
            );
        }
        assert!(
            a.is_effects_active()
                || !a.frame_scratch.word_decorations.is_empty()
                || !a.frame_scratch.ink.is_empty(),
            "effects actually emitted something while on"
        );
        // Toggle everything off: the snapshot must equal the never-touched one.
        for t in [&mut a, &mut b] {
            t.set_sparkle_words_enabled(false);
            t.set_cursor_glow(false, "lumen", None, None, 260, 24, 0.7, 0.6, true);
        }
        for t in [&mut a, &mut b, &mut plain] {
            t.advance_effects(33.0);
            fill_frame(t);
        }
        assert!(
            same_snapshot_across_instances(&a.frame_scratch, &plain.frame_scratch),
            "toggled off must restore the byte-identical snapshot"
        );
    }

    #[test]
    fn reports_alternate_scroll_via_decset_1007() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        assert!(!t.is_alternate_scroll(), "mode 1007 defaults off");
        t.process(b"\x1b[?1007h");
        assert!(t.is_alternate_scroll(), "1007 set → wheel becomes arrows");
        t.process(b"\x1b[?1007l");
        assert!(
            !t.is_alternate_scroll(),
            "1007 reset → wheel scrolls history"
        );
    }

    /// The word-separator setting reshapes double-click words, and clearing it
    /// restores the default class-based word logic exactly.
    #[test]
    fn set_word_separators_reshapes_double_click_words() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.process(b"foo-bar baz");
        assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo"));
        t.set_word_separators(Some(" ".to_string()));
        assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo-bar"));
        t.set_word_separators(None);
        assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo"));
    }

    // --- Cooperative width-reflow offload ------------------------------------
    //
    // The render-grace net is driven via `pump_reflow_on_render_tick` (what the
    // wasm-only `render`/`render_offscreen` call); the rest uses the JS API.

    /// Swap the engine for one whose bulk history lives in the tiered store
    /// (tiny ring → nearly all scroll-off spills to tiered) and feed `lines`
    /// numbered lines short enough not to wrap at the test widths.
    fn install_tiered_history(t: &mut AtermGpuTerminal, rows: u16, cols: u16, lines: usize) {
        let sb = aterm_core::scrollback::Scrollback::new(64, 512, 64_000_000);
        t.term = Terminal::with_scrollback(rows, cols, 8, sb);
        t.rows = t.term.grid().rows() as usize;
        t.cols = t.term.grid().cols() as usize;
        let mut buf = Vec::new();
        for i in 0..lines {
            buf.extend_from_slice(format!("L{i}-hist\r\n").as_bytes());
        }
        t.term.process(&buf);
    }

    #[test]
    fn resize_without_tiered_store_never_defers() {
        // A ring-only engine (no tiered store, e.g. restored from an old
        // checkpoint): every resize is the plain bounded path, nothing stashes.
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.term = Terminal::new(24, 80);
        for i in 0..50 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        t.resize(24, 40);
        assert!(!t.reflow_pending(), "no tiered store → no deferred job");
        assert!(!t.pump_reflow(), "nothing to pump");
        assert_eq!(t.cols, 40, "resize applied synchronously");
    }

    #[test]
    fn scrolled_wrapped_row_len_and_wrap_flag_are_tier_aware() {
        // After a width-shrink reflow overflows wrapped rows into scrollback,
        // a scrolled-back history row reports correct row_len / row_is_wrapped
        // (not just text).
        let Some(mut t) = AtermGpuTerminal::new_from_system(3, 40, 16.0) else {
            return;
        };
        let a = "A".repeat(30);
        let b = "B".repeat(30);
        let c = "C".repeat(30);
        t.process(format!("{a}\r\n").as_bytes());
        t.process(format!("{b}\r\n").as_bytes());
        t.process(c.as_bytes());

        t.resize(3, 20);
        while t.pump_reflow() {}

        t.scroll_to_top();
        assert_eq!(t.row_text(0).as_deref(), Some("A".repeat(20).as_str()));
        assert_eq!(t.row_text(1).as_deref(), Some("A".repeat(10).as_str()));

        assert_eq!(t.row_len(0), Some(20), "history head-row length");
        assert_eq!(t.row_is_wrapped(0), Some(false), "history head not wrapped");
        assert_eq!(
            t.row_len(1),
            Some(10),
            "scrolled wrapped continuation must report its materialized length, not None"
        );
        assert_eq!(
            t.row_is_wrapped(1),
            Some(true),
            "scrolled wrapped continuation must report is_wrapped=true, not None"
        );
        assert_eq!(t.row_len(99), None);
        assert_eq!(t.row_is_wrapped(99), None);
    }

    #[test]
    fn display_row_cache_distinguishes_main_and_alt_screen() {
        // The display_row_cache key includes the alt-screen bit, so a main<->alt
        // swap at a coinciding content_gen + offset + row does not serve the
        // other buffer's cell.
        let Some(mut t) = AtermGpuTerminal::new_from_system(5, 20, 16.0) else {
            return;
        };
        for _ in 0..30 {
            t.process(b"\r\n");
        }
        t.process(b"\x1b[HM");
        let main_gen = t.term.grid().content_gen();
        assert_eq!(t.cell_text(0, 0), "M", "primed main cell at (0,0)");

        t.process(b"\x1b[?1049h");
        t.process(b"\x1b[HA");
        assert!(
            t.term.grid().content_gen() <= main_gen,
            "alt buffer starts below main_gen so we can drive it up to collide"
        );
        let mut guard = 0;
        while t.term.grid().content_gen() != main_gen && guard < 10_000 {
            let g: &[u8] = if guard % 2 == 0 {
                b"\x1b[2;1Hx"
            } else {
                b"\x1b[2;1Hy"
            };
            t.process(g);
            guard += 1;
        }
        assert_eq!(
            t.term.grid().content_gen(),
            main_gen,
            "manufactured the content_gen collision across the swap"
        );

        assert_eq!(
            t.cell_text(0, 0),
            "A",
            "alt-screen cell must not be served from the main-screen cache at a coinciding key"
        );
    }

    #[test]
    fn large_tiered_history_defers_rewrap_and_stepped_pumps_reattach_it() {
        // Pumping in random small budgets re-attaches the same history a
        // one-shot rewrap yields.
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();
        assert!(before > INLINE_REFLOW_MAX_LINES, "precondition: {before}");

        t.resize(24, 40);
        assert!(t.reflow_pending(), "a deep history defers its rewrap");
        assert_eq!(t.cols, 40, "the visible grid resized synchronously");
        let during = t.term.grid().scrollback_lines();
        assert!(
            during < before / 4,
            "during the detach window only the bounded ring is visible \
             (got {during} of {before})"
        );

        // Pump in random small budgets (deterministic LCG) until done.
        let mut lcg = 0x0600_60DD_u64;
        let mut pumps = 0usize;
        loop {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
            t.pump_reflow_budget(1 + ((lcg >> 33) % 3_000) as u32);
            pumps += 1;
            assert!(pumps < 100_000, "stepped pumping must terminate");
            if !t.pump_reflow() {
                break;
            }
            assert!(
                t.reflow_pending(),
                "work remains while pump_reflow returns true"
            );
        }
        assert!(
            pumps > 4,
            "small budgets must actually chunk ({pumps} pumps)"
        );
        assert!(!t.reflow_pending(), "window closed after the final step");
        assert!(!t.pump_reflow(), "further pumps are no-ops");
        let after = t.term.grid().scrollback_lines();
        assert!(
            after >= before,
            "history preserved across detach→stepped pumps→re-attach \
             (before={before}, after={after})"
        );
    }

    #[test]
    fn resize_mid_stepping_supersedes_without_losing_history() {
        // A width change while a job is half-stepped: nothing re-detaches; the
        // job keeps its progress and still re-attaches (content valid, wrapping
        // stale until the next width change).
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();

        t.resize(24, 40);
        assert!(t.reflow_pending());
        t.pump_reflow_budget(500);
        for _ in 0..3 {
            assert!(t.pump_reflow(), "mid-stepping: work must remain");
        }
        assert!(t.reflow_pending(), "job is half-stepped");

        t.resize(24, 60); // supersede mid-stepping
        assert!(
            t.reflow_pending(),
            "the half-stepped job survives a re-resize"
        );
        assert_eq!(t.cols, 60, "the newest geometry wins for the grid");

        let mut pumps = 0usize;
        while t.pump_reflow() {
            pumps += 1;
            assert!(pumps < 100_000, "stepping must terminate");
        }
        let after = t.term.grid().scrollback_lines();
        assert!(
            after >= before,
            "no history lost across the mid-stepping supersede \
             (before={before}, after={after})"
        );
    }

    #[test]
    fn small_tiered_history_rewraps_inline_on_resize() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, 500);
        let before = t.term.grid().scrollback_lines();
        assert!(before > 100, "precondition: deep history ({before} lines)");
        t.resize(24, 40);
        assert!(
            !t.reflow_pending(),
            "≤ INLINE_REFLOW_MAX_LINES rewraps inline, mirroring the native bound"
        );
        assert!(
            t.term.grid().scrollback_lines() >= before,
            "inline rewrap preserves history"
        );
    }

    #[test]
    fn resize_during_the_window_supersedes_without_losing_history() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();
        t.resize(24, 40);
        assert!(t.reflow_pending());
        // The store is out: a second width change detaches nothing; the
        // stashed job survives and re-attaches (native supersede semantics).
        t.resize(24, 60);
        assert!(t.reflow_pending(), "the stashed job survives a re-resize");
        assert_eq!(t.cols, 60, "the newest geometry wins for the grid");
        while t.pump_reflow() {}
        assert!(
            t.term.grid().scrollback_lines() >= before,
            "no history lost across the superseding resize"
        );
    }

    #[test]
    fn never_pumped_host_completes_via_the_render_grace_net() {
        // A host that never calls `pump_reflow`: the render-tick net closes the
        // window alone, one bounded step per frame after grace.
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();
        t.resize(24, 40);
        assert!(t.reflow_pending());
        t.pump_reflow_on_render_tick();
        assert!(t.reflow_pending(), "no pump inside the grace window");
        for _ in 0..REFLOW_PUMP_GRACE_RENDERS {
            t.pump_reflow_on_render_tick();
        }
        assert!(
            t.reflow_pending(),
            "a multi-step job is still converging right after grace \
             (one bounded step per frame — not one catch-up frame)"
        );
        let mut frames = 0usize;
        while t.reflow_pending() {
            t.pump_reflow_on_render_tick();
            frames += 1;
            assert!(
                frames < 10_000,
                "the render net must converge across frames"
            );
        }
        assert!(
            t.term.grid().scrollback_lines() >= before,
            "history restored by the per-frame safety-net steps"
        );
    }

    #[test]
    fn never_pumped_host_streaming_output_converges_via_the_backlog_net() {
        // A host that neither pumps nor renders but keeps feeding output: past
        // the cap every `process` call advances one bounded step, so the window
        // closes after finitely many calls.
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        t.resize(24, 40);
        assert!(t.reflow_pending());
        let mut buf = Vec::new();
        for i in 0..(REFLOW_BACKLOG_MAX_LINES + 2_000) {
            buf.extend_from_slice(format!("W{i}-window\r\n").as_bytes());
        }
        t.process(&buf); // blows past the cap; each further call steps once
        let mut calls = 0usize;
        while t.reflow_pending() {
            t.process(format!("T{calls}-tail\r\n").as_bytes());
            calls += 1;
            assert!(
                calls < 1_000,
                "the backlog net must converge within finitely many process \
                 calls (still pending after {calls})"
            );
        }
        assert!(
            t.term.grid().scrollback_lines() > INLINE_REFLOW_MAX_LINES,
            "history survived the stream-while-detached window"
        );
    }

    #[test]
    fn teardown_with_a_half_stepped_job_drops_cleanly() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        t.resize(24, 40);
        assert!(t.reflow_pending());
        t.pump_reflow_budget(100);
        assert!(t.pump_reflow(), "step partway before teardown");
        assert!(t.reflow_pending(), "job is half-stepped at drop time");
        drop(t);
    }

    /// Budgeted search: resumed slices complete to the one-shot triplets, a
    /// superseded cursor restarts, and empty-query / invalid-regex edges give
    /// an empty complete result.
    #[test]
    fn search_budgeted_matches_one_shot_and_restarts_when_superseded() {
        let Some(mut t) = AtermGpuTerminal::new_from_system(6, 40, 16.0) else {
            return;
        };
        for i in 0..60 {
            t.process(format!("row {i} NEEDLE-{i} tail\r\n").as_bytes());
        }
        let one_shot = t.search("NEEDLE", true, false);
        let mut cursor = None;
        let mut all_matches = Vec::new();
        let mut search_id = None;
        let step = loop {
            let step = t.search_budgeted("NEEDLE", true, false, cursor, 9);
            if step.reset() {
                assert!(search_id.is_none(), "only the first turn resets here");
                all_matches.clear();
                search_id = step.search_id();
            } else {
                assert_eq!(step.search_id(), search_id);
            }
            all_matches.extend(step.matches());
            if step.complete() {
                break step;
            }
            assert!(step.cursor().is_some(), "incomplete step carries a cursor");
            cursor = step.cursor();
        };
        assert_eq!(step.cursor(), None, "complete step drops the cursor");
        assert_eq!(all_matches, one_shot, "budgeted equals one-shot");
        assert_eq!(step.lowest_retained_line(), 0);

        // Supersession: a new pattern with the old cursor restarts progress.
        let first = t.search_budgeted("NEEDLE", true, false, None, 5);
        assert!(!first.complete());
        assert!(first.reset());
        let first_id = first.search_id();
        let switched = t.search_budgeted("row", true, false, first.cursor(), 5);
        assert!(switched.rows_fed() <= 5, "superseded cursor restarted");
        assert!(switched.reset());
        assert_ne!(switched.search_id(), first_id);
        t.search_budgeted_cancel();
        // Edges: empty query and invalid regex answer empty complete results.
        let empty = t.search_budgeted("", true, false, switched.cursor(), 5);
        assert!(empty.complete() && empty.matches().is_empty());
        assert!(empty.reset() && empty.search_id().is_none());
        let bad = t.search_budgeted("f(oo", false, true, None, 5);
        assert!(bad.complete() && bad.matches().is_empty());
        assert!(bad.reset() && bad.search_id().is_none());
    }
}
