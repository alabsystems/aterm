// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Development seams: environment variables a developer or test may use to repoint or
//! instrument aterm, which a shipped binary does not read at all.
//!
//! Anything a user tunes is a setting, not an env var. [`dev_seam!`](macro@crate::dev_seam) is
//! the one reader and expands in the calling crate under
//! `cfg(any(debug_assertions, feature = "dev-seams"))`, so every crate that reads a seam
//! declares `dev-seams` in its manifest and forwards it to its dependencies. A release
//! build enables neither, compiling every read to `None`.

/// Read the development seam `$name`: `Some(value)` only when the variable is set in a build
/// that compiles seams; `None` in every shipped binary. See the [module docs](mod@crate::dev_seam).
#[macro_export]
macro_rules! dev_seam {
    ($name:expr) => {{
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        let seam: ::core::option::Option<::std::ffi::OsString> = ::std::env::var_os($name);
        #[cfg(not(any(debug_assertions, feature = "dev-seams")))]
        let seam: ::core::option::Option<::std::ffi::OsString> = {
            let _ = $name;
            ::core::option::Option::None
        };
        seam
    }};
}
