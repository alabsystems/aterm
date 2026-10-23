// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The single modal-overlay slot for the in-grid surfaces.

use crate::chrome_band::ChromePalette;

use crate::app_introspect::AuxTarget;
use crate::overview::OverviewState;
use crate::palette::PaletteState;
use crate::settings::SettingsGeom;
use crate::widget::TrayInput;

/// The paint/lifecycle contract shared by every overlay surface.
pub(crate) trait OverlayModel {
    /// The surface's own repaint fingerprint; [`Overlay::fingerprint`] folds in the variant.
    fn fingerprint(&self) -> u64;

    /// The overlay height in rows the card wants, clamped to the rows the composed frame has.
    fn wanted_rows(&self, avail: usize) -> usize;

    /// Paint the surface as a [`TrayInput`] card composited through `composite_tray`.
    fn tray(&self, geom: &SettingsGeom, palette: ChromePalette) -> TrayInput;

    /// The smoke layer the surface lays over the whole window behind its card, if any.
    fn scrim(&self) -> Option<crate::widget::Rgba> {
        None
    }

    /// The `controls front` truncation signal `(scroll, total, visible)`.
    fn scroll_extent(&self) -> (usize, usize, usize);
    /// Cross-platform accessibility tree ([`aterm_a11y::model::TreeUpdate`]) for this surface.
    #[cfg(a11y_tree)]
    fn a11y(&self) -> aterm_a11y::model::TreeUpdate;
}

impl OverlayModel for PaletteState {
    fn fingerprint(&self) -> u64 {
        PaletteState::fingerprint(self)
    }
    fn wanted_rows(&self, avail: usize) -> usize {
        avail
    }
    fn tray(&self, geom: &SettingsGeom, palette: ChromePalette) -> TrayInput {
        crate::palette::palette_tray(self, geom, palette)
    }
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

/// Which surface a live [`Overlay`] holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OverlayKind {
    Palette,
    /// The Overview (`docs/DESIGN-owner-surfaces-2026-10-16.md` §6).
    Overview,
}

impl OverlayKind {
    /// The `controls` target that reads this surface.
    pub(crate) const fn target(self) -> AuxTarget {
        match self {
            OverlayKind::Palette => AuxTarget::Menu,
            OverlayKind::Overview => AuxTarget::Overview,
        }
    }

    /// The `controls front` `kind=` token: its target's keyword.
    pub(crate) const fn keyword(self) -> &'static str {
        self.target().keyword()
    }

    const fn tag(self) -> u64 {
        match self {
            OverlayKind::Palette => 3,
            OverlayKind::Overview => 7,
        }
    }
}

const fn seal(tag: u64, inner: u64) -> u64 {
    (tag.rotate_left(56) ^ inner) | 1
}

/// The single modal-overlay slot on a window: one variant is live at a time.
pub(crate) enum Overlay {
    Palette(PaletteState),
    /// The Overview (design §6).
    Overview(OverviewState),
}

impl Overlay {
    /// The repaint fingerprint folded into `RepaintKey::settings_fp`.
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
    pub(crate) const fn kind(&self) -> OverlayKind {
        match self {
            Overlay::Palette(_) => OverlayKind::Palette,
            Overlay::Overview(_) => OverlayKind::Overview,
        }
    }

    /// The `controls front` status line of the open surface.
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
