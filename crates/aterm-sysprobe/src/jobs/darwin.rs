// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Decoders for `libproc` descriptor layouts, `proc_archinfo` and `kinfo_proc` ([`SDK_ROWS`], [`kinfo`]).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// `PROC_PIDLISTTHREADS`: a process's thread handles (`uint64_t` each).
pub const PROC_PIDLISTTHREADS: i32 = 6;
/// `PROC_PIDARCHINFO`: a process's CPU type and subtype (`struct proc_archinfo`).
pub const PROC_PIDARCHINFO: i32 = 19;
/// `sizeof(struct proc_archinfo)`.
pub const ARCHINFO_SIZE: usize = 8;
/// `offsetof(struct proc_archinfo, p_cputype)` (`cpu_type_t`).
pub const ARCHINFO_CPUTYPE_AT: usize = 0;
/// `CPU_TYPE_X86_64`: an x86_64 process, which an arm64 Mac runs under Rosetta.
pub const CPU_TYPE_X86_64: i32 = 0x0100_0007;
/// `SSTOP` in `pbi_status`: stopped (a start-suspended spawn, `SIGSTOP`, a tracer).
pub const SSTOP: u32 = 4;
/// `PROC_FLAG_TRACED` in `pbi_flags`: a debugger traces the process.
pub const PROC_FLAG_TRACED: u32 = 0x2;
/// `PROC_PIDFDVNODEPATHINFO`: a vnode descriptor with its path.
pub const PROC_PIDFDVNODEPATHINFO: i32 = 2;
/// `PROC_PIDFDSOCKETINFO`: a socket descriptor.
pub const PROC_PIDFDSOCKETINFO: i32 = 3;
/// `PROC_PIDFDPIPEINFO`: a pipe descriptor.
pub const PROC_PIDFDPIPEINFO: i32 = 6;
/// `PROC_PIDFDKQUEUEINFO`: a kqueue descriptor.
pub const PROC_PIDFDKQUEUEINFO: i32 = 7;
/// `PROC_PIDFDKQUEUE_EXTINFO` (private, `sys/proc_info_private.h`): a kqueue's knotes.
pub const PROC_PIDFDKQUEUE_EXTINFO: i32 = 9;
/// `PROC_KQUEUE_SLEEP`: a thread sleeps on the kqueue.
pub const PROC_KQUEUE_SLEEP: u32 = 2;
/// `PROC_FLAG_SLEADER`: the process leads its session.
pub const PROC_FLAG_SLEADER: u32 = 0x20;
/// `PROC_FLAG_CONTROLT`: the process has a controlling terminal.
pub const PROC_FLAG_CONTROLT: u32 = 0x80;
/// `FREAD` in `proc_fileinfo.fi_openflags`.
pub const FREAD: u32 = 0x1;
/// `FWRITE` in `proc_fileinfo.fi_openflags`.
pub const FWRITE: u32 = 0x2;
/// `PIPE_WANTR` (kernel-internal): a reader waits on this pipe end.
pub const PIPE_WANTR: u32 = 0x8;
/// `PIPE_WANTW` (kernel-internal): a writer waits on this pipe end.
pub const PIPE_WANTW: u32 = 0x10;
/// `SOCKINFO_IN`: an IP socket other than TCP.
pub const SOCKINFO_IN: i32 = 1;
/// `SOCKINFO_TCP`: a TCP socket.
pub const SOCKINFO_TCP: i32 = 2;
/// `INI_IPV4` in `insi_vflag`.
pub const INI_IPV4: u8 = 0x1;
/// `INI_IPV6` in `insi_vflag`.
pub const INI_IPV6: u8 = 0x2;
/// `S_IFMT`, `S_IFREG`, `S_IFCHR` of `vst_mode`.
pub const S_IFMT: u16 = 0o170_000;
/// A regular file.
pub const S_IFREG: u16 = 0o100_000;
/// A character device.
pub const S_IFCHR: u16 = 0o020_000;
/// `NODEV`: `e_tdev` of a process with no controlling terminal.
pub const NODEV: u32 = u32::MAX;

/// `sizeof(struct vnode_fdinfowithpath)`.
pub const VNODE_PATH_SIZE: usize = 1200;
/// `offsetof(struct vnode_fdinfowithpath, pfi.fi_openflags)`.
pub const VNODE_OPENFLAGS_AT: usize = 0;
/// `… pvip.vip_vi.vi_stat.vst_dev`.
pub const VNODE_DEV_AT: usize = 24;
/// `… vst_mode` (`uint16_t`).
pub const VNODE_MODE_AT: usize = 28;
/// `… vst_ino`.
pub const VNODE_INO_AT: usize = 32;
/// `… vst_size` (`off_t`).
pub const VNODE_SIZE_AT: usize = 112;
/// `… pvip.vip_path` (`MAXPATHLEN` bytes).
pub const VNODE_PATH_AT: usize = 176;
/// `MAXPATHLEN`.
pub const MAXPATHLEN: usize = 1024;

/// `sizeof(struct pipe_fdinfo)`.
pub const PIPE_SIZE: usize = 184;
/// `offsetof(struct pipe_fdinfo, pfi.fi_openflags)`.
pub const PIPE_OPENFLAGS_AT: usize = 0;
/// `… pipeinfo.pipe_stat.vst_size`: the bytes in this end's buffer.
pub const PIPE_QUEUED_AT: usize = 112;
/// `… pipeinfo.pipe_handle`.
pub const PIPE_HANDLE_AT: usize = 160;
/// `… pipeinfo.pipe_peerhandle`.
pub const PIPE_PEER_AT: usize = 168;
/// `… pipeinfo.pipe_status`.
pub const PIPE_STATUS_AT: usize = 176;

/// `sizeof(struct socket_fdinfo)`.
pub const SOCKET_SIZE: usize = 792;
/// `offsetof(struct socket_fdinfo, psi.soi_family)`.
pub const SOCKET_FAMILY_AT: usize = 184;
/// `… psi.soi_rcv.sbi_cc`: bytes received, not yet read.
pub const SOCKET_RCV_CC_AT: usize = 208;
/// `… psi.soi_kind`.
pub const SOCKET_KIND_AT: usize = 256;
/// `… psi.soi_proto.pri_tcp.tcpsi_ini.insi_fport` (network order, low 16 bits).
pub const SOCKET_FPORT_AT: usize = 264;
/// `… insi_lport`.
pub const SOCKET_LPORT_AT: usize = 268;
/// `… insi_vflag`.
pub const SOCKET_VFLAG_AT: usize = 288;
/// `… insi_faddr` (16 bytes; IPv4 in its last four).
pub const SOCKET_FADDR_AT: usize = 296;
/// `… psi.soi_proto.pri_tcp.tcpsi_state`.
pub const SOCKET_TCP_STATE_AT: usize = 344;

/// `CTL_KERN`, `KERN_PROC`, `KERN_PROC_PID`: the sysctl name of one process's `kinfo_proc`.
pub const KERN_PROC: i32 = 14;
/// `KERN_PROC_PID`.
pub const KERN_PROC_PID: i32 = 1;
/// `sizeof(struct kinfo_proc)`.
pub const KINFO_SIZE: usize = 648;
/// `offsetof(struct kinfo_proc, kp_proc.p_un.__p_starttime.tv_sec)`.
pub const KINFO_START_SEC_AT: usize = 0;
/// `… tv_usec` (`int32_t`).
pub const KINFO_START_USEC_AT: usize = 8;
/// `… kp_proc.p_flag`.
pub const KINFO_FLAG_AT: usize = 32;
/// `… kp_proc.p_stat` (`char`).
pub const KINFO_STAT_AT: usize = 36;
/// `… kp_proc.p_pid`.
pub const KINFO_PID_AT: usize = 40;
/// `… kp_proc.p_comm` (`MAXCOMLEN + 1` bytes).
pub const KINFO_COMM_AT: usize = 243;
/// `… kp_eproc.e_pcred.p_ruid`.
pub const KINFO_RUID_AT: usize = 392;
/// `… kp_eproc.e_ppid`.
pub const KINFO_PPID_AT: usize = 560;
/// `… kp_eproc.e_pgid`.
pub const KINFO_PGID_AT: usize = 564;
/// `… kp_eproc.e_tdev`.
pub const KINFO_TDEV_AT: usize = 572;
/// `… kp_eproc.e_tpgid`.
pub const KINFO_TPGID_AT: usize = 576;
/// `… kp_eproc.e_flag`.
pub const KINFO_EFLAG_AT: usize = 612;
/// `P_CONTROLT` in `p_flag`: the process has a controlling terminal.
pub const P_CONTROLT: u32 = 0x2;
/// `EPROC_SLEADER` in `e_flag`: the process leads its session.
pub const EPROC_SLEADER: u32 = 0x2;

/// `sizeof(struct kqueue_fdinfo)`.
pub const KQUEUE_SIZE: usize = 168;
/// `offsetof(struct kqueue_fdinfo, kqueueinfo.kq_state)`.
pub const KQUEUE_STATE_AT: usize = 160;

/// `sizeof(struct kevent_extinfo)` (private; held by the L6 self-check).
pub const KNOTE_SIZE: usize = 104;
/// `kqext_kev.ident`.
pub const KNOTE_IDENT_AT: usize = 0;
/// `kqext_kev.filter` (`int16_t`).
pub const KNOTE_FILTER_AT: usize = 8;

/// Public struct sizes, field offsets, and constants verified against the macOS SDK.
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("PROC_PIDLISTTHREADS", PROC_PIDLISTTHREADS as i64),
    ("PROC_PIDARCHINFO", PROC_PIDARCHINFO as i64),
    ("sizeof(struct proc_archinfo)", ARCHINFO_SIZE as i64),
    (
        "offsetof(struct proc_archinfo, p_cputype)",
        ARCHINFO_CPUTYPE_AT as i64,
    ),
    ("CPU_TYPE_X86_64", CPU_TYPE_X86_64 as i64),
    ("SSTOP", SSTOP as i64),
    ("PROC_FLAG_TRACED", PROC_FLAG_TRACED as i64),
    ("PROC_PIDFDVNODEPATHINFO", PROC_PIDFDVNODEPATHINFO as i64),
    ("PROC_PIDFDSOCKETINFO", PROC_PIDFDSOCKETINFO as i64),
    ("PROC_PIDFDPIPEINFO", PROC_PIDFDPIPEINFO as i64),
    ("PROC_PIDFDKQUEUEINFO", PROC_PIDFDKQUEUEINFO as i64),
    ("PROC_KQUEUE_SLEEP", PROC_KQUEUE_SLEEP as i64),
    ("PROC_FLAG_SLEADER", PROC_FLAG_SLEADER as i64),
    ("PROC_FLAG_CONTROLT", PROC_FLAG_CONTROLT as i64),
    ("SOCKINFO_IN", SOCKINFO_IN as i64),
    ("SOCKINFO_TCP", SOCKINFO_TCP as i64),
    ("INI_IPV4", INI_IPV4 as i64),
    ("INI_IPV6", INI_IPV6 as i64),
    ("S_IFMT", S_IFMT as i64),
    ("S_IFREG", S_IFREG as i64),
    ("S_IFCHR", S_IFCHR as i64),
    ("MAXPATHLEN", MAXPATHLEN as i64),
    ("(uint32_t)NODEV", NODEV as i64),
    ("FREAD", FREAD as i64),
    ("FWRITE", FWRITE as i64),
    (
        "sizeof(struct vnode_fdinfowithpath)",
        VNODE_PATH_SIZE as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pfi.fi_openflags)",
        VNODE_OPENFLAGS_AT as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_dev)",
        VNODE_DEV_AT as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_mode)",
        VNODE_MODE_AT as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_ino)",
        VNODE_INO_AT as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_vi.vi_stat.vst_size)",
        VNODE_SIZE_AT as i64,
    ),
    (
        "offsetof(struct vnode_fdinfowithpath, pvip.vip_path)",
        VNODE_PATH_AT as i64,
    ),
    ("sizeof(struct pipe_fdinfo)", PIPE_SIZE as i64),
    (
        "offsetof(struct pipe_fdinfo, pfi.fi_openflags)",
        PIPE_OPENFLAGS_AT as i64,
    ),
    (
        "offsetof(struct pipe_fdinfo, pipeinfo.pipe_stat.vst_size)",
        PIPE_QUEUED_AT as i64,
    ),
    (
        "offsetof(struct pipe_fdinfo, pipeinfo.pipe_handle)",
        PIPE_HANDLE_AT as i64,
    ),
    (
        "offsetof(struct pipe_fdinfo, pipeinfo.pipe_peerhandle)",
        PIPE_PEER_AT as i64,
    ),
    (
        "offsetof(struct pipe_fdinfo, pipeinfo.pipe_status)",
        PIPE_STATUS_AT as i64,
    ),
    ("sizeof(struct socket_fdinfo)", SOCKET_SIZE as i64),
    (
        "offsetof(struct socket_fdinfo, psi.soi_family)",
        SOCKET_FAMILY_AT as i64,
    ),
    (
        "offsetof(struct socket_fdinfo, psi.soi_rcv.sbi_cc)",
        SOCKET_RCV_CC_AT as i64,
    ),
    (
        "offsetof(struct socket_fdinfo, psi.soi_kind)",
        SOCKET_KIND_AT as i64,
    ),
    (
        "offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_ini.insi_fport)",
        SOCKET_FPORT_AT as i64,
    ),
    (
        "offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_ini.insi_lport)",
        SOCKET_LPORT_AT as i64,
    ),
    (
        "offsetof(struct socket_fdinfo, psi.soi_proto.pri_in.insi_fport)",
        SOCKET_FPORT_AT as i64,
    ),
    (
        "offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_ini.insi_vflag)",
        SOCKET_VFLAG_AT as i64,
    ),
    (
        "offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_ini.insi_faddr)",
        SOCKET_FADDR_AT as i64,
    ),
    (
        "offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_ini.insi_faddr.ina_46.i46a_addr4)",
        SOCKET_FADDR_AT as i64 + 12,
    ),
    (
        "offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_state)",
        SOCKET_TCP_STATE_AT as i64,
    ),
    ("KERN_PROC", KERN_PROC as i64),
    ("KERN_PROC_PID", KERN_PROC_PID as i64),
    ("P_CONTROLT", P_CONTROLT as i64),
    ("EPROC_SLEADER", EPROC_SLEADER as i64),
    ("sizeof(struct kinfo_proc)", KINFO_SIZE as i64),
    (
        "offsetof(struct kinfo_proc, kp_proc.p_un.__p_starttime.tv_sec)",
        KINFO_START_SEC_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_proc.p_un.__p_starttime.tv_usec)",
        KINFO_START_USEC_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_proc.p_flag)",
        KINFO_FLAG_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_proc.p_stat)",
        KINFO_STAT_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_proc.p_pid)",
        KINFO_PID_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_proc.p_comm)",
        KINFO_COMM_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_eproc.e_pcred.p_ruid)",
        KINFO_RUID_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_eproc.e_ppid)",
        KINFO_PPID_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_eproc.e_pgid)",
        KINFO_PGID_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_eproc.e_tdev)",
        KINFO_TDEV_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_eproc.e_tpgid)",
        KINFO_TPGID_AT as i64,
    ),
    (
        "offsetof(struct kinfo_proc, kp_eproc.e_flag)",
        KINFO_EFLAG_AT as i64,
    ),
    ("sizeof(struct kqueue_fdinfo)", KQUEUE_SIZE as i64),
    (
        "offsetof(struct kqueue_fdinfo, kqueueinfo.kq_state)",
        KQUEUE_STATE_AT as i64,
    ),
    ("offsetof(struct kevent64_s, ident)", KNOTE_IDENT_AT as i64),
    (
        "offsetof(struct kevent64_s, filter)",
        KNOTE_FILTER_AT as i64,
    ),
];

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn i32_at(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn i64_at(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_ne_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// A vnode descriptor (`vnode_fdinfowithpath`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VnodeFd {
    /// `fi_openflags`.
    pub openflags: u32,
    /// `vst_mode`.
    pub mode: u16,
    /// `vst_dev`.
    pub dev: u32,
    /// `vst_ino`.
    pub ino: u64,
    /// `vst_size`, never negative.
    pub size: u64,
    /// `vip_path`, up to its NUL.
    pub path: Vec<u8>,
}

impl VnodeFd {
    /// A regular file.
    #[must_use]
    pub const fn regular(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }

    /// A character device.
    #[must_use]
    pub const fn char_device(&self) -> bool {
        self.mode & S_IFMT == S_IFCHR
    }
}

/// Decode a `vnode_fdinfowithpath`; `None` when `b` is shorter than the struct.
#[must_use]
pub fn vnode(b: &[u8]) -> Option<VnodeFd> {
    if b.len() < VNODE_PATH_SIZE {
        return None;
    }
    let raw = b.get(VNODE_PATH_AT..VNODE_PATH_AT + MAXPATHLEN)?;
    let end = raw.iter().position(|c| *c == 0).unwrap_or(raw.len());
    Some(VnodeFd {
        openflags: u32_at(b, VNODE_OPENFLAGS_AT)?,
        mode: u16_at(b, VNODE_MODE_AT)?,
        dev: u32_at(b, VNODE_DEV_AT)?,
        ino: u64_at(b, VNODE_INO_AT)?,
        size: u64::try_from(i64_at(b, VNODE_SIZE_AT)?).unwrap_or(0),
        path: raw[..end].to_vec(),
    })
}

/// A pipe descriptor (`pipe_fdinfo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PipeFd {
    /// `fi_openflags` (`FREAD`, `FWRITE`).
    pub openflags: u32,
    /// The bytes in this end's buffer.
    pub queued: u64,
    /// This end's handle.
    pub handle: u64,
    /// The other end's handle.
    pub peer: u64,
    /// `pipe_status`.
    pub status: u32,
}

impl PipeFd {
    /// Whether this is the write end (opened for writing only).
    #[must_use]
    pub const fn write_end(&self) -> bool {
        self.openflags & FWRITE != 0 && self.openflags & FREAD == 0
    }
}

/// Decode a `pipe_fdinfo`; `None` when `b` is shorter than the struct.
#[must_use]
pub fn pipe(b: &[u8]) -> Option<PipeFd> {
    if b.len() < PIPE_SIZE {
        return None;
    }
    Some(PipeFd {
        openflags: u32_at(b, PIPE_OPENFLAGS_AT)?,
        queued: u64::try_from(i64_at(b, PIPE_QUEUED_AT)?).unwrap_or(0),
        handle: u64_at(b, PIPE_HANDLE_AT)?,
        peer: u64_at(b, PIPE_PEER_AT)?,
        status: u32_at(b, PIPE_STATUS_AT)?,
    })
}

/// A socket descriptor's TCP part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpFd {
    /// `tcpsi_state` (`TSI_S_*`).
    pub state: i32,
    /// Remote peer address when the socket is connected.
    pub remote: Option<SocketAddr>,
    /// The local port.
    pub local_port: u16,
    /// Bytes received, not yet read.
    pub queued_read: u64,
}

/// Decode a `socket_fdinfo`.
#[must_use]
pub fn socket(b: &[u8]) -> Option<Option<TcpFd>> {
    if b.len() < SOCKET_SIZE {
        return None;
    }
    if i32_at(b, SOCKET_KIND_AT)? != SOCKINFO_TCP {
        return Some(None);
    }
    let port = |at| i32_at(b, at).map(|p| u16::from_be(p as u16));
    let fport = port(SOCKET_FPORT_AT)?;
    let vflag = *b.get(SOCKET_VFLAG_AT)?;
    let ip = if vflag & INI_IPV4 != 0 {
        let a: [u8; 4] = b
            .get(SOCKET_FADDR_AT + 12..SOCKET_FADDR_AT + 16)?
            .try_into()
            .ok()?;
        Some(IpAddr::V4(Ipv4Addr::from(a)))
    } else if vflag & INI_IPV6 != 0 {
        let a: [u8; 16] = b
            .get(SOCKET_FADDR_AT..SOCKET_FADDR_AT + 16)?
            .try_into()
            .ok()?;
        Some(IpAddr::V6(Ipv6Addr::from(a)))
    } else {
        None
    };
    let remote = ip
        .filter(|_| fport != 0)
        .map(|ip| SocketAddr::new(ip, fport));
    Some(Some(TcpFd {
        state: i32_at(b, SOCKET_TCP_STATE_AT)?,
        remote,
        local_port: port(SOCKET_LPORT_AT)?,
        queued_read: u64::from(u32_at(b, SOCKET_RCV_CC_AT)?),
    }))
}

/// The fields of a `kinfo_proc` the probe reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kinfo {
    /// `p_pid`.
    pub pid: u32,
    /// `e_ppid`.
    pub ppid: u32,
    /// `e_pgid`.
    pub pgid: u32,
    /// `p_stat` (`SIDL` … `SZOMB`).
    pub stat: u32,
    /// `e_pcred.p_ruid`.
    pub uid: u32,
    /// `p_starttime` in µs since the epoch.
    pub start_us: u64,
    /// The controlling terminal's device, `None` without one.
    pub tty: Option<u32>,
    /// That terminal's foreground group.
    pub tpgid: Option<u32>,
    /// `EPROC_SLEADER`.
    pub session_leader: bool,
    /// `p_comm`.
    pub comm: String,
}

/// Decode a `kinfo_proc`; `None` when `b` is shorter than the struct.
#[must_use]
pub fn kinfo(b: &[u8]) -> Option<Kinfo> {
    if b.len() < KINFO_SIZE {
        return None;
    }
    let comm = b.get(KINFO_COMM_AT..KINFO_COMM_AT + 17)?;
    let end = comm.iter().position(|c| *c == 0).unwrap_or(16);
    let sec = u64::try_from(i64_at(b, KINFO_START_SEC_AT)?).unwrap_or(0);
    let usec = u64::try_from(i32_at(b, KINFO_START_USEC_AT)?).unwrap_or(0);
    let tdev = u32_at(b, KINFO_TDEV_AT)?;
    let has_tty = u32_at(b, KINFO_FLAG_AT)? & P_CONTROLT != 0 && tdev != NODEV;
    let tty = has_tty.then_some(tdev);
    Some(Kinfo {
        pid: u32_at(b, KINFO_PID_AT)?,
        ppid: u32_at(b, KINFO_PPID_AT)?,
        pgid: u32_at(b, KINFO_PGID_AT)?,
        stat: u32::from(*b.get(KINFO_STAT_AT)?),
        uid: u32_at(b, KINFO_RUID_AT)?,
        start_us: sec.saturating_mul(1_000_000).saturating_add(usec),
        tty,
        tpgid: tty.and(u32_at(b, KINFO_TPGID_AT).filter(|g| *g != 0)),
        session_leader: u32_at(b, KINFO_EFLAG_AT)? & EPROC_SLEADER != 0,
        comm: String::from_utf8_lossy(&comm[..end]).into_owned(),
    })
}

/// Decode a `proc_archinfo`'s `p_cputype`; `None` when `b` is shorter than the struct.
#[must_use]
pub fn archinfo_cputype(b: &[u8]) -> Option<i32> {
    if b.len() < ARCHINFO_SIZE {
        return None;
    }
    i32_at(b, ARCHINFO_CPUTYPE_AT)
}

/// Decode a `kqueue_fdinfo`'s `kq_state`; `None` when `b` is short.
#[must_use]
pub fn kqueue_state(b: &[u8]) -> Option<u32> {
    if b.len() < KQUEUE_SIZE {
        return None;
    }
    u32_at(b, KQUEUE_STATE_AT)
}

/// Decode the first `n` `kevent_extinfo`s of `b`: `(ident, filter)` each.
#[must_use]
pub fn knotes(b: &[u8], n: usize) -> Vec<(u64, i16)> {
    (0..n)
        .map_while(|i| {
            let at = i.checked_mul(KNOTE_SIZE)?;
            if at + KNOTE_SIZE > b.len() {
                return None;
            }
            let ident = u64_at(b, at + KNOTE_IDENT_AT)?;
            let filter = u16_at(b, at + KNOTE_FILTER_AT)? as i16;
            Some((ident, filter))
        })
        .collect()
}
