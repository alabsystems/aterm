// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm_objc::MainThreadBound` and an unsound twin, measured side by side.
//!
//! The container's third obligation (the value is dropped on the main thread) is discharged by
//! [`aterm_objc::run_on_main`] inside `Drop`, which libtest cannot exercise: it runs each test
//! on a worker and parks its main thread joining them. So this target is `harness = false` and
//! its `main` is the process main thread. It answers libtest's argv as one test named
//! `main_thread_bound_drive` ([`asked`]): a filter that does not match, a matching `--skip`,
//! `--ignored` or `--bench` runs nothing and passes, and `--list` names it. Off macOS it passes
//! without running; on macOS a NOT RUN (`2`: no main thread, no witness, no probe class) is a
//! failing exit.
//!
//! The drop obligation has no type-system half, so the counterexample is not a `compile_fail`
//! doctest but `NaiveBound<T>`: the same `unsafe impl<T> Send` and witnessed `new`, with a drop
//! in place. Stage 3 measures where its `T`'s destructor runs against `MainThreadBound`'s;
//! stage 4 does the same with a `Retained` of a declared class whose ivar has a destructor
//! (`S3`'s release end in the crate's soundness list), which `NaiveBound` lets `-dealloc` run on
//! a spawned thread through safe code.
//!
//! A `Drop` that drops in place instead of rescheduling fails stages 2 and 4 (the destructor
//! lands on the worker) and stage 5's needs-drop child (it stops hanging); removing the
//! `needs_drop` short-circuit makes stage 5's drop-less child hang. Stages 3 and 4b are the
//! counterexample's arms and expect the worker: they fail if `NaiveBound` stops being unsound.
//! Stage 5 is a child-process differential because the failure the short-circuit prevents is a
//! hang: a child dropping a needs-drop container on a worker with its main thread parked must
//! never return, and the same child with a drop-less `T` must exit 0.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

/// The name libtest's argv selects this target by: its `[[test]] name`.
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

/// What libtest's argv asks of the one test this binary is.
enum Asked {
    Run,
    /// `--list`: print the name when the rest of the argv selects it.
    List {
        selected: bool,
    },
    /// Filtered out, skipped, `--ignored`, `--bench` or `--help`.
    Nothing,
}

/// Read libtest's argv as libtest would for one test named [`TEST_NAME`]: filters and `--skip`
/// match by substring (whole under `--exact`), options that take a value consume it, and an
/// unknown option is an error rather than a filter that silently selects nothing.
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

/// How long stage 5's needs-drop child, which must never return, is watched before it counts as
/// hung; it always costs this whole window. A `Drop` that does not reschedule returns in
/// milliseconds.
#[cfg(target_os = "macos")]
const HANG_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

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
        Ok("hang") => macos::child_drop_with_destructor(),
        Ok("nodrop") => macos::child_drop_without_destructor(),
        _ => macos::parent(),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::mem::ManuallyDrop;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use aterm_objc::{
        Bool, Id, MainThread, MainThreadBound, Retained, Sel, class, declare_class, msg, sel,
    };

    use super::{FAIL, HANG_WINDOW, MODE, NOT_RUN, PASS, WATCHDOG};

    // SAFETY: CoreFoundation is a system framework, always present, and these are its
    // declarations. The main thread must service its queue or every dispatched drop blocks.
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
    }

    /// Whether this thread is the process main thread, asked of Foundation directly: a stage that
    /// used the crate's own [`MainThread`] witness could not catch the witness being wrong.
    fn is_main_thread() -> bool {
        // SAFETY: `+[NSThread isMainThread]` is a side-effect-free class-method
        // `BOOL` query and the cast is exactly `-(BOOL)(id, SEL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = msg();
            f(class(c"NSThread").as_id(), sel!(isMainThread)).as_bool()
        }
    }

    // -- The counterexample ----------------------------------------------

    /// `MainThreadBound`'s twin without the rescheduling drop: the same witnessed `new` and the
    /// same `unsafe impl<T> Send`, whose SAFETY argument holds for two of its three clauses. The
    /// compiler cannot tell the two apart.
    struct NaiveBound<T>(ManuallyDrop<T>);

    // SAFETY: deliberately false; that is the measurement. The value is born on the main thread
    // (`new` takes a witness) and never read, but `Drop` destroys it wherever the container
    // dies. The compiler does not object; stage 3 does.
    unsafe impl<T> Send for NaiveBound<T> {}

    impl<T> NaiveBound<T> {
        fn new(inner: T, _mt: MainThread) -> Self {
            Self(ManuallyDrop::new(inner))
        }
    }

    impl<T> Drop for NaiveBound<T> {
        fn drop(&mut self) {
            // SAFETY: the container's own drop: the value is destroyed exactly once and never
            // read again. What is missing is not a safety condition on this line but the thread
            // it runs on.
            unsafe { ManuallyDrop::drop(&mut self.0) };
        }
    }

    // -- The payloads whose destructors report where they ran ------------

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

    /// Stage 4's ivars: a destructor that records the thread `-dealloc` ran on, and a `Cell` so
    /// the type is `!Send` as real ivars are.
    struct DeallocWatch {
        _calls: Cell<i64>,
        /// Held for its `Drop`, which is the observation stage 4 makes.
        _where_died: WhereDidIDie,
    }

    declare_class! {
        /// A class whose Rust ivar destructor reports its thread — `S3`'s
        /// release end, made observable.
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

    /// Pump the main run loop until `rx` produces, or the watchdog fires.
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

    // -- The child probes: stage 5's differential (the failure it guards is a hang) --

    /// A needs-drop container dropped on a worker with the main thread parked, as in a libtest
    /// binary: `run_on_main`'s first documented hang, through `Drop`. Must never return.
    pub fn child_drop_with_destructor() -> i32 {
        assert!(is_main_thread(), "the probe must start on the main thread");
        // SAFETY: this process's main thread is where `main` is running.
        let mt = MainThread::new().expect("the probe runs on the main thread");
        let flag = std::sync::Arc::new(AtomicU32::new(0));
        let bound = MainThreadBound::new(WhereDidIDie(flag), mt);
        eprintln!("child(hang): dropping a needs-drop container on a worker, main thread parked");
        let worker = std::thread::spawn(move || {
            drop(bound);
            println!("HANG-CHILD-RETURNED");
        });
        // Parks rather than pumping, as a libtest main thread does joining its workers.
        let _ = worker.join();
        println!("HANG-CHILD-JOINED");
        PASS
    }

    /// The same position, with a `T` that has no destructor. The short-circuit
    /// means this never reaches the queue, so it must return at once.
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

        // -- Stage 1: the direct path -------------------------------------
        eprintln!("stage 1: dropped ON the main thread, the destructor runs there and does not");
        eprintln!("         dispatch (which from this position would be the SIGTRAP re-entrancy)");
        let flag = std::sync::Arc::new(AtomicU32::new(0));
        drop(MainThreadBound::new(WhereDidIDie(flag.clone()), mt));
        eprintln!("       died on {}", where_it_died(&flag));
        check(
            flag.load(Ordering::SeqCst) == 1,
            "stage1: the destructor ran on the main thread",
        );

        // -- Stage 2: the reschedule --------------------------------------
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

        // -- Stage 3: the counterexample ----------------------------------
        eprintln!("stage 3: the SAME program with `NaiveBound` — same `unsafe impl Send`, no");
        eprintln!("         reschedule. It compiles, and it lands somewhere else.");
        let flag = std::sync::Arc::new(AtomicU32::new(0));
        let naive = NaiveBound::new(WhereDidIDie(flag.clone()), mt);
        let (tx, rx) = mpsc::channel::<bool>();
        let worker = std::thread::spawn(move || {
            let off_main = !is_main_thread();
            drop(naive);
            let _ = tx.send(off_main);
        });
        let off_main = pump_until(&rx);
        let _ = worker.join();
        eprintln!("       died on {}", where_it_died(&flag));
        check(
            off_main == Some(true),
            "stage3: the naive container was dropped off the main thread too",
        );
        check(
            flag.load(Ordering::SeqCst) == 2,
            "stage3: and ITS destructor ran on the WORKER — the difference between the two \
             containers is the `Drop` impl and nothing else, and no compiler diagnostic \
             distinguishes them",
        );

        // -- Stage 4: S3's release end, on a real declared class -----------
        eprintln!("stage 4: an aterm_objc::Retained of a declared class — S3's release end");
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
        // Messaged once, so the instance is live and the class registered before its death
        // is measured.
        // SAFETY: `-ping` is registered `v@:` by the macro above and `obj` owns a live +1.
        unsafe {
            let send: unsafe extern "C-unwind" fn(Id, Sel) = msg();
            send(obj.as_id(), sel!(ping));
        }
        let bound = MainThreadBound::new(obj, mt);
        let (tx, rx) = mpsc::channel::<bool>();
        let worker = std::thread::spawn(move || {
            let off_main = !is_main_thread();
            // The last release happens here, on a worker, through safe code.
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
            "stage4: the last release was performed off the main thread",
        );
        check(
            flag.load(Ordering::SeqCst) == 1,
            "stage4: and -dealloc ran the Rust ivar destructor on the MAIN thread — S3's release \
             end, closed for everything this container holds",
        );

        eprintln!("stage 4b: the same object in a NaiveBound");
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
        let naive = NaiveBound::new(obj, mt);
        let (tx, rx) = mpsc::channel::<bool>();
        let worker = std::thread::spawn(move || {
            let off_main = !is_main_thread();
            drop(naive);
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
            "stage4b: dropped off the main thread",
        );
        check(
            flag.load(Ordering::SeqCst) == 2,
            "stage4b: and a DECLARED CLASS's -dealloc ran its Rust ivar destructor on a spawned \
             thread — the hole `MainThreadBound`'s Drop exists to close",
        );

        // -- Stage 5: the needs_drop short-circuit, by differential --------
        eprintln!(
            "stage 5: the `needs_drop` skip is a GUARANTEE — proved against the hang it avoids"
        );
        match probe("hang", HANG_WINDOW) {
            ProbeOutcome::StillRunning => check(
                true,
                "stage5: a needs-drop container dropped off-main with the main thread PARKED \
                 never returns — which is why the short-circuit for a drop-less T is not an \
                 optimisation",
            ),
            ProbeOutcome::Exited { code, stdout } => {
                eprintln!("       child exited {code:?} stdout={stdout:?}");
                check(
                    false,
                    "stage5: the needs-drop child RETURNED — the hazard premise is false",
                );
            }
            ProbeOutcome::Signalled(sig) => {
                eprintln!("       child died on signal {sig}");
                check(
                    true,
                    "stage5: the needs-drop child did not return (it was signalled)",
                );
            }
            ProbeOutcome::Failed(e) => {
                eprintln!("       {e}");
                check(false, "stage5: the needs-drop probe could not be launched");
            }
        }
        match probe("nodrop", WATCHDOG) {
            ProbeOutcome::Exited { code, stdout } => {
                check(
                    code == Some(PASS) && stdout.contains("nodrop-child-returned"),
                    "stage5: the SAME position with a drop-less T exits 0 — same binary, same \
                     thread, same parked main thread",
                );
            }
            other => {
                eprintln!("       {other:?}");
                check(false, "stage5: the drop-less child exited cleanly");
            }
        }

        // -- Stage 6: get_on_main from a worker ---------------------------
        eprintln!("stage 6: get_on_main from a worker reaches the value ON the main thread");
        let bound = std::sync::Arc::new(MainThreadBound::new(Cell::new(41_i64), mt));
        let (tx, rx) = mpsc::channel::<(i64, bool)>();
        let shared = std::sync::Arc::clone(&bound);
        let worker = std::thread::spawn(move || {
            // Sharing `Arc<MainThreadBound<Cell<i64>>>` needs `Sync`, which `Cell` is not:
            // this line uses the `Sync` impl.
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
            "stage6: the closure ran on the main thread and its value crossed back",
        );
        let bound = std::sync::Arc::try_unwrap(bound).ok();
        check(
            bound.map(|b| b.into_inner(mt).get()) == Some(42),
            "stage6: and the mutation stuck",
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

    // -- Child-process plumbing ------------------------------------------

    #[derive(Debug)]
    enum ProbeOutcome {
        StillRunning,
        Signalled(i32),
        Exited { code: Option<i32>, stdout: String },
        Failed(String),
    }

    /// Re-execute this binary in `mode` and watch it for `budget`.
    fn probe(mode: &str, budget: Duration) -> ProbeOutcome {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return ProbeOutcome::Failed(format!("current_exe: {e}")),
        };
        let mut child = match std::process::Command::new(exe)
            .env(MODE, mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ProbeOutcome::Failed(format!("spawn: {e}")),
        };

        let deadline = Instant::now() + budget;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    use std::os::unix::process::ExitStatusExt as _;
                    if let Some(sig) = status.signal() {
                        return ProbeOutcome::Signalled(sig);
                    }
                    let mut stdout = String::new();
                    if let Some(mut out) = child.stdout.take() {
                        use std::io::Read as _;
                        let _ = out.read_to_string(&mut stdout);
                    }
                    return ProbeOutcome::Exited {
                        code: status.code(),
                        stdout: stdout.to_lowercase(),
                    };
                }
                Ok(None) => {}
                Err(e) => return ProbeOutcome::Failed(format!("try_wait: {e}")),
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return ProbeOutcome::StillRunning;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
