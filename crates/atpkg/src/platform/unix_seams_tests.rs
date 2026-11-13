// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::path::{Path, PathBuf};

type Read = Option<(u64, u64)>;

thread_local! {
    static ATTRIBUTES: RefCell<Vec<(PathBuf, Read)>> = const { RefCell::new(Vec::new()) };
}

/// Read `path`'s attributes as `read` on this thread (`None`: `statx` unanswered).
pub(crate) fn attributes_of(path: &Path, read: Read) {
    ATTRIBUTES.with(|a| a.borrow_mut().push((path.to_path_buf(), read)));
}

/// The `(stx_attributes, stx_attributes_mask)` of `STATX_ATTR_IMMUTABLE` set.
pub(crate) const IMMUTABLE: (u64, u64) = (super::ATTR_IMMUTABLE, super::ATTR_IMMUTABLE);

/// Forget every stand-in of this thread.
pub(crate) fn clear() {
    ATTRIBUTES.with(|a| a.borrow_mut().clear());
}

pub(super) fn attributes(path: &Path) -> Option<Read> {
    ATTRIBUTES.with(|a| {
        a.borrow()
            .iter()
            .rev()
            .find(|(p, _)| p == path)
            .map(|(_, read)| *read)
    })
}
