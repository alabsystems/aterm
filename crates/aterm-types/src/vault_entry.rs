// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Where the vault's subscriber writes a plain entry, and the two GitHub token entries, named
//! once. aterm-secrets classes and materializes the entries (`entry_class`,
//! `materialize::target_path`), and agh adopts the two tokens; neither links the other, so the
//! names live here, below both.

use std::path::{Path, PathBuf};

/// The folder under the home directory a plain (not `~/`) entry name lands in.
pub const SECRETS_DIR: &str = ".secrets";

/// The development GitHub token: agh's `dev` profile, the `gh:dev` want.
pub const DEV_GH_TOKEN: &str = "gh_access_token";

/// The release channel's GitHub token, the public owner's writer: agh's `release` profile, the
/// `gh:release` want.
pub const RELEASE_GH_TOKEN: &str = "gh_access_token_alabsystems";

/// Where plain entry `name` lands under `home`: `home/.secrets/name`. It does not judge the
/// name; `aterm_secrets::materialize::target_path` does, and lands a plain name here.
#[must_use]
pub fn plain_entry_path(home: &Path, name: &str) -> PathBuf {
    home.join(SECRETS_DIR).join(name)
}

/// [`plain_entry_path`] as `~/.secrets/name`, the spelling the secrets registry's pointer gives,
/// with `/` on every platform.
#[must_use]
pub fn plain_entry_pointer(name: &str) -> String {
    format!("~/{SECRETS_DIR}/{name}")
}

#[cfg(test)]
#[path = "vault_entry_tests.rs"]
mod tests;
