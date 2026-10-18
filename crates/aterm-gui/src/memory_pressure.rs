// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS memory-pressure notification seam.

use std::os::raw::c_void;
use std::ptr;

use crate::crash_restore::PressureLevel;

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
    on_level: Box<dyn Fn(PressureLevel) + Send>,
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

/// The level a reported mask is (`None` for a mask with no level bit, which the source
/// never sends).
fn level_of(level: usize) -> Option<PressureLevel> {
    if level & CRITICAL != 0 {
        Some(PressureLevel::Critical)
    } else if level & WARN != 0 {
        Some(PressureLevel::Warn)
    } else if level & NORMAL != 0 {
        Some(PressureLevel::Normal)
    } else {
        None
    }
}

/// What a level asks of the scrollback's shed (`App::shed_memory_under_pressure`):
/// `Some(critical)` for a warning or a critical level, `None` for a return to NORMAL.
/// NORMAL reaches the window (the crash-restore journal's shed brings its history rings
/// back once it has held, `crash_restore::ShedPolicy`) but trims nothing: before this
/// source registered NORMAL, the scrollback's shed only ever saw warning and critical.
pub(crate) fn scrollback_shed(level: PressureLevel) -> Option<bool> {
    match level {
        PressureLevel::Normal => None,
        PressureLevel::Warn => Some(false),
        PressureLevel::Critical => Some(true),
    }
}

extern "C" fn handler(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is the leaked `Box<Ctx>` installed below.
    let ctx = unsafe { &*(ctx as *const Ctx) };
    // SAFETY: `ctx.source` is the live source this handler is attached to.
    let raw = unsafe { dispatch_source_get_data(ctx.source) };
    let Some(level) = level_of(raw) else {
        aterm_log::info!("{}", level_line(raw));
        return;
    };
    if level == PressureLevel::Normal {
        aterm_log::info!("{}", level_line(raw));
    } else {
        aterm_log::warn!("{}", level_line(raw));
    }
    // Never unwind across the C boundary.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (ctx.on_level)(level);
    }));
}

/// Register the process-wide macOS memory-pressure notifier. The callback runs on a
/// libdispatch background thread and should only enqueue cheap, thread-safe work.
/// It is called for every level the OS reports: WARN and CRITICAL shed (the scrollback
/// and the crash-restore journal's history rings), NORMAL is logged ([`level_line`]) and
/// starts the journal's wait before its rings come back ([`scrollback_shed`]: the
/// scrollback takes nothing from it).
pub(crate) fn install<F>(on_level: F)
where
    F: Fn(PressureLevel) + Send + 'static,
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
            on_level: Box::new(on_level),
            source,
        }));
        dispatch_set_context(source, ctx.cast());
        dispatch_source_set_event_handler_f(source, handler);
        dispatch_resume(source);
    }
}

#[cfg(test)]
mod tests {
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

    /// Every level reaches the window, highest bit first, and a return to NORMAL never
    /// sheds the scrollback: before this source asked for NORMAL, the scrollback's shed
    /// only ever saw warning and critical, and the journal's rings coming back is NORMAL's
    /// one effect.
    #[test]
    fn a_return_to_normal_is_recorded_and_never_sheds_the_scrollback() {
        use super::{CRITICAL, NORMAL, WARN, level_of, scrollback_shed};
        use crate::crash_restore::PressureLevel;
        assert_eq!(level_of(NORMAL), Some(PressureLevel::Normal));
        assert_eq!(level_of(0), None);
        assert_eq!(level_of(WARN), Some(PressureLevel::Warn));
        assert_eq!(level_of(CRITICAL), Some(PressureLevel::Critical));
        assert_eq!(level_of(WARN | CRITICAL), Some(PressureLevel::Critical));
        assert_eq!(scrollback_shed(PressureLevel::Normal), None);
        assert_eq!(scrollback_shed(PressureLevel::Warn), Some(false));
        assert_eq!(scrollback_shed(PressureLevel::Critical), Some(true));
    }
}
