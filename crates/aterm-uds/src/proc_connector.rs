// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux's proc connector as one listener for the whole process ([`route`], [`Route`], [`Hub`],
//! [`prove`], [`listen`], [`State`], [`Closed`]).
//!
//! From Linux 6.6 (upstream bfdfdc2f3b70) a process in the initial user and pid namespaces may
//! join the connector's `CN_IDX_PROC` group without a capability and hear every process's
//! `exec` and exit on the machine. Every message it hears is the whole machine's, so one socket
//! serves every session of the process: the listener's thread reads each `exec`'s
//! `/proc/<pid>/stat` once, routes it to the session whose shell leads the process's session,
//! and wakes only that session's gather ([`Hub::heard`]); a build's thousands of `exec`s in one
//! tab wake no other.
//!
//! A subscription proves itself before anything is routed ([`prove`]): the kernel must ack it,
//! and the listener must then hear the `exec` of a probe child it spawned itself. Without the
//! ack the group is still joined, and in another user namespace it still hears every event
//! while some other process on the machine listens (the kernel ignores a request from another
//! user or pid namespace, which leaves the socket unfiltered, and Linux 6.8 never lowers its
//! listener count when a listener closes), so events alone prove nothing: they would stop
//! whenever the other listener goes. Without the probe's `exec` an ack proves nothing either. A
//! listener that is not proven reports a closed reason ([`Closed`]) and routes nothing; it never
//! requires root and never asks for a capability.
//!
//! Measured on Linux 6.8 (the Linux lane, `docs/DESIGN-session-log-2026-10-15.md` §10, unit
//! L2): the 4-byte `PROC_CN_MCAST_LISTEN` and 6.6's `proc_input` asking for every event are
//! both acked; a `proc_input` that narrows the events is not, as the kernel's own filter drops
//! its ack (`PROC_EVENT_NONE` is in no mask). So the listener asks for every event first, and
//! narrows to [`MASK`] once that is acked ([`prove`]).
//!
//! It lives in aterm-uds, not aterm-pty, because the process readers it routes by are here
//! (`process::procfs`'s stat line and start tick, the executable path through
//! `aterm_sysprobe`), as is `spawnfd`, which starts its probe; aterm-pty reads one terminal,
//! and the listener serves every session of the process.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Instant;

/// `NETLINK_CONNECTOR`, the protocol of the socket.
pub const NETLINK_CONNECTOR: i32 = 11;
/// `CN_IDX_PROC`: the proc connector's index, and the multicast group the socket joins.
pub const CN_IDX_PROC: u32 = 1;
/// `CN_VAL_PROC`.
pub const CN_VAL_PROC: u32 = 1;
/// `PROC_CN_MCAST_LISTEN`.
pub const PROC_CN_MCAST_LISTEN: u32 = 1;
/// `PROC_EVENT_NONE`: the kind of an ack.
pub const PROC_EVENT_NONE: u32 = 0;
/// `PROC_EVENT_FORK`.
pub const PROC_EVENT_FORK: u32 = 0x1;
/// `PROC_EVENT_EXEC`.
pub const PROC_EVENT_EXEC: u32 = 0x2;
/// `PROC_EVENT_EXIT`.
pub const PROC_EVENT_EXIT: u32 = 0x8000_0000;
/// 6.6's `PROC_EVENT_ALL`: every kind a mask may name.
pub const PROC_EVENT_ALL: u32 = PROC_EVENT_FORK
    | PROC_EVENT_EXEC
    | 0x4
    | 0x40
    | 0x80
    | 0x100
    | 0x200
    | 0x2000_0000
    | 0x4000_0000
    | PROC_EVENT_EXIT;
/// The kinds a live subscription narrows to where the kernel takes a mask (6.6 on): `exec`s,
/// which are routed, and exits, which end what was routed. Forks are left out: routing reads a
/// process's session at its `exec`, and every thread a build starts would send one.
pub const MASK: u32 = PROC_EVENT_EXEC | PROC_EVENT_EXIT;

/// Linux's `EINVAL`, as an ack carries it (this file's tests run on every unix).
pub const LINUX_EINVAL: u32 = 22;
/// Linux's `ENOPROTOOPT`.
pub const LINUX_ENOPROTOOPT: u32 = 92;

const NLMSG_DONE: u16 = 3;
const NLMSG_HDRLEN: usize = 16;
const CN_MSG_LEN: usize = 20;
/// `struct proc_event`'s head: `what`, `cpu`, `timestamp_ns`.
const EVENT_HEAD: usize = 16;

/// The bytes one receive reads at most: a connector datagram is one message of under 100
/// bytes, so this holds every one.
pub const RECV_BUF: usize = 4096;
/// The events one session's inbox holds before it counts the rest lost ([`Route::drain`]).
pub const INBOX_MAX: usize = 256;
/// The group leaders whose exits the hub remembers to route ([`Hub::heard`]), oldest forgotten
/// first.
pub const ROUTED_MAX: usize = 4096;
/// How many times a subscription is sent again when the kernel dropped messages while its ack
/// was awaited (the ack may be among them), and a probe started again when it dropped messages
/// while the probe's `exec` was awaited.
pub const LOST_RESENDS: u32 = 2;

/// How a subscription asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// The 4-byte `PROC_CN_MCAST_LISTEN` every kernel takes: every event.
    Op,
    /// 6.6's `struct proc_input`: `PROC_CN_MCAST_LISTEN` and an event mask. A kernel before 6.6
    /// ignores it unanswered (it takes only the 4-byte form).
    Input(u32),
}

/// One connector message asking to listen in `form`, its `cn_msg` ack field `ack` (the
/// kernel's answer carries `ack + 1`).
#[must_use]
pub fn listen_message(form: Form, ack: u32) -> Vec<u8> {
    let mut data = PROC_CN_MCAST_LISTEN.to_ne_bytes().to_vec();
    if let Form::Input(mask) = form {
        data.extend_from_slice(&mask.to_ne_bytes());
    }
    let total = NLMSG_HDRLEN + CN_MSG_LEN + data.len();
    let mut message = Vec::with_capacity(total);
    message.extend_from_slice(&u32::try_from(total).unwrap_or(u32::MAX).to_ne_bytes());
    message.extend_from_slice(&NLMSG_DONE.to_ne_bytes());
    message.extend_from_slice(&0u16.to_ne_bytes());
    message.extend_from_slice(&0u32.to_ne_bytes());
    message.extend_from_slice(&0u32.to_ne_bytes());
    message.extend_from_slice(&CN_IDX_PROC.to_ne_bytes());
    message.extend_from_slice(&CN_VAL_PROC.to_ne_bytes());
    message.extend_from_slice(&0u32.to_ne_bytes());
    message.extend_from_slice(&ack.to_ne_bytes());
    message.extend_from_slice(&u16::try_from(data.len()).unwrap_or(u16::MAX).to_ne_bytes());
    message.extend_from_slice(&0u16.to_ne_bytes());
    message.extend_from_slice(&data);
    message
}

/// One message the kernel sent the group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    /// An answer to a subscription: its `cn_msg` ack field (the request's plus one) and its
    /// error (`0`: taken).
    Ack {
        /// The request's ack field plus one.
        ack: u32,
        /// Linux's errno, `0` for none.
        err: u32,
    },
    /// Process `pid` (its thread group) replaced its image at `at_ns` on the kernel's
    /// monotonic clock (`ktime_get_ns`).
    Exec {
        /// The process.
        pid: i32,
        /// When, on `CLOCK_MONOTONIC`, in nanoseconds.
        at_ns: u64,
    },
    /// Process `pid`'s main thread exited (a thread's exit is [`Heard::Other`]).
    Exit {
        /// The process.
        pid: i32,
    },
    /// Any other kind, or another connector's message.
    Other,
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let word = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_ne_bytes([word[0], word[1], word[2], word[3]]))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    let lo = u64::from(u32_at(bytes, at)?);
    let hi = u64::from(u32_at(bytes, at.checked_add(4)?)?);
    Some(if cfg!(target_endian = "little") {
        lo | (hi << 32)
    } else {
        (lo << 32) | hi
    })
}

/// The messages of one datagram the socket received, in order, into `out`. A message cut
/// short, or one whose length runs past the datagram, ends the read.
pub fn parse(datagram: &[u8], out: &mut Vec<Heard>) {
    let mut at = 0usize;
    while let Some(len) = u32_at(datagram, at).and_then(|l| usize::try_from(l).ok()) {
        let Some(end) = at.checked_add(len).filter(|e| *e <= datagram.len()) else {
            return;
        };
        if len < NLMSG_HDRLEN {
            return;
        }
        out.push(message(&datagram[at..end]));
        at = end.saturating_add(3) & !3;
    }
}

/// One netlink message: a connector message from the proc connector, or [`Heard::Other`].
fn message(m: &[u8]) -> Heard {
    let cn = NLMSG_HDRLEN;
    if u32_at(m, cn) != Some(CN_IDX_PROC) || u32_at(m, cn + 4) != Some(CN_VAL_PROC) {
        return Heard::Other;
    }
    let ack = u32_at(m, cn + 12).unwrap_or(0);
    let ev = cn + CN_MSG_LEN;
    let data = ev + EVENT_HEAD;
    let pid = |off: usize| u32_at(m, data + off).and_then(|p| i32::try_from(p).ok());
    match u32_at(m, ev) {
        Some(PROC_EVENT_NONE) => {
            u32_at(m, data).map_or(Heard::Other, |err| Heard::Ack { ack, err })
        }
        Some(PROC_EVENT_EXEC) => match (pid(4), u64_at(m, ev + 8)) {
            (Some(pid), Some(at_ns)) => Heard::Exec { pid, at_ns },
            _ => Heard::Other,
        },
        Some(PROC_EVENT_EXIT) => match (pid(0), pid(4)) {
            (Some(task), Some(pid)) if task == pid => Heard::Exit { pid },
            _ => Heard::Other,
        },
        _ => Heard::Other,
    }
}

/// What one receive answered ([`Netlink::recv`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recv {
    /// A datagram of this many bytes.
    Data(usize),
    /// Nothing within the wait.
    Idle,
    /// The kernel dropped messages for want of room in the socket's buffer (`ENOBUFS`).
    Lost,
    /// The socket failed (Linux's errno).
    Failed(i32),
}

/// The socket a listener speaks through: the kernel's, or a test's.
pub trait Netlink {
    /// Send one message (`Err`: the errno).
    ///
    /// # Errors
    /// The send's errno.
    fn send(&mut self, message: &[u8]) -> Result<(), i32>;
    /// Receive one datagram into `buf`, waiting at most `wait_ms` (`-1`: no bound).
    fn recv(&mut self, buf: &mut [u8], wait_ms: i32) -> Recv;
}

/// Why a listener routes nothing: the closed reasons `status exec_watch_why=` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// The kernel refused the socket (no connector built in, a sandbox's filter).
    Socket,
    /// Joining `CN_IDX_PROC` was refused: a kernel before 6.6 without `CAP_NET_ADMIN`.
    Bind,
    /// A subscription could not be sent.
    Send,
    /// No subscription was acked: another user or pid namespace (a container, Flatpak), or a
    /// kernel that answers nothing.
    NoAck,
    /// A subscription was answered with an error.
    Refused,
    /// The probe child could not be started.
    NoProbe,
    /// Acked, but the probe child's `exec` never came (a policy that filters the messages, a
    /// pid namespace whose numbers are not the kernel's).
    ProbeUnheard,
    /// Acked, but the kernel dropped messages while each probe's `exec` was awaited, so whether
    /// it came is not known (a machine too busy at the start for the socket's buffer).
    ProbeLost,
    /// The socket failed after the subscription was proven.
    Failed,
}

impl Closed {
    /// Every reason, in the order [`Closed::as_str`] numbers them.
    pub const ALL: [Self; 9] = [
        Self::Socket,
        Self::Bind,
        Self::Send,
        Self::NoAck,
        Self::Refused,
        Self::NoProbe,
        Self::ProbeUnheard,
        Self::ProbeLost,
        Self::Failed,
    ];

    /// The reason's word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Socket => "socket",
            Self::Bind => "bind",
            Self::Send => "send",
            Self::NoAck => "no-ack",
            Self::Refused => "refused",
            Self::NoProbe => "no-probe",
            Self::ProbeUnheard => "probe-unheard",
            Self::ProbeLost => "probe-lost",
            Self::Failed => "failed",
        }
    }

    const fn index(self) -> u8 {
        match self {
            Self::Socket => 0,
            Self::Bind => 1,
            Self::Send => 2,
            Self::NoAck => 3,
            Self::Refused => 4,
            Self::NoProbe => 5,
            Self::ProbeUnheard => 6,
            Self::ProbeLost => 7,
            Self::Failed => 8,
        }
    }
}

/// Where a listener stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Subscribing and waiting for its proof ([`prove`]).
    Proving,
    /// Proven: it routes. `masked`: the kernel took [`MASK`]; else it hears every kind.
    Live {
        /// Whether the kernel took [`MASK`].
        masked: bool,
    },
    /// Not proven, or failed since: it routes nothing.
    Closed(Closed),
}

const CLOSED_BASE: u8 = 10;

impl State {
    /// Every state a listener can stand at, in the order [`State::as_str`] names them.
    pub const ALL: [Self; 12] = [
        Self::Proving,
        Self::Live { masked: true },
        Self::Live { masked: false },
        Self::Closed(Closed::Socket),
        Self::Closed(Closed::Bind),
        Self::Closed(Closed::Send),
        Self::Closed(Closed::NoAck),
        Self::Closed(Closed::Refused),
        Self::Closed(Closed::NoProbe),
        Self::Closed(Closed::ProbeUnheard),
        Self::Closed(Closed::ProbeLost),
        Self::Closed(Closed::Failed),
    ];

    /// The state's word (`status exec_watch_why=`): `proving`, `live` (the kernel took
    /// [`MASK`]), `live-unmasked` (it hears every kind), or the closed reason
    /// ([`Closed::as_str`]).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proving => "proving",
            Self::Live { masked: true } => "live",
            Self::Live { masked: false } => "live-unmasked",
            Self::Closed(why) => why.as_str(),
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::Proving => 0,
            Self::Live { masked: true } => 1,
            Self::Live { masked: false } => 2,
            Self::Closed(why) => CLOSED_BASE + why.index(),
        }
    }

    fn of_code(code: u8) -> Self {
        match code {
            0 => Self::Proving,
            1 => Self::Live { masked: true },
            2 => Self::Live { masked: false },
            _ => Self::Closed(
                Closed::ALL
                    .get(usize::from(code.saturating_sub(CLOSED_BASE)))
                    .copied()
                    .unwrap_or(Closed::Failed),
            ),
        }
    }
}

/// How long a proof waits, in milliseconds: for each subscription's ack, and for the probe
/// child's `exec`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Waits {
    /// For one ack.
    pub ack_ms: u32,
    /// For the probe's `exec`.
    pub probe_ms: u32,
}

/// The shipping listener's waits: an ack comes at once on an idle machine, a probe's `exec`
/// within milliseconds.
pub const WAITS: Waits = Waits {
    ack_ms: 500,
    probe_ms: 2_000,
};

/// The probe a proof starts: a child of this process that execs ([`prove`]).
pub trait Prober {
    /// Start it; its pid (`None`: it could not be started).
    fn start(&mut self) -> Option<i32>;
    /// The proof is over: reap it.
    fn finish(&mut self);
}

/// A proven subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proven {
    /// Whether the kernel took [`MASK`].
    pub masked: bool,
}

enum Waited<T> {
    Got(T),
    Over,
    Failed,
}

/// Receive through `net` for at most `wait_ms` until `pick` takes a message; a drop the kernel
/// reports sets `lost`.
fn wait_for<T>(
    net: &mut dyn Netlink,
    buf: &mut [u8],
    wait_ms: u32,
    lost: &mut bool,
    mut pick: impl FnMut(Heard) -> Option<T>,
) -> Waited<T> {
    let start = Instant::now();
    let mut heard = Vec::new();
    loop {
        let spent = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);
        let Some(left) = wait_ms.checked_sub(spent).filter(|l| *l > 0) else {
            return Waited::Over;
        };
        match net.recv(buf, i32::try_from(left).unwrap_or(i32::MAX)) {
            Recv::Data(n) => {
                heard.clear();
                parse(buf.get(..n).unwrap_or(buf), &mut heard);
                if let Some(t) = heard.iter().copied().find_map(&mut pick) {
                    return Waited::Got(t);
                }
            }
            Recv::Lost => *lost = true,
            Recv::Idle => return Waited::Over,
            Recv::Failed(_) => return Waited::Failed,
        }
    }
}

enum Answer {
    Acked,
    FormRefused,
    Silent,
}

/// Send one subscription in `form` with ack field `ack` and wait for its answer, sending it
/// again (at most [`LOST_RESENDS`] times) when the kernel dropped messages meanwhile.
fn subscribe(
    net: &mut dyn Netlink,
    buf: &mut [u8],
    form: Form,
    ack: u32,
    wait_ms: u32,
) -> Result<Answer, Closed> {
    let answer = ack.wrapping_add(1);
    for _ in 0..=LOST_RESENDS {
        net.send(&listen_message(form, ack))
            .map_err(|_| Closed::Send)?;
        let mut lost = false;
        let got = wait_for(net, buf, wait_ms, &mut lost, |heard| match heard {
            Heard::Ack { ack, err } if ack == answer => Some(err),
            _ => None,
        });
        match got {
            Waited::Got(0) => return Ok(Answer::Acked),
            Waited::Got(LINUX_EINVAL | LINUX_ENOPROTOOPT) => return Ok(Answer::FormRefused),
            Waited::Got(_) => return Err(Closed::Refused),
            Waited::Failed => return Err(Closed::Failed),
            Waited::Over if lost => {}
            Waited::Over => return Ok(Answer::Silent),
        }
    }
    Ok(Answer::Silent)
}

/// Subscribe through `net` and prove it, the acks' fields counted from `nonce`: first 6.6's
/// `proc_input` asking for every event, whose ack the kernel's filter lets through; unanswered
/// (a kernel before 6.6) or refused as a form (`EINVAL`, `ENOPROTOOPT`), the 4-byte form, which
/// hears every kind (mask unsupported). Once the `proc_input` is acked, [`MASK`] narrows it (the
/// kernel's filter drops that ack; one answering it with an error leaves every kind). Then the
/// probe child `prober` starts must be heard `exec`ing; when the kernel dropped messages while
/// it was awaited (its `exec` may be among them), a probe is started again, at most
/// [`LOST_RESENDS`] times.
///
/// # Errors
/// The closed reason: [`Closed::Send`], [`Closed::NoAck`] (no form acked), [`Closed::Refused`],
/// [`Closed::NoProbe`], [`Closed::ProbeUnheard`], [`Closed::ProbeLost`] (messages dropped in
/// every probe's wait) or [`Closed::Failed`].
pub fn prove(
    net: &mut dyn Netlink,
    prober: &mut dyn Prober,
    nonce: u32,
    waits: Waits,
) -> Result<Proven, Closed> {
    let mut buf = vec![0u8; RECV_BUF];
    let input = match subscribe(
        net,
        &mut buf,
        Form::Input(PROC_EVENT_ALL),
        nonce,
        waits.ack_ms,
    )? {
        Answer::Acked => true,
        Answer::FormRefused | Answer::Silent => false,
    };
    if !input {
        match subscribe(net, &mut buf, Form::Op, nonce.wrapping_add(2), waits.ack_ms)? {
            Answer::Acked => {}
            Answer::FormRefused => return Err(Closed::Refused),
            Answer::Silent => return Err(Closed::NoAck),
        }
    }
    let mask_ack = nonce.wrapping_add(4);
    if input {
        net.send(&listen_message(Form::Input(MASK), mask_ack))
            .map_err(|_| Closed::Send)?;
    }
    let mut mask_refused = false;
    let mut lost = false;
    let mut heard = Waited::Over;
    for _ in 0..=LOST_RESENDS {
        let Some(probe) = prober.start() else {
            return Err(Closed::NoProbe);
        };
        lost = false;
        heard = wait_for(
            net,
            &mut buf,
            waits.probe_ms,
            &mut lost,
            |heard| match heard {
                Heard::Exec { pid, .. } if pid == probe => Some(()),
                Heard::Ack { ack, err } if ack == mask_ack.wrapping_add(1) && err != 0 => {
                    mask_refused = true;
                    None
                }
                _ => None,
            },
        );
        prober.finish();
        if !lost || !matches!(heard, Waited::Over) {
            break;
        }
    }
    if mask_refused {
        net.send(&listen_message(Form::Op, nonce.wrapping_add(6)))
            .map_err(|_| Closed::Send)?;
    }
    match heard {
        Waited::Got(()) => Ok(Proven {
            masked: input && !mask_refused,
        }),
        Waited::Over if lost => Err(Closed::ProbeLost),
        Waited::Over => Err(Closed::ProbeUnheard),
        Waited::Failed => Err(Closed::Failed),
    }
}

/// An image a process mapped: the executable the kernel names (Linux `/proc/<pid>/exe`) and
/// the identity of the mapped file (device, inode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The executable's path.
    pub path: PathBuf,
    /// The mapped file's device and inode.
    pub file: Option<(u64, u64)>,
}

/// One `exec` routed to a session, read by the listener's thread just after it was heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exec {
    /// The process.
    pub pid: i32,
    /// Its process group.
    pub pgid: i32,
    /// Its terminal's foreground group when it was read (`0`: none).
    pub tpgid: i32,
    /// Its start on the boot clock (`CLOCK_BOOTTIME`), in nanoseconds, as `/proc/<pid>/stat`
    /// reads it (a tick's floor).
    pub start_ns: u64,
    /// When it exec'd, on the boot clock, in nanoseconds.
    pub at_ns: u64,
    /// The image it mapped.
    pub image: Image,
    /// Its environment's `PATH`, read for a group's leader only.
    pub env_path: Option<Box<str>>,
}

/// What a session's route hands its gather.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// One of its processes exec'd.
    Exec(Exec),
    /// A group leader an `exec` was routed for exited.
    Exit {
        /// The process.
        pid: i32,
    },
}

/// What one `/proc/<pid>/stat` read shows of a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    /// Its process group.
    pub pgrp: i32,
    /// Its session (the shell that leads it).
    pub session: i32,
    /// Its terminal's foreground group (`0`: none).
    pub tpgid: i32,
    /// Its start on the boot clock, in nanoseconds (a tick's floor).
    pub start_ns: u64,
    /// Whether it has exec'd since its fork (`PF_FORKNOEXEC` clear).
    pub execd: bool,
}

/// How the hub reads a process: the kernel's `/proc`, or a test's.
pub trait Kernel {
    /// `pid`'s stat line (`None`: gone, reaped, unreadable; a zombie still reads).
    fn stat(&mut self, pid: i32) -> Option<Stat>;
    /// The executable `pid` maps (`None`: gone, exited, another user's): Linux's
    /// `/proc/<pid>/exe`, which reads only until the process exits.
    fn exe(&mut self, pid: i32) -> Option<PathBuf>;
    /// The identity (device, inode) of the file `pid` maps, while it maps one.
    fn exe_file(&mut self, pid: i32) -> Option<(u64, u64)>;
    /// `pid`'s environment's `PATH`.
    fn env_path(&mut self, pid: i32) -> Option<Box<str>>;
    /// The boot clock less the monotonic one now, in nanoseconds: an event is stamped on the
    /// monotonic clock and a start reads on the boot clock.
    fn boot_offset_ns(&mut self) -> Option<u64>;
}

#[derive(Debug, Default)]
struct Queue {
    events: VecDeque<Event>,
    lost: bool,
    signaled: bool,
}

/// One session's inbox: what the hub routed to it, and the pipe whose read end its gather polls.
#[derive(Debug)]
struct Inbox {
    shell: i32,
    queue: Mutex<Queue>,
    wake_rd: std::os::fd::OwnedFd,
    wake_wr: std::os::fd::OwnedFd,
}

/// Make a descriptor non-blocking.
fn nonblocking(fd: &std::os::fd::OwnedFd) -> bool {
    use std::os::fd::AsRawFd as _;
    // SAFETY: F_GETFL/F_SETFL on a descriptor this process owns.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        flags >= 0 && libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
    }
}

impl Inbox {
    fn new(shell: i32) -> Option<Self> {
        let (rd, wr) = std::io::pipe().ok()?;
        let (wake_rd, wake_wr) = (
            std::os::fd::OwnedFd::from(rd),
            std::os::fd::OwnedFd::from(wr),
        );
        (nonblocking(&wake_rd) && nonblocking(&wake_wr)).then(|| Self {
            shell,
            queue: Mutex::default(),
            wake_rd,
            wake_wr,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wake the gather once for everything queued since its last drain.
    fn signal(&self, queue: &mut Queue) {
        use std::os::fd::AsRawFd as _;
        if !std::mem::replace(&mut queue.signaled, true) {
            // SAFETY: one byte from a live buffer into this inbox's own non-blocking pipe; a
            // full pipe already wakes the gather.
            unsafe { libc::write(self.wake_wr.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
        }
    }

    fn push(&self, event: Event) {
        let mut queue = self.lock();
        if queue.events.len() >= INBOX_MAX {
            queue.lost = true;
        } else {
            queue.events.push_back(event);
        }
        self.signal(&mut queue);
    }

    fn lose(&self) {
        let mut queue = self.lock();
        queue.lost = true;
        self.signal(&mut queue);
    }
}

/// The listener's routes: every session that asked, the processes whose `exec`s it routed, and
/// where it stands ([`State`]).
#[derive(Debug)]
pub struct Hub {
    state: AtomicU8,
    routes: Mutex<Vec<Arc<Inbox>>>,
    routed: Mutex<VecDeque<(i32, Weak<Inbox>)>>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    /// A hub proving its subscription, routing nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
            routes: Mutex::new(Vec::new()),
            routed: Mutex::new(VecDeque::new()),
        }
    }

    /// Where its listener stands.
    #[must_use]
    pub fn state(&self) -> State {
        State::of_code(self.state.load(Ordering::Acquire))
    }

    /// Its listener now stands at `state`.
    pub fn set_state(&self, state: State) {
        self.state.store(state.code(), Ordering::Release);
    }

    /// Route the `exec`s of the session `shell` leads to a new inbox (`None`: no pipe could be
    /// made, `EMFILE`).
    #[must_use]
    pub fn register(self: &Arc<Self>, shell: i32) -> Option<Route> {
        let inbox = Arc::new(Inbox::new(shell)?);
        self.routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&inbox));
        Some(Route {
            inbox,
            hub: Arc::clone(self),
        })
    }

    fn unregister(&self, inbox: &Arc<Inbox>) {
        self.routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|i| !Arc::ptr_eq(i, inbox));
    }

    /// Route what one datagram held, reading each process through `kernel`: an `exec` goes to
    /// the session whose shell leads the process's session, read from `/proc/<pid>/stat` once
    /// its executable was read (a program that runs for a millisecond may exit within
    /// microseconds of its `exec`, and its executable reads only until then, while its stat
    /// reads until it is reaped), when that start is no later than the `exec` (a pid reused
    /// since started after it, so it is never trusted; the process the stat reads was then the
    /// one whose executable was read first) and it has exec'd since its fork. The mapped file's
    /// identity is read with the executable, before that stat, which vouches for both: a second
    /// stat after it, as an earlier form read, would only shorten the time a short program's
    /// file still reads. A group leader's exit goes to the session its `exec` went to (what an
    /// exit ends there is a group's: a stopped drawer's mark goes with its leader); no other
    /// process's exit is routed, so a build's thousands of compiler exits wake nothing. A
    /// process no session leads costs one `readlink`, one `stat` of its file and one stat read,
    /// and wakes nothing (one that exited first, the `readlink` alone).
    pub fn heard(&self, heard: &[Heard], kernel: &mut dyn Kernel) {
        for h in heard {
            match *h {
                Heard::Exec { pid, at_ns } => self.exec(pid, at_ns, kernel),
                Heard::Exit { pid } => self.exit(pid),
                Heard::Ack { .. } | Heard::Other => {}
            }
        }
    }

    fn inbox_of(&self, session: i32) -> Option<Arc<Inbox>> {
        self.routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|i| i.shell == session)
            .cloned()
    }

    fn exec(&self, pid: i32, at_ns: u64, kernel: &mut dyn Kernel) {
        if pid <= 1
            || self
                .routes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        {
            return;
        }
        let Some(path) = kernel.exe(pid) else {
            return;
        };
        let file = kernel.exe_file(pid);
        let Some(first) = kernel.stat(pid) else {
            return;
        };
        let Some(inbox) = self.inbox_of(first.session) else {
            return;
        };
        let Some(at_ns) = kernel
            .boot_offset_ns()
            .and_then(|offset| at_ns.checked_add(offset))
        else {
            return;
        };
        if !first.execd || first.start_ns > at_ns {
            return;
        }
        let leader = first.pgrp == pid;
        let env_path = if leader { kernel.env_path(pid) } else { None };
        if leader {
            self.note_routed(pid, &inbox);
        }
        inbox.push(Event::Exec(Exec {
            pid,
            pgid: first.pgrp,
            tpgid: first.tpgid,
            start_ns: first.start_ns,
            at_ns,
            image: Image { path, file },
            env_path,
        }));
    }

    fn note_routed(&self, pid: i32, inbox: &Arc<Inbox>) {
        let mut routed = self.routed.lock().unwrap_or_else(PoisonError::into_inner);
        routed.retain(|(p, _)| *p != pid);
        if routed.len() >= ROUTED_MAX {
            routed.pop_front();
        }
        routed.push_back((pid, Arc::downgrade(inbox)));
    }

    fn exit(&self, pid: i32) {
        let gone = {
            let mut routed = self.routed.lock().unwrap_or_else(PoisonError::into_inner);
            routed
                .iter()
                .position(|(p, _)| *p == pid)
                .and_then(|i| routed.remove(i))
        };
        if let Some(inbox) = gone.and_then(|(_, inbox)| inbox.upgrade()) {
            inbox.push(Event::Exit { pid });
        }
    }

    /// The kernel dropped messages (`ENOBUFS`): any session may have lost an `exec`, so each is
    /// told ([`Route::drain`]).
    pub fn lost(&self) {
        let routes = self
            .routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for inbox in routes {
            inbox.lose();
        }
    }
}

/// What [`listen`] stopped at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listened {
    /// A wait passed with nothing to read.
    Idle,
    /// The socket failed; the hub is [`Closed::Failed`].
    Failed,
}

/// Route what `net` receives through `hub`, reading processes through `kernel`, until a wait of
/// `wait_ms` passes with nothing (`-1`: none does) or the socket fails.
pub fn listen(net: &mut dyn Netlink, hub: &Hub, kernel: &mut dyn Kernel, wait_ms: i32) -> Listened {
    let mut buf = vec![0u8; RECV_BUF];
    let mut heard = Vec::new();
    loop {
        match net.recv(&mut buf, wait_ms) {
            Recv::Data(n) => {
                heard.clear();
                parse(buf.get(..n).unwrap_or(&buf), &mut heard);
                hub.heard(&heard, kernel);
            }
            Recv::Lost => hub.lost(),
            Recv::Idle => return Listened::Idle,
            Recv::Failed(_) => {
                hub.set_state(State::Closed(Closed::Failed));
                return Listened::Failed;
            }
        }
    }
}

/// One session's route: the read end its gather polls, and what the hub routed to it.
/// Dropping it ends the routing.
#[derive(Debug)]
pub struct Route {
    inbox: Arc<Inbox>,
    hub: Arc<Hub>,
}

impl Drop for Route {
    fn drop(&mut self) {
        self.hub.unregister(&self.inbox);
    }
}

impl Route {
    /// The descriptor `poll(2)` reports readable once something is routed.
    #[must_use]
    pub fn fd(&self) -> i32 {
        use std::os::fd::AsRawFd as _;
        self.inbox.wake_rd.as_raw_fd()
    }

    /// The shell whose session it routes.
    #[must_use]
    pub fn shell(&self) -> i32 {
        self.inbox.shell
    }

    /// Where the listener stands.
    #[must_use]
    pub fn state(&self) -> State {
        self.hub.state()
    }

    /// A handle that reads where the listener stands from any thread.
    #[must_use]
    pub fn state_reader(&self) -> StateReader {
        StateReader(Arc::clone(&self.hub))
    }

    /// Take everything routed since the last drain into `out`, oldest first, and whether any
    /// was lost since (the kernel dropped messages, or the inbox was full at [`INBOX_MAX`]).
    pub fn drain(&self, out: &mut Vec<Event>) -> bool {
        use std::os::fd::AsRawFd as _;
        let mut sink = [0u8; 64];
        // SAFETY: reading into `sink`'s 64 bytes from this inbox's own non-blocking pipe.
        while unsafe {
            libc::read(
                self.inbox.wake_rd.as_raw_fd(),
                sink.as_mut_ptr().cast(),
                sink.len(),
            )
        } > 0
        {}
        let mut queue = self.inbox.lock();
        out.extend(queue.events.drain(..));
        queue.signaled = false;
        std::mem::take(&mut queue.lost)
    }

    /// A route of its own hub, for a reader under test: it routes only what its
    /// [`RouteScript`] posts.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn scripted(shell: i32) -> Option<(Self, RouteScript)> {
        let hub = Arc::new(Hub::new());
        hub.set_state(State::Live { masked: true });
        let route = hub.register(shell)?;
        let script = RouteScript {
            inbox: Arc::clone(&route.inbox),
            hub: Arc::clone(&hub),
        };
        Some((route, script))
    }
}

/// Reads where a route's listener stands ([`Route::state_reader`]).
#[derive(Clone, Debug)]
pub struct StateReader(Arc<Hub>);

impl StateReader {
    /// Where the listener stands now.
    #[must_use]
    pub fn get(&self) -> State {
        self.0.state()
    }
}

/// The test side of a [`Route::scripted`] route.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug)]
pub struct RouteScript {
    inbox: Arc<Inbox>,
    hub: Arc<Hub>,
}

#[cfg(any(test, debug_assertions))]
impl RouteScript {
    /// Route `events` and wake the gather.
    pub fn post(&self, events: &[Event]) {
        for event in events {
            self.inbox.push(event.clone());
        }
    }

    /// The kernel dropped messages.
    pub fn lose(&self) {
        self.inbox.lose();
    }

    /// The listener now stands at `state`.
    pub fn set_state(&self, state: State) {
        self.hub.set_state(state);
    }

    /// Events routed and not yet drained.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.inbox.lock().events.len()
    }
}

/// The ack field a process's proof counts from: its pid's, so two aterm processes subscribing
/// at once never take each other's acks.
#[cfg(target_os = "linux")]
fn nonce() -> u32 {
    0x4c32_0000_u32 ^ std::process::id().wrapping_mul(16)
}

#[cfg(target_os = "linux")]
static SHARED: std::sync::OnceLock<Arc<Hub>> = std::sync::OnceLock::new();

/// Route the `exec`s of the session `shell` leads through this process's one listener, which
/// the first call starts on a thread of its own (`aterm-proc-connector`): it subscribes and
/// proves itself ([`prove`]), then routes ([`listen`]). `None`: no route could be made.
#[cfg(target_os = "linux")]
#[must_use]
pub fn route(shell: i32) -> Option<Route> {
    SHARED.get_or_init(start).register(shell)
}

/// See the Linux twin: no proc connector here.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub const fn route(_shell: i32) -> Option<Route> {
    None
}

/// Where this process's listener stands (`None`: none was started).
#[must_use]
pub fn shared_state() -> Option<State> {
    #[cfg(target_os = "linux")]
    {
        SHARED.get().map(|hub| hub.state())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn start() -> Arc<Hub> {
    let hub = Arc::new(Hub::new());
    let run = Arc::clone(&hub);
    let started = std::thread::Builder::new()
        .name("aterm-proc-connector".into())
        .stack_size(256 * 1024)
        .spawn(move || run_listener(&run));
    if started.is_err() {
        hub.set_state(State::Closed(Closed::Failed));
    }
    hub
}

#[cfg(target_os = "linux")]
fn run_listener(hub: &Hub) {
    let mut net = match linux::Socket::open() {
        Ok(net) => net,
        Err(why) => return hub.set_state(State::Closed(why)),
    };
    let mut kernel = linux::Procfs::new();
    match prove(&mut net, &mut linux::ShProbe::default(), nonce(), WAITS) {
        Ok(proven) => hub.set_state(State::Live {
            masked: proven.masked,
        }),
        Err(why) => return hub.set_state(State::Closed(why)),
    }
    while listen(&mut net, hub, &mut kernel, -1) == Listened::Idle {}
}

/// The kernel's socket and `/proc`.
#[cfg(target_os = "linux")]
pub mod linux {
    use super::{CN_IDX_PROC, Closed, NETLINK_CONNECTOR, Netlink, Prober, Recv, Stat};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    /// The receive buffer a listener asks for: a build's burst of `exec`s while the thread is
    /// descheduled. The kernel caps it at `rmem_max` without a word; nothing forces it.
    const RCVBUF: libc::c_int = 4 << 20;

    fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    /// A `NETLINK_CONNECTOR` socket that joined `CN_IDX_PROC`.
    #[derive(Debug)]
    pub struct Socket {
        fd: OwnedFd,
    }

    impl Socket {
        /// Open it and join the group.
        ///
        /// # Errors
        /// [`Closed::Socket`] or [`Closed::Bind`] (a kernel before 6.6 without
        /// `CAP_NET_ADMIN`).
        #[cfg_attr(trust_verify, trust::skip)]
        pub fn open() -> Result<Self, Closed> {
            // SAFETY: socket(2) takes three integers and returns a new descriptor or -1.
            let raw = unsafe {
                libc::socket(
                    libc::AF_NETLINK,
                    libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                    NETLINK_CONNECTOR,
                )
            };
            if raw < 0 {
                return Err(Closed::Socket);
            }
            // SAFETY: `raw` is a fresh descriptor this process exclusively owns.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            let addr = libc::sockaddr_nl {
                nl_family: libc::sa_family_t::try_from(libc::AF_NETLINK).unwrap_or(16),
                nl_pid: 0,
                nl_groups: CN_IDX_PROC,
            };
            // SAFETY: `addr` is one live sockaddr_nl of the length passed.
            let bound = unsafe {
                libc::bind(
                    fd.as_raw_fd(),
                    (&raw const addr).cast(),
                    libc::socklen_t::try_from(size_of::<libc::sockaddr_nl>()).unwrap_or(12),
                )
            };
            if bound != 0 {
                return Err(Closed::Bind);
            }
            let rcvbuf = RCVBUF;
            // SAFETY: one live c_int of the length passed; a refused size leaves the default.
            unsafe {
                libc::setsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    (&raw const rcvbuf).cast(),
                    libc::socklen_t::try_from(size_of::<libc::c_int>()).unwrap_or(4),
                )
            };
            Ok(Self { fd })
        }
    }

    impl Netlink for Socket {
        #[cfg_attr(trust_verify, trust::skip)]
        fn send(&mut self, message: &[u8]) -> Result<(), i32> {
            // SAFETY: `message` is live for the call, its length passed.
            let sent = unsafe {
                libc::send(
                    self.fd.as_raw_fd(),
                    message.as_ptr().cast(),
                    message.len(),
                    0,
                )
            };
            if usize::try_from(sent).ok() == Some(message.len()) {
                Ok(())
            } else {
                Err(errno())
            }
        }

        #[cfg_attr(trust_verify, trust::skip)]
        fn recv(&mut self, buf: &mut [u8], wait_ms: i32) -> Recv {
            let mut fds = [libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            // SAFETY: one pollfd, live for the call.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, wait_ms) };
            if ready == 0 {
                return Recv::Idle;
            }
            if ready < 0 {
                return if errno() == libc::EINTR {
                    Recv::Idle
                } else {
                    Recv::Failed(errno())
                };
            }
            // SAFETY: `buf` is live and writable for its length.
            let n = unsafe {
                libc::recv(
                    self.fd.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            match usize::try_from(n) {
                Ok(0) => Recv::Idle,
                Ok(n) => Recv::Data(n),
                Err(_) => match errno() {
                    libc::ENOBUFS => Recv::Lost,
                    libc::EAGAIN | libc::EINTR => Recv::Idle,
                    other => Recv::Failed(other),
                },
            }
        }
    }

    /// The probe: `/bin/sh -c :`, a child that execs once and exits.
    #[derive(Debug, Default)]
    pub struct ShProbe {
        child: Option<crate::spawnfd::Child>,
    }

    impl Prober for ShProbe {
        fn start(&mut self) -> Option<i32> {
            let child = crate::spawnfd::command("/bin/sh")
                .args(["-c", ":"])
                .stdin(crate::spawnfd::Stdio::null())
                .stdout(crate::spawnfd::Stdio::null())
                .stderr(crate::spawnfd::Stdio::null())
                .spawn()
                .ok()?;
            let pid = i32::try_from(child.id()).ok();
            self.child = Some(child);
            pid
        }

        fn finish(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.wait();
            }
        }
    }

    /// The kernel's `/proc`.
    #[derive(Debug)]
    pub struct Procfs {
        per_second: Option<u64>,
    }

    impl Procfs {
        /// Reads with the kernel's tick rate (`AT_CLKTCK`).
        #[must_use]
        pub fn new() -> Self {
            Self {
                per_second: crate::process::stat_ticks_per_second(),
            }
        }
    }

    impl Default for Procfs {
        fn default() -> Self {
            Self::new()
        }
    }

    /// The monotonic clock now, in nanoseconds.
    #[cfg_attr(trust_verify, trust::skip)]
    fn monotonic_ns() -> Option<u64> {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `now` is one writable timespec that outlives the call.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
            return None;
        }
        u64::try_from(now.tv_sec)
            .ok()?
            .checked_mul(1_000_000_000)?
            .checked_add(u64::try_from(now.tv_nsec).ok()?)
    }

    impl super::Kernel for Procfs {
        fn stat(&mut self, pid: i32) -> Option<Stat> {
            let text =
                crate::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
                    .ok()?;
            super::stat_of(&text, pid, self.per_second?)
        }

        fn exe(&mut self, pid: i32) -> Option<std::path::PathBuf> {
            aterm_sysprobe::process_path(u32::try_from(pid).ok()?)
        }

        fn exe_file(&mut self, pid: i32) -> Option<(u64, u64)> {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata(format!("/proc/{pid}/exe"))
                .ok()
                .map(|m| (m.dev(), m.ino()))
        }

        fn env_path(&mut self, pid: i32) -> Option<Box<str>> {
            let environ = aterm_sysprobe::read_bounded(std::path::Path::new(&format!(
                "/proc/{pid}/environ"
            )))?;
            super::env_path_of(&environ)
        }

        fn boot_offset_ns(&mut self) -> Option<u64> {
            let mono = monotonic_ns()?;
            let boot = crate::process::boot_clock_now()?.0;
            boot.checked_sub(mono)
        }
    }
}

/// The longest `PATH` an [`Exec`] carries: a longer one steers nothing.
pub const ENV_PATH_MAX: usize = 4096;

/// The `PATH` of a process's environment block (`NUL`-separated), at most [`ENV_PATH_MAX`]
/// bytes; no other variable is kept.
#[must_use]
pub fn env_path_of(environ: &str) -> Option<Box<str>> {
    environ
        .split('\0')
        .find_map(|kv| kv.strip_prefix("PATH="))
        .filter(|p| p.len() <= ENV_PATH_MAX)
        .map(Into::into)
}

/// `pid`'s [`Stat`] from its `/proc/<pid>/stat` text at `per_second` ticks a second: `None` for
/// another pid's line, one cut short, or a group, session or start that does not read. A zombie
/// reads: a program that exited just after its executable was read is still that program
/// ([`Hub::heard`]).
#[cfg(any(unix, windows))]
#[must_use]
pub fn stat_of(text: &str, pid: i32, per_second: u64) -> Option<Stat> {
    const PF_FORKNOEXEC: u64 = 0x40;
    let line = crate::process::procfs::stat_line(text, pid)?;
    Some(Stat {
        pgrp: i32::try_from(line.pgrp).ok()?,
        session: i32::try_from(line.session).ok()?,
        tpgid: i32::try_from(line.tpgid).unwrap_or(0),
        start_ns: crate::process::procfs::tick_start(line.start, per_second)?.0,
        execd: line.flags & PF_FORKNOEXEC == 0,
    })
}

#[cfg(test)]
#[path = "proc_connector_tests.rs"]
mod tests;
