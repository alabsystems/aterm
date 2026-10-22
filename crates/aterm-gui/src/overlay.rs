// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The single modal-overlay slot for the in-grid surfaces: the command Palette and
//! the Overview share one card slot, one
//! rasterize/composite call and one `RepaintKey::settings_fp` term. One [`Overlay`] enum
//! makes their mutual exclusion structural. Settings is not one of them: it is a native tab.
//!
//! Each variant's model fans out to the SAME paint/lifecycle observers through the
//! [`OverlayModel`] trait: pixels ([`OverlayModel::tray`]), the repaint fingerprint
//! ([`OverlayModel::fingerprint`]), and — on Linux, where the AT-SPI publisher
//! runs — the accessibility tree ([`OverlayModel::a11y`]). Concrete compatibility
//! introspection calls each surviving overlay model's inherent serializer; native
//! Settings inspection compiles the native semantic tree instead.

use crate::chrome_band::ChromePalette;

use crate::app_introspect::AuxTarget;
use crate::overview::OverviewState;
use crate::palette::PaletteState;
use crate::settings::SettingsGeom;
use crate::widget::TrayInput;

/// The paint/lifecycle contract shared by every overlay surface. `tray` and `fingerprint`
/// read the same `&self`, so pixels and the repaint key cannot diverge.
pub(crate) trait OverlayModel {
    /// The surface's own repaint fingerprint; [`Overlay::fingerprint`] folds in the variant.
    fn fingerprint(&self) -> u64;

    /// The overlay height in rows the card wants, clamped to the rows the composed frame has
    /// (`avail`).
    fn wanted_rows(&self, avail: usize) -> usize;

    /// Paint the surface as a [`TrayInput`] card of [`crate::widget::DrawPrim`]s, composited
    /// through `composite_tray`. A floating card returns its own rect in the `TrayInput`,
    /// which tells the splice where.
    /// `palette` is the chrome palette the card paints from
    /// ([`crate::app_render::ChromeSurface::OverGrid`], or `NativePage` for a modal
    /// over a native page).
    fn tray(&self, geom: &SettingsGeom, palette: ChromePalette) -> TrayInput;

    /// The smoke layer the surface lays over the whole window behind its card, if any
    /// (the palette's on Windows, [`crate::palette::palette_scrim`]). The shared splice
    /// paints it under the card's prims across the frame. `None`: the card alone.
    fn scrim(&self) -> Option<crate::widget::Rgba> {
        None
    }

    /// The `controls front` truncation signal `(scroll, total, visible)`: rows scrolled past
    /// the top, the model's row count, and rows shown on the card. A non-scrolling surface
    /// returns `(0, total, visible)`.
    fn scroll_extent(&self) -> (usize, usize, usize);
    /// The cross-platform accessibility tree ([`aterm_a11y::model::TreeUpdate`]) for this surface,
    /// built from the SAME `&self` the pixels read — another observer
    /// (screen readers) that can never diverge from the glass. Gated on `a11y_tree`
    /// (Linux, where the tree is published); there it is REQUIRED for every variant, so a surface
    /// left out is a compile error (the same exhaustive-fan-out guarantee as the rest).
    #[cfg(a11y_tree)]
    fn a11y(&self) -> aterm_a11y::model::TreeUpdate;
}

impl OverlayModel for PaletteState {
    fn fingerprint(&self) -> u64 {
        PaletteState::fingerprint(self)
    }
    fn wanted_rows(&self, avail: usize) -> usize {
        // The card is content-sized but floats centred, so its geometry needs the whole
        // viewport. `palette_layout` retains the natural command-row height.
        avail
    }
    fn tray(&self, geom: &SettingsGeom, palette: ChromePalette) -> TrayInput {
        crate::palette::palette_tray(self, geom, palette)
    }
    /// None while the palette picks a theme (Choose Theme… and Choose Tab Theme…
    /// open scoped to it, and a highlighted Theme row anywhere previews on the
    /// terminal): the preview is the colours the window will draw, so it is
    /// shown undimmed. The scope and the highlight both feed the fingerprint.
    fn scrim(&self) -> Option<crate::widget::Rgba> {
        let previewing = self.is_themes_only() || self.highlighted_theme().is_some();
        crate::palette::palette_scrim().filter(|_| !previewing)
    }
    fn scroll_extent(&self) -> (usize, usize, usize) {
        PaletteState::scroll_extent(self)
    }
    #[cfg(a11y_tree)]
    fn a11y(&self) -> aterm_a11y::model::TreeUpdate {
        crate::palette::palette_a11y(self)
    }
}

impl OverlayModel for OverviewState {
    fn fingerprint(&self) -> u64 {
        OverviewState::fingerprint(self)
    }
    fn wanted_rows(&self, avail: usize) -> usize {
        // Floats centred like the palette: the tray spans the whole viewport
        // and `overview_layout` content-sizes the card within it.
        avail
    }
    fn tray(&self, geom: &SettingsGeom, palette: ChromePalette) -> TrayInput {
        crate::overview::overview_tray(self, geom, palette)
    }
    fn scroll_extent(&self) -> (usize, usize, usize) {
        OverviewState::scroll_extent(self)
    }
    #[cfg(a11y_tree)]
    fn a11y(&self) -> aterm_a11y::model::TreeUpdate {
        crate::overview::overview_a11y(self)
    }
}

/// Which surface a live [`Overlay`] holds: a cheap tag for the input gate and the
/// `settings_card` cache key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OverlayKind {
    Palette,
    /// The Overview (`docs/DESIGN-owner-surfaces-2026-10-16.md` §6): every
    /// session of this aterm, Owner-gated on the wire like the map it replaced.
    Overview,
}

impl OverlayKind {
    /// The `controls` target that reads this surface. Palette's is `Menu`.
    pub(crate) const fn target(self) -> AuxTarget {
        match self {
            OverlayKind::Palette => AuxTarget::Menu,
            OverlayKind::Overview => AuxTarget::Overview,
        }
    }

    /// The `controls front` `kind=` token: its target's keyword, so `kind=<x>` re-parses
    /// via `AuxTarget::parse` to the SAME surface, letting a driver pipe `controls front`
    /// -> `controls <kind>`.
    pub(crate) const fn keyword(self) -> &'static str {
        self.target().keyword()
    }

    /// The variant tag [`Overlay::fingerprint`] seals the inner hash with. Distinct per
    /// kind.
    fn tag(self) -> u64 {
        match self {
            OverlayKind::Palette => 3,
            OverlayKind::Overview => 7,
        }
    }
}

/// Fold a variant tag into a surface's inner fingerprint: rotated into the top byte so
/// two kinds with equal inner hashes still differ, and `| 1` so the result is never the
/// closed sentinel `0`.
fn seal(tag: u64, inner: u64) -> u64 {
    (tag.rotate_left(56) ^ inner) | 1
}

/// The single modal-overlay slot on a window: one variant is live at a time.
pub(crate) enum Overlay {
    Palette(PaletteState),
    /// The Overview (design §6) — every session grouped by window with its
    /// need, phase, task and driver words, then who manages whom; every read
    /// of it is Owner-gated on the wire.
    Overview(OverviewState),
}

impl Overlay {
    /// The repaint fingerprint folded into `RepaintKey::settings_fp`. Seeded per variant so
    /// two surfaces never collide in the `settings_card` cache (which would reuse the wrong
    /// surface's card); `| 1` keeps `0` for "closed".
    pub(crate) fn fingerprint(&self) -> u64 {
        seal(self.kind().tag(), self.model().fingerprint())
    }

    /// The active surface's model: the one point the splice and the fingerprint fan out from.
    pub(crate) fn model(&self) -> &dyn OverlayModel {
        match self {
            Overlay::Palette(p) => p,
            Overlay::Overview(m) => m,
        }
    }

    /// Which surface this slot holds.
    pub(crate) fn kind(&self) -> OverlayKind {
        match self {
            Overlay::Palette(_) => OverlayKind::Palette,
            Overlay::Overview(_) => OverlayKind::Overview,
        }
    }

    /// The `controls front` status line of the open surface: its kind, repaint fp and scroll
    /// extent. The caller emits the closed case as `overlay open=false`.
    pub(crate) fn status_line(&self) -> String {
        let (scroll, total, visible) = self.model().scroll_extent();
        format!(
            "overlay kind={} open=true fp={} scroll={} total={} visible={}",
            self.kind().keyword(),
            self.fingerprint(),
            scroll,
            total,
            visible,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `status_line()` (the `controls front` open case) reports `open=true`, the surface
    /// `kind=<keyword>`, the exact `Overlay::fingerprint()`, and a `scroll/total/visible`
    /// extent — for every shipping variant.
    #[test]
    fn status_line_reports_open_kind_fp_and_extent() {
        let cases = [
            Overlay::Palette(PaletteState::new()),
            Overlay::Overview(OverviewState::new(
                crate::WindowId(0),
                Vec::new(),
                Vec::new(),
            )),
        ];
        for o in &cases {
            let line = o.status_line();
            assert!(line.starts_with("overlay kind="), "shape: {line}");
            assert!(line.contains("open=true"), "open flag: {line}");
            assert!(
                line.contains(&format!("fp={}", o.fingerprint())),
                "fp binds the repaint key: {line}"
            );
            assert!(line.contains("scroll="), "extent scroll: {line}");
            assert!(line.contains("total="), "extent total: {line}");
            assert!(line.contains("visible="), "extent visible: {line}");
        }
    }
}
