// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/menu.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// LOCAL PATCH (aterm): Objective-C goes through `aterm_objc` (typed `send` prototypes,
// `Obj`/`Retained` handles, `MainThread` witnesses, the seam's `consts`) where upstream used objc2
// and its framework bindings.
use aterm_objc::send::{alloc, send_id, send_id_id, send_id_id_sel_id, send_v_id, send_v_usize};
use aterm_objc::{autoreleasepool, class, sel, Id, MainThread, Obj, Sel};

use super::aterm_objc_seam::consts::{
    NS_EVENT_MODIFIER_FLAG_COMMAND, NS_EVENT_MODIFIER_FLAG_OPTION,
};

/// LOCAL PATCH (aterm): the key is an `Id` borrowed from a live `NSString`
/// rather than an `&NSString`, and the mask is the raw `NSUInteger` the setter
/// takes rather than an `NSEventModifierFlags` bitflags value.
struct KeyEquivalent {
    key: Id,
    masks: Option<usize>,
}

/// Build and install the default application menu.
///
/// # Safety
///
/// `app` must be a live `NSApplication`.
pub(super) unsafe fn initialize(app: Id, mtm: MainThread) {
    autoreleasepool(|_| {
        // SAFETY (for this whole block): every receiver below is either a
        // class object from `class()`, a +1 handle this scope owns, a
        // borrowed autoreleased return that is live until the pool pops, or
        // `app`, which the caller pins as a live `NSApplication`; each send's
        // prototype matches the runtime's own encoding (a debug build checks
        // it at the send, `aterm_objc::send`). `mtm` witnesses the thread
        // `NSMenu` and `NSMenuItem` require.
        unsafe {
            let _ = mtm;

            let menubar = Obj::from_owned(send_id(class(c"NSMenu").as_id(), sel!(new)))
                .expect("+[NSMenu new] to answer a menu");
            let app_menu_item = Obj::from_owned(send_id(class(c"NSMenuItem").as_id(), sel!(new)))
                .expect("+[NSMenuItem new] to answer an item");
            send_v_id(menubar.id(), sel!(addItem:), app_menu_item.id());

            let app_menu = Obj::from_owned(send_id(class(c"NSMenu").as_id(), sel!(new)))
                .expect("+[NSMenu new] to answer a menu");
            let process_info = send_id(class(c"NSProcessInfo").as_id(), sel!(processInfo));
            let process_name = send_id(process_info, sel!(processName));

            let about_item_title = append(nsstr("About "), process_name);
            let about_item =
                menu_item(mtm, about_item_title, sel!(orderFrontStandardAboutPanel:), None);

            let services_menu = Obj::from_owned(send_id(class(c"NSMenu").as_id(), sel!(new)))
                .expect("+[NSMenu new] to answer a menu");
            let services_item = menu_item(mtm, nsstr("Services"), Sel::NULL, None);
            send_v_id(services_item.id(), sel!(setSubmenu:), services_menu.id());

            let sep_first = send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem));

            let hide_item_title = append(nsstr("Hide "), process_name);
            let hide_item = menu_item(
                mtm,
                hide_item_title,
                sel!(hide:),
                Some(KeyEquivalent { key: nsstr("h"), masks: None }),
            );

            let hide_others_item = menu_item(
                mtm,
                nsstr("Hide Others"),
                sel!(hideOtherApplications:),
                Some(KeyEquivalent {
                    key: nsstr("h"),
                    masks: Some(NS_EVENT_MODIFIER_FLAG_OPTION | NS_EVENT_MODIFIER_FLAG_COMMAND),
                }),
            );

            let show_all_item =
                menu_item(mtm, nsstr("Show All"), sel!(unhideAllApplications:), None);

            let sep = send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem));

            let quit_item_title = append(nsstr("Quit "), process_name);
            let quit_item = menu_item(
                mtm,
                quit_item_title,
                sel!(terminate:),
                Some(KeyEquivalent { key: nsstr("q"), masks: None }),
            );

            send_v_id(app_menu.id(), sel!(addItem:), about_item.id());
            send_v_id(app_menu.id(), sel!(addItem:), sep_first);
            send_v_id(app_menu.id(), sel!(addItem:), services_item.id());
            send_v_id(app_menu.id(), sel!(addItem:), hide_item.id());
            send_v_id(app_menu.id(), sel!(addItem:), hide_others_item.id());
            send_v_id(app_menu.id(), sel!(addItem:), show_all_item.id());
            send_v_id(app_menu.id(), sel!(addItem:), sep);
            send_v_id(app_menu.id(), sel!(addItem:), quit_item.id());
            send_v_id(app_menu_item.id(), sel!(setSubmenu:), app_menu.id());

            send_v_id(app, sel!(setServicesMenu:), services_menu.id());
            send_v_id(app, sel!(setMainMenu:), menubar.id());
        }
    });
}

/// `-[NSString stringByAppendingString:]`, +0 autoreleased.
///
/// # Safety
/// Both arguments must be live `NSString`s.
unsafe fn append(a: Id, b: Id) -> Id {
    // SAFETY: the caller pins both as live `NSString`s; the return is
    // autoreleased into the enclosing pool.
    unsafe { send_id_id(a, sel!(stringByAppendingString:), b) }
}

/// LOCAL PATCH (aterm): objc2's `ns_string!` built a STATIC `NSString` at
/// compile time. `aterm_objc::ns_string` answers a +1 [`Obj`]; this wrapper
/// autoreleases it so the call sites read as they did, at the cost of one
/// object per literal per launch that the enclosing pool reclaims.
fn nsstr(s: &str) -> Id {
    aterm_objc::ns_string(s).expect("Foundation to accept a menu title").autorelease()
}

/// LOCAL PATCH (aterm): answers an owning [`Obj`] where it answered a
/// `Retained<NSMenuItem>`, and takes the action as a bare [`Sel`] — [`Sel::NULL`]
/// for "no action" — where it took `Option<Sel>`.
///
/// # Safety
/// `title` and any `key_equivalent.key` must be live `NSString`s.
unsafe fn menu_item(
    mtm: MainThread,
    title: Id,
    selector: Sel,
    key_equivalent: Option<KeyEquivalent>,
) -> Obj {
    let (key, masks) = match key_equivalent {
        Some(ke) => (ke.key, ke.masks),
        None => (nsstr(""), None),
    };
    // SAFETY: `+alloc` answers +1 and the initialiser consumes it and answers
    // +1; `-initWithTitle:action:keyEquivalent:` is `@@:@:@` and a NIL action
    // is what AppKit reads as "no action". `mtm` witnesses the thread
    // `NSMenuItem` requires, and the title and key are live `NSString`s.
    let item = unsafe {
        let _ = mtm;
        let raw = alloc(class(c"NSMenuItem"));
        Obj::from_owned(send_id_id_sel_id(
            raw,
            sel!(initWithTitle:action:keyEquivalent:),
            title,
            selector,
            key,
        ))
        .expect("-[NSMenuItem initWithTitle:action:keyEquivalent:] to answer an item")
    };
    if let Some(masks) = masks {
        // SAFETY: `item` is a live `NSMenuItem` and `masks` is an `NSUInteger` modifier mask.
        unsafe { send_v_usize(item.id(), sel!(setKeyEquivalentModifierMask:), masks) };
    }

    item
}
