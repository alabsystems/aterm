// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn policy_preserves_other_arguments_without_writing_persistent_preferences() {
    // SAFETY: these Foundation objects are created, queried and released on this test's
    // thread. The suite is private to this test process; no AppKit object is created.
    autoreleasepool(|_| unsafe {
        let suite = ns_string(&format!(
            "org.alabsystems.aterm.tests.native-restoration.{}",
            std::process::id()
        ))
        .unwrap();
        let defaults = Obj::from_owned(send_id_id(
            send_id(FOUNDATION.class(c"NSUserDefaults").as_id(), sel!(alloc)),
            sel!(initWithSuiteName:),
            suite.id(),
        ))
        .unwrap();
        let domain = argument_domain();
        let arguments = Obj::from_owned(send_id(
            FOUNDATION.class(c"NSMutableDictionary").as_id(),
            sel!(new),
        ))
        .unwrap();
        let sentinel = ns_string("unrelated-launch-argument").unwrap();
        let value = ns_string("preserved").unwrap();
        send_v_id_id(
            arguments.id(),
            sel!(setObject:forKey:),
            value.id(),
            sentinel.id(),
        );
        let keep = ns_string("NSQuitAlwaysKeepsWindows").unwrap();
        let ignore = ns_string("ApplePersistenceIgnoreState").unwrap();
        let number: unsafe extern "C-unwind" fn(Id, Sel, Bool) -> Id = aterm_objc::checked_msg();
        let class = FOUNDATION.class(c"NSNumber").as_id();
        send_v_id_id(
            arguments.id(),
            sel!(setObject:forKey:),
            number(class, sel!(numberWithBool:), Bool::YES),
            keep.id(),
        );
        send_v_id_id(
            arguments.id(),
            sel!(setObject:forKey:),
            number(class, sel!(numberWithBool:), Bool::NO),
            ignore.id(),
        );
        send_v_id_id(
            defaults.id(),
            sel!(setVolatileDomain:forName:),
            arguments.id(),
            domain,
        );
        let before = send_id_id(defaults.id(), sel!(persistentDomainForName:), suite.id());
        assert!(before.is_null() || aterm_objc::send::send_usize(before, sel!(count)) == 0);
        assert!(send_bool_id(defaults.id(), sel!(boolForKey:), keep.id()));
        assert!(!send_bool_id(defaults.id(), sel!(boolForKey:), ignore.id()));

        apply_to(defaults.id()).unwrap();
        assert!(!send_bool_id(defaults.id(), sel!(boolForKey:), keep.id()));
        assert!(send_bool_id(defaults.id(), sel!(boolForKey:), ignore.id()));
        let kept = send_id_id(defaults.id(), sel!(objectForKey:), sentinel.id());
        assert!(send_bool_id(kept, sel!(isEqualToString:), value.id()));
        let original = send_id_id(arguments.id(), sel!(objectForKey:), keep.id());
        assert!(aterm_objc::send::send_bool(original, sel!(boolValue)));
        let after = send_id_id(defaults.id(), sel!(persistentDomainForName:), suite.id());
        assert!(after.is_null() || aterm_objc::send::send_usize(after, sel!(count)) == 0);
    });
}
