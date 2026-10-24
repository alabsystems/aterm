// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS implementation of the job probe.

use std::ffi::{c_int, c_void};
use std::path::PathBuf;

use super::darwin::{self, PIPE_WANTR};
use super::{
    Counters, FdKind, FdReading, Knote, KnoteFilter, Lane, Lanes, MAX_FD_DETAILS, MAX_FDS,
    MAX_KNOTES, MAX_THREADS, ProcFacts, RunState, StartFacts, TcpState, ThreadReading,
};
use crate::procs::imp::{rusage, short_info, timebase};
use crate::procs::ticks_to_ns;

const TASKALL_SIZE: usize = std::mem::size_of::<libc::proc_taskallinfo>();
const BSD_SIZE: usize = std::mem::size_of::<libc::proc_bsdinfo>();
const THREAD_SIZE: usize = std::mem::size_of::<libc::proc_threadinfo>();
const FDINFO_SIZE: usize = std::mem::size_of::<libc::proc_fdinfo>();
const _: () = assert!(TASKALL_SIZE == 232 && BSD_SIZE == 136);
const _: () = assert!(THREAD_SIZE == 112 && FDINFO_SIZE == 8);

fn task_all(pid: c_int) -> Option<libc::proc_taskallinfo> {
    // SAFETY: plain integers and bytes; all-zero is valid.
    let mut info: libc::proc_taskallinfo = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is `TASKALL_SIZE` writable bytes of the flavor's layout.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKALLINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            TASKALL_SIZE as c_int,
        )
    };
    (usize::try_from(n).ok() == Some(TASKALL_SIZE)).then_some(info)
}

fn bsd_info(pid: c_int) -> Option<libc::proc_bsdinfo> {
    // SAFETY: plain integers and bytes; all-zero is valid.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is `BSD_SIZE` writable bytes of the flavor's layout.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            BSD_SIZE as c_int,
        )
    };
    (usize::try_from(n).ok() == Some(BSD_SIZE)).then_some(info)
}

fn comm(bytes: &[libc::c_char]) -> String {
    let raw: Vec<u8> = bytes
        .iter()
        .map(|c| *c as u8)
        .take_while(|c| *c != 0)
        .collect();
    String::from_utf8_lossy(&raw).into_owned()
}

/// Facts from a `proc_bsdinfo` (the whole of a zombie's, which has no task).
fn from_bsd(pid: u32, b: &libc::proc_bsdinfo) -> ProcFacts {
    let tty = (b.e_tdev != darwin::NODEV && b.pbi_flags & darwin::PROC_FLAG_CONTROLT != 0)
        .then_some(b.e_tdev);
    ProcFacts {
        pid,
        ppid: b.pbi_ppid,
        pgid: b.pbi_pgid,
        sid: None,
        start: Some(
            b.pbi_start_tvsec
                .saturating_mul(1_000_000)
                .saturating_add(b.pbi_start_tvusec),
        ),
        uid: Some(b.pbi_uid),
        name: comm(&b.pbi_comm),
        state: RunState::from_bsd(b.pbi_status),
        tpgid: tty.and(Some(b.e_tpgid).filter(|g| *g != 0)),
        tty,
        ..ProcFacts::default()
    }
}

/// `sysctl(KERN_PROC_PID)`.
pub(crate) fn kinfo(pid: c_int) -> Option<darwin::Kinfo> {
    let mut buf = vec![0u8; darwin::KINFO_SIZE];
    let mut len: libc::size_t = buf.len();
    let mut mib = [
        libc::CTL_KERN,
        darwin::KERN_PROC,
        darwin::KERN_PROC_PID,
        pid,
    ];
    // SAFETY: `mib` names four integers; `buf` is `len` writable bytes and the kernel writes at
    // most `len`, reporting how many in `len`.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            buf.as_mut_ptr().cast::<c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != darwin::KINFO_SIZE {
        return None;
    }
    darwin::kinfo(&buf).filter(|k| k.pid == pid.unsigned_abs())
}

fn getsid(pid: c_int) -> Option<u32> {
    // SAFETY: plain integer argument; errors are a negative return.
    let sid = unsafe { libc::getsid(pid) };
    u32::try_from(sid).ok().filter(|s| *s > 0)
}

fn counters(pid: c_int, task: Option<&libc::proc_taskinfo>) -> Option<Counters> {
    let u = rusage(pid)?;
    let (numer, denom) = timebase();
    let ns = |ticks: u64| ticks_to_ns(ticks, numer, denom);
    let nonneg = |v: i32| u64::try_from(v).ok();
    Some(Counters {
        cpu_ns: ns(u.ri_user_time.saturating_add(u.ri_system_time)),
        child_cpu_ns: Some(ns(u
            .ri_child_user_time
            .saturating_add(u.ri_child_system_time))),
        syscalls: task.and_then(|t| nonneg(t.pti_syscalls_unix)),
        wakeups: Some(u.ri_interrupt_wkups),
        ctx_switches: task.and_then(|t| nonneg(t.pti_csw)),
        disk_read: Some(u.ri_diskio_bytesread),
        disk_written: Some(u.ri_diskio_byteswritten),
        logical_writes: Some(u.ri_logical_writes),
        runnable_ns: Some(ns(u.ri_runnable_time)),
        pageins: Some(u.ri_pageins),
    })
}

/// `pid`'s thread handles, at most `room` of them (`PROC_PIDLISTTHREADS`); `None` when refused.
fn thread_handles(pid: c_int, room: usize) -> Option<Vec<u64>> {
    let mut handles = vec![0u64; room];
    let bytes = c_int::try_from(handles.len() * 8).ok()?;
    // SAFETY: `handles` is `bytes` writable bytes; the call returns how many bytes it wrote.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            darwin::PROC_PIDLISTTHREADS,
            0,
            handles.as_mut_ptr().cast::<c_void>(),
            bytes,
        )
    };
    let n = usize::try_from(n).ok()? / 8;
    handles.truncate(n.min(room));
    Some(handles)
}

/// Each listed thread read, and whether the list (of up to `MAX_THREADS + 1`) was cut.
fn threads(pid: c_int, listed: &[u64]) -> (Vec<ThreadReading>, bool) {
    let truncated = listed.len() > MAX_THREADS;
    let handles = &listed[..listed.len().min(MAX_THREADS)];
    let mut out = Vec::with_capacity(handles.len());
    for &h in handles {
        // SAFETY: plain integers and bytes; all-zero is valid.
        let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is `THREAD_SIZE` writable bytes of the flavor's
        // layout; the argument is the thread's handle.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTHREADINFO,
                h,
                (&raw mut info).cast::<c_void>(),
                THREAD_SIZE as c_int,
            )
        };
        if usize::try_from(got).ok() != Some(THREAD_SIZE) {
            continue;
        }
        out.push(ThreadReading {
            id: h,
            state: RunState::from_mach_thread(info.pth_run_state),
            cpu_ns: Some(info.pth_user_time.saturating_add(info.pth_system_time)),
            wchan: None,
        });
    }
    (out, truncated)
}

/// Whether this Mac is arm64 (`hw.optional.arm64`, which reads 1 under Rosetta too), read once.
fn host_arm64() -> bool {
    static ARM64: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARM64.get_or_init(|| crate::macos::sysctl_int(c"hw.optional.arm64") == Some(1))
}

/// Whether `pid` is an x86_64 process on an arm64 Mac (`PROC_PIDARCHINFO`); `None` when its arch cannot be read there.
fn translated(pid: c_int) -> Option<bool> {
    if !host_arm64() {
        return Some(false);
    }
    let mut buf = [0u8; darwin::ARCHINFO_SIZE];
    // SAFETY: `buf` is `ARCHINFO_SIZE` writable bytes of the flavor's layout.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            darwin::PROC_PIDARCHINFO,
            0,
            buf.as_mut_ptr().cast::<c_void>(),
            darwin::ARCHINFO_SIZE as c_int,
        )
    };
    if usize::try_from(n).ok() != Some(darwin::ARCHINFO_SIZE) {
        return None;
    }
    darwin::archinfo_cputype(&buf).map(|t| t == darwin::CPU_TYPE_X86_64)
}

/// The [`StartFacts`] of `pid` from its listed thread `handles` and its BSD record `bsd` at `now_us`, its arch read only for a lone `0`, `local` left unread for the caller that holds the executable's path. `None` for an empty list and, for a lone `0`, a BSD record or an arch that cannot be read.
fn start_facts(
    pid: c_int,
    handles: &[u64],
    bsd: Option<&libc::proc_bsdinfo>,
    now_us: u64,
) -> Option<StartFacts> {
    if handles.is_empty() {
        return None;
    }
    if handles != [0] {
        return Some(StartFacts::default());
    }
    let b = bsd?;
    let start_us = b
        .pbi_start_tvsec
        .saturating_mul(1_000_000)
        .saturating_add(b.pbi_start_tvusec);
    Some(StartFacts {
        lone_zero: true,
        stopped: b.pbi_status == darwin::SSTOP,
        traced: b.pbi_flags & darwin::PROC_FLAG_TRACED != 0,
        translated: translated(pid)?,
        local: None,
        age_ms: now_us.checked_sub(start_us).map(|us| us / 1000),
    })
}

/// See [`super::read_start_in`]: the list asks for two slots, so one call tells a lone `0` from a started process, which reads nothing more.
pub(super) fn read_start(
    pid: u32,
    now_us: u64,
    mounts: &std::cell::OnceCell<crate::volumes::Table>,
) -> Option<StartFacts> {
    let p = c_int::try_from(pid).ok().filter(|p| *p > 0)?;
    let listed = thread_handles(p, 2)?;
    let bsd = if listed == [0] { bsd_info(p) } else { None };
    let mut facts = start_facts(p, &listed, bsd.as_ref(), now_us)?;
    if facts.lone_zero {
        facts.local = crate::process_path(pid)
            .and_then(|exe| super::local_at(&exe, mounts.get_or_init(crate::volumes::table)));
    }
    Some(facts)
}

/// The process's state from its BSD status, refined by its threads.
fn refine(state: RunState, threads: Option<&[ThreadReading]>) -> RunState {
    let Some(ts) = threads.filter(|t| !t.is_empty()) else {
        return state;
    };
    if !matches!(state, RunState::Running | RunState::Sleeping) {
        return state;
    }
    if ts.iter().any(|t| t.state == RunState::Running) {
        RunState::Running
    } else if ts.iter().any(|t| t.state == RunState::Uninterruptible) {
        RunState::Uninterruptible
    } else if ts.iter().all(|t| t.state == RunState::Stopped) {
        RunState::Stopped
    } else {
        RunState::Sleeping
    }
}

fn fd_list(pid: c_int, nfiles: u32) -> Option<(Vec<libc::proc_fdinfo>, bool)> {
    let want = usize::try_from(nfiles)
        .unwrap_or(MAX_FDS)
        .clamp(16, MAX_FDS)
        + 1;
    let mut list = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0
        };
        want
    ];
    let bytes = c_int::try_from(want * FDINFO_SIZE).ok()?;
    // SAFETY: `list` is `bytes` writable bytes; the call returns how many bytes it wrote.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            list.as_mut_ptr().cast::<c_void>(),
            bytes,
        )
    };
    let n = usize::try_from(n).ok()? / FDINFO_SIZE;
    list.truncate(n.min(want));
    let truncated = list.len() > MAX_FDS || list.len() == want;
    list.truncate(MAX_FDS);
    Some((list, truncated))
}

/// `proc_pidfdinfo(pid, fd, flavor)` into `buf`.
fn fd_info(pid: c_int, fd: i32, flavor: i32, buf: &mut [u8]) -> Option<usize> {
    let size = c_int::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is `size` writable bytes.
    let n = unsafe { libc::proc_pidfdinfo(pid, fd, flavor, buf.as_mut_ptr().cast(), size) };
    usize::try_from(n).ok().filter(|n| *n > 0)
}

/// What vnode descriptor `fd` of `pid`, a process started at `start_us`, holds.
fn vnode_kind(pid: c_int, fd: i32, start_us: Option<u64>) -> FdKind {
    let mut buf = vec![0u8; darwin::VNODE_PATH_SIZE];
    match fd_info(pid, fd, darwin::PROC_PIDFDVNODEPATHINFO, &mut buf).and(darwin::vnode(&buf)) {
        Some(v) if v.char_device() => FdKind::Device { tty: v.terminal() },
        Some(v) => FdKind::File {
            regular: v.regular(),
            size: v.size,
            dev: u64::from(v.dev),
            ino: v.ino,
            lf_tail: None,
            fresh_dylib: v.regular() && super::fresh_dylib(&v.path, v.mtime_s, start_us),
        },
        None => FdKind::Other,
    }
}

fn pipe_kind(pid: c_int, fd: i32) -> FdKind {
    let mut buf = [0u8; darwin::PIPE_SIZE];
    match fd_info(pid, fd, darwin::PROC_PIDFDPIPEINFO, &mut buf).and(darwin::pipe(&buf)) {
        Some(p) => FdKind::Pipe {
            handle: p.handle,
            peer: p.peer,
            status: Some(p.status),
            queued: Some(p.queued),
            write_end: Some(p.write_end()),
        },
        None => FdKind::Other,
    }
}

fn socket_kind(pid: c_int, fd: i32) -> FdKind {
    let mut buf = vec![0u8; darwin::SOCKET_SIZE];
    match fd_info(pid, fd, darwin::PROC_PIDFDSOCKETINFO, &mut buf).and(darwin::socket(&buf)) {
        Some(Some(t)) => FdKind::Tcp {
            state: TcpState::from_darwin(t.state),
            remote: t.remote,
            local_port: t.local_port,
            queued_read: Some(t.queued_read),
        },
        Some(None) => FdKind::Socket,
        None => FdKind::Other,
    }
}

fn kqueue_kind(pid: c_int, fd: i32, lanes: Lanes) -> FdKind {
    let mut buf = [0u8; darwin::KQUEUE_SIZE];
    let sleeping = fd_info(pid, fd, darwin::PROC_PIDFDKQUEUEINFO, &mut buf)
        .and(darwin::kqueue_state(&buf))
        .map(|s| s & darwin::PROC_KQUEUE_SLEEP != 0);
    let knotes = lanes
        .kqueue_detail
        .is_on()
        .then(|| knotes_of(pid, fd))
        .flatten();
    FdKind::EventQueue { sleeping, knotes }
}

fn knotes_of(pid: c_int, fd: i32) -> Option<Vec<Knote>> {
    let mut buf = vec![0u8; darwin::KNOTE_SIZE * MAX_KNOTES];
    let size = c_int::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is `size` writable bytes; this flavor returns how many
    // `kevent_extinfo`s it wrote (a negative return is an error).
    let n = unsafe {
        libc::proc_pidfdinfo(
            pid,
            fd,
            darwin::PROC_PIDFDKQUEUE_EXTINFO,
            buf.as_mut_ptr().cast(),
            size,
        )
    };
    let n = usize::try_from(n).ok()?.min(MAX_KNOTES);
    Some(
        darwin::knotes(&buf, n)
            .into_iter()
            .map(|(ident, filter)| Knote {
                ident,
                filter: KnoteFilter::from_darwin(filter),
            })
            .collect(),
    )
}

fn fds(
    pid: c_int,
    nfiles: u32,
    lanes: Lanes,
    start_us: Option<u64>,
) -> Option<(Vec<FdReading>, bool)> {
    let (list, mut truncated) = fd_list(pid, nfiles)?;
    let mut out = Vec::with_capacity(list.len());
    let mut details = 0usize;
    for f in list {
        let detail = |details: &mut usize| {
            *details += 1;
            *details <= MAX_FD_DETAILS
        };
        let kind = match i32::try_from(f.proc_fdtype).unwrap_or(-1) {
            libc::PROX_FDTYPE_VNODE if detail(&mut details) => vnode_kind(pid, f.proc_fd, start_us),
            libc::PROX_FDTYPE_PIPE if detail(&mut details) => pipe_kind(pid, f.proc_fd),
            libc::PROX_FDTYPE_SOCKET if detail(&mut details) => socket_kind(pid, f.proc_fd),
            libc::PROX_FDTYPE_KQUEUE if detail(&mut details) => kqueue_kind(pid, f.proc_fd, lanes),
            libc::PROX_FDTYPE_VNODE
            | libc::PROX_FDTYPE_PIPE
            | libc::PROX_FDTYPE_SOCKET
            | libc::PROX_FDTYPE_KQUEUE => {
                truncated = true;
                FdKind::Other
            }
            _ => FdKind::Other,
        };
        out.push(FdReading {
            fd: f.proc_fd,
            kind,
        });
    }
    Some((out, truncated))
}

/// Read one process (see the parent module), with the [`StartFacts`] its thread list gives at `now_us`.
pub(super) fn read(
    pid: u32,
    want_fds: bool,
    lanes: Lanes,
    now_us: u64,
) -> Option<(ProcFacts, Option<StartFacts>)> {
    let p = c_int::try_from(pid).ok().filter(|p| *p > 0)?;
    let all = task_all(p);
    let bsd = match &all {
        Some(a) => Some(a.pbsd),
        None => bsd_info(p),
    };
    let mut facts = match &bsd {
        Some(b) => from_bsd(pid, b),
        None => match short_info(p) {
            Some(s) => {
                let end = s.comm.iter().position(|b| *b == 0).unwrap_or(16);
                ProcFacts {
                    pid,
                    ppid: s.ppid,
                    pgid: s.pgid,
                    uid: Some(s.uid),
                    name: String::from_utf8_lossy(&s.comm[..end]).into_owned(),
                    state: RunState::from_bsd(s.status),
                    ..ProcFacts::default()
                }
            }
            None => {
                let k = kinfo(p)?;
                ProcFacts {
                    pid,
                    ppid: k.ppid,
                    pgid: k.pgid,
                    sid: k.session_leader.then_some(pid),
                    start: Some(k.start_us),
                    uid: Some(k.uid),
                    name: k.comm,
                    state: RunState::from_bsd(k.stat),
                    tty: k.tty,
                    tpgid: k.tpgid,
                    ..ProcFacts::default()
                }
            }
        },
    };
    facts.sid = getsid(p).or(facts.sid);
    if facts.state == RunState::Zombie {
        return Some((facts, None));
    }
    facts.counters = counters(p, all.as_ref().map(|a| &a.ptinfo));
    let listed = thread_handles(p, MAX_THREADS + 1);
    let start = listed
        .as_deref()
        .and_then(|l| start_facts(p, l, bsd.as_ref(), now_us));
    if let Some(list) = &listed {
        let (ts, cut) = threads(p, list);
        facts.state = refine(facts.state, Some(&ts));
        facts.threads = Some(ts);
        facts.threads_truncated = cut;
    }
    if want_fds {
        let nfiles = all.as_ref().map_or(64, |a| a.pbsd.pbi_nfiles);
        if let Some((list, cut)) = fds(p, nfiles, lanes, facts.start) {
            facts.fds = Some(list);
            facts.fds_truncated = cut;
        }
    }
    Some((facts, start))
}

/// Device (`vst_dev`) and inode of every vnode `pid` holds, plus [`MAX_FDS`] truncation.
pub(super) fn vnode_ids(pid: u32) -> Option<(Vec<(u32, u64)>, bool)> {
    let p = c_int::try_from(pid).ok().filter(|p| *p > 0)?;
    let (list, cut) = fd_list(p, MAX_FDS as u32).filter(|(list, _)| !list.is_empty())?;
    let mut buf = vec![0u8; darwin::VNODE_PATH_SIZE];
    let ids = list
        .iter()
        .filter(|f| i32::try_from(f.proc_fdtype).ok() == Some(libc::PROX_FDTYPE_VNODE))
        .filter_map(|f| {
            fd_info(p, f.proc_fd, darwin::PROC_PIDFDVNODEPATHINFO, &mut buf)?;
            darwin::vnode(&buf).map(|v| (v.dev, v.ino))
        })
        .collect();
    Some((ids, cut))
}

/// The kernel's path for `pid`'s descriptor `fd`, from its name cache.
pub(super) fn fd_path(pid: u32, fd: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let p = c_int::try_from(pid).ok().filter(|p| *p > 0)?;
    let mut buf = vec![0u8; darwin::VNODE_PATH_SIZE];
    fd_info(p, fd, darwin::PROC_PIDFDVNODEPATHINFO, &mut buf)?;
    let v = darwin::vnode(&buf)?;
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&v.path));
    path.is_absolute().then_some(path)
}

/// Close `fds`, ignoring errors (each is this process's own, opened here).
fn close_all(fds: &[c_int]) {
    for fd in fds {
        // SAFETY: `fd` is a descriptor this module opened and owns.
        unsafe { libc::close(*fd) };
    }
}

fn own_pipe() -> Option<[c_int; 2]> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: `fds` is two writable descriptors' room.
    (unsafe { libc::pipe(fds.as_mut_ptr()) } == 0).then_some(fds)
}

/// L6: this process's own kqueue, watching one pipe for reading, lists that
/// one knote through the private flavor, with the right descriptor and
/// filter.
fn check_kqueue_detail() -> Lane {
    let Some([r, w]) = own_pipe() else {
        return Lane::Off("no-pipe");
    };
    // SAFETY: no arguments; a negative return is an error.
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        close_all(&[r, w]);
        return Lane::Off("no-kqueue");
    }
    let change = libc::kevent {
        ident: r as libc::uintptr_t,
        filter: libc::EVFILT_READ,
        flags: libc::EV_ADD,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one change, no events asked back, no timeout needed for a registration.
    let rc = unsafe {
        libc::kevent(
            kq,
            &raw const change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    let pid = c_int::try_from(std::process::id()).unwrap_or(0);
    let verdict = if rc < 0 {
        Lane::Off("no-registration")
    } else {
        match knotes_of(pid, kq) {
            Some(ks)
                if ks.len() == 1
                    && ks[0].filter == KnoteFilter::Read
                    && ks[0].ident == r as u64 =>
            {
                Lane::On
            }
            Some(_) => Lane::Off("unexpected-knotes"),
            None => Lane::Off("refused"),
        }
    };
    close_all(&[kq, r, w]);
    verdict
}

/// How long the L7 self-check waits for its reader to block before it calls the lane off.
const L7_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// L7: a thread of this process blocked reading its own pipe shows `PIPE_WANTR` on that end.
fn check_pipe_flags() -> Lane {
    let Some([r, w]) = own_pipe() else {
        return Lane::Off("no-pipe");
    };
    let reader = std::thread::Builder::new()
        .name("aterm-sysprobe-l7".to_owned())
        .spawn(move || {
            let mut byte = 0u8;
            // SAFETY: one writable byte; `r` stays open until this thread is joined.
            unsafe { libc::read(r, (&raw mut byte).cast(), 1) }
        });
    let Ok(reader) = reader else {
        close_all(&[r, w]);
        return Lane::Off("no-thread");
    };
    let pid = c_int::try_from(std::process::id()).unwrap_or(0);
    let mut seen = None;
    let deadline = std::time::Instant::now() + L7_HANG;
    while std::time::Instant::now() < deadline {
        let mut buf = [0u8; darwin::PIPE_SIZE];
        if let Some(p) =
            fd_info(pid, r, darwin::PROC_PIDFDPIPEINFO, &mut buf).and(darwin::pipe(&buf))
        {
            seen = Some(p.status);
            if p.status & PIPE_WANTR != 0 {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let byte = 1u8;
    // SAFETY: one readable byte into our own pipe's write end.
    unsafe { libc::write(w, (&raw const byte).cast(), 1) };
    let _ = reader.join();
    close_all(&[r, w]);
    match seen {
        Some(s) if s & PIPE_WANTR != 0 => Lane::On,
        Some(_) => Lane::Off("no-reader-flag"),
        None => Lane::Off("refused"),
    }
}

pub(super) fn check_lanes() -> Lanes {
    Lanes {
        kqueue_detail: check_kqueue_detail(),
        pipe_flags: check_pipe_flags(),
    }
}
