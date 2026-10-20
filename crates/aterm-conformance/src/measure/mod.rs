// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `measure` check's instruments: pixel-checked paint and event-loop spin,
//! run against the binary a release ships.
//!
//! * [`scan`] reads a captured take (PNG frames decoded by `aterm-png`) and
//!   [`paint::judge`] decides it against a row's expectation;
//! * [`starvation`] is the instrument's account of its own run, which can only
//!   take a green away;
//! * [`paint_probe`] and [`spin`] drive a private headless instance through
//!   the binary's own `ctl` ([`instance`]);
//! * the stand-in TUIs the scenes run are the crate's `fake-tui` binary.
//!
//! The explicit measurement matrices are `benches/paint.rs` and `benches/spin.rs`.

pub mod frame;
pub mod instance;
pub mod paint;
pub mod paint_probe;
pub mod scan;
pub mod spin;
pub mod starvation;

use std::path::PathBuf;
use std::sync::Mutex;

/// Takes run one at a time in a process: concurrent takes contend for frames
/// and CPU, and a spin window measures idleness.
static ONE_TAKE: Mutex<()> = Mutex::new(());

/// What a row is measured with: the binary under measurement and the stand-in
/// TUI the scenes run.
#[derive(Clone, Debug)]
pub struct Rig {
    pub aterm: PathBuf,
    pub fake_tui: PathBuf,
}

impl Rig {
    /// The release `aterm` named by the first of `vars` that is set, and the
    /// `fake-tui` at `fake_tui`.
    ///
    /// # Panics
    /// None of `vars` is set, or it names no file: a measuring row with
    /// nothing to measure is red, never "not measured".
    #[must_use]
    pub fn release(vars: &[&str], fake_tui: &str) -> Self {
        let aterm = vars
            .iter()
            .find_map(|v| std::env::var_os(v).map(PathBuf::from))
            .unwrap_or_else(|| {
                panic!(
                    "this row measures a release aterm: set {} to its executable \
                     (<aterm.app>/Contents/MacOS/aterm)",
                    vars.join(" or ")
                )
            });
        assert!(aterm.is_file(), "{} is not a file", aterm.display());
        Self {
            aterm,
            fake_tui: PathBuf::from(fake_tui),
        }
    }
}
