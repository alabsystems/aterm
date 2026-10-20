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
    /// What the Mac is, in the parts `[names]` fills `{hardware}` with
    /// (`hardware::Description::parts`: `MacBook Air`, `M5`, `32GB`, `4TB`); empty when a
    /// fact could not be read, and from an agent older than the field. The controller finds a
    /// Mac its baseline renamed by it (`controller::naming::renamed_address`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hardware: Vec<String>,
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
    /// Live `aterm` sessions running on this host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<HostedSession>,
}

/// One live `aterm` tab/session reported in [`Hello::sessions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HostedSession {
    /// PID of the `aterm` instance hosting this session.
    pub instance_pid: u32,
    /// Local tab/session index within that `aterm` instance.
    pub local_id: u64,
    /// Globally unique session ID (`s-...`).
    pub sid: String,
    /// Session lifecycle state (`alive`, `idle`, `running`, …).
    pub state: String,
    /// Tab title (`user_title` when set, otherwise the terminal title).
    pub title: String,
    /// Window index (`0`, `1`, … or `none`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub window: String,
    /// Whether this tab is the active tab of its window.
    #[serde(default)]
    pub active: bool,
    /// Whether its window is focused.
    #[serde(default)]
    pub focused: bool,
    /// Foreground program (`claude`, `codex`, `agy`, `muse`, `gemini`, `zsh`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Live AI CLI status word (`working`, `waiting`, `idle`, `confirm`, `error`, `done`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Live AI CLI status detail (`permission`, `plan`, `question`, `thinking`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_detail: Option<String>,
    /// Milliseconds the AI CLI has been in its current state (`agent_since_ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_since_ms: Option<u64>,
    /// Milliseconds since a person last typed in this tab (`human_ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_ms: Option<u64>,
    /// Effective Autopilot mode (`off`, `safe`, `cruise`, `land`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot: Option<String>,
    /// Typed session role (`operator`, `worker`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Typed escalation/attention reason, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    /// Live supervisor holder, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<String>,
    /// Active task summary, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Terminal grid columns (`211`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u32>,
    /// Terminal grid rows (`70`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,
    /// Working directory (`~/aterm`, `$HOME/trust`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
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
