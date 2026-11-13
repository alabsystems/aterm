// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Center, DEFAULT_ACTION, clicked_session, status_answer};

#[test]
fn a_response_routes_to_the_session_its_notice_names() {
    let own = 4242;
    let ours = Some("4242");
    let incarnation = "p4242-captured-new-launch";
    let instance = Some(incarnation);
    assert_eq!(
        clicked_session(DEFAULT_ACTION, Some("7"), ours, instance, own, incarnation),
        Some(7)
    );
    assert_eq!(
        clicked_session(DEFAULT_ACTION, Some("0"), ours, instance, own, incarnation),
        Some(0)
    );
    for (case, action, session, pid) in [
        (
            "a dismissal",
            "com.apple.UNNotificationDismissActionIdentifier",
            Some("7"),
            ours,
        ),
        ("a custom action", "reply", Some("7"), ours),
        ("another process's", DEFAULT_ACTION, Some("7"), Some("4243")),
        ("no pid", DEFAULT_ACTION, Some("7"), None),
        ("a garbled pid", DEFAULT_ACTION, Some("7"), Some("42x")),
        ("about no tab", DEFAULT_ACTION, None, ours),
        ("a garbled session", DEFAULT_ACTION, Some("-1"), ours),
    ] {
        assert_eq!(
            clicked_session(action, session, pid, instance, own, incarnation),
            None,
            "{case}"
        );
    }
    for previous in [None, Some(""), Some("p4242-captured-old-launch")] {
        assert_eq!(
            clicked_session(DEFAULT_ACTION, Some("7"), ours, previous, own, incarnation),
            None,
            "an unbound or earlier incarnation must not select this launch's tab"
        );
    }
}

#[test]
fn macos_answers_read_as_ask_refuse_or_grant() {
    assert_eq!(status_answer(0), Center::Unasked);
    assert_eq!(status_answer(1), Center::Refused);
    assert_eq!(status_answer(3), Center::Provisional, "quiet delivery only");
    for granted in [2, 4] {
        assert_eq!(status_answer(granted), Center::Granted, "{granted}");
    }
    for unknown in [-1, 5, 99] {
        assert_eq!(status_answer(unknown), Center::Refused, "{unknown}");
    }
}

/// A unit test's center is inert whatever it is asked for: a windowed
/// instance's arms are not even compiled into a test build, so the
/// center reads absent, posts nothing, and an ask is only counted.
#[cfg(target_os = "macos")]
#[test]
fn a_test_build_never_reaches_the_notification_center() {
    for center in [
        super::NoticeCenter::for_instance(false),
        super::NoticeCenter::for_instance(true),
        super::NoticeCenter::inert(),
    ] {
        assert_eq!(center.answer(), Center::Absent);
        assert!(!center.post(&super::Notice {
            title: "aterm",
            body: "needs you",
            session: Some(0),
        }));
        center.adopt_clicks();
        let _ = super::take_asks();
        center.ask(Box::new(|| unreachable!("the inert ask answers nothing")));
        assert_eq!(super::take_asks(), 1);
    }
}

/// EVERY PROTOTYPE THE NOTICES STATE IS THE RUNTIME'S, read without the
/// center: the live arms run only in a bundle, where no test reaches them,
/// and a release build checks no send at the call.
#[cfg(target_os = "macos")]
#[test]
fn every_prototype_the_notices_state_is_the_runtimes() {
    use std::ffi::CStr;

    use aterm_objc::{
        ClassPtr, Id, Sel, block_check, block_signatures, class, method_types, protocol,
        protocol_method_extended_types, protocol_method_types, prototype_check, sel,
        strip_method_offsets,
    };

    use super::abi;

    type SendId = unsafe extern "C-unwind" fn(Id, Sel) -> Id;
    type SendVoidId = unsafe extern "C-unwind" fn(Id, Sel, Id);
    type SendIsize = unsafe extern "C-unwind" fn(Id, Sel) -> isize;
    type Alloc = unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id;

    assert!(super::open_framework(), "UserNotifications did not load");
    let named = |name: &'static CStr| {
        let cls = class(name);
        assert!(!cls.is_null(), "{name:?} is not registered");
        cls
    };
    // SAFETY: a live class's own class, its metaclass.
    let meta = |name: &'static CStr| unsafe { aterm_objc::class_of(named(name).as_id()) };
    let center = named(c"UNUserNotificationCenter");
    let content = named(c"UNMutableNotificationContent");

    // SAFETY: live classes; `prototype_check` reads an encoding and sends nothing.
    let sends: Vec<(&str, Result<(), String>)> = unsafe {
        vec![
            (
                "getNotificationSettingsWithCompletionHandler:",
                prototype_check::<abi::GetSettings>(
                    center,
                    sel!(getNotificationSettingsWithCompletionHandler:),
                ),
            ),
            (
                "requestAuthorizationWithOptions:completionHandler:",
                prototype_check::<abi::RequestAuthorization>(
                    center,
                    sel!(requestAuthorizationWithOptions:completionHandler:),
                ),
            ),
            (
                "addNotificationRequest:withCompletionHandler:",
                prototype_check::<abi::AddRequest>(
                    center,
                    sel!(addNotificationRequest:withCompletionHandler:),
                ),
            ),
            (
                "+requestWithIdentifier:content:trigger:",
                prototype_check::<abi::MakeRequest>(
                    meta(c"UNNotificationRequest"),
                    sel!(requestWithIdentifier:content:trigger:),
                ),
            ),
            (
                "+dictionaryWithObjects:forKeys:count:",
                prototype_check::<abi::MakeDictionary>(
                    meta(c"NSDictionary"),
                    sel!(dictionaryWithObjects:forKeys:count:),
                ),
            ),
            (
                "+currentNotificationCenter",
                prototype_check::<SendId>(
                    meta(c"UNUserNotificationCenter"),
                    sel!(currentNotificationCenter),
                ),
            ),
            (
                "setDelegate:",
                prototype_check::<SendVoidId>(center, sel!(setDelegate:)),
            ),
            (
                "authorizationStatus",
                prototype_check::<SendIsize>(
                    named(c"UNNotificationSettings"),
                    sel!(authorizationStatus),
                ),
            ),
            (
                "+alloc",
                prototype_check::<Alloc>(meta(c"UNMutableNotificationContent"), sel!(alloc)),
            ),
            ("init", prototype_check::<SendId>(content, sel!(init))),
            (
                "setTitle:",
                prototype_check::<SendVoidId>(content, sel!(setTitle:)),
            ),
            (
                "setBody:",
                prototype_check::<SendVoidId>(content, sel!(setBody:)),
            ),
            (
                "setUserInfo:",
                prototype_check::<SendVoidId>(content, sel!(setUserInfo:)),
            ),
            (
                "actionIdentifier",
                prototype_check::<SendId>(named(c"UNNotificationResponse"), sel!(actionIdentifier)),
            ),
            (
                "notification",
                prototype_check::<SendId>(named(c"UNNotificationResponse"), sel!(notification)),
            ),
            (
                "request",
                prototype_check::<SendId>(named(c"UNNotification"), sel!(request)),
            ),
            (
                "content",
                prototype_check::<SendId>(named(c"UNNotificationRequest"), sel!(content)),
            ),
            (
                "userInfo",
                prototype_check::<SendId>(named(c"UNNotificationContent"), sel!(userInfo)),
            ),
        ]
    };
    for (row, result) in sends {
        assert_eq!(result, Ok(()), "{row}");
    }
    // SAFETY: as above.
    let signed_twin = unsafe {
        prototype_check::<unsafe extern "C-unwind" fn(Id, Sel) -> usize>(
            named(c"UNNotificationSettings"),
            sel!(authorizationStatus),
        )
    };
    assert!(signed_twin.is_err(), "an unsigned status read passed");

    let server = protocol(c"UNUserNotificationServerProtocol");
    assert!(
        !server.is_null(),
        "UserNotifications no longer registers UNUserNotificationServerProtocol: \
         find where this macOS records the center's handler signatures"
    );
    let handler = |row: Sel| {
        // SAFETY: a live protocol object; a read-only query.
        let types = unsafe { protocol_method_extended_types(server, row, true) }
            .unwrap_or_else(|| panic!("{row:?} is not in the server protocol"));
        let blocks = block_signatures(&types);
        assert_eq!(blocks.len(), 1, "{row:?}: {types}");
        blocks[0].clone()
    };
    let settings = handler(sel!(getNotificationSettingsForBundleIdentifier:withCompletionHandler:));
    for (row, result) in [
        ("settings", block_check::<abi::SettingsHandler>(&settings)),
        (
            "authorization",
            block_check::<abi::AuthorizationHandler>(&handler(sel!(
                requestAuthorizationWithOptions:forBundleIdentifier:completionHandler:
            ))),
        ),
        (
            "add",
            block_check::<abi::AddHandler>(&handler(sel!(
                addNotificationRequest:forBundleIdentifier:withCompletionHandler:
            ))),
        ),
    ] {
        assert_eq!(result, Ok(()), "the {row} handler");
    }
    assert!(
        block_check::<fn(usize)>(&settings).is_err(),
        "a settings handler taking an options word passed"
    );

    let (delegate, cls) = delegate_class();
    let receive =
        sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:);
    let present = sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:);
    for row in [receive, present] {
        // SAFETY: a live protocol and a class this module declared; read-only queries.
        let (want, got) = unsafe {
            (
                protocol_method_types(delegate, row, true),
                method_types(cls, row),
            )
        };
        let want = want.unwrap_or_else(|| panic!("{row:?} is not a delegate row"));
        assert_eq!(
            got.as_deref(),
            Some(strip_method_offsets(&want).as_str()),
            "{row:?}"
        );
    }
    assert_eq!(
        block_check::<abi::ResponseDone>(&delegate_block(delegate, receive)),
        Ok(()),
        "the response handler"
    );
    assert_eq!(
        block_check::<abi::PresentDone>(&delegate_block(delegate, present)),
        Ok(()),
        "the presentation handler"
    );

    unsafe extern "C" {
        fn dlsym(
            handle: *mut std::ffi::c_void,
            symbol: *const std::ffi::c_char,
        ) -> *mut std::ffi::c_void;
    }
    const RTLD_DEFAULT: *mut std::ffi::c_void = -2_isize as *mut std::ffi::c_void;
    // SAFETY: a NUL-terminated symbol name; the answer is the address of an `NSString *const`, or null.
    let symbol = unsafe {
        dlsym(
            RTLD_DEFAULT,
            c"UNNotificationDefaultActionIdentifier".as_ptr(),
        )
    };
    assert!(
        !symbol.is_null(),
        "no UNNotificationDefaultActionIdentifier"
    );
    // SAFETY: the exported constant is an immortal `NSString *`.
    let exported = unsafe { aterm_objc::ns_string_to_rust(*symbol.cast::<Id>()) };
    assert_eq!(exported, super::DEFAULT_ACTION);
}

/// THE DELEGATE'S CLASS, claiming the host's
/// `UNUserNotificationCenterDelegate`, with that protocol: the protocol
/// must be the host's before the class claims it, or the claim registers a
/// stand-in with no rows that every later reader sees, so each test that
/// declares the class asks here first.
#[cfg(target_os = "macos")]
fn delegate_class() -> (aterm_objc::ProtocolPtr, aterm_objc::ClassPtr) {
    use aterm_objc::ClassType as _;
    let delegate = aterm_objc::protocol(c"UNUserNotificationCenterDelegate");
    assert!(
        !delegate.is_null()
            && !aterm_objc::protocols_registered_by_aterm()
                .contains(&c"UNUserNotificationCenterDelegate"),
        "this host registers no UNUserNotificationCenterDelegate of its own"
    );
    (delegate, super::clicks::NoticeDelegate::class())
}

/// Stand-ins for what macOS hands the delegate: a response
/// (`UNNotificationResponse`) for the notice it answers (`UNNotification`)
/// for its request (`UNNotificationRequest`) for its content
/// (`UNNotificationContent`), each an `NSObject` subclass made at run time
/// that answers only the selectors the delegate reads off that link, with
/// the runtime's `-(id)` encoding.
#[cfg(target_os = "macos")]
mod stand_ins {
    use aterm_objc::{Id, Obj, Retained};

    aterm_objc::declare_class! {
        /// A `UNNotificationResponse`: the action taken, and the notice.
        pub(super) struct Response: NSObject {
            const NAME: &str = "ATermTestNoticeResponse";
            type Ivars = (Obj, Retained<Notification>);

            @sel(actionIdentifier)
            fn action_identifier(&self) -> Id {
                self.ivars().0.id()
            }

            @sel(notification)
            fn notification(&self) -> Id {
                self.ivars().1.as_id()
            }
        }
    }

    aterm_objc::declare_class! {
        /// A `UNNotification`: its request.
        pub(super) struct Notification: NSObject {
            const NAME: &str = "ATermTestNotice";
            type Ivars = Retained<Request>;

            @sel(request)
            fn request(&self) -> Id {
                self.ivars().as_id()
            }
        }
    }

    aterm_objc::declare_class! {
        /// A `UNNotificationRequest`: its content.
        pub(super) struct Request: NSObject {
            const NAME: &str = "ATermTestNoticeRequest";
            type Ivars = Retained<Content>;

            @sel(content)
            fn content(&self) -> Id {
                self.ivars().as_id()
            }
        }
    }

    aterm_objc::declare_class! {
        /// A `UNNotificationContent`: its `userInfo`, or nil.
        pub(super) struct Content: NSObject {
            const NAME: &str = "ATermTestNoticeContent";
            type Ivars = Option<Obj>;

            @sel(userInfo)
            fn user_info(&self) -> Id {
                self.ivars().as_ref().map_or(Id::NIL, Obj::id)
            }
        }
    }

    /// A notice whose content carries `info`.
    pub(super) fn notification(info: Option<Obj>) -> Option<Retained<Notification>> {
        let witness = crate::appkit::test_witness();
        let content = Content::alloc_init(witness, info)?;
        let request = Request::alloc_init(witness, content)?;
        Notification::alloc_init(witness, request)
    }

    /// The response `action` makes to a notice whose content carries `info`.
    pub(super) fn response(action: &str, info: Option<Obj>) -> Option<Retained<Response>> {
        let action = aterm_objc::ns_string(action)?;
        Response::alloc_init(crate::appkit::test_witness(), (action, notification(info)?))
    }
}

/// The `userInfo` a post writes for `session` ([`super::user_info`]),
/// owned.
#[cfg(target_os = "macos")]
fn posted_info(session: Option<u64>) -> Option<aterm_objc::Obj> {
    aterm_objc::autoreleasepool(|_| {
        // SAFETY: an autoreleased dictionary, live for this pool, retained to outlive it.
        super::user_info(session).and_then(|info| unsafe { aterm_objc::Obj::retain(info) })
    })
}

/// A CLICK ON ONE OF aterm's NOTICES GOES TO ITS SESSION, through the
/// real delegate: `-userNotificationCenter:didReceiveNotificationResponse:
/// withCompletionHandler:` sent to a `NoticeDelegate`, with a response
/// built of stand-ins ([`stand_ins`]) around the very `userInfo` a post
/// writes ([`super::user_info`]) and a real block for the completion
/// handler.
#[cfg(target_os = "macos")]
#[test]
fn a_click_on_a_notice_goes_to_its_session() {
    use std::cell::Cell;
    use std::rc::Rc;

    use aterm_objc::{BlockPtr, Id, RcBlock, Sel, checked_msg, sel};

    type DidReceive = unsafe extern "C-unwind" fn(Id, Sel, Id, Id, BlockPtr);

    let _ = delegate_class();
    let delegate = super::clicks::NoticeDelegate::alloc_init(crate::appkit::test_witness(), ())
        .expect("a delegate");
    let respond = |action: &str, session: Option<u64>| {
        let response = stand_ins::response(action, posted_info(session)).expect("a response");
        let calls = Rc::new(Cell::new(0_u32));
        let counted = Rc::clone(&calls);
        // SAFETY: `abi::ResponseDone` is the handler's prototype, and the closure's.
        let done = unsafe {
            RcBlock::of::<super::abi::ResponseDone, _>(move || counted.set(counted.get() + 1))
        }
        .expect("a handler block");
        let _ = crate::notify::take_wakes();
        aterm_objc::autoreleasepool(|_| {
            // SAFETY: the delegate row is `DidReceive`; the delegate and
            // the response are live, the center nil (the delegate reads
            // none) and the block a live `abi::ResponseDone`.
            unsafe {
                let send: DidReceive = checked_msg();
                send(
                    delegate.as_id(),
                    sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
                    Id::NIL,
                    response.as_id(),
                    done.as_block_ptr(),
                );
            }
        });
        (crate::notify::take_wakes(), calls.get())
    };

    let (wakes, calls) = respond(DEFAULT_ACTION, Some(7));
    assert!(
        matches!(
            wakes.as_slice(),
            [crate::Wake::NoticeClicked { session: 7 }]
        ),
        "the click goes to session 7: {wakes:?}"
    );
    assert_eq!(calls, 1, "the handler runs once");
    for (case, action, session) in [
        ("a notice about no tab", DEFAULT_ACTION, None),
        (
            "a dismissal",
            "com.apple.UNNotificationDismissActionIdentifier",
            Some(7),
        ),
    ] {
        let (wakes, calls) = respond(action, session);
        assert!(wakes.is_empty(), "{case} goes nowhere: {wakes:?}");
        assert_eq!(calls, 1, "{case}: the handler runs once");
    }
}

/// A NOTICE THAT ARRIVES WHILE aterm IS IN FRONT STILL SHOWS, through the
/// real delegate's `-userNotificationCenter:willPresentNotification:
/// withCompletionHandler:`, with a stand-in notice about a tab and a real
/// block: the handler is handed List | Banner
/// (`UNNotificationPresentationOptions`, `1 << 3 | 1 << 4`: no sound, no
/// badge), exactly once, and the presentation goes to no tab, since it is
/// not a click.
#[cfg(target_os = "macos")]
#[test]
fn a_notice_shows_while_aterm_is_in_front() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use aterm_objc::{BlockPtr, Id, RcBlock, Sel, checked_msg, sel};

    type WillPresent = unsafe extern "C-unwind" fn(Id, Sel, Id, Id, BlockPtr);
    const LIST_AND_BANNER: usize = (1 << 3) | (1 << 4);

    let _ = delegate_class();
    let delegate = super::clicks::NoticeDelegate::alloc_init(crate::appkit::test_witness(), ())
        .expect("a delegate");
    let notice = stand_ins::notification(posted_info(Some(7))).expect("a notice");
    let handed = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&handed);
    // SAFETY: `abi::PresentDone` is the handler's prototype, and the closure's.
    let done = unsafe {
        RcBlock::of::<super::abi::PresentDone, _>(move |options: usize| {
            seen.borrow_mut().push(options);
        })
    }
    .expect("a handler block");
    let _ = crate::notify::take_wakes();
    aterm_objc::autoreleasepool(|_| {
        // SAFETY: the delegate row is `WillPresent`; the delegate and the
        // notice are live, the center nil and the block a live
        // `abi::PresentDone`.
        unsafe {
            let send: WillPresent = checked_msg();
            send(
                delegate.as_id(),
                sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
                Id::NIL,
                notice.as_id(),
                done.as_block_ptr(),
            );
        }
    });
    assert_eq!(*handed.borrow(), vec![LIST_AND_BANNER]);
    let wakes = crate::notify::take_wakes();
    assert!(wakes.is_empty(), "showing is not a click: {wakes:?}");
}

/// The one block signature a delegate protocol row records.
#[cfg(target_os = "macos")]
fn delegate_block(delegate: aterm_objc::ProtocolPtr, row: aterm_objc::Sel) -> String {
    // SAFETY: a live protocol object; a read-only query.
    let extended = unsafe { aterm_objc::protocol_method_extended_types(delegate, row, true) }
        .unwrap_or_else(|| panic!("{row:?} has no extended row"));
    let blocks = aterm_objc::block_signatures(&extended);
    assert_eq!(blocks.len(), 1, "{row:?}: {extended}");
    blocks[0].clone()
}
