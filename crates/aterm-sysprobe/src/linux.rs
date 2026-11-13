// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux `/proc` and `/sys` parsers (pure functions over `&str` and `&[u8]`) and [`Root`] readers (std I/O under a configurable root directory).

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

#[path = "linux_tcp.rs"]
pub mod tcp;

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

/// The value of a `Key:   123 kB` / `key 123` line.
fn keyed(text: &str, key: &str) -> Option<u64> {
    whole_lines(text).find_map(|l| {
        let mut it = l.split_whitespace();
        if it.next()?.trim_end_matches(':') != key {
            return None;
        }
        crate::parse_uint(it.next()?)
    })
}

/// `MemTotal` from `/proc/meminfo`, KiB: the machine's physical memory. Its available memory is
/// [`memory_detail`]'s (`MemAvailable`), which nothing here reads.
#[must_use]
pub fn mem_total_kib(text: &str) -> Option<u64> {
    keyed(text, "MemTotal")
}

/// `pswpin + pswpout` from `/proc/vmstat` (pages).
#[must_use]
pub fn swap_pages(vmstat: &str) -> Option<u64> {
    Some(keyed(vmstat, "pswpin")?.saturating_add(keyed(vmstat, "pswpout")?))
}

/// One value of a binary auxiliary vector (`/proc/self/auxv`), ended by `AT_NULL` (`0`).
#[must_use]
pub fn auxv_value(bytes: &[u8], key: u64) -> Option<u64> {
    const W: usize = std::mem::size_of::<usize>();
    let word = |b: &[u8]| -> u64 {
        let mut a = [0u8; W];
        a.copy_from_slice(b);
        u64::try_from(usize::from_ne_bytes(a)).unwrap_or(u64::MAX)
    };
    let (pairs, _) = bytes.as_chunks::<{ 2 * W }>();
    pairs
        .iter()
        .take_while(|pair| word(&pair[..W]) != 0)
        .find_map(|pair| {
            let k = word(&pair[..W]);
            (k == key).then(|| word(&pair[W..]))
        })
}

/// The process group (`pgrp`, the fifth field) of a `/proc/<pid>/stat` line.
#[must_use]
pub fn stat_pgrp(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    crate::parse_uint(fields.nth(2)?)
}

/// The pids of a `/proc/<pid>/task/<tid>/children` text.
#[must_use]
pub fn children(text: &str) -> Vec<u32> {
    text.split_inclusive(' ')
        .filter_map(|word| crate::parse_uint(word.strip_suffix(' ')?))
        .collect()
}

/// The most logical cores any reader here places (ids `0..MAX_CPUS`).
pub const MAX_CPUS: usize = 4096;

/// The most bytes of `/proc/stat` one [`StatReader::sample`] takes: room for the per-core lines of about 800 cores.
pub const STAT_CAP: usize = 64 * 1024;

/// The bytes of a process's name `comm` keeps: 15 (`TASK_COMM_LEN` less its NUL).
const COMM_CUT: usize = 15;

/// Every core's ticks from `/proc/stat`'s `cpuN` lines, indexed by `N` (a core missing between two others, which the kernel leaves out while it is offline, reads all zero).
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
        let id: usize = crate::parse_uint(id)?;
        if id >= MAX_CPUS {
            return None;
        }
        let mut f = [0u64; 7];
        let mut n = 0;
        for (slot, field) in f.iter_mut().zip(fields.by_ref()) {
            *slot = crate::parse_uint(field)?;
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
    let k = torn.len().min(3);
    let torn_is_other = !torn.is_empty() && torn.as_bytes()[..k] != b"cpu"[..k];
    if cut && !ended && !torn_is_other {
        return None;
    }
    (!cores.is_empty()).then_some(cores)
}

/// The reader behind [`crate::Probe::core_ticks`] and [`crate::Probe::forks`] on Linux: one
/// read of `/proc/stat` gives both.
#[derive(Debug)]
pub struct StatReader {
    path: PathBuf,
    buf: Vec<u8>,
    present: usize,
    forks: Option<u64>,
    blocked: Option<u64>,
}

impl StatReader {
    /// A reader of `<root>/proc/stat` that pads its answer to `present` cores (at most [`MAX_CPUS`]).
    #[must_use]
    pub fn new(root: &Path, present: usize) -> Self {
        Self {
            path: root.join("proc/stat"),
            buf: vec![0; STAT_CAP],
            present: present.min(MAX_CPUS),
            forks: None,
            blocked: None,
        }
    }

    /// One reading; `None` when the file cannot be read or parsed. The same read's `processes`
    /// and `procs_blocked` lines are kept for [`Self::forks`] and [`Self::blocked`].
    pub fn sample(&mut self) -> Option<Vec<CoreTicks>> {
        self.forks = None;
        self.blocked = None;
        let (n, cut) = crate::read_into(&self.path, &mut self.buf).ok()?;
        let text = crate::utf8_prefix(&self.buf[..n]);
        self.forks = proc_stat_forks(text);
        self.blocked = proc_stat_count(text, "procs_blocked");
        let mut cores = proc_stat_cores(text, cut)?;
        if cores.len() < self.present {
            cores.resize(self.present, CoreTicks::default());
        }
        Some(cores)
    }

    /// Every `fork` and `clone` since boot as the last [`Self::sample`] read them
    /// ([`proc_stat_forks`]); `None` before one, or where that read held no whole `processes`
    /// line.
    #[must_use]
    pub const fn forks(&self) -> Option<u64> {
        self.forks
    }

    /// The threads in uninterruptible sleep now, every user's (`procs_blocked`: state `D`), as the
    /// last [`Self::sample`] read them; `None` before one, or where that read held no whole line.
    #[must_use]
    pub const fn blocked(&self) -> Option<u64> {
        self.blocked
    }
}

/// `/proc/stat`'s `processes` line: every `fork` and `clone` since boot. Only a whole line
/// counts, so a read cut inside it gives `None`.
#[must_use]
pub fn proc_stat_forks(text: &str) -> Option<u64> {
    proc_stat_count(text, "processes")
}

/// `/proc/stat`'s one-number line `key` (`processes`, `procs_blocked`). Only a whole line counts,
/// so a read cut inside it gives `None`.
#[must_use]
pub fn proc_stat_count(text: &str, key: &str) -> Option<u64> {
    whole_lines(text).find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next()? == key)
            .then(|| {
                let n = crate::parse_uint(fields.next()?)?;
                fields.next().is_none().then_some(n)
            })
            .flatten()
    })
}

/// A kernel CPU list (`0-3,5,8-11`, as `/sys/devices/system/cpu/present` holds it) as ascending, distinct ids.
#[must_use]
pub fn cpulist(text: &str) -> Option<Vec<u16>> {
    let mut ids = Vec::new();
    for part in text.trim().split(',') {
        let (lo, hi) = part.split_once('-').unwrap_or((part, part));
        let (lo, hi): (u16, u16) = (crate::parse_uint(lo.trim())?, crate::parse_uint(hi.trim())?);
        if lo > hi || usize::from(hi) >= MAX_CPUS {
            return None;
        }
        ids.extend(lo..=hi);
    }
    ids.sort_unstable();
    ids.dedup();
    (!ids.is_empty()).then_some(ids)
}

/// [`MemoryDetail`] from `/proc/meminfo` (KiB × 1024): `None` without `MemTotal`, `MemFree` or
/// `MemAvailable` (the kernel's own estimate of what it can hand out without swapping, which
/// [`MemoryDetail::available`] is here, held to `MemTotal`).
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
    let available = bytes("MemAvailable")?.min(total);
    let shared = bytes("Shmem");
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
        available,
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
        memorystatus_level_pct: None,
        exact: false,
    })
}

/// `/proc/cpuinfo`'s first `model name` (x86) or `Model` (ARM boards).
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

/// `/etc/os-release`'s `PRETTY_NAME`, unquoted.
#[must_use]
pub fn os_pretty_name(os_release: &str) -> Option<String> {
    os_release.lines().find_map(|line| {
        let value = line.strip_prefix("PRETTY_NAME=")?;
        let value = value.trim().trim_matches('"').trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// The most bytes of `/proc/net/dev` one [`links_under_into`] takes: room for about 500 interfaces.
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

/// Every interface's counters from `/proc/net/dev`.
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
                *slot = crate::parse_uint(field)?;
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

/// `/sys/class/net/<if>/flags` (`0x1003`) as its bits; `None` where it does not parse.
#[must_use]
pub fn hex_flags(text: &str) -> Option<u32> {
    let t = text.trim();
    let digits = t.strip_prefix("0x").unwrap_or(t);
    digits
        .bytes()
        .all(|b| b.is_ascii_hexdigit())
        .then(|| u32::from_str_radix(digits, 16).ok())
        .flatten()
}

/// Whether the interface is up (`IFF_UP` set and `operstate` is `up` or `unknown`).
#[must_use]
pub fn link_up(flags: u32, operstate: &str) -> bool {
    flags & IFF_UP != 0 && matches!(operstate.trim(), "up" | "unknown")
}

/// `/sys/class/net/<if>/speed` (Mb/s) as bits per second.
#[must_use]
pub fn speed_bps(text: &str) -> Option<u64> {
    let mbps: i64 = crate::parse_int(text.trim())?;
    u64::try_from(mbps)
        .ok()
        .filter(|m| *m > 0)
        .map(|m| m.saturating_mul(1_000_000))
}

/// A link's kind from what sysfs says of it.
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

/// Every interface under `root` ([`crate::net_links_into`] on Linux, with root `/`).
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
            crate::with_bounded(p, |t| crate::parse_uint(t.trim()))
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

/// `nlmsghdr`'s size, `NLMSG_HDRLEN` (`linux/netlink.h`).
const NLMSG_HDRLEN: usize = 16;
/// `NLMSG_NOOP` (`linux/netlink.h`): a message to skip.
const NLMSG_NOOP: u16 = 1;
/// `NLMSG_ERROR` (`linux/netlink.h`): an `int` errno (negative) or an acknowledgement (0).
const NLMSG_ERROR: u16 = 2;
/// `NLMSG_DONE` (`linux/netlink.h`): a dump's end, its payload the dump's `int` result.
const NLMSG_DONE: u16 = 3;
/// `NLMSG_OVERRUN` (`linux/netlink.h`): data was lost.
const NLMSG_OVERRUN: u16 = 4;
/// `NLM_F_REQUEST` (`linux/netlink.h`).
const NLM_F_REQUEST: u16 = 0x1;
/// `NLM_F_DUMP`, `NLM_F_ROOT | NLM_F_MATCH` (`linux/netlink.h`).
const NLM_F_DUMP: u16 = 0x300;
/// `NLM_F_DUMP_INTR` (`linux/netlink.h`): the table changed while the dump ran.
const NLM_F_DUMP_INTR: u16 = 0x10;
/// `SOCK_DIAG_BY_FAMILY` (`linux/sock_diag.h`): a dump request's type and each record's.
const SOCK_DIAG_BY_FAMILY: u16 = 20;
/// `nlattr`'s header size, `NLA_HDRLEN` (`linux/netlink.h`).
const NLA_HDRLEN: usize = 4;
/// `NLA_TYPE_MASK` (`linux/netlink.h`): an attribute's type without its two flag bits.
const NLA_TYPE_MASK: u16 = 0x3fff;
const INET_DIAG_MSG_LEN: usize = 72;
const INET_DIAG_DST_AT: usize = 24;
const INET_DIAG_INFO_ATTR: u16 = 2;
/// Byte offset of `tcpi_rtt` (`u32`, microseconds) inside Linux UAPI `struct tcp_info`.
const TCPI_RTT_AT: usize = 68;
const LINUX_AF_INET: u8 = 2;
const LINUX_AF_INET6: u8 = 10;
/// `AF_UNIX` (`linux/socket.h`), the family a unix dump names and every record of it carries.
const LINUX_AF_UNIX: u8 = 1;
/// `TCP_LISTEN` (`net/tcp_states.h`), the state a listening `AF_UNIX` socket is in.
const LINUX_TCP_LISTEN: u8 = 10;
/// `UDIAG_SHOW_NAME` (`linux/unix_diag.h`): each record carries its bound name.
const UDIAG_SHOW_NAME: u32 = 0x1;
/// `UDIAG_SHOW_VFS` (`linux/unix_diag.h`): each record carries the file its bind made.
const UDIAG_SHOW_VFS: u32 = 0x2;
/// `UNIX_DIAG_NAME` (`linux/unix_diag.h`): the attribute holding the bound name.
const UNIX_DIAG_NAME: u16 = 0;
/// `UNIX_DIAG_VFS` (`linux/unix_diag.h`): the attribute holding a `unix_diag_vfs`, the bound
/// file's inode then its device, two `u32`s.
const UNIX_DIAG_VFS: u16 = 1;
/// `unix_diag_msg`'s size (`linux/unix_diag.h`): family, type, state, pad, inode, cookie.
const UNIX_DIAG_MSG_LEN: usize = 16;

/// `n` rounded up to the 4 bytes netlink aligns every message and attribute to (`NLMSG_ALIGN`,
/// `NLA_ALIGN`).
const fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// The `u16` at `at`, in host byte order as netlink writes it.
fn ne_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

/// The `u32` at `at`, in host byte order.
fn ne_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// A `SOCK_DIAG_BY_FAMILY` dump request of `N` bytes under sequence `seq`: its `nlmsghdr`
/// (length `N`, `NLM_F_REQUEST | NLM_F_DUMP`, port 0), then zeros for the family's request.
fn sock_diag_dump_request<const N: usize>(seq: u32) -> [u8; N] {
    let mut r = [0_u8; N];
    r[0..4].copy_from_slice(&u32::try_from(N).unwrap_or(u32::MAX).to_ne_bytes());
    r[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    r[6..8].copy_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
    r[8..12].copy_from_slice(&seq.to_ne_bytes());
    r
}

/// One message of a netlink datagram: its header's type, flags and sequence, and its payload.
struct NlMessage<'a> {
    kind: u16,
    flags: u16,
    seq: u32,
    payload: &'a [u8],
}

/// The messages of one netlink datagram in order, each read by its header's length, the next
/// one at that length rounded up to 4 (`NLMSG_ALIGN`). An `Err` ends the walk: a header cut
/// short, or a length under a header or past the datagram, framing the kernel never writes.
fn nl_messages(datagram: &[u8]) -> impl Iterator<Item = Result<NlMessage<'_>, &'static str>> {
    let mut rest = datagram;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let header = (
            ne_u32(rest, 0),
            ne_u16(rest, 4),
            ne_u16(rest, 6),
            ne_u32(rest, 8),
        );
        let (Some(len), Some(kind), Some(flags), Some(seq)) = header else {
            rest = &[];
            return Some(Err("a message header cut short"));
        };
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        if len < NLMSG_HDRLEN || len > rest.len() {
            rest = &[];
            return Some(Err("a message length past its datagram"));
        }
        let payload = &rest[NLMSG_HDRLEN..len];
        rest = rest.get(align4(len)..).unwrap_or_default();
        Some(Ok(NlMessage {
            kind,
            flags,
            seq,
            payload,
        }))
    })
}

/// The attributes after a record's fixed part in order, `(type, value)` with the type's two flag
/// bits taken off (`NLA_TYPE_MASK`), each read by its length, the next one at that length rounded
/// up to 4 (`NLA_ALIGN`). An `Err` ends the walk: a header cut short, or a length under a header
/// or past the record.
fn nl_attributes(attrs: &[u8]) -> impl Iterator<Item = Result<(u16, &[u8]), &'static str>> {
    let mut rest = attrs;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (Some(len), Some(kind)) = (ne_u16(rest, 0), ne_u16(rest, 2)) else {
            rest = &[];
            return Some(Err("an attribute header cut short"));
        };
        let len = usize::from(len);
        if len < NLA_HDRLEN || len > rest.len() {
            rest = &[];
            return Some(Err("an attribute past its record"));
        }
        let value = &rest[NLA_HDRLEN..len];
        rest = rest.get(align4(len)..).unwrap_or_default();
        Some(Ok((kind & NLA_TYPE_MASK, value)))
    })
}

fn inet_diag_remote_peer(family: u8, dst: &[u8]) -> bool {
    if dst.len() < 16 {
        return false;
    }
    match family {
        LINUX_AF_INET => dst[0] != 127 && dst[..4] != [0, 0, 0, 0],
        LINUX_AF_INET6 => {
            if dst[..16] == [0u8; 16] {
                return false;
            }
            if dst[..15] == [0u8; 15] && dst[15] == 1 {
                return false;
            }
            if dst[..10] == [0u8; 10]
                && dst[10..12] == [0xff, 0xff]
                && (dst[12] == 127 || dst[12..16] == [0, 0, 0, 0])
            {
                return false;
            }
            true
        }
        _ => false,
    }
}

/// The request for a dump of every listening `AF_UNIX` socket with its name and bound file
/// (`sock_diag(7)`): one `nlmsghdr` of type `SOCK_DIAG_BY_FAMILY` with
/// `NLM_F_REQUEST | NLM_F_DUMP` and sequence `seq`, then a `unix_diag_req` for family `AF_UNIX`,
/// states `1 << TCP_LISTEN`, `UDIAG_SHOW_NAME | UDIAG_SHOW_VFS`, and no cookie.
#[must_use]
pub fn unix_diag_request(seq: u32) -> [u8; 40] {
    let mut r = sock_diag_dump_request::<40>(seq);
    r[NLMSG_HDRLEN] = LINUX_AF_UNIX;
    r[20..24].copy_from_slice(&(1_u32 << LINUX_TCP_LISTEN).to_ne_bytes());
    r[28..32].copy_from_slice(&(UDIAG_SHOW_NAME | UDIAG_SHOW_VFS).to_ne_bytes());
    r[32..40].fill(0xff);
    r
}

/// What one datagram of the socket dump said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dump {
    /// Its records were read, and the dump goes on in the next datagram.
    More,
    /// The dump ended here (`NLMSG_DONE`).
    Done,
}

/// Why a socket dump cannot be read whole: a dump read in part proves no absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpFault {
    /// A message the kernel does not write: a length past its datagram, another request's
    /// sequence, a record of another family, an attribute past its record, an overrun.
    Torn(&'static str),
    /// The kernel refused the request: `NLMSG_ERROR`, or `NLMSG_DONE` carrying an errno.
    Refused(i32),
    /// The kernel marked the dump interrupted (`NLM_F_DUMP_INTR`): the table changed under it.
    Interrupted,
}

/// A listening `AF_UNIX` socket bound at a path, as one record of the socket dump names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnixDiagListener {
    /// The socket's inode, which a `/proc/<pid>/fd` link names it by (`socket:[<inode>]`).
    pub inode: u64,
    /// The name it was bound at: the attribute's bytes before the first NUL.
    pub path: Vec<u8>,
    /// The file its bind made, `None` when the record carries none.
    pub file: Option<BoundFile>,
}

/// The file a listener's bind made (`UNIX_DIAG_VFS`), which every name linked to it shares, by
/// its inode. The record's device is not kept: it is the superblock's (`s_dev`), which on btrfs
/// is no file's `st_dev`, as `stat(2)` there reports each subvolume's own device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundFile {
    /// The low 32 bits of its inode number: the record keeps no more.
    pub ino: u32,
}

impl BoundFile {
    /// Whether `ino`, an inode number as `stat(2)` reports one, is this file's, by its low 32 bits.
    #[must_use]
    pub fn is_inode(&self, ino: u64) -> bool {
        u64::from(self.ino) == ino & u64::from(u32::MAX)
    }
}

/// One `SOCK_DIAG_BY_FAMILY` record's payload: `Some` for a listener bound at a path, `None` for
/// any other socket.
fn unix_diag_record(record: &[u8]) -> Result<Option<UnixDiagListener>, DumpFault> {
    let msg = record
        .get(..UNIX_DIAG_MSG_LEN)
        .ok_or(DumpFault::Torn("a socket record cut short"))?;
    if msg[0] != LINUX_AF_UNIX {
        return Err(DumpFault::Torn("a socket record of another family"));
    }
    let state = msg[2];
    let inode = ne_u32(msg, 4).ok_or(DumpFault::Torn("a socket record cut short"))?;
    let (mut name, mut file) = (None, None);
    for attr in nl_attributes(&record[UNIX_DIAG_MSG_LEN..]) {
        let (kind, value) = attr.map_err(DumpFault::Torn)?;
        if kind == UNIX_DIAG_NAME && name.is_none() {
            let end = value.iter().position(|b| *b == 0).unwrap_or(value.len());
            name = Some(value[..end].to_vec());
        }
        if kind == UNIX_DIAG_VFS && file.is_none() {
            let (Some(ino), 8) = (ne_u32(value, 0), value.len()) else {
                return Err(DumpFault::Torn("a bound file attribute of another size"));
            };
            file = Some(BoundFile { ino });
        }
    }
    Ok(name
        .filter(|name| state == LINUX_TCP_LISTEN && !name.is_empty())
        .map(|path| UnixDiagListener {
            inode: u64::from(inode),
            path,
            file,
        }))
}

/// One datagram of the answer to request `seq`, its records' listeners appended to `into` when
/// the whole datagram reads: a `SOCK_DIAG_BY_FAMILY` record in state `TCP_LISTEN` whose
/// `UNIX_DIAG_NAME` attribute holds a path, with the file its `UNIX_DIAG_VFS` attribute names. A
/// name is its attribute's bytes before the first NUL (the kernel's copy of `sun_path` ends with
/// one), whatever bytes they are: each record and attribute is length-delimited, so no socket name
/// can make a second record, which `/proc/net/unix`'s text allows. An abstract name (a leading
/// NUL) and a record with no name are left out.
///
/// # Errors
///
/// A [`DumpFault`]: the datagram is empty or torn (a file attribute not 8 bytes long included),
/// answers another request, carries an error or an overrun, or marks the dump interrupted.
/// Nothing is appended then.
pub fn unix_diag_listeners(
    datagram: &[u8],
    seq: u32,
    into: &mut Vec<UnixDiagListener>,
) -> Result<Dump, DumpFault> {
    if datagram.is_empty() {
        return Err(DumpFault::Torn("an empty datagram"));
    }
    let mut read = Vec::new();
    let mut dump = Dump::More;
    for message in nl_messages(datagram) {
        let message = message.map_err(DumpFault::Torn)?;
        if message.seq != seq {
            return Err(DumpFault::Torn("a message of another request"));
        }
        if message.flags & NLM_F_DUMP_INTR != 0 {
            return Err(DumpFault::Interrupted);
        }
        let errno = || {
            message
                .payload
                .get(..4)
                .and_then(|b| Some(i32::from_ne_bytes(b.try_into().ok()?)))
                .ok_or(DumpFault::Torn("an error or end cut short"))
        };
        match message.kind {
            NLMSG_NOOP => {}
            NLMSG_ERROR => match errno()? {
                e if e < 0 => return Err(DumpFault::Refused(e.saturating_neg())),
                _ => return Err(DumpFault::Torn("an acknowledgement nobody asked for")),
            },
            NLMSG_DONE => match errno()? {
                e if e < 0 => return Err(DumpFault::Refused(e.saturating_neg())),
                _ => {
                    dump = Dump::Done;
                    break;
                }
            },
            NLMSG_OVERRUN => return Err(DumpFault::Torn("an overrun")),
            SOCK_DIAG_BY_FAMILY => read.extend(unix_diag_record(message.payload)?),
            _ => return Err(DumpFault::Torn("a message of another type")),
        }
    }
    into.append(&mut read);
    Ok(dump)
}

/// One network namespace's TCP sockets by inode.
pub type TcpSockets = std::sync::Arc<std::collections::HashMap<u64, crate::jobs::linux::TcpRow>>;

/// Count the descriptors inside a `/proc/<pid>/fd` directory (at most 4096) that link to one of
/// `sockets`, the TCP sockets of the newest dump ([`tcp::TcpDump::sockets`]): a Unix or netlink
/// socket is no network socket. The dump names this network namespace's sockets alone, the same
/// namespace as the machine total the counts are shares of, so a socket of a process in another
/// namespace counts in neither.
#[must_use]
pub fn tcp_sockets_in_fd_dir(
    fd_dir: &Path,
    sockets: &std::collections::HashMap<u64, crate::jobs::linux::TcpRow>,
) -> Option<u32> {
    let entries = std::fs::read_dir(fd_dir).ok()?;
    let mut count = 0u32;
    for entry in entries.flatten().take(4096) {
        if let Ok(target) = std::fs::read_link(entry.path())
            && let Some(inode) = target
                .to_str()
                .and_then(|t| t.strip_prefix("socket:[")?.strip_suffix(']'))
                .and_then(crate::parse_uint::<u64>)
            && sockets.contains_key(&inode)
        {
            count = count.saturating_add(1);
        }
    }
    Some(count)
}

/// The kernel's count of every thread on the machine, kernel threads included.
#[must_use]
pub fn loadavg_tasks(text: &str) -> Option<u64> {
    loadavg_counts(text).map(|(_, total)| total)
}

/// `/proc/loadavg`'s fourth field, `running/total`: the threads runnable now (the reader among
/// them) and every thread, kernel threads included; `None` unless both read and the total is
/// positive.
#[must_use]
pub fn loadavg_counts(text: &str) -> Option<(u64, u64)> {
    let line = whole_lines(text).next()?;
    let (running, total) = line.split_whitespace().nth(3)?.split_once('/')?;
    let total: u64 = crate::parse_uint(total)?;
    let running: u64 = crate::parse_uint(running)?;
    (total > 0).then_some((running, total))
}

/// One PSI file's ten-second averages (`/proc/pressure/memory`): the share of wall time some task, and every task, stalled on memory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Psi {
    /// `some avg10`, percent.
    pub some_avg10: f32,
    /// `full avg10`, percent.
    pub full_avg10: f32,
}

/// `some avg10` at or past this is [`MemoryLevel::Warn`]: Android lmkd's default partial-stall threshold (70 ms of each second).
pub const PSI_WARN_SOME_AVG10: f32 = 7.0;

/// `full avg10` at or past this is [`MemoryLevel::Critical`].
pub const PSI_CRITICAL_FULL_AVG10: f32 = 60.0;

/// A PSI file's `some` and `full` ten-second averages (`some avg10=1.25 avg60=… total=…`).
#[must_use]
pub fn psi(text: &str) -> Option<Psi> {
    let avg10 = |kind: &str| {
        whole_lines(text).find_map(|l| {
            let mut it = l.split_whitespace();
            if it.next()? != kind {
                return None;
            }
            let raw = it.find_map(|f| f.strip_prefix("avg10="))?;
            raw.parse::<f32>()
                .ok()
                .filter(|v| !raw.starts_with('+') && v.is_finite() && *v >= 0.0)
        })
    };
    Some(Psi {
        some_avg10: avg10("some")?,
        full_avg10: avg10("full")?,
    })
}

/// The pressure level PSI reads: critical at [`PSI_CRITICAL_FULL_AVG10`], warn at [`PSI_WARN_SOME_AVG10`], else normal.
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

/// Core layout from present/online cores, hybrid PMU core lists, and per-core `cpu_capacity`, fastest first.
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

/// One kind of core: its design (`midr_el1`'s implementer and part number,
/// [`MIDR_DESIGN`]) and its top clock (`cpuinfo_max_freq`, kHz, where cpufreq
/// says).
pub type Design = (u64, Option<u64>);

/// `midr_el1`'s implementer (bits 31..24) and part number (bits 15..4): the
/// design, without its variant, architecture and revision.
pub const MIDR_DESIGN: u64 = 0xff00_fff0;

/// `capacities` with each core raised to the highest capacity among the
/// cores of its [`Design`], so one design at one clock is one level however
/// firmware spreads its `cpu_capacity`. Unchanged unless every core in
/// `capacities` has a design.
#[must_use]
pub fn capacities_by_design(
    capacities: &[(u16, u32)],
    designs: &[(u16, Design)],
) -> Vec<(u16, u32)> {
    let design = |id: u16| designs.iter().find(|(d, _)| *d == id).map(|(_, k)| *k);
    if capacities.iter().any(|(id, _)| design(*id).is_none()) {
        return capacities.to_vec();
    }
    capacities
        .iter()
        .map(|&(id, own)| {
            let kind = design(id);
            let top = capacities
                .iter()
                .filter(|(other, _)| design(*other) == kind)
                .map(|(_, c)| *c)
                .fold(own, u32::max);
            (id, top)
        })
        .collect()
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

/// The process's cgroup v2 path, from `/proc/self/cgroup`'s `0::<path>` line.
#[must_use]
pub fn cgroup_v2_path(text: &str) -> Option<&str> {
    let path = whole_lines(text).find_map(|l| l.strip_prefix("0::"))?;
    (path.starts_with('/') && !path.split('/').any(|c| c == ".." || c == ".")).then_some(path)
}

/// `cpu.max` (`<quota> <period>` in µs, or `max <period>`) as thousandths of a core.
#[must_use]
pub fn cpu_max_millicores(text: &str) -> Option<Option<u32>> {
    let mut it = text.split_whitespace();
    let quota = it.next()?;
    let period: u64 = crate::parse_uint(it.next()?).filter(|p| *p > 0)?;
    if quota == "max" {
        return Some(None);
    }
    let quota: u64 = crate::parse_uint(quota)?;
    let milli = u128::from(quota) * 1000 / u128::from(period);
    Some(Some(u32::try_from(milli).unwrap_or(u32::MAX)))
}

/// `memory.max` (bytes, or `max`): `Some(None)` for no limit, `None` for a malformed text.
#[must_use]
pub fn memory_max(text: &str) -> Option<Option<u64>> {
    let word = text.split_whitespace().next()?;
    if word == "max" {
        return Some(None);
    }
    crate::parse_uint(word).map(Some)
}

/// The most thermal zones [`Root::zones`] reads.
pub const MAX_ZONES: usize = 32;
/// The most trip points read per zone.
pub const MAX_TRIPS: usize = 16;

/// A thermal trip point the throttle level reads (`active` trips start fans and are left out).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TripKind {
    /// The kernel starts slowing the cores (passive cooling).
    Passive,
    /// Hot: the platform's last warning before critical.
    Hot,
    /// Critical: the kernel shuts the machine down.
    Critical,
}

/// A `trip_point_N_type` text as a [`TripKind`]; `None` for `active` and anything else.
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
    /// Its passive, hot and critical trip points (a trip at or below 0 °C, which some firmware writes for a disabled trip, is left out).
    pub trips: Vec<(TripKind, i64)>,
}

/// The heuristic throttle level from the thermal zones and whether the CPUs' thermal throttle counters rose since the last reading (`None`: the machine has no such counters).
///
/// # Errors
/// [`Why::Absent`] when no zone has a trip point and there are no counters (most virtual machines): nothing to judge from.
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

/// A process's user (`Uid:`'s first field, its real uid) from `/proc/<pid>/status`.
#[must_use]
pub fn status_uid(text: &str) -> Option<u32> {
    keyed(text, "Uid").and_then(|u| u32::try_from(u).ok())
}

/// A process's memory from `/proc/<pid>/status`: its anonymous pages in memory and swapped
/// (`RssAnon + VmSwap`), bytes.
#[must_use]
pub fn status_footprint(text: &str) -> Option<u64> {
    status_memory(text).map(|(footprint, _)| footprint)
}

/// A process's memory from `/proc/<pid>/status`, bytes: its footprint ([`status_footprint`])
/// and its anonymous pages in memory now (`RssAnon`), its resident figure: below the footprint
/// by what was swapped. `None` where it names no anonymous memory (a kernel thread).
#[must_use]
pub fn status_memory(text: &str) -> Option<(u64, u64)> {
    let anon = keyed(text, "RssAnon")?;
    let swap = keyed(text, "VmSwap").unwrap_or(0);
    Some((
        anon.saturating_add(swap).saturating_mul(1024),
        anon.saturating_mul(1024),
    ))
}

/// The most bytes of one `/proc/<pid>/status` [`Root::processes`] reads.
pub const STATUS_CAP: usize = 1 << 20;

/// The most DRM cards [`Root::gpu`] reads.
pub const MAX_CARDS: usize = 16;

/// DRM drivers that drive a firmware framebuffer, not a GPU: a card of one
/// is no GPU.
const FRAMEBUFFER_DRIVERS: &[&str] = &[
    "simpledrm",
    "simple-framebuffer",
    "efidrm",
    "vesadrm",
    "ofdrm",
    "vkms",
];

/// Why a DRM driver publishes no device-wide busy figure [`Root::gpu`]
/// reads (amdgpu's sysfs file and NVIDIA's NVML are read there).
fn gpu_unread(driver: &str) -> Why {
    match driver {
        "i915" | "xe" => Why::NeedsPrivilege("cap_perfmon"),
        _ => Why::Unsupported,
    }
}

/// One NVIDIA card as NVML reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvidiaCard {
    /// Its device-wide busy percent (at most 100), or why it is unread.
    pub busy: Result<u8, Why>,
    /// Its board power draw, milliwatts (`nvmlDeviceGetPowerUsage`), or why it is unread.
    pub power_mw: Result<u32, Why>,
    /// NVML's own name for it (`NVIDIA GB10`), where NVML gave one.
    pub name: Option<String>,
    /// Whether it works in the system's memory ([`GpuReading::unified`]).
    pub unified: bool,
}

impl NvidiaCard {
    /// A card NVML did not read, for `why`: no busy figure, no name, no memory fact.
    #[must_use]
    pub const fn unread(why: Why) -> Self {
        Self {
            busy: Err(why),
            power_mw: Err(why),
            name: None,
            unified: false,
        }
    }
}

/// Reads one NVIDIA card by its PCI slot name (`000f:01:00.0`).
pub type NvidiaRead = fn(&str) -> NvidiaCard;

/// The inert [`NvidiaRead`] of a fixture root (and of the system root off Linux): never asks NVML, and answers [`Why::NotRead`].
#[must_use]
pub const fn no_nvml(_slot: &str) -> NvidiaCard {
    NvidiaCard::unread(Why::NotRead)
}

/// [`Root::system`]'s [`NvidiaRead`]: NVML on Linux.
#[cfg(target_os = "linux")]
const SYSTEM_NVIDIA: NvidiaRead = crate::nvml::card;
/// [`Root::system`]'s [`NvidiaRead`]: no NVIDIA driver to ask off Linux.
#[cfg(not(target_os = "linux"))]
const SYSTEM_NVIDIA: NvidiaRead = no_nvml;

/// Whether reading an awake card's sysfs figures leaves its runtime power management where it was, from its device's `power/runtime_status`, `power/control` and `boot_vga`.
pub(crate) fn pm_settled(runtime_status: &str, control: &str, boot_vga: &str) -> bool {
    runtime_status == "unsupported" || control == "on" || boot_vga == "1"
}

/// The wall clock, unix ms, that a sweep reads beside `/proc/uptime` to turn each process's boot-clock start into its birth ([`ProcessReading::birth_ms`]).
pub type WallMs = fn() -> Option<i64>;

/// [`Root::new`]'s [`WallMs`]: the system's wall clock.
fn system_wall_ms() -> Option<i64> {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// `/proc/uptime`'s first field (the boot clock, sleep included, in seconds to the centisecond) in nanoseconds.
#[must_use]
pub fn parse_uptime_ns(text: &str) -> Option<u64> {
    let field = text.split_whitespace().next()?;
    let (whole, frac) = match field.split_once('.') {
        Some((_, "")) => return None,
        Some(parts) => parts,
        None => (field, ""),
    };
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || frac.len() > 9
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let secs: u64 = whole.parse().ok()?;
    let frac_ns: u64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<9}").parse().ok()?
    };
    secs.checked_mul(1_000_000_000)?.checked_add(frac_ns)
}

/// `/proc`, `/sys` and `/sys/fs/cgroup` under one root directory: `/` on a Linux machine ([`Root::system`]), a fixture tree in tests.
#[derive(Clone, Debug)]
pub struct Root {
    base: PathBuf,
    nvidia: NvidiaRead,
    wall_ms: WallMs,
}

impl Root {
    /// The tree under `base`, whose NVIDIA cards are not read ([`no_nvml`]).
    #[must_use]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            nvidia: no_nvml,
            wall_ms: system_wall_ms,
        }
    }

    /// The machine's own tree, `/`, its NVIDIA cards read through NVML.
    #[must_use]
    pub fn system() -> Self {
        Self {
            nvidia: SYSTEM_NVIDIA,
            ..Self::new("/")
        }
    }

    /// This root with `nvidia` reading its NVIDIA cards: the test arm a fixture tree's NVIDIA card answers through.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_nvidia(self, nvidia: NvidiaRead) -> Self {
        Self { nvidia, ..self }
    }

    /// This root with `wall_ms` as its wall clock: the test arm a fixture tree's births are read against.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_wall_ms(self, wall_ms: WallMs) -> Self {
        Self { wall_ms, ..self }
    }

    /// `rel` under the root.
    fn at(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }

    /// The text of at most 4 KiB of `rel`, as written (a device-tree string keeps its NUL);
    /// `None` when unreadable.
    #[must_use]
    pub fn text(&self, rel: &str) -> Option<String> {
        self.read(rel, str::to_owned)
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

    /// [`crate::machine_words`].
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

    /// Count `pid`'s descriptors under `<root>/proc/<pid>/fd` that are one of `sockets`
    /// ([`tcp_sockets_in_fd_dir`]).
    #[must_use]
    pub fn open_socket_count(
        &self,
        pid: u32,
        sockets: &std::collections::HashMap<u64, crate::jobs::linux::TcpRow>,
    ) -> Option<u32> {
        tcp_sockets_in_fd_dir(&self.at(&format!("proc/{pid}/fd")), sockets)
    }

    /// Every thread on the machine (`/proc/loadavg`, [`loadavg_tasks`]), the count `kernel/pid_max` and `threads-max` bound.
    #[must_use]
    pub fn tasks(&self) -> Option<u64> {
        self.read("proc/loadavg", loadavg_tasks).flatten()
    }

    /// `/proc/loadavg`'s runnable and total threads ([`loadavg_counts`]), one read.
    #[must_use]
    pub fn loadavg_counts(&self) -> Option<(u64, u64)> {
        self.read("proc/loadavg", loadavg_counts).flatten()
    }

    /// The machine's task tables ([`crate::room::TaskRoom`]): every task against the smaller of
    /// `kernel/pid_max` and `threads-max` (each thread takes a pid) and against `threads-max`,
    /// `fs/file-nr`'s open files against `fs/file-max`, and one user's limit `nproc` (its
    /// `RLIMIT_NPROC`, which the caller reads); no per-user count.
    #[must_use]
    pub fn task_room(&self, nproc: Option<u64>) -> crate::room::TaskRoom {
        let count = |rel: &str| {
            self.read(rel, |t| crate::parse_uint::<u64>(t.trim()))
                .flatten()
        };
        let counts = self.loadavg_counts();
        let tasks = counts.map(|(_, total)| total);
        let threads_max = count("proc/sys/kernel/threads-max");
        crate::room::TaskRoom {
            run_queue: crate::runq::of_running(counts.map(|(running, _)| running)),
            procs: tasks,
            procs_max: crate::machine::smaller(count("proc/sys/kernel/pid_max"), threads_max),
            user_procs: None,
            user_procs_max: nproc,
            threads: tasks,
            threads_max,
            files: self
                .read("proc/sys/fs/file-nr", crate::room::file_nr_open)
                .flatten(),
            files_max: count("proc/sys/fs/file-max"),
        }
    }

    /// The cores the OS took away: present but not online.
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

    /// The hybrid PMUs' core lists: every `/sys/devices/cpu_<kind>/cpus` (`cpu_core`, `cpu_atom` on Intel's hybrid parts), at most eight.
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

    /// Each of `ids`' `cpu_capacity` (`/sys/devices/system/cpu/cpuN/`), for the cores that have one.
    #[must_use]
    pub fn capacities(&self, ids: &[u16]) -> Vec<(u16, u32)> {
        ids.iter()
            .filter_map(|id| {
                let c = self
                    .read(
                        &format!("sys/devices/system/cpu/cpu{id}/cpu_capacity"),
                        |t| crate::parse_uint::<u32>(t.trim()),
                    )
                    .flatten()?;
                Some((*id, c))
            })
            .collect()
    }

    /// Each of `ids`' [`Design`]: its `regs/identification/midr_el1` and `cpufreq/cpuinfo_max_freq` (`/sys/devices/system/cpu/cpuN/`), for the cores that publish a MIDR (ARM).
    #[must_use]
    pub fn designs(&self, ids: &[u16]) -> Vec<(u16, Design)> {
        ids.iter()
            .filter_map(|id| {
                let cpu = format!("sys/devices/system/cpu/cpu{id}");
                let midr = self
                    .read(&format!("{cpu}/regs/identification/midr_el1"), |t| {
                        let t = t.trim();
                        let h = t.strip_prefix("0x").unwrap_or(t);
                        h.bytes()
                            .all(|b| b.is_ascii_hexdigit())
                            .then(|| u64::from_str_radix(h, 16).ok())
                            .flatten()
                    })
                    .flatten()?;
                let top = self
                    .read(&format!("{cpu}/cpufreq/cpuinfo_max_freq"), |t| {
                        crate::parse_uint::<u64>(t.trim())
                    })
                    .flatten();
                Some((*id, (midr & MIDR_DESIGN, top)))
            })
            .collect()
    }

    /// The cores grouped by performance level, fastest first ([`layout_from`] over [`Self::present_or_parallelism`], [`Self::hybrid_pmus`] and [`Self::capacities`], one level per [`Design`]).
    #[must_use]
    pub fn cpu_layout(&self) -> CpuLayout {
        let present = self.present_or_parallelism();
        let pmus = self.hybrid_pmus();
        let capacities = if pmus.len() >= 2 {
            Vec::new()
        } else {
            capacities_by_design(&self.capacities(&present), &self.designs(&present))
        };
        layout_from(&present, self.online().as_deref(), &pmus, &capacities)
    }

    /// Memory PSI (`/proc/pressure/memory`).
    ///
    /// # Errors
    /// `not_enabled:psi` when the file is missing or refuses the read (a kernel built without PSI, or booted with `psi=0`).
    pub fn psi(&self) -> Result<Psi, Why> {
        self.read("proc/pressure/memory", psi)
            .flatten()
            .ok_or(Why::NotEnabled("psi"))
    }

    /// Whether zswap is on (`/sys/module/zswap/parameters/enabled`): `None` when the kernel has no zswap.
    #[must_use]
    pub fn zswap_enabled(&self) -> Option<bool> {
        self.read("sys/module/zswap/parameters/enabled", |t| {
            matches!(t.trim(), "Y" | "y" | "1")
        })
    }

    /// Memory by class, swap, commit ([`memory_detail`]) and the PSI level ([`psi_level`]; `None` without PSI).
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

    /// What this process may use.
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

    /// The thermal zones (`/sys/class/thermal/thermal_zoneN`), at most [`MAX_ZONES`], each with its temperature and up to [`MAX_TRIPS`] passive, hot and critical trips.
    #[must_use]
    pub fn zones(&self) -> Vec<Zone> {
        let int = |t: &str| crate::parse_int::<i64>(t.trim());
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

    /// The CPUs' thermal throttle events since boot, summed.
    #[must_use]
    pub fn throttle_count(&self) -> Option<u64> {
        let int = |t: &str| crate::parse_uint::<u64>(t.trim());
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

    /// The GPU, device-wide, from the DRM cards (`/sys/class/drm/cardN`, at most [`MAX_CARDS`]).
    #[must_use]
    pub fn gpu(&self) -> Option<GpuReading> {
        let mut readings: Vec<GpuReading> = Vec::new();
        for (_, card) in self.numbered("sys/class/drm", "card", MAX_CARDS) {
            let dev = format!("sys/class/drm/{card}/device");
            let (driver, slot) = self
                .read(&format!("{dev}/uevent"), |t| {
                    let field = |key: &str| {
                        whole_lines(t)
                            .find_map(|l| l.strip_prefix(key))
                            .map(str::to_owned)
                    };
                    (field("DRIVER="), field("PCI_SLOT_NAME="))
                })
                .unwrap_or_default();
            let driver = driver.or_else(|| {
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
            let mut card = None;
            let busy = if status == "suspended" {
                Err(Why::Asleep)
            } else if driver != "amdgpu" && driver != "nvidia" {
                Err(gpu_unread(&driver))
            } else if !pm_settled(&status, &word("power/control"), &word("boot_vga")) {
                Err(Why::NotRead)
            } else if driver == "amdgpu" {
                self.read(&format!("{dev}/gpu_busy_percent"), |t| {
                    crate::parse_uint::<u32>(t.trim())
                })
                .flatten()
                .map(|p| u8::try_from(p.min(100)).unwrap_or(100))
                .ok_or(Why::Unsupported)
            } else {
                let read = slot
                    .as_deref()
                    .map_or(NvidiaCard::unread(Why::NotRead), self.nvidia);
                let busy = read.busy.map(|p| p.min(100));
                card = Some(read);
                busy
            };
            // An NVIDIA card's power is NVML's; one not read (asleep, its runtime PM unsettled)
            // carries the busy figure's reason.
            let power_mw = (driver == "nvidia").then(|| match &card {
                Some(c) => c.power_mw,
                None => Err(busy.err().unwrap_or(Why::NotRead)),
            });
            let (named, unified) = card.map_or((None, false), |c| (c.name, c.unified));
            readings.push(GpuReading {
                device_pct: busy.ok(),
                unread: busy.err(),
                name: named.or(Some(driver)),
                unified,
                power_mw,
                ..GpuReading::default()
            });
        }
        let asleep = |r: &GpuReading| r.unread == Some(Why::Asleep);
        let pick = readings
            .iter()
            .position(|r| r.device_pct.is_some())
            .or_else(|| readings.iter().position(|r| !asleep(r)))
            .unwrap_or(0);
        (pick < readings.len()).then(|| readings.swap_remove(pick))
    }

    /// Process `pid`'s footprint (`procs::footprint` on Linux): [`status_footprint`] of its
    /// `status`, read whole up to [`STATUS_CAP`].
    ///
    /// # Errors
    /// `InvalidData` where the file is past [`STATUS_CAP`]; `NotFound` where it is unreadable or
    /// names no anonymous memory (a kernel thread).
    pub fn footprint(&self, pid: u32) -> std::io::Result<u64> {
        match self.status_memory(pid) {
            Some(Some((footprint, _))) => Ok(footprint),
            Some(None) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "process status exceeds the bounded reader",
            )),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "process status is unreadable or names no anonymous memory",
            )),
        }
    }

    /// `f` over the text of `proc/<pid>/status` up to [`STATUS_CAP`]: `Some(Some(val))`;
    /// `Some(None)` when the file is past [`STATUS_CAP`]; `None` when it is unreadable.
    fn status<T>(&self, pid: u32, f: impl FnOnce(&str) -> T) -> Option<Option<T>> {
        let path = self.at(&format!("proc/{pid}/status"));
        let mut buf = [0u8; crate::READ_CAP];
        let (n, cut) = crate::read_into(&path, &mut buf).ok()?;
        if !cut {
            return Some(Some(f(crate::utf8_prefix(&buf[..n]))));
        }
        let mut whole = vec![0u8; STATUS_CAP];
        match crate::read_into(&path, &mut whole).ok()? {
            (n, false) => Some(Some(f(crate::utf8_prefix(&whole[..n])))),
            (_, true) => Some(None),
        }
    }

    /// Process `pid`'s footprint and resident anonymous pages from its `status`
    /// ([`status_memory`]): `Some(Some(bytes))`; `Some(None)` when the file is past
    /// [`STATUS_CAP`] (both are unknown, and no other figure stands in for
    /// them); `None` when it is unreadable or names no anonymous memory (a
    /// kernel thread), for the caller's fallback.
    fn status_memory(&self, pid: u32) -> Option<Option<(u64, u64)>> {
        match self.status(pid, status_memory)? {
            Some(mem) => mem.map(Some),
            None => Some(None),
        }
    }

    /// One sweep of `/proc`.
    #[must_use]
    pub fn processes(&self, hz: u64, page_bytes: u64) -> Option<Sweep> {
        let hz = hz.max(1);
        let mut procs = Vec::new();
        let mut truncated = false;
        let clock = self
            .read("proc/uptime", parse_uptime_ns)
            .flatten()
            .zip((self.wall_ms)());
        for entry in std::fs::read_dir(self.at("proc")).ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(crate::parse_uint::<u32>)
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
            let ticks = stat.utime.saturating_add(stat.stime);
            let ns = u128::from(ticks) * 1_000_000_000 / u128::from(hz);
            let child_ticks = stat.cutime.saturating_add(stat.cstime);
            let child_ns = u128::from(child_ticks) * 1_000_000_000 / u128::from(hz);
            let (uid, footprint, resident) = if stat.kernel_thread() {
                (Some(0), Some(0), None)
            } else {
                let statm = || {
                    self.read(&format!("proc/{pid}/statm"), parse_statm_resident)
                        .flatten()
                        .map(|pages| pages.saturating_mul(page_bytes))
                };
                let (uid, footprint, resident) =
                    match self.status(pid, |t| (status_uid(t), status_memory(t))) {
                        Some(Some((uid, Some((footprint, anon))))) => {
                            (uid, Some(footprint), Some(anon))
                        }
                        Some(Some((uid, None))) => (uid, statm(), None),
                        Some(None) => (None, None, None),
                        None => (None, statm(), None),
                    };
                let Some(confirm) = self.read(&format!("proc/{pid}/stat"), parse_stat).flatten()
                else {
                    continue;
                };
                if confirm.start != stat.start {
                    continue;
                }
                (uid, footprint, resident)
            };
            let start = u64::try_from(u128::from(stat.start) * 1_000_000_000 / u128::from(hz)).ok();
            procs.push(ProcessReading {
                scheduling: None,
                pid,
                start,
                ppid: stat.ppid,
                exec: Some(stat.exec()),
                cpu_ns: Some(u64::try_from(ns).unwrap_or(u64::MAX)),
                child_cpu_ns: Some(u64::try_from(child_ns).unwrap_or(u64::MAX)),
                footprint,
                resident,
                disk_read: None,
                disk_written: None,
                io_read: None,
                io_written: None,
                energy_nj: None,
                name: crate::procs::whole_name(
                    stat.name.as_bytes(),
                    COMM_CUT,
                    self.longer_names(pid),
                ),
                pgid: stat.pgrp,
                uid,
                state: RunState::from_linux(stat.state),
                session_leader: Some(stat.session == pid),
                sid: stat.session,
                has_tty: Some(stat.tty_nr != 0),
                runnable_ns: None,
                cores: None,
                peak_footprint: None,
                read_at,
                coalition: None,
                version: None,
                birth_ms: start.zip(clock).and_then(|(start, (boot, wall))| {
                    crate::procs::birth_of_boot_start(start, boot, wall)
                }),
                reaps_unwaited: None,
            });
        }
        Some(Sweep { procs, truncated })
    }

    /// `pid`'s longer names for [`crate::procs::whole_name`], each read only once pulled, neither
    /// cut: its `exe` link's file name (only its own user, or root, may read the link), less the
    /// ` (deleted)` the kernel adds for an unlinked image; then its `argv[0]`'s, where that ends
    /// in its NUL and holds no space (a program that rewrote its arguments into a title, which
    /// may name other files, is not read).
    fn longer_names(
        &self,
        pid: u32,
    ) -> impl Iterator<Item = Option<(Vec<u8>, Option<usize>)>> + '_ {
        let exe = std::iter::once_with(move || {
            let link = std::fs::read_link(self.at(&format!("proc/{pid}/exe"))).ok()?;
            let name = link.file_name()?.to_str()?;
            let name = name.strip_suffix(" (deleted)").unwrap_or(name);
            Some((name.as_bytes().to_vec(), None))
        });
        let argv0 = std::iter::once_with(move || {
            self.read(&format!("proc/{pid}/cmdline"), |t| {
                let (argv0, _) = t.split_once('\0')?;
                let name = argv0.rsplit('/').next()?;
                (!argv0.contains(' ')).then(|| (name.as_bytes().to_vec(), None))
            })
            .flatten()
        });
        exe.chain(argv0)
    }

    /// What this tree's readers can say ([`Caps`]): each row from a probe of the file it reads.
    #[must_use]
    pub fn caps(&self) -> Caps {
        let readable = |rel: &str| self.read(rel, |_| ()).is_some();
        let yes_if = |ok: bool, why: Why| if ok { Cap::Yes } else { Cap::No(why) };
        let meminfo = self.read("proc/meminfo", memory_detail).flatten();
        let others = if readable("proc/1/stat") {
            Cap::Yes
        } else {
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
            coalitions: Cap::No(Why::Unsupported),
            more: Vec::new(),
            capacity: self.capacity().source,
        }
    }
}

/// The reader behind [`crate::Probe::throttle`] on Linux.
#[derive(Debug)]
pub struct ThrottleReader {
    root: Root,
    last: Option<u64>,
}

impl ThrottleReader {
    /// A reader of `root` that has read nothing.
    #[must_use]
    pub const fn new(root: Root) -> Self {
        Self { root, last: None }
    }

    /// One reading ([`throttle_level`] over [`Root::zones`] and whether [`Root::throttle_count`] rose since the last reading; the first reading has nothing to compare and counts as no rise).
    ///
    /// # Errors
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
