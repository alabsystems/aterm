// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The subtle TRANSIENT NOTICE: a small floating [`DrawPrim`] pill near the top of the
//! window that appears, holds, then FADES away over a few seconds — used for two
//! non-disruptive update moments:
//!   * [`NoticeKind::UpdateReady`] — a strictly-newer build just STAGED. "Update ready"
//!     (accent-tinted); CLICKING it APPLIES the update in one gesture
//!     (`App::apply_update_or_details` — details-overlay fallback when nothing is
//!     actually staged). The persistent affordances (version-menu ⬆️ on macOS /
//!     tab-strip ↻ elsewhere) stay after it fades.
//!   * [`NoticeKind::LevelUp`] — the app just RELAUNCHED into a newer build (a re-exec
//!     handoff set `$ATERM_UPDATED_FROM`). A quiet, cursor-themed "leveled-up" flourish
//!     ("Updated to build N") that celebrates the swap, then fades — the "level up" analog
//!     the design asked for, without literally saying "level up" or blocking the flow.
//!
//! GLOBAL (App-level), painted into every window like `config_notice` — and it borrows the
//! SAME timed-lifecycle shape (`is_expired` + `deadline`), but renders through the SACRED
//! `settings_card`/`badge_card` composite path as pure [`DrawPrim`]s (native chrome, NOT
//! terminal grid cells). The whole pill's alpha is multiplied by [`TransientNotice::alpha`]
//! so the fade is a real ramp, re-rasterized as the quantized alpha changes.

use std::time::{Duration, Instant};

use aterm_render::Theme;

use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::row_baseline;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// How long the notice stays up before it is fully gone (hold + fade).
const TTL: Duration = Duration::from_millis(5200);
/// The fade-out tail: the last stretch of [`TTL`] over which alpha ramps 1→0.
const FADE: Duration = Duration::from_millis(1400);
/// Animation cadence during the fade tail (≈30 fps) — the deadline granularity.
const FRAME: Duration = Duration::from_millis(33);

/// Which update moment the notice marks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    /// A strictly-newer build staged and is ready to install. Clickable → APPLY (one
    /// click; see `App::notice_click`).
    UpdateReady { version: String, build: u64 },
    /// The app relaunched into a newer build (post-update celebration).
    LevelUp { build: u64 },
    /// Nonmodal updater status used by automatic/background paths. It is deliberately
    /// not clickable: details remain in Settings/the Version menu.
    UpdateStatus { text: String },
}

/// A single transient, self-expiring notice.
pub(crate) struct TransientNotice {
    kind: NoticeKind,
    spawned: Instant,
}

impl TransientNotice {
    pub(crate) fn update_ready(version: String, build: u64, now: Instant) -> Self {
        Self {
            kind: NoticeKind::UpdateReady { version, build },
            spawned: now,
        }
    }

    pub(crate) fn level_up(build: u64, now: Instant) -> Self {
        Self {
            kind: NoticeKind::LevelUp { build },
            spawned: now,
        }
    }

    pub(crate) fn update_status(text: impl Into<String>, now: Instant) -> Self {
        Self {
            kind: NoticeKind::UpdateStatus { text: text.into() },
            spawned: now,
        }
    }

    /// Whether this notice is `UpdateReady` (the clickable variant).
    pub(crate) fn is_update_ready(&self) -> bool {
        matches!(self.kind, NoticeKind::UpdateReady { .. })
    }

    /// Whether this is the post-update decorative flourish. Serious mode may
    /// discard this variant while preserving actionable/status update notices.
    pub(crate) fn is_level_up(&self) -> bool {
        matches!(self.kind, NoticeKind::LevelUp { .. })
    }

    /// Fully gone (past its whole lifetime) — the caller drops it.
    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.spawned) >= TTL
    }

    /// The next wake time: animate every [`FRAME`] once the fade tail begins, otherwise
    /// just wake at the fade start (a steady hold needs no intermediate repaints).
    pub(crate) fn deadline(&self, now: Instant) -> Instant {
        let elapsed = now.duration_since(self.spawned);
        let fade_start = TTL.saturating_sub(FADE);
        if elapsed < fade_start {
            self.spawned + fade_start
        } else {
            now + FRAME
        }
    }

    /// The whole-pill alpha at `now`: `1.0` through the hold, then a linear ramp to `0`
    /// across the fade tail.
    pub(crate) fn alpha(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.spawned).as_secs_f32();
        let ttl = TTL.as_secs_f32();
        let fade = FADE.as_secs_f32();
        let fade_start = ttl - fade;
        if elapsed <= fade_start {
            1.0
        } else {
            ((ttl - elapsed) / fade).clamp(0.0, 1.0)
        }
    }

    /// A repaint fingerprint folded into `RepaintKey::notice_fp`, quantized so the pill
    /// re-presents on each fade step but NOT every idle frame during the hold. `0` is the
    /// no-notice sentinel, so a live notice is forced non-zero.
    pub(crate) fn fingerprint(&self, now: Instant) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match &self.kind {
            NoticeKind::UpdateReady { version, build } => {
                0u8.hash(&mut h);
                version.hash(&mut h);
                build.hash(&mut h);
            }
            NoticeKind::LevelUp { build } => {
                1u8.hash(&mut h);
                build.hash(&mut h);
            }
            NoticeKind::UpdateStatus { text } => {
                2u8.hash(&mut h);
                text.hash(&mut h);
            }
        }
        // Quantize alpha to ~24 steps so the fade animates without churning the hold.
        ((self.alpha(now) * 24.0) as u64).hash(&mut h);
        h.finish() | 1
    }

    /// The pill caption.
    fn text(&self) -> String {
        match &self.kind {
            // A staged build can share the running build's display version (the
            // updater orders by build number — see `menu::staged_apply_label`), so
            // naming only the version would announce the version already running.
            NoticeKind::UpdateReady { version, build } => {
                if version == crate::build_info::version_display() {
                    format!("\u{2191} Update ready \u{2014} build {build}")
                } else {
                    format!("\u{2191} Update ready \u{2014} v{version}")
                }
            }
            NoticeKind::LevelUp { build } => {
                format!("\u{2726} Updated \u{2014} now on build {build}")
            }
            NoticeKind::UpdateStatus { text } => text.clone(),
        }
    }
}

/// The pill rect `(x, y, w, h)` in tray px — where [`notice_tray`] draws it and where a
/// click on an `UpdateReady` notice is tested.
pub(crate) fn notice_rect(n: &TransientNotice, g: &SettingsGeom) -> (f32, f32, f32, f32) {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let tray_w = g.cols as f32 * cw;
    let size = (px * 0.82).max(11.0);
    let tw = text_w(&n.text(), size);
    let pad_x = 0.9 * cw;
    let pill_w = (tw + 2.0 * pad_x).min(tray_w - 2.0 * cw);
    let pill_h = size + 0.7 * ch;
    // Top-centre, a little below the first row so it clears the toolbar/tab strip.
    let x = ((tray_w - pill_w) * 0.5).max(0.0);
    let y = 0.55 * ch;
    (x, y, pill_w, pill_h)
}

/// Build the notice pill as pure [`DrawPrim`]s, with the whole pill's alpha scaled by
/// `n.alpha(now)` (the fade). `UpdateReady` is accent-tinted; `LevelUp` is cursor-themed
/// (the flourish glyph + border take the live cursor colour).
pub(crate) fn notice_tray(
    n: &TransientNotice,
    g: &SettingsGeom,
    theme: Theme,
    cursor: [u8; 3],
    now: Instant,
) -> TrayInput {
    let r = Roles::from_theme(theme);
    let px = g.font_px;
    let (x, y, w, h) = notice_rect(n, g);
    let a = n.alpha(now);
    let sa = |base: u8| -> u8 { (f32::from(base) * a) as u8 };
    let radius = (h * 0.5).min(13.0);

    // Accent for UpdateReady; the live cursor colour for the LevelUp flourish.
    let accent = match &n.kind {
        NoticeKind::UpdateReady { .. } | NoticeKind::UpdateStatus { .. } => r.accent,
        NoticeKind::LevelUp { .. } => cursor,
    };

    let mut prims: Vec<DrawPrim> = Vec::new();
    // Soft drop shadow.
    prims.push(DrawPrim::Panel {
        x: x - 2.0,
        y: y + 1.5,
        w: w + 4.0,
        h: h + 4.0,
        radius: radius + 2.0,
        fill: rgba([0, 0, 0], sa(0x28)),
        blur: false,
    });
    // Body: opaque-ish elevated surface so the caption stays legible over content.
    prims.push(DrawPrim::Panel {
        x,
        y,
        w,
        h,
        radius,
        fill: rgba(r.elevated, sa(0xF2)),
        blur: false,
    });
    // Accent hairline (a touch of the accent/cursor colour as the pill's rim).
    prims.push(DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius,
        width: 1.5,
        color: rgba(accent, sa(0xC0)),
    });
    // Caption: the leading glyph takes the accent/cursor colour, the words stay primary.
    let cap = n.text();
    // Caption step (px*0.82 snaps to the 0.8 Caption factor), floored at 11px.
    let size = TypeStep::Caption.px_clamped(px, 11.0, f32::INFINITY);
    let tx = x + (w - text_w(&cap, size.get())) * 0.5;
    let base = row_baseline(y, h, size.get());
    // Split the leading marker glyph so it can be accent-coloured.
    let mut chars = cap.chars();
    let marker = chars.next().map(|c| c.to_string()).unwrap_or_default();
    let rest: String = chars.collect();
    prims.push(text_prim(
        tx,
        base,
        marker.clone(),
        size,
        TextWeight::Regular,
        TextFace::Mono,
        rgba(accent, sa(0xFF)),
    ));
    prims.push(text_prim(
        tx + text_w(&marker, size.get()),
        base,
        rest,
        size,
        TextWeight::Regular,
        TextFace::Mono,
        rgba(r.text_primary, sa(0xFF)),
    ));

    TrayInput {
        prims,
        card: (x, y, w, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> SettingsGeom {
        SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 120,
            panel_rows: 40,
        }
    }

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn alpha_holds_then_fades_to_zero() {
        let now = t0();
        let n = TransientNotice::update_ready("0.5.15".into(), 830, now);
        assert_eq!(n.alpha(now), 1.0, "full at spawn");
        // Just before the fade tail: still full.
        assert_eq!(n.alpha(now + (TTL - FADE) - Duration::from_millis(1)), 1.0);
        // Deep in the fade: below full, above zero.
        let mid = n.alpha(now + TTL - FADE / 2);
        assert!(mid > 0.0 && mid < 1.0, "mid-fade alpha {mid}");
        // At/after TTL: gone.
        assert_eq!(n.alpha(now + TTL), 0.0);
        assert!(n.is_expired(now + TTL));
        assert!(!n.is_expired(now));
    }

    #[test]
    fn fingerprint_zeroes_never_and_changes_over_fade() {
        let now = t0();
        let n = TransientNotice::level_up(830, now);
        assert_ne!(n.fingerprint(now), 0);
        // The hold is stable; the fade tail changes the fingerprint.
        let hold_a = n.fingerprint(now);
        let hold_b = n.fingerprint(now + Duration::from_millis(200));
        assert_eq!(hold_a, hold_b, "stable during hold");
        let fade_a = n.fingerprint(now + TTL - FADE + Duration::from_millis(100));
        let fade_b = n.fingerprint(now + TTL - FADE + Duration::from_millis(700));
        assert_ne!(fade_a, fade_b, "changes across the fade");
    }

    #[test]
    fn update_ready_is_clickable_level_up_is_not() {
        let now = t0();
        assert!(TransientNotice::update_ready("0.5.15".into(), 830, now).is_update_ready());
        assert!(!TransientNotice::level_up(830, now).is_update_ready());
        assert!(
            !TransientNotice::update_status("Update paused", now).is_update_ready(),
            "automatic status notices never trigger another apply attempt"
        );
    }

    #[test]
    fn tray_paints_caption_within_the_pill() {
        let now = t0();
        let n = TransientNotice::update_ready("0.5.15".into(), 830, now);
        let g = geom();
        let t = notice_tray(&n, &g, Theme::default(), [0, 255, 0], now);
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s.contains("Update ready")))
        );
        let (x, _, w, _) = notice_rect(&n, &g);
        let tray_w = g.cols as f32 * g.cw;
        assert!(x >= 0.0 && x + w <= tray_w, "pill fits within the tray");
    }
}
