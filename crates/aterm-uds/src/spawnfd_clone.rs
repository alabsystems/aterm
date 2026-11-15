// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LINUX'S SPAWN, WITH NO COPY OF THIS PROCESS (ATERM-KI-1113-811): one `clone(CLONE_VM)` on a
//! stack of its own. A `fork` copies the parent's page tables and marks every private page
//! copy-on-write, so its cost grows with the parent's size (190 ms for 1.3 GB resident here) and
//! every other thread of the parent stalls behind it on its next page fault; this child shares
//! the parent's memory until its exec. The child's steps are [`child`]'s. Without `CLONE_VFORK`
//! no thread waits inside the clone: the spawning one waits on the child's report instead, which
//! a start bounds ([`Plan::start_within`]), and keeps the child's stack and everything it reads
//! until the kernel shows it exec'd or exited: it clears the child's tid word then
//! (`CLONE_CHILD_CLEARTID`), and the report is read after. Below Linux 5.16 a core-dumping signal in a
//! child that shares the memory ends the parent too (the kernel's `zap_threads`, limited to one
//! thread group from 5.16), so there the same child runs in a copy, as a fork's would, and costs
//! what a fork costs.
//!
//! A child that shares the memory without `CLONE_VFORK` marks it shared with another process
//! for as long as it lives (the kernel's `MMF_MULTIPROCESS`), as no `posix_spawn` child does: an
//! `oom_score_adj` write on this process then walks every process (nothing here writes one), and
//! the OOM killer ends this process with a child it picks before that child's exec, as it would
//! a vfork child. Where a shadow stack is on (x86's, arm64's GCS) the kernel gives the child a
//! fresh one, as it gives a thread.

use super::{Image, InheritStrip, Source, off_the_targets, placeable_copy};
use core::ffi::{CStr, c_char, c_int, c_void};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

#[path = "spawnfd_clone_child.rs"]
mod child;
#[cfg(test)]
pub(in crate::spawnfd) use child::Seam;
use child::Slot;
pub(super) use child::{Limit, Step};
pub(crate) use child::{bind_parent_death, strip_from_three};

/// The child's stack: ample for its frames in a debug build, and one inaccessible page below it,
/// so an overflow faults in the child and ends it alone.
const STACK: usize = 256 * 1024;

unsafe extern "C" {
    /// glibc's `clone(2)` wrapper: the child runs `body(arg)` on `stack` and exits with what it
    /// returns; then come the parent's and the child's tid words and the thread pointer.
    fn clone(
        body: unsafe extern "C" fn(*mut c_void) -> c_int,
        stack: *mut c_void,
        flags: c_int,
        arg: *mut c_void,
        ...
    ) -> c_int;
}

const CLONE_VM: c_int = 0x100;
const CLONE_CHILD_CLEARTID: c_int = 0x0020_0000;
const SIGCHLD: c_int = 17;
const SIG_SETMASK: core::ffi::c_long = 2;

/// How long a start that gave up on its child waits for the child it killed to end: the fork
/// path's reap's bound.
const KILLED_ENDS_WITHIN: Duration = Duration::from_secs(2);

/// What one spawn makes its child: built in the parent (it allocates), then started once per
/// try ([`Self::start`]), on the thread whose death or exit the child's tie follows.
pub(super) struct Plan {
    argv: Vec<CString>,
    envp: Vec<CString>,
    /// Name the starting thread in the child's environment ([`crate::tether::THREAD_VAR`]).
    thread_var: bool,
    search: super::search::Search,
    cwd: Option<CString>,
    slots: Vec<Slot>,
    handed: Vec<RawFd>,
    /// What the spawn lock holds across the clone: the handed descriptors and a pass's source.
    handing: Vec<RawFd>,
    null: Option<[OwnedFd; 2]>,
    strip: Option<InheritStrip>,
    reset_below: c_int,
    terminal: Option<RawFd>,
    limits: Vec<Limit>,
    pass: Option<(RawFd, RawFd)>,
    keep_mask: bool,
    pgroup: Option<i32>,
    setsid: bool,
    tie: Option<i32>,
    /// Descriptors the plan holds for its child until it drops (a pipe's far end, a copy).
    _kept: Vec<OwnedFd>,
}

/// What a spawn asks of its child, beside its image ([`Plan::new`]).
#[derive(Default)]
pub(super) struct Spec {
    /// Slots 0 up, at most five: stdio, then a lane spawn's two lanes.
    pub(super) slots: Vec<Source>,
    /// Descriptors the child keeps at their own numbers (guards, a tether's exec status).
    pub(super) handed: Vec<RawFd>,
    /// Descriptors the plan holds until it drops.
    pub(super) kept: Vec<OwnedFd>,
    pub(super) cwd: Option<CString>,
    /// Keep this process's whole table (a pass-through spawn): no strip, slots as they are.
    pub(super) inherit_all: bool,
    pub(super) default_signals: bool,
    /// Exec with the starting thread's mask, not an empty one.
    pub(super) keep_mask: bool,
    pub(super) pgroup: Option<i32>,
    pub(super) setsid: bool,
    /// Run the program's own image, never the shell over it ([`super::NativeImage`]).
    pub(super) native: bool,
    /// The pid the child is tied to by its parent-death signal ([`crate::tether::bind`]).
    pub(super) tie: Option<i32>,
    pub(super) thread_var: bool,
    /// The terminal slave the child leads a session on, put on 0, 1 and 2 (no slots then), its
    /// signals below 32 back to their defaults ([`Step::Terminal`], [`Step::Live`]).
    pub(super) terminal: Option<RawFd>,
    /// Resource limits the child sets on itself ([`Step::Limits`]).
    pub(super) limits: Vec<Limit>,
    /// A descriptor the child holds at a number of its own: from, at ([`Step::Pass`]).
    pub(super) pass: Option<(RawFd, RawFd)>,
    /// Exec the program exactly as named: no search, and no shell over it.
    pub(super) exact: bool,
}

impl Plan {
    /// The plan for `image` as `spec` asks.
    ///
    /// # Errors
    /// The search's ([`super::search::Search::new`]), more than five slots, or `/dev/null`'s
    /// open.
    pub(super) fn new(image: &Image, spec: Spec) -> io::Result<Self> {
        let Spec {
            slots,
            handed,
            kept,
            cwd,
            inherit_all,
            default_signals,
            keep_mask,
            pgroup,
            setsid,
            native,
            tie,
            thread_var,
            terminal,
            limits,
            pass,
            exact,
        } = spec;
        if slots.len() > 5 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a spawn places at most five slots",
            ));
        }
        let wants_null = slots.iter().any(|slot| match slot {
            Source::Null => true,
            Source::Inherit => !inherit_all,
            Source::Fd(_) => false,
        });
        let null = if wants_null {
            Some([null(libc::O_RDONLY)?, null(libc::O_WRONLY)?])
        } else {
            None
        };
        let [null_read, null_write] = null.as_ref().map_or([-1, -1], |[read, write]| {
            [read.as_raw_fd(), write.as_raw_fd()]
        });
        let slots = (0..)
            .zip(slots)
            .map(|(at, slot)| match slot {
                Source::Inherit if inherit_all => Slot::AsIs,
                Source::Inherit => Slot::Inherit,
                Source::Null if at == 0 => Slot::Dup(null_read),
                Source::Null => Slot::Dup(null_write),
                Source::Fd(fd) => Slot::Dup(fd),
            })
            .collect();
        let search = if exact {
            super::search::Search::exact(&image.program)
        } else {
            super::search::Search::new(&image.program, image.search.as_deref(), native)?
        };
        let handing = handed
            .iter()
            .copied()
            .chain(pass.map(|(from, _)| from))
            .collect();
        let reset_below = if default_signals {
            65
        } else if terminal.is_some() {
            32
        } else {
            0
        };
        Ok(Self {
            argv: image.argv.clone(),
            envp: image.envp.clone(),
            thread_var,
            search,
            cwd,
            slots,
            handed,
            handing,
            null,
            strip: (!inherit_all).then(InheritStrip::prepare),
            reset_below,
            terminal,
            limits,
            pass,
            keep_mask,
            pgroup,
            setsid,
            tie,
            _kept: kept,
        })
    }

    /// One try: the child's pid once its exec succeeded, else the errno the step that stopped it
    /// failed with (its exec's, last), the child reaped.
    ///
    /// # Errors
    /// That errno, the clone's, or the report pipe's.
    pub(super) fn start(&self) -> io::Result<i32> {
        match self.run(None)? {
            Outcome::Started(pid) => Ok(pid),
            Outcome::Stopped(_, error) => Err(error),
            Outcome::Hung { .. } => Err(io::Error::other(
                "the child's exec-status report could not be read",
            )),
        }
    }

    /// One try bounded by `within`: a child that has not let go of this process by then (its
    /// exec, its exit) is killed, and reaped when it ends soon after ([`KILLED_ENDS_WITHIN`]); one
    /// that may still read this plan keeps it, its stack and what it reads for good.
    ///
    /// # Errors
    /// The clone's, or the report pipe's.
    pub(super) fn start_within(self, within: Duration) -> io::Result<Outcome> {
        let outcome = self.run(Some(Instant::now() + within));
        if let Ok(Outcome::Hung { holds: true }) = outcome {
            std::mem::forget(self);
        }
        outcome
    }

    /// One try: the clone, then the child's release of this process and its report, until
    /// `deadline` when there is one.
    fn run(&self, deadline: Option<Instant>) -> io::Result<Outcome> {
        let thread = self
            .thread_var
            .then(|| {
                // SAFETY: `gettid` takes nothing and cannot fail.
                let thread = unsafe { libc::syscall(libc::SYS_gettid) };
                CString::new(format!("{}={thread}", crate::tether::THREAD_VAR))
            })
            .transpose()
            .map_err(io::Error::other)?;
        let envp: Vec<*const c_char> = self
            .envp
            .iter()
            .chain(&thread)
            .map(|entry| entry.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        let argv = Image::pointers(&self.argv);
        let candidates: Vec<*const c_char> = self
            .search
            .candidates
            .iter()
            .map(|path| path.as_ptr())
            .collect();
        let scripts: Vec<Vec<*const c_char>> = if self.search.script {
            self.search
                .candidates
                .iter()
                .map(|path| {
                    [super::BSHELL.as_ptr(), path.as_ptr()]
                        .into_iter()
                        .chain(argv.iter().skip(1).copied())
                        .collect()
                })
                .collect()
        } else {
            Vec::new()
        };
        let script_argvs: Vec<*const *const c_char> = scripts.iter().map(|s| s.as_ptr()).collect();
        // Both ends above every slot: in a process that closed its stdio, a fresh pipe lands
        // there, and an inherited slot would hand the child the end as its own stdio. The write
        // end is off a pass's number too, which the pass's `dup3` would close.
        let (report_rd, report_wr) = crate::pipe()?;
        let report_rd = off_the_targets(OwnedFd::from(report_rd))?;
        let mut report_wr = off_the_targets(OwnedFd::from(report_wr))?;
        #[cfg(test)]
        if let Some(at) = REPORT_AT.with(std::cell::Cell::get) {
            report_wr = tests::moved_onto(report_wr, at);
        }
        if let Some((_, at)) = self.pass
            && report_wr.as_raw_fd() == at
        {
            report_wr = placeable_copy(report_wr.as_raw_fd())?;
        }
        let [null_read, null_write] = self.null.as_ref().map_or([-1, -1], |[read, write]| {
            [read.as_raw_fd(), write.as_raw_fd()]
        });
        // On the heap, and freed by hand: the child reads `args`, and the kernel writes `word`
        // for it, while this thread runs on, so neither may move or drop before the child let
        // go of this process's memory.
        let args = Box::into_raw(Box::new(child::Args {
            walk: child::Walk {
                candidates: &candidates,
                scripts: self.search.script.then_some(script_argvs.as_slice()),
                argv: argv.as_ptr(),
                envp: envp.as_ptr(),
            },
            cwd: self.cwd.as_deref(),
            slots: &self.slots,
            null: [null_read, null_write],
            handed: &self.handed,
            strip: self.strip.map(InheritStrip::bound),
            reset_below: self.reset_below,
            terminal: self.terminal,
            limits: &self.limits,
            pass: self.pass,
            mask: 0,
            pgroup: self.pgroup,
            setsid: self.setsid,
            tie: self.tie,
            report: report_wr.as_raw_fd(),
            #[cfg(any(test, debug_assertions))]
            stall_ms: STALL_MS.with(std::cell::Cell::get),
            #[cfg(test)]
            seam: SEAM.with(std::cell::Cell::get),
        }));
        let word = Box::into_raw(Box::new(AtomicU32::new(HELD)));
        #[cfg(test)]
        let tripwire = tests::arm_tripwire();
        let launched = {
            let _handing = crate::spawn_lock::handing(&self.handing);
            // SAFETY: `args` and `word` stay live and unchanged by this thread until the child
            // let go of this process (below).
            unsafe { launch(args, word, self.keep_mask) }
        };
        #[cfg(test)]
        let stranger = STRANGER
            .with(std::cell::Cell::get)
            .then(|| report_wr.try_clone());
        drop(report_wr);
        #[cfg_attr(
            not(test),
            expect(unused_mut, reason = "only a test measures the stack")
        )]
        let (pid, mut stack, shared) = match launched {
            Ok(launched) => launched,
            Err(error) => {
                // SAFETY: no child was made; the boxes this function made, each freed once.
                drop(unsafe { (Box::from_raw(args), Box::from_raw(word)) });
                return Err(error);
            }
        };
        #[cfg(test)]
        LAST_PID.with(|last| last.set(pid));
        // Whether the child let go of this process's memory; a copy never held it, and its
        // report's end is its exec or exit.
        let (report, let_go) = if !shared {
            (read_report(report_rd.as_fd(), deadline), true)
        // SAFETY: the box made above, live until freed below.
        } else if released(unsafe { &*word }, deadline) {
            (report_now(report_rd.as_fd()), true)
        } else {
            (Report::Silent, false)
        };
        #[cfg(test)]
        drop((tripwire, stranger));
        let outcome = match report {
            Report::Ended => Outcome::Started(pid),
            Report::Said(word) => {
                reap_as(pid, deadline);
                stopped(word)
            }
            Report::Cut => {
                reap_as(pid, deadline);
                Outcome::Stopped(
                    Step::Exec,
                    io::Error::other("the child's exec-status report was cut short"),
                )
            }
            Report::Silent => {
                // SAFETY: our own child, not yet reaped, so the pid is still its.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                let reaped = reap_within(pid, KILLED_ENDS_WITHIN);
                if !reaped {
                    crate::reaper::hand_off_pid(pid);
                }
                Outcome::Hung {
                    holds: !let_go && !reaped,
                }
            }
        };
        if let Outcome::Hung { holds: true } = outcome {
            // The child may still run, or a call of its may still read or write what it holds,
            // and the kernel writes `word` when it ends: all of it stays as it is, for good.
            std::mem::forget(stack);
            std::mem::forget((envp, argv, thread, candidates, scripts, script_argvs));
            return Ok(outcome);
        }
        #[cfg(test)]
        tests::measure(stack.bytes());
        drop(stack);
        // SAFETY: the child exec'd or ended, or ran in a copy of this memory and never held these;
        // the boxes this function made, each freed once.
        drop(unsafe { (Box::from_raw(args), Box::from_raw(word)) });
        Ok(outcome)
    }
}

/// How one start ended ([`Plan::start_within`]).
pub(super) enum Outcome {
    /// The child exec'd, or ended with nothing to say: its pid, the caller's to reap.
    Started(i32),
    /// The step the child stopped at and its error; the child reaped, or handed to the reaper.
    Stopped(Step, io::Error),
    /// The child had not let go of this process by the deadline: killed, and reaped or handed
    /// to the reaper; `holds` when it may still read the plan, which then stays for good.
    Hung { holds: bool },
}

/// The word's value until the kernel clears it ([`released`]).
const HELD: u32 = 1;

/// Wait until the child let go of this process's memory: the kernel cleared `word` at its exec
/// or its exit (`CLONE_CHILD_CLEARTID`, at every exit from Linux 5.16, the only kernels where the
/// child shares this memory, a core-dumping one's included) and woke this wait; `false` at
/// `deadline` first. Another process that holds the report's write end (a child another thread
/// made meanwhile) delays nothing here.
fn released(word: &AtomicU32, deadline: Option<Instant>) -> bool {
    loop {
        if word.load(Ordering::Acquire) != HELD {
            return true;
        }
        let timeout = match deadline {
            None => None,
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return false;
                }
                Some(libc::timespec {
                    tv_sec: libc::time_t::try_from(left.as_secs()).unwrap_or(libc::time_t::MAX),
                    tv_nsec: libc::c_long::from(left.subsec_nanos()),
                })
            }
        };
        // SAFETY: a live word and timeout (or none: no bound); `FUTEX_WAIT` (not private: the
        // kernel's wake is not) returns at once when the word is no longer `HELD`, at a wake, a
        // signal or the timeout.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                word.as_ptr(),
                libc::FUTEX_WAIT,
                HELD,
                timeout
                    .as_ref()
                    .map_or(std::ptr::null(), std::ptr::from_ref),
                std::ptr::null::<u32>(),
                0,
            )
        };
    }
}

/// What a child's report pipe held at its end.
enum Report {
    /// Nothing: the child exec'd (its end closed on the exec) or ended.
    Ended,
    Said([u8; 8]),
    Cut,
    /// Nothing by the deadline, or a read that failed.
    Silent,
}

/// The report of a child that let go of this process: its eight bytes, which it wrote before
/// its exit in one write under `PIPE_BUF`, or nothing (its exec). Never a wait: another process
/// may hold the write end.
fn report_now(fd: BorrowedFd<'_>) -> Report {
    let mut ready = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live `pollfd`, no wait.
    if unsafe { libc::poll(&mut ready, 1, 0) } <= 0 {
        return Report::Ended;
    }
    let mut word = [0_u8; 8];
    loop {
        // SAFETY: `word` is live and writable; the pipe is readable, so this does not wait.
        let n = unsafe { libc::read(fd.as_raw_fd(), word.as_mut_ptr().cast(), word.len()) };
        return match n {
            0 => Report::Ended,
            8 => Report::Said(word),
            n if n < 0 => {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                Report::Silent
            }
            _ => Report::Cut,
        };
    }
}

/// The report on `fd`, read until its end, eight bytes or `deadline`.
fn read_report(fd: BorrowedFd<'_>, deadline: Option<Instant>) -> Report {
    let mut word = [0_u8; 8];
    let mut got = 0;
    while got < word.len() {
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Report::Silent;
            }
            let ms = c_int::try_from(left.as_millis().saturating_add(1)).unwrap_or(c_int::MAX);
            let mut ready = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one live `pollfd`.
            match unsafe { libc::poll(&mut ready, 1, ms) } {
                0 => continue,
                n if n < 0 => {
                    if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Report::Silent;
                }
                _ => {}
            }
        }
        let rest = &mut word[got..];
        // SAFETY: `rest` is a live, writable slice of `word`.
        let n = unsafe { libc::read(fd.as_raw_fd(), rest.as_mut_ptr().cast(), rest.len()) };
        if n == 0 {
            break;
        }
        if n < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Report::Silent;
        }
        got += n.unsigned_abs();
    }
    match got {
        0 => Report::Ended,
        8 => Report::Said(word),
        _ => Report::Cut,
    }
}

/// The step and errno a child's eight-byte report names.
fn stopped(word: [u8; 8]) -> Outcome {
    let [a, b, c, d, e, f, g, h] = word;
    let step = Step::from_word(u32::from_be_bytes([a, b, c, d]));
    let errno = i32::from_be_bytes([e, f, g, h]);
    match step {
        Some(step) => Outcome::Stopped(step, io::Error::from_raw_os_error(errno)),
        None => Outcome::Stopped(
            Step::Exec,
            io::Error::other(format!(
                "the child's report names no step it takes ({word:?})"
            )),
        ),
    }
}

/// `/dev/null` opened with `flags`, above every slot.
fn null(flags: c_int) -> io::Result<OwnedFd> {
    // SAFETY: a C string literal; the result is a fresh descriptor or -1.
    let fd = unsafe { libc::open(c"/dev/null".as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: just opened here, owned by nothing else.
    off_the_targets(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Reap a child that reported a failed step and is ending: at once with no deadline, else
/// within [`KILLED_ENDS_WITHIN`] and then by the reaper.
fn reap_as(pid: i32, deadline: Option<Instant>) {
    if deadline.is_none() {
        reap(pid);
    } else if !reap_within(pid, KILLED_ENDS_WITHIN) {
        crate::reaper::hand_off_pid(pid);
    }
}

/// Wait for a child that failed before its exec.
fn reap(pid: i32) {
    let mut raw = 0;
    // SAFETY: our own child; `raw` is a live local.
    while unsafe { libc::waitpid(pid, &mut raw, 0) } < 0
        && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
    {}
}

/// Wait up to `bound` for a child that is ending; whether it ended and was reaped.
fn reap_within(pid: i32, bound: Duration) -> bool {
    let until = Instant::now() + bound;
    loop {
        let mut raw = 0;
        // SAFETY: our own child; `raw` is a live local.
        let got = unsafe { libc::waitpid(pid, &mut raw, libc::WNOHANG) };
        if got == pid {
            return true;
        }
        if got < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return false;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Start the child over `*args` on a stack of its own, every signal blocked across the clone (32
/// and 33, which glibc's `sigprocmask` leaves open, included), so none can run a handler of this
/// process's in it before it resets them; the child execs with this thread's mask when
/// `keep_mask`, else an empty one. Returns at once with the child's pid, its stack and whether it
/// shares this process's memory, where it runs until its exec or exit, the kernel clearing
/// `*word` then.
///
/// # Safety
/// `args` and `word` are live, and unchanged by this process, until the child let go of its
/// memory ([`released`]).
unsafe fn launch(
    args: *mut child::Args<'_>,
    word: *mut AtomicU32,
    keep_mask: bool,
) -> io::Result<(i32, Stack, bool)> {
    let stack = Stack::new()?;
    let all = u64::MAX;
    let mut old = 0_u64;
    // SAFETY: two live 64-bit sets.
    if let Err(errno) = unsafe { child::rt_sigprocmask(SIG_SETMASK, &all, &mut old) } {
        return Err(io::Error::from_raw_os_error(errno));
    }
    // SAFETY: the caller's; no child reads it yet.
    unsafe { (*args).mask = if keep_mask { old } else { 0 } };
    let mut shared = shares_memory();
    let pid = loop {
        let flags = SIGCHLD
            | if shared {
                CLONE_VM | CLONE_CHILD_CLEARTID
            } else {
                0
            };
        // SAFETY: `entry` reads `args` alone, which the caller keeps live until the child let
        // go of this process's memory, as it keeps `word`, which the kernel clears then. The
        // stack is mapped, writable and this child's, kept until then too, and every signal is
        // blocked across the clone (above), as `entry` requires.
        let pid = unsafe {
            clone(
                child::entry,
                stack.top(),
                flags,
                args.cast(),
                std::ptr::null_mut::<c_int>(),
                std::ptr::null_mut::<c_void>(),
                word.cast::<c_int>(),
            )
        };
        if pid < 0 && shared && io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
            // A translator that runs a memory-sharing clone as a thread's alone (qemu-user)
            // refuses it: this process's spawns run in a copy from now on.
            COPY_FROM_NOW.store(true, Ordering::Relaxed);
            shared = false;
            continue;
        }
        break pid;
    };
    let error = (pid < 0).then(io::Error::last_os_error);
    // SAFETY: the set this thread had.
    let _ = unsafe { child::rt_sigprocmask(SIG_SETMASK, &old, std::ptr::null_mut()) };
    match error {
        Some(error) => Err(error),
        None => Ok((pid, stack, shared)),
    }
}

/// Whether the child shares this process's memory: from Linux 5.16, where a core-dumping signal
/// in it ends its own thread group alone, natively (a translator runs a memory-sharing clone
/// without `CLONE_VFORK` as a thread's or not at all), and while no clone was refused
/// ([`COPY_FROM_NOW`]) and a test has not asked for a copy.
fn shares_memory() -> bool {
    static SHARES: OnceLock<bool> = OnceLock::new();
    #[cfg(test)]
    if COPY.with(std::cell::Cell::get) {
        return false;
    }
    !COPY_FROM_NOW.load(Ordering::Relaxed)
        && *SHARES.get_or_init(|| {
            kernel_release().is_some_and(|release| release >= (5, 16))
                && crate::process::translator().is_none()
        })
}

/// Set once a memory-sharing clone was refused (`EINVAL`).
static COPY_FROM_NOW: AtomicBool = AtomicBool::new(false);

/// The running kernel's major and minor release (`uname`).
fn kernel_release() -> Option<(u32, u32)> {
    // SAFETY: `utsname` is plain bytes; zero is a valid value.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: a live, writable `utsname`.
    if unsafe { libc::uname(&mut name) } != 0 {
        return None;
    }
    // SAFETY: `uname` NUL-terminates each field.
    let release = unsafe { CStr::from_ptr(name.release.as_ptr()) }.to_bytes();
    let mut parts = release.split(|&b| !b.is_ascii_digit());
    let mut number = || std::str::from_utf8(parts.next()?).ok()?.parse::<u32>().ok();
    Some((number()?, number()?))
}

/// The child's stack: [`STACK`] bytes above one inaccessible page, unmapped on drop.
struct Stack {
    base: *mut c_void,
    len: usize,
    #[cfg(test)]
    guard: usize,
}

impl Stack {
    fn new() -> io::Result<Self> {
        // SAFETY: `sysconf` reads one value.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .ok()
            .filter(|&page| page > 0)
            .ok_or_else(|| io::Error::other("no page size"))?;
        let len = STACK + page;
        // SAFETY: a fresh anonymous private mapping nothing else uses.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_STACK | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        #[cfg_attr(not(test), expect(unused_mut, reason = "only a test paints the stack"))]
        let mut stack = Self {
            base,
            len,
            #[cfg(test)]
            guard: page,
        };
        // SAFETY: the mapping's lowest page, made inaccessible.
        if unsafe { libc::mprotect(base, page, libc::PROT_NONE) } != 0 {
            return Err(io::Error::last_os_error());
        }
        #[cfg(test)]
        tests::paint(stack.bytes());
        Ok(stack)
    }

    /// The stack's writable bytes, above its guard page.
    #[cfg(test)]
    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: the mapping's writable part, which this value alone uses while no child runs.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.base.byte_add(self.guard).cast(),
                self.len - self.guard,
            )
        }
    }

    /// The stack's top, where the child starts (it grows down): page-aligned.
    fn top(&self) -> *mut c_void {
        // SAFETY: one past the mapping's end, which `clone` takes as the child's first frame.
        unsafe { self.base.byte_add(self.len) }
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made; dropped only once the child that ran on it exec'd or
        // exited.
        unsafe { libc::munmap(self.base, self.len) };
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// TEST SEAM: how long this thread's spawns' children wait before their first step, as a
    /// loaded machine starves one ([`stall_children`]).
    static STALL_MS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// TEST SEAM: this thread's spawns' children wait `ms` before their first step (0: none).
#[cfg(any(test, debug_assertions))]
pub(super) fn stall_children(ms: u32) {
    STALL_MS.with(|stall| stall.set(ms));
}

#[cfg(test)]
thread_local! {
    /// TEST SEAM: this thread's spawns run their child in a copy ([`shares_memory`]).
    static COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// TEST SEAM: the fault this thread's spawns make their child make ([`child::Seam`]).
    static SEAM: std::cell::Cell<child::Seam> = const { std::cell::Cell::new(child::Seam::None) };
    /// TEST SEAM: the number this thread's spawns' report write end is put at first.
    static REPORT_AT: std::cell::Cell<Option<RawFd>> = const { std::cell::Cell::new(None) };
    /// TEST SEAM: this thread's spawns keep a copy of their report's write end, as another
    /// process holding it would, until the start ends.
    static STRANGER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The pid of this thread's last spawn's child.
    static LAST_PID: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

/// TEST SEAM: `body` with this thread's spawns' report write end put at `report_at` first, and
/// a copy of it kept by this process when `stranger`.
#[cfg(test)]
pub(in crate::spawnfd) fn with_report<R>(
    report_at: Option<RawFd>,
    stranger: bool,
    body: impl FnOnce() -> R,
) -> R {
    let was = (
        REPORT_AT.with(|r| r.replace(report_at)),
        STRANGER.with(|s| s.replace(stranger)),
    );
    let result = body();
    REPORT_AT.with(|r| r.set(was.0));
    STRANGER.with(|s| s.set(was.1));
    result
}

/// The pid of this thread's last spawn's child.
#[cfg(test)]
pub(in crate::spawnfd) fn last_pid() -> i32 {
    LAST_PID.with(std::cell::Cell::get)
}

/// TEST SEAM: `body` with this thread's spawns' children run in a copy when `copy`, making
/// `seam`'s fault.
#[cfg(test)]
pub(in crate::spawnfd) fn with_child<R>(copy: bool, seam: Seam, body: impl FnOnce() -> R) -> R {
    let was = (
        COPY.with(|c| c.replace(copy)),
        SEAM.with(|s| s.replace(seam)),
    );
    let result = body();
    COPY.with(|c| c.set(was.0));
    SEAM.with(|s| s.set(was.1));
    result
}

/// TEST SEAM: end the calling test COULD NOT RUN where a spawn's child runs in a copy of this
/// process rather than in its memory.
#[cfg(test)]
pub(in crate::spawnfd) fn sharing_or_could_not_run() {
    if let Some(translator) = crate::process::translator() {
        crate::host::could_not_run(format!(
            "{translator} runs a CLONE_VM child in a copy of its parent"
        ));
    }
    if !shares_memory() {
        crate::host::could_not_run(
            "below Linux 5.16, or once a clone refused to share it, a spawn's child runs in a copy",
        );
    }
}

#[cfg(test)]
#[path = "spawnfd_clone_tests.rs"]
mod tests;
