// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux installer's bootstrap verifier: the signed-release chain `tools/install.sh`
//! checks before it downloads a Linux release and again before it runs it ([`run`]).
//!
//! Linux has no Apple code-signing chain to bootstrap from, so the installer checks the
//! updater's: the roster the paper master signed ([`MASTER_PUBKEYS`]), a live machine's
//! signature over the appcast and its attribution, the release's tag, origin and Linux
//! triples, the durable floors and revocations `.aterm-update/state.toml` records beside
//! the store, the store's ancestry (no other account can replace what it installs), and
//! the candidate's ELF header and digest. It is the program install.sh embedded as a
//! `python3 -I` heredoc (`ATERM_LINUX_VERIFY_PY`), check for check and message for message:
//! a release one refuses the other refuses with the same words, and what one admits the
//! other prints as the same line.
//!
//! It ships on its own, the one executable besides `aterm` that does: a static Linux build
//! per architecture, published as a release asset ([`pin::asset_name`]) and pinned in
//! install.sh by sha256 ([`pin::Pin`]), so the bootstrap needs neither Python nor OpenSSL
//! and runs no code it has not checked. Its trust anchor is compiled in; the test seam that
//! names another ([`TEST_MASTER_PUBKEYS`]) exists only under `cfg(any(test,
//! debug_assertions))`, so the release build has none.
//!
//! The command line, as install.sh calls it:
//!
//! - `aterm-bootstrap-verify --store-path <store>`: the pre-flight, before any download.
//! - `aterm-bootstrap-verify <proofs> <tag> <owner/repo> <arch> <store> [<candidate>]`:
//!   prints `name\tsha256\tsize\tbuild\troster_seq`.
//!
//! A refusal exits 1 with `install.sh: Linux verification refused: <why>` ([`REFUSED`]).

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

/// The paper master's Ed25519 public key, the root every roster is signed under. It is
/// `aterm_update_core::pins::PAPER_MASTER_PUBKEYS`, which the tests hold it to.
pub const MASTER_PUBKEYS: &[&str] = &["DtiLfpk0iUSrK1/LkyIVf+4C2eGjD2Myf4Sr/FCoMPQ="];

/// What every refusal's line starts with, the words install.sh's callers strip.
pub const REFUSED: &str = "install.sh: Linux verification refused: ";

/// The test seam: whitespace-separated base64 keys trusted in place of
/// [`MASTER_PUBKEYS`], so a test signs its own rosters. Read only by a build with debug
/// assertions (the tests' host build); the shipped release build trusts the paper master
/// alone.
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

/// The verifier's command line (`args` without the program name) at `now` (Unix seconds):
/// the line to print when it admits a release (none for `--store-path`), or why not.
///
/// # Errors
/// Every refusal install.sh's embedded verifier made, in its words.
#[cfg(unix)]
pub fn run(args: &[std::ffi::OsString], now: i64) -> Verdict<Option<String>> {
    chain::main(args, now, &master_pubkeys())
}

/// Off Unix there is no install.sh to verify for.
///
/// # Errors
/// Always.
#[cfg(not(unix))]
pub fn run(_args: &[std::ffi::OsString], _now: i64) -> Verdict<Option<String>> {
    Err(Refusal::new(
        "the bootstrap verifier runs on Linux, for tools/install.sh",
    ))
}
