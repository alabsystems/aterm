// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A **placeholder** scene: the empty stand-in that keeps the scene *framework* (the trait, the
//! bridge, the GPU/CPU compositor, the per-window worker-thread isolation, the config toggle,
//! and the Trust isolation gate) alive and buildable while the actual scene ART is rewritten
//! from scratch.
//!
//! It paints nothing and never animates, so with the scene band on it reserves the rows but
//! shows a blank canvas — the "place" for the real scenes, held open. Replace this (and register
//! the new scenes in [`crate::registry`]) when the rewrite lands.

use crate::bind::Drives;
use crate::scene::{Env, Scene, SceneFrame, TextPulse};

/// The empty stand-in scene (see the module docs). Deterministic and inert.
pub struct Placeholder;

impl Placeholder {
    /// Construct the placeholder (the `seed` is accepted for API symmetry with real scenes).
    #[must_use]
    pub fn new(_seed: u32) -> Self {
        Self
    }
}

impl Default for Placeholder {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Scene for Placeholder {
    fn id(&self) -> &'static str {
        "placeholder"
    }

    fn tick(&mut self, _dt: f32, _drives: &Drives, _env: &Env) {}

    fn emit(&self, _env: &Env, _out: &mut SceneFrame) {}

    /// Never active → the host returns to 0% idle (no wasted frames for an empty scene).
    fn is_active(&self) -> bool {
        false
    }

    fn on_text(&mut self, _pulse: TextPulse) {}

    fn describe(&self) -> String {
        "placeholder (scene art pending full rewrite)".to_string()
    }
}
