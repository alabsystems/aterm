// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! aterm's icon as a decoded bitmap made off the main thread, for the views that show it: the
//! permission walk's helper tile and every alert aterm builds.
//!
//! The system's image of an app's icon (`-[NSWorkspace iconForFile:]`, `NSApplicationIcon`) is an
//! IconServices image that generates its pixels when it first draws, with a synchronous XPC to
//! `iconservicesagent`. On a loaded Mac that took 7.76 s on the main thread inside a Core Animation
//! commit (a macOS 27 first-launch row, 2026-10-28). Here a worker thread asks IconServices, under
//! the appearance of the view that shows the icon so the system icon style (Dark, Tinted, Clear)
//! applies, and draws the answer into a bitmap of its own; only that decoded bitmap crosses to
//! the main thread.

/// The icon's side in pixels: 64 points (the tile's icon and an alert's) at 4x, past the sharpest
/// backing scale a Mac screen has. One bitmap serves every screen a window moves to, and AppKit
/// scales it down sharply at 1x and 2x; it is 256 KiB.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const PIXELS: usize = 256;

/// Which of an icon file's images to draw `want` pixels wide, by their pixel `widths` in file
/// order (0 for one whose width is unknown): the narrowest at least that wide, scaled down and
/// sharp, else the widest; `None` when no width is known.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn pick_icon_image(widths: &[usize], want: usize) -> Option<usize> {
    let known = || widths.iter().copied().enumerate().filter(|&(_, w)| w > 0);
    known()
        .filter(|&(_, w)| w >= want)
        .min_by_key(|&(_, w)| w)
        .or_else(|| known().max_by_key(|&(_, w)| w))
        .map(|(at, _)| at)
}

/// Make an icon off the calling thread, so the caller never waits for it: `render` runs on a lane
/// thread of its own with `role` and `deliver` takes what it made there. On macOS `render` reaches
/// IconServices, whose icon generation is a synchronous XPC to `iconservicesagent`. False, with
/// both closures dropped on the caller, when the thread did not start.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn render_off_thread<T: Send + 'static>(
    role: crate::qos::Role,
    render: impl FnOnce() -> Option<T> + Send + 'static,
    deliver: impl FnOnce(Option<T>) + Send + 'static,
) -> bool {
    match crate::qos::spawn_lane_thread("aterm-icon", role, move || deliver(render())) {
        Ok(_) => true,
        Err(e) => {
            aterm_log::warn!("app icon: the icon worker could not start: {e}");
            false
        }
    }
}

/// The icons made for alerts, one per appearance name (a "look"), and the looks a worker is
/// making.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) struct Shelf<T> {
    made: Vec<(String, T)>,
    asked: Vec<String>,
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
impl<T> Shelf<T> {
    pub(crate) const fn new() -> Self {
        Self {
            made: Vec::new(),
            asked: Vec::new(),
        }
    }

    /// The icon to show under `look`: the one made for it, else one made for another look (a
    /// style off for a moment beats a main-thread IconServices draw), else none.
    pub(crate) fn pick(&self, look: &str) -> Option<&T> {
        self.made
            .iter()
            .find(|(made, _)| made == look)
            .or_else(|| self.made.first())
            .map(|(_, icon)| icon)
    }

    /// Whether a worker should make the icon for `look`: true once, until [`Self::put`] or
    /// [`Self::forget`] for it.
    pub(crate) fn wants(&mut self, look: &str) -> bool {
        if self.made.iter().any(|(made, _)| made == look) || self.asked.iter().any(|a| a == look) {
            return false;
        }
        self.asked.push(look.to_owned());
        true
    }

    /// The worker for `look` delivered.
    pub(crate) fn put(&mut self, look: &str, icon: Option<T>) {
        self.forget(look);
        if let Some(icon) = icon {
            self.made.retain(|(made, _)| made != look);
            self.made.push((look.to_owned(), icon));
        }
    }

    /// The worker for `look` did not start or made nothing: a later ask may try again.
    pub(crate) fn forget(&mut self, look: &str) {
        self.asked.retain(|asked| asked != look);
    }
}

#[cfg(target_os = "macos")]
pub(crate) use live::{Bitmap, alert, look_of, ns_image, render, warm_for_alerts};

#[cfg(target_os = "macos")]
mod live {
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::path::{Path, PathBuf};
    use std::ptr::{self, NonNull};
    use std::rc::Rc;
    use std::sync::Mutex;

    use aterm_cg::{CGImageRef, CGImageRelease};
    use aterm_objc::{CGSize, Id, Obj, RcBlock, Sel, class, sel};

    use super::{PIXELS, Shelf, pick_icon_image, render_off_thread};
    use crate::appkit::{self, MainThread};
    use crate::cg_capture::{
        CGBitmapContextCreate, CGColorSpaceCreateWithName, CGColorSpaceRelease, CGContextDrawImage,
        CGContextRef, CGContextRelease, CGPoint, CGRect,
    };

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        static kCGColorSpaceSRGB: *const c_void;
        fn CGImageRetain(image: CGImageRef) -> CGImageRef;
        fn CGBitmapContextCreateImage(context: CGContextRef) -> CGImageRef;
        fn CGContextSetInterpolationQuality(context: CGContextRef, quality: i32);
    }

    #[link(name = "ImageIO", kind = "framework")]
    unsafe extern "C" {
        static kCGImagePropertyPixelWidth: *const c_void;
        fn CGImageSourceCreateWithURL(url: *const c_void, options: *const c_void) -> *mut c_void;
        fn CGImageSourceGetCount(source: *mut c_void) -> usize;
        fn CGImageSourceCopyPropertiesAtIndex(
            source: *mut c_void,
            index: usize,
            options: *const c_void,
        ) -> *const c_void;
        fn CGImageSourceCreateImageAtIndex(
            source: *mut c_void,
            index: usize,
            options: *const c_void,
        ) -> *mut c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            length: isize,
            is_directory: u8,
        ) -> *const c_void;
        fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberGetValue(number: *const c_void, the_type: isize, value: *mut c_void) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    /// `kCFNumberSInt64Type`.
    const CF_NUMBER_SINT64: isize = 4;

    /// `kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little`: BGRA, the layout Core
    /// Graphics and the window server draw without converting.
    pub(super) const BITMAP_INFO: u32 = 2 | (2 << 12);

    /// `kCGInterpolationHigh`.
    const INTERPOLATION_HIGH: i32 = 3;

    /// aterm's own icon in its bundle: `CFBundleIconFile` is `aterm` in every bundle `ship`
    /// builds (`aterm-release`'s `stamp_info_plist`).
    const BUNDLE_ICON: &str = "Contents/Resources/aterm.icns";

    /// An alert's icon, in points.
    const ALERT_ICON: f64 = 64.0;

    /// The icons made for alerts, by look.
    static ALERT_ICONS: Mutex<Shelf<Bitmap>> = Mutex::new(Shelf::new());

    /// A CGImage, held +1 and released wherever it drops.
    pub(crate) struct Bitmap(NonNull<aterm_cg::CGImage>);

    // SAFETY: a CGImage is immutable once made, and `CGImageRetain`/`CGImageRelease` are atomic
    // reference counts any thread may call (Core Foundation's rule for immutable objects); every
    // `Bitmap` [`render`] hands out is one `CGBitmapContextCreateImage` made over pixels already
    // drawn, with no image source, file or IconServices image behind it, and the worker keeps no
    // reference.
    unsafe impl Send for Bitmap {}

    impl Bitmap {
        /// Take ownership of a +1 CGImage, or `None` for null.
        fn adopt(image: CGImageRef) -> Option<Self> {
            NonNull::new(image).map(Self)
        }

        /// Retain a borrowed CGImage, or `None` for null.
        ///
        /// # Safety
        /// `image` is null or a live CGImage.
        unsafe fn retain(image: CGImageRef) -> Option<Self> {
            if image.is_null() {
                return None;
            }
            // SAFETY: a live CGImage, as the caller says; the +1 is this value's.
            Self::adopt(unsafe { CGImageRetain(image) })
        }

        /// The CGImage, borrowed.
        pub(crate) fn as_ptr(&self) -> CGImageRef {
            self.0.as_ptr()
        }
    }

    impl Drop for Bitmap {
        fn drop(&mut self) {
            // SAFETY: the +1 `adopt` took, released once.
            unsafe { CGImageRelease(self.0.as_ptr()) };
        }
    }

    /// The icon of the app bundle at `bundle` as [`PIXELS`] square of decoded pixels, made on the
    /// calling thread, which is never the main one: the system's icon for the bundle drawn under
    /// the appearance named `look` (so it carries the system icon style, as Finder, the Dock and
    /// System Settings show it), else the bundle's own `aterm.icns` read through ImageIO.
    pub(crate) fn render(bundle: &Path, look: Option<&str>) -> Option<Bitmap> {
        aterm_objc::autoreleasepool(|_| {
            // SAFETY: off the main thread inside this pool, as each function asks; each source,
            // image and dictionary is made, read and released on this thread.
            let image = unsafe { system_icon(bundle, look) }
                .or_else(|| unsafe { icns_image(&bundle.join(BUNDLE_ICON), PIXELS) })?;
            // SAFETY: a live CGImage this frame holds.
            unsafe { decoded(image.as_ptr(), PIXELS) }
        })
    }

    /// `image` drawn into a bitmap context of its own, `pixels` square, and that context's image:
    /// pixels decoded and scaled here, so whatever thread draws the result reads memory and
    /// never decodes, reads a file or asks IconServices (an image straight from ImageIO, or from
    /// an IconServices image, defers those to the first draw).
    ///
    /// # Safety
    /// `image` is a live CGImage.
    pub(super) unsafe fn decoded(image: CGImageRef, pixels: usize) -> Option<Bitmap> {
        let side = pixels as f64;
        // SAFETY: the Create rule gives +1 on the colour space, the context and the image; the
        // colour space and context are released once here and the image's +1 goes to the
        // `Bitmap`. A null data pointer and 0 bytes per row let Core Graphics own the buffer, and
        // `CGBitmapContextCreateImage` copies (or copies on write) it, so the image outlives the
        // context. `kCGColorSpaceSRGB` is a constant CFString.
        unsafe {
            let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
            if space.is_null() {
                return None;
            }
            let context =
                CGBitmapContextCreate(ptr::null_mut(), pixels, pixels, 8, 0, space, BITMAP_INFO);
            CGColorSpaceRelease(space);
            if context.is_null() {
                return None;
            }
            CGContextSetInterpolationQuality(context, INTERPOLATION_HIGH);
            CGContextDrawImage(context, square(side), image);
            let made = CGBitmapContextCreateImage(context);
            CGContextRelease(context);
            Bitmap::adopt(made)
        }
    }

    fn square(side: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: crate::cg_capture::CGSize {
                width: side,
                height: side,
            },
        }
    }

    /// The image of the icon file at `icns` best for `pixels` ([`pick_icon_image`]), as ImageIO
    /// gives it (still to decode: see [`decoded`]).
    ///
    /// # Safety
    /// Any thread. ImageIO documents no threading rule of its own; the source here is made, read
    /// and released on the calling thread and shared with none.
    pub(super) unsafe fn icns_image(icns: &Path, pixels: usize) -> Option<Bitmap> {
        use std::os::unix::ffi::OsStrExt;
        if !icns.is_file() {
            return None;
        }
        let bytes = icns.as_os_str().as_bytes();
        let length = isize::try_from(bytes.len()).ok()?;
        // SAFETY: the Create rule gives +1 on the URL, the source and the image, and the Copy
        // rule +1 on each properties dictionary: each is released once below (the image's +1 goes
        // to the `Bitmap`); `CFDictionaryGetValue` borrows (Get rule) a value that lives as long
        // as its dictionary, and its type is checked before `CFNumberGetValue` writes the `i64`.
        unsafe {
            let url =
                CFURLCreateFromFileSystemRepresentation(ptr::null(), bytes.as_ptr(), length, 0);
            if url.is_null() {
                return None;
            }
            let source = CGImageSourceCreateWithURL(url, ptr::null());
            CFRelease(url);
            if source.is_null() {
                return None;
            }
            let widths: Vec<usize> = (0..CGImageSourceGetCount(source))
                .map(|index| {
                    let props = CGImageSourceCopyPropertiesAtIndex(source, index, ptr::null());
                    if props.is_null() {
                        return 0;
                    }
                    let number = CFDictionaryGetValue(props, kCGImagePropertyPixelWidth);
                    let mut width = 0_i64;
                    let read = !number.is_null()
                        && CFGetTypeID(number) == CFNumberGetTypeID()
                        && CFNumberGetValue(
                            number,
                            CF_NUMBER_SINT64,
                            (&raw mut width).cast::<c_void>(),
                        ) != 0;
                    CFRelease(props);
                    if read {
                        usize::try_from(width).unwrap_or(0)
                    } else {
                        0
                    }
                })
                .collect();
            let image = pick_icon_image(&widths, pixels).map(|index| {
                CGImageSourceCreateImageAtIndex(source, index, ptr::null())
                    .cast::<aterm_cg::CGImage>()
            });
            CFRelease(source.cast_const());
            Bitmap::adopt(image?)
        }
    }

    /// The system's icon for `bundle` under the appearance named `look`, as a CGImage the
    /// IconServices image rendered on this thread.
    ///
    /// # Safety
    /// Off the main thread, inside an autorelease pool. Apple's Threading Programming Guide
    /// (Thread Safety Summary) lists NSImage among the classes usable from any thread one thread
    /// at a time, and says one thread can create an NSImage and pass it to the main thread for
    /// drawing; this image is made, rendered and dropped here alone. The guide names no rule for
    /// NSWorkspace or NSAppearance, and the SDK marks none of them main-actor-only (as it does
    /// NSView); the appearance is made here from its name, so no AppKit object crosses threads.
    unsafe fn system_icon(bundle: &Path, look: Option<&str>) -> Option<Bitmap> {
        let path = appkit::nsstring(bundle.to_str()?)?;
        let made: Rc<Cell<Option<Bitmap>>> = Rc::new(Cell::new(None));
        let into = Rc::clone(&made);
        let draw = move || {
            // SAFETY: as this function's caller says; see `cg_image_of`.
            into.set(unsafe { workspace_icon(path.id()) });
        };
        // SAFETY: `+appearanceNamed:` `+(NSAppearance *)(NSString *)` +0 (nil for an unknown
        // name); `-performAsCurrentDrawingAppearance:` `-(void)(void (^)(void))` runs the block
        // once, synchronously, on this thread with the appearance current for drawing, which is
        // what IconServices reads to pick the icon's style; the block outlives the call.
        unsafe {
            let appearance = match look.and_then(appkit::nsstring) {
                Some(name) => appkit::send_id_id(
                    class(c"NSAppearance").as_id(),
                    sel!(appearanceNamed:),
                    name.id(),
                ),
                None => Id::NIL,
            };
            if appearance.is_null() {
                draw();
            } else {
                let block = RcBlock::new0(draw)?;
                let perform: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::BlockPtr) =
                    aterm_objc::checked_msg();
                perform(
                    appearance,
                    sel!(performAsCurrentDrawingAppearance:),
                    block.as_block_ptr(),
                );
            }
        }
        made.take()
    }

    /// `-[NSWorkspace iconForFile:]` of `path`, rendered to a CGImage [`PIXELS`] wide.
    ///
    /// # Safety
    /// As [`system_icon`]; `path` is a live NSString.
    unsafe fn workspace_icon(path: Id) -> Option<Bitmap> {
        let side = PIXELS as f64;
        let mut proposed = aterm_objc::CGRect {
            origin: aterm_objc::CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: side,
                height: side,
            },
        };
        // SAFETY: `+sharedWorkspace` +0; `-iconForFile:` `-(NSImage *)(NSString *)` +0, kept by
        // the caller's pool; `-CGImageForProposedRect:context:hints:` is
        // `-(CGImageRef)(NSRect *, NSGraphicsContext *, NSDictionary *)`, given a live rect (with
        // no context, its points are pixels) and nil for both objects, and returns a CGImage the
        // NSImage owns (not retained), retained into the `Bitmap` before the pool pops.
        unsafe {
            let workspace = appkit::send_id(class(c"NSWorkspace").as_id(), sel!(sharedWorkspace));
            if workspace.is_null() {
                return None;
            }
            let icon = appkit::send_id_id(workspace, sel!(iconForFile:), path);
            if icon.is_null() {
                return None;
            }
            let f: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void, Id, Id) -> *mut c_void =
                aterm_objc::checked_msg();
            let image = f(
                icon,
                sel!(CGImageForProposedRect:context:hints:),
                (&raw mut proposed).cast::<c_void>(),
                Id::NIL,
                Id::NIL,
            );
            Bitmap::retain(image.cast::<aterm_cg::CGImage>())
        }
    }

    /// The name of the appearance `owner` (an NSView, NSWindow or NSApplication) draws in: its
    /// `effectiveAppearance`'s `name`, the look [`render`] takes.
    ///
    /// # Safety
    /// Main thread; `owner` is live and answers `-effectiveAppearance`.
    pub(crate) unsafe fn look_of(owner: Id) -> Option<String> {
        // SAFETY: as the caller says; `-effectiveAppearance` `-(NSAppearance *)` +0, `-name`
        // `-(NSAppearanceName)` +0, read into a Rust string at once.
        unsafe {
            let appearance = appkit::send_id(owner, sel!(effectiveAppearance));
            if appearance.is_null() {
                return None;
            }
            let name = appkit::send_id(appearance, sel!(name));
            (!name.is_null()).then(|| appkit::nsstring_to_rust(name))
        }
    }

    /// An NSImage `points` square over `bitmap`, which draws without decoding or asking
    /// IconServices.
    ///
    /// # Safety
    /// Main thread.
    pub(crate) unsafe fn ns_image(bitmap: &Bitmap, points: f64) -> Option<Obj> {
        // SAFETY: as the caller says; `+alloc` and `-initWithCGImage:size:`
        // (`-(id)(CGImageRef, NSSize)`) give a +1 NSImage, adopted, that retains the live CGImage.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void, CGSize) -> Id =
                aterm_objc::checked_msg();
            Obj::from_owned(f(
                appkit::alloc(class(c"NSImage")),
                sel!(initWithCGImage:size:),
                bitmap.as_ptr().cast::<c_void>(),
                CGSize {
                    width: points,
                    height: points,
                },
            ))
        }
    }

    /// The running app's bundle, or `None` for a bare executable (a dev build run by path), whose
    /// alerts keep AppKit's default icon.
    fn own_bundle(_mtm: MainThread) -> Option<PathBuf> {
        // SAFETY: main thread (the witness); `+mainBundle` +0, `-bundlePath` +0, read at once.
        let path = unsafe {
            let bundle = appkit::send_id(class(c"NSBundle").as_id(), sel!(mainBundle));
            if bundle.is_null() {
                return None;
            }
            appkit::nsstring_to_rust(appkit::send_id(bundle, sel!(bundlePath)))
        };
        let path = PathBuf::from(path);
        path.extension()
            .is_some_and(|ext| ext == "app")
            .then_some(path)
    }

    /// The look alerts show in: the application's appearance.
    fn app_look(mtm: MainThread) -> Option<String> {
        let app = appkit::existing_application(mtm)?;
        // SAFETY: main thread; NSApplication answers `-effectiveAppearance`.
        unsafe { look_of(app) }
    }

    /// Start making the alerts' icon for the app's current look, once per look, on a background
    /// worker, so an alert finds it ready.
    pub(crate) fn warm_for_alerts() {
        let Some(mtm) = MainThread::new() else {
            return;
        };
        let Some(bundle) = own_bundle(mtm) else {
            return;
        };
        let Some(look) = app_look(mtm) else {
            return;
        };
        let wanted = ALERT_ICONS.lock().is_ok_and(|mut shelf| shelf.wants(&look));
        if !wanted {
            return;
        }
        let (asked, made_for) = (look.clone(), look.clone());
        let started = render_off_thread(
            crate::qos::Role::Background,
            move || render(&bundle, Some(&look)),
            move |icon| {
                if let Ok(mut shelf) = ALERT_ICONS.lock() {
                    shelf.put(&made_for, icon);
                }
            },
        );
        if !started && let Ok(mut shelf) = ALERT_ICONS.lock() {
            shelf.forget(&asked);
        }
    }

    /// A new NSAlert (+1) whose icon is the worker-made bitmap of aterm's icon when one is ready,
    /// so laying it out and drawing it asks IconServices nothing on the main thread; with none
    /// yet (the first moments of a launch, a bare executable) it keeps AppKit's default icon.
    pub(crate) fn alert(mtm: MainThread) -> Option<Obj> {
        // SAFETY: `+[NSAlert new]` is +1, adopted by `Obj::from_owned`.
        let alert =
            unsafe { Obj::from_owned(appkit::send_id(class(c"NSAlert").as_id(), sel!(new))) }?;
        warm_for_alerts();
        let look = app_look(mtm).unwrap_or_default();
        let shelf = ALERT_ICONS.lock().ok();
        if let Some(bitmap) = shelf.as_ref().and_then(|shelf| shelf.pick(&look)) {
            // SAFETY: main thread (the witness); `-setIcon:` `-(void)(NSImage *)` retains it.
            unsafe {
                if let Some(icon) = ns_image(bitmap, ALERT_ICON) {
                    appkit::send_v_id(alert.id(), sel!(setIcon:), icon.id());
                }
            }
        }
        Some(alert)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Shelf, pick_icon_image, render_off_thread};

    #[test]
    fn the_icon_is_made_on_a_worker_and_never_on_the_asking_thread() {
        use std::sync::mpsc;
        let asker = std::thread::current().id();
        let (rendered_tx, rendered) = mpsc::channel();
        let (delivered_tx, delivered) = mpsc::channel();
        let (go_tx, go) = mpsc::channel::<()>();
        let started = render_off_thread(
            crate::qos::Role::Responsive,
            move || {
                let _ = rendered_tx.send(std::thread::current().id());
                // Held until the asker has its answer back: the ask must not wait for the icon.
                go.recv_timeout(Duration::from_secs(60)).ok()?;
                Some(7_u8)
            },
            move |icon| {
                let _ = delivered_tx.send((std::thread::current().id(), icon));
            },
        );
        assert!(started, "the worker started");
        let worker = rendered
            .recv_timeout(Duration::from_secs(60))
            .expect("the icon source was asked");
        assert_ne!(worker, asker, "the icon source ran on the asking thread");
        go_tx.send(()).expect("the worker waits");
        let (on, icon) = delivered
            .recv_timeout(Duration::from_secs(60))
            .expect("the icon was delivered");
        assert_eq!((on, icon), (worker, Some(7)), "delivered from the worker");
    }

    #[test]
    fn the_icon_file_image_is_the_narrowest_wide_enough_else_the_widest() {
        // An `.icns` lists 16, 32, 64, 128, 256, 512 and 1024 pixel images, @1x and @2x pairs
        // repeating widths.
        let icns = [16, 32, 32, 64, 128, 256, 256, 512, 512, 1024];
        assert_eq!(pick_icon_image(&icns, 128), Some(4), "exact");
        assert_eq!(pick_icon_image(&icns, 192), Some(5), "next wider");
        assert_eq!(pick_icon_image(&icns, 2048), Some(9), "the widest there is");
        assert_eq!(
            pick_icon_image(&[0, 32, 0], 64),
            Some(1),
            "unknown widths skipped"
        );
        assert_eq!(pick_icon_image(&[0, 0], 64), None);
        assert_eq!(pick_icon_image(&[], 64), None);
    }

    #[test]
    fn an_alert_takes_its_looks_icon_else_another_looks_and_each_look_is_made_once() {
        let mut shelf = Shelf::new();
        assert_eq!(shelf.pick("NSAppearanceNameAqua"), None, "nothing made yet");
        assert!(
            shelf.wants("NSAppearanceNameAqua"),
            "the first ask starts a worker"
        );
        assert!(!shelf.wants("NSAppearanceNameAqua"), "one worker per look");
        shelf.put("NSAppearanceNameAqua", Some(1_u8));
        assert!(!shelf.wants("NSAppearanceNameAqua"), "made: no worker");
        assert_eq!(shelf.pick("NSAppearanceNameAqua"), Some(&1));
        assert_eq!(
            shelf.pick("NSAppearanceNameDarkAqua"),
            Some(&1),
            "another look's icon beats an IconServices draw on the main thread"
        );
        assert!(
            shelf.wants("NSAppearanceNameDarkAqua"),
            "the new look is made"
        );
        shelf.put("NSAppearanceNameDarkAqua", Some(2));
        assert_eq!(shelf.pick("NSAppearanceNameDarkAqua"), Some(&2), "its own");
        assert_eq!(shelf.pick("NSAppearanceNameAqua"), Some(&1), "its own");
        assert!(shelf.wants("Other"));
        shelf.put("Other", None);
        assert!(
            shelf.wants("Other"),
            "a worker that made nothing may be asked again"
        );
        shelf.forget("Other");
        assert!(
            shelf.wants("Other"),
            "a worker that never started may be asked again"
        );
    }

    /// The bundle's icon file drawn into a bitmap of the icon's own is plain decoded pixels at the
    /// icon's size: an image straight from ImageIO is not (negative control: return `icns_image`'s
    /// image undrawn and the pixel-layout check fails at 256, the size check too at 100).
    #[cfg(target_os = "macos")]
    #[test]
    fn the_icon_crosses_threads_as_decoded_pixels_of_its_own_size() {
        use super::PIXELS;
        use super::live::{BITMAP_INFO, decoded, icns_image};

        use aterm_cg::{CGImageGetBitmapInfo, CGImageGetHeight, CGImageGetWidth};

        let icns = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/aterm-mac/aterm.icns");
        // A size no icns entry has, so only a draw makes it.
        for want in [PIXELS, 100] {
            // SAFETY: a test thread; the images are live while read.
            unsafe {
                let raw = icns_image(&icns, want).expect("the checkout's aterm.icns reads");
                let made = decoded(raw.as_ptr(), want).expect("drawn into a bitmap");
                let image = made.as_ptr();
                assert_eq!(
                    (CGImageGetWidth(image), CGImageGetHeight(image)),
                    (want, want),
                    "drawn at the size asked"
                );
                assert_eq!(
                    CGImageGetBitmapInfo(image),
                    BITMAP_INFO,
                    "the bitmap context's own pixels"
                );
            }
        }
    }
}
