// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn exact_owner_and_bounded_pid_free_grammar() {
    assert!(allowed(Scope::Owner, Principal::Instance, false, false));
    assert!(!allowed(Scope::Owner, Principal::Harness, false, false));
    for (scope, selected, remote) in [
        (Scope::Owner, true, false),
        (Scope::Owner, false, true),
        (Scope::Bridge, false, false),
        (
            Scope::Fleet {
                content: true,
                drive: true,
            },
            false,
            false,
        ),
    ] {
        assert!(!allowed(scope, Principal::Instance, selected, remote));
    }
    assert_eq!(
        parse("inspect session=s-a912afd80a5a598f9ace"),
        Some(Command::Inspect("s-a912afd80a5a598f9ace"))
    );
    assert_eq!(parse("attach"), Some(Command::Attach));
    assert_eq!(
        parse("attach-accounted-original-store-v1"),
        Some(Command::AttachAccountedExpected)
    );
    assert_eq!(
        parse("attach-accounted-native-job-original-store-v1"),
        Some(Command::AttachNativeExpected)
    );
    assert_eq!(parse("capabilities"), Some(Command::Capabilities));
    let nonce = "a".repeat(64);
    assert_eq!(
        parse(&format!("enroll nonce={nonce}")),
        Some(Command::Enroll(&nonce))
    );
    for rest in [
        "",
        "attach session=s-a",
        "attach-accounted-original-store-v1 path=/foreign",
        "attach-accounted-native-job-original-store-v1 path=/foreign",
        "inspect pid=2",
        "inspect session=s-a cpu_units=1",
        "inspect session=s-A",
        "inspect session=s-",
        "capabilities extra",
        "enroll nonce=a",
        "enroll nonce=ABC",
        "enroll pid=1",
    ] {
        assert!(parse(rest).is_none(), "{rest}");
    }
    assert!(parse(&format!("inspect session=s-{}", "a".repeat(63))).is_none());
}
