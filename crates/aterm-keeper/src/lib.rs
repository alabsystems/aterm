// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! PTY keeper service, wire protocol, and client link.

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

#[cfg_attr(
    not(unix),
    expect(
        unused_macros,
        reason = "every verb that prints past the usage is Unix-only; the macro stays defined \
                  so a future non-Unix line cannot reach std's panicking println!"
    )
)]
macro_rules! println {
    () => {
        ::aterm_uds::stdout::print(::std::format_args!("\n"))
    };
    ($($arg:tt)*) => {
        ::aterm_uds::stdout::println(::std::format_args!($($arg)*))
    };
}

pub mod cli;
pub mod core;
pub mod identity;
pub mod job;
pub mod wire;

#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod code;
#[cfg(unix)]
pub mod frameio;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
mod sys;

pub use crate::core::{KeeperCore, RelaunchBrake};

/// Tier-1 of `PtyKeeperCustody`, `KeeperRelaunchBrake` and
/// `KeeperDeathJudgement` over the real core.
#[cfg(all(test, unix))]
mod conformance_custody;

/// Tier-1 of `KeeperCompanionCustody` (the crash-restore companions) over the real core.
#[cfg(all(test, unix))]
mod conformance_companion;

/// Create a `.ubd`-quiet `.noindex` scratch directory for `aterm-keeper` unit tests, bounded
/// under `/tmp` so keeper socket paths stay strictly below BSD `SUN_LEN`.
#[cfg(all(test, unix))]
pub(crate) fn test_scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let mut end = tag.len().min(12);
    while end > 0 && !tag.is_char_boundary(end) {
        end -= 1;
    }
    let short = &tag[..end];
    aterm_tempfile::Builder::new()
        .prefix(&format!("akk-{short}-"))
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("keeper scratch dir")
}

#[cfg(all(test, unix))]
mod scratch_tests {
    /// NEGATIVE CONTROL: even an oversized UTF-8 tag keeps `.noindex` naming, private `0700`
    /// permissions, and a keeper socket path strictly below BSD `SUN_LEN` (`103` bytes).
    #[test]
    fn test_scratch_dir_stays_private_and_under_unix_sun_len() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = crate::test_scratch_dir("oversized-keeper-tag-ééé-that-would-exceed-sun-len");
        let name = scratch
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .expect("utf-8 scratch dir name");
        assert!(name.ends_with(".noindex"), "missing .noindex: {name}");
        let mode = std::fs::symlink_metadata(scratch.path())
            .expect("stat scratch")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "scratch mode {mode:o}");
        let sock = scratch.path().join("designated.sock");
        assert!(
            sock.as_os_str().as_bytes().len() < 103,
            "keeper socket path {} ({} bytes) must stay below BSD SUN_LEN (103)",
            sock.display(),
            sock.as_os_str().as_bytes().len()
        );
    }
}
