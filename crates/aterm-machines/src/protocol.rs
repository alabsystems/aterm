// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wire between the controller and a host's forced-command dispatcher (design §2, §4.4).
//!
//! The fleet SSH key's `authorized_keys` line forces `<agent> machines agent ssh`, which reads
//! `SSH_ORIGINAL_COMMAND` (split on spaces, never through a shell) as one of:
//!
//! | command | runs as | stdin | stdout |
//! |---|---|---|---|
//! | `hello` | the admin | — | one [`Hello`] JSON line |
//! | `put <sha256>` | the admin | the artifact's bytes | `ok <sha256>` once the bytes hash to it |
//! | `apply` | root, via `sudo -n <agent> machines agent apply` | the signed envelope | [`Line`]s |
//! | `attach <run>` | the admin | — | [`Line`]s from the start of that run |
//! | `screen` | the admin | the viewer's bytes | screensharingd's bytes, from the host's `127.0.0.1:5900` |
//!
//! Anything else is refused with `refused: <why>` and exit status 2.

use crate::model::{HostReport, ItemReport};
use serde::{Deserialize, Serialize};

/// The agent's installed root (design §3.4).
pub const AGENT_ROOT: &str = crate::AGENT_ROOT;

/// The stable path the sudoers rule and the forced command name.
pub const AGENT_EXE: &str = "/Library/Management/aterm/bin/aterm";

/// The SSHSIG namespace every envelope is signed in.
pub const SIG_NAMESPACE: &str = "aterm-machines";

/// The `authorized_keys` options for the fleet key (design §3.1): `restrict` and the forced
/// command, nothing re-enabled. No forwarding of any kind: `port-forwarding` would admit
/// `-R`, a TCP or Unix-socket listener on the host that `permitopen` does not limit (measured
/// in the VM), and a session-less `-N` that never runs the forced command. Screen Sharing
/// travels as the `screen` request.
#[must_use]
pub fn authorized_keys_line(public_key: &str, comment: &str) -> String {
    format!(
        "restrict,command=\"{AGENT_EXE} machines agent ssh\" {} {comment}",
        public_key.trim()
    )
}

/// What `hello` answers: everything the controller needs before it signs anything, readable
/// without root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Hello {
    /// [`crate::SCHEMA`].
    pub schema: u32,
    /// The agent's version.
    pub agent_version: String,
    /// SHA-256 (hex) of the running agent executable: `apply` upgrades a host whose agent
    /// is not the controller's own build (design §4.5). Empty when it could not be read.
    #[serde(default)]
    pub agent_sha256: String,
    /// The item families this agent knows (`power`, `keyboard`, …).
    pub families: Vec<String>,
    /// IOPlatformUUID.
    pub uuid: String,
    /// IOPlatformSerialNumber.
    pub serial_number: String,
    /// `scutil --get ComputerName`.
    pub computer_name: String,
    /// `scutil --get LocalHostName`.
    pub local_host_name: String,
    /// `sw_vers -productVersion`.
    pub os_version: String,
    /// `sw_vers -buildVersion`.
    pub os_build: String,
    /// `uname -m`.
    pub arch: String,
    /// Who owns `/dev/console`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_user: Option<String>,
    /// The serial floor per signing principal.
    pub floors: Vec<Floor>,
    /// SHA-256 (hex) of each artifact in the agent's root cache.
    pub cached: Vec<String>,
    /// The run in progress, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<String>,
    /// The newest finished run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<String>,
}

/// One principal's serial floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Floor {
    /// The principal in `allowed_signers`.
    pub principal: String,
    /// The highest serial applied (or refused-after-verify) for it.
    pub serial: u64,
}

/// One line of an `apply` or `attach` stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Line {
    /// The run started: its id.
    Started {
        /// The run id.
        run: String,
    },
    /// One item finished.
    Item(ItemReport),
    /// The run finished: the whole report.
    Report(Box<HostReport>),
    /// The agent refused the envelope or the request.
    Refused {
        /// Why, one line.
        why: String,
    },
}
