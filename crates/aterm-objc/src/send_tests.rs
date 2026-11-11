// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn same(a: &str, b: &str) -> bool {
    abi_shape(a) == abi_shape(b)
}

#[test]
fn the_shape_keeps_sign_width_and_layout_and_merges_pointers() {
    assert!(!same("q@:Q", "Q@:Q"));
    assert!(!same("f@:", "d@:"));
    assert!(!same("S@:", "Q@:"));
    assert!(!same("v@:{CGSize=dd}", "v@:{CGPoint=dd}"));
    assert!(!same("v@:@", "v@:@@"), "the arity counts");
    assert!(same("v@:^{CGColor=}", "v@:@"));
    assert!(same("v@:@?", "v@:@"));
    assert!(same("@@:r^@Q", "@@:^@Q"));
    assert!(same("B@:#", "B@:@"));
    assert!(same("Vv@:", "v@:"));
    assert!(same("v@:@\"NSString\"", "v@:@"));
    assert!(same("@@:^*qqqqBB@qq", "@@:^*qqqqBB@qq"));
    assert_eq!(
        abi_shape("{CGRect={CGPoint=dd}{CGSize=dd}}@:[4i]^^v"),
        ["{CGRect={CGPoint=dd}{CGSize=dd}}", "p", "p", "[4i]", "p"]
    );
}

#[test]
fn a_swift_override_may_differ_only_in_a_wide_integers_sign() {
    let swift = "_TtGCs23_ContiguousArrayStorageCSo8NSScreen_$";
    assert!(swift_override_of_the_same_width(swift, "q@:", "Q@:"));
    assert!(swift_override_of_the_same_width(swift, "v@:i", "v@:I"));
    assert!(swift_override_of_the_same_width(
        "Swift.__StringStorage",
        "q@:",
        "Q@:"
    ));
    assert!(swift_override_of_the_same_width(
        "Swift.__StringStorage",
        "v@:^S{?=qq}",
        "v@:^S{_NSRange=QQ}"
    ));
    assert!(swift_override_of_the_same_width(
        "Swift.__StringStorage",
        "c@:*qQ",
        "B@:*QQ"
    ));
    assert!(!swift_override_of_the_same_width(
        "Swift.__StringStorage",
        "v@:^S{?=qi}",
        "v@:^S{_NSRange=QQ}"
    ));
    assert!(!swift_override_of_the_same_width(
        "Swift.__StringStorage",
        "v@:^S{?=dd}",
        "v@:^S{_NSRange=QQ}"
    ));
    assert!(!swift_override_of_the_same_width(
        "NSWindow", "q@:Q", "Q@:Q"
    ));
    assert!(!swift_override_of_the_same_width(swift, "v@:s", "v@:S"));
    assert!(!swift_override_of_the_same_width(swift, "q@:", "I@:"));
}

#[test]
fn a_prototype_is_checked_without_a_send() {
    let string = crate::class(c"NSString");
    // SAFETY: live classes; nothing below sends a message.
    unsafe {
        prototype_check::<unsafe extern "C-unwind" fn(Id, Sel) -> usize>(
            string,
            crate::sel!(length),
        )
        .expect("-[NSString length] is NSUInteger");
        let wrong = prototype_check::<unsafe extern "C-unwind" fn(Id, Sel) -> isize>(
            string,
            crate::sel!(length),
        )
        .expect_err("a signed -length");
        assert!(
            wrong.contains("`Q@:`") && wrong.contains("`q@:`"),
            "{wrong}"
        );
        let meta = crate::class_of(string.as_id());
        prototype_check::<unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id>(
            meta,
            crate::sel!(string),
        )
        .expect("+[NSString string] is on the metaclass");
        assert!(
            prototype_check::<unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id>(
                string,
                crate::sel!(string),
            )
            .is_err(),
            "+string is no instance method"
        );
        assert!(
            prototype_check::<unsafe extern "C-unwind" fn(Id, Sel)>(
                string,
                crate::sel!(atermNoSuchSelector),
            )
            .is_err()
        );
    }
}

#[test]
fn a_block_prototype_is_checked_against_its_recorded_signature() {
    assert_eq!(
        block_signatures("v40@0:8@\"UNUserNotificationCenter\"16@\"UNNotification\"24@?<v@?Q>32"),
        ["v@?Q"]
    );
    assert_eq!(
        block_signatures("v32@0:8@?<v@?@\"NSError\">16@?<v@?@?<v@?B>>24"),
        ["v@?@\"NSError\"", "v@?@?<v@?B>"]
    );
    assert!(
        block_signatures("v24@0:8@?16").is_empty(),
        "a bare block records none"
    );
    assert!(
        same("v@:@?<v@?Q>@", "v@:@?@"),
        "a recorded signature is one pointer"
    );
    block_check::<fn(usize)>("v@?Q").expect("NSUInteger options");
    block_check::<fn()>("v@?").expect("no arguments");
    block_check::<fn(Bool, Id)>(&format!("v@?{}@\"NSError\"", Bool::ENCODING))
        .expect("BOOL granted, NSError *error");
    block_check::<fn(Id)>("v@?@\"UNNotificationSettings\"").expect("one object");
    for (case, result) in [
        ("a signed options word", block_check::<fn(isize)>("v@?Q")),
        ("an argument too many", block_check::<fn(Id)>("v@?")),
        ("an argument too few", block_check::<fn()>("v@?@")),
        (
            "a scalar for an object's place",
            block_check::<fn(i32)>("v@?@"),
        ),
        ("a return", block_check::<fn() -> usize>("v@?")),
        ("non-ASCII signature bytes", block_check::<fn()>("é")),
        (
            "non-ASCII bytes after valid prefix",
            block_check::<fn()>("v@?é"),
        ),
    ] {
        assert!(result.is_err(), "{case}");
    }
    assert_eq!(
        block_signatures("v24@0:8@?<\"unterminated>"),
        ["\"unterminated>"],
        "a '>' inside an unterminated quote is not a closing angle bracket"
    );
}

#[test]
#[cfg(debug_assertions)]
fn a_send_cast_to_the_wrong_prototype_panics_in_a_debug_build() {
    let s = crate::ns_string("four").expect("NSString");
    // SAFETY: `-length` is `-(NSUInteger)length` on a live NSString.
    assert_eq!(unsafe { send_usize(s.id(), crate::sel!(length)) }, 4);
    let wrong = std::panic::catch_unwind(|| {
        // SAFETY: never sent: the check refuses the signed prototype first.
        unsafe { send_isize(s.id(), crate::sel!(length)) }
    });
    let message = wrong.expect_err("a signed read of `-length` must panic");
    let message = message.downcast_ref::<String>().expect("a formatted panic");
    assert!(
        message.contains("length") && message.contains("`Q@:`") && message.contains("`q@:`"),
        "{message}"
    );
}

#[test]
#[cfg(debug_assertions)]
fn a_reused_checked_cast_is_checked_per_receiver_and_selector() {
    let s = crate::ns_string("four").expect("NSString");
    // SAFETY: `s` is a live NSString; `-length`, `-hash` and `+hash` return `NSUInteger`.
    let (len, _, _) = unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        let g: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> usize = checked_msg();
        (
            f(s.id(), crate::sel!(length)),
            f(s.id(), crate::sel!(hash)),
            g(crate::class(c"NSString"), crate::sel!(hash)),
        )
    };
    assert_eq!(len, 4);
    for attempt in 0..2 {
        let wrong = std::panic::catch_unwind(|| {
            // SAFETY: never sent; the check refuses `NSUInteger` for `@@:` first.
            unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
                f(s.id(), crate::sel!(description))
            }
        });
        assert!(
            wrong.is_err(),
            "attempt {attempt}: a wrong prototype was sent"
        );
    }
    let wrong_class = std::panic::catch_unwind(|| {
        // SAFETY: never sent: the check refuses it first.
        unsafe {
            let f: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Bool = checked_msg();
            f(crate::class(c"NSString"), crate::sel!(string))
        }
    });
    assert!(
        wrong_class.is_err(),
        "a class method's prototype went unchecked"
    );
}
