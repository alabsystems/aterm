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
    /// Live `aterm` sessions running on this host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<HostedSession>,
    /// Live system telemetry snapshot (`CPU`, `GPU`, `RAM`, `Disk`, `Uptime`, `RTT`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vitals: Option<HostVitals>,
}

/// Live system telemetry snapshot reported in [`Hello::vitals`] or sampled by `aterm machines
/// monitor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HostVitals {
    /// Logical CPU core count (`hw.ncpu` / `nproc`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    /// Whole-machine CPU busy percentage (`0..=100`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_busy_pct: Option<u8>,
    /// Load averages (`1m`, `5m`, `15m`) scaled by 100 (`142` = `1.42`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_avg_100: Option<[u32; 3]>,
    /// Accelerator / GPU model name (`"Apple M5 GPU"`, `"NVIDIA GB10"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_name: Option<String>,
    /// GPU core count, where the host reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_cores: Option<u32>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_total_bytes: Option<u64>,
    /// Used RAM in bytes, where the reader measured its classes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_used_bytes: Option<u64>,
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
    /// Capacity of the volume holding this account's home, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_total_bytes: Option<u64>,
    /// Occupied bytes on the account home volume, independent of writable available space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_used_bytes: Option<u64>,
    /// Writable available bytes on the account home volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_avail_bytes: Option<u64>,
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
        if bytes == 0 {
            return "0 MiB".to_owned();
        }
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
        let pct = self.cpu_busy_pct.map(|p| p.min(100));
        let cores = self
            .cpu_cores
            .filter(|n| *n > 0)
            .map_or_else(|| "cores unknown".to_owned(), |n| format!("{n}c"));
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
            None if self.cpu_cores.is_some() || self.load_avg_100.is_some() => {
                format!("utilization unavailable ({cores} \u{b7} load {load})")
            }
            None => "unprobed".to_owned(),
        }
    }

    /// Format GPU utilization, model, temperature, and power.
    #[must_use]
    pub fn format_gpu(&self) -> String {
        let mut parts = Vec::new();
        if let Some(pct) = self.gpu_busy_pct.map(|p| p.min(100)) {
            parts.push(format!("{pct}% {}", Self::bar_8(pct)));
        }
        if let Some(name) = self.gpu_name.as_deref().filter(|s| !s.is_empty()) {
            parts.push(name.to_owned());
        }
        if let Some(cores) = self.gpu_cores.filter(|n| *n > 0) {
            parts.push(format!("{cores} cores"));
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
        if !self.has_memory() {
            return "unprobed".to_owned();
        }
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
        if let Some(pct) = Self::used_percent(self.mem_used_bytes, self.mem_total_bytes) {
            extra.insert(0, format!("{pct}% {}", Self::bar_8(pct)));
        }
        let suffix = if extra.is_empty() {
            String::new()
        } else {
            format!(" ({})", extra.join(" \u{b7} "))
        };
        format!(
            "{} used/{} total{suffix}",
            Self::observed_bytes(self.mem_used_bytes),
            Self::observed_bytes(self.mem_total_bytes),
        )
    }

    /// Format account home volume capacity, used %, and available space.
    #[must_use]
    pub fn format_disk(&self) -> String {
        if self.disk_total_bytes.is_none()
            && self.disk_used_bytes.is_none()
            && self.disk_avail_bytes.is_none()
        {
            return "unprobed".to_owned();
        }
        let share = Self::used_percent(self.disk_used_bytes, self.disk_total_bytes)
            .map(|pct| format!("{pct}% \u{b7} "))
            .unwrap_or_default();
        format!(
            "{} used/{} total ({share}{} available)",
            Self::observed_bytes(self.disk_used_bytes),
            Self::observed_bytes(self.disk_total_bytes),
            Self::observed_bytes(self.disk_avail_bytes)
        )
    }

    fn observed_bytes(bytes: Option<u64>) -> String {
        bytes.map_or_else(|| "unknown".to_owned(), Self::human_bytes)
    }

    fn used_percent(used: Option<u64>, total: Option<u64>) -> Option<u8> {
        let total = total.filter(|n| *n > 0)?;
        Some((u128::from(used?) * 100 / u128::from(total)).min(100) as u8)
    }

    fn has_memory(&self) -> bool {
        self.mem_total_bytes.is_some()
            || self.mem_used_bytes.is_some()
            || self.mem_wired_bytes.is_some()
            || self.mem_compressed_bytes.is_some()
            || self.swap_used_bytes.is_some()
            || self.mem_pressure.is_some()
    }

    /// Format a single-line comprehensive system monitor summary for a computer row.
    #[must_use]
    pub fn format_summary_line(&self) -> String {
        let mut parts = Vec::new();
        if self.cpu_cores.is_some() || self.cpu_busy_pct.is_some() || self.load_avg_100.is_some() {
            parts.push(format!("CPU {}", self.format_cpu()));
        }
        let gpu = self.format_gpu();
        if gpu != "-" {
            parts.push(format!("GPU {gpu}"));
        }
        if self.has_memory() {
            parts.push(format!("RAM {}", self.format_memory()));
        }
        if self.disk_total_bytes.is_some()
            || self.disk_used_bytes.is_some()
            || self.disk_avail_bytes.is_some()
        {
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
mod tests {
    use super::*;

    #[test]
    fn vitals_accept_legacy_numbers_and_omit_unread_values_on_the_wire() {
        let old = r#"{"cpu_cores":18,"mem_total_bytes":17179869184,"mem_used_bytes":0,"disk_total_bytes":1024000,"disk_used_bytes":614400,"disk_avail_bytes":204800}"#;
        let v: HostVitals = aterm_json::from_str(old).unwrap();
        assert_eq!(v.cpu_cores, Some(18));
        assert_eq!(v.mem_total_bytes, Some(16 << 30));
        assert_eq!(v.mem_used_bytes, Some(0));
        assert_eq!(v.disk_total_bytes, Some(1_024_000));
        assert_eq!(v.disk_used_bytes, Some(614_400));
        assert_eq!(v.disk_avail_bytes, Some(204_800));
        assert_eq!(v.gpu_cores, None);
        let json = aterm_json::to_value(&v).unwrap();
        assert_eq!(json.get("mem_used_bytes").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(
            json.get("disk_used_bytes").and_then(|v| v.as_u64()),
            Some(614_400)
        );
        assert!(json.get("gpu_cores").is_none());
        assert_eq!(
            aterm_json::from_str::<HostVitals>("{}").unwrap(),
            HostVitals::default()
        );
        assert_eq!(
            aterm_json::from_str::<HostVitals>(r#"{"mem_used_bytes":null}"#)
                .unwrap()
                .mem_used_bytes,
            None
        );
        let absent = aterm_json::to_value(HostVitals::default()).unwrap();
        for field in [
            "cpu_cores",
            "mem_total_bytes",
            "mem_used_bytes",
            "disk_total_bytes",
            "disk_used_bytes",
            "disk_avail_bytes",
        ] {
            assert!(absent.get(field).is_none(), "{field}: {absent}");
        }
    }

    #[test]
    fn vitals_format_partial_and_zero_readings_without_inventing_capacity() {
        let sparse = HostVitals {
            cpu_cores: Some(18),
            load_avg_100: Some([250, 300, 400]),
            mem_total_bytes: Some(16 << 30),
            disk_total_bytes: Some(1 << 40),
            gpu_cores: Some(40),
            ..Default::default()
        };
        assert!(!sparse.format_cpu().contains('%'));
        assert!(sparse.format_cpu().contains("18c"));
        assert!(sparse.format_cpu().contains("load 2.50/3.00/4.00"));
        assert!(
            sparse
                .format_memory()
                .contains("unknown used/16.0 GiB total")
        );
        assert!(!sparse.format_memory().contains('%'));
        assert!(sparse.format_disk().contains("unknown used/1.00 TiB total"));
        assert!(sparse.format_disk().contains("unknown available"));
        assert!(!sparse.format_disk().contains('%'));
        assert_eq!(sparse.format_gpu(), "40 cores");
        let zero = HostVitals {
            cpu_busy_pct: Some(0),
            gpu_busy_pct: Some(0),
            mem_used_bytes: Some(0),
            disk_used_bytes: Some(0),
            disk_avail_bytes: Some(0),
            ..sparse
        };
        assert!(zero.format_cpu().contains("0%"));
        assert!(zero.format_gpu().contains("0%"));
        assert!(
            zero.format_memory()
                .contains("0 MiB used/16.0 GiB total (0%")
        );
        assert!(zero.format_disk().contains("0 MiB used/1.00 TiB total (0%"));
        assert!(zero.format_disk().contains("0 MiB available"));
        let usage_only = HostVitals {
            mem_used_bytes: Some(1 << 30),
            disk_used_bytes: Some(2 << 30),
            disk_avail_bytes: Some(3 << 30),
            ..Default::default()
        };
        let line = usage_only.format_summary_line();
        assert!(line.contains("1.0 GiB used/unknown total"), "{line}");
        assert!(line.contains("2.0 GiB used/unknown total"), "{line}");
        assert!(line.contains("3.0 GiB available"), "{line}");
        assert!(!line.contains('%'), "{line}");
    }
}
