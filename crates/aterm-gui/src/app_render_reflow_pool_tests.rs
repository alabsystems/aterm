// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ReflowHandoff, reflow_pool_submit, reflow_worker_ceiling};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[test]
fn a_pooled_rewrap_runs_at_user_initiated_not_the_inherited_band() {
    // SAFETY: `qos_class_self` is libpthread's, part of `libSystem`, linked into every process on this platform. It reads only the calling thread's own requested class and takes no arguments.
    unsafe extern "C" {
        fn qos_class_self() -> u32;
    }
    const USER_INITIATED_RAW: u32 = 0x19;

    let seen = Arc::new((Mutex::new(None::<u32>), Condvar::new()));
    let posted = Arc::clone(&seen);
    let stranded = reflow_pool_submit(ReflowHandoff {
        run: Box::new(move || {
            // SAFETY: see the extern above.
            let class = unsafe { qos_class_self() };
            let (lock, cv) = &*posted;
            *lock.lock().unwrap_or_else(|p| p.into_inner()) = Some(class);
            cv.notify_all();
        }),
        inline: Box::new(|| {}),
        priority: true,
    });
    if !stranded.is_empty() {
        aterm_spec::host::could_not_run("no rewrap worker could start here (threads or fds)");
    }

    let (lock, cv) = &*seen;
    let mut class = lock.lock().unwrap_or_else(|p| p.into_inner());
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while class.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no pooled worker drove the job");
        let (next, _) = cv
            .wait_timeout(class, left)
            .unwrap_or_else(|p| p.into_inner());
        class = next;
    }
    let class = class.expect("the job posted its class");
    assert_eq!(
        class, USER_INITIATED_RAW,
        "an aterm-reflow worker ran at QoS class {class:#x}, not USER_INITIATED \
         ({USER_INITIATED_RAW:#x}) — a rewrap the human is waiting on, holding \
         the pool lock, below the class qos.rs gives it"
    );
}

#[test]
fn the_pool_runs_every_job_once_and_never_exceeds_its_ceiling() {
    const JOBS: usize = 64;
    let ceiling = reflow_worker_ceiling();
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let ran = Arc::new(AtomicUsize::new(0));
    let stranded_inline = Arc::new(AtomicUsize::new(0));
    let done = Arc::new((Mutex::new(0usize), Condvar::new()));

    for _ in 0..JOBS {
        let (live, peak, ran, done) = (live.clone(), peak.clone(), ran.clone(), Arc::clone(&done));
        let inline_seen = stranded_inline.clone();
        let run = move || {
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(2));
            live.fetch_sub(1, Ordering::SeqCst);
            ran.fetch_add(1, Ordering::SeqCst);
            let (lock, cv) = &*done;
            *lock.lock().unwrap_or_else(|p| p.into_inner()) += 1;
            cv.notify_all();
        };
        let inline = move || {
            inline_seen.fetch_add(1, Ordering::SeqCst);
        };
        let stranded = reflow_pool_submit(ReflowHandoff {
            run: Box::new(run),
            inline: Box::new(inline),
            priority: false,
        });
        for job in stranded {
            (job.inline)();
        }
    }

    let (lock, cv) = &*done;
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut seen = lock.lock().unwrap_or_else(|p| p.into_inner());
    while *seen + stranded_inline.load(Ordering::SeqCst) < JOBS {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "the pool left {} of {JOBS} jobs undriven — a dropped job wedges \
             that session's tiered history forever",
            JOBS - *seen
        );
        let (next, _) = cv
            .wait_timeout(seen, Duration::from_millis(50))
            .unwrap_or_else(|p| p.into_inner());
        seen = next;
    }
    drop(seen);

    assert_eq!(
        ran.load(Ordering::SeqCst) + stranded_inline.load(Ordering::SeqCst),
        JOBS,
        "every submitted job must be driven exactly once"
    );
    let observed = peak.load(Ordering::SeqCst);
    assert!(
        observed <= ceiling,
        "{observed} jobs were in flight at once, above the pool ceiling \
         {ceiling} — the bound MPT-1 exists to impose is not being imposed"
    );
    assert!(
        observed >= 1 || stranded_inline.load(Ordering::SeqCst) == JOBS,
        "no job was ever observed in flight and nothing fell back to the \
         inline arm, so the ceiling assertion above proved nothing"
    );
}
