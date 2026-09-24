// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! DEVELOPMENT SEAMS: the environment variables a developer or a test may use to
//! repoint or instrument aterm, and that a shipped binary does not read at all.
//!
//! The owner's rule (2026-09-22, verbatim): *"I want the one true best batteries
//! included default on path. Delete alternatives. in the future, we could add
//! settings, but NOT ENV VARS those are for development."* So an environment variable
//! that changes what aterm does is either internal protocol (a parent handing its own
//! child something), a build stamp, or one of these seams — and a seam compiles only
//! under `cfg(any(debug_assertions, feature = "dev-seams"))`. `debug_assertions` is on
//! in every dev and test build and off in `--release`; the `dev-seams` feature exists
//! so a QA build can be optimized and still carry them. The release cutter enables
//! neither (`aterm-release`'s buildplan pins that), so a shipped binary reads no seam:
//! setting one against it does nothing, silently, as it would for any other name.
//!
//! [`dev_seam!`](crate::dev_seam) is the ONE reader. It expands IN THE CALLING CRATE,
//! so `feature = "dev-seams"` is that crate's own feature: every crate that reads a
//! seam declares `dev-seams` in its manifest and forwards it to the crates it depends
//! on. `aterm-update-core`'s `env_reads` gate holds the rule: a seam's name may be read
//! only through this macro, and a release-code env read of any other `ATERM_*` /
//! `ATPKG_*` name must be on that gate's allow-list.

/// Read the development seam `$name` — `Some(value)` only in a build that compiles
/// seams (`debug_assertions`, or the calling crate's `dev-seams` feature) and only
/// when the variable is set; `None` in every shipped binary, whatever the environment
/// holds. See the [module docs](crate::dev_seam).
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

#[cfg(test)]
mod tests {
    /// A test build is a debug build, so the seam reads the environment.
    #[test]
    fn a_test_build_reads_the_seam() {
        const NAME: &str = "ATERM_DEV_SEAM_TEST_PROBE_UNSET";
        assert_eq!(crate::dev_seam!(NAME), None, "unset reads as None");
        assert_eq!(crate::dev_seam!("PATH"), std::env::var_os("PATH"));
    }
}
