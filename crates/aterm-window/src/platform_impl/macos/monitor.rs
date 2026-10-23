// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/monitor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).
#![allow(clippy::unnecessary_cast)]

use std::collections::VecDeque;
use std::fmt;

use aterm_cg::{CGDirectDisplayID, CGDisplay, CGDisplayBounds, CGDisplayCopyDisplayMode};
use aterm_objc::cf::CFUUID;
use tracing::warn;

use aterm_objc::send::{send_f64, send_id, send_id_id, send_id_usize, send_u32, send_usize};
use aterm_objc::{autoreleasepool, class, run_on_main, sel, Id, Obj};

use super::ffi;
use crate::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};

pub struct NativeDisplayMode(pub ffi::CGDisplayModeRef);

impl Drop for NativeDisplayMode {
    fn drop(&mut self) {
        unsafe {
            ffi::CGDisplayModeRelease(self.0);
        }
    }
}

/// CoreGraphics display handle backed by a `CFUUID`.
#[derive(Clone)]
pub struct MonitorHandle(CFUUID);

type MonitorUuid = [u8; 16];

impl MonitorHandle {
    /// Returns the [`MonitorUuid`] for comparing [`MonitorHandle`]s.
    fn uuid(&self) -> MonitorUuid {
        self.0.bytes()
    }

    fn display_id(&self) -> CGDirectDisplayID {
        unsafe { ffi::CGDisplayGetDisplayIDFromUUID(self.0.as_ptr()) }
    }

    #[track_caller]
    pub(crate) fn new(display_id: CGDirectDisplayID) -> Option<Self> {
        if display_id == 0 {
            warn!("constructing monitor from invalid display ID 0; falling back to main monitor");
        }
        // SAFETY: `CGDisplayCreateUUIDFromDisplayID` returns NULL or a +1 CFUUID.
        unsafe { CFUUID::from_create(ffi::CGDisplayCreateUUIDFromDisplayID(display_id)) }.map(Self)
    }
}

impl PartialEq for MonitorHandle {
    fn eq(&self, other: &Self) -> bool {
        self.uuid() == other.uuid()
    }
}

impl Eq for MonitorHandle {}

impl PartialOrd for MonitorHandle {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MonitorHandle {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.uuid().cmp(&other.uuid())
    }
}

impl std::hash::Hash for MonitorHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.uuid().hash(state);
    }
}

/// Returns all active monitors with valid display UUIDs.
pub fn available_monitors() -> VecDeque<MonitorHandle> {
    if let Ok(displays) = CGDisplay::active_displays() {
        let mut monitors = VecDeque::with_capacity(displays.len());
        for display in displays {
            // LOCAL PATCH (aterm): upstream unwraps here ("just fetched from
            // `CGGetActiveDisplayList`, should be fine"). A display can leave
            // between that list and `CGDisplayCreateUUIDFromDisplayID` —
            // unplug, sleep/wake, an arrangement change — and under aterm's
            // trampoline policy the panic is a process abort from inside
            // whatever AppKit callback asked for the monitor list. Skip the
            // vanished display and say so.
            if let Some(monitor) = MonitorHandle::new(display) {
                monitors.push_back(monitor);
            } else {
                warn!("display {display} vanished during enumeration; skipping it");
            }
        }
        monitors
    } else {
        VecDeque::with_capacity(0)
    }
}

/// Returns the primary monitor, or `None` during display reconfiguration.
pub fn primary_monitor() -> Option<MonitorHandle> {
    MonitorHandle::new(CGDisplay::main().id).or_else(|| {
        warn!("main display has no UUID right now; falling back to the first active display");
        available_monitors().pop_front()
    })
}

impl fmt::Debug for MonitorHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MonitorHandle")
            .field("name", &self.name())
            .field("native_identifier", &self.native_identifier())
            .field("size", &self.size())
            .field("position", &self.position())
            .field("scale_factor", &self.scale_factor())
            .field("refresh_rate_millihertz", &self.refresh_rate_millihertz())
            .finish_non_exhaustive()
    }
}

impl MonitorHandle {
    pub fn name(&self) -> Option<String> {
        let screen_num = CGDisplay::new(self.display_id()).model_number();
        Some(format!("Monitor #{screen_num}"))
    }

    #[inline]
    pub fn native_identifier(&self) -> u32 {
        self.display_id()
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        let display = CGDisplay::new(self.display_id());
        let height = display.pixels_high();
        let width = display.pixels_wide();
        LogicalSize::new(width as f64, height as f64).to_physical(self.scale_factor())
    }

    #[inline]
    pub fn position(&self) -> PhysicalPosition<i32> {
        let bounds = unsafe { CGDisplayBounds(self.native_identifier()) };
        let position = LogicalPosition::new(bounds.origin.x, bounds.origin.y);
        position.to_physical(self.scale_factor())
    }

    pub fn scale_factor(&self) -> f64 {
        run_on_main(|mtm| match self.ns_screen(mtm) {
            // SAFETY: `screen` owns a live `NSScreen` and `-backingScaleFactor` has signature `d16@0:8`.
            Some(screen) => unsafe { send_f64(screen.id(), sel!(backingScaleFactor)) },
            None => 1.0,
        })
    }

    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        unsafe {
            let current_display_mode =
                NativeDisplayMode(CGDisplayCopyDisplayMode(self.display_id()) as _);
            let refresh_rate = ffi::CGDisplayModeGetRefreshRate(current_display_mode.0);
            if refresh_rate > 0.0 {
                return Some((refresh_rate * 1000.0).round() as u32);
            }

            let mut display_link = std::ptr::null_mut();
            if ffi::CVDisplayLinkCreateWithCGDisplay(self.display_id(), &mut display_link)
                != ffi::kCVReturnSuccess
            {
                return None;
            }
            let time = ffi::CVDisplayLinkGetNominalOutputVideoRefreshPeriod(display_link);
            ffi::CVDisplayLinkRelease(display_link);

            if time.flags & ffi::kCVTimeIsIndefinite != 0 {
                return None;
            }

            (time.time_scale as i64).checked_div(time.time_value).map(|v| (v * 1000) as u32)
        }
    }

    /// Returns the retained `NSScreen` for this monitor, or `None` if detached.
    pub(crate) fn ns_screen(&self, w: aterm_objc::MainThread) -> Option<Obj> {
        let _ = w;
        let uuid = self.uuid();
        autoreleasepool(|_| {
            // SAFETY: `+screens` is `@16@0:8` and answers a live `NSArray`;
            // `-count` is `Q16@0:8` and `-objectAtIndex:` is `@24@0:8Q16`,
            // which cannot be out of range for an index below the count.
            unsafe {
                let screens = send_id(class(c"NSScreen").as_id(), sel!(screens));
                let n = send_usize(screens, sel!(count));
                for i in 0..n {
                    let screen = send_id_usize(screens, sel!(objectAtIndex:), i);
                    let other_native_id = get_display_id(screen);
                    if let Some(other) = MonitorHandle::new(other_native_id) {
                        if uuid == other.uuid() {
                            return Obj::retain(screen);
                        }
                    } else {
                        warn!(other_native_id, "comparing against screen with invalid display ID");
                    }
                }
                None
            }
        })
    }
}

/// Returns the `CGDirectDisplayID` for a live `NSScreen`.
pub(crate) fn get_display_id(screen: Id) -> u32 {
    // SAFETY: `-deviceDescription` is `@16@0:8` on `NSScreen` and answers a +0
    // autoreleased `NSDictionary` whose entries are borrowed for as long as it
    // is; `-objectForKey:` is `@24@0:8@16`. The value at `@"NSScreenNumber"` is
    // documented to be an `NSNumber`, and `-unsignedIntValue` is `I16@0:8` — an
    // `unsigned int`, matching `CGDirectDisplayID`'s `uint32_t`, which is why
    // this is `send_u32` and not `send_usize`.
    // <https://developer.apple.com/documentation/appkit/nsscreen/1388360-devicedescription?language=objc>
    autoreleasepool(|_| unsafe {
        let key = aterm_objc::ns_string("NSScreenNumber").expect("a Foundation string");
        let device_description = send_id(screen, sel!(deviceDescription));
        let number = send_id_id(device_description, sel!(objectForKey:), key.id());
        assert!(!number.is_null(), "failed getting screen display id from device description");
        send_u32(number, sel!(unsignedIntValue))
    })
}

/// Converts between CoreGraphics and AppKit screen coordinates.
pub(crate) fn flip_window_screen_coordinates(frame: aterm_objc::CGRect) -> aterm_objc::CGPoint {
    let main_screen_height = CGDisplay::main().bounds().size.height;

    let y = main_screen_height - frame.size.height - frame.origin.y;
    aterm_objc::CGPoint { x: frame.origin.x, y }
}
