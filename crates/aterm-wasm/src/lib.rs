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

// What JS sees is marked `#[export]` (aterm-wasm-abi, which replaces
// wasm-bindgen: docs/AUDIT-third-party-2026-10-15.md, wasm-platform WP.7), and
// `verify glue` writes the module's JS glue from it.
#[cfg(target_arch = "wasm32")]
use aterm_wasm_abi::export;

// The allocator and return slot the glue calls.
#[cfg(target_arch = "wasm32")]
aterm_wasm_abi::export_runtime!();

mod dirty_band_present_api;
// The terminal type the binding modules shared by aterm-wasm and
// aterm-gpu-web extend (aterm-gpu-web mounts them by `#[path]`).
use crate::AtermTerminal as WebTerminal;

// Expands a shared module's `bind!` — its exported impl — onto this crate's
// terminal under this crate's export attribute (`aterm-gpu-web` still binds
// with wasm-bindgen). The attribute names the JS class after the impl's
// type, a bare identifier, so the name is brought into the shared module here.
macro_rules! web_terminal_impl {
    ($bind:ident) => {
        use crate::AtermTerminal;
        #[cfg(target_arch = "wasm32")]
        use aterm_wasm_abi::export;
        $bind!(AtermTerminal, export);
    };
}

mod effects_api;
mod facade_api;
#[cfg(test)]
use facade_api::selection_range_for;
mod host;
mod messages_api;
mod notifications_api;
mod predict_api;
mod scroll_input_api;
mod scrollback_tiers_api;
mod theme_api;

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
// no single event-loop task grows with session history.
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
/// convergence with every task still short.
const REFLOW_BACKLOG_MAX_LINES: usize = 20_000;

/// Default per-pump rewrap budget, in INPUT history lines, for
/// [`AtermTerminal::pump_reflow`] (host-tunable via
/// [`AtermTerminal::pump_reflow_budget`]). Sizing: measured native release
/// cost is ~1.4 µs per near-full 80-col history line (aterm-grid's
/// `reflow_step` bench, 2026-07-14: 50k lines one-shot in ~69ms;
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
#[cfg_attr(target_arch = "wasm32", export)]
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
#[cfg_attr(target_arch = "wasm32", export)]
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
    // `cell_frame_damage_scoped_into` (only the damaged rows under the DMG-1
    // continuity proof, the full refill otherwise) instead of allocating a
    // fresh `RenderInput` (the outer container Vecs + a per-row inner Vec for
    // each row) every frame — the same
    // kept scratch its two sibling frontends (aterm-gpu-web's `frame_scratch`,
    // the native windowed `input_scratch`) already hold (E8).
    frame_scratch: RenderInput,
    // TEST BUILDS ONLY: which arm the last `refill_frame_scratch` took (DMG-1)
    // — `Scoped` on an ordinary echo frame, `Full` (with the refusing clause)
    // otherwise, `None` before the first refill — for the reach tests. The
    // shipped build neither stores nor returns it.
    #[cfg(test)]
    last_refill: Option<aterm_core::render::FrameRefill>,
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
    // The MESSAGE BAND (the unified message engine's rows, composed above
    // the grid through the renderer's `band` module — the same composition
    // the native window runs). Empty until a `notice` posts a row; with no
    // committed row `render()` is byte-identical to a band-less build.
    // pub(crate) so the messages_api module (and tests) reach it (the
    // effects-field posture).
    pub(crate) band: messages_api::WebBand,
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
    // Resident row buffers for the predictor (the wasm twin of aterm-gui's
    // `predict_seen`): `Predictor::reconcile_seen` runs its observe closure once
    // per retired guess plus the head and the tail guard's scan, all on the SAME
    // row, so extracting the row per probe would allocate a fresh Vec and
    // re-resolve the whole row (palette, decorations, the lot) each time;
    // `Terminal::logical_row_into` refills these in place instead. The cursor
    // row a run's first keypress hands the predictor lives here too.
    pub(crate) pred_row_scratch: predict_api::PredRows,
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
    // Named themes, splits and registered themes (the shared theme_api):
    // the selector on screen and the host's last appearance. pub(crate) so
    // theme_api reaches it (the effects-field posture).
    pub(crate) web_theme: theme_api::WebThemeState,
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
/// [`AtermTerminal::refill_frame_scratch`]. The baseline moves through
/// [`Terminal::rebase_colors`], so a program's OSC 10/11/12/17 colours survive
/// a host theme change, as they survive a native reload; the palette and the
/// selection foreground keep their current baseline.
fn apply_terminal_theme_colors(term: &mut Terminal, fg: u32, bg: u32, cursor: u32, selection: u32) {
    let mut base = term.color_baseline();
    base.foreground = theme_api::unpack(fg);
    base.background = theme_api::unpack(bg);
    base.cursor = Some(theme_api::unpack(cursor));
    base.selection_background = Some(theme_api::unpack(selection));
    term.rebase_colors(&base);
}

impl AtermTerminal {
    /// Refill every engine-owned frame channel AND consume the frame's damage
    /// session, through the engine's ONE damage consumer,
    /// [`Terminal::cell_frame_damage_scoped_into`] — the extraction the native
    /// GUI renders through. Under its continuity proof only the damage
    /// tracker's rows re-resolve (a keystroke echo refills one row, not
    /// `rows × cols`); any break falls back to the full refill, which is always
    /// sound. The unconditional restamp covers the live implicit background,
    /// cursor and selection colours, so sparse tails, OSC 10/11/12 resets, and
    /// DECSCNM remain one coherent terminal snapshot either way.
    ///
    /// The effects pipeline closes a damage session only when one is still
    /// open (`EffectsPipeline::apply`), so it does not re-take the session
    /// this call consumed — a second `take_damage` would bump the extraction
    /// generation and force the full arm on every frame with sparkle words or
    /// rain on.
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

/// The renderer half of a theme install, which `theme_api` (shared with
/// aterm-gpu-web) calls on whichever crate compiles it.
impl AtermTerminal {
    /// The CPU face this terminal renders with.
    pub(crate) fn render_face(&self) -> &Renderer {
        &self.renderer
    }

    /// Everything a theme change does outside the engine's colours: the
    /// repaint, the effects' and band's theme-derived inks, and the renderer.
    pub(crate) fn install_renderer_theme(&mut self, parts: aterm_types::scheme::ThemeParts) {
        let aterm_types::scheme::ThemeParts { fg, bg, cursor, .. } = parts;
        // Theme is appearance-only (selection band / idle cursor / padding / default
        // cells) — not tracked by the row-diff — so force one full repaint next frame.
        self.force_full_repaint = true;
        // Keep the effects' derive-from-theme default in sync (glow/trail colours
        // passed as `None` follow the cursor colour, like the native app).
        self.theme_cursor = cursor & 0x00FF_FFFF;
        self.theme_fg = fg & 0x00FF_FFFF;
        self.theme_bg = bg & 0x00FF_FFFF;
        self.effects
            .set_matrix_rain_theme(self.theme_bg, self.theme_fg, self.theme_cursor);
        // The pet's frozen contrast sample is keyed to the theme it was taken
        // against; a new theme retires it (the native palette-authority edge)
        // while the cat's behaviour, position and breed carry on.
        self.effects.invalidate_companion_colors();
        // The band's inks derive from the theme (the next frame repaints it).
        self.band.set_theme(fg, bg, cursor);
        self.renderer.set_theme(Theme::from(parts));
    }

    /// The renderer half of `set_selection_fg` (the engine half is the caller's).
    pub(crate) fn install_renderer_selection_fg(&mut self, fg: Option<u32>) {
        self.force_full_repaint = true;
        self.renderer.set_selection_fg(fg);
    }

    /// TEST BUILDS ONLY: render once, so the frame gate settles.
    #[cfg(test)]
    pub(crate) fn settle_frame(&mut self) {
        self.render();
    }
}

#[cfg_attr(target_arch = "wasm32", export)]
impl AtermTerminal {
    /// Build a `rows`x`cols` terminal rendered with `font_bytes` (a TTF/OTF) at
    /// `px` cell font-size. `font_bytes` is injected by the host (fetched in JS),
    /// keeping the engine free of filesystem font discovery. `fg`/`bg`/`cursor`/
    /// `selection` are 0x00RRGGBB and seed the renderer's DEFAULT theme colors;
    /// per-cell SGR colors still flow through the grid independently.
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(target_arch = "wasm32", export(constructor))]
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
        let web_theme = theme_api::WebThemeState::new(term.color_scheme());
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
            #[cfg(test)]
            last_refill: None,
            smart: SmartSelection::with_builtin_rules(),
            effects: EffectsPipeline::new(),
            theme_cursor: cursor & 0x00FF_FFFF,
            theme_fg: fg & 0x00FF_FFFF,
            theme_bg: bg & 0x00FF_FFFF,
            notifications,
            band: messages_api::WebBand::new(aterm_time::Instant::now(), fg, bg, cursor),
            scroll_input: scroll_input_api::ScrollInputState::default(),
            predict: aterm_predict::Predictor::default(),
            pred_row_scratch: predict_api::PredRows::default(),
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
            web_theme,
        })
    }

    /// Inject a broad-coverage (CJK + symbols) fallback face from font bytes, so
    /// glyphs the primary face lacks render real shapes instead of `.notdef` tofu.
    /// The canvas renderer can't read the host filesystem, so the host pushes the
    /// OS font bytes in. Throws on a bad blob; the existing face stays.
    pub fn set_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_fallback_bytes(bytes)
    }

    /// APPEND another fallback face to the chain (does NOT reset it like
    /// [`set_fallback_font`]). The chain is tried in order, so the host can push a
    /// CJK fallback first then Arabic/Devanagari/Thai/Hebrew faces after it — a
    /// glyph the earlier faces miss still reaches a covering face instead of tofu.
    /// Throws on a bad blob; the existing chain stays.
    pub fn add_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.add_fallback_bytes(bytes)
    }

    /// Inject a colour-emoji (sbix) face from font bytes, driving the existing
    /// ColorEmoji colour path. Same rationale as [`set_fallback_font`]: the host
    /// supplies the OS emoji font. Throws on a bad blob; the slot stays.
    pub fn set_emoji_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_color_font_bytes(bytes.to_vec())
    }

    /// Inject a REAL bold weight of the primary family so SGR-bold cells render as a
    /// true heavier weight instead of synthetic embolden. The host supplies the
    /// bold-variant bytes (the canvas can't read the filesystem). Throws on a bad
    /// blob; the existing weight stays.
    pub fn set_bold_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_bold_font(bytes)
    }

    /// Inject a broad-coverage SYMBOL fallback face from font bytes, so symbol
    /// glyphs the primary + fallback faces lack render real shapes instead of
    /// tofu. The byte-injection sibling of the config `symbol_font` path: the host
    /// supplies the OS symbol bytes (the canvas can't read the filesystem).
    /// Throws on a bad blob.
    pub fn set_symbol_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_symbol_fallback_bytes(bytes)
    }

    /// Swap the PRIMARY face (the host's `terminalFontFamily`) from font bytes and
    /// re-rasterize. The host re-reads cell_width/cell_height + recomputes the grid
    /// after (the new face may have different metrics). Throws on a bad blob; the
    /// existing face stays.
    pub fn set_primary_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_primary_font(bytes)
    }

    // ── registered-font (handle) twins ──────────────────────────────────────
    // Per-pane engine builds inject the SAME OS faces; the byte-based setters
    // above re-marshal each blob across the JS/wasm boundary per call. These
    // twins take a `register_font` handle instead, so panes 2..N of a shared
    // worker/page copy nothing (see REGISTERED_FONTS).

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
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn chrome_pad(&self) -> u16 {
        self.renderer.pad() as u16
    }

    /// The chrome top head band set via [`Self::set_chrome`] (px; 0 = none).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
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
        // The band borrows slots 1, 3, 4 and 6 (its error, warn and meter hues).
        self.band.set_palette_color(index, [r, g, b]);
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

    /// Replace the default fg/bg/cursor/selection theme live (0x00RRGGBB), so a host
    /// theme change re-themes the pane without rebuilding it. Per-cell SGR colours
    /// flow independently; pair with set_palette_color for the ANSI palette.
    ///
    /// A program's OSC 10/11/12/17 colours survive it (the engine baseline is
    /// rebased, as on native). It takes the colours back from a named theme
    /// (`set_theme_by_name`), so a later `set_color_scheme` leaves them alone,
    /// and the reported scheme returns to the host's last `set_color_scheme`.
    pub fn set_theme(&mut self, fg: u32, bg: u32, cursor: u32, selection: u32) {
        self.install_host_colors(aterm_types::scheme::ThemeParts {
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
        self.term
            .set_default_selection_foreground(fg.map(theme_api::unpack));
        self.install_renderer_selection_fg(fg);
    }

    /// Set the per-cell minimum contrast ratio (xterm's `minimumContrastRatio`,
    /// 1..=21): every glyph fg is floored against its OWN cell bg — the classic
    /// rescue for bright-white SGR text on a light theme. `ratio <= 1.0` turns
    /// the floor off (the default; xterm treats 1 as "do nothing"). Cells whose
    /// fg == bg are never adjusted (SGR 8 conceal renders fg = bg and must stay
    /// hidden). While `set_background_opacity` is below 1 the renderer floors
    /// at no less than 4.5:1 (`effective_minimum_contrast`), as native does.
    /// Appearance-only, so force one full repaint next frame.
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
    /// band and glyph pixels stay opaque so text keeps its contrast, and below
    /// 1.0 glyphs are floored to at least 4.5:1 against their cell
    /// (`effective_minimum_contrast`), as on native.
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

    /// Safety net #1 (see `REFLOW_PUMP_GRACE_RENDERS`): called by `render`.
    /// After the grace window, ONE budgeted step per frame — never the whole
    /// job in a single frame.
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
        // construction: skip the refill (`cell_frame_damage_scoped_into`), the
        // compute_dirty_rows row diff and the cache clone_from AND the raster/expand
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
        // E8 + DMG-1: refill the kept scratch in place (the gpu-web/native
        // kept-scratch pattern), re-resolving only the damaged rows when the
        // continuity proof holds; the effects/stamp passes below re-fill the
        // host-owned overlay channels, so a reused scratch never carries a
        // previous frame's state. The refill also CONSUMES the damage session
        // (WF-1): the NEXT net-new grid change opens a fresh session and
        // advances the epoch the frame gate compares. aterm-render never reads
        // the tracker (it diffs snapshots), and this render loop is the
        // engine's only damage consumer here.
        // THE BAND'S UN-SPLICE (DMG-1): take last frame's band rows off the
        // kept scratch, so the refill below may re-resolve only the damaged
        // rows under a band instead of refilling every row. Refused (nothing
        // changed) when nothing was prepended or anything else touched it — the
        // refill then takes its full arm, which is always sound.
        let _ = self.band.undo(&mut self.frame_scratch, self.rows);
        self.refill_frame_scratch();
        // Fill the overlay channels (aurora/trail/sparkle) for the host-advanced
        // instant. With every effect off this only clears the channels a reused
        // scratch may carry — byte-identical to the pre-effects render.
        let (cw, ch) = self.renderer.cell_size();
        // The band's rows land above the grid AFTER `apply` (the compose
        // below); the pipeline must read the page's pointer against the grid
        // where this frame draws it, so it learns their height first.
        self.hand_band_to_effects();
        self.effects
            .apply(&mut self.term, &mut self.frame_scratch, cw, ch);
        // Present the banked sub-row scroll residual via the M1b band translate.
        // The stamp sets the grid band to the whole frame `[0, rows)`;
        // `compose_band_into_frame` below then moves it down below any
        // committed message-band rows. Stamped every frame: the KEPT scratch
        // would otherwise carry a stale shift.
        self.scroll_input
            .stamp(&mut self.frame_scratch, self.rows, ch);
        // THE MESSAGE BAND: its committed rows prepended above the grid (every
        // window-space effect stream translated down with the grid), the grid
        // band moved below them, and their gutters handed to the renderer.
        // No committed row: nothing moves and the bleed stays `None`.
        self.compose_band_into_frame();
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

    /// THE BAND'S HOST HOOKS (`messages_api`, which the GPU module carries
    /// line for line): the CPU face's cell, pad and grid-top metrics.
    pub(crate) fn band_metrics(&self) -> messages_api::BandMetrics {
        let (cell_w, cell_h) = self.renderer.cell_size();
        messages_api::BandMetrics {
            cell_w,
            cell_h,
            pad: self.renderer.pad(),
            grid_top: self.renderer.grid_top(),
        }
    }

    /// The band's gutters for the frame being composed, handed to the
    /// rasterizer.
    pub(crate) fn set_band_bleed(&mut self, bleed: Option<aterm_render::ChromeBleed>) {
        self.renderer.set_chrome_bleed(bleed);
    }

    /// The band changed: reopen the frame gate for one frame. The CPU frame
    /// is sized from the rows it composes, so a moved row count needs
    /// nothing more here.
    pub(crate) fn note_band_change(&mut self, _rows_moved: bool) {
        self.note_host_visual_change();
    }

    /// Last-rendered framebuffer width in pixels.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Last-rendered framebuffer height in pixels.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Cell width in device pixels — the host computes cols = floor(canvasW / cellWidth).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn cell_width(&self) -> usize {
        self.renderer.cell_size().0
    }

    /// Cell height in device pixels — the host computes rows = floor(canvasH / cellHeight).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
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
}

/// Metadata for a legacy-contract search ([`AtermTerminal::search_meta`]).
#[cfg_attr(target_arch = "wasm32", export)]
pub struct SearchMeta {
    incomplete: bool,
    match_count: u32,
}

#[cfg_attr(target_arch = "wasm32", export)]
impl SearchMeta {
    /// True when the results may be truncated: index eviction dropped old rows
    /// before they could be searched, or the engine's match cap was reached.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn incomplete(&self) -> bool {
        self.incomplete
    }

    /// Number of matches the paired [`AtermTerminal::search`] call returns
    /// (i.e. its flat triplet array length / 3), after any cap.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn match_count(&self) -> u32 {
        self.match_count
    }
}

/// One slice of a budgeted search ([`AtermTerminal::search_budgeted`]).
#[cfg_attr(target_arch = "wasm32", export)]
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

#[cfg_attr(target_arch = "wasm32", export)]
impl BudgetedSearchResult {
    /// Stable match DELTA as flat `[abs_line, start_col, len]` triplets (same
    /// coordinate contract as [`AtermTerminal::search`]). Append across calls;
    /// already-reported matches keep their order and positions.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn matches(&self) -> Vec<u32> {
        self.matches.clone()
    }

    /// Whether every retained row has been scanned and every match delta has
    /// been delivered. Dense searches can have `rows_fed == total_rows` while
    /// this remains false for bounded backlog-drain turns.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Token to resume with; `undefined` once complete.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// Stable identity for the logical search, including its completing step;
    /// `undefined` only for an empty/invalid query result.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn search_id(&self) -> Option<u64> {
        self.search_id
    }

    /// Whether this step starts a new logical result stream. Clear previously
    /// accumulated match deltas before appending this step when true.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn reset(&self) -> bool {
        self.reset
    }

    /// True when the results may be truncated: index eviction dropped old
    /// rows, or the engine's match cap was reached. (The legacy
    /// [`AtermTerminal::search`] export silently drops this signal.)
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn incomplete_index(&self) -> bool {
        self.incomplete_index
    }

    /// Final oldest absolute line retained by the completed search index. The
    /// deterministic eviction schedule makes this stable from the first turn.
    /// When nonzero with `incomplete_index`, older history was evicted; a zero
    /// watermark with `incomplete_index` indicates match-cap-only truncation.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn lowest_retained_line(&self) -> u32 {
        self.lowest_retained_line
    }

    /// Rows scanned so far (progress numerator; restarts reset it).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn rows_fed(&self) -> u32 {
        self.rows_fed
    }

    /// Total rows this search will scan (progress denominator).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
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
#[cfg_attr(target_arch = "wasm32", export)]
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
#[cfg_attr(target_arch = "wasm32", export)]
pub struct LinkHit {
    url: String,
    start_col: u16,
    end_col: u16,
    kind: u8,
}

#[cfg_attr(target_arch = "wasm32", export)]
impl LinkHit {
    /// The link's URL/target text.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Inclusive start display column of the link span.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn start_col(&self) -> u16 {
        self.start_col
    }

    /// Exclusive end display column of the link span.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn end_col(&self) -> u16 {
        self.end_col
    }

    /// Link kind: 0=osc8, 1=url, 2=file_path, 3=other.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn kind(&self) -> u8 {
        self.kind
    }
}

/// Selection bounds in DISPLAY viewport cell coords (0 = top visible row),
/// inclusive of `start`, with `end` already side-adjusted to match
/// `selection_text` and the painted highlight.
#[cfg_attr(target_arch = "wasm32", export)]
pub struct SelectionRange {
    start_x: u16,
    start_y: u16,
    end_x: u16,
    end_y: u16,
}

#[cfg_attr(target_arch = "wasm32", export)]
impl SelectionRange {
    /// Start column (display-relative).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn start_x(&self) -> u16 {
        self.start_x
    }

    /// Start row (display-relative, 0 = top visible row).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn start_y(&self) -> u16 {
        self.start_y
    }

    /// End column (display-relative, side-adjusted/inclusive).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn end_x(&self) -> u16 {
        self.end_x
    }

    /// End row (display-relative).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn end_y(&self) -> u16 {
        self.end_y
    }
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
        let web_theme = theme_api::WebThemeState::new(term.color_scheme());
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
            #[cfg(test)]
            last_refill: None,
            smart: SmartSelection::with_builtin_rules(),
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
            pred_row_scratch: predict_api::PredRows::default(),
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
            web_theme,
        })
    }
}

/// A test terminal on the host's system font: `new_from_system`, or COULD NOT
/// RUN where this host has no system monospace font. Never a silent pass: a
/// missing font decides nothing about the code.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn system_font_terminal(rows: u16, cols: u16, px: f32) -> AtermTerminal {
    AtermTerminal::new_from_system(rows, cols, px)
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font on this host"))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// The bundled face, injected the way a JS host injects a fetched one. A
    /// security property must not be skipped on a box with no system font, so
    /// those tests build from these bytes rather than `new_from_system`.
    const BUNDLED_FACE: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

    /// The theme the host hands the constructor seeds the engine's implicit
    /// colours (`apply_terminal_theme_colors`), so every frame channel the
    /// renderer takes from the engine (default fg and bg, cursor, selection
    /// pair) is the host's, and an OSC 110/111/112/117/119 reset returns to
    /// it rather than to the engine's built-ins.
    #[test]
    fn the_host_theme_is_the_frames_colour_baseline() {
        let (fg, bg, cursor, selection) = (0x0011_2233, 0x0044_5566, 0x0077_8899, 0x000A_0B0C);
        let mut t = AtermTerminal::new(3, 8, BUNDLED_FACE, 16.0, fg, bg, cursor, selection)
            .expect("the bundled face parses");
        t.set_selection_fg(Some(0x0012_3456));
        let channels = |t: &mut AtermTerminal| {
            t.refill_frame_scratch();
            let f = &t.frame_scratch;
            (
                f.default_fg,
                f.default_bg,
                f.cursor_color,
                f.selection_bg,
                f.selection_fg,
            )
        };
        let host = (fg, bg, cursor, selection, 0x0012_3456);
        assert_eq!(channels(&mut t), host, "at construction");
        t.process(b"\x1b]10;rgb:aaaa/bbbb/cccc\x1b\\\x1b]11;rgb:dddd/eeee/ffff\x1b\\");
        t.process(b"\x1b]12;rgb:2121/4343/6565\x1b\\");
        t.process(b"\x1b]17;rgb:0101/0202/0303\x1b\\\x1b]19;rgb:0404/0505/0606\x1b\\");
        assert_eq!(
            channels(&mut t),
            (
                0x00aa_bbcc,
                0x00dd_eeff,
                0x0021_4365,
                0x0001_0203,
                0x0004_0506
            ),
            "precondition: the program recoloured every channel"
        );
        t.process(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]117\x07\x1b]119\x07");
        assert_eq!(channels(&mut t), host, "after the OSC resets");
    }

    /// `search_meta` and `search_summary` describe exactly the result set the
    /// legacy `search` export returns, plus what it drops: the engine's
    /// match-cap truncation (E9a) and, for the summary, line snippets and a
    /// `total` the `max_matches` cap leaves alone. An empty query or a bad
    /// regex is silent on all three.
    #[test]
    fn search_meta_and_summary_carry_what_the_legacy_export_drops() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        t.process(b"alpha beta\r\nalpha gamma\r\nno match here\r\n");
        assert_eq!(t.search("alpha", true, false).len() / 3, 2);
        let meta = t.search_meta("alpha", true, false);
        assert_eq!((meta.match_count(), meta.incomplete()), (2, false));
        let summary = t.search_summary("alpha", true, false, 1).expect("summary");
        assert_eq!(
            summary.matches("\"absRow\":").count(),
            1,
            "max_matches caps the matches: {summary}"
        );
        assert!(
            summary.contains("\"col\":0,\"len\":5,\"snippet\":\"alpha beta\""),
            "span and snippet: {summary}"
        );
        assert!(
            summary.ends_with("\"total\":2,\"incomplete\":false}"),
            "the cap leaves the total: {summary}"
        );
        for (query, regex) in [("", false), ("(", true)] {
            assert!(t.search(query, true, regex).is_empty(), "{query:?}");
            let meta = t.search_meta(query, true, regex);
            assert_eq!(
                (meta.match_count(), meta.incomplete()),
                (0, false),
                "{query:?}"
            );
            assert_eq!(
                t.search_summary(query, true, regex, 0).as_deref(),
                Some(r#"{"matches":[],"total":0,"incomplete":false}"#),
                "{query:?}"
            );
        }
        // 1,300 lines x 78 "a"s = 101,400 single-char matches > the 100k cap.
        t.set_scrollback_limit(3000);
        let line = "a".repeat(78) + "\r\n";
        for _ in 0..1_300 {
            t.process(line.as_bytes());
        }
        let cap = aterm_core::search::MAX_SEARCH_MATCHES;
        assert_eq!(
            t.search("a", true, false).len() / 3,
            cap,
            "the legacy export silently caps"
        );
        let meta = t.search_meta("a", true, false);
        assert_eq!(
            (meta.match_count() as usize, meta.incomplete()),
            (cap, true)
        );
        let summary = t.search_summary("a", true, false, 1).expect("summary");
        assert!(
            summary.ends_with(&format!("\"total\":{cap},\"incomplete\":true}}")),
            "{summary}"
        );
    }

    /// The P7 grid-mirror batch export: one `{text, wrapped, len, widths?}`
    /// record per display row, `widths` only on a row holding a wide cell,
    /// and `undefined` for a range past the live grid (resize skew).
    #[test]
    fn row_range_json_batches_rows_matching_the_per_row_exports() {
        let mut t = crate::system_font_terminal(6, 20, 16.0);
        t.process(b"hello\r\nc\xe6\xbc\xa2x\r\n");
        let json = t.row_range_json(0, 3).expect("range available");
        assert_eq!(json.matches("\"text\":").count(), 3, "{json}");
        assert!(
            json.contains("{\"text\":\"hello\",\"wrapped\":false,\"len\":5}"),
            "a narrow row omits widths: {json}"
        );
        assert!(json.contains("\"text\":\"c漢x\""), "{json}");
        assert_eq!(
            json.matches("\"widths\":").count(),
            1,
            "only the wide row has widths: {json}"
        );
        assert!(
            json.contains("\"widths\":\"12"),
            "wide lead marked '2': {json}"
        );
        assert!(t.row_range_json(0, 999).is_none(), "past the grid");
    }

    /// The host's input-intent words (the JS-to-wasm contract) each note one
    /// console input of their kind and buy one frame; an unknown word is
    /// refused without scheduling one.
    #[test]
    fn console_input_words_note_one_input_each_and_refuse_unknowns() {
        use aterm_effects::kitty_pet::PetInputKind;

        let mut t = AtermTerminal::new(8, 48, BUNDLED_FACE, 14.0, 0, 0, 0, 0)
            .expect("the bundled face parses");
        let generation = t.host_visual_gen;
        assert!(!t.note_console_input("unknown"));
        assert_eq!(t.host_visual_gen, generation, "a refusal schedules nothing");
        for (word, kind) in [
            ("text", PetInputKind::Text),
            ("delete", PetInputKind::Delete),
            ("navigate", PetInputKind::Navigate),
            ("submit", PetInputKind::Submit),
            ("paste", PetInputKind::Paste),
            ("kill", PetInputKind::Kill),
            ("kill-forward", PetInputKind::KillForward),
        ] {
            let (seq, generation) = (t.effects.cursor_pet_console_input_seq(), t.host_visual_gen);
            assert!(t.note_console_input(word), "{word}");
            assert_eq!(t.effects.cursor_pet_console_input_seq(), seq + 1, "{word}");
            assert_eq!(t.effects.cursor_pet_console_input_kind(), Some(kind));
            assert!(t.host_visual_gen > generation, "{word} must wake one frame");
        }
    }

    /// The paste export is the engine's formatter on THIS instance's mode, so
    /// a JS host cannot get the order wrong: a planted `ESC[201~` (or its
    /// 8-bit `0x9B 2 0 1 ~` spelling) is inert text before the one closing
    /// guard is appended, so it can never end its own bracket and let the
    /// tail run as keystrokes.
    #[test]
    fn paste_export_wraps_only_sanitized_text_so_a_planted_terminator_stays_inert() {
        let mut t = AtermTerminal::new(24, 80, BUNDLED_FACE, 16.0, 0, 0, 0, 0)
            .expect("the bundled face parses");
        assert_eq!(
            t.format_paste("a\nb"),
            b"a\rb",
            "no guards until DECSET 2004"
        );
        t.process(b"\x1b[?2004h");
        assert_eq!(
            t.format_paste("safe\x1b[201~rm -rf ~"),
            b"\x1b[200~safe[201~rm -rf ~\x1b[201~"
        );
        assert_eq!(
            t.format_paste("a\u{009b}201~b"),
            b"\x1b[200~a201~b\x1b[201~",
            "the 8-bit CSI spelling of the terminator is stripped as well"
        );
    }

    /// Font handles: identical bytes share one handle and distinct bytes get
    /// their own, a handle builds an engine and installs the shared emoji
    /// copy, and an unknown handle is a catchable error, never a panic.
    #[test]
    fn registered_font_handles_dedupe_build_and_refuse_unknowns() {
        let handle = register_font(BUNDLED_FACE);
        assert_eq!(
            register_font(BUNDLED_FACE),
            handle,
            "identical bytes share one handle"
        );
        assert_ne!(
            register_font(b"other-bytes"),
            handle,
            "distinct bytes, distinct handles"
        );
        let mut t = AtermTerminal::new_registered(4, 20, handle, 16.0, 0, 0, 0, 0)
            .expect("registered ctor builds");
        t.set_emoji_font_registered(handle)
            .expect("the shared copy installs");
        assert!(t.set_fallback_font_registered(u32::MAX).is_err());
        assert!(AtermTerminal::new_registered(4, 20, u32::MAX, 16.0, 0, 0, 0, 0).is_err());
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

    /// `last_command_output`'s three answers (the host's "Copy Last Command
    /// Output" JSON): `undefined` without shell integration, the LATEST
    /// block's whole output with its exit code, and an honest `evicted`
    /// marker once the block's rows scroll past the cap, never shifted text.
    #[test]
    fn last_command_output_is_none_then_the_latest_block_then_evicted() {
        let mut t = crate::system_font_terminal(10, 40, 16.0);
        t.process(b"plain output, no OSC 133 anywhere\r\n$ ");
        assert!(t.last_command_output().is_none(), "no blocks: undefined");
        run_osc133_block(&mut t, "first", &["OLD-OUTPUT"], 0);
        run_osc133_block(&mut t, "second", &["NEW-LINE-1", "NEW-LINE-2"], 3);
        let json = t.last_command_output().expect("a completed block exists");
        assert!(json.starts_with(r#"{"status":"ok","text":"#), "{json}");
        assert!(
            json.contains("NEW-LINE-1") && json.contains("NEW-LINE-2"),
            "{json}"
        );
        assert!(
            !json.contains("OLD-OUTPUT"),
            "an older block leaked in: {json}"
        );
        assert!(json.ends_with(r#","exitCode":3}"#), "{json}");
        t.set_scrollback_limit(6);
        for i in 0..40 {
            t.process(format!("filler-{i}\r\n").as_bytes());
        }
        assert_eq!(
            t.last_command_output().as_deref(),
            Some(r#"{"status":"evicted"}"#)
        );
    }

    /// A host-minted scheme lets its OSC 8 link through to `link_at` as an
    /// OSC-8 hit spanning exactly its cells; revoking it shuts the door.
    #[test]
    fn a_minted_scheme_links_through_osc8_until_revoked() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        let link = |n: u8| format!("\x1b]8;;orca://focus/w{n}\x07YY\x1b]8;;\x07 plain\r\n");
        t.process(link(0).as_bytes());
        assert!(t.link_at(0, 0).is_none(), "an unminted custom scheme");
        assert!(t.authorize_hyperlink_scheme("orca"));
        t.process(link(1).as_bytes());
        let hit = t.link_at(1, 1).expect("a minted scheme must linkify");
        assert_eq!(
            (hit.url(), hit.start_col(), hit.end_col(), hit.kind()),
            ("orca://focus/w1".to_string(), 0, 2, 0)
        );
        t.revoke_hyperlink_scheme("orca");
        t.process(link(2).as_bytes());
        assert!(
            t.link_at(2, 0).is_none(),
            "a revoked scheme must stop linkifying"
        );
    }

    /// `serialize` is replayable ANSI: replayed into a fresh engine it
    /// reproduces the viewport AND lands the source's history in the target's
    /// scrollback. Regression: the layout prints history onto the screen and
    /// then paints the viewport with absolute CUP + erase-line; without the
    /// scroll-off epilogue that paint ERASED the last min(history, rows)
    /// history lines (all of it when history < rows).
    #[test]
    fn serialize_replay_lands_history_in_scrollback_not_under_the_viewport_paint() {
        let mut a = crate::system_font_terminal(6, 40, 16.0);
        for i in 0..9 {
            a.process(format!("hist {i}\r\n").as_bytes());
        }
        a.process(b"\x1b[2J\x1b[H\x1b[1;32mviewport\x1b[0m line\r\n$ ");
        let snapshot = a.serialize(None);

        let mut b = crate::system_font_terminal(6, 40, 16.0);
        b.process(snapshot.as_bytes());
        for r in 0..6 {
            assert_eq!(
                a.row_text(r),
                b.row_text(r),
                "viewport row {r} after replay"
            );
        }
        let source_hist = a.serialize_scrollback(None);
        assert!(
            source_hist.contains("hist 0"),
            "precondition: the source holds history"
        );
        assert_eq!(
            b.serialize_scrollback(None),
            source_hist,
            "replayed scrollback must match the source's history exactly"
        );
    }

    /// JS hands the binding raw u16 rows/cols; the grid clamps them to 1..=4096.
    /// Construction and `resize` must both re-sync `self.rows`/`self.cols` to the
    /// CLAMPED grid dims: sizing the framebuffer from the raw 65535×65535 is an
    /// unbounded alloc / wasm32 u32 overflow → OOB.
    #[test]
    fn oversized_dims_are_clamped_to_the_grid_bound() {
        let mut t = crate::system_font_terminal(u16::MAX, u16::MAX, 16.0);
        assert!(
            t.rows <= 4096 && t.cols <= 4096,
            "ctor clamps to grid bound"
        );
        assert_eq!(t.rows, t.term.grid().rows() as usize, "ctor syncs to grid");
        assert_eq!(t.cols, t.term.grid().cols() as usize, "ctor syncs to grid");
        t.resize(0, 0);
        assert_eq!((t.rows, t.cols), (1, 1), "resize(0) is clamped UP to 1");
        t.resize(u16::MAX, u16::MAX);
        assert!(
            t.rows <= 4096 && t.cols <= 4096,
            "resize clamps to grid bound"
        );
    }

    /// The persistent WindowCpu reuses unchanged rows across frames and the
    /// WF-1 gate skips settled ones; neither may change the picture. A pane
    /// drawn after every step (and re-rendered settled) equals ONE full render
    /// of the same final content, and that content rasterized real glyphs.
    #[test]
    fn incremental_and_gated_renders_equal_a_single_full_render() {
        let mut warm = crate::system_font_terminal(24, 80, 16.0);
        let mut fresh = crate::system_font_terminal(24, 80, 16.0);
        let steps: &[&[u8]] = &[b"$ ", b"ls -la\r\n", b"file one\r\n", b"\x1b[1;1HX"];
        for s in steps {
            warm.process(s);
            warm.render();
            warm.render();
            assert!(
                warm.last_render_skipped(),
                "the settled re-render takes the gate"
            );
            fresh.process(s);
        }
        fresh.render();
        assert_eq!(
            (warm.width(), warm.height()),
            (fresh.width(), fresh.height())
        );
        assert_eq!(
            warm.rgba(),
            fresh.rgba(),
            "damage-tracked and gated renders drifted"
        );
        let rgba = fresh.rgba();
        assert!(
            rgba.as_chunks::<4>().0.iter().any(|px| px[..] != rgba[..4]),
            "glyphs must produce non-background pixels"
        );
    }

    #[test]
    fn set_theme_repaints_an_idle_pane_to_the_new_background() {
        // With the persistent cache, an appearance-only change (no cell moved) would
        // be skipped by the row-diff — set_theme must force a full repaint so the
        // background actually re-themes on an otherwise-idle frame (CPU finding #7).
        let mut t = crate::system_font_terminal(24, 80, 16.0);
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
        let want = [(new_bg >> 16) as u8, (new_bg >> 8) as u8, new_bg as u8];
        assert_ne!(
            before.as_slice(),
            &want,
            "test is meaningful: bg actually changed"
        );
        assert_eq!(
            &t.rgba()[centre..centre + 3],
            &want,
            "an idle pane must repaint to the new theme bg after set_theme"
        );
    }

    /// `set_scrollback_limit(0)` is unlimited, never a zero-line cap, and a
    /// later shrink re-clamps a scrolled-back viewport to the new limit.
    #[test]
    fn scrollback_limit_zero_is_unlimited_and_a_shrink_reclamps_the_viewport() {
        let mut t = crate::system_font_terminal(5, 40, 16.0);
        t.set_scrollback_limit(0);
        for i in 0..300 {
            t.process(format!("line-{i}\r\n").as_bytes());
        }
        t.scroll_to_top();
        assert_eq!(t.display_offset(), 296, "0 kept every line");
        t.set_scrollback_limit(50);
        assert_eq!(
            t.display_offset(),
            50,
            "a shrink re-clamps a scrolled viewport"
        );
        assert_eq!(t.row_text(0).as_deref(), Some("line-246"));
    }

    /// A host that rebuilds a row PER-CELL (a buffer facade that walks any
    /// non-ASCII row through `cell_text` + `cell_is_wide`) must see the same
    /// display-relative rows `row_text` serves. Regression: the v0.49
    /// live-frame retarget of the shared cell-grapheme helper made every
    /// scrolled-back per-cell read return the LIVE screen, so a host walking
    /// restored scrollback (a box-drawing table) found no history row.
    /// The single-slot row cache behind those reads is keyed by content
    /// generation and display offset: a scroll or a write must refresh the
    /// row it cached. Triple-click is display-anchored the same way.
    #[test]
    fn scrolled_cell_reads_are_display_relative_like_row_text() {
        let mut t = crate::system_font_terminal(5, 20, 16.0);
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
        let agree = |t: &AtermTerminal, at: &str| {
            for r in 0..5u16 {
                assert_eq!(per_cell_row(t, r), row_text_trimmed(t, r), "{at}, row {r}");
            }
        };

        agree(&t, "live bottom");
        // Prime the cache with display row 0 at offset 0, then scroll: the
        // same (row) key at a new offset must re-resolve.
        let _ = per_cell_row(&t, 0);
        t.scroll_to_top();
        assert_eq!(
            per_cell_row(&t, 0),
            "│🦀line0│",
            "oldest line, rebuilt per-cell"
        );
        agree(&t, "top of retention");

        t.scroll_to_bottom();
        t.scroll_lines(2);
        agree(&t, "straddling the history/live boundary");
        assert_eq!(
            t.selection_line(3, 0),
            Some(row_text_trimmed(&t, 3)),
            "triple-click copies the row the user sees while straddling"
        );

        // A write bumps content_gen at an unchanged offset and row.
        t.scroll_to_bottom();
        let _ = per_cell_row(&t, 0);
        t.process(b"\x1b[2J\x1b[HZEBRA\r\n");
        assert_eq!(
            per_cell_row(&t, 0),
            "ZEBRA",
            "a write must refresh the cached row"
        );
    }

    #[test]
    fn display_row_cache_distinguishes_main_and_alt_screen() {
        // P4 regression: the single-slot display_row_cache backing cell_text /
        // cell_is_wide keyed (content_gen, display_offset, row) with NO
        // alt-screen bit. The main and alt grids keep INDEPENDENT content_gen
        // counters, so a main<->alt swap landing on a coinciding
        // content_gen+offset+row served one buffer's cell for the other.
        let mut t = crate::system_font_terminal(5, 20, 16.0);
        // MAIN: push content_gen well above a freshly-entered alt buffer's, then
        // park a distinct glyph at (0,0) and prime the per-row cache by reading it.
        for _ in 0..30 {
            t.process(b"\r\n");
        }
        t.process(b"\x1b[HM");
        let main_gen = t.term.grid().content_gen();
        assert_eq!(t.cell_text(0, 0), "M", "primed main cell at (0,0)");

        // Enter the alternate screen and drive its INDEPENDENT content_gen to
        // EXACTLY main's cached value, writing only to row 1 so (0,0) keeps
        // its "A": the (content_gen, display_offset, row) collision.
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

    /// `search` hands out `[abs_line, col, len]` triplets in absolute rows,
    /// and `scroll_search_line_into_view` lands a scrollback match on a
    /// visible row.
    #[test]
    fn search_finds_a_token_in_scrollback_and_scrolls_it_into_view() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
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
        assert_eq!((col, len), (0, 19));
        let origin = t.search_display_origin();
        assert!(line < origin, "the match is above the live viewport");
        assert_eq!(t.display_offset(), 0, "starts at the live bottom");
        t.scroll_search_line_into_view(line);
        let display_row = i64::from(line) - i64::from(origin) + t.display_offset() as i64;
        assert!(
            (0..24).contains(&display_row),
            "match landed on a visible row, got {display_row}"
        );
    }

    /// The mode getters the host reads follow the program: each DECSET mode
    /// defaults off and follows its set and reset, and DECSCUSR surfaces as
    /// its `CursorStyle` discriminant.
    #[test]
    fn host_mode_getters_follow_the_program() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        #[expect(clippy::type_complexity, reason = "a test's case table")]
        let modes: [(&str, fn(&AtermTerminal) -> bool); 3] = [
            ("1", AtermTerminal::is_app_cursor_mode),
            ("1007", AtermTerminal::is_alternate_scroll),
            ("1004", AtermTerminal::is_focus_event_mode),
        ];
        for (mode, get) in modes {
            assert!(!get(&t), "mode {mode} defaults off");
            t.process(format!("\x1b[?{mode}h").as_bytes());
            assert!(get(&t), "mode {mode} set");
            t.process(format!("\x1b[?{mode}l").as_bytes());
            assert!(!get(&t), "mode {mode} reset");
        }
        t.process(b"\x1b[5 q");
        assert_eq!(t.cursor_style(), 5, "DECSCUSR 5 → BlinkingBar");
        t.process(b"\x1b[2 q");
        assert_eq!(t.cursor_style(), 2, "DECSCUSR 2 → SteadyBlock");
    }

    /// `encode_key` speaks the LIVE keyboard mode the program negotiated, and
    /// the worker path's stateless `encode_key_with_mode`, fed the mirrored
    /// `keyboard_mode_bits`, is byte-identical at every step, including
    /// win32-input-mode, which only a host-declared ConPTY can turn on.
    #[test]
    fn encode_key_follows_the_live_mode_and_its_mirrored_bits() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        let win32_pair = &b"\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_"[..];
        for (step, program, conpty, key, mods, want) in [
            ("fresh", &b""[..], false, "ArrowUp", 0, &b"\x1b[A"[..]),
            (
                "DECCKM",
                &b"\x1b[?1h"[..],
                false,
                "ArrowUp",
                0,
                &b"\x1bOA"[..],
            ),
            ("no kitty", &b"\x1b[?1l"[..], false, "Enter", 1, &b"\n"[..]),
            (
                "kitty pushed",
                &b"\x1b[>1u"[..],
                false,
                "Enter",
                1,
                &b"\x1b[13;2u"[..],
            ),
            (
                "?9001h alone",
                &b"\x1b[<u\x1b[?9001h"[..],
                false,
                "Enter",
                1,
                &b"\n"[..],
            ),
            (
                "?9001h on ConPTY",
                &b"\x1b[?9001h"[..],
                true,
                "Enter",
                1,
                win32_pair,
            ),
        ] {
            if conpty {
                t.set_win32_input_enabled(true);
            }
            t.process(program);
            assert_eq!(
                t.encode_key(key, mods, 0, None).as_deref(),
                Some(want),
                "{step}"
            );
            assert_eq!(
                encode_key_with_mode(key, mods, 0, None, t.keyboard_mode_bits()).as_deref(),
                Some(want),
                "{step}: mirrored bits"
            );
        }
    }

    /// The host word-separator setting reshapes double-click words end-to-end
    /// (xterm.js `wordSeparators` semantics), and clearing it restores the
    /// engine's default class-based word logic exactly. A click mid-word
    /// selects the WHOLE word (the expand_semantic fix).
    #[test]
    fn set_word_separators_reshapes_double_click_words() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        t.process(b"foo-bar baz");
        assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo"));
        t.set_word_separators(Some(" ".to_string()));
        assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo-bar"));
        t.set_word_separators(None);
        assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo"));
    }

    /// The mouse surface the host drives: the motion getters follow
    /// 1000/1002/1003, and the encoders take the host's 0-based
    /// `(col, row, button, mods)` and wheel direction.
    #[test]
    fn mouse_getters_follow_the_mode_and_encoders_take_host_cells() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        assert!(!t.is_mouse_tracking(), "mouse tracking defaults off");
        assert!(t.encode_mouse_press(0, 0, 0, 0).is_none(), "no report off");
        t.process(b"\x1b[?1000h\x1b[?1006h");
        assert!(t.is_mouse_tracking() && !t.mouse_wants_motion(), "1000");
        assert_eq!(
            t.encode_mouse_press(4, 2, 0, 0).as_deref(),
            Some(&b"\x1b[<0;5;3M"[..])
        );
        assert_eq!(
            t.encode_mouse_wheel(4, 2, true, 0).as_deref(),
            Some(&b"\x1b[<64;5;3M"[..])
        );
        t.process(b"\x1b[?1002h");
        assert!(
            t.mouse_wants_motion() && !t.mouse_wants_any_motion(),
            "1002"
        );
        t.process(b"\x1b[?1003h");
        assert!(t.mouse_wants_any_motion(), "1003 reports any motion");
    }

    /// A link under the pointer is read from the cells it is drawn in (r16
    /// selection-stride): after two ideographs the URL's columns are its own
    /// and its text is the URL. Slicing the row's text one column a
    /// character read `tps://example.com/a`, no link at all.
    #[test]
    fn a_link_after_wide_characters_is_read_from_its_cells() {
        let mut t = AtermTerminal::new(4, 60, BUNDLED_FACE, 16.0, 0, 0, 0, 0)
            .expect("the bundled face parses");
        t.process("\u{65e5}\u{672c} https://example.com/a\r\n".as_bytes());
        for col in [5, 25] {
            let hit = t
                .link_at(0, col)
                .unwrap_or_else(|| panic!("column {col} is over the URL"));
            assert_eq!(
                (hit.url(), hit.start_col(), hit.end_col(), hit.kind()),
                ("https://example.com/a".to_string(), 5, 26, 1),
                "link at column {col}"
            );
        }
    }

    #[test]
    fn selection_range_reports_display_coords_and_none_off_screen() {
        let mut term = Terminal::new(5, 20);
        for i in 0..20 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        term.scroll_display(4);
        let offset = term.grid().display_offset() as i32;
        assert_eq!(offset, 4);

        // Select display rows 0..=1 — the host passes TERMINAL-relative rows
        // (display_row - offset), exactly as the wasm entry points do.
        {
            let sel = term.text_selection_mut();
            sel.start_selection(0 - offset, 0, SelectionSide::Left, SelectionType::Lines);
            sel.update_selection(1 - offset, 19, SelectionSide::Right);
            sel.complete_selection();
        }
        let r = selection_range_for(&term, 5, 20).expect("selection in viewport");
        assert_eq!((r.start_y(), r.end_y()), (0, 1), "display rows 0..=1");
        // And it agrees with selection_text (the scrollback lines, not live rows).
        let copied = term.selection_to_string().expect("text");
        let want0 = term.display_row_text(0).unwrap();
        let want1 = term.display_row_text(1).unwrap();
        assert_eq!(copied, format!("{want0}\n{want1}"));

        // Back at the live bottom the selection lies wholly above the viewport.
        term.scroll_to_bottom();
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
        let mut plain = crate::system_font_terminal(24, 80, 16.0);
        let mut toggled = crate::system_font_terminal(24, 80, 16.0);
        let content = b"a curious cat watched the build fail: fuck\r\n$ ";
        plain.process(content);
        plain.render();

        toggled.process(content);
        toggled.set_sparkle_words_enabled(true);
        toggled.set_cursor_glow(true, "phaser", None, None, 260, 24, 0.7, 0.6, true);
        toggled.set_cursor_trail(true, 260, 24, None);
        toggled.advance_effects(50.0);
        toggled.render();
        // A FIRE burn: type-then-erase ignites a REAL burn while leaving the
        // cell bytes and the cursor position identical to the plain engine.
        toggled.set_cursor_glow(true, "ember", None, None, 400, 64, 1.0, 2.0, true);
        toggled.process(b"z\x1b[D \x1b[D");
        toggled.advance_effects(50.0);
        toggled.render();
        assert_ne!(
            plain.rgba(),
            toggled.rgba(),
            "a live burn must visibly change the frame (non-vacuous toggle)"
        );
        // A PET: free sprites over the grid, enabled by seed, brought onto
        // the glass by a witnessed type-then-erase.
        toggled.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
        toggled.set_cursor_pet(true, 7);
        toggled.render();
        toggled.note_typed_char('p');
        toggled.process(b"p\x1b[D \x1b[D");
        let mut on_glass = false;
        for _ in 0..240 {
            toggled.advance_effects(33.0);
            toggled.render();
            if toggled.cursor_pet_alpha() > 0 {
                on_glass = true;
                break;
            }
        }
        assert!(on_glass, "the pet must reach the glass through the binding");
        assert_ne!(
            plain.rgba(),
            toggled.rgba(),
            "a drawn pet must visibly change the frame (non-vacuous toggle)"
        );
        toggled.set_cursor_pet(false, 7);
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
        let mut plain = crate::system_font_terminal(24, 80, 16.0);
        let mut chromed = crate::system_font_terminal(24, 80, 16.0);
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

        chromed.set_chrome(0, 0);
        chromed.render();
        assert_eq!((chromed.width(), chromed.height()), (w, h));
        assert_eq!(
            plain.rgba(),
            chromed.rgba(),
            "0/0 chrome must restore the byte-identical exact-fit frame"
        );
    }

    /// SPILL BAND through the REAL pipeline: an EMBERFORGE burn on the top
    /// row (the flame field rises above the grid: the effects box relaxes
    /// upward by exactly `head` under chrome) must light the TOP strip, and
    /// the band bytes `render` exports, packed as the four strips, must
    /// composite source-over onto the theme bg into exactly the rendered
    /// frame's band region (the seam-continuity law).
    #[test]
    fn spill_exports_track_band_content_through_the_real_pipeline() {
        let mut t = crate::system_font_terminal(12, 40, 16.0);
        let (pad, head) = (12usize, 30usize);
        t.set_chrome(pad as u16, head as u16);
        // The glow observes cursor motion FRAME-TO-FRAME, so keystrokes
        // interleave with renders, each WITNESSED through the committed-char
        // seam as a JS keydown handler would: the movement-admission gate
        // leaves unwitnessed motion dark.
        t.set_cursor_glow(true, "ember", None, None, 400, 64, 1.0, 2.0, true);
        // Seed the just-enabled engines' cursor anchor (apply is the anchor
        // authority) so the first witness has an owner to check against.
        t.render();
        for (ch, bytes) in [('a', b"a"), ('b', b"b"), ('c', b"c"), ('d', b"d")] {
            assert!(
                t.note_typed_char(ch),
                "a witnessed simple scalar must arm movement provenance"
            );
            t.process(bytes);
            t.advance_effects(30.0);
            t.render();
        }
        // A frame whose band coverage is LIVE (nonzero alpha), not merely
        // rev-ticked, or the parity below proves nothing.
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
            t.render();
        }
        assert!(lit_frame, "the witnessed burn must reach the band live");
        let (w, h) = (t.width(), t.height());
        let grid_h = h - 2 * pad - head;
        assert_eq!(
            t.spill_len(),
            (w * (pad + head) + w * pad + 2 * pad * grid_h) * 4,
            "spill buffer sized to the four band strips"
        );

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
    }

    // --- The resident pet through the binding (design 2026-08-30, Phase 1) --

    /// Drive the pet onto the glass through the binding alone: the glow
    /// style that names it, the seed door, then witnessed keystrokes at
    /// 33 ms, until `cursor_pet_alpha` goes positive. Returns the frame it
    /// took, or `None` when the budget ran out. Every character is typed
    /// through `note_typed_char` + echo exactly as a JS keydown handler
    /// would, so the caret the pet chases is a witnessed one.
    fn wake_the_pet(t: &mut AtermTerminal, seed: u64) -> Option<u32> {
        t.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
        t.set_cursor_pet(true, seed);
        // Seed the just-enabled engines' cursor anchor (apply is the anchor
        // authority) so the first witness below has an owner to check against.
        t.render();
        for frame in 0..240u32 {
            if frame % 6 == 0 {
                let ch = char::from(b'a' + u8::try_from(frame / 6 % 26).unwrap_or(0));
                t.note_typed_char(ch);
                t.process(&[ch as u8]);
            }
            t.advance_effects(33.0);
            t.render();
            if t.cursor_pet_alpha() > 0 {
                return Some(frame);
            }
        }
        None
    }

    /// Advance 100 ms frames with no input until the binding reports idle,
    /// or the budget runs out — then render ONCE more. The gate key is read
    /// before the pipeline advances, so the render that first observes idle
    /// was keyed active; the active→idle edge changes the key on the NEXT
    /// render, which the gate's contract buys as the one settle frame that
    /// paints the released overlay channels (`frame_gate_skips_settled_
    /// frames_and_reopens_on_every_change_class` pins the same one-frame
    /// shape). After a `true` here the next `render()` takes the gate.
    fn settle(t: &mut AtermTerminal, budget_frames: u32) -> bool {
        for _ in 0..budget_frames {
            t.advance_effects(100.0);
            t.render();
            if !t.is_effects_active() {
                t.render();
                return true;
            }
        }
        false
    }

    /// The seed door is the ONLY entropy: two instances fed the same seed,
    /// the same bytes and the same `dt` stream are pixel-identical at every
    /// step (the engine is clockless and dieless — nothing in-wasm rolls a
    /// die), and a different seed dresses a visibly different cat (the
    /// non-vacuity half: the seed actually reaches the coat).
    #[test]
    fn cursor_pet_is_deterministic_for_one_seed() {
        let mut a = crate::system_font_terminal(24, 80, 16.0);
        let mut b = crate::system_font_terminal(24, 80, 16.0);
        let mut c = crate::system_font_terminal(24, 80, 16.0);
        const SEED: u64 = 7;
        const OTHER_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
        let (look, other) = (
            aterm_effects::kitty_registry::KittyLook::for_launch(SEED),
            aterm_effects::kitty_registry::KittyLook::for_launch(OTHER_SEED),
        );
        assert_ne!(
            (look.coat, look.iris),
            (other.coat, other.iris),
            "pick seeds that decode to different coats, or the second half is vacuous"
        );
        for (t, seed) in [(&mut a, SEED), (&mut b, SEED), (&mut c, OTHER_SEED)] {
            t.process(b"$ ");
            assert!(
                wake_the_pet(t, seed).is_some(),
                "the pet must reach the glass through the binding"
            );
        }
        let mut other_seed_differs = false;
        for step in 0..40u32 {
            let ch = char::from(b'a' + u8::try_from(step % 26).unwrap_or(0));
            for t in [&mut a, &mut b, &mut c] {
                if step % 8 == 0 {
                    t.process(b"\r\n");
                }
                t.note_typed_char(ch);
                t.process(&[ch as u8]);
                t.advance_effects(33.0);
                t.render();
            }
            assert_eq!(
                a.rgba(),
                b.rgba(),
                "same seed + same bytes + same dt stream must be pixel-identical (step {step})"
            );
            other_seed_differs |= a.rgba() != c.rgba();
        }
        assert!(
            other_seed_differs,
            "a different seed must dress a visibly different cat"
        );
    }

    /// The pet defaults OFF. The pointer and the press are value-shadowed
    /// doors on the WF-1 gate: a pointer sample that moved reopens it, the
    /// same sample again does not; a leave reopens it once; a press outside
    /// the body passes (`0`) and leaves the gate shut, a press inside pets
    /// (`1`) and buys the frame that runs the purr. So an idle hover cannot
    /// delete the gate and a stroked cat cannot be a stale frame.
    #[test]
    fn pointer_and_pet_press_reopen_the_frame_gate() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        assert!(!t.cursor_pet_enabled(), "the pet defaults OFF");
        assert_eq!(t.cursor_pet_alpha(), 0, "nothing on glass before opt-in");
        t.process(b"$ ");
        assert!(
            wake_the_pet(&mut t, 7).is_some(),
            "the pet must reach the glass through the binding"
        );
        assert!(settle(&mut t, 600), "the pet must sleep first");
        t.render();
        assert!(t.last_render_skipped(), "settled frame must take the gate");

        // A moved pointer reopens; the same sample again does not.
        let (px, py) = (t.width() as f32 * 0.5, t.height() as f32 * 0.5);
        t.note_pointer_px(px, py);
        assert!(t.needs_frame(), "a moved pointer must reopen the gate");
        t.render();
        assert!(!t.last_render_skipped(), "the pointer frame must draw");
        assert!(settle(&mut t, 600), "a watching cat settles again");
        t.render();
        assert!(t.last_render_skipped(), "re-settled after the pointer");
        t.note_pointer_px(px, py);
        assert!(
            !t.needs_frame(),
            "an identical pointer sample is value-shadowed and must not reopen the gate"
        );
        t.render();
        assert!(t.last_render_skipped(), "an idle hover must gate");
        t.note_pointer_px(f32::NAN, py);
        assert!(!t.needs_frame(), "a non-finite sample is dropped");

        // A leave reopens once.
        t.note_pointer_leave();
        assert!(t.needs_frame(), "the pointer leaving must reopen the gate");
        t.render();
        assert!(!t.last_render_skipped(), "the leave frame must draw");
        assert!(settle(&mut t, 600), "the cat settles after the leave");
        t.render();
        assert!(t.last_render_skipped(), "re-settled after the leave");
        t.note_pointer_leave();
        assert!(!t.needs_frame(), "a second leave is value-shadowed");

        // A press outside passes and gates; a press inside pets and draws.
        let (x0, x1, y0, y1) = t
            .effects
            .companion_hit_rect()
            .expect("a sleeping cat on glass has a hit target");
        let outside = ((x1 + 64) as f32, (y1 + 64) as f32);
        assert_eq!(t.pet_press_px(outside.0, outside.1), 0, "outside passes");
        assert!(
            !t.needs_frame(),
            "a pass changed nothing the gate cannot see"
        );
        t.render();
        assert!(t.last_render_skipped(), "a passed press must gate");
        let inside = (((x0 + x1) / 2) as f32, ((y0 + y1) / 2) as f32);
        assert_eq!(t.pet_press_px(inside.0, inside.1), 1, "inside pets");
        assert!(
            t.needs_frame(),
            "a petted cat needs the frame that runs the purr"
        );
        t.render();
        assert!(!t.last_render_skipped(), "the petting frame must draw");
    }

    /// A walking pet is free sprites over the grid, so its motion presents
    /// as SUB-FRAME bands (the rows the sprite vacated and entered), never
    /// a full-frame band, and the partially-updated persistent buffer stays
    /// byte-exact against a from-scratch expansion of the same frame — the
    /// dirty-band oracle of `dirty_band_present_api`.
    #[test]
    fn walking_pet_presents_sub_frame_bands() {
        let mut t = crate::system_font_terminal(8, 40, 14.0);
        let full_expansion = |t: &AtermTerminal| {
            let mut rgba = Vec::new();
            let mut bands = Vec::new();
            crate::dirty_band_present_api::expand_full(
                t.win.frame_pixels(),
                &mut rgba,
                &mut bands,
                t.width,
                t.height,
            );
            rgba
        };
        t.process(b"$ ");
        assert!(
            wake_the_pet(&mut t, 7).is_some(),
            "the pet must reach the glass through the binding"
        );
        let full = (t.width * t.height) as i32;
        for ch in "walk".chars().cycle().take(24) {
            t.note_typed_char(ch);
            t.process(ch.to_string().as_bytes());
            t.advance_effects(33.0);
            t.render();
            assert!(!t.last_render_skipped(), "a walking cat never gates");
            let banded_px: i32 = t
                .present_bands
                .as_chunks::<4>()
                .0
                .iter()
                .map(|q| q[2] * q[3])
                .sum();
            assert!(
                t.present_band_count() >= 1,
                "a walking frame must surface as at least one band"
            );
            assert!(
                banded_px < full,
                "a walking cat never pays a full-frame band ({banded_px} of {full} px)"
            );
            assert_eq!(
                t.rgba,
                full_expansion(&t),
                "the banded update is byte-exact against a full expansion"
            );
        }
        assert!(
            t.cursor_pet_alpha() > 0,
            "the cat was on glass while walking"
        );
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

    /// The inline bound splits the two resize paths: a history at or under
    /// `INLINE_REFLOW_MAX_LINES` rewraps inline, mirroring the native bound;
    /// a deeper one detaches, and any schedule of small bounded pumps
    /// re-attaches it with its content intact (aterm-grid's
    /// schedule-independence property, asserted here as the JS-visible
    /// outcome: counts + oldest line).
    #[test]
    fn small_history_rewraps_inline_and_a_deep_one_defers_to_stepped_pumps() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        install_tiered_history(&mut t, 24, 80, 500);
        let before = t.term.grid().scrollback_lines();
        assert!(before > 100, "precondition: deep history ({before} lines)");
        t.resize(24, 40);
        assert!(!t.reflow_pending(), "a small history rewraps inline");
        assert!(
            t.term.grid().scrollback_lines() >= before,
            "inline rewrap preserves history"
        );
        assert!(t.serialize_scrollback(None).contains("L0-hist"));

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

    /// A second width change during the detach window, whether the job is
    /// still stashed or already half-stepped, supersedes without losing
    /// history: the store is out, so nothing re-detaches; the job keeps its
    /// progress and re-attaches (content valid, wrapping stale for the newest
    /// width), and the NEXT width change re-detaches and re-wraps it (the
    /// self-heal, the native supersede semantics).
    #[test]
    fn a_resize_during_the_window_supersedes_without_losing_history() {
        for half_stepped in [false, true] {
            let mut t = crate::system_font_terminal(24, 80, 16.0);
            install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
            let before = t.term.grid().scrollback_lines();
            t.resize(24, 40);
            assert!(t.reflow_pending());
            if half_stepped {
                t.pump_reflow_budget(500);
                for _ in 0..3 {
                    assert!(t.pump_reflow(), "mid-stepping: work must remain");
                }
            }
            t.resize(24, 60);
            assert!(
                t.reflow_pending(),
                "half_stepped={half_stepped}: the job survives"
            );
            assert_eq!(t.cols, 60, "the newest geometry wins for the grid");
            while t.pump_reflow() {}
            let after = t.term.grid().scrollback_lines();
            assert!(
                after >= before,
                "half_stepped={half_stepped}: no history lost across the supersede \
                 (before={before}, after={after})"
            );
            t.resize(24, 30);
            assert!(
                t.reflow_pending(),
                "stale wrapping re-detaches and re-wraps"
            );
            while t.pump_reflow() {}
            assert!(
                t.serialize_scrollback(None).contains("L0-hist"),
                "half_stepped={half_stepped}: oldest line intact after the supersede"
            );
        }
    }

    #[test]
    fn never_pumped_host_completes_via_the_render_grace_net() {
        // SAFETY ARGUMENT UNDER TEST: a host that never calls `pump_reflow`
        // (an un-updated embedder) must still get its history back — after
        // the grace window `render` pumps ONE bounded step per frame, so the
        // job CONVERGES across frames (never the whole rewrap in one frame).
        let mut t = crate::system_font_terminal(24, 80, 16.0);
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
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        t.resize(24, 40);
        assert!(t.reflow_pending());

        // Blow past the backlog cap in one burst (no single pump can close
        // the window)…
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

    /// Driving the budgeted export to completion yields exactly the legacy
    /// one-shot triplets, cursor discipline included (Some while incomplete,
    /// None at completion); an empty query or a bad regex answers at once,
    /// empty and complete, like the legacy export's silence.
    #[test]
    fn search_budgeted_resumes_to_the_one_shot_result() {
        let mut t = crate::system_font_terminal(6, 40, 16.0);
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
        assert_eq!(all_matches, one_shot, "budgeted != one-shot");
        assert!(!step.incomplete_index(), "small buffer is exhaustive");
        for (query, regex) in [("", false), ("f(oo", true)] {
            let r = t.search_budgeted(query, false, regex, None, 5);
            assert!(
                r.complete() && r.matches().is_empty() && r.reset() && r.search_id().is_none(),
                "{query:?}"
            );
        }
    }

    /// DMG-1 ON THE WEB PATH: across a mutation stream (typing, wraps and
    /// scrolls, erases, SGR, an OSC 11 recolour, the alt screen, a history
    /// viewport and a resize) with the damage-reading effects on, every
    /// rendered frame's engine channels equal a FULL extraction of the same
    /// terminal, and an echo re-resolves ONE row. The host extracts through
    /// `cell_frame_damage_scoped_into` (which consumes the damage session),
    /// and the effects pipeline closes a session only when one is still open,
    /// so sparkle words and rain do not re-take it; a second `take_damage`
    /// would force `Full { cause: DamageTaken }` on every echo. The oracle is
    /// seeded from the rendered scratch (so the host-owned overlay channels
    /// the effects wrote are carried over) and refilled with
    /// `cell_frame_into`, which rewrites every engine channel — so only an
    /// engine divergence can make the two differ. Both arms must be reached,
    /// or the equality proves nothing about the scoped one.
    #[test]
    fn web_path_scoped_refill_matches_a_full_extraction_over_a_mutation_stream() {
        use aterm_core::render::FrameRefill;
        let mut t = crate::system_font_terminal(8, 40, 14.0);
        t.set_sparkle_words_enabled(true);
        t.set_matrix_rain_enabled(true);
        let steps: &[&[u8]] = &[
            b"$ ",
            b"e",
            b"c",
            b"ho hello cat",
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
        let mut check = |t: &mut AtermTerminal, what: &str| {
            let (rows, cols) = (t.rows, t.cols);
            let mut oracle = t.frame_scratch.clone();
            t.term.cell_frame_into(&mut oracle, rows, cols);
            assert!(
                t.frame_scratch == oracle,
                "the rendered frame diverged from a full extraction after: {what}"
            );
            match t.last_refill {
                Some(FrameRefill::Scoped { .. }) => scoped += 1,
                Some(FrameRefill::Full { .. }) => full += 1,
                None => panic!("render must refill"),
            }
        };
        for step in steps {
            t.process(step);
            t.advance_effects(16.0);
            t.render();
            if *step == b"e" || *step == b"c" {
                assert_eq!(
                    t.last_refill,
                    Some(FrameRefill::Scoped { rows_refilled: 1 }),
                    "an echo must refill exactly its one row"
                );
            }
            check(&mut t, &String::from_utf8_lossy(step));
        }
        t.scroll_lines(3);
        t.render();
        check(&mut t, "history viewport");
        t.scroll_lines(-3);
        t.render();
        check(&mut t, "back to the live bottom");
        t.resize(10, 30);
        t.render();
        check(&mut t, "resize");
        t.process(b"after resize");
        t.render();
        check(&mut t, "echo after resize");
        assert!(scoped >= 5, "the scoped arm must be reached ({scoped})");
        assert!(full >= 2, "the full arm must be reached ({full})");
    }

    /// WF-1 frame gate, two-sided. Side 1 (skip): a second `render()` with
    /// nothing changed must take the gate (zero present bands, `rgba`
    /// byte-identical). Side 2 (reopen): every class of change the gate
    /// folds — grid damage, blink phase, selection, viewport scroll — must
    /// defeat the skip. This is the reach fence: if the gate silently
    /// stopped firing (or fired always), one of the two sides fails.
    #[test]
    fn frame_gate_skips_settled_frames_and_reopens_on_every_change_class() {
        let mut t = crate::system_font_terminal(8, 40, 14.0);
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

        // REOPEN side 2: renderer-held blink phase (no grid damage).
        t.render();
        assert!(t.last_render_skipped(), "re-settled before blink");
        t.set_cursor_blink_phase(false);
        t.render();
        assert!(!t.last_render_skipped(), "a blink flip must draw");
        t.set_cursor_blink_phase(false); // same phase again: shadow de-dups
        t.render();
        assert!(
            t.last_render_skipped(),
            "an idempotent blink re-assert must gate"
        );

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

    /// The drag half of the WF-1 gate: `selection_extend` bumps ONLY when the
    /// resolved selection actually moved in cell coordinates. Skip side: a
    /// cell-identical (sub-cell pointer) extend gates. Draw side: a one-cell
    /// move draws, and so does an extend with IDENTICAL arguments after the
    /// viewport scrolled under a stationary pointer (an argument shadow, not
    /// a comparison of the engine's resolved `TextSelection`, would lose it).
    /// The de-dup must never change the picture.
    #[test]
    fn redundant_selection_extend_gates_but_every_real_move_still_draws() {
        let mut t = crate::system_font_terminal(8, 40, 14.0);
        for _ in 0..20 {
            t.process(b"a line of selectable text\r\n");
        }
        t.render();

        // ---- SKIP side: cell-identical jitter ------------------------------
        t.selection_start(2, 4);
        t.selection_extend(2, 10);
        t.render();
        assert!(!t.last_render_skipped(), "the first extend must draw");
        let baseline = t.rgba();
        let span = t.selection_text();
        t.selection_extend(2, 10); // the pointer moved sub-cell: same cell
        assert!(
            !t.needs_frame(),
            "a redundant extend must not reopen the gate"
        );
        t.render();
        assert!(
            t.last_render_skipped(),
            "a cell-identical extend must be gated away"
        );
        assert_eq!(t.present_band_count(), 0, "gated frame exports zero bands");
        assert_eq!(t.rgba(), baseline, "gated frame retains the framebuffer");
        assert_eq!(t.selection_text(), span, "and the selection is unchanged");

        // ---- DRAW side 1: a genuine one-cell move --------------------------
        t.selection_extend(2, 11);
        assert!(t.needs_frame(), "a one-cell move must reopen the gate");
        t.render();
        assert!(!t.last_render_skipped(), "a one-cell move must draw");
        assert_ne!(t.selection_text(), span, "and it really moved");

        // ---- DRAW side 2: the viewport scrolls under a stationary pointer --
        // `selection_extend` takes a DISPLAY row; a scroll moves the
        // terminal-relative anchor under an unmoved display row, so the SAME
        // arguments name a different cell of the buffer.
        t.selection_finish();
        t.render();
        t.render();
        assert!(t.last_render_skipped(), "re-settled before the scroll");
        t.selection_start(3, 2);
        t.selection_extend(3, 12);
        t.render();
        let before_scroll = t.selection_text();
        t.scroll_lines(2);
        t.render();
        t.render();
        assert!(t.last_render_skipped(), "re-settled after the scroll");
        t.selection_extend(3, 12); // identical ARGUMENTS, different cell
        assert!(
            t.needs_frame(),
            "a scroll under a stationary pointer must still reopen the gate"
        );
        t.render();
        assert!(
            !t.last_render_skipped(),
            "an extend whose terminal-relative anchor moved must draw"
        );
        assert_ne!(
            t.selection_text(),
            before_scroll,
            "and the selection really did change"
        );

        // ---- Pixel invisibility: the gate must never change the picture ----
        let mut fresh = crate::system_font_terminal(8, 40, 14.0);
        for _ in 0..20 {
            fresh.process(b"a line of selectable text\r\n");
        }
        // The SAME gesture in the same order, but rendered only ONCE at the end
        // — so if the de-dup had swallowed a real update, `t` would differ here.
        fresh.selection_start(3, 2);
        fresh.selection_extend(3, 12);
        fresh.scroll_lines(2);
        fresh.selection_extend(3, 12);
        fresh.render();
        assert_eq!(
            t.rgba(),
            fresh.rgba(),
            "de-duping redundant extends must be pixel-invisible"
        );
    }

    /// Glass floors glyphs on the web too: a low-contrast SGR fg (#2A2D35 on
    /// the #111318 default) gets an opaque glyph pixel at >= 4.5:1 under 0.6
    /// opacity with no `set_minimum_contrast` call. Negative control: the
    /// opaque frame paints nothing near that contrast.
    #[test]
    fn web_translucency_floors_the_rendered_glyphs() {
        let face = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
        let bg = aterm_types::Rgb::new(0x11, 0x13, 0x18);
        let best = |opacity: f32| {
            let mut t = AtermTerminal::new(
                1,
                4,
                face,
                32.0,
                0x00d0_d0d0,
                0x0011_1318,
                0x00ff_ffff,
                0x0040_4040,
            )
            .expect("the bundled face parses");
            t.set_background_opacity(opacity);
            t.process(b"\x1b[?25l\x1b[38;2;42;45;53mXXXX");
            t.render();
            let rgba = t.rgba();
            rgba.as_chunks::<4>()
                .0
                .iter()
                .filter(|&&[_, _, _, a]| a == 255)
                .map(|&[r, g, b, _]| aterm_types::Rgb::new(r, g, b).contrast(bg))
                .fold(0.0_f64, f64::max)
        };
        let glass = best(0.6);
        assert!(glass >= 4.5, "glass floored the glyphs to only {glass}:1");
        let opaque = best(1.0);
        assert!(
            opaque < 4.5,
            "control: the unfloored glyph is sub-AA ({opaque}:1)"
        );
    }
}
