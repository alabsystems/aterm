// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS memory-pressure notification seam.

use std::os::raw::c_void;
use std::ptr;

type DispatchObject = *mut c_void;

// DISPATCH_MEMORYPRESSURE_* mask bits (dispatch/source.h).
const NORMAL: usize = 0x01;
const WARN: usize = 0x02;
const CRITICAL: usize = 0x04;
// DISPATCH_QUEUE_PRIORITY_DEFAULT.
const QUEUE_PRIORITY_DEFAULT: isize = 0;

unsafe extern "C" {
    #[allow(non_upper_case_globals)]
    static _dispatch_source_type_memorypressure: c_void;
    fn dispatch_source_create(
        ty: *const c_void,
        handle: usize,
        mask: usize,
        queue: DispatchObject,
    ) -> DispatchObject;
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> DispatchObject;
    fn dispatch_source_set_event_handler_f(
        source: DispatchObject,
        handler: extern "C" fn(*mut c_void),
    );
    fn dispatch_set_context(object: DispatchObject, context: *mut c_void);
    fn dispatch_source_get_data(source: DispatchObject) -> usize;
    fn dispatch_resume(object: DispatchObject);
}

/// Leaked handler context: the user callback + the source whose pressure level the
/// handler reads. Both live for the process lifetime.
struct Ctx {
    on_pressure: Box<dyn Fn(bool) + Send>,
    source: DispatchObject,
}

// SAFETY: the handler runs on a serial dispatch queue; the callback is `Send` and
// the leaked source pointer is only read, never freed.
unsafe impl Send for Ctx {}

/// The line this source logs for every level the OS reports, on the dispatch
/// thread itself. It is the run's own record of where memory pressure stood: the
/// main thread's shed line is written only when the main thread gets to it, and
/// never for a return to normal, so without this a run killed under critical
/// pressure and one that had recovered long before read the same to the next
/// launch's recovery census (`recovery_census::PRESSURE_LINE` is its prefix).
pub(crate) fn level_line(level: usize) -> String {
    let word = if level & CRITICAL != 0 {
        "critical"
    } else if level & WARN != 0 {
        "warn"
    } else {
        "normal"
    };
    format!("memory pressure: the system reports {word} (level {level:#x})")
}

/// What a reported level asks of the shed: `Some(critical)` for a warning or a
/// critical level, `None` for a return to NORMAL — registered only so the line above
/// records it; a level that is neither warning nor critical sheds nothing.
fn shed_for(level: usize) -> Option<bool> {
    (level & (WARN | CRITICAL) != 0).then_some(level & CRITICAL != 0)
}

extern "C" fn handler(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is the leaked `Box<Ctx>` installed below.
    let ctx = unsafe { &*(ctx as *const Ctx) };
    // SAFETY: `ctx.source` is the live source this handler is attached to.
    let level = unsafe { dispatch_source_get_data(ctx.source) };
    let Some(critical) = shed_for(level) else {
        aterm_log::info!("{}", level_line(level));
        return;
    };
    aterm_log::warn!("{}", level_line(level));
    // Never unwind across the C boundary.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (ctx.on_pressure)(critical);
    }));
}

/// Register the process-wide macOS memory-pressure notifier. The callback runs on a
/// libdispatch background thread and should only enqueue cheap, thread-safe work.
/// It is called for WARN and CRITICAL only; a return to NORMAL is logged
/// ([`level_line`]) and nothing else.
pub(crate) fn install<F>(on_pressure: F)
where
    F: Fn(bool) + Send + 'static,
{
    // SAFETY: standard libdispatch source setup. The source and context are
    // deliberately process-lifetime allocations so no handler can race a free.
    unsafe {
        let queue = dispatch_get_global_queue(QUEUE_PRIORITY_DEFAULT, 0);
        let source = dispatch_source_create(
            ptr::addr_of!(_dispatch_source_type_memorypressure),
            0,
            NORMAL | WARN | CRITICAL,
            queue,
        );
        if source.is_null() {
            return;
        }
        let ctx = Box::into_raw(Box::new(Ctx {
            on_pressure: Box::new(on_pressure),
            source,
        }));
        dispatch_set_context(source, ctx.cast());
        dispatch_source_set_event_handler_f(source, handler);
        dispatch_resume(source);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn registration_does_not_crash() {
        super::install(|_critical| {});
    }

    /// The source's line names each level the census reads, highest bit first, and
    /// starts with the census's prefix.
    #[test]
    fn the_level_line_names_the_level_the_census_reads() {
        use super::{CRITICAL, NORMAL, WARN, level_line};
        assert!(level_line(CRITICAL).contains("reports critical"));
        assert!(level_line(WARN | CRITICAL).contains("reports critical"));
        assert!(level_line(WARN).contains("reports warn"));
        assert!(level_line(NORMAL).contains("reports normal"));
        for level in [NORMAL, WARN, CRITICAL] {
            assert!(level_line(level).starts_with(crate::recovery_census::PRESSURE_LINE));
        }
    }

    /// Registering NORMAL must not turn a return to normal into a shed: before this
    /// source asked for it, the callback only ever saw warning and critical.
    #[test]
    fn a_return_to_normal_is_recorded_and_never_shed() {
        use super::{CRITICAL, NORMAL, WARN, shed_for};
        assert_eq!(shed_for(NORMAL), None);
        assert_eq!(shed_for(0), None);
        assert_eq!(shed_for(WARN), Some(false));
        assert_eq!(shed_for(CRITICAL), Some(true));
        assert_eq!(shed_for(WARN | CRITICAL), Some(true));
    }
}
