// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The calling thread's QoS class on macOS: the class it has requested
//! ([`requested_class_of_self`]), and a lift to the default class for a short piece of work a
//! thread at a higher class waits on ([`lift`], then [`restore`]), so that work is not left at a
//! class a loaded Mac starves of a core while its waiter waits (a priority inversion).
//!
//! macOS runs every thread of a process in its background band (darwin-bg: a launchd job of
//! `ProcessType` `Background`, a `taskpolicy -b` child) at the band's priority whatever class the
//! thread requests, so a lift there changes the class it reads back and nothing it runs at. What
//! each thread of a process runs at is read from outside it ([`thread_base_priorities`]).

use std::ffi::c_void;

use libc::qos_class_t::{QOS_CLASS_BACKGROUND, QOS_CLASS_DEFAULT, QOS_CLASS_UTILITY};

/// The class and relative priority [`lift`] raised the calling thread from.
#[derive(Clone, Copy, Debug)]
pub struct Lifted {
    class: libc::qos_class_t,
    relative: libc::c_int,
}

/// Raise the calling thread to the default class where it had requested less (utility or
/// background), returning what it had requested: `None` where it had asked for the default or
/// more, or the platform declined to read or set its class.
#[must_use]
pub fn lift() -> Option<Lifted> {
    let (class, relative) = requested_class_of_self()?;
    let from = [QOS_CLASS_UTILITY, QOS_CLASS_BACKGROUND]
        .into_iter()
        .find(|below| *below as libc::c_uint == class)?;
    // SAFETY: sets this thread's own class; takes no pointers and mutates no shared state.
    let rc = unsafe { libc::pthread_set_qos_class_self_np(QOS_CLASS_DEFAULT, 0) };
    (rc == 0).then_some(Lifted {
        class: from,
        relative,
    })
}

/// Put the calling thread back at the class and relative priority `lifted` names: call it on the
/// thread [`lift`] raised, as another thread would be set to the first one's class instead.
pub fn restore(lifted: Lifted) {
    // SAFETY: as in `lift`; `class` and `relative` are the pair a thread had requested.
    unsafe { libc::pthread_set_qos_class_self_np(lifted.class, lifted.relative) };
}

/// The class and relative priority the calling thread has requested (its own word, which no
/// override another thread pushes onto it changes), or `None` where the platform declined to
/// answer.
#[must_use]
pub fn requested_class_of_self() -> Option<(libc::c_uint, libc::c_int)> {
    // SAFETY: `pthread_self` names the calling thread, alive across the call.
    unsafe { requested_class_of_thread(pthread_self().addr()) }
}

/// The class and relative priority the thread `handle` names has requested, read from the calling
/// thread, so nothing has to run on that thread to say it, or `None` where the platform declined
/// to answer.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn requested_class_of<T>(
    handle: &std::thread::JoinHandle<T>,
) -> Option<(libc::c_uint, libc::c_int)> {
    use std::os::unix::thread::JoinHandleExt as _;
    // SAFETY: the borrowed handle is neither joined nor detached, so the thread it names stays
    // valid across the call.
    unsafe { requested_class_of_thread(handle.as_pthread_t()) }
}

/// The class and relative priority `thread` has requested.
///
/// # Safety
///
/// `thread` must name a thread that stays valid (neither joined nor detached and exited) across
/// the call.
unsafe fn requested_class_of_thread(
    thread: libc::pthread_t,
) -> Option<(libc::c_uint, libc::c_int)> {
    let mut class: libc::c_uint = 0;
    let mut relative: libc::c_int = 0;
    // SAFETY: both out-parameters are live, correctly typed locals, and the caller keeps `thread`
    // valid across the call.
    let rc = unsafe { pthread_get_qos_class_np(thread, &mut class, &mut relative) };
    (rc == 0).then_some((class, relative))
}

/// The base priority each thread of the process `pid` runs at now (`<sys/proc_info.h>`'s
/// `pth_priority`), read from outside it, in the order the kernel lists the threads: every one
/// reads 4 in the background band (darwin-bg), and a thread at the default class outside any band
/// and clamp reads 31. A thread that ends between the listing and its read is left out. `None`
/// where the process is gone or the kernel declined to list its threads (another user's).
#[must_use]
pub fn thread_base_priorities(pid: i32) -> Option<Vec<i32>> {
    /// `<sys/proc_info.h>`'s flavor listing a process's thread handles (`u64` each).
    const PROC_PIDLISTTHREADS: libc::c_int = 6;
    let record = libc::c_int::try_from(size_of::<libc::proc_threadinfo>()).ok()?;
    let mut handles: Vec<u64> = vec![0; 64];
    let listed = loop {
        let bytes = libc::c_int::try_from(handles.len() * size_of::<u64>()).ok()?;
        // SAFETY: `handles` is `bytes` writable bytes of `u64` slots, which this flavor fills.
        let wrote = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDLISTTHREADS,
                0,
                handles.as_mut_ptr().cast(),
                bytes,
            )
        };
        if wrote <= 0 {
            return None;
        }
        // A full buffer may have cut the list short: ask again with room to spare.
        if wrote < bytes {
            break usize::try_from(wrote).ok()? / size_of::<u64>();
        }
        handles.resize(handles.len() * 2, 0);
    };
    let priorities = handles[..listed]
        .iter()
        .filter_map(|&handle| {
            let mut info = std::mem::MaybeUninit::<libc::proc_threadinfo>::uninit();
            // SAFETY: `info` is `record` writable bytes laid out as the record this flavor fills.
            let read = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    handle,
                    info.as_mut_ptr().cast(),
                    record,
                )
            };
            // SAFETY: an exact-size read initialized every (integer and byte) field.
            (read == record).then(|| unsafe { info.assume_init() }.pth_priority)
        })
        .collect();
    Some(priorities)
}

unsafe extern "C" {
    /// Spelled as `asleep` declares it: one symbol, one signature.
    fn pthread_self() -> *mut c_void;
    fn pthread_get_qos_class_np(
        thread: libc::pthread_t,
        class: *mut libc::c_uint,
        relative_priority: *mut libc::c_int,
    ) -> libc::c_int;
}
