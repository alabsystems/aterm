// Modified by the aterm project in 2026; see the repository NOTICE.
// (Every `objc2` binding call in this file was replaced with a typed send
// through `aterm_objc`; `Retained<NSCursor>` became `aterm_objc::Obj` in this
// file's four `pub(crate)` return types and in `CustomCursor`'s field. Search
// for the aterm local-patch marker.)
use std::ffi::{c_uchar, c_void};
use std::slice;
use std::sync::OnceLock;

// LOCAL PATCH (aterm): objc2's `Retained`/`Sel` and its twelve `NSCursor`,
// `NSImage`, `NSData` and `NSString` bindings are gone. `Obj` is the +1 handle
// `Retained<NSCursor>` was; `NSDeviceRGBColorSpace`, the one framework GLOBAL
// this file reads, is bound in the seam.
use aterm_objc::send::{
    alloc, send_bool_cls, send_bool_id, send_bool_sel, send_charptr, send_f64, send_id,
    send_id_cptr_usize, send_id_id, send_id_planeptr_isize_isize_isize_isize_bool_bool_id_isize_isize,
    send_id_id_point, send_id_sel, send_id_size, send_usize, send_v_id,
};
use aterm_objc::{CGPoint, CGSize, Obj, autoreleasepool, class, sel};

use super::aterm_objc_seam as seam;

use crate::cursor::{CursorImage, OnlyCursorImageSource};
use crate::window::CursorIcon;

/// LOCAL PATCH (aterm): the field is an [`Obj`], and the four traits
/// `crate::cursor::CustomCursor` derives over it are WRITTEN OUT below.
///
/// `Obj` implements none of `Clone`/`PartialEq`/`Eq`/`Hash` on purpose (a
/// retain should be visible, not smuggled in by a derive), while
/// `crate::cursor::CustomCursor` derives all of them over whatever the platform
/// puts in `inner` — and that is PUBLIC winit API, so a caller may clone one
/// and use one as a `HashMap` key.
///
/// Equality and hashing are the two that could have gone wrong in silence:
/// objc2 forwarded both to `-isEqual:`/`-hash`, and pointer identity is the
/// obvious replacement. It is the same ANSWER here — measured, `NSCursor`
/// inherits both IMPs from `NSObject` and overrides neither — but a different
/// QUESTION, so the sends stay. See the roadmap.
#[derive(Debug)]
pub struct CustomCursor(pub(crate) Obj);

// SAFETY: NSCursor is immutable and thread-safe
// TODO(madsmtm): Put this logic in objc2-app-kit itself
unsafe impl Send for CustomCursor {}
unsafe impl Sync for CustomCursor {}

impl Clone for CustomCursor {
    /// A second +1 handle to the same cursor — `Retained`'s `Clone`, which was
    /// an `objc_retain` too.
    fn clone(&self) -> Self {
        Self(self.0.clone_retained())
    }
}

impl PartialEq for CustomCursor {
    fn eq(&self, other: &Self) -> bool {
        same_cursor(&self.0, &other.0)
    }
}

impl Eq for CustomCursor {}

impl std::hash::Hash for CustomCursor {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // SAFETY: `self.0` owns a +1 reference to a live `NSCursor`; `-hash` is
        // `Q@:8` on `NSObject` and every subclass.
        let h = unsafe { send_usize(self.0.id(), sel!(hash)) };
        h.hash(state);
    }
}

/// Do two cursor handles denote the same cursor, by `-isEqual:`?
///
/// LOCAL PATCH (aterm): `window_delegate.rs`'s `set_cursor` compared two
/// `Retained<NSCursor>` with `==`, which objc2 forwarded here. Named rather
/// than re-derived at the call site.
#[must_use]
pub(crate) fn same_cursor(a: &Obj, b: &Obj) -> bool {
    // SAFETY: both handles own +1 references to live objects; `-isEqual:` is
    // `B@:@` on `NSObject` and every subclass.
    unsafe { send_bool_id(a.id(), sel!(isEqual:), b.id()) }
}

impl CustomCursor {
    pub(crate) fn new(cursor: OnlyCursorImageSource) -> CustomCursor {
        Self(cursor_from_image(&cursor.0))
    }
}

pub(crate) fn cursor_from_image(cursor: &CursorImage) -> Obj {
    let width = cursor.width;
    let height = cursor.height;

    // SAFETY: `@88@0:8^*16q24q32q40q48B56B60@64q72q80` — ten arguments, four
    // of them on the stack on AAPCS64. `+alloc` is +1 and the initialiser
    // consumes it and answers +1; a null `planes` asks the receiver to allocate
    // its own buffer; `NS_DEVICE_RGB_COLOR_SPACE` is a live `NSString` global.
    let bitmap = unsafe {
        let raw = alloc(class(c"NSBitmapImageRep"));
        let obj = send_id_planeptr_isize_isize_isize_isize_bool_bool_id_isize_isize(
            raw,
            sel!(initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:),
            std::ptr::null_mut::<*mut c_uchar>(),
            width as isize,
            height as isize,
            8,
            4,
            true,
            false,
            seam::NS_DEVICE_RGB_COLOR_SPACE,
            width as isize * 4,
            32,
        );
        Obj::from_owned(obj).expect("NSBitmapImageRep to accept the cursor's dimensions")
    };
    // SAFETY: `-bitmapData` is `*16@0:8` and answers an INTERIOR pointer into
    // `bitmap`'s pixel storage, valid while `bitmap` lives — the whole of this
    // function. The buffer is `width * 4` bytes per row over `height` rows,
    // exactly `cursor.rgba.len()` (`CursorImage::from_rgba` rejects any other).
    let bitmap_data =
        unsafe { slice::from_raw_parts_mut(send_charptr(bitmap.id(), sel!(bitmapData)), cursor.rgba.len()) };
    bitmap_data.copy_from_slice(&cursor.rgba);

    // SAFETY: `-initWithSize:` is `@32@0:8{CGSize=dd}16` and consumes the +1
    // `+alloc`; `-addRepresentation:` is `v@:@` and the image retains the
    // representation itself.
    let image = unsafe {
        let raw = alloc(class(c"NSImage"));
        let obj = send_id_size(
            raw,
            sel!(initWithSize:),
            CGSize { width: width.into(), height: height.into() },
        );
        let image = Obj::from_owned(obj).expect("NSImage to accept the cursor's size");
        send_v_id(image.id(), sel!(addRepresentation:), bitmap.id());
        image
    };

    let hotspot = CGPoint { x: cursor.hotspot_x as f64, y: cursor.hotspot_y as f64 };

    // SAFETY: `-initWithImage:hotSpot:` is `@40@0:8@16{CGPoint=dd}24` and
    // consumes the +1 allocation.
    unsafe {
        let raw = alloc(class(c"NSCursor"));
        let obj = send_id_id_point(raw, sel!(initWithImage:hotSpot:), image.id(), hotspot);
        Obj::from_owned(obj).expect("NSCursor to accept the cursor image")
    }
}

pub(crate) fn default_cursor() -> Obj {
    class_cursor(sel!(arrowCursor))
}

/// A `+…Cursor` class accessor's result, retained.
///
/// LOCAL PATCH (aterm): all seventeen are `@16@0:8` on `NSCursor`'s metaclass
/// and all are +0 autoreleased (none is named `new`/`alloc`/`copy`), so the
/// retain objc2's bindings applied is applied here once instead of seventeen
/// times.
#[must_use]
fn class_cursor(s: aterm_objc::Sel) -> Obj {
    // SAFETY: `s` is one of `NSCursor`'s `@16@0:8` class accessors (every call
    // site passes a literal one) and the result is +0 autoreleased. The pool is
    // explicit: `set_cursor` is reachable from `WindowDelegate::new`, outside
    // AppKit's own event pool.
    autoreleasepool(|_| {
        unsafe { Obj::retain(send_id(class(c"NSCursor").as_id(), s)) }
            .expect("+[NSCursor …Cursor] to answer a cursor")
    })
}

/// LOCAL PATCH (aterm): the send changed, the logic did not —
/// `-respondsToSelector:` is asked first because these selectors are
/// undocumented and may vanish, which is why upstream asked it too.
fn try_cursor_from_selector(s: aterm_objc::Sel) -> Option<Obj> {
    let cls = class(c"NSCursor").as_id();
    // SAFETY: `+respondsToSelector:` is `B24@0:8:16` and `-performSelector:`
    // is `@24@0:8:16`, returning under the invoked method's own convention —
    // a `+…Cursor` accessor, so +0 autoreleased.
    unsafe {
        if send_bool_sel(cls, sel!(respondsToSelector:), s) {
            autoreleasepool(|_| Obj::retain(send_id_sel(cls, sel!(performSelector:), s)))
        } else {
            tracing::warn!("cursor `{}` appears to be invalid", s.name().to_string_lossy());
            None
        }
    }
}

macro_rules! def_undocumented_cursor {
    {$(
        $(#[$($m:meta)*])*
        fn $name:ident();
    )*} => {$(
        $(#[$($m)*])*
        #[allow(non_snake_case)]
        fn $name() -> Obj {
            try_cursor_from_selector(sel!($name)).unwrap_or_else(default_cursor)
        }
    )*};
}

def_undocumented_cursor!(
    // Undocumented cursors: https://stackoverflow.com/a/46635398/5435443
    fn _helpCursor();
    fn _zoomInCursor();
    fn _zoomOutCursor();
    fn _windowResizeNorthEastCursor();
    fn _windowResizeNorthWestCursor();
    fn _windowResizeSouthEastCursor();
    fn _windowResizeSouthWestCursor();
    fn _windowResizeNorthEastSouthWestCursor();
    fn _windowResizeNorthWestSouthEastCursor();

    // While these two are available, the former just loads a white arrow,
    // and the latter loads an ugly deflated beachball!
    // pub fn _moveCursor();
    // pub fn _waitCursor();

    // An even more undocumented cursor...
    // https://bugs.eclipse.org/bugs/show_bug.cgi?id=522349
    fn busyButClickableCursor();
);

// Note that loading `busybutclickable` with this code won't animate
// the frames; instead you'll just get them all in a column.
fn load_webkit_cursor(name: &str) -> Obj {
    // Snatch a cursor from WebKit; They fit the style of the native
    // cursors, and will seem completely standard to macOS users.
    //
    // https://stackoverflow.com/a/21786835/5435443
    //
    // LOCAL PATCH (aterm): `ns_string!` minted a STATIC `NSString`;
    // `aterm_objc::ns_string` builds one at +1 per call. Reached only when the
    // application sets a `Move`/`AllScroll`/`Cell` cursor, never per frame.
    let root = seam::nsstring(
        "/System/Library/Frameworks/ApplicationServices.framework/Versions/A/Frameworks/\
         HIServices.framework/Versions/A/Resources/cursors",
    )
    .expect("a literal ASCII path to be a valid NSString");
    let name = seam::nsstring(name).expect("a literal ASCII cursor name to be a valid NSString");

    // SAFETY: read from the live runtime —
    // `-stringByAppendingPathComponent:` and `+dictionaryWithContentsOfFile:`
    // are `@24@0:8@16` (+0 autoreleased), `-initByReferencingFile:` the same
    // shape at +1 (consuming the allocation), `-objectForKey:` `@24@0:8@16` (+0
    // BORROWED), `-isKindOfClass:` `B24@0:8#16`, `-doubleValue` `d16@0:8`. The
    // pool wraps the whole body: five of those returns are autoreleased and
    // this is reachable outside AppKit's own event pool.
    autoreleasepool(|_| unsafe {
        let cursor_path = Obj::retain(send_id_id(
            root.id(),
            sel!(stringByAppendingPathComponent:),
            name.id(),
        ))
        .expect("appending a path component to answer a string");

        let pdf_name = seam::nsstring("cursor.pdf").expect("a literal to be a valid NSString");
        let pdf_path = Obj::retain(send_id_id(
            cursor_path.id(),
            sel!(stringByAppendingPathComponent:),
            pdf_name.id(),
        ))
        .expect("appending a path component to answer a string");
        let image = {
            let raw = alloc(class(c"NSImage"));
            Obj::from_owned(send_id_id(raw, sel!(initByReferencingFile:), pdf_path.id()))
                .expect("the WebKit cursor PDF to load")
        };

        // TODO: Handle PLists better
        let info_name = seam::nsstring("info.plist").expect("a literal to be a valid NSString");
        let info_path = Obj::retain(send_id_id(
            cursor_path.id(),
            sel!(stringByAppendingPathComponent:),
            info_name.id(),
        ))
        .expect("appending a path component to answer a string");
        let info = send_id_id(
            class(c"NSDictionary").as_id(),
            sel!(dictionaryWithContentsOfFile:),
            info_path.id(),
        );

        // `-objectForKey:` is BORROWED and lives as long as the dictionary,
        // i.e. the whole of this pool; nothing is retained, as objc2's
        // `&NSObject` borrow did not.
        //
        // UPSTREAM BUG PRESERVED DELIBERATELY: both coordinates read `"hotx"`,
        // so every WebKit cursor's vertical hotspot equals its horizontal one.
        // Changing it here would smuggle a behaviour change into a mechanical
        // port; it is upstream winit's line to fix. Recorded, not introduced.
        let number_cls = class(c"NSNumber");
        let hot = |key: &str| -> f64 {
            let k = seam::nsstring(key).expect("a literal to be a valid NSString");
            let n = send_id_id(info, sel!(objectForKey:), k.id());
            if !n.is_null() && send_bool_cls(n, sel!(isKindOfClass:), number_cls) {
                send_f64(n, sel!(doubleValue))
            } else {
                0.0
            }
        };
        let hotspot = CGPoint { x: hot("hotx"), y: hot("hotx") };

        let raw = alloc(class(c"NSCursor"));
        Obj::from_owned(send_id_id_point(
            raw,
            sel!(initWithImage:hotSpot:),
            image.id(),
            hotspot,
        ))
        .expect("NSCursor to accept the WebKit cursor image")
    })
}

fn webkit_move() -> Obj {
    load_webkit_cursor("move")
}

fn webkit_cell() -> Obj {
    load_webkit_cursor("cell")
}

pub(crate) fn invisible_cursor() -> Obj {
    // 16x16 GIF data for invisible cursor
    // You can reproduce this via ImageMagick.
    // $ convert -size 16x16 xc:none cursor.gif
    static CURSOR_BYTES: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x10, 0x00, 0x10, 0x00, 0xf0, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00,
        0x00, 0x00, 0x10, 0x00, 0x10, 0x00, 0x00, 0x02, 0x0e, 0x84, 0x8f, 0xa9, 0xcb, 0xed, 0x0f,
        0xa3, 0x9c, 0xb4, 0xda, 0x8b, 0xb3, 0x3e, 0x05, 0x00, 0x3b,
    ];

    fn new_invisible() -> Obj {
        // TODO: Consider using `dataWithBytesNoCopy:`
        //
        // SAFETY: `-initWithBytes:length:` is `@32@0:8r^v16Q24` — CONST and
        // COPIED during the call, which is what lets a `'static` slice be
        // handed over. Every initialiser here consumes its +1 allocation.
        unsafe {
            let raw = alloc(class(c"NSData"));
            let data = Obj::from_owned(send_id_cptr_usize(
                raw,
                sel!(initWithBytes:length:),
                CURSOR_BYTES.as_ptr().cast::<c_void>(),
                CURSOR_BYTES.len(),
            ))
            .expect("NSData to accept a 55-byte literal");

            let raw = alloc(class(c"NSImage"));
            let image = Obj::from_owned(send_id_id(raw, sel!(initWithData:), data.id()))
                .expect("NSImage to decode the 16x16 transparent GIF");

            let raw = alloc(class(c"NSCursor"));
            Obj::from_owned(send_id_id_point(
                raw,
                sel!(initWithImage:hotSpot:),
                image.id(),
                CGPoint { x: 0.0, y: 0.0 },
            ))
            .expect("NSCursor to accept the invisible cursor image")
        }
    }

    // Cache this for efficiency
    static CURSOR: OnceLock<CustomCursor> = OnceLock::new();
    CURSOR.get_or_init(|| CustomCursor(new_invisible())).0.clone_retained()
}

pub(crate) fn cursor_from_icon(icon: CursorIcon) -> Obj {
    match icon {
        CursorIcon::Default => default_cursor(),
        CursorIcon::Pointer => class_cursor(sel!(pointingHandCursor)),
        CursorIcon::Grab => class_cursor(sel!(openHandCursor)),
        CursorIcon::Grabbing => class_cursor(sel!(closedHandCursor)),
        CursorIcon::Text => class_cursor(sel!(IBeamCursor)),
        CursorIcon::VerticalText => class_cursor(sel!(IBeamCursorForVerticalLayout)),
        CursorIcon::Copy => class_cursor(sel!(dragCopyCursor)),
        CursorIcon::Alias => class_cursor(sel!(dragLinkCursor)),
        CursorIcon::NotAllowed | CursorIcon::NoDrop => {
            class_cursor(sel!(operationNotAllowedCursor))
        },
        CursorIcon::ContextMenu => class_cursor(sel!(contextualMenuCursor)),
        CursorIcon::Crosshair => class_cursor(sel!(crosshairCursor)),
        CursorIcon::EResize => class_cursor(sel!(resizeRightCursor)),
        CursorIcon::NResize => class_cursor(sel!(resizeUpCursor)),
        CursorIcon::WResize => class_cursor(sel!(resizeLeftCursor)),
        CursorIcon::SResize => class_cursor(sel!(resizeDownCursor)),
        CursorIcon::EwResize | CursorIcon::ColResize => class_cursor(sel!(resizeLeftRightCursor)),
        CursorIcon::NsResize | CursorIcon::RowResize => class_cursor(sel!(resizeUpDownCursor)),
        CursorIcon::Help => _helpCursor(),
        CursorIcon::ZoomIn => _zoomInCursor(),
        CursorIcon::ZoomOut => _zoomOutCursor(),
        CursorIcon::NeResize => _windowResizeNorthEastCursor(),
        CursorIcon::NwResize => _windowResizeNorthWestCursor(),
        CursorIcon::SeResize => _windowResizeSouthEastCursor(),
        CursorIcon::SwResize => _windowResizeSouthWestCursor(),
        CursorIcon::NeswResize => _windowResizeNorthEastSouthWestCursor(),
        CursorIcon::NwseResize => _windowResizeNorthWestSouthEastCursor(),
        // This is the wrong semantics for `Wait`, but it's the same as
        // what's used in Safari and Chrome.
        CursorIcon::Wait | CursorIcon::Progress => busyButClickableCursor(),
        CursorIcon::Move | CursorIcon::AllScroll => webkit_move(),
        CursorIcon::Cell => webkit_cell(),
        _ => default_cursor(),
    }
}
