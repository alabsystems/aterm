// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The person's records that this login's vault subscriber or vault server is off by choice,
//! which the jobs `aterm secrets` installs (`crate::service`, on Unix) write and every member
//! reads: a member with no such job (a Windows one) reads them as a Mac does, and finds none.

use std::path::{Path, PathBuf};

/// `~/.config/aterm/secrets`.
pub(crate) fn secrets_dir(home: &Path) -> PathBuf {
    home.join(".config").join("aterm").join("secrets")
}

/// The record that the person turned this login's subscriber off:
/// `~/.config/aterm/secrets/subscriber-off`. `agent off` writes it, and so does a `join` or
/// `create` with `--no-agent` where no subscriber runs; an install of the subscriber
/// (`service::install`) removes it. aterm's setup (System State, `aterm init`) reads it as the
/// person's choice: Sync shows off, and the files stay as last synced with no subscriber installed.
pub fn subscriber_off_path(home: &Path) -> PathBuf {
    secrets_dir(home).join("subscriber-off")
}

/// Whether the person turned this login's subscriber off ([`subscriber_off_path`]). A record
/// that cannot be read is no record.
pub fn subscriber_turned_off(home: &Path) -> bool {
    crate::fsx::kind(&subscriber_off_path(home)).is_ok_and(|kind| kind != crate::fsx::Kind::Missing)
}

/// The record that the person turned this login's vault server off:
/// `~/.config/aterm/secrets/server-off`. `server off` writes it, and an install of the server
/// (`service::install`) removes it, so automatic setup (`sync.steward.server.standby`) and the
/// mend's `service::move_held` leave the server off by choice.
pub fn server_off_path(home: &Path) -> PathBuf {
    secrets_dir(home).join("server-off")
}

/// Whether the person turned this login's vault server off ([`server_off_path`]). A record that
/// cannot be read is no record.
pub fn server_turned_off(home: &Path) -> bool {
    crate::fsx::kind(&server_off_path(home)).is_ok_and(|kind| kind != crate::fsx::Kind::Missing)
}
