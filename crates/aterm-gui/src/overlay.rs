// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The single modal-overlay slot for transient and compatibility surfaces. About,
//! Update, and the command Palette share one card slot, one rasterize/composite call, and
//! one `RepaintKey::settings_fp` term. Rather than parallel `Option` fields on
//! `WindowState` (whose mutual exclusion had to be enforced by hand, and whose on_key gate
//! ordering was a latent "hidden overlay swallows keys" bug), they collapse into one
//! [`Overlay`] enum. Mutual exclusion is now STRUCTURAL — one slot can only hold one
//! variant. Settings exists here only under `cfg(test)`; shipping Settings is a native tab.
//!
//! Each variant's model fans out to the SAME paint/lifecycle observers through the
//! [`OverlayModel`] trait: pixels ([`OverlayModel::tray`]), the repaint fingerprint
//! ([`OverlayModel::fingerprint`]), and — under the `a11y-accesskit` feature — the
//! cross-platform accessibility tree ([`OverlayModel::a11y`]). Concrete compatibility
//! introspection calls each surviving overlay model's inherent serializer; native
//! Settings inspection compiles the native semantic tree instead.

use aterm_render::Theme;

use crate::about::AboutState;
use crate::palette::PaletteState;
use crate::settings::{PreviewCtx, SettingsGeom, SettingsState};
use crate::update_screen::UpdateState;
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
    /// (Settings/Palette paint a frosted top band; About paints an opaque floating
    /// dialog — the card rect in the returned `TrayInput` tells the splice where.)
    /// `ctx` carries App-tracked host facts the pure painters cannot know: Settings
    /// reads the OS appearance, About reads the display scale; Palette ignores it.
    fn tray(&self, geom: &SettingsGeom, theme: Theme, ctx: PreviewCtx) -> TrayInput;

    /// The `controls front` truncation signal: `(scroll, total, visible)` — rows scrolled
    /// past the top, the full model row count, and rows actually shown on the card. Read
    /// from the SAME `&self` as the tray. Non-scrolling surfaces (About/Update) return
    /// `(0, total, visible)`.
    fn scroll_extent(&self) -> (usize, usize, usize);
    /// The cross-platform accessibility tree ([`accesskit::TreeUpdate`]) for this surface,
    /// built from the SAME `&self` the pixels read — another observer
    /// (screen readers) that can never diverge from the glass. Feature-gated because the
    /// AccessKit dep is opt-in; when present it is REQUIRED for every variant, so a surface
    /// left out is a compile error (the same exhaustive-fan-out guarantee as the rest).
    #[cfg(feature = "a11y-accesskit")]
    fn a11y(&self) -> accesskit::TreeUpdate;
}

impl OverlayModel for SettingsState {
    fn fingerprint(&self) -> u64 {
        SettingsState::fingerprint(self)
    }
    fn wanted_rows(&self, avail: usize) -> usize {
        crate::settings::wanted_rows(&self.fields).min(avail)
    }
    fn tray(&self, geom: &SettingsGeom, theme: Theme, ctx: PreviewCtx) -> TrayInput {
        crate::settings::settings_tray(self, geom, theme, ctx)
    }
    fn scroll_extent(&self) -> (usize, usize, usize) {
        SettingsState::scroll_extent(self)
    }
    #[cfg(feature = "a11y-accesskit")]
    fn a11y(&self) -> accesskit::TreeUpdate {
        crate::accesskit_tree::settings_tree(self)
    }
}

impl OverlayModel for AboutState {
    fn fingerprint(&self) -> u64 {
        AboutState::fingerprint(self)
    }
    fn wanted_rows(&self, avail: usize) -> usize {
        // The About dialog floats CENTRED like a native window, so its tray spans the
        // whole frame (transparent outside the card); the card itself is content-sized
        // by `about_layout` and clamps to whatever height is actually available.
        avail
    }
    fn tray(&self, geom: &SettingsGeom, theme: Theme, ctx: PreviewCtx) -> TrayInput {
        crate::about::about_tray(self, geom, theme, ctx.scale)
    }
    fn scroll_extent(&self) -> (usize, usize, usize) {
        AboutState::scroll_extent(self)
    }
    #[cfg(feature = "a11y-accesskit")]
    fn a11y(&self) -> accesskit::TreeUpdate {
        crate::about::about_a11y(self)
    }
}

impl OverlayModel for UpdateState {
    fn fingerprint(&self) -> u64 {
        UpdateState::fingerprint(self)
    }
    fn wanted_rows(&self, avail: usize) -> usize {
        // Floats CENTRED like About, so its tray spans the frame (transparent outside the
        // card); the card is content-sized by `update_layout` and clamps to `avail`.
        avail
    }
    fn tray(&self, geom: &SettingsGeom, theme: Theme, _ctx: PreviewCtx) -> TrayInput {
        crate::update_screen::update_tray(self, geom, theme)
    }
    fn scroll_extent(&self) -> (usize, usize, usize) {
        UpdateState::scroll_extent(self)
    }
    #[cfg(feature = "a11y-accesskit")]
    fn a11y(&self) -> accesskit::TreeUpdate {
        crate::update_screen::update_a11y(self)
    }
}

impl OverlayModel for PaletteState {
    fn fingerprint(&self) -> u64 {
        PaletteState::fingerprint(self)
    }
    fn wanted_rows(&self, avail: usize) -> usize {
        // The card is content-sized but floats centred like About, so its geometry needs
        // the whole viewport. `palette_layout` retains the natural command-row height.
        avail
    }
    fn tray(&self, geom: &SettingsGeom, theme: Theme, _ctx: PreviewCtx) -> TrayInput {
        crate::palette::palette_tray(self, geom, theme)
    }
    fn scroll_extent(&self) -> (usize, usize, usize) {
        PaletteState::scroll_extent(self)
    }
    #[cfg(feature = "a11y-accesskit")]
    fn a11y(&self) -> accesskit::TreeUpdate {
        crate::palette::palette_a11y(self)
    }
}

/// Which surface a live [`Overlay`] holds — a cheap tag for the input gate + the
/// `settings_card` cache key (so two surfaces never share a cache line).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OverlayKind {
    #[cfg(test)]
    Settings,
    #[cfg(test)]
    About,
    Palette,
    #[cfg(test)]
    Update,
}

impl OverlayKind {
    /// The `controls front` `kind=` token — chosen so `kind=<x>` re-parses via
    /// `AuxTarget::parse` to the SAME surface, letting a driver pipe `controls front`
    /// -> `controls <kind>`. Palette maps to `"menu"` (its `AuxTarget`).
    pub(crate) fn keyword(self) -> &'static str {
        match self {
            #[cfg(test)]
            OverlayKind::Settings => "settings",
            #[cfg(test)]
            OverlayKind::About => "about",
            OverlayKind::Palette => "menu",
            #[cfg(test)]
            OverlayKind::Update => "update",
        }
    }
}

/// The single modal-overlay slot on a window. Exactly one variant is live at a time;
/// mutual exclusion is structural (a slot holds one value), replacing the old
/// hand-maintained "clear the other two" dance.
pub(crate) enum Overlay {
    /// Retired Settings-card representation, retained only for the legacy model and
    /// input regression tests. Shipping Settings is always a native tab app.
    #[cfg(test)]
    Settings(SettingsState),
    /// Retired modal, retained only for its low-level regression tests. Shipping
    /// About is the native Settings `/about` route.
    #[cfg(test)]
    About(AboutState),
    Palette(PaletteState),
    /// Retired modal, retained only for its low-level regression tests. Shipping
    /// Software Update is the native Settings `/updates` route.
    #[cfg(test)]
    Update(UpdateState),
}

impl Overlay {
    /// The DISCRIMINANT-SEEDED, forced-nonzero repaint fingerprint folded into
    /// `RepaintKey::settings_fp`. Domain-separates per variant so two surfaces can NEVER
    /// collide in the `settings_card` fp cache (which would reuse the WRONG surface's card
    /// — a silent WYSIWYG corruption), and `| 1` keeps `0` reserved for "closed".
    pub(crate) fn fingerprint(&self) -> u64 {
        // The INNER hash comes from the same `OverlayModel` that paints the tray; the
        // wrapper folds in the variant tag + forces non-zero.
        let inner = self.model().fingerprint();
        let tag: u64 = match self {
            #[cfg(test)]
            Overlay::Settings(_) => 1,
            #[cfg(test)]
            Overlay::About(_) => 2,
            Overlay::Palette(_) => 3,
            #[cfg(test)]
            Overlay::Update(_) => 4,
        };
        (tag.rotate_left(56) ^ inner) | 1
    }

    /// The active surface's model as a trait object — the single point the shared splice
    /// and repaint fingerprint fan out from.
    pub(crate) fn model(&self) -> &dyn OverlayModel {
        match self {
            #[cfg(test)]
            Overlay::Settings(s) => s,
            #[cfg(test)]
            Overlay::About(a) => a,
            Overlay::Palette(p) => p,
            #[cfg(test)]
            Overlay::Update(u) => u,
        }
    }

    /// The active surface's model as a mutable trait object.
    #[allow(
        dead_code,
        reason = "part of the OverlayModel surface; mutators go through the concrete accessor shims today"
    )]
    pub(crate) fn model_mut(&mut self) -> &mut dyn OverlayModel {
        match self {
            #[cfg(test)]
            Overlay::Settings(s) => s,
            #[cfg(test)]
            Overlay::About(a) => a,
            Overlay::Palette(p) => p,
            #[cfg(test)]
            Overlay::Update(u) => u,
        }
    }

    /// Which surface this slot holds.
    pub(crate) fn kind(&self) -> OverlayKind {
        match self {
            #[cfg(test)]
            Overlay::Settings(_) => OverlayKind::Settings,
            #[cfg(test)]
            Overlay::About(_) => OverlayKind::About,
            Overlay::Palette(_) => OverlayKind::Palette,
            #[cfg(test)]
            Overlay::Update(_) => OverlayKind::Update,
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

    /// The wrapper fingerprint must be DISJOINT across variants even when the inner
    /// fingerprints coincide — otherwise the `settings_card` fp cache could reuse one
    /// surface's card for another (silent WYSIWYG corruption). It must also never be `0`
    /// (the closed sentinel).
    #[test]
    fn fingerprint_is_disjoint_across_variants_and_nonzero() {
        // Force the inner fingerprints to coincide by constructing the wrapper value
        // directly from a shared inner hash: the wrapper's tag+rotate must still separate
        // them. We assert the wrapper formula over identical inner values.
        for inner in [0u64, 1, 42, u64::MAX, 0x00FF_00FF_00FF_00FF] {
            let s = (1u64.rotate_left(56) ^ inner) | 1;
            let a = (2u64.rotate_left(56) ^ inner) | 1;
            let p = (3u64.rotate_left(56) ^ inner) | 1;
            assert_ne!(s, a, "settings vs about collide at inner={inner:#x}");
            assert_ne!(s, p, "settings vs palette collide at inner={inner:#x}");
            assert_ne!(a, p, "about vs palette collide at inner={inner:#x}");
            assert_ne!(s, 0, "settings fp must be nonzero");
            assert_ne!(a, 0, "about fp must be nonzero");
            assert_ne!(p, 0, "palette fp must be nonzero");
        }

        // And over real, live models (whose inner hashes will differ too).
        let settings = Overlay::Settings(SettingsState::from_config(
            &crate::app_config::Config::default(),
        ));
        let about = Overlay::About(AboutState::new());
        let palette = Overlay::Palette(PaletteState::new());
        let (fs, fa, fp) = (
            settings.fingerprint(),
            about.fingerprint(),
            palette.fingerprint(),
        );
        assert_ne!(fs, fa);
        assert_ne!(fs, fp);
        assert_ne!(fa, fp);
        assert_ne!(fs, 0);
        assert_ne!(fa, 0);
        assert_ne!(fp, 0);
        assert_eq!(settings.kind(), OverlayKind::Settings);
        assert_eq!(about.kind(), OverlayKind::About);
        assert_eq!(palette.kind(), OverlayKind::Palette);
    }

    /// `status_line()` (the `controls front` open case) reports `open=true`, the surface
    /// `kind=<keyword>`, the exact `Overlay::fingerprint()`, and a `scroll/total/visible`
    /// extent — for EVERY variant (exhaustive fan-out). The `kind` keyword must re-parse
    /// via `AuxTarget::parse` to the SAME surface, so a driver can pipe `controls front`
    /// into `controls <kind>`.
    #[test]
    fn status_line_reports_open_kind_fp_and_extent() {
        let cfg = crate::app_config::Config::default();
        let cases = [
            Overlay::Settings(SettingsState::from_config(&cfg)),
            Overlay::About(AboutState::new()),
            Overlay::Palette(PaletteState::new()),
            Overlay::Update(UpdateState::from_status(1, "1.0", None, false)),
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
