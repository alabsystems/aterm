// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_objc::Encode;

use super::*;

use crate::launchd::Startup;
use crate::menu::{self, MenuAction};
use crate::model::{Choice, Clocks, Mode};

/// The menu's other rows as `startup` and version 1 show them, the lid option off and no
/// pause.
fn plain(startup: Startup) -> menu::Shown<'static> {
    menu::Shown {
        startup,
        lid_awake: false,
        paused: None,
        version: "1",
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize {
            width: w,
            height: h,
        },
    }
}

#[test]
fn the_image_is_the_icons_size() {
    assert_eq!(f64::from(POINTS), icon::SIZE_POINTS);
}

#[test]
fn the_icons_pngs_decode_to_a_bitmap_per_scale() {
    use aterm_objc::send::send_isize;
    for lit in [false, true] {
        for scale in SCALES {
            let px = POINTS * scale;
            let rep = bitmap(&icon::png(lit, px)).expect("decodes");
            // SAFETY: `-pixelsWide` and `-pixelsHigh` are `q16@0:8` on the live bitmap.
            let (w, h) = unsafe {
                (
                    send_isize(rep.id(), sel!(pixelsWide)),
                    send_isize(rep.id(), sel!(pixelsHigh)),
                )
            };
            let px = isize::try_from(px).expect("px");
            assert_eq!((w, h), (px, px), "lit={lit} scale={scale}");
        }
    }
    assert!(bitmap(&[]).is_none());
    assert!(bitmap(b"not a png").is_none());
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
fn a_menu_of_the_same_shape_is_retitled_in_place() {
    const NOW: Clocks = Clocks {
        since_boot: 300_000,
        wall: 1_790_000_000,
    };
    let off = menu::compose(Mode::Off, NOW, plain(Startup::Off));
    assert_eq!(changed_rows(&off, &off), Some(vec![]));
    let forever = menu::compose(Mode::Forever, NOW, plain(Startup::Off));
    let forever_row = off
        .iter()
        .position(|r| {
            matches!(
                r,
                Row::Action {
                    action: MenuAction::Activate(Choice::Forever),
                    ..
                }
            )
        })
        .expect("forever row");
    assert_eq!(changed_rows(&off, &forever), Some(vec![forever_row]));
    let timed = Mode::chosen(Choice::Hours1, NOW);
    let a = menu::compose(timed, NOW, plain(Startup::On));
    let b = menu::compose(timed, NOW + 60, plain(Startup::On));
    assert_eq!(changed_rows(&a, &b).map(|c| c.len()), Some(1));
    assert_eq!(changed_rows(&off, &off[1..]), None);
    let mut kinds = off.clone();
    kinds[1] = Row::Info("x".into());
    assert_eq!(changed_rows(&off, &kinds), None);
}

/// Every menu `compose` makes has one shape, so the open menu is always retitled in place
/// ([`StatusUi::retitle_menu`]), never emptied while AppKit tracks it; and across a span's end
/// the open menu takes the checkmark off that span's row.
#[test]
fn an_open_menu_is_always_retitled_in_place() {
    const NOW: Clocks = Clocks {
        since_boot: 300_000,
        wall: 1_790_000_000,
    };
    let mut modes = vec![
        Mode::Off,
        Mode::Forever,
        Mode::activate(Some(2 * crate::model::HOUR), NOW),
    ];
    modes.extend(Choice::ALL.map(|choice| Mode::chosen(choice, NOW)));
    let menus: Vec<Vec<Row>> = modes
        .iter()
        .flat_map(|&mode| {
            [
                Startup::Off,
                Startup::On,
                Startup::Blocked,
                Startup::Disabled,
            ]
            .into_iter()
            .flat_map(move |startup| {
                [NOW, NOW + 30, NOW + 25 * crate::model::HOUR]
                    .map(|at| menu::compose(mode, at, plain(startup)))
            })
        })
        .collect();
    for shown in &menus {
        for now in &menus {
            assert!(changed_rows(shown, now).is_some(), "{shown:#?}\n{now:#?}");
        }
    }

    let hour = Mode::chosen(Choice::Hours1, NOW);
    let before = menu::compose(hour, NOW + (crate::model::HOUR - 30), plain(Startup::Off));
    let after = menu::compose(hour, NOW + crate::model::HOUR + 1, plain(Startup::Off));
    let row = |choice: Choice| {
        before
            .iter()
            .position(|r| {
                matches!(r, Row::Action { action: MenuAction::Activate(c), .. } if *c == choice)
            })
            .expect("a row for each choice")
    };
    let checked = |rows: &[Row], i: usize| matches!(rows[i], Row::Action { checked: true, .. });
    assert_eq!(
        changed_rows(&before, &after),
        Some(vec![row(Choice::Hours1)])
    );
    assert!(checked(&before, row(Choice::Hours1)) && !checked(&before, row(Choice::Forever)));
    assert!(!checked(&after, row(Choice::Hours1)) && !checked(&after, row(Choice::Forever)));
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
        (c"NSStatusBar", sel!(systemStatusBar), true, types(id, &[])),
        (
            c"NSStatusBar",
            sel!(statusItemWithLength:),
            false,
            types(id, &[d]),
        ),
        (
            c"NSStatusBar",
            sel!(removeStatusItem:),
            false,
            types(v, &[id]),
        ),
        (c"NSStatusItem", sel!(button), false, types(id, &[])),
        (
            c"NSStatusBarButton",
            sel!(setImagePosition:),
            false,
            types(v, &[uq]),
        ),
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
            sel!(setAccessibilityLabel:),
            false,
            types(v, &[id]),
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
            sel!(setImage:),
            false,
            types(v, &[id]),
        ),
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
        (c"NSMenu", sel!(init), false, types(id, &[])),
        (c"NSMenu", sel!(setAutoenablesItems:), false, types(v, &[b])),
        (c"NSMenu", sel!(addItem:), false, types(v, &[id])),
        (c"NSMenu", sel!(removeAllItems), false, types(v, &[])),
        (
            c"NSMenuItem",
            sel!(initWithTitle:action:keyEquivalent:),
            false,
            types(id, &[id, sel_, id]),
        ),
        (c"NSMenuItem", sel!(setTarget:), false, types(v, &[id])),
        (c"NSMenuItem", sel!(setEnabled:), false, types(v, &[b])),
        (c"NSMenuItem", sel!(setTitle:), false, types(v, &[id])),
        (c"NSMenuItem", sel!(setTag:), false, types(v, &[q])),
        (c"NSMenuItem", sel!(setState:), false, types(v, &[q])),
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
