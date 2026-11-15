// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CHILD HALF OF A LINUX SPAWN: what runs between the clone and the exec ([`super::start`]).
//! From Linux 5.16 the child shares this process's memory and starts on the spawning thread's
//! registers, its thread pointer among them, while every thread of this process runs on, the
//! spawning one too (`CLONE_VM` without `CLONE_VFORK`). So nothing here allocates, takes a lock,
//! panics, writes a thread-local or calls a libc wrapper: each step is one system call made by
//! its own instruction ([`sys`]) over what the parent prepared, and the only memory written is
//! the child's own stack (`errno` is never written: a call answers its error). Every signal is
//! blocked from before the clone until the mask step, by which time no handler of this process's
//! is left to run here. Below 5.16, and under a translator, the same steps run in a copy of the
//! process, where they hold as well.

#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]

use core::ffi::{CStr, c_char, c_int, c_long, c_uint, c_void};
use std::os::fd::RawFd;

/// What a child gets in one of its first five slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Slot {
    /// Untouched: a pass-through spawn's child holds the caller's table as it stands.
    AsIs,
    /// This process's descriptor at that number, open across the exec; `/dev/null` where it is
    /// closed (read-only at 0, write-only above).
    Inherit,
    /// The descriptor at this number, which sits above every slot, copied over the slot.
    Dup(RawFd),
}

/// The exec the child makes: each candidate in turn ([`super::super::search::Search`]), the
/// system shell over one the kernel will not run when `scripts` holds that candidate's argv.
pub(super) struct Walk<'a> {
    pub(super) candidates: &'a [*const c_char],
    pub(super) scripts: Option<&'a [*const *const c_char]>,
    pub(super) argv: *const *const c_char,
    pub(super) envp: *const *const c_char,
}

/// One resource limit the child sets on itself: `prlimit64`'s resource and its soft and hard
/// values, decided in the parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::spawnfd) struct Limit {
    pub(in crate::spawnfd) resource: c_uint,
    pub(in crate::spawnfd) value: [u64; 2],
}

/// Everything the child reads, which the parent keeps alive until the child exec'd or exited.
pub(super) struct Args<'a> {
    pub(super) walk: Walk<'a>,
    pub(super) cwd: Option<&'a CStr>,
    pub(super) slots: &'a [Slot],
    /// `/dev/null` read-only and write-only, for an inherited slot this process has closed.
    pub(super) null: [RawFd; 2],
    /// Descriptors the child keeps at their own numbers, open across the exec.
    pub(super) handed: &'a [RawFd],
    /// Mark every descriptor from 3 close-on-exec first, with this bound for the last resort.
    pub(super) strip: Option<RawFd>,
    /// Every signal below this goes back to its default action, whatever it was ([`reset`]).
    pub(super) reset_below: c_int,
    /// The terminal slave the child leads a session on ([`terminal`]).
    pub(super) terminal: Option<RawFd>,
    pub(super) limits: &'a [Limit],
    /// A descriptor the child holds at a number of its own: from, at.
    pub(super) pass: Option<(RawFd, RawFd)>,
    /// The mask the child execs with.
    pub(super) mask: u64,
    pub(super) pgroup: Option<i32>,
    pub(super) setsid: bool,
    /// The pid this child must still be the child of once its parent-death signal is set.
    pub(super) tie: Option<i32>,
    /// The write end the child reports the step that failed on ([`Step`], then its errno).
    pub(super) report: RawFd,
    /// TEST SEAM: a wait before the first step, in milliseconds.
    #[cfg(any(test, debug_assertions))]
    pub(super) stall_ms: u32,
    #[cfg(test)]
    pub(super) seam: Seam,
}

/// The step a child stopped at, the first word of its report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(in crate::spawnfd) enum Step {
    /// A slot's placement.
    Slots = 1,
    /// A handed descriptor's close-on-exec flag.
    Handed = 2,
    /// `setsid`, the slave made the controlling terminal, then put on 0, 1 and 2 (`login_tty`).
    Terminal = 3,
    /// `fstat` and `TCGETS` on the terminal at 0, which died after it was taken.
    Live = 4,
    Limits = 5,
    /// The working directory's `chdir`.
    Folder = 6,
    Group = 7,
    Session = 8,
    /// The parent-death signal.
    Tie = 9,
    Pass = 10,
    /// The exec (each candidate's, the last one's errno).
    Exec = 11,
}

impl Step {
    /// The step a report's first word names.
    pub(super) const fn from_word(word: u32) -> Option<Self> {
        Some(match word {
            1 => Self::Slots,
            2 => Self::Handed,
            3 => Self::Terminal,
            4 => Self::Live,
            5 => Self::Limits,
            6 => Self::Folder,
            7 => Self::Group,
            8 => Self::Session,
            9 => Self::Tie,
            10 => Self::Pass,
            11 => Self::Exec,
            _ => return None,
        })
    }
}

/// TEST SEAM: a fault the child makes on purpose, so a test can show what guards against it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::spawnfd) enum Seam {
    None,
    /// Raise this signal at the child before its first step.
    Raise(c_int),
    /// [`Self::Raise`], and leave this process's handlers in place.
    RaiseKeepingHandlers(c_int),
    /// Allocate before the first step.
    Allocate,
    /// Wait, with every signal blocked, before the first step, until a `SIGKILL`.
    Stall,
    /// Close descriptor 0 once the terminal is placed, so the terminal is dead at its check.
    DeadAfterTerminal,
}

const F_GETFD: c_long = 1;
const F_SETFD: c_long = 2;
const FD_CLOEXEC: c_long = 1;
const SIG_SETMASK: c_long = 2;
const SIGKILL: c_int = 9;
const SIGSTOP: c_int = 19;
const SIGPIPE: c_int = 13;
const SIGTERM: c_long = 15;
const SIG_IGN: u64 = 1;
const PR_SET_PDEATHSIG: c_long = 1;
const TCGETS: c_long = 0x5401;
const TIOCSCTTY: c_long = 0x540E;
const ENOENT: c_int = 2;
const ENOEXEC: c_int = 8;
const EACCES: c_int = 13;
const ENODEV: c_int = 19;
const ENOTDIR: c_int = 20;
const ESRCH: c_int = 3;
const ETIMEDOUT: c_int = 110;
const ESTALE: c_int = 116;
const BSHELL: &CStr = c"/bin/sh";

/// The child's body over `arg`, an [`Args`]: an exec, or the step that stopped it and its errno
/// written to the report pipe (eight bytes, big-endian) and `exit_group(127)`. It returns to no
/// one.
///
/// # Safety
/// Only as a clone's body, with `arg` the `Args` its parent keeps alive until the child exec'd
/// or exited, and every signal blocked across the clone, so the child starts with them all
/// blocked.
pub(super) unsafe extern "C" fn entry(arg: *mut c_void) -> c_int {
    // SAFETY: the function's contract.
    let args = unsafe { &*arg.cast::<Args<'_>>() };
    // SAFETY: in the child, before its exec, every signal still blocked (the contract).
    let (step, errno) = unsafe { steps(args) };
    let [a, b, c, d] = (step as u32).to_be_bytes();
    let [e, f, g, h] = errno.to_be_bytes();
    let word = [a, b, c, d, e, f, g, h];
    // SAFETY: eight live bytes to the report pipe's write end (one write, under `PIPE_BUF`, so
    // the parent reads them whole), then the end of this child alone.
    unsafe {
        let _ = sys::write(args.report, word.as_ptr().cast(), word.len());
        sys::exit_group(127)
    }
}

/// The steps, in the order of `std`'s child and of a forked tab shell's; the one that failed
/// and its errno (the exec's, last).
///
/// # Safety
/// In the child, before its exec, every signal blocked.
unsafe fn steps(args: &Args<'_>) -> (Step, c_int) {
    #[cfg(any(test, debug_assertions))]
    if args.stall_ms > 0 {
        // SAFETY: a wait over a timespec on this stack.
        unsafe { sys::nanosleep_ms(args.stall_ms) };
    }
    #[cfg(test)]
    let keep_handlers = seam(args.seam);
    #[cfg(not(test))]
    let keep_handlers = false;
    // SAFETY (whole body): the function's contract; every pointer is the parent's, live.
    unsafe {
        if !keep_handlers {
            reset(args.reset_below);
        }
        if let Some(bound) = args.strip {
            strip_from_three(bound);
        }
        if let Some(slave) = args.terminal {
            if let Err(errno) = terminal(slave) {
                return (Step::Terminal, errno);
            }
            #[cfg(test)]
            if args.seam == Seam::DeadAfterTerminal {
                let _ = sys::close(0);
            }
            if let Err(errno) = live() {
                return (Step::Live, errno);
            }
        }
        for (target, slot) in (0..).zip(args.slots) {
            let placed = match *slot {
                Slot::AsIs => Ok(0),
                Slot::Dup(source) => sys::dup3(source, target),
                Slot::Inherit if sys::fcntl(target, F_GETFD, 0).is_err() => {
                    let [read, write] = args.null;
                    sys::dup3(if target == 0 { read } else { write }, target)
                }
                Slot::Inherit => sys::fcntl(target, F_SETFD, 0),
            };
            if let Err(errno) = placed {
                return (Step::Slots, errno);
            }
        }
        for &fd in args.handed {
            if let Err(errno) = keep_open(fd) {
                return (Step::Handed, errno);
            }
        }
        for limit in args.limits {
            if let Err(errno) = sys::prlimit64(limit.resource, &raw const limit.value) {
                return (Step::Limits, errno);
            }
        }
        if let Some(dir) = args.cwd
            && let Err(errno) = sys::chdir(dir.as_ptr())
        {
            return (Step::Folder, errno);
        }
        if let Some(group) = args.pgroup
            && let Err(errno) = sys::setpgid(0, group)
        {
            return (Step::Group, errno);
        }
        if args.setsid
            && let Err(errno) = sys::setsid()
        {
            return (Step::Session, errno);
        }
        if let Some(parent) = args.tie {
            let tied = bind_parent_death(parent);
            if tied != 0 {
                return (Step::Tie, tied);
            }
        }
        if let Some((from, at)) = args.pass {
            let placed = if from == at {
                keep_open(at)
            } else {
                sys::dup3(from, at)
            };
            if let Err(errno) = placed {
                return (Step::Pass, errno);
            }
        }
        // An empty or kept set this thread had: it cannot fail.
        let _ = sys::rt_sigprocmask(SIG_SETMASK, &raw const args.mask, std::ptr::null_mut());
        (Step::Exec, walk(&args.walk))
    }
}

/// Make the seam's fault; whether the handlers stay.
#[cfg(test)]
fn seam(seam: Seam) -> bool {
    match seam {
        Seam::None | Seam::DeadAfterTerminal => false,
        Seam::Raise(signal) | Seam::RaiseKeepingHandlers(signal) => {
            // SAFETY: `getpid`, `gettid` and `tgkill` take and return integers.
            unsafe { sys::raise(signal) };
            matches!(seam, Seam::RaiseKeepingHandlers(_))
        }
        Seam::Allocate => {
            std::hint::black_box(Box::new(0_u8));
            false
        }
        Seam::Stall => loop {
            // SAFETY: no descriptors and no timeout: a wait only a `SIGKILL` ends.
            let _ = unsafe { sys::ppoll_forever() };
        },
    }
}

/// `fd` open across the exec: its close-on-exec flag cleared.
///
/// # Safety
/// In the child, before its exec.
unsafe fn keep_open(fd: RawFd) -> Result<c_long, c_int> {
    // SAFETY: integer arguments only.
    unsafe {
        let flags = sys::fcntl(fd, F_GETFD, 0)?;
        sys::fcntl(fd, F_SETFD, flags & !FD_CLOEXEC)
    }
}

/// `login_tty(slave)`: a session of the child's own, `slave` its controlling terminal, then
/// `slave` on 0, 1 and 2 (a number it already holds keeps it, open across the exec) and closed at
/// its own number above them.
///
/// # Safety
/// In the child, before its exec.
unsafe fn terminal(slave: RawFd) -> Result<(), c_int> {
    // SAFETY: integer arguments only.
    unsafe {
        sys::setsid()?;
        sys::ioctl(slave, TIOCSCTTY, std::ptr::null_mut())?;
        for target in 0..3 {
            if slave == target {
                keep_open(target)?;
            } else {
                sys::dup3(slave, target)?;
            }
        }
        if slave > 2 {
            sys::close(slave)?;
        }
    }
    Ok(())
}

/// Whether the terminal at 0 still answers: `fstat` and `TCGETS` (`tcgetattr`) on it, each into
/// a buffer of this stack with room for its structure on every architecture here.
///
/// # Safety
/// In the child, before its exec.
unsafe fn live() -> Result<(), c_int> {
    let mut stat = [0_u64; 32];
    let mut termios = [0_u32; 16];
    // SAFETY: 256 and 64 writable bytes, more than the kernel's `stat` (144 bytes at most here)
    // and `termios` (36).
    unsafe {
        sys::fstat(0, stat.as_mut_ptr().cast())?;
        sys::ioctl(0, TCGETS, termios.as_mut_ptr().cast())?;
    }
    Ok(())
}

/// Every signal below `below` back to its default action whatever it was, and above it every
/// caught one (an ignored one stays ignored, as an exec would leave it), with `SIGPIPE` to its
/// default as `std`'s child sets it. Signals 32 and 33, which glibc keeps for itself and its
/// `sigaction` refuses, go through the same raw call.
///
/// # Safety
/// In the child, before its exec, every signal blocked.
unsafe fn reset(below: c_int) {
    let default = [0_u64; 4];
    for signal in 1..=64 {
        if signal == SIGKILL || signal == SIGSTOP {
            continue;
        }
        let reset = signal < below || signal == SIGPIPE || {
            let mut old = [0_u64; 4];
            // SAFETY: `old` is room for the kernel's action on every architecture here (32 bytes
            // on x86_64 and aarch64, 24 on riscv64), its handler first.
            let read = unsafe { sys::rt_sigaction(signal, std::ptr::null(), old.as_mut_ptr()) };
            let [handler, ..] = old;
            read.is_ok() && handler > SIG_IGN
        };
        if reset {
            // SAFETY: an all-zero action is `SIG_DFL` with no flags and an empty mask.
            let _ = unsafe { sys::rt_sigaction(signal, default.as_ptr(), std::ptr::null_mut()) };
        }
    }
}

/// `PR_SET_PDEATHSIG` to `SIGTERM`, then the check that `parent` did not die before it was set
/// ([`crate::tether::bind`]): 0, `prctl`'s errno, or `ESRCH` when `parent` is no longer the parent.
pub(crate) fn bind_parent_death(parent: i32) -> c_int {
    // SAFETY: `prctl` with one integer argument and `getppid`; neither touches memory of ours.
    unsafe {
        if let Err(errno) = sys::prctl(PR_SET_PDEATHSIG, SIGTERM) {
            return errno;
        }
        if sys::getppid() != Ok(c_long::from(parent)) {
            return ESRCH;
        }
    }
    0
}

/// Each candidate, `execve`d in turn: glibc's `execvp` loop. A file the kernel will not run goes
/// to the shell when there is a script argv for it, and that exec's errno is the candidate's.
/// A missing or unreachable candidate passes to the next, one that may not be run is remembered
/// and passes too, and any other error ends the walk with it.
///
/// # Safety
/// In the child; every pointer the walk holds is live.
unsafe fn walk(walk: &Walk<'_>) -> c_int {
    let mut denied = false;
    let mut last = ENOENT;
    for (at, &candidate) in walk.candidates.iter().enumerate() {
        // SAFETY: NUL-terminated strings and null-terminated arrays the parent owns.
        let mut errno = unsafe { sys::execve(candidate, walk.argv, walk.envp) };
        if errno == ENOEXEC
            && let Some(&script) = walk.scripts.and_then(|scripts| scripts.get(at))
        {
            // SAFETY: as above: the shell's path, and this candidate's script argv.
            errno = unsafe { sys::execve(BSHELL.as_ptr(), script, walk.envp) };
        }
        match errno {
            EACCES => denied = true,
            ENOENT | ESTALE | ENOTDIR | ENODEV | ETIMEDOUT => {}
            _ => return errno,
        }
        last = errno;
    }
    if denied { EACCES } else { last }
}

/// Mark every descriptor from 3 up close-on-exec ([`super::super::InheritStrip::apply`]):
/// `close_range`, else the numbers `/proc/self/fd` lists, else each number below `bound`. Raw
/// system calls alone, so a forked or cloned child may run it.
pub(crate) fn strip_from_three(bound: RawFd) {
    if close_range_cloexec() || proc_self_fd() {
        return;
    }
    for fd in 3..bound {
        // SAFETY: integer arguments only; an unused number is a harmless `EBADF`.
        let _ = unsafe { sys::fcntl(fd, F_SETFD, FD_CLOEXEC) };
    }
}

/// `close_range(3, ~0U, CLOSE_RANGE_CLOEXEC)` (Linux 5.11).
fn close_range_cloexec() -> bool {
    const CLOSE_RANGE_CLOEXEC: c_long = 1 << 2;
    // SAFETY: three integer arguments; the kernel touches no memory of ours.
    unsafe { sys::close_range(3, c_long::from(u32::MAX), CLOSE_RANGE_CLOEXEC).is_ok() }
}

/// Mark every number `/proc/self/fd` lists (but its own directory's), read with `getdents64`
/// into a stack buffer; whether the listing was read to its end.
pub(super) fn proc_self_fd() -> bool {
    const AT_FDCWD: c_long = -100;
    const RECLEN_AT: usize = 16;
    const NAME_AT: usize = 19;
    // Each architecture's own flags: `O_DIRECTORY` is 0o200000 on x86_64 but 0o40000 on aarch64,
    // where 0o200000 is `O_DIRECT`, which a `/proc` directory refuses.
    let flags = c_long::from(libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY);
    // SAFETY: a C string literal; the result is a fresh descriptor or an errno.
    let opened = unsafe { sys::openat(AT_FDCWD, c"/proc/self/fd".as_ptr(), flags) };
    let Some(dir) = opened.ok().and_then(|dir| RawFd::try_from(dir).ok()) else {
        return false;
    };
    let mut buf = [0_u8; 4096];
    let complete = loop {
        // SAFETY: `buf` is a live, writable buffer of the length passed.
        let read = unsafe { sys::getdents64(dir, buf.as_mut_ptr(), buf.len()) };
        let Some(end) = read.ok().and_then(|end| usize::try_from(end).ok()) else {
            break false;
        };
        if end == 0 {
            break true;
        }
        let mut at = 0_usize;
        while let Some(record) = buf.get(at..end.min(buf.len())) {
            let (Some(&lo), Some(&hi)) = (record.get(RECLEN_AT), record.get(RECLEN_AT + 1)) else {
                break;
            };
            let length = usize::from(u16::from_ne_bytes([lo, hi]));
            if length == 0 {
                break;
            }
            if let Some(fd) = record
                .get(NAME_AT..length.min(record.len()))
                .and_then(decimal)
                && fd >= 3
                && fd != dir
            {
                // SAFETY: integer arguments only; an unused number is EBADF.
                let _ = unsafe { sys::fcntl(fd, F_SETFD, FD_CLOEXEC) };
            }
            let Some(next) = at.checked_add(length) else {
                break;
            };
            at = next;
        }
    };
    // SAFETY: the directory descriptor opened above.
    let _ = unsafe { sys::close(dir) };
    complete
}

/// The number a `/proc/self/fd` entry's NUL-terminated name spells, `None` for `.` and `..`.
fn decimal(name: &[u8]) -> Option<RawFd> {
    let mut fd: RawFd = 0;
    let mut digits = 0_usize;
    for &byte in name.iter().take_while(|&&byte| byte != 0) {
        let digit = byte.checked_sub(b'0').filter(|&d| d <= 9)?;
        fd = fd.checked_mul(10)?.checked_add(RawFd::from(digit))?;
        digits = digits.saturating_add(1);
    }
    (digits > 0).then_some(fd)
}

/// System calls made by the instruction itself (`svc`, `syscall`, `ecall`), never through a
/// libc function: no cancellation point, no `errno`, no other thread state. Each answers the
/// kernel's word, or the errno its negative word names.
mod sys {
    use core::ffi::{c_char, c_int, c_long, c_uint, c_void};
    use std::os::fd::RawFd;

    #[cfg(target_arch = "x86_64")]
    mod nr {
        use core::ffi::c_long;
        pub(super) const WRITE: c_long = 1;
        pub(super) const CLOSE: c_long = 3;
        pub(super) const FSTAT: c_long = 5;
        pub(super) const RT_SIGACTION: c_long = 13;
        pub(super) const RT_SIGPROCMASK: c_long = 14;
        pub(super) const IOCTL: c_long = 16;
        #[cfg(test)]
        pub(super) const GETPID: c_long = 39;
        pub(super) const EXECVE: c_long = 59;
        pub(super) const FCNTL: c_long = 72;
        pub(super) const CHDIR: c_long = 80;
        pub(super) const SETPGID: c_long = 109;
        pub(super) const GETPPID: c_long = 110;
        pub(super) const SETSID: c_long = 112;
        pub(super) const PRCTL: c_long = 157;
        #[cfg(test)]
        pub(super) const GETTID: c_long = 186;
        pub(super) const GETDENTS64: c_long = 217;
        pub(super) const EXIT_GROUP: c_long = 231;
        #[cfg(test)]
        pub(super) const TGKILL: c_long = 234;
        pub(super) const OPENAT: c_long = 257;
        #[cfg(test)]
        pub(super) const PPOLL: c_long = 271;
        #[cfg(any(test, debug_assertions))]
        pub(super) const NANOSLEEP: c_long = 35;
        pub(super) const DUP3: c_long = 292;
        pub(super) const PRLIMIT64: c_long = 302;
        pub(super) const CLOSE_RANGE: c_long = 436;
    }

    /// The generic table aarch64 and riscv64 share (`asm-generic/unistd.h`; both have the
    /// 64-bit `fstat`, `__ARCH_WANT_NEW_STAT`).
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    mod nr {
        use core::ffi::c_long;
        pub(super) const DUP3: c_long = 24;
        pub(super) const FCNTL: c_long = 25;
        pub(super) const IOCTL: c_long = 29;
        pub(super) const CHDIR: c_long = 49;
        pub(super) const OPENAT: c_long = 56;
        pub(super) const CLOSE: c_long = 57;
        pub(super) const GETDENTS64: c_long = 61;
        pub(super) const WRITE: c_long = 64;
        #[cfg(test)]
        pub(super) const PPOLL: c_long = 73;
        #[cfg(any(test, debug_assertions))]
        pub(super) const NANOSLEEP: c_long = 101;
        pub(super) const FSTAT: c_long = 80;
        pub(super) const EXIT_GROUP: c_long = 94;
        #[cfg(test)]
        pub(super) const TGKILL: c_long = 131;
        pub(super) const RT_SIGACTION: c_long = 134;
        pub(super) const RT_SIGPROCMASK: c_long = 135;
        pub(super) const SETPGID: c_long = 154;
        pub(super) const SETSID: c_long = 157;
        pub(super) const PRCTL: c_long = 167;
        #[cfg(test)]
        pub(super) const GETPID: c_long = 172;
        pub(super) const GETPPID: c_long = 173;
        #[cfg(test)]
        pub(super) const GETTID: c_long = 178;
        pub(super) const EXECVE: c_long = 221;
        pub(super) const PRLIMIT64: c_long = 261;
        pub(super) const CLOSE_RANGE: c_long = 436;
    }

    /// The kernel's signal set is 64 bits on every architecture here.
    const SIGSET_BYTES: c_long = 8;

    /// System call `number` over `args`: the kernel's word, a failure's errno when it is one of
    /// the 4095 negative words the kernel answers an error with.
    ///
    /// # Safety
    /// The call's own: each pointer among `args` is live for what that call reads or writes.
    #[inline(always)]
    unsafe fn call(number: c_long, args: [c_long; 6]) -> Result<c_long, c_int> {
        let [a, b, c, d, e, f] = args;
        let word: c_long;
        // SAFETY: the caller's. The instruction reads the number and six argument registers,
        // writes the answer's (and on x86_64 clobbers rcx and r11), and touches memory only
        // where the call's pointers say: the asm's default memory clobber holds the compiler to
        // that.
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!(
                "svc 0",
                in("x8") number,
                inlateout("x0") a => word,
                in("x1") b,
                in("x2") c,
                in("x3") d,
                in("x4") e,
                in("x5") f,
                options(nostack),
            );
        }
        #[cfg(target_arch = "x86_64")]
        // SAFETY: as above.
        unsafe {
            core::arch::asm!(
                "syscall",
                inlateout("rax") number => word,
                in("rdi") a,
                in("rsi") b,
                in("rdx") c,
                in("r10") d,
                in("r8") e,
                in("r9") f,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack),
            );
        }
        #[cfg(target_arch = "riscv64")]
        // SAFETY: as above.
        unsafe {
            core::arch::asm!(
                "ecall",
                in("a7") number,
                inlateout("a0") a => word,
                in("a1") b,
                in("a2") c,
                in("a3") d,
                in("a4") e,
                in("a5") f,
                options(nostack),
            );
        }
        match c_int::try_from(word) {
            Ok(negative @ -4095..=-1) => Err(negative.saturating_neg()),
            _ => Ok(word),
        }
    }

    /// A pointer as a system call's argument word.
    fn word<T>(pointer: *const T) -> c_long {
        pointer as c_long
    }

    pub(super) unsafe fn rt_sigaction(
        signal: c_int,
        action: *const u64,
        old: *mut u64,
    ) -> Result<c_long, c_int> {
        // SAFETY: the caller's.
        unsafe {
            call(
                nr::RT_SIGACTION,
                [
                    c_long::from(signal),
                    word(action),
                    word(old),
                    SIGSET_BYTES,
                    0,
                    0,
                ],
            )
        }
    }

    pub(crate) unsafe fn rt_sigprocmask(
        how: c_long,
        set: *const u64,
        old: *mut u64,
    ) -> Result<c_long, c_int> {
        // SAFETY: the caller's.
        unsafe {
            call(
                nr::RT_SIGPROCMASK,
                [how, word(set), word(old), SIGSET_BYTES, 0, 0],
            )
        }
    }

    pub(super) unsafe fn dup3(source: RawFd, target: RawFd) -> Result<c_long, c_int> {
        // SAFETY: integers only.
        unsafe {
            call(
                nr::DUP3,
                [c_long::from(source), c_long::from(target), 0, 0, 0, 0],
            )
        }
    }

    pub(super) unsafe fn fcntl(fd: RawFd, command: c_long, arg: c_long) -> Result<c_long, c_int> {
        // SAFETY: integers only (`F_GETFD`, `F_SETFD`).
        unsafe { call(nr::FCNTL, [c_long::from(fd), command, arg, 0, 0, 0]) }
    }

    pub(super) unsafe fn ioctl(
        fd: RawFd,
        request: c_long,
        arg: *mut c_void,
    ) -> Result<c_long, c_int> {
        // SAFETY: the caller's: `arg` is what `request` reads or fills.
        unsafe { call(nr::IOCTL, [c_long::from(fd), request, word(arg), 0, 0, 0]) }
    }

    pub(super) unsafe fn fstat(fd: RawFd, stat: *mut c_void) -> Result<c_long, c_int> {
        // SAFETY: the caller's room for the kernel's `stat`.
        unsafe { call(nr::FSTAT, [c_long::from(fd), word(stat), 0, 0, 0, 0]) }
    }

    /// `prlimit64(0, resource, value, NULL)`: this process's limit, the old one not read.
    pub(super) unsafe fn prlimit64(
        resource: c_uint,
        value: *const [u64; 2],
    ) -> Result<c_long, c_int> {
        // SAFETY: the caller's live soft and hard values.
        unsafe {
            call(
                nr::PRLIMIT64,
                [0, c_long::from(resource), word(value), 0, 0, 0],
            )
        }
    }

    pub(super) unsafe fn chdir(path: *const c_char) -> Result<c_long, c_int> {
        // SAFETY: the caller's NUL-terminated path.
        unsafe { call(nr::CHDIR, [word(path), 0, 0, 0, 0, 0]) }
    }

    pub(super) unsafe fn setpgid(pid: i32, group: i32) -> Result<c_long, c_int> {
        // SAFETY: integers only.
        unsafe {
            call(
                nr::SETPGID,
                [c_long::from(pid), c_long::from(group), 0, 0, 0, 0],
            )
        }
    }

    pub(super) unsafe fn setsid() -> Result<c_long, c_int> {
        // SAFETY: no arguments.
        unsafe { call(nr::SETSID, [0; 6]) }
    }

    pub(super) unsafe fn prctl(option: c_long, value: c_long) -> Result<c_long, c_int> {
        // SAFETY: integers only (`PR_SET_PDEATHSIG`).
        unsafe { call(nr::PRCTL, [option, value, 0, 0, 0, 0]) }
    }

    pub(super) unsafe fn getppid() -> Result<c_long, c_int> {
        // SAFETY: no arguments.
        unsafe { call(nr::GETPPID, [0; 6]) }
    }

    /// `execve`, which returns only on a failure: its errno.
    pub(super) unsafe fn execve(
        path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> c_int {
        // SAFETY: the caller's live strings and arrays; the kernel keeps none on an error.
        let answer = unsafe { call(nr::EXECVE, [word(path), word(argv), word(envp), 0, 0, 0]) };
        answer.err().unwrap_or(super::ENOEXEC)
    }

    pub(super) unsafe fn write(
        fd: RawFd,
        bytes: *const c_void,
        len: usize,
    ) -> Result<c_long, c_int> {
        let len = c_long::try_from(len).unwrap_or(c_long::MAX);
        // SAFETY: the caller's live bytes.
        unsafe { call(nr::WRITE, [c_long::from(fd), word(bytes), len, 0, 0, 0]) }
    }

    pub(super) unsafe fn openat(
        dir: c_long,
        path: *const c_char,
        flags: c_long,
    ) -> Result<c_long, c_int> {
        // SAFETY: the caller's NUL-terminated path.
        unsafe { call(nr::OPENAT, [dir, word(path), flags, 0, 0, 0]) }
    }

    pub(super) unsafe fn getdents64(fd: RawFd, buf: *mut u8, len: usize) -> Result<c_long, c_int> {
        let len = c_long::try_from(len).unwrap_or(c_long::MAX);
        // SAFETY: the caller's live, writable buffer.
        unsafe { call(nr::GETDENTS64, [c_long::from(fd), word(buf), len, 0, 0, 0]) }
    }

    pub(super) unsafe fn close(fd: RawFd) -> Result<c_long, c_int> {
        // SAFETY: an integer.
        unsafe { call(nr::CLOSE, [c_long::from(fd), 0, 0, 0, 0, 0]) }
    }

    pub(super) unsafe fn close_range(
        first: c_long,
        last: c_long,
        flags: c_long,
    ) -> Result<c_long, c_int> {
        // SAFETY: integers only.
        unsafe { call(nr::CLOSE_RANGE, [first, last, flags, 0, 0, 0]) }
    }

    pub(super) unsafe fn exit_group(code: c_int) -> ! {
        // SAFETY: ends this process; nothing runs after it.
        let _ = unsafe { call(nr::EXIT_GROUP, [c_long::from(code), 0, 0, 0, 0, 0]) };
        loop {
            std::hint::spin_loop();
        }
    }

    /// `nanosleep` for `ms` milliseconds, its remainder not read.
    #[cfg(any(test, debug_assertions))]
    pub(super) unsafe fn nanosleep_ms(ms: u32) {
        let time = [
            c_long::from(ms / 1000),
            c_long::from(ms % 1000).saturating_mul(1_000_000),
        ];
        // SAFETY: a live timespec (seconds, then nanoseconds) on this stack.
        let _ = unsafe { call(nr::NANOSLEEP, [word(time.as_ptr()), 0, 0, 0, 0, 0]) };
    }

    /// `ppoll` over no descriptors with no timeout: a wait a signal alone ends.
    #[cfg(test)]
    pub(super) unsafe fn ppoll_forever() -> Result<c_long, c_int> {
        // SAFETY: no descriptors, no timeout, no mask: nothing is read or written.
        unsafe { call(nr::PPOLL, [0; 6]) }
    }

    /// `tgkill(getpid(), gettid(), signal)`: `signal` at this thread.
    #[cfg(test)]
    pub(super) unsafe fn raise(signal: c_int) {
        // SAFETY: integers only.
        unsafe {
            let (Ok(pid), Ok(tid)) = (call(nr::GETPID, [0; 6]), call(nr::GETTID, [0; 6])) else {
                return;
            };
            let _ = call(nr::TGKILL, [pid, tid, c_long::from(signal), 0, 0, 0]);
        }
    }
}

pub(super) use sys::rt_sigprocmask;
