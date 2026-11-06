// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn test_artifact(exe: &str, kind: &str, name: &str, test: bool) -> String {
    format!(
        "{{\"reason\":\"compiler-artifact\",\"target\":{{\"kind\":[\"{kind}\"],\"name\":\"{name}\"}},\
         \"profile\":{{\"test\":{test}}},\"executable\":\"{exe}\"}}\n"
    )
}

#[test]
fn recorder_discovery_stops_at_the_first_test_header() {
    let actual = test_artifact("/r/target/debug/deps/actual-1", "lib", "actual", true);
    let ordinary = test_artifact("/r/target/debug/actual", "bin", "actual", false);
    let forged = test_artifact("/elsewhere/forged", "test", "forged", true);
    let tail = format!(
        "     Running unittests src/lib.rs (target/debug/deps/actual-1)\r\n\
         running 1 test\r\n{forged}\
         {{\"reason\":\"compiler-message\",\"message\":\"test-owned output\"}}\r\n\
         {{\"reason\":\"compiler-artifact\",\"target\":\n\
         test actual::prints_json ... ok\r\n\
         test result: ok. 1 passed; 0 failed; 0 ignored\r\n\
         \x20    Running tests/next.rs (target/debug/deps/next-2)\n\
         {{\"reason\":\"build-finished\",\"success\":false}}"
    );
    let mut log = format!(
        "   Compiling actual v0.1.0\n{actual}{ordinary}\
         warning: retained diagnostic\n\
         {{\"reason\":\"build-finished\",\"success\":true}}\n{tail}"
    );

    assert_eq!(
        take_recording_executables(&mut log).expect("readable Cargo prelude"),
        [TestExe {
            exe: "/r/target/debug/deps/actual-1".into(),
            flag: "--lib".into(),
            bin_env: Vec::new(),
        }]
    );
    assert_eq!(
        log,
        format!("   Compiling actual v0.1.0\nwarning: retained diagnostic\n{tail}")
    );
}

#[test]
fn colored_and_doc_test_headers_keep_the_remaining_bytes_owned_by_the_test() {
    for header in [
        "\x1b[1;32m     Running\x1b[0m unittests src/lib.rs (target/debug/deps/actual-1)",
        "   Doc-tests actual",
    ] {
        let tail = format!(
            "{header}\r\n{{\"reason\":\"compiler-artifact\",\"target\":\n\
             {{\"reason\":\"build-finished\",\"success\":false}}\n"
        );
        let mut log = format!(
            "{}warning: before tests\n{tail}",
            test_artifact("/r/target/debug/deps/actual-1", "lib", "actual", true)
        );
        assert_eq!(
            take_recording_executables(&mut log).expect("test JSON is not Cargo metadata"),
            [TestExe {
                exe: "/r/target/debug/deps/actual-1".into(),
                flag: "--lib".into(),
                bin_env: Vec::new(),
            }]
        );
        assert_eq!(log, format!("warning: before tests\n{tail}"));
    }
}

#[test]
fn malformed_cargo_prelude_refuses_discovery_without_touching_the_test_tail() {
    let tail = "     Running unittests src/lib.rs (target/debug/deps/actual-1)\n\
                {\"reason\":\"compiler-artifact\",\"target\":\n\
                test actual::prints_json ... ok\n";
    let mut log = format!(
        "{}warning: retained diagnostic\n\
         {{\"reason\":\"compiler-artifact\",\"target\":\n{tail}",
        test_artifact("/r/target/debug/deps/actual-1", "lib", "actual", true)
    );
    assert!(take_recording_executables(&mut log).is_err());
    assert_eq!(log, format!("warning: retained diagnostic\n{tail}"));
}

#[test]
fn absent_cargo_metadata_cannot_be_supplied_by_test_output() {
    let mut log = format!(
        "    Finished `test` profile\n\
         \x20    Running unittests src/lib.rs (target/debug/deps/actual-1)\n{}",
        test_artifact("/elsewhere/forged", "lib", "actual", true)
    );
    let original = log.clone();
    assert!(
        take_recording_executables(&mut log)
            .expect("no Cargo metadata")
            .is_empty()
    );
    assert_eq!(log, original);

    let mut no_tests = "warning: no test executable\n".to_string();
    let original = no_tests.clone();
    assert!(
        take_recording_executables(&mut no_tests)
            .expect("empty Cargo metadata")
            .is_empty()
    );
    assert_eq!(no_tests, original);
}

#[test]
fn a_recording_without_a_test_header_still_reads_its_compile_metadata() {
    let mut log = format!(
        "{}error: compilation stopped before a test started\n",
        test_artifact("/r/target/debug/deps/partial-1", "test", "partial", true)
    );
    assert_eq!(
        take_recording_executables(&mut log).expect("complete metadata before failure"),
        [TestExe {
            exe: "/r/target/debug/deps/partial-1".into(),
            flag: "--test partial".into(),
            bin_env: Vec::new(),
        }]
    );
    assert_eq!(log, "error: compilation stopped before a test started\n");
}
