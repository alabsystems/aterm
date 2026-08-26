// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
//
// `aterm-gpu-web` — the GPU rendering substrate for the Electron renderer.
//
// Sibling of `the aterm-wasm crate`: that crate parses PTY bytes with the aterm
// engine (`aterm-core`) and rasterizes the grid on the CPU (`aterm-render`),
// then JS `putImageData`s the RGBA frame onto a `<canvas>`. THIS crate keeps the
// same engine front-end but renders on the GPU via `aterm-gpu` (wgpu's WebGL2
// backend — orca's deliberate terminal-acceleration target; production refuses
// unsafe-WebGPU), drawing straight into a `<canvas>` WebGL2 surface — no CPU
// readback, no `putImageData`, on the primary present path.
//
// The init path is ASYNC: a browser cannot block the main thread, so adapter +
// device acquisition is `await`ed (`wasm_bindgen_futures`), NOT blocked on (the
// native aterm-gpu path's own `block_on`). The surface is created from the
// `HtmlCanvasElement` via wgpu's `SurfaceTarget::Canvas`. The async core
// (`GpuContext::from_instance`) and the canvas surface path are backend-agnostic,
// so the WebGL backend reuses them unchanged.
//
// SCOPE (this file): a COMPILING wasm32 GPU pipeline + a real WebGL2-from-canvas
// init that configures the swapchain, plus a `render` that draws the ACTUAL
// terminal grid — aterm-gpu's instanced-cell-quad encode (glyph atlas + bg/glyph/
// cursor quads rendered offscreen, then blitted into the canvas swapchain) via
// `present_input`. A secondary offscreen render+readback path (`render_offscreen`
// + `rgba`/`width`/`height`) returns the framebuffer bytes so an e2e harness can
// pixel-compare GPU vs CPU even where reading the live canvas is awkward.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

mod effects_api;
mod notifications_api;
mod predict_api;
mod scroll_input_api;
mod scrollback_tiers_api;

use aterm_core::grid::{PendingScrollbackReflow, ReflowStep};
use aterm_core::selection::SmartSelection;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{BlockText, ClipboardAccess, CursorStyle, MouseMode, Rgb, Terminal};
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::{RenderInput, Renderer, SpillBand, Theme};

// ---------------------------------------------------------------------------
// Cooperative (thread-free) width-reflow offload — the wasm L0-freeze fix.
// Mirrors `aterm-wasm` exactly (see that crate's module-level comment for the
// full design): this process is single-threaded by target, so the native
// worker-thread offload has no thread here; instead `resize` detaches the
// tiered history in O(1) and the rewrap runs in LATER, budget-bounded host
// tasks — each `pump_reflow` (host-called, or the render-grace /
// output-backlog safety nets) advances one `reflow_step` slice, so no single
// event-loop task grows with session history.
// ---------------------------------------------------------------------------

/// Histories at or below this many lines are rewrapped INLINE by `resize`
/// (bounded, imperceptible). Mirrors aterm-gui's `INLINE_REFLOW_MAX_LINES`
/// (app_render.rs) and aterm-wasm's twin constant.
const INLINE_REFLOW_MAX_LINES: usize = 20_000;

/// Never-pumped-host safety net #1: `render`/`render_offscreen` calls after a
/// deferred rewrap is stashed before the module pumps it itself (~2s at
/// 60fps — long enough for an updated host's idle-scheduled pump to win).
/// After the grace window, ONE budgeted step per frame.
const REFLOW_PUMP_GRACE_RENDERS: u32 = 120;

/// Never-pumped-host safety net #2: once the detach-window lazy-buffer
/// backlog exceeds this bound, EVERY `process` call pumps one budgeted step,
/// so a host that never renders and never pumps still converges within
/// finitely many calls — amortized convergence with every task short, instead
/// of one unbounded catch-up pump (see aterm-wasm's twin constant).
const REFLOW_BACKLOG_MAX_LINES: usize = 20_000;

/// Default per-pump rewrap budget in INPUT history lines (host-tunable via
/// [`AtermGpuTerminal::pump_reflow_budget`]). Sizing note in aterm-wasm's twin
/// constant: measured ~1.4 µs per near-full 80-col line native release
/// (aterm-grid's `reflow_step_timing`, 2026-07-14), so 2_000 ≈ ~3ms native —
/// a short task even at typical wasm slowdowns.
const REFLOW_STEP_BUDGET_LINES: usize = 2_000;

// GpuContext is used only by the wasm async init path (`init`); on the native
// target (a compile-verification surface only) it would be unused.
#[cfg(target_arch = "wasm32")]
use aterm_gpu::GpuContext;
#[cfg(any(target_arch = "wasm32", test))]
use aterm_gpu::SurfacePresentFailure;
use aterm_gpu::{GpuRenderer, GpuSurface, WindowGpu};

std::thread_local! {
    /// Host-registered font blobs, marshaled across the JS/wasm boundary ONCE
    /// per module (this GPU module has its own linear memory — its own registry):
    /// per-pane engine builds reference them by `u32` handle, so the ~100–400MB
    /// face blobs are never re-copied per pane. The transient per-call copies
    /// otherwise fragment the linear memory into a per-pane high-water ratchet.
    static REGISTERED_FONTS: std::cell::RefCell<Vec<std::sync::Arc<Vec<u8>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Register a font blob for handle-based reuse by every engine in this module.
/// Content-interned: registering identical bytes returns a handle to ONE shared
/// copy (re-registration returns the same storage, so handles stay cheap).
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

/// Construction is split in two, matching the browser lifecycle:
///   1. [`AtermGpuTerminal::new`] — synchronous: build the engine grid + a CPU
///      face from injected font bytes (for cell metrics / the glyph atlas). No
///      GPU touched yet, so it can run before WebGL is confirmed.
///   2. [`AtermGpuTerminal::init`] — async: acquire the GPU and create +
///      configure the canvas surface. Separated so the host can fall back to the
///      CPU path (`the aterm-wasm crate`) if WebGL is unavailable WITHOUT having
///      paid for the engine teardown.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct AtermGpuTerminal {
    term: Terminal,
    // CPU face: owns the glyph rasterizer + cell metrics. Reused for cols/rows
    // sizing here, and handed to the GPU renderer to build the glyph atlas.
    cpu: Renderer,
    rows: usize,
    cols: usize,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    theme: Theme,
    // Read only by the wasm GPU paths (`init` rebuilds the face from these). On the
    // native verification target they are stored-but-unread.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    font_bytes: Vec<u8>,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    px: f32,
    // GPU side: None until `init` succeeds. Once set, `render` presents on the GPU;
    // the host wires `render` into a requestAnimationFrame loop.
    gpu: Option<GpuState>,
    // Offscreen readback cache: the last `render_offscreen` frame, expanded to
    // RGBA8 (width*height*4 bytes), so an e2e harness can pixel-compare GPU vs CPU
    // without reading the live canvas. Mirrors `the aterm-wasm crate`'s `rgba` buffer.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    rgba: Vec<u8>,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fb_width: usize,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fb_height: usize,
    // Built-in smart-selection rules (url/file_path/email/...) for scroll-correct
    // link detection via smart_word_at; reused across link_at calls. Mirrors
    // the aterm-wasm crate so the ONE engine per pane serves both draw + state.
    smart: SmartSelection,
    // Host-injected OS fallback faces (CJK/symbols + colour emoji). Kept so `init`
    // can RE-APPLY them to the fresh GPU CPU face it builds from `font_bytes`
    // (which lacks the fallbacks); fonts injected before init would otherwise be
    // lost. Empty until the host calls `set_fallback_font` / `set_emoji_font`.
    // INTERNED Arc (shared across panes via aterm_render::intern_font_bytes_slice) so
    // this reinit-retention isn't a per-pane ~180MB (emoji) / ~100MB (CJK) duplicate.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fallback_font: Option<std::sync::Arc<Vec<u8>>>,
    // ADDITIONAL fallback faces appended via `add_fallback_font` (most-preferred
    // first), kept so `init` can re-apply the whole chain to the fresh GPU CPU
    // face. Interned Arcs (shared across panes) so this retention isn't a per-pane
    // duplicate. Empty until the host appends a second fallback face.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fallback_chain_extra: Vec<std::sync::Arc<Vec<u8>>>,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    emoji_font: Option<std::sync::Arc<Vec<u8>>>,
    // Host-injected REAL bold weight, kept so `init` re-applies it to the fresh GPU
    // CPU face (built from `font_bytes`, which lacks the bold variant otherwise).
    // Interned Arc (shared across panes). None until `set_bold_font`.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    bold_font: Option<std::sync::Arc<Vec<u8>>>,
    // Host-injected SYMBOL fallback face, kept so `init` re-applies it to the fresh
    // GPU CPU face (built from `font_bytes`, which lacks the symbol face otherwise).
    // Interned Arc (shared across panes). None until `set_symbol_font`.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    symbol_font: Option<std::sync::Arc<Vec<u8>>>,
    // Live line-height multiplier (the host's terminalLineHeight), re-applied to the
    // fresh GPU CPU face at `init`. `1.0` until the host calls `set_line_height`.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    line_height: f32,
    // Host-selected text shaping (ligatures + OpenType `font_features`). Applied to
    // both the CPU face and the live GPU face by `set_ligatures`/`set_font_features`,
    // and RE-APPLIED in `init` to the fresh GPU CPU face (built from `font_bytes`
    // alone, which starts at the default shaping) so a setting chosen before init
    // survives the rebuild — same retention contract as the fallback/emoji faces.
    text_shaping: aterm_render::TextShapingConfig,
    // Reused per-frame engine snapshot for the present/offscreen paths. `render`
    // (the rAF present path) and `render_offscreen` refill this in place via
    // `cell_frame_into` instead of allocating a fresh `RenderInput` (the outer
    // container Vecs + a per-row inner Vec for each row) every frame — mirrors the
    // native windowed frontend's kept `input_scratch`. On the native verification
    // target the present paths are unused, so the field is stored-but-unread.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    frame_scratch: RenderInput,
    // The shared visual-effects pipeline (cursor aurora/trail + sparkle words) —
    // the SAME state machines the native app drives, host-clocked via
    // `advance_effects`. Defaults OFF: `apply` then only clears the (already
    // empty) overlay channels — byte-identical output. Mirrors aterm-wasm.
    pub(crate) effects: EffectsPipeline,
    // Theme cursor colour (0x00RRGGBB): the default the glow/trail colours
    // derive from when the host passes none (the native resolution).
    pub(crate) theme_cursor: u32,
    // Theme colours needed by the backend-neutral PHOSPHOR resolver.
    pub(crate) theme_fg: u32,
    pub(crate) theme_bg: u32,
    // Bounded OSC 9/99/777 notification queue, fed by the engine callbacks
    // wired at construction and drained by `take_notifications`. pub(crate)
    // so the notifications_api module reaches it (the effects-field posture).
    pub(crate) notifications: notifications_api::NotificationQueue,
    // Sub-row scroll input accumulator (fractional/pixel wheel deltas): whole
    // rows flip into `scroll_display`, the residual presents as the M1b band
    // shift at render time. pub(crate) so the scroll_input_api module (and
    // tests) reach it (the effects-field posture). Mirrors aterm-wasm.
    pub(crate) scroll_input: scroll_input_api::ScrollInputState,
    // Mosh-style predictive local echo (the shared aterm-predict state machine —
    // the SAME predictor the native app runs). Default mode Off ⇒ fully inert
    // until the host opts in via set_predictive_echo. pub(crate) so the
    // predict_api module (and tests) reach it (the effects-field posture).
    // Mirrors aterm-wasm.
    pub(crate) predict: aterm_predict::Predictor,
    // Resident scratch row for the predictive-echo reconcile probe (the GPU
    // twin of aterm-gui's `pred_row_scratch`): `Predictor::reconcile` runs its
    // observe closure once per retired guess plus the head, all on the SAME
    // row, so `Terminal::render_row` would allocate a fresh Vec and re-resolve
    // the whole row (palette, decorations, the lot) once per pending guess just
    // to read one `ch`. `render_row_into` refills this buffer in place instead.
    // Mirrors aterm-wasm.
    pub(crate) pred_row_scratch: Vec<aterm_core::terminal::RenderCell>,
    // Chrome-band spill rasterizer (the cross-pane window-space effects
    // export): refreshed per `render`/`render_offscreen` from the CPU face's
    // geometry (set_chrome keeps it and the live GPU renderer in lockstep) —
    // spill is CPU integer math over the emission streams, identical on both
    // engines by construction. Identity at 0/0 chrome. Mirrors aterm-wasm.
    pub(crate) spill: SpillBand,
    // A deferred width-change scrollback rewrap (the cooperative offload; see
    // the module-level constants). `Some` while the tiered history is detached
    // awaiting `pump_reflow` / safety-net pumps; carries its own stepping
    // progress between pumps; dropped with the engine at teardown (the detach
    // window cannot outlive the grid it belongs to). Mirrors aterm-wasm.
    pending_reflow: Option<PendingScrollbackReflow>,
    // Render-call countdown for safety net #1 (armed to
    // REFLOW_PUMP_GRACE_RENDERS when a job is stashed).
    reflow_grace: u32,
    // Per-pump `reflow_step` budget in input lines (REFLOW_STEP_BUDGET_LINES
    // unless the host tuned it via `pump_reflow_budget`).
    reflow_budget: usize,
    // Single-slot cache of the last DISPLAY row read cell-by-cell (cell_text /
    // cell_is_wide). A scrolled-back row is a HISTORY row that visible_row_view
    // materializes from scrollback, so a host walking it per cell re-materializes
    // the whole row every access (O(cols²) per row). Caching the once-materialized
    // row collapses the walk to O(cols). Keyed by (content_gen, display_offset,
    // row) so any write, resize, or scroll invalidates it. Mirrors aterm-wasm.
    display_row_cache: std::cell::RefCell<GpuDisplayRowCache>,
    // This pane's membership in the module-global scrollback byte budget
    // (audit E1): registered at construction, share applied at the frame/drain
    // boundaries. pub(crate) so scrollback_tiers_api reaches it. Mirrors
    // aterm-wasm.
    pub(crate) budget_share: aterm_core::terminal::scrollback_shared_budget::ScrollbackBudgetShare,
    // ---- WF-1 twin readiness (aterm-wasm's settled-frame gate, mirrored here).
    // The CPU twin's `render()` skips its whole refill->diff->raster pipeline
    // when nothing observable changed, proving "nothing changed" from
    // (damage_epoch, THIS counter, effects_active): the counter is the host
    // half — every wasm-layer mutator of renderer-held/presentation state the
    // engine's damage epoch cannot see bumps it.
    //
    // THIS CRATE NOW HAS THAT GATE (see `open_frame`), so the counter is READ,
    // not merely maintained: a mutator that forgets to bump can serve a stale
    // frame. That raised the bar on the mutator audit, and the audit was
    // finished to match — every `pub fn` here that changes anything the
    // renderer or the engine snapshot can see bumps, in lib.rs as well as in
    // the mirrored `effects_api` half (crates/aterm-effects/tests/
    // web_binding_parity.rs enforces the latter's parity with the twin, and
    // `every_visual_mutator_bumps_the_host_visual_generation` pins the former).
    // The bump is deliberately cheap and deliberately over-eager: a spurious
    // bump costs exactly one frame, a missing one costs correctness.
    host_visual_gen: u64,
    // The gate key of the last frame BUILT and handed to the canvas present.
    // `None` = "the canvas cannot be assumed to hold any frame of ours", which
    // is the state after construction, after `init` (re)creates the surface,
    // after a failed present, and after an offscreen readback.
    last_frame_key: Option<FrameGateKey>,
    // The `scroll_frac_px` the last presented frame carried. A frame that
    // RELEASES a banked sub-row residual moves every pixel of the band without
    // any grid damage, so the gate must refuse one more frame after a nonzero
    // residual — the E3 frac clause, mirrored from the twin.
    last_present_frac: i32,
    // `last_render_skipped()` — the gate's two-sided reach witness for tests
    // and benches.
    last_render_gated: bool,
    // Shadow of the last blink phase handed to the renderer, so a host timer
    // that re-asserts the SAME phase (coarse timers do) does not force a
    // render. `None` = never set (the first set always bumps). Mirrors the twin.
    blink_phase_shadow: Option<bool>,
    // Shadow of the last hollow-cursor override — same de-dup contract.
    hollow_shadow: Option<bool>,
}

/// The WF-1 frame-gate key: every input of a rendered frame that can change
/// between two `render()` calls at stable dims/config. Grid content folds into
/// `damage_epoch` (one u64 that advances iff a damage session existed); host
/// visual state folds into `host_visual_gen`; an ACTIVE effects pipeline never
/// skips (it animates by definition), and the active->idle transition renders
/// exactly one more frame (the key differs on `effects_active`) so the settled
/// overlay channels are painted before the gate closes. Byte-for-byte the CPU
/// twin's key (`aterm-wasm`), because it gates the same engine on the same host
/// loop — only the present at the end differs.
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
    /// resolved at. The main and alt grids keep INDEPENDENT `content_gen`
    /// counters (both seeded at 1), so a screen swap landing on a coinciding
    /// `content_gen`+`display_offset`+`row` must still invalidate — hence the
    /// alt-screen bit (mirrors `CachedSearchIndex`'s key).
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
    // Per-window present state (prior-frame snapshot for the scissored dirty-row
    // present path). One per surface, per aterm-gpu's design. Drives the
    // `present_input` (canvas) and `render_input` (offscreen readback) paths.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    win: WindowGpu,
}

/// Convert the GPU crate's typed swapchain failure into the string error exposed
/// across the wasm-bindgen boundary. Keeping this seam target-neutral lets the
/// native test suite prove that the browser API cannot silently turn a dropped
/// present into `Ok(())`.
#[cfg(any(target_arch = "wasm32", test))]
fn webgl_present_result(result: Result<(), SurfacePresentFailure>) -> Result<(), String> {
    result.map_err(|failure| format!("WebGL canvas present failed: {failure:?}"))
}

impl AtermGpuTerminal {
    /// ---- WF-1 FRAME GATE ---------------------------------------------------
    /// Run the two frame-boundary pumps, then decide whether this tick has
    /// anything to draw. `Some(key)` = draw it and record `key`; `None` = this
    /// frame is byte-identical to the last one presented, so skip the whole
    /// build AND the whole present.
    ///
    /// The pumps run FIRST and outside the decision, exactly as in the twin: a
    /// reflow re-attach marks full damage, so running it before the key is read
    /// is what lets the epoch term see it.
    ///
    /// Gate terms and why each is sufficient:
    /// - `damage_epoch`: advances iff the grid changed since the session
    ///   `build_frame` consumed — writes, scrolls, erases, resizes, recolours.
    /// - `host_visual_gen`: every wasm-layer mutator of renderer-held or
    ///   presentation state (fonts, theme, palette, selection, blink, hollow,
    ///   chrome, spill config, effects config, ...) bumps it.
    /// - `effects_active`: an active pipeline animates every frame — never
    ///   skip; the active->idle edge changes the key, buying the one settle
    ///   frame that paints the cleared overlay channels.
    /// - frac terms: a pending or just-released sub-row translate re-presents
    ///   the whole band even with zero damage (the E3 frac clause).
    /// - `pending_reflow`: belt-and-braces bail; it forces its own repaint.
    ///
    /// SKIPPING THE PRESENT IS SAFE ON WEBGL: an untouched canvas keeps its
    /// last composited image, which is exactly the frame `last_frame_key`
    /// describes — that is the whole reason the key is dropped whenever the
    /// canvas may NOT hold that frame (a failed present, a fresh surface from
    /// `init`, an offscreen readback).
    ///
    /// AND IT LEAVES aterm-gpu's PRIOR-FRAME BOOKKEEPING COHERENT, which is the
    /// non-obvious half. `present_input`'s scissored dirty-row repaint is valid
    /// only while the persistent offscreen still holds the prior PRESENTED
    /// frame (`WindowGpu::prev_input` + its `PresentPrev` validity flag). A
    /// gated tick returns BEFORE `present_input`, so it touches neither: the
    /// next real present diffs against the frame that is genuinely on glass,
    /// exactly as if the gated ticks had never been called. A gate placed
    /// INSIDE the present — after the encode had already run against a stale
    /// base — would not have that property.
    fn open_frame(&mut self) -> Option<FrameGateKey> {
        // Deferred-reflow safety net #1: a host that never calls `pump_reflow`
        // still re-attaches its rewrapped history within the grace window.
        self.pump_reflow_on_render_tick();
        // Frame-boundary scrollback maintenance (audit E1): apply a pending
        // global-budget share, then promote one bounded staged batch — LZ4
        // lives HERE, never on the ingest path (SCROLL-1).
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

    /// Record the frame `build_frame` just produced as the one now on glass, so
    /// an unchanged successor can skip. (The epoch in `key` was latched before
    /// `build_frame`'s `take_damage`, and `take_damage` never changes the epoch
    /// VALUE — only re-arms it — so an idle successor re-reads the same number
    /// and matches.)
    fn record_presented_frame(&mut self, key: FrameGateKey) {
        self.last_frame_key = Some(key);
        self.last_present_frac = self.frame_scratch.scroll_frac_px;
    }

    /// Build ONE frame into the kept scratch: the whole engine-side half of a
    /// render tick, shared verbatim by the canvas present (`render`), the
    /// offscreen readback (`render_offscreen`) and the native
    /// `render_headless` seam so all three can never drift apart.
    ///
    /// Order matters and mirrors the aterm-wasm twin exactly: engine refill ->
    /// damage consume -> effects overlay channels -> sub-row scroll stamp ->
    /// chrome spill export. The spill refresh deliberately precedes any GPU
    /// access: it is CPU math over the emission streams, so the exports stay
    /// coherent even when a present fails, and stay testable without a GPU.
    /// The two frame-boundary pumps are NOT here — they belong to
    /// `open_frame`, which runs them on every tick including gated ones.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn build_frame(&mut self) {
        // Refill the kept scratch in place rather than allocating a fresh snapshot
        // each rAF frame; `term`, `frame_scratch`, and `gpu` are disjoint fields, so
        // the fill borrow ends before any `gpu` borrow in the callers.
        self.refill_frame_scratch();
        // WF-1: consume the damage session the snapshot above just captured, so
        // the NEXT net-new grid change opens a fresh session and advances the
        // epoch `open_frame` compares. Before the gate existed nothing on this
        // path called `take_damage`, so the epoch advanced exactly once per
        // instance lifetime and could not serve as a change detector at all.
        // Safe here for the same reason it is safe in the twin: aterm-gpu
        // diffs SNAPSHOTS rather than reading the tracker, this loop is the
        // engine's only damage consumer in this crate, and `take_damage` does
        // not change the epoch VALUE — so the `term.damage_epoch() ==
        // input.snapshot_seq` identity the effects pipeline checks still holds.
        self.term.take_damage();
        let rows = self.rows;
        // Fill the overlay channels (aurora/trail/sparkle) for the host-advanced
        // instant; with every effect off this only clears the channels a reused
        // scratch may carry — byte-identical to the pre-effects present.
        let (cw, ch) = self.cpu.cell_size();
        self.effects
            .apply(&mut self.term, &mut self.frame_scratch, cw, ch);
        // Present the banked sub-row scroll residual via the M1b band shift
        // (the whole canvas frame is grid — no spliced chrome rows). Stamped
        // every frame: the KEPT scratch would otherwise carry a stale shift.
        self.scroll_input.stamp(&mut self.frame_scratch, rows, ch);
        // Refresh the chrome-band spill export from this frame's snapshot —
        // BEFORE any GPU access: spill is CPU math over the emission streams
        // (present-independent), which keeps the exports coherent even when a
        // present fails and natively testable without a GPU. The fingerprint
        // short-circuit makes a second same-frame pass (render_offscreen) free.
        self.spill.update(&self.cpu, &self.frame_scratch);
    }

    /// Refill every engine-owned frame channel. `cell_frame_into` includes the
    /// live implicit background and cursor colour, so sparse tails, OSC
    /// 10/11/12 resets, and DECSCNM remain one coherent terminal snapshot.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn refill_frame_scratch(&mut self) {
        self.term
            .cell_frame_into(&mut self.frame_scratch, self.rows, self.cols);
    }

    /// WF-1 (twin readiness): record a host-visible visual change the engine's
    /// damage epoch cannot see — renderer-held or presentation state.
    ///
    /// In aterm-wasm this reopens the settled-frame gate for exactly one frame.
    /// Here there is no frame gate yet, so the bump is maintained but not read;
    /// see the `host_visual_gen` field for why it is kept real rather than
    /// stubbed out. Idempotence is the caller's choice: unconditional
    /// callers (the mirrored `effects_api` mutators) simply buy one render.
    pub(crate) fn note_host_visual_change(&mut self) {
        self.host_visual_gen = self.host_visual_gen.wrapping_add(1);
    }

    /// Read one `(grapheme, is_wide)` display cell through the single-slot row
    /// cache, refreshing it from [`display_row_grapheme_cells`] only on a
    /// `(content_gen, display_offset, row)` change. Collapses a host's per-cell
    /// walk of a scrolled-back row from O(cols²) to O(cols). `None` for an
    /// out-of-range row/col. Mirrors the aterm-wasm twin.
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
    /// Build a `rows`x`cols` terminal. `font_bytes` (a TTF/OTF) is injected by the
    /// host (fetched in JS) — the engine does no filesystem font discovery on
    /// wasm. `px` is the cell font-size; `fg`/`bg`/`cursor`/`selection` are
    /// 0x00RRGGBB and seed the DEFAULT theme (per-cell SGR colors still flow
    /// through the grid independently).
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
        console_error_panic_hook::set_once();
        let theme = Theme {
            fg,
            bg,
            cursor,
            selection,
        };
        // Build the CPU face now (cheap, GPU-independent) so cell metrics are
        // available before WebGPU init and the host can size the canvas.
        let mut cpu = Renderer::from_bytes(font_bytes, px, theme)?;
        // No filesystem on the web: a real miss surfaces via
        // `take_missing_font_classes` (E1 lazy fonts), never system discovery.
        cpu.set_runtime_font_discovery(false);
        // The engine clamps rows/cols to the grid's 1..=4096 ingress bound (a
        // hostile JS caller can hand any u16, e.g. `new AtermGpuTerminal(8192,8192,…)`);
        // read the CLAMPED dims back and store THOSE. `render`/`render_offscreen`/`init`
        // size the GPU framebuffer from these `self.rows`/`self.cols` (via `frame_size`
        // → `offscreen_texture`/swapchain), so keeping a separate unclamped copy would
        // drive an oversized texture (wgpu validation abort) or a ~tens-of-GB alloc and
        // diverge the framebuffer from the grid the engine actually holds.
        // Tiered store attached at construction (audit E1); compression drains
        // at the render frame boundary (see scrollback_tiers_api).
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
            px,
            gpu: None,
            rgba: Vec::new(),
            fb_width: 0,
            fb_height: 0,
            smart: SmartSelection::with_builtin_rules(),
            fallback_font: None,
            fallback_chain_extra: Vec::new(),
            emoji_font: None,
            bold_font: None,
            symbol_font: None,
            line_height: 1.0,
            text_shaping: aterm_render::TextShapingConfig::default(),
            frame_scratch: RenderInput::empty(),
            effects: EffectsPipeline::new(),
            theme_cursor: cursor & 0x00FF_FFFF,
            theme_fg: fg & 0x00FF_FFFF,
            theme_bg: bg & 0x00FF_FFFF,
            notifications,
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

    /// Feed raw PTY output bytes into the engine.
    pub fn process(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
        self.pump_reflow_on_output();
    }

    /// Feed PTY output as a JS string. wasm-bindgen encodes it (UTF-8, via
    /// `encodeInto`) straight into wasm memory, so the host avoids a separate
    /// JS-side `TextEncoder.encode` allocation + copy on the hot output path.
    /// Byte-identical to `process(new TextEncoder().encode(s))`.
    pub fn process_str(&mut self, s: &str) {
        self.term.process(s.as_bytes());
        self.pump_reflow_on_output();
    }

    /// Inject a broad-coverage (CJK + symbols) fallback face from font bytes, so
    /// glyphs the primary face lacks render real shapes instead of `.notdef` tofu.
    /// Applies to the CPU face (metrics) and the live GPU face if `init` already
    /// ran; the bytes are also remembered so `init` re-applies them to the fresh
    /// GPU face it builds. No-throw: a bad blob leaves the existing faces untouched.
    pub fn set_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_fallback_bytes(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_fallback_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        // Borrowed-slice intern: identical lookup + identical returned Arc, but it
        // allocates ONLY for a genuinely new blob. Panes 2..N inject the same OS
        // face, so the owned twin's `to_vec` was a ~100MB copy that the store then
        // memcmp'd against the existing entry and dropped (wasm memory never shrinks).
        self.fallback_font = Some(aterm_render::intern_font_bytes_slice(bytes));
        // set_* RESETS the chain, so any previously appended extras are gone.
        self.fallback_chain_extra.clear();
        Ok(())
    }

    /// APPEND another fallback face to the chain (does NOT reset it like
    /// [`set_fallback_font`]). Applies to the CPU face and the live GPU face if
    /// `init` already ran; the bytes are also remembered so `init` re-applies the
    /// whole chain to the fresh GPU face. Lets the host push a CJK fallback then
    /// Arabic/Devanagari/Thai/Hebrew faces so a glyph the earlier faces miss still
    /// reaches a covering face. No-throw: a bad blob leaves the chain untouched.
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
    /// ColorEmoji colour path. Same wiring as [`set_fallback_font`]. No-throw
    /// (the `String` Err surfaces as a catchable JS exception).
    pub fn set_emoji_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change(); // WF-1 gate
                                        // Intern ONCE up front and install/retain that shared Arc, mirroring the
                                        // `_registered` twin below. The old shape paid THREE full ~180MB copies of
                                        // the same slice (CPU install, GPU install, retention) where the last two
                                        // were pure waste: `set_color_font_bytes` already interned the blob, so the
                                        // retention intern only memcmp'd a fresh copy against that very entry and
                                        // dropped it. `set_color_font_arc` runs the identical `ttf_parser` validation
                                        // and ends in the identical `install_color_font`, so the no-throw contract
                                        // and the installed face are unchanged — only the copies are gone. (A
                                        // MALFORMED blob now lands in the intern store before validation rejects it,
                                        // exactly as `register_font` above already does; no new class of retention.)
        let arc = aterm_render::intern_font_bytes_slice(bytes);
        self.cpu.set_color_font_arc(arc.clone())?;
        if let Some(gpu) = self.gpu.as_mut() {
            // The live GPU face still takes its own copy (no Arc twin on
            // GpuRenderer yet) — rare, since the worker seeds fonts before `init`,
            // so `gpu` is None during pane builds.
            gpu.renderer.set_emoji_font_bytes(bytes.to_vec())?;
            gpu.win.invalidate_present();
        }
        self.emoji_font = Some(arc);
        Ok(())
    }

    /// Inject a REAL bold weight of the primary family so SGR-bold cells render as a
    /// true heavier weight instead of synthetic embolden. Applies to the CPU face
    /// and the live GPU face if `init` already ran; remembered so `init` re-applies
    /// it to the fresh GPU face. No-throw: a bad blob leaves the existing weight.
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

    /// Inject a broad-coverage SYMBOL fallback face from font bytes (the
    /// byte-injection sibling of the config `symbol_font` path). Applies to the
    /// CPU face and the live GPU face if `init` already ran; remembered so `init`
    /// re-applies it to the fresh GPU face. No-throw: a bad blob leaves the
    /// existing faces untouched.
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

    // ── registered-font (handle) twins ──────────────────────────────────────
    // Per-pane engine builds inject the SAME OS faces; the byte-based setters
    // above re-marshal each blob across the JS/wasm boundary per call AND
    // re-intern via a `to_vec` copy. These twins take a `register_font` handle
    // and store the SHARED Arc directly, so panes 2..N copy nothing.

    /// [`AtermGpuTerminal::new`] from a registered PRIMARY font handle.
    #[allow(clippy::too_many_arguments)]
    pub fn new_registered(
        rows: u16,
        cols: u16,
        font_handle: u32,
        px: f32,
        fg: u32,
        bg: u32,
        cursor: u32,
        selection: u32,
    ) -> Result<AtermGpuTerminal, String> {
        let bytes = registered_font(font_handle)?;
        Self::new(rows, cols, &bytes, px, fg, bg, cursor, selection)
    }

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

    /// [`AtermGpuTerminal::set_emoji_font`] from a registered handle. Installs
    /// the SHARED interned copy on the CPU face (no `to_vec` of the ~190MB emoji
    /// face per pane); a LIVE GPU face still receives its own copy (rare — the
    /// worker seeds fonts before `init`, so `gpu` is None during pane builds).
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

    /// Swap the PRIMARY face (the host's `terminalFontFamily`) from font bytes and
    /// re-rasterize, on the CPU face and the live GPU face. The injected bytes
    /// REPLACE `font_bytes` so a later `init` builds the GPU face from the new
    /// family directly. The host re-reads cell metrics + resizes the grid after.
    /// No-throw: a bad blob leaves the existing face untouched.
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

    /// Scale the cell BOX height (the host's `terminalLineHeight`) WITHOUT changing
    /// the glyph px, on the CPU face and the live GPU face. Remembered so `init`
    /// re-applies it. The host re-reads cell_height + resizes the grid after.
    pub fn set_line_height(&mut self, scale: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_line_height(scale);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_line_height(scale);
            gpu.win.invalidate_present();
        }
        self.line_height = scale;
    }

    /// Programming LIGATURES on/off (`=>`, `!=`, `===` …). Mirrors the native
    /// `ligatures` config knob. Applies to the CPU face and the live GPU face if `init`
    /// ran; the choice is remembered so `init` re-applies it to the fresh GPU face.
    /// Preserves any configured `font_features`.
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

    /// OpenType FONT FEATURES for the primary face, as a space-separated spec
    /// (`"+ss01 zero -calt"`). Mirrors the native `font_features` config knob. An
    /// empty/blank spec clears all features. Applies to the CPU face and the live GPU
    /// face; remembered so `init` re-applies it. Preserves the current ligature mode.
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

    /// Set an ANSI/indexed palette colour (index 0–255; 0–15 are the 16 ANSI
    /// colours) to RGB components, so SGR-indexed cell colours resolve through the
    /// host's theme palette instead of the engine's built-in VGA defaults. The
    /// palette lives on the shared grid (`self.term`), so this applies to both the
    /// GPU and CPU-fallback draw paths. Per-cell truecolor SGR flows independently.
    pub fn set_palette_color(&mut self, index: u8, r: u8, g: u8, b: u8) {
        self.note_host_visual_change(); // WF-1 gate
        self.term.set_palette_color_components(index, r, g, b);
    }

    /// Authorize OSC 52 clipboard *write* so the engine queues OSC 52 app-events
    /// for the host to drain (see aterm-wasm). Without it the engine is fail-closed
    /// (CF-004) and drops PTY-origin OSC 52 set sequences. The grid is shared, so
    /// this covers both the GPU and CPU-fallback paths.
    pub fn authorize_clipboard_write(&mut self) {
        self.term.authorize_clipboard_access(ClipboardAccess::Write);
    }

    /// Revoke OSC 52 clipboard *write* authorization (user toggled the setting
    /// off), returning the engine to its fail-closed default.
    pub fn revoke_clipboard_write(&mut self) {
        self.term.revoke_clipboard_access(ClipboardAccess::Write);
    }

    /// Mint an EXTRA OSC 8 URI scheme onto the engine's safe allowlist (orca
    /// deep-links §7; see aterm-wasm — kept in parity). The grid is shared, so
    /// this covers both the GPU and CPU-fallback paths. Returns `false` when
    /// refused (malformed / never-allow / bounded set full), `true` when live.
    pub fn authorize_hyperlink_scheme(&mut self, scheme: &str) -> bool {
        self.term.authorize_hyperlink_scheme(scheme)
    }

    /// Remove a host-minted extra scheme (case-insensitive), restoring the
    /// engine's default allowlist posture for it (parity with aterm-wasm).
    pub fn revoke_hyperlink_scheme(&mut self, scheme: &str) {
        self.term.revoke_hyperlink_scheme(scheme);
    }

    /// Set the cursor blink phase (see aterm-wasm). Applies to the live GPU renderer
    /// AND the CPU face so the GPU present + offscreen readback paths agree.
    ///
    /// WF-1 gate: value-shadowed, so a coarse host blink timer re-asserting the
    /// SAME phase (they do) does not defeat the settled-frame skip. Mirrors the
    /// twin's de-dup exactly.
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
    /// DECSCUSR style when `false`. Applies to both GPU and CPU faces.
    ///
    /// WF-1 gate: value-shadowed like the blink phase — a host that re-asserts
    /// focus state on every window event must not reopen the gate for it.
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

    /// Drain the engine's pending query replies (DA1/DA2/DSR/CPR/DECRQM/OSC color/
    /// window-size, …) so the host can forward them to the PTY — the renderer is the
    /// authoritative responder. Call after each `process`.
    pub fn take_response(&mut self) -> Option<Vec<u8>> {
        self.term.take_response()
    }

    /// Drain pending OSC app-events as a JSON array of `[code, payload]` pairs
    /// (`[[7,"/home"],[52,"copied"]]`); `None` when empty. REAL decoded payloads
    /// (OSC 52 clipboard / OSC 7 cwd / OSC 133 mark) — distinct from PTY replies.
    pub fn take_osc_events(&mut self) -> Option<String> {
        if !self.term.has_osc_events() {
            return None;
        }
        let mut pairs = Vec::new();
        while let Some((code, payload)) = self.term.take_osc_event() {
            pairs.push(format!("[{code},{}]", json_string(&payload)));
        }
        Some(format!("[{}]", pairs.join(",")))
    }

    /// Display-relative cursor column (0-based).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor_x(&self) -> u16 {
        self.term.grid().cursor().col
    }

    /// Display-relative cursor row (0-based, top of viewport).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor_y(&self) -> u16 {
        self.term.grid().cursor().row
    }

    /// The dedicated LIVE application cursor colour (OSC 12) as packed
    /// `0x00RRGGBB`, or `undefined` after OSC 21 `cursor=` selected dynamic
    /// foreground-following behavior (and in a raw unconfigured core). OSC 112
    /// restores the host-configured cursor baseline. Read per frame so
    /// glow/trail colour derivation can follow app-driven cursor changes.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor_color(&self) -> Option<u32> {
        self.term
            .cursor_color()
            .map(|c| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b))
    }

    /// Absolute row index of the live/last line (xterm `buffer.active.baseY`):
    /// `oldest_absolute_row() + scrollback_lines()`. `usize` → plain JS number.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn base_y(&self) -> usize {
        self.term.grid().base_y()
    }

    /// Absolute row index of the TOP visible line for the current viewport
    /// (`base_y - display_offset`); the search/link origin.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn display_origin_absolute(&self) -> usize {
        self.term.grid().display_origin_absolute()
    }

    /// Soft-wrap flag for a visible `row`: `true` if it continues the previous
    /// row (autowrap), `None` when out of range.
    pub fn row_is_wrapped(&self, row: u16) -> Option<bool> {
        self.term.grid().row_is_wrapped(row)
    }

    /// Logical length of a visible `row` (last non-empty cell + 1, 0 if blank);
    /// `None` when out of range.
    pub fn row_len(&self, row: u16) -> Option<u16> {
        self.term.grid().row_len(row)
    }

    /// Grapheme text at DISPLAY cell `row`/`col` (display_offset-aware, like
    /// `row_text`) — base char plus complex cluster and combining marks. Empty
    /// string for a blank cell, a wide-continuation spacer, or out-of-range
    /// coords. Hosts rebuild scrolled-back rows per-cell from this, so it must
    /// track the scroll position; the live-frame reader is `get_line_text`.
    pub fn cell_text(&self, row: u16, col: u16) -> String {
        self.with_display_row_cell(row, col, |(text, _)| text.clone())
            .unwrap_or_default()
    }

    /// Whether the DISPLAY cell at `row`/`col` is a wide (double-width)
    /// character; `None` when out of range. Resolved through the same
    /// display-offset-aware row view as `cell_text` so a host's per-cell walk
    /// sees one coherent row.
    pub fn cell_is_wide(&self, row: u16, col: u16) -> Option<bool> {
        self.with_display_row_cell(row, col, |(_, wide)| *wide)
    }

    /// Drain the edge-triggered BEL flag: `true` if a BEL fired since the last
    /// call, then clears it (poll-based flash/ring without the bell callback).
    pub fn drain_bell(&mut self) -> bool {
        self.term.drain_bell()
    }

    /// Drain the missing-font CLASS bits (1 = text/mono fallback, 2 = colour
    /// emoji) accumulated by renders since the last call — see the aterm-wasm
    /// twin. Misses accumulate on the PRE-init metrics face before `init` and
    /// inside the GPU renderer's wrapped CPU face after it, so both are drained
    /// and OR-ed.
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

    /// Tell the engine the real device-pixel cell size so CSI 14t/16t reports are
    /// accurate (the engine has no canvas otherwise).
    pub fn set_cell_pixel_size(&mut self, width: u16, height: u16) {
        self.term.set_cell_pixel_size(width, height);
    }

    /// Set the engine's scrollback line limit (history lines retained behind the live
    /// viewport). `lines == 0` means unlimited (bounded only by the byte budgets). The
    /// limit is ONE TOTAL retention count (audit E1) across the hot ring, staged
    /// lines, and the tiered store together — the store takes the remainder after the
    /// ring's fixed share, so "retain N lines" retains N, not N + ring. Targets the
    /// primary-content grid — reaching the saved primary through an alt screen; the
    /// alt buffer keeps its spec'd zero scrollback — and re-clamps the scroll
    /// position. Without this the engine keeps its construction default
    /// (`DEFAULT_LINE_LIMIT`, 100k total).
    pub fn set_scrollback_limit(&mut self, lines: u32) {
        self.note_host_visual_change(); // WF-1 gate
        let limit = if lines == 0 {
            None
        } else {
            Some(lines as usize)
        };
        self.term.set_scrollback_line_limit(limit);
    }

    /// Replace the default fg/bg/cursor/selection theme live (0x00RRGGBB) on both the
    /// GPU renderer and the CPU face, so a host theme change re-themes the pane
    /// without a device/face rebuild.
    pub fn set_theme(&mut self, fg: u32, bg: u32, cursor: u32, selection: u32) {
        self.note_host_visual_change(); // WF-1 gate
                                        // Keep the effects' derive-from-theme default in sync (glow/trail colours
                                        // passed as `None` follow the cursor colour, like the native app).
        self.theme_cursor = cursor & 0x00FF_FFFF;
        self.theme_fg = fg & 0x00FF_FFFF;
        self.theme_bg = bg & 0x00FF_FFFF;
        self.effects
            .set_matrix_rain_theme(self.theme_bg, self.theme_fg);
        let theme = Theme {
            fg,
            bg,
            cursor,
            selection,
        };
        apply_terminal_theme_colors(&mut self.term, fg, bg, cursor, selection);
        self.theme = theme;
        self.cpu.set_theme(theme);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_theme(theme);
            // Force the next present to repaint everything: the selection band,
            // idle cursor, and padding border are theme-derived but not content,
            // so the dirty-row diff alone would leave them in the OLD theme.
            gpu.win.invalidate_present();
        }
    }

    /// Explicit selected-text foreground (theme `selectionForeground`), 0x00RRGGBB,
    /// or `undefined` for the WCAG contrast-floor default. Set on both the CPU
    /// fallback face and the live GPU renderer; forces a full present (appearance).
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

    /// Set the per-cell minimum contrast ratio (xterm's `minimumContrastRatio`,
    /// 1..=21; `ratio <= 1.0` = off, the default — xterm treats 1 as "do
    /// nothing"): every glyph fg is floored against its OWN cell bg. Cells whose
    /// fg == bg are never adjusted (SGR 8 conceal renders fg = bg and must stay
    /// hidden). Set on both the CPU fallback face and the live GPU renderer;
    /// forces a full present (appearance-only, not content).
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_minimum_contrast(ratio);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_minimum_contrast(ratio);
            gpu.win.invalidate_present();
        }
    }

    /// Set the DEFAULT-background opacity (0..=1; Ghostty's
    /// `background-opacity`; `1.0` = opaque, the default — byte-identical
    /// output). Only pixels whose bg resolved to the frame's DEFAULT
    /// background go translucent; SGR-colored bg cells, the selection band and
    /// glyph pixels stay opaque. Set on both the CPU fallback face and the
    /// live GPU renderer; forces a full present (appearance-only, not
    /// content). NOTE: the on-glass effect additionally needs a canvas/surface
    /// that composites alpha; the offscreen readback (`render_offscreen` +
    /// `rgba`) carries the alpha either way.
    pub fn set_background_opacity(&mut self, opacity: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_background_opacity(opacity);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_background_opacity(opacity);
            gpu.win.invalidate_present();
        }
    }

    /// Window-chrome for WINDOW-SPACE effects in an embedder: interior padding
    /// (`pad`, px per edge) plus a top-only rise band (`head`, px) — the
    /// `[head][pad][grid][pad]` frame layout. The swapchain resizes to the
    /// padded frame (the host re-reads the canvas dims and offsets it by
    /// `-pad,-(pad+head)` so the grid stays put) and effect emissions become
    /// window-absolute. Set on both the CPU fallback face and the live GPU
    /// renderer (pad/head parity is gated by aterm-gpu's CPU==GPU suite);
    /// `0/0` (the default) is byte-identical to the exact-fit frame.
    pub fn set_chrome(&mut self, pad: u16, head: u16) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_pad(pad as usize);
        self.cpu.set_head(head as usize);
        self.effects.set_chrome(pad, head);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_pad(pad as usize);
            gpu.renderer.set_head(head as usize);
            let (w, h) = gpu.renderer.frame_size(self.rows, self.cols);
            gpu.renderer
                .resize_surface(&mut gpu.win, &mut gpu.surface, w as u32, h as u32);
            gpu.win.invalidate_present();
        }
    }

    /// The chrome interior padding set via [`Self::set_chrome`] (px; 0 = exact
    /// fit). Read from the CPU face — set_chrome keeps it and the live GPU
    /// renderer in lockstep.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn chrome_pad(&self) -> u16 {
        self.cpu.pad() as u16
    }

    /// The chrome top head band set via [`Self::set_chrome`] (px; 0 = none).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn chrome_head(&self) -> u16 {
        self.cpu.head() as u16
    }

    // ── SPILL BAND — the cross-pane window-space effects export ─────────────
    // The aterm-wasm surface, verbatim (see there for the full host contract):
    // spill is CPU integer math over the SAME emission streams both engines
    // feed their renderers, so the exported bytes are engine-independent by
    // construction — the CPU==GPU parity story needs no readback here.

    /// Monotone revision of the spill-band content: advances ONLY when the
    /// exported bytes changed. Typing-only frames with a settled (or
    /// grid-interior) glow, idle re-renders, and 0/0 chrome keep it still —
    /// an unchanged value is the engine's word that the host may skip its
    /// blit without reading a single spill byte.
    pub fn spill_rev(&self) -> u32 {
        self.spill.rev()
    }

    /// Number of dirty rects from the LAST `render`/`render_offscreen` (0 on
    /// a no-change frame). Read with [`spill_rects_ptr`](Self::spill_rects_ptr).
    pub fn spill_rect_count(&self) -> u32 {
        (self.spill.rects().len() / 4) as u32
    }

    /// Byte offset (in wasm linear memory) of the packed dirty-rect array:
    /// `spill_rect_count()` rects of 4 `i32`s — `x, y, w, h`, FRAME-ABSOLUTE
    /// device px. Consume synchronously after a render; never cache the view.
    pub fn spill_rects_ptr(&self) -> usize {
        self.spill.rects().as_ptr() as usize
    }

    /// Byte offset (in wasm linear memory) of the straight-alpha RGBA spill
    /// buffer: four packed row-major strips — **top** `(0, 0, frameW,
    /// pad+head)`, **bottom** `(0, frameH−pad, frameW, pad)`, **left** `(0,
    /// pad+head, pad, gridH)`, **right** `(frameW−pad, pad+head, pad, gridH)`
    /// with `gridH = frameH − 2·pad − head` (frame dims per `frame_size`, the
    /// swapchain size). The pointer is STABLE across frames (the buffer
    /// re-rasters in place); it moves only when chrome or the grid size
    /// changes — wasm memory GROWTH still detaches JS views (rebuild per
    /// read, the `aterm-wasm` `rgba_ptr` rule).
    pub fn spill_ptr(&self) -> usize {
        self.spill.rgba().as_ptr() as usize
    }

    /// Byte length of the spill buffer (`0` at 0/0 chrome — the identity law:
    /// no band, no bytes, no per-frame cost).
    pub fn spill_len(&self) -> usize {
        self.spill.rgba().len()
    }

    /// Include `HaloMode::Over` VEILS (light-theme smoke/steam) in the spill
    /// band (default `true`, keeping the seam-continuity law universal).
    /// `false` scopes the spill to additive light + fire ink — the policy
    /// escape if veils over neighbouring panes read badly. Applies from the
    /// next render.
    pub fn set_spill_include_veils(&mut self, on: bool) {
        self.note_host_visual_change(); // WF-1 gate
        self.spill.set_include_veils(on);
    }

    /// Set the CURSOR-fill opacity (0..=1; Ghostty's `cursor-opacity`; `1.0` =
    /// opaque fill + block cut-out, the default — byte-identical output).
    /// Below 1.0 the cursor fill blends over the cell so the glyph shows
    /// through. Set on both the CPU fallback face and the live GPU renderer;
    /// forces a full present (appearance-only, not content).
    pub fn set_cursor_opacity(&mut self, opacity: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_cursor_opacity(opacity);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_cursor_opacity(opacity);
            gpu.win.invalidate_present();
        }
    }

    /// Mark the pane unfocused (`true`) / focused (`false`): when unfocused, the
    /// selection band paints with the dimmer inactive bg (xterm
    /// `selectionInactiveBackground`). Set on both the CPU fallback face and the
    /// live GPU renderer; forces a full present (appearance-only, not content).
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

    /// Re-rasterize at a new cell font px (host DPI / devicePixelRatio change) on
    /// both the CPU fallback face and the live GPU renderer (which also drops its
    /// atlas). The host re-reads cell_width/cell_height + resizes the grid after.
    pub fn set_px(&mut self, px: f32) {
        self.note_host_visual_change(); // WF-1 gate
        self.cpu.set_px(px);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_px(px);
            gpu.win.invalidate_present();
        }
    }

    /// Resize the grid AND, if the GPU is live, the swapchain to match the new
    /// pixel extent (host recomputes cols/rows for the canvas first).
    ///
    /// The visible grid, the bounded ring and the swapchain resize
    /// SYNCHRONOUSLY. A width change with a deep tiered history does NOT
    /// rewrap that history here: it is detached in O(1)
    /// (`resize_offloading_scrollback`) and rewrapped in LATER, budget-bounded
    /// host tasks — see [`Self::pump_reflow`]; small histories
    /// (≤ `INLINE_REFLOW_MAX_LINES`) rewrap inline. Mirrors aterm-wasm's
    /// `resize` (the cooperative wasm L0-freeze fix — see that crate for the
    /// full design notes).
    pub fn resize(&mut self, rows: u16, cols: u16) {
        // WF-1 gate: a resize reconfigures the SWAPCHAIN below, and a fresh
        // swapchain's contents are undefined — so the canvas can no longer be
        // assumed to hold the frame `last_frame_key` describes. A grid-changing
        // resize also marks damage (which would reopen the gate on its own),
        // but a same-dims call that still reconfigures the surface would not:
        // bump unconditionally rather than reason about which is which.
        self.note_host_visual_change();
        if let Some(pending) = self.term.resize_offloading_scrollback(rows, cols) {
            if pending.line_count() <= INLINE_REFLOW_MAX_LINES {
                // Small-history fast path: rewrap now (bounded by the inline cap).
                // A follow-up job (RFL-3 width convergence) cannot arise here —
                // nothing can change the width mid-call on this single-threaded
                // path — but dropping one would wedge the detach window, so
                // route it to the pump, belt and suspenders.
                if let Some(follow) = self.term.finish_resize_offload(pending.reflow()) {
                    self.pending_reflow = Some(follow);
                    self.reflow_grace = REFLOW_PUMP_GRACE_RENDERS;
                }
            } else {
                // Stash for a later host turn. Overwriting an already-stashed
                // job is only reachable after a reset/erase re-created the
                // store during the old detach window; the clear_gen guard
                // would discard the old job's content on re-attach anyway, so
                // newest-wins loses nothing durable (see aterm-wasm's twin).
                self.pending_reflow = Some(pending);
                self.reflow_grace = REFLOW_PUMP_GRACE_RENDERS;
            }
        }
        // Re-sync to the CLAMPED grid dims, not the raw args: the resize clamps to
        // 1..=4096, and `frame_size` below (plus `render`/`render_offscreen`) sizes the
        // GPU surface from these fields — storing the raw u16 would drive the swapchain
        // past the grid the engine actually holds (oversized texture / huge alloc).
        self.rows = self.term.grid().rows() as usize;
        self.cols = self.term.grid().cols() as usize;
        // The prediction coordinate space just changed: drop in-flight guesses
        // rather than ghost-paint them at stale coords (the native resize rule).
        self.predict.reset();
        if let Some(gpu) = self.gpu.as_mut() {
            let (w, h) = gpu.renderer.frame_size(self.rows, self.cols);
            gpu.renderer
                .resize_surface(&mut gpu.win, &mut gpu.surface, w as u32, h as u32);
        }
    }

    /// Advance a deferred width-change scrollback rewrap (stashed by
    /// [`Self::resize`]) by ONE BOUNDED step — at most the configured budget
    /// of history lines ([`Self::pump_reflow_budget`], default
    /// `REFLOW_STEP_BUDGET_LINES`) — re-attaching the rewrapped history when
    /// the step completes the job. Returns `true` while work REMAINS (the
    /// host should schedule another pump — a `setTimeout(0)` chain or
    /// `requestIdleCallback`); `false` once nothing is pending (the job just
    /// completed and re-attached — re-attach marks full damage, so the next
    /// render repaints — or there was nothing to do).
    ///
    /// COST: O(budget × cols) per call (`PendingScrollbackReflow::reflow_step`;
    /// a logical line is never split, so a soft-wrapped run longer than the
    /// budget is rewrapped whole by the step that completes it). Any pump
    /// schedule yields history content IDENTICAL to a one-shot rewrap —
    /// aterm-grid's `reflow_step_any_schedule_matches_one_shot` property.
    ///
    /// NEVER-PUMPED SAFETY: hosts that never call this still complete —
    /// `render`/`render_offscreen` pump one step per frame after
    /// `REFLOW_PUMP_GRACE_RENDERS` frames, `process` pumps one step per call
    /// past `REFLOW_BACKLOG_MAX_LINES` of staged backlog, and teardown drops
    /// the job with the engine. The store can never stay detached while the
    /// module keeps operating unboundedly.
    pub fn pump_reflow(&mut self) -> bool {
        let Some(pending) = self.pending_reflow.take() else {
            return false;
        };
        self.reflow_grace = 0;
        match pending.reflow_step(self.reflow_budget) {
            ReflowStep::InProgress(job) => {
                self.pending_reflow = Some(job);
                true
            }
            ReflowStep::Done(reflowed) => {
                // CONVERGENCE (RFL-3): a width change that landed while this
                // job was stepping means the re-attach hands back a
                // re-detached job at the settled width — keep pumping it; the
                // `true` already means "schedule another pump".
                match self.term.finish_resize_offload(reflowed) {
                    Some(follow) => {
                        self.pending_reflow = Some(follow);
                        true
                    }
                    None => false,
                }
            }
        }
    }

    /// Tune the per-pump rewrap budget (INPUT history lines per
    /// [`Self::pump_reflow`] step). `0` restores the default
    /// (`REFLOW_STEP_BUDGET_LINES`, 2_000 ≈ ~3ms native — see the constant's
    /// sizing note). Hosts with generous idle windows can raise it to finish
    /// deep histories in fewer tasks; latency-sensitive hosts can lower it.
    pub fn pump_reflow_budget(&mut self, max_lines: u32) {
        self.reflow_budget = if max_lines == 0 {
            REFLOW_STEP_BUDGET_LINES
        } else {
            max_lines as usize
        };
    }

    /// True while a deferred scrollback rewrap is stashed (deep history is
    /// temporarily detached: only the ring is visible/searchable; a partly
    /// stepped job holds its progress here between pumps). The host should
    /// keep scheduling [`Self::pump_reflow`] while this is set.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn reflow_pending(&self) -> bool {
        self.pending_reflow.is_some()
    }

    /// `true` when the LAST `render` call was elided by the WF-1 frame gate:
    /// nothing observable had changed, so the canvas retained the previous
    /// frame's composited image and no swapchain work ran. The gate's reach
    /// witness for tests and benches; hosts should prefer
    /// [`needs_frame`](Self::needs_frame), which answers BEFORE the call.
    pub fn last_render_skipped(&self) -> bool {
        self.last_render_gated
    }

    /// Whether the next `render` would actually draw — the
    /// exported form of the WF-1 frame gate, so a JS host can skip the wasm
    /// call (and its own rAF work) entirely on a settled pane.
    ///
    /// `&mut self` because reading `Terminal::damage_epoch` latches the current
    /// damage session (idempotent; the same read `render` performs). Advisory
    /// in one direction only: `true` may prove spurious (the render may still
    /// gate) but `false` is authoritative — a `false` here and the following
    /// `render()` is guaranteed to skip, because every term below is exactly
    /// the gate's own.
    ///
    /// HOST HAZARD, worth stating where the temptation lives: a host that stops
    /// calling `render` on settled panes also stops running whatever liveness
    /// check it does inside its draw function. Chromium silently evicts the
    /// oldest WebGL context past its live-context budget WITHOUT firing
    /// `webglcontextlost`, and GL calls on an evicted context are silent
    /// no-ops, so a host that polls `isContextLost()` from inside `drawFrame`
    /// must move that poll OUT of the draw path before idling on this — see
    /// docs/matrix-rain-design.md. The in-`render` gate is unaffected either
    /// way; only the host-skip pattern needs the separate poll.
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

    /// Safety net #1 (see `REFLOW_PUMP_GRACE_RENDERS`): called by the wasm-only
    /// `render`/`render_offscreen` paths (hence unused on the native
    /// compile-verification target, where tests drive it directly). After the
    /// grace window, ONE budgeted step per frame — never the whole job in a
    /// single frame (that was the point of the stepping seam).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
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

    /// Safety net #2 (see `REFLOW_BACKLOG_MAX_LINES`): called by `process`
    /// after feeding — ONE budgeted step per call while the staged window
    /// output is past the cap, so a stream-while-detached session converges
    /// without any single unbounded catch-up task.
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

    // ---------------------------------------------------------------------
    // Engine-state surface — passthroughs mirroring `the aterm-wasm crate`'s
    // `AtermTerminal`. Why: ONE engine per pane. The host's input handlers
    // (scroll/selection/search/mouse/link/cursor/focus) call these `term.*`
    // methods; exposing the SAME surface here lets the GPU drawer reuse the
    // single engine for both drawing AND state, so bytes are parsed once.
    // ---------------------------------------------------------------------

    /// Scroll the viewport through scrollback: positive `delta` reveals older
    /// lines, negative reveals newer. The host redraws afterwards.
    pub fn scroll_lines(&mut self, delta: i32) {
        // Whole-row navigation lands row-aligned: drop any banked sub-row
        // residual (the scroll_input_api reset-on-snap contract).
        self.scroll_input.reset();
        self.term.scroll_display(delta);
    }

    /// Snap the viewport to the live bottom (latest output).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_input.reset();
        self.term.scroll_to_bottom();
    }

    /// Snap the viewport to the oldest retained scrollback line.
    pub fn scroll_to_top(&mut self) {
        self.scroll_input.reset();
        self.term.scroll_to_top();
    }

    /// Lines the viewport is scrolled up from the live bottom (0 = at bottom).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// True when the alternate screen is active (TUIs own their own scrolling),
    /// so the host should let wheel events pass through to the app.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn is_alt_screen(&self) -> bool {
        self.term.is_alternate_screen()
    }

    /// Serialize the terminal to a REPLAYABLE ANSI string (mirrors the CPU
    /// `AtermTerminal::serialize`) — the aterm-native replacement for xterm's
    /// SerializeAddon. `scrollback_rows`: None = all history, Some(n) = last n,
    /// Some(0) = viewport only. Operates on the shared engine grid.
    pub fn serialize(&self, scrollback_rows: Option<u32>) -> String {
        let grid = self.term.grid();
        let cap = scrollback_rows.map(|n| n as usize);
        let active_history = grid.scrollback_lines();
        let take = cap.map_or(active_history, |n| n.min(active_history));
        let mut out = String::from("\x1b[0m");
        for i in (active_history - take)..active_history {
            let line = grid
                .get_history_line(i)
                .and_then(|l| l.as_str().map(|s| s.trim_end().to_string()))
                .unwrap_or_default();
            out.push_str(&line);
            out.push_str("\r\n");
        }
        out.push_str("\x1b[H");
        for r in 0..self.rows as u16 {
            out.push_str(&format!("\x1b[{};1H\x1b[K", r + 1));
            if let Some(row_ansi) = grid.row_ansi_text_screen(r) {
                out.push_str(&row_ansi);
            }
            out.push_str("\x1b[0m");
        }
        let c = self.term.cursor();
        out.push_str(&format!(
            "\x1b[{};{}H",
            c.row as usize + 1,
            c.col as usize + 1
        ));
        out
    }

    /// Scrollback HISTORY only (main buffer) — mirrors the CPU
    /// `AtermTerminal::serialize_scrollback`.
    pub fn serialize_scrollback(&self, max_rows: Option<u32>) -> String {
        let grid = self.term.main_grid();
        let history = grid.scrollback_lines();
        if history == 0 {
            return String::new();
        }
        let take = max_rows.map_or(history, |n| (n as usize).min(history));
        let mut out = String::new();
        for i in (history - take)..history {
            let line = grid
                .get_history_line(i)
                .and_then(|l| l.as_str().map(|s| s.trim_end().to_string()))
                .unwrap_or_default();
            out.push_str(&line);
            out.push_str("\r\n");
        }
        out
    }

    /// The last completed OSC-133 block's output as JSON, following the
    /// `take_osc_events` JSON-drain convention (CM-A3, "Copy Last Command
    /// Output") — mirrors the CPU binding:
    ///   `{"status":"ok","text":"…","exitCode":0}` — output read in full
    ///     (`exitCode` is `null` when the block was finalized without OSC 133 D,
    ///     e.g. an interrupted command whose next prompt closed it);
    ///   `{"status":"evicted"}` — the block's rows scrolled past the scrollback
    ///     cap (DL-1: an honest marker, never silently-shifted/empty text);
    ///   `undefined` — nothing to copy: no shell integration, no finished block
    ///     yet (incl. a snapshot-rehydrated pane — blocks are excluded from
    ///     checkpoints), or the block never reached its output phase.
    ///
    /// `&self` — the read rides `Terminal::last_completed_block`, which was added
    /// alongside this binding precisely so the facade does not need the `&mut`
    /// `output_blocks()` (`make_contiguous`) path.
    pub fn last_command_output(&self) -> Option<String> {
        let block = self.term.last_completed_block()?;
        match self.term.block_output_text(block) {
            BlockText::Text(text) => {
                let exit = block
                    .exit_code
                    .map_or_else(|| "null".to_owned(), |c| c.to_string());
                Some(format!(
                    "{{\"status\":\"ok\",\"text\":{},\"exitCode\":{exit}}}",
                    json_string(&text)
                ))
            }
            BlockText::Evicted => Some("{\"status\":\"evicted\"}".to_owned()),
            BlockText::NotAvailable => None,
        }
    }

    /// The window title (OSC 0/2), or `None` when unset (mirrors the CPU binding).
    pub fn title(&self) -> Option<String> {
        let title = self.term.title();
        if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        }
    }

    /// Whether bracketed-paste mode (DECSET 2004) is active (mirrors the CPU binding).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn bracketed_paste_mode(&self) -> bool {
        self.term.modes().bracketed_paste()
    }

    /// True when DEC private mode 1007 (alternate scroll) is set: while the
    /// alternate screen is active and mouse tracking is off, the host converts
    /// wheel ticks into arrow-key presses (aterm-gui's WheelPlan behaviour) so
    /// TUIs without mouse support still wheel-scroll. Mirrors aterm-wasm.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn is_alternate_scroll(&self) -> bool {
        self.term.modes().alternate_scroll()
    }

    /// True when DECCKM (application cursor keys) is set: the host encodes
    /// arrows/Home/End as SS3 instead of CSI for full-screen apps.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn is_app_cursor_mode(&self) -> bool {
        self.term.modes().application_cursor_keys()
    }

    /// True when a TUI has enabled mouse tracking (DECSET 9/1000/1002/1003).
    /// The host then ENCODES canvas mouse events to the PTY instead of running
    /// selection/scroll/link for them (unless Shift = user override).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn is_mouse_tracking(&self) -> bool {
        self.term.mouse_tracking_enabled()
    }

    /// True when the active mouse mode reports MOTION (1002 drag, 1003 any).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn mouse_wants_motion(&self) -> bool {
        matches!(
            self.term.mouse_mode(),
            MouseMode::ButtonEvent | MouseMode::AnyEvent
        )
    }

    /// True for AnyEvent (1003): report motion even with NO button pressed.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn mouse_wants_any_motion(&self) -> bool {
        matches!(self.term.mouse_mode(), MouseMode::AnyEvent)
    }

    /// True when DECSET 1004 (focus reporting) is active: the host sends CSI I
    /// on focus-in and CSI O on focus-out so apps track terminal focus.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn is_focus_event_mode(&self) -> bool {
        self.term.focus_reporting_enabled()
    }

    /// True when DEC mode 2031 (color-scheme update notifications) is set: the
    /// app wants `CSI ? 997 ; n` on OS light/dark theme changes.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn is_color_scheme_updates_mode(&self) -> bool {
        self.term.report_color_scheme_enabled()
    }

    /// Active DECSCUSR cursor style as the discriminant of `aterm_core`'s
    /// `CursorStyle`. The GPU renderer paints the shape from the grid; this
    /// getter exists for host introspection/tests, mirroring aterm-wasm.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor_style(&self) -> u8 {
        self.term.cursor_style() as u8
    }

    /// Set the host-preferred DEFAULT cursor style (shape used before any DECSCUSR and
    /// restored after RIS/DECSTR). `n` per DECSCUSR: 1=blinking block, 2=steady block,
    /// 3=blinking underline, 4=steady underline, 5=blinking bar, 6=steady bar;
    /// out-of-range ignored. Does NOT clobber an app's live DECSCUSR. Mirrors aterm-wasm.
    pub fn set_default_cursor_style(&mut self, n: u8) {
        self.note_host_visual_change(); // WF-1 gate
        if let Some(style) = CursorStyle::from_param(u16::from(n)) {
            self.term.set_default_cursor_style(style);
        }
    }

    /// Push the host OS color scheme into the engine. `dark = true` selects a dark
    /// appearance, `false` light. When the scheme CHANGES and the app enabled DEC mode
    /// 2031, the engine queues an unsolicited `CSI ? 997 ; Ps n`; drain it via
    /// `take_response` and forward to the PTY. A no-op when unchanged. Mirrors aterm-wasm.
    pub fn set_color_scheme(&mut self, dark: bool) {
        self.note_host_visual_change(); // WF-1 gate
        let scheme = if dark {
            aterm_types::Appearance::Dark
        } else {
            aterm_types::Appearance::Light
        };
        self.term.set_color_scheme(scheme);
    }

    /// Encode a mouse-button PRESS at 0-based cell `col`/`row` for the active
    /// mouse mode+encoding (`None` when tracking is off). See aterm-wasm.
    pub fn encode_mouse_press(&self, col: u16, row: u16, button: u8, mods: u8) -> Option<Vec<u8>> {
        self.term.encode_mouse_press(button, col, row, mods)
    }

    /// Encode a mouse-button RELEASE; `None` in X10 press-only mode.
    pub fn encode_mouse_release(
        &self,
        col: u16,
        row: u16,
        button: u8,
        mods: u8,
    ) -> Option<Vec<u8>> {
        self.term.encode_mouse_release(button, col, row, mods)
    }

    /// Encode mouse MOTION at `col`/`row`; `button` is the held button (3=none).
    pub fn encode_mouse_motion(&self, col: u16, row: u16, button: u8, mods: u8) -> Option<Vec<u8>> {
        self.term.encode_mouse_motion(button, col, row, mods)
    }

    /// Encode a mouse WHEEL tick at `col`/`row` (`up` = wheel-up); `None` in X10.
    ///
    /// Vertical-only by design — see the twin in `aterm-wasm` for why the web
    /// bindings did not follow the engine's 4-way widening.
    pub fn encode_mouse_wheel(&self, col: u16, row: u16, up: bool, mods: u8) -> Option<Vec<u8>> {
        let dir = if up {
            aterm_types::mouse::WheelDir::Up
        } else {
            aterm_types::mouse::WheelDir::Down
        };
        self.term.encode_mouse_wheel(dir, col, row, mods)
    }

    /// Encode a keyboard event through the engine's FULL encoder (legacy +
    /// xterm modifyOtherKeys + Kitty), driven by the LIVE
    /// `Terminal::keyboard_mode()`. `key` is a DOM `KeyboardEvent.key` value;
    /// `mods` is the engine `Modifiers` bitfield (SHIFT=1, ALT=2, CTRL=4,
    /// SUPER=8); `event_type` is 0=Press, 1=Repeat, 2=Release;
    /// `base_layout_key` is the physical key's US-QWERTY char for Kitty
    /// `REPORT_ALTERNATE_KEYS`. `None` when the event encodes to nothing or
    /// the key has no terminal encoding. Mirrors aterm-wasm.
    pub fn encode_key(
        &self,
        key: &str,
        mods: u8,
        event_type: u8,
        base_layout_key: Option<char>,
    ) -> Option<Vec<u8>> {
        aterm_types::keyboard::encode_dom_key(
            key,
            mods,
            event_type,
            base_layout_key,
            self.term.keyboard_mode(),
        )
    }

    /// Enable/disable the Kitty keyboard protocol capability (default ON). When
    /// disabled the engine acts as if the protocol is unsupported — no `CSI ? u`
    /// reply, push/set/pop consumed-and-ignored, `keyboard_mode` never carries
    /// kitty bits — for hosts whose platform consumes kitty sequences itself
    /// (Windows ConPTY; xterm.js `vtExtensions.kittyKeyboard = false`). The
    /// engine (`term`) survives `init`, so no pre-init retention is needed.
    /// Mirrors aterm-wasm.
    pub fn set_kitty_keyboard_enabled(&mut self, enabled: bool) {
        self.term.set_kitty_keyboard_enabled(enabled);
    }

    /// The live `Terminal::keyboard_mode()` as its raw bitflags value, for
    /// hosts that run the engine in a Web Worker: mirror these bits into the
    /// main-thread engine-state snapshot and feed them to the free
    /// [`encode_key_with_mode`], which encodes keydowns synchronously without
    /// an instance. `KeyboardMode` is a `bitflags` struct over `u16` (bits
    /// 0..=14 defined); the value is zero-extended to `u32` for headroom.
    /// Mirrors aterm-wasm.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn keyboard_mode_bits(&self) -> u32 {
        u32::from(self.term.keyboard_mode().bits())
    }

    /// Convert a display-relative row (0 = top of viewport) to the
    /// TERMINAL-relative row the selection model stores: `display_row -
    /// display_offset`, negative for scrollback. The renderer and
    /// `selection_to_string` both read terminal-relative rows, so converting
    /// here keeps the highlight and copied text correct while scrolled (#sel-fix).
    fn display_row_to_terminal(&self, display_row: i32) -> i32 {
        display_row - self.term.grid().display_offset() as i32
    }

    /// Begin a character selection at display `row`/`col` (clears any prior one).
    pub fn selection_start(&mut self, row: i32, col: u16) {
        self.note_host_visual_change(); // WF-1 gate
        let row = self.display_row_to_terminal(row);
        self.term.text_selection_mut().start_selection(
            row,
            col,
            SelectionSide::Left,
            SelectionType::Simple,
        );
    }

    /// Select the whole word/URL at display `row`/`col` (double-click) and return
    /// its text. Mirrors aterm-gui's select_word: a Semantic selection EXPANDED to
    /// the word's inclusive cell span (smart_word_at's end col is exclusive); on
    /// whitespace it falls back to the clicked cell. The selection stays active so
    /// the highlight paints.
    pub fn selection_word(&mut self, row: i32, col: u16) -> Option<String> {
        self.note_host_visual_change(); // WF-1 gate
                                        // smart_word_at is display-offset-aware (takes the DISPLAY row); the
                                        // selection anchor must be terminal-relative.
        let (start, last) = match self
            .term
            .smart_word_at(row as usize, col as usize, &self.smart)
        {
            Some((s, e)) => (s as u16, e.saturating_sub(1).max(s) as u16),
            None => (col, col),
        };
        let term_row = self.display_row_to_terminal(row);
        let sel = self.term.text_selection_mut();
        sel.start_selection(term_row, col, SelectionSide::Left, SelectionType::Semantic);
        sel.expand_semantic(start, last);
        sel.complete_selection();
        self.term.selection_to_string()
    }

    /// Select the whole line at display `row` (triple-click) and return its text.
    /// Mirrors aterm-gui's select_line: a Lines selection expanded to the full row
    /// width. `col` is accepted for a uniform host API but unused (whole row).
    pub fn selection_line(&mut self, row: i32, col: u16) -> Option<String> {
        self.note_host_visual_change(); // WF-1 gate
        let _ = col;
        let row = self.display_row_to_terminal(row);
        let max_col = (self.cols as u16).saturating_sub(1);
        let sel = self.term.text_selection_mut();
        sel.start_selection(row, 0, SelectionSide::Left, SelectionType::Lines);
        sel.expand_lines(max_col);
        sel.complete_selection();
        self.term.selection_to_string()
    }

    /// Move the selection endpoint to `row`/`col` (during a drag).
    ///
    /// WF-1 gate, and the ONE selection verb that is not an unconditional bump:
    /// this is the only mutator on this facade a host invokes at POINTER
    /// cadence. A pointer moving sub-cell re-asserts the SAME cell across
    /// consecutive frames, and each such re-assertion would otherwise reopen
    /// the gate for a frame that draws nothing new. So bump only when the
    /// RESOLVED selection actually differs.
    ///
    /// The comparison is over the engine's own `TextSelection` value taken
    /// around the update, NOT the caller's `(row, col)` arguments — see the
    /// aterm-wasm twin for the three cases an argument shadow gets wrong
    /// (display-row conversion under auto-scroll, a stale shadow surviving into
    /// the next drag, and the engine moving anchors on content scroll).
    pub fn selection_extend(&mut self, row: i32, col: u16) {
        let row = self.display_row_to_terminal(row);
        let before = self.term.text_selection().clone();
        self.term
            .text_selection_mut()
            .update_selection(row, col, SelectionSide::Right);
        if *self.term.text_selection() != before {
            self.note_host_visual_change();
        }
    }

    /// Finalize the selection (mouse released).
    pub fn selection_finish(&mut self) {
        self.note_host_visual_change(); // WF-1 gate
        self.term.text_selection_mut().complete_selection();
    }

    /// Drop the current selection so the highlight clears on the next render.
    pub fn selection_clear(&mut self) {
        self.note_host_visual_change(); // WF-1 gate
        self.term.text_selection_mut().clear();
    }

    /// Override the characters that BREAK a double-click word (the host's
    /// word-separator setting, xterm.js `wordSeparators` semantics): a word
    /// becomes a maximal run of NON-separator characters. `undefined` restores
    /// the engine's default class-based word logic (alphanumeric + `_`)
    /// exactly. Smart-selection RULES (url/file_path/email/…) still take
    /// precedence for both `selection_word` and `link_at`; the separators only
    /// shape the plain-word fallback. Mirrors aterm-wasm.
    pub fn set_word_separators(&mut self, separators: Option<String>) {
        self.smart.set_word_separators(separators.as_deref());
    }

    /// The selected text, if any (`None` when the selection is empty).
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    /// Current selection bounds in DISPLAY viewport cell coords (0 = top visible
    /// row), side-adjusted to match `selection_text` and the painted highlight.
    /// `None` when there is no selection OR it lies fully outside the viewport.
    pub fn selection_range(&self) -> Option<SelectionRange> {
        selection_range_for(&self.term, self.rows, self.cols)
    }

    /// Detect a link under display `row`/`col`. Prefers an OSC-8 hyperlink, then
    /// falls back to smart-selection rules (url/file_path). `None` for plain
    /// words. `kind`: 0=osc8, 1=url, 2=file_path, 3=other. See aterm-wasm.
    pub fn link_at(&self, row: u16, col: u16) -> Option<LinkHit> {
        // OSC-8 lookups are NOT display_offset-aware, so only consult
        // hyperlink_at when the viewport isn't scrolled.
        if self.term.grid().display_offset() == 0 {
            if let Some(url) = self.term.hyperlink_at(row, col) {
                let url = url.to_string();
                let (s, e) = self.osc8_span(row, col);
                return Some(LinkHit {
                    url,
                    start_col: s,
                    end_col: e,
                    kind: 0,
                });
            }
        }

        // Smart-selection fallback is scroll-correct on any scrollback row.
        let (sc, ec) = self
            .term
            .smart_word_at(row as usize, col as usize, &self.smart)?;
        let text = self.term.display_row_text(row as usize)?;
        let matched = slice_by_columns(&text, sc, ec);
        let kind = classify(&matched);
        if kind == 3 {
            return None;
        }
        Some(LinkHit {
            url: matched,
            start_col: sc as u16,
            end_col: ec as u16,
            kind,
        })
    }

    /// Scroll-correct text of a display `row` (display_offset-aware), for a TS
    /// fallback that re-runs link matching in JS.
    pub fn row_text(&self, row: u16) -> Option<String> {
        self.term.display_row_text(row as usize)
    }

    /// Batch row-range export for the P7 grid mirror (E9): the text/wrap/len
    /// (+ per-column wide map) of `count` DISPLAY rows starting at `first_row`
    /// (display_offset-aware, same coords as [`AtermGpuTerminal::row_text`]) in
    /// ONE wasm-boundary crossing, replacing the per-row `row_text` +
    /// `row_is_wrapped` + `row_len` + per-cell `cell_is_wide` walk. Returns a
    /// JSON array of exactly `count` records
    /// `{text, wrapped, len, widths?}` in row order; `widths` is a per-column
    /// digit string (`'2'` wide lead / `'1'` otherwise, length == cols) OMITTED
    /// for all-narrow rows so the host reuses its cached all-`'1'` string.
    /// `undefined` when the range is unavailable (a row is out of the live grid,
    /// e.g. resize skew) — the host falls back to the per-row path that frame.
    pub fn row_range_json(&self, first_row: u32, count: u32) -> Option<String> {
        let rows = u32::try_from(self.rows).unwrap_or(u32::MAX);
        let cols = self.cols;
        let end = first_row.checked_add(count)?;
        // Any row past the live grid ⇒ range unavailable this frame.
        if end > rows {
            return None;
        }
        let mut out = String::from("[");
        for y in first_row..end {
            if y != first_row {
                out.push(',');
            }
            let row_u16 = u16::try_from(y).ok()?;
            // Text matches the per-row `row_text` fallback exactly
            // (display_row_text), so a path switch never spuriously re-dirties.
            let text = self.term.display_row_text(y as usize).unwrap_or_default();
            let wrapped = self.term.grid().row_is_wrapped(row_u16).unwrap_or(false);
            let len = self
                .term
                .grid()
                .row_len(row_u16)
                .map_or(self.cols, usize::from);
            // Per-column wide map from the same source `cell_is_wide` reads; the
            // continuation spacer of a wide cell reports narrow, matching the
            // host's `cell_is_wide(y,x) ? '2' : '1'` per-cell walk.
            //
            // Read straight off ONE row view rather than through
            // `display_row_grapheme_cells`: that accessor builds a
            // `Vec<(String, bool)>` with a heap `String` per column carrying the
            // resolved cluster text — text this loop never touches (the row's
            // text came from `display_row_text` above), so a 200-col mirror
            // refresh allocated and freed up to 200 Strings + a Vec PER ROW
            // purely to read one bit each. `view.cell(col).is_wide()` is exactly
            // the predicate that accessor computes for its `.1`, so the digit
            // string is unchanged; an out-of-grid row yields `Empty`, whose
            // `cell()` is `None` ⇒ the all-`'1'`/omitted shape the old `None`
            // arm produced. It also materializes a scrolled-in history row once
            // per record instead of twice.
            let view = self.term.grid().visible_row_view(row_u16);
            let mut widths = String::with_capacity(cols);
            let mut any_wide = false;
            for col in 0..cols {
                let wide = u16::try_from(col)
                    .ok()
                    .and_then(|c| view.cell(c))
                    .is_some_and(|c| c.is_wide());
                if wide {
                    widths.push('2');
                    any_wide = true;
                } else {
                    widths.push('1');
                }
            }
            out.push_str("{\"text\":");
            out.push_str(&json_string(&text));
            out.push_str(",\"wrapped\":");
            out.push_str(if wrapped { "true" } else { "false" });
            out.push_str(",\"len\":");
            out.push_str(&len.to_string());
            if any_wide {
                out.push_str(",\"widths\":");
                out.push_str(&json_string(&widths));
            }
            out.push('}');
        }
        out.push(']');
        Some(out)
    }

    /// Search the full retained buffer for `query`, returning matches as a flat
    /// `[abs_line, start_col, len]` triplet array. Empty query / regex error →
    /// empty array. `is_regex` compiles `query` as a regex (parity with aterm-wasm;
    /// the core already accepts it — the web GPU path previously hardcoded false).
    /// See aterm-wasm for the coordinate contract.
    pub fn search(&mut self, query: &str, case_sensitive: bool, is_regex: bool) -> Vec<u32> {
        if query.is_empty() {
            return Vec::new();
        }
        let Ok(results) =
            self.term
                .indexed_search()
                .search_results_opts(query, case_sensitive, is_regex)
        else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(results.matches.len() * 3);
        for m in &results.matches {
            out.push(u32::try_from(m.line).unwrap_or(u32::MAX));
            out.push(u32::try_from(m.start_col).unwrap_or(u32::MAX));
            out.push(u32::try_from(m.len()).unwrap_or(u32::MAX));
        }
        out
    }

    /// Metadata for a [`AtermGpuTerminal::search`]-contract query (E9a):
    /// GPU-module parity with aterm-wasm's `search_meta` — carries the
    /// engine's `incomplete` signal the legacy `search` export drops. Same
    /// stateless contract: re-runs `query` on the cached index (one query,
    /// never a rebuild, on unchanged content); empty query or invalid regex
    /// reports `incomplete == false`, `match_count == 0`.
    pub fn search_meta(&mut self, query: &str, case_sensitive: bool, is_regex: bool) -> SearchMeta {
        if query.is_empty() {
            return SearchMeta {
                incomplete: false,
                match_count: 0,
            };
        }
        let Ok(results) =
            self.term
                .indexed_search()
                .search_results_opts(query, case_sensitive, is_regex)
        else {
            return SearchMeta {
                incomplete: false,
                match_count: 0,
            };
        };
        SearchMeta {
            incomplete: results.incomplete,
            match_count: u32::try_from(results.matches.len()).unwrap_or(u32::MAX),
        }
    }

    /// GPU-module parity with the aterm-wasm crate's `search_summary`: a
    /// snippet-carrying superset of [`AtermGpuTerminal::search_meta`]. Returns
    /// `{matches:[{absRow,col,len,snippet}], total, incomplete}` where `matches`
    /// is capped to `max_matches` (0 = uncapped), `total` is the full match
    /// count before the cap, `incomplete` is the engine's eviction/match-cap
    /// truncation signal (which [`AtermGpuTerminal::search`] drops), and
    /// `snippet` is the match line's text (absolute-row coordinate). Empty
    /// query or invalid regex ⇒ `{matches:[],total:0,incomplete:false}`
    /// (mirroring `search`'s silence).
    ///
    /// A bounded READ over an already-built full-content index — not a
    /// from-scratch rebuild on the hot path ([`Terminal::search_summary_results`]):
    /// after a [`AtermGpuTerminal::search_budgeted`] scan completes over the same
    /// query + content snapshot, THAT retained index answers directly (zero
    /// rebuild); otherwise the O(1)-reused one-shot index serves it, rebuilding
    /// only on a content-key miss. Only the `≤max_matches` capped rows pay a
    /// snippet read.
    pub fn search_summary(
        &mut self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        max_matches: u32,
    ) -> Option<String> {
        let empty = || Some("{\"matches\":[],\"total\":0,\"incomplete\":false}".to_owned());
        if query.is_empty() {
            return empty();
        }
        let (matches, total, incomplete) = {
            let Ok(results) = self
                .term
                .search_summary_results(query, case_sensitive, is_regex)
            else {
                return empty();
            };
            let total = results.matches.len();
            let cap = if max_matches == 0 {
                total
            } else {
                (max_matches as usize).min(total)
            };
            // Copy the capped (line, start_col, len) triplets out so the &self
            // index borrow ends before the &self snippet reads below.
            let matches: Vec<(u64, usize, usize)> = results.matches[..cap]
                .iter()
                .map(|m| (m.line as u64, m.start_col, m.len()))
                .collect();
            (matches, total, results.incomplete)
        };
        let mut out = String::from("{\"matches\":[");
        for (i, (abs, col, len)) in matches.iter().enumerate() {
            if i != 0 {
                out.push(',');
            }
            let snippet = self.term.abs_row_text(*abs).unwrap_or_default();
            out.push_str("{\"absRow\":");
            out.push_str(&abs.to_string());
            out.push_str(",\"col\":");
            out.push_str(&col.to_string());
            out.push_str(",\"len\":");
            out.push_str(&len.to_string());
            out.push_str(",\"snippet\":");
            out.push_str(&json_string(&snippet));
            out.push('}');
        }
        out.push_str("],\"total\":");
        out.push_str(&total.to_string());
        out.push_str(",\"incomplete\":");
        out.push_str(if incomplete { "true" } else { "false" });
        out.push('}');
        Some(out)
    }

    /// Absolute row of display row 0 at the live bottom. A match at absolute
    /// `line` is at display row `line - origin + display_offset`.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn search_display_origin(&self) -> u32 {
        let grid = self.term.grid();
        let origin = grid
            .oldest_absolute_row()
            .saturating_add(grid.scrollback_lines() as u64);
        u32::try_from(origin).unwrap_or(u32::MAX)
    }

    /// Scroll the viewport so the match at absolute `line` is visible (top row),
    /// clamped to the retained scrollback. Host redraws after.
    pub fn scroll_search_line_into_view(&mut self, line: u32) {
        let grid = self.term.grid();
        let origin = grid
            .oldest_absolute_row()
            .saturating_add(grid.scrollback_lines() as u64);
        let scrollback = grid.scrollback_lines();
        let current = grid.display_offset();
        let want = origin.saturating_sub(u64::from(line));
        let want = (want as usize).min(scrollback);
        let delta = want as i64 - current as i64;
        if let Ok(delta) = i32::try_from(delta) {
            self.term.scroll_display(delta);
        }
    }

    /// Budgeted, resumable variant of [`AtermGpuTerminal::search`] (P1.1):
    /// GPU-module parity with the aterm-wasm export — see that crate's
    /// `search_budgeted` for the full cursor/staleness/equality contract. Runs
    /// at most `row_budget` rows per call; the returned cursor resumes; a
    /// stale/foreign cursor, a new pattern, or changed content restarts from
    /// scratch. Empty query or invalid regex: an immediate empty `complete`
    /// result (and an empty query drops any in-flight state). A zero row budget
    /// is clamped to one; backlog-drain turns may deliver deltas without
    /// advancing `rows_fed`.
    pub fn search_budgeted(
        &mut self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        resume_cursor: Option<u64>,
        row_budget: u32,
    ) -> BudgetedSearchResult {
        let empty = || BudgetedSearchResult {
            matches: Vec::new(),
            complete: true,
            cursor: None,
            search_id: None,
            reset: true,
            incomplete_index: false,
            lowest_retained_line: 0,
            rows_fed: 0,
            total_rows: 0,
        };
        if query.is_empty() {
            self.term.cancel_budgeted_search();
            return empty();
        }
        let Ok(step) = self.term.search_budgeted(
            query,
            case_sensitive,
            is_regex,
            resume_cursor,
            row_budget as usize,
        ) else {
            return empty();
        };
        let mut matches = Vec::with_capacity(step.results.matches.len() * 3);
        for m in &step.results.matches {
            matches.push(u32::try_from(m.line).unwrap_or(u32::MAX));
            matches.push(u32::try_from(m.start_col).unwrap_or(u32::MAX));
            matches.push(u32::try_from(m.len()).unwrap_or(u32::MAX));
        }
        BudgetedSearchResult {
            matches,
            complete: step.complete,
            cursor: step.cursor,
            search_id: Some(step.search_id),
            reset: step.reset,
            incomplete_index: step.results.incomplete,
            lowest_retained_line: u32::try_from(step.results.lowest_retained_line)
                .unwrap_or(u32::MAX),
            rows_fed: u32::try_from(step.rows_fed).unwrap_or(u32::MAX),
            total_rows: u32::try_from(step.total_rows).unwrap_or(u32::MAX),
        }
    }

    /// Drop any in-flight [`AtermGpuTerminal::search_budgeted`] state (frees
    /// the partial index; outstanding cursors go stale and restart if resumed).
    pub fn search_budgeted_cancel(&mut self) {
        self.term.cancel_budgeted_search();
    }

    /// Release the search index's heap (fed E-1 `search_index_release`): drops
    /// the cached full-content index AND any in-flight budgeted search so a
    /// dormant/closed pane's index footprint returns to the allocator, making
    /// federation idle-eviction REAL rather than a logical clear that retains
    /// peak capacity. The next search rebuilds from the live buffer — one
    /// rebuild paid, byte-identical results.
    pub fn search_index_release(&mut self) {
        self.term.release_search_index();
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

/// STATELESS key encoder for worker-hosted engines: encode a DOM keyboard
/// event against an explicit mode-bits snapshot instead of a live terminal.
///
/// Contract: the engine lives in a Web Worker while keydown handling runs on
/// the main thread, so the host mirrors
/// [`AtermGpuTerminal::keyboard_mode_bits`] through its engine-state snapshot
/// and encodes synchronously here, accepting one-frame staleness — the same
/// tradeoff the host already accepts for DECCKM gating via
/// `is_app_cursor_mode`.
///
/// Parameters match [`AtermGpuTerminal::encode_key`] (`key` = DOM
/// `KeyboardEvent.key`; `mods` = SHIFT=1, ALT=2, CTRL=4, SUPER=8;
/// `event_type` = 0=Press, 1=Repeat, 2=Release; `base_layout_key` = US-QWERTY
/// char for Kitty `REPORT_ALTERNATE_KEYS`), plus `mode_bits` from
/// `keyboard_mode_bits` (a `u16` bitflags value zero-extended to `u32`;
/// undefined bits are truncated away). With fresh bits the output is
/// byte-identical to the instance method. Mirrors aterm-wasm.
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
/// Mirrors `the aterm-wasm crate`'s `LinkHit` so the host link input is unchanged.
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

/// Selection bounds in DISPLAY viewport cell coords (0 = top visible row),
/// inclusive of `start`, with `end` already side-adjusted to match
/// `selection_text` and the painted highlight. Mirrors the aterm-wasm crate.
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

/// Project the engine's selection (terminal-relative rows) into DISPLAY viewport
/// coords, clamping partially-scrolled selections to `[0, rows)`. Uses the SAME
/// `project_range` + side-adjustment the renderer and `selection_text` use, so
/// the three always agree. `None` when there is no selection or it is fully
/// outside the viewport. Mirrors the aterm-wasm crate.
fn selection_range_for(term: &Terminal, rows: usize, cols: usize) -> Option<SelectionRange> {
    let last_col = (cols as u16).saturating_sub(1);
    let proj = term.text_selection().project_range(last_col)?;
    let offset = term.grid().display_offset() as i32;
    let rows_i = rows as i32;

    // terminal_row -> display_row = terminal_row + display_offset.
    let start_disp = proj.start_row + offset;
    let end_disp = proj.end_row + offset;

    // Fully outside the viewport (both ends above the top or below the bottom).
    if end_disp < 0 || start_disp >= rows_i {
        return None;
    }

    // Clamp to the viewport: a row scrolled off the top enters at col 0 of the
    // top row; one past the bottom exits at the last col of the bottom row.
    let (start_y, start_x) = if start_disp < 0 {
        (0u16, 0u16)
    } else {
        (start_disp as u16, proj.start_col)
    };
    let (end_y, end_x) = if end_disp >= rows_i {
        ((rows_i - 1) as u16, last_col)
    } else {
        (end_disp as u16, proj.end_col)
    };

    Some(SelectionRange {
        start_x,
        start_y,
        end_x,
        end_y,
    })
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

/// Slice `text` to the half-open display-column range `[start_col, end_col)`.
/// Approximates display width as 1 per char — correct for the ASCII URLs/paths
/// that dominate link detection (mirrors the aterm-wasm crate).
fn slice_by_columns(text: &str, start_col: usize, end_col: usize) -> String {
    text.chars()
        .skip(start_col)
        .take(end_col.saturating_sub(start_col))
        .collect()
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
// ASYNC WebGL init + present — wasm32 only (the WebGL backend + the
// HtmlCanvasElement / wasm_bindgen_futures glue exist only on the browser
// target). On native this whole block is absent; native callers drive
// aterm-gpu directly via its synchronous `GpuRenderer::new` + window surface.
// ---------------------------------------------------------------------------
/// An empty `RawDisplayHandle::Web` provider. wgpu 29 requires the instance to
/// carry a display handle before `create_surface()`, but the WebGL backend reads
/// the canvas from the WINDOW handle and ignores the display — so this ZST marker
/// only exists to satisfy wgpu-core's display-handle gate on the canvas path.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
struct WebDisplay;

#[cfg(target_arch = "wasm32")]
impl wgpu::rwh::HasDisplayHandle for WebDisplay {
    fn display_handle(&self) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
        let raw = wgpu::rwh::RawDisplayHandle::Web(wgpu::rwh::WebDisplayHandle::new());
        // SAFETY: the Web display handle is an empty marker (no borrowed data),
        // so a 'static borrow is sound — there is nothing for it to outlive.
        Ok(unsafe { wgpu::rwh::DisplayHandle::borrow_raw(raw) })
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl AtermGpuTerminal {
    /// ASYNC: acquire the GPU and create + configure a WebGL2 surface on `canvas`.
    ///
    /// This is the browser equivalent of aterm-gpu's native `GpuRenderer::new` +
    /// `create_window_surface`, but every blocking step is `await`ed AND the
    /// surface is created BEFORE the adapter (the WebGL backend enumerates its
    /// adapter against the canvas surface — the GL context lives on the canvas):
    ///   - `wgpu::Instance` with the WebGL (GL) backend,
    ///   - `instance.create_surface(SurfaceTarget::Canvas(canvas))`,
    ///   - `GpuContext::from_instance_with_surface(instance, Some(&surface)).await`
    ///     — adapter + device, NO blocking `block_on`,
    ///   - `GpuRenderer::from_parts(ctx, cpu_face, ..)` — the portable, thread-
    ///     free, font-discovery-free renderer assembly (all wgpu pipelines built),
    ///   - `configure_window_surface(surface, w, h)` — same format selection as
    ///     native's `create_window_surface`.
    ///
    /// Returns `Err` (a JS string) if WebGL is unavailable or any step fails, so
    /// the host can fall back to the CPU `aterm-wasm` path.
    pub async fn init(&mut self, canvas: web_sys::HtmlCanvasElement) -> Result<(), String> {
        self.init_with_target(wgpu::SurfaceTarget::Canvas(canvas))
            .await
    }

    /// Worker variant: acquire the GPU + create the WebGL2 surface on a TRANSFERRED
    /// `OffscreenCanvas`, so the entire GPU render+present runs off the renderer main
    /// thread (the universal off-main win — wgpu maps `SurfaceTarget::OffscreenCanvas`
    /// to the OffscreenCanvas WebGL2 context inside the worker). Same shared init as
    /// the on-canvas path; only the surface target differs.
    pub async fn init_offscreen(&mut self, canvas: web_sys::OffscreenCanvas) -> Result<(), String> {
        self.init_with_target(wgpu::SurfaceTarget::OffscreenCanvas(canvas))
            .await
    }

    /// Shared GPU bring-up for both the on-canvas (`init`) and worker OffscreenCanvas
    /// (`init_offscreen`) paths: build the WebGL instance + the surface from `target`,
    /// then the adapter/device/renderer + swapchain config (all canvas-agnostic).
    /// Non-`pub` so wasm-bindgen leaves it a normal Rust method (its `SurfaceTarget`
    /// arg isn't a JS type).
    async fn init_with_target(
        &mut self,
        target: wgpu::SurfaceTarget<'static>,
    ) -> Result<(), String> {
        // The browser WebGL2 backend. GL is the only backend compiled into the
        // wasm closure (default-features = false + features=["webgl"]); wgpu maps
        // `Backends::GL` to the canvas WebGL2 context on wasm32.
        //
        // wgpu 29 gates `create_surface()` on the instance carrying SOME display
        // handle (wgpu-core returns MissingDisplayHandle for (None, None) — the
        // safe `SurfaceTarget::Canvas` path passes no display handle). The WebGL
        // backend reads the canvas from the WINDOW handle and ignores the display,
        // so we attach an empty `RawDisplayHandle::Web` marker purely to satisfy
        // that gate. Without it, canvas surface creation fails headless.
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor {
                backends: wgpu::Backends::GL,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            }
            .with_display_handle(Box::new(WebDisplay)),
        );

        // The WebGL backend (unlike WebGPU) can only acquire an adapter from a
        // surface — the GL context lives ON the <canvas>. So create the surface
        // FIRST, then request the compatible adapter via the shared async core.
        // `create_surface` is on the instance directly; the rest of init mirrors
        // native.
        let surface_raw = instance
            .create_surface(target)
            .map_err(|e| format!("create canvas surface failed: {e}"))?;

        // Adapter + device, AWAITED (browsers forbid blocking the main thread).
        // Reuses aterm-gpu's shared async core, but passes the canvas surface as
        // the compatibility target so the GL backend can produce an adapter.
        let ctx = GpuContext::from_instance_with_surface(instance, Some(&surface_raw))
            .await
            .map_err(|e| format!("WebGL adapter/device init failed: {e}"))?;

        // Build the CPU face from the injected font bytes (no system font
        // discovery on wasm) and assemble the portable GPU renderer on the
        // acquired context — this builds every wgpu pipeline.
        let mut cpu = Renderer::from_bytes(&self.font_bytes, self.px, self.theme)?;
        // Same as the ctor face: no system font discovery on the web (E1).
        cpu.set_runtime_font_discovery(false);
        // Re-apply any fonts the host injected BEFORE init: the fresh face above is
        // built from `font_bytes` alone, so it lacks them otherwise.
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
        // Re-apply a line-height set before init (the fresh face is built at 1.0).
        if (self.line_height - 1.0).abs() > 0.001 {
            cpu.set_line_height(self.line_height);
        }
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
        // Re-apply window-chrome set before init (set_chrome; the fresh face
        // defaults to the exact-fit frame) — frame_size below then sizes the
        // swapchain to the padded frame. Same retention contract as above.
        cpu.set_pad(self.cpu.pad());
        cpu.set_head(self.cpu.head());
        let renderer = GpuRenderer::from_parts(ctx, cpu, None, self.theme)?;

        // Configure the already-created canvas swapchain (NON-sRGB format, sized
        // to the grid) on the renderer's adapter/device. Reuses aterm-gpu's
        // `configure_window_surface` (same format selection as native).
        let (w, h) = renderer.frame_size(self.rows, self.cols);
        let surface = renderer
            .configure_window_surface(surface_raw, w as u32, h as u32)
            .map_err(|e| format!("configure canvas surface failed: {e}"))?;

        self.gpu = Some(GpuState {
            renderer,
            surface,
            win: WindowGpu::new(),
        });
        // WF-1: a FRESH swapchain holds no frame of ours, so the settled-frame
        // gate must not be able to skip the first present onto it. (Also covers
        // re-`init` onto a new canvas, and every mutator the host applied
        // before `init` — their bumps are moot once the key is dropped.)
        self.last_frame_key = None;
        self.last_render_gated = false;
        Ok(())
    }

    /// Present one frame on the GPU canvas. Errors (returned as JS strings) when
    /// WebGL is uninitialized or the canvas surface reports a typed transient
    /// present failure (`Reconfigured`, `Timeout`, `Occluded`, or `Validation`).
    /// `Reconfigured` means the resize repair already ran but this frame was not
    /// presented; the host should keep its rAF loop alive and retry a later frame,
    /// not treat a single dropped present as terminal shutdown.
    ///
    /// Draws the ACTUAL terminal grid: snapshot the engine state
    /// (`term.cell_frame`), then aterm-gpu's `present_input` renders it offscreen
    /// (glyph atlas upload + instanced bg/glyph/cursor quads) and blits that
    /// texture into the WebGL2 canvas swapchain — the same encode the native
    /// CPU==GPU parity tests gate, now on the WebGL backend.
    pub fn render(&mut self) -> Result<(), String> {
        // WF-1 frame gate: pumps + the settled-frame decision. `None` means the
        // canvas already holds a byte-identical frame, so neither the engine
        // build NOR the swapchain acquire/blit/submit/present below runs.
        let Some(key) = self.open_frame() else {
            // A gated tick reports EXACTLY what an ungated one would have: the
            // gate must never turn a missing `init` into `Ok(())`. (It also
            // cannot fire before the first successful present, because
            // `last_frame_key` starts `None` and `init` resets it.)
            return if self.gpu.is_some() {
                Ok(())
            } else {
                Err("render() before init()".to_string())
            };
        };
        // The whole engine-side half of the tick (refill -> damage consume ->
        // effects -> scroll stamp -> spill), shared verbatim with
        // `render_offscreen` and the native `render_headless` seam.
        self.build_frame();
        self.record_presented_frame(key);
        let gpu = self.gpu.as_mut().ok_or("render() before init()")?;
        // `invert == false`: straight present (the visual-bell flash is host-driven).
        // `None` overlay/tray: the web/canvas path has no native drag-drop overlay or
        // settings card (GUI-only).
        let presented = webgl_present_result(gpu.renderer.present_input(
            &mut gpu.win,
            &mut gpu.surface,
            &self.frame_scratch,
            false,
            None,
            None,
        ));
        if presented.is_err() {
            // The canvas did NOT receive this frame (Reconfigured / Timeout /
            // Occluded / Validation). Drop the key so the retry the host is
            // told to make actually draws instead of gating.
            self.last_frame_key = None;
        }
        presented
    }

    /// SECONDARY (e2e) path: render the current grid OFFSCREEN and read the pixels
    /// back into the internal RGBA8 framebuffer, so a host harness can pixel-compare
    /// GPU vs CPU output without reading the live canvas (a WebGL swapchain is not
    /// CPU-readable). Mirrors `the aterm-wasm crate`'s `render()`+`rgba()` contract:
    /// the same `cell_frame` snapshot, the same `Frame` (0xTTRRGGBB; TT is the
    /// transmittance byte, 0 = opaque) expanded to RGBA8 (alpha 0xff except on
    /// default-bg pixels under `set_background_opacity`). Errors if WebGL was
    /// not initialized.
    pub fn render_offscreen(&mut self) -> Result<(), String> {
        // Deliberately UNGATED: this is the e2e pixel-compare path, and a
        // harness that asks for a readback must get one. It still runs the same
        // frame-boundary pumps every tick pays.
        self.pump_reflow_on_render_tick();
        self.drain_compress_backlog_on_render();
        // The SAME frame build `render` runs — same kept scratch, same effects
        // overlays, same sub-row stamp, same spill refresh — so the e2e
        // pixel-compare path can never diverge from the canvas path.
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
        // aterm Frame pixels are packed 0xTTRRGGBB (TT = transmittance byte,
        // 255 - alpha; 0 = opaque, the historical bytes — see aterm-wasm's
        // `render`); expand to RGBA8 for ImageData. Alpha is 0xff at the
        // default opacity, translucent on default-bg pixels below it.
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

// Native-only construction helper for the host-side regression tests: the wasm
// `new` takes injected `font_bytes`, but a native test has no JS to fetch a font,
// so build the CPU face from the first available SYSTEM monospace font instead.
// Mirrors `the aterm-wasm crate`'s `new_from_system`, and — critically — applies
// the SAME clamp-sync as `new` (store the grid's CLAMPED dims, never the raw args)
// so the framebuffer can't be sized past the 1..=4096 grid bound.
#[cfg(not(target_arch = "wasm32"))]
impl AtermGpuTerminal {
    /// The NATIVE-visible half of `render`: the identical frame gate and the
    /// identical frame build, minus the WebGL present (a swapchain needs a
    /// browser canvas, so `render` itself is a `wasm32`-only export).
    ///
    /// Returns `true` when the frame was elided by the settled-frame gate.
    ///
    /// This exists so the gate and the per-tick frame build are measurable and
    /// testable OFF the browser: `crates/aterm-bench/benches/gpu_web_frame_gate.rs`
    /// drives it, and the crate's own native tests use it as the gate's reach
    /// witness. It is deliberately NOT a second implementation — it calls the
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
        // Same tiered-store attachment as `new` (audit E1) so tests/benches
        // measure the shipped engine shape.
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
            px,
            gpu: None,
            rgba: Vec::new(),
            fb_width: 0,
            fb_height: 0,
            smart: SmartSelection::with_builtin_rules(),
            fallback_font: None,
            fallback_chain_extra: Vec::new(),
            emoji_font: None,
            bold_font: None,
            symbol_font: None,
            line_height: 1.0,
            text_shaping: aterm_render::TextShapingConfig::default(),
            frame_scratch: RenderInput::empty(),
            effects: EffectsPipeline::new(),
            theme_cursor: theme.cursor & 0x00FF_FFFF,
            theme_fg: theme.fg & 0x00FF_FFFF,
            theme_bg: theme.bg & 0x00FF_FFFF,
            notifications,
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
    use super::*;

    /// The WF-1 bumps mirrored into `effects_api` must be LIVE, not a stub.
    /// Parity with aterm-wasm is a text guard: it proves the call sites exist,
    /// never that `note_host_visual_change` does anything here. If this hook
    /// were an empty body, the mirrored module would look wired while dropping
    /// every bump — and now that this crate HAS the settled-frame gate, an
    /// effects config change would be served a STALE frame (decorations/comets
    /// ignite inside `apply`, which a gated frame never runs, while
    /// `is_active()` still reads false at gate time).
    ///
    /// Also pins the other half of the twin's contract: `advance_effects` must
    /// NOT bump. Hosts pump it once per rAF tick, so bumping there would hold a
    /// future gate open on every frame and delete the optimization outright.
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

        // One representative per mutator family the mirror carries: focus/
        // visibility gates, keystroke ignition, cursor wake, PHOSPHOR rain,
        // and sparkle words.
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

    /// THE GATE, both sides. A settled tick must skip the entire frame; every
    /// class of change must reopen it.
    ///
    /// The SKIP side is the win. The REOPEN sides are the whole risk: a gate is
    /// only as correct as the completeness of what reopens it, so each class of
    /// change the engine's damage epoch CANNOT see gets its own case here.
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

        // REOPEN 6: an ACTIVE effects pipeline animates, so it must never gate,
        // and the active->idle edge must buy exactly one settle frame.
        // (Matrix rain is the one ambient effect a WEB host can actually ignite
        // — the glow/trail engines need a typed-move proof the web glue has
        // never carried; see `spill_exports_surface_band_content_on_the_cpu_face`.)
        t.set_matrix_rain_enabled(true);
        assert!(!t.render_headless(), "an ignited effect must draw");
        let mut saw_active = false;
        for i in 0u32..60 {
            // Real output every so often: rain deliberately holds still on a
            // pane the user is only READING, so a purely idle terminal is a
            // state where it is correct for the gate to close.
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

    /// THE MUTATOR AUDIT. The gate reads `host_visual_gen`, so any `pub fn`
    /// that changes what a frame looks like and does NOT bump serves a stale
    /// frame. Before the gate landed, this crate had ZERO real bumps in
    /// lib.rs — the audit was only done for the mirrored `effects_api` half —
    /// and shipping the gate on that half alone would have frozen the canvas on
    /// a theme flip, a font swap, or a drag.
    ///
    /// Every entry is called from a SETTLED terminal and must reopen the gate.
    /// Fallible entries are called with deliberately invalid arguments: the
    /// bump must precede validation, so "the host asked for something" always
    /// buys a frame and can never buy a stale one.
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
            // LAST, because it changes the grid under everything above: a
            // resize reconfigures the swapchain, whose fresh contents are
            // undefined, so it must reopen the gate even at unchanged dims.
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

    /// The gate must never let the canvas keep a frame it did not receive.
    ///
    /// Native builds have no swapchain, so `render` (the wasm32 export) always
    /// reports "before init()". That is exactly the case worth pinning: a gated
    /// tick has to report what an ungated one would, and a terminal that has
    /// never presented must never gate.
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

        // (`render_offscreen`'s ungated contract cannot be asserted here — it is
        // a wasm32-only export, like `render`. It holds by construction: it
        // never calls `open_frame`, and it drops `last_frame_key` afterwards
        // because the frame it produced went to the readback target, not to the
        // canvas the key is a claim about.)
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

    /// E9a parity with `the aterm-wasm crate`'s `search_meta`: the GPU module's meta
    /// export must agree with its legacy `search` export and carry the
    /// `incomplete` signal that export drops.
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

    /// JS hands the GPU binding raw u16 rows/cols; the grid clamps them to 1..=4096,
    /// but the binding used to store the RAW args and feed them to `frame_size`/
    /// `cell_frame_into`, sizing the GPU framebuffer from an unclamped 65535×65535 →
    /// oversized texture (wgpu abort) / ~tens-of-GB alloc / framebuffer-grid divergence.
    /// Construction and `resize` must both re-sync `self.rows`/`self.cols` to the
    /// CLAMPED grid dims. Mirrors `the aterm-wasm crate`'s regression test.
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

    /// Mirrors `aterm-wasm`'s shrink-search test: retention shrink must
    /// invalidate the search index like any content change — a
    /// shrink-then-search may not return absolute rows the engine just
    /// evicted.
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

    /// Mirrors `aterm-wasm`'s set_scrollback_limit test: the limit is the ONE
    /// total retention bound across ring + staged + store (audit E1) — shrink
    /// evicts oldest immediately; grow widens the store share.
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

    /// Mirrors `aterm-wasm`'s encode_key tests: the GPU binding shares the same
    /// engine front-end, so DECCKM / modifiers / Kitty negotiation must flow
    /// through `Terminal::keyboard_mode()` identically.
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

    /// Native stand-in for the effects render path (GPU init is wasm-only):
    /// exactly what `render`/`render_offscreen` do before the GPU encode —
    /// refill the kept snapshot, apply the effects pipeline onto it, then
    /// refresh the spill-band export from the same snapshot.
    fn fill_frame(t: &mut AtermGpuTerminal) {
        t.refill_frame_scratch();
        let (cw, ch) = t.cpu.cell_size();
        t.effects.apply(&mut t.term, &mut t.frame_scratch, cw, ch);
        t.spill.update(&t.cpu, &t.frame_scratch);
    }

    /// Snapshot equality ACROSS TWO ENGINE INSTANCES.
    ///
    /// `RenderInput`'s `PartialEq` compares the three sprite atlases by published
    /// `Arc` IDENTITY, never `version` (the split-pane audit: baker versions are
    /// deterministic per engine instance, so a rebuilt engine replays its
    /// predecessor's versions with different texels — identity is the only sound
    /// key for the damage gates). Identity is therefore a PER-INSTANCE property:
    /// two engines that bake byte-identical atlases still publish different
    /// `Arc`s, so `==` reports unequal for frames that are pixel-for-pixel the
    /// same. Determinism is a claim about CONTENT, so compare atlas VALUES here,
    /// then neutralize the identity difference and let the real `PartialEq` judge
    /// every other field — that way this helper cannot drift as fields are added.
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

    /// SPILL BAND on the GPU engine's CPU face (present is wasm-only; spill is
    /// engine-independent CPU math over the same emission streams): the WATER
    /// jump splash on the top row must surface band bytes whose source-over
    /// composite onto the theme bg equals the CPU face's composed band
    /// byte-for-byte — the face aterm-gpu's CPU==GPU suite gates, so the
    /// exported spill is valid for the GPU-presented frame too. Plus the
    /// export disciplines: strip sizing, pointer stability, idle rev/rects
    /// stillness, and the 0/0-chrome identity.
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
        // The glow observes cursor motion frame-to-frame: keystrokes
        // interleave with frames, each WITNESSED through the committed-char
        // seam exactly as a JS keydown handler would (RED from 201449c2 until
        // that seam existed: the movement-admission gate — 'COLD PROGRAM
        // MOVEMENT IS NOT A TRAIL EVENT' — leaves unwitnessed motion dark, and
        // note_keystroke is deliberately text-blind). The admitted row-0
        // splashes then arc water droplets above the grid into the head band.
        for (ch, bytes) in [('a', b"a"), ('b', b"b"), ('c', b"c")] {
            assert!(
                t.note_typed_char(ch),
                "a witnessed simple scalar must arm movement provenance"
            );
            t.process(bytes);
            t.advance_effects(30.0);
            fill_frame(&mut t);
        }
        // Find a frame whose band coverage is LIVE (nonzero alpha), not merely
        // rev-ticked: the rev also advances on the ERASE that follows a
        // droplet's exit, and parity below must sample a frame with something
        // on it to be non-vacuous. Check-then-advance: the last witnessed
        // splash is typically still airborne right now.
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

        // AN UNWITNESSED PROGRAM LEAP-WRITE GOES DARK — the movement-admission
        // gate's whole point ('COLD PROGRAM MOVEMENT IS NOT A TRAIL EVENT'): a
        // denied relocation retires the live wake in one teardown, so the
        // airborne droplets vanish and the band drains to zero coverage
        // instead of a splash following program output. Measured mechanism:
        // the same leap used to be this test's splash vehicle before
        // 201449c2 gated it.
        t.process(b"\x1b[1;30Hz");
        let mut drained = false;
        for _ in 0..40 {
            t.advance_effects(16.0);
            fill_frame(&mut t);
            if t.frame_scratch.cursor_glow_add.is_empty()
                && t.spill
                    .rgba()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|px| px[3] == 0)
            {
                drained = true;
                break;
            }
        }
        assert!(
            drained,
            "an unwitnessed leap retires the wake — cold movement stays dark"
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

    /// `set_chrome` before GPU init must stick on the CPU fallback face (the
    /// init retention block re-applies it to the fresh face, and `frame_size`
    /// then sizes the swapchain to the padded frame); 0/0 restores exact-fit.
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

    /// Determinism through the GPU binding's snapshot path: two instances fed
    /// the same bytes + the same dt stream produce IDENTICAL `RenderInput`s
    /// (full PartialEq — cells and every effect overlay channel), so the GPU
    /// encode draws identical frames; and toggling everything back off
    /// restores the untouched-instance snapshot exactly.
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

    /// Mirrors `aterm-wasm`: the host word-separator setting reshapes
    /// double-click words (xterm.js `wordSeparators`), and clearing it
    /// restores the engine's default class-based word logic exactly.
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

    // --- Cooperative width-reflow offload (mirrors aterm-wasm's suite) ------
    //
    // The GPU present paths are wasm-only, so the render-grace safety net is
    // driven through `pump_reflow_on_render_tick` directly (the exact fn the
    // wasm `render`/`render_offscreen` call); everything else runs the same
    // JS-visible API the host drives.

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
        // A ring-only engine (no tiered store — the pre-E1 ctor shape, still
        // reachable via checkpoint-restore of old grids): every resize is the
        // plain bounded path — nothing stashes, nothing to pump.
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
        // P1 regression (twin of the aterm-wasm test): after a
        // width-shrink reflow overflows wrapped rows into scrollback, a
        // scrolled-back HISTORY row returned correct TEXT but row_len /
        // row_is_wrapped resolved through Grid::row (None past the ring base).
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
        // P4 regression (twin of the aterm-wasm test): the single-slot
        // display_row_cache keyed (content_gen, display_offset, row) with NO
        // alt-screen bit, so a main<->alt swap at a coinciding content_gen +
        // offset + row served one buffer's cell for the other.
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
        // The content-intact defer test, driven in RANDOM SMALL BUDGETS: every
        // pump is one bounded `reflow_step` slice; any schedule must re-attach
        // the same history a one-shot rewrap yields (aterm-grid's
        // schedule-independence property; here the JS-visible outcome).
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
        // A width change while a job is HALF-STEPPED: the store is out, so
        // nothing re-detaches; the partly-stepped job keeps its progress and
        // still re-attaches (content valid, wrapping stale for the newest
        // width, self-healing on the next width change).
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
        // SAFETY ARGUMENT UNDER TEST: an un-updated host never calls
        // `pump_reflow`; the render-tick net (wired into the wasm-only
        // `render`/`render_offscreen`) must close the window by itself —
        // ONE bounded step per frame after grace, converging across frames.
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
        // SAFETY ARGUMENT UNDER TEST: a host that neither pumps nor renders
        // (hidden tab) but keeps feeding output — past the cap EVERY `process`
        // call advances one bounded step, so the window closes after finitely
        // many calls (amortized convergence; no unbounded catch-up task).
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

    /// GPU-module parity for the budgeted export: resumed slices complete to
    /// the legacy one-shot triplets, a superseded cursor restarts, and the
    /// empty-query / invalid-regex edges answer an empty complete result.
    /// Mirrors the aterm-wasm crate's budgeted tests.
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
