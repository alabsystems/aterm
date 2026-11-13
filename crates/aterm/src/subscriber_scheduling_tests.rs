// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

fn words(name: &str, args: &[&str]) -> Vec<OsString> {
    std::iter::once(name)
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect()
}

#[test]
fn the_plain_watch_holds_before_dispatch_without_replacing_itself() {
    let argv = words("/fixture/renamed-aterm", &["secrets", "watch"]);
    let original = argv.clone();
    let events = RefCell::new(Vec::new());
    enter_with(&argv, || events.borrow_mut().push("hold"));
    events.borrow_mut().push("dispatch");
    assert_eq!(events.into_inner(), ["hold", "dispatch"]);
    assert_eq!(argv, original);
}

#[test]
fn help_invalid_watch_and_other_alias_routes_do_not_change_process_policy() {
    for name in ["/fixture/aterm", "/fixture/atpkg", "/fixture/aterm-fleet"] {
        for args in [
            &["secrets", "watch", "--help"][..],
            &["secrets", "watch", "extra"],
            &["secrets", "sync"],
            &["secrets", "serve"],
            &["secrets", "not-watch"],
            &[],
        ] {
            let argv = words(name, args);
            let original = argv.clone();
            enter_with(&argv, || panic!("this route must not install a hold"));
            assert_eq!(argv, original);
        }
    }
    for name in [
        "/fixture/atpkg",
        "/fixture/aterm-fleet",
        "/fixture/aterm-gui",
    ] {
        let argv = words(name, &["secrets", "watch"]);
        let original = argv.clone();
        enter_with(&argv, || panic!("an alias must keep its own routing"));
        assert_eq!(argv, original);
    }
}
