// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux installer's bootstrap verifier.

use std::fmt;

pub mod pin;

#[cfg(unix)]
mod accounts;
#[cfg(unix)]
mod binary;
#[cfg(unix)]
mod chain;
#[cfg(unix)]
mod host;
#[cfg(unix)]
mod py;
#[cfg(unix)]
mod store;

#[cfg(unix)]
pub use accounts::{Account, Accounts, Group, private_group, user_private_group};
#[cfg(unix)]
pub use binary::verify_binary;
#[cfg(unix)]
pub use chain::{Ask, Signed, deadline, verify_ed25519, verify_manifest, withdrawn_floor};
#[cfg(unix)]
pub use host::{REMOTE_FILESYSTEMS, acl_rule, filesystem_type, remote_filesystem_type};
#[cfg(unix)]
pub use store::{Policy, check_install_path, preflight_store, read_install_policy};

/// The paper master's Ed25519 public key, the root every roster is signed under.
pub const MASTER_PUBKEYS: &[&str] = &["DtiLfpk0iUSrK1/LkyIVf+4C2eGjD2Myf4Sr/FCoMPQ="];

/// What every refusal's line starts with, the words install.sh's callers strip.
pub const REFUSED: &str = "install.sh: Linux verification refused: ";

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
#[cfg(unix)]
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

/// The verifier's command line (`args` without the program name) at `now` (Unix seconds).
/// # Errors
/// Every refusal install.sh's embedded verifier made, in its words.
#[cfg(unix)]
pub fn run(args: &[std::ffi::OsString], now: i64) -> Verdict<Option<String>> {
    chain::main(args, now, &master_pubkeys())
}

/// Off Unix there is no install.sh to verify for.
/// # Errors
/// Always.
#[cfg(not(unix))]
pub fn run(_args: &[std::ffi::OsString], _now: i64) -> Verdict<Option<String>> {
    Err(Refusal::new(
        "the bootstrap verifier runs on Linux, for tools/install.sh",
    ))
}
