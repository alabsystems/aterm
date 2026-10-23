// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS memory-pressure notification seam.

use std::os::raw::c_void;
use std::ptr;

use crate::crash_restore::PressureLevel;

type DispatchObject = *mut c_void;

const NORMAL: usize = 0x01;
const WARN: usize = 0x02;
const CRITICAL: usize = 0x04;
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

struct Ctx {
    on_level: Box<dyn Fn(PressureLevel) + Send>,
    source: DispatchObject,
}

// SAFETY: the handler runs on a serial dispatch queue; the callback is `Send` and the leaked source
// pointer is only read, never freed.
unsafe impl Send for Ctx {}

/// The line this source logs for every level the OS reports, on the dispatch thread itself.
pub(crate) fn level_line(mask: usize, level: Option<PressureLevel>) -> String {
    let word = level.map_or("normal", PressureLevel::word);
    format!("memory pressure: the system reports {word} (level {mask:#x})")
}

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

fn resolve(mask: usize, current: impl FnOnce() -> Option<usize>) -> Option<PressureLevel> {
    let levels = mask & (NORMAL | WARN | CRITICAL);
    if levels.count_ones() > 1
        && let Some(now) = current()
            .filter(|now| now.count_ones() == 1)
            .and_then(level_of)
    {
        return Some(now);
    }
    level_of(levels)
}

fn current_level() -> Option<usize> {
    let mut level: libc::c_int = 0;
    let mut len: libc::size_t = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is a NUL-terminated literal; `level` and `len` are live locals written only
    // during the call, `len` holds the buffer's real size, and the new-value pair is null/0 (read
    // only).
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&mut level as *mut libc::c_int).cast::<libc::c_void>(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<libc::c_int>() {
        return None;
    }
    usize::try_from(level)
        .ok()
        .filter(|level| [NORMAL, WARN, CRITICAL].contains(level))
}

/// What a level asks of the scrollback's shed (`App::shed_memory_under_pressure`).
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
    let level = resolve(raw, current_level);
    if level.is_none_or(|level| level == PressureLevel::Normal) {
        aterm_log::info!("{}", level_line(raw, level));
    } else {
        aterm_log::warn!("{}", level_line(raw, level));
    }
    let Some(level) = level else {
        return;
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (ctx.on_level)(level);
    }));
}

/// Register the process-wide macOS memory-pressure notifier.
pub(crate) fn install<F>(on_level: F)
where
    F: Fn(PressureLevel) + Send + 'static,
{
    // SAFETY: standard libdispatch source setup.
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
    use super::{CRITICAL, NORMAL, WARN, level_line, level_of, resolve, scrollback_shed};
    use crate::crash_restore::PressureLevel;

    #[test]
    fn the_level_line_names_the_level_the_census_reads() {
        let line = |mask| level_line(mask, level_of(mask));
        assert!(line(CRITICAL).contains("reports critical"));
        assert!(line(WARN | CRITICAL).contains("reports critical"));
        assert!(line(WARN).contains("reports warn"));
        assert!(line(NORMAL).contains("reports normal"));
        assert!(
            level_line(WARN | NORMAL, Some(PressureLevel::Normal))
                .contains("reports normal (level 0x3)"),
            "a merged mask's line says where the pressure stands, and what was delivered"
        );
        for level in [NORMAL, WARN, CRITICAL] {
            assert!(line(level).starts_with(crate::recovery_census::PRESSURE_LINE));
        }
    }

    #[test]
    fn a_return_to_normal_is_recorded_and_never_sheds_the_scrollback() {
        assert_eq!(level_of(NORMAL), Some(PressureLevel::Normal));
        assert_eq!(level_of(0), None);
        assert_eq!(level_of(WARN), Some(PressureLevel::Warn));
        assert_eq!(level_of(CRITICAL), Some(PressureLevel::Critical));
        assert_eq!(level_of(WARN | CRITICAL), Some(PressureLevel::Critical));
        assert_eq!(scrollback_shed(PressureLevel::Normal), None);
        assert_eq!(scrollback_shed(PressureLevel::Warn), Some(false));
        assert_eq!(scrollback_shed(PressureLevel::Critical), Some(true));
    }

    #[test]
    fn a_merged_mask_is_read_as_the_level_the_system_is_at_now() {
        let now = |level| move || Some(level);
        assert_eq!(
            resolve(WARN | NORMAL, now(NORMAL)),
            Some(PressureLevel::Normal),
            "the spike eased"
        );
        assert_eq!(
            level_of(WARN | NORMAL),
            Some(PressureLevel::Warn),
            "control: the old reading kept the rings shed"
        );
        assert_eq!(
            resolve(NORMAL | WARN | CRITICAL, now(WARN)),
            Some(PressureLevel::Warn)
        );
        assert_eq!(
            resolve(WARN | NORMAL, || None),
            Some(PressureLevel::Warn),
            "an unread level: the highest bit"
        );
        assert_eq!(
            resolve(WARN | NORMAL, now(WARN | CRITICAL)),
            Some(PressureLevel::Warn),
            "an answer that is no one level settles nothing"
        );
        let unread = || -> Option<usize> { panic!("a single level is never read") };
        assert_eq!(resolve(CRITICAL, unread), Some(PressureLevel::Critical));
        assert_eq!(resolve(NORMAL, unread), Some(PressureLevel::Normal));
        assert_eq!(resolve(0, unread), None);
        assert_eq!(
            resolve(WARN | 0x10, unread),
            Some(PressureLevel::Warn),
            "a bit that is no level is not one"
        );
    }

    #[test]
    fn the_systems_level_reads_as_one_of_the_sources_bits() {
        let level = super::current_level().expect("kern.memorystatus_vm_pressure_level");
        assert!([NORMAL, WARN, CRITICAL].contains(&level), "{level:#x}");
    }
}
