// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! `aterm-wasm` — the in-page rendering substrate that replaces `@xterm/xterm`'s
//! rendering in the Electron renderer.
//!
//! Architecture (see docs/rust-migration): the daemon keeps the PTY and streams
//! bytes to the renderer; here, in the renderer process, the aterm engine
//! (`aterm-core`) parses those bytes into its grid and the pure-Rust CPU
//! rasterizer (`aterm-render`) turns the grid into an RGBA framebuffer that JS
//! blits to a `<canvas>`. No GPU/winit/DOM dependency — everything compiles to
//! `wasm32-unknown-unknown`. Fonts are injected as bytes (fetched in JS) so there
//! is no `std::fs` font discovery.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

mod dirty_band_present_api;
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
use aterm_render::{RenderInput, Renderer, SpillBand, Theme, WindowCpu};

// ---------------------------------------------------------------------------
// Cooperative (thread-free) width-reflow offload — the wasm L0-freeze fix.
//
// This process is single-threaded by target (wasm32-unknown-unknown, no
// atomics): the native a69a6bb3 offload has no worker thread to hand the
// O(history) scrollback rewrap to. Instead, `resize` DETACHES the tiered
// history in O(1) (`Terminal::resize_offloading_scrollback`), resizes the
// visible grid + bounded ring synchronously, and stashes the `Send` job in
// module state; the rewrap then runs in LATER host tasks — either the host
// calls the exported `pump_reflow()` from schedulable moments
// (setTimeout/requestIdleCallback), or the safety nets below drive it.
//
// Each pump is BOUNDED: `PendingScrollbackReflow::reflow_step` rewraps at
// most `REFLOW_STEP_BUDGET_LINES` history lines per call (carving only at
// logical-line boundaries; any step schedule is content-identical to the
// one-shot rewrap — aterm-grid's `reflow_step_any_schedule_matches_one_shot`
// property). So the offload bounds LATENCY per task, not just scheduling:
// no single event-loop task grows with session history. (The pre-seam
// honest label — "one long, host-schedulable task" — is retired.)
// ---------------------------------------------------------------------------

/// Histories at or below this many lines are rewrapped INLINE by `resize`
/// (bounded, imperceptible) instead of deferred. Mirrors aterm-gui's
/// `INLINE_REFLOW_MAX_LINES` (app_render.rs): the size at which a synchronous
/// rewrap is safe without freezing the liveness-critical thread/loop.
const INLINE_REFLOW_MAX_LINES: usize = 20_000;

/// Never-pumped-host safety net #1: `render()` calls after a deferred rewrap
/// is stashed before `render` pumps it itself. Long enough that an updated
/// host's idle-scheduled `pump_reflow()` wins the race (~2s at 60fps); short
/// enough that an UN-updated host restores deep history promptly. Guarantees
/// the detach window closes on any host that keeps rendering.
const REFLOW_PUMP_GRACE_RENDERS: u32 = 120;

/// Never-pumped-host safety net #2: while a rewrap is stashed, output that
/// scrolls off stages in the grid's lazy buffer (flushed on re-attach). If a
/// host never renders and never pumps (hidden tab, streaming PTY), that
/// buffer is the one thing that can grow: once the staged backlog exceeds
/// this bound, EVERY `process` call pumps one budgeted step, so the job
/// completes within ceil(history / `REFLOW_STEP_BUDGET_LINES`) × 2 further
/// process calls and the window closes. The backlog past the cap is bounded
/// by the output those finitely-many calls themselves feed — amortized
/// convergence with every task still short, instead of the pre-seam single
/// unbounded catch-up pump.
const REFLOW_BACKLOG_MAX_LINES: usize = 20_000;

/// Default per-pump rewrap budget, in INPUT history lines, for
/// [`AtermTerminal::pump_reflow`] (host-tunable via
/// [`AtermTerminal::pump_reflow_budget`]). Sizing: measured native release
/// cost is ~1.4 µs per near-full 80-col history line (aterm-grid's
/// `reflow_step_timing` harness, 2026-07-14: 50k lines one-shot in ~69ms;
/// worst 4_000-line step ~4.8ms), so 2_000 lines ≈ ~3ms native — still a
/// short task at the 2-3× slowdowns typical of wasm — while a deep 1M-line
/// history completes in ~1_000 pumps (~seconds when host-scheduled
/// back-to-back, bounded-latency either way).
const REFLOW_STEP_BUDGET_LINES: usize = 2_000;

std::thread_local! {
    /// Host-registered font blobs, marshaled across the JS/wasm boundary ONCE
    /// per module: per-pane engine builds reference them by `u32` handle, so the
    /// ~100–400MB face blobs (Apple Color Emoji, Noto CJK) are never re-copied
    /// per pane. The transient per-call copies otherwise fragment the linear
    /// memory into a per-pane high-water ratchet — wasm memory never shrinks.
    static REGISTERED_FONTS: std::cell::RefCell<Vec<std::sync::Arc<Vec<u8>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Register a font blob for handle-based reuse by every engine in this module.
/// Content-interned: registering identical bytes returns a handle to ONE shared
/// copy (and re-registration returns the same storage, so handles stay cheap).
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

/// A terminal + CPU renderer pair. Feed PTY bytes with [`AtermTerminal::process`],
/// then [`AtermTerminal::render`] to refresh the RGBA framebuffer, then read it
/// back via [`AtermTerminal::rgba`] (+ `width`/`height`) to draw onto a canvas.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct AtermTerminal {
    term: Terminal,
    renderer: Renderer,
    rows: usize,
    cols: usize,
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    // Persistent per-window damage cache: `render()` uses the damage-tracked
    // `render_input_cached` so a 1-cell change re-rasterizes only its row, not the
    // whole grid every frame. The GPU sibling already holds a persistent WindowGpu.
    win: WindowCpu,
    // Set by appearance-only changes (theme/palette/font) that the row-diff can't
    // see; consumed by `render()` to force one full repaint. Without it the
    // persistent cache would leave selection/cursor/recoloured cells stale.
    force_full_repaint: bool,
    // Reused per-frame engine snapshot: `render` refills this in place via
    // `cell_frame_into` instead of allocating a fresh `RenderInput` (the outer
    // container Vecs + a per-row inner Vec for each row) every frame — the same
    // kept scratch its two sibling frontends (aterm-gpu-web's `frame_scratch`,
    // the native windowed `input_scratch`) already hold (E8).
    frame_scratch: RenderInput,
    // Built-in smart-selection rules (url/file_path/email/...) for scroll-correct
    // link detection via smart_word_at; reused across link_at calls.
    smart: SmartSelection,
    // The shared visual-effects pipeline (cursor aurora/trail + sparkle words) —
    // the SAME state machines the native app drives, host-clocked via
    // `advance_effects`. Everything defaults OFF: `apply` then leaves the frame
    // byte-identical to the pre-effects path. pub(crate) so the effects_api
    // module (and tests) reach it without widening the public field surface.
    pub(crate) effects: EffectsPipeline,
    // Theme cursor colour (0x00RRGGBB) — the default the glow/trail colours
    // derive from when the host passes none, mirroring the native resolution.
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
    // tests) reach it (the effects-field posture).
    pub(crate) scroll_input: scroll_input_api::ScrollInputState,
    // Mosh-style predictive local echo (the shared aterm-predict state machine —
    // the SAME predictor the native app runs). Default mode Off ⇒ fully inert
    // until the host opts in via set_predictive_echo. pub(crate) so the
    // predict_api module (and tests) reach it (the effects-field posture).
    pub(crate) predict: aterm_predict::Predictor,
    // Resident scratch row for the predictive-echo reconcile probe (the wasm
    // twin of aterm-gui's `pred_row_scratch`): `Predictor::reconcile` runs its
    // observe closure once per retired guess plus the head, all on the SAME
    // row, so `Terminal::render_row` would allocate a fresh Vec and re-resolve
    // the whole row (palette, decorations, the lot) once per pending guess just
    // to read one `ch`. `render_row_into` refills this buffer in place instead.
    pub(crate) pred_row_scratch: Vec<aterm_core::terminal::RenderCell>,
    // Chrome-band spill rasterizer (the cross-pane window-space effects
    // export): refreshed per `render`, read back via the `spill_*` bindings.
    // Identity at 0/0 chrome — empty buffer, zero per-frame work. pub(crate)
    // so in-crate tests reach the buffer without raw-pointer reads.
    pub(crate) spill: SpillBand,
    // A deferred width-change scrollback rewrap (the cooperative offload; see
    // the module-level constants). `Some` while the tiered history is detached
    // awaiting `pump_reflow` / safety-net pumps; carries its own stepping
    // progress between pumps; dropped with the engine at teardown (the detach
    // window cannot outlive the grid it belongs to).
    pending_reflow: Option<PendingScrollbackReflow>,
    // Render-call countdown for safety net #1 (armed to
    // REFLOW_PUMP_GRACE_RENDERS when a job is stashed).
    reflow_grace: u32,
    // Per-pump `reflow_step` budget in input lines (REFLOW_STEP_BUDGET_LINES
    // unless the host tuned it via `pump_reflow_budget`).
    reflow_budget: usize,
    // Single-slot cache of the last DISPLAY row read cell-by-cell (cell_text /
    // cell_is_wide). The buffer facade walks a non-ASCII row per cell; a
    // scrolled-back row is a HISTORY row that visible_row_view materializes from
    // scrollback, so resolving it per cell re-materializes the whole row every
    // access (O(cols²) per row). Caching the once-materialized row collapses the
    // walk to O(cols). Keyed by (content_gen, display_offset, row) so any write,
    // resize, or scroll invalidates it; RefCell because the reads are `&self`.
    display_row_cache: std::cell::RefCell<DisplayRowCache>,
    // This pane's membership in the module-global scrollback byte budget
    // (audit E1): registered at construction, share applied at the frame/drain
    // boundaries. pub(crate) so scrollback_tiers_api reaches it (the
    // effects-field posture).
    pub(crate) budget_share: aterm_core::terminal::scrollback_shared_budget::ScrollbackBudgetShare,
    // Packed (x,y,w,h) dirty present bands of the last `render()` (audit E3)
    // — the spill_rects export pattern. pub(crate) for dirty_band_present_api.
    pub(crate) present_bands: Vec<i32>,
    // The sub-row translate presented LAST frame: while nonzero (and on the
    // frame it releases) the whole grid band shifted, so partial bands would
    // lie — those frames export one full band (audit E3, fractional-scroll
    // clause). pub(crate) for dirty_band_present_api.
    pub(crate) last_present_frac: i32,
    // ---- WF-1 frame gate (the web twin of the native RepaintKey / D-1 design):
    // `render()` skips the whole refill->diff->clone->raster pipeline when NOTHING
    // observable changed since the last rendered frame. The engine half of that
    // proof is `damage_epoch` (advances iff the grid changed since the last
    // consumed session); the HOST half is the counter below, bumped by every
    // wasm-API mutator of renderer-held or presentation state the epoch cannot
    // see (selection, blink phase, hollow cursor, default cursor style, spill
    // config, keystroke ignition, color scheme). The enumeration rule is
    // conservative: any `&mut` method that can change pixels without marking
    // grid damage either sets `force_full_repaint` (the existing appearance
    // discipline) or bumps this counter.
    host_visual_gen: u64,
    // The (epoch, host gen, effects-active) triple of the last frame `render()`
    // actually rendered. `None` until the first render; equality with the
    // current triple — with effects idle, no pending frac shift, no pending
    // reflow job, and no forced repaint — is the skip proof.
    last_frame_key: Option<FrameGateKey>,
    // Whether the last `render()` took the gate's skip path (present_bands
    // cleared to ZERO bands, framebuffer + rgba untouched). Exposed via
    // `last_render_skipped()` — the gate's two-sided reach witness for tests
    // and benches.
    last_render_gated: bool,
    // Shadow of the last blink phase handed to the renderer, so a host timer
    // that re-asserts the SAME phase (coarse timers do) does not force a
    // render. `None` = never set (the first set always bumps).
    blink_phase_shadow: Option<bool>,
    // Shadow of the last hollow-cursor override — same de-dup contract.
    hollow_shadow: Option<bool>,
}

/// The WF-1 frame-gate key: every input of a rendered frame that can change
/// between two `render()` calls at stable dims/config. Grid content folds into
/// `damage_epoch` (one u64 that advances iff a damage session existed — the
/// same engine seam the native GUI's RepaintKey consumes); host visual state
/// folds into `host_visual_gen`; an ACTIVE effects pipeline never skips (it
/// animates by definition), and the active->idle transition renders exactly one
/// more frame (the key differs on `effects_active`) so the settled/cleared
/// overlay channels are painted before the gate closes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FrameGateKey {
    damage_epoch: u64,
    host_visual_gen: u64,
    effects_active: bool,
}

/// The single-slot per-row display-cell cache backing `cell_text`/`cell_is_wide`.
#[derive(Default)]
struct DisplayRowCache {
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
/// aligned with the renderer theme. The renderer cannot own these colours by
/// itself: sparse row tails and live selection colours are synthesized by
/// `Terminal`, then stamped into every frame by
/// [`AtermTerminal::refill_frame_scratch`].
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

impl AtermTerminal {
    /// Refill every engine-owned frame channel. `cell_frame_into` includes the
    /// live implicit background and cursor colour, so sparse tails, OSC
    /// 10/11/12 resets, and DECSCNM remain one coherent terminal snapshot.
    fn refill_frame_scratch(&mut self) {
        self.term
            .cell_frame_into(&mut self.frame_scratch, self.rows, self.cols);
    }

    /// Read one `(grapheme, is_wide)` display cell through the single-slot row
    /// cache, refreshing it from [`display_row_grapheme_cells`] only on a
    /// `(content_gen, display_offset, row)` change. Collapses a host's per-cell
    /// walk of a scrolled-back row from O(cols²) to O(cols). `None` for an
    /// out-of-range row/col.
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
impl AtermTerminal {
    /// Build a `rows`x`cols` terminal rendered with `font_bytes` (a TTF/OTF) at
    /// `px` cell font-size. `font_bytes` is injected by the host (fetched in JS),
    /// keeping the engine free of filesystem font discovery. `fg`/`bg`/`cursor`/
    /// `selection` are 0x00RRGGBB and seed the renderer's DEFAULT theme colors;
    /// per-cell SGR colors still flow through the grid independently.
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
    ) -> Result<AtermTerminal, String> {
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();
        let theme = Theme {
            fg,
            bg,
            cursor,
            selection,
        };
        let mut renderer = Renderer::from_bytes(font_bytes, px, theme)?;
        // No filesystem on the web: system font discovery can never succeed, so
        // a real miss surfaces via `take_missing_font_classes` immediately
        // (E1 lazy fonts) instead of paying per-char fs attempts first.
        renderer.set_runtime_font_discovery(false);
        // Programming ligatures ON for the in-page renderer (the bundled
        // JetBrains Mono carries =>, !=, === …). Explicit, though Enabled is the
        // default, so the intent survives any future default change.
        renderer.set_text_shaping(aterm_render::TextShapingConfig {
            ligature_mode: aterm_render::LigatureMode::Enabled,
            ..Default::default()
        });
        // The engine clamps rows/cols to the grid's 1..=4096 ingress bound;
        // read the CLAMPED dims back so `render()` (which sizes the framebuffer
        // from `self.rows`/`self.cols`) can never be driven past the grid by a raw
        // JS u16. Storing the args verbatim would leave a 65535×65535 framebuffer
        // request → unbounded alloc / wasm32 u32 overflow → OOB.
        // Tiered store attached at construction (audit E1) — deep history lives
        // in the hot/warm LZ4 tiers, not raw ring cells; compression is drained
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
            present_bands: Vec::new(),
            last_present_frac: 0,
            term,
            renderer,
            rows,
            cols,
            rgba: Vec::new(),
            width: 0,
            height: 0,
            win: WindowCpu::new(),
            force_full_repaint: false,
            frame_scratch: RenderInput::empty(),
            smart: SmartSelection::with_builtin_rules(),
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
            display_row_cache: std::cell::RefCell::new(DisplayRowCache::default()),
            host_visual_gen: 0,
            last_frame_key: None,
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
    /// The canvas renderer can't read the host filesystem, so the host pushes the
    /// OS font bytes in. No-throw: a bad blob leaves the existing face untouched.
    pub fn set_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_fallback_bytes(bytes)
    }

    /// APPEND another fallback face to the chain (does NOT reset it like
    /// [`set_fallback_font`]). The chain is tried in order, so the host can push a
    /// CJK fallback first then Arabic/Devanagari/Thai/Hebrew faces after it — a
    /// glyph the earlier faces miss still reaches a covering face instead of tofu.
    /// No-throw: a bad blob leaves the existing chain untouched.
    pub fn add_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.add_fallback_bytes(bytes)
    }

    /// Inject a colour-emoji (sbix) face from font bytes, driving the existing
    /// ColorEmoji colour path. Same rationale as [`set_fallback_font`]: the host
    /// supplies the OS emoji font. No-throw (the `String` Err surfaces as a
    /// catchable JS exception); a bad blob leaves the slot untouched.
    pub fn set_emoji_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_color_font_bytes(bytes.to_vec())
    }

    /// Inject a REAL bold weight of the primary family so SGR-bold cells render as a
    /// true heavier weight instead of synthetic embolden. The host supplies the
    /// bold-variant bytes (the canvas can't read the filesystem). No-throw: a bad
    /// blob surfaces a catchable JS exception and leaves the existing weight intact.
    pub fn set_bold_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_bold_font(bytes)
    }

    /// Inject a broad-coverage SYMBOL fallback face from font bytes, so symbol
    /// glyphs the primary + fallback faces lack render real shapes instead of
    /// tofu. The byte-injection sibling of the config `symbol_font` path: the host
    /// supplies the OS symbol bytes (the canvas can't read the filesystem).
    /// No-throw: a bad blob surfaces a catchable JS exception and leaves the
    /// existing face untouched.
    pub fn set_symbol_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_symbol_fallback_bytes(bytes)
    }

    /// Swap the PRIMARY face (the host's `terminalFontFamily`) from font bytes and
    /// re-rasterize. The host re-reads cell_width/cell_height + recomputes the grid
    /// after (the new face may have different metrics). No-throw on a bad blob.
    pub fn set_primary_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_primary_font(bytes)
    }

    // ── registered-font (handle) twins ──────────────────────────────────────
    // Per-pane engine builds inject the SAME OS faces; the byte-based setters
    // above re-marshal each blob across the JS/wasm boundary per call. These
    // twins take a `register_font` handle instead, so panes 2..N of a shared
    // worker/page copy nothing (see REGISTERED_FONTS).

    /// [`AtermTerminal::new`] from a registered PRIMARY font handle.
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
    ) -> Result<AtermTerminal, String> {
        let bytes = registered_font(font_handle)?;
        Self::new(rows, cols, &bytes, px, fg, bg, cursor, selection)
    }

    /// [`AtermTerminal::set_fallback_font`] from a registered handle.
    pub fn set_fallback_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.set_fallback_font(&bytes)
    }

    /// [`AtermTerminal::add_fallback_font`] from a registered handle.
    pub fn add_fallback_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.add_fallback_font(&bytes)
    }

    /// [`AtermTerminal::set_emoji_font`] from a registered handle. Installs the
    /// SHARED interned copy (no `to_vec` of the ~190MB emoji face per pane).
    pub fn set_emoji_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.force_full_repaint = true;
        self.renderer.set_color_font_arc(bytes)
    }

    /// [`AtermTerminal::set_bold_font`] from a registered handle.
    pub fn set_bold_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.set_bold_font(&bytes)
    }

    /// [`AtermTerminal::set_symbol_font`] from a registered handle.
    pub fn set_symbol_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.set_symbol_font(&bytes)
    }

    /// Scale the cell BOX height (the host's `terminalLineHeight`) WITHOUT changing
    /// the glyph px, so rows space out while text keeps its size. The host re-reads
    /// cell_height + recomputes the grid after.
    pub fn set_line_height(&mut self, scale: f32) {
        self.force_full_repaint = true;
        self.renderer.set_line_height(scale);
    }

    /// Window-chrome for WINDOW-SPACE effects in an embedder: interior padding
    /// (`pad`, px per edge) plus a top-only rise band (`head`, px) around the
    /// grid — the `[head][pad][grid][pad]` frame aterm-render composes. The
    /// framebuffer grows accordingly (`width`/`height` report the padded frame;
    /// the host re-reads them and offsets its canvas by `-pad,-(pad+head)` so
    /// the grid stays put) and effect emissions (glow, trail, fire) become
    /// window-absolute, escaping the grid into the chrome instead of clipping
    /// at the cell edge. `0/0` (the default) is byte-identical to the
    /// historical exact-fit frame.
    pub fn set_chrome(&mut self, pad: u16, head: u16) {
        self.force_full_repaint = true;
        self.renderer.set_pad(pad as usize);
        self.renderer.set_head(head as usize);
        self.effects.set_chrome(pad, head);
    }

    /// The chrome interior padding set via [`Self::set_chrome`] (px; 0 = exact fit).
    /// Hosts read these back so canvas offsets and pointer math share one truth.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn chrome_pad(&self) -> u16 {
        self.renderer.pad() as u16
    }

    /// The chrome top head band set via [`Self::set_chrome`] (px; 0 = none).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn chrome_head(&self) -> u16 {
        self.renderer.head() as u16
    }

    /// Programming LIGATURES on/off (`=>`, `!=`, `===` …). Mirrors the native
    /// `ligatures` config knob so the in-page renderer honours the host's typography
    /// setting instead of being pinned to the constructor default. Preserves any
    /// configured `font_features`. Forces a full repaint so the change shows at once.
    pub fn set_ligatures(&mut self, on: bool) {
        let mut cfg = self.renderer.text_shaping().clone();
        cfg.ligature_mode = if on {
            aterm_render::LigatureMode::Enabled
        } else {
            aterm_render::LigatureMode::Disabled
        };
        self.renderer.set_text_shaping(cfg);
        self.force_full_repaint = true;
    }

    /// OpenType FONT FEATURES for the primary face, as a space-separated spec
    /// (`"+ss01 zero -calt"` — bare/`+tag` enables, `-tag` disables, `tag=N` sets a
    /// value). Mirrors the native `font_features` config knob. An empty/blank spec
    /// clears all features. Preserves the current ligature mode; forces a repaint.
    pub fn set_font_features(&mut self, spec: &str) {
        let features = aterm_render::parse_font_features(spec);
        let mut cfg = self.renderer.text_shaping().clone();
        cfg.font_features = if features.is_empty() {
            Vec::new()
        } else {
            vec![aterm_render::FontFeatureSet {
                font_id: 0,
                features,
            }]
        };
        self.renderer.set_text_shaping(cfg);
        self.force_full_repaint = true;
    }

    /// Set an ANSI/indexed palette colour (index 0–255; 0–15 are the 16 ANSI
    /// colours) to RGB components, so the renderer resolves SGR-indexed cell colours
    /// through the host's theme palette instead of the engine's built-in VGA
    /// defaults. Per-cell truecolor SGR still flows independently.
    pub fn set_palette_color(&mut self, index: u8, r: u8, g: u8, b: u8) {
        self.force_full_repaint = true;
        self.term.set_palette_color_components(index, r, g, b);
    }

    /// Authorize OSC 52 clipboard *write* (set) so the engine queues OSC 52
    /// app-events for the host to drain via `take_osc_events`. Without this the
    /// engine is fail-closed (CF-004) and silently drops PTY-origin OSC 52 set
    /// sequences, so they never reach the host. The host still gates the actual
    /// clipboard write on its own user setting (defense in depth).
    pub fn authorize_clipboard_write(&mut self) {
        self.term.authorize_clipboard_access(ClipboardAccess::Write);
    }

    /// Revoke OSC 52 clipboard *write* authorization (the user toggled the
    /// clipboard setting off). Returns the engine to its fail-closed default.
    pub fn revoke_clipboard_write(&mut self) {
        self.term.revoke_clipboard_access(ClipboardAccess::Write);
    }

    /// Mint an EXTRA OSC 8 URI scheme onto the engine's safe allowlist (orca
    /// deep-links §7) — e.g. `authorize_hyperlink_scheme("orca")` so
    /// host-emitted `orca://` OSC-8 hyperlinks linkify. Returns `false`
    /// (refused, nothing changes) for a malformed / over-long scheme, a
    /// never-allow scheme (`javascript`/`data`/`file`/…, however cased), or
    /// when the bounded set (4) is full; `true` when live (idempotent).
    /// Every other OSC-8 guard — byte cap, control-char and BiDi filters,
    /// the OSC-8 capability gate — still applies to extra-scheme URIs.
    pub fn authorize_hyperlink_scheme(&mut self, scheme: &str) -> bool {
        self.term.authorize_hyperlink_scheme(scheme)
    }

    /// Remove a host-minted extra scheme (case-insensitive), restoring the
    /// engine's default allowlist posture for it.
    pub fn revoke_hyperlink_scheme(&mut self, scheme: &str) {
        self.term.revoke_hyperlink_scheme(scheme);
    }

    /// Set the cursor blink phase: `true` draws the cursor this frame, `false`
    /// hides it. The host drives a ~530ms blink timer; independent of DECSCUSR.
    pub fn set_cursor_blink_phase(&mut self, on: bool) {
        // WF-1 gate: blink is renderer-held — the damage epoch can't see it.
        // De-dup through the shadow so a coarse host timer re-asserting the
        // same phase doesn't defeat the settled-frame skip.
        if self.blink_phase_shadow != Some(on) {
            self.blink_phase_shadow = Some(on);
            self.note_host_visual_change();
        }
        self.renderer.set_cursor_blink_phase(on);
    }

    /// Force a hollow (unfocused) cursor when `true`, or restore the terminal's
    /// DECSCUSR style when `false` — the standard focused/unfocused affordance.
    pub fn set_cursor_hollow(&mut self, hollow: bool) {
        // WF-1 gate: renderer-held override, invisible to the damage epoch.
        if self.hollow_shadow != Some(hollow) {
            self.hollow_shadow = Some(hollow);
            self.note_host_visual_change();
        }
        self.renderer.set_cursor_style_override(if hollow {
            Some(CursorStyle::HollowBlock)
        } else {
            None
        });
    }

    /// Drain the engine's pending query replies (DA1/DA2/DSR/CPR/DECRQM/OSC color/
    /// window-size, …) — the host forwards these to the PTY so the RENDERER (not the
    /// daemon, which stays silent) is the authoritative responder. Call after each
    /// `process`; returns `None` when nothing is pending.
    pub fn take_response(&mut self) -> Option<Vec<u8>> {
        self.term.take_response()
    }

    /// Drain pending OSC app-events as a JSON array of `[code, payload]` pairs
    /// (`[[7,"/home"],[52,"copied"]]`); `None` when the queue is empty. These
    /// carry REAL decoded payloads (OSC 52 clipboard / OSC 7 cwd / OSC 133 mark)
    /// the host routes to UI handlers — distinct from `take_response` (PTY replies).
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
    /// row (autowrap), `undefined`/`None` when out of range.
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
    /// call, then clears it (so a poll-based host can flash/ring without the
    /// synchronous bell callback).
    pub fn drain_bell(&mut self) -> bool {
        self.term.drain_bell()
    }

    /// Drain the missing-font CLASS bits (1 = text/mono fallback, 2 = colour
    /// emoji) accumulated by renders since the last call. The host polls this
    /// after a frame and lazily injects ONLY the face class actually missed —
    /// an ASCII-only session never pays the multi-hundred-MB emoji/CJK payload.
    /// Latch per class host-side: a bit can re-fire if the injected faces still
    /// miss a char.
    pub fn take_missing_font_classes(&mut self) -> u8 {
        self.renderer.take_missing_font_classes()
    }

    /// Seed the engine's DEFAULT foreground/background so its OSC 10/11 colour-query
    /// replies report the host theme (the engine otherwise reports its built-in
    /// defaults). RGB components, 0–255.
    pub fn set_default_foreground(&mut self, r: u8, g: u8, b: u8) {
        self.force_full_repaint = true;
        self.term.set_default_foreground(Rgb { r, g, b });
    }

    pub fn set_default_background(&mut self, r: u8, g: u8, b: u8) {
        self.force_full_repaint = true;
        self.term.set_default_background(Rgb { r, g, b });
    }

    /// Tell the engine the real device-pixel cell size so its CSI 14t/16t
    /// window/cell-pixel reports are accurate (the engine has no canvas otherwise).
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
        // WF-1 gate (defensive): a shrink can re-clamp a scrolled viewport;
        // the engine marks damage for that, but one extra render is cheaper
        // than coupling this gate to that guarantee.
        self.note_host_visual_change();
        let limit = if lines == 0 {
            None
        } else {
            Some(lines as usize)
        };
        self.term.set_scrollback_line_limit(limit);
    }

    /// Replace the default fg/bg/cursor/selection theme live (0x00RRGGBB), so a host
    /// theme change re-themes the pane without rebuilding it. Per-cell SGR colours
    /// flow independently; pair with set_palette_color for the ANSI palette.
    pub fn set_theme(&mut self, fg: u32, bg: u32, cursor: u32, selection: u32) {
        // Theme is appearance-only (selection band / idle cursor / padding / default
        // cells) — not tracked by the row-diff — so force one full repaint next frame.
        self.force_full_repaint = true;
        // Keep the effects' derive-from-theme default in sync (glow/trail colours
        // passed as `None` follow the cursor colour, like the native app).
        self.theme_cursor = cursor & 0x00FF_FFFF;
        self.theme_fg = fg & 0x00FF_FFFF;
        self.theme_bg = bg & 0x00FF_FFFF;
        self.effects
            .set_matrix_rain_theme(self.theme_bg, self.theme_fg);
        apply_terminal_theme_colors(&mut self.term, fg, bg, cursor, selection);
        self.renderer.set_theme(Theme {
            fg,
            bg,
            cursor,
            selection,
        });
    }

    /// Set the explicit selected-text foreground (theme `selectionForeground`),
    /// 0x00RRGGBB, or `undefined` to restore the WCAG contrast-floor default.
    /// Appearance-only, so force one full repaint next frame.
    pub fn set_selection_fg(&mut self, fg: Option<u32>) {
        self.force_full_repaint = true;
        self.term
            .set_default_selection_foreground(fg.map(|color| Rgb {
                r: ((color >> 16) & 0xff) as u8,
                g: ((color >> 8) & 0xff) as u8,
                b: (color & 0xff) as u8,
            }));
        self.renderer.set_selection_fg(fg);
    }

    /// Set the per-cell minimum contrast ratio (xterm's `minimumContrastRatio`,
    /// 1..=21): every glyph fg is floored against its OWN cell bg — the classic
    /// rescue for bright-white SGR text on a light theme. `ratio <= 1.0` turns
    /// the floor off (the default; xterm treats 1 as "do nothing"). Cells whose
    /// fg == bg are never adjusted (SGR 8 conceal renders fg = bg and must stay
    /// hidden). Appearance-only, so force one full repaint next frame.
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.force_full_repaint = true;
        self.renderer.set_minimum_contrast(ratio);
    }

    /// Set the DEFAULT-background opacity (0..=1; Ghostty's
    /// `background-opacity`). `1.0` (the default) keeps output byte-identical.
    /// Below 1.0, pixels whose bg resolved to the frame's DEFAULT background
    /// come out of [`rgba`](Self::rgba)/[`rgba_ptr`](Self::rgba_ptr) with
    /// `alpha = round(opacity*255)`, so `putImageData` onto a (transparent)
    /// canvas lets the page show through. SGR-colored bg cells, the selection
    /// band and glyph pixels stay opaque so text keeps its contrast.
    /// Appearance-only, so force one full repaint next frame.
    pub fn set_background_opacity(&mut self, opacity: f32) {
        self.force_full_repaint = true;
        self.renderer.set_background_opacity(opacity);
    }

    /// Set the CURSOR-fill opacity (0..=1; Ghostty's `cursor-opacity`). `1.0`
    /// (the default) keeps the opaque fill + block-cursor glyph cut-out
    /// byte-identical. Below 1.0 the cursor fill blends over the cell so the
    /// glyph shows through. Appearance-only, so force one full repaint.
    pub fn set_cursor_opacity(&mut self, opacity: f32) {
        self.force_full_repaint = true;
        self.renderer.set_cursor_opacity(opacity);
    }

    /// Mark the pane unfocused (`true`) / focused (`false`): when unfocused, the
    /// selection band paints with the dimmer inactive bg (xterm
    /// `selectionInactiveBackground`) instead of the active selection colour.
    /// Appearance-only, so force one full repaint next frame.
    pub fn set_selection_inactive(&mut self, inactive: bool) {
        self.force_full_repaint = true;
        self.renderer.set_selection_inactive(inactive);
    }

    /// Set the inactive (unfocused) selection background (0x00RRGGBB), or
    /// `undefined` to derive it from the active selection bg blended toward the
    /// theme bg. Only takes visible effect while the pane is marked unfocused.
    /// Appearance-only, so force one full repaint next frame.
    pub fn set_selection_inactive_bg(&mut self, bg: Option<u32>) {
        self.force_full_repaint = true;
        self.renderer.set_selection_inactive_bg(bg);
    }

    /// Re-rasterize at a new cell font px (host DPI / devicePixelRatio change) so the
    /// pane rebuilds its cell metrics instead of staying frozen at the construction
    /// dpr. The host re-reads cell_width/cell_height + recomputes the grid after.
    pub fn set_px(&mut self, px: f32) {
        self.force_full_repaint = true;
        self.renderer.set_px(px);
    }

    /// Resize the grid (after the host recomputes cols/rows for the canvas).
    ///
    /// The visible grid and the bounded in-memory ring resize SYNCHRONOUSLY
    /// (O(viewport + ring)). A width change with a deep tiered history does
    /// NOT rewrap that history here: it is detached in O(1)
    /// (`resize_offloading_scrollback`, the same audited boundary the native
    /// app uses) and rewrapped in LATER, budget-bounded host tasks — see
    /// [`Self::pump_reflow`].
    /// Small histories (≤ `INLINE_REFLOW_MAX_LINES`) rewrap inline: bounded,
    /// imperceptible, mirroring the native inline bound. This keeps the
    /// synchronous cost of a resize independent of session history — the
    /// browser-tab analog of the native L0 whole-Mac-freeze fix, on a loop
    /// with no worker thread to offload to.
    pub fn resize(&mut self, rows: u16, cols: u16) {
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
                // Stash for a later host turn. If a job is ALREADY stashed here,
                // this overwrite drops it — that is only reachable when the grid
                // re-acquired a tiered store during the old job's detach window
                // (a reset/erase), which advanced `scrollback_clear_gen`, so the
                // old job's re-attach would have discarded its content anyway
                // (the audited don't-resurrect-erased-history guard). Newest
                // wins, deterministically — the native racing-workers analog.
                //
                // A width re-resize while stashed does NOT land here: the store
                // is out, so `resize_offloading_scrollback` detaches nothing
                // (plain bounded resize, returns None) and the stashed job
                // still re-attaches — content valid, wrapping possibly stale
                // for the newest width, self-healing on the next width change
                // (the same supersede semantics as the native worker).
                self.pending_reflow = Some(pending);
                self.reflow_grace = REFLOW_PUMP_GRACE_RENDERS;
            }
        }
        // Re-sync to the CLAMPED grid dims, not the raw args: the resize clamps
        // to 1..=4096, and `render()` sizes the framebuffer from these fields.
        self.rows = self.term.grid().rows() as usize;
        self.cols = self.term.grid().cols() as usize;
        // The prediction coordinate space just changed: drop in-flight guesses
        // rather than ghost-paint them at stale coords (the native resize rule).
        self.predict.reset();
    }

    /// Advance a deferred width-change scrollback rewrap (stashed by
    /// [`Self::resize`]) by ONE BOUNDED step — at most the configured budget
    /// of history lines ([`Self::pump_reflow_budget`], default
    /// `REFLOW_STEP_BUDGET_LINES`) — re-attaching the rewrapped history when
    /// the step completes the job. Returns `true` while work REMAINS (the
    /// host should schedule another pump — a `setTimeout(0)` chain or
    /// `requestIdleCallback`); `false` once nothing is pending (the job just
    /// completed and re-attached — re-attach marks full damage, so the next
    /// `render` repaints — or there was nothing to do).
    ///
    /// COST: O(budget × cols) per call (`PendingScrollbackReflow::reflow_step`;
    /// a logical line is never split, so a soft-wrapped run longer than the
    /// budget is rewrapped whole by the step that completes it). Any pump
    /// schedule yields history content IDENTICAL to a one-shot rewrap —
    /// aterm-grid's `reflow_step_any_schedule_matches_one_shot` property.
    ///
    /// NEVER-PUMPED SAFETY: a host that never calls this still completes the
    /// rewrap — `render` pumps one step per frame once
    /// `REFLOW_PUMP_GRACE_RENDERS` frames have passed, `process` pumps one
    /// step per call while the detach-window backlog exceeds
    /// `REFLOW_BACKLOG_MAX_LINES` — and a torn-down module drops the job WITH
    /// the engine. There is no host behavior that leaves the store detached
    /// while the module keeps operating unboundedly.
    pub fn pump_reflow(&mut self) -> bool {
        let Some(pending) = self.pending_reflow.take() else {
            return false;
        };
        self.reflow_grace = 0;
        // One bounded rewrap step; on completion, the O(1) re-attach (guarded
        // against reset/erase races inside `finish_resize_offload`).
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

    /// Safety net #1 (see `REFLOW_PUMP_GRACE_RENDERS`): called by `render`.
    /// After the grace window, ONE budgeted step per frame — never the whole
    /// job in a single frame (that was the point of the stepping seam).
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

    /// Safety net #2 (see `REFLOW_BACKLOG_MAX_LINES`): called by `process`/
    /// `process_str` after feeding — ONE budgeted step per call while the
    /// staged window output is past the cap, so a stream-while-detached
    /// session converges without any single unbounded catch-up task.
    fn pump_reflow_on_output(&mut self) {
        if self.pending_reflow.is_some() && self.term.lazy_backlog_len() > REFLOW_BACKLOG_MAX_LINES
        {
            self.pump_reflow();
        }
    }

    /// Rasterize the current grid into the internal RGBA8 framebuffer via the
    /// damage-tracked path: only rows that changed since the last frame are
    /// re-rendered (the rest reuse the persistent cache), so streaming output and
    /// single-keystroke edits don't re-rasterize the whole grid every frame.
    pub fn render(&mut self) {
        // Deferred-reflow safety net #1: a host that never calls `pump_reflow`
        // still re-attaches its rewrapped history within the grace window (the
        // countdown gives an updated host's idle-scheduled pump time to win).
        self.pump_reflow_on_render_tick();
        // Frame-boundary scrollback maintenance (audit E1): apply a pending
        // global-budget share, then promote one bounded staged batch into the
        // LZ4 store — compression lives HERE, never on the ingest path.
        self.drain_compress_backlog_on_render();
        // ---- WF-1 FRAME GATE ---------------------------------------------------
        // Computed AFTER the pumps (a reflow re-attach marks full damage, so the
        // epoch term sees it). When the key equals the last RENDERED frame's key
        // and nothing present-time is pending, this frame is byte-identical by
        // construction: skip the three full-grid passes (cell_frame_into resolve,
        // compute_dirty_rows row diff, cache clone_from) AND the raster/expand
        // entirely. The skip is observable only as ZERO present bands — exactly
        // what an unchanged frame already exported through the GateHit arm — with
        // `rgba` retaining the last frame's bytes (the host contract for band
        // count 0 is "skip putImageData"; re-reading `rgba` stays valid).
        //
        // Gate terms and why each is sufficient:
        // - `damage_epoch`: advances iff the grid changed since the session this
        //   gate consumed below — writes, scrolls, erases, resizes, recolors
        //   (OSC/DECSCNM mark full damage), display-offset moves.
        // - `host_visual_gen`: every wasm-layer mutator of renderer-held state
        //   (selection, blink, hollow, spill config, ...) bumps it.
        // - `effects_active`: an active pipeline animates every frame — never
        //   skip; the active->idle edge changes the key, buying the one settle
        //   frame that paints the cleared overlay channels.
        // - frac terms: a pending or just-released sub-row translate re-presents
        //   the whole band even with zero damage (the E3 frac clause).
        // - `pending_reflow`/`force_full_repaint`: belt-and-braces bails; both
        //   already force their own repaint semantics.
        let gate_cell_h = self.renderer.cell_size().1;
        let gate_key = FrameGateKey {
            damage_epoch: self.term.damage_epoch(),
            host_visual_gen: self.host_visual_gen,
            effects_active: self.effects.is_active(),
        };
        if !self.force_full_repaint
            && !gate_key.effects_active
            && self.pending_reflow.is_none()
            && self.last_present_frac == 0
            && self.scroll_input.frac_px(gate_cell_h) == 0
            && self.last_frame_key == Some(gate_key)
        {
            // ZERO bands = "frame unchanged, skip RGBA reads and putImageData"
            // (the documented present_band_count contract). Nothing else moves.
            self.present_bands.clear();
            self.last_render_gated = true;
            return;
        }
        self.last_render_gated = false;
        // ---- END WF-1 FRAME GATE ----------------------------------------------
        // An appearance-only change (theme/palette/font) doesn't move any cell, so
        // the row-diff wouldn't repaint it — drop the cache to force one full frame.
        if self.force_full_repaint {
            self.win.invalidate();
            self.force_full_repaint = false;
        }
        // E8: refill the kept scratch in place rather than allocating a fresh
        // snapshot each frame (the gpu-web/native kept-scratch pattern);
        // `cell_frame_into` fully overwrites the engine-owned channels, and the
        // effects/stamp passes below re-fill the host-owned overlay channels, so
        // a reused scratch never carries a previous frame's state.
        self.refill_frame_scratch();
        // WF-1: consume the damage session the snapshot above just captured, so
        // the NEXT net-new grid change opens a fresh session and advances the
        // epoch the frame gate compares. Before the gate existed nothing on the
        // web path ever called take_damage, so the epoch advanced exactly once
        // per instance lifetime and could never serve as a change detector.
        // aterm-render never reads the tracker (it diffs snapshots), and this
        // render loop is the engine's only damage consumer here, so consuming
        // the session cannot starve any other reader.
        self.term.take_damage();
        // Fill the overlay channels (aurora/trail/sparkle) for the host-advanced
        // instant. With every effect off this only clears the channels a reused
        // scratch may carry — byte-identical to the pre-effects render.
        let (cw, ch) = self.renderer.cell_size();
        self.effects
            .apply(&mut self.term, &mut self.frame_scratch, cw, ch);
        // Present the banked sub-row scroll residual via the M1b band translate
        // (the whole canvas frame is grid — no spliced chrome rows). Stamped
        // every frame: the KEPT scratch would otherwise carry a stale shift.
        self.scroll_input
            .stamp(&mut self.frame_scratch, self.rows, ch);
        // Dirty-band present (audit E3): a frame carrying (or releasing) a
        // sub-row translate shifts the whole grid band, so it full-expands
        // from the TRANSLATED view while the borrow is live; every other
        // frame expands only the recorded damage from the untranslated cache
        // after the borrow ends. aterm pixels are packed 0xTTRRGGBB — TT is
        // the renderer's TRANSMITTANCE byte (255 − alpha; 0 = opaque) —
        // expanded to straight RGBA8 for ImageData, band-scoped.
        let frac_px = self.frame_scratch.scroll_frac_px;
        let frac_full = frac_px != 0 || self.last_present_frac != 0;
        self.last_present_frac = frac_px;
        {
            let view = self
                .renderer
                .render_input_cached(&mut self.win, &self.frame_scratch);
            self.width = view.width();
            self.height = view.height();
            if frac_full {
                let (w, h) = (self.width, self.height);
                dirty_band_present_api::expand_full(
                    view.pixels(),
                    &mut self.rgba,
                    &mut self.present_bands,
                    w,
                    h,
                );
            }
        }
        if !frac_full {
            self.expand_damage_to_rgba();
        }
        // Refresh the chrome-band spill export from the SAME frame snapshot the
        // renderer just composed, so `spill_rev`/`spill_ptr` are coherent with
        // this frame the moment `render` returns. Length-check no-op at 0/0
        // chrome and on frames whose band-relevant emissions are unchanged.
        self.spill.update(&self.renderer, &self.frame_scratch);
        // WF-1: this frame RENDERED — record its key so an unchanged successor
        // can skip. (The epoch in `gate_key` was latched before the take above,
        // and `take_damage` never changes the epoch VALUE — only re-arms it —
        // so an idle successor re-reads the same number and matches.)
        self.last_frame_key = Some(gate_key);
    }

    /// `true` when the LAST [`render`](Self::render) call was elided by the
    /// WF-1 frame gate: nothing observable had changed, so the framebuffer,
    /// `rgba`, and spill exports all retained the previous frame's bytes and
    /// `present_band_count()` reported 0. Hosts can use this (or the cheaper
    /// [`needs_frame`](Self::needs_frame) BEFORE calling `render`) to idle
    /// their loop; tests and benches use it as the gate's reach witness.
    pub fn last_render_skipped(&self) -> bool {
        self.last_render_gated
    }

    /// Whether the next [`render`](Self::render) would actually draw — the
    /// exported form of the WF-1 frame gate, so a JS host can skip the wasm
    /// call (and its own canvas work) entirely on settled frames.
    ///
    /// `&mut self` because reading [`Terminal::damage_epoch`] latches the
    /// current damage session (idempotent; the same read `render` performs).
    /// Advisory in one direction only: `true` may prove spurious (the render
    /// may still gate) but `false` is authoritative — a `false` here and the
    /// following `render()` is guaranteed to skip, because every term below is
    /// exactly the gate's own.
    pub fn needs_frame(&mut self) -> bool {
        let cell_h = self.renderer.cell_size().1;
        let key = FrameGateKey {
            damage_epoch: self.term.damage_epoch(),
            host_visual_gen: self.host_visual_gen,
            effects_active: self.effects.is_active(),
        };
        self.force_full_repaint
            || key.effects_active
            || self.pending_reflow.is_some()
            || self.last_present_frac != 0
            || self.scroll_input.frac_px(cell_h) != 0
            || self.last_frame_key != Some(key)
    }

    /// WF-1: record a host-visible visual change the engine's damage epoch
    /// cannot see (renderer-held or presentation state), reopening the frame
    /// gate for exactly one frame. Idempotence is the caller's choice: value
    /// shadows (blink/hollow) de-dup before calling; unconditional callers
    /// (selection ops) simply buy one render, never a stale skip.
    pub(crate) fn note_host_visual_change(&mut self) {
        self.host_visual_gen = self.host_visual_gen.wrapping_add(1);
    }

    /// Last-rendered framebuffer width in pixels.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Last-rendered framebuffer height in pixels.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Cell width in device pixels — the host computes cols = floor(canvasW / cellWidth).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cell_width(&self) -> usize {
        self.renderer.cell_size().0
    }

    /// Cell height in device pixels — the host computes rows = floor(canvasH / cellHeight).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cell_height(&self) -> usize {
        self.renderer.cell_size().1
    }

    /// Copy of the last-rendered RGBA8 framebuffer (`width*height*4` bytes),
    /// ready for `ctx.putImageData(new ImageData(rgba, width, height), 0, 0)`.
    pub fn rgba(&self) -> Vec<u8> {
        self.rgba.clone()
    }

    /// Byte offset of the last-rendered RGBA8 framebuffer within wasm linear
    /// memory, for a ZERO-COPY `putImageData` from JS (no copy out of wasm, unlike
    /// [`rgba`]). The host builds `new Uint8ClampedArray(memory.buffer, ptr,
    /// width*height*4)` and must read it synchronously right after `render()` and
    /// before any other engine call — the next `render`/`process` may reallocate
    /// `self.rgba`, and any wasm memory growth detaches the JS view.
    pub fn rgba_ptr(&self) -> usize {
        self.rgba.as_ptr() as usize
    }

    // ── SPILL BAND — the cross-pane window-space effects export ─────────────
    // The chrome band (`[head][pad][grid][pad]` minus the grid box) rendered as
    // straight-alpha RGBA the host may composite OVER NEIGHBOURING PANES:
    // source-over onto this pane's own theme bg reproduces the composed frame's
    // band bytes exactly (the seam-continuity law), so the `.pane` clip line
    // never shows a seam. Refreshed by `render()`; all identity at 0/0 chrome.

    /// Monotone revision of the spill-band content: advances ONLY when the
    /// exported bytes changed. Typing-only frames with a settled (or
    /// grid-interior) glow, idle re-renders, and 0/0 chrome keep it still —
    /// an unchanged value is the engine's word that the host may skip its
    /// blit without reading a single spill byte.
    pub fn spill_rev(&self) -> u32 {
        self.spill.rev()
    }

    /// Number of dirty rects from the LAST `render()` (0 on a no-change
    /// frame). Read together with [`spill_rects_ptr`](Self::spill_rects_ptr).
    pub fn spill_rect_count(&self) -> u32 {
        (self.spill.rects().len() / 4) as u32
    }

    /// Byte offset (in wasm linear memory) of the packed dirty-rect array:
    /// `spill_rect_count()` rects of 4 `i32`s — `x, y, w, h`, FRAME-ABSOLUTE
    /// device px. Same read discipline as [`rgba_ptr`](Self::rgba_ptr):
    /// consume synchronously after `render()`, never cache the JS view.
    pub fn spill_rects_ptr(&self) -> usize {
        self.spill.rects().as_ptr() as usize
    }

    /// Byte offset (in wasm linear memory) of the straight-alpha RGBA spill
    /// buffer: four packed row-major strips — **top** `(0, 0, width,
    /// pad+head)`, **bottom** `(0, height−pad, width, pad)`, **left** `(0,
    /// pad+head, pad, gridH)`, **right** `(width−pad, pad+head, pad, gridH)`
    /// with `gridH = height − 2·pad − head` — in that order. The pointer is
    /// STABLE across frames (the buffer re-rasters in place); it moves only
    /// when chrome or the grid size changes, so a host may hold its view
    /// between frames of one geometry (wasm memory GROWTH still detaches JS
    /// views — rebuild per read, the `rgba_ptr` rule).
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
    /// escape if veils over neighbouring panes read badly; the band then
    /// intentionally diverges from the in-frame veil pixels at the clip line.
    /// Applies from the next `render()`.
    pub fn set_spill_include_veils(&mut self, on: bool) {
        // WF-1 gate: spill config changes the exports `render` refreshes; a
        // gated render would leave them stale against the new setting.
        self.note_host_visual_change();
        self.spill.set_include_veils(on);
    }

    /// Scroll the viewport through scrollback: positive `delta` reveals older
    /// lines, negative reveals newer. `render` already honors the display offset,
    /// so the host only needs to redraw afterwards.
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

    /// True when DEC private mode 1007 (alternate scroll) is set: while the
    /// alternate screen is active and mouse tracking is off, the host converts
    /// wheel ticks into arrow-key presses (aterm-gui's WheelPlan behaviour) so
    /// TUIs without mouse support (less, man, plain vim) still wheel-scroll.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn is_alternate_scroll(&self) -> bool {
        self.term.modes().alternate_scroll()
    }

    /// True when DECCKM (application cursor keys) is set: the host must encode
    /// arrows/Home/End as SS3 (ESC O A) instead of CSI (ESC [ A) so full-screen
    /// apps (vi, less, readline) receive the sequences they expect.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn is_app_cursor_mode(&self) -> bool {
        self.term.modes().application_cursor_keys()
    }

    /// True when a TUI has enabled mouse tracking (any of DECSET 9/1000/1002/1003).
    /// The host then ENCODES canvas mouse events to the PTY instead of running
    /// selection/scroll/link for them (unless Shift is held = user override).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn is_mouse_tracking(&self) -> bool {
        self.term.mouse_tracking_enabled()
    }

    /// True when the active mouse mode reports MOTION (ButtonEvent 1002 = drag
    /// while a button is down, AnyEvent 1003 = all motion), so the host only
    /// forwards `mousemove` when an app actually wants it (no spam in 1000).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn mouse_wants_motion(&self) -> bool {
        matches!(
            self.term.mouse_mode(),
            MouseMode::ButtonEvent | MouseMode::AnyEvent
        )
    }

    /// True for AnyEvent (1003): report motion even with NO button pressed.
    /// 1002 only reports motion while a button is held; the host uses this to
    /// decide whether a button-less `mousemove` should be forwarded.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn mouse_wants_any_motion(&self) -> bool {
        matches!(self.term.mouse_mode(), MouseMode::AnyEvent)
    }

    /// True when DECSET 1004 (focus reporting) is active: the host sends CSI I on
    /// focus-in and CSI O on focus-out so apps (vim, tmux) track terminal focus.
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
    /// `CursorStyle` (1=BlinkingBlock, 2=SteadyBlock, 3=BlinkingUnderline,
    /// 4=SteadyUnderline, 5=BlinkingBar, 6=SteadyBar, 7=Hidden, 8=HollowBlock).
    /// The CPU renderer ALREADY paints this shape from the grid (cell_frame copies
    /// it into the render input, draw_cursor honors it), so this getter exists for
    /// host introspection/tests — no JS overlay is needed to draw the shape.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cursor_style(&self) -> u8 {
        self.term.cursor_style() as u8
    }

    /// Set the host-preferred DEFAULT cursor style (shape used before any DECSCUSR and
    /// restored after RIS/DECSTR). `n` follows the DECSCUSR convention: 1=blinking
    /// block, 2=steady block, 3=blinking underline, 4=steady underline, 5=blinking bar,
    /// 6=steady bar; out-of-range (0, 7+) is ignored. Unlike a render override this does
    /// NOT clobber an app's live DECSCUSR (e.g. vim insert-mode bar).
    pub fn set_default_cursor_style(&mut self, n: u8) {
        // WF-1 gate: cursor-style presentation may repaint the cursor cell
        // without a grid write; bump rather than audit the engine's marking.
        self.note_host_visual_change();
        if let Some(style) = CursorStyle::from_param(u16::from(n)) {
            self.term.set_default_cursor_style(style);
        }
    }

    /// Push the host OS color scheme into the engine. `dark = true` selects a dark
    /// appearance, `false` light. When the scheme CHANGES and the app enabled DEC mode
    /// 2031, the engine queues an unsolicited `CSI ? 997 ; Ps n` (1=dark, 2=light);
    /// drain it via `take_response` and forward to the PTY so subscribed apps live-
    /// update their theme. A no-op when the scheme is unchanged.
    pub fn set_color_scheme(&mut self, dark: bool) {
        // WF-1 gate (defensive): scheme changes can recolor engine-resolved
        // dynamic colors; whether the engine marks damage for every arm is its
        // business — one render buys certainty.
        self.note_host_visual_change();
        let scheme = if dark {
            aterm_types::Appearance::Dark
        } else {
            aterm_types::Appearance::Light
        };
        self.term.set_color_scheme(scheme);
    }

    /// Encode a mouse-button PRESS at 0-based on-screen cell `col`/`row` for the
    /// app's active mouse mode+encoding (returns `None`/`undefined` when tracking
    /// is off). `button` is the raw X10 button code (0=left,1=middle,2=right) and
    /// `mods` is the OR of Shift(4)/Alt(8)/Ctrl(16) masks — the engine combines
    /// them. Bytes are sent verbatim to the PTY.
    pub fn encode_mouse_press(&self, col: u16, row: u16, button: u8, mods: u8) -> Option<Vec<u8>> {
        self.term.encode_mouse_press(button, col, row, mods)
    }

    /// Encode a mouse-button RELEASE (see [`AtermTerminal::encode_mouse_press`]);
    /// `None` in X10 press-only mode.
    pub fn encode_mouse_release(
        &self,
        col: u16,
        row: u16,
        button: u8,
        mods: u8,
    ) -> Option<Vec<u8>> {
        self.term.encode_mouse_release(button, col, row, mods)
    }

    /// Encode mouse MOTION at `col`/`row`; `button` is the held button (3 = none).
    /// `None` unless the mode reports motion (1002 while a button is down, 1003
    /// always) — see [`AtermTerminal::mouse_wants_motion`].
    pub fn encode_mouse_motion(&self, col: u16, row: u16, button: u8, mods: u8) -> Option<Vec<u8>> {
        self.term.encode_mouse_motion(button, col, row, mods)
    }

    /// Encode a mouse WHEEL tick at `col`/`row` (`up` = wheel-up); the host sends
    /// these instead of scrolling scrollback while tracking is on. `None` in X10.
    pub fn encode_mouse_wheel(&self, col: u16, row: u16, up: bool, mods: u8) -> Option<Vec<u8>> {
        self.term.encode_mouse_wheel(up, col, row, mods)
    }

    /// Encode a keyboard event through the engine's FULL encoder — legacy +
    /// xterm modifyOtherKeys + Kitty progressive enhancement, driven by the
    /// LIVE `Terminal::keyboard_mode()` (DECCKM/DECBKM/1035/1036/1039 and the
    /// negotiated Kitty flags are exact), replacing the host's legacy-only TS
    /// encoder that acked Kitty on the wire but could never speak it.
    ///
    /// `key` is a DOM `KeyboardEvent.key` value (mapped by the shared
    /// `aterm_types::keyboard::map_dom_key` table); `mods` is the engine
    /// `Modifiers` bitfield (SHIFT=1, ALT=2, CTRL=4, SUPER=8); `event_type` is
    /// 0=Press, 1=Repeat, 2=Release; `base_layout_key` is the US-QWERTY char of
    /// the physical key for Kitty `REPORT_ALTERNATE_KEYS` (pass `undefined`
    /// when unknown). Returns `None` when the event encodes to nothing (e.g. a
    /// release without the Kitty protocol) or the key has no terminal encoding
    /// (modifier-only / IME / unidentified DOM keys — never guessed).
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
    /// (Windows ConPTY; xterm.js `vtExtensions.kittyKeyboard = false`).
    pub fn set_kitty_keyboard_enabled(&mut self, enabled: bool) {
        self.term.set_kitty_keyboard_enabled(enabled);
    }

    /// The live `Terminal::keyboard_mode()` as its raw bitflags value, for
    /// hosts that run the engine in a Web Worker: mirror these bits into the
    /// main-thread engine-state snapshot and feed them to the free
    /// [`encode_key_with_mode`], which encodes keydowns synchronously without
    /// an instance. `KeyboardMode` is a `bitflags` struct over `u16` (bits
    /// 0..=14 defined); the value is zero-extended to `u32` for headroom.
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
        // WF-1 gate: selection is Terminal-held but marks NO grid damage (the
        // native GUI folds it as its own RepaintKey term for the same reason).
        // Every selection mutator below bumps unconditionally — idempotent
        // inserts would need a fingerprint compare for zero benefit.
        self.note_host_visual_change();
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
        self.note_host_visual_change(); // WF-1 gate (see selection_start)
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
        self.note_host_visual_change(); // WF-1 gate (see selection_start)
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
    pub fn selection_extend(&mut self, row: i32, col: u16) {
        self.note_host_visual_change(); // WF-1 gate (see selection_start)
        let row = self.display_row_to_terminal(row);
        self.term
            .text_selection_mut()
            .update_selection(row, col, SelectionSide::Right);
    }

    /// Finalize the selection (mouse released).
    pub fn selection_finish(&mut self) {
        self.note_host_visual_change(); // WF-1 gate (see selection_start)
        self.term.text_selection_mut().complete_selection();
    }

    /// Drop the current selection so the highlight clears on the next render.
    pub fn selection_clear(&mut self) {
        self.note_host_visual_change(); // WF-1 gate (see selection_start)
        self.term.text_selection_mut().clear();
    }

    /// Override the characters that BREAK a double-click word (the host's
    /// word-separator setting, xterm.js `wordSeparators` semantics): a word
    /// becomes a maximal run of NON-separator characters. `undefined` restores
    /// the engine's default class-based word logic (alphanumeric + `_`)
    /// exactly. Smart-selection RULES (url/file_path/email/…) still take
    /// precedence for both `selection_word` and `link_at`; the separators only
    /// shape the plain-word fallback.
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
    /// falls back to smart-selection rules (url/file_path). Returns `None` for
    /// plain words. `kind`: 0=osc8, 1=url, 2=file_path, 3=other.
    pub fn link_at(&self, row: u16, col: u16) -> Option<LinkHit> {
        // OSC-8 lookups are NOT display_offset-aware (only valid at the live
        // bottom), so only consult hyperlink_at when the viewport isn't scrolled.
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

        // Smart-selection fallback is scroll-correct (display_row_text is
        // display_offset-aware) and works on any scrollback row.
        let (sc, ec) = self
            .term
            .smart_word_at(row as usize, col as usize, &self.smart)?;
        let text = self.term.display_row_text(row as usize)?;
        let matched = slice_by_columns(&text, sc, ec);
        let kind = classify(&matched);
        if kind == 3 {
            // A plain word is not a link.
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

    /// Serialize the terminal to a REPLAYABLE ANSI string — the aterm-native
    /// replacement for `@xterm/addon-serialize`'s `serialize({scrollback})`, so the
    /// renderer no longer needs a shadow xterm.js buffer to snapshot/restore/fork a
    /// pane. Layout: SGR reset, then the capped recent history (text + CRLF), then
    /// `CSI H`, then each visible row placed with absolute CUP + erase-line (so a
    /// full-width row can't autowrap on replay) emitted via the engine's
    /// `row_ansi_text_screen` (minimal change-based SGR, wide-char aware), then the cursor
    /// restored. `scrollback_rows` = `None` prepends ALL history, `Some(n)` the last
    /// `n`, `Some(0)` viewport-only. Ported from the daemon's proven `serialize_ansi`
    /// (orca-terminal headless) so the output stays byte-compatible with the existing
    /// string-based replay pipeline.
    pub fn serialize(&self, scrollback_rows: Option<u32>) -> String {
        use std::fmt::Write as _;
        let grid = self.term.grid();
        let cap = scrollback_rows.map(|n| n as usize);
        let active_history = grid.scrollback_lines();
        let take = cap.map_or(active_history, |n| n.min(active_history));
        // Pre-size: `take` reaches the construction default of 100k lines, and a
        // doubling `String` would re-copy the whole (multi-megabyte) output ~two
        // dozen times on the way there.
        let mut out = String::with_capacity(
            take.saturating_mul(8)
                .saturating_add(self.rows.saturating_mul(16)),
        );
        out.push_str("\x1b[0m");
        for i in (active_history - take)..active_history {
            // Bind the `Cow<'_, Line>` so `as_str`'s borrow outlives the read:
            // the old `.and_then(|l| l.as_str().map(|s| s.trim_end().to_string()))`
            // allocated, copied and freed one owned `String` per history line
            // purely because the `Cow` died inside the closure. A missing or
            // non-UTF-8 line contributes nothing before its CRLF, exactly as the
            // old `unwrap_or_default()` did.
            let line = grid.get_history_line(i);
            if let Some(s) = line.as_ref().and_then(|l| l.as_str()) {
                out.push_str(s.trim_end());
            }
            out.push_str("\r\n");
        }
        if take > 0 {
            // Scroll the printed history OFF the screen so it lands in the replay
            // target's scrollback: the trailing printed lines are still on the
            // visible grid here, and the absolute-CUP viewport paint below would
            // ERASE them — losing a viewport-sized chunk of history on every
            // replay (all of it when take < rows). One LF per resident text line
            // (at most rows-1: the final CRLF left the bottom row blank) from the
            // bottom row scrolls each top line into history and leaves a clean
            // screen for the viewport paint.
            let _ = write!(out, "\x1b[{};1H", self.rows);
            for _ in 0..take.min(self.rows.saturating_sub(1)) {
                out.push('\n');
            }
        }
        out.push_str("\x1b[H");
        for r in 0..self.rows as u16 {
            // `write!` straight into `out` — `push_str(&format!(…))` allocated a
            // throwaway `String` per visible row.
            let _ = write!(out, "\x1b[{};1H\x1b[K", r + 1);
            if let Some(row_ansi) = grid.row_ansi_text_screen(r) {
                out.push_str(&row_ansi);
            }
            out.push_str("\x1b[0m");
        }
        let c = self.term.cursor();
        let _ = write!(out, "\x1b[{};{}H", c.row as usize + 1, c.col as usize + 1);
        out
    }

    /// Scrollback HISTORY ONLY (the off-screen lines above the viewport) as flowing
    /// text + CRLF, no cursor/grid framing. Reads the MAIN buffer's scrollback (aterm
    /// keeps it in the inactive grid while the alt screen is active) so an in-alt
    /// (vim/htop/less) snapshot still recovers the pre-TUI history — the only
    /// recoverable history on cold-restore of an alt-screen session. `max_rows` caps
    /// to the last `n` lines (`None` = all). Mirrors the daemon's serialize_scrollback_ansi.
    pub fn serialize_scrollback(&self, max_rows: Option<u32>) -> String {
        let grid = self.term.main_grid();
        let history = grid.scrollback_lines();
        if history == 0 {
            return String::new();
        }
        let take = max_rows.map_or(history, |n| (n as usize).min(history));
        let mut out = String::with_capacity(take.saturating_mul(8));
        for i in (history - take)..history {
            // Same borrow-don't-copy discipline as `serialize`: one owned
            // `String` per history line, allocated and immediately freed, was
            // pure churn over a scrollback that defaults to 100k lines.
            let line = grid.get_history_line(i);
            if let Some(s) = line.as_ref().and_then(|l| l.as_str()) {
                out.push_str(s.trim_end());
            }
            out.push_str("\r\n");
        }
        out
    }

    /// The last completed OSC-133 block's output as JSON, following the
    /// `take_osc_events` JSON-drain convention (CM-A3, "Copy Last Command
    /// Output"):
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

    /// The window title (OSC 0/2), or `None` when unset — replaces the separate
    /// title channel that fed off the shadow xterm so snapshots keep window titles.
    pub fn title(&self) -> Option<String> {
        let title = self.term.title();
        if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        }
    }

    /// Whether bracketed-paste mode (DECSET 2004) is active. The input seam reads
    /// this to wrap pasted text in `ESC[200~ … ESC[201~` itself (replacing the old
    /// reliance on xterm's `terminal.paste()`, which consulted xterm's own mode).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn bracketed_paste_mode(&self) -> bool {
        self.term.modes().bracketed_paste()
    }

    /// Search the full retained buffer (scrollback + visible) for `query`,
    /// returning matches as a flat `[abs_line, start_col, len]` triplet array so
    /// the JS host can highlight + scroll without re-scanning text. Lines are
    /// ABSOLUTE rows (the index's native coordinate); the host maps them to
    /// display rows via [`AtermTerminal::search_display_origin`] /
    /// [`AtermTerminal::scroll_search_line_into_view`], which stay correct as the
    /// viewport scrolls. Empty `query` (or a regex error) yields an empty array.
    ///
    /// One-shot: pays the whole index build in this call and DROPS the
    /// engine's incomplete-results signal. Prefer
    /// [`AtermTerminal::search_budgeted`], which slices the work across calls
    /// and reports `incomplete_index`.
    pub fn search(&mut self, query: &str, case_sensitive: bool, is_regex: bool) -> Vec<u32> {
        if query.is_empty() {
            return Vec::new();
        }
        // Reuse the cached full-content index (O(1) on unchanged content). When
        // is_regex is false this is a plain substring search; when true the engine
        // compiles `query` as a regex (an invalid pattern yields Err → empty array,
        // so a half-typed regex highlights nothing rather than throwing).
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

    /// Metadata for a [`AtermTerminal::search`]-contract query — most
    /// importantly the engine's `incomplete` signal, which that legacy export
    /// has always DROPPED (E9a, correctness-first): when index eviction or the
    /// engine's match cap truncated the results, the host has been presenting
    /// a truncated match list/count as if it were exhaustive.
    ///
    /// Stateless on purpose: it re-runs `query` against the SAME cached
    /// full-content index `search` uses (O(1) index reuse on unchanged
    /// content, so the added cost is one query, never a rebuild) and reports
    /// on exactly the results that call would return — no staleness if the
    /// host asks without (or long after) a paired `search`. Empty query or
    /// invalid regex: `incomplete == false`, `match_count == 0`, mirroring
    /// the legacy export's empty array.
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

    /// Absolute row of display row 0 at the live bottom (`display_offset == 0`):
    /// `oldest_absolute_row + scrollback_lines`. A match at absolute `line` is at
    /// display row `line - origin + display_offset`, so the host computes the
    /// on-screen cell of any [`AtermTerminal::search`] match without a round-trip.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn search_display_origin(&self) -> u32 {
        let grid = self.term.grid();
        let origin = grid
            .oldest_absolute_row()
            .saturating_add(grid.scrollback_lines() as u64);
        u32::try_from(origin).unwrap_or(u32::MAX)
    }

    /// Scroll the viewport so the match at absolute `line` is visible, placing it
    /// at (or near) the top row. Clamps the target display_offset to the retained
    /// scrollback so a live-region match snaps to the bottom. Host redraws after.
    pub fn scroll_search_line_into_view(&mut self, line: u32) {
        let grid = self.term.grid();
        let origin = grid
            .oldest_absolute_row()
            .saturating_add(grid.scrollback_lines() as u64);
        let scrollback = grid.scrollback_lines();
        let current = grid.display_offset();
        // Target offset that lands `line` on display row 0; clamp to [0, scrollback].
        let want = origin.saturating_sub(u64::from(line));
        let want = (want as usize).min(scrollback);
        // scroll_display takes a delta (positive = older); convert from current.
        let delta = want as i64 - current as i64;
        if let Ok(delta) = i32::try_from(delta) {
            self.term.scroll_display(delta);
        }
    }

    /// Budgeted, resumable variant of [`AtermTerminal::search`] (P1.1): runs at
    /// most `row_budget` rows of index-build + verification per call and
    /// returns a [`BudgetedSearchResult`] with a cursor to continue, so the
    /// host can slice a deep-scrollback search across event-loop turns and
    /// CANCEL a superseded query mid-search (drop the cursor; the next call
    /// with a different pattern supersedes the in-flight state).
    ///
    /// Pass `resume_cursor: None` (or a stale value) to start; pass
    /// each step's `cursor` back to continue. A cursor is only valid for the
    /// same engine instance, pattern/options, and unchanged content — any
    /// mismatch restarts from scratch (fresh cursor, progress reset), never
    /// stale results. CPU/GPU wasm modules are separate cursor domains; keep a
    /// token with the engine that issued it. On the
    /// Each response contains a stable match DELTA (at most 4,096 records).
    /// When `reset` is true (or `search_id` changes), clear prior deltas before
    /// appending this step; this makes even a one-turn stale-content restart
    /// unambiguous after the resume cursor disappears. When `complete == true`,
    /// the deltas for that `search_id` equal a one-shot [`AtermTerminal::search`].
    /// Unlike that legacy API,
    /// `incomplete_index` reports eviction or match-cap truncation and
    /// `lowest_retained_line` identifies the searchable suffix. Empty query or
    /// invalid regex: an immediate empty `complete` result (matching the legacy
    /// API's silence on half-typed regexes). `row_budget == 0` is clamped to one
    /// row so a scanning turn always progresses; a turn may instead drain a
    /// bounded delta backlog without scanning another row.
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
            // An empty query also abandons any in-flight search: the host
            // cleared the find field, so free the partial index now.
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

    /// Drop any in-flight [`AtermTerminal::search_budgeted`] state (frees the
    /// partial index; outstanding cursors go stale and restart if resumed).
    /// Call when the find UI closes or the query is abandoned between slices.
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

    /// Batch row-range export for the P7 grid mirror (E9): the text/wrap/len
    /// (+ per-column wide map) of `count` DISPLAY rows starting at `first_row`
    /// (display_offset-aware, same coords as [`AtermTerminal::row_text`]) in ONE
    /// wasm-boundary crossing, replacing the per-row `row_text` +
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

    /// Federated search summary (fed E-1): the span-marked, snippet-enriched
    /// results for `query` in ONE call — superseding the bare-triplet
    /// [`AtermTerminal::search`] + per-match `row_text` round-trip. Returns JSON
    /// `{matches:[{absRow,col,len,snippet}], total, incomplete}` where `matches`
    /// is capped to `max_matches` (0 = uncapped), `total` is the full match
    /// count before the cap, `incomplete` is the engine's eviction/match-cap
    /// truncation signal (which [`AtermTerminal::search`] drops), and `snippet`
    /// is the match line's text (absolute-row coordinate, the same space
    /// federated matches carry). Empty query or invalid regex ⇒
    /// `{matches:[],total:0,incomplete:false}` (mirroring `search`'s silence).
    ///
    /// A bounded READ over an already-built full-content index — not a
    /// from-scratch rebuild on the hot path
    /// ([`Terminal::search_summary_results`]): after a
    /// [`AtermTerminal::search_budgeted`] scan completes over the same query +
    /// content snapshot, THAT retained index answers directly (zero rebuild);
    /// otherwise the O(1)-reused one-shot index ([`Terminal::indexed_search`])
    /// serves it, rebuilding only on a content-key miss. Either way only the
    /// `≤max_matches` capped rows pay a snippet read. Keeps the E9a
    /// [`AtermTerminal::search_meta`] `{incomplete,match_count}` shape as a
    /// compat alias — this export supersedes it with snippets.
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
}

/// Metadata for a legacy-contract search ([`AtermTerminal::search_meta`]).
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

    /// Number of matches the paired [`AtermTerminal::search`] call returns
    /// (i.e. its flat triplet array length / 3), after any cap.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn match_count(&self) -> u32 {
        self.match_count
    }
}

/// One slice of a budgeted search ([`AtermTerminal::search_budgeted`]).
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
    /// coordinate contract as [`AtermTerminal::search`]). Append across calls;
    /// already-reported matches keep their order and positions.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn matches(&self) -> Vec<u32> {
        self.matches.clone()
    }

    /// Whether every retained row has been scanned and every match delta has
    /// been delivered. Dense searches can have `rows_fed == total_rows` while
    /// this remains false for bounded backlog-drain turns.
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

    /// True when the results may be truncated: index eviction dropped old
    /// rows, or the engine's match cap was reached. (The legacy
    /// [`AtermTerminal::search`] export silently drops this signal.)
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn incomplete_index(&self) -> bool {
        self.incomplete_index
    }

    /// Final oldest absolute line retained by the completed search index. The
    /// deterministic eviction schedule makes this stable from the first turn.
    /// When nonzero with `incomplete_index`, older history was evicted; a zero
    /// watermark with `incomplete_index` indicates match-cap-only truncation.
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

impl AtermTerminal {
    /// Expand an OSC-8 hyperlink to the span of contiguous cells sharing its
    /// link. Cells group by `id=` when present (OSC 8 grouping), else by URL.
    /// Returns `[start_col, end_col_exclusive)`. Only valid at display_offset 0.
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
/// the main thread, so the host mirrors [`AtermTerminal::keyboard_mode_bits`]
/// through its engine-state snapshot and encodes synchronously here, accepting
/// one-frame staleness — the same tradeoff the host already accepts for
/// DECCKM gating via `is_app_cursor_mode`.
///
/// Parameters match [`AtermTerminal::encode_key`] (`key` = DOM
/// `KeyboardEvent.key`; `mods` = SHIFT=1, ALT=2, CTRL=4, SUPER=8;
/// `event_type` = 0=Press, 1=Repeat, 2=Release; `base_layout_key` = US-QWERTY
/// char for Kitty `REPORT_ALTERNATE_KEYS`), plus `mode_bits` from
/// `keyboard_mode_bits` (a `u16` bitflags value zero-extended to `u32`;
/// undefined bits are truncated away). With fresh bits the output is
/// byte-identical to the instance method.
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
/// `selection_text` and the painted highlight.
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
/// outside the viewport.
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

/// JSON-escape `s` and wrap it in double quotes for the `take_osc_events` /
/// `take_notifications` arrays.
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
/// No `unicode-width` dep here, so we approximate display width as 1 per char —
/// correct for the ASCII URLs/paths that dominate link detection.
fn slice_by_columns(text: &str, start_col: usize, end_col: usize) -> String {
    text.chars()
        .skip(start_col)
        .take(end_col.saturating_sub(start_col))
        .collect()
}

/// Classify a matched span: 1=url (scheme or www. host), 2=file_path (absolute,
/// relative, home, or contains `/` with no scheme), else 3=other (plain word).
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

// Native-only constructor for headless tests/benches: discovers a system font so
// the render pipeline can be exercised without injecting font bytes. The wasm
// build always uses `new` with injected fonts.
#[cfg(not(target_arch = "wasm32"))]
impl AtermTerminal {
    pub fn new_from_system(rows: u16, cols: u16, px: f32) -> Option<AtermTerminal> {
        let renderer = Renderer::from_system(px, Theme::default())?;
        // Same clamp-sync as `new`: store the grid's CLAMPED dims, never the raw
        // args, so the framebuffer can't be sized past the 1..=4096 grid bound.
        // Same tiered-store attachment as `new` (audit E1) so tests/benches
        // measure the shipped engine shape.
        let mut term = scrollback_tiers_api::tiered_terminal(rows, cols);
        let budget_share = scrollback_tiers_api::register_budget_share(&term);
        let theme = Theme::default();
        apply_terminal_theme_colors(&mut term, theme.fg, theme.bg, theme.cursor, theme.selection);
        // Same notification wiring as `new` (fail-closed until authorized).
        let notifications = notifications_api::wire_notification_queue(&mut term);
        let rows = term.grid().rows() as usize;
        let cols = term.grid().cols() as usize;
        Some(Self {
            budget_share,
            present_bands: Vec::new(),
            last_present_frac: 0,
            term,
            renderer,
            rows,
            cols,
            rgba: Vec::new(),
            width: 0,
            height: 0,
            win: WindowCpu::new(),
            force_full_repaint: false,
            frame_scratch: RenderInput::empty(),
            smart: SmartSelection::with_builtin_rules(),
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
            display_row_cache: std::cell::RefCell::new(DisplayRowCache::default()),
            host_visual_gen: 0,
            last_frame_key: None,
            last_render_gated: false,
            blink_phase_shadow: None,
            hollow_shadow: None,
        })
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

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
        let Some(mut t) = AtermTerminal::new_from_system(3, 8, 16.0) else {
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
    fn dynamic_cursor_osc10_changes_wasm_pixels() {
        let Some(mut t) = AtermTerminal::new_from_system(2, 4, 16.0) else {
            return;
        };
        t.process(b"\x1b]21;cursor=\x07\x1b]10;#21C365\x07\x1b[2 q");
        t.render();
        assert_eq!(t.term.cursor_color(), None);
        assert_eq!(t.frame_scratch.cursor_color, 0x0021_C365);
        let (cw, ch) = t.renderer.cell_size();
        assert_eq!(
            t.rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| **pixel == [0x21, 0xc3, 0x65, 0xff])
                .count(),
            cw * ch,
            "the steady blank block cursor uses the dynamic OSC 10 color"
        );

        t.process(b"\x1b]10;#BADA55\x07");
        t.render();
        assert_eq!(t.frame_scratch.cursor_color, 0x00BA_DA55);
        assert_eq!(
            t.rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| **pixel == [0xba, 0xda, 0x55, 0xff])
                .count(),
            cw * ch,
            "changing OSC 10 changes wasm cursor pixels while the cursor slot stays dynamic"
        );
    }

    #[test]
    fn serialize_round_trips_visible_grid_into_a_fresh_engine() {
        // The serialize output is replayable ANSI: feeding it into a fresh engine
        // must reproduce the visible rows — proving it can replace xterm's
        // SerializeAddon for snapshot/restore without a shadow xterm buffer.
        let Some(mut a) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        a.process(b"\x1b[1;32mhello\x1b[0m world\r\nsecond line\r\n$ ");
        let snapshot = a.serialize(None);
        assert!(snapshot.contains("hello"), "serialize carries visible text");

        let Some(mut b) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        b.process(snapshot.as_bytes());
        for r in 0..3u16 {
            assert_eq!(
                a.row_text(r),
                b.row_text(r),
                "row {r} differs after serialize→replay"
            );
        }
    }

    #[test]
    fn search_meta_agrees_with_search_and_is_silent_on_empty_or_bad_input() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.process(b"alpha beta\r\nalpha gamma\r\nno match here\r\n");
        let matches = t.search("alpha", true, false);
        let meta = t.search_meta("alpha", true, false);
        assert_eq!(
            meta.match_count() as usize,
            matches.len() / 3,
            "meta counts the same result set the legacy export returns"
        );
        assert_eq!(meta.match_count(), 2);
        assert!(!meta.incomplete(), "small buffer: nothing truncated");
        // Empty query and invalid regex mirror the legacy export's silence.
        let empty = t.search_meta("", true, false);
        assert_eq!((empty.incomplete(), empty.match_count()), (false, 0));
        let bad = t.search_meta("(", true, true);
        assert_eq!((bad.incomplete(), bad.match_count()), (false, 0));
    }

    #[test]
    fn search_meta_surfaces_the_match_cap_truncation_the_legacy_export_drops() {
        // E9a: the legacy `search` export silently caps at MAX_SEARCH_MATCHES;
        // `search_meta` must carry the engine's `incomplete` signal for exactly
        // that result set.
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.set_scrollback_limit(3000);
        // 1,300 lines x 78 "a"s = 101,400 single-char matches > the 100k cap.
        let line = "a".repeat(78) + "\r\n";
        for _ in 0..1_300 {
            t.process(line.as_bytes());
        }
        let matches = t.search("a", true, false);
        let meta = t.search_meta("a", true, false);
        assert_eq!(
            matches.len() / 3,
            aterm_core::search::MAX_SEARCH_MATCHES,
            "legacy export returns the silently capped set"
        );
        assert_eq!(
            meta.match_count() as usize,
            aterm_core::search::MAX_SEARCH_MATCHES
        );
        assert!(
            meta.incomplete(),
            "the cap must surface out-of-band via search_meta"
        );
    }

    #[test]
    fn search_summary_carries_snippets_total_and_incomplete_superseding_meta() {
        // fed E-1: search_summary returns absolute-row matches WITH line-text
        // snippets + total + the incomplete flag in one call — the same result
        // set the legacy `search` export returns, enriched.
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.process(b"alpha beta\r\nalpha gamma\r\nno match here\r\n");
        let json = t.search_summary("alpha", true, false, 0).expect("summary");
        // total agrees with the legacy export's match count.
        let legacy = t.search("alpha", true, false).len() / 3;
        assert_eq!(legacy, 2);
        assert!(
            json.contains("\"total\":2"),
            "total counts all matches: {json}"
        );
        assert!(
            json.contains("\"incomplete\":false"),
            "nothing truncated: {json}"
        );
        // Each match carries its span coords AND a text snippet from its line.
        assert_eq!(
            json.matches("\"absRow\":").count(),
            2,
            "one absRow per match: {json}"
        );
        assert!(
            json.contains("\"snippet\":\"alpha beta\""),
            "snippet is the match line: {json}"
        );
        assert!(
            json.contains("\"snippet\":\"alpha gamma\""),
            "second snippet: {json}"
        );
        assert!(json.contains("\"col\":0"), "start_col present: {json}");
        // max_matches caps `matches` but not `total`.
        let capped = t.search_summary("alpha", true, false, 1).expect("capped");
        assert_eq!(
            capped.matches("\"absRow\":").count(),
            1,
            "cap limits matches: {capped}"
        );
        assert!(
            capped.contains("\"total\":2"),
            "cap does not change total: {capped}"
        );
        // Empty query and invalid regex mirror the legacy export's silence.
        let empty = t.search_summary("", true, false, 0).expect("empty");
        assert_eq!(empty, "{\"matches\":[],\"total\":0,\"incomplete\":false}");
        let bad = t.search_summary("(", true, true, 0).expect("bad regex");
        assert_eq!(bad, "{\"matches\":[],\"total\":0,\"incomplete\":false}");
    }

    #[test]
    fn search_index_release_reclaims_then_search_still_finds() {
        // fed E-1 search_index_release: after eviction the next search rebuilds
        // and still finds everything (real reclaim, never a false empty cache).
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        t.process(b"NEEDLE here\r\nfiller\r\nNEEDLE again\r\n");
        assert_eq!(t.search("NEEDLE", true, false).len() / 3, 2);
        t.search_index_release();
        assert_eq!(
            t.search("NEEDLE", true, false).len() / 3,
            2,
            "post-release search rebuilds and finds the same matches"
        );
    }

    #[test]
    fn row_range_json_batches_rows_matching_the_per_row_exports() {
        let Some(mut t) = AtermTerminal::new_from_system(6, 20, 16.0) else {
            return;
        };
        t.process(b"hello\r\nc\xe6\xbc\xa2x\r\n");
        // Batch export of the first 3 display rows in one call.
        let json = t.row_range_json(0, 3).expect("range available");
        // Exactly 3 records, text agrees with the per-row row_text fallback.
        assert_eq!(
            json.matches("\"text\":").count(),
            3,
            "one record per row: {json}"
        );
        assert!(
            json.contains("\"text\":\"hello\""),
            "narrow row text: {json}"
        );
        assert!(json.contains("\"text\":\"c漢x\""), "wide row text: {json}");
        // The wide row carries a per-column widths map with a '2' lead; the
        // all-narrow rows OMIT widths (host reuses its cached all-'1' string).
        assert!(
            json.contains("\"widths\":\"12"),
            "wide lead marked '2': {json}"
        );
        assert_eq!(
            json.matches("\"widths\":").count(),
            1,
            "only the wide row has widths: {json}"
        );
        // Text is byte-identical to the per-row export it replaces.
        for y in 0..3u16 {
            let rec_text = t.row_text(y).unwrap_or_default();
            assert!(
                json.contains(&json_string(&rec_text)),
                "row {y} text {rec_text:?} in {json}"
            );
        }
        // A range past the live grid is unavailable (resize-skew fallback).
        assert!(
            t.row_range_json(0, 999).is_none(),
            "over-long range → undefined"
        );
    }

    #[test]
    fn register_font_dedupes_identical_bytes() {
        let a = register_font(b"font-blob-a");
        let a2 = register_font(b"font-blob-a");
        let b = register_font(b"font-blob-b");
        assert_eq!(a, a2, "identical bytes share one handle");
        assert_ne!(a, b, "distinct bytes get distinct handles");
    }

    #[test]
    fn registered_engine_builds_and_seeds_from_handles() {
        // A plain (non-collection) monospace face readable on the host; skip-gate
        // when absent (CI containers without system fonts).
        let candidates = [
            "/System/Library/Fonts/Monaco.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        ];
        let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else {
            return;
        };
        let handle = register_font(&bytes);
        assert_eq!(
            register_font(&bytes),
            handle,
            "re-registration returns the same handle"
        );
        let mut t = AtermTerminal::new_registered(
            24,
            80,
            handle,
            16.0,
            0x00ff_ffff,
            0x0000_0000,
            0x00ff_ffff,
            0x0033_3333,
        )
        .expect("registered ctor builds");
        t.set_fallback_font_registered(handle)
            .expect("fallback via handle");
        t.add_fallback_font_registered(handle)
            .expect("add fallback via handle");
        t.set_symbol_font_registered(handle)
            .expect("symbol via handle");
        t.set_emoji_font_registered(handle)
            .expect("emoji via handle (any parseable face installs)");
        t.set_bold_font_registered(handle).expect("bold via handle");
        t.process(b"handle-built engine renders\r\n");
        assert!(
            t.row_text(0).unwrap_or_default().contains("handle-built"),
            "engine built from handles parses + stores text"
        );
        // An unknown handle is a catchable error, never a panic.
        assert!(t.set_fallback_font_registered(u32::MAX).is_err());
        assert!(AtermTerminal::new_registered(24, 80, u32::MAX, 16.0, 0, 0, 0, 0).is_err());
    }

    #[test]
    fn serialize_scrollback_is_history_only() {
        let Some(mut a) = AtermTerminal::new_from_system(4, 20, 16.0) else {
            return;
        };
        for i in 0..12 {
            a.process(format!("line {i}\r\n").as_bytes());
        }
        let hist = a.serialize_scrollback(None);
        assert!(hist.contains("line 0"), "scrollback keeps the oldest line");
    }

    /// Drive one OSC 133 command block (prompt → command → output → done),
    /// then the NEXT prompt's A so the block is sealed + archived — the state
    /// `last_command_output` reads.
    fn run_osc133_block(t: &mut AtermTerminal, cmd: &str, output_lines: &[&str], exit: i32) {
        t.process(b"\x1b]133;A\x07");
        t.process(format!("$ {cmd}").as_bytes());
        t.process(b"\x1b]133;B\x07\r\n");
        t.process(b"\x1b]133;C\x07");
        for line in output_lines {
            t.process(line.as_bytes());
            t.process(b"\r\n");
        }
        t.process(format!("\x1b]133;D;{exit}\x07").as_bytes());
        t.process(b"\x1b]133;A\x07");
    }

    #[test]
    fn last_command_output_returns_latest_block_output_json() {
        let Some(mut t) = AtermTerminal::new_from_system(10, 40, 16.0) else {
            return;
        };
        run_osc133_block(&mut t, "first", &["OLD-OUTPUT"], 0);
        run_osc133_block(&mut t, "second", &["NEW-LINE-1", "NEW-LINE-2"], 3);
        let json = t.last_command_output().expect("a completed block exists");
        assert!(
            json.contains(r#""status":"ok""#),
            "readable output reports ok: {json}"
        );
        assert!(
            json.contains("NEW-LINE-1") && json.contains("NEW-LINE-2"),
            "the LATEST block's full multi-line output is carried: {json}"
        );
        assert!(
            !json.contains("OLD-OUTPUT"),
            "an older block's output must not leak in: {json}"
        );
        assert!(
            json.contains(r#""exitCode":3"#),
            "the exit code rides along: {json}"
        );
    }

    #[test]
    fn last_command_output_reports_evicted_after_scrollback_cap() {
        let Some(mut t) = AtermTerminal::new_from_system(4, 40, 16.0) else {
            return;
        };
        t.set_scrollback_limit(6);
        run_osc133_block(&mut t, "doomed", &["EVICT-ME"], 0);
        // Flood plain output (no new marks) until the block's rows scroll past
        // the 6-line cap: the JSON must say so, never silently-shifted text.
        for i in 0..40 {
            t.process(format!("filler-{i}\r\n").as_bytes());
        }
        let json = t.last_command_output().expect("the block is still tracked");
        assert_eq!(
            json, r#"{"status":"evicted"}"#,
            "evicted rows surface the honest marker"
        );
    }

    #[test]
    fn authorize_hyperlink_scheme_gates_osc8_custom_links() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        // Unminted: the custom scheme is refused at parse time — no engine hit.
        t.process(b"\x1b]8;;orca://focus/w1\x07XX\x1b]8;;\x07\r\n");
        assert!(
            t.link_at(0, 0).is_none(),
            "an unminted custom scheme must not linkify"
        );
        // Never-allow / malformed schemes refuse at the mint (false, no state).
        assert!(!t.authorize_hyperlink_scheme("javascript"));
        assert!(!t.authorize_hyperlink_scheme("1orca"));
        // Minted: the same sequence linkifies with the OSC-8 kind.
        assert!(t.authorize_hyperlink_scheme("orca"));
        t.process(b"\x1b]8;;orca://focus/w1\x07YY\x1b]8;;\x07\r\n");
        let hit = t.link_at(1, 0).expect("a minted scheme must linkify");
        assert_eq!(hit.url(), "orca://focus/w1");
        assert_eq!(hit.kind(), 0, "engine OSC-8 hit, not a smart-match");
        // Revoke restores the default allowlist.
        t.revoke_hyperlink_scheme("orca");
        t.process(b"\x1b]8;;orca://focus/w2\x07ZZ\x1b]8;;\x07\r\n");
        assert!(
            t.link_at(2, 0).is_none(),
            "a revoked scheme must stop linkifying"
        );
    }

    #[test]
    fn last_command_output_is_none_without_shell_integration() {
        let Some(mut t) = AtermTerminal::new_from_system(10, 40, 16.0) else {
            return;
        };
        t.process(b"plain output, no OSC 133 anywhere\r\n$ ");
        assert!(
            t.last_command_output().is_none(),
            "no blocks → undefined, the menu item stays hidden"
        );
    }

    #[test]
    fn serialize_replay_lands_history_in_scrollback_not_under_the_viewport_paint() {
        // Regression: the serialize layout prints history lines onto the visible
        // screen, then paints the viewport with absolute CUP + erase-line. Without
        // the scroll-off epilogue the last min(take, rows) history lines were
        // ERASED by that paint — a replay lost a viewport-sized chunk of history
        // (ALL of it when history < rows, e.g. a prompt + marker snapshot).
        let Some(mut a) = AtermTerminal::new_from_system(6, 40, 16.0) else {
            return;
        };
        // 3 history lines (< rows=6) + a distinct visible viewport.
        for i in 0..9 {
            a.process(format!("hist {i}\r\n").as_bytes());
        }
        a.process(b"\x1b[2J\x1b[Hviewport line");
        let snapshot = a.serialize(None);

        let Some(mut b) = AtermTerminal::new_from_system(6, 40, 16.0) else {
            return;
        };
        b.process(snapshot.as_bytes());
        // The viewport replayed intact...
        assert_eq!(
            a.row_text(0),
            b.row_text(0),
            "viewport row differs after replay"
        );
        // ...AND the source's history lives in the replay target's SCROLLBACK
        // (byte-identical: nothing eaten by the viewport paint, no injected blanks).
        let source_hist = a.serialize_scrollback(None);
        assert!(
            source_hist.contains("hist 0"),
            "test setup: source must hold history"
        );
        assert_eq!(
            b.serialize_scrollback(None),
            source_hist,
            "replayed scrollback must match the source's history exactly"
        );
    }

    /// JS hands the binding raw u16 rows/cols; the grid clamps them to 1..=4096,
    /// but the binding used to store the RAW args and feed them to `cell_frame`,
    /// sizing the framebuffer from an unclamped 65535×65535 → unbounded alloc /
    /// wasm32 u32 overflow → OOB. Construction and `resize` must both re-sync
    /// `self.rows`/`self.cols` to the CLAMPED grid dims.
    #[test]
    fn oversized_dims_are_clamped_to_the_grid_bound() {
        let Some(mut t) = AtermTerminal::new_from_system(u16::MAX, u16::MAX, 16.0) else {
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
    }

    #[test]
    fn renders_text_to_a_nonempty_rgba_framebuffer() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            // No system font available in this environment; skip rather than fail.
            eprintln!("no system font; skipping render test");
            return;
        };
        t.process(b"\x1b[1;32mhello\x1b[0m world\r\nsecond line");
        t.render();
        assert!(t.width() > 0 && t.height() > 0, "frame has dimensions");
        let rgba = t.rgba();
        assert_eq!(rgba.len(), t.width() * t.height() * 4, "RGBA8 buffer size");
        // Some pixel must differ from the top-left (background) pixel — i.e. glyphs
        // were actually rasterized, not a blank frame.
        let bg = &rgba[0..4];
        assert!(
            rgba.as_chunks::<4>().0.iter().any(|px| px != bg),
            "rendered glyphs should produce non-background pixels"
        );
    }

    #[test]
    fn incremental_render_is_byte_identical_to_a_single_full_render() {
        // The persistent WindowCpu reuses unchanged rows across frames; the output
        // must still equal a fresh full render of the same final content — proving
        // the damage tracking is correct, not just faster.
        let Some(mut warm) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping damage-parity test");
            return;
        };
        let Some(mut fresh) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        let steps: &[&[u8]] = &[b"$ ", b"ls -la\r\n", b"file one\r\n", b"\x1b[1;1HX"];
        for s in steps {
            warm.process(s);
            warm.render(); // incremental every step → warm cache + dirty-row reuse
            fresh.process(s);
        }
        fresh.render(); // one full render of the final state (cold cache)
        assert_eq!(warm.width(), fresh.width());
        assert_eq!(warm.height(), fresh.height());
        assert_eq!(
            warm.rgba(),
            fresh.rgba(),
            "incremental damage-tracked render must equal a fresh full render"
        );
    }

    #[test]
    fn set_theme_repaints_an_idle_pane_to_the_new_background() {
        // With the persistent cache, an appearance-only change (no cell moved) would
        // be skipped by the row-diff — set_theme must force a full repaint so the
        // background actually re-themes on an otherwise-idle frame (CPU finding #7).
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping set_theme repaint test");
            return;
        };
        // Clear, then park the cursor in the far corner so the sampled centre cell
        // is pure background (no glyph, no cursor).
        t.process(b"\x1b[2J\x1b[24;80H");
        t.render();
        let w = t.width();
        let centre = ((t.height() / 2) * w + w / 2) * 4;
        let before = t.rgba()[centre..centre + 3].to_vec();

        let new_bg = 0x0012_3456u32; // distinctive, unlike the default theme bg
        t.set_theme(0x00ff_ffff, new_bg, 0x0050_fa7b, 0x0026_4f78);
        t.render(); // idle frame: no content changed, only the theme
        let after = t.rgba();
        let want = [(new_bg >> 16) as u8, (new_bg >> 8) as u8, new_bg as u8];
        assert_eq!(
            &after[centre..centre + 3],
            &want,
            "an idle pane must repaint to the new theme bg after set_theme"
        );
        assert_ne!(
            before.as_slice(),
            &want,
            "test is meaningful: bg actually changed"
        );
    }

    #[test]
    fn scrolls_into_scrollback_and_extracts_a_selection() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping scroll/select test");
            return;
        };
        for i in 0..200 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(t.display_offset(), 0, "live output stays at the bottom");
        t.scroll_lines(40);
        assert_eq!(t.display_offset(), 40, "scrolling up reveals older history");
        t.scroll_to_bottom();
        assert_eq!(t.display_offset(), 0, "scroll_to_bottom snaps back to live");
        t.selection_start(0, 0);
        t.selection_extend(1, 4);
        t.selection_finish();
        let selected = t.selection_text().expect("a drag selects text");
        assert!(!selected.is_empty(), "selection should not be empty");
    }

    #[test]
    fn double_click_selects_whole_word_triple_click_whole_line() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping word/line select test");
            return;
        };
        t.process(b"hello world");
        // Double-click anywhere in "hello" (cols 0..5) selects the WHOLE word,
        // not just the clicked cell — the expand_semantic fix.
        let word = t.selection_word(0, 2).expect("word selection");
        assert_eq!(word, "hello", "double-click selects the full word");
        // Triple-click selects the whole line (trailing blanks trimmed).
        let line = t.selection_line(0, 2).expect("line selection");
        assert_eq!(line, "hello world", "triple-click selects the full line");
    }

    /// Retention shrink must invalidate the search index like any content
    /// change: a shrink-then-search may not return absolute rows the engine
    /// just evicted (the introspection harness caught `search` handing out
    /// rows `line` already reported evicted, until the next write re-synced
    /// the cache).
    #[test]
    fn retention_shrink_search_returns_no_evicted_rows() {
        let Some(mut t) = AtermTerminal::new_from_system(5, 40, 16.0) else {
            eprintln!("no system font; skipping shrink-search test");
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

    #[test]
    fn set_scrollback_limit_governs_ring_retention() {
        let Some(mut t) = AtermTerminal::new_from_system(5, 40, 16.0) else {
            eprintln!("no system font; skipping scrollback-limit test");
            return;
        };
        // Shrink below the 10k construction ring: retention follows the limit.
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
        assert_eq!(
            t.row_text(0).as_deref(),
            Some("line-296"),
            "the oldest retained line sits 200 above the live top"
        );

        // Grow PAST the old fixed 10k ring: a bigger limit actually retains more.
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

        // 0 = unlimited; a later shrink truncates and re-clamps the viewport.
        t.scroll_to_bottom();
        t.set_scrollback_limit(0);
        t.process(b"tail\r\n");
        t.scroll_to_top();
        t.set_scrollback_limit(50);
        assert_eq!(
            t.display_offset(),
            50,
            "shrink re-clamps a scrolled viewport"
        );
    }

    #[test]
    fn search_stays_fresh_and_complete_across_a_height_only_resize() {
        let Some(mut t) = AtermTerminal::new_from_system(10, 40, 16.0) else {
            eprintln!("no system font; skipping height-resize search test");
            return;
        };
        for i in 0..30 {
            t.process(format!("needle-{i}\r\n").as_bytes());
        }
        assert_eq!(t.search("needle-", true, false).len() / 3, 30);

        // Height-only resize (cols unchanged), reflow pump drained like the host.
        t.resize(5, 40);
        while t.pump_reflow() {}

        // Pre-fix the rows-only resize dropped ALL ring history and
        // renumbered the survivors, so this search returned only the 5
        // visible rows at shifted absolute lines — "stale"/wrong results
        // until new output arrived.
        let hits = t.search("needle-", true, false);
        assert_eq!(
            hits.len() / 3,
            30,
            "history stays searchable after the resize"
        );
        // The oldest retained content is still reachable and identical.
        t.scroll_to_top();
        assert_eq!(t.display_offset(), 26, "retention grew by the demoted rows");
        assert_eq!(t.row_text(0).as_deref(), Some("needle-0"));

        // Staleness identity: a fresh engine given the same writes + resize
        // reports byte-identical matches.
        let Some(mut fresh) = AtermTerminal::new_from_system(10, 40, 16.0) else {
            return;
        };
        for i in 0..30 {
            fresh.process(format!("needle-{i}\r\n").as_bytes());
        }
        fresh.resize(5, 40);
        while fresh.pump_reflow() {}
        assert_eq!(
            hits,
            fresh.search("needle-", true, false),
            "post-resize results equal a from-scratch engine (no stale cache)"
        );
    }

    #[test]
    fn row_text_straddles_the_history_live_boundary_correctly() {
        let Some(mut t) = AtermTerminal::new_from_system(5, 40, 16.0) else {
            eprintln!("no system font; skipping straddle test");
            return;
        };
        for i in 0..30 {
            t.process(format!("line{i}\r\n").as_bytes());
        }
        // Live bottom: line26..line29 + the blank cursor row.
        assert_eq!(t.row_text(0).as_deref(), Some("line26"));

        // Scroll up 2: the viewport straddles history (rows 0-1) and live
        // rows (2-4). Pre-fix the live rows re-applied the offset and
        // repeated the history rows (line24/line25/line24/line25/line26).
        t.scroll_lines(2);
        for (r, want) in ["line24", "line25", "line26", "line27", "line28"]
            .iter()
            .enumerate()
        {
            assert_eq!(
                t.row_text(r as u16).as_deref(),
                Some(*want),
                "display row {r} while straddling"
            );
        }

        // Selection is display-anchored at the same boundary: triple-click on
        // display row 3 must copy the row the user sees there.
        assert_eq!(t.selection_line(3, 0).as_deref(), Some("line27"));

        // Back at the bottom, reads are unchanged (identity at offset 0).
        t.scroll_to_bottom();
        assert_eq!(t.row_text(0).as_deref(), Some("line26"));
        assert_eq!(t.row_text(3).as_deref(), Some("line29"));
    }

    #[test]
    fn scrolled_cell_reads_are_display_relative_like_row_text() {
        // A host that rebuilds a row PER-CELL (orc's buffer facade walks any
        // non-ASCII row through cell_text + cell_is_wide) must see the same
        // display-relative rows row_text serves. Regression: the v0.49
        // live-frame retarget of the shared cell-grapheme helper made every
        // scrolled-back per-cell read return the LIVE screen instead of the
        // scrolled row — orc's restored-scrollback walk (a box-drawing table,
        // 100% non-ASCII rows) could no longer find any history row.
        let Some(mut t) = AtermTerminal::new_from_system(5, 20, 16.0) else {
            eprintln!("no system font; skipping display-relative cell-read test");
            return;
        };
        for i in 0..30 {
            // Box-drawing edges force the host's per-cell path; a wide emoji
            // exercises complex-cluster pairing for materialized history rows.
            t.process(format!("│🦀line{i}│\r\n").as_bytes());
        }
        let per_cell_row = |t: &AtermTerminal, row: u16| -> String {
            let mut out = String::new();
            let mut col = 0u16;
            while col < 20 {
                let wide = t.cell_is_wide(row, col).unwrap_or(false);
                let text = t.cell_text(row, col);
                out.push_str(if text.is_empty() { " " } else { &text });
                col += if wide { 2 } else { 1 };
            }
            out.trim_end().to_string()
        };
        let row_text_trimmed = |t: &AtermTerminal, row: u16| {
            t.row_text(row).unwrap_or_default().trim_end().to_string()
        };

        // Identity at the live bottom (display_offset == 0).
        for r in 0..5u16 {
            assert_eq!(
                per_cell_row(&t, r),
                row_text_trimmed(&t, r),
                "per-cell rebuild differs from row_text at the live bottom, row {r}"
            );
        }

        // Top of retention: display row 0 is the OLDEST retained line.
        t.scroll_to_top();
        assert_eq!(
            per_cell_row(&t, 0),
            "│🦀line0│",
            "oldest history line, rebuilt per-cell while scrolled to top"
        );
        for r in 0..5u16 {
            assert_eq!(
                per_cell_row(&t, r),
                row_text_trimmed(&t, r),
                "per-cell rebuild differs from row_text at the top, row {r}"
            );
        }

        // Straddling the history/live boundary keeps both arms aligned.
        t.scroll_to_bottom();
        t.scroll_lines(2);
        for r in 0..5u16 {
            assert_eq!(
                per_cell_row(&t, r),
                row_text_trimmed(&t, r),
                "per-cell rebuild differs from row_text while straddling, row {r}"
            );
        }
        t.scroll_to_bottom();
    }

    #[test]
    fn per_cell_row_cache_invalidates_on_scroll_and_write() {
        // cell_text/cell_is_wide serve from a single-slot per-row cache (the
        // O(cols²)->O(cols) fix for scrolled-back per-cell walks). The cache is
        // keyed by (content_gen, display_offset, row); a scroll or a write must
        // refresh it, never serving the stale row a prior read cached.
        let Some(mut t) = AtermTerminal::new_from_system(5, 40, 16.0) else {
            eprintln!("no system font; skipping per-cell cache test");
            return;
        };
        for i in 0..30 {
            t.process(format!("row{i}\r\n").as_bytes());
        }
        let per_cell = |t: &AtermTerminal, row: u16| -> String {
            (0..8)
                .map(|c| t.cell_text(row, c))
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        // Prime the cache for display row 0 at the live bottom, and confirm the
        // per-cell reads agree with row_text (which does not use the cache).
        let bottom = per_cell(&t, 0);
        assert_eq!(bottom, t.row_text(0).unwrap().trim_end());
        assert!(bottom.starts_with("row"), "bottom row0 was {bottom:?}");

        // SCROLL changes display_offset: display row 0 is now the OLDEST line,
        // so the cache must refresh instead of serving the primed bottom row.
        t.scroll_to_top();
        assert_eq!(
            per_cell(&t, 0),
            "row0",
            "scroll must refresh the cached row"
        );

        // WRITE bumps content_gen WITHOUT changing display_offset (re-prime at
        // the bottom first, offset stays 0 across the write): display row 0 must
        // reflect the post-write content, isolating content_gen invalidation.
        t.scroll_to_bottom();
        let _prime = per_cell(&t, 0);
        t.process(b"\x1b[2J\x1b[HZEBRA\r\n");
        assert_eq!(
            per_cell(&t, 0),
            "ZEBRA",
            "a write must refresh the cached row (content_gen changed)"
        );
    }

    #[test]
    fn scrolled_wrapped_row_len_and_wrap_flag_are_tier_aware() {
        // P1 regression: after a width-shrink reflow overflows wrapped rows
        // into scrollback, a
        // scrolled-back HISTORY row returns correct TEXT (tier-aware via
        // visible_row_view) but row_len/row_is_wrapped resolved through Grid::row
        // — which is None past the ring base — so orc's wrapped-line stitching
        // saw isWrapped=false / len=cols for every scrolled wrap continuation.
        let Some(mut t) = AtermTerminal::new_from_system(3, 40, 16.0) else {
            eprintln!("no system font; skipping tier-aware wrap-metadata test");
            return;
        };
        // Three 30-char lines fill the 3 visible rows at 40 cols (no wrap yet);
        // the last has no newline so the cursor rests on the bottom row.
        let a = "A".repeat(30);
        let b = "B".repeat(30);
        let c = "C".repeat(30);
        t.process(format!("{a}\r\n").as_bytes());
        t.process(format!("{b}\r\n").as_bytes());
        t.process(c.as_bytes());

        // Shrink width to 20: each 30-char line rewraps to a 20-col head + a
        // 10-col WRAPPED continuation, overflowing the 3-row window; the top rows
        // spill to scrollback. A history this small rewraps inline (pump is a
        // no-op).
        t.resize(3, 20);
        while t.pump_reflow() {}

        // The three oldest content rows are HISTORY rows (past the ring base).
        t.scroll_to_top();
        // Text is already tier-aware (the sibling that always worked).
        assert_eq!(t.row_text(0).as_deref(), Some("A".repeat(20).as_str()));
        assert_eq!(t.row_text(1).as_deref(), Some("A".repeat(10).as_str()));

        // The head row: full-width, not a wrap continuation.
        assert_eq!(t.row_len(0), Some(20), "history head-row length");
        assert_eq!(t.row_is_wrapped(0), Some(false), "history head not wrapped");
        // The continuation row: pre-fix these were None because they resolved
        // through Grid::row past the ring base.
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
        // Out-of-range still yields None (the row past the grid).
        assert_eq!(t.row_len(99), None);
        assert_eq!(t.row_is_wrapped(99), None);
    }

    #[test]
    fn display_row_cache_distinguishes_main_and_alt_screen() {
        // P4 regression: the single-slot display_row_cache backing cell_text /
        // cell_is_wide keyed (content_gen, display_offset, row) with NO
        // alt-screen bit. The main and alt grids keep INDEPENDENT content_gen
        // counters, so a main<->alt swap landing on a coinciding
        // content_gen+offset+row served one buffer's cell for the other.
        let Some(mut t) = AtermTerminal::new_from_system(5, 20, 16.0) else {
            eprintln!("no system font; skipping alt-screen cache-key test");
            return;
        };
        // MAIN: push content_gen well above a freshly-entered alt buffer's, then
        // park a distinct glyph at (0,0) and prime the per-row cache by reading it.
        for _ in 0..30 {
            t.process(b"\r\n");
        }
        t.process(b"\x1b[HM");
        let main_gen = t.term.grid().content_gen();
        assert_eq!(t.cell_text(0, 0), "M", "primed main cell at (0,0)");

        // Enter the alternate screen: a fresh, low-content_gen independent grid.
        t.process(b"\x1b[?1049h");
        t.process(b"\x1b[HA");
        assert!(
            t.term.grid().content_gen() <= main_gen,
            "alt buffer starts below main_gen so we can drive it up to collide"
        );
        // Drive alt's INDEPENDENT content_gen to EXACTLY main's cached value,
        // writing only to row 1 so (0,0) keeps its "A" — manufacturing the
        // (content_gen, display_offset, row) collision a single-slot cache
        // without an alt-screen key bit cannot tell apart.
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

        // Read alt (0,0). Pre-fix the un-keyed cache serves the primed MAIN "M";
        // the alt bit in the key forces a refresh to the real alt cell "A".
        assert_eq!(
            t.cell_text(0, 0),
            "A",
            "alt-screen cell must not be served from the main-screen cache at a coinciding key"
        );
    }

    #[test]
    fn search_finds_a_token_in_scrollback_and_scrolls_it_into_view() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping search test");
            return;
        };
        // Push a unique token far into scrollback, then bury it under filler.
        t.process(b"UNIQUE_SEARCH_TOKEN here\r\n");
        for i in 0..200 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }
        let hits = t.search("UNIQUE_SEARCH_TOKEN", true, false);
        assert_eq!(
            hits.len(),
            3,
            "exactly one match → one [line,col,len] triple"
        );
        let (line, col, len) = (hits[0], hits[1], hits[2]);
        assert_eq!(col, 0, "token starts at column 0");
        assert_eq!(len, "UNIQUE_SEARCH_TOKEN".len() as u32, "match length");
        // The match is in scrollback, so it is not visible at the live bottom.
        let origin = t.search_display_origin();
        assert!(
            line < origin,
            "token line is above the live viewport origin"
        );
        // Scrolling it into view must move the viewport off the bottom and land
        // the match within the visible rows.
        assert_eq!(t.display_offset(), 0, "starts at the live bottom");
        t.scroll_search_line_into_view(line);
        assert!(t.display_offset() > 0, "viewport scrolled up to the match");
        let display_row = (line as i64) - (origin as i64) + (t.display_offset() as i64);
        assert!(
            (0..24).contains(&display_row),
            "match landed on a visible row, got {display_row}"
        );
        // A case-sensitive miss and an empty query both yield nothing.
        assert!(t.search("unique_search_token", true, false).is_empty());
        assert!(t.search("", false, false).is_empty());
        // Regex search: a pattern matches the token; an invalid pattern is Err →
        // empty (so a half-typed regex highlights nothing rather than throwing).
        assert_eq!(
            t.search("UNIQUE_[A-Z_]+TOKEN", true, true).len(),
            3,
            "regex matches"
        );
        assert!(
            t.search("UNIQUE_[", true, true).is_empty(),
            "invalid regex → empty"
        );
    }

    #[test]
    fn tracks_application_cursor_mode_via_decckm() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping app-cursor-mode test");
            return;
        };
        assert!(
            !t.is_app_cursor_mode(),
            "DECCKM defaults off (cursor → CSI)"
        );
        // CSI ? 1 h sets DECCKM (application cursor keys); CSI ? 1 l resets it.
        t.process(b"\x1b[?1h");
        assert!(
            t.is_app_cursor_mode(),
            "DECCKM set → application cursor keys"
        );
        t.process(b"\x1b[?1l");
        assert!(!t.is_app_cursor_mode(), "DECCKM reset → normal cursor keys");
    }

    #[test]
    fn encode_key_follows_the_live_keyboard_mode() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping encode_key test");
            return;
        };
        // Plain arrow: legacy CSI A.
        assert_eq!(
            t.encode_key("ArrowUp", 0, 0, None).as_deref(),
            Some(&b"\x1b[A"[..])
        );
        // Modified arrows: the xterm CSI 1;{mod} form the old TS encoder dropped.
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
        // DECCKM set → SS3 for the unmodified arrow.
        t.process(b"\x1b[?1h");
        assert_eq!(
            t.encode_key("ArrowUp", 0, 0, None).as_deref(),
            Some(&b"\x1bOA"[..]),
            "DECCKM arrow must be SS3"
        );
        t.process(b"\x1b[?1l");
        // A release without the Kitty protocol encodes to nothing.
        assert!(t.encode_key("ArrowUp", 0, 2, None).is_none());
        // Modifier-only / IME / unidentified DOM keys are never guessed.
        assert!(t.encode_key("Shift", 0, 0, None).is_none());
        assert!(t.encode_key("Dead", 0, 0, None).is_none());
        assert!(t.encode_key("Unidentified", 0, 0, None).is_none());
        // An out-of-range event_type is refused, not guessed.
        assert!(t.encode_key("ArrowUp", 0, 3, None).is_none());
    }

    #[test]
    fn encode_key_shift_enter_speaks_kitty_after_negotiation() {
        // Mirrors keyboard_mode.rs's shift_enter_e2e tests through the wasm-level
        // API: drive the exact Kitty push an app performs, then confirm the bytes.
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping kitty shift-enter test");
            return;
        };
        // No protocol: plain Enter is CR; Shift+Enter is aterm's imposed LF.
        assert_eq!(
            t.encode_key("Enter", 0, 0, None).as_deref(),
            Some(&[0x0d][..])
        );
        assert_eq!(
            t.encode_key("Enter", 1, 0, None).as_deref(),
            Some(&[0x0a][..])
        );
        // The Kitty query must be answered so apps detect support.
        t.process(b"\x1b[?u");
        assert_eq!(t.take_response().unwrap_or_default(), b"\x1b[?0u");
        // Push disambiguate (what a kitty-aware app sends to turn it on).
        t.process(b"\x1b[>1u");
        assert_eq!(
            t.encode_key("Enter", 1, 0, None).as_deref(),
            Some(&b"\x1b[13;2u"[..]),
            "Shift+Enter under pushed kitty disambiguate must be CSI-u"
        );
        // Plain Enter stays legacy CR under disambiguate.
        assert_eq!(
            t.encode_key("Enter", 0, 0, None).as_deref(),
            Some(&[0x0d][..])
        );
    }

    #[test]
    fn encode_key_with_mode_matches_the_instance_encoder() {
        // Stateless with mode_bits = 0: plain legacy CSI A, no instance needed
        // (this is the worker-hosted main-thread path before any snapshot).
        assert_eq!(
            encode_key_with_mode("ArrowUp", 0, 0, None, 0).as_deref(),
            Some(&b"\x1b[A"[..])
        );
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping encode_key_with_mode test");
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

    #[test]
    fn reports_alternate_scroll_via_decset_1007() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping alternate-scroll test");
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

    /// The host word-separator setting reshapes double-click words end-to-end
    /// (xterm.js `wordSeparators` semantics), and clearing it restores the
    /// engine's default class-based word logic exactly.
    #[test]
    fn set_word_separators_reshapes_double_click_words() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping word-separator test");
            return;
        };
        t.process(b"foo-bar baz");
        // Default: '-' breaks, double-click on 'o' selects "foo".
        assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo"));
        // Space-only separators: the hyphenated token is one word.
        t.set_word_separators(Some(" ".to_string()));
        assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo-bar"));
        // Clearing restores the default exactly.
        t.set_word_separators(None);
        assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo"));
    }

    #[test]
    fn reports_mouse_tracking_and_encodes_sgr_reports() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping mouse-tracking test");
            return;
        };
        // No tracking by default → encoders return None, motion not wanted.
        assert!(!t.is_mouse_tracking(), "mouse tracking defaults off");
        assert!(t.encode_mouse_press(0, 0, 0, 0).is_none(), "no report off");
        assert!(!t.mouse_wants_motion(), "no motion wanted off");
        // DECSET 1000 (normal tracking) + 1006 (SGR encoding).
        t.process(b"\x1b[?1000h\x1b[?1006h");
        assert!(t.is_mouse_tracking(), "1000 enables tracking");
        assert!(!t.mouse_wants_motion(), "1000 does not report motion");
        // Left press at col 4 / row 2 → SGR \e[<0;5;3M (encoders +1 to coords).
        let press = t.encode_mouse_press(4, 2, 0, 0).expect("press encoded");
        assert_eq!(press, b"\x1b[<0;5;3M", "SGR press report");
        let release = t.encode_mouse_release(4, 2, 0, 0).expect("release encoded");
        assert_eq!(release, b"\x1b[<0;5;3m", "SGR release uses lowercase m");
        // Normal mode (1000) reports no motion.
        assert!(
            t.encode_mouse_motion(0, 0, 0, 0).is_none(),
            "1000 no motion"
        );
        // Switch to 1002 (button-event) → motion while a button is held.
        t.process(b"\x1b[?1002h");
        assert!(t.mouse_wants_motion(), "1002 reports drag motion");
        assert!(!t.mouse_wants_any_motion(), "1002 is not any-motion");
        // 1003 (any-event) reports motion with no button held.
        t.process(b"\x1b[?1003h");
        assert!(t.mouse_wants_any_motion(), "1003 reports any motion");
        // Wheel-up → button 64 → SGR \e[<64;...M.
        let wheel = t.encode_mouse_wheel(4, 2, true, 0).expect("wheel encoded");
        assert_eq!(wheel, b"\x1b[<64;5;3M", "SGR wheel-up report");
        // DECRST 1003/1002/1000 clears tracking entirely.
        t.process(b"\x1b[?1003l\x1b[?1002l\x1b[?1000l");
        assert!(
            !t.is_mouse_tracking(),
            "clearing all modes disables tracking"
        );
    }

    #[test]
    fn reports_focus_event_mode_via_decset_1004() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping focus-mode test");
            return;
        };
        assert!(!t.is_focus_event_mode(), "focus reporting defaults off");
        t.process(b"\x1b[?1004h");
        assert!(t.is_focus_event_mode(), "1004 enables focus reporting");
        t.process(b"\x1b[?1004l");
        assert!(
            !t.is_focus_event_mode(),
            "1004 reset disables focus reporting"
        );
    }

    #[test]
    fn tracks_cursor_style_via_decscusr() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping cursor-style test");
            return;
        };
        // DECSCUSR is CSI Ps SP q; Ps=5 → BlinkingBar (discriminant 5), Ps=2 →
        // SteadyBlock (2). The engine paints the shape; we just read it back.
        t.process(b"\x1b[5 q");
        assert_eq!(t.cursor_style(), 5, "DECSCUSR 5 → BlinkingBar");
        t.process(b"\x1b[2 q");
        assert_eq!(t.cursor_style(), 2, "DECSCUSR 2 → SteadyBlock");
    }

    #[test]
    fn detects_a_url_link_under_the_cursor() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping link detection test");
            return;
        };
        t.process(b"https://example.com/foo bar");
        // Column 5 is inside "https://example.com/foo".
        let hit = t.link_at(0, 5).expect("a URL under the cursor is a link");
        assert!(
            hit.kind() == 0 || hit.kind() == 1,
            "expected osc8 or url kind, got {}",
            hit.kind()
        );
        assert!(
            hit.url().contains("example.com"),
            "url should contain the host, got {:?}",
            hit.url()
        );
    }

    // --- Font-independent tests (operate on the engine + helpers directly) ---

    #[test]
    fn selection_range_reports_display_coords_while_scrolled() {
        // 5 rows, 20 cols; push lines so some scroll into history.
        let mut term = Terminal::new(5, 20);
        for i in 0..20 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        term.scroll_display(4);
        let offset = term.grid().display_offset() as i32;
        assert_eq!(offset, 4);

        // Select display rows 0..=1 — the host passes TERMINAL-relative rows
        // (display_row - offset), exactly as the fixed wasm entry points do.
        {
            let sel = term.text_selection_mut();
            sel.start_selection(0 - offset, 0, SelectionSide::Left, SelectionType::Lines);
            sel.update_selection(1 - offset, 19, SelectionSide::Right);
            sel.complete_selection();
        }

        // selection_range maps back to DISPLAY coords 0..=1.
        let r = selection_range_for(&term, 5, 20).expect("selection in viewport");
        assert_eq!((r.start_y(), r.end_y()), (0, 1), "display rows 0..=1");

        // And it agrees with selection_text (the scrollback lines, not live rows).
        let copied = term.selection_to_string().expect("text");
        let want0 = term.display_row_text(0).unwrap();
        let want1 = term.display_row_text(1).unwrap();
        assert_eq!(copied, format!("{want0}\n{want1}"));
    }

    #[test]
    fn selection_range_is_none_when_fully_in_scrollback() {
        let mut term = Terminal::new(5, 20);
        for i in 0..20 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        // Select a region in scrollback (terminal rows -8..=-7), then view the
        // live bottom (offset 0) so the selection is entirely above the viewport.
        {
            let sel = term.text_selection_mut();
            sel.start_selection(-8, 0, SelectionSide::Left, SelectionType::Lines);
            sel.update_selection(-7, 19, SelectionSide::Right);
            sel.complete_selection();
        }
        assert_eq!(term.grid().display_offset(), 0);
        assert!(
            selection_range_for(&term, 5, 20).is_none(),
            "selection fully off-screen yields None"
        );
    }

    /// Effects default OFF must leave the render byte-identical: an instance
    /// that never touches the effects API and one that toggles effects on and
    /// back off must produce the same pixels for the same content (the
    /// empty-overlay render path is proven byte-identical by the aterm-render
    /// suite; this pins the BINDING's default-off wiring to that path).
    #[test]
    fn effects_off_is_byte_identical() {
        let Some(mut plain) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping effects-off identity test");
            return;
        };
        let Some(mut toggled) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        let content = b"a curious cat watched the build fail: fuck\r\n$ ";
        plain.process(content);
        plain.render();

        toggled.process(content);
        // Exercise the whole surface, then land back on OFF.
        toggled.set_sparkle_words_enabled(true);
        toggled.set_cursor_glow(true, "phaser", None, None, 260, 24, 0.7, 0.6, true);
        toggled.set_cursor_trail(true, 260, 24, None);
        toggled.advance_effects(50.0);
        toggled.render();
        // A FIRE burn too: the spliced fire/halo/under/char streams must all
        // vanish with the toggle. Type-then-erase ignites a REAL burn (typed
        // glyphs ignite; navigation does not) while leaving the cell bytes and
        // the cursor position identical to the plain engine.
        toggled.set_cursor_glow(true, "ember", None, None, 400, 64, 1.0, 2.0, true);
        toggled.process(b"z\x1b[D \x1b[D");
        toggled.advance_effects(50.0);
        toggled.render();
        assert_ne!(
            plain.rgba(),
            toggled.rgba(),
            "a live burn must visibly change the frame (non-vacuous toggle)"
        );
        toggled.set_sparkle_words_enabled(false);
        toggled.set_cursor_glow(false, "lumen", None, None, 260, 24, 0.7, 0.6, true);
        toggled.set_cursor_trail(false, 260, 24, None);
        toggled.advance_effects(50.0);
        toggled.render();

        assert_eq!(plain.width(), toggled.width());
        assert_eq!(
            plain.rgba(),
            toggled.rgba(),
            "effects toggled on->off must restore the byte-identical frame"
        );
    }

    /// `set_chrome` grows the composed frame by the pad/head chrome (the host
    /// re-reads width/height and offsets its canvas), and 0/0 restores the
    /// exact-fit frame byte-identically (the identity law through the wasm
    /// binding).
    #[test]
    fn set_chrome_pads_the_frame_and_zero_restores_identity() {
        let Some(mut plain) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        let Some(mut chromed) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        let content = b"chrome test\r\n$ ";
        plain.process(content);
        plain.render();
        let (w, h) = (plain.width(), plain.height());

        chromed.process(content);
        chromed.set_chrome(12, 30);
        chromed.render();
        assert_eq!(chromed.width(), w + 24, "frame gains 2*pad in width");
        assert_eq!(
            chromed.height(),
            h + 24 + 30,
            "frame gains 2*pad + head in height"
        );
        // The grid still holds the same cells; only the frame grew.
        assert_eq!(chromed.row_text(0), plain.row_text(0));

        chromed.set_chrome(0, 0);
        chromed.render();
        assert_eq!(chromed.width(), w);
        assert_eq!(chromed.height(), h);
        assert_eq!(
            plain.rgba(),
            chromed.rgba(),
            "0/0 chrome must restore the byte-identical exact-fit frame"
        );
    }

    /// SPILL BAND through the REAL pipeline: an EMBERFORGE burn on the top
    /// row (the pipeline now splices the glow engine's fire/halo/under
    /// streams, and the flame field rises above the grid — the effects box
    /// relaxes upward by exactly `head` under chrome) must light the TOP
    /// strip and surface band bytes whose source-over composite onto the
    /// theme bg equals the rendered frame's band region byte-for-byte (the
    /// seam-continuity law), size the buffer exactly to the four strips, keep
    /// the pointer stable across animation frames, and stay identity (len 0,
    /// rev still) at 0/0 chrome.
    #[test]
    fn spill_exports_track_band_content_through_the_real_pipeline() {
        let Some(mut t) = AtermTerminal::new_from_system(12, 40, 16.0) else {
            return;
        };
        t.render();
        assert_eq!(t.spill_len(), 0, "0/0 chrome: identity — no band, no bytes");
        assert_eq!(t.spill_rev(), 0);
        assert_eq!(t.spill_rect_count(), 0);

        let (pad, head) = (12usize, 30usize);
        t.set_chrome(pad as u16, head as u16);
        // The glow observes cursor motion FRAME-TO-FRAME, so keystrokes
        // interleave with renders; a row-0 burn then licks into the head band.
        t.set_cursor_glow(true, "ember", None, None, 400, 64, 1.0, 2.0, true);
        for ch in [b"a", b"b", b"c", b"d"] {
            t.process(ch);
            t.advance_effects(30.0);
            t.render();
        }
        for _ in 0..40 {
            t.advance_effects(16.0);
            t.render();
            if t.spill_rev() > 0 && t.spill_rect_count() > 0 {
                break;
            }
        }
        let (w, h) = (t.width(), t.height());
        let grid_h = h - 2 * pad - head;
        assert_eq!(
            t.spill_len(),
            (w * (pad + head) + w * pad + 2 * pad * grid_h) * 4,
            "spill buffer sized to the four band strips"
        );
        assert!(t.spill_rev() > 0, "the fire must reach the band");
        assert!(t.spill_rect_count() > 0, "band content must report rects");

        // Byte parity against the engine's OWN presented frame: walk the four
        // strips (top, bottom, left, right — the documented packing) and
        // compose each spill pixel over the theme bg with the renderer's own
        // over_rgb; transparent spill pixels must sit on untouched bg bytes.
        let bg = Theme::default().bg & 0x00FF_FFFF;
        let frame = t.rgba();
        let frame_rgb = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            (u32::from(frame[i]) << 16) | (u32::from(frame[i + 1]) << 8) | u32::from(frame[i + 2])
        };
        let strips = [
            (0usize, 0usize, w, pad + head),
            (0, h - pad, w, pad),
            (0, pad + head, pad, grid_h),
            (w - pad, pad + head, pad, grid_h),
        ];
        let buf = t.spill.rgba().to_vec();
        let mut lit = [0usize; 4];
        let mut off = 0usize;
        for (si, (sx, sy, sw, sh)) in strips.into_iter().enumerate() {
            for yy in 0..sh {
                for xx in 0..sw {
                    let px = &buf[(off + yy * sw + xx) * 4..][..4];
                    let (x, y) = (sx + xx, sy + yy);
                    let composed = if px[3] == 0 {
                        bg
                    } else {
                        lit[si] += 1;
                        aterm_render::over_rgb(
                            bg,
                            (u32::from(px[0]) << 16) | (u32::from(px[1]) << 8) | u32::from(px[2]),
                            px[3],
                        )
                    };
                    assert_eq!(
                        composed,
                        frame_rgb(x, y),
                        "spill ∘ bg must equal the frame band at ({x},{y})"
                    );
                }
            }
            off += sw * sh;
        }
        assert!(
            lit[0] > 0,
            "the fire must rise into the TOP strip (the head band)"
        );

        // Pointer stability across an animating content frame.
        let ptr = t.spill_ptr();
        t.advance_effects(16.0);
        t.render();
        assert_eq!(t.spill_ptr(), ptr, "same geometry ⇒ the export never moves");

        // Idle re-render (no clock advance): the engine's word to skip the blit.
        let rev = t.spill_rev();
        t.render();
        assert_eq!(t.spill_rev(), rev, "idle re-render must not tick the rev");
        assert_eq!(t.spill_rect_count(), 0);
    }

    /// Typing-only frames with a grid-interior glow keep the spill revision
    /// still: the emissions move with the cursor, but nothing reaches the
    /// band, so the host may skip its blit on the engine's word.
    #[test]
    fn spill_rev_holds_still_for_typing_under_an_interior_glow() {
        let Some(mut t) = AtermTerminal::new_from_system(12, 40, 16.0) else {
            return;
        };
        t.set_chrome(12, 30);
        // Park the cursor deep in the grid, then enable the same style the
        // band test proves emissive — here everything it emits is interior.
        t.process(b"\r\n\r\n\r\n\r\n\r\n\r\n      hello");
        t.set_cursor_glow(true, "water", None, None, 400, 64, 1.0, 2.0, true);
        for _ in 0..12 {
            t.advance_effects(16.0);
            t.render();
        }
        let rev = t.spill_rev();
        t.process(b"x");
        t.render();
        t.process(b"y");
        t.render();
        assert_eq!(
            t.spill_rev(),
            rev,
            "typing under a grid-interior glow must not tick the spill rev"
        );
        assert_eq!(t.spill_rect_count(), 0);
    }

    /// Effects ON actually reach the pixels through the wasm path (sparkle ink
    /// + decorations change the frame for matched words).
    #[test]
    fn effects_on_changes_pixels_for_matched_words() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping effects-on pixel test");
            return;
        };
        t.process(b"kitty cat says fuck\r\n");
        t.render();
        let before = t.rgba();
        t.set_sparkle_words_enabled(true);
        t.advance_effects(120.0);
        t.render();
        assert_ne!(
            before,
            t.rgba(),
            "sparkle words ON must visibly decorate matched words"
        );
    }

    /// Determinism: two instances fed the SAME bytes and the SAME dt stream
    /// produce identical frames at every step — the engines never read a wall
    /// clock (the injected-clock contract), so replay is exact.
    #[test]
    fn effects_on_is_deterministic_across_instances() {
        let Some(mut a) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping effects determinism test");
            return;
        };
        let Some(mut b) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        for t in [&mut a, &mut b] {
            t.set_sparkle_words_enabled(true);
            t.set_cursor_glow(
                true,
                "sparkle",
                Some(0x0050_FA7B),
                None,
                400,
                24,
                0.9,
                0.8,
                true,
            );
            t.process(b"the cat typed fuck and moved on\r\n");
        }
        for step in 0..40u32 {
            // Cursor motion so the aurora spawns comets/particles too.
            let bytes = format!("x{}", if step % 8 == 0 { "\r\n" } else { "" });
            a.process(bytes.as_bytes());
            b.process(bytes.as_bytes());
            a.advance_effects(33.0);
            b.advance_effects(33.0);
            a.render();
            b.render();
            assert_eq!(
                a.rgba(),
                b.rgba(),
                "same bytes + same dt stream must be pixel-identical (step {step})"
            );
        }
    }

    /// Self-termination / idle-to-zero: after the animation budgets elapse the
    /// binding reports inactive and the frame fingerprint is stable (two more
    /// advanced renders change nothing) — the host's rAF loop can stop.
    #[test]
    fn effects_settle_to_stable_idle() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            eprintln!("no system font; skipping effects settle test");
            return;
        };
        t.set_sparkle_words_enabled(true);
        // Idle cat one-shots off so "settled" means settled forever, not
        // "until the next scheduled blink" (that path is deadline-driven).
        t.set_sparkle_feline("cat", true, true, false);
        t.set_cursor_glow(true, "lumen", None, None, 260, 24, 0.7, 0.6, true);
        t.process(b"cat + fuck\r\n");
        let mut settled = false;
        for _ in 0..600 {
            t.advance_effects(100.0);
            t.render();
            if !t.is_effects_active() {
                settled = true;
                break;
            }
        }
        assert!(settled, "effects must self-terminate within the budget");
        let stable = t.rgba();
        t.advance_effects(500.0);
        t.render();
        assert_eq!(stable, t.rgba(), "settled frame must be a fixed point");
        assert!(!t.is_effects_active(), "still idle after further advance");
    }

    #[test]
    fn json_string_escapes_payloads() {
        assert_eq!(json_string("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(json_string("x\ny"), r#""x\ny""#);
    }

    // --- Cooperative width-reflow offload (the wasm L0-freeze fix) ----------
    //
    // The state machine under test: resize → (inline | stash) → pump* →
    // re-attach, plus the two never-pumped safety nets and the supersede
    // path. The ctor attaches the engine-default tiered store (audit E1);
    // these tests install a TINY-ring one instead so nearly all scroll-off
    // spills to the store and the deferred path is exercised densely.

    /// Swap the engine for one whose bulk history lives in the tiered store
    /// (tiny ring → nearly all scroll-off spills to tiered, like a real
    /// session) and feed `lines` numbered lines short enough not to wrap at
    /// the test widths (so line counts stay comparable across rewraps).
    fn install_tiered_history(t: &mut AtermTerminal, rows: u16, cols: u16, lines: usize) {
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
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
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
    fn small_tiered_history_rewraps_inline_on_resize() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
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
        assert!(
            t.serialize_scrollback(None).contains("L0-hist"),
            "oldest line intact after the inline rewrap"
        );
    }

    #[test]
    fn large_tiered_history_defers_rewrap_and_stepped_pumps_reattach_it() {
        // The content-intact defer test, now driven in RANDOM SMALL BUDGETS:
        // every pump is a bounded `reflow_step` slice, and any schedule must
        // re-attach content identical to what a one-shot rewrap yields
        // (aterm-grid's schedule-independence property; here we assert the
        // JS-visible outcome: counts + oldest line intact).
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
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
        let mut lcg = 0x5EED_1234_u64;
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
        assert!(
            t.serialize_scrollback(None).contains("L0-hist"),
            "oldest line intact after the stepped rewrap"
        );
    }

    #[test]
    fn resize_mid_stepping_supersedes_without_losing_history() {
        // A width change while a job is HALF-STEPPED (not merely stashed):
        // the store is out, so nothing re-detaches; the partly-stepped job
        // keeps its progress and still re-attaches (content valid, wrapping
        // stale for the newest width, self-healing on the next width change —
        // the same supersede semantics as the never-pumped stash).
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
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
        assert!(
            t.serialize_scrollback(None).contains("L0-hist"),
            "oldest line intact after the mid-stepping supersede"
        );
    }

    #[test]
    fn resize_during_the_window_supersedes_without_losing_history() {
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();

        t.resize(24, 40);
        assert!(t.reflow_pending());
        // A second width change while detached: the store is out, so nothing
        // re-detaches — the stashed job survives and still re-attaches
        // (content valid; wrapping stale for the newest width, self-healing
        // on the next width change — the native supersede semantics).
        t.resize(24, 60);
        assert!(t.reflow_pending(), "the stashed job survives a re-resize");
        assert_eq!(t.cols, 60, "the newest geometry wins for the grid");

        while t.pump_reflow() {}
        let after = t.term.grid().scrollback_lines();
        assert!(
            after >= before,
            "no history lost across the superseding resize \
             (before={before}, after={after})"
        );
        // The self-heal: the NEXT width change re-detaches the (stale-wrapped)
        // store and re-reflows it to the current width.
        t.resize(24, 30);
        assert!(
            t.reflow_pending(),
            "stale wrapping re-detaches and re-wraps"
        );
        while t.pump_reflow() {}
        assert!(
            t.serialize_scrollback(None).contains("L0-hist"),
            "oldest line intact after the whole supersede dance"
        );
    }

    #[test]
    fn never_pumped_host_completes_via_the_render_grace_net() {
        // SAFETY ARGUMENT UNDER TEST: a host that never calls `pump_reflow`
        // (an un-updated embedder) must still get its history back — after
        // the grace window `render` pumps ONE bounded step per frame, so the
        // job CONVERGES across frames (never the whole rewrap in one frame).
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();
        t.resize(24, 40);
        assert!(t.reflow_pending());
        // Grace window: the deferred job must NOT run on the first frames (an
        // updated host's idle pump gets its chance to win the race)…
        t.render();
        assert!(t.reflow_pending(), "no pump inside the grace window");
        // …then one budgeted step per frame: > 25k input lines at 2_000/step
        // (plus the refill steps) needs a couple dozen frames beyond grace.
        for _ in 0..REFLOW_PUMP_GRACE_RENDERS {
            t.render();
        }
        assert!(
            t.reflow_pending(),
            "a multi-step job is still converging right after grace \
             (one bounded step per frame — not one catch-up frame)"
        );
        let mut frames = 0usize;
        while t.reflow_pending() {
            t.render();
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
        // SAFETY ARGUMENT UNDER TEST: a host that neither pumps NOR renders
        // (hidden tab) but keeps feeding PTY output stages scroll-off in the
        // lazy buffer while detached — past the cap, EVERY `process` call
        // advances the job one bounded step, so the window closes after
        // finitely many calls (amortized convergence; no single unbounded
        // catch-up task).
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
            return;
        };
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        t.resize(24, 40);
        assert!(t.reflow_pending());

        // Blow past the backlog cap in one burst (no pump can close the
        // window in a single call any more — that was the point)…
        let mut buf = Vec::new();
        for i in 0..(REFLOW_BACKLOG_MAX_LINES + 2_000) {
            buf.extend_from_slice(format!("W{i}-window\r\n").as_bytes());
        }
        t.process(&buf);
        // …then keep streaming: each call steps once; the job must converge
        // well within the step count of a ~25k-line history (≈26 steps at
        // 2_000/step for drain + refill).
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
        let history = t.serialize_scrollback(None);
        assert!(
            history.contains("L0-hist"),
            "pre-resize history survived the stream-while-detached window"
        );
        assert!(
            history.contains("W0-window"),
            "output produced DURING the window landed in history (audit bug B order)"
        );
    }

    #[test]
    fn teardown_with_a_half_stepped_job_drops_cleanly() {
        // Module teardown (JS `free()` → Drop) while a job is stashed AND
        // half-stepped: the engine and the detached history (with its
        // stepping progress) drop TOGETHER — no wedge can outlive the
        // instance, and nothing panics.
        let Some(mut t) = AtermTerminal::new_from_system(24, 80, 16.0) else {
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

    #[test]
    fn search_budgeted_resumes_to_the_one_shot_result() {
        // Driving the budgeted export to completion — for literal, folded, and
        // regex modes — yields exactly the legacy one-shot triplets, cursor
        // discipline included (Some while incomplete, None at completion).
        let Some(mut t) = AtermTerminal::new_from_system(6, 40, 16.0) else {
            return;
        };
        for i in 0..60 {
            t.process(format!("row {i} NEEDLE-{i} tail\r\n").as_bytes());
        }
        for &(query, cs, rx) in &[
            ("NEEDLE", true, false),
            ("needle", false, false),
            ("NEEDLE-[0-9]+", true, true),
        ] {
            let one_shot = t.search(query, cs, rx);
            let mut cursor = None;
            let mut all_matches = Vec::new();
            let mut search_id = None;
            let step = loop {
                let step = t.search_budgeted(query, cs, rx, cursor, 9);
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
                assert!(step.rows_fed() <= step.total_rows());
                cursor = step.cursor();
            };
            assert_eq!(step.cursor(), None, "complete step drops the cursor");
            assert_eq!(
                all_matches, one_shot,
                "budgeted != one-shot for {query:?} cs={cs} rx={rx}"
            );
            assert!(!step.incomplete_index(), "small buffer is exhaustive");
            assert_eq!(step.lowest_retained_line(), 0);
        }
    }

    #[test]
    fn search_budgeted_supersession_and_edge_cases_stay_safe() {
        let Some(mut t) = AtermTerminal::new_from_system(6, 40, 16.0) else {
            return;
        };
        for i in 0..60 {
            t.process(format!("row {i} NEEDLE-{i} tail\r\n").as_bytes());
        }
        // A new pattern supersedes the in-flight search: old cursor restarts.
        let first = t.search_budgeted("NEEDLE", true, false, None, 5);
        assert!(!first.complete());
        assert!(first.reset());
        let first_id = first.search_id();
        let switched = t.search_budgeted("row", true, false, first.cursor(), 5);
        assert!(switched.rows_fed() <= 5, "superseded cursor restarted");
        assert!(switched.reset());
        assert_ne!(switched.search_id(), first_id);
        // Cancel between slices: the cursor goes stale, restart is silent.
        t.search_budgeted_cancel();
        let resumed = t.search_budgeted("row", true, false, switched.cursor(), 5);
        assert!(resumed.rows_fed() <= 5, "cancelled cursor restarted");
        assert!(resumed.reset());
        assert_ne!(resumed.search_id(), switched.search_id());
        // Empty query: immediate empty complete result (and frees state).
        let empty = t.search_budgeted("", true, false, resumed.cursor(), 5);
        assert!(empty.complete() && empty.matches().is_empty());
        assert!(empty.reset() && empty.search_id().is_none());
        // Invalid regex: silent empty complete result, like the legacy export.
        let bad = t.search_budgeted("f(oo", false, true, None, 5);
        assert!(bad.complete() && bad.matches().is_empty());
        assert!(bad.reset() && bad.search_id().is_none());
    }

    /// WF-1 frame gate, two-sided + byte parity. Side 1 (skip): a second
    /// `render()` with nothing changed must take the gate (zero present bands,
    /// `rgba` byte-identical). Side 2 (reopen): every class of change the gate
    /// folds — grid damage, blink phase, selection, viewport scroll — must
    /// defeat the skip and produce output byte-identical to a fresh instance
    /// fed the same bytes (the gate can never be the thing that changes
    /// pixels). This is the reach fence: if the gate silently stopped firing
    /// (or fired always), one of the two sides fails.
    #[test]
    fn frame_gate_skips_settled_frames_and_reopens_on_every_change_class() {
        let Some(mut t) = AtermTerminal::new_from_system(8, 40, 14.0) else {
            eprintln!("no system font; skipping frame-gate test");
            return;
        };
        t.process(b"hello gate");
        t.render();
        assert!(!t.last_render_skipped(), "first render must draw");
        let baseline = t.rgba();

        // SKIP side: settled frame -> gated, zero bands, bytes retained.
        t.render();
        assert!(t.last_render_skipped(), "settled frame must take the gate");
        assert_eq!(t.present_band_count(), 0, "gated frame exports zero bands");
        assert_eq!(t.rgba(), baseline, "gated frame retains the framebuffer");
        assert!(!t.needs_frame(), "needs_frame must agree with the gate");

        // REOPEN side 1: grid damage (echo).
        t.process(b"!");
        assert!(t.needs_frame(), "damage must reopen the gate");
        t.render();
        assert!(!t.last_render_skipped(), "an echo frame must draw");
        // Byte parity with a fresh instance fed the same total byte stream:
        // the gate must be unobservable in pixels.
        let Some(mut fresh) = AtermTerminal::new_from_system(8, 40, 14.0) else {
            return;
        };
        fresh.process(b"hello gate!");
        fresh.render();
        assert_eq!(t.rgba(), fresh.rgba(), "gate must be pixel-invisible");

        // REOPEN side 2: renderer-held blink phase (no grid damage).
        t.render();
        assert!(t.last_render_skipped(), "re-settled before blink");
        t.set_cursor_blink_phase(false);
        t.render();
        assert!(!t.last_render_skipped(), "a blink flip must draw");
        t.set_cursor_blink_phase(false); // same phase again: shadow de-dups
        t.render();
        assert!(t.last_render_skipped(), "an idempotent blink re-assert must gate");

        // REOPEN side 3: selection (Terminal-held, no grid damage).
        t.selection_start(2, 1);
        t.selection_extend(2, 8);
        t.render();
        assert!(!t.last_render_skipped(), "a selection change must draw");

        // REOPEN side 4: viewport scroll (display-offset damage).
        for _ in 0..12 {
            t.process(b"line\r\n");
        }
        t.render();
        t.render();
        assert!(t.last_render_skipped(), "re-settled before scroll");
        t.scroll_lines(3);
        t.render();
        assert!(
            !t.last_render_skipped(),
            "a viewport scroll must draw (display-offset damage -> epoch)"
        );
    }
}

