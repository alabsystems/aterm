// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux arm's PARSERS — pure functions over `&str` and `&[u8]`,
//! compiled and tested on every platform. A Linux build reads each file
//! through [`crate::read_bounded`] (one 4 KiB buffer) and hands it to these
//! functions, so every parser tolerates a text cut at 4 KiB — a torn last
//! line is ignored, and a fact that was cut off reads `None`.
//!
//! | file | parser | into |
//! |---|---|---|
//! | `/proc/meminfo` | [`meminfo`] | [`crate::physical_memory_mib`], [`crate::machine_memory`] |
//! | `/proc/vmstat` | [`swap_pages`] | [`crate::swapped_kib`] |
//! | `/proc/self/auxv` | [`auxv_value`] | the page size |
//! | `/proc/<pid>/stat` | [`stat_pgrp`] | group-field parser; no complete live group reader here |
//! | `/proc/stat` | [`proc_stat_cores`] through [`StatReader`] | [`crate::Probe::core_ticks`] |
//! | `/proc/meminfo` | [`memory_detail`] | [`crate::memory_detail`] |
//! | `/sys/devices/system/cpu/present` | [`cpulist`] | [`crate::cpu_layout`] |
//!
//! `/proc/stat` is the one file read past 4 KiB: its per-core lines take
//! about 80 bytes each, so [`StatReader`] reads it into a [`STAT_CAP`]-byte
//! buffer it keeps, and [`proc_stat_cores`] refuses a text cut inside them.

use std::path::{Path, PathBuf};

use crate::{CoreTicks, MemoryDetail};

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

/// `/proc/meminfo`'s two facts, KiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    /// `MemTotal`.
    pub total_kib: u64,
    /// `MemAvailable`.
    pub available_kib: u64,
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

/// One value of a binary auxiliary vector (`/proc/self/auxv`): native-endian
/// `(key, value)` pairs of the platform's word, ended by `AT_NULL`. `None`
/// when the key is absent or the bytes are cut.
#[must_use]
pub fn auxv_value(bytes: &[u8], key: u64) -> Option<u64> {
    const W: usize = std::mem::size_of::<usize>();
    let word = |b: &[u8]| -> u64 {
        let mut a = [0u8; W];
        a.copy_from_slice(b);
        u64::try_from(usize::from_ne_bytes(a)).unwrap_or(u64::MAX)
    };
    let (pairs, _) = bytes.as_chunks::<{ 2 * W }>();
    pairs.iter().find_map(|pair| {
        let k = word(&pair[..W]);
        (k == key).then(|| word(&pair[W..]))
    })
}

/// The process group (`pgrp`, the fifth field) of a `/proc/<pid>/stat`
/// line. The second field, the command name in parentheses, may itself hold
/// spaces and `)`, so the fields are counted from the LAST `)`. `None` for a
/// line that is cut or malformed.
#[must_use]
pub fn stat_pgrp(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    // state, ppid, pgrp
    fields.nth(2)?.parse().ok()
}

/// The most logical cores any reader here places (ids `0..MAX_CPUS`): a
/// larger id is read as a malformed text, never as a vector that large.
pub const MAX_CPUS: usize = 4096;

/// The most bytes of `/proc/stat` one [`StatReader::sample`] takes: room for
/// the per-core lines of about 800 cores.
pub const STAT_CAP: usize = 64 * 1024;

/// Every core's ticks from `/proc/stat`'s `cpuN` lines, indexed by `N` (a
/// core missing between two others, which the kernel leaves out while it is
/// offline, reads all zero). The aggregate `cpu` line is skipped and the
/// first line that is not a core's ends the list. Columns: `user nice
/// system idle [iowait irq softirq …]`; `system` takes `irq + softirq`,
/// `idle` takes `iowait`; `steal`, `guest` and `guest_nice` are left out
/// (guest time is already inside `user` and `nice`).
///
/// `cut`: the read stopped at its cap. A cut text whose core lines might go
/// on past it is `None`, as is a text with no core line, a malformed core
/// line, or an id at or past [`MAX_CPUS`]. PURE.
#[must_use]
pub fn proc_stat_cores(text: &str, cut: bool) -> Option<Vec<CoreTicks>> {
    let end = if text.ends_with('\n') {
        text.len()
    } else {
        text.rfind('\n').map_or(0, |i| i + 1)
    };
    let (whole, torn) = text.split_at(end);
    let mut cores: Vec<CoreTicks> = Vec::new();
    let mut ended = false;
    for line in whole.lines() {
        let mut fields = line.split_whitespace();
        let Some(id) = fields.next().and_then(|label| label.strip_prefix("cpu")) else {
            ended = true;
            break;
        };
        if id.is_empty() {
            continue;
        }
        let id: usize = id.parse().ok()?;
        if id >= MAX_CPUS {
            return None;
        }
        let mut f = [0u64; 7];
        let mut n = 0;
        for (slot, field) in f.iter_mut().zip(fields.by_ref()) {
            *slot = field.parse().ok()?;
            n += 1;
        }
        if n < 4 {
            return None;
        }
        if cores.len() <= id {
            cores.resize(id + 1, CoreTicks::default());
        }
        cores[id] = CoreTicks {
            user: f[0],
            nice: f[1],
            system: f[2].saturating_add(f[5]).saturating_add(f[6]),
            idle: f[3].saturating_add(f[4]),
        };
    }
    // A cut read is whole only when a line that is not a core's was seen,
    // or the torn tail plainly begins one (`intr`: the kernel's next line).
    let k = torn.len().min(3);
    let torn_is_other = !torn.is_empty() && torn.as_bytes()[..k] != b"cpu"[..k];
    if cut && !ended && !torn_is_other {
        return None;
    }
    (!cores.is_empty()).then_some(cores)
}

/// The reader behind [`crate::Probe::core_ticks`] on Linux: `<root>/proc/stat`
/// read into a [`STAT_CAP`]-byte buffer it keeps (one allocation, when it is
/// built), parsed by [`proc_stat_cores`], and padded with zeroed cores up to
/// the present count it was given. Compiled everywhere, so a fixture root
/// tests it on any host.
#[derive(Debug)]
pub struct StatReader {
    path: PathBuf,
    buf: Vec<u8>,
    present: usize,
}

impl StatReader {
    /// A reader of `<root>/proc/stat` that pads its answer to `present`
    /// cores (at most [`MAX_CPUS`]).
    #[must_use]
    pub fn new(root: &Path, present: usize) -> Self {
        Self {
            path: root.join("proc/stat"),
            buf: vec![0; STAT_CAP],
            present: present.min(MAX_CPUS),
        }
    }

    /// One reading; `None` when the file cannot be read or parsed.
    pub fn sample(&mut self) -> Option<Vec<CoreTicks>> {
        let (n, cut) = crate::read_into(&self.path, &mut self.buf).ok()?;
        let mut cores = proc_stat_cores(crate::utf8_prefix(&self.buf[..n]), cut)?;
        if cores.len() < self.present {
            cores.resize(self.present, CoreTicks::default());
        }
        Some(cores)
    }
}

/// A kernel CPU list (`0-3,5,8-11`, as `/sys/devices/system/cpu/present`
/// holds it) as ascending, distinct ids. `None` for an empty or malformed
/// list, a range that runs backwards, or an id at or past [`MAX_CPUS`]. PURE.
#[must_use]
pub fn cpulist(text: &str) -> Option<Vec<u16>> {
    let mut ids = Vec::new();
    for part in text.trim().split(',') {
        let (lo, hi) = part.split_once('-').unwrap_or((part, part));
        let (lo, hi): (u16, u16) = (lo.trim().parse().ok()?, hi.trim().parse().ok()?);
        if lo > hi || usize::from(hi) >= MAX_CPUS {
            return None;
        }
        ids.extend(lo..=hi);
    }
    ids.sort_unstable();
    ids.dedup();
    (!ids.is_empty()).then_some(ids)
}

/// [`MemoryDetail`] from `/proc/meminfo` (KiB × 1024). Linux has no page
/// classes like macOS's, so each field holds what maps and zero otherwise:
///
/// | field | from |
/// |---|---|
/// | `total`, `free` | `MemTotal`, `MemFree` (both required) |
/// | `active`, `inactive` | `Active`, `Inactive` |
/// | `wired` | `Unevictable + SUnreclaim + KernelStack + PageTables`: memory that is never paged out |
/// | `compressed`, `stored` | zswap's `Zswap` (its pool) and `Zswapped` (what the pool holds), where the kernel reports them |
/// | `file_backed` | `Active(file) + Inactive(file)` |
/// | `app` | `AnonPages` |
/// | `swap_total`, `swap_used` | `SwapTotal`, `SwapTotal − SwapFree` |
/// | `speculative`, `purgeable` | 0: no counterpart |
/// | `pressure` | `None`: no Linux reader here |
///
/// A missing optional key reads 0. PURE.
#[must_use]
pub fn memory_detail(text: &str) -> Option<MemoryDetail> {
    let bytes = |key: &str| keyed(text, key).unwrap_or(0).saturating_mul(1024);
    let swap_total = bytes("SwapTotal");
    Some(MemoryDetail {
        total: keyed(text, "MemTotal")?.saturating_mul(1024),
        free: keyed(text, "MemFree")?.saturating_mul(1024),
        speculative: 0,
        active: bytes("Active"),
        inactive: bytes("Inactive"),
        wired: bytes("Unevictable")
            .saturating_add(bytes("SUnreclaim"))
            .saturating_add(bytes("KernelStack"))
            .saturating_add(bytes("PageTables")),
        compressed: bytes("Zswap"),
        stored: bytes("Zswapped"),
        purgeable: 0,
        file_backed: bytes("Active(file)").saturating_add(bytes("Inactive(file)")),
        app: bytes("AnonPages"),
        swap_used: swap_total.saturating_sub(bytes("SwapFree")),
        swap_total,
        pressure: None,
    })
}
