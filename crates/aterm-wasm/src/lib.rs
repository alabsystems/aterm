// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! In-page WebAssembly terminal engine and CPU RGBA8 renderer (`aterm-wasm`).

#[cfg(target_arch = "wasm32")]
use aterm_wasm_abi::export;

#[cfg(target_arch = "wasm32")]
aterm_wasm_abi::export_runtime!();

mod dirty_band_present_api;
use crate::AtermTerminal as WebTerminal;

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
mod frame_geometry_api;
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

/// Histories up to this many lines are rewrapped inline by `resize`, matching
/// aterm-gui's `INLINE_REFLOW_MAX_LINES`.
const INLINE_REFLOW_MAX_LINES: usize = 20_000;

/// `render()` calls after a rewrap is stashed before `render` pumps it itself.
const REFLOW_PUMP_GRACE_RENDERS: u32 = 120;

/// While a rewrap is stashed, output that scrolls off stages in the grid's lazy buffer.
const REFLOW_BACKLOG_MAX_LINES: usize = 20_000;

/// Default per-pump rewrap budget in input history lines for
/// [`AtermTerminal::pump_reflow`] (tunable via [`AtermTerminal::pump_reflow_budget`]).
const REFLOW_STEP_BUDGET_LINES: usize = 2_000;

std::thread_local! {
    /// Host-registered font blobs referenced by `u32` handle across panes.
    static REGISTERED_FONTS: std::cell::RefCell<Vec<std::sync::Arc<Vec<u8>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Register a font byte blob and return its content-interned handle.
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

#[inline]
fn registered_font(handle: u32) -> Result<std::sync::Arc<Vec<u8>>, String> {
    REGISTERED_FONTS.with(|cell| {
        cell.borrow()
            .get(handle as usize)
            .cloned()
            .ok_or_else(|| format!("unregistered font handle {handle}"))
    })
}

/// This module's identity record: compiled in unstamped, rewritten after the link by `ship site`
/// with the version and commit of the source it builds the module from
/// (`aterm_stamp::rewrite`), and installed by the first terminal, so the engine reports that
/// version (XTVERSION) rather than `0.0.0`.
#[cfg(target_arch = "wasm32")]
static STAMP: [u8; aterm_stamp::LEN] = aterm_stamp::UNSTAMPED_RECORD;

/// Terminal state machine and CPU RGBA8 framebuffer renderer.
#[cfg_attr(target_arch = "wasm32", export)]
pub struct AtermTerminal {
    term: Terminal,
    renderer: Renderer,
    rows: usize,
    cols: usize,
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    win: WindowCpu,
    force_full_repaint: bool,
    frame_scratch: RenderInput,
    #[cfg(test)]
    last_refill: Option<aterm_core::render::FrameRefill>,
    smart: SmartSelection,
    pub(crate) effects: EffectsPipeline,
    pub(crate) theme_cursor: u32,
    pub(crate) theme_fg: u32,
    pub(crate) theme_bg: u32,
    pub(crate) notifications: notifications_api::NotificationQueue,
    pub(crate) band: messages_api::WebBand,
    pub(crate) scroll_input: scroll_input_api::ScrollInputState,
    pub(crate) predict: aterm_predict::Predictor,
    pub(crate) pred_row_scratch: predict_api::PredRows,
    pub(crate) spill: SpillBand,
    pending_reflow: Option<PendingScrollbackReflow>,
    reflow_grace: u32,
    reflow_budget: usize,
    display_row_cache: std::cell::RefCell<facade_api::DisplayRowCache>,
    pub(crate) budget_share: aterm_core::terminal::scrollback_shared_budget::ScrollbackBudgetShare,
    pub(crate) present_bands: Vec<i32>,
    pub(crate) last_present_frac: i32,
    host_visual_gen: u64,
    last_frame_key: Option<FrameGateKey>,
    last_render_gated: bool,
    blink_phase_shadow: Option<bool>,
    hollow_shadow: Option<bool>,
    pub(crate) web_theme: theme_api::WebThemeState,
}

/// The frame-gate key: every input of a rendered frame that can change between two
/// `render()` calls at stable dims and config.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FrameGateKey {
    damage_epoch: u64,
    host_visual_gen: u64,
    effects_active: bool,
}

/// Keep the engine's implicit cells and OSC 110/111/112/117 reset baselines
/// aligned with the renderer theme.
#[inline]
fn apply_terminal_theme_colors(term: &mut Terminal, fg: u32, bg: u32, cursor: u32, selection: u32) {
    let mut base = term.color_baseline();
    base.foreground = theme_api::unpack(fg);
    base.background = theme_api::unpack(bg);
    base.cursor = Some(theme_api::unpack(cursor));
    base.selection_background = Some(theme_api::unpack(selection));
    term.rebase_colors(&base);
}

impl AtermTerminal {
    /// Refill every engine-owned frame channel and consume the frame's damage session
    /// through the engine's one damage consumer,
    /// [`Terminal::cell_frame_damage_scoped_into`], as the native GUI does.
    #[inline]
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
}

/// The renderer half of a theme install, which `theme_api` (shared with
/// aterm-gpu-web) calls on whichever crate compiles it.
impl AtermTerminal {
    /// Borrow the underlying CPU [`Renderer`].
    #[inline]
    pub(crate) const fn render_face(&self) -> &Renderer {
        &self.renderer
    }

    /// Apply theme colors to the renderer, effects pipeline, and chrome band.
    pub(crate) fn install_renderer_theme(&mut self, parts: aterm_types::scheme::ThemeParts) {
        let aterm_types::scheme::ThemeParts { fg, bg, cursor, .. } = parts;
        self.force_full_repaint = true;
        self.theme_cursor = cursor & 0x00FF_FFFF;
        self.theme_fg = fg & 0x00FF_FFFF;
        self.theme_bg = bg & 0x00FF_FFFF;
        self.effects
            .set_matrix_rain_theme(self.theme_bg, self.theme_fg, self.theme_cursor);
        self.effects.invalidate_companion_colors();
        self.band.set_theme(fg, bg, cursor);
        self.renderer.set_theme(Theme::from(parts));
    }

    /// Apply the selection foreground override to the renderer.
    #[inline]
    pub(crate) fn install_renderer_selection_fg(&mut self, fg: Option<u32>) {
        self.force_full_repaint = true;
        self.renderer.set_selection_fg(fg);
    }

    /// Test helper: render one frame to settle the frame gate.
    #[cfg(test)]
    pub(crate) fn settle_frame(&mut self) {
        self.render().expect("renderable frame");
    }
}

#[cfg_attr(target_arch = "wasm32", export)]
impl AtermTerminal {
    /// Construct a `rows`×`cols` terminal rendered with `font_bytes` at `px` font size.
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
        #[cfg(target_arch = "wasm32")]
        aterm_stamp::install(&STAMP, aterm_stamp::Stamp::UNSTAMPED);
        let theme = Theme {
            fg,
            bg,
            cursor,
            selection,
        };
        let mut renderer = Renderer::from_bytes(font_bytes, px, theme)?;
        renderer.set_runtime_font_discovery(false);
        renderer.set_text_shaping(aterm_render::TextShapingConfig {
            ligature_mode: aterm_render::LigatureMode::Enabled,
            ..Default::default()
        });
        let (rows, cols) = frame_geometry_api::checked_grid_size(&renderer, rows, cols, 0)?;
        let mut term = scrollback_tiers_api::tiered_terminal(rows, cols);
        let budget_share = scrollback_tiers_api::register_budget_share(&term);
        apply_terminal_theme_colors(&mut term, fg, bg, cursor, selection);
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
            display_row_cache: std::cell::RefCell::new(facade_api::DisplayRowCache::default()),
            host_visual_gen: 0,
            last_frame_key: None,
            last_render_gated: false,
            blink_phase_shadow: None,
            hollow_shadow: None,
            web_theme,
        })
    }

    /// Replace the fallback font chain with a single fallback face from `bytes`.
    pub fn set_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_fallback_bytes(bytes)
    }

    /// Append a fallback font face from `bytes` to the fallback chain.
    pub fn add_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.add_fallback_bytes(bytes)
    }

    /// Set the color-emoji (`sbix`) font face from `bytes`.
    pub fn set_emoji_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_color_font_bytes(bytes.to_vec())
    }

    /// Set the bold primary font face from `bytes`.
    pub fn set_bold_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_bold_font(bytes)
    }

    /// Set the symbol fallback font face from `bytes`.
    pub fn set_symbol_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_symbol_fallback_bytes(bytes)
    }

    /// Replace the primary font face from `bytes` and re-rasterize.
    pub fn set_primary_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.force_full_repaint = true;
        self.renderer.set_primary_font(bytes)
    }

    /// [`Self::set_fallback_font`] using a [`register_font`] handle.
    pub fn set_fallback_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.set_fallback_font(&bytes)
    }

    /// [`Self::add_fallback_font`] using a [`register_font`] handle.
    pub fn add_fallback_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.add_fallback_font(&bytes)
    }

    /// [`Self::set_emoji_font`] using a [`register_font`] handle.
    pub fn set_emoji_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.force_full_repaint = true;
        self.renderer.set_color_font_arc(bytes)
    }

    /// [`Self::set_bold_font`] using a [`register_font`] handle.
    pub fn set_bold_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.set_bold_font(&bytes)
    }

    /// [`Self::set_symbol_font`] using a [`register_font`] handle.
    pub fn set_symbol_font_registered(&mut self, handle: u32) -> Result<(), String> {
        let bytes = registered_font(handle)?;
        self.set_symbol_font(&bytes)
    }

    /// Set the cell line-height multiplier (`scale`) and re-rasterize.
    pub fn set_line_height(&mut self, scale: f32) {
        self.force_full_repaint = true;
        self.renderer.set_line_height(scale);
    }

    /// Configure window-space chrome padding (`pad`) and header height (`head`) in pixels.
    pub fn set_chrome(&mut self, pad: u16, head: u16) -> Result<(), String> {
        let (cw, ch) = self.renderer.cell_size();
        self.checked_surface_geometry(cw, ch, usize::from(pad), usize::from(head))?;
        self.force_full_repaint = true;
        self.renderer.set_pad(pad as usize);
        self.renderer.set_head(head as usize);
        self.effects.set_chrome(pad, head);
        Ok(())
    }

    /// Chrome edge padding in pixels set via [`Self::set_chrome`].
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn chrome_pad(&self) -> u16 {
        self.renderer.pad() as u16
    }

    /// Chrome top header band height in pixels set via [`Self::set_chrome`].
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn chrome_head(&self) -> u16 {
        self.renderer.head() as u16
    }

    /// Enable or disable programming ligatures (`calt`/`liga`).
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

    /// Set OpenType font features from a space-separated tag spec.
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

    /// Set 256-color palette entry `index` to `(r, g, b)`.
    pub fn set_palette_color(&mut self, index: u8, r: u8, g: u8, b: u8) {
        self.force_full_repaint = true;
        self.term.set_palette_color_components(index, r, g, b);
        self.band.set_palette_color(index, [r, g, b]);
    }

    /// Set the cursor blink visibility phase for the next frame.
    pub fn set_cursor_blink_phase(&mut self, on: bool) {
        // Frame gate: blink is renderer-held, invisible to the damage epoch. The shadow
        // keeps a timer re-asserting the same phase from forcing a render.
        if self.blink_phase_shadow != Some(on) {
            self.blink_phase_shadow = Some(on);
            self.note_host_visual_change();
        }
        self.renderer.set_cursor_blink_phase(on);
    }

    /// Force a hollow unfocused cursor (`true`) or restore DECSCUSR style (`false`).
    pub fn set_cursor_hollow(&mut self, hollow: bool) {
        // Frame gate: a renderer-held override, invisible to the damage epoch.
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

    /// Drain accumulated missing-font class bits (`1` = text/mono, `2` = emoji).
    pub fn take_missing_font_classes(&mut self) -> u8 {
        self.renderer.take_missing_font_classes()
    }

    /// Set the default foreground color `(r, g, b)` for OSC 10 replies and rendering.
    pub fn set_default_foreground(&mut self, r: u8, g: u8, b: u8) {
        self.force_full_repaint = true;
        self.term.set_default_foreground(Rgb { r, g, b });
    }

    pub fn set_default_background(&mut self, r: u8, g: u8, b: u8) {
        self.force_full_repaint = true;
        self.term.set_default_background(Rgb { r, g, b });
    }

    /// Set default foreground, background, cursor, and selection colors (`0x00RRGGBB`).
    pub fn set_theme(&mut self, fg: u32, bg: u32, cursor: u32, selection: u32) {
        self.install_host_colors(aterm_types::scheme::ThemeParts {
            fg,
            bg,
            cursor,
            selection,
        });
    }

    /// Set or clear the explicit selection foreground color (`0x00RRGGBB`).
    pub fn set_selection_fg(&mut self, fg: Option<u32>) {
        self.term
            .set_default_selection_foreground(fg.map(theme_api::unpack));
        self.install_renderer_selection_fg(fg);
    }

    /// Set the minimum glyph contrast ratio (`1.0..=21.0`).
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.force_full_repaint = true;
        self.renderer.set_minimum_contrast(ratio);
    }

    /// Set the default-background opacity (`0.0..=1.0`).
    pub fn set_background_opacity(&mut self, opacity: f32) {
        self.force_full_repaint = true;
        self.renderer.set_background_opacity(opacity);
    }

    /// Set the cursor fill opacity (`0.0..=1.0`).
    pub fn set_cursor_opacity(&mut self, opacity: f32) {
        self.force_full_repaint = true;
        self.renderer.set_cursor_opacity(opacity);
    }

    /// Mark the terminal pane unfocused (`true`) or focused (`false`) for selection styling.
    pub fn set_selection_inactive(&mut self, inactive: bool) {
        self.force_full_repaint = true;
        self.renderer.set_selection_inactive(inactive);
    }

    /// Set or clear the unfocused selection background color (`0x00RRGGBB`).
    pub fn set_selection_inactive_bg(&mut self, bg: Option<u32>) {
        self.force_full_repaint = true;
        self.renderer.set_selection_inactive_bg(bg);
    }

    /// Re-rasterize at a new font size `px`.
    pub fn set_px(&mut self, px: f32) -> Result<(), String> {
        let (cw, ch, _) = self.renderer.cell_geometry(px);
        self.checked_surface_geometry(cw, ch, self.renderer.pad(), self.renderer.head())?;
        self.force_full_repaint = true;
        self.renderer.set_px(px);
        Ok(())
    }

    /// Resize the terminal grid to `rows`×`cols`.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        let (rows, cols) = frame_geometry_api::checked_grid_size(
            &self.renderer,
            rows,
            cols,
            self.band.committed(),
        )?;
        if let Some(pending) = self.term.resize_offloading_scrollback(rows, cols) {
            if pending.line_count() <= INLINE_REFLOW_MAX_LINES {
                // Small-history fast path. A follow-up job cannot arise on this
                // single-threaded path, but dropping one would wedge the detach window,
                // so route it to the pump.
                if let Some(follow) = self.term.finish_resize_offload(pending.reflow()) {
                    self.pending_reflow = Some(follow);
                    self.reflow_grace = REFLOW_PUMP_GRACE_RENDERS;
                }
            } else {
                self.pending_reflow = Some(pending);
                self.reflow_grace = REFLOW_PUMP_GRACE_RENDERS;
            }
        }
        self.rows = self.term.grid().rows() as usize;
        self.cols = self.term.grid().cols() as usize;
        self.predict.reset();
        Ok(())
    }

    /// Advances a pending scrollback reflow by one budgeted step per frame after [`REFLOW_PUMP_GRACE_RENDERS`].
    #[inline]
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

    /// Advances a pending scrollback reflow by one budgeted step when the lazy backlog exceeds [`REFLOW_BACKLOG_MAX_LINES`].
    #[inline]
    fn pump_reflow_on_output(&mut self) {
        if self.pending_reflow.is_some() && self.term.lazy_backlog_len() > REFLOW_BACKLOG_MAX_LINES
        {
            self.pump_reflow();
        }
    }

    /// Render damaged rows and active overlays into the internal RGBA8 framebuffer.
    pub fn render(&mut self) -> Result<(), String> {
        let (width, height) = self.checked_surface_size()?;
        let bytes = width * height * 4;
        self.rgba
            .try_reserve_exact(bytes.saturating_sub(self.rgba.len()))
            .map_err(|e| format!("cannot reserve RGBA framebuffer: {e}"))?;
        self.pump_reflow_on_render_tick();
        self.drain_compress_backlog_on_render();
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
            self.present_bands.clear();
            self.last_render_gated = true;
            return Ok(());
        }
        self.last_render_gated = false;
        let mut fresh_win = self.force_full_repaint.then(WindowCpu::new);
        let _ = self.band.undo(&mut self.frame_scratch, self.rows);
        self.refill_frame_scratch();
        let (cw, ch) = self.renderer.cell_size();
        self.hand_band_to_effects();
        self.effects
            .apply(&mut self.term, &mut self.frame_scratch, cw, ch);
        self.scroll_input
            .stamp(&mut self.frame_scratch, self.rows, ch);
        self.compose_band_into_frame();
        let frac_px = self.frame_scratch.scroll_frac_px;
        let frac_full = frac_px != 0 || self.last_present_frac != 0;
        {
            let win = fresh_win.as_mut().unwrap_or(&mut self.win);
            let view = self
                .renderer
                .try_render_input_cached(win, &self.frame_scratch)?;
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
        if let Some(win) = fresh_win {
            self.win = win;
        }
        self.force_full_repaint = false;
        self.last_present_frac = frac_px;
        if !frac_full {
            self.expand_damage_to_rgba();
        }
        self.spill.update(&self.renderer, &self.frame_scratch);
        self.last_frame_key = Some(gate_key);
        Ok(())
    }

    /// Whether the next [`Self::render`] call has work to draw.
    ///
    /// Applies any pending global scrollback-budget share before reading `damage_epoch`;
    /// only when scrolled into history with staged lines still unpromoted can `render`'s
    /// bounded backlog trickle clamp `display_offset` and open a frame afterwards.
    pub fn needs_frame(&mut self) -> bool {
        self.apply_pending_scrollback_budget();
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

    /// Mark a renderer or presentation state change that requires a frame.
    #[inline]
    pub(crate) const fn note_host_visual_change(&mut self) {
        self.host_visual_gen = self.host_visual_gen.wrapping_add(1);
    }

    /// Cell, padding, and grid-top metrics for the chrome band.
    #[inline]
    pub(crate) fn band_metrics(&self) -> messages_api::BandMetrics {
        let (cell_w, cell_h) = self.renderer.cell_size();
        messages_api::BandMetrics {
            cell_w,
            cell_h,
            pad: self.renderer.pad(),
            grid_top: self.renderer.grid_top(),
        }
    }

    /// Set the chrome band bleed gutters for the next frame.
    #[inline]
    pub(crate) fn set_band_bleed(&mut self, bleed: Option<aterm_render::ChromeBleed>) {
        self.renderer.set_chrome_bleed(bleed);
    }

    /// Mark the chrome band dirty for the next frame.
    #[inline]
    pub(crate) const fn note_band_change(&mut self, _rows_moved: bool) {
        self.note_host_visual_change();
    }

    /// Last-rendered framebuffer width in pixels.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Last-rendered framebuffer height in pixels.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Cell width in device pixels.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn cell_width(&self) -> usize {
        self.renderer.cell_size().0
    }

    /// Cell height in device pixels.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn cell_height(&self) -> usize {
        self.renderer.cell_size().1
    }

    /// Copy of the last-rendered RGBA8 framebuffer (`width * height * 4` bytes).
    pub fn rgba(&self) -> Vec<u8> {
        self.rgba.clone()
    }

    /// Byte offset of the RGBA8 framebuffer in WebAssembly linear memory.
    pub fn rgba_ptr(&self) -> usize {
        self.rgba.as_ptr() as usize
    }
}

/// Metadata for a synchronous search ([`AtermTerminal::search_meta`]).
#[cfg_attr(target_arch = "wasm32", export)]
pub struct SearchMeta {
    incomplete: bool,
    match_count: u32,
}

#[cfg_attr(target_arch = "wasm32", export)]
impl SearchMeta {
    /// Whether search results may be truncated due to index eviction or match caps.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn incomplete(&self) -> bool {
        self.incomplete
    }

    /// Number of matches returned by the paired [`AtermTerminal::search`] call.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn match_count(&self) -> u32 {
        self.match_count
    }
}

/// Single step result from [`AtermTerminal::search_budgeted`].
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
    /// Match delta as flat `[abs_line, start_col, len]` triplets.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn matches(&self) -> Vec<u32> {
        self.matches.clone()
    }

    /// Whether all retained rows have been scanned and all match deltas delivered.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn complete(&self) -> bool {
        self.complete
    }

    /// Continuation cursor token, or `None` once complete.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// Stable identifier for the logical search stream.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn search_id(&self) -> Option<u64> {
        self.search_id
    }

    /// Whether the caller must discard previously accumulated match deltas.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn reset(&self) -> bool {
        self.reset
    }

    /// Whether results may be truncated due to index eviction or match caps.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn incomplete_index(&self) -> bool {
        self.incomplete_index
    }

    /// Oldest absolute line retained by the completed search index.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn lowest_retained_line(&self) -> u32 {
        self.lowest_retained_line
    }

    /// Rows scanned so far.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn rows_fed(&self) -> u32 {
        self.rows_fed
    }

    /// Total rows to scan in this search.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn total_rows(&self) -> u32 {
        self.total_rows
    }
}

impl AtermTerminal {
    /// Expand an OSC-8 hyperlink to the span of contiguous cells sharing its
    /// link.
    fn osc8_span(&self, row: u16, col: u16) -> (u16, u16) {
        let cols = u16::try_from(self.cols).unwrap_or(u16::MAX);
        if col >= cols {
            return (col, col);
        }
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

        let mut end = col.saturating_add(1);
        while end < cols && same(end) {
            end += 1;
        }

        (start, end)
    }
}

/// Statelessly encode a DOM keyboard event against terminal mode bits.
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
    let mode = KeyboardMode::from_bits_truncate(mode_bits);
    encode_dom_key(key, mods, event_type, base_layout_key, mode)
}

/// Detected hyperlink or path span under a grid cell.
#[cfg_attr(target_arch = "wasm32", export)]
pub struct LinkHit {
    url: String,
    start_col: u16,
    end_col: u16,
    kind: u8,
}

#[cfg_attr(target_arch = "wasm32", export)]
impl LinkHit {
    /// Link target URL or path text.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// Inclusive start grid column.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn start_col(&self) -> u16 {
        self.start_col
    }

    /// Exclusive end grid column.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn end_col(&self) -> u16 {
        self.end_col
    }

    /// Link kind (`0` = OSC 8, `1` = URL, `2` = file path, `3` = other).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn kind(&self) -> u8 {
        self.kind
    }
}

/// Viewport-relative selection bounds.
#[cfg_attr(target_arch = "wasm32", export)]
pub struct SelectionRange {
    start_x: u16,
    start_y: u16,
    end_x: u16,
    end_y: u16,
}

#[cfg_attr(target_arch = "wasm32", export)]
impl SelectionRange {
    /// Inclusive start column (viewport-relative).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn start_x(&self) -> u16 {
        self.start_x
    }

    /// Start row (`0` = top visible row).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn start_y(&self) -> u16 {
        self.start_y
    }

    /// Inclusive end column (viewport-relative).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn end_x(&self) -> u16 {
        self.end_x
    }

    /// End row (`0` = top visible row).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn end_y(&self) -> u16 {
        self.end_y
    }
}

/// JSON-escape `s` and wrap it in double quotes.
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

#[cfg(not(target_arch = "wasm32"))]
impl AtermTerminal {
    pub fn new_from_system(rows: u16, cols: u16, px: f32) -> Option<AtermTerminal> {
        let renderer = Renderer::from_system(px, Theme::default())?;
        let (rows, cols) = frame_geometry_api::checked_grid_size(&renderer, rows, cols, 0).ok()?;
        let mut term = scrollback_tiers_api::tiered_terminal(rows, cols);
        let budget_share = scrollback_tiers_api::register_budget_share(&term);
        let theme = Theme::default();
        apply_terminal_theme_colors(&mut term, theme.fg, theme.bg, theme.cursor, theme.selection);
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
            display_row_cache: std::cell::RefCell::new(facade_api::DisplayRowCache::default()),
            host_visual_gen: 0,
            last_frame_key: None,
            last_render_gated: false,
            blink_phase_shadow: None,
            hollow_shadow: None,
            web_theme,
        })
    }
}

/// Build a test terminal using the host system font.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn system_font_terminal(rows: u16, cols: u16, px: f32) -> AtermTerminal {
    AtermTerminal::new_from_system(rows, cols, px)
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font on this host"))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "lib_tests.rs"]
mod tests;
