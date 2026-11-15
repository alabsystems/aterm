// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A SPAWN COSTS A PROCESS SHAPED LIKE THE WINDOW.

#[cfg(target_vendor = "apple")]
mod bench {
    use aterm_uds::spawnfd::{InheritNothing as _, command};
    use std::time::{Duration, Instant};

    /// The window's shape, measured: 6 585 VM regions and about 400 MB
    /// resident. The bench maps `CHUNKS` chunks of `CHUNK` bytes and makes
    /// every other one inaccessible, so each is a region of its own; the
    /// writable half is touched resident.
    const CHUNK: usize = 128 * 1024;
    const CHUNKS: usize = 6_400;
    /// Spawns timed per lane, and rewrites timed after a spawn.
    const SPAWNS: usize = 40;
    const REWRITES: usize = 9;

    /// How many VM regions this process has (`PROC_PIDREGIONINFO`, walked).
    fn region_count() -> usize {
        unsafe extern "C" {
            fn proc_pidinfo(
                pid: i32,
                flavor: i32,
                arg: u64,
                buffer: *mut core::ffi::c_void,
                size: i32,
            ) -> i32;
            fn getpid() -> i32;
        }
        const PROC_PIDREGIONINFO: i32 = 7;
        let mut info = [0u8; 96];
        let (mut at, mut count) = (0u64, 0usize);
        loop {
            // SAFETY: `info` is 96 writable bytes, the size passed.
            let got = unsafe {
                proc_pidinfo(
                    getpid(),
                    PROC_PIDREGIONINFO,
                    at,
                    info.as_mut_ptr().cast(),
                    96,
                )
            };
            if got != 96 {
                return count;
            }
            let word = |o: usize| u64::from_ne_bytes(info[o..o + 8].try_into().expect("8 bytes"));
            count += 1;
            at = word(80) + word(88);
        }
    }

    /// The p50 and p90 of `samples`, in milliseconds.
    fn percentiles(mut samples: Vec<Duration>) -> (f64, f64) {
        samples.sort_unstable();
        let at = |q: usize| samples[(samples.len() - 1) * q / 100].as_secs_f64() * 1e3;
        (at(50), at(90))
    }

    pub fn run() {
        unsafe extern "C" {
            fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
            -> *mut u8;
            fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
            fn getpagesize() -> i32;
        }
        const PROT_NONE: i32 = 0;
        const PROT_READ_WRITE: i32 = 1 | 2;
        const MAP_PRIVATE_ANON: i32 = 0x0002 | 0x1000;
        let len = CHUNK * CHUNKS;
        // SAFETY: an anonymous private mapping this function alone uses and never unmaps (the
        // process exits with it).
        let base = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ_WRITE,
                MAP_PRIVATE_ANON,
                -1,
                0,
            )
        };
        assert!(!base.is_null() && base as isize != -1, "mmap");
        // SAFETY: `getpagesize` takes nothing and cannot fail.
        let page = usize::try_from(unsafe { getpagesize() }).expect("a page size");
        let writable: Vec<*mut u8> = (0..CHUNKS)
            .step_by(2)
            // SAFETY: every chunk start lies inside the mapping.
            .map(|i| unsafe { base.add(i * CHUNK) })
            .collect();
        for i in (1..CHUNKS).step_by(2) {
            // SAFETY: a chunk of the mapping; made inaccessible, splitting it off.
            assert_eq!(
                unsafe { mprotect(base.add(i * CHUNK), CHUNK, PROT_NONE) },
                0,
                "mprotect"
            );
        }
        let rewrite = || {
            let started = Instant::now();
            for &chunk in &writable {
                for offset in (0..CHUNK).step_by(page) {
                    // SAFETY: inside a writable chunk of the mapping.
                    unsafe { chunk.add(offset).write_volatile(1) };
                }
            }
            started.elapsed()
        };
        rewrite();
        let regions = region_count();
        let resident_mb = writable.len() * CHUNK / (1024 * 1024);
        println!("regions={regions} resident={resident_mb}MB page={page}");
        let baseline = percentiles((0..REWRITES).map(|_| rewrite()).collect());
        println!(
            "rewrite with no spawn: p50 {:.2} ms p90 {:.2} ms",
            baseline.0, baseline.1
        );
        type Spawn = Box<dyn Fn() -> Box<dyn FnOnce()>>;
        let lanes: [(&str, Spawn); 3] = [
            (
                "std Command, no hook (posix_spawn baseline)",
                Box::new(|| {
                    #[expect(
                        clippy::disallowed_types,
                        reason = "the measured baseline: std's own spawn"
                    )]
                    let mut child = std::process::Command::new("/usr/bin/true")
                        .spawn()
                        .expect("spawn");
                    Box::new(move || {
                        child.wait().expect("wait");
                    })
                }),
            ),
            (
                "std Command + InheritNothing (fork + strip)",
                Box::new(|| {
                    #[expect(
                        clippy::disallowed_types,
                        reason = "the measured lane: std's fork with the strip installed"
                    )]
                    let mut child = std::process::Command::new("/usr/bin/true")
                        .inherit_nothing()
                        .spawn()
                        .expect("spawn");
                    Box::new(move || {
                        child.wait().expect("wait");
                    })
                }),
            ),
            (
                "spawnfd::command (posix_spawn + CLOEXEC_DEFAULT)",
                Box::new(|| {
                    let mut child = command("/usr/bin/true").spawn().expect("spawn");
                    Box::new(move || {
                        child.wait().expect("wait");
                    })
                }),
            ),
        ];
        for (name, spawn) in &lanes {
            let spawns = percentiles(
                (0..SPAWNS)
                    .map(|_| {
                        let started = Instant::now();
                        let reap = spawn();
                        let took = started.elapsed();
                        reap();
                        took
                    })
                    .collect(),
            );
            let bills = percentiles(
                (0..REWRITES)
                    .map(|_| {
                        spawn()();
                        rewrite()
                    })
                    .collect(),
            );
            println!(
                "{name}: spawn() p50 {:.3} ms p90 {:.3} ms; \
                 rewrite after it p50 {:.2} ms p90 {:.2} ms",
                spawns.0, spawns.1, bills.0, bills.1
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod bench {
    use aterm_uds::spawnfd::terminal::{self, Start, Started};
    use aterm_uds::spawnfd::{InheritNothing as _, Tether as _, command};
    use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    /// The resident sizes measured: a small process, the atpkg lib's test process with its
    /// suite loaded (1.3 GB) and the window on this host (2.4 GB). A size the machine cannot
    /// hold twice over is named and passed over.
    const SIZES_MB: [usize; 3] = [100, 1_300, 2_400];
    /// Each mapping's size: the resident memory is many mappings, as glibc's arenas are.
    const MAPPING: usize = 64 * 1024 * 1024;
    /// Spawns timed per lane and size, and rewrites timed after a spawn.
    const SPAWNS: usize = 40;
    const REWRITES: usize = 5;
    /// How long the sibling thread sleeps between its page touches.
    const TICK: Duration = Duration::from_millis(1);
    /// A sibling gap longer than this is a frame dropped at 60 Hz.
    const FRAME: Duration = Duration::from_millis(16);

    /// A `kB` field of a `/proc` file, in bytes.
    fn kb_field(file: &str, field: &str) -> usize {
        let text = std::fs::read_to_string(file).expect(file);
        let kb: usize = text
            .lines()
            .find_map(|l| l.strip_prefix(field))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
            .expect(field);
        kb * 1024
    }

    /// `bytes` of anonymous memory in [`MAPPING`]-sized mappings with no huge pages, every
    /// page written: what a process that size holds resident. Unmapped on drop.
    struct Resident {
        maps: Vec<(*mut u8, usize)>,
        page: usize,
    }

    impl Resident {
        fn new(bytes: usize) -> Self {
            // SAFETY: `sysconf` reads one value.
            let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).expect("page");
            let mut maps = Vec::new();
            let mut left = bytes;
            while left > 0 {
                let len = left.min(MAPPING);
                // SAFETY: a fresh anonymous private mapping this bench alone uses.
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
                // SAFETY: the mapping above, whole.
                assert_eq!(
                    unsafe { libc::madvise(base, len, libc::MADV_NOHUGEPAGE) },
                    0
                );
                maps.push((base.cast::<u8>(), len));
                left -= len;
            }
            let held = Self { maps, page };
            held.rewrite();
            held
        }

        /// Write one byte of every page; how long that took.
        fn rewrite(&self) -> Duration {
            let started = Instant::now();
            for &(base, len) in &self.maps {
                for at in (0..len).step_by(self.page) {
                    // SAFETY: inside a writable mapping `new` made.
                    unsafe { base.add(at).write_volatile(1) };
                }
            }
            started.elapsed()
        }
    }

    impl Drop for Resident {
        fn drop(&mut self) {
            for &(base, len) in &self.maps {
                // SAFETY: a mapping `new` made, which nothing uses now.
                unsafe { libc::munmap(base.cast(), len) };
            }
        }
    }

    /// A thread that maps, writes and unmaps a page every [`TICK`], as a window's render and
    /// I/O threads touch memory, keeping its longest gap and its gaps over a [`FRAME`].
    struct Sibling {
        stop: Arc<AtomicBool>,
        worst_us: Arc<AtomicU64>,
        dropped: Arc<AtomicU64>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Sibling {
        fn start() -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let worst_us = Arc::new(AtomicU64::new(0));
            let dropped = Arc::new(AtomicU64::new(0));
            let (s, w, d) = (
                Arc::clone(&stop),
                Arc::clone(&worst_us),
                Arc::clone(&dropped),
            );
            let thread = std::thread::spawn(move || {
                let mut last = Instant::now();
                while !s.load(Ordering::Relaxed) {
                    // SAFETY: a fresh one-page mapping, written once and unmapped.
                    unsafe {
                        let page = libc::mmap(
                            std::ptr::null_mut(),
                            4096,
                            libc::PROT_READ | libc::PROT_WRITE,
                            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                            -1,
                            0,
                        );
                        if page != libc::MAP_FAILED {
                            page.cast::<u8>().write_volatile(1);
                            libc::munmap(page, 4096);
                        }
                    }
                    let now = Instant::now();
                    let gap = now - last;
                    last = now;
                    w.fetch_max(
                        u64::try_from(gap.as_micros()).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                    if gap > FRAME {
                        d.fetch_add(1, Ordering::Relaxed);
                    }
                    std::thread::sleep(TICK);
                }
            });
            Self {
                stop,
                worst_us,
                dropped,
                thread: Some(thread),
            }
        }

        /// Forget the gaps so far; the thread runs on.
        fn reset(&self) {
            self.worst_us.store(0, Ordering::Relaxed);
            self.dropped.store(0, Ordering::Relaxed);
        }

        /// The longest gap in ms, and the gaps over a frame, since [`Self::reset`].
        fn read(&self) -> (f64, u64) {
            (
                self.worst_us.load(Ordering::Relaxed) as f64 / 1e3,
                self.dropped.load(Ordering::Relaxed),
            )
        }
    }

    impl Drop for Sibling {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The p50, p95 and p99 of `samples`, in milliseconds.
    fn percentiles(mut samples: Vec<Duration>) -> [f64; 3] {
        samples.sort_unstable();
        let at = |q: usize| samples[(samples.len() - 1) * q / 100].as_secs_f64() * 1e3;
        [at(50), at(95), at(99)]
    }

    /// A fresh pty's master and slave, each close-on-exec.
    fn pty() -> (OwnedFd, OwnedFd) {
        // SAFETY: `posix_openpt`, `grantpt` and `unlockpt` take flags and the master's number;
        // `name` is a live buffer `ptsname_r` fills, and `open` reads the path it holds.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
            assert!(master >= 0, "posix_openpt");
            let master = OwnedFd::from_raw_fd(master);
            assert_eq!(libc::grantpt(master.as_raw_fd()), 0);
            assert_eq!(libc::unlockpt(master.as_raw_fd()), 0);
            let mut name = [0_u8; 64];
            assert_eq!(
                libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr().cast(), name.len()),
                0
            );
            let slave = libc::open(
                name.as_ptr().cast(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            );
            assert!(slave >= 0, "open the slave");
            (master, OwnedFd::from_raw_fd(slave))
        }
    }

    pub fn run() {
        // A spawn, and its reap, which answers the child's `maxrss` where the lane reads one.
        type Spawn = Box<dyn Fn() -> Box<dyn FnOnce() -> Option<u64>>>;
        let lanes: [(&str, Spawn); 5] = [
            (
                "spawnfd::command",
                Box::new(|| {
                    let mut child = command("/bin/true").spawn().expect("spawn");
                    Box::new(move || {
                        let usage = child.wait_with_rusage().expect("wait");
                        assert!(usage.status.success());
                        Some(usage.maxrss)
                    })
                }),
            ),
            (
                "spawnfd::command, tethered",
                Box::new(|| {
                    let mut child = command("/bin/true").tether().spawn().expect("spawn");
                    Box::new(move || {
                        let usage = child.wait_with_rusage().expect("wait");
                        assert!(usage.status.success());
                        Some(usage.maxrss)
                    })
                }),
            ),
            (
                "terminal::start, a tab's shell's (its pty opened in the lane)",
                Box::new(|| {
                    let (master, slave) = pty();
                    let started = terminal::start(
                        &Start {
                            program: c"/bin/true",
                            argv: &[c"true".to_owned()],
                            envp: &[],
                            cwd: None,
                            limits: &[],
                            pass: None,
                        },
                        slave.as_fd(),
                        Duration::from_secs(60),
                    )
                    .expect("start");
                    let Started::Running(pid) = started else {
                        panic!("a terminal start of /bin/true: {started:?}");
                    };
                    Box::new(move || {
                        let mut status = 0;
                        // SAFETY: this lane's own child; `status` is a live local.
                        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
                        drop((master, slave));
                        None
                    })
                }),
            ),
            (
                "std Command + InheritNothing (fork + strip)",
                Box::new(|| {
                    #[expect(
                        clippy::disallowed_types,
                        reason = "the measured lane: std's fork with the strip installed"
                    )]
                    let mut child = std::process::Command::new("/bin/true")
                        .inherit_nothing()
                        .spawn()
                        .expect("spawn");
                    Box::new(move || {
                        assert!(child.wait().expect("wait").success());
                        None
                    })
                }),
            ),
            (
                "std Command, no hook (glibc posix_spawn floor)",
                Box::new(|| {
                    #[expect(
                        clippy::disallowed_types,
                        reason = "the measured floor: std's own spawn"
                    )]
                    let mut child = std::process::Command::new("/bin/true")
                        .spawn()
                        .expect("spawn");
                    Box::new(move || {
                        assert!(child.wait().expect("wait").success());
                        None
                    })
                }),
            ),
        ];
        let sibling = Sibling::start();
        for mb in SIZES_MB {
            let bytes = mb * 1024 * 1024;
            let available = kb_field("/proc/meminfo", "MemAvailable:");
            if available < 2 * bytes {
                println!(
                    "{mb} MB: not run, {} MB available",
                    available / (1024 * 1024)
                );
                continue;
            }
            let held = Resident::new(bytes);
            let rss = kb_field("/proc/self/status", "VmRSS:") / (1024 * 1024);
            let alone = percentiles((0..REWRITES).map(|_| held.rewrite()).collect());
            println!(
                "{mb} MB: VmRSS {rss} MB; rewrite with no spawn p50 {:.1} ms",
                alone[0]
            );
            for (name, spawn) in &lanes {
                sibling.reset();
                let mut maxrss = None;
                let spawns = percentiles(
                    (0..SPAWNS)
                        .map(|_| {
                            let started = Instant::now();
                            let reap = spawn();
                            let took = started.elapsed();
                            maxrss = maxrss.max(reap());
                            took
                        })
                        .collect(),
                );
                let (worst, dropped) = sibling.read();
                let bills = percentiles(
                    (0..REWRITES)
                        .map(|_| {
                            let _ = spawn()();
                            held.rewrite()
                        })
                        .collect(),
                );
                let maxrss = maxrss.map_or("unread".to_owned(), |b| format!("{} MB", b >> 20));
                println!(
                    "  {name}: spawn() n {SPAWNS} p50 {:.2} p95 {:.2} p99 {:.2} ms; \
                     rewrite after it p50 {:.1} ms; sibling's worst gap {worst:.1} ms, \
                     {dropped} over 16 ms; a child's maxrss {maxrss}",
                    spawns[0], spawns[1], spawns[2], bills[0]
                );
            }
            drop(held);
        }
    }
}

fn main() {
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    bench::run();
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    println!("spawn_cost measures macOS's region walk and Linux's fork; nothing to measure here");
}
