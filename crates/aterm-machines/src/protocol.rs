// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wire protocol between the controller and a host's forced-command dispatcher ([`Hello`], [`Line`]).

use crate::model::{HostReport, ItemReport};
use serde::{Deserialize, Serialize};

/// The agent's installed root.
pub const AGENT_ROOT: &str = crate::AGENT_ROOT;

/// The stable path the sudoers rule and the forced command name.
pub const AGENT_EXE: &str = "/Library/Management/aterm/bin/aterm";

/// The SSHSIG namespace every envelope is signed in.
pub const SIG_NAMESPACE: &str = "aterm-machines";

/// Format the `authorized_keys` line for the fleet key with `restrict` and the forced command.
#[must_use]
pub fn authorized_keys_line(public_key: &str, comment: &str) -> String {
    format!(
        "restrict,command=\"{AGENT_EXE} machines agent ssh\" {} {comment}",
        public_key.trim()
    )
}

/// Host state returned by `hello`, readable without root before signing an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Hello {
    /// [`crate::SCHEMA`].
    pub schema: u32,
    /// The agent's version.
    pub agent_version: String,
    /// SHA-256 (hex) of the running agent executable, or empty when unreadable.
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
    /// Hardware description parts (`MacBook Air`, `M5`, `32GB`, `4TB`) used to fill `{hardware}`.
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
    /// The observed sessions and whether their inventory was read completely.
    #[serde(flatten)]
    pub sessions: SessionSnapshot,
    /// This host's machine now (cores and load, GPU, memory, the home volume, uptime), read in
    /// the agent's process through capacity's reading ([`crate::facts::gather_vitals`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vitals: Option<aterm_capacity::host_vitals::HostVitals>,
}

/// Session rows together with whether every instance answered completely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionSnapshot {
    /// Every live session actually observed.
    #[serde(rename = "sessions", default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<HostedSession>,
    /// True only when every enumerated instance supplied its entire response.
    /// Missing observation metadata means unknown, never a complete empty inventory.
    #[serde(rename = "sessions_complete", default)]
    pub complete: bool,
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
    /// Foreground program (`claude`, `codex`, `agy`, `muse`, `zsh`, …).
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
    /// What the program says about itself (OSC 7501, `program_state=<state>[:<kind>]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_state: Option<String>,
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
    /// The run started.
    Started {
        /// The run id.
        run: String,
    },
    /// One item finished (boxed as the report is: a row is the largest line by far).
    Item(Box<ItemReport>),
    /// The run finished.
    Report(Box<HostReport>),
    /// The agent refused the envelope or the request.
    Refused {
        /// Why, one line.
        why: String,
    },
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
