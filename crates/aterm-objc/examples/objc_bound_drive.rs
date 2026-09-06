// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CONTAINER DRIVER: `aterm_objc::MainThreadBound`, and the UNSOUND version
//! of it, compiled and measured side by side.
//!
//! # Why this is a driver and not a test
//!
//! `MainThreadBound`'s third obligation — *the value is dropped on the main
//! thread* — is discharged by [`aterm_objc::run_on_main`] inside `Drop`, and
//! `run_on_main` cannot be exercised by libtest in either direction: a libtest
//! binary runs each test on a worker (so the direct branch is never taken) and
//! parks its main thread joining those workers (so the dispatched branch blocks
//! for ever). `tests/main_thread_bound.rs` holds the half that can be held on a
//! worker and opens by saying so. This file holds the rest.
//!
//! # THE PROOF OBLIGATION, and why an argument would not have discharged it
//!
//! The `Send` and `Sync` impls on `MainThreadBound<T>` are **unconditional in
//! `T`**. Two of the four obligations that pay for them are enforced by the
//! type system and their counterexamples are `compile_fail` doctests whose
//! error text was read rather than assumed. The third — the drop — has **no
//! type-system half at all**. Nothing about `unsafe impl<T> Send` makes the
//! `Drop` impl exist, and a reader has no way to tell a container that
//! reschedules from one that does not by looking at either type's signature.
//!
//! So the counterexample here is not a program that fails to compile. It is
//! [`NaiveBound<T>`]: a container declared **in this file**, with the same
//! `unsafe impl<T> Send`, the same witness on `new`, the same accessors — and
//! an ordinary derived drop. It compiles. It is what a reasonable person writes.
//! Stage 3 measures the thread its `T`'s destructor runs on, against
//! `MainThreadBound`'s, with the same `T`, the same worker and the same run
//! loop. One lands on the main thread and one does not.
//!
//! Stage 4 does it again with the payload that actually matters: an
//! `aterm_objc::Retained` of a [`declare_class!`][aterm_objc::declare_class]
//! class whose Rust ivar has a destructor. That is `S3` in the crate's
//! soundness list — *`dealloc` runs on whatever thread performs the last
//! release* — recorded there as CLOSED at its birth end and **open at its
//! release end**. For a value inside this container it is closed, and stage 4
//! is the measurement; `NaiveBound` is the same measurement with the closure
//! removed, and it runs a declared class's `-dealloc` and its Rust ivar
//! destructor on a spawned thread through 100% safe code.
//!
//! # WHICH STAGES CAN FAIL — plant-verified, two plants
//!
//! | plant | what fails |
//! |---|---|
//! | `Drop` drops in place instead of rescheduling | stage 2 (the destructor lands on the worker), stage 4 (a declared class's `-dealloc` lands there), and stage 5 — the needs-drop child stops hanging, because there is no dispatch left to block on |
//! | the `needs_drop` short-circuit removed | stage 5's second half: the drop-less child HANGS from the same position the real one returns from |
//!
//! Stages 3 and 4b are the counterexample's own arms and are expected to report
//! the WORKER; they fail if `NaiveBound` ever stops being unsound, which would
//! mean the comparison had lost its subject.
//!
//! # The `needs_drop` short-circuit is a GUARANTEE, not an optimisation
//!
//! `Drop` skips the dispatch entirely when `T` has no destructor. Stage 5
//! proves that is load-bearing with a child-process differential, because the
//! failure it prevents is a HANG and a hang cannot be asserted from inside the
//! thing that is hanging: the child drops a container of a needs-drop `T` on a
//! worker while its main thread is parked (a libtest binary's exact shape) and
//! must never return, and the same child dropping a drop-less `T` from the same
//! position must exit 0.

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(run() as u8)
}

const PASS: i32 = 0;
const FAIL: i32 = 1;
const NOT_RUN: i32 = 2;

/// The env var that turns this binary into one of its own child probes.
const MODE: &str = "ATERM_OBJC_BOUND_DRIVE_MODE";

/// How long the parent waits before declaring a child hung.
///
/// Generous on purpose: a FALSE "it hung" would be the worst outcome this file
/// could produce, and the `nodrop` child proves the same budget is far more
/// than the working path needs.
const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(not(target_os = "macos"))]
fn run() -> i32 {
    eprintln!("objc-bound-drive: NOT RUN — libdispatch's main queue is a macOS/Darwin subject.");
    NOT_RUN
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

    use super::{FAIL, MODE, NOT_RUN, PASS, WATCHDOG};

    // SAFETY: CoreFoundation is a system framework, always present. The main
    // thread has to SERVICE its queue or every dispatched drop blocks; that is
    // what this is for.
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
    }

    /// Whether this thread is the process main thread, asked of Foundation
    /// directly rather than through the crate's own witness.
    ///
    /// Deliberately independent of [`MainThread`]: a stage that used the same
    /// witness the capability uses could not catch the capability being wrong
    /// about the thread.
    fn is_main_thread() -> bool {
        // SAFETY: `+[NSThread isMainThread]` is a side-effect-free class-method
        // `BOOL` query and the cast is exactly `-(BOOL)(id, SEL)`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Bool = msg();
            f(class(c"NSThread").as_id(), sel!(isMainThread)).as_bool()
        }
    }

    // -----------------------------------------------------------------------
    // THE COUNTEREXAMPLE. It compiles. It is what a reasonable person writes,
    // and it is unsound in exactly one place.
    // -----------------------------------------------------------------------

    /// `MainThreadBound`'s twin, WITHOUT the rescheduling drop.
    ///
    /// Everything else is identical: the witness on `new`, the witness on
    /// `get`, the same `unsafe impl<T> Send` with a SAFETY comment that is true
    /// of two of its three clauses. The third clause — "and the value is
    /// dropped on the main thread" — is the one that has nothing behind it, and
    /// nothing in the type system notices.
    ///
    /// This is the point of the file: the compiler cannot tell these two apart.
    struct NaiveBound<T>(ManuallyDrop<T>);

    // SAFETY: THIS COMMENT IS FALSE, DELIBERATELY, AND THAT IS THE
    // MEASUREMENT. Two of its three clauses hold — the value is born on the
    // main thread (`new` takes a witness) and read only there (`get` takes
    // one) — and the third does not: there is no `Drop` impl, so `T`'s
    // destructor runs wherever the container happens to die. Nothing in this
    // file's compilation objects. Stage 3 is what objects.
    unsafe impl<T> Send for NaiveBound<T> {}

    impl<T> NaiveBound<T> {
        fn new(inner: T, _mt: MainThread) -> Self {
            Self(ManuallyDrop::new(inner))
        }
    }

    impl<T> Drop for NaiveBound<T> {
        fn drop(&mut self) {
            // SAFETY: the container's own drop, so the value is destroyed
            // exactly once and never read again. What is missing is not a
            // safety condition on THIS line; it is the thread this line runs
            // on.
            unsafe { ManuallyDrop::drop(&mut self.0) };
        }
    }

    // -----------------------------------------------------------------------
    // The payloads whose destructors report where they ran.
    // -----------------------------------------------------------------------

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

    /// The ivars of the declared class stage 4 uses: a destructor that records
    /// the thread `-dealloc` ran on, plus a `Cell` so the type is `!Send` for
    /// the same structural reason the real ones are.
    struct DeallocWatch {
        _calls: Cell<i64>,
        where_died: WhereDidIDie,
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

    // -----------------------------------------------------------------------
    // THE CHILD PROBES — stage 5's differential, which cannot be written any
    // other way because the failure it guards against is a hang.
    // -----------------------------------------------------------------------

    /// A container of a needs-drop `T`, dropped on a worker, main thread
    /// PARKED. This must never return: it is `run_on_main`'s first documented
    /// hang, reached through `Drop`, and it is the shape a libtest binary has.
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
        // The main thread PARKS here rather than pumping — exactly what a
        // libtest binary's main thread does while joining its workers.
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

    // -----------------------------------------------------------------------

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

        // -- STAGE 1: the direct path -------------------------------------
        eprintln!("stage 1: dropped ON the main thread, the destructor runs there and does not");
        eprintln!("         dispatch (which from this position would be the SIGTRAP re-entrancy)");
        let flag = std::sync::Arc::new(AtomicU32::new(0));
        drop(MainThreadBound::new(WhereDidIDie(flag.clone()), mt));
        eprintln!("       died on {}", where_it_died(&flag));
        check(
            flag.load(Ordering::SeqCst) == 1,
            "stage1: the destructor ran on the main thread",
        );

        // -- STAGE 2: the reschedule --------------------------------------
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

        // -- STAGE 3: THE COUNTEREXAMPLE ----------------------------------
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

        // -- STAGE 4: S3's release end, on a real declared class -----------
        eprintln!("stage 4: an aterm_objc::Retained of a declared class — S3's release end");
        let flag = std::sync::Arc::new(AtomicU32::new(0));
        let Some(obj): Option<Retained<Watched>> = Watched::alloc_init(
            mt,
            DeallocWatch {
                _calls: Cell::new(0),
                where_died: WhereDidIDie(flag.clone()),
            },
        ) else {
            eprintln!("objc-bound-drive: NOT RUN — the probe class could not be instantiated.");
            return NOT_RUN;
        };
        // Message it once, so the instance is demonstrably live and the class
        // demonstrably registered before anything is said about its death.
        // SAFETY: `-ping` is registered `v@:` by the macro above and `obj` owns
        // a live +1.
        unsafe {
            let send: unsafe extern "C-unwind" fn(Id, Sel) = msg();
            send(obj.as_id(), sel!(ping));
        }
        let bound = MainThreadBound::new(obj, mt);
        let (tx, rx) = mpsc::channel::<bool>();
        let worker = std::thread::spawn(move || {
            let off_main = !is_main_thread();
            // The last release happens HERE, on a worker, through 100% safe
            // code: no `unsafe` token, no misuse, just a `Window` moved into a
            // thread.
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
                where_died: WhereDidIDie(flag.clone()),
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

        // -- STAGE 5: the needs_drop short-circuit, by differential --------
        eprintln!(
            "stage 5: the `needs_drop` skip is a GUARANTEE — proved against the hang it avoids"
        );
        match probe("hang") {
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
        match probe("nodrop") {
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

        // -- STAGE 6: get_on_main from a worker ---------------------------
        eprintln!("stage 6: get_on_main from a worker reaches the value ON the main thread");
        let bound = std::sync::Arc::new(MainThreadBound::new(Cell::new(41_i64), mt));
        let (tx, rx) = mpsc::channel::<(i64, bool)>();
        let shared = std::sync::Arc::clone(&bound);
        let worker = std::thread::spawn(move || {
            // `Arc<MainThreadBound<Cell<i64>>>` requires `Sync`, and `Cell` is
            // not — so this line is the `Sync` impl being used, not asserted.
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

    // -----------------------------------------------------------------------
    // CHILD-PROCESS PLUMBING — the same shape `objc_dispatch_drive` uses.
    // -----------------------------------------------------------------------

    #[derive(Debug)]
    enum ProbeOutcome {
        StillRunning,
        Signalled(i32),
        Exited { code: Option<i32>, stdout: String },
        Failed(String),
    }

    /// Re-execute this binary in `mode` and watch it for [`WATCHDOG`].
    fn probe(mode: &str) -> ProbeOutcome {
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

        let deadline = Instant::now() + WATCHDOG;
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

    /// The ivar field the stages read through their `Arc`, kept alive so the
    /// struct is not merely a destructor with a name.
    const _: () = {
        assert!(size_of::<WhereDidIDie>() == size_of::<std::sync::Arc<AtomicU32>>());
    };

    impl DeallocWatch {
        /// Never called; it exists so `where_died` is a READ field and not a
        /// silently-dead one.
        #[allow(dead_code)]
        fn watcher(&self) -> &WhereDidIDie {
            &self.where_died
        }
    }
}
