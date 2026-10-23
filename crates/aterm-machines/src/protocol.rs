// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wire between the controller and a host's forced-command dispatcher.
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
    /// Live `aterm` sessions running on this host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<HostedSession>,
    /// Live system telemetry snapshot (`CPU`, `GPU`, `RAM`, `Disk`, `Uptime`, `RTT`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vitals: Option<HostVitals>,
}

/// Live system telemetry snapshot reported in [`Hello::vitals`] or sampled by `aterm machines monitor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HostVitals {
    /// Logical CPU core count (`hw.ncpu` / `nproc`).
    #[serde(default)]
    pub cpu_cores: u32,
    /// Whole-machine CPU busy percentage (`0..=100`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_busy_pct: Option<u8>,
    /// Load averages (`1m`, `5m`, `15m`) scaled by 100 (`142` = `1.42`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_avg_100: Option<[u32; 3]>,
    /// Accelerator / GPU model name (`"Apple M5 GPU"`, `"NVIDIA GB10"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_name: Option<String>,
    /// Accelerator / GPU device busy percentage (`0..=100`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_busy_pct: Option<u8>,
    /// Accelerator / GPU temperature in Celsius (`nvidia-smi` / SMC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_temp_c: Option<u16>,
    /// Accelerator / GPU power draw in tenths of a Watt (`34` = `3.4 W`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_power_deciwatts: Option<u32>,
    /// Total physical RAM in bytes.
    #[serde(default)]
    pub mem_total_bytes: u64,
    /// Used RAM in bytes (`total - available`).
    #[serde(default)]
    pub mem_used_bytes: u64,
    /// Wired kernel RAM in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_wired_bytes: Option<u64>,
    /// Compressed RAM in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_compressed_bytes: Option<u64>,
    /// Swap used in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_used_bytes: Option<u64>,
    /// Memory pressure word (`"normal"`, `"warn"`, `"critical"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_pressure: Option<String>,
    /// Root volume total capacity in bytes.
    #[serde(default)]
    pub disk_total_bytes: u64,
    /// Root volume used capacity in bytes.
    #[serde(default)]
    pub disk_used_bytes: u64,
    /// Root volume available capacity in bytes.
    #[serde(default)]
    pub disk_avail_bytes: u64,
    /// Host uptime in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,
    /// Measured TCP/SSH round-trip latency in microseconds (`1_400` = `1.4 ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u64>,
}

impl HostVitals {
    /// Format a compact 8-block bar (`▕███░░░░░▏`) for a `0..=100` percentage.
    #[must_use]
    pub fn bar_8(pct: u8) -> String {
        let filled = ((u16::from(pct.min(100)) * 8 + 50) / 100) as usize;
        let mut s = String::with_capacity(30);
        s.push('\u{2595}');
        for i in 0..8 {
            s.push(if i < filled { '\u{2588}' } else { '\u{2591}' });
        }
        s.push('\u{258f}');
        s
    }

    /// Format bytes in human-readable GiB/TiB/MiB.
    #[must_use]
    pub fn human_bytes(bytes: u64) -> String {
        const MIB: f64 = 1024.0 * 1024.0;
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;
        let b = bytes as f64;
        if b >= TIB {
            format!("{:.2} TiB", b / TIB)
        } else if b >= 100.0 * GIB {
            format!("{:.0} GiB", b / GIB)
        } else if b >= GIB {
            format!("{:.1} GiB", b / GIB)
        } else {
            format!("{:.0} MiB", (b / MIB).max(1.0))
        }
    }

    /// Format uptime seconds (`"4d 11h"`, `"3h 22m"`, `"45s"`).
    #[must_use]
    pub fn format_uptime(secs: u64) -> String {
        let days = secs / 86_400;
        let hours = (secs % 86_400) / 3_600;
        let mins = (secs % 3_600) / 60;
        if days > 0 {
            format!("{days}d {hours}h")
        } else if hours > 0 {
            format!("{hours}h {mins}m")
        } else if mins > 0 {
            format!("{mins}m")
        } else {
            format!("{secs}s")
        }
    }

    /// Format CPU busy % + core count + load average.
    #[must_use]
    pub fn format_cpu(&self) -> String {
        let pct = self.cpu_busy_pct.or_else(|| {
            let load1 = self.load_avg_100?[0];
            let cores = self.cpu_cores.max(1);
            Some((load1.saturating_mul(100) / (cores * 100)).min(100) as u8)
        });
        let cores = if self.cpu_cores > 0 {
            format!("{}c", self.cpu_cores)
        } else {
            "-".to_owned()
        };
        let load = self.load_avg_100.map_or_else(
            || "-".to_owned(),
            |[l1, l5, l15]| {
                format!(
                    "{:.2}/{:.2}/{:.2}",
                    f64::from(l1) / 100.0,
                    f64::from(l5) / 100.0,
                    f64::from(l15) / 100.0
                )
            },
        );
        match pct {
            Some(p) => format!("{p:>2}% {} ({cores} \u{b7} load {load})", Self::bar_8(p)),
            None if self.cpu_cores > 0 => format!("{cores} \u{b7} load {load}"),
            None => "unprobed".to_owned(),
        }
    }

    /// Format GPU utilization, model, temperature, and power.
    #[must_use]
    pub fn format_gpu(&self) -> String {
        let mut parts = Vec::new();
        if let Some(pct) = self.gpu_busy_pct {
            parts.push(format!("{pct}% {}", Self::bar_8(pct)));
        }
        if let Some(name) = self.gpu_name.as_deref().filter(|s| !s.is_empty()) {
            parts.push(name.to_owned());
        }
        if let Some(temp) = self.gpu_temp_c {
            parts.push(format!("{temp}\u{b0}C"));
        }
        if let Some(dw) = self.gpu_power_deciwatts {
            parts.push(format!("{:.1}W", f64::from(dw) / 10.0));
        }
        if parts.is_empty() {
            "-".to_owned()
        } else {
            parts.join(" \u{b7} ")
        }
    }

    /// Format RAM usage, percentage bar, pressure, wired/compressed, and swap.
    #[must_use]
    pub fn format_memory(&self) -> String {
        if self.mem_total_bytes == 0 {
            return "unprobed".to_owned();
        }
        if self.mem_used_bytes == 0
            && self.mem_wired_bytes.is_none()
            && self.mem_pressure.is_none()
            && self.cpu_busy_pct.is_none()
            && self.load_avg_100.is_none()
        {
            return format!("{} total", Self::human_bytes(self.mem_total_bytes));
        }
        let pct = ((self.mem_used_bytes.saturating_mul(100)) / self.mem_total_bytes.max(1)).min(100)
            as u8;
        let mut extra = Vec::new();
        if let Some(p) = self.mem_pressure.as_deref().filter(|s| !s.is_empty()) {
            extra.push(p.to_owned());
        }
        if let Some(w) = self.mem_wired_bytes.filter(|w| *w > 0) {
            extra.push(format!("wired {}", Self::human_bytes(w)));
        }
        if let Some(c) = self.mem_compressed_bytes.filter(|c| *c > 0) {
            extra.push(format!("comp {}", Self::human_bytes(c)));
        }
        if let Some(sw) = self.swap_used_bytes.filter(|sw| *sw > 0) {
            extra.push(format!("swap {}", Self::human_bytes(sw)));
        }
        let suffix = if extra.is_empty() {
            String::new()
        } else {
            format!(" \u{b7} {}", extra.join(" \u{b7} "))
        };
        format!(
            "{}/{} ({pct}% {}{suffix})",
            Self::human_bytes(self.mem_used_bytes),
            Self::human_bytes(self.mem_total_bytes),
            Self::bar_8(pct),
        )
    }

    /// Format root disk capacity, used %, and available space.
    #[must_use]
    pub fn format_disk(&self) -> String {
        if self.disk_total_bytes == 0 {
            return "unprobed".to_owned();
        }
        if self.disk_used_bytes == 0 && self.disk_avail_bytes == 0 {
            return format!("{} total", Self::human_bytes(self.disk_total_bytes));
        }
        let pct = ((self.disk_used_bytes.saturating_mul(100)) / self.disk_total_bytes.max(1))
            .min(100) as u8;
        format!(
            "{}/{} ({pct}% \u{b7} {} free)",
            Self::human_bytes(self.disk_used_bytes),
            Self::human_bytes(self.disk_total_bytes),
            Self::human_bytes(self.disk_avail_bytes)
        )
    }

    /// Format a single-line comprehensive system monitor summary for a computer row.
    #[must_use]
    pub fn format_summary_line(&self) -> String {
        let mut parts = Vec::new();
        if self.cpu_cores > 0 || self.cpu_busy_pct.is_some() || self.load_avg_100.is_some() {
            parts.push(format!("CPU {}", self.format_cpu()));
        }
        let gpu = self.format_gpu();
        if gpu != "-" {
            parts.push(format!("GPU {gpu}"));
        }
        if self.mem_total_bytes > 0 {
            parts.push(format!("RAM {}", self.format_memory()));
        }
        if self.disk_total_bytes > 0 {
            parts.push(format!("Disk {}", self.format_disk()));
        }
        if let Some(up) = self.uptime_secs.filter(|u| *u > 0) {
            parts.push(format!("up {}", Self::format_uptime(up)));
        }
        if let Some(rtt) = self.rtt_us.filter(|r| *r > 0) {
            parts.push(format!("RTT {:.1} ms", f64::from(rtt as u32) / 1000.0));
        }
        if parts.is_empty() {
            "telemetry unprobed".to_owned()
        } else {
            parts.join(" \u{b7} ")
        }
    }
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
    /// One item finished (boxed as the report is: a row is the largest line by far).
    Item(Box<ItemReport>),
    /// The run finished: the whole report.
    Report(Box<HostReport>),
    /// The agent refused the envelope or the request.
    Refused {
        /// Why, one line.
        why: String,
    },
}
