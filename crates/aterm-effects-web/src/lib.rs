// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Effects-only WebAssembly binding for PHOSPHOR rain.
//!
//! This crate intentionally exports no terminal parser. [`AtermRainOverlay`]
//! consumes the embedding terminal's authoritative, post-parse visible cells
//! through one persistent four-`u32`-lanes-per-cell staging buffer. Its packed
//! output and atlas accessors are pointers into resident wasm linear memory, so
//! a WebGL/Canvas host can use typed-array views without per-frame JS objects.
//!
//! Mutable calls may grow wasm memory and detach existing JS views. Reacquire a
//! view from `wasm.memory.buffer` after `resize_staging`, `sync_snapshot`, or
//! `emit`; consume output synchronously before the next mutable call.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use aterm_effects::matrix_overlay::{
    EXTERNAL_CELL_DEFAULT_BACKGROUND, EXTERNAL_CELL_INLINE_IMAGE, EXTERNAL_CELL_OVERLINE,
    EXTERNAL_CELL_SELECTED, EXTERNAL_CELL_STRIKETHROUGH, EXTERNAL_CELL_UNDERLINE,
    EXTERNAL_CELL_WIDE_CONTINUATION, EXTERNAL_RAIN_CELL_WORDS, EXTERNAL_RAIN_OPAQUE_SCALAR,
    ExternalRainCell, ExternalRainGeometry, ExternalRainLiveState, ExternalRainOverlay,
    ExternalRainSnapshot, ExternalRainSync, MAX_EXTERNAL_HIDDEN_CURSOR_ROWS,
    MAX_EXTERNAL_RAIN_CELLS, MAX_EXTERNAL_RAIN_COLS, MAX_EXTERNAL_RAIN_ROWS,
};
use aterm_effects::matrix_rain::{
    MAX_RAIN_ADD, MAX_RAIN_QUADS, RainConfig, RainHue, RainVisibility,
};

/// Packed [`aterm_render::SpriteQuad`] word count and order:
/// `row,x,y,w,h,ax,ay,aw,ah,tint,alpha,flip_x`.
const QUAD_WORDS: usize = 12;
/// Packed [`aterm_render::RainHalo`] word count and order:
/// `row,x,y,w,h,color,cx,cy,rx,ry`.
const HALO_WORDS: usize = 10;

const SYNC_UNCHANGED: u8 = 0;
const SYNC_RESAMPLED: u8 = 1;
const SYNC_RESCANNED: u8 = 2;
const SYNC_DEFERRED: u8 = 3;

/// Parser-free PHOSPHOR overlay for an external browser terminal.
///
/// The host writes `rows*cols*4` `u32` words at [`Self::staging_ptr`] in
/// row-major `scalar,fg,bg,flags` order, then calls [`Self::sync_snapshot`].
/// The core copies/scans only when `revision` or a sampling gate changes.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct AtermRainOverlay {
    overlay: ExternalRainOverlay,
    config: RainConfig,
    rows: u16,
    cols: u16,
    staging: Vec<ExternalRainCell>,
    row_flags: Vec<u32>,
    single_width_rows: Vec<bool>,
    cursor: Option<(u16, u16)>,
    hidden_cursor_rows: [u16; MAX_EXTERNAL_HIDDEN_CURSOR_ROWS],
    hidden_cursor_rows_len: usize,
    display_offset: i32,
    is_alt_screen: bool,
    clock_remainder_ms: f64,
    packed_quads: Vec<u32>,
    packed_halos: Vec<u32>,
    has_fresh_sync: bool,
    output_valid: bool,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermRainOverlay {
    /// Construct an enabled overlay with MatrixRain defaults.
    ///
    /// `seed_hi:seed_lo` forms the deterministic 64-bit replay seed. The
    /// constructor allocates all bounded output storage up front; only staging
    /// geometry and atlas generations may grow wasm memory later.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(
        rows: u16,
        cols: u16,
        default_bg: u32,
        theme_fg: u32,
        seed_lo: u32,
        seed_hi: u32,
    ) -> Result<AtermRainOverlay, String> {
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();

        validate_dimensions(rows, cols)?;
        let config = RainConfig {
            enabled: true,
            default_bg: default_bg & 0x00FF_FFFF,
            theme_fg: theme_fg & 0x00FF_FFFF,
            seed: u64::from(seed_lo) | (u64::from(seed_hi) << 32),
            ..RainConfig::default()
        };
        let count = usize::from(rows) * usize::from(cols);
        let staging = vec![ExternalRainCell::blank(config.theme_fg, config.default_bg); count];
        Ok(Self {
            overlay: ExternalRainOverlay::new(config),
            config,
            rows,
            cols,
            staging,
            row_flags: vec![0; usize::from(rows)],
            single_width_rows: vec![true; usize::from(rows)],
            cursor: None,
            hidden_cursor_rows: [0; MAX_EXTERNAL_HIDDEN_CURSOR_ROWS],
            hidden_cursor_rows_len: 0,
            display_offset: 0,
            is_alt_screen: false,
            clock_remainder_ms: 0.0,
            packed_quads: Vec::with_capacity(MAX_RAIN_QUADS * QUAD_WORDS),
            packed_halos: Vec::with_capacity(MAX_RAIN_ADD * HALO_WORDS),
            has_fresh_sync: false,
            output_valid: false,
        })
    }

    /// Resize and clear the persistent cell staging buffer.
    ///
    /// Existing capacity is reused; shrinking never shrinks allocation. The
    /// host must reacquire the typed-array view and fill all lanes before sync.
    pub fn resize_staging(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        // Even a rejected resize invalidates the old-dimension presentation.
        self.require_sync();
        validate_dimensions(rows, cols)?;
        self.rows = rows;
        self.cols = cols;
        let count = usize::from(rows) * usize::from(cols);
        self.staging.resize(
            count,
            ExternalRainCell::blank(self.config.theme_fg, self.config.default_bg),
        );
        self.staging.fill(ExternalRainCell::blank(
            self.config.theme_fg,
            self.config.default_bg,
        ));
        self.row_flags.resize(usize::from(rows), 0);
        self.row_flags.fill(0);
        self.single_width_rows.resize(usize::from(rows), true);
        self.single_width_rows.fill(true);
        Ok(())
    }

    /// Visible staging rows.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Visible staging columns.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Four u32 lanes per staging cell: `scalar,fg,bg,flags`.
    pub fn cell_words(&self) -> u32 {
        EXTERNAL_RAIN_CELL_WORDS as u32
    }

    /// Semantic default-background cell flag.
    pub fn cell_flag_default_background(&self) -> u32 {
        EXTERNAL_CELL_DEFAULT_BACKGROUND
    }

    /// Wide-glyph continuation cell flag.
    pub fn cell_flag_wide_continuation(&self) -> u32 {
        EXTERNAL_CELL_WIDE_CONTINUATION
    }

    /// Any-underline cell flag.
    pub fn cell_flag_underline(&self) -> u32 {
        EXTERNAL_CELL_UNDERLINE
    }

    /// Strikethrough cell flag.
    pub fn cell_flag_strikethrough(&self) -> u32 {
        EXTERNAL_CELL_STRIKETHROUGH
    }

    /// Overline cell flag.
    pub fn cell_flag_overline(&self) -> u32 {
        EXTERNAL_CELL_OVERLINE
    }

    /// Current host selection cell flag.
    pub fn cell_flag_selected(&self) -> u32 {
        EXTERNAL_CELL_SELECTED
    }

    /// Inline-image/host-visual cell flag.
    pub fn cell_flag_inline_image(&self) -> u32 {
        EXTERNAL_CELL_INLINE_IMAGE
    }

    /// Occupied but non-single-scalar staging value.
    pub fn opaque_scalar(&self) -> u32 {
        EXTERNAL_RAIN_OPAQUE_SCALAR
    }

    /// Byte offset of the writable staging buffer in wasm linear memory.
    ///
    /// Build `new Uint32Array(wasm.memory.buffer, ptr, staging_len_words())`.
    /// Never retain the view across a mutable wasm call.
    pub fn staging_ptr(&self) -> usize {
        self.staging.as_ptr() as usize
    }

    /// Active staging length in u32 words.
    pub fn staging_len_words(&self) -> usize {
        self.staging.len() * EXTERNAL_RAIN_CELL_WORDS
    }

    /// Byte offset of one u32 row flag per visible row.
    ///
    /// Zero means ordinary single-width geometry; any nonzero value protects
    /// the entire DEC double-width/double-height row from fixed-cell rain.
    pub fn row_flags_ptr(&self) -> usize {
        self.row_flags.as_ptr() as usize
    }

    /// Number of u32 row flags.
    pub fn row_flags_len(&self) -> usize {
        self.row_flags.len()
    }

    /// Set live cursor/scroll/alternate-screen state.
    ///
    /// Negative cursor coordinates mean DECTCEM-hidden/unknown cursor. A cursor
    /// row or reading-mode change unpublishes the old frame and requires
    /// `sync_snapshot` (the revision may stay unchanged); the core then updates
    /// sampling/gates without rebuilding occupancy.
    pub fn set_live_state(
        &mut self,
        cursor_row: i32,
        cursor_col: i32,
        display_offset: i32,
        is_alt_screen: bool,
    ) {
        let cursor = if cursor_row < 0 || cursor_col < 0 {
            None
        } else {
            u16::try_from(cursor_row)
                .ok()
                .zip(u16::try_from(cursor_col).ok())
        };
        let sampling_or_reading_changed = self.cursor.map(|(row, _)| row)
            != cursor.map(|(row, _)| row)
            || self.display_offset != display_offset
            || self.is_alt_screen != is_alt_screen;
        self.cursor = cursor;
        self.display_offset = display_offset;
        self.is_alt_screen = is_alt_screen;
        if sampling_or_reading_changed {
            self.require_sync();
        }
    }

    /// Copy at most five recently damaged composer rows for a hidden cursor.
    /// This is an event-time tiny typed-array copy, not a per-frame object list.
    pub fn set_hidden_cursor_rows(&mut self, rows: &[u16]) -> Result<(), String> {
        if rows.len() > MAX_EXTERNAL_HIDDEN_CURSOR_ROWS {
            self.require_sync();
            return Err(format!(
                "hidden cursor band has {} rows (max {})",
                rows.len(),
                MAX_EXTERNAL_HIDDEN_CURSOR_ROWS
            ));
        }
        if &self.hidden_cursor_rows[..self.hidden_cursor_rows_len] == rows {
            return Ok(());
        }
        self.hidden_cursor_rows[..rows.len()].copy_from_slice(rows);
        self.hidden_cursor_rows_len = rows.len();
        self.require_sync();
        Ok(())
    }

    /// Synchronize the authoritative cells currently in staging.
    ///
    /// Return codes: 0 unchanged, 1 literal material resampled, 2 occupancy
    /// rescanned, 3 deferred by disabled/reduced/visibility drain. `revision`
    /// must change for cell, selection, or row-attribute changes. Both clocks
    /// are u32 and may wrap; a wrap safely rebases the weather sequence.
    pub fn sync_snapshot(&mut self, revision: u32, content_seq: u32) -> Result<u8, String> {
        self.invalidate_output();
        self.has_fresh_sync = false;
        self.single_width_rows.clear();
        self.single_width_rows
            .extend(self.row_flags.iter().map(|&flags| flags == 0));
        let live = ExternalRainLiveState {
            cursor: self.cursor,
            hidden_cursor_rows: &self.hidden_cursor_rows[..self.hidden_cursor_rows_len],
            display_offset: self.display_offset,
            is_alt_screen: self.is_alt_screen,
        };
        let snapshot = ExternalRainSnapshot {
            rows: self.rows,
            cols: self.cols,
            revision: u64::from(revision),
            content_seq: u64::from(content_seq),
            default_bg: self.config.default_bg,
            theme_fg: self.config.theme_fg,
            cells: &self.staging,
            single_width_rows: &self.single_width_rows,
        };
        let result = self
            .overlay
            .sync_snapshot(snapshot, live)
            .map_err(|error| error.to_string())?;
        self.has_fresh_sync = result != ExternalRainSync::Deferred;
        Ok(match result {
            ExternalRainSync::Unchanged => SYNC_UNCHANGED,
            ExternalRainSync::Resampled => SYNC_RESAMPLED,
            ExternalRainSync::Rescanned => SYNC_RESCANNED,
            ExternalRainSync::Deferred => SYNC_DEFERRED,
        })
    }

    /// Advance the injected animation clock from a requestAnimationFrame delta.
    /// Fractional milliseconds accumulate; non-finite/non-positive input is ignored.
    pub fn advance_effects(&mut self, dt_ms: f64) {
        if !dt_ms.is_finite() || dt_ms <= 0.0 {
            return;
        }
        self.clock_remainder_ms = (self.clock_remainder_ms + dt_ms).min(u64::MAX as f64);
        let whole = self.clock_remainder_ms.floor();
        if whole < 1.0 {
            return;
        }
        let elapsed = whole as u64;
        self.clock_remainder_ms -= elapsed as f64;
        self.overlay.advance_ms(elapsed);
    }

    /// Emit one effects-only frame and repack resident typed-array output.
    pub fn emit(&mut self, cell_w: u16, cell_h: u16) -> Result<u64, String> {
        self.invalidate_output();
        if !self.has_fresh_sync {
            return Ok(0);
        }
        let live = ExternalRainLiveState {
            cursor: self.cursor,
            hidden_cursor_rows: &self.hidden_cursor_rows[..self.hidden_cursor_rows_len],
            display_offset: self.display_offset,
            is_alt_screen: self.is_alt_screen,
        };
        let fingerprint = self
            .overlay
            .emit(ExternalRainGeometry { cell_w, cell_h }, live)
            .map_err(|error| error.to_string())?;
        self.pack_output();
        self.output_valid = true;
        Ok(fingerprint)
    }

    /// Whether a shared host ticker should remain armed.
    pub fn is_active(&self) -> bool {
        self.has_fresh_sync && self.overlay.is_active()
    }

    /// Note one user keystroke.
    pub fn note_keystroke(&mut self) {
        self.overlay.note_keystroke();
    }

    /// Note a visual bell.
    pub fn note_bell(&mut self) {
        self.overlay.note_bell();
    }

    /// Note wheel/PgUp input in an alternate-screen TUI.
    pub fn note_alt_scroll(&mut self) {
        self.overlay.note_alt_scroll();
    }

    /// Note command completion; `failed` selects the bounded ember tint.
    pub fn note_exit_status(&mut self, failed: bool) {
        self.overlay.note_exit_status(failed);
    }

    /// Enable/disable the engine. Enabling requires a fresh sync before emit.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
        self.apply_config();
    }

    /// Set visibility: 0 focused, 1 visible-unfocused, 2 hidden.
    pub fn set_visibility(&mut self, state: u32) -> Result<(), String> {
        let visibility = match state {
            0 => RainVisibility::Focused,
            1 => RainVisibility::VisibleUnfocused,
            2 => RainVisibility::Hidden,
            _ => return Err(format!("invalid visibility state {state}")),
        };
        self.overlay.set_visibility(visibility);
        if visibility == RainVisibility::Hidden {
            self.require_sync();
        } else {
            self.invalidate_output();
        }
        Ok(())
    }

    /// Accessibility motion gate. Either transition requires a fresh sync.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.overlay.set_reduced_motion(reduced);
        self.require_sync();
    }

    /// Change theme colors (`0x00RRGGBB`). A fresh cell fill/sync is required.
    pub fn set_theme(&mut self, default_bg: u32, theme_fg: u32) {
        self.config.default_bg = default_bg & 0x00FF_FFFF;
        self.config.theme_fg = theme_fg & 0x00FF_FFFF;
        self.apply_config();
    }

    /// Configure tick/density/speed/trail/material mutation and idle sleep.
    pub fn set_rate(
        &mut self,
        fps: u32,
        density: u32,
        speed: u32,
        trail: u32,
        mutation_ms: u32,
        idle_secs: u32,
    ) {
        self.config.fps = fps.clamp(12, 60) as u8;
        self.config.density = density.clamp(1, 12) as u8;
        self.config.speed = speed.clamp(1, 10) as u8;
        self.config.trail = trail.clamp(1, 10) as u8;
        self.config.mutation_ms = mutation_ms.clamp(80, 2000) as u16;
        self.config.idle_secs = idle_secs.clamp(2, 120) as u16;
        self.apply_config();
    }

    /// Configure body/head alpha. Negative values select theme-derived alpha.
    pub fn set_alpha(&mut self, alpha: i32, head_alpha: i32) {
        self.config.alpha_override = (alpha >= 0).then_some(alpha.clamp(0, 255) as u8);
        self.config.head_alpha_override =
            (head_alpha >= 0).then_some(head_alpha.clamp(0, 255) as u8);
        self.apply_config();
    }

    /// Set hue: 0 matrix, 1 theme foreground, 2 custom `0x00RRGGBB`.
    pub fn set_hue(&mut self, mode: u32, custom: u32) -> Result<(), String> {
        self.config.hue = match mode {
            0 => RainHue::Matrix,
            1 => RainHue::Theme,
            2 => RainHue::Custom(custom & 0x00FF_FFFF),
            _ => return Err(format!("invalid hue mode {mode}")),
        };
        self.apply_config();
        Ok(())
    }

    /// Configure reading/turn/bell/literal-material behaviors.
    pub fn set_behavior(
        &mut self,
        suppress_in_alt_screen: bool,
        turn_wave: bool,
        bell_alert: bool,
        output_material: bool,
    ) {
        self.config.suppress_in_alt_screen = suppress_in_alt_screen;
        self.config.turn_wave = turn_wave;
        self.config.bell_alert = bell_alert;
        self.config.output_material = output_material;
        self.apply_config();
    }

    /// Replace the deterministic replay seed.
    pub fn set_seed(&mut self, seed_lo: u32, seed_hi: u32) {
        self.config.seed = u64::from(seed_lo) | (u64::from(seed_hi) << 32);
        self.apply_config();
    }

    /// Twelve u32 lanes per emitted glyph quad.
    pub fn quad_words(&self) -> u32 {
        QUAD_WORDS as u32
    }

    /// Packed glyph-quad word pointer. Read exactly [`Self::quads_len_words`].
    pub fn quads_ptr(&self) -> usize {
        self.packed_quads.as_ptr() as usize
    }

    /// Packed glyph-quad length in u32 words.
    pub fn quads_len_words(&self) -> usize {
        self.packed_quads.len()
    }

    /// Ten u32 lanes per emitted halo.
    pub fn halo_words(&self) -> u32 {
        HALO_WORDS as u32
    }

    /// Packed halo word pointer. Read exactly [`Self::halos_len_words`].
    pub fn halos_ptr(&self) -> usize {
        self.packed_halos.as_ptr() as usize
    }

    /// Packed halo length in u32 words.
    pub fn halos_len_words(&self) -> usize {
        self.packed_halos.len()
    }

    /// Straight-alpha RGBA8 atlas pointer, or zero with no visible frame.
    pub fn atlas_ptr(&self) -> usize {
        self.current_atlas()
            .map_or(0, |atlas| atlas.rgba.as_ptr() as usize)
    }

    /// Straight-alpha atlas byte length.
    pub fn atlas_len(&self) -> usize {
        self.current_atlas().map_or(0, |atlas| atlas.rgba.len())
    }

    /// Atlas width in texels.
    pub fn atlas_width(&self) -> u32 {
        self.current_atlas().map_or(0, |atlas| atlas.width)
    }

    /// Atlas height in texels.
    pub fn atlas_height(&self) -> u32 {
        self.current_atlas().map_or(0, |atlas| atlas.height)
    }

    /// Monotonic atlas generation; cache WebGL uploads against this value.
    pub fn atlas_version(&self) -> u64 {
        self.current_atlas().map_or(0, |atlas| atlas.version)
    }
}

impl AtermRainOverlay {
    fn apply_config(&mut self) {
        self.overlay.set_config(self.config);
        self.require_sync();
    }

    fn require_sync(&mut self) {
        self.has_fresh_sync = false;
        self.invalidate_output();
    }

    fn invalidate_output(&mut self) {
        self.packed_quads.clear();
        self.packed_halos.clear();
        self.output_valid = false;
    }

    fn pack_output(&mut self) {
        debug_assert!(self.overlay.quads().len() <= MAX_RAIN_QUADS);
        debug_assert!(self.overlay.halos().len() <= MAX_RAIN_ADD);
        for quad in self.overlay.quads().iter().take(MAX_RAIN_QUADS) {
            self.packed_quads.extend_from_slice(&[
                u32::from(quad.row),
                u32::from(quad.x),
                u32::from(quad.y),
                u32::from(quad.w),
                u32::from(quad.h),
                u32::from(quad.ax),
                u32::from(quad.ay),
                u32::from(quad.aw),
                u32::from(quad.ah),
                quad.tint,
                u32::from(quad.alpha),
                u32::from(quad.flip_x),
            ]);
        }
        for halo in self.overlay.halos().iter().take(MAX_RAIN_ADD) {
            self.packed_halos.extend_from_slice(&[
                u32::from(halo.row),
                u32::from(halo.x),
                u32::from(halo.y),
                u32::from(halo.w),
                u32::from(halo.h),
                halo.color,
                u32::from(halo.cx),
                u32::from(halo.cy),
                u32::from(halo.rx),
                u32::from(halo.ry),
            ]);
        }
    }

    fn current_atlas(&self) -> Option<&aterm_render::SceneAtlas> {
        if self.output_valid {
            self.overlay.atlas()
        } else {
            None
        }
    }
}

fn validate_dimensions(rows: u16, cols: u16) -> Result<(), String> {
    if rows == 0 || cols == 0 {
        return Err("rain staging viewport is empty".to_owned());
    }
    if rows > MAX_EXTERNAL_RAIN_ROWS || cols > MAX_EXTERNAL_RAIN_COLS {
        return Err(format!(
            "rain staging viewport {cols}x{rows} exceeds {}x{}",
            MAX_EXTERNAL_RAIN_COLS, MAX_EXTERNAL_RAIN_ROWS
        ));
    }
    let count = usize::from(rows) * usize::from(cols);
    if count > MAX_EXTERNAL_RAIN_CELLS {
        return Err(format!(
            "rain staging viewport has {count} cells (max {MAX_EXTERNAL_RAIN_CELLS})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    const BG: u32 = 0x0011_1318;
    const FG: u32 = 0x00D0_D0D0;

    fn overlay() -> AtermRainOverlay {
        let mut overlay = AtermRainOverlay::new(40, 60, BG, FG, 0x11D3, 0xA7E2).unwrap();
        // Literal-material mode is honestly empty on a blank screen. Seed one
        // real output glyph so emission/packing tests exercise visible rain.
        overlay.staging[0] = ExternalRainCell::glyph('A', FG, BG, true);
        overlay
    }

    fn wake(overlay: &mut AtermRainOverlay) {
        for _ in 0..12 {
            overlay.note_keystroke();
            overlay.advance_effects(83.0);
            overlay.emit(8, 16).unwrap();
        }
        assert!(overlay.quads_len_words() > 0);
    }

    #[test]
    fn staging_is_exactly_four_u32_lanes_and_raw_writes_sync() {
        assert_eq!(size_of::<ExternalRainCell>(), 4 * size_of::<u32>());
        assert_eq!(offset_of!(ExternalRainCell, scalar), 0);
        assert_eq!(offset_of!(ExternalRainCell, fg), 4);
        assert_eq!(offset_of!(ExternalRainCell, bg), 8);
        assert_eq!(offset_of!(ExternalRainCell, flags), 12);

        let mut overlay = overlay();
        assert_eq!(overlay.cell_words(), 4);
        let word_len = overlay.staging_len_words();
        // SAFETY: staging_ptr addresses `word_len` initialized u32 lanes in an
        // exclusively borrowed Vec<repr(C) ExternalRainCell>. No method runs or
        // reallocates the vector while this native stand-in for a JS view lives.
        let words =
            unsafe { std::slice::from_raw_parts_mut(overlay.staging_ptr() as *mut u32, word_len) };
        let index = (2 * usize::from(overlay.cols()) + 7) * EXTERNAL_RAIN_CELL_WORDS;
        words[index] = u32::from('R');
        words[index + 1] = FG;
        words[index + 2] = BG;
        words[index + 3] = EXTERNAL_CELL_DEFAULT_BACKGROUND;
        assert_eq!(overlay.staging[2 * 60 + 7].scalar, u32::from('R'));

        overlay.set_live_state(30, 0, 0, false);
        assert_eq!(overlay.sync_snapshot(1, 1).unwrap(), SYNC_RESCANNED);
        assert_eq!(overlay.quads_len_words(), 0);
        wake(&mut overlay);
        assert!(overlay.atlas_ptr() != 0 && overlay.atlas_len() > 0);
    }

    #[test]
    fn packed_quad_halo_and_atlas_views_match_core_without_reallocation() {
        let mut overlay = overlay();
        overlay.set_live_state(30, 0, 0, false);
        overlay.sync_snapshot(1, 1).unwrap();
        let quad_ptr = overlay.quads_ptr();
        let halo_ptr = overlay.halos_ptr();
        wake(&mut overlay);
        assert_eq!(quad_ptr, overlay.quads_ptr());
        assert_eq!(halo_ptr, overlay.halos_ptr());
        assert_eq!(
            overlay.quads_len_words(),
            overlay.overlay.quads().len() * QUAD_WORDS
        );
        assert_eq!(
            overlay.halos_len_words(),
            overlay.overlay.halos().len() * HALO_WORDS
        );
        for (packed, quad) in overlay
            .packed_quads
            .as_chunks::<QUAD_WORDS>().0.iter().zip(overlay.overlay.quads())
        {
            assert_eq!(
                *packed,
                [
                    u32::from(quad.row),
                    u32::from(quad.x),
                    u32::from(quad.y),
                    u32::from(quad.w),
                    u32::from(quad.h),
                    u32::from(quad.ax),
                    u32::from(quad.ay),
                    u32::from(quad.aw),
                    u32::from(quad.ah),
                    quad.tint,
                    u32::from(quad.alpha),
                    u32::from(quad.flip_x),
                ]
            );
        }
        for (packed, halo) in overlay
            .packed_halos
            .as_chunks::<HALO_WORDS>().0.iter().zip(overlay.overlay.halos())
        {
            assert_eq!(
                *packed,
                [
                    u32::from(halo.row),
                    u32::from(halo.x),
                    u32::from(halo.y),
                    u32::from(halo.w),
                    u32::from(halo.h),
                    halo.color,
                    u32::from(halo.cx),
                    u32::from(halo.cy),
                    u32::from(halo.rx),
                    u32::from(halo.ry),
                ]
            );
        }
        let atlas = overlay.overlay.atlas().unwrap();
        assert_eq!(overlay.atlas_ptr(), atlas.rgba.as_ptr() as usize);
        assert_eq!(overlay.atlas_len(), atlas.rgba.len());
        assert_eq!(overlay.atlas_version(), atlas.version);

        overlay.sync_snapshot(1, 1).unwrap();
        assert_eq!(overlay.quads_len_words(), 0);
        assert_eq!(overlay.halos_len_words(), 0);
        assert_eq!(overlay.atlas_ptr(), 0);
    }

    #[test]
    fn selection_flag_and_hide_refocus_fail_closed() {
        let mut overlay = overlay();
        for cell in &mut overlay.staging {
            cell.flags |= EXTERNAL_CELL_SELECTED;
        }
        overlay.set_live_state(30, 0, 0, false);
        overlay.sync_snapshot(1, 1).unwrap();
        for _ in 0..12 {
            overlay.note_keystroke();
            overlay.advance_effects(83.0);
            assert_eq!(overlay.emit(8, 16).unwrap(), 0);
        }
        assert_eq!(overlay.quads_len_words(), 0);

        overlay.set_visibility(2).unwrap();
        overlay.set_visibility(0).unwrap();
        assert_eq!(overlay.emit(8, 16).unwrap(), 0);
        assert_eq!(overlay.atlas_ptr(), 0);
        assert!(!overlay.is_active());
    }

    #[test]
    fn live_resize_hidden_band_and_row_geometry_fail_closed() {
        let mut active = overlay();
        active.set_live_state(30, 0, 0, false);
        active.sync_snapshot(1, 1).unwrap();
        wake(&mut active);

        active.set_live_state(31, 0, 0, false);
        assert_eq!(active.quads_len_words(), 0);
        assert_eq!(active.atlas_ptr(), 0);
        assert_eq!(active.emit(8, 16).unwrap(), 0, "live change requires sync");
        active.sync_snapshot(1, 1).unwrap();
        wake(&mut active);

        let oversized_band = [0, 1, 2, 3, 4, 5];
        assert!(active.set_hidden_cursor_rows(&oversized_band).is_err());
        assert_eq!(active.quads_len_words(), 0);
        assert_eq!(active.atlas_ptr(), 0);
        assert_eq!(active.emit(8, 16).unwrap(), 0);

        assert!(active.resize_staging(4096, 4096).is_err());
        assert_eq!(active.quads_len_words(), 0);
        assert_eq!(active.atlas_ptr(), 0);
        assert_eq!(active.emit(8, 16).unwrap(), 0);

        let mut rows_protected = overlay();
        rows_protected.row_flags.fill(1);
        rows_protected.set_live_state(30, 0, 0, false);
        rows_protected.sync_snapshot(1, 1).unwrap();
        for _ in 0..12 {
            rows_protected.note_keystroke();
            rows_protected.advance_effects(83.0);
            assert_eq!(rows_protected.emit(8, 16).unwrap(), 0);
        }
        assert_eq!(rows_protected.quads_len_words(), 0);
    }

    #[test]
    fn staging_limits_and_pointers_reuse_capacity() {
        assert!(AtermRainOverlay::new(0, 60, BG, FG, 0, 0).is_err());
        assert!(AtermRainOverlay::new(4096, 4096, BG, FG, 0, 0).is_err());
        let mut overlay = AtermRainOverlay::new(20, 40, BG, FG, 0, 0).unwrap();
        let ptr = overlay.staging_ptr();
        overlay.resize_staging(10, 40).unwrap();
        assert_eq!(ptr, overlay.staging_ptr());
        overlay.resize_staging(20, 40).unwrap();
        assert_eq!(ptr, overlay.staging_ptr());
    }
}
