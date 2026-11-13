// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Layout, PathBuf};

static WRITES: super::Mutex<Vec<(PathBuf, usize)>> = super::Mutex::new(Vec::new());

pub(super) fn note(layout: &Layout) {
    let mut w = WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match w.iter_mut().find(|(p, _)| *p == layout.prefix) {
        Some((_, n)) => *n += 1,
        None => w.push((layout.prefix.clone(), 1)),
    }
}

/// The durable writes of `layout`'s record so far.
#[cfg(unix)]
pub(crate) fn writes(layout: &Layout) -> usize {
    let w = WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    w.iter()
        .find(|(p, _)| *p == layout.prefix)
        .map_or(0, |(_, n)| *n)
}
