// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide launch flags parsed from the command line.

use std::sync::OnceLock;

/// The renderer a launch flag forces. `--cpu` / `--gpu`, the last one wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RendererFlag {
    Cpu,
    Gpu,
}

/// Render and font flags for this launch.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Launch {
    /// `--cpu` / `--gpu`: outranks config `gpu`.
    pub(crate) renderer: Option<RendererFlag>,
    /// `--font-px N`: outranks config `font_px`; inside `FONT_PX_MIN..=FONT_PX_MAX`.
    pub(crate) font_px: Option<f32>,
    /// `--font NAME|PATH`: outranks config `font_family`; never blank.
    pub(crate) font_family: Option<String>,
    /// `--scale F`: render scale override; finite and positive.
    pub(crate) scale: Option<f64>,
}

impl Launch {
    /// No flags: what every reader sees until (or unless) a command line is installed.
    pub(crate) const NONE: Self = Self {
        renderer: None,
        font_px: None,
        font_family: None,
        scale: None,
    };
}

static LAUNCH: OnceLock<Launch> = OnceLock::new();

/// Installs this process's launch flags (first install wins).
pub(crate) fn install(flags: Launch) {
    let _ = LAUNCH.set(flags);
}

/// This process's launch flags ([`Launch::NONE`] when none were installed).
pub(crate) fn flags() -> &'static Launch {
    LAUNCH.get().unwrap_or(&Launch::NONE)
}
