// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux's spawn ([`super`]): it copies nothing of this process, no handler of this process's
//! runs in its child, and its child neither allocates nor outgrows its stack.

use super::{Seam, sharing_or_could_not_run, with_child};
use crate::spawnfd::tests::{STAGE_ENV, native_shared_child_applicable, run_staged};
use crate::spawnfd::{LaneCommand, Tether as _, command, spawn, transparent};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    /// This process's pid, set across each spawn on the thread that makes it, which a child
    /// sharing this process's memory starts on and reads: an allocation by another pid that reads
    /// it set is the child's. This thread's own allocations while its child runs pass.
    static ARMED: Cell<i32> = const { Cell::new(0) };
    /// Paint each stack this thread's spawns map, and record how deep the child reached.
    static PAINT: Cell<Option<usize>> = const { Cell::new(None) };
}

/// The test binary's allocator: `System`'s, but one that allocates in a spawn's child (with
/// [`ARMED`] set to its parent's pid) ends that child with status 99 before the allocation.
struct Tripwire;

/// The status a child that allocated ends with.
const ALLOCATED: i32 = 99;

// SAFETY: `System`'s, but for the exit, which returns nothing.
unsafe impl GlobalAlloc for Tripwire {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        tripped();
        // SAFETY: the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        tripped();
        // SAFETY: the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Tripwire = Tripwire;

fn tripped() {
    let parent = ARMED.with(Cell::get);
    // SAFETY: `getpid` takes nothing and cannot fail.
    if parent != 0 && unsafe { libc::getpid() } != parent {
        // SAFETY: ends this child alone, writing nothing of this process's.
        unsafe { libc::_exit(ALLOCATED) }
    }
}

/// Armed until dropped ([`ARMED`]).
pub(super) struct Armed;

impl Drop for Armed {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.set(0));
    }
}

/// Arm the tripwire on this thread from one clone until its child is done ([`super::launch`]).
pub(super) fn arm_tripwire() -> Armed {
    // SAFETY: `getpid` takes nothing and cannot fail.
    let pid = unsafe { libc::getpid() };
    ARMED.with(|armed| armed.set(pid));
    Armed
}

/// `fd` put at number `at` (close-on-exec), whatever was there closed.
pub(super) fn moved_onto(fd: std::os::fd::OwnedFd, at: std::os::fd::RawFd) -> std::os::fd::OwnedFd {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    // SAFETY: two numbers; `at` is the test's to take.
    let moved = unsafe { libc::dup3(fd.as_raw_fd(), at, libc::O_CLOEXEC) };
    assert_eq!(
        moved,
        at,
        "dup3 onto {at}: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: the number dup3 just made, owned here alone.
    unsafe { std::os::fd::OwnedFd::from_raw_fd(moved) }
}

/// The byte a painted stack starts filled with.
const PAINT_BYTE: u8 = 0xA5;

/// Fill `stack` with [`PAINT_BYTE`] when this thread asked for its spawns' stacks painted.
pub(super) fn paint(stack: &mut [u8]) {
    if PAINT.with(Cell::get).is_some() {
        stack.fill(PAINT_BYTE);
    }
}

/// Record how much of a painted `stack` its child wrote, the deepest so far.
pub(super) fn measure(stack: &[u8]) {
    PAINT.with(|deepest| {
        if let Some(was) = deepest.get() {
            let untouched = stack.iter().take_while(|&&b| b == PAINT_BYTE).count();
            deepest.set(Some(was.max(stack.len() - untouched)));
        }
    });
}

#[repr(C)]
struct Rusage {
    fields: [i64; 18],
}

/// This thread's minor page faults so far (`RUSAGE_THREAD`).
fn minor_faults() -> i64 {
    const RUSAGE_THREAD: i32 = 1;
    const MINFLT_AT: usize = 4 + 4;
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    let mut usage = Rusage { fields: [0; 18] };
    // SAFETY: a live, writable `struct rusage` (two timevals, then fourteen longs).
    assert_eq!(unsafe { getrusage(RUSAGE_THREAD, &mut usage) }, 0);
    usage.fields[MINFLT_AT]
}

/// A spawn copies nothing of this process: after one, writing each page this process already
/// wrote faults on none of them. A `fork` marks every private page copy-on-write, so the first
/// write to each after it faults (the control, the same spawn's child run in a copy, faults on
/// at least half). Run staged, so no other test's fork marks the pages meanwhile.
#[test]
fn a_linux_spawn_leaves_its_parents_pages_writable() {
    const NAME: &str =
        "spawnfd::clone_spawn::tests::a_linux_spawn_leaves_its_parents_pages_writable";
    if !native_shared_child_applicable(NAME) {
        return;
    }
    if std::env::var(STAGE_ENV).as_deref() != Ok("pages") {
        let out = run_staged(NAME, "pages");
        assert!(out.contains("\nSTAGED pages\n"), "{out}");
        return;
    }
    sharing_or_could_not_run();
    // SAFETY: `sysconf` reads one value.
    let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).expect("a page");
    let pages = 4 * 1024 * 1024 / page;
    let len = pages * page;
    // SAFETY: a fresh anonymous private mapping this test alone uses, unmapped below.
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(base, libc::MAP_FAILED, "mmap");
    // SAFETY: the mapping above; a huge page would make one fault of 512 pages.
    unsafe { libc::madvise(base, len, libc::MADV_NOHUGEPAGE) };
    let base = base.cast::<u8>();
    let rewrite = || {
        let before = minor_faults();
        for at in 0..pages {
            // SAFETY: inside the mapping.
            unsafe { base.add(at * page).write_volatile(1) };
        }
        minor_faults() - before
    };
    rewrite();
    // SAFETY: the mapping, kept resident so reclaim faults on none of it; best effort.
    let locked = unsafe { libc::mlock(base.cast(), len) } == 0;
    let quiet = rewrite();
    let arms: [(&str, &dyn Fn() -> std::process::ExitStatus); 4] = [
        ("command", &|| command("/bin/true").status().expect("spawn")),
        ("tethered command", &|| {
            command("/bin/true").tether().status().expect("spawn")
        }),
        ("lane", &|| {
            spawn(LaneCommand::new("/bin/true"))
                .expect("spawn")
                .wait()
                .expect("wait")
        }),
        ("transparent", &|| {
            transparent("/bin/true").status().expect("spawn")
        }),
    ];
    let bound = quiet + i64::try_from(pages / 16).expect("pages");
    for (arm, start) in arms {
        assert!(start().success(), "{arm}");
        let faults = rewrite();
        println!("{arm}: {faults} faults rewriting {pages} pages after a spawn (quiet {quiet})");
        assert!(
            faults <= bound,
            "{arm}: {faults} of {pages} pages faulted after a spawn"
        );
    }
    let copied = with_child(true, Seam::None, || {
        command("/bin/true").status().expect("spawn")
    });
    assert!(copied.success());
    let faults = rewrite();
    println!("copy: {faults} faults rewriting {pages} pages (locked {locked})");
    assert!(
        faults >= i64::try_from(pages / 2).expect("pages"),
        "the control: a child run in a copy left {faults} of {pages} pages to fault"
    );
    // SAFETY: the mapping above, which nothing uses now.
    unsafe { libc::munmap(base.cast(), len) };
    println!("\nSTAGED pages");
}

static CAUGHT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn caught(_signal: libc::c_int) {
    CAUGHT.fetch_add(1, Ordering::SeqCst);
}

/// A signal at a spawn's child before its exec never runs a handler of this process's there,
/// where the handler would run on this process's memory: the child set it back to its default
/// action first, so the signal ends the child. The control keeps the handlers, and the handler
/// runs in the child, counting in this process's memory. Run staged, as it installs a handler.
#[test]
fn a_handler_of_this_process_never_runs_in_a_child_before_its_exec() {
    const NAME: &str = "spawnfd::clone_spawn::tests::a_handler_of_this_process_never_runs_in_a_child_before_its_exec";
    if !native_shared_child_applicable(NAME) {
        return;
    }
    if std::env::var(STAGE_ENV).as_deref() != Ok("handler") {
        let out = run_staged(NAME, "handler");
        assert!(out.contains("\nSTAGED handler\n"), "{out}");
        return;
    }
    use std::os::unix::process::ExitStatusExt as _;
    sharing_or_could_not_run();
    // SAFETY: zero is a valid `sigaction`; this staged process alone installs the handler.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = caught as extern "C" fn(libc::c_int) as usize;
        assert_eq!(
            libc::sigaction(libc::SIGUSR2, &action, std::ptr::null_mut()),
            0
        );
    }
    let ended = with_child(false, Seam::Raise(libc::SIGUSR2), || {
        command("/bin/true").status().expect("spawn")
    });
    assert_eq!(ended.signal(), Some(libc::SIGUSR2), "{ended:?}");
    assert_eq!(
        CAUGHT.load(Ordering::SeqCst),
        0,
        "a handler ran in the child"
    );
    let kept = with_child(false, Seam::RaiseKeepingHandlers(libc::SIGUSR2), || {
        command("/bin/true").status().expect("spawn")
    });
    assert!(kept.success(), "the control's child: {kept:?}");
    assert_eq!(
        CAUGHT.load(Ordering::SeqCst),
        1,
        "the control: the kept handler ran in the child"
    );
    println!("\nSTAGED handler");
}

/// A spawn's child allocates nothing (an allocation there runs this process's allocator on its
/// memory while its other threads run on): every spawn of this crate's tests runs with the
/// tripwire armed, and the control, a child that allocates, trips it.
#[test]
fn a_childs_allocation_trips_the_test_allocator() {
    let allocated = with_child(false, Seam::Allocate, || {
        command("/bin/true").status().expect("spawn")
    });
    assert_eq!(allocated.code(), Some(ALLOCATED), "{allocated:?}");
    let copied = with_child(true, Seam::Allocate, || {
        command("/bin/true").status().expect("spawn")
    });
    assert_eq!(copied.code(), Some(ALLOCATED), "{copied:?}");
    assert!(command("/bin/true").status().expect("spawn").success());
}

/// A spawn's child reaches less than a quarter of its stack on the deepest path a test can take
/// it down: every signal reset, the strip, three slots, a chdir, and a walk of three candidates
/// that ends in the shell over a file without a `#!`.
#[test]
fn a_childs_deepest_path_uses_a_quarter_of_its_stack() {
    const NAME: &str =
        "spawnfd::clone_spawn::tests::a_childs_deepest_path_uses_a_quarter_of_its_stack";
    if !native_shared_child_applicable(NAME) {
        return;
    }
    use std::os::unix::fs::PermissionsExt as _;
    sharing_or_could_not_run();
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let script = dir.path().join("plain");
    std::fs::write(&script, "exit 7\n").expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = format!("{0}/none:{0}/nor:{0}", dir.path().display());
    PAINT.with(|deepest| deepest.set(Some(0)));
    let status = command("plain")
        .env("PATH", &path)
        .current_dir(dir.path())
        .default_signals()
        .stdout(crate::spawnfd::Stdio::null())
        .status()
        .expect("spawn");
    let deepest = PAINT
        .with(|deepest| deepest.replace(None))
        .expect("painted");
    assert_eq!(status.code(), Some(7), "the shell ran the plain file");
    println!(
        "the child reached {deepest} of {} stack bytes",
        super::STACK
    );
    assert!(deepest > 0, "the stack was painted and measured");
    assert!(
        deepest < super::STACK / 4,
        "the child reached {deepest} of {} stack bytes",
        super::STACK
    );
}

/// The strip's listing, alone (the strip tries it where `close_range` is refused, before Linux
/// 5.11): it opens `/proc/self/fd`, reads it to its end and marks a descriptor this process holds
/// without `FD_CLOEXEC`. Run staged, since it marks every descriptor of its process.
#[test]
fn the_strips_listing_marks_each_descriptor_it_reads() {
    use std::os::fd::AsRawFd as _;
    const NAME: &str =
        "spawnfd::clone_spawn::tests::the_strips_listing_marks_each_descriptor_it_reads";
    if std::env::var(STAGE_ENV).as_deref() != Ok("listing") {
        let out = run_staged(NAME, "listing");
        assert!(
            out.contains("\nSTAGED listing complete=true marked=true\n"),
            "{out}"
        );
        return;
    }
    let held = std::fs::File::open("/dev/null").expect("open /dev/null");
    let fd = held.as_raw_fd();
    // SAFETY: integer arguments on a descriptor `held` owns.
    let cleared = unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
    assert_eq!(cleared, 0, "clear FD_CLOEXEC");
    let complete = super::child::proc_self_fd();
    // SAFETY: integer arguments on a descriptor `held` owns.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    let marked = flags >= 0 && flags & libc::FD_CLOEXEC != 0;
    println!("\nSTAGED listing complete={complete} marked={marked}");
}

/// The thread that spawns gets its own signal mask back, though the clone blocked every signal
/// across it.
#[test]
fn a_spawn_gives_its_thread_back_its_signal_mask() {
    let mask = || {
        let mut now = 0_u64;
        // SAFETY: a null set reads this thread's mask into a live 64-bit set.
        assert_eq!(
            unsafe { super::child::rt_sigprocmask(0, std::ptr::null(), &mut now) },
            Ok(0)
        );
        now
    };
    // SAFETY: an empty-initialized set with one signal, blocked on this test's thread alone.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    let before = mask();
    assert_ne!(before & (1 << (libc::SIGUSR1 - 1)), 0, "SIGUSR1 blocked");
    assert!(command("/bin/true").status().expect("spawn").success());
    assert!(transparent("/bin/true").status().expect("spawn").success());
    assert_eq!(mask(), before, "the spawning thread's mask");
    // SAFETY: as above, unblocked again.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}
