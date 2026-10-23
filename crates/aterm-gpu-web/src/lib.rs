// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

#[cfg(target_arch = "wasm32")]
use aterm_wasm_abi::export;

#[cfg(target_arch = "wasm32")]
aterm_wasm_abi::export_runtime!();

use crate::AtermGpuTerminal as WebTerminal;

macro_rules! web_terminal_impl {
    ($bind:ident) => {
        use crate::AtermGpuTerminal;
        #[cfg(target_arch = "wasm32")]
        use aterm_wasm_abi::export;
        $bind!(AtermGpuTerminal, export);
    };
}

#[path = "../../aterm-wasm/src/effects_api.rs"]
mod effects_api;
#[path = "../../aterm-wasm/src/facade_api.rs"]
mod facade_api;
#[path = "../../aterm-wasm/src/frame_geometry_api.rs"]
mod frame_geometry_api;
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
#[path = "../../aterm-wasm/src/theme_api.rs"]
mod theme_api;

use aterm_core::grid::{PendingScrollbackReflow, ReflowStep};
use aterm_core::selection::SmartSelection;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{BlockText, ClipboardAccess, CursorStyle, MouseMode, Rgb, Terminal};
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::{RenderInput, Renderer, SpillBand, Theme};

/// Histories at or below this many lines are rewrapped inline by `resize`, as in
const INLINE_REFLOW_MAX_LINES: usize = 20_000;

/// Safety net for a host that never pumps: after this many `render` /
const REFLOW_PUMP_GRACE_RENDERS: u32 = 120;

/// Safety net for a host that never renders or pumps: past this lazy-buffer
const REFLOW_BACKLOG_MAX_LINES: usize = 20_000;

/// Default per-pump rewrap budget in input history lines (tunable via
const REFLOW_STEP_BUDGET_LINES: usize = 2_000;

#[cfg(target_arch = "wasm32")]
use aterm_gpu::GpuContext;
#[cfg(any(target_arch = "wasm32", test))]
use aterm_gpu::SurfacePresentFailure;
use aterm_gpu::{GpuRenderer, GpuSurface, WindowGpu};

std::thread_local! {
    /// Host-registered font blobs, copied across the JS/wasm boundary once per
    static REGISTERED_FONTS: std::cell::RefCell<Vec<std::sync::Arc<Vec<u8>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Register a font blob for handle-based reuse by every engine in this module.
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

/// Construction is split to match the browser lifecycle: 1.
#[cfg_attr(target_arch = "wasm32", export)]
pub struct AtermGpuTerminal {
    term: Terminal,
    cpu: Renderer,
    rows: usize,
    cols: usize,
    theme: Theme,
    font_bytes: Vec<u8>,
    gpu: Option<GpuState>,
    #[cfg(target_arch = "wasm32")]
    rgba: Vec<u8>,
    #[cfg(target_arch = "wasm32")]
    fb_width: usize,
    #[cfg(target_arch = "wasm32")]
    fb_height: usize,
    smart: SmartSelection,
    fallback_font: Option<std::sync::Arc<Vec<u8>>>,
    fallback_chain_extra: Vec<std::sync::Arc<Vec<u8>>>,
    emoji_font: Option<std::sync::Arc<Vec<u8>>>,
    bold_font: Option<std::sync::Arc<Vec<u8>>>,
    symbol_font: Option<std::sync::Arc<Vec<u8>>>,
    text_shaping: aterm_render::TextShapingConfig,
    frame_scratch: RenderInput,
    #[cfg(test)]
    last_refill: Option<aterm_core::render::FrameRefill>,
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
    display_row_cache: std::cell::RefCell<GpuDisplayRowCache>,
    pub(crate) budget_share: aterm_core::terminal::scrollback_shared_budget::ScrollbackBudgetShare,
    host_visual_gen: u64,
    last_frame_key: Option<FrameGateKey>,
    last_present_frac: i32,
    last_render_gated: bool,
    blink_phase_shadow: Option<bool>,
    hollow_shadow: Option<bool>,
    pub(crate) web_theme: theme_api::WebThemeState,
}

/// Frame-gate key: every input of a rendered frame that can change between two
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

/// The GPU half of the terminal, populated by [`AtermGpuTerminal::init`].
struct GpuState {
    renderer: GpuRenderer,
    surface: GpuSurface,
    win: WindowGpu,
}

/// Convert the GPU crate's typed swapchain failure into the string error
#[cfg(any(target_arch = "wasm32", test))]
fn webgl_present_result(result: Result<(), SurfacePresentFailure>) -> Result<(), String> {
    result.map_err(|failure| format!("WebGL canvas present failed: {failure:?}"))
}

#[cfg(any(target_arch = "wasm32", test))]
impl AtermGpuTerminal {
    /// The CPU face `init` builds the GPU renderer on: from the injected font
    fn gpu_face(&self) -> Result<Renderer, String> {
        let mut cpu = Renderer::from_bytes(&self.font_bytes, self.cpu.px(), self.theme)?;
        cpu.set_runtime_font_discovery(false);
        if let Some(bytes) = self.fallback_font.as_deref() {
            cpu.set_fallback_bytes(bytes)?;
        }
        for bytes in &self.fallback_chain_extra {
            cpu.add_fallback_bytes(bytes)?;
        }
        if let Some(arc) = self.emoji_font.as_ref() {
            // Already an interned Arc: install the shared blob rather than
            cpu.set_color_font_arc(std::sync::Arc::clone(arc))?;
        }
        if let Some(bytes) = self.bold_font.as_deref() {
            cpu.set_bold_font(bytes)?;
        }
        if let Some(bytes) = self.symbol_font.as_deref() {
            cpu.set_symbol_fallback_bytes(bytes)?;
        }
        cpu.set_line_height(self.cpu.line_height());
        cpu.set_text_shaping(self.text_shaping.clone());
        cpu.set_minimum_contrast(self.cpu.minimum_contrast());
        cpu.set_background_opacity(self.cpu.background_opacity());
        cpu.set_cursor_opacity(self.cpu.cursor_opacity());
        cpu.set_cursor_blink_phase(self.cpu.cursor_blink_phase());
        cpu.set_cursor_style_override(self.cpu.cursor_style_override());
        cpu.set_selection_fg(self.cpu.selection_fg());
        cpu.set_selection_inactive(self.cpu.selection_inactive());
        cpu.set_selection_inactive_bg(self.cpu.selection_inactive_bg());
        cpu.set_pad(self.cpu.pad());
        cpu.set_head(self.cpu.head());
        Ok(cpu)
    }
}

impl AtermGpuTerminal {
    /// Frame gate: run the two frame-boundary pumps, then decide whether this
    fn open_frame(&mut self) -> Option<FrameGateKey> {
        self.pump_reflow_on_render_tick();
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
    #[inline]
    fn record_presented_frame(&mut self, key: FrameGateKey) {
        self.last_frame_key = Some(key);
        self.last_present_frac = self.frame_scratch.scroll_frac_px;
    }

    /// Build one frame into the kept scratch; shared by `render`,
    fn build_frame(&mut self) {
        let _ = self.band.undo(&mut self.frame_scratch, self.rows);
        self.refill_frame_scratch();
        let rows = self.rows;
        let (cw, ch) = self.cpu.cell_size();
        self.hand_band_to_effects();
        self.effects
            .apply(&mut self.term, &mut self.frame_scratch, cw, ch);
        self.scroll_input.stamp(&mut self.frame_scratch, rows, ch);
        self.compose_band_into_frame();
        self.spill.update(&self.cpu, &self.frame_scratch);
    }

    /// Refill every engine-owned frame channel and consume the frame's damage
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

    /// Record a visual change the engine's damage epoch cannot see (renderer-held or presentation state), reopening the frame gate for one frame.
    #[inline]
    pub(crate) fn note_host_visual_change(&mut self) {
        self.host_visual_gen = self.host_visual_gen.wrapping_add(1);
    }

    /// Band host hook: the CPU face's cell, pad and grid-top metrics (the face the GPU renderer is built from, so both agree).
    #[inline]
    pub(crate) fn band_metrics(&self) -> messages_api::BandMetrics {
        let (cell_w, cell_h) = self.cpu.cell_size();
        messages_api::BandMetrics {
            cell_w,
            cell_h,
            pad: self.cpu.pad(),
            grid_top: self.cpu.grid_top(),
        }
    }

    /// Set the band's gutters on the CPU face (read by spill and the offscreen path) and on the live GPU renderer's inner face.
    #[inline]
    pub(crate) fn set_band_bleed(&mut self, bleed: Option<aterm_render::ChromeBleed>) {
        self.cpu.set_chrome_bleed(bleed);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_chrome_bleed(bleed);
        }
    }

    /// The band changed: reopen the frame gate for one frame and, when its row count moved, resize the swapchain to the new frame height.
    #[inline]
    pub(crate) fn note_band_change(&mut self, rows_moved: bool) {
        self.note_host_visual_change();
        if rows_moved {
            self.size_swapchain();
        }
    }

    /// Size the live swapchain to the frame [`Self::build_frame`] composes:
    fn size_swapchain(&mut self) {
        let Ok((w, h)) = self.checked_surface_size() else {
            return;
        };
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer
                .resize_surface(&mut gpu.win, &mut gpu.surface, w as u32, h as u32);
        }
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
impl AtermGpuTerminal {
    /// The CPU face: the GPU face wraps a copy of it and reads its floors, theme and opacity off it.
    #[inline]
    pub(crate) const fn render_face(&self) -> &Renderer {
        &self.cpu
    }

    /// Everything a theme change does outside the engine's colours.
    pub(crate) fn install_renderer_theme(&mut self, parts: aterm_types::scheme::ThemeParts) {
        let aterm_types::scheme::ThemeParts { fg, bg, cursor, .. } = parts;
        self.note_host_visual_change();
        self.theme_cursor = cursor & 0x00FF_FFFF;
        self.theme_fg = fg & 0x00FF_FFFF;
        self.theme_bg = bg & 0x00FF_FFFF;
        self.effects
            .set_matrix_rain_theme(self.theme_bg, self.theme_fg, self.theme_cursor);
        self.effects.invalidate_companion_colors();
        let theme = Theme::from(parts);
        self.theme = theme;
        self.cpu.set_theme(theme);
        self.band.set_theme(fg, bg, cursor);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_theme(theme);
            gpu.win.invalidate_present();
        }
    }

    /// The renderer half of `set_selection_fg` (the engine half is the caller's), on both faces.
    pub(crate) fn install_renderer_selection_fg(&mut self, fg: Option<u32>) {
        self.note_host_visual_change();
        self.cpu.set_selection_fg(fg);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_selection_fg(fg);
            gpu.win.invalidate_present();
        }
    }

    /// Test builds only: build one headless frame, so the frame gate settles.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn settle_frame(&mut self) {
        self.render_headless().expect("renderable frame");
    }
}

#[cfg_attr(target_arch = "wasm32", export)]
impl AtermGpuTerminal {
    /// Build a `rows`x`cols` terminal.
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
    ) -> Result<AtermGpuTerminal, String> {
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
        let mut cpu = Renderer::from_bytes(font_bytes, px, theme)?;
        cpu.set_runtime_font_discovery(false);
        let (rows, cols) = frame_geometry_api::checked_grid_size(&cpu, rows, cols, 0)?;
        let mut term = scrollback_tiers_api::tiered_terminal(rows, cols);
        let budget_share = scrollback_tiers_api::register_budget_share(&term);
        apply_terminal_theme_colors(&mut term, fg, bg, cursor, selection);
        let notifications = notifications_api::wire_notification_queue(&mut term);
        let web_theme = theme_api::WebThemeState::new(term.color_scheme());
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
            pred_row_scratch: predict_api::PredRows::default(),
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
            web_theme,
        })
    }

    /// Inject a broad-coverage (CJK + symbols) fallback face from font bytes, so glyphs the primary face lacks render real shapes instead of `.notdef` tofu.
    pub fn set_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change();
        self.cpu.set_fallback_bytes(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_fallback_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        self.fallback_font = Some(aterm_render::intern_font_bytes_slice(bytes));
        self.fallback_chain_extra.clear();
        Ok(())
    }

    /// Append another fallback face to the chain (unlike [`Self::set_fallback_font`], which resets it), e.g. CJK then Arabic/Devanagari/Thai/Hebrew.
    pub fn add_fallback_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change();
        self.cpu.add_fallback_bytes(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.add_fallback_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        self.fallback_chain_extra
            .push(aterm_render::intern_font_bytes_slice(bytes));
        Ok(())
    }

    /// Inject a colour-emoji (sbix) face from font bytes, driving the existing ColorEmoji colour path.
    pub fn set_emoji_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change();
        let arc = aterm_render::intern_font_bytes_slice(bytes);
        self.cpu.set_color_font_arc(arc.clone())?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_emoji_font_bytes(bytes.to_vec())?;
            gpu.win.invalidate_present();
        }
        self.emoji_font = Some(arc);
        Ok(())
    }

    /// Inject a real bold weight of the primary family so SGR-bold cells use it instead of synthetic embolden.
    pub fn set_bold_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change();
        self.cpu.set_bold_font(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_bold_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        self.bold_font = Some(aterm_render::intern_font_bytes_slice(bytes));
        Ok(())
    }

    /// Inject a symbol fallback face from font bytes (the byte-injection counterpart of the config `symbol_font`).
    pub fn set_symbol_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change();
        self.cpu.set_symbol_fallback_bytes(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_symbol_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        self.symbol_font = Some(aterm_render::intern_font_bytes_slice(bytes));
        Ok(())
    }

    /// [`AtermGpuTerminal::set_fallback_font`] from a registered handle.
    pub fn set_fallback_font_registered(&mut self, handle: u32) -> Result<(), String> {
        self.note_host_visual_change();
        let bytes = registered_font(handle)?;
        self.cpu.set_fallback_bytes(&bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_fallback_font_bytes(&bytes)?;
            gpu.win.invalidate_present();
        }
        self.fallback_font = Some(bytes);
        self.fallback_chain_extra.clear();
        Ok(())
    }

    /// [`AtermGpuTerminal::add_fallback_font`] from a registered handle.
    pub fn add_fallback_font_registered(&mut self, handle: u32) -> Result<(), String> {
        self.note_host_visual_change();
        let bytes = registered_font(handle)?;
        self.cpu.add_fallback_bytes(&bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.add_fallback_font_bytes(&bytes)?;
            gpu.win.invalidate_present();
        }
        self.fallback_chain_extra.push(bytes);
        Ok(())
    }

    /// [`AtermGpuTerminal::set_emoji_font`] from a registered handle.
    pub fn set_emoji_font_registered(&mut self, handle: u32) -> Result<(), String> {
        self.note_host_visual_change();
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
        self.note_host_visual_change();
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
        self.note_host_visual_change();
        let bytes = registered_font(handle)?;
        self.cpu.set_symbol_fallback_bytes(&bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_symbol_font_bytes(&bytes)?;
            gpu.win.invalidate_present();
        }
        self.symbol_font = Some(bytes);
        Ok(())
    }

    /// Swap the primary face (the host's `terminalFontFamily`) and re-rasterize on the CPU and live GPU faces.
    pub fn set_primary_font(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.note_host_visual_change();
        self.cpu.set_primary_font(bytes)?;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_primary_font_bytes(bytes)?;
            gpu.win.invalidate_present();
        }
        self.font_bytes = bytes.to_vec();
        Ok(())
    }

    /// Scale the cell box height (`terminalLineHeight`) without changing glyph px, on the CPU and live GPU faces; remembered for `init`.
    pub fn set_line_height(&mut self, scale: f32) {
        self.note_host_visual_change();
        self.cpu.set_line_height(scale);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_line_height(scale);
            gpu.win.invalidate_present();
        }
    }

    /// Programming ligatures on/off (`=>`, `!=`, `===` …), like the native `ligatures` knob.
    pub fn set_ligatures(&mut self, on: bool) {
        self.note_host_visual_change();
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

    /// OpenType font features for the primary face as a space-separated spec (`"+ss01 zero -calt"`), like the native `font_features` knob.
    pub fn set_font_features(&mut self, spec: &str) {
        self.note_host_visual_change();
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

    /// Set palette colour `index` (0–255; 0–15 are the ANSI colours) so SGR-indexed cells resolve through the host's theme.
    pub fn set_palette_color(&mut self, index: u8, r: u8, g: u8, b: u8) {
        self.note_host_visual_change();
        self.term.set_palette_color_components(index, r, g, b);
        self.band.set_palette_color(index, [r, g, b]);
    }

    /// Set the cursor blink phase on the live GPU renderer and the CPU face.
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

    /// Force a hollow (unfocused) cursor when `true`, or restore the terminal's DECSCUSR style when `false`, on both faces.
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

    /// Drain the missing-font class bits (1 = text/mono fallback, 2 = colour emoji) accumulated since the last call.
    pub fn take_missing_font_classes(&mut self) -> u8 {
        let mut classes = self.cpu.take_missing_font_classes();
        if let Some(gpu) = self.gpu.as_mut() {
            classes |= gpu.renderer.take_missing_font_classes();
        }
        classes
    }

    /// Seed the engine's default foreground/background so OSC 10/11 colour-query replies report the host theme.
    pub fn set_default_foreground(&mut self, r: u8, g: u8, b: u8) {
        self.note_host_visual_change();
        self.term.set_default_foreground(Rgb { r, g, b });
    }

    pub fn set_default_background(&mut self, r: u8, g: u8, b: u8) {
        self.note_host_visual_change();
        self.term.set_default_background(Rgb { r, g, b });
    }

    /// Replace the default fg/bg/cursor/selection theme (0x00RRGGBB) on the GPU renderer and the CPU face without a device or face rebuild.
    pub fn set_theme(&mut self, fg: u32, bg: u32, cursor: u32, selection: u32) {
        self.install_host_colors(aterm_types::scheme::ThemeParts {
            fg,
            bg,
            cursor,
            selection,
        });
    }

    /// Explicit selected-text foreground (0x00RRGGBB), or `undefined` for the WCAG contrast-floor default.
    pub fn set_selection_fg(&mut self, fg: Option<u32>) {
        self.term
            .set_default_selection_foreground(fg.map(theme_api::unpack));
        self.install_renderer_selection_fg(fg);
    }

    /// Per-cell minimum contrast ratio (xterm's `minimumContrastRatio`, 1..=21; `<= 1.0` is off, the default).
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.note_host_visual_change();
        self.cpu.set_minimum_contrast(ratio);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_minimum_contrast(ratio);
            gpu.win.invalidate_present();
        }
    }

    /// Default-background opacity (0..=1, like Ghostty's `background-opacity`; `1.0` is opaque and the default).
    pub fn set_background_opacity(&mut self, opacity: f32) {
        self.note_host_visual_change();
        self.cpu.set_background_opacity(opacity);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_background_opacity(opacity);
            gpu.win.invalidate_present();
        }
    }

    /// Window chrome for window-space effects.
    pub fn set_chrome(&mut self, pad: u16, head: u16) -> Result<(), String> {
        let (cw, ch) = self.cpu.cell_size();
        self.checked_surface_geometry(cw, ch, usize::from(pad), usize::from(head))?;
        self.note_host_visual_change();
        self.cpu.set_pad(pad as usize);
        self.cpu.set_head(head as usize);
        self.effects.set_chrome(pad, head);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_pad(pad as usize);
            gpu.renderer.set_head(head as usize);
        }
        self.size_swapchain();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.win.invalidate_present();
        }
        Ok(())
    }

    /// The chrome interior padding from [`Self::set_chrome`] (px; 0 = exact fit), read from the CPU face.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn chrome_pad(&self) -> u16 {
        self.cpu.pad() as u16
    }

    /// The chrome top head band set via [`Self::set_chrome`] (px; 0 = none).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn chrome_head(&self) -> u16 {
        self.cpu.head() as u16
    }

    /// Cursor-fill opacity (0..=1, like Ghostty's `cursor-opacity`; `1.0` is the default opaque fill with block cut-out).
    pub fn set_cursor_opacity(&mut self, opacity: f32) {
        self.note_host_visual_change();
        self.cpu.set_cursor_opacity(opacity);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_cursor_opacity(opacity);
            gpu.win.invalidate_present();
        }
    }

    /// Mark the pane unfocused (`true`) or focused (`false`).
    pub fn set_selection_inactive(&mut self, inactive: bool) {
        self.note_host_visual_change();
        self.cpu.set_selection_inactive(inactive);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_selection_inactive(inactive);
            gpu.win.invalidate_present();
        }
    }

    /// Set the inactive (unfocused) selection bg (0x00RRGGBB), or `undefined` to derive it from the active selection bg blended toward the theme bg.
    pub fn set_selection_inactive_bg(&mut self, bg: Option<u32>) {
        self.note_host_visual_change();
        self.cpu.set_selection_inactive_bg(bg);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_selection_inactive_bg(bg);
            gpu.win.invalidate_present();
        }
    }

    /// Re-rasterize at a new cell font px (DPI change) on both faces; the GPU renderer also drops its atlas.
    pub fn set_px(&mut self, px: f32) -> Result<(), String> {
        let (cw, ch, _) = self.cpu.cell_geometry(px);
        self.checked_surface_geometry(cw, ch, self.cpu.pad(), self.cpu.head())?;
        self.note_host_visual_change();
        self.cpu.set_px(px);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer.set_px(px);
            gpu.win.invalidate_present();
        }
        Ok(())
    }

    /// Resize the grid and, if the GPU is live, the swapchain.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        let (rows, cols) =
            frame_geometry_api::checked_grid_size(&self.cpu, rows, cols, self.band.committed())?;
        self.note_host_visual_change();
        if let Some(pending) = self.term.resize_offloading_scrollback(rows, cols) {
            if pending.line_count() <= INLINE_REFLOW_MAX_LINES {
                // Small history: rewrap now. A follow-up job would wedge the
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
        self.size_swapchain();
        Ok(())
    }

    /// Whether the next `render` would draw.
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

    /// Render-tick safety net (see `REFLOW_PUMP_GRACE_RENDERS`): after the grace
    #[inline]
    fn pump_reflow_on_render_tick(&mut self) {
        if self.pending_reflow.is_none() {
            return;
        }
        let grace = self.reflow_grace;
        if grace > 0 {
            self.reflow_grace = grace - 1;
            return;
        }
        self.pump_reflow();
    }

    /// Output safety net (see `REFLOW_BACKLOG_MAX_LINES`), called by `process`:
    #[inline]
    fn pump_reflow_on_output(&mut self) {
        if self.pending_reflow.is_some() && self.term.lazy_backlog_len() > REFLOW_BACKLOG_MAX_LINES
        {
            self.pump_reflow();
        }
    }

    /// Cell width in device pixels — the host computes cols = floor(canvasW / cellWidth).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn cell_width(&self) -> usize {
        self.cpu.cell_size().0
    }

    /// Cell height in device pixels — the host computes rows = floor(canvasH / cellHeight).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn cell_height(&self) -> usize {
        self.cpu.cell_size().1
    }

    /// True once [`AtermGpuTerminal::init`] has acquired a GPU + surface.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn gpu_ready(&self) -> bool {
        self.gpu.is_some()
    }

    /// The acquired GPU adapter name and backend once initialized, else empty.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
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
#[cfg_attr(target_arch = "wasm32", export)]
pub struct SearchMeta {
    incomplete: bool,
    match_count: u32,
}

#[cfg_attr(target_arch = "wasm32", export)]
impl SearchMeta {
    /// True when the results may be truncated.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn incomplete(&self) -> bool {
        self.incomplete
    }

    /// Number of matches the paired [`AtermGpuTerminal::search`] call returns (its flat triplet array length / 3), after any cap.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn match_count(&self) -> u32 {
        self.match_count
    }
}

/// One slice of a budgeted search ([`AtermGpuTerminal::search_budgeted`]).
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
    /// Stable match delta as flat `[abs_line, start_col, len]` triplets (same coordinate contract as [`AtermGpuTerminal::search`]).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn matches(&self) -> Vec<u32> {
        self.matches.clone()
    }

    /// Whether every retained row has been scanned and every match delta has been delivered.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Token to resume with; `undefined` once complete.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// Stable identity for the logical search, including its completing step; `undefined` only for an empty/invalid query result.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn search_id(&self) -> Option<u64> {
        self.search_id
    }

    /// Whether this step starts a new logical result stream.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn reset(&self) -> bool {
        self.reset
    }

    /// True when the results may be truncated (eviction or the match cap).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn incomplete_index(&self) -> bool {
        self.incomplete_index
    }

    /// Final oldest absolute line retained by the completed search index, stable from the first turn.
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

impl AtermGpuTerminal {
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

/// Stateless key encoder for worker-hosted engines.
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

/// A detected link under a cell.
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

    /// Inclusive start column of the link span.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub fn start_col(&self) -> u16 {
        self.start_col
    }

    /// Exclusive end column of the link span, a grid column as `start_col` is.
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

/// Selection bounds in display viewport cell coords (0 = top visible row), inclusive of `start`, with `end` side-adjusted to match `selection_text` and the painted highlight.
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

/// JSON-escape `s` and wrap it in double quotes for `take_osc_events` / `take_notifications`.
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

#[cfg(target_arch = "wasm32")]
#[export]
impl AtermGpuTerminal {
    /// Acquire the GPU and create + configure a WebGL2 surface on `canvas`.
    pub fn init(&mut self, canvas: aterm_gpu::web::HtmlCanvasElement) -> Result<(), String> {
        self.init_with_target(aterm_gpu::web::CanvasTarget::Canvas(canvas))
    }

    /// Worker variant: create the WebGL2 surface on a transferred `OffscreenCanvas`, so rendering and present run off the main thread.
    pub fn init_offscreen(
        &mut self,
        canvas: aterm_gpu::web::OffscreenCanvas,
    ) -> Result<(), String> {
        self.init_with_target(aterm_gpu::web::CanvasTarget::Offscreen(canvas))
    }

    /// Shared GPU bring-up for `init` and `init_offscreen`: context and
    fn init_with_target(&mut self, target: aterm_gpu::web::CanvasTarget) -> Result<(), String> {
        let (w, h) = self.checked_surface_size()?;
        let (ctx, canvas_surface) = GpuContext::for_canvas(target)?;

        let renderer = GpuRenderer::from_parts(ctx, self.gpu_face()?, None, self.theme)?;

        let surface = renderer
            .configure_canvas_surface(canvas_surface, w as u32, h as u32)
            .map_err(|e| format!("configure canvas surface failed: {e}"))?;

        self.gpu = Some(GpuState {
            renderer,
            surface,
            win: WindowGpu::new(),
        });
        self.last_frame_key = None;
        self.last_render_gated = false;
        Ok(())
    }

    /// Present one frame on the GPU canvas.
    pub fn render(&mut self) -> Result<(), String> {
        self.checked_surface_size()?;
        let Some(key) = self.open_frame() else {
            return if self.gpu.is_some() {
                Ok(())
            } else {
                Err("render() before init()".to_string())
            };
        };
        self.build_frame();
        self.record_presented_frame(key);
        let gpu = self.gpu.as_mut().ok_or("render() before init()")?;
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
            self.last_frame_key = None;
        }
        presented
    }

    /// Render the current grid offscreen and read the pixels back into the internal RGBA8 buffer, for GPU-vs-CPU pixel comparison (a WebGL swapchain is not CPU-readable).
    pub fn render_offscreen(&mut self) -> Result<(), String> {
        let (width, height) = self.checked_surface_size()?;
        let bytes = width * height * 4;
        self.rgba
            .try_reserve_exact(bytes.saturating_sub(self.rgba.len()))
            .map_err(|e| format!("cannot reserve RGBA framebuffer: {e}"))?;
        self.pump_reflow_on_render_tick();
        self.drain_compress_backlog_on_render();
        self.build_frame();
        self.last_frame_key = None;
        self.last_render_gated = false;
        let gpu = self
            .gpu
            .as_mut()
            .ok_or("render_offscreen() before init()")?;
        let frame = gpu
            .renderer
            .try_render_input(&mut gpu.win, &self.frame_scratch, None)?;
        self.fb_width = frame.width;
        self.fb_height = frame.height;
        self.rgba.clear();
        for &p in &frame.pixels {
            self.rgba.push((p >> 16) as u8);
            self.rgba.push((p >> 8) as u8);
            self.rgba.push(p as u8);
            self.rgba.push(0xff - (p >> 24) as u8);
        }
        Ok(())
    }

    /// Width in pixels of the last [`render_offscreen`](Self::render_offscreen) framebuffer.
    #[export(getter)]
    pub fn width(&self) -> usize {
        self.fb_width
    }

    /// Height in pixels of the last [`render_offscreen`](Self::render_offscreen) framebuffer.
    #[export(getter)]
    pub fn height(&self) -> usize {
        self.fb_height
    }

    /// Copy of the last [`render_offscreen`](Self::render_offscreen) RGBA8 framebuffer (`width*height*4` bytes), ready for `ctx.putImageData(new ImageData(rgba, width, height), 0, 0)` or a pixel diff.
    pub fn rgba(&self) -> Vec<u8> {
        self.rgba.clone()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl AtermGpuTerminal {
    /// The native half of `render`: the same frame gate and frame build, minus the WebGL present.
    pub fn render_headless(&mut self) -> Result<bool, String> {
        self.checked_surface_size()?;
        let Some(key) = self.open_frame() else {
            return Ok(true);
        };
        self.build_frame();
        self.record_presented_frame(key);
        Ok(false)
    }

    pub fn new_from_system(rows: u16, cols: u16, px: f32) -> Option<AtermGpuTerminal> {
        let theme = Theme::default();
        let cpu = Renderer::from_system(px, theme)?;
        let (rows, cols) = frame_geometry_api::checked_grid_size(&cpu, rows, cols, 0).ok()?;
        let mut term = scrollback_tiers_api::tiered_terminal(rows, cols);
        let budget_share = scrollback_tiers_api::register_budget_share(&term);
        apply_terminal_theme_colors(&mut term, theme.fg, theme.bg, theme.cursor, theme.selection);
        let notifications = notifications_api::wire_notification_queue(&mut term);
        let web_theme = theme_api::WebThemeState::new(term.color_scheme());
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
            pred_row_scratch: predict_api::PredRows::default(),
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
            web_theme,
        })
    }
}

/// A test terminal on the host's system font: `new_from_system`, or COULD NOT RUN where this host has no system monospace font.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn system_font_terminal(rows: u16, cols: u16, px: f32) -> AtermGpuTerminal {
    AtermGpuTerminal::new_from_system(rows, cols, px)
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font on this host"))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use aterm_core::render::FrameRefill;

    use super::*;

    /// The bundled face, injected as a JS host would. Paste tests use it so a
    const BUNDLED_FACE: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

    /// A link's columns and text come from the cells it is drawn in, whatever
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
    #[test]
    fn the_gpu_face_is_the_face_the_host_set() {
        let mut t = AtermGpuTerminal::new(8, 48, BUNDLED_FACE, 14.0, 0, 0, 0, 0)
            .expect("the bundled face parses");
        t.set_px(21.0).expect("admitted geometry");
        t.set_line_height(1.25);
        t.set_chrome(4, 2).expect("admitted geometry");
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
        t.process(b"x\r");
        t.process(b"program repaint\r");
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 1);

        assert!(t.note_console_input("text"));
        t.process("中文🙂".as_bytes());
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 2);
        assert!(!t.note_typed_char('\u{0301}'));
        assert_eq!(t.effects.cursor_pet_console_input_seq(), 3);

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
    #[test]
    fn paste_export_wraps_only_sanitized_text_so_a_planted_terminator_stays_inert() {
        let mut t = AtermGpuTerminal::new(24, 80, BUNDLED_FACE, 14.0, 0, 0, 0, 0)
            .expect("the bundled face parses");

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
    #[test]
    fn effects_config_mutators_bump_the_host_visual_generation() {
        let mut t = crate::system_font_terminal(6, 40, 14.0);
        let start = t.host_visual_gen;
        t.advance_effects(16.0);
        assert_eq!(
            t.host_visual_gen, start,
            "advance_effects must not bump — it runs every rAF tick"
        );

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
    fn settled(rows: u16, cols: u16) -> AtermGpuTerminal {
        let mut t = crate::system_font_terminal(rows, cols, 14.0);
        for r in 0..rows {
            t.process(format!("line {r} of selectable text\r\n").as_bytes());
        }
        t.render_headless().expect("renderable frame");
        t.render_headless().expect("renderable frame");
        assert!(t.last_render_skipped(), "setup must reach a settled frame");
        t
    }

    /// The frame gate, both sides: a settled tick skips the whole frame, and
    #[test]
    fn frame_gate_skips_settled_ticks_and_reopens_on_every_change_class() {
        let mut t = settled(8, 40);
        assert!(!t.needs_frame(), "needs_frame must agree with the gate");

        t.process(b"!");
        assert!(t.needs_frame(), "damage must reopen the gate");
        assert!(
            !t.render_headless().expect("renderable frame"),
            "an echo tick must draw"
        );
        assert!(
            t.render_headless().expect("renderable frame"),
            "and then re-settle"
        );

        t.set_cursor_blink_phase(false);
        assert!(
            !t.render_headless().expect("renderable frame"),
            "a blink flip must draw"
        );
        t.set_cursor_blink_phase(false);
        assert!(
            t.render_headless().expect("renderable frame"),
            "an idempotent blink re-assert must gate"
        );

        t.selection_start(2, 1);
        t.selection_extend(2, 8);
        assert!(
            !t.render_headless().expect("renderable frame"),
            "a selection change must draw"
        );
        t.selection_extend(2, 8);
        assert!(
            t.render_headless().expect("renderable frame"),
            "a cell-identical extend must be gated away"
        );
        t.selection_extend(2, 9);
        assert!(
            !t.render_headless().expect("renderable frame"),
            "a one-cell move must draw"
        );
        t.selection_clear();
        assert!(
            !t.render_headless().expect("renderable frame"),
            "clearing a selection must draw"
        );
        t.render_headless().expect("renderable frame");

        t.scroll_lines(3);
        assert!(
            !t.render_headless().expect("renderable frame"),
            "a viewport scroll must draw"
        );
        t.render_headless().expect("renderable frame");
        t.scroll_to_bottom();
        assert!(
            !t.render_headless().expect("renderable frame"),
            "and scrolling back must draw"
        );
        t.render_headless().expect("renderable frame");

        t.set_theme(0x00FF_0000, 0x0000_2200, 0x00FF_FFFF, 0x0033_3366);
        assert!(
            !t.render_headless().expect("renderable frame"),
            "a theme change must draw"
        );
        t.render_headless().expect("renderable frame");
        t.set_palette_color(1, 9, 9, 9);
        assert!(
            !t.render_headless().expect("renderable frame"),
            "a palette change must draw"
        );
        t.render_headless().expect("renderable frame");

        t.set_matrix_rain_enabled(true);
        assert!(
            !t.render_headless().expect("renderable frame"),
            "an ignited effect must draw"
        );
        let mut saw_active = false;
        for i in 0u32..60 {
            // Real output now and then: rain holds still on a pane that is only
            if i.is_multiple_of(10) {
                t.process(b"x");
            }
            t.advance_effects(16.0);
            let active = t.effects.is_active();
            saw_active |= active;
            let gated = t.render_headless().expect("renderable frame");
            if active {
                assert!(!gated, "an active effects pipeline must never gate");
                assert!(t.needs_frame(), "and needs_frame must say so");
            }
        }
        assert!(
            saw_active,
            "the rain session never went active — this case proved nothing"
        );
        t.set_matrix_rain_enabled(false);
        let mut settled = false;
        for _ in 0..10 {
            if t.render_headless().expect("renderable frame") {
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
    #[test]
    fn every_visual_mutator_bumps_the_host_visual_generation() {
        let mut t = settled(8, 40);
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
            ("set_chrome", |t| {
                t.set_chrome(4, 2).expect("admitted geometry")
            }),
            ("set_cursor_opacity", |t| t.set_cursor_opacity(0.5)),
            ("set_selection_inactive", |t| t.set_selection_inactive(true)),
            ("set_selection_inactive_bg", |t| {
                t.set_selection_inactive_bg(Some(0x0020_2020))
            }),
            ("set_px", |t| t.set_px(15.0).expect("admitted geometry")),
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
            ("resize", |t| t.resize(8, 40).expect("admitted geometry")),
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
            t.render_headless().expect("renderable frame");
        }
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
        let mut t = crate::system_font_terminal(3, 8, 16.0);
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
    fn webgl_present_result_propagates_every_surface_failure() {
        assert_eq!(webgl_present_result(Ok(())), Ok(()));
        for failure in [
            SurfacePresentFailure::Reconfigured,
            SurfacePresentFailure::Timeout,
            SurfacePresentFailure::Occluded,
            SurfacePresentFailure::Validation,
        ] {
            assert!(webgl_present_result(Err(failure)).is_err(), "{failure:?}");
        }
    }

    /// `search_meta` agrees with `search` and carries the `incomplete` signal
    #[test]
    fn search_meta_agrees_with_search_and_is_silent_on_empty_or_bad_input() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
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

    /// Reject an oversized frame before constructing or changing a grid. Existing
    #[test]
    fn oversized_frames_are_refused_before_state_changes() {
        let new = |rows, cols| {
            AtermGpuTerminal::new(
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
        t.render_headless().expect("first frame");
        t.render_headless().expect("settled frame");
        let frame_rows = t.frame_scratch.rows;
        let frame_cols = t.frame_scratch.cols;
        let metrics = (
            t.cell_width(),
            t.cell_height(),
            t.chrome_pad(),
            t.chrome_head(),
        );
        let key = t.last_frame_key;
        let generation = t.host_visual_gen;
        type Change = fn(&mut AtermGpuTerminal) -> Result<(), String>;
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
            assert_eq!(
                (t.frame_scratch.rows, t.frame_scratch.cols),
                (frame_rows, frame_cols),
                "{name}: previous frame remains"
            );
        }
        t.set_line_height(1_000_000.0);
        assert!(
            t.render_headless().is_err(),
            "oversized geometry is refused before building a frame"
        );
        assert_eq!(
            (t.frame_scratch.rows, t.frame_scratch.cols),
            (frame_rows, frame_cols)
        );
        t.set_line_height(1.0);
        t.render_headless()
            .expect("corrected geometry builds again");
        t.resize(0, 0).expect("zero resize clamps up");
        assert_eq!((t.rows, t.cols), (1, 1));

        let mut banded =
            AtermGpuTerminal::new(2, 1, BUNDLED_FACE, 6_000.0, 0xffffff, 0, 0xffffff, 0x444444)
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
            banded.render_headless().is_err(),
            "the added physical band row exceeds the budget"
        );
        assert_eq!(banded.frame_scratch.rows, 0, "no frame was built");
    }

    /// Shrinking retention invalidates the search index like any content
    #[test]
    fn retention_shrink_search_returns_no_evicted_rows() {
        let mut t = crate::system_font_terminal(5, 40, 16.0);
        for i in 0..300 {
            t.process(format!("needle-{i}\r\n").as_bytes());
        }
        assert_eq!(t.search("needle-", true, false).len() / 3, 300);

        t.set_scrollback_limit(50);
        let hits = t.search("needle-", true, false);
        assert_eq!(hits.len() / 3, 54, "evicted rows no longer match");
        assert_eq!(hits[0], 246, "the first match is the oldest RETAINED row");
    }

    /// `set_scrollback_limit` is one total bound across ring + staged + store:
    #[test]
    fn set_scrollback_limit_governs_ring_retention() {
        let mut t = crate::system_font_terminal(5, 40, 16.0);
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

    /// win32-input-mode is the host's to grant: conhost's `?9001h` changes
    #[test]
    fn a_conpty_host_opts_in_to_win32_input_records() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
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

    /// The stateless encoder over mirrored `keyboard_mode_bits` (the worker path).
    #[test]
    fn encode_key_with_mode_matches_the_instance_encoder() {
        assert_eq!(
            encode_key_with_mode("ArrowUp", 0, 0, None, 0).as_deref(),
            Some(&b"\x1b[A"[..])
        );
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        assert_eq!(t.keyboard_mode_bits(), 0, "fresh terminal: no mode bits");
        t.process(b"\x1b[?1h");
        assert_eq!(
            encode_key_with_mode("ArrowUp", 0, 0, None, t.keyboard_mode_bits()).as_deref(),
            Some(&b"\x1bOA"[..]),
            "snapshot bits under DECCKM must be SS3"
        );
        t.process(b"\x1b[?1l");
        t.process(b"\x1b[>1u");
        assert_eq!(
            encode_key_with_mode("Enter", 1, 0, None, t.keyboard_mode_bits()).as_deref(),
            Some(&b"\x1b[13;2u"[..]),
            "snapshot bits under pushed kitty must be CSI-u Shift+Enter"
        );
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
    #[test]
    fn build_frame_echo_refills_one_row_with_sparkle_and_rain_on() {
        let mut t = crate::system_font_terminal(8, 40, 16.0);
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
    #[test]
    fn build_frame_scoped_refill_matches_a_full_extraction_over_a_mutation_stream() {
        let mut t = crate::system_font_terminal(8, 40, 16.0);
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
        t.resize(10, 30).expect("admitted geometry");
        t.build_frame();
        check(&mut t, "resize");
        t.process(b"after resize");
        t.build_frame();
        check(&mut t, "echo after resize");
        assert!(scoped >= 5, "the scoped arm must be reached ({scoped})");
        assert!(full >= 2, "the full arm must be reached ({full})");
    }

    /// Native stand-in for the effects render path (GPU init is wasm-only):
    fn fill_frame(t: &mut AtermGpuTerminal) {
        t.refill_frame_scratch();
        let (cw, ch) = t.cpu.cell_size();
        t.effects.apply(&mut t.term, &mut t.frame_scratch, cw, ch);
        t.spill.update(&t.cpu, &t.frame_scratch);
    }

    /// Snapshot equality across two engine instances.
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
    #[test]
    fn spill_exports_surface_band_content_on_the_cpu_face() {
        let mut t = crate::system_font_terminal(12, 40, 16.0);
        fill_frame(&mut t);
        assert_eq!(t.spill_len(), 0, "0/0 chrome: identity — no band, no bytes");
        assert_eq!(t.spill_rev(), 0);

        let (pad, head) = (12usize, 30usize);
        t.set_chrome(pad as u16, head as u16)
            .expect("admitted geometry");
        t.set_cursor_glow(true, "water", None, None, 400, 64, 1.0, 2.0, true);
        fill_frame(&mut t);
        for (ch, bytes) in [('a', b"a"), ('b', b"b"), ('c', b"c")] {
            assert!(
                t.note_typed_char(ch),
                "a witnessed simple scalar must arm movement provenance"
            );
            t.process(bytes);
            t.advance_effects(30.0);
            fill_frame(&mut t);
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

        t.effects.set_serious(true);
        t.process(b"\x1b[1;30Hz");
        const DRAIN_FRAMES: usize = 125;
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

        let ptr = t.spill_ptr();
        t.advance_effects(16.0);
        fill_frame(&mut t);
        assert_eq!(t.spill_ptr(), ptr, "same geometry ⇒ the export never moves");

        let rev = t.spill_rev();
        fill_frame(&mut t);
        assert_eq!(t.spill_rev(), rev, "idle re-frame must not tick the rev");
        assert_eq!(t.spill_rect_count(), 0);

        t.set_chrome(0, 0).expect("admitted geometry");
        fill_frame(&mut t);
        assert_eq!(t.spill_len(), 0);
        let rev = t.spill_rev();
        fill_frame(&mut t);
        assert_eq!(t.spill_rev(), rev, "steady 0/0 chrome stays still");
    }

    /// `set_chrome` before GPU init sticks on the CPU face (init re-applies it
    #[test]
    fn set_chrome_sticks_pre_init_and_zero_restores() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        assert_eq!((t.cpu.pad(), t.cpu.head()), (0, 0), "exact-fit by default");
        t.set_chrome(8, 24).expect("admitted geometry");
        assert_eq!((t.cpu.pad(), t.cpu.head()), (8, 24));
        let (cw, ch) = t.cpu.cell_size();
        let (w, h) = t.cpu.frame_size(24, 80);
        assert_eq!(w, 80 * cw + 16, "frame gains 2*pad in width");
        assert_eq!(h, 24 * ch + 16 + 24, "frame gains 2*pad + head in height");
        t.set_chrome(0, 0).expect("admitted geometry");
        assert_eq!(t.cpu.frame_size(24, 80), (80 * cw, 24 * ch));
    }

    /// Two instances fed the same bytes and dt stream produce identical
    #[test]
    fn effects_snapshots_are_deterministic_and_restore_on_off() {
        let mut a = crate::system_font_terminal(24, 80, 16.0);
        let mut b = crate::system_font_terminal(24, 80, 16.0);
        let mut plain = crate::system_font_terminal(24, 80, 16.0);
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

    /// The word-separator setting reshapes double-click words, and clearing it
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

    /// Swap the engine for one whose bulk history lives in the tiered store
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
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        t.term = Terminal::new(24, 80);
        for i in 0..50 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        t.resize(24, 40).expect("admitted geometry");
        assert!(!t.reflow_pending(), "no tiered store → no deferred job");
        assert!(!t.pump_reflow(), "nothing to pump");
        assert_eq!(t.cols, 40, "resize applied synchronously");
    }

    #[test]
    fn scrolled_wrapped_row_len_and_wrap_flag_are_tier_aware() {
        let mut t = crate::system_font_terminal(3, 40, 16.0);
        let a = "A".repeat(30);
        let b = "B".repeat(30);
        let c = "C".repeat(30);
        t.process(format!("{a}\r\n").as_bytes());
        t.process(format!("{b}\r\n").as_bytes());
        t.process(c.as_bytes());

        t.resize(3, 20).expect("admitted geometry");
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

    #[test]
    fn large_tiered_history_defers_rewrap_and_stepped_pumps_reattach_it() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
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
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();

        t.resize(24, 40).expect("admitted geometry");
        assert!(t.reflow_pending());
        t.pump_reflow_budget(500);
        for _ in 0..3 {
            assert!(t.pump_reflow(), "mid-stepping: work must remain");
        }
        assert!(t.reflow_pending(), "job is half-stepped");

        t.resize(24, 60).expect("admitted geometry");
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
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        install_tiered_history(&mut t, 24, 80, 500);
        let before = t.term.grid().scrollback_lines();
        assert!(before > 100, "precondition: deep history ({before} lines)");
        t.resize(24, 40).expect("admitted geometry");
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
    fn never_pumped_host_completes_via_the_render_grace_net() {
        let mut t = crate::system_font_terminal(24, 80, 16.0);
        install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
        let before = t.term.grid().scrollback_lines();
        t.resize(24, 40).expect("admitted geometry");
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
        assert!(
            t.term.grid().scrollback_lines() > INLINE_REFLOW_MAX_LINES,
            "history survived the stream-while-detached window"
        );
    }

    /// Budgeted search: resumed slices complete to the one-shot triplets, a
    #[test]
    fn search_budgeted_matches_one_shot_and_restarts_when_superseded() {
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
        assert_eq!(all_matches, one_shot, "budgeted equals one-shot");
        assert_eq!(step.lowest_retained_line(), 0);

        let first = t.search_budgeted("NEEDLE", true, false, None, 5);
        assert!(!first.complete());
        assert!(first.reset());
        let first_id = first.search_id();
        let switched = t.search_budgeted("row", true, false, first.cursor(), 5);
        assert!(switched.rows_fed() <= 5, "superseded cursor restarted");
        assert!(switched.reset());
        assert_ne!(switched.search_id(), first_id);
        t.search_budgeted_cancel();
        let empty = t.search_budgeted("", true, false, switched.cursor(), 5);
        assert!(empty.complete() && empty.matches().is_empty());
        assert!(empty.reset() && empty.search_id().is_none());
        let bad = t.search_budgeted("f(oo", false, true, None, 5);
        assert!(bad.complete() && bad.matches().is_empty());
        assert!(bad.reset() && bad.search_id().is_none());
    }
}
