// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Update watch: exits with [`crate::launchd::EXIT_RESTART`] when the mug binary is replaced.

use std::path::{Path, PathBuf};

use crate::code::code_fingerprint;

type Print = Vec<u8>;

/// The print the mug started with, and the rule that says when the code changed.
#[derive(Debug)]
pub(super) struct Watch {
    origin: PathBuf,
    baseline: Option<Print>,
}

impl Watch {
    /// Watches the code of `origin` against the code of `running`.
    pub(super) fn start(origin: PathBuf, running: &Path) -> Self {
        let baseline = code_fingerprint(running);
        Self { origin, baseline }
    }

    /// Whether the code was replaced since [`Watch::start`].
    pub(super) fn replaced(&mut self) -> bool {
        let origin = &self.origin;
        replaced(&mut self.baseline, code_fingerprint(origin), || {
            code_fingerprint(origin)
        })
    }
}

fn replaced(
    baseline: &mut Option<Print>,
    now: Option<Print>,
    again: impl FnOnce() -> Option<Print>,
) -> bool {
    let Some(now) = now else {
        return false;
    };
    match baseline {
        None => {
            *baseline = Some(now);
            false
        }
        Some(was) if *was == now => false,
        Some(_) => again().as_ref() == Some(&now),
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
