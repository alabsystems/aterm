// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::path::{Path, PathBuf};

thread_local! {
    static PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Run `f` with `path` as the fallback on this thread, cleared after it even on unwind.
pub(crate) fn with<T>(path: &Path, f: impl FnOnce() -> T) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            PATH.with(|p| p.replace(None));
        }
    }
    PATH.with(|p| p.replace(Some(path.to_path_buf())));
    let _clear = Clear;
    f()
}

/// The fallback this thread's test named, if any.
pub(crate) fn path() -> Option<PathBuf> {
    PATH.with(|p| p.borrow().clone())
}
