// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux `/proc` and `/sys` parsers (pure functions over `&str` and `&[u8]`)
//! and [`Root`] readers (std I/O under a configurable root directory). Both
//! compile and run on every platform so fixture trees test the same code a
//! Linux build runs on `/`.
//!
//! Every file is read through [`crate::with_bounded`] (one 4 KiB stack
//! buffer), so every parser tolerates a text cut at 4 KiB — a torn last
//! line is ignored, and a fact that was cut off reads `None`.
//!
//! | file | parser | into |
//! |---|---|---|
//! | `/proc/meminfo` | [`meminfo`] | [`crate::physical_memory_mib`], [`crate::machine_memory`] |
//! | `/proc/meminfo` | [`memory_detail`] | [`crate::memory_detail`] (classes, commit) |
//! | `/proc/pressure/memory` | [`psi`], [`psi_level`] | the pressure level |
//! | `/proc/vmstat` | [`swap_pages`] | [`crate::swapped_kib`] |
//! | `/proc/self/auxv` | [`auxv_value`] | the page size |
//! | `/proc/<pid>/stat` | [`stat_pgrp`] | group-field parser; no complete live group reader here ([`crate::stdin_sharers`] reads a child's) |
//! | `/proc/<pid>/task/<tid>/children` | [`children`] | [`crate::stdin_sharers`] |
//! | `/proc/<pid>/stat`, `status`, `statm` | [`crate::jobs::linux::parse_stat`], [`status_footprint`], [`crate::procs::parse_statm_resident`] | [`crate::processes`] |
//! | `/proc/stat` | [`proc_stat_cores`] through [`StatReader`] | [`crate::Probe::core_ticks`] |
//! | `/sys/devices/system/cpu/{present,online}` | [`cpulist`] | [`crate::cpu_layout`], withdrawn cores |
//! | `/sys/devices/cpu_*/cpus`, `cpuN/cpu_capacity` | [`layout_from`] | [`crate::cpu_layout`] |
//! | `/proc/self/cgroup`, `cpu.max`, `memory.max` | [`cgroup_v2_path`], [`cpu_max_millicores`], [`memory_max`] | [`crate::capacity`] |
//! | `/sys/class/thermal/thermal_zone*`, `thermal_throttle/*_count` | [`trip_kind`], [`throttle_level`] | [`crate::Probe::throttle`] |
//! | `/sys/class/drm/card*/device/*` | [`Root::gpu`] | [`crate::gpu`] |
//! | `/proc/net/dev` | [`net_dev`] through [`links_under_into`] | [`crate::net_links_into`] |
//! | `/sys/class/net/<if>/{flags,operstate,speed}` | [`hex_flags`], [`link_up`], [`speed_bps`] | [`crate::net_links_into`] |
//! | `/proc/loadavg` | [`loadavg_tasks`] through [`Root::tasks`] | [`crate::pid_counts`] |
//!
//! `/proc/stat` and `/proc/net/dev` are the files read past 4 KiB:
//! `/proc/stat`'s per-core lines take about 80 bytes each, so
//! [`StatReader`] reads it into a [`STAT_CAP`]-byte buffer it keeps, and
//! [`proc_stat_cores`] refuses a text cut inside them; `/proc/net/dev`
//! takes about 130 bytes an interface, so [`links_under_into`] reads it
//! into a [`NET_DEV_CAP`]-byte buffer and reads the interfaces before the
//! cut.
//!
//! Every directory listed is bounded: at most [`MAX_CPUS`] cores, at most
//! [`MAX_ZONES`] thermal zones of at most [`MAX_TRIPS`] trip points, at most
//! [`MAX_CARDS`] DRM cards, at most [`crate::procs::MAX_PROCESSES`]
//! processes, and a cgroup path at most [`MAX_CGROUP_DEPTH`] deep.

use std::path::{Path, PathBuf};

use crate::jobs::RunState;
use crate::jobs::linux::parse_stat;
use crate::machine::{
    Capacity, Commit, LinkCounters, LinkKind, LinkReading, MAX_LINKS, Throttle, ThrottleLevel,
    kind_by_name, layout_by_rank, level_names,
};
use crate::procs::{MAX_PROCESSES, ProcessReading, Sweep, parse_statm_resident};
use crate::{
    Cap, CapSource, Caps, Cluster, CoreTicks, CpuLayout, GpuReading, MemoryDetail, MemoryLevel, Why,
};

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
/// spaces and `)`, so the fields are counted from the last `)`. `None` for a
/// line that is cut or malformed.
#[must_use]
pub fn stat_pgrp(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    // state, ppid, pgrp
    fields.nth(2)?.parse().ok()
}

/// The pids of a `/proc/<pid>/task/<tid>/children` text: the kernel writes
/// each child as its pid and one space (`"812 9001 "`), so only a number
/// its space closes is read, and a text cut at the cap loses its torn last
/// pid rather than naming a shorter one. Anything else is skipped.
#[must_use]
pub fn children(text: &str) -> Vec<u32> {
    text.split_inclusive(' ')
        .filter_map(|word| word.strip_suffix(' ')?.parse().ok())
        .collect()
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
/// line, or an id at or past [`MAX_CPUS`].
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
/// the present count it was given.
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
/// list, a range that runs backwards, or an id at or past [`MAX_CPUS`].
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

/// [`MemoryDetail`] from `/proc/meminfo` (KiB × 1024). Linux has no exact
/// partition of physical memory like macOS's, so [`MemoryDetail::exact`] is
/// false and `total` less the classes is memory no class names:
///
/// | field | from |
/// |---|---|
/// | `total`, `free` | `MemTotal`, `MemFree` (both required) |
/// | `app` | `AnonPages`: anonymous memory of processes, resident |
/// | `shared` | `Shmem`: tmpfs and shared memory, listed by the kernel under `Cached` but reclaimable only by swap, so never counted as cache |
/// | `wired` | kernel memory the kernel cannot give back: `SUnreclaim + KernelStack + PageTables` (all of `Slab` where `SUnreclaim` is missing) |
/// | `file_backed` | the reclaimable cache: `Cached − Shmem + Buffers + SReclaimable` (the dentry and inode caches, which `free(1)` counts as cache too) |
/// | `compressed`, `stored` | zswap's pool (`Zswap`) and what it holds (`Zswapped`), where the kernel reports them; zram is a swap device, counted under swap |
/// | `active`, `inactive` | `Active`, `Inactive` |
/// | `swap_total`, `swap_used` | `SwapTotal`, `SwapTotal − SwapFree` |
/// | `commit` | `Committed_AS` against `CommitLimit`, a heuristic ([`Commit::exact`] false): under overcommit the limit is advisory |
/// | `speculative`, `purgeable` | `None`: no counterpart |
/// | `pressure` | `None` here; [`Root::memory_detail`] reads PSI |
///
/// A missing optional key reads `None` where the field is optional and 0
/// where it is summed.
#[must_use]
pub fn memory_detail(text: &str) -> Option<MemoryDetail> {
    let bytes = |key: &str| keyed(text, key).map(|k| k.saturating_mul(1024));
    let sum = |keys: &[&str]| {
        keys.iter()
            .map(|k| bytes(k).unwrap_or(0))
            .fold(0u64, u64::saturating_add)
    };
    let total = bytes("MemTotal")?;
    let free = bytes("MemFree")?;
    let shared = bytes("Shmem");
    // The slab the kernel can reclaim (dentries, inodes) is cache, as
    // `free(1)` counts it; only what it cannot is the kernel's own.
    let (slab, reclaimable) = match bytes("SUnreclaim") {
        Some(unreclaimable) => (unreclaimable, bytes("SReclaimable").unwrap_or(0)),
        None => (bytes("Slab").unwrap_or(0), 0),
    };
    let swap_total = bytes("SwapTotal").unwrap_or(0);
    let commit = match (bytes("Committed_AS"), bytes("CommitLimit")) {
        (Some(charge), Some(limit)) => Some(Commit {
            charge,
            limit,
            exact: false,
        }),
        _ => None,
    };
    Some(MemoryDetail {
        total,
        free,
        speculative: None,
        active: bytes("Active"),
        inactive: bytes("Inactive"),
        wired: slab.saturating_add(sum(&["KernelStack", "PageTables"])),
        compressed: bytes("Zswap"),
        stored: bytes("Zswapped"),
        purgeable: None,
        file_backed: bytes("Cached")
            .unwrap_or(0)
            .saturating_sub(shared.unwrap_or(0))
            .saturating_add(bytes("Buffers").unwrap_or(0))
            .saturating_add(reclaimable),
        app: bytes("AnonPages").unwrap_or(0),
        shared,
        swap_used: swap_total.saturating_sub(bytes("SwapFree").unwrap_or(swap_total)),
        swap_total,
        commit,
        pressure: None,
        counters: None,
        kernel_available_pct: None,
        exact: false,
    })
}

/// `/proc/cpuinfo`'s first `model name` (x86) or `Model` (ARM boards). PURE.
#[must_use]
pub fn cpu_model(cpuinfo: &str) -> Option<String> {
    cpuinfo.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        let key = key.trim();
        (key == "model name" || key == "Model")
            .then(|| value.trim().to_owned())
            .filter(|v| !v.is_empty())
    })
}

/// `/etc/os-release`'s `PRETTY_NAME`, unquoted. PURE.
#[must_use]
pub fn os_pretty_name(os_release: &str) -> Option<String> {
    os_release.lines().find_map(|line| {
        let value = line.strip_prefix("PRETTY_NAME=")?;
        let value = value.trim().trim_matches('"').trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// The most bytes of `/proc/net/dev` one [`links_under_into`] takes: room
/// for about 500 interfaces.
pub const NET_DEV_CAP: usize = 64 * 1024;

/// One interface's row of `/proc/net/dev`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetDev {
    /// Its name.
    pub name: String,
    /// Received: `bytes packets errs drop`.
    pub rx: LinkCounters,
    /// Sent: `bytes packets errs drop`.
    pub tx: LinkCounters,
}

/// Every interface's counters from `/proc/net/dev`: past its two header
/// lines (which hold `|`), each line is `name: ` and sixteen counters, the
/// first eight received (`bytes packets errs drop fifo frame compressed
/// multicast`) and the next eight sent (`bytes packets errs drop fifo colls
/// carrier compressed`). A line with fewer than twelve counters, a counter
/// that does not parse, or a name that is empty or holds `/` (which no
/// kernel name does) is left out, as is a torn last line.
#[must_use]
pub fn net_dev(text: &str) -> Vec<NetDev> {
    whole_lines(text)
        .filter(|l| !l.contains('|'))
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let name = name.trim();
            if name.is_empty() || name.contains('/') || name == "." || name == ".." {
                return None;
            }
            let mut f = [0u64; 12];
            let mut n = 0;
            for (slot, field) in f.iter_mut().zip(rest.split_whitespace()) {
                *slot = field.parse().ok()?;
                n += 1;
            }
            (n == f.len()).then(|| NetDev {
                name: name.to_owned(),
                rx: LinkCounters {
                    bytes: f[0],
                    packets: f[1],
                    errors: f[2],
                    drops: f[3],
                },
                tx: LinkCounters {
                    bytes: f[8],
                    packets: f[9],
                    errors: f[10],
                    drops: f[11],
                },
            })
        })
        .collect()
}

/// `IFF_UP` (`linux/if.h`).
const IFF_UP: u32 = 0x1;
/// `IFF_LOOPBACK` (`linux/if.h`).
const IFF_LOOPBACK: u32 = 0x8;

/// `/sys/class/net/<if>/flags` (`0x1003`) as its bits; `None` where it
/// does not parse.
#[must_use]
pub fn hex_flags(text: &str) -> Option<u32> {
    let t = text.trim();
    let digits = t.strip_prefix("0x").unwrap_or(t);
    u32::from_str_radix(digits, 16).ok()
}

/// Up and running: `IFF_UP` set (the flags file holds the administrative
/// flags only; the kernel never stores `IFF_RUNNING` there) and an
/// `operstate` of `up`, or `unknown` for a driver that keeps none (the
/// loopback, many tunnels).
#[must_use]
pub fn link_up(flags: u32, operstate: &str) -> bool {
    flags & IFF_UP != 0 && matches!(operstate.trim(), "up" | "unknown")
}

/// `/sys/class/net/<if>/speed` (Mb/s) as bits per second; `None` for
/// `-1`, zero, or what does not parse (the kernel answers `-1` or refuses
/// the read for a link that is down and for Wi-Fi).
#[must_use]
pub fn speed_bps(text: &str) -> Option<u64> {
    let mbps: i64 = text.trim().parse().ok()?;
    u64::try_from(mbps)
        .ok()
        .filter(|m| *m > 0)
        .map(|m| m.saturating_mul(1_000_000))
}

/// A link's kind from what sysfs says of it: a refused name
/// ([`crate::machine::never_the_machines`]) is other whatever sysfs says, as
/// on macOS; `wireless` present is Wi-Fi; a cellular name ([`kind_by_name`])
/// is cellular; a link with a device is wired; anything else (a bridge, a
/// tunnel, a `veth`) is other.
#[must_use]
pub fn sys_kind(name: &str, wireless: bool, physical: bool) -> LinkKind {
    if crate::machine::never_the_machines(name) {
        LinkKind::Other
    } else if wireless {
        LinkKind::Wifi
    } else if kind_by_name(name) == LinkKind::Cellular {
        LinkKind::Cellular
    } else if physical {
        LinkKind::Wired
    } else {
        LinkKind::Other
    }
}

/// Every interface under `root` ([`crate::net_links_into`] on Linux, with
/// root `/`): `<root>/proc/net/dev` read into the first [`NET_DEV_CAP`]
/// bytes of `scratch` (made that long where it is shorter, and kept by the
/// caller between readings) and parsed by [`net_dev`], at most
/// [`MAX_LINKS`] interfaces, then per interface its
/// `<root>/sys/class/net/<if>/` `ifindex`, `flags` and `operstate` (each
/// read into one [`crate::READ_CAP`]-byte stack buffer, no heap copy),
/// whether `device` and `wireless` exist, and `speed` where a device backs
/// it. One path buffer names every interface's files. A fact that cannot
/// be read reads as nothing: index 0, not up, no device. `None` where
/// `/proc/net/dev` cannot be read. Compiled everywhere, so a fixture root
/// tests it on any host.
#[must_use]
pub fn links_under_into(root: &Path, scratch: &mut Vec<u8>) -> Option<Vec<LinkReading>> {
    if scratch.len() < NET_DEV_CAP {
        scratch.resize(NET_DEV_CAP, 0);
    }
    let buf = &mut scratch[..NET_DEV_CAP];
    let (n, _) = crate::read_into(&root.join("proc/net/dev"), buf).ok()?;
    let mut dir = root.join("sys/class/net");
    Some(
        net_dev(crate::utf8_prefix(&buf[..n]))
            .into_iter()
            .take(MAX_LINKS)
            .map(|row| {
                dir.push(&row.name);
                let link = sys_link(&mut dir, row);
                dir.pop();
                link
            })
            .collect(),
    )
}

/// `f` over the path `dir/file`, `dir` given back as it came.
fn in_dir<T>(dir: &mut PathBuf, file: &str, f: impl FnOnce(&Path) -> T) -> T {
    dir.push(file);
    let out = f(dir.as_path());
    dir.pop();
    out
}

/// `row` joined to the sysfs facts in `dir`, its `/sys/class/net/<if>`
/// ([`links_under_into`]); `dir` is given back as it came.
fn sys_link(dir: &mut PathBuf, row: NetDev) -> LinkReading {
    let flags = in_dir(dir, "flags", |p| crate::with_bounded(p, hex_flags))
        .flatten()
        .unwrap_or(0);
    let physical = in_dir(dir, "device", Path::exists);
    let wireless = in_dir(dir, "wireless", Path::exists);
    LinkReading {
        index: in_dir(dir, "ifindex", |p| {
            crate::with_bounded(p, |t| t.trim().parse().ok())
        })
        .flatten()
        .unwrap_or(0),
        kind: sys_kind(&row.name, wireless, physical),
        up: in_dir(dir, "operstate", |p| {
            crate::with_bounded(p, |s| link_up(flags, s))
        })
        .unwrap_or(false),
        loopback: flags & IFF_LOOPBACK != 0,
        physical: Some(physical),
        rate_bps: if physical {
            in_dir(dir, "speed", |p| crate::with_bounded(p, speed_bps)).flatten()
        } else {
            None
        },
        name: row.name,
        rx: row.rx,
        tx: row.tx,
    }
}

/// The kernel's count of every thread on the machine, kernel threads
/// included: the total of `/proc/loadavg`'s fourth field (`running/total`),
/// from the file's first whole line. `None` for a torn line, a field that
/// is not two counts, or a total of zero (the reading thread is one).
/// PURE.
#[must_use]
pub fn loadavg_tasks(text: &str) -> Option<u64> {
    let line = whole_lines(text).next()?;
    let (running, total) = line.split_whitespace().nth(3)?.split_once('/')?;
    let total: u64 = total.parse().ok()?;
    (running.parse::<u64>().is_ok() && total > 0).then_some(total)
}

/// One PSI file's ten-second averages (`/proc/pressure/memory`): the share
/// of wall time some task, and every task, stalled on memory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Psi {
    /// `some avg10`, percent.
    pub some_avg10: f32,
    /// `full avg10`, percent.
    pub full_avg10: f32,
}

/// `some avg10` at or past this is [`MemoryLevel::Warn`]: Android lmkd's
/// default partial-stall threshold (70 ms of each second).
pub const PSI_WARN_SOME_AVG10: f32 = 7.0;

/// `full avg10` at or past this is [`MemoryLevel::Critical`]: systemd-oomd's
/// default `ManagedOOMMemoryPressureLimit`, where it starts killing.
pub const PSI_CRITICAL_FULL_AVG10: f32 = 60.0;

/// A PSI file's `some` and `full` ten-second averages (`some avg10=1.25
/// avg60=… total=…`). `None` without both lines or with a malformed average.
#[must_use]
pub fn psi(text: &str) -> Option<Psi> {
    let avg10 = |kind: &str| {
        whole_lines(text).find_map(|l| {
            let mut it = l.split_whitespace();
            if it.next()? != kind {
                return None;
            }
            it.find_map(|f| f.strip_prefix("avg10="))?
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
        })
    };
    Some(Psi {
        some_avg10: avg10("some")?,
        full_avg10: avg10("full")?,
    })
}

/// The pressure level PSI reads: critical at [`PSI_CRITICAL_FULL_AVG10`],
/// warn at [`PSI_WARN_SOME_AVG10`], else normal.
#[must_use]
pub fn psi_level(p: &Psi) -> MemoryLevel {
    if p.full_avg10 >= PSI_CRITICAL_FULL_AVG10 {
        MemoryLevel::Critical
    } else if p.some_avg10 >= PSI_WARN_SOME_AVG10 {
        MemoryLevel::Warn
    } else {
        MemoryLevel::Normal
    }
}

/// The layout from the present cores, the online ones (`online`; the
/// present ones where unread), the hybrid PMUs' core lists (`name`, ids:
/// `/sys/devices/cpu_core/cpus` and its kin) and each core's
/// `cpu_capacity`, fastest first.
///
/// 1. The PMUs, when there are at least two and they partition the online
///    cores exactly (a PMU lists only online cores): `cpu_core` is
///    "Performance", `cpu_atom` "Efficiency", any other `cpu_<kind>` named
///    by its kind, in that order. A present core that is offline joins the
///    cluster of its nearest online neighbour by id (the kernel numbers one
///    kind's cores together), so a withdrawn core keeps its place.
/// 2. Else the capacities, when every present core has one and at least two
///    differ: one cluster per capacity, highest first
///    ([`crate::machine::layout_by_rank`]).
/// 3. Else one cluster `"CPU"` of the present cores.
#[must_use]
pub fn layout_from(
    present: &[u16],
    online: Option<&[u16]>,
    pmus: &[(String, Vec<u16>)],
    capacities: &[(u16, u32)],
) -> CpuLayout {
    if let Some(layout) = layout_of_pmus(present, online.unwrap_or(present), pmus) {
        return layout;
    }
    let mut distinct: Vec<u32> = capacities.iter().map(|(_, c)| *c).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let covered = present
        .iter()
        .all(|id| capacities.iter().any(|(c, _)| c == id));
    if covered && distinct.len() >= 2 {
        let ranked: Vec<(u16, u32)> = capacities
            .iter()
            .filter(|(id, _)| present.contains(id))
            .copied()
            .collect();
        if let Some(layout) = layout_by_rank(&ranked) {
            return layout;
        }
    }
    CpuLayout {
        clusters: vec![Cluster {
            name: level_names(1).swap_remove(0),
            logical: present.to_vec(),
        }],
    }
}

/// [`layout_from`]'s first rule: `None` unless at least two PMUs partition
/// the online cores among `present` exactly; each offline present core then
/// joins its nearest online neighbour's cluster.
fn layout_of_pmus(
    present: &[u16],
    online: &[u16],
    pmus: &[(String, Vec<u16>)],
) -> Option<CpuLayout> {
    if pmus.len() < 2 {
        return None;
    }
    let rank = |name: &str| match name {
        "cpu_core" => 0,
        "cpu_atom" => 1,
        _ => 2,
    };
    let mut ordered: Vec<&(String, Vec<u16>)> = pmus.iter().collect();
    ordered.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0)));
    let mut seen: Vec<u16> = Vec::with_capacity(present.len());
    let mut clusters = Vec::with_capacity(ordered.len());
    for (name, ids) in ordered {
        let mut logical: Vec<u16> = ids
            .iter()
            .copied()
            .filter(|i| present.contains(i))
            .collect();
        logical.sort_unstable();
        logical.dedup();
        if logical.iter().any(|i| seen.contains(i)) {
            return None;
        }
        seen.extend(&logical);
        if logical.is_empty() {
            continue;
        }
        clusters.push(Cluster {
            name: pmu_name(name),
            logical,
        });
    }
    let live: Vec<u16> = present
        .iter()
        .copied()
        .filter(|i| online.contains(i))
        .collect();
    if seen.len() != live.len() || clusters.len() < 2 {
        return None;
    }
    // The offline cores, each beside its nearest online neighbour (the
    // lower one on a tie).
    for &id in present.iter().filter(|i| !seen.contains(i)) {
        let near = |c: &Cluster| {
            c.logical
                .iter()
                .map(|&j| (u32::from(id.abs_diff(j)), u8::from(j > id)))
                .min()
        };
        let home = clusters
            .iter()
            .enumerate()
            .filter_map(|(k, c)| near(c).map(|d| (d, k)))
            .min()
            .map(|(_, k)| k)?;
        clusters[home].logical.push(id);
        clusters[home].logical.sort_unstable();
    }
    Some(CpuLayout { clusters })
}

/// A hybrid PMU's level name: `cpu_core` "Performance", `cpu_atom`
/// "Efficiency", `cpu_<kind>` its kind with a capital (`cpu_lowpower`
/// "Lowpower").
fn pmu_name(pmu: &str) -> String {
    match pmu {
        "cpu_core" => "Performance".to_owned(),
        "cpu_atom" => "Efficiency".to_owned(),
        other => {
            let kind = other.strip_prefix("cpu_").unwrap_or(other);
            let mut chars = kind.chars();
            chars.next().map_or_else(String::new, |c| {
                c.to_uppercase().chain(chars).collect::<String>()
            })
        }
    }
}

/// The deepest cgroup path [`Root::capacity`] walks up from.
pub const MAX_CGROUP_DEPTH: usize = 64;

/// The process's cgroup v2 path, from `/proc/self/cgroup`'s `0::<path>`
/// line. `None` on a cgroup v1 machine (no such line) or for a path that is
/// not absolute or that walks up through `..`.
#[must_use]
pub fn cgroup_v2_path(text: &str) -> Option<&str> {
    let path = whole_lines(text).find_map(|l| l.strip_prefix("0::"))?;
    (path.starts_with('/') && !path.split('/').any(|c| c == ".." || c == ".")).then_some(path)
}

/// `cpu.max` (`<quota> <period>` in µs, or `max <period>`) as thousandths
/// of a core: `Some(None)` for no limit, `None` for a malformed text or a
/// zero period.
#[must_use]
pub fn cpu_max_millicores(text: &str) -> Option<Option<u32>> {
    let mut it = text.split_whitespace();
    let quota = it.next()?;
    let period: u64 = it.next()?.parse().ok().filter(|p| *p > 0)?;
    if quota == "max" {
        return Some(None);
    }
    let quota: u64 = quota.parse().ok()?;
    let milli = u128::from(quota) * 1000 / u128::from(period);
    Some(Some(u32::try_from(milli).unwrap_or(u32::MAX)))
}

/// `memory.max` (bytes, or `max`): `Some(None)` for no limit, `None` for a
/// malformed text.
#[must_use]
pub fn memory_max(text: &str) -> Option<Option<u64>> {
    let word = text.split_whitespace().next()?;
    if word == "max" {
        return Some(None);
    }
    word.parse().ok().map(Some)
}

/// The most thermal zones [`Root::zones`] reads.
pub const MAX_ZONES: usize = 32;
/// The most trip points read per zone.
pub const MAX_TRIPS: usize = 16;

/// A thermal trip point the throttle level reads (`active` trips start fans
/// and are left out).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TripKind {
    /// The kernel starts slowing the cores (passive cooling).
    Passive,
    /// Hot: the platform's last warning before critical.
    Hot,
    /// Critical: the kernel shuts the machine down.
    Critical,
}

/// A `trip_point_N_type` text as a [`TripKind`]; `None` for `active` and
/// anything else.
#[must_use]
pub fn trip_kind(text: &str) -> Option<TripKind> {
    match text.trim() {
        "passive" => Some(TripKind::Passive),
        "hot" => Some(TripKind::Hot),
        "critical" => Some(TripKind::Critical),
        _ => None,
    }
}

/// One thermal zone: its temperature and trip points, millidegrees Celsius.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    /// The zone's temperature.
    pub temp_mc: i64,
    /// Its passive, hot and critical trip points (a trip at or below 0 °C,
    /// which some firmware writes for a disabled trip, is left out).
    pub trips: Vec<(TripKind, i64)>,
}

/// The heuristic throttle level from the thermal zones and whether the CPUs'
/// thermal throttle counters rose since the last reading (`None`: the
/// machine has no such counters).
///
/// * `severe`: a zone at or past its critical trip;
/// * `heavy`: a zone at or past its hot trip, or past a passive trip while
///   the counters rose;
/// * `light`: a zone past a passive trip, or the counters rose;
/// * `none`: otherwise.
///
/// # Errors
///
/// [`Why::Absent`] when no zone has a trip point and there are no counters
/// (most virtual machines): nothing to judge from.
pub fn throttle_level(zones: &[Zone], counters_rose: Option<bool>) -> Result<ThrottleLevel, Why> {
    if zones.iter().all(|z| z.trips.is_empty()) && counters_rose.is_none() {
        return Err(Why::Absent);
    }
    let reached = |kind: TripKind| {
        zones
            .iter()
            .any(|z| z.trips.iter().any(|&(k, t)| k == kind && z.temp_mc >= t))
    };
    let rose = counters_rose == Some(true);
    Ok(if reached(TripKind::Critical) {
        ThrottleLevel::Severe
    } else if reached(TripKind::Hot) || (reached(TripKind::Passive) && rose) {
        ThrottleLevel::Heavy
    } else if reached(TripKind::Passive) || rose {
        ThrottleLevel::Light
    } else {
        ThrottleLevel::Nominal
    })
}

/// A process's memory from `/proc/<pid>/status`: `RssAnon + VmSwap` (its
/// anonymous memory, resident or swapped out — the nearest Linux has to
/// macOS's footprint), in bytes. `None` without `RssAnon` (a kernel
/// thread, or a kernel older than 4.5); a missing `VmSwap` reads 0.
#[must_use]
pub fn status_footprint(text: &str) -> Option<u64> {
    let anon = keyed(text, "RssAnon")?;
    let swap = keyed(text, "VmSwap").unwrap_or(0);
    Some(anon.saturating_add(swap).saturating_mul(1024))
}

/// The most bytes of one `/proc/<pid>/status` [`Root::processes`] reads. A
/// user in the kernel's limit of 65,536 supplementary groups lists every
/// one on `Groups:`, ahead of the memory lines, about 720 KiB; a status
/// longer than this is read as no footprint, never a short one.
pub const STATUS_CAP: usize = 1 << 20;

/// The most DRM cards [`Root::gpu`] reads.
pub const MAX_CARDS: usize = 16;

/// DRM drivers that drive a firmware framebuffer, not a GPU: a card of one
/// is no GPU.
const FRAMEBUFFER_DRIVERS: &[&str] = &["simpledrm", "efidrm", "vesadrm", "ofdrm", "vkms"];

/// Why a DRM driver publishes no device-wide busy figure in sysfs.
fn gpu_unread(driver: &str) -> Why {
    match driver {
        // The i915 and xe engine-busy counters are perf PMU events, readable
        // only with CAP_PERFMON (or perf_event_paranoid ≤ 0).
        "i915" | "xe" => Why::NeedsPrivilege("cap_perfmon"),
        // NVIDIA's proprietary driver publishes its figures through NVML, a
        // library this build does not load.
        "nvidia" => Why::NotRead,
        _ => Why::Unsupported,
    }
}

/// Whether reading an awake card's sysfs figures leaves its runtime power
/// management where it was, from its device's `power/runtime_status`,
/// `power/control` and `boot_vga`: runtime PM is off (`unsupported`) or
/// forbidden (`on`), so nothing can suspend it; or the card is the one the
/// firmware booted the display on (an APU, a desktop's only card), which
/// the desktop holds awake. A hybrid laptop's dGPU (`auto`, not the boot
/// display) is none of these.
pub(crate) fn pm_settled(runtime_status: &str, control: &str, boot_vga: &str) -> bool {
    runtime_status == "unsupported" || control == "on" || boot_vga == "1"
}

/// The owner of a `/proc/<pid>` entry: the process's user (its effective
/// uid while it is dumpable). `None` off Unix or where unreadable.
fn owner(entry: &std::fs::DirEntry) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        entry.metadata().ok().map(|m| m.uid())
    }
    #[cfg(not(unix))]
    {
        let _ = entry;
        None
    }
}

/// `/proc`, `/sys` and `/sys/fs/cgroup` under one root directory: `/` on a
/// Linux machine ([`Root::system`]), a fixture tree in tests. Every reader
/// is std file I/O, bounded, and compiles on every host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    base: PathBuf,
}

impl Root {
    /// The tree under `base`.
    #[must_use]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// The machine's own tree, `/`.
    #[must_use]
    pub fn system() -> Self {
        Self::new("/")
    }

    /// `rel` under the root.
    fn at(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }

    /// `f` over the text of at most 4 KiB of `rel`; `None` when unreadable.
    fn read<T>(&self, rel: &str, f: impl FnOnce(&str) -> T) -> Option<T> {
        crate::with_bounded(&self.at(rel), f)
    }

    /// The names in directory `rel` that are `prefix` followed by digits,
    /// with those digits, ascending, at most `cap` of them.
    fn numbered(&self, rel: &str, prefix: &str, cap: usize) -> Vec<(u32, String)> {
        let Ok(dir) = std::fs::read_dir(self.at(rel)) else {
            return Vec::new();
        };
        let mut out: Vec<(u32, String)> = dir
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let n = name.strip_prefix(prefix)?;
                (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| n.parse().ok().map(|k| (k, name.clone())))
                    .flatten()
            })
            .collect();
        out.sort_unstable();
        out.truncate(cap);
        out
    }

    /// [`crate::machine_words`]: `/proc/cpuinfo`'s first model (its first
    /// 4 KiB hold the first CPU's) and `/etc/os-release`'s `PRETTY_NAME`.
    #[must_use]
    pub fn machine_words(&self) -> crate::MachineWords {
        crate::MachineWords {
            cpu: self.read("proc/cpuinfo", cpu_model).flatten(),
            os: self.read("etc/os-release", os_pretty_name).flatten(),
        }
    }

    /// The present cores (`/sys/devices/system/cpu/present`).
    #[must_use]
    pub fn present(&self) -> Option<Vec<u16>> {
        self.read("sys/devices/system/cpu/present", cpulist)
            .flatten()
    }

    /// The online cores (`/sys/devices/system/cpu/online`).
    #[must_use]
    pub fn online(&self) -> Option<Vec<u16>> {
        self.read("sys/devices/system/cpu/online", cpulist)
            .flatten()
    }

    /// The present cores, else `0..available_parallelism`.
    #[must_use]
    pub fn present_or_parallelism(&self) -> Vec<u16> {
        self.present()
            .unwrap_or_else(|| (0..crate::machine::parallelism()).collect())
    }

    /// Every thread on the machine (`/proc/loadavg`, [`loadavg_tasks`]),
    /// the count `kernel/pid_max` and `threads-max` bound. The kernel keeps
    /// one count for the whole machine, not one per namespace. `None` where
    /// unreadable.
    #[must_use]
    pub fn tasks(&self) -> Option<u64> {
        self.read("proc/loadavg", loadavg_tasks).flatten()
    }

    /// The cores the OS took away: present but not online. `None` where
    /// either list is unreadable.
    #[must_use]
    pub fn withdrawn(&self) -> Option<Vec<u16>> {
        let online = self.online()?;
        Some(
            self.present()?
                .into_iter()
                .filter(|id| !online.contains(id))
                .collect(),
        )
    }

    /// The hybrid PMUs' core lists: every `/sys/devices/cpu_<kind>/cpus`
    /// (`cpu_core`, `cpu_atom` on Intel's hybrid parts), at most eight.
    #[must_use]
    pub fn hybrid_pmus(&self) -> Vec<(String, Vec<u16>)> {
        let Ok(dir) = std::fs::read_dir(self.at("sys/devices")) else {
            return Vec::new();
        };
        let mut out: Vec<(String, Vec<u16>)> = dir
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                if !name.starts_with("cpu_") {
                    return None;
                }
                let ids = self
                    .read(&format!("sys/devices/{name}/cpus"), cpulist)
                    .flatten()?;
                Some((name, ids))
            })
            .collect();
        out.sort();
        out.truncate(8);
        out
    }

    /// Each of `ids`' `cpu_capacity` (`/sys/devices/system/cpu/cpuN/`), for
    /// the cores that have one.
    #[must_use]
    pub fn capacities(&self, ids: &[u16]) -> Vec<(u16, u32)> {
        ids.iter()
            .filter_map(|id| {
                let c = self
                    .read(
                        &format!("sys/devices/system/cpu/cpu{id}/cpu_capacity"),
                        |t| t.trim().parse::<u32>().ok(),
                    )
                    .flatten()?;
                Some((*id, c))
            })
            .collect()
    }

    /// The cores grouped by performance level, fastest first
    /// ([`layout_from`] over [`Self::present_or_parallelism`],
    /// [`Self::hybrid_pmus`] and [`Self::capacities`]).
    #[must_use]
    pub fn cpu_layout(&self) -> CpuLayout {
        let present = self.present_or_parallelism();
        let pmus = self.hybrid_pmus();
        let capacities = if pmus.len() >= 2 {
            Vec::new()
        } else {
            self.capacities(&present)
        };
        layout_from(&present, self.online().as_deref(), &pmus, &capacities)
    }

    /// Memory PSI (`/proc/pressure/memory`).
    ///
    /// # Errors
    ///
    /// `not_enabled:psi` when the file is missing or refuses the read (a
    /// kernel built without PSI, or booted with `psi=0`).
    pub fn psi(&self) -> Result<Psi, Why> {
        self.read("proc/pressure/memory", psi)
            .flatten()
            .ok_or(Why::NotEnabled("psi"))
    }

    /// Whether zswap is on (`/sys/module/zswap/parameters/enabled`): `None`
    /// when the kernel has no zswap.
    #[must_use]
    pub fn zswap_enabled(&self) -> Option<bool> {
        self.read("sys/module/zswap/parameters/enabled", |t| {
            matches!(t.trim(), "Y" | "y" | "1")
        })
    }

    /// Memory by class, swap, commit ([`memory_detail`]) and the PSI level
    /// ([`psi_level`]; `None` without PSI). With zswap off, its pool is not
    /// compressed memory: `compressed` and `stored` are `None`.
    #[must_use]
    pub fn memory_detail(&self) -> Option<MemoryDetail> {
        let mut m = self.read("proc/meminfo", memory_detail).flatten()?;
        m.pressure = self.psi().ok().as_ref().map(psi_level);
        if self.zswap_enabled() != Some(true) {
            m.compressed = None;
            m.stored = None;
        }
        Some(m)
    }

    /// What this process may use: the tightest `cpu.max` and `memory.max`
    /// on its cgroup v2 path, from its own cgroup up to the hierarchy's root
    /// (`/sys/fs/cgroup`; inside a container's cgroup namespace that root is
    /// the container). The host's under cgroup v1, with no limit, or where
    /// nothing is readable.
    #[must_use]
    pub fn capacity(&self) -> Capacity {
        let Some(path) = self
            .read("proc/self/cgroup", |t| cgroup_v2_path(t).map(str::to_owned))
            .flatten()
        else {
            return Capacity::default();
        };
        if !self.at("sys/fs/cgroup/cgroup.controllers").is_file() {
            return Capacity::default();
        }
        let mut cpu: Option<u32> = None;
        let mut mem: Option<u64> = None;
        let mut rel = path.trim_matches('/').to_owned();
        for _ in 0..=MAX_CGROUP_DEPTH {
            let dir = if rel.is_empty() {
                "sys/fs/cgroup".to_owned()
            } else {
                format!("sys/fs/cgroup/{rel}")
            };
            if let Some(Some(m)) = self
                .read(&format!("{dir}/cpu.max"), cpu_max_millicores)
                .flatten()
            {
                cpu = Some(cpu.map_or(m, |c| c.min(m)));
            }
            if let Some(Some(b)) = self
                .read(&format!("{dir}/memory.max"), memory_max)
                .flatten()
            {
                mem = Some(mem.map_or(b, |c| c.min(b)));
            }
            if rel.is_empty() {
                break;
            }
            rel = rel
                .rsplit_once('/')
                .map_or_else(String::new, |(up, _)| up.to_owned());
        }
        Capacity {
            cpu_millicores: cpu,
            mem_bytes: mem,
            source: if cpu.is_some() || mem.is_some() {
                CapSource::CgroupLimit
            } else {
                CapSource::Host
            },
        }
    }

    /// The thermal zones (`/sys/class/thermal/thermal_zoneN`), at most
    /// [`MAX_ZONES`], each with its temperature and up to [`MAX_TRIPS`]
    /// passive, hot and critical trips; a zone whose temperature does not
    /// read is left out.
    #[must_use]
    pub fn zones(&self) -> Vec<Zone> {
        let int = |t: &str| t.trim().parse::<i64>().ok();
        self.numbered("sys/class/thermal", "thermal_zone", MAX_ZONES)
            .into_iter()
            .filter_map(|(_, name)| {
                let dir = format!("sys/class/thermal/{name}");
                let temp_mc = self.read(&format!("{dir}/temp"), int).flatten()?;
                let mut trips = Vec::new();
                for i in 0..MAX_TRIPS {
                    let Some(kind) = self.read(&format!("{dir}/trip_point_{i}_type"), trip_kind)
                    else {
                        break;
                    };
                    let temp = self
                        .read(&format!("{dir}/trip_point_{i}_temp"), int)
                        .flatten();
                    if let (Some(kind), Some(t)) = (kind, temp)
                        && t > 0
                    {
                        trips.push((kind, t));
                    }
                }
                Some(Zone { temp_mc, trips })
            })
            .collect()
    }

    /// The CPUs' thermal throttle events since boot, summed: every present
    /// core's `thermal_throttle/core_throttle_count` and
    /// `package_throttle_count` (Intel), at most 256 cores. `None` where no
    /// core has the counters.
    #[must_use]
    pub fn throttle_count(&self) -> Option<u64> {
        let int = |t: &str| t.trim().parse::<u64>().ok();
        let mut sum: Option<u64> = None;
        for id in self.present_or_parallelism().into_iter().take(256) {
            for counter in ["core_throttle_count", "package_throttle_count"] {
                let rel = format!("sys/devices/system/cpu/cpu{id}/thermal_throttle/{counter}");
                if let Some(Some(n)) = self.read(&rel, int) {
                    sum = Some(sum.unwrap_or(0).saturating_add(n));
                }
            }
        }
        sum
    }

    /// The GPU, device-wide, from the DRM cards (`/sys/class/drm/cardN`, at
    /// most [`MAX_CARDS`]); a firmware framebuffer (`simpledrm`) is no GPU.
    /// Each card's driver comes from its device's `uevent` (`DRIVER=`).
    ///
    /// Nothing here wakes a card or holds one awake:
    ///
    /// * A card whose `power/runtime_status` reads `suspended` is
    ///   [`Why::Asleep`] and is not read further.
    /// * An awake amdgpu card answers `gpu_busy_percent` only where the read
    ///   cannot move its runtime power management ([`pm_settled`]): before
    ///   Linux's `get_if_active` sysfs change (6.8 LTS and older) that read
    ///   takes a runtime-PM reference and re-arms amdgpu's 5 s autosuspend
    ///   timer, so polling a hybrid laptop's dGPU once a second would keep
    ///   it from ever suspending. Elsewhere it is [`Why::NotRead`].
    /// * Every other driver answers why it cannot ([`gpu_unread`]).
    ///
    /// The answer is the first card with a figure; else, when every card is
    /// asleep, the first asleep one (known idle); else the first awake card,
    /// unread with its reason. An awake card nobody can read is never
    /// replaced by a sleeping one: on a laptop whose iGPU draws the desktop
    /// unreadably while its dGPU sleeps, the GPU is unknown, not idle.
    #[must_use]
    pub fn gpu(&self) -> Option<GpuReading> {
        let mut readings: Vec<GpuReading> = Vec::new();
        for (_, card) in self.numbered("sys/class/drm", "card", MAX_CARDS) {
            let dev = format!("sys/class/drm/{card}/device");
            let driver = self
                .read(&format!("{dev}/uevent"), |t| {
                    whole_lines(t)
                        .find_map(|l| l.strip_prefix("DRIVER="))
                        .map(str::to_owned)
                })
                .flatten()
                .or_else(|| {
                    std::fs::read_link(self.at(&format!("{dev}/driver")))
                        .ok()?
                        .file_name()?
                        .to_str()
                        .map(str::to_owned)
                });
            let Some(driver) = driver else {
                continue;
            };
            if FRAMEBUFFER_DRIVERS.contains(&driver.as_str()) {
                continue;
            }
            let word = |file: &str| {
                self.read(&format!("{dev}/{file}"), |t| t.trim().to_owned())
                    .unwrap_or_default()
            };
            let status = word("power/runtime_status");
            let mut reading = GpuReading {
                name: Some(driver.clone()),
                ..GpuReading::default()
            };
            if status == "suspended" {
                reading.unread = Some(Why::Asleep);
            } else if driver == "amdgpu" {
                if pm_settled(&status, &word("power/control"), &word("boot_vga")) {
                    reading.device_pct = self
                        .read(&format!("{dev}/gpu_busy_percent"), |t| {
                            t.trim().parse::<u32>().ok()
                        })
                        .flatten()
                        .map(|p| u8::try_from(p.min(100)).unwrap_or(100));
                    if reading.device_pct.is_none() {
                        reading.unread = Some(Why::Unsupported);
                    }
                } else {
                    reading.unread = Some(Why::NotRead);
                }
            } else {
                reading.unread = Some(gpu_unread(&driver));
            }
            readings.push(reading);
        }
        let asleep = |r: &GpuReading| r.unread == Some(Why::Asleep);
        let pick = readings
            .iter()
            .position(|r| r.device_pct.is_some())
            .or_else(|| readings.iter().position(|r| !asleep(r)))
            .unwrap_or(0);
        (pick < readings.len()).then(|| readings.swap_remove(pick))
    }

    /// Process `pid`'s footprint from its `status` ([`status_footprint`]):
    /// `Some(Some(bytes))`; `Some(None)` when the file is past
    /// [`STATUS_CAP`] (the footprint is unknown, and no other figure stands
    /// in for it); `None` when it is unreadable or names no anonymous
    /// memory (a kernel thread), for the caller's fallback. The common
    /// status fits one [`crate::READ_CAP`] stack buffer; a longer one (a
    /// user in thousands of groups) is read again whole, so its memory
    /// lines are never cut off.
    fn status_footprint(&self, pid: u32) -> Option<Option<u64>> {
        let path = self.at(&format!("proc/{pid}/status"));
        let mut buf = [0u8; crate::READ_CAP];
        let (n, cut) = crate::read_into(&path, &mut buf).ok()?;
        if !cut {
            return status_footprint(crate::utf8_prefix(&buf[..n])).map(Some);
        }
        let mut whole = vec![0u8; STATUS_CAP];
        match crate::read_into(&path, &mut whole).ok()? {
            (n, false) => status_footprint(crate::utf8_prefix(&whole[..n])).map(Some),
            (_, true) => Some(None),
        }
    }

    /// One sweep of `/proc`: each process's parent, name, CPU time (clock
    /// ticks at `hz`), state, group, whether it leads its session and has a
    /// terminal (`stat`, [`parse_stat`]), its owner (the owner of
    /// `/proc/<pid>`) and its memory (`status`'s `RssAnon + VmSwap`,
    /// [`status_footprint`]; else `statm`'s resident pages of `page_bytes`).
    /// At most [`MAX_PROCESSES`]. `None` where `/proc` cannot be listed.
    /// Every process's `stat` and `status` are readable by any user unless
    /// `/proc` is mounted `hidepid`, which hides other users'.
    #[must_use]
    pub fn processes(&self, hz: u64, page_bytes: u64) -> Option<Sweep> {
        let hz = hz.max(1);
        let mut procs = Vec::new();
        let mut truncated = false;
        for entry in std::fs::read_dir(self.at("proc")).ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if procs.len() >= MAX_PROCESSES {
                truncated = true;
                break;
            }
            let Some(stat) = self.read(&format!("proc/{pid}/stat"), parse_stat).flatten() else {
                continue;
            };
            let read_at = Some(std::time::Instant::now());
            let footprint = self.status_footprint(pid).unwrap_or_else(|| {
                self.read(&format!("proc/{pid}/statm"), parse_statm_resident)
                    .flatten()
                    .map(|pages| pages.saturating_mul(page_bytes))
            });
            let ticks = stat.utime.saturating_add(stat.stime);
            let ns = u128::from(ticks) * 1_000_000_000 / u128::from(hz);
            let Some(confirm) = self.read(&format!("proc/{pid}/stat"), parse_stat).flatten() else {
                continue;
            };
            if confirm.start != stat.start {
                continue;
            }
            procs.push(ProcessReading {
                pid,
                start: u64::try_from(u128::from(stat.start) * 1_000_000_000 / u128::from(hz)).ok(),
                ppid: stat.ppid,
                cpu_ns: Some(u64::try_from(ns).unwrap_or(u64::MAX)),
                footprint,
                disk_read: None,
                disk_written: None,
                name: stat.name,
                pgid: stat.pgrp,
                uid: owner(&entry),
                state: RunState::from_linux(stat.state),
                session_leader: Some(stat.session == pid),
                has_tty: Some(stat.tty_nr != 0),
                runnable_ns: None,
                peak_footprint: None,
                read_at,
            });
        }
        Some(Sweep { procs, truncated })
    }

    /// What this tree's readers can say ([`Caps`]): each row from a probe of
    /// the file it reads.
    #[must_use]
    pub fn caps(&self) -> Caps {
        let readable = |rel: &str| self.read(rel, |_| ()).is_some();
        let yes_if = |ok: bool, why: Why| if ok { Cap::Yes } else { Cap::No(why) };
        let meminfo = self.read("proc/meminfo", memory_detail).flatten();
        let others = if readable("proc/1/stat") {
            Cap::Yes
        } else {
            // `hidepid`: other users' processes are hidden, so sums over
            // them are lower bounds.
            Cap::Estimate
        };
        let has_zones = self.zones().iter().any(|z| !z.trips.is_empty());
        Caps {
            os: "linux",
            per_core: yes_if(readable("proc/stat"), Why::Unsupported),
            core_kinds: if self.present().is_some() {
                Cap::Yes
            } else {
                Cap::Estimate
            },
            cores_withdrawn: yes_if(self.online().is_some(), Why::Unsupported),
            memory_classes: if meminfo.is_some() {
                Cap::Estimate
            } else {
                Cap::No(Why::Unsupported)
            },
            pressure: match self.psi() {
                Ok(_) => Cap::Yes,
                Err(why) => Cap::No(why),
            },
            commit: if meminfo.as_ref().is_some_and(|m| m.commit.is_some()) {
                Cap::Estimate
            } else {
                Cap::No(Why::Unsupported)
            },
            compressor: match self.zswap_enabled() {
                Some(true) => Cap::Yes,
                Some(false) => Cap::No(Why::NotEnabled("zswap")),
                None => Cap::No(Why::Unsupported),
            },
            swap: yes_if(meminfo.is_some(), Why::Unsupported),
            gpu_device: crate::caps::gpu_cap(self.gpu().as_ref()),
            gpu_per_proc: Cap::No(Why::NotRead),
            proc_cpu_others: others,
            proc_memory_others: others,
            throttle: if has_zones || self.throttle_count().is_some() {
                Cap::Estimate
            } else {
                Cap::No(Why::Absent)
            },
            volume: Cap::Yes,
            capacity: self.capacity().source,
        }
    }
}

/// The reader behind [`crate::Probe::throttle`] on Linux: a [`Root`] and
/// the last throttle count it read, so a reading can say whether the
/// counters rose.
#[derive(Debug)]
pub struct ThrottleReader {
    root: Root,
    last: Option<u64>,
}

impl ThrottleReader {
    /// A reader of `root` that has read nothing.
    #[must_use]
    pub fn new(root: Root) -> Self {
        Self { root, last: None }
    }

    /// One reading ([`throttle_level`] over [`Root::zones`] and whether
    /// [`Root::throttle_count`] rose since the last reading; the first
    /// reading has nothing to compare and counts as no rise).
    ///
    /// # Errors
    ///
    /// [`Why::Absent`] on a machine with no trip point and no counter.
    pub fn sample(&mut self) -> Result<Throttle, Why> {
        let count = self.root.throttle_count();
        let rose = count.map(|now| self.last.is_some_and(|before| now > before));
        self.last = count;
        throttle_level(&self.root.zones(), rose).map(|level| Throttle {
            level,
            exact: false,
        })
    }
}
