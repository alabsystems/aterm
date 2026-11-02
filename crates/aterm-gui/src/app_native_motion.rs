// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host side of native-page motion (`docs/DESIGN-init-on-by-default-2026-10-25.md` §5.1,
//! §5.6): the motion clock and its dev seam, the Low Power sample, the per-window driver that
//! wakes the loop for the next frame of a moving node ([`crate::native_ui::NodeMotion`]) a seen
//! page shows, and the paint-only frame that draws it.
//!
//! A page declares a moving node and its painter draws it at the clock's `t_ms`; the host reads
//! the next frame off the retained tree (`CompiledUi::motion`), never a `view()`, and folds it
//! into the loop's wake under [`crate::metrics::DeadlineOwner::NativeMotion`]. Frames run only
//! for a page seen in a focused (or recorded) window with no overlay over it, and only while the
//! hands are off the keys app-wide (`hands_off_keys`): a key held them, and the wake folds in
//! again when the typing gap closes. A settled page (no moving node) owes no wake at all.
//!
//! A motion frame is paint-only ([`MotionTray::paint`]): the raster keeps the prims its last
//! paint drew and where each moving node's fall, repaints the nodes whose frame changed, splices
//! their prims in, rasters their rects alone over the kept prims, and patches those rows of the
//! window's card in place ([`copy_card_regions`]), naming them to the GPU arm
//! ([`aterm_gpu::TrayDelta`]) so it uploads them alone. No `view()`, compile, card clone or
//! accessibility staging runs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::chrome_band::ChromePalette;
use crate::native_app::DamageRegion;
use crate::native_appearance::AppearancePreferences;
use crate::native_ui::{CompiledUi, LogicalRect, MotionSpan};
use crate::platform::AppRt;
use crate::settings::Roles;
use crate::tray_raster::RasterRect;
use crate::widget::DrawPrim;
use crate::{App, WindowId};

/// The dev seam that pins the motion clock (debug and dev-seam builds): every moving node, and
/// every view's `animation_phase_ms`, then shows this millisecond, and nothing ticks.
pub(crate) const MOTION_AT_SEAM: &str = "ATERM_DEV_MOTION_AT_MS";

/// Fallback sampling interval while motion runs. macOS power and thermal notifications force
/// a sample even when a quieted page has no motion wake left to resume it.
pub(crate) const LOW_POWER_RESAMPLE: Duration = Duration::from_secs(5);

/// Whether an injected input event is typing, as a key pressed at the keyboard is
/// (`WindowEvent::KeyboardInput` stamps `last_keystroke_at`): a key chord is; text and raw bytes
/// an agent writes (`ctl type`, `ctl send`) and the mouse are not, so a driven key holds native
/// motion for the typing gap as a person's key does (§5.6).
pub(crate) const fn input_is_typing(event: &crate::input::InputEvent) -> bool {
    matches!(event, crate::input::InputEvent::Key { .. })
}

/// The pinned motion clock [`MOTION_AT_SEAM`] names, where it names a whole millisecond.
pub(crate) fn motion_pin_from_seam() -> Option<u64> {
    let raw = aterm_types::dev_seam!(MOTION_AT_SEAM)?;
    let pin = raw.to_str()?.trim().parse().ok();
    if pin.is_none() {
        aterm_log::warn!("{MOTION_AT_SEAM} names no millisecond; the motion clock runs");
    }
    pin
}

/// The serial of a window card's pixels ([`crate::SettingsCard::serial`]): a fresh card, or a
/// card a motion frame patched, takes the next one, so one serial names one set of pixels.
pub(crate) fn next_card_serial() -> u64 {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    SERIAL
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
        .max(1)
}

/// The logical rect a node at `rect` covers, rounded out to whole points.
fn damage_of(rect: LogicalRect) -> Option<DamageRegion> {
    let x = rect.x.floor().max(0.0);
    let y = rect.y.floor().max(0.0);
    let right = rect.right().ceil().max(0.0);
    let bottom = rect.bottom().ceil().max(0.0);
    (right > x && bottom > y).then_some(DamageRegion::Rect {
        x: x as u32,
        y: y as u32,
        width: (right - x) as u32,
        height: (bottom - y) as u32,
    })
}

/// The device rect of a `width` × `height` raster at `scale` a node at `rect` paints.
fn device_rect_of(
    rect: LogicalRect,
    scale: f32,
    (width, height): (u32, u32),
) -> Option<RasterRect> {
    match damage_of(rect)? {
        DamageRegion::Rect {
            x,
            y,
            width: w,
            height: h,
        } => RasterRect::from_logical(x, y, w, h, scale, width, height),
        DamageRegion::All => None,
    }
}

/// What the moving nodes of `compiled` repaint between `painted_ms` and `t_ms`: the union of
/// the rects of those whose frame changed, `None` where none did.
pub(crate) fn due_motion_damage(
    compiled: &CompiledUi,
    painted_ms: u64,
    t_ms: u64,
) -> Option<DamageRegion> {
    compiled
        .motion
        .iter()
        .filter_map(|&index| compiled.paint.get(index))
        .filter(|node| {
            node.motion()
                .is_some_and(|motion| motion.frame_at(painted_ms) != motion.frame_at(t_ms))
        })
        .filter_map(|node| damage_of(node.motion_rect()?))
        .reduce(crate::app_render::union_native_render_damage)
}

/// When the next frame of a moving node of `compiled` shows, after the frames painted at
/// `painted_ms`: `None` where nothing moves on.
pub(crate) fn next_motion_ms(compiled: &CompiledUi, painted_ms: u64) -> Option<u64> {
    compiled
        .motion
        .iter()
        .filter_map(|&index| {
            compiled
                .paint
                .get(index)?
                .motion()?
                .next_frame_after(painted_ms)
        })
        .min()
}

/// A native raster's tray, kept while its page shows a moving node (§5.1): the prims its last
/// paint drew, and where each moving node's fall among them, so a motion frame repaints those
/// nodes, splices their prims in and rasters their rects alone, with no `view()` or compile.
#[derive(Debug)]
pub(crate) struct MotionTray {
    prims: Vec<DrawPrim>,
    spans: Vec<MotionSpan>,
    /// The roles the last frame painted with and the palette and preferences they came from,
    /// so a frame whose palette is unchanged reuses them.
    roles: Option<(ChromePalette, AppearancePreferences, Roles)>,
}

impl MotionTray {
    /// The tray `prims` a paint drew, with its moving nodes' `spans`
    /// (`CompiledUi::tray_with_motion`).
    pub(crate) const fn new(prims: Vec<DrawPrim>, spans: Vec<MotionSpan>) -> Self {
        Self {
            prims,
            spans,
            roles: None,
        }
    }

    /// The roles `chrome` paints with under the person's appearance now: the last frame's
    /// where neither changed.
    fn roles_for(&mut self, chrome: ChromePalette) -> Roles {
        let preferences = crate::native_appearance::current_preferences();
        match self.roles {
            Some((palette, held, roles)) if palette == chrome && held == preferences => roles,
            _ => {
                let roles = crate::native_appearance::roles(chrome, preferences);
                self.roles = Some((chrome, preferences, roles));
                roles
            }
        }
    }

    /// One paint-only motion frame: repaint each moving node of `compiled` whose frame changed
    /// between `painted_ms` and `t_ms`, splice its prims in place of its old ones, and raster
    /// its rect over the kept prims into `rgba` (`size` device pixels at `scale`). The device
    /// rects it patched, in paint order (none where no frame changed).
    pub(crate) fn paint(
        &mut self,
        compiled: &CompiledUi,
        (painted_ms, t_ms): (u64, u64),
        (chrome, theme): (ChromePalette, aterm_render::Theme),
        scale: f32,
        rgba: &mut [u8],
        size: (u32, u32),
    ) -> Vec<RasterRect> {
        let mut regions = Vec::new();
        let (mut added, mut removed) = (0usize, 0usize);
        let mut roles = None;
        for index in 0..self.spans.len() {
            let (node_index, held) = {
                let span = &mut self.spans[index];
                span.prims = span.prims.start - removed + added..span.prims.end - removed + added;
                (span.node, span.prims.clone())
            };
            let Some(node) = compiled.paint.get(node_index) else {
                continue;
            };
            let Some(motion) = node.motion() else {
                continue;
            };
            if motion.frame_at(painted_ms) == motion.frame_at(t_ms) {
                continue;
            }
            let roles = *roles.get_or_insert_with(|| self.roles_for(chrome));
            let fresh = compiled.paint_motion_node(node_index, roles, theme, 13.0, t_ms);
            let grown = fresh.len();
            self.prims.splice(held.clone(), fresh);
            self.spans[index].prims = held.start..held.start + grown;
            removed += held.len();
            added += grown;
            if let Some(region) = node
                .motion_rect()
                .and_then(|rect| device_rect_of(rect, scale, size))
            {
                regions.push(region);
            }
        }
        regions.retain(|&region| {
            let patch =
                crate::tray_raster::rasterize_tray_region(&self.prims, scale, [0, 0, 0, 0], region);
            crate::tray_raster::apply_raster_patch(rgba, size.0, size.1, region, &patch)
        });
        regions
    }
}

impl crate::NativeLeafRaster {
    /// Paint one motion frame of this raster at `t_ms` ([`MotionTray::paint`]): the device rects
    /// it patched, none where it keeps no tray or no frame changed. The raster then shows `t_ms`.
    pub(crate) fn paint_motion_frame(
        &mut self,
        t_ms: u64,
        palette: (ChromePalette, aterm_render::Theme),
        scale: f32,
    ) -> Vec<RasterRect> {
        let painted_ms = self.motion_ms;
        let size = (self.width, self.height);
        let regions = match self.motion_tray.as_mut() {
            Some(tray) => tray.paint(
                &self.compiled,
                (painted_ms, t_ms),
                palette,
                scale,
                &mut self.rgba,
                size,
            ),
            None => Vec::new(),
        };
        self.motion_ms = self.motion_ms.max(t_ms);
        if let Some(rect) = regions.iter().copied().reduce(union_rect) {
            self.raster_fingerprint_once.take();
            self.last_work = crate::NativeRasterWork::Motion {
                rect,
                pixels: regions.iter().map(|region| region.pixels()).sum(),
            };
        }
        regions
    }
}

/// The smallest rect holding both.
fn union_rect(a: RasterRect, b: RasterRect) -> RasterRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    RasterRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

/// The rows `regions` span, as a half-open band.
pub(crate) fn rows_of(regions: &[RasterRect]) -> Option<(u32, u32)> {
    let rect = regions.iter().copied().reduce(union_rect)?;
    Some((rect.y, rect.y + rect.height))
}

/// Copy `regions` of `src` into `dst`, two rasters of one size `width` pixels wide: the window
/// card a motion frame patches in place of a fresh copy of its raster. False (nothing copied)
/// where a region falls outside either.
pub(crate) fn copy_card_regions(
    dst: &mut [u8],
    src: &[u8],
    width: u32,
    regions: &[RasterRect],
) -> bool {
    let stride = width as usize * 4;
    if dst.len() != src.len()
        || regions.iter().any(|region| {
            region.x + region.width > width
                || (region.y + region.height) as usize * stride > src.len()
        })
    {
        return false;
    }
    for region in regions {
        let (from, to) = (
            region.x as usize * 4,
            (region.x + region.width) as usize * 4,
        );
        for row in region.y as usize..(region.y + region.height) as usize {
            let at = row * stride;
            dst[at + from..at + to].copy_from_slice(&src[at + from..at + to]);
        }
    }
    true
}

impl App {
    /// The motion clock: milliseconds since the app's epoch, or the pinned millisecond
    /// ([`MOTION_AT_SEAM`]). A view reads it as `animation_phase_ms` and a moving node is
    /// painted at it.
    pub(crate) fn native_motion_ms(&self) -> u64 {
        self.motion_pin_ms
            .unwrap_or_else(|| self.native_preview_phase_ms())
    }

    /// Pin the motion clock at `pin` (or let it run again with `None`), as the seam does from
    /// launch: every native page repaints at once at the pinned time.
    #[cfg(test)]
    pub(crate) fn pin_native_motion(&mut self, pin: Option<u64>) {
        self.motion_pin_ms = pin;
        let windows = self.windows.keys().copied().collect::<Vec<_>>();
        for wid in windows {
            self.invalidate_native_ui_cache(wid);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.native_motion_fired_ms = None;
                ws.next_native_motion_tick = None;
            }
        }
        self.request_redraw_all_windows();
    }

    /// Read Low Power Mode and the thermal state again, at most every [`LOW_POWER_RESAMPLE`]
    /// unless `force`: true where it changed, after asking every window to redraw, since each
    /// native page's compile stamp moves with it (`ViewMotionCx::low_power`).
    pub(crate) fn resample_low_power(&mut self, now: Instant, force: bool) -> bool {
        if !force
            && self
                .low_power_sampled_at
                .is_some_and(|at| now.saturating_duration_since(at) < LOW_POWER_RESAMPLE)
        {
            return false;
        }
        self.low_power_sampled_at = Some(now);
        let low_power = self.apprt.low_power();
        if low_power == self.low_power {
            return false;
        }
        self.low_power = low_power;
        self.request_redraw_all_windows();
        true
    }

    /// Whether window `wid` may run motion frames now: seen (focused with glass, or recorded),
    /// no overlay over its page, not occluded.
    fn native_motion_may_run(&self, wid: WindowId) -> bool {
        let recorded = self.video_rec.as_ref().is_some_and(|rec| rec.window == wid);
        self.windows.get(&wid).is_some_and(|ws| {
            !ws.occluded
                && crate::native_preview_may_tick(
                    crate::has_os_window(ws),
                    ws.focused,
                    recorded,
                    ws.overlay.is_some(),
                )
        })
    }

    /// When window `wid` owes its next motion frame (§5.6): the earliest next frame over the
    /// moving nodes of the native leaves it shows, read from their retained trees, never later
    /// than the frame after the last tick that fired (so a frame that could not paint never
    /// spins the loop), and held past the typing gap while the hands are on the keys. `None`
    /// where nothing moves, the clock is pinned, or the window may not run motion.
    pub(crate) fn next_native_motion_tick(&self, wid: WindowId) -> Option<Instant> {
        if self.motion_pin_ms.is_some() {
            return None;
        }
        let ws = self.windows.get(&wid)?;
        // The cheap test first: almost every window holds no moving node.
        if !ws.leaf_render_cache.values().any(|cache| {
            cache
                .native
                .as_ref()
                .is_some_and(|raster| !raster.compiled.motion.is_empty())
        }) {
            return None;
        }
        if !self.native_motion_may_run(wid) {
            return None;
        }
        let plan = self.active_visible_leaf_plan(wid)?;
        let next_ms = plan
            .leaves
            .iter()
            .filter_map(|leaf| ws.leaf_render_cache.get(&leaf.view)?.native.as_ref())
            .filter_map(|raster| {
                let painted = raster.motion_ms.max(ws.native_motion_fired_ms.unwrap_or(0));
                next_motion_ms(&raster.compiled, painted)
            })
            .min()?;
        let at = self.lat_epoch + Duration::from_millis(next_ms);
        // Held while a key was typed in a focused window: the wake comes back as the gap closes.
        Some(match self.last_keystroke_at {
            Some(typed) if self.any_os_window_focused() => {
                at.max(typed + crate::AUTOMATIC_UPDATE_KEYSTROKE_GAP)
            }
            _ => at,
        })
    }

    /// Serve window `wid`'s due motion tick at `now`: record the tick, so the next frame paints
    /// the moving nodes whose frame changed (and only them) at the clock's time, paint-only.
    /// True where a frame is owed, so the caller redraws the window. A tick that meets keys
    /// typed in the gap, or a window that may no longer run motion, owes nothing.
    pub(crate) fn native_motion_due(&mut self, wid: WindowId, now: Instant) -> bool {
        if self.motion_pin_ms.is_some() || !self.update_apply_hands_off_keys(now) {
            return false;
        }
        if self.resample_low_power(now, false) {
            // The pages recompile without their moving nodes; that redraw is already asked.
            return false;
        }
        if !self.native_motion_may_run(wid) {
            return false;
        }
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return false;
        };
        let t_ms = self.native_motion_ms();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        ws.native_motion_fired_ms = Some(t_ms);
        plan.leaves.iter().any(|leaf| {
            ws.leaf_render_cache
                .get(&leaf.view)
                .and_then(|cache| cache.native.as_ref())
                .is_some_and(|raster| {
                    due_motion_damage(&raster.compiled, raster.motion_ms, t_ms).is_some()
                })
        })
    }

    /// The motion-clock time window `wid`'s frame paints a moving node at, paint-only, where
    /// a motion tick fired since the raster's `painted_ms` and the frame may still run: the
    /// window seen and the hands off the keys, so a motion frame never rides an input frame.
    pub(crate) fn native_motion_frame_ms(&self, wid: WindowId, painted_ms: u64) -> Option<u64> {
        if self.motion_pin_ms.is_some() {
            return None;
        }
        let fired = self.windows.get(&wid)?.native_motion_fired_ms?;
        (fired > painted_ms
            && self.update_apply_hands_off_keys(Instant::now())
            && self.native_motion_may_run(wid))
        .then(|| self.native_motion_ms().max(fired))
    }
}

#[cfg(test)]
#[path = "app_native_motion_tests.rs"]
mod tests;
