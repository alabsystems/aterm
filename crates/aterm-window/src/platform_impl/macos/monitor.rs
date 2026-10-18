// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/monitor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE.
// (`flip_window_screen_coordinates` takes and answers `aterm_objc`'s
// `CGRect`/`CGPoint` rather than objc2's `NSRect`/`NSPoint`, because its one
// live caller — `window_delegate.rs` — computes in those. Every `objc2`
// binding call is now a typed send through `aterm_objc`, and `ns_screen`
// answers an `aterm_objc::Obj` rather than a `Retained<NSScreen>`. The
// CoreFoundation and CoreGraphics calls go through `aterm_objc::cf`/`cg`
// rather than the `core-foundation`/`core-graphics` crates, and
// `MonitorHandle` holds an `aterm_objc::cf::CFUUID`. Search for the aterm
// local-patch marker.)
//
// NOTE ON THIS NOTICE: see the same note at the head of `view.rs`; it was
// missing here too until W8.
#![allow(clippy::unnecessary_cast)]

use std::collections::VecDeque;
use std::fmt;

use aterm_cg::{CGDirectDisplayID, CGDisplay, CGDisplayBounds, CGDisplayCopyDisplayMode};
use aterm_objc::cf::CFUUID;
use tracing::warn;

// LOCAL PATCH (aterm): objc2's `Retained`/`AnyObject`, its `NSScreen` and
// `NSNumber` bindings, `ns_string!` and `objc2_foundation::run_on_main` are all
// gone. `aterm_objc::run_on_main` is the same primitive — W10 built it for
// exactly this call — and `Obj` is the +1 handle `Retained<NSScreen>` was.
use aterm_objc::send::{send_f64, send_id, send_id_id, send_id_usize, send_u32, send_usize};
use aterm_objc::{autoreleasepool, class, run_on_main, sel, Id, Obj};

use super::ffi;
use crate::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};

// LOCAL PATCH (aterm), 2026-10-16 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 5):
// `VideoModeHandle` and `video_modes` are gone with exclusive fullscreen, the one thing a video
// mode was for (no aterm crate asked for either). `NativeDisplayMode` stays as the owner of the
// current mode `refresh_rate_millihertz` reads, without the `Clone`/`Send`/`Sync` a handle that
// was passed around needed.
pub struct NativeDisplayMode(pub ffi::CGDisplayModeRef);

impl Drop for NativeDisplayMode {
    fn drop(&mut self) {
        unsafe {
            ffi::CGDisplayModeRelease(self.0);
        }
    }
}

/// `CGDirectDisplayID` is documented as:
/// > a framebuffer, a color correction (gamma) table, and possibly an attached monitor.
///
/// That is, it doesn't actually represent the monitor itself. Instead, we use the UUID of the
/// monitor, as retrieved from `CGDisplayCreateUUIDFromDisplayID` (this makes the monitor ID stable,
/// even across reboots and video mode changes).
///
/// NOTE: I'd be perfectly valid to store `[u8; 16]` in here instead, we only store `CFUUID` to
/// avoid having to re-create it when we want to fetch the display ID.
///
/// LOCAL PATCH (aterm): `aterm_objc::cf::CFUUID` is `Send + Sync` itself (a CFUUID is
/// immutable), so the two `unsafe impl`s upstream wrote for this type are gone.
#[derive(Clone)]
pub struct MonitorHandle(CFUUID);

type MonitorUuid = [u8; 16];

impl MonitorHandle {
    /// Internal comparisons of [`MonitorHandle`]s are done first requesting a UUID for the handle.
    fn uuid(&self) -> MonitorUuid {
        self.0.bytes()
    }

    fn display_id(&self) -> CGDirectDisplayID {
        unsafe { ffi::CGDisplayGetDisplayIDFromUUID(self.0.as_ptr()) }
    }

    #[track_caller]
    pub(crate) fn new(display_id: CGDirectDisplayID) -> Option<Self> {
        // kCGNullDirectDisplay
        if display_id == 0 {
            // `CGDisplayCreateUUIDFromDisplayID` checks kCGNullDirectDisplay internally.
            warn!("constructing monitor from invalid display ID 0; falling back to main monitor");
        }
        // SAFETY: `CGDisplayCreateUUIDFromDisplayID` answers NULL or a +1 CFUUID (Create rule).
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

/// LOCAL PATCH (aterm): upstream unwraps the main display's UUID. It is `None`
/// for an instant mid-reconfiguration, and under aterm's trampoline policy
/// that panic is a process abort. Fall back to the first active display, and
/// to `None` — which the callers in event_loop.rs / window_delegate.rs already
/// return — when there is none.
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
    // TODO: Be smarter about this:
    // <https://github.com/glfw/glfw/blob/57cbded0760a50b9039ee0cb3f3c14f60145567c/src/cocoa_monitor.m#L44-L126>
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
        // This is already in screen coordinates. If we were using `NSScreen`,
        // then a conversion would've been needed:
        // flip_window_screen_coordinates(self.ns_screen(mtm)?.frame())
        let bounds = unsafe { CGDisplayBounds(self.native_identifier()) };
        let position = LogicalPosition::new(bounds.origin.x, bounds.origin.y);
        position.to_physical(self.scale_factor())
    }

    pub fn scale_factor(&self) -> f64 {
        // LOCAL PATCH (aterm): `aterm_objc::run_on_main` — W10 built it as the
        // first-party twin of `objc2_foundation::run_on_main`, and it hands the
        // closure a `MainThread` witness directly, so the seam crossing that
        // used to sit on this line is gone.
        run_on_main(|mtm| match self.ns_screen(mtm) {
            // SAFETY: `screen` owns a +1 to a live `NSScreen`;
            // `-backingScaleFactor` is `d16@0:8`.
            Some(screen) => unsafe { send_f64(screen.id(), sel!(backingScaleFactor)) },
            None => 1.0, // default to 1.0 when we can't find the screen
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

            // This value is indefinite if an invalid display link was specified
            if time.flags & ffi::kCVTimeIsIndefinite != 0 {
                return None;
            }

            (time.time_scale as i64).checked_div(time.time_value).map(|v| (v * 1000) as u32)
        }
    }

    /// The `NSScreen` this monitor names, +1, or `None` if it is not attached.
    ///
    /// LOCAL PATCH (aterm), W9: takes `aterm_objc::MainThread`, because its two
    /// cross-file callers are in `window_delegate.rs`, which is ported and holds
    /// a witness.
    ///
    /// LOCAL PATCH (aterm), W12: answers an `Obj`, not a `Retained<NSScreen>`.
    /// THIS SIGNATURE WAS A CROSS-FILE CONSUMPTION THE ENDGAME METRIC COULD NOT
    /// SEE: `window_delegate.rs` is off the list and held no `objc2` token on
    /// either of its two call lines, because `seam::obj_of<T>` took the binding
    /// type through a GENERIC parameter. Porting this deletes both.
    ///
    /// The witness is kept though nothing consumes one — `+[NSScreen screens]`
    /// never needed a marker at the runtime; objc2's `MainThreadOnly` demanded
    /// it of every `NSScreen` method — for `menu.rs`'s reason: dropping a
    /// requirement upstream enforced is a behaviour change, not a port.
    ///
    /// The array is walked by INDEX: `Retained<NSArray>::into_iter()` compiled
    /// to `-countByEnumeratingWithState:objects:count:`, whose mutation guard
    /// and stack buffer are a capability this crate does not have and does not
    /// need for a handful of displays. Both sends are already censused.
    pub(crate) fn ns_screen(&self, w: aterm_objc::MainThread) -> Option<Obj> {
        let _ = w;
        let uuid = self.uuid();
        // The array and its elements are +0 autoreleased, so the pool is
        // explicit and the ONE screen that is answered is retained out of it.
        autoreleasepool(|_| {
            // SAFETY: `+screens` is `@16#0:8` and answers a live `NSArray`;
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
                        // Display ID was just fetched from live NSScreen, but can still result in
                        // `None` with certain Thunderbolt docked monitors.
                        warn!(other_native_id, "comparing against screen with invalid display ID");
                    }
                }
                None
            }
        })
    }
}

/// The `CGDirectDisplayID` behind a live `NSScreen`.
///
/// LOCAL PATCH (aterm): takes a raw `id`, not an `&NSScreen`. That PARAMETER
/// TYPE was the second cross-file consumption in this file —
/// `window_delegate.rs::current_monitor_inner` had no `objc2` token on its call
/// line and reached it through `seam::objc2_ref`.
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

/// Core graphics screen coordinates are relative to the top-left corner of
/// the so-called "main" display, with y increasing downwards - which is
/// exactly what we want in Winit.
///
/// However, `NSWindow` and `NSScreen` changes these coordinates to:
/// 1. Be relative to the bottom-left corner of the "main" screen.
/// 2. Be relative to the bottom-left corner of the window/screen itself.
/// 3. Have y increasing upwards.
///
/// This conversion happens to be symmetric, so we only need this one function
/// to convert between the two coordinate systems.
// LOCAL PATCH (aterm): `NSRect`/`NSPoint` -> `aterm_objc::CGRect`/`CGPoint`.
// This function's ONLY caller is `window_delegate.rs`, whose bindings W8 ported
// (the second call in this file is commented out, six lines above), and the
// geometry it now computes in is that file's. The two struct pairs are
// `#[repr(C)]` over `f64` in the same order on every 64-bit Apple target, so
// this is a re-typing and not a conversion. `CGDisplay::bounds()` answers
// `aterm_objc::CGRect` too, now that `CGDisplay` is `aterm_cg`'s.
pub(crate) fn flip_window_screen_coordinates(frame: aterm_objc::CGRect) -> aterm_objc::CGPoint {
    // It is intentional that we use `CGMainDisplayID` (as opposed to
    // `NSScreen::mainScreen`), because that's what the screen coordinates
    // are relative to, no matter which display the window is currently on.
    let main_screen_height = CGDisplay::main().bounds().size.height;

    let y = main_screen_height - frame.size.height - frame.origin.y;
    aterm_objc::CGPoint { x: frame.origin.x, y }
}
