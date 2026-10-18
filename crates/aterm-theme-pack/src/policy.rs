// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The pack's licence policy: shared by the library and its build script
//! (`#[path]`-included there), so the build refuses exactly what the
//! library would.

/// The SPDX licences a palette's own project may carry for its theme to
/// ship in the pack: permissive, with no copyleft and no field of use.
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
#[must_use]
pub fn admitted(spdx: &str) -> bool {
    ADMITTED_LICENCES.contains(&spdx)
}

/// Whether a file under `spdx` carries its notice by repeating the whole
/// licence text (MIT, ISC, the BSDs, 0BSD, CC0, the Unlicense). Apache-2.0
/// does not: a copy carries the copyright line and the short notice that
/// points at the licence.
#[must_use]
pub fn notice_is_whole_text(spdx: &str) -> bool {
    spdx != "Apache-2.0"
}
