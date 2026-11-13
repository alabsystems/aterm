// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_objc::Encode;

use super::*;

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize {
            width: w,
            height: h,
        },
    }
}

/// A `w` × `h` template PNG, inked on its left half.
fn png(w: u32, h: u32) -> Vec<u8> {
    let cov = aterm_template_icon::coverage((w, h), (f64::from(w), f64::from(h)), |p| {
        p.x < f64::from(w) / 2.0
    });
    aterm_template_icon::png(&cov, w, h)
}

#[test]
fn the_menu_drops_from_the_buttons_bottom_left_corner() {
    let flipped = popup_point(rect(0.0, 0.0, 24.0, 22.0), true);
    assert_eq!((flipped.x, flipped.y), (0.0, 22.0 + POPUP_GAP));
    let unflipped = popup_point(rect(0.0, 0.0, 24.0, 22.0), false);
    assert_eq!((unflipped.x, unflipped.y), (0.0, -POPUP_GAP));
    let moved = popup_point(rect(5.0, 2.0, 24.0, 22.0), true);
    assert_eq!((moved.x, moved.y), (5.0, 24.0 + POPUP_GAP));
    let moved = popup_point(rect(5.0, 2.0, 24.0, 22.0), false);
    assert_eq!((moved.x, moved.y), (5.0, 2.0 - POPUP_GAP));
}

#[test]
fn a_png_decodes_to_a_bitmap_of_its_pixels() {
    use aterm_objc::send::send_isize;
    for (w, h) in [(14_u32, 18_u32), (28, 36), (18, 18)] {
        let rep = bitmap(&png(w, h)).expect("decodes");
        // SAFETY: `-pixelsWide` and `-pixelsHigh` are `q16@0:8` on the live bitmap.
        let got = unsafe {
            (
                send_isize(rep.id(), sel!(pixelsWide)),
                send_isize(rep.id(), sel!(pixelsHigh)),
            )
        };
        let want = (
            isize::try_from(w).expect("w"),
            isize::try_from(h).expect("h"),
        );
        assert_eq!(got, want, "{w}x{h}");
    }
    assert!(bitmap(&[]).is_none());
    assert!(bitmap(b"not a png").is_none());
}

#[test]
fn the_image_is_a_template_of_its_size_with_a_bitmap_per_scale() {
    use aterm_objc::send::{send_size, send_usize};
    let size = CGSize {
        width: 14.0,
        height: 18.0,
    };
    let image =
        template_image(&[&png(14, 18), &png(28, 36)], size, "lock closed").expect("an image");
    // SAFETY: `-isTemplate` is `B16@0:8`, `-size` `{CGSize=dd}16@0:8`, `-representations`
    // `@16@0:8` (+0, read inside this pool), `-count` `Q16@0:8` and
    // `-accessibilityDescription` `@16@0:8`, all on the live image and without side effects.
    autoreleasepool(|_| unsafe {
        assert!(send_bool(image.id(), sel!(isTemplate)));
        let got = send_size(image.id(), sel!(size));
        assert_eq!((got.width, got.height), (14.0, 18.0));
        let reps = send_id(image.id(), sel!(representations));
        assert_eq!(send_usize(reps, sel!(count)), 2);
        let said = send_id(image.id(), sel!(accessibilityDescription));
        assert_eq!(aterm_objc::ns_string_to_rust(said), "lock closed");
    });
    // A picture that does not decode makes no image, never one with a hole in it.
    assert!(template_image(&[&png(14, 18), b"not a png"], size, "x").is_none());
}

#[test]
fn only_a_mouse_buttons_event_is_asked_its_click_count() {
    for kind in [1, 2, 3, 4, 25, 26] {
        assert!(of_a_mouse_button(kind), "{kind}");
    }
    // A move, a drag, a key, an application-defined event and a scroll.
    for kind in [0, 5, 6, 7, 10, 11, 15, 22, 27] {
        assert!(!of_a_mouse_button(kind), "{kind}");
    }
}

fn types(ret: &str, args: &[&str]) -> String {
    format!("{ret}@:{}", args.concat())
}

fn runtime(cls: &'static std::ffi::CStr, sel: Sel, class_method: bool) -> Option<String> {
    let cls = class(cls);
    assert!(!cls.is_null(), "{cls:?}");
    // SAFETY: `cls` is a live, registered class, and so is its metaclass; reading a method's
    // encoding has no side effects and opens no WindowServer connection.
    unsafe {
        let cls = if class_method {
            aterm_objc::class_of(cls.as_id())
        } else {
            cls
        };
        aterm_objc::method_types(cls, sel).map(|t| aterm_objc::strip_method_offsets(&t))
    }
}

#[test]
fn every_send_here_has_the_prototype_the_runtime_reports() {
    let (v, id, b, sel_, q, uq, d) = (
        <() as Encode>::ENCODING,
        Id::ENCODING,
        Bool::ENCODING,
        Sel::ENCODING,
        isize::ENCODING,
        usize::ENCODING,
        f64::ENCODING,
    );
    let (pt, sz, rc) = (CGPoint::ENCODING, CGSize::ENCODING, CGRect::ENCODING);
    let rows: &[(&'static std::ffi::CStr, Sel, bool, String)] = &[
        (c"NSApplication", sel!(currentEvent), false, types(id, &[])),
        (c"NSEvent", sel!(type), false, types(uq, &[])),
        (c"NSEvent", sel!(modifierFlags), false, types(uq, &[])),
        (c"NSEvent", sel!(timestamp), false, types(d, &[])),
        (c"NSEvent", sel!(clickCount), false, types(q, &[])),
        (
            c"NSStatusBarButton",
            sel!(setTarget:),
            false,
            types(v, &[id]),
        ),
        (
            c"NSStatusBarButton",
            sel!(setAction:),
            false,
            types(v, &[sel_]),
        ),
        (
            c"NSStatusBarButton",
            sel!(sendActionOn:),
            false,
            types(q, &[uq]),
        ),
        (
            c"NSStatusBarButton",
            sel!(setAccessibilityCustomActions:),
            false,
            types(v, &[id]),
        ),
        (
            c"NSAccessibilityCustomAction",
            sel!(initWithName:target:selector:),
            false,
            types(id, &[id, id, sel_]),
        ),
        (c"NSArray", sel!(arrayWithObject:), true, types(id, &[id])),
        (
            c"NSStatusBarButton",
            sel!(highlight:),
            false,
            types(v, &[b]),
        ),
        (c"NSStatusBarButton", sel!(bounds), false, types(rc, &[])),
        (c"NSStatusBarButton", sel!(isFlipped), false, types(b, &[])),
        (
            c"NSMenu",
            sel!(popUpMenuPositioningItem:atLocation:inView:),
            false,
            types(b, &[id, pt, id]),
        ),
        (c"NSImage", sel!(initWithSize:), false, types(id, &[sz])),
        (c"NSImage", sel!(addRepresentation:), false, types(v, &[id])),
        (c"NSImage", sel!(setTemplate:), false, types(v, &[b])),
        (
            c"NSImage",
            sel!(setAccessibilityDescription:),
            false,
            types(v, &[id]),
        ),
        (
            c"NSBitmapImageRep",
            sel!(imageRepWithData:),
            true,
            types(id, &[id]),
        ),
        (c"NSBitmapImageRep", sel!(setSize:), false, types(v, &[sz])),
        (
            c"NSData",
            sel!(initWithBytes:length:),
            false,
            types(id, &["r^v", uq]),
        ),
    ];
    for (cls, sel, class_method, want) in rows {
        assert_eq!(
            runtime(cls, *sel, *class_method).as_deref(),
            Some(want.as_str()),
            "{cls:?} {sel:?}"
        );
    }
}
