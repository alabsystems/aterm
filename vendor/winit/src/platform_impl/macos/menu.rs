// Modified by the aterm project in 2026; see the repository NOTICE.
// (Every `objc2` binding call in this file was replaced with a typed send
// through `aterm_objc`. `initialize` takes a raw `id` and a main-thread
// witness instead of an `&NSApplication`, and `menu_item` answers an
// `aterm_objc::Obj` instead of a `Retained<NSMenuItem>`.)

// LOCAL PATCH (aterm): objc2's `Retained`/`Sel`/`sel!`/`ns_string!` and its
// eight `NSApplication`, `NSMenu`, `NSMenuItem`, `NSProcessInfo` and `NSString`
// bindings are gone. The three `NSEventModifierFlags` bits this file reads are
// the seam's constants, at the SDK's own values.
use aterm_objc::send::{alloc, send_id, send_id_id, send_id_id_sel_id, send_v_id, send_v_usize};
use aterm_objc::{Id, MainThread, Obj, Sel, autoreleasepool, class, sel};

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
/// LOCAL PATCH (aterm): takes the application as a raw `id` plus a
/// [`MainThread`] witness, where it took an `&NSApplication` and derived
/// `MainThreadMarker::from(app)` from the binding's type. The witness is not
/// decoration and it is not merely preserved for symmetry: `objc2-app-kit`
/// typed `NSMenu` and `NSMenuItem` `MainThreadOnly`, so `+new` on either was
/// reachable only through a marker, and dropping the parameter would delete a
/// requirement upstream enforced rather than port it.
///
/// # Safety
///
/// `app` must be a live `NSApplication`.
pub(super) unsafe fn initialize(app: Id, mtm: MainThread) {
    // The +0 autoreleased returns below — `+processInfo`, `-processName`,
    // `-stringByAppendingString:` and `+separatorItem` — are borrowed until a
    // pool pops. AppKit wraps its own pool around the delegate callback this
    // runs inside, but the crate's rule is that an autoreleased return's
    // lifetime is the CALLER's obligation, so the scope is explicit here.
    autoreleasepool(|_| {
        // SAFETY (for this whole block): every receiver below is either a
        // class object from `class()`, a +1 handle this scope owns, or a
        // borrowed autoreleased return that is live until the pool pops; each
        // send's prototype is censused in
        // `aterm-objc/tests/winit_sent_prototypes.rs` against the runtime's own
        // encoding. `mtm` witnesses the thread `NSMenu` and `NSMenuItem`
        // require.
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

            // About menu item
            let about_item_title = append(nsstr("About "), process_name);
            let about_item = menu_item(
                mtm,
                about_item_title,
                sel!(orderFrontStandardAboutPanel:),
                None,
            );

            // Services menu item
            let services_menu = Obj::from_owned(send_id(class(c"NSMenu").as_id(), sel!(new)))
                .expect("+[NSMenu new] to answer a menu");
            let services_item = menu_item(mtm, nsstr("Services"), Sel::NULL, None);
            send_v_id(services_item.id(), sel!(setSubmenu:), services_menu.id());

            // Separator menu item
            let sep_first = send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem));

            // Hide application menu item
            let hide_item_title = append(nsstr("Hide "), process_name);
            let hide_item = menu_item(
                mtm,
                hide_item_title,
                sel!(hide:),
                Some(KeyEquivalent { key: nsstr("h"), masks: None }),
            );

            // Hide other applications menu item
            let hide_others_item = menu_item(
                mtm,
                nsstr("Hide Others"),
                sel!(hideOtherApplications:),
                Some(KeyEquivalent {
                    key: nsstr("h"),
                    masks: Some(
                        NS_EVENT_MODIFIER_FLAG_OPTION | NS_EVENT_MODIFIER_FLAG_COMMAND,
                    ),
                }),
            );

            // Show applications menu item
            let show_all_item =
                menu_item(mtm, nsstr("Show All"), sel!(unhideAllApplications:), None);

            // Separator menu item
            let sep = send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem));

            // Quit application menu item
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

/// An `NSString` for a literal, +0 for the rest of the enclosing pool.
///
/// LOCAL PATCH (aterm): objc2's `ns_string!` built a STATIC `NSString` at
/// compile time. `aterm_objc::ns_string` answers a +1 [`Obj`]; this wrapper
/// autoreleases it so the call sites read as they did, at the cost of one
/// object per literal per launch that the enclosing pool reclaims.
fn nsstr(s: &str) -> Id {
    aterm_objc::ns_string(s)
        .expect("Foundation to accept a menu title")
        .autorelease()
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
    // is what AppKit reads as "no action", which is what `Option<Sel>::None`
    // compiled to before. `mtm` witnesses the thread `NSMenuItem` requires.
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
        // SAFETY: `item` owns a +1 to a live `NSMenuItem`;
        // `-setKeyEquivalentModifierMask:` takes an `NSEventModifierFlags`,
        // which is an `NSUInteger`.
        unsafe { send_v_usize(item.id(), sel!(setKeyEquivalentModifierMask:), masks) };
    }

    item
}
