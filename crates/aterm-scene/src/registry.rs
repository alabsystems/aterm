// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The **scene registry** — the modularity seam. Scenes are addressed by NAME, so a panel
//! manifest resolves to a [`Scene`] here, and each scene ships a default [`Binding`] (which live
//! stat drives which behaviour) as DATA. Adding a built-in scene is one match arm; a third-party
//! scene is its own `Scene` impl + a name.
//!
//! **The built-in scenes were deleted for a full rewrite** — every name currently resolves to
//! the inert [`Placeholder`], so the scene *framework* (trait + bridge + compositor + the
//! worker-thread isolation + the Trust gate) stays live and buildable with no art. Re-populate
//! `build_scene`/`scene_names`/`default_binding` when the new scenes land.

use crate::Placeholder;
use crate::bind::Binding;
use crate::scene::Scene;

/// Every built-in scene name, in a stable order. Empty until the scene rewrite re-populates it.
#[must_use]
pub fn scene_names() -> &'static [&'static str] {
    &[]
}

/// Build a scene by name. Until the rewrite, every name resolves to the inert [`Placeholder`]
/// (the scene band shows a blank canvas — the "place" held open). `seed`/`skin` are accepted for
/// API symmetry with future real scenes.
#[must_use]
pub fn build_scene(_name: &str, seed: u32, _skin: u32) -> Box<dyn Scene> {
    Box::new(Placeholder::new(seed))
}

/// The default stat→behaviour binding for a scene (the manifest can override it). The
/// placeholder consumes no drives, so this is empty until the rewrite.
#[must_use]
pub fn default_binding(_name: &str) -> Binding {
    Binding::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_builds_placeholder_until_rewrite() {
        // The built-in scenes were deleted for a full rewrite: the name list is empty and every
        // name resolves to the inert placeholder — the framework stays live with no art.
        assert!(
            scene_names().is_empty(),
            "no built-in scenes until the rewrite"
        );
        let s = build_scene("anything", 1, 0x00FF_FFFF);
        assert_eq!(s.id(), "placeholder");
        assert!(!s.is_active(), "placeholder never animates (0% idle)");
        let _ = default_binding("anything");
    }
}
