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
const INLINE_REFLOW_MAX_LINES: usize = 20_000;

/// Never-pumped-host safety net #1: `render()` calls after a rewrap is stashed before
const REFLOW_PUMP_GRACE_RENDERS: u32 = 120;

/// Never-pumped-host safety net #2: while a rewrap is stashed, output that scrolls off
const REFLOW_BACKLOG_MAX_LINES: usize = 20_000;

/// Default per-pump rewrap budget in input history lines for
const REFLOW_STEP_BUDGET_LINES: usize = 2_000;

std::thread_local! {
    /// Host-registered font blobs, marshaled across the JS/wasm boundary once per
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
    display_row_cache: std::cell::RefCell<DisplayRowCache>,
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
    key: Option<(bool, u64, usize, u16)>,
    /// `(grapheme, is_wide)` per display column of `key.row`.
    cells: Vec<(String, bool)>,
}

/// Keep the engine's implicit cells and OSC 110/111/112/117 reset baselines
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

    /// Read one `(grapheme, is_wide)` display cell through the single-slot row
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
            display_row_cache: std::cell::RefCell::new(DisplayRowCache::default()),
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
    pub fn chrome_pad(&self) -> u16 {
        self.renderer.pad() as u16
    }

    /// Chrome top header band height in pixels set via [`Self::set_chrome`].
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn chrome_head(&self) -> u16 {
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

    /// Safety net #1 (see `REFLOW_PUMP_GRACE_RENDERS`): called by `render`. After the
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

    /// Safety net #2 (see `REFLOW_BACKLOG_MAX_LINES`): called by `process`/
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

    /// Mark a renderer or presentation state change that requires a frame.
    #[inline]
    pub(crate) fn note_host_visual_change(&mut self) {
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

    /// Cell width in device pixels.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn cell_width(&self) -> usize {
        self.renderer.cell_size().0
    }

    /// Cell height in device pixels.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn cell_height(&self) -> usize {
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
    pub fn incomplete(&self) -> bool {
        self.incomplete
    }

    /// Number of matches returned by the paired [`AtermTerminal::search`] call.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn match_count(&self) -> u32 {
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
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Continuation cursor token, or `None` once complete.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// Stable identifier for the logical search stream.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn search_id(&self) -> Option<u64> {
        self.search_id
    }

    /// Whether the caller must discard previously accumulated match deltas.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn reset(&self) -> bool {
        self.reset
    }

    /// Whether results may be truncated due to index eviction or match caps.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn incomplete_index(&self) -> bool {
        self.incomplete_index
    }

    /// Oldest absolute line retained by the completed search index.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn lowest_retained_line(&self) -> u32 {
        self.lowest_retained_line
    }

    /// Rows scanned so far.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn rows_fed(&self) -> u32 {
        self.rows_fed
    }

    /// Total rows to scan in this search.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn total_rows(&self) -> u32 {
        self.total_rows
    }
}

impl AtermTerminal {
    /// Expand an OSC-8 hyperlink to the span of contiguous cells sharing its
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
    pub fn start_col(&self) -> u16 {
        self.start_col
    }

    /// Exclusive end grid column.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn end_col(&self) -> u16 {
        self.end_col
    }

    /// Link kind (`0` = OSC 8, `1` = URL, `2` = file path, `3` = other).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn kind(&self) -> u8 {
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
    pub fn start_x(&self) -> u16 {
        self.start_x
    }

    /// Start row (`0` = top visible row).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn start_y(&self) -> u16 {
        self.start_y
    }

    /// Inclusive end column (viewport-relative).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn end_x(&self) -> u16 {
        self.end_x
    }

    /// End row (`0` = top visible row).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn end_y(&self) -> u16 {
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

/// Build a test terminal using the host system font.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn system_font_terminal(rows: u16, cols: u16, px: f32) -> AtermTerminal {
    AtermTerminal::new_from_system(rows, cols, px)
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font on this host"))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// The bundled face, injected the way a JS host injects a fetched one. A
    const BUNDLED_FACE: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

    /// The theme the host hands the constructor seeds the engine's implicit
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

    /// Drive one OSC 133 command block (prompt, command, output, done), then the next
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

    /// Reject an oversized frame before constructing or changing a grid. Existing
    #[test]
    fn oversized_frames_are_refused_before_state_changes() {
        let new = |rows, cols| {
            AtermTerminal::new(
                rows,
                cols,
                BUNDLED_FACE,
                16.0,
                0xffffff,
                0,
                0xffffff,
                0x444444,
            )
        };
        assert!(
            new(u16::MAX, u16::MAX).is_err(),
            "constructor refuses before allocating the oversized grid"
        );
        let mut t = new(0, 0).expect("zero dimensions clamp up");
        assert_eq!((t.rows, t.cols), (1, 1));
        t.resize(2, 8).expect("small resize");
        t.process(b"kept");
        t.render().expect("first frame");
        t.render().expect("settled frame");
        let pixels = t.rgba();
        let bands = t.present_bands.clone();
        let size = (t.width(), t.height());
        let metrics = (
            t.cell_width(),
            t.cell_height(),
            t.chrome_pad(),
            t.chrome_head(),
        );
        let key = t.last_frame_key;
        let generation = t.host_visual_gen;
        type Change = fn(&mut AtermTerminal) -> Result<(), String>;
        let attempts: [(&str, Change); 3] = [
            ("resize", |t| t.resize(u16::MAX, u16::MAX)),
            ("chrome", |t| t.set_chrome(u16::MAX, u16::MAX)),
            ("zoom", |t| t.set_px(1_000_000.0)),
        ];
        for (name, change) in attempts {
            assert!(change(&mut t).is_err(), "{name}");
            assert_eq!((t.rows, t.cols), (2, 8), "{name}");
            assert_eq!(t.row_text(0).as_deref(), Some("kept"), "{name}");
            assert_eq!(
                (t.term.grid().rows(), t.term.grid().cols()),
                (2, 8),
                "{name}"
            );
            assert_eq!(
                (
                    t.cell_width(),
                    t.cell_height(),
                    t.chrome_pad(),
                    t.chrome_head()
                ),
                metrics,
                "{name}"
            );
            assert_eq!(t.host_visual_gen, generation, "{name}");
            assert_eq!(t.last_frame_key, key, "{name}");
            assert!(
                !t.needs_frame(),
                "{name} did not consume or invalidate the settled frame"
            );
            assert_eq!(t.rgba(), pixels, "{name}: previous RGBA remains");
            assert_eq!(t.present_bands, bands, "{name}: previous damage remains");
            assert_eq!((t.width(), t.height()), size, "{name}");
        }
        t.set_line_height(1_000_000.0);
        assert!(
            t.render().is_err(),
            "a later geometry-changing setter can exceed the frame cap"
        );
        assert!(t.force_full_repaint, "a refusal keeps the repaint pending");
        assert!(
            !t.win.frame_pixels().is_empty(),
            "the previous packed frame remains"
        );
        assert_eq!(t.rgba(), pixels);
        assert_eq!(t.present_bands, bands);
        assert_eq!((t.width(), t.height()), size);
        t.set_line_height(1.0);
        t.render().expect("corrected geometry renders again");
        assert!(!t.force_full_repaint, "a successful repaint settles it");
        assert_eq!(t.rgba(), pixels);
        t.resize(0, 0).expect("zero resize clamps up");
        assert_eq!((t.rows, t.cols), (1, 1));

        let mut banded =
            AtermTerminal::new(2, 1, BUNDLED_FACE, 6_000.0, 0xffffff, 0, 0xffffff, 0x444444)
                .expect("the grid alone fits the frame budget");
        assert!(banded.checked_surface_size().is_ok());
        let reply = banded.notice_at(
            "post ci sev=warn Attention",
            aterm_time::Instant::now(),
            aterm_messages::WallStamp { unix_ms: 0 },
        );
        assert!(reply.starts_with("OK"), "{reply}");
        assert_eq!(banded.band_rows(), 1);
        assert!(
            banded.render().is_err(),
            "the added physical band row exceeds the budget"
        );
        assert_eq!(banded.frame_scratch.rows, 0, "no frame was built");
    }

    /// The persistent WindowCpu reuses unchanged rows across frames and the
    #[test]
    fn incremental_and_gated_renders_equal_a_single_full_render() {
        let mut warm = crate::system_font_terminal(24, 80, 16.0);
        let mut fresh = crate::system_font_terminal(24, 80, 16.0);
        let steps: &[&[u8]] = &[b"$ ", b"ls -la\r\n", b"file one\r\n", b"\x1b[1;1HX"];
        for s in steps {
            warm.process(s);
            warm.render().expect("renderable frame");
            warm.render().expect("renderable frame");
            assert!(
                warm.last_render_skipped(),
                "the settled re-render takes the gate"
            );
            fresh.process(s);
        }
        fresh.render().expect("renderable frame");
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
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        t.process(b"\x1b[2J\x1b[24;80H");
        t.render().expect("renderable frame");
        let w = t.width();
        let centre = ((t.height() / 2) * w + w / 2) * 4;
        let before = t.rgba()[centre..centre + 3].to_vec();

        let new_bg = 0x0012_3456u32;
        t.set_theme(0x00ff_ffff, new_bg, 0x0050_fa7b, 0x0026_4f78);
        t.render().expect("renderable frame");
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
    #[test]
    fn scrolled_cell_reads_are_display_relative_like_row_text() {
        let mut t = crate::system_font_terminal(5, 20, 16.0);
        for i in 0..30 {
            // Box-drawing edges force the host's per-cell path; a wide emoji
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
        let mut t = crate::system_font_terminal(5, 20, 16.0);
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

    /// `search` hands out `[abs_line, col, len]` triplets in absolute rows,
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

    /// The link `link_at` names is the one painted under the cell the host
    #[test]
    fn a_link_is_named_by_the_cell_it_is_painted_in() {
        let osc8 = |url: &str, text: &str| format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\");
        let (a, b) = ("https://a.example/", "https://b.example/");
        let rows = [
            (
                format!(
                    "see {} {} end",
                    osc8(a, "\u{5d0}\u{5d1}\u{5d2}"),
                    osc8(b, "\u{5d3}\u{5d4}\u{5d5}")
                ),
                [
                    (4, Some(b)),
                    (5, Some(b)),
                    (7, None),
                    (9, Some(a)),
                    (10, Some(a)),
                    (12, None),
                ],
            ),
            (
                format!("\x1b#6ab{} {}", osc8(a, "XY"), osc8(b, "ZW")),
                [
                    (4, Some(a)),
                    (5, Some(a)),
                    (7, Some(a)),
                    (8, None),
                    (10, Some(b)),
                    (13, Some(b)),
                ],
            ),
            (
                format!("ab{} {}", osc8(a, "XY"), osc8(b, "ZW")),
                [
                    (2, Some(a)),
                    (3, Some(a)),
                    (4, None),
                    (5, Some(b)),
                    (6, Some(b)),
                    (7, None),
                ],
            ),
        ];
        for (row, cells) in rows {
            let mut t = AtermTerminal::new(4, 40, BUNDLED_FACE, 16.0, 0, 0, 0, 0)
                .expect("the bundled face parses");
            t.process(format!("{row}\r\n").as_bytes());
            for (col, want) in cells {
                let got = t.link_at(0, col).map(|hit| hit.url());
                assert_eq!(got.as_deref(), want, "{row:?}: painted cell {col}");
            }
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

        {
            let sel = term.text_selection_mut();
            sel.start_selection(0 - offset, 0, SelectionSide::Left, SelectionType::Lines);
            sel.update_selection(1 - offset, 19, SelectionSide::Right);
            sel.complete_selection();
        }
        let r = selection_range_for(&term, 5, 20).expect("selection in viewport");
        assert_eq!((r.start_y(), r.end_y()), (0, 1), "display rows 0..=1");
        let copied = term.selection_to_string().expect("text");
        let want0 = term.display_row_text(0).unwrap();
        let want1 = term.display_row_text(1).unwrap();
        assert_eq!(copied, format!("{want0}\n{want1}"));

        term.scroll_to_bottom();
        assert!(
            selection_range_for(&term, 5, 20).is_none(),
            "selection fully off-screen yields None"
        );
    }

    /// An instance that never touches the effects API and one that toggles every
    #[test]
    fn effects_off_is_byte_identical() {
        let mut plain = crate::system_font_terminal(24, 80, 16.0);
        let mut toggled = crate::system_font_terminal(24, 80, 16.0);
        let content = b"a curious cat watched the build fail: fuck\r\n$ ";
        plain.process(content);
        plain.render().expect("renderable frame");

        toggled.process(content);
        toggled.set_sparkle_words_enabled(true);
        toggled.set_cursor_glow(true, "phaser", None, None, 260, 24, 0.7, 0.6, true);
        toggled.set_cursor_trail(true, 260, 24, None);
        toggled.advance_effects(50.0);
        toggled.render().expect("renderable frame");
        toggled.set_cursor_glow(true, "ember", None, None, 400, 64, 1.0, 2.0, true);
        toggled.process(b"z\x1b[D \x1b[D");
        toggled.advance_effects(50.0);
        toggled.render().expect("renderable frame");
        assert_ne!(
            plain.rgba(),
            toggled.rgba(),
            "a live burn must visibly change the frame (non-vacuous toggle)"
        );
        toggled.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
        toggled.set_cursor_pet(true, 7);
        toggled.render().expect("renderable frame");
        toggled.note_typed_char('p');
        toggled.process(b"p\x1b[D \x1b[D");
        let mut on_glass = false;
        for _ in 0..240 {
            toggled.advance_effects(33.0);
            toggled.render().expect("renderable frame");
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
        toggled.render().expect("renderable frame");

        assert_eq!(plain.width(), toggled.width());
        assert_eq!(
            plain.rgba(),
            toggled.rgba(),
            "effects toggled on->off must restore the byte-identical frame"
        );
    }

    /// `set_chrome` grows the frame by the pad/head chrome, and 0/0 restores the
    #[test]
    fn set_chrome_pads_the_frame_and_zero_restores_identity() {
        let mut plain = crate::system_font_terminal(24, 80, 16.0);
        let mut chromed = crate::system_font_terminal(24, 80, 16.0);
        let content = b"chrome test\r\n$ ";
        plain.process(content);
        plain.render().expect("renderable frame");
        let (w, h) = (plain.width(), plain.height());

        chromed.process(content);
        chromed.set_chrome(12, 30).expect("admitted geometry");
        chromed.render().expect("renderable frame");
        assert_eq!(chromed.width(), w + 24, "frame gains 2*pad in width");
        assert_eq!(
            chromed.height(),
            h + 24 + 30,
            "frame gains 2*pad + head in height"
        );

        chromed.set_chrome(0, 0).expect("admitted geometry");
        chromed.render().expect("renderable frame");
        assert_eq!((chromed.width(), chromed.height()), (w, h));
        assert_eq!(
            plain.rgba(),
            chromed.rgba(),
            "0/0 chrome must restore the byte-identical exact-fit frame"
        );
    }

    /// SPILL BAND through the REAL pipeline: an EMBERFORGE burn on the top
    #[test]
    fn spill_exports_track_band_content_through_the_real_pipeline() {
        let mut t = crate::system_font_terminal(12, 40, 16.0);
        let (pad, head) = (12usize, 30usize);
        t.set_chrome(pad as u16, head as u16)
            .expect("admitted geometry");
        t.set_cursor_glow(true, "ember", None, None, 400, 64, 1.0, 2.0, true);
        t.render().expect("renderable frame");
        for (ch, bytes) in [('a', b"a"), ('b', b"b"), ('c', b"c"), ('d', b"d")] {
            assert!(
                t.note_typed_char(ch),
                "a witnessed simple scalar must arm movement provenance"
            );
            t.process(bytes);
            t.advance_effects(30.0);
            t.render().expect("renderable frame");
        }
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
            t.render().expect("renderable frame");
        }
        assert!(lit_frame, "the witnessed burn must reach the band live");
        let (w, h) = (t.width(), t.height());
        let grid_h = h - 2 * pad - head;
        assert_eq!(
            t.spill_len(),
            (w * (pad + head) + w * pad + 2 * pad * grid_h) * 4,
            "spill buffer sized to the four band strips"
        );

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

    /// Drive the pet onto the glass through the binding alone (the naming glow style,
    fn wake_the_pet(t: &mut AtermTerminal, seed: u64) -> Option<u32> {
        t.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
        t.set_cursor_pet(true, seed);
        t.render().expect("renderable frame");
        for frame in 0..240u32 {
            if frame % 6 == 0 {
                let ch = char::from(b'a' + u8::try_from(frame / 6 % 26).unwrap_or(0));
                t.note_typed_char(ch);
                t.process(&[ch as u8]);
            }
            t.advance_effects(33.0);
            t.render().expect("renderable frame");
            if t.cursor_pet_alpha() > 0 {
                return Some(frame);
            }
        }
        None
    }

    /// Advance 100 ms frames with no input until the binding reports idle (or the
    fn settle(t: &mut AtermTerminal, budget_frames: u32) -> bool {
        for _ in 0..budget_frames {
            t.advance_effects(100.0);
            t.render().expect("renderable frame");
            if !t.is_effects_active() {
                t.render().expect("renderable frame");
                return true;
            }
        }
        false
    }

    /// The seed is the only entropy: same seed, bytes and `dt` stream give identical
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
                t.render().expect("renderable frame");
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
        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "settled frame must take the gate");

        let (px, py) = (t.width() as f32 * 0.5, t.height() as f32 * 0.5);
        t.note_pointer_px(px, py);
        assert!(t.needs_frame(), "a moved pointer must reopen the gate");
        t.render().expect("renderable frame");
        assert!(!t.last_render_skipped(), "the pointer frame must draw");
        assert!(settle(&mut t, 600), "a watching cat settles again");
        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "re-settled after the pointer");
        t.note_pointer_px(px, py);
        assert!(
            !t.needs_frame(),
            "an identical pointer sample is value-shadowed and must not reopen the gate"
        );
        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "an idle hover must gate");
        t.note_pointer_px(f32::NAN, py);
        assert!(!t.needs_frame(), "a non-finite sample is dropped");

        t.note_pointer_leave();
        assert!(t.needs_frame(), "the pointer leaving must reopen the gate");
        t.render().expect("renderable frame");
        assert!(!t.last_render_skipped(), "the leave frame must draw");
        assert!(settle(&mut t, 600), "the cat settles after the leave");
        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "re-settled after the leave");
        t.note_pointer_leave();
        assert!(!t.needs_frame(), "a second leave is value-shadowed");

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
        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "a passed press must gate");
        let inside = (((x0 + x1) / 2) as f32, ((y0 + y1) / 2) as f32);
        assert_eq!(t.pet_press_px(inside.0, inside.1), 1, "inside pets");
        assert!(
            t.needs_frame(),
            "a petted cat needs the frame that runs the purr"
        );
        t.render().expect("renderable frame");
        assert!(!t.last_render_skipped(), "the petting frame must draw");
    }

    /// A walking pet is free sprites over the grid, so its motion presents
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
            t.render().expect("renderable frame");
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

    /// Swap in an engine whose history lives in the tiered store (tiny ring) and feed
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
    #[test]
    fn small_history_rewraps_inline_and_a_deep_one_defers_to_stepped_pumps() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        install_tiered_history(&mut t, 24, 80, 500);
        let before = t.term.grid().scrollback_lines();
        assert!(before > 100, "precondition: deep history ({before} lines)");
        t.resize(24, 40).expect("admitted geometry");
        assert!(!t.reflow_pending(), "a small history rewraps inline");
        assert!(
            t.term.grid().scrollback_lines() >= before,
            "inline rewrap preserves history"
        );
        assert!(t.serialize_scrollback(None).contains("L0-hist"));

        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();
        assert!(before > INLINE_REFLOW_MAX_LINES, "precondition: {before}");
        t.resize(24, 40).expect("admitted geometry");
        assert!(t.reflow_pending(), "a deep history defers its rewrap");
        assert_eq!(t.cols, 40, "the visible grid resized synchronously");
        let during = t.term.grid().scrollback_lines();
        assert!(
            during < before / 4,
            "during the detach window only the bounded ring is visible \
             (got {during} of {before})"
        );

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
    #[test]
    fn a_resize_during_the_window_supersedes_without_losing_history() {
        for half_stepped in [false, true] {
            let mut t = crate::system_font_terminal(24, 80, 16.0);
            install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
            let before = t.term.grid().scrollback_lines();
            t.resize(24, 40).expect("admitted geometry");
            assert!(t.reflow_pending());
            if half_stepped {
                t.pump_reflow_budget(500);
                for _ in 0..3 {
                    assert!(t.pump_reflow(), "mid-stepping: work must remain");
                }
            }
            t.resize(24, 60).expect("admitted geometry");
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
            t.resize(24, 30).expect("admitted geometry");
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
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();
        t.resize(24, 40).expect("admitted geometry");
        assert!(t.reflow_pending());
        t.render().expect("renderable frame");
        assert!(t.reflow_pending(), "no pump inside the grace window");
        for _ in 0..REFLOW_PUMP_GRACE_RENDERS {
            t.render().expect("renderable frame");
        }
        assert!(
            t.reflow_pending(),
            "a multi-step job is still converging right after grace \
             (one bounded step per frame — not one catch-up frame)"
        );
        let mut frames = 0usize;
        while t.reflow_pending() {
            t.render().expect("renderable frame");
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
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        t.resize(24, 40).expect("admitted geometry");
        assert!(t.reflow_pending());

        let mut buf = Vec::new();
        for i in 0..(REFLOW_BACKLOG_MAX_LINES + 2_000) {
            buf.extend_from_slice(format!("W{i}-window\r\n").as_bytes());
        }
        t.process(&buf);
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
            t.render().expect("renderable frame");
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
        t.render().expect("renderable frame");
        check(&mut t, "history viewport");
        t.scroll_lines(-3);
        t.render().expect("renderable frame");
        check(&mut t, "back to the live bottom");
        t.resize(10, 30).expect("admitted geometry");
        t.render().expect("renderable frame");
        check(&mut t, "resize");
        t.process(b"after resize");
        t.render().expect("renderable frame");
        check(&mut t, "echo after resize");
        assert!(scoped >= 5, "the scoped arm must be reached ({scoped})");
        assert!(full >= 2, "the full arm must be reached ({full})");
    }

    /// WF-1 frame gate, two-sided. Side 1 (skip): a second `render()` with
    #[test]
    fn frame_gate_skips_settled_frames_and_reopens_on_every_change_class() {
        let mut t = crate::system_font_terminal(8, 40, 14.0);
        t.process(b"hello gate");
        t.render().expect("renderable frame");
        assert!(!t.last_render_skipped(), "first render must draw");
        let baseline = t.rgba();

        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "settled frame must take the gate");
        assert_eq!(t.present_band_count(), 0, "gated frame exports zero bands");
        assert_eq!(t.rgba(), baseline, "gated frame retains the framebuffer");
        assert!(!t.needs_frame(), "needs_frame must agree with the gate");

        t.process(b"!");
        assert!(t.needs_frame(), "damage must reopen the gate");
        t.render().expect("renderable frame");
        assert!(!t.last_render_skipped(), "an echo frame must draw");

        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "re-settled before blink");
        t.set_cursor_blink_phase(false);
        t.render().expect("renderable frame");
        assert!(!t.last_render_skipped(), "a blink flip must draw");
        t.set_cursor_blink_phase(false);
        t.render().expect("renderable frame");
        assert!(
            t.last_render_skipped(),
            "an idempotent blink re-assert must gate"
        );

        t.selection_start(2, 1);
        t.selection_extend(2, 8);
        t.render().expect("renderable frame");
        assert!(!t.last_render_skipped(), "a selection change must draw");

        for _ in 0..12 {
            t.process(b"line\r\n");
        }
        t.render().expect("renderable frame");
        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "re-settled before scroll");
        t.scroll_lines(3);
        t.render().expect("renderable frame");
        assert!(
            !t.last_render_skipped(),
            "a viewport scroll must draw (display-offset damage -> epoch)"
        );
    }

    /// The drag half of the WF-1 gate: `selection_extend` bumps ONLY when the
    #[test]
    fn redundant_selection_extend_gates_but_every_real_move_still_draws() {
        let mut t = crate::system_font_terminal(8, 40, 14.0);
        for _ in 0..20 {
            t.process(b"a line of selectable text\r\n");
        }
        t.render().expect("renderable frame");

        t.selection_start(2, 4);
        t.selection_extend(2, 10);
        t.render().expect("renderable frame");
        assert!(!t.last_render_skipped(), "the first extend must draw");
        let baseline = t.rgba();
        let span = t.selection_text();
        t.selection_extend(2, 10);
        assert!(
            !t.needs_frame(),
            "a redundant extend must not reopen the gate"
        );
        t.render().expect("renderable frame");
        assert!(
            t.last_render_skipped(),
            "a cell-identical extend must be gated away"
        );
        assert_eq!(t.present_band_count(), 0, "gated frame exports zero bands");
        assert_eq!(t.rgba(), baseline, "gated frame retains the framebuffer");
        assert_eq!(t.selection_text(), span, "and the selection is unchanged");

        t.selection_extend(2, 11);
        assert!(t.needs_frame(), "a one-cell move must reopen the gate");
        t.render().expect("renderable frame");
        assert!(!t.last_render_skipped(), "a one-cell move must draw");
        assert_ne!(t.selection_text(), span, "and it really moved");

        t.selection_finish();
        t.render().expect("renderable frame");
        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "re-settled before the scroll");
        t.selection_start(3, 2);
        t.selection_extend(3, 12);
        t.render().expect("renderable frame");
        let before_scroll = t.selection_text();
        t.scroll_lines(2);
        t.render().expect("renderable frame");
        t.render().expect("renderable frame");
        assert!(t.last_render_skipped(), "re-settled after the scroll");
        t.selection_extend(3, 12);
        assert!(
            t.needs_frame(),
            "a scroll under a stationary pointer must still reopen the gate"
        );
        t.render().expect("renderable frame");
        assert!(
            !t.last_render_skipped(),
            "an extend whose terminal-relative anchor moved must draw"
        );
        assert_ne!(
            t.selection_text(),
            before_scroll,
            "and the selection really did change"
        );

        let mut fresh = crate::system_font_terminal(8, 40, 14.0);
        for _ in 0..20 {
            fresh.process(b"a line of selectable text\r\n");
        }
        fresh.selection_start(3, 2);
        fresh.selection_extend(3, 12);
        fresh.scroll_lines(2);
        fresh.selection_extend(3, 12);
        fresh.render().expect("renderable frame");
        assert_eq!(
            t.rgba(),
            fresh.rgba(),
            "de-duping redundant extends must be pixel-invisible"
        );
    }

    /// Glass floors glyphs on the web too: a low-contrast SGR fg (#2A2D35 on
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
            t.render().expect("renderable frame");
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
