// Modified by the aterm project in 2026; see the repository NOTICE.
// (`flip_window_screen_coordinates` takes and answers `aterm_objc`'s
// `CGRect`/`CGPoint` rather than objc2's `NSRect`/`NSPoint`, because its one
// live caller — `window_delegate.rs` — computes in those. Every `objc2`
// binding call is now a typed send through `aterm_objc`, and `ns_screen`
// answers an `aterm_objc::Obj` rather than a `Retained<NSScreen>`. Search for
// the aterm local-patch marker.)
//
// NOTE ON THIS NOTICE: see the same note at the head of `view.rs`; it was
// missing here too until W8.
#![allow(clippy::unnecessary_cast)]

use std::collections::VecDeque;
use std::ffi::c_void;
use std::fmt;

use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex};
use core_foundation::base::{CFRelease, TCFType};
use core_foundation::string::CFString;
use core_foundation::uuid::{CFUUIDGetUUIDBytes, CFUUID};
use core_graphics::display::{
    CGDirectDisplayID, CGDisplay, CGDisplayBounds, CGDisplayCopyDisplayMode,
};
use tracing::warn;

// LOCAL PATCH (aterm): objc2's `Retained`/`AnyObject`, its `NSScreen` and
// `NSNumber` bindings, `ns_string!` and `objc2_foundation::run_on_main` are all
// gone. `aterm_objc::run_on_main` is the same primitive — W10 built it for
// exactly this call — and `Obj` is the +1 handle `Retained<NSScreen>` was.
use aterm_objc::send::{send_f64, send_id, send_id_id, send_id_usize, send_u32, send_usize};
use aterm_objc::{Id, Obj, autoreleasepool, class, run_on_main, sel};

use super::ffi;
use crate::dpi::{LogicalPosition, PhysicalPosition, PhysicalSize};

#[derive(Clone)]
pub struct VideoModeHandle {
    size: PhysicalSize<u32>,
    bit_depth: u16,
    refresh_rate_millihertz: u32,
    pub(crate) monitor: MonitorHandle,
    pub(crate) native_mode: NativeDisplayMode,
}

impl PartialEq for VideoModeHandle {
    fn eq(&self, other: &Self) -> bool {
        self.size == other.size
            && self.bit_depth == other.bit_depth
            && self.refresh_rate_millihertz == other.refresh_rate_millihertz
            && self.monitor == other.monitor
    }
}

impl Eq for VideoModeHandle {}

impl std::hash::Hash for VideoModeHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.size.hash(state);
        self.bit_depth.hash(state);
        self.refresh_rate_millihertz.hash(state);
        self.monitor.hash(state);
    }
}

impl std::fmt::Debug for VideoModeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoModeHandle")
            .field("size", &self.size)
            .field("bit_depth", &self.bit_depth)
            .field("refresh_rate_millihertz", &self.refresh_rate_millihertz)
            .field("monitor", &self.monitor)
            .finish()
    }
}

pub struct NativeDisplayMode(pub ffi::CGDisplayModeRef);

unsafe impl Send for NativeDisplayMode {}
unsafe impl Sync for NativeDisplayMode {}

impl Drop for NativeDisplayMode {
    fn drop(&mut self) {
        unsafe {
            ffi::CGDisplayModeRelease(self.0);
        }
    }
}

impl Clone for NativeDisplayMode {
    fn clone(&self) -> Self {
        unsafe {
            ffi::CGDisplayModeRetain(self.0);
        }
        NativeDisplayMode(self.0)
    }
}

impl VideoModeHandle {
    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn bit_depth(&self) -> u16 {
        self.bit_depth
    }

    pub fn refresh_rate_millihertz(&self) -> u32 {
        self.refresh_rate_millihertz
    }

    pub fn monitor(&self) -> MonitorHandle {
        self.monitor.clone()
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
#[derive(Clone)]
pub struct MonitorHandle(CFUUID);

// SAFETY: CFUUID is immutable.
// FIXME(madsmtm): Upstream this into `objc2-core-foundation`.
unsafe impl Send for MonitorHandle {}
unsafe impl Sync for MonitorHandle {}

type MonitorUuid = [u8; 16];

impl MonitorHandle {
    /// Internal comparisons of [`MonitorHandle`]s are done first requesting a UUID for the handle.
    fn uuid(&self) -> MonitorUuid {
        let uuid = unsafe { CFUUIDGetUUIDBytes(self.0.as_concrete_TypeRef()) };
        MonitorUuid::from([
            uuid.byte0,
            uuid.byte1,
            uuid.byte2,
            uuid.byte3,
            uuid.byte4,
            uuid.byte5,
            uuid.byte6,
            uuid.byte7,
            uuid.byte8,
            uuid.byte9,
            uuid.byte10,
            uuid.byte11,
            uuid.byte12,
            uuid.byte13,
            uuid.byte14,
            uuid.byte15,
        ])
    }

    fn display_id(&self) -> CGDirectDisplayID {
        unsafe { ffi::CGDisplayGetDisplayIDFromUUID(self.0.as_concrete_TypeRef()) }
    }

    #[track_caller]
    pub(crate) fn new(display_id: CGDirectDisplayID) -> Option<Self> {
        // kCGNullDirectDisplay
        if display_id == 0 {
            // `CGDisplayCreateUUIDFromDisplayID` checks kCGNullDirectDisplay internally.
            warn!("constructing monitor from invalid display ID 0; falling back to main monitor");
        }
        let ptr = unsafe { ffi::CGDisplayCreateUUIDFromDisplayID(display_id) };
        if ptr.is_null() {
            return None;
        }
        Some(Self(unsafe { CFUUID::wrap_under_create_rule(ptr) }))
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
            match MonitorHandle::new(display) {
                Some(monitor) => monitors.push_back(monitor),
                None => warn!("display {display} vanished during enumeration; skipping it"),
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
        PhysicalSize::from_logical::<_, f64>((width as f64, height as f64), self.scale_factor())
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

    pub fn video_modes(&self) -> impl Iterator<Item = VideoModeHandle> {
        let refresh_rate_millihertz = self.refresh_rate_millihertz().unwrap_or(0);
        let monitor = self.clone();

        unsafe {
            let modes = {
                let array = ffi::CGDisplayCopyAllDisplayModes(self.display_id(), std::ptr::null());
                if array.is_null() {
                    // Occasionally, certain CalDigit Thunderbolt Hubs report a spurious monitor
                    // during sleep/wake/cycling monitors. It tends to have null
                    // or 1 video mode only. See <https://github.com/bevyengine/bevy/issues/17827>.
                    warn!(monitor = ?self, "failed to get a list of display modes");
                    Vec::new()
                } else {
                    let array_count = CFArrayGetCount(array);
                    let modes: Vec<_> = (0..array_count)
                        .map(move |i| {
                            let mode = CFArrayGetValueAtIndex(array, i) as *mut _;
                            ffi::CGDisplayModeRetain(mode);
                            mode
                        })
                        .collect();
                    CFRelease(array as *const _);
                    modes
                }
            };

            modes.into_iter().map(move |mode| {
                let cg_refresh_rate_hertz = ffi::CGDisplayModeGetRefreshRate(mode);

                // CGDisplayModeGetRefreshRate returns 0.0 for any display that
                // isn't a CRT
                let refresh_rate_millihertz = if cg_refresh_rate_hertz > 0.0 {
                    (cg_refresh_rate_hertz * 1000.0).round() as u32
                } else {
                    refresh_rate_millihertz
                };

                let pixel_encoding =
                    CFString::wrap_under_create_rule(ffi::CGDisplayModeCopyPixelEncoding(mode))
                        .to_string();
                let bit_depth = if pixel_encoding.eq_ignore_ascii_case(ffi::IO32BitDirectPixels) {
                    32
                } else if pixel_encoding.eq_ignore_ascii_case(ffi::IO16BitDirectPixels) {
                    16
                } else if pixel_encoding.eq_ignore_ascii_case(ffi::kIO30BitDirectPixels) {
                    30
                } else {
                    unimplemented!()
                };

                VideoModeHandle {
                    size: PhysicalSize::new(
                        ffi::CGDisplayModeGetPixelWidth(mode) as u32,
                        ffi::CGDisplayModeGetPixelHeight(mode) as u32,
                    ),
                    refresh_rate_millihertz,
                    bit_depth,
                    monitor: monitor.clone(),
                    native_mode: NativeDisplayMode(mode),
                }
            })
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

    /// LOCAL PATCH (aterm), W9 phase 3: the same screen as a BARE POINTER.
    ///
    /// `crate::platform::macos`'s `MonitorHandleExtMacOS::ns_screen` — a
    /// macOS-COMPILED file that lives outside `platform_impl/` — used to write
    /// `objc2::rc::Retained::as_ptr(&s)` itself. That was the only `objc2` name
    /// left in `src/platform/`, and neither endgame metric could see it,
    /// because both were scoped to `platform_impl/macos` and `aterm-gui/src`.
    /// Widening the scope is the real fix (`tests/objc2_exit_condition.rs` now
    /// derives it from `platform/mod.rs`'s own `cfg` gates); moving the one
    /// line here is what keeps the widened count from regressing, and it puts
    /// the `objc2` name in the file that already owns five of them and is
    /// scheduled to lose all of them together.
    ///
    /// THE +0 IS UPSTREAM'S, unchanged: the handle is dropped as this returns,
    /// so the pointer is only valid because AppKit owns the screen.
    /// A public trait method answering `*mut c_void` has no other option, and
    /// re-signaturing winit's public API is not this campaign's business.
    pub(crate) fn ns_screen_ptr(&self, w: aterm_objc::MainThread) -> Option<*mut c_void> {
        self.ns_screen(w).map(|s| s.id().as_ptr())
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
        assert!(
            !number.is_null(),
            "failed getting screen display id from device description"
        );
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
// this is a re-typing and not a conversion; `CGDisplay::bounds()` is
// `core-graphics`' own type and is unchanged.
pub(crate) fn flip_window_screen_coordinates(frame: aterm_objc::CGRect) -> aterm_objc::CGPoint {
    // It is intentional that we use `CGMainDisplayID` (as opposed to
    // `NSScreen::mainScreen`), because that's what the screen coordinates
    // are relative to, no matter which display the window is currently on.
    let main_screen_height = CGDisplay::main().bounds().size.height;

    let y = main_screen_height - frame.size.height - frame.origin.y;
    aterm_objc::CGPoint { x: frame.origin.x, y }
}
