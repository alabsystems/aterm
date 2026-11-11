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
/// aterm-gui and aterm-wasm.
const INLINE_REFLOW_MAX_LINES: usize = 20_000;

/// `render` / `render_offscreen` calls with a stashed rewrap before each frame runs one budgeted step.
const REFLOW_PUMP_GRACE_RENDERS: u32 = 120;

/// Lazy-buffer backlog past which every `process` call pumps one budgeted step.
const REFLOW_BACKLOG_MAX_LINES: usize = 20_000;

/// Default per-pump rewrap budget in input history lines (tunable via
/// [`AtermGpuTerminal::pump_reflow_budget`]); a few milliseconds of work.
const REFLOW_STEP_BUDGET_LINES: usize = 2_000;

#[cfg(target_arch = "wasm32")]
use aterm_gpu::GpuContext;
#[cfg(any(target_arch = "wasm32", test))]
use aterm_gpu::SurfacePresentFailure;
use aterm_gpu::{GpuRenderer, GpuSurface, WindowGpu};

std::thread_local! {
    /// Host-registered font blobs, copied across the JS/wasm boundary once per
    /// module.
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

/// WebGL2 terminal engine.
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
    display_row_cache: std::cell::RefCell<facade_api::DisplayRowCache>,
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
/// `render()` calls at stable dims/config.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FrameGateKey {
    damage_epoch: u64,
    host_visual_gen: u64,
    effects_active: bool,
}

/// Keep the engine's implicit cells and OSC 110/111/112/117 reset baselines
/// aligned with the renderer theme: frame refills take sparse row tails and
/// selection colours from `Terminal`, so a renderer-only theme change is lost.
#[inline]
fn apply_terminal_theme_colors(term: &mut Terminal, fg: u32, bg: u32, cursor: u32, selection: u32) {
    let mut base = term.color_baseline();
    base.foreground = theme_api::unpack(fg);
    base.background = theme_api::unpack(bg);
    base.cursor = Some(theme_api::unpack(cursor));
    base.selection_background = Some(theme_api::unpack(selection));
    term.rebase_colors(&base);
}

/// The GPU half of the terminal, populated by `AtermGpuTerminal::init`.
struct GpuState {
    renderer: GpuRenderer,
    surface: GpuSurface,
    win: WindowGpu,
    /// The browser lane's crown: its wall-clock ease reset before each
    /// present, so every run meets the same budget (`set_bloom_for_lane`).
    #[cfg(all(target_arch = "wasm32", debug_assertions))]
    lane_crown: bool,
    /// The browser lane's fx art, drawn beside the glow in every frame
    /// (`set_fx_art_for_lane`); empty for none.
    #[cfg(all(target_arch = "wasm32", debug_assertions))]
    lane_art: Vec<aterm_core::render::FxLayer>,
}

/// Convert the GPU crate's typed swapchain failure into the string error
/// exposed across the module boundary; target-neutral so native tests can prove a
/// dropped present never becomes `Ok(())`.
#[cfg(any(target_arch = "wasm32", test))]
fn webgl_present_result(result: Result<(), SurfacePresentFailure>) -> Result<(), String> {
    result.map_err(|failure| format!("WebGL canvas present failed: {failure:?}"))
}

#[cfg(any(target_arch = "wasm32", test))]
impl AtermGpuTerminal {
    /// The CPU face `init` builds the GPU renderer on: from the injected font
    /// bytes at the live face's px, without system font discovery, carrying every
    /// face and setting the host applied before init.
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
    /// tick draws.
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

    /// One tick through the frame gate, shared by `render` and `render_headless`: the geometry
    /// preflight, then a refusal before `init` (`ready` false) that runs no pump and builds no
    /// frame, then `open_frame`, `build_frame` and `present`. The frame counts as on glass only
    /// once `present` succeeded; a failed present leaves the gate open. `Ok(true)` when the gate
    /// held the tick.
    fn present_frame(
        &mut self,
        ready: bool,
        present: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<bool, String> {
        self.checked_surface_size()?;
        if !ready {
            return Err("render() before init()".to_string());
        }
        let Some(key) = self.open_frame() else {
            return Ok(true);
        };
        self.build_frame();
        let presented = present(self);
        if presented.is_ok() {
            self.record_presented_frame(key);
        } else {
            self.last_frame_key = None;
        }
        presented.map(|()| false)
    }

    /// Record the frame `build_frame` just produced as the one on glass.
    #[inline]
    const fn record_presented_frame(&mut self, key: FrameGateKey) {
        self.last_frame_key = Some(key);
        self.last_present_frac = self.frame_scratch.scroll_frac_px;
    }

    /// Build one frame into the kept scratch; shared by `render`,
    /// `render_offscreen` and the native `render_headless` seam.
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
    /// session through the engine's one damage consumer,
    /// [`Terminal::cell_frame_damage_scoped_into`]: only damaged rows re-resolve
    /// under its continuity proof, else it refills fully.
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
    pub(crate) const fn note_host_visual_change(&mut self) {
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
    /// the grid plus committed band rows at the renderer's pad and
    /// head.
    fn size_swapchain(&mut self) {
        let Ok((w, h)) = self.checked_surface_size() else {
            return;
        };
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.renderer
                .resize_surface(&mut gpu.win, &mut gpu.surface, w as u32, h as u32);
        }
    }
}

/// The renderer half of a theme install, which `theme_api` (shared with
/// aterm-wasm) calls on whichever crate compiles it.
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
            display_row_cache: std::cell::RefCell::new(facade_api::DisplayRowCache::default()),
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
    pub const fn chrome_pad(&self) -> u16 {
        self.cpu.pad() as u16
    }

    /// The chrome top head band set via [`Self::set_chrome`] (px; 0 = none).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn chrome_head(&self) -> u16 {
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
                // detach window if dropped, so route any to the pump.
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
    ///
    /// Applies any pending global scrollback-budget share before reading `damage_epoch`;
    /// only when scrolled into history with staged lines still unpromoted can `render`'s
    /// bounded backlog trickle clamp `display_offset` and open a frame afterwards.
    pub fn needs_frame(&mut self) -> bool {
        self.apply_pending_scrollback_budget();
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

    /// Pump one budgeted rewrap step per frame after [`REFLOW_PUMP_GRACE_RENDERS`].
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

    /// Pump one budgeted rewrap step when staged output exceeds [`REFLOW_BACKLOG_MAX_LINES`].
    #[inline]
    fn pump_reflow_on_output(&mut self) {
        if self.pending_reflow.is_some() && self.term.lazy_backlog_len() > REFLOW_BACKLOG_MAX_LINES
        {
            self.pump_reflow();
        }
    }

    /// Cell width in device pixels — the host computes cols = floor(canvasW / cellWidth).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn cell_width(&self) -> usize {
        self.cpu.cell_size().0
    }

    /// Cell height in device pixels — the host computes rows = floor(canvasH / cellHeight).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn cell_height(&self) -> usize {
        self.cpu.cell_size().1
    }

    /// True once `AtermGpuTerminal::init` has acquired a GPU + surface.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn gpu_ready(&self) -> bool {
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
    pub const fn incomplete(&self) -> bool {
        self.incomplete
    }

    /// Number of matches the paired [`AtermGpuTerminal::search`] call returns (its flat triplet array length / 3), after any cap.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn match_count(&self) -> u32 {
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
    pub const fn complete(&self) -> bool {
        self.complete
    }

    /// Token to resume with; `undefined` once complete.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// Stable identity for the logical search, including its completing step; `undefined` only for an empty/invalid query result.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn search_id(&self) -> Option<u64> {
        self.search_id
    }

    /// Whether this step starts a new logical result stream.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn reset(&self) -> bool {
        self.reset
    }

    /// True when the results may be truncated (eviction or the match cap).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn incomplete_index(&self) -> bool {
        self.incomplete_index
    }

    /// Final oldest absolute line retained by the completed search index, stable from the first turn.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn lowest_retained_line(&self) -> u32 {
        self.lowest_retained_line
    }

    /// Rows scanned so far (progress numerator; restarts reset it).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn rows_fed(&self) -> u32 {
        self.rows_fed
    }

    /// Total rows this search will scan (progress denominator).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn total_rows(&self) -> u32 {
        self.total_rows
    }
}

impl AtermGpuTerminal {
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
    pub const fn start_col(&self) -> u16 {
        self.start_col
    }

    /// Exclusive end column of the link span, a grid column as `start_col` is.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn end_col(&self) -> u16 {
        self.end_col
    }

    /// Link kind: 0=osc8, 1=url, 2=file_path, 3=other.
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn kind(&self) -> u8 {
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
    pub const fn start_x(&self) -> u16 {
        self.start_x
    }

    /// Start row (display-relative, 0 = top visible row).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn start_y(&self) -> u16 {
        self.start_y
    }

    /// End column (display-relative, side-adjusted/inclusive).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn end_x(&self) -> u16 {
        self.end_x
    }

    /// End row (display-relative).
    #[cfg_attr(target_arch = "wasm32", export(getter))]
    pub const fn end_y(&self) -> u16 {
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
    /// pending canvas surface from `target`, then the renderer and swapchain
    /// config.
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
            #[cfg(all(target_arch = "wasm32", debug_assertions))]
            lane_crown: false,
            #[cfg(all(target_arch = "wasm32", debug_assertions))]
            lane_art: Vec::new(),
        });
        self.last_frame_key = None;
        self.last_render_gated = false;
        Ok(())
    }

    /// Present one frame on the GPU canvas.
    pub fn render(&mut self) -> Result<(), String> {
        let ready = self.gpu.is_some();
        self.present_frame(ready, |t| {
            let gpu = t.gpu.as_mut().ok_or("render() before init()")?;
            #[cfg(all(target_arch = "wasm32", debug_assertions))]
            if gpu.lane_crown {
                gpu.renderer.reset_glow_ease_for_test(&mut gpu.win);
            }
            #[cfg(all(target_arch = "wasm32", debug_assertions))]
            if !gpu.lane_art.is_empty() {
                let layers = &mut t.frame_scratch.fx_layers;
                layers.retain(|l| !gpu.lane_art.iter().any(|a| a.id == l.id));
                layers.extend(gpu.lane_art.iter().cloned());
            }
            webgl_present_result(gpu.renderer.present_input(
                &mut gpu.win,
                &mut gpu.surface,
                &t.frame_scratch,
                false,
                None,
                None,
            ))
        })
        .map(|_| ())
    }

    /// Render the current grid offscreen and read the pixels back into the internal RGBA8 buffer, for GPU-vs-CPU pixel comparison (a WebGL swapchain is not CPU-readable).
    pub fn render_offscreen(&mut self) -> Result<(), String> {
        let (width, height) = self.checked_surface_size()?;
        if self.gpu.is_none() {
            return Err("render_offscreen() before init()".to_string());
        }
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

/// The SDR crown's strength in the browser lane: `cursor_glow_sdr_boost`'s
/// default.
#[cfg(all(target_arch = "wasm32", debug_assertions))]
const LANE_CROWN: f32 = 0.25;

/// The browser lane's fx art (`set_fx_art_for_lane`) over the glow scene's
/// prompt row and the row below it, window pixels from `origin` in cells of
/// `(cw, ch)`: exact rects at Under, Glow and Over, which share the fx
/// streams the glow ramps ride, and a linear sprite at Over, a stream of its
/// own (the pet's and the word icons' sprites are linear layers).
#[cfg(all(target_arch = "wasm32", debug_assertions))]
fn lane_art(origin: (usize, usize), (cw, ch): (usize, usize)) -> Vec<aterm_core::render::FxLayer> {
    use aterm_core::render::{
        FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, FxSprite,
        FxTexels, PxRect,
    };
    use std::sync::Arc;
    let px = |v: usize| i32::try_from(v).unwrap_or(i32::MAX);
    let at = |k: usize| {
        (
            px(origin.0 + (2 + 5 * k) * cw),
            px(origin.1 + (k % 2) * ch + 3),
        )
    };
    let (w, h) = (
        u16::try_from(4 * cw).unwrap_or(u16::MAX),
        u16::try_from(ch / 2).unwrap_or(u16::MAX),
    );
    let layer = |id: u64, slot: FxSlot, blend: FxBlend, items: Vec<FxItem>, ink: FxInk| {
        FxLayer::new(
            0x4c41_4e45_0000 + id,
            Arc::new(FxContent::new(items, vec![ink; 2])),
            slot,
            FxSpace::Window,
            blend,
        )
    };
    let rects = |from: usize| {
        (from..from + 3)
            .map(|k| {
                let (x, y) = at(k);
                FxItem::Rect(FxRect { x, y, w, h, ink: 0 })
            })
            .collect()
    };
    let texels: Vec<u8> = (0..16u8)
        .flat_map(|k| {
            [
                0x30 + 9 * k,
                0xC0 - 7 * k,
                0x60,
                [0, 0x50, 0xB0, 0xFF][usize::from(k % 4)],
            ]
        })
        .collect();
    let Some(sprite) = FxArt::new(4, 4, FxTexels::Rgba8Straight, texels) else {
        return Vec::new();
    };
    let sprite = Arc::new(sprite);
    let sprites = (2..5)
        .map(|k| {
            let (x, y) = at(k);
            FxItem::Sprite(FxSprite {
                x,
                y,
                dw: w,
                dh: h,
                flip_x: k % 2 == 1,
                art: Arc::clone(&sprite),
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })
        })
        .collect();
    vec![
        layer(
            0,
            FxSlot::Under,
            FxBlend::Exact,
            rects(0),
            FxInk::over(0x0030_80C0, 0xA0),
        ),
        layer(
            1,
            FxSlot::Glow,
            FxBlend::Exact,
            rects(1),
            FxInk::add(0x0040_2010),
        ),
        layer(
            2,
            FxSlot::Over,
            FxBlend::Exact,
            rects(2),
            FxInk::over(0x00C0_4080, 0x60),
        ),
        layer(
            3,
            FxSlot::Over,
            FxBlend::Linear,
            sprites,
            FxInk::over(0x00FF_FFFF, 0xFF),
        ),
    ]
}

/// The browser lane's glow scene (`verify browser`, `crates/aterm-verify/
/// browser/lane.mjs`): a dev build's seams that route the cursor glow's
/// beam runs, so its ramps are held to the quads they stand for through
/// `cell.glsl` and `hdr_glow.glsl` on WebGL2.
#[cfg(all(target_arch = "wasm32", debug_assertions))]
#[export]
impl AtermGpuTerminal {
    /// Lay every beam run as a ramp drawn in the fx stream (`ramps`), or every
    /// row as the quad it stands for (the oracle). Call after `init`.
    pub fn set_glow_ramps_for_lane(&mut self, ramps: bool) -> Result<(), String> {
        let gpu = self
            .gpu
            .as_mut()
            .ok_or("set_glow_ramps_for_lane() before init()")?;
        let (cost, route) = if ramps {
            (aterm_render::RampCost::ALWAYS, aterm_gpu::RampsFx::Always)
        } else {
            (aterm_render::RampCost::NEVER, aterm_gpu::RampsFx::Never)
        };
        gpu.renderer.set_ramps_fx_for_test(Some(route));
        self.effects.cursor_mut().glow.set_ramp_cost(cost);
        Ok(())
    }

    /// Turn the cursor glow's bloom and its SDR crown (at
    /// `cursor_glow_sdr_boost`'s default, its ease reset before each present)
    /// on or off. Call after `init`.
    pub fn set_bloom_for_lane(&mut self, on: bool) -> Result<(), String> {
        let gpu = self
            .gpu
            .as_mut()
            .ok_or("set_bloom_for_lane() before init()")?;
        gpu.renderer.set_bloom(on);
        gpu.renderer
            .set_sdr_glow_boost(if on { LANE_CROWN } else { 0.0 });
        gpu.lane_crown = on;
        Ok(())
    }

    /// Draw the fx art of [`lane_art`] beside the glow in every frame
    /// (`on`), or none: the fx stream a frame opens for its own art, the
    /// one its ramps ride beside it. Call after `init`.
    pub fn set_fx_art_for_lane(&mut self, on: bool) -> Result<(), String> {
        let origin = (self.cpu.pad(), self.cpu.grid_top());
        let cell = self.cpu.cell_size();
        let gpu = self
            .gpu
            .as_mut()
            .ok_or("set_fx_art_for_lane() before init()")?;
        gpu.lane_art = if on {
            lane_art(origin, cell)
        } else {
            Vec::new()
        };
        Ok(())
    }

    /// The last frame's glow ramps and the lit rows they carry, over both
    /// glow streams, then the additive stream's ramps (the ones the bloom
    /// and the crown read as their item list): `[ramps, lit rows, additive]`.
    pub fn glow_ramps_for_lane(&self) -> Vec<u32> {
        let add = &self.frame_scratch.cursor_glow_add_ramps;
        let streams = [&self.frame_scratch.glow_under_ramps, add];
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let ramps = count(streams.iter().map(|s| s.items.len()).sum());
        let lit = count(streams.iter().map(|s| s.lit).sum());
        vec![ramps, lit, count(add.items.len())]
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl AtermGpuTerminal {
    /// The native half of `render`: the same frame gate and frame build, minus the WebGL present.
    pub fn render_headless(&mut self) -> Result<bool, String> {
        self.present_frame(true, |_| Ok(()))
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
            display_row_cache: std::cell::RefCell::new(facade_api::DisplayRowCache::default()),
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
#[path = "lib_tests.rs"]
mod tests;
