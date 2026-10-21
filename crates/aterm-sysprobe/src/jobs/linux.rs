// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The job probe's Linux arm: `/proc/<pid>/…` read under a root, so it is
//! compiled and tested on every host against fixture trees.
//!
//! * `stat` — state, parent, group, session, terminal (`tty_nr`, `tpgid`),
//!   CPU times (own and reaped children's), start time, major faults;
//! * `status` — uid and context switches; `io` — `syscr + syscw`, `wchar`,
//!   `read_bytes`, `write_bytes` (another user's is refused: counters
//!   unknown, never zero); `schedstat` — time on a core and waiting for one;
//! * `wchan` and `task/<tid>/{stat,wchan}` — the kernel function each waits
//!   in, which Linux names publicly;
//! * `fd/<n>` links and `fdinfo/<n>` — a regular file's offset (an appending
//!   writer's grows, and no path is touched), a pipe's inode and direction,
//!   a socket's inode (then its row in `net/tcp` and `net/tcp6`: state, peer,
//!   receive queue), an epoll set's interests (`tfd:` lines).
//!
//! Every file is read through a bounded buffer; `net/tcp` gets
//! [`NET_TCP_CAP`].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use super::{
    Counters, FdKind, FdReading, Knote, KnoteFilter, MAX_FD_DETAILS, MAX_FDS, MAX_KNOTES,
    MAX_THREADS, ProcFacts, RunState, TcpState, ThreadReading,
};

/// The most of one `net/tcp` table read: about 1,700 sockets.
pub const NET_TCP_CAP: usize = 256 * 1024;

/// The kernel's clock ticks per second (`_SC_CLK_TCK`), 100 where unread.
#[must_use]
pub fn clock_ticks() -> u64 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `sysconf` reads a constant.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        u64::try_from(hz).ok().filter(|h| *h > 0).unwrap_or(100)
    }
    #[cfg(not(target_os = "linux"))]
    {
        100
    }
}

/// The fields of `/proc/<pid>/stat` the probe reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatLine {
    /// `comm`, between the first `(` and the last `)`.
    pub name: String,
    /// The state letter.
    pub state: u8,
    /// The parent.
    pub ppid: u32,
    /// The process group.
    pub pgrp: u32,
    /// The session.
    pub session: u32,
    /// The controlling terminal's device, 0 for none.
    pub tty_nr: u32,
    /// The terminal's foreground group, `-1` (here 0) for none.
    pub tpgid: u32,
    /// Major faults.
    pub majflt: u64,
    /// User time, ticks.
    pub utime: u64,
    /// System time, ticks.
    pub stime: u64,
    /// Reaped children's user time, ticks.
    pub cutime: u64,
    /// Reaped children's system time, ticks.
    pub cstime: u64,
    /// The thread count.
    pub threads: u64,
    /// Start time, ticks since boot.
    pub start: u64,
}

/// Parse a `/proc/<pid>/stat` line; `None` when it is malformed. The name
/// is the text between the first `(` and the LAST `)`, so a `comm` holding
/// parentheses or spaces cannot move a field.
#[must_use]
pub fn parse_stat(text: &str) -> Option<StatLine> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_owned();
    let f: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    // After the name, 0-based: state(0) ppid(1) pgrp(2) session(3) tty_nr(4)
    // tpgid(5) flags(6) minflt(7) cminflt(8) majflt(9) cmajflt(10) utime(11)
    // stime(12) cutime(13) cstime(14) priority(15) nice(16) threads(17)
    // itrealvalue(18) starttime(19).
    let num = |i: usize| -> Option<u64> { f.get(i)?.parse().ok() };
    let tpgid: i64 = f.get(5)?.parse().ok()?;
    let cut = |t: i64| u64::try_from(t.max(0)).unwrap_or(0);
    let cutime: i64 = f.get(13)?.parse().ok()?;
    let cstime: i64 = f.get(14)?.parse().ok()?;
    Some(StatLine {
        name,
        state: *f.first()?.as_bytes().first()?,
        ppid: f.get(1)?.parse().ok()?,
        pgrp: f.get(2)?.parse().ok()?,
        session: f.get(3)?.parse().ok()?,
        tty_nr: f
            .get(4)?
            .parse::<i64>()
            .ok()
            .map_or(0, |t| u32::try_from(t).unwrap_or(0)),
        tpgid: u32::try_from(tpgid).unwrap_or(0),
        majflt: num(9)?,
        utime: num(11)?,
        stime: num(12)?,
        cutime: cut(cutime),
        cstime: cut(cstime),
        threads: num(17)?,
        start: num(19)?,
    })
}

/// `/proc/<pid>/io`'s counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IoLine {
    /// Bytes written through any descriptor.
    pub wchar: Option<u64>,
    /// Read-family system calls.
    pub syscr: Option<u64>,
    /// Write-family system calls.
    pub syscw: Option<u64>,
    /// Bytes read from storage.
    pub read_bytes: Option<u64>,
    /// Bytes written to storage.
    pub write_bytes: Option<u64>,
}

fn keyed(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.split_whitespace().next()?.parse().ok())?
    })
}

/// Parse `/proc/<pid>/io`.
#[must_use]
pub fn parse_io(text: &str) -> IoLine {
    IoLine {
        wchar: keyed(text, "wchar"),
        syscr: keyed(text, "syscr"),
        syscw: keyed(text, "syscw"),
        read_bytes: keyed(text, "read_bytes"),
        write_bytes: keyed(text, "write_bytes"),
    }
}

/// `/proc/<pid>/status`'s uid (the real one, first of four) and context
/// switches (voluntary + involuntary).
#[must_use]
pub fn parse_status(text: &str) -> (Option<u32>, Option<u64>) {
    let uid = text.lines().find_map(|l| {
        l.strip_prefix("Uid:")
            .and_then(|r| r.split_whitespace().next()?.parse().ok())
    });
    let csw = keyed(text, "voluntary_ctxt_switches")
        .zip(keyed(text, "nonvoluntary_ctxt_switches"))
        .map(|(a, b)| a.saturating_add(b));
    (uid, csw)
}

/// `/proc/<pid>/schedstat`: `(ns on a core, ns waiting for one)`.
#[must_use]
pub fn parse_schedstat(text: &str) -> Option<(u64, u64)> {
    let mut f = text.split_whitespace();
    Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
}

/// `/proc/<pid>/wchan`: the kernel function, `None` for `0` or empty (on a
/// core, or not waiting).
#[must_use]
pub fn parse_wchan(text: &str) -> Option<String> {
    let w = text.trim();
    (!w.is_empty() && w != "0").then(|| w.chars().take(64).collect())
}

/// What a `fd/<n>` link names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FdLink {
    /// `pipe:[inode]`.
    Pipe(u64),
    /// `socket:[inode]`.
    Socket(u64),
    /// `anon_inode:[eventpoll]`.
    EventPoll,
    /// `anon_inode:inotify`.
    Inotify,
    /// A terminal under `/dev`.
    Tty,
    /// Any other device under `/dev`.
    Device,
    /// A file's path.
    Path(PathBuf),
    /// Anything else.
    Other,
}

/// Classify a `fd/<n>` link's target.
#[must_use]
pub fn parse_fd_link(target: &str) -> FdLink {
    let inode = |rest: &str| rest.strip_prefix('[')?.strip_suffix(']')?.parse().ok();
    if let Some(n) = target.strip_prefix("pipe:").and_then(inode) {
        FdLink::Pipe(n)
    } else if let Some(n) = target.strip_prefix("socket:").and_then(inode) {
        FdLink::Socket(n)
    } else if target == "anon_inode:[eventpoll]" {
        FdLink::EventPoll
    } else if target == "anon_inode:inotify" {
        FdLink::Inotify
    } else if target.starts_with("/dev/pts/")
        || target.starts_with("/dev/tty")
        || target == "/dev/console"
        || target == "/dev/ptmx"
    {
        FdLink::Tty
    } else if target.starts_with("/dev/") {
        FdLink::Device
    } else if target.starts_with('/')
        && !target.starts_with("/proc/")
        && !target.starts_with("/sys/")
    {
        FdLink::Path(PathBuf::from(target))
    } else {
        FdLink::Other
    }
}

/// `/proc/<pid>/fdinfo/<n>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FdInfo {
    /// The descriptor's offset.
    pub pos: u64,
    /// Its open flags (octal in the file).
    pub flags: u32,
    /// Its inode, where the kernel prints one.
    pub ino: Option<u64>,
    /// An epoll set's interests: `(tfd, events)`.
    pub interests: Vec<(u64, u32)>,
}

impl FdInfo {
    /// Opened write-only (`O_WRONLY`).
    #[must_use]
    pub const fn write_only(&self) -> bool {
        self.flags & 0o3 == 0o1
    }
}

/// Parse `/proc/<pid>/fdinfo/<n>`.
#[must_use]
pub fn parse_fdinfo(text: &str) -> FdInfo {
    let mut info = FdInfo::default();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("pos:") {
            info.pos = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("flags:") {
            info.flags = u32::from_str_radix(v.trim(), 8).unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("ino:") {
            info.ino = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("tfd:") {
            // `tfd:        5 events:       19 data: …` (events in hex).
            let mut f = v.split_whitespace();
            let tfd = f.next().and_then(|t| t.parse().ok());
            let events = f
                .skip_while(|w| *w != "events:")
                .nth(1)
                .and_then(|e| u32::from_str_radix(e, 16).ok());
            if let (Some(t), Some(e)) = (tfd, events)
                && info.interests.len() < MAX_KNOTES
            {
                info.interests.push((t, e));
            }
        }
    }
    info
}

/// One row of `/proc/<pid>/net/tcp` or `tcp6`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpRow {
    /// The socket's inode.
    pub inode: u64,
    /// Its state.
    pub state: TcpState,
    /// Its local port.
    pub local_port: u16,
    /// Its peer (`None` for the wildcard on port 0).
    pub remote: Option<SocketAddr>,
    /// Bytes received, not read.
    pub rx_queue: u64,
}

/// `ADDR:PORT` in the table's hex: the address as the kernel's native-order
/// words, the port as a plain number.
fn parse_addr(field: &str, v6: bool) -> Option<(IpAddr, u16)> {
    let (addr, port) = field.split_once(':')?;
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

/// Parse a `net/tcp` (or, with `v6`, `net/tcp6`) table; malformed rows are
/// skipped, and so is a torn last line.
#[must_use]
pub fn parse_net_tcp(text: &str, v6: bool) -> Vec<TcpRow> {
    let whole = match text.rfind('\n') {
        Some(e) => &text[..=e],
        None => "",
    };
    whole
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let (_, local_port) = parse_addr(f.get(1)?, v6)?;
            let (rip, rport) = parse_addr(f.get(2)?, v6)?;
            let state = TcpState::from_linux(u8::from_str_radix(f.get(3)?, 16).ok()?);
            let (_, rx) = f.get(4)?.split_once(':')?;
            Some(TcpRow {
                inode: f.get(9)?.parse().ok()?,
                state,
                local_port,
                remote: (rport != 0).then(|| SocketAddr::new(rip, rport)),
                rx_queue: u64::from_str_radix(rx, 16).ok()?,
            })
        })
        .collect()
}

/// `Max processes`' soft limit in a `/proc/<pid>/limits`; `None` for
/// `unlimited` or where absent.
#[must_use]
pub fn max_processes(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("Max processes"))
        .and_then(|r| r.split_whitespace().next()?.parse().ok())
}

/// Whether `wchan` is an event wait (`epoll_wait`, `poll`, `select`).
#[must_use]
pub fn event_wait(wchan: &str) -> bool {
    matches!(
        wchan,
        "ep_poll"
            | "do_epoll_wait"
            | "do_select"
            | "do_sys_poll"
            | "poll_schedule_timeout"
            | "core_sys_select"
            | "do_poll"
    ) || wchan.starts_with("ep_poll")
}

fn read_text(path: &Path) -> Option<String> {
    crate::read_bounded(path)
}

fn read_big(path: &Path, cap: usize) -> Option<String> {
    let mut buf = vec![0u8; cap];
    let (n, _) = crate::read_into(path, &mut buf).ok()?;
    Some(crate::utf8_prefix(&buf[..n]).to_owned())
}

fn ticks_ns(ticks: u64, hz: u64) -> u64 {
    u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(hz.max(1))).unwrap_or(u64::MAX)
}

fn threads(dir: &Path, hz: u64) -> Option<(Vec<ThreadReading>, bool)> {
    let mut tids: Vec<u64> = std::fs::read_dir(dir.join("task"))
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect();
    tids.sort_unstable();
    let truncated = tids.len() > MAX_THREADS;
    tids.truncate(MAX_THREADS);
    let out = tids
        .into_iter()
        .filter_map(|tid| {
            let t = dir.join("task").join(tid.to_string());
            let stat = read_text(&t.join("stat")).and_then(|s| parse_stat(&s))?;
            Some(ThreadReading {
                id: tid,
                state: RunState::from_linux(stat.state),
                cpu_ns: Some(ticks_ns(stat.utime.saturating_add(stat.stime), hz)),
                wchan: read_text(&t.join("wchan")).and_then(|w| parse_wchan(&w)),
            })
        })
        .collect();
    Some((out, truncated))
}

/// The process's sockets' TCP rows, from its own network namespace's tables.
fn tcp_rows(dir: &Path) -> Vec<TcpRow> {
    let mut rows = Vec::new();
    for (name, v6) in [("tcp", false), ("tcp6", true)] {
        if let Some(text) = read_big(&dir.join("net").join(name), NET_TCP_CAP) {
            rows.extend(parse_net_tcp(&text, v6));
        }
    }
    rows
}

fn fds(dir: &Path, sleeping: Option<bool>) -> Option<(Vec<FdReading>, bool)> {
    let mut nums: Vec<i32> = std::fs::read_dir(dir.join("fd"))
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect();
    nums.sort_unstable();
    let mut truncated = nums.len() > MAX_FDS;
    nums.truncate(MAX_FDS);
    let mut tcp: Option<Vec<TcpRow>> = None;
    let mut out = Vec::with_capacity(nums.len());
    for (i, fd) in nums.into_iter().enumerate() {
        if i >= MAX_FD_DETAILS {
            truncated = true;
            out.push(FdReading {
                fd,
                kind: FdKind::Other,
            });
            continue;
        }
        let link = std::fs::read_link(dir.join("fd").join(fd.to_string()))
            .ok()
            .map_or(FdLink::Other, |t| parse_fd_link(&t.to_string_lossy()));
        let info = || {
            read_text(&dir.join("fdinfo").join(fd.to_string()))
                .map(|t| parse_fdinfo(&t))
                .unwrap_or_default()
        };
        let kind = match link {
            FdLink::Pipe(ino) => FdKind::Pipe {
                handle: ino,
                peer: ino,
                status: None,
                queued: None,
                write_end: Some(info().write_only()),
            },
            FdLink::Socket(ino) => {
                let rows = tcp.get_or_insert_with(|| tcp_rows(dir));
                match rows.iter().find(|r| r.inode == ino) {
                    Some(r) => FdKind::Tcp {
                        state: r.state,
                        remote: r.remote,
                        local_port: r.local_port,
                        queued_read: Some(r.rx_queue),
                    },
                    None => FdKind::Socket,
                }
            }
            FdLink::EventPoll => FdKind::EventQueue {
                sleeping,
                knotes: Some(
                    info()
                        .interests
                        .into_iter()
                        .map(|(tfd, events)| Knote {
                            ident: tfd,
                            filter: if events & 0x1 != 0 {
                                KnoteFilter::Read
                            } else if events & 0x4 != 0 {
                                KnoteFilter::Write
                            } else {
                                KnoteFilter::Other(0)
                            },
                        })
                        .collect(),
                ),
            },
            FdLink::Inotify => FdKind::Watch,
            FdLink::Tty => FdKind::Device { tty: true },
            FdLink::Device => FdKind::Device { tty: false },
            FdLink::Path(_) => {
                let i = info();
                FdKind::File {
                    regular: true,
                    size: i.pos,
                    dev: 0,
                    ino: i.ino.unwrap_or(0),
                    lf_tail: None,
                }
            }
            FdLink::Other => FdKind::Other,
        };
        out.push(FdReading { fd, kind });
    }
    Some((out, truncated))
}

/// Read one process under `root` (`/proc`, or a fixture tree), with `hz`
/// clock ticks per second; `None` when its `stat` is unreadable.
#[must_use]
pub fn read(root: &Path, pid: u32, want_fds: bool, hz: u64) -> Option<ProcFacts> {
    let dir = root.join(pid.to_string());
    let stat = parse_stat(&read_text(&dir.join("stat"))?)?;
    let state = RunState::from_linux(stat.state);
    let tty = (stat.tty_nr != 0).then_some(stat.tty_nr);
    let (uid, csw) = read_text(&dir.join("status"))
        .map(|t| parse_status(&t))
        .unwrap_or_default();
    let mut facts = ProcFacts {
        pid,
        ppid: stat.ppid,
        pgid: stat.pgrp,
        sid: Some(stat.session),
        start: Some(stat.start),
        uid,
        name: stat.name.clone(),
        state,
        tty,
        tpgid: tty.and(Some(stat.tpgid).filter(|g| *g != 0)),
        ..ProcFacts::default()
    };
    if state == RunState::Zombie {
        return Some(facts);
    }
    // `io` is ptrace-gated: another user's process refuses it, and then
    // every counter is unknown rather than its CPU alone being trusted.
    if let Some(io) = read_text(&dir.join("io")).map(|t| parse_io(&t)) {
        let sched = read_text(&dir.join("schedstat")).and_then(|t| parse_schedstat(&t));
        facts.counters = Some(Counters {
            cpu_ns: ticks_ns(stat.utime.saturating_add(stat.stime), hz),
            child_cpu_ns: Some(ticks_ns(stat.cutime.saturating_add(stat.cstime), hz)),
            syscalls: io.syscr.zip(io.syscw).map(|(r, w)| r.saturating_add(w)),
            wakeups: None,
            ctx_switches: csw,
            disk_read: io.read_bytes,
            disk_written: io.write_bytes,
            logical_writes: io.wchar,
            runnable_ns: sched.map(|(run, wait)| run.saturating_add(wait)),
            pageins: Some(stat.majflt),
        });
    }
    facts.wchan = read_text(&dir.join("wchan")).and_then(|w| parse_wchan(&w));
    if let Some((ts, cut)) = threads(&dir, hz) {
        facts.threads = Some(ts);
        facts.threads_truncated = cut;
    }
    if want_fds {
        let sleeping = facts.wchan.as_deref().map(event_wait);
        if let Some((list, cut)) = fds(&dir, sleeping) {
            facts.fds = Some(list);
            facts.fds_truncated = cut;
        }
    }
    Some(facts)
}
