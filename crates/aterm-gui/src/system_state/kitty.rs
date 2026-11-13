// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The setup screen's kitty, baked before it is drawn (`docs/DESIGN-init-on-by-default-2026-10-25.md`
//! §4.5): every pose the lockup shows is baked on a background thread, current pose first, into a
//! cache the raster reads by pose, sized for the page's working set (each pose, on both grounds,
//! at two display scales), so a pose change never bakes on the thread that paints. The view
//! draws the pose it last drew until the next one is in the cache ([`pose_to_draw`]); the bake
//! wakes the window once it lands. Each pose is baked at the whole device size it is drawn at
//! and drawn one to one, never resampled, so the 29 pt kitty is crisp at 1× and 2×.

use std::sync::{Mutex, PoisonError};

use aterm_effects::pet_glyphs_gen::PetGlyphId;

use crate::widget::ImageArt;

/// The poses the lockup draws (§4.5's table): standing and sitting between steps, looking up
/// for the person, an ear back for a failure, the loaf when done, and the walk SCR-3 glides in.
pub(crate) const POSES: [PetGlyphId; 9] = [
    PetGlyphId::PetStand,
    PetGlyphId::PetSit,
    PetGlyphId::PetSitLookup,
    PetGlyphId::PetSitEar,
    PetGlyphId::PetLoaf,
    PetGlyphId::PetWalk0,
    PetGlyphId::PetWalk1,
    PetGlyphId::PetWalk2,
    PetGlyphId::PetWalk3,
];

/// A baked pose: the art and its device width and height.
type Key = (ImageArt, u32, u32);

/// The sizes a background bake has been asked for, in device pixels tall.
static ASKED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// The device size `art` is drawn at in a `h`-point-tall box on a display of `scale`: whole
/// device pixels, its width from its own aspect.
pub(crate) fn device_size(art: ImageArt, h: f32, scale: f32) -> (u32, u32) {
    let height = (h * scale).round().max(1.0);
    let width = (height * art.aspect()).round().max(1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a kitty a few dozen device pixels across"
    )]
    (width as u32, height as u32)
}

/// Whether `key` is baked and kept.
fn baked(key: Key) -> bool {
    let (art, w, h) = key;
    art.is_baked(w, h)
}

/// Bake `key` now, on the calling thread, and keep it.
fn bake_now(key: Key) {
    let (art, w, h) = key;
    let _ = art.baked(w, h);
}

/// `pose`'s key on the `dark` or light ground for an `h`-point kitty on a display of `scale`.
fn key(pose: PetGlyphId, dark: bool, h: f32, scale: f32) -> Key {
    let art = ImageArt::Kitty { pose, dark };
    let (w, h) = device_size(art, h, scale);
    (art, w, h)
}

/// Bake every pose for an `h`-point kitty on a display of `scale`, on both grounds (the view
/// does not know the theme its page paints on), on a background thread, `first` first; once per
/// size. Each pose that lands wakes the window, so a view waiting on it draws it.
pub(crate) fn prewarm(first: PetGlyphId, h: f32, scale: f32) {
    // A test bakes on its own thread, into its own cache: a pose the raster asks for is baked
    // there.
    if cfg!(test) {
        return;
    }
    let height = key(first, true, h, scale).2;
    {
        let mut asked = ASKED.lock().unwrap_or_else(PoisonError::into_inner);
        if asked.contains(&height) {
            return;
        }
        asked.push(height);
    }
    let order: Vec<PetGlyphId> = std::iter::once(first)
        .chain(POSES.into_iter().filter(|pose| *pose != first))
        .collect();
    let started = std::thread::Builder::new()
        .name("aterm-kitty-bake".into())
        .spawn(move || {
            for pose in order {
                let mut fresh = false;
                for dark in [true, false] {
                    let key = key(pose, dark, h, scale);
                    if !baked(key) {
                        bake_now(key);
                        fresh = true;
                    }
                }
                if fresh {
                    crate::workstation_setup::send_setup_wake(crate::Wake::KittyBaked);
                }
            }
        });
    if started.is_err() {
        // No thread: the raster bakes each pose as it first draws it.
        ASKED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|asked| *asked != height);
    }
}

/// Whether `pose` is baked on both grounds at this size.
fn ready(pose: PetGlyphId, h: f32, scale: f32) -> bool {
    [true, false]
        .into_iter()
        .all(|dark| baked(key(pose, dark, h, scale)))
}

/// The pose the view draws: `wanted` once it is baked at this size, else the last pose drawn
/// (`last`) while that one is, else `wanted` (the raster bakes it on first draw). A pose change
/// never waits on a bake: the old pose stays until the new one is ready.
pub(crate) fn pose_to_draw(
    wanted: PetGlyphId,
    last: Option<PetGlyphId>,
    h: f32,
    scale: f32,
) -> PetGlyphId {
    match last {
        Some(last) if last != wanted && !ready(wanted, h, scale) && ready(last, h, scale) => last,
        _ => wanted,
    }
}

#[cfg(test)]
#[path = "kitty_tests.rs"]
mod tests;
