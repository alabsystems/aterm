// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The single modal-overlay slot for the in-grid surfaces: the command Palette, the
//! identity picker and the Overview share one card slot, one
//! rasterize/composite call, and one `RepaintKey::settings_fp` term. Rather than parallel `Option` fields on
//! `WindowState` (whose mutual exclusion had to be enforced by hand, and whose on_key gate
//! ordering was a latent "hidden overlay swallows keys" bug), they collapse into one
//! [`Overlay`] enum. Mutual exclusion is now STRUCTURAL — one slot can only hold one
//! variant. Settings is not one of them: shipping Settings is a native tab.
//!
//! Each variant's model fans out to the SAME paint/lifecycle observers through the
//! [`OverlayModel`] trait: pixels ([`OverlayModel::tray`]), the repaint fingerprint
//! ([`OverlayModel::fingerprint`]), and — on Linux, where the AT-SPI publisher
//! runs — the accessibility tree ([`OverlayModel::a11y`]). Concrete compatibility
//! introspection calls each surviving overlay model's inherent serializer; native
//! Settings inspection compiles the native semantic tree instead.

use crate::chrome_band::ChromePalette;

use crate::overview::OverviewState;
use crate::palette::PaletteState;
use crate::session_picker::SessionPickerState;
use crate::settings::SettingsGeom;
use crate::widget::TrayInput;

/// The paint/lifecycle contract shared by every overlay surface. `tray` and
/// `fingerprint` read the SAME `&self`, so pixels and the repaint key cannot diverge.
pub(crate) trait OverlayModel {
    /// The INNER repaint fingerprint (never `0` while open). The [`Overlay`] wrapper folds
    /// in the variant discriminant + forces non-zero so two surfaces can never collide in
    /// the `settings_card` fp cache or the `RepaintKey`.
    fn fingerprint(&self) -> u64;

    /// The overlay height (rows) the card wants, CLAMPED to the rows the composed frame
    /// actually has (`avail`) — the single source the shared splice consults.
    fn wanted_rows(&self, avail: usize) -> usize;

    /// Paint the surface as a [`TrayInput`] card of pure [`crate::widget::DrawPrim`]s
    /// — the PIXELS, captured WYSIWYG through the SACRED `composite_tray` path.
    /// (Palette paints a frosted top band; a floating card returns its own rect
    /// in the `TrayInput`, which tells the splice where.)
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

    /// The `controls front` truncation signal: `(scroll, total, visible)` — rows scrolled
    /// past the top, the full model row count, and rows actually shown on the card. Read
    /// from the SAME `&self` as the tray. A non-scrolling surface returns
    /// `(0, total, visible)`.
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

impl OverlayModel for SessionPickerState {
    fn fingerprint(&self) -> u64 {
        SessionPickerState::fingerprint(self)
    }
    fn wanted_rows(&self, avail: usize) -> usize {
        // Floats centred like the palette: the tray spans the whole viewport
        // and `picker_layout` content-sizes the card within it.
        avail
    }
    fn tray(&self, geom: &SettingsGeom, palette: ChromePalette) -> TrayInput {
        crate::session_picker::picker_tray(self, geom, palette)
    }
    fn scroll_extent(&self) -> (usize, usize, usize) {
        SessionPickerState::scroll_extent(self)
    }
    #[cfg(a11y_tree)]
    fn a11y(&self) -> aterm_a11y::model::TreeUpdate {
        crate::session_picker::picker_a11y(self)
    }
}

/// Which surface a live [`Overlay`] holds — a cheap tag for the input gate + the
/// `settings_card` cache key (so two surfaces never share a cache line).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OverlayKind {
    Palette,
    /// The identity picker (File ▸ New Window / New Tab With Identity…).
    SessionPicker,
    /// The Overview (`docs/DESIGN-owner-surfaces-2026-10-16.md` §6): every
    /// session of this aterm, Owner-gated on the wire like the map it replaced.
    Overview,
}

impl OverlayKind {
    /// The `controls front` `kind=` token — chosen so `kind=<x>` re-parses via
    /// `AuxTarget::parse` to the SAME surface, letting a driver pipe `controls front`
    /// -> `controls <kind>`. Palette maps to `"menu"` (its `AuxTarget`).
    pub(crate) fn keyword(self) -> &'static str {
        match self {
            OverlayKind::Palette => "menu",
            OverlayKind::SessionPicker => "session-picker",
            OverlayKind::Overview => "overview",
        }
    }

    /// The variant tag [`Overlay::fingerprint`] seals the inner hash with. Distinct per
    /// kind.
    fn tag(self) -> u64 {
        match self {
            OverlayKind::Palette => 3,
            OverlayKind::SessionPicker => 6,
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

/// The single modal-overlay slot on a window. Exactly one variant is live at a time;
/// mutual exclusion is structural (a slot holds one value), replacing the old
/// hand-maintained "clear the other two" dance.
pub(crate) enum Overlay {
    Palette(PaletteState),
    /// The identity picker — palette-style filter over the identity roster;
    /// its selection opens a window or tab under the identity.
    SessionPicker(SessionPickerState),
    /// The Overview (design §6) — every session grouped by window with its
    /// need, phase, task and driver words, then who manages whom; every read
    /// of it is Owner-gated on the wire.
    Overview(OverviewState),
}

impl Overlay {
    /// The DISCRIMINANT-SEEDED, forced-nonzero repaint fingerprint folded into
    /// `RepaintKey::settings_fp`. Domain-separates per variant so two surfaces can NEVER
    /// collide in the `settings_card` fp cache (which would reuse the WRONG surface's card
    /// — a silent WYSIWYG corruption), and `| 1` keeps `0` reserved for "closed".
    pub(crate) fn fingerprint(&self) -> u64 {
        // The INNER hash comes from the same `OverlayModel` that paints the tray; the
        // wrapper folds in the variant tag + forces non-zero.
        seal(self.kind().tag(), self.model().fingerprint())
    }

    /// The active surface's model as a trait object — the single point the shared splice
    /// and repaint fingerprint fan out from.
    pub(crate) fn model(&self) -> &dyn OverlayModel {
        match self {
            Overlay::Palette(p) => p,
            Overlay::SessionPicker(p) => p,
            Overlay::Overview(m) => m,
        }
    }

    /// Which surface this slot holds.
    pub(crate) fn kind(&self) -> OverlayKind {
        match self {
            Overlay::Palette(_) => OverlayKind::Palette,
            Overlay::SessionPicker(_) => OverlayKind::SessionPicker,
            Overlay::Overview(_) => OverlayKind::Overview,
        }
    }

    /// The single `controls front` status line (design §5): which surface is open, its
    /// repaint fp, and the scroll-truncation extent — the cheap open/closed + "does the
    /// front `image` show the whole surface" signal. The closed case is emitted by the
    /// caller as `overlay open=false`.
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
    /// extent — for every shipping variant. The `kind` keyword must re-parse
    /// via `AuxTarget::parse` to the SAME surface, so a driver can pipe `controls front`
    /// into `controls <kind>`.
    #[test]
    fn status_line_reports_open_kind_fp_and_extent() {
        let cases = [
            Overlay::Palette(PaletteState::new()),
            Overlay::SessionPicker(SessionPickerState::new(
                crate::WindowId(0),
                crate::session_picker::PickerIntent::NewTabWithIdentity,
                Vec::new(),
            )),
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
            // The kind keyword must re-parse to a supported controls target (pipe-ability).
            let kw = o.kind().keyword();
            assert!(
                crate::app_introspect::AuxTarget::parse(kw).is_some(),
                "kind={kw} must re-parse via AuxTarget::parse"
            );
        }
    }
}
