// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux's TCP readers (`docs/DESIGN-stats-deck-linux-2026-11-05.md` step L1: D1, D12, D13).
//!
//! A reading reads the kernel's TCP counters, `/proc/net/snmp`'s `CurrEstab` and
//! `/proc/net/sockstat`'s `tw`, on kept descriptors, and walks no socket table: `/proc/net/tcp`
//! and `tcp6` walk the whole established hash, which the kernel sizes from memory (524,288 buckets
//! on a 128 GB DGX Spark, ≈ 3.4–5.1 ms a reading there), and a `sock_diag` dump walks it too. One
//! all-states dump of both families runs at most once an epoch, after a reading publishes
//! ([`after_publish`]), and every other TCP reader shares it: the states the counters do not give,
//! the connections each reading looks up by cookie for their RTT, the jobs pass's sockets
//! ([`newest_within`], which takes one on the jobs pass's own path when none is two epochs fresh)
//! and the socket scopes ([`newest`]). The dump names this network namespace's sockets alone: the
//! jobs pass names a process's in another namespace from that namespace's own tables, read after
//! a reading publishes ([`namespace_sockets`], [`read_namespaces`]).

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::{
    Dump, DumpFault, INET_DIAG_DST_AT, INET_DIAG_INFO_ATTR, INET_DIAG_MSG_LEN, LINUX_AF_INET,
    LINUX_AF_INET6, NLM_F_DUMP, NLM_F_DUMP_INTR, NLM_F_REQUEST, NLMSG_DONE, NLMSG_ERROR,
    NLMSG_HDRLEN, NLMSG_NOOP, NLMSG_OVERRUN, SOCK_DIAG_BY_FAMILY, TCPI_RTT_AT,
    inet_diag_remote_peer, ne_u32, nl_attributes, nl_messages, whole_lines,
};
use crate::jobs::TcpState;
use crate::jobs::linux::TcpRow;
use crate::machine::TcpCounts;

#[cfg(all(test, target_os = "linux"))]
pub(crate) use live::dump_now;
#[cfg(target_os = "linux")]
pub use live::{after_publish, newest, newest_within, reading};

/// The most connections one dump keeps by inode, and the most it finds to follow, in each family
/// (REC's cap, REC:873): a record with no inode (`TIME_WAIT`, a request, an orphan) takes no
/// place, and one family's records never take the other's. Every record still counts toward its
/// state.
pub const DUMP_KEEP: usize = 4096;
/// The most connections a reading looks up by cookie (A3 ruling 11).
pub const FOLLOW_MAX: usize = 64;
/// The dump's epoch while a window in view shows the bars or a page that reads them, focused or
/// in the background.
pub const EPOCH: Duration = Duration::from_secs(10);
/// The dump's epoch while no window in view shows the bars or a page that reads them.
pub const HIDDEN_EPOCH: Duration = Duration::from_secs(60);
/// A lane's bound on one run's wall time (A2's lane rule), which the dump's self-check holds.
pub const LANE_BOUND: Duration = Duration::from_millis(15);
/// How many first runs the dump's self-check judges.
pub const SELF_CHECK_RUNS: usize = 30;
/// How many of those runs past [`LANE_BOUND`] put their 95th percentile past it: the percentile
/// is the run at sorted index `ceil(30 × 0.95) − 1` = 28, the second slowest, so a second slow
/// run decides it.
pub const SLOW_RUNS_TO_RETIRE: usize = SELF_CHECK_RUNS - ((SELF_CHECK_RUNS * 95).div_ceil(100) - 1);
/// `inet_diag_sockid`'s size: the ports, the addresses, the interface and the cookie.
const SOCKID_LEN: usize = 48;
/// Where `inet_diag_msg`'s `idiag_timer` sits.
const INET_DIAG_TIMER_AT: usize = 2;
/// The `idiag_timer` a timewait socket's record carries (`inet_twsk_diag_fill`); a full socket's
/// is 0, 1, 2 or 4.
const TIMEWAIT_TIMER: u8 = 3;
/// Where `inet_diag_msg`'s `idiag_rqueue` sits: bytes received and not read (a listener's accept
/// backlog).
const INET_DIAG_RQUEUE_AT: usize = 56;
/// Where `inet_diag_msg`'s `idiag_inode` sits.
const INET_DIAG_INODE_AT: usize = 68;
/// Where an `inet_diag_sockid` sits in a record, and in a request after its first 8 bytes.
const SOCKID_AT: usize = 4;
/// Every TCP state (`TCPF_ALL`, `net/tcp_states.h`).
const TCPF_ALL: u32 = 0xfff;
/// `IPPROTO_TCP`.
const IPPROTO_TCP: u8 = 6;
/// `ENOENT`: a looked-up connection is gone, or its cookie now names another socket.
const ENOENT: i32 = 2;
/// An `inet_diag_req_v2` request's size: its `nlmsghdr`, then family, protocol, extensions, a pad
/// byte, the states and the socket id.
pub const DIAG_REQ_LEN: usize = NLMSG_HDRLEN + 8 + SOCKID_LEN;
/// The most of another namespace's `net/tcp` or `tcp6` table read: about 1,700 sockets.
pub const NET_TCP_CAP: usize = 256 * 1024;
/// The most other namespaces whose sockets are held at once: past it, a namespace's sockets go
/// unnamed until one held is asked for no more ([`namespace_sockets`]).
pub const NAMESPACES_HELD: usize = 64;
/// How long a namespace the jobs pass stops asking for stays held: three of the pass's longest
/// gaps (30 s while every job waits on its own, `Demand::Slow`), so one asked for at any pace the
/// pass keeps is never let go.
pub const NAMESPACE_HOLD: Duration = Duration::from_secs(90);

/// `CurrEstab` from `/proc/net/snmp`'s two `Tcp:` lines, names then values: the connections in
/// `ESTABLISHED`, both families, and in `CLOSE_WAIT` (RFC 1213's `tcpCurrEstab`) on a kernel that
/// counts them, Linux 6.10's a46d0ea5c949 or a backport of it, which a dump measures
/// ([`curr_estab_counts_close_wait`]).
#[must_use]
pub fn snmp_curr_estab(text: &str) -> Option<u32> {
    let mut tcp = whole_lines(text).filter_map(|l| l.strip_prefix("Tcp:"));
    let (names, values) = (tcp.next()?, tcp.next()?);
    names
        .split_whitespace()
        .zip(values.split_whitespace())
        .find(|(name, _)| *name == "CurrEstab")
        .and_then(|(_, value)| crate::parse_uint(value))
}

/// `tw` from `/proc/net/sockstat`'s `TCP:` line: the namespace's timewait sockets, both families
/// (its one death row holds them all), the connections in `TIME_WAIT` and the orphaned ones the
/// kernel holds in `FIN_WAIT2` as timewait sockets ([`TcpDump::timewait_fin_wait2`]).
#[must_use]
pub fn sockstat_time_wait(text: &str) -> Option<u32> {
    let line = whole_lines(text).find_map(|l| l.strip_prefix("TCP:"))?;
    let words: Vec<&str> = line.split_whitespace().collect();
    words
        .as_chunks::<2>()
        .0
        .iter()
        .find(|[name, _]| *name == "tw")
        .and_then(|[_, value]| crate::parse_uint(value))
}

/// A reading's state counts: `ESTABLISHED` and `TIME_WAIT` from this reading's counters, every
/// other state from the newest `dump`. Where this kernel's `CurrEstab` counts `CLOSE_WAIT`, the
/// dump's `CLOSE_WAIT` comes off it, and the dump's timewait `FIN_WAIT2` sockets come off `tw`, so
/// no connection counts twice. With no dump only the counters are read: the other states are
/// unread, not 0 ([`crate::TcpSummary::counters_only`]), and `ESTABLISHED` may hold `CLOSE_WAIT`.
#[must_use]
pub fn reading_counts(curr_estab: u32, time_wait: u32, dump: Option<&TcpDump>) -> TcpCounts {
    let Some(dump) = dump else {
        return TcpCounts {
            established: curr_estab,
            time_wait,
            ..TcpCounts::default()
        };
    };
    let close_wait = if dump.close_wait_in_curr_estab {
        dump.counts.close_wait
    } else {
        0
    };
    TcpCounts {
        established: curr_estab.saturating_sub(close_wait),
        time_wait: time_wait.saturating_sub(dump.timewait_fin_wait2),
        ..dump.counts
    }
}

/// Whether `CurrEstab`, read beside a dump that counted `dump`, counts `CLOSE_WAIT`: whichever of
/// `ESTABLISHED` and `ESTABLISHED + CLOSE_WAIT` it is nearer (a tie, no `CLOSE_WAIT`, says yes and
/// subtracts nothing).
#[must_use]
pub const fn curr_estab_counts_close_wait(curr_estab: u32, dump: &TcpCounts) -> bool {
    let with = dump.established.saturating_add(dump.close_wait);
    curr_estab.abs_diff(with) <= curr_estab.abs_diff(dump.established)
}

/// An established connection to a remote peer that a dump found: its family, and its
/// `inet_diag_sockid` as the kernel wrote it, cookie included, which a lookup sends back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Followed {
    family: u8,
    id: [u8; SOCKID_LEN],
}

impl Followed {
    /// Its peer's address bytes (`idiag_dst`), an IPv4 address in the first four.
    fn dst(&self) -> &[u8] {
        let at = INET_DIAG_DST_AT - SOCKID_AT;
        &self.id[at..at + 16]
    }

    /// Its destination's prefix: an IPv4 /24 (an IPv4-mapped IPv6 address's too), else an IPv6
    /// /48. A site's many connections share one path, so one of them stands for it first.
    fn prefix(&self) -> (bool, [u8; 6]) {
        let d = self.dst();
        let v4 = if self.family == LINUX_AF_INET {
            Some(&d[..3])
        } else if d[..10] == [0; 10] && d[10..12] == [0xff, 0xff] {
            Some(&d[12..15])
        } else {
            None
        };
        match v4 {
            Some(p) => (true, [p[0], p[1], p[2], 0, 0, 0]),
            None => (false, [d[0], d[1], d[2], d[3], d[4], d[5]]),
        }
    }
}

/// At most [`FOLLOW_MAX`] of `found`: the first connection to each destination prefix in dump
/// order, then the rest in dump order.
#[must_use]
pub fn choose_followed(found: &[Followed]) -> Vec<Followed> {
    let mut seen = HashSet::new();
    let (first, rest): (Vec<&Followed>, Vec<&Followed>) =
        found.iter().partition(|f| seen.insert(f.prefix()));
    first
        .into_iter()
        .chain(rest)
        .take(FOLLOW_MAX)
        .copied()
        .collect()
}

/// One all-states dump of both families.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TcpDump {
    /// Every connection's state, both families, kept or not.
    pub counts: TcpCounts,
    /// Of those in `FIN_WAIT2`, the orphans the kernel holds as timewait sockets (`idiag_timer`
    /// 3), which `sockstat`'s `tw` counts too.
    pub timewait_fin_wait2: u32,
    /// Whether this kernel's `CurrEstab` counts `CLOSE_WAIT`, measured beside this dump
    /// ([`curr_estab_counts_close_wait`]).
    pub close_wait_in_curr_estab: bool,
    /// Each family's first [`DUMP_KEEP`] connections with an inode, by that inode, the one a
    /// `/proc/<pid>/fd` link names (`socket:[<inode>]`). One in `TIME_WAIT` has no inode and is
    /// not kept.
    pub sockets: HashMap<u64, TcpRow>,
    /// The connections a reading looks up ([`choose_followed`]).
    pub followed: Vec<Followed>,
}

/// What an all-states dump has read so far.
#[derive(Debug, Default)]
pub struct DumpRead {
    counts: TcpCounts,
    timewait_fin_wait2: u32,
    sockets: HashMap<u64, TcpRow>,
    found: Vec<Followed>,
    /// Each family's connections kept by inode, IPv4's then IPv6's.
    kept: [usize; 2],
    /// Each family's connections found to follow.
    finds: [usize; 2],
}

impl DumpRead {
    /// The dump its datagrams read.
    #[must_use]
    pub fn finish(self) -> TcpDump {
        TcpDump {
            counts: self.counts,
            timewait_fin_wait2: self.timewait_fin_wait2,
            close_wait_in_curr_estab: false,
            followed: choose_followed(&self.found),
            sockets: self.sockets,
        }
    }

    /// One `SOCK_DIAG_BY_FAMILY` record: its state counted, and, within its family's
    /// [`DUMP_KEEP`], the connection kept by inode and an established one to a remote peer found
    /// to follow.
    fn record(&mut self, record: &[u8]) -> Result<(), DumpFault> {
        let msg = record
            .get(..INET_DIAG_MSG_LEN)
            .ok_or(DumpFault::Torn("a socket record cut short"))?;
        let family = msg[0];
        if family != LINUX_AF_INET && family != LINUX_AF_INET6 {
            return Err(DumpFault::Torn("a socket record of another family"));
        }
        let state = TcpState::from_linux(msg[1]);
        self.counts.record(state);
        if state == TcpState::FinWait2 && msg[INET_DIAG_TIMER_AT] == TIMEWAIT_TIMER {
            self.timewait_fin_wait2 = self.timewait_fin_wait2.saturating_add(1);
        }
        let side = usize::from(family == LINUX_AF_INET6);
        let dst = &msg[INET_DIAG_DST_AT..INET_DIAG_DST_AT + 16];
        let inode = ne_u32(msg, INET_DIAG_INODE_AT).map_or(0, u64::from);
        if inode != 0 && self.kept[side] < DUMP_KEEP {
            self.kept[side] += 1;
            let port = |at: usize| u16::from_be_bytes([msg[at], msg[at + 1]]);
            let remote_port = port(SOCKID_AT + 2);
            self.sockets.insert(
                inode,
                TcpRow {
                    inode,
                    state,
                    local_port: port(SOCKID_AT),
                    remote: (remote_port != 0)
                        .then(|| SocketAddr::new(address(family, dst), remote_port)),
                    rx_queue: ne_u32(msg, INET_DIAG_RQUEUE_AT).map_or(0, u64::from),
                },
            );
        }
        if state == TcpState::Established
            && self.finds[side] < DUMP_KEEP
            && inet_diag_remote_peer(family, dst)
        {
            self.finds[side] += 1;
            let mut id = [0; SOCKID_LEN];
            id.copy_from_slice(&msg[SOCKID_AT..SOCKID_AT + SOCKID_LEN]);
            self.found.push(Followed { family, id });
        }
        Ok(())
    }
}

/// `family`'s address in a record's 16 address bytes: an IPv4 one in the first four.
fn address(family: u8, bytes: &[u8]) -> IpAddr {
    if family == LINUX_AF_INET {
        IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]))
    } else {
        let mut v6 = [0; 16];
        v6.copy_from_slice(&bytes[..16]);
        IpAddr::V6(Ipv6Addr::from(v6))
    }
}

/// The `int` an `NLMSG_ERROR` or `NLMSG_DONE` carries first.
fn nl_errno(payload: &[u8]) -> Result<i32, DumpFault> {
    payload
        .get(..4)
        .and_then(|b| Some(i32::from_ne_bytes(b.try_into().ok()?)))
        .ok_or(DumpFault::Torn("an error or end cut short"))
}

/// One datagram of the all-states dump answering request `seq`, its records read into `into`.
///
/// # Errors
///
/// A [`DumpFault`]: the datagram is empty or torn (a record cut short or of another family),
/// answers another request, carries an error or an overrun, or marks the dump interrupted. `into`
/// may then hold part of the datagram: a dump read in part is discarded whole.
pub fn tcp_diag_records(datagram: &[u8], seq: u32, into: &mut DumpRead) -> Result<Dump, DumpFault> {
    if datagram.is_empty() {
        return Err(DumpFault::Torn("an empty datagram"));
    }
    for message in nl_messages(datagram) {
        let message = message.map_err(DumpFault::Torn)?;
        if message.seq != seq {
            return Err(DumpFault::Torn("a message of another request"));
        }
        if message.flags & NLM_F_DUMP_INTR != 0 {
            return Err(DumpFault::Interrupted);
        }
        match message.kind {
            NLMSG_NOOP => {}
            NLMSG_ERROR => {
                return Err(match nl_errno(message.payload)? {
                    e if e < 0 => DumpFault::Refused(e.saturating_neg()),
                    _ => DumpFault::Torn("an acknowledgement nobody asked for"),
                });
            }
            NLMSG_DONE => {
                return match nl_errno(message.payload)? {
                    e if e < 0 => Err(DumpFault::Refused(e.saturating_neg())),
                    _ => Ok(Dump::Done),
                };
            }
            NLMSG_OVERRUN => return Err(DumpFault::Torn("an overrun")),
            SOCK_DIAG_BY_FAMILY => into.record(message.payload)?,
            _ => return Err(DumpFault::Torn("a message of another type")),
        }
    }
    Ok(Dump::More)
}

/// An `inet_diag_req_v2` for TCP under sequence `seq` with `flags`, family and the rest zero.
fn diag_request(seq: u32, flags: u16) -> [u8; DIAG_REQ_LEN] {
    let mut r = [0_u8; DIAG_REQ_LEN];
    r[0..4].copy_from_slice(
        &u32::try_from(DIAG_REQ_LEN)
            .unwrap_or(u32::MAX)
            .to_ne_bytes(),
    );
    r[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    r[6..8].copy_from_slice(&flags.to_ne_bytes());
    r[8..12].copy_from_slice(&seq.to_ne_bytes());
    r[NLMSG_HDRLEN + 1] = IPPROTO_TCP;
    r[NLMSG_HDRLEN + 4..NLMSG_HDRLEN + 8].copy_from_slice(&TCPF_ALL.to_ne_bytes());
    r
}

/// The all-states dump request for `family` under sequence `seq`: every state, no extension (a
/// reading's lookups read the RTTs).
#[must_use]
pub fn dump_request(family: u8, seq: u32) -> [u8; DIAG_REQ_LEN] {
    let mut r = diag_request(seq, NLM_F_REQUEST | NLM_F_DUMP);
    r[NLMSG_HDRLEN] = family;
    r
}

/// The request that looks up `followed` under sequence `seq`: no `NLM_F_DUMP`, `INET_DIAG_INFO`,
/// and the socket id its dump wrote, cookie included, so the kernel answers for that one
/// connection or says it is gone.
#[must_use]
pub fn lookup_request(followed: &Followed, seq: u32) -> [u8; DIAG_REQ_LEN] {
    let mut r = diag_request(seq, NLM_F_REQUEST);
    r[NLMSG_HDRLEN] = followed.family;
    r[NLMSG_HDRLEN + 2] = 1 << (INET_DIAG_INFO_ATTR - 1);
    r[NLMSG_HDRLEN + 8..].copy_from_slice(&followed.id);
    r
}

/// What a lookup's answer said.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lookup {
    /// The connection's smoothed RTT (`tcpi_rtt`), milliseconds.
    Rtt(f32),
    /// It is gone (`ENOENT`): a reading stops following it.
    Gone,
    /// No RTT: no answer to this request, an answer with none, or another error.
    Unread,
}

/// The answer to lookup `seq` in `datagram`.
#[must_use]
pub fn lookup_answer(datagram: &[u8], seq: u32) -> Lookup {
    for message in nl_messages(datagram) {
        let Ok(message) = message else {
            return Lookup::Unread;
        };
        if message.seq != seq {
            continue;
        }
        match message.kind {
            NLMSG_ERROR => {
                return match nl_errno(message.payload) {
                    Ok(e) if e == -ENOENT => Lookup::Gone,
                    _ => Lookup::Unread,
                };
            }
            SOCK_DIAG_BY_FAMILY => {
                return rtt_of(message.payload).map_or(Lookup::Unread, Lookup::Rtt);
            }
            _ => {}
        }
    }
    Lookup::Unread
}

/// A record's `tcpi_rtt` in milliseconds, when its `INET_DIAG_INFO` holds a positive one.
fn rtt_of(record: &[u8]) -> Option<f32> {
    for attr in nl_attributes(record.get(INET_DIAG_MSG_LEN..)?) {
        let (kind, info) = attr.ok()?;
        if kind == INET_DIAG_INFO_ATTR {
            let us = ne_u32(info, TCPI_RTT_AT)?;
            return (us > 0).then_some(us as f32 / 1_000.0);
        }
    }
    None
}

/// The median of `rtts` (sorted in place), or `None` when empty.
#[must_use]
pub fn median_ms(rtts: &mut [f32]) -> Option<f32> {
    if rtts.is_empty() {
        return None;
    }
    rtts.sort_by(f32::total_cmp);
    let n = rtts.len();
    Some((rtts[(n - 1) / 2] + rtts[n / 2]) * 0.5)
}

/// One other network namespace whose sockets the jobs pass asked for.
#[derive(Debug)]
struct HeldNamespace {
    /// The root it was asked for under, and its inode.
    key: (PathBuf, u64),
    /// A process in it, the last one asked for: where its tables are read.
    proc_dir: PathBuf,
    /// Whether the last read through `proc_dir` failed (the process left the namespace, or its
    /// tables would not open): not read again until a pass asks through a live process.
    proc_dir_failed: bool,
    /// When its tables were read; `None` before the first read.
    read_at: Option<Instant>,
    /// When the jobs pass last asked for it: one not asked for in [`NAMESPACE_HOLD`] is let go.
    asked_at: Instant,
    sockets: Option<super::TcpSockets>,
}

/// The other network namespaces the jobs pass asks for, at most [`NAMESPACES_HELD`]: asked for on
/// the reading's path, which only looks them up, and read after a reading publishes
/// ([`read_namespaces`]), at most once an [`EPOCH`] each and only when asked for since the last
/// read.
#[derive(Debug, Default)]
pub struct Namespaces {
    held: Vec<HeldNamespace>,
    /// The reads' self-check, the dump's ([`Pace::took`]): once it retires them, no namespace is
    /// held or read, and another namespace's sockets go unnamed.
    pace: Pace,
}

impl Namespaces {
    /// None held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            held: Vec::new(),
            pace: Pace::new(),
        }
    }

    /// The sockets held for namespace `key`, asked for at `now` by the process at `proc_dir`:
    /// `None` until its tables are first read. A namespace not held is taken in while fewer than
    /// [`NAMESPACES_HELD`] are, and one held is never let go while it is asked for, so a pass over
    /// more namespaces than that still finds every one it holds.
    pub fn ask(
        &mut self,
        key: (PathBuf, u64),
        proc_dir: &Path,
        now: Instant,
    ) -> Option<super::TcpSockets> {
        self.let_go(now);
        if let Some(held) = self.held.iter_mut().find(|h| h.key == key) {
            held.asked_at = now;
            proc_dir.clone_into(&mut held.proc_dir);
            held.proc_dir_failed = false;
            return held.sockets.clone();
        }
        if self.held.len() < NAMESPACES_HELD && !self.pace.retired() {
            self.held.push(HeldNamespace {
                key,
                proc_dir: proc_dir.to_path_buf(),
                proc_dir_failed: false,
                read_at: None,
                asked_at: now,
                sockets: None,
            });
        }
        None
    }

    /// The namespace whose tables are due at `now` (never read, or asked for since a read an
    /// [`EPOCH`] ago, through a process no read failed in), the least lately read first: its key
    /// and where to read it. It first lets go every namespace not asked for in
    /// [`NAMESPACE_HOLD`], so one a pass stops asking for is read no more.
    #[must_use]
    pub fn due(&mut self, now: Instant) -> Option<((PathBuf, u64), PathBuf)> {
        self.let_go(now);
        self.held
            .iter()
            .filter(|h| {
                !h.proc_dir_failed
                    && h.read_at.is_none_or(|at| {
                        h.asked_at > at && now.saturating_duration_since(at) >= EPOCH
                    })
            })
            .min_by_key(|h| h.read_at)
            .map(|h| (h.key.clone(), h.proc_dir.clone()))
    }

    /// Keeps `sockets`, read at `now`, for namespace `key`, when it is still held.
    pub fn store(&mut self, key: &(PathBuf, u64), sockets: super::TcpSockets, now: Instant) {
        if let Some(held) = self.held.iter_mut().find(|h| h.key == *key) {
            held.sockets = Some(sockets);
            held.read_at = Some(now);
        }
    }

    /// Marks namespace `key`'s read through its process as failed: the sockets last read stand,
    /// and it is not due again until a pass asks for it through a live process.
    pub fn failed(&mut self, key: &(PathBuf, u64)) {
        if let Some(held) = self.held.iter_mut().find(|h| h.key == *key) {
            held.proc_dir_failed = true;
        }
    }

    /// Records one read's wall time for the self-check; once it retires the reads, every
    /// namespace is let go.
    pub fn judge(&mut self, wall: Duration) {
        self.pace.took(wall);
        if self.pace.retired() {
            self.held.clear();
        }
    }

    fn let_go(&mut self, now: Instant) {
        self.held
            .retain(|h| now.saturating_duration_since(h.asked_at) < NAMESPACE_HOLD);
    }
}

static NAMESPACES: Mutex<Namespaces> = Mutex::new(Namespaces::new());

fn held_namespaces() -> std::sync::MutexGuard<'static, Namespaces> {
    NAMESPACES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The network namespace a `ns/net` link names (`net:[<inode>]`).
fn namespace_of(link: &Path) -> Option<u64> {
    let target = std::fs::read_link(link).ok()?;
    target
        .to_str()?
        .strip_prefix("net:[")?
        .strip_suffix(']')
        .and_then(crate::parse_uint)
}

/// The TCP sockets of the process at `proc_dir` (`<root>/<pid>`) when it lives in another
/// network namespace than `<root>/self`, whose sockets no dump of this one names: that
/// namespace's own `net/tcp` and `tcp6`, as last read after a reading published
/// ([`read_namespaces`]). This only looks them up, so the reading's path walks no table. `None`
/// in the same namespace, when either namespace is unread, or before that namespace's first read.
#[must_use]
pub fn namespace_sockets(proc_dir: &Path) -> Option<super::TcpSockets> {
    let root = proc_dir.parent()?;
    let theirs = namespace_of(&proc_dir.join("ns/net"))?;
    if namespace_of(&root.join("self/ns/net"))? == theirs {
        return None;
    }
    held_namespaces().ask((root.to_path_buf(), theirs), proc_dir, Instant::now())
}

/// Reads the asked-for namespaces' tables that are due ([`Namespaces::due`]), least lately read
/// first, each outside the lock: after a reading publishes, off its path. It reads one at least,
/// and another only while the slowest read so far still fits in `budget`; each read is judged by
/// the self-check ([`Namespaces::judge`]), which retires reads that run past [`LANE_BOUND`]. How
/// many it read.
pub fn read_namespaces(budget: Duration) -> usize {
    read_due(budget, true)
}

/// Every due namespace read now, whatever the self-check: a test's fixture namespaces. It is
/// none of the self-check's runs, and it starts the self-check over, so no test's read retires
/// the process's reads, however loaded the machine. Its one caller is a unix test.
#[cfg(all(test, unix))]
pub(crate) fn read_namespaces_now() -> usize {
    held_namespaces().pace = Pace::new();
    read_due(Duration::MAX, false)
}

fn read_due(budget: Duration, judged: bool) -> usize {
    let began = Instant::now();
    let mut slowest = Duration::ZERO;
    let mut read = 0;
    while read == 0 || began.elapsed().saturating_add(slowest) <= budget {
        let Some((key, proc_dir)) = held_namespaces().due(Instant::now()) else {
            break;
        };
        let start = Instant::now();
        let tables = namespace_tables(&proc_dir, key.1);
        let took = start.elapsed();
        slowest = slowest.max(took);
        let mut held = held_namespaces();
        if judged {
            held.judge(took);
        }
        match tables {
            Some(sockets) => held.store(&key, Arc::new(sockets), Instant::now()),
            None => held.failed(&key),
        }
        read += 1;
    }
    read
}

/// `proc_dir`'s namespace's `net/tcp` and `tcp6` rows that hold an inode, by inode. `None` when
/// neither table opens or the process no longer lives in namespace `ns` (it left, or its pid now
/// names another process), so a failed read is never taken for an empty one.
fn namespace_tables(proc_dir: &Path, ns: u64) -> Option<HashMap<u64, TcpRow>> {
    let mut buf = vec![0_u8; NET_TCP_CAP];
    let mut sockets = HashMap::new();
    let mut opened = false;
    for (name, v6) in [("tcp", false), ("tcp6", true)] {
        if let Ok((n, _)) = crate::read_into(&proc_dir.join("net").join(name), &mut buf) {
            opened = true;
            sockets.extend(
                parse_net_tcp(crate::utf8_prefix(&buf[..n]), v6)
                    .into_iter()
                    .filter(|row| row.inode != 0)
                    .map(|row| (row.inode, row)),
            );
        }
    }
    (opened && namespace_of(&proc_dir.join("ns/net")) == Some(ns)).then_some(sockets)
}

fn parse_addr(field: &str, v6: bool) -> Option<(IpAddr, u16)> {
    let (addr, port) = field.split_once(':')?;
    if !addr.bytes().all(|b| b.is_ascii_hexdigit()) || !port.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = if v6 {
        if addr.len() != 32 {
            return None;
        }
        let mut b = [0u8; 16];
        for (i, chunk) in b.chunks_mut(4).enumerate() {
            let w = u32::from_str_radix(addr.get(i * 8..i * 8 + 8)?, 16).ok()?;
            chunk.copy_from_slice(&w.to_ne_bytes());
        }
        IpAddr::V6(Ipv6Addr::from(b))
    } else {
        let w = u32::from_str_radix(addr, 16).ok()?;
        IpAddr::V4(Ipv4Addr::from(w.to_ne_bytes()))
    };
    Some((ip, port))
}

/// Parse a `net/tcp` (or, with `v6`, `net/tcp6`) table, skipping a torn last row.
#[must_use]
pub fn parse_net_tcp(text: &str, v6: bool) -> Vec<TcpRow> {
    whole_lines(text)
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let (_, local_port) = parse_addr(f.get(1)?, v6)?;
            let (rip, rport) = parse_addr(f.get(2)?, v6)?;
            let st_str = *f.get(3)?;
            if !st_str.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let state = TcpState::from_linux(u8::from_str_radix(st_str, 16).ok()?);
            let (_, rx) = f.get(4)?.split_once(':')?;
            if !rx.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some(TcpRow {
                inode: crate::parse_uint(f.get(9)?)?,
                state,
                local_port,
                remote: (rport != 0).then(|| SocketAddr::new(rip, rport)),
                rx_queue: u64::from_str_radix(rx, 16).ok()?,
            })
        })
        .collect()
}

/// When the all-states dump runs, and its self-check, which also judges the other namespaces'
/// reads ([`Namespaces::judge`]).
#[derive(Clone, Debug, Default)]
pub struct Pace {
    /// The runs judged so far, at most [`SELF_CHECK_RUNS`].
    runs: usize,
    /// Of those, the runs past [`LANE_BOUND`].
    slow: usize,
    retired: bool,
}

impl Pace {
    /// A dump that has run none.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            runs: 0,
            slow: 0,
            retired: false,
        }
    }

    /// Whether a dump is due when the last one was tried `since` ago (`None`: never): at once,
    /// then once an [`EPOCH`], or once a [`HIDDEN_EPOCH`] while `hidden` (no window in view shows
    /// the bars or a page that reads them). Never once retired.
    #[must_use]
    pub fn due(&self, since: Option<Duration>, hidden: bool) -> bool {
        let epoch = if hidden { HIDDEN_EPOCH } else { EPOCH };
        !self.retired && since.is_none_or(|since| since >= epoch)
    }

    /// Records one run's wall time. Over the first [`SELF_CHECK_RUNS`], a 95th percentile past
    /// [`LANE_BOUND`] retires the dump (or the reads it judges), as soon as it is certain: at the [`SLOW_RUNS_TO_RETIRE`]th
    /// slow run, so no more slow runs follow it. No lane yet takes a reader that slow off the
    /// vitals thread (A2's S7), so it stops, and the states and RTTs only it gives go unread.
    pub fn took(&mut self, wall: Duration) {
        if self.runs >= SELF_CHECK_RUNS {
            return;
        }
        self.runs += 1;
        if wall > LANE_BOUND {
            self.slow += 1;
        }
        self.retired |= self.slow >= SLOW_RUNS_TO_RETIRE;
    }

    /// Whether the self-check retired the dump.
    #[must_use]
    pub const fn retired(&self) -> bool {
        self.retired
    }
}

#[cfg(target_os = "linux")]
mod live {
    use std::fs::File;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::fs::FileExt as _;
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::time::{Duration, Instant};

    use super::{
        Followed, Lookup, Pace, TcpDump, curr_estab_counts_close_wait, dump_request, lookup_answer,
        lookup_request, median_ms, reading_counts, snmp_curr_estab, sockstat_time_wait,
        tcp_diag_records,
    };
    use crate::linux::{Dump, DumpFault, LINUX_AF_INET, LINUX_AF_INET6};
    use crate::machine::TcpSummary;

    /// The most of `/proc/net/snmp` or `sockstat` read: their `Tcp:` and `TCP:` lines come early.
    const COUNTERS_CAP: usize = 8 * 1024;
    /// One datagram's buffer: the kernel's dump datagrams are at most 32 KiB.
    const DATAGRAM_CAP: usize = 32 * 1024;
    /// The most datagrams one family's dump reads before it is called torn, past any table whose
    /// dump the self-check keeps.
    const DUMP_DATAGRAMS: usize = 4096;

    /// This process's TCP reader: one, since its dump serves every reader in the process.
    static LANE: Mutex<Lane> = Mutex::new(Lane::new());

    struct Lane {
        snmp: Option<File>,
        sockstat: Option<File>,
        /// The lookups' socket.
        diag: Option<OwnedFd>,
        newest: Option<(Instant, Arc<TcpDump>)>,
        /// When the last dump was tried, read or not.
        tried: Option<Instant>,
        followed: Vec<Followed>,
        pace: Pace,
        seq: u32,
        counters: Vec<u8>,
        datagram: Vec<u8>,
    }

    impl Lane {
        const fn new() -> Self {
            Self {
                snmp: None,
                sockstat: None,
                diag: None,
                newest: None,
                tried: None,
                followed: Vec::new(),
                pace: Pace::new(),
                seq: 0,
                counters: Vec::new(),
                datagram: Vec::new(),
            }
        }

        /// Tries a dump now, and keeps it when it reads whole, with `CurrEstab` read beside it
        /// to measure whether this kernel's counts `CLOSE_WAIT`. A `judged` run is one of the
        /// self-check's ([`Pace::took`]); a test's is not.
        fn dump(&mut self, judged: bool) {
            let began = Instant::now();
            self.tried = Some(began);
            self.datagram.resize(DATAGRAM_CAP, 0);
            let dump = take_dump(&mut self.seq, &mut self.datagram);
            if judged {
                self.pace.took(began.elapsed());
            }
            if self.pace.retired() {
                self.newest = None;
                self.followed.clear();
            } else if let Ok(mut dump) = dump {
                let curr_estab = read_kept(
                    &mut self.snmp,
                    "/proc/net/snmp",
                    &mut self.counters,
                    snmp_curr_estab,
                );
                dump.close_wait_in_curr_estab =
                    curr_estab.is_some_and(|c| curr_estab_counts_close_wait(c, &dump.counts));
                self.followed.clone_from(&dump.followed);
                self.newest = Some((Instant::now(), Arc::new(dump)));
            }
        }

        /// The newest dump, when no older than `max_age`.
        fn fresh(&self, max_age: Duration) -> Option<Arc<TcpDump>> {
            self.newest
                .as_ref()
                .filter(|(at, _)| at.elapsed() <= max_age)
                .map(|(_, dump)| Arc::clone(dump))
        }
    }

    fn tcp_lane() -> MutexGuard<'static, Lane> {
        LANE.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// [`crate::tcp_summary_into`] on Linux: the counters, the newest dump's other states (none
    /// held: the counters alone, [`TcpSummary::counters_only`]), and the median RTT of the
    /// followed connections, each looked up by cookie. `None` when the counters cannot be read.
    #[must_use]
    pub fn reading() -> Option<TcpSummary> {
        let mut lane = tcp_lane();
        let lane = &mut *lane;
        let curr_estab = read_kept(
            &mut lane.snmp,
            "/proc/net/snmp",
            &mut lane.counters,
            snmp_curr_estab,
        )?;
        let time_wait = read_kept(
            &mut lane.sockstat,
            "/proc/net/sockstat",
            &mut lane.counters,
            sockstat_time_wait,
        )?;
        let dump = lane.newest.as_ref().map(|(_, dump)| &**dump);
        let counts = reading_counts(curr_estab, time_wait, dump);
        let counters_only = dump.is_none();
        Some(TcpSummary {
            counts,
            counters_only,
            srtt_ms: follow(lane),
        })
    }

    /// After a reading publishes: the all-states dump, when one is due ([`Pace::due`]), else,
    /// while a dump is held, the other namespaces' tables that are due, as many as fit in a
    /// lane's bound ([`super::read_namespaces`]). `hidden`: no window in view shows the bars or a page that
    /// reads them.
    pub fn after_publish(hidden: bool) {
        let mut lane = tcp_lane();
        let since = lane.tried.map(|at| at.elapsed());
        if lane.pace.due(since, hidden) {
            lane.dump(true);
            return;
        }
        let held = lane.newest.is_some();
        drop(lane);
        if held {
            super::read_namespaces(super::LANE_BOUND);
        }
    }

    /// The newest dump at any age, for the socket scopes; `None` before the first or once the
    /// self-check retired it.
    #[must_use]
    pub fn newest() -> Option<Arc<TcpDump>> {
        tcp_lane().newest.as_ref().map(|(_, dump)| Arc::clone(dump))
    }

    /// The newest dump no older than `max_age`, for the jobs pass. When it is older, one is
    /// taken now, on the jobs pass's own path, if one is due on the [`EPOCH`] pace whatever the
    /// deck's audience: a jobs pass is asked for, and names its sockets from a fresh dump. No
    /// consumer dumps more than once an epoch, and that dump spares the next reading's.
    #[must_use]
    pub fn newest_within(max_age: Duration) -> Option<Arc<TcpDump>> {
        let mut lane = tcp_lane();
        if let Some(dump) = lane.fresh(max_age) {
            return Some(dump);
        }
        let since = lane.tried.map(|at| at.elapsed());
        if lane.pace.due(since, false) {
            lane.dump(true);
        }
        lane.fresh(max_age)
    }

    /// A dump taken now whatever the pace: a test's own sockets in it. It is none of the
    /// self-check's runs, and it starts the self-check over, so no test's dump retires the
    /// process's lane, however loaded the machine.
    #[cfg(test)]
    pub(crate) fn dump_now() {
        let mut lane = tcp_lane();
        lane.pace = Pace::new();
        lane.dump(false);
    }

    /// `parse` over `path`'s text, read at offset 0 on a descriptor kept open in `slot` (opened
    /// when empty, dropped when a read fails, so the next reading opens it again).
    fn read_kept<T>(
        slot: &mut Option<File>,
        path: &str,
        buf: &mut Vec<u8>,
        parse: fn(&str) -> Option<T>,
    ) -> Option<T> {
        if slot.is_none() {
            *slot = File::open(path).ok();
        }
        let file = slot.as_ref()?;
        buf.resize(COUNTERS_CAP, 0);
        let mut n = 0;
        while n < buf.len() {
            match file.read_at(&mut buf[n..], n as u64) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    *slot = None;
                    return None;
                }
            }
        }
        parse(crate::utf8_prefix(&buf[..n]))
    }

    /// The followed connections' median RTT, each looked up by cookie; a gone one is followed no
    /// more.
    fn follow(lane: &mut Lane) -> Option<f32> {
        if lane.followed.is_empty() {
            return None;
        }
        if lane.diag.is_none() {
            lane.diag = open_diag();
        }
        let fd = lane.diag.as_ref()?.as_raw_fd();
        lane.datagram.resize(DATAGRAM_CAP, 0);
        let (seq, buf) = (&mut lane.seq, &mut lane.datagram);
        let mut rtts = Vec::with_capacity(lane.followed.len());
        lane.followed.retain(|followed| {
            *seq = seq.wrapping_add(1);
            let answer = if send_all(fd, &lookup_request(followed, *seq)) {
                recv(fd, buf).map_or(Lookup::Unread, |n| lookup_answer(&buf[..n], *seq))
            } else {
                Lookup::Unread
            };
            match answer {
                Lookup::Rtt(ms) => {
                    rtts.push(ms);
                    true
                }
                Lookup::Gone => false,
                Lookup::Unread => true,
            }
        });
        median_ms(&mut rtts)
    }

    /// A non-blocking, close-on-exec `NETLINK_SOCK_DIAG` socket. The kernel answers a request
    /// while it is sent, so its answer is queued before `send` returns.
    fn open_diag() -> Option<OwnedFd> {
        // SAFETY: plain integer arguments; the result is checked before it is owned.
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                libc::NETLINK_SOCK_DIAG,
            )
        };
        // SAFETY: `raw` is a descriptor this call just opened, which nothing else owns.
        (raw >= 0).then(|| unsafe { OwnedFd::from_raw_fd(raw) })
    }

    fn send_all(fd: i32, request: &[u8]) -> bool {
        // SAFETY: `fd` is an open socket and `request` is `request.len()` readable bytes.
        let sent = unsafe { libc::send(fd, request.as_ptr().cast(), request.len(), 0) };
        usize::try_from(sent).is_ok_and(|n| n == request.len())
    }

    /// One datagram into `buf`: its length, or `None` when none is queued or the read failed.
    fn recv(fd: i32, buf: &mut [u8]) -> Option<usize> {
        loop {
            // SAFETY: `fd` is an open socket and `buf` is writable for `buf.len()` bytes.
            let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return usize::try_from(n).ok().filter(|n| *n > 0);
        }
    }

    /// One all-states dump of both families, each on a socket of its own: a dump left unfinished
    /// leaves its socket busy (`EBUSY`) and its unread datagrams queued.
    fn take_dump(seq: &mut u32, buf: &mut [u8]) -> Result<TcpDump, DumpFault> {
        let errno = || std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let mut read = super::DumpRead::default();
        for family in [LINUX_AF_INET, LINUX_AF_INET6] {
            let sock = open_diag().ok_or_else(|| DumpFault::Refused(errno()))?;
            *seq = seq.wrapping_add(1);
            if !send_all(sock.as_raw_fd(), &dump_request(family, *seq)) {
                return Err(DumpFault::Refused(errno()));
            }
            let mut done = false;
            for _ in 0..DUMP_DATAGRAMS {
                let n = recv(sock.as_raw_fd(), buf)
                    .ok_or(DumpFault::Torn("a dump that stopped answering"))?;
                if tcp_diag_records(&buf[..n], *seq, &mut read)? == Dump::Done {
                    done = true;
                    break;
                }
            }
            if !done {
                return Err(DumpFault::Torn("a dump past its bound"));
            }
        }
        Ok(read.finish())
    }
}

#[cfg(test)]
#[path = "linux_tcp_tests.rs"]
mod tests;
