// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// An inert live read: this creates no load, thread, command or GUI request. Its structural
/// assertions establish neither CPU-accounting freshness nor an actual-window CPU share.
#[test]
fn named_self_cpu_live_cut_reads_only_this_generation_and_keeps_counter_brackets() {
    let cut = super::super::read(Instant::now()).expect("a self-only Mach read");
    assert_eq!(cut.process.pid, std::process::id());
    assert_ne!(cut.process.start, 0);
    assert!(cut.end_ns >= cut.begin_ns);
    assert!(cut.process_after.ns >= cut.process_before.ns);
    assert_eq!(
        cut.coverage.inspected,
        cut.coverage.listed.min(super::super::MAX_THREADS)
    );
    assert_eq!(
        cut.coverage.capped,
        cut.coverage.listed > super::super::MAX_THREADS
    );
    assert!(cut.coverage.unread <= cut.coverage.inspected);
    assert!(cut.threads.len() <= cut.coverage.inspected);
    assert!(cut.coverage.listed > 0, "the calling test thread is alive");
    assert!(cut.threads.iter().all(|thread| thread.id != 0));
    assert!(
        cut.threads
            .iter()
            .filter_map(|thread| thread.cpu)
            .all(|cpu| cpu.precision_ns == 2000)
    );
    for thread in &cut.threads {
        match (thread.cpu, thread.split) {
            (Some(cpu), Some(split)) => assert_eq!(
                split.user_ns + split.system_ns,
                cpu.ns,
                "thread {}: the split is the sum's own record",
                thread.id
            ),
            (None, None) => {}
            other => panic!(
                "thread {}: a counter without its split: {other:?}",
                thread.id
            ),
        }
    }
}

unsafe extern "C" {
    fn dispatch_queue_create(
        label: *const std::ffi::c_char,
        attr: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn dispatch_async_f(
        queue: *mut std::ffi::c_void,
        context: *mut std::ffi::c_void,
        work: extern "C" fn(*mut std::ffi::c_void),
    );
    fn dispatch_release(object: *mut std::ffi::c_void);
    fn pthread_threadid_np(thread: *mut std::ffi::c_void, id: *mut u64) -> i32;
}

/// What the queued work holds: it says it started, then waits to be let go.
struct Held {
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

/// The one-minute hang detector for each step another thread must take.
const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

extern "C" fn hold(context: *mut std::ffi::c_void) {
    // SAFETY: the test hands this work exactly one `Box<Held>`, released here once.
    let held = unsafe { Box::from_raw(context.cast::<Held>()) };
    let _ = held.started.send(());
    let _ = held.release.recv_timeout(HANG_DETECTOR);
}

/// A serial queue of this test's own, labelled, holds one piece of work on a worker thread: a
/// cut that asks for every thread's queue finds that label on exactly one thread, and not on the
/// calling test thread, whose queue is read too. The negative control is the same cut not
/// asking: no thread carries a queue then. The work is let go and the queue released before the
/// test returns.
#[test]
fn named_self_cpu_reads_the_dispatch_queue_a_thread_drains() {
    const LABEL: &std::ffi::CStr = c"aterm.sysprobe.self-cpu.queue-test";
    let (started_tx, started) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    // SAFETY: a NUL-terminated label and a null (serial) attribute; the queue is released below.
    let queue = unsafe { dispatch_queue_create(LABEL.as_ptr(), std::ptr::null_mut()) };
    assert!(!queue.is_null(), "dispatch_queue_create");
    let held = Box::into_raw(Box::new(Held {
        started: started_tx,
        release: release_rx,
    }));
    // SAFETY: `queue` is live, and `hold` takes ownership of the one box it is handed.
    unsafe { dispatch_async_f(queue, held.cast(), hold) };
    started
        .recv_timeout(HANG_DETECTOR)
        .expect("the queued work started");
    let asked = super::super::read_asking(Instant::now(), &mut |_, _| true);
    let plain = super::super::read(Instant::now());
    let _ = release.send(());
    // SAFETY: the queue is this test's; its pending work keeps its own reference.
    unsafe { dispatch_release(queue) };
    let asked = asked.expect("a self cut asking every queue");
    let label = LABEL.to_str().unwrap();
    let draining: Vec<_> = asked
        .threads
        .iter()
        .filter(|t| t.queue == Some(Queue::Label(label.to_owned())))
        .collect();
    assert_eq!(draining.len(), 1, "{:?}", asked.threads);
    let plain = plain.expect("a self cut asking none");
    assert!(
        plain.threads.iter().all(|t| t.queue.is_none()),
        "negative control: a cut that asks for no queue reads none"
    );
    let mut me = 0u64;
    // SAFETY: a null thread names the calling one, and `me` is a live slot for its id.
    assert_eq!(
        unsafe { pthread_threadid_np(std::ptr::null_mut(), &raw mut me) },
        0
    );
    let mine = asked
        .threads
        .iter()
        .find(|t| t.id == me)
        .expect("the calling thread's row");
    assert!(
        mine.queue.is_some() && mine.queue != draining[0].queue,
        "the calling thread's queue is read, and is not the held one: {mine:?}"
    );
}
