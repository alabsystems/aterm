// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether a process is a Cargo test binary, by the name Cargo gives one
//! (`aterm_uds::test_binary` re-exports it): what keeps a test off the person's state, and what a
//! debug build reads to wait for a kernel read a test needs as a hang detector, not its
//! production bound (`kproc`'s table).

use std::ffi::OsStr;
use std::path::Path;

/// Whether `exe` is a Cargo test binary: an executable with the name Cargo gives one,
/// `<name>-<16 hex digits>`, in any folder, so that a copy kept aside to compare two builds stays
/// out of the person's folders as the original does. Under any other name it cannot be told from a
/// development program, and it runs on the person's real state, sockets and fleet port. Cargo names
/// an example and a build script the same way, and each of those is a program.
///
/// Linux names a running image that a later build unlinked `<path> (deleted)` (its
/// `/proc/self/exe`, which `current_exe` reads), and that image is still the test binary it was.
#[must_use]
pub fn is_test_binary(exe: &Path) -> bool {
    #[cfg(unix)]
    let exe = {
        use std::os::unix::ffi::OsStrExt as _;
        exe.as_os_str()
            .as_bytes()
            .strip_suffix(b" (deleted)")
            .map_or(exe, |path| Path::new(OsStr::from_bytes(path)))
    };
    let executable = exe.extension().is_none_or(|ext| ext == "exe");
    let named = exe
        .file_stem()
        .and_then(OsStr::to_str)
        .and_then(|stem| stem.rsplit_once('-'))
        .is_some_and(|(name, hash)| {
            !name.is_empty()
                && !name.starts_with("build_script_")
                && hash.len() == 16
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    let example = exe
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "examples");
    executable && named && !example
}

/// Whether this process is a Cargo test binary ([`is_test_binary`] of its executable), read once.
#[cfg(debug_assertions)]
#[must_use]
pub fn this_is_a_test_binary() -> bool {
    static IS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS.get_or_init(|| std::env::current_exe().is_ok_and(|exe| is_test_binary(&exe)))
}
