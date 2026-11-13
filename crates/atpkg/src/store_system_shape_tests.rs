// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::RefCell;
use std::path::{Path, PathBuf};

thread_local! {
    static FORCED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// Answer "system-shaped" for `prefix` on this thread from now on.
#[cfg(unix)]
pub(crate) fn force(prefix: &Path) {
    FORCED.with(|f| f.borrow_mut().push(prefix.to_path_buf()));
}

pub(super) fn forced(prefix: &Path) -> bool {
    FORCED.with(|f| f.borrow().iter().any(|p| p == prefix))
}
