// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Development seams: environment variables a developer or test may use to repoint or
//! instrument aterm, which a shipped binary does not read at all.
//!
//! Anything a user tunes is a setting, not an env var. A seam compiles only under
//! `cfg(any(debug_assertions, feature = "dev-seams"))`; the feature lets a QA build be optimized
//! and keep them. The release build enables neither, so setting a seam against a shipped binary
//! does nothing.
//!
//! [`dev_seam!`](crate::dev_seam) is the ONE reader. It expands IN THE CALLING CRATE,
//! so `feature = "dev-seams"` is that crate's own feature: every crate that reads a
//! seam declares `dev-seams` in its manifest and forwards it to the crates it depends
//! on. A seam's name may be read only through this macro, and a release build
//! compiles every read through it to `None`, so no seam ships.

/// Read the development seam `$name`: `Some(value)` only when the variable is set in a build
/// that compiles seams; `None` in every shipped binary. See the [module docs](crate::dev_seam).
#[macro_export]
macro_rules! dev_seam {
    ($name:expr) => {{
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        let seam: ::core::option::Option<::std::ffi::OsString> = ::std::env::var_os($name);
        #[cfg(not(any(debug_assertions, feature = "dev-seams")))]
        let seam: ::core::option::Option<::std::ffi::OsString> = {
            // Named so a const that exists only for this read is not dead code in a
            // release build; the value is never looked up.
            let _ = $name;
            ::core::option::Option::None
        };
        seam
    }};
}
