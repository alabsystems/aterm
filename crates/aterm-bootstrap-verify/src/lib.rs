// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The installers' bootstrap verifier: tools/install.sh's on Linux, and
//! apps/aterm-win/get-aterm.ps1's on Windows ([`windows`]). Each script pins this crate's
//! static build by SHA-256 ([`pin`]) and runs it before anything it downloaded runs.

use std::fmt;

pub mod pin;
pub mod windows;

#[cfg(unix)]
mod accounts;
#[cfg(unix)]
mod binary;
mod chain;
#[cfg(unix)]
mod host;
mod py;
#[cfg(unix)]
mod store;

#[cfg(unix)]
pub use accounts::{Account, Accounts, Group, private_group, user_private_group};
#[cfg(unix)]
pub use binary::verify_binary;
pub use chain::{Ask, Policy, Signed, deadline, verify_ed25519, verify_manifest, withdrawn_floor};
#[cfg(unix)]
pub use host::{REMOTE_FILESYSTEMS, acl_rule, filesystem_type, remote_filesystem_type};
#[cfg(unix)]
pub use store::{check_install_path, preflight_store, read_install_policy};

/// The paper master's Ed25519 public key, the root every roster is signed under.
pub const MASTER_PUBKEYS: &[&str] = &["DtiLfpk0iUSrK1/LkyIVf+4C2eGjD2Myf4Sr/FCoMPQ="];

/// What every refusal's line starts with, the words install.sh's callers strip.
pub const REFUSED: &str = "install.sh: Linux verification refused: ";

/// What every refusal of the Windows command line ([`run_windows`]) starts with, the words
/// get-aterm.ps1 shows.
pub const WINDOWS_REFUSED: &str = "get-aterm.ps1: Windows verification refused: ";

/// The test seam: whitespace-separated base64 keys trusted in place of [`MASTER_PUBKEYS`].
#[cfg(any(test, debug_assertions))]
pub const TEST_MASTER_PUBKEYS: &str = "ATERM_BOOTSTRAP_VERIFY_TEST_MASTER_PUBKEYS";

/// The largest Linux executable a release may name, as the updater bounds it.
pub const MAX_BINARY: u64 = 512 * 1024 * 1024;

/// Why the verifier refused: the words after [`REFUSED`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

impl Refusal {
    /// A refusal saying `why`.
    pub fn new(why: impl Into<String>) -> Self {
        Self(why.into())
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A verdict: the value, or why it was refused.
pub type Verdict<T> = Result<T, Refusal>;

/// Refuse with `why` unless `condition` holds.
pub(crate) fn require(condition: bool, why: &str) -> Verdict<()> {
    if condition {
        Ok(())
    } else {
        Err(Refusal::new(why))
    }
}

/// The keys a roster must be signed under: [`MASTER_PUBKEYS`], or in a build with debug
/// assertions the ones [`TEST_MASTER_PUBKEYS`] names when it is set.
#[must_use]
pub fn master_pubkeys() -> Vec<String> {
    #[cfg(any(test, debug_assertions))]
    if let Some(keys) = std::env::var_os(TEST_MASTER_PUBKEYS) {
        return keys
            .to_string_lossy()
            .split_whitespace()
            .map(str::to_string)
            .collect();
    }
    MASTER_PUBKEYS
        .iter()
        .map(|key| (*key).to_string())
        .collect()
}

/// Unix timestamp for `now`, failing closed (`i64::MAX`) when the clock precedes 1970 or
/// overflows `i64` so expiry deadlines (`valid_until`, `not_after`) reject rather than admit.
#[must_use]
pub fn unix_now_fail_closed(now: std::time::SystemTime) -> i64 {
    now.duration_since(std::time::UNIX_EPOCH)
        .map_or(i64::MAX, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// The verifier's command line (`args` without the program name) at `now` (Unix seconds).
/// # Errors
/// Every refusal install.sh's embedded verifier made, in its words.
#[cfg(unix)]
pub fn run(args: &[std::ffi::OsString], now: i64) -> Verdict<Option<String>> {
    chain::main(args, now, &master_pubkeys())
}

/// On Windows, the command line is get-aterm.ps1's ([`run_windows`]).
/// # Errors
/// Every refusal of [`windows::verify_windows_manifest`] and [`windows::verify_zip`].
#[cfg(windows)]
pub fn run(args: &[std::ffi::OsString], now: i64) -> Verdict<Option<String>> {
    run_windows(args, now)
}

/// Elsewhere there is no installer to verify for.
/// # Errors
/// Always.
#[cfg(not(any(unix, windows)))]
pub fn run(_args: &[std::ffi::OsString], _now: i64) -> Verdict<Option<String>> {
    Err(Refusal::new(
        "the bootstrap verifier runs on Linux, for tools/install.sh, and on Windows, for \
         apps/aterm-win/get-aterm.ps1",
    ))
}

/// get-aterm.ps1's command line (`args` without the program name) at `now` (Unix seconds):
/// `<dir> <latest> <tag> <slug> <arch> [<zip>]`, which prints the signed Windows zip's
/// `name\tdigest\tsize\tbuild\troster_seq` line, or [`windows::NO_ZIP`] when `<tag>` is the
/// latest release and it carries none, and holds a `<tag>` older than `<latest>` to the latest
/// release's roster and floors. The Windows build's [`run`]; it builds on every platform, so
/// the chain it asks is tested on each.
/// # Errors
/// Every refusal of [`windows::verify_windows_manifest`] and [`windows::verify_zip`].
pub fn run_windows(args: &[std::ffi::OsString], now: i64) -> Verdict<Option<String>> {
    windows::main(args, now, &master_pubkeys())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
