// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dispatch classification through the publisher's default and host tool adapters.

#[cfg(unix)]
use atpkg_publish::artifact::tools::HostTools;
use atpkg_publish::artifact::tools::{Run, RunError, Tools};

use super::fixture::{FakeTools, ran};

#[test]
fn default_complete_stdout_distinguishes_pre_dispatch_capacity_from_returned_output() {
    let mut tools = FakeTools::new("aarch64-apple-darwin");
    tools.answer = Box::new(|_| Some(ran(0, "abc", "")));
    let query = Run::new("/fixture/publisher-query", ["query"]);
    let error = tools
        .run_complete(&query, aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES + 1)
        .unwrap_err();
    assert!(matches!(error, RunError::Spawn(_)), "{error:?}");
    assert!(tools.runs().is_empty(), "capacity refuses before dispatch");

    assert_eq!(tools.run_complete(&query, 3).unwrap().stdout, b"abc");
    assert_eq!(tools.runs().len(), 1);
    assert_eq!(
        tools.run_complete(&query, 2).unwrap_err(),
        RunError::AfterStart("complete stdout exceeds its bound".into())
    );
    assert_eq!(tools.runs().len(), 2, "overflow follows an actual run");
}

#[cfg(unix)]
#[test]
fn host_spawn_refusals_stay_pre_dispatch_for_all_capture_modes() {
    let world = aterm_tempfile::tempdir().unwrap();
    let query = Run::new(world.path().join("no-executable"), ["query"]);
    for outcome in [
        HostTools.run(&query),
        HostTools.run(&query.clone().bounded()),
        HostTools.run_complete(&query, 64),
    ] {
        let error = outcome.unwrap_err();
        assert!(matches!(error, RunError::Spawn(_)), "{error:?}");
    }
}

#[cfg(unix)]
#[test]
fn host_bounded_and_unbounded_capture_keep_stdout_and_stderr() {
    let echo = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/echo"));
    let ls = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/ls"));
    let world = aterm_tempfile::tempdir().unwrap();
    for bounded in [false, true] {
        let mut stdout = Run::new(&echo, ["publisher-capture-marker"]);
        let mut stderr = Run::new(&ls, [world.path().join("absent-input")]);
        if bounded {
            stdout = stdout.bounded();
            stderr = stderr.bounded();
        }
        let out = HostTools.run(&stdout).unwrap();
        assert!(out.success());
        assert_eq!(out.stdout, b"publisher-capture-marker\n");
        assert!(out.stderr.is_empty());
        let out = HostTools.run(&stderr).unwrap();
        assert!(!out.success());
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("absent-input"));
    }
}

#[cfg(unix)]
#[test]
fn host_complete_stdout_overflow_follows_a_real_child_answer() {
    let echo = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/echo"));
    let query = Run::new(&echo, ["publisher-size-marker"]).bounded();
    let expected = b"publisher-size-marker\n";
    assert_eq!(
        HostTools
            .run_complete(&query, expected.len())
            .unwrap()
            .stdout,
        expected
    );
    let error = HostTools
        .run_complete(&query, expected.len() - 1)
        .unwrap_err();
    assert!(matches!(error, RunError::AfterStart(_)), "{error:?}");
    assert!(error.to_string().contains("complete-capture limit"));
}
