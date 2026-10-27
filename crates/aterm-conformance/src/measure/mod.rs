// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `measure` check's instruments.

pub mod aos;
pub mod frame;
pub mod instance;
pub mod paint;
pub mod paint_probe;
pub mod rewind;
pub mod rewind_cost;
pub mod scan;
pub mod spin;
pub mod starvation;

use std::path::PathBuf;
use std::sync::Mutex;

static ONE_TAKE: Mutex<()> = Mutex::new(());

/// What a row is measured with: the binary under measurement and the stand-in TUI the scenes run.
#[derive(Clone, Debug)]
pub struct Rig {
    pub aterm: PathBuf,
    pub fake_tui: PathBuf,
}

impl Rig {
    /// The release `aterm` named by the first of `vars` that is set.
    /// # Panics
    /// None of `vars` is set, or it names no file: a measuring row with nothing to measure is red,
    /// never "not measured".
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
