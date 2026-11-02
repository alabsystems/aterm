// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The calling thread's QoS class on macOS: the class it has requested
//! ([`requested_class_of_self`]), and a lift to the default class for a short piece of work a
//! thread at a higher class waits on ([`lift`], then [`restore`]), so that work is not left at a
//! class a loaded Mac starves of a core while its waiter waits (a priority inversion).
//!
//! macOS runs every thread of a process in its background band (darwin-bg: a launchd job of
//! `ProcessType` `Background`, a `taskpolicy -b` child) at the band's priority whatever class the
//! thread requests, so a lift there changes the class it reads back and nothing it runs at.

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
    let mut class: libc::c_uint = 0;
    let mut relative: libc::c_int = 0;
    // SAFETY: both out-parameters are live, correctly typed locals, and `pthread_self` names the
    // calling thread, alive across the call.
    let rc = unsafe { pthread_get_qos_class_np(pthread_self().addr(), &mut class, &mut relative) };
    (rc == 0).then_some((class, relative))
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
