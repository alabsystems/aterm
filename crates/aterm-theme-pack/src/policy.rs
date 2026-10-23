// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Licence policy shared by the theme pack library and its build script.

/// Permissive SPDX licences admitted by the theme pack.
pub const ADMITTED_LICENCES: [&str; 8] = [
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "0BSD",
    "CC0-1.0",
    "Unlicense",
];

/// Whether `spdx` is in [`ADMITTED_LICENCES`].
#[inline]
#[must_use]
pub fn admitted(spdx: &str) -> bool {
    ADMITTED_LICENCES.contains(&spdx)
}

/// Whether a file under `spdx` carries its notice by repeating the whole licence text.
#[inline]
#[must_use]
pub fn notice_is_whole_text(spdx: &str) -> bool {
    spdx != "Apache-2.0"
}
