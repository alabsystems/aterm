// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS memory-pressure notifications. A development build can hear a stand-in system or
//! none instead ([`SYSTEM_SEAM`]); a shipped binary reads no seam and always hears the kernel.

use std::ffi::OsString;
use std::os::raw::c_void;
use std::ptr;

use crate::crash_restore::PressureLevel;

/// The system this process hears: unset, the kernel's; `normal`, `warn` or `critical`, a
/// stand-in system held at that level, which reports it once as this process installs its
/// source; [`NO_SYSTEM`], none, so only the `pressure` verb moves the level. Live test
/// instances and `aterm-lab` worlds set [`NO_SYSTEM`], so the host's own pressure never sheds
/// what a test waits on.
const SYSTEM_SEAM: &str = "ATERM_DEBUG_SYSTEM_PRESSURE";

/// [`SYSTEM_SEAM`]'s word for no system source.
const NO_SYSTEM: &str = "none";

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

// SAFETY: a dispatch source never runs its handler concurrently with itself; the callback is
// `Send` and the leaked source pointer is only read, never freed.
unsafe impl Send for Ctx {}

/// The line this source logs for every level the OS reports, on the dispatch thread itself.
pub(crate) fn level_line(mask: usize, level: Option<PressureLevel>) -> String {
    let word = level.map_or("normal", PressureLevel::word);
    format!("memory pressure: the system reports {word} (level {mask:#x})")
}

const fn level_of(level: usize) -> Option<PressureLevel> {
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

/// The level a delivered `mask` means: where it merges several, the level the system is at
/// now (`current`), else its highest.
fn resolve(mask: usize, current: impl FnOnce() -> Option<PressureLevel>) -> Option<PressureLevel> {
    let levels = mask & (NORMAL | WARN | CRITICAL);
    if levels.count_ones() > 1
        && let Some(now) = current()
    {
        return Some(now);
    }
    level_of(levels)
}

/// The system's level now (`kern.memorystatus_vm_pressure_level`, `aterm_sysprobe`'s read).
fn current_level() -> Option<PressureLevel> {
    aterm_sysprobe::memory_pressure().map(|level| match level {
        aterm_sysprobe::MemoryLevel::Normal => PressureLevel::Normal,
        aterm_sysprobe::MemoryLevel::Warn => PressureLevel::Warn,
        aterm_sysprobe::MemoryLevel::Critical => PressureLevel::Critical,
    })
}

/// What a level asks of the scrollback's shed (`App::shed_memory_under_pressure`).
pub(crate) const fn scrollback_shed(level: PressureLevel) -> Option<bool> {
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
    report(raw, resolve(raw, current_level), &*ctx.on_level);
}

/// Log what the system reported (`raw`, read as `level`) and hand `on_level` the level.
fn report(raw: usize, level: Option<PressureLevel>, on_level: &dyn Fn(PressureLevel)) {
    if level.is_none_or(|level| level == PressureLevel::Normal) {
        aterm_log::info!("{}", level_line(raw, level));
    } else {
        aterm_log::warn!("{}", level_line(raw, level));
    }
    let Some(level) = level else {
        return;
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        on_level(level);
    }));
}

/// Where this process hears the system's memory pressure from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The kernel's notifications: a shipped binary's only source.
    Kernel,
    /// A system held at this level.
    StandIn(PressureLevel),
    /// None ([`NO_SYSTEM`]): only the `pressure` verb drives the level.
    Unheard,
}

/// The source [`SYSTEM_SEAM`]'s value `system` names; a value that names neither a level nor
/// [`NO_SYSTEM`] is the kernel's.
fn source(system: Option<OsString>) -> Source {
    match system.as_deref().and_then(std::ffi::OsStr::to_str) {
        Some(NO_SYSTEM) => Source::Unheard,
        word => word
            .and_then(PressureLevel::parse)
            .map_or(Source::Kernel, Source::StandIn),
    }
}

/// The bit the kernel's source reports for `level`.
const fn mask_of(level: PressureLevel) -> usize {
    match level {
        PressureLevel::Normal => NORMAL,
        PressureLevel::Warn => WARN,
        PressureLevel::Critical => CRITICAL,
    }
}

/// Register the process-wide macOS memory-pressure notifier ([`Source`]).
pub(crate) fn install<F>(on_level: F)
where
    F: Fn(PressureLevel) + Send + 'static,
{
    match source(aterm_types::dev_seam!(SYSTEM_SEAM)) {
        Source::Unheard => {
            aterm_log::info!(
                "memory pressure: no system source ({SYSTEM_SEAM}={NO_SYSTEM}); only the \
                 `pressure` verb moves it"
            );
            return;
        }
        Source::StandIn(level) => {
            report(mask_of(level), Some(level), &on_level);
            return;
        }
        Source::Kernel => {}
    }
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
    use super::{
        CRITICAL, NO_SYSTEM, NORMAL, Source, WARN, level_line, level_of, mask_of, resolve,
        scrollback_shed, source,
    };
    use crate::crash_restore::PressureLevel;

    #[test]
    fn the_seam_picks_the_source() {
        let seam = |v: &str| source(Some(v.into()));
        assert_eq!(source(None), Source::Kernel);
        assert_eq!(seam("critical"), Source::StandIn(PressureLevel::Critical));
        assert_eq!(seam("warn"), Source::StandIn(PressureLevel::Warn));
        assert_eq!(seam("normal"), Source::StandIn(PressureLevel::Normal));
        assert_eq!(
            seam(NO_SYSTEM),
            Source::Unheard,
            "a live test's instance hears no system"
        );
        assert_eq!(seam(""), Source::Kernel);
        assert_eq!(seam("swapping"), Source::Kernel, "a word that is no level");
        for level in [
            PressureLevel::Normal,
            PressureLevel::Warn,
            PressureLevel::Critical,
        ] {
            assert_eq!(
                level_of(mask_of(level)),
                Some(level),
                "the stand-in reports the bit the kernel would"
            );
        }
    }

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
            resolve(WARN | NORMAL, now(PressureLevel::Normal)),
            Some(PressureLevel::Normal),
            "the spike eased"
        );
        assert_eq!(
            level_of(WARN | NORMAL),
            Some(PressureLevel::Warn),
            "control: the old reading kept the rings shed"
        );
        assert_eq!(
            resolve(NORMAL | WARN | CRITICAL, now(PressureLevel::Warn)),
            Some(PressureLevel::Warn)
        );
        assert_eq!(
            resolve(WARN | NORMAL, || None),
            Some(PressureLevel::Warn),
            "an unread level: the highest bit"
        );
        let unread = || -> Option<PressureLevel> { panic!("a single level is never read") };
        assert_eq!(resolve(CRITICAL, unread), Some(PressureLevel::Critical));
        assert_eq!(resolve(NORMAL, unread), Some(PressureLevel::Normal));
        assert_eq!(resolve(0, unread), None);
        assert_eq!(
            resolve(WARN | 0x10, unread),
            Some(PressureLevel::Warn),
            "a bit that is no level is not one"
        );
    }
}
