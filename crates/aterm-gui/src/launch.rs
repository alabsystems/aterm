// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Launch flags: what this process's command line pins.
//!
//! No environment variable changes what a shipped aterm does, so a flag is never
//! carried to its reader as an `ATERM_*` variable (that would also leak it into
//! every child shell). `cli::parse_cli` parses the flags once and installs them
//! here; a launch with no flags (every Finder/.app launch, every unit test) reads
//! [`Launch::NONE`].

use std::sync::OnceLock;

/// The renderer a launch flag forces. `--cpu` / `--gpu`, the last one wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RendererFlag {
    Cpu,
    Gpu,
}

/// The render/font flags of one launch. Every field is `None` unless its flag
/// was given; each value was validated by `parse_cli` before it got here.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Launch {
    /// `--cpu` / `--gpu`: outranks config `gpu`.
    pub(crate) renderer: Option<RendererFlag>,
    /// `--font-px N`: outranks config `font_px`; inside `FONT_PX_MIN..=FONT_PX_MAX`.
    pub(crate) font_px: Option<f32>,
    /// `--font NAME|PATH`: outranks config `font_family`; never blank.
    pub(crate) font_family: Option<String>,
    /// `--scale F`: the render scale every window uses instead of its display's
    /// (and headless instead of 1.0); finite and positive.
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

/// Install this process's launch flags. First install wins: `main_entry` installs
/// the parsed command line before any window exists, and a diagnostic verb
/// (`--diagnose`, `--show-config`, …) installs the flags parsed before it so its
/// report names the effective values.
pub(crate) fn install(flags: Launch) {
    let _ = LAUNCH.set(flags);
}

/// This process's launch flags ([`Launch::NONE`] when none were installed).
pub(crate) fn flags() -> &'static Launch {
    LAUNCH.get().unwrap_or(&Launch::NONE)
}
