// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux arm's PARSERS — pure functions over `&str`, compiled and tested
//! on every platform (ruling 210).
//!
//! There is no live Linux sampler this round: nothing here opens a file, and
//! a Linux [`crate::Probe`] reads like every platform without one. When the
//! sampler lands it reads each file through [`crate::read_bounded`] (one
//! 4 KiB buffer) and hands the text to these functions, so every parser
//! tolerates a text cut at 4 KiB — a torn last line is ignored, and a fact
//! that was cut off reads `None`, which is never heavy.
//!
//! | file | parser | into |
//! |---|---|---|
//! | `/proc/stat` | [`cpu_ticks`] | `Reading::busy_ticks` |
//! | `/proc/pressure/{cpu,memory,io}` | [`psi`] | `Reading::psi` |
//! | `/proc/meminfo` | [`meminfo`] | `mem_mib`, `mem_used_pm` |
//! | `/proc/vmstat` | [`swap_pages`] | `Reading::swap_pages` |
//! | `thermal_zone*/temp`, `trip_point_*` | [`millidegrees`], [`zone_thermal`] | `Reading::thermal` |
//! | `/proc/<pid>/stat`, `statm` | [`pid_stat`], [`statm_resident`] | [`ProcRow`] |

use crate::{Instant, ProcRow, Psi, Reading, Thermal};

/// Complete lines only: a text cut mid-line (at the 4 KiB cap) loses its
/// last, torn line rather than parsing half a number.
fn whole_lines(text: &str) -> impl Iterator<Item = &str> {
    let end = if text.ends_with('\n') {
        text.len()
    } else {
        text.rfind('\n').map_or(0, |i| i + 1)
    };
    text[..end].lines()
}

/// `(busy, total)` jiffies from the aggregate `cpu ` line of `/proc/stat`:
/// busy is user + nice + system + irq + softirq + steal, total adds idle and
/// iowait. `guest`/`guest_nice` are already inside user/nice and are not
/// counted twice.
#[must_use]
pub fn cpu_ticks(stat: &str) -> Option<(u64, u64)> {
    let line = whole_lines(stat).find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if v.len() < 4 {
        return None;
    }
    let at = |i: usize| v.get(i).copied().unwrap_or(0);
    let busy = at(0) + at(1) + at(2) + at(5) + at(6) + at(7);
    Some((busy, busy + at(3) + at(4)))
}

/// A percentage with up to two decimals (`12.34`) as permille, rounded,
/// capped at 1000.
fn percent_to_pm(s: &str) -> Option<u16> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, "0"));
    let whole: u32 = whole.parse().ok()?;
    let mut hundredths: u32 = 0;
    for (i, ch) in frac.chars().take(2).enumerate() {
        let d = ch.to_digit(10)?;
        hundredths += d * if i == 0 { 10 } else { 1 };
    }
    let pm = (whole * 100 + hundredths + 5) / 10;
    Some(u16::try_from(pm.min(1000)).unwrap_or(1000))
}

/// `(some, full)` `avg10` of one `/proc/pressure/*` file, permille.
#[must_use]
pub fn psi(text: &str) -> (Option<u16>, Option<u16>) {
    let avg10 = |kind: &str| {
        whole_lines(text)
            .find(|l| l.split_whitespace().next() == Some(kind))?
            .split_whitespace()
            .find_map(|f| f.strip_prefix("avg10="))
            .and_then(percent_to_pm)
    };
    (avg10("some"), avg10("full"))
}

/// The engine's [`Psi`] from the three pressure files (any may be absent).
#[must_use]
pub fn psi_of(cpu: Option<&str>, memory: Option<&str>, io: Option<&str>) -> Option<Psi> {
    if cpu.is_none() && memory.is_none() && io.is_none() {
        return None;
    }
    let (cpu_some_pm, _) = cpu.map_or((None, None), psi);
    let (memory_some_pm, memory_full_pm) = memory.map_or((None, None), psi);
    let (_, io_full_pm) = io.map_or((None, None), psi);
    Some(Psi {
        cpu_some_pm,
        memory_some_pm,
        memory_full_pm,
        io_full_pm,
    })
}

/// `/proc/meminfo`'s two facts, KiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    /// `MemTotal`.
    pub total_kib: u64,
    /// `MemAvailable`.
    pub available_kib: u64,
}

impl MemInfo {
    /// Physical memory, MiB.
    #[must_use]
    pub fn mib(self) -> u32 {
        u32::try_from(self.total_kib / 1024).unwrap_or(u32::MAX)
    }

    /// Memory in use, MiB: total less available.
    #[must_use]
    pub fn used_mib(self) -> u32 {
        let used = self.total_kib.saturating_sub(self.available_kib);
        u32::try_from(used / 1024).unwrap_or(u32::MAX)
    }

    /// Memory in use, permille.
    #[must_use]
    pub fn used_pm(self) -> Option<u16> {
        if self.total_kib == 0 {
            return None;
        }
        let avail = self.available_kib.min(self.total_kib);
        u16::try_from(1000 - avail * 1000 / self.total_kib).ok()
    }
}

/// The value of a `Key:   123 kB` / `key 123` line.
fn keyed(text: &str, key: &str) -> Option<u64> {
    whole_lines(text).find_map(|l| {
        let mut it = l.split_whitespace();
        if it.next()?.trim_end_matches(':') != key {
            return None;
        }
        it.next()?.parse().ok()
    })
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`.
#[must_use]
pub fn meminfo(text: &str) -> Option<MemInfo> {
    Some(MemInfo {
        total_kib: keyed(text, "MemTotal")?,
        available_kib: keyed(text, "MemAvailable")?,
    })
}

/// `pswpin + pswpout` from `/proc/vmstat` (pages).
#[must_use]
pub fn swap_pages(vmstat: &str) -> Option<u64> {
    Some(keyed(vmstat, "pswpin")?.saturating_add(keyed(vmstat, "pswpout")?))
}

/// A thermal-zone `temp` or `trip_point_*_temp` file: millidegrees Celsius.
#[must_use]
pub fn millidegrees(text: &str) -> Option<i64> {
    text.trim().parse().ok()
}

/// One zone's state from its temperature and its trip points
/// `(type, millidegrees)`: past a `critical` or `hot` trip reads
/// [`Thermal::Critical`], past a `passive` trip [`Thermal::Serious`]
/// (the kernel is throttling), else [`Thermal::Nominal`].
#[must_use]
pub fn zone_thermal(temp_mc: i64, trips: &[(&str, i64)]) -> Thermal {
    let past = |kinds: &[&str]| {
        trips
            .iter()
            .any(|(t, at)| kinds.contains(&t.trim()) && *at > 0 && temp_mc >= *at)
    };
    if past(&["critical", "hot"]) {
        Thermal::Critical
    } else if past(&["passive"]) {
        Thermal::Serious
    } else {
        Thermal::Nominal
    }
}

/// `/proc/<pid>/stat`'s facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PidStat {
    /// Field 1.
    pub pid: u32,
    /// Field 2, without its parentheses (it may itself hold spaces and
    /// parentheses: everything to the LAST `)`).
    pub comm: String,
    /// Field 4.
    pub ppid: u32,
    /// Fields 14 + 15, `utime + stime`, in clock ticks.
    pub ticks: u64,
}

/// Parse `/proc/<pid>/stat`.
#[must_use]
pub fn pid_stat(text: &str) -> Option<PidStat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    let pid = text[..open].trim().parse().ok()?;
    let comm = text[open + 1..close].to_string();
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    // After the comm, field 3 (state) is index 0.
    let field = |n: usize| rest.get(n - 3).and_then(|s| s.parse::<u64>().ok());
    Some(PidStat {
        pid,
        comm,
        ppid: u32::try_from(field(4)?).ok()?,
        ticks: field(14)?.saturating_add(field(15)?),
    })
}

/// Resident pages from `/proc/<pid>/statm` (its second field).
#[must_use]
pub fn statm_resident(text: &str) -> Option<u64> {
    text.split_whitespace().nth(1)?.parse().ok()
}

/// Clock ticks (`_SC_CLK_TCK`, usually 100) to nanoseconds, saturating.
#[must_use]
pub fn ticks_ns(ticks: u64, clk_tck: u64) -> u64 {
    if clk_tck == 0 {
        return 0;
    }
    u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(clk_tck)).unwrap_or(u64::MAX)
}

/// One sweep row from a pid's `stat` and `statm`: cumulative CPU and the
/// resident set (`statm` × `page_kib`) as the footprint. `None` when `stat`
/// does not parse (the process went away mid-read).
#[must_use]
pub fn proc_row(
    stat: &str,
    statm: Option<&str>,
    clk_tck: u64,
    page_kib: u32,
    uid_is_ours: bool,
) -> Option<ProcRow> {
    let s = pid_stat(stat)?;
    Some(ProcRow {
        pid: s.pid,
        ppid: s.ppid,
        uid_is_ours,
        name: s.comm,
        bundle: None,
        cpu_ns: Some(ticks_ns(s.ticks, clk_tck)),
        footprint_kib: statm
            .and_then(statm_resident)
            .map(|p| p.saturating_mul(u64::from(page_kib))),
    })
}

/// The texts one Linux reading is made from; any may be missing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Texts<'a> {
    /// `/proc/stat`.
    pub stat: Option<&'a str>,
    /// `/proc/meminfo`.
    pub meminfo: Option<&'a str>,
    /// `/proc/vmstat`.
    pub vmstat: Option<&'a str>,
    /// `/proc/pressure/cpu`.
    pub psi_cpu: Option<&'a str>,
    /// `/proc/pressure/memory`.
    pub psi_memory: Option<&'a str>,
    /// `/proc/pressure/io`.
    pub psi_io: Option<&'a str>,
}

/// One [`Reading`] from the texts: the hottest zone's state (already folded
/// by the caller through [`zone_thermal`]) and the page size in KiB come
/// in beside them. Linux has no kernel pressure LEVEL (PSI stands in) and no
/// Low Power Mode.
#[must_use]
pub fn reading(
    at: Instant,
    cores: u16,
    page_kib: u32,
    texts: Texts<'_>,
    thermal: Option<Thermal>,
) -> Reading {
    let mem = texts.meminfo.and_then(meminfo);
    Reading {
        at,
        cores,
        mem_mib: mem.map_or(0, MemInfo::mib),
        busy_ticks: texts.stat.and_then(cpu_ticks),
        pressure: None,
        mem_used_pm: mem.and_then(MemInfo::used_pm),
        mem_used_mib: mem.map(MemInfo::used_mib),
        swap_pages: texts.vmstat.and_then(swap_pages),
        page_kib,
        thermal,
        low_power: None,
        psi: psi_of(texts.psi_cpu, texts.psi_memory, texts.psi_io),
    }
}
