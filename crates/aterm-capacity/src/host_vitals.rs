// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT ONE HOST SAYS OF ITS MACHINE ([`HostVitals`]): the figures `aterm machines`' `hello`
//! carries for each Mac and the Fleet Monitor shows, each read once in process from
//! aterm-sysprobe, its available memory as `sys mem` and this crate's admission read it
//! (`aterm_sysprobe::MemoryDetail::available`) and its home volume as admission places it
//! ([`crate::reading::volume_of`]). It lives
//! here, beside those readings, so both of its readers share one definition and one rendering
//! ([`HostVitals::summary`]): aterm-machines fills and prints it, and the window shows it without
//! depending on aterm-machines.

use serde::{Deserialize, Serialize};

/// One host's machine as `hello` reports it and `aterm machines monitor` samples it. Every
/// figure is `None` where it was not read, and is then left off the wire; a hello an older agent
/// wrote parses, keys this one no longer knows ignored and keys it lacks absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HostVitals {
    /// Logical cores: the CPU layout's (`aterm_sysprobe::cpu_layout`), absent where it reads
    /// none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    /// Load averages (`1m`, `5m`, `15m`) scaled by 100 (`142` = `1.42`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_avg_100: Option<[u32; 3]>,
    /// Accelerator / GPU model name (`"Apple M5 GPU"`, `"NVIDIA GB10"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_name: Option<String>,
    /// GPU core count, where the host reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_cores: Option<u32>,
    /// How busy the GPU is (`0..=100`): the accelerator's device utilization
    /// (`aterm_sysprobe::GpuReading::device_pct`, what [`crate::reading::Reading::gpu_busy`]
    /// reads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_busy_pct: Option<u8>,
    /// Total physical RAM in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_total_bytes: Option<u64>,
    /// Memory the kernel can hand out now without paging, bytes
    /// (`aterm_sysprobe::MemoryDetail::available`): what `sys mem` calls available and admission
    /// holds work to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_available_bytes: Option<u64>,
    /// Memory in use, bytes (`aterm_sysprobe::MemoryDetail::used`): app (shared included), wired
    /// and compressed, at most the total.
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
    /// Bytes free on that volume: what this account may still write (`f_bavail`,
    /// `aterm_sysprobe::volumes::Reading::free`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free_bytes: Option<u64>,
    /// Host uptime in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,
    /// Measured TCP/SSH round-trip latency in microseconds (`1_400` = `1.4 ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u64>,
}

impl HostVitals {
    /// Bytes used on the account home's volume: its size less what is free
    /// ([`Self::disk_free_bytes`]), so blocks the volume reserves from this account count as
    /// used, as `sys disk` counts a volume's used bytes
    /// ([`aterm_vitals::model::VolumeSample::used`]); `None` unless both were read.
    #[must_use]
    pub fn disk_used(&self) -> Option<u64> {
        Some(self.disk_total_bytes?.saturating_sub(self.disk_free_bytes?))
    }

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

    /// Format the core count and, where read, the load average (`18c · load 2.50/3.00/4.00`),
    /// or `unprobed` where neither was read.
    #[must_use]
    pub fn format_cpu(&self) -> String {
        if self.cpu_cores.is_none() && self.load_avg_100.is_none() {
            return "unprobed".to_owned();
        }
        let cores = self
            .cpu_cores
            .filter(|n| *n > 0)
            .map_or_else(|| "cores unknown".to_owned(), |n| format!("{n}c"));
        match self.load_avg_100 {
            Some([l1, l5, l15]) => format!(
                "{cores} \u{b7} load {:.2}/{:.2}/{:.2}",
                f64::from(l1) / 100.0,
                f64::from(l5) / 100.0,
                f64::from(l15) / 100.0
            ),
            None => cores,
        }
    }

    /// Format GPU utilization, model and core count.
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
        if parts.is_empty() {
            "-".to_owned()
        } else {
            parts.join(" \u{b7} ")
        }
    }

    /// Format RAM usage, percentage bar, available memory, pressure, wired/compressed, and swap.
    #[must_use]
    pub fn format_memory(&self) -> String {
        if !self.has_memory() {
            return "unprobed".to_owned();
        }
        let mut extra = Vec::new();
        if let Some(pct) = Self::used_percent(self.mem_used_bytes, self.mem_total_bytes) {
            extra.push(format!("{pct}% {}", Self::bar_8(pct)));
        }
        if let Some(a) = self.mem_available_bytes {
            extra.push(format!("{} available", Self::human_bytes(a)));
        }
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
            format!(" ({})", extra.join(" \u{b7} "))
        };
        format!(
            "{} used/{} total{suffix}",
            Self::observed_bytes(self.mem_used_bytes),
            Self::observed_bytes(self.mem_total_bytes),
        )
    }

    /// Format the account home volume's capacity, used % and free space.
    #[must_use]
    pub fn format_disk(&self) -> String {
        if !self.has_disk() {
            return "unprobed".to_owned();
        }
        let used = self.disk_used();
        let share = Self::used_percent(used, self.disk_total_bytes)
            .map(|pct| format!("{pct}% \u{b7} "))
            .unwrap_or_default();
        format!(
            "{} used/{} total ({share}{} free)",
            Self::observed_bytes(used),
            Self::observed_bytes(self.disk_total_bytes),
            Self::observed_bytes(self.disk_free_bytes)
        )
    }

    /// `bytes` in [`Self::human_bytes`]' words, `unknown` where unread.
    #[must_use]
    pub fn observed_bytes(bytes: Option<u64>) -> String {
        bytes.map_or_else(|| "unknown".to_owned(), Self::human_bytes)
    }

    /// The share of `total` that `used` is, whole percent, at most 100: only where both were
    /// read and the total is positive.
    #[must_use]
    pub fn used_percent(used: Option<u64>, total: Option<u64>) -> Option<u8> {
        let total = total.filter(|n| *n > 0)?;
        Some((u128::from(used?) * 100 / u128::from(total)).min(100) as u8)
    }

    fn has_memory(&self) -> bool {
        self.mem_total_bytes.is_some()
            || self.mem_available_bytes.is_some()
            || self.mem_used_bytes.is_some()
            || self.mem_wired_bytes.is_some()
            || self.mem_compressed_bytes.is_some()
            || self.swap_used_bytes.is_some()
            || self.mem_pressure.is_some()
    }

    fn has_disk(&self) -> bool {
        self.disk_total_bytes.is_some() || self.disk_free_bytes.is_some()
    }

    /// Format a single-line comprehensive system monitor summary for a computer row
    /// ([`Self::summary`]), `telemetry unprobed` where nothing was read.
    #[must_use]
    pub fn format_summary_line(&self) -> String {
        self.summary()
            .unwrap_or_else(|| "telemetry unprobed".to_owned())
    }

    /// THE ONE LINE A MACHINE'S VITALS READ AS, wherever they are shown (`aterm machines`, the
    /// Fleet Monitor's rows): its CPU, GPU, memory, home volume, uptime and round trip, each
    /// part only where it was read; `None` where nothing was.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.cpu_cores.is_some() || self.load_avg_100.is_some() {
            parts.push(format!("CPU {}", self.format_cpu()));
        }
        let gpu = self.format_gpu();
        if gpu != "-" {
            parts.push(format!("GPU {gpu}"));
        }
        if self.has_memory() {
            parts.push(format!("RAM {}", self.format_memory()));
        }
        if self.has_disk() {
            parts.push(format!("Disk {}", self.format_disk()));
        }
        if let Some(up) = self.uptime_secs.filter(|u| *u > 0) {
            parts.push(format!("up {}", Self::format_uptime(up)));
        }
        if let Some(rtt) = self.rtt_us.filter(|r| *r > 0) {
            parts.push(format!("RTT {:.1} ms", (rtt as f64) / 1000.0));
        }
        (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vitals_omit_unread_values_on_the_wire_and_keep_a_measured_zero() {
        let wire = r#"{"cpu_cores":18,"mem_total_bytes":17179869184,"mem_used_bytes":0,"disk_total_bytes":1024000,"disk_free_bytes":204800}"#;
        let v: HostVitals = aterm_json::from_str(wire).unwrap();
        assert_eq!(v.cpu_cores, Some(18));
        assert_eq!(v.mem_total_bytes, Some(16 << 30));
        assert_eq!(v.mem_used_bytes, Some(0));
        assert_eq!(v.disk_total_bytes, Some(1_024_000));
        assert_eq!(v.disk_free_bytes, Some(204_800));
        assert_eq!(v.gpu_cores, None);
        let json = aterm_json::to_value(&v).unwrap();
        assert_eq!(json.get("mem_used_bytes").and_then(|v| v.as_u64()), Some(0));
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
            "mem_available_bytes",
            "mem_used_bytes",
            "disk_total_bytes",
            "disk_free_bytes",
        ] {
            assert!(absent.get(field).is_none(), "{field}: {absent}");
        }
    }

    /// LAW: the disk's used bytes are its size less what is free (`f_bavail`), so the blocks a
    /// volume reserves from this account count as used: what `sys disk` reads as used on the
    /// same volume ([`aterm_vitals::model::VolumeSample::used`]). CONTROL: `df`'s Used column
    /// (size less `f_bfree`) on the same volume is less, so a used figure that left the reserve
    /// out fails.
    #[test]
    fn disk_used_is_the_size_less_what_this_account_may_write() {
        let total = 1_000_000_u64;
        let f_bavail = 200_000_u64;
        let f_bfree = 250_000_u64;
        let v = HostVitals {
            disk_total_bytes: Some(total),
            disk_free_bytes: Some(f_bavail),
            ..HostVitals::default()
        };
        assert_eq!(v.disk_used(), Some(total - f_bavail));
        let sys_disk = aterm_vitals::model::VolumeSample {
            total,
            free: f_bavail,
            reserved: f_bfree - f_bavail,
            mount: "/".into(),
        };
        assert_eq!(v.disk_used(), Some(sys_disk.used()), "as sys disk reads it");
        assert_ne!(
            v.disk_used(),
            Some(total - f_bfree),
            "CONTROL: df's Used counts reserved blocks as free"
        );
        assert!(
            v.format_disk().contains("(80% \u{b7} "),
            "{}",
            v.format_disk()
        );
        let past = HostVitals {
            disk_free_bytes: Some(total + 1),
            ..v.clone()
        };
        assert_eq!(past.disk_used(), Some(0), "never below nothing");
        let free_only = HostVitals {
            disk_free_bytes: Some(f_bavail),
            ..HostVitals::default()
        };
        assert_eq!(free_only.disk_used(), None);
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
        assert_eq!(sparse.format_cpu(), "18c \u{b7} load 2.50/3.00/4.00");
        assert!(
            sparse
                .format_memory()
                .contains("unknown used/16.0 GiB total")
        );
        assert!(!sparse.format_memory().contains('%'));
        assert!(sparse.format_disk().contains("unknown used/1.00 TiB total"));
        assert!(sparse.format_disk().contains("unknown free"));
        assert!(!sparse.format_disk().contains('%'));
        assert_eq!(sparse.format_gpu(), "40 cores");
        let zero = HostVitals {
            gpu_busy_pct: Some(0),
            mem_used_bytes: Some(0),
            mem_available_bytes: Some(0),
            disk_free_bytes: Some(1 << 40),
            ..sparse.clone()
        };
        assert!(zero.format_gpu().contains("0%"));
        assert!(
            zero.format_memory()
                .contains("0 MiB used/16.0 GiB total (0%")
        );
        assert!(zero.format_memory().contains("0 MiB available"));
        assert!(zero.format_disk().contains("0 MiB used/1.00 TiB total (0%"));
        assert!(zero.format_disk().contains("1.00 TiB free"));
        let usage_only = HostVitals {
            mem_used_bytes: Some(1 << 30),
            disk_free_bytes: Some(3 << 30),
            ..Default::default()
        };
        let line = usage_only.format_summary_line();
        assert!(line.contains("1.0 GiB used/unknown total"), "{line}");
        assert!(line.contains("unknown used/unknown total"), "{line}");
        assert!(line.contains("3.0 GiB free"), "{line}");
        assert!(!line.contains('%'), "{line}");
        assert_eq!(HostVitals::default().format_cpu(), "unprobed");
        assert_eq!(
            HostVitals {
                cpu_cores: Some(18),
                ..Default::default()
            }
            .format_cpu(),
            "18c"
        );
        assert_eq!(
            HostVitals {
                load_avg_100: Some([100, 0, 0]),
                ..Default::default()
            }
            .format_cpu(),
            "cores unknown \u{b7} load 1.00/0.00/0.00"
        );
        let large_rtt = HostVitals {
            rtt_us: Some(5_000_000_000),
            ..Default::default()
        };
        assert_eq!(large_rtt.format_summary_line(), "RTT 5000000.0 ms");
        assert_eq!(HostVitals::default().summary(), None);
        assert_eq!(
            HostVitals::default().format_summary_line(),
            "telemetry unprobed"
        );
    }

    #[test]
    fn a_share_needs_a_measured_numerator_and_a_positive_capacity() {
        assert_eq!(HostVitals::used_percent(None, Some(100)), None);
        assert_eq!(HostVitals::used_percent(Some(0), None), None);
        assert_eq!(HostVitals::used_percent(Some(0), Some(0)), None);
        assert_eq!(HostVitals::used_percent(Some(0), Some(100)), Some(0));
        assert_eq!(
            HostVitals::used_percent(Some(u64::MAX / 2), Some(u64::MAX)),
            Some(49)
        );
        assert_eq!(
            HostVitals::used_percent(Some(u64::MAX), Some(u64::MAX)),
            Some(100)
        );
        assert_eq!(HostVitals::used_percent(Some(u64::MAX), Some(1)), Some(100));
        assert_eq!(HostVitals::observed_bytes(None), "unknown");
        assert_eq!(HostVitals::observed_bytes(Some(0)), "0 MiB");
    }
}
