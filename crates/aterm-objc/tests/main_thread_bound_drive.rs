// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Measures [`aterm_objc::MainThreadBound`] on the real process main thread (`harness = false`).
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]
#![cfg_attr(
    target_os = "macos",
    expect(
        clippy::disallowed_methods,
        reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
    )
)]

const TEST_NAME: &str = "main_thread_bound_drive";

fn main() -> std::process::ExitCode {
    let code = match asked(std::env::args().skip(1)) {
        Ok(Asked::Run) => run(),
        Ok(Asked::List { selected }) => {
            if selected {
                println!("{TEST_NAME}: test");
            }
            PASS
        }
        Ok(Asked::Nothing) => PASS,
        Err(e) => {
            eprintln!("{TEST_NAME}: {e}");
            FAIL
        }
    };
    std::process::ExitCode::from(code as u8)
}

enum Asked {
    Run,
    /// `--list`: print the name when the rest of the argv selects it.
    List {
        selected: bool,
    },
    /// Filtered out, skipped, `--ignored`, `--bench` or `--help`.
    Nothing,
}

fn asked(args: impl IntoIterator<Item = String>) -> Result<Asked, String> {
    let (mut filters, mut skips) = (Vec::new(), Vec::new());
    let (mut list, mut exact, mut ignored, mut bench) = (false, false, false, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                println!("usage: {TEST_NAME} [FILTER...] [--exact] [--skip PAT] [--list]");
                return Ok(Asked::Nothing);
            }
            "--list" => list = true,
            "--exact" => exact = true,
            "--ignored" => ignored = true,
            "--bench" => bench = true,
            "--include-ignored"
            | "--test"
            | "--nocapture"
            | "--no-capture"
            | "--show-output"
            | "--quiet"
            | "-q"
            | "--report-time"
            | "--ensure-time"
            | "--shuffle"
            | "--force-run-in-process"
            | "--exclude-should-panic" => {}
            "--skip" | "--test-threads" | "--format" | "--logfile" | "--color"
            | "--shuffle-seed" | "-Z" => {
                let value = match inline {
                    Some(v) => v,
                    None => args.next().ok_or_else(|| format!("{flag} needs a value"))?,
                };
                if flag == "--skip" {
                    skips.push(value);
                }
            }
            _ if flag.starts_with("-Z") => {}
            _ if flag.starts_with('-') => return Err(format!("unrecognized option `{arg}`")),
            _ => filters.push(arg),
        }
    }
    let hit = |pat: &String| {
        if exact {
            pat == TEST_NAME
        } else {
            TEST_NAME.contains(pat.as_str())
        }
    };
    let selected = !ignored
        && !bench
        && (filters.is_empty() || filters.iter().any(hit))
        && !skips.iter().any(hit);
    Ok(if list {
        Asked::List { selected }
    } else if selected {
        Asked::Run
    } else {
        Asked::Nothing
    })
}

const PASS: i32 = 0;
const FAIL: i32 = 1;
#[cfg(target_os = "macos")]
const NOT_RUN: i32 = 2;

/// The env var that turns this binary into one of its own child probes.
#[cfg(target_os = "macos")]
const MODE: &str = "ATERM_OBJC_BOUND_DRIVE_MODE";

/// A hang detector, never a latency budget, for anything that must complete: a dispatched drop,
/// a `get_on_main`, the drop-less child. Each wait returns the moment it completes.
#[cfg(target_os = "macos")]
const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(not(target_os = "macos"))]
fn run() -> i32 {
    eprintln!("objc-bound-drive: not run — libdispatch's main queue is a macOS/Darwin subject.");
    PASS
}

#[cfg(target_os = "macos")]
fn run() -> i32 {
    match std::env::var(MODE).as_deref() {
        Ok("nodrop") => macos::child_drop_without_destructor(),
        _ => macos::parent(),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use aterm_objc::{
        Bool, Id, MainThread, MainThreadBound, Retained, Sel, class, declare_class, msg, sel,
    };

    use super::{FAIL, MODE, NOT_RUN, PASS, WATCHDOG};

    // SAFETY: CoreFoundation is a system framework, always present, and these are its
    // declarations. The main thread must service its queue or every dispatched drop blocks.
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
    }

    fn is_main_thread() -> bool {
        // SAFETY: `+[NSThread isMainThread]` is a side-effect-free class-method
        // `BOOL` query and the cast is exactly `-(BOOL)(id, SEL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = msg();
            f(class(c"NSThread").as_id(), sel!(isMainThread)).as_bool()
        }
    }

    /// 1 if the destructor ran on the main thread, 2 if it did not, 0 if it has
    /// not run.
    #[derive(Clone)]
    struct WhereDidIDie(std::sync::Arc<AtomicU32>);

    impl Drop for WhereDidIDie {
        fn drop(&mut self) {
            self.0
                .store(if is_main_thread() { 1 } else { 2 }, Ordering::SeqCst);
        }
    }

    struct DeallocWatch {
        _calls: Cell<i64>,
        _where_died: WhereDidIDie,
    }

    declare_class! {
        struct Watched: NSObject {
            const NAME: &str = "AtermBoundDriveWatched";
            type Ivars = DeallocWatch;

            @sel(ping)
            fn ping(&self) {
                let ivars = self.ivars();
                ivars._calls.set(ivars._calls.get() + 1);
            }
        }
    }

    fn pump_until<T>(rx: &mpsc::Receiver<T>) -> Option<T> {
        let deadline = Instant::now() + WATCHDOG;
        while Instant::now() < deadline {
            // SAFETY: the mode is CoreFoundation's own default-mode constant
            // and this is the main thread's run loop.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, 0) };
            if let Ok(v) = rx.try_recv() {
                return Some(v);
            }
        }
        None
    }

    fn where_it_died(flag: &std::sync::Arc<AtomicU32>) -> &'static str {
        match flag.load(Ordering::SeqCst) {
            0 => "not yet",
            1 => "the MAIN thread",
            _ => "a WORKER",
        }
    }

    /// A drop-less container dropped on a worker with the main thread parked, as in a libtest
    /// binary. The short-circuit means this never reaches the queue, so it must return at once.
    pub fn child_drop_without_destructor() -> i32 {
        assert!(is_main_thread(), "the probe must start on the main thread");
        // SAFETY: as above.
        let mt = MainThread::new().expect("the probe runs on the main thread");
        assert!(!std::mem::needs_drop::<[u8; 8]>());
        let bound = MainThreadBound::new([0_u8; 8], mt);
        let worker = std::thread::spawn(move || {
            drop(bound);
        });
        let _ = worker.join();
        println!("NODROP-CHILD-RETURNED");
        PASS
    }

    pub fn parent() -> i32 {
        let mut failures: Vec<String> = Vec::new();
        let mut check = |ok: bool, what: &str| {
            if ok {
                eprintln!("  ok   {what}");
            } else {
                eprintln!("  FAIL {what}");
                failures.push(what.to_owned());
            }
        };

        if !is_main_thread() {
            eprintln!("objc-bound-drive: NOT RUN — main() is not on the main thread.");
            return NOT_RUN;
        }
        let Some(mt) = MainThread::new() else {
            eprintln!("objc-bound-drive: NOT RUN — no main-thread witness.");
            return NOT_RUN;
        };

        eprintln!("stage 1: dropped ON the main thread, the destructor runs there and does not");
        eprintln!("         dispatch (which from this position would be the SIGTRAP re-entrancy)");
        let flag = std::sync::Arc::new(AtomicU32::new(0));
        drop(MainThreadBound::new(WhereDidIDie(flag.clone()), mt));
        eprintln!("       died on {}", where_it_died(&flag));
        check(
            flag.load(Ordering::SeqCst) == 1,
            "stage1: the destructor ran on the main thread",
        );

        eprintln!("stage 2: dropped on a WORKER, the destructor still runs on the MAIN thread");
        let flag = std::sync::Arc::new(AtomicU32::new(0));
        let bound = MainThreadBound::new(WhereDidIDie(flag.clone()), mt);
        let (tx, rx) = mpsc::channel::<bool>();
        let worker = std::thread::spawn(move || {
            let off_main = !is_main_thread();
            drop(bound);
            let _ = tx.send(off_main);
        });
        let off_main = pump_until(&rx);
        let _ = worker.join();
        eprintln!("       died on {}", where_it_died(&flag));
        check(
            off_main == Some(true),
            "stage2: the container really was dropped off the main thread",
        );
        check(
            flag.load(Ordering::SeqCst) == 1,
            "stage2: and the destructor still ran on the MAIN thread",
        );

        eprintln!("stage 3: an aterm_objc::Retained of a declared class — S3's release end");
        let flag = std::sync::Arc::new(AtomicU32::new(0));
        let Some(obj): Option<Retained<Watched>> = Watched::alloc_init(
            mt,
            DeallocWatch {
                _calls: Cell::new(0),
                _where_died: WhereDidIDie(flag.clone()),
            },
        ) else {
            eprintln!("objc-bound-drive: NOT RUN — the probe class could not be instantiated.");
            return NOT_RUN;
        };
        // SAFETY: `-ping` is registered `v@:` by the macro above and `obj` owns a live +1.
        unsafe {
            let send: unsafe extern "C-unwind" fn(Id, Sel) = msg();
            send(obj.as_id(), sel!(ping));
        }
        let bound = MainThreadBound::new(obj, mt);
        let (tx, rx) = mpsc::channel::<bool>();
        let worker = std::thread::spawn(move || {
            let off_main = !is_main_thread();
            drop(bound);
            let _ = tx.send(off_main);
        });
        let off_main = pump_until(&rx);
        let _ = worker.join();
        eprintln!(
            "       -dealloc's ivar destructor ran on {}",
            where_it_died(&flag)
        );
        check(
            off_main == Some(true),
            "stage3: the last release was performed off the main thread",
        );
        check(
            flag.load(Ordering::SeqCst) == 1,
            "stage3: and -dealloc ran the Rust ivar destructor on the MAIN thread — S3's release \
             end, closed for everything this container holds",
        );

        eprintln!("stage 4: a drop-less container dropped off-main with the main thread parked");
        let outcome = probe("nodrop", WATCHDOG);
        if let Err(e) = &outcome {
            eprintln!("       {e}");
        }
        check(
            outcome.is_ok_and(|stdout| stdout.contains("nodrop-child-returned")),
            "stage4: a drop-less T dropped on a worker with the main thread parked exits 0",
        );

        eprintln!("stage 5: get_on_main from a worker reaches the value ON the main thread");
        let bound = std::sync::Arc::new(MainThreadBound::new(Cell::new(41_i64), mt));
        let (tx, rx) = mpsc::channel::<(i64, bool)>();
        let shared = std::sync::Arc::clone(&bound);
        let worker = std::thread::spawn(move || {
            let got = shared.get_on_main(|cell| {
                cell.set(cell.get() + 1);
                (cell.get(), is_main_thread())
            });
            let _ = tx.send(got);
        });
        let got = pump_until(&rx);
        let _ = worker.join();
        check(
            got == Some((42, true)),
            "stage5: the closure ran on the main thread and its value crossed back",
        );
        let bound = std::sync::Arc::try_unwrap(bound).ok();
        check(
            bound.map(|b| b.into_inner(mt).get()) == Some(42),
            "stage5: and the mutation stuck",
        );

        if failures.is_empty() {
            eprintln!("objc-bound-drive: PASS");
            PASS
        } else {
            eprintln!("objc-bound-drive: FAIL ({} stage(s))", failures.len());
            for f in &failures {
                eprintln!("  - {f}");
            }
            FAIL
        }
    }

    fn probe(mode: &str, budget: Duration) -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        let mut child = std::process::Command::new(exe)
            .env(MODE, mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn: {e}"))?;

        let deadline = Instant::now() + budget;
        loop {
            if let Some(status) = child.try_wait().map_err(|e| format!("try_wait: {e}"))? {
                if !status.success() {
                    return Err(format!("child exited {status}"));
                }
                let mut stdout = String::new();
                if let Some(mut out) = child.stdout.take() {
                    use std::io::Read as _;
                    let _ = out.read_to_string(&mut stdout);
                }
                return Ok(stdout.to_lowercase());
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("child still running after {budget:?}: hung"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
