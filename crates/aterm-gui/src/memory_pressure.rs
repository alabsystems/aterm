// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS memory-pressure notification seam.

use std::os::raw::c_void;
use std::ptr;

type DispatchObject = *mut c_void;

// DISPATCH_MEMORYPRESSURE_* mask bits (dispatch/source.h).
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

extern "C" fn handler(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is the leaked `Box<Ctx>` installed below.
    let ctx = unsafe { &*(ctx as *const Ctx) };
    // SAFETY: `ctx.source` is the live source this handler is attached to.
    let level = unsafe { dispatch_source_get_data(ctx.source) };
    let critical = level & CRITICAL != 0;
    // Never unwind across the C boundary.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (ctx.on_pressure)(critical);
    }));
}

/// Register the process-wide macOS memory-pressure notifier. The callback runs on a
/// libdispatch background thread and should only enqueue cheap, thread-safe work.
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
            WARN | CRITICAL,
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
}
