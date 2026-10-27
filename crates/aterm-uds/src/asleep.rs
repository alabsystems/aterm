// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support (test and debug builds): the call a thread of this process sleeps in, as the
//! kernel has it. A test reads it to tell a thread queued in the kernel for a file lock
//! (`flock(2)` without `LOCK_NB`, which the holder's release ends) from one that polls, which
//! sleeps between its tries in some other call (a timer's, a channel's), with no clock read: a
//! poll of any period is asleep elsewhere, so no wait on the poller's next tick decides it.
//!
//! macOS reads the thread's run state (`thread_info`), and while it sleeps, its saved registers
//! (`thread_get_state` with the thread suspended): a thread asleep in a system call is stopped
//! just past the trap in that call's stub, which `dladdr` names, with the call's first argument
//! still in its first argument register. Linux reads `/proc/self/task/<tid>/syscall`.

use std::io;
use std::path::Path;

/// A thread of this process, named so another thread can ask where it sleeps ([`asleep`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thread {
    /// Its Mach port name in this task.
    #[cfg(target_os = "macos")]
    port: u32,
    /// Its kernel thread id.
    #[cfg(target_os = "linux")]
    tid: i64,
}

/// Where a sleeping thread sleeps ([`asleep`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asleep {
    /// In `flock(2)` on descriptor `fd`: queued in the kernel for that file's lock.
    Flock {
        /// The descriptor the call names.
        fd: i32,
    },
    /// In another call: its symbol (macOS, the saved program counter's, or the counter itself
    /// where the loader names none), or `syscall <n>` or `no call` (Linux).
    Elsewhere(String),
}

/// The calling thread.
#[must_use]
pub fn this_thread() -> Thread {
    imp::this_thread()
}

/// Every thread of this process now.
///
/// # Errors
/// The kernel's refusal to list them (macOS `task_threads`), or an unreadable
/// `/proc/self/task` (Linux).
pub fn threads() -> io::Result<Vec<Thread>> {
    imp::threads()
}

/// Where `thread` sleeps now: `Ok(None)` while it runs or is runnable (or woke while it was
/// read), `Ok(Some(_))` while it sleeps in the kernel.
///
/// # Errors
/// The kernel's refusal to read the thread (one that has ended, on macOS), or the unreadable
/// `/proc` entry (Linux).
pub fn asleep(thread: Thread) -> io::Result<Option<Asleep>> {
    imp::asleep(thread)
}

/// The threads of this process, the caller apart, asleep now in `flock(2)` on a descriptor for
/// the file at `path` (the same device and inode): those queued in the kernel for its lock. A
/// thread that ends or wakes while it is read is not among them.
///
/// # Errors
/// `path`'s metadata, or [`threads`]' error.
pub fn queued_for(path: &Path) -> io::Result<Vec<Thread>> {
    use std::os::unix::fs::MetadataExt as _;
    let want = std::fs::metadata(path)?;
    let me = this_thread();
    Ok(threads()?
        .into_iter()
        .filter(|t| *t != me)
        .filter(|t| match asleep(*t) {
            Ok(Some(Asleep::Flock { fd })) => {
                names(fd).is_some_and(|(dev, ino)| dev == want.dev() && ino == want.ino())
            }
            _ => false,
        })
        .collect())
}

/// The device and inode of the file descriptor `fd` names now (`None` once it is closed), by
/// `fstat(2)` of the number itself: no descriptor is opened or closed, so no lock on the file
/// (a `fcntl(2)` record lock of this process's included) is touched. Spelled as std's
/// `MetadataExt` spells them.
fn names(fd: i32) -> Option<(u64, u64)> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fstat` writes one `stat` into the live buffer or fails, writing nothing; a
    // closed `fd` answers EBADF.
    if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `fstat` returned 0, so it filled the buffer.
    let st = unsafe { st.assume_init() };
    // macOS's `dev_t` is an `i32`, which std sign-extends; Linux's is a `u64` already.
    #[cfg(target_os = "macos")]
    let dev = st.st_dev as u64;
    #[cfg(not(target_os = "macos"))]
    let dev = st.st_dev;
    Some((dev, st.st_ino))
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, c_char, c_void};
    use std::io;

    use super::{Asleep, Thread};

    type MachPort = u32;
    type KernReturn = i32;

    #[repr(C)]
    struct DlInfo {
        fname: *const c_char,
        fbase: *mut c_void,
        sname: *const c_char,
        saddr: *mut c_void,
    }

    unsafe extern "C" {
        static mach_task_self_: MachPort;
        fn pthread_self() -> *mut c_void;
        fn pthread_mach_thread_np(thread: *mut c_void) -> MachPort;
        fn task_threads(task: MachPort, list: *mut *mut MachPort, count: *mut u32) -> KernReturn;
        fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
        fn vm_deallocate(task: MachPort, address: usize, size: usize) -> KernReturn;
        fn thread_suspend(thread: MachPort) -> KernReturn;
        fn thread_resume(thread: MachPort) -> KernReturn;
        fn thread_get_state(
            thread: MachPort,
            flavor: i32,
            state: *mut u32,
            count: *mut u32,
        ) -> KernReturn;
        fn thread_info(
            thread: MachPort,
            flavor: u32,
            info: *mut i32,
            count: *mut u32,
        ) -> KernReturn;
        fn dladdr(addr: *const c_void, info: *mut DlInfo) -> i32;
    }

    const THREAD_BASIC_INFO: u32 = 3;
    const THREAD_BASIC_INFO_COUNT: u32 = 10;
    /// `thread_basic_info`'s `run_state`, after its two `time_value_t`s, `cpu_usage` and
    /// `policy`.
    const RUN_STATE: usize = 6;
    const TH_STATE_WAITING: i32 = 3;
    const TH_STATE_UNINTERRUPTIBLE: i32 = 4;

    fn kern(what: &str, kr: KernReturn) -> io::Error {
        io::Error::other(format!("{what}: kern_return_t {kr}"))
    }

    pub(super) fn this_thread() -> Thread {
        // SAFETY: the calling thread's own pthread handle, and its port name in this task.
        let port = unsafe { pthread_mach_thread_np(pthread_self()) };
        Thread { port }
    }

    pub(super) fn threads() -> io::Result<Vec<Thread>> {
        let mut list: *mut MachPort = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: `list` and `count` are live out-parameters; on success the kernel maps an array
        // of `count` port names (each a new send right) into this task at `list`.
        let kr = unsafe { task_threads(mach_task_self_, &raw mut list, &raw mut count) };
        if kr != 0 {
            return Err(kern("task_threads", kr));
        }
        let n = count as usize;
        let mut found = Vec::with_capacity(n);
        for i in 0..n {
            // SAFETY: `i < count`, inside the array the kernel mapped.
            let port = unsafe { *list.add(i) };
            found.push(Thread { port });
            // SAFETY: drops the send right `task_threads` added; the thread's own (pthread's)
            // keeps the name valid while it lives.
            unsafe { mach_port_deallocate(mach_task_self_, port) };
        }
        // SAFETY: returns the array `task_threads` mapped, `count` port names long, which
        // nothing reads after this.
        unsafe {
            vm_deallocate(
                mach_task_self_,
                list as usize,
                n * std::mem::size_of::<MachPort>(),
            )
        };
        Ok(found)
    }

    fn sleeping(port: MachPort) -> io::Result<bool> {
        let mut info = [0i32; THREAD_BASIC_INFO_COUNT as usize];
        let mut count = THREAD_BASIC_INFO_COUNT;
        // SAFETY: `info` holds `count` integers, the size the flavor fills.
        let kr = unsafe { thread_info(port, THREAD_BASIC_INFO, info.as_mut_ptr(), &raw mut count) };
        if kr != 0 {
            return Err(kern("thread_info", kr));
        }
        Ok(matches!(
            info[RUN_STATE],
            TH_STATE_WAITING | TH_STATE_UNINTERRUPTIBLE
        ))
    }

    /// The thread's saved program counter and first argument register, or the call's refusal:
    /// no allocation, since the thread it reads is suspended and may hold the allocator's lock.
    ///
    /// # Safety
    /// `port` names a thread of this task that is suspended.
    #[cfg(target_arch = "aarch64")]
    unsafe fn pc_arg(port: MachPort) -> Result<(u64, u64), KernReturn> {
        const ARM_THREAD_STATE64: i32 = 6;
        // x0-x28, fp, lr, sp, pc, then cpsr and a pad word: 68 u32 words.
        let mut state = [0u64; 34];
        let mut count = 68u32;
        // SAFETY: `state` is 68 u32 words, the size the flavor fills.
        let kr = unsafe {
            thread_get_state(
                port,
                ARM_THREAD_STATE64,
                state.as_mut_ptr().cast::<u32>(),
                &raw mut count,
            )
        };
        if kr != 0 {
            return Err(kr);
        }
        Ok((state[32], state[0]))
    }

    /// The thread's saved program counter and first argument register, as the aarch64 twin
    /// reads them.
    ///
    /// # Safety
    /// As the aarch64 twin: `port` names a thread of this task that is suspended.
    #[cfg(target_arch = "x86_64")]
    unsafe fn pc_arg(port: MachPort) -> Result<(u64, u64), KernReturn> {
        const X86_THREAD_STATE64: i32 = 4;
        // rax, rbx, rcx, rdx, rdi, rsi, rbp, rsp, r8-r15, then rip, rflags, cs, fs, gs: 21
        // u64s, 42 u32 words.
        let mut state = [0u64; 21];
        let mut count = 42u32;
        // SAFETY: `state` is 42 u32 words, the size the flavor fills.
        let kr = unsafe {
            thread_get_state(
                port,
                X86_THREAD_STATE64,
                state.as_mut_ptr().cast::<u32>(),
                &raw mut count,
            )
        };
        if kr != 0 {
            return Err(kr);
        }
        Ok((state[16], state[4]))
    }

    fn symbol(pc: u64) -> Option<String> {
        let mut info = DlInfo {
            fname: std::ptr::null(),
            fbase: std::ptr::null_mut(),
            sname: std::ptr::null(),
            saddr: std::ptr::null_mut(),
        };
        let addr = usize::try_from(pc).ok()? as *const c_void;
        // SAFETY: `dladdr` reads only the loader's tables for `addr`, which it does not
        // dereference, and fills the live `info`.
        if unsafe { dladdr(addr, &raw mut info) } == 0 || info.sname.is_null() {
            return None;
        }
        // SAFETY: a non-null `dli_sname` is a NUL-terminated string the loader owns for as long
        // as the image stays loaded; it is copied out at once.
        Some(
            unsafe { CStr::from_ptr(info.sname) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    pub(super) fn asleep(thread: Thread) -> io::Result<Option<Asleep>> {
        let port = thread.port;
        if !sleeping(port)? {
            return Ok(None);
        }
        // SAFETY: `port` names a thread of this task; it is resumed below on the one path that
        // suspended it, before anything that could wait on a lock it holds.
        let kr = unsafe { thread_suspend(port) };
        if kr != 0 {
            return Err(kern("thread_suspend", kr));
        }
        // SAFETY: suspended just above.
        let read = unsafe { pc_arg(port) };
        // SAFETY: balances the suspend above.
        let kr = unsafe { thread_resume(port) };
        if kr != 0 {
            return Err(kern("thread_resume", kr));
        }
        let (pc, arg) = read.map_err(|kr| kern("thread_get_state", kr))?;
        if !sleeping(port)? {
            return Ok(None);
        }
        Ok(Some(match symbol(pc) {
            Some(name) if name == "flock" => Asleep::Flock {
                fd: super::low_c_int(arg),
            },
            Some(name) => Asleep::Elsewhere(name),
            None => Asleep::Elsewhere(format!("pc {pc:#x}")),
        }))
    }
}

/// Truncates a 64-bit syscall argument register to its low 32-bit signed C `int` descriptor.
pub(crate) fn low_c_int(reg: u64) -> i32 {
    let low = u32::try_from(reg & 0xffff_ffff).unwrap_or(u32::MAX);
    i32::from_ne_bytes(low.to_ne_bytes())
}

#[cfg(any(target_os = "linux", test))]
mod proc_syscall {
    use super::{Asleep, low_c_int};

    /// `flock(2)`'s number in `/proc/<pid>/task/<tid>/syscall`, which the kernel formats in its
    /// own architecture's syscall table even when this process runs under a binary translator
    /// (`x86_64` under Rosetta on an `aarch64` kernel reports `32`, not `73`).
    pub(super) const fn sys_flock_for_kernel_arch(arch: &str) -> Option<i64> {
        match arch.trim_ascii().as_bytes() {
            b"x86_64" => Some(73),
            b"aarch64" => Some(32),
            _ => None,
        }
    }

    pub(super) fn parse_with(line: &str, flock_nr: i64) -> Option<Asleep> {
        let mut words = line.split_whitespace();
        let first = words.next()?;
        if first == "running" {
            return None;
        }
        Some(match first.parse::<i64>() {
            Ok(n) if n == flock_nr => Asleep::Flock {
                fd: words
                    .next()
                    .and_then(|a| a.strip_prefix("0x"))
                    .and_then(|a| u64::from_str_radix(a, 16).ok())
                    .map_or(-1, low_c_int),
            },
            Ok(-1) => Asleep::Elsewhere("no call".to_string()),
            Ok(n) => Asleep::Elsewhere(format!("syscall {n}")),
            Err(_) => Asleep::Elsewhere(first.to_string()),
        })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io;
    use std::sync::OnceLock;

    use super::proc_syscall::{parse_with, sys_flock_for_kernel_arch};
    use super::{Asleep, Thread};

    #[cfg(target_arch = "x86_64")]
    const TARGET_SYS_FLOCK: i64 = 73;
    #[cfg(target_arch = "aarch64")]
    const TARGET_SYS_FLOCK: i64 = 32;

    pub(super) fn sys_flock() -> i64 {
        static NR: OnceLock<i64> = OnceLock::new();
        *NR.get_or_init(|| {
            std::fs::read_to_string("/proc/sys/kernel/arch")
                .ok()
                .and_then(|arch| sys_flock_for_kernel_arch(&arch))
                .unwrap_or(TARGET_SYS_FLOCK)
        })
    }

    pub(super) fn this_thread() -> Thread {
        // SAFETY: `gettid` takes no argument and cannot fail.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        Thread {
            tid: i64::from(tid),
        }
    }

    pub(super) fn threads() -> io::Result<Vec<Thread>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/proc/self/task")? {
            if let Some(tid) = entry?.file_name().to_str().and_then(|s| s.parse().ok()) {
                found.push(Thread { tid });
            }
        }
        Ok(found)
    }

    /// What `/proc/<pid>/task/<tid>/syscall` says: `running`, `-1 …` (asleep in no call), or
    /// the call's number and its arguments in hex.
    pub(super) fn parse(line: &str) -> Option<Asleep> {
        parse_with(line, sys_flock())
    }

    pub(super) fn asleep(thread: Thread) -> io::Result<Option<Asleep>> {
        let line = std::fs::read_to_string(format!("/proc/self/task/{}/syscall", thread.tid))?;
        Ok(parse(&line))
    }
}

#[cfg(test)]
mod tests {
    use super::{Asleep, asleep, queued_for, this_thread, threads};
    use std::sync::mpsc;
    use std::time::Duration;

    /// The one-minute hang detector on every wait here that must end.
    const HANG: Duration = Duration::from_secs(60);

    /// Waits, up to [`HANG`], until `probe` answers, and returns that.
    fn until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
        let begun = std::time::Instant::now();
        while begun.elapsed() < HANG {
            if let Some(found) = probe() {
                return found;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("{what}: not within {HANG:?}");
    }

    /// **A THREAD QUEUED FOR A HELD FILE LOCK IS ASLEEP IN `flock(2)` ON THAT FILE; ONE PARKED
    /// ON A CHANNEL IS ASLEEP ELSEWHERE.** The waiter reads `Flock` on its own descriptor, and
    /// [`queued_for`] finds it, and only it, by the file's path. The parked thread is the
    /// control: the reading tells the kernel's lock queue from any other sleep, so it is no
    /// "asleep at all" in disguise, and the parked thread is queued for nothing.
    #[test]
    fn a_thread_queued_for_a_held_lock_sleeps_in_flock_on_it_and_a_parked_one_elsewhere() {
        use std::os::fd::AsRawFd as _;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join("lock");
        let held = std::fs::File::create(&path).expect("the holder's file");
        #[expect(
            clippy::disallowed_methods,
            reason = "a test's own hold, released by its explicit unlock below"
        )]
        held.lock().expect("the holder takes the lock");

        let (named, name) = mpsc::channel();
        let queued_path = path.clone();
        let queued = std::thread::spawn(move || {
            let file = std::fs::File::open(&queued_path).expect("the waiter's file");
            named
                .send((this_thread(), file.as_raw_fd()))
                .expect("name the waiter");
            #[expect(
                clippy::disallowed_methods,
                reason = "a test's own take, released by its explicit unlock below"
            )]
            file.lock().expect("the waiter takes the lock");
            file.unlock().expect("the waiter lets go");
        });
        let (waiter, fd) = name.recv_timeout(HANG).expect("the waiter's name");
        until("the waiter asleep in flock(2) on its descriptor", || {
            (asleep(waiter).expect("read the waiter") == Some(Asleep::Flock { fd })).then_some(())
        });
        assert!(
            threads().expect("this process's threads").contains(&waiter),
            "the listing holds the waiter"
        );
        assert_eq!(
            queued_for(&path).expect("the threads queued for the lock"),
            vec![waiter],
            "the waiter, and only it, is queued for the held file"
        );

        let (named, name) = mpsc::channel();
        let (wake, woken) = mpsc::channel::<()>();
        let parked = std::thread::spawn(move || {
            named.send(this_thread()).expect("name the parked thread");
            let _ = woken.recv();
        });
        let sleeper = name.recv_timeout(HANG).expect("the parked thread's name");
        let at = until("the parked thread asleep", || {
            asleep(sleeper).expect("read the parked thread")
        });
        assert!(
            matches!(at, Asleep::Elsewhere(_)),
            "CONTROL: a thread parked on a channel read {at:?}"
        );
        assert_eq!(
            queued_for(&path).expect("the threads queued for the lock"),
            vec![waiter],
            "CONTROL: the parked thread is queued for nothing"
        );
        wake.send(()).expect("wake the parked thread");
        parked.join().expect("the parked thread ends");

        held.unlock().expect("the holder lets go");
        queued.join().expect("the waiter took the lock and let go");
        assert_eq!(
            queued_for(&path).expect("the threads queued for the lock"),
            vec![],
            "none is queued once the lock is free"
        );
    }

    /// Linux's reading of `/proc/<pid>/task/<tid>/syscall`'s shapes.
    #[test]
    fn the_proc_syscall_line_names_flock_and_its_descriptor_another_call_or_none() {
        use super::low_c_int;
        use super::proc_syscall::{parse_with, sys_flock_for_kernel_arch};
        assert_eq!(parse_with("running\n", 73), None);
        assert_eq!(
            parse_with("-1 0x7ffc 0x401000\n", 73),
            Some(Asleep::Elsewhere("no call".into()))
        );
        assert_eq!(sys_flock_for_kernel_arch("x86_64\n"), Some(73));
        assert_eq!(sys_flock_for_kernel_arch("aarch64\n"), Some(32));
        assert_eq!(sys_flock_for_kernel_arch("unknown\n"), None);
        assert_eq!(low_c_int(0x1_0000_001a), 26);
        assert_eq!(low_c_int(0x8000_0000), i32::MIN);
        assert_eq!(low_c_int(0xffff_ffff_ffff_fffe), -2);
        for (flock_nr, other) in [(73, 32), (32, 73)] {
            assert_eq!(
                parse_with(
                    &format!("{flock_nr} 0x1a 0x2 0x0 0x0 0x0 0x0 0x7ffc 0x401000\n"),
                    flock_nr
                ),
                Some(Asleep::Flock { fd: 26 })
            );
            assert_eq!(
                parse_with(
                    &format!("{flock_nr} 0x10000001a 0x2 0x0 0x0 0x0 0x0 0x7ffc 0x401000\n"),
                    flock_nr
                ),
                Some(Asleep::Flock { fd: 26 }),
                "upper 32-bit register noise truncated to C int descriptor"
            );
            assert_eq!(
                parse_with(
                    &format!("{flock_nr} 0xfffffffffffffffe 0x2 0x0 0x0 0x0 0x0 0x7ffc 0x401000\n"),
                    flock_nr
                ),
                Some(Asleep::Flock { fd: -2 }),
                "sign-extended 64-bit negative descriptor preserved"
            );
            assert_eq!(
                parse_with(&format!("{other} 0x1a 0x2\n"), flock_nr),
                Some(Asleep::Elsewhere(format!("syscall {other}")))
            );
        }
        #[cfg(target_os = "linux")]
        {
            use super::imp::{parse, sys_flock};
            let live = sys_flock();
            let other = if live == 32 { 98 } else { 202 };
            assert_eq!(
                parse(&format!(
                    "{live} 0x1a 0x2 0x0 0x0 0x0 0x0 0x7ffc 0x401000\n"
                )),
                Some(Asleep::Flock { fd: 26 })
            );
            assert_eq!(
                parse(&format!("{other} 0x1 0x2\n")),
                Some(Asleep::Elsewhere(format!("syscall {other}")))
            );
        }
    }
}
