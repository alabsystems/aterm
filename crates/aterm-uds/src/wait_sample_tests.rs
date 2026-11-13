// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::process::Birth;

const UUID: &str = "F63BF418-8F75-34A4-AAF1-8CAF2B9B6A05";
const C_UUID: &str = "FBA7B23E-AA60-3A90-9AA4-A7A2E0AD63EE";

/// Once a native report exists, a collection failure keeps both its original I/O class and
/// the report path. NEGATIVE CONTROL: a plain error carries no path.
#[test]
fn a_failed_sample_can_return_its_retained_report() {
    let path = Path::new("private/wait-deadbeef.sample.txt");
    let error = retain_report(
        io::Error::new(io::ErrorKind::PermissionDenied, "sample refused"),
        path,
    );
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(error.to_string(), "sample refused");
    assert_eq!(retained_report(&error), Some(path));

    let before_reservation = io::Error::new(io::ErrorKind::NotFound, "identity vanished");
    assert_eq!(retained_report(&before_reservation), None);
}

fn decode(report: &str, uuid: &str) -> Kind {
    classify(report, &kernel(uuid), Some(&c_image()))
}

fn kernel(uuid: &str) -> Image {
    Image {
        name: KERNEL_NAME,
        path: KERNEL_PATH,
        uuid: uuid.to_owned(),
        start: 0x1000,
        end: 0x1fff,
    }
}

fn c_image() -> Image {
    Image {
        name: C_NAME,
        path: C_PATH,
        uuid: C_UUID.to_owned(),
        start: 0x7000,
        end: 0x7fff,
    }
}

fn report(graph: &str) -> String {
    format!(
        "Analysis of sampling own stand-in\n\nCall graph:\n{graph}\n\nTotal number in stack (recursive counted multiple, when >=5):\n\nBinary Images:\n  0x1000 - 0x1fff libsystem_kernel.dylib (13432.1.9) <{UUID}> {KERNEL_PATH}\n  0x7000 - 0x7fff libsystem_c.dylib (1786.0.3) <{C_UUID}> {C_PATH}\n"
    )
}

const WAIT: &str = "    10 Thread_166387592 DispatchQueue_1: com.apple.main-thread\n      10 start  (in dyld) + 6688  [0x9000]\n        10 ???  (in stand-in) load address 0x8000 + 0x740  [0x8740]\n          10 read  (in libsystem_kernel.dylib) + 8  [0x1008]";
// The shape observed in an owned /bin/sleep's sample, with normalized addresses.
const TIMER: &str = "    10 Thread_166387592 DispatchQueue_1: com.apple.main-thread\n      10 start  (in dyld) + 6688  [0x9000]\n        10 ???  (in sleep) load address 0x8000 + 0x740  [0x8740]\n          10 nanosleep  (in libsystem_c.dylib) + 220  [0x7008]\n            10 __semwait_signal  (in libsystem_kernel.dylib) + 8  [0x1008]";

#[test]
fn a_timer_needs_its_authenticated_direct_nanosleep_caller() {
    assert_eq!(decode(&report(TIMER), UUID), Kind::Timer);
    let nocancel = TIMER
        .replace("nanosleep ", "nanosleep$NOCANCEL ")
        .replace("__semwait_signal ", "__semwait_signal_nocancel ");
    assert_eq!(decode(&report(&nocancel), UUID), Kind::Timer);
    assert_eq!(
        classify(&report(TIMER), &kernel(UUID), None),
        Kind::Unparsed
    );
    for graph in [
        TIMER.replace("nanosleep  (in libsystem_c.dylib)", "nanosleep  (in stand-in)"),
        TIMER.replace("[0x7008]", "[0x8008]"),
        TIMER.replace("          10 nanosleep  (in libsystem_c.dylib) + 220  [0x7008]\n", ""),
        TIMER.replace("            10 __semwait_signal", "            10 wrapper  (in stand-in) + 8  [0x8008]\n              10 __semwait_signal"),
        format!("{TIMER}\n        + 5 wrapper  (in stand-in) + 8  [0x8008]\n        +   5 __semwait_signal  (in libsystem_kernel.dylib) + 8  [0x1008]"),
    ] {
        assert_eq!(decode(&report(&graph), UUID), Kind::Unparsed);
    }
    for text in [
        report(TIMER).replace(C_PATH, "/tmp/libsystem_c.dylib"),
        report(TIMER).replace(C_UUID, "00000000-0000-0000-0000-000000000000"),
        report(TIMER).replace("0x7000 - 0x7fff", "0x6000 - 0x7fff"),
        report(TIMER).replace("0x7000 - 0x7fff", "0x7000 - 0x8fff"),
    ] {
        assert_eq!(decode(&text, UUID), Kind::Unparsed);
    }
    let sibling = format!(
        "{TIMER}\n          + 5 __semwait_signal  (in libsystem_kernel.dylib) + 8  [0x1008]"
    );
    assert_eq!(decode(&report(&sibling), UUID), Kind::Timer);
}

#[test]
fn an_apple_report_names_only_its_authenticated_waiting_leaves() {
    assert_eq!(decode(&report(WAIT), UUID), Kind::Read);
    for (symbol, kind) in [
        ("flock", Kind::Lock),
        ("read", Kind::Read),
        ("kevent", Kind::Event),
        ("mach_msg2_trap", Kind::Ipc),
        ("__psynch_mutexwait", Kind::Mutex),
        ("wait4", Kind::Children),
    ] {
        let graph = WAIT.replace("10 read ", &format!("10 {symbol} "));
        assert_eq!(decode(&report(&graph), UUID), kind);
    }
    for symbol in [
        "fcntl",
        "__fcntl",
        "__fcntl_nocancel",
        "sigsuspend",
        "__sigsuspend",
        "__sigsuspend_nocancel",
        "__semwait_signal",
        "__semwait_signal_nocancel",
    ] {
        assert_eq!(
            decode(
                &report(&WAIT.replace("10 read ", &format!("10 {symbol} "))),
                UUID
            ),
            Kind::Unparsed
        );
    }
}

#[test]
fn a_program_symbol_or_image_cannot_name_a_wait() {
    for graph in [
        WAIT.replace("read  (in libsystem_kernel.dylib)", "read  (in stand-in)"),
        WAIT.replace("[0x1008]", "[0x8008]"),
        format!("{WAIT}\n            10 user_work  (in stand-in) + 8  [0x8008]"),
    ] {
        assert_eq!(decode(&report(&graph), UUID), Kind::Unparsed);
    }
    let own_image = report(WAIT).replace(KERNEL_PATH, "/tmp/libsystem_kernel.dylib");
    assert_eq!(decode(&own_image, UUID), Kind::Unparsed);
    assert_eq!(decode(&report(WAIT), "another-image"), Kind::Unparsed);
    assert_eq!(
        decode(
            &report(WAIT).replace("0x1000 - 0x1fff", "0x0800 - 0x1fff"),
            UUID
        ),
        Kind::Unparsed
    );
    assert_eq!(
        decode(
            &report(WAIT).replace("0x1000 - 0x1fff", "0x1000 - 0x8fff"),
            UUID
        ),
        Kind::Unparsed
    );
    let duplicate = format!(
        "{}  0x1000 - 0x1fff libsystem_kernel.dylib (1) <{UUID}> {KERNEL_PATH}\n",
        report(WAIT)
    );
    assert_eq!(decode(&duplicate, UUID), Kind::Unparsed);
}

#[test]
fn all_threads_and_branches_must_agree_on_the_waiting_leaf() {
    let another = WAIT.replace("Thread_166387592", "Thread_166387593");
    assert_eq!(
        decode(&report(&format!("{WAIT}\n{another}")), UUID),
        Kind::Read
    );
    let other_kind = another.replace("10 read ", "10 flock ");
    assert_eq!(
        decode(&report(&format!("{WAIT}\n{other_kind}")), UUID),
        Kind::Unparsed
    );
    let branch = "\n        + 5 flock  (in libsystem_kernel.dylib) + 8  [0x1008]";
    assert_eq!(
        decode(&report(&format!("{WAIT}{branch}")), UUID),
        Kind::Unparsed
    );
    let branch = branch.replace("flock", "read");
    assert_eq!(
        decode(&report(&format!("{WAIT}{branch}")), UUID),
        Kind::Read
    );
    assert_eq!(decode(&report("    10 Thread_1"), UUID), Kind::Unparsed);
    assert_eq!(decode("", UUID), Kind::Unparsed);
    let mut oversized = report(WAIT);
    oversized.push_str(&" ".repeat(MAX_REPORT));
    assert_eq!(decode(&oversized, UUID), Kind::Unparsed);
}

#[test]
fn the_pid_birth_exec_and_same_uid_read_must_all_still_match() {
    let expected = Identity {
        pid: 123,
        birth: Birth(456),
        idversion: Some(7),
    };
    assert!(identity_matches(expected, Some(expected)));
    for current in [
        None,
        Some(Identity {
            pid: 124,
            ..expected
        }),
        Some(Identity {
            birth: Birth(457),
            ..expected
        }),
        Some(Identity {
            idversion: Some(8),
            ..expected
        }),
    ] {
        assert!(!identity_matches(expected, current));
    }
    let incomplete = Identity {
        idversion: None,
        ..expected
    };
    assert!(!identity_matches(incomplete, Some(incomplete)));
}
