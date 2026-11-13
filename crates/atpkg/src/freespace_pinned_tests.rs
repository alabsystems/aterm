// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};

static PINS: std::sync::Mutex<Vec<(PathBuf, u64)>> = std::sync::Mutex::new(Vec::new());

/// [`super::available_bytes`] answers its free bytes for every path under its root until it
/// drops.
pub(crate) struct Pin(PathBuf);

/// Pin `free` bytes free for every path under `root`.
pub(crate) fn pin(root: &Path, free: u64) -> Pin {
    pins().push((root.to_path_buf(), free));
    Pin(root.to_path_buf())
}

/// The bytes the latest pin over `start` names.
pub(super) fn under(start: &Path) -> Option<u64> {
    pins()
        .iter()
        .rev()
        .find(|(root, _)| start.starts_with(root))
        .map(|&(_, free)| free)
}

fn pins() -> std::sync::MutexGuard<'static, Vec<(PathBuf, u64)>> {
    PINS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Drop for Pin {
    fn drop(&mut self) {
        let mut pins = pins();
        if let Some(at) = pins.iter().rposition(|(root, _)| *root == self.0) {
            pins.remove(at);
        }
    }
}
