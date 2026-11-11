// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The one reader of `ATERM_TEST_NEGATIVE_CONTROL`: whether a named negative control is on in
//! this process.
//!
//! A negative control restores an old rule so a test can show that the rule it replaced bites
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §9). A unit test turns one on for its own thread;
//! a lab instance, a separate process a lab lane starts, cannot reach a thread of the test, so
//! it is started with `ATERM_TEST_NEGATIVE_CONTROL=<name>[,<name>…]` and every switch the
//! variable names is on for that whole process. [`on`] reads the variable once per process.
//!
//! It exists only where `any(test, debug_assertions)` holds, never through `dev_seam!`, whose
//! `dev-seams` feature a release build can turn on: a release build has neither the reader nor
//! any branch a control guards, so no environment variable can restore an old trust rule there.
//! The census `debug-seam` guard fails every mention of the variable that a release build
//! compiles, here or anywhere else.

/// Whether the negative control `name` (`old-gap-rule`, `unsettled-ok`, …) is on for this
/// whole process: `ATERM_TEST_NEGATIVE_CONTROL` names it, alone or in a comma-separated list.
/// The variable is read the first time any control is asked for, and never again.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn on(name: &str) -> bool {
    static NAMED: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMED
        .get_or_init(|| named(std::env::var_os("ATERM_TEST_NEGATIVE_CONTROL").as_deref()))
        .iter()
        .any(|on| on == name)
}

/// The control names a value of the variable lists: comma-separated, each trimmed, empty
/// names dropped. A value that is not UTF-8 names nothing.
#[cfg(any(test, debug_assertions))]
fn named(value: Option<&std::ffi::OsStr>) -> Vec<String> {
    value
        .and_then(std::ffi::OsStr::to_str)
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "negative_control_tests.rs"]
mod tests;
