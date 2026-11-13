// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::num::NonZeroUsize;
use std::time::Duration;

/// A wait for something that must happen: a hang detector, never a latency budget.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// What a child asked to report prints its answer after.
const ASK: &str = "ATERM_CORES_TEST_REPORT";

fn n(v: usize) -> NonZeroUsize {
    NonZeroUsize::new(v).expect("a positive count")
}

fn machine(v: usize) -> std::io::Result<NonZeroUsize> {
    Ok(n(v))
}

fn unknown() -> std::io::Result<NonZeroUsize> {
    Err(std::io::Error::other("no parallelism here"))
}

/// A grant bounds the machine's parallelism and never raises it. NEGATIVE CONTROL: unset, the
/// machine's own parallelism comes back unchanged, so the bound is the grant's alone.
#[test]
fn a_grant_bounds_the_machine_and_unset_leaves_it_alone() {
    let set = |v: &str| bounded(machine(18), Some(OsStr::new(v))).expect("a known machine");
    assert_eq!(set("4"), n(4), "a grant below the machine bounds it");
    assert_eq!(set("18"), n(18));
    assert_eq!(set("64"), n(18), "a grant above the machine is the machine");
    assert_eq!(
        bounded(machine(18), None).expect("a known machine"),
        n(18),
        "CONTROL: with no grant, the machine's parallelism, unchanged"
    );
    assert_eq!(
        bounded(unknown(), Some(OsStr::new("3"))).expect("the grant answers"),
        n(3),
        "an unknown machine with a grant is the grant"
    );
    assert!(
        bounded(unknown(), None).is_err(),
        "CONTROL: an unknown machine with no grant keeps std's error"
    );
}

/// A set value the grant cannot read grants one core, never the machine. NEGATIVE CONTROL: the
/// well-formed spelling of each beside it is read as its number.
#[test]
fn a_malformed_grant_grants_one_core() {
    for bad in [
        "",
        "0",
        "-2",
        "+4",
        " 4",
        "4 ",
        "four",
        "4.0",
        "99999999999999999999999999",
    ] {
        assert_eq!(
            grant(Some(OsStr::new(bad))),
            Some(NonZeroUsize::MIN),
            "{bad:?} is malformed"
        );
        assert_eq!(
            bounded(machine(18), Some(OsStr::new(bad))).expect("a known machine"),
            NonZeroUsize::MIN
        );
    }
    for (good, cores) in [("4", 4), ("04", 4), ("1", 1), ("12", 12)] {
        assert_eq!(
            grant(Some(OsStr::new(good))),
            Some(n(cores)),
            "CONTROL: {good:?}"
        );
    }
    assert_eq!(grant(None), None, "CONTROL: unset is no grant");
}

/// A pool every test thread of a binary shares holds the grant times the binary's width, at
/// most the machine; a width it cannot read counts as one thread, never more. NEGATIVE CONTROL:
/// with no grant the width changes nothing (the machine), and one thread's pool
/// ([`bounded`]) stays at the per-thread grant.
#[test]
fn a_shared_pool_holds_the_grant_of_every_test_thread() {
    fn os(v: &str) -> Option<&OsStr> {
        Some(OsStr::new(v))
    }
    let shared = |g: Option<&OsStr>, w: Option<&OsStr>| {
        bounded_for_binary(machine(18), g, w).expect("a known machine")
    };
    assert_eq!(
        shared(os("2"), os("8")),
        n(16),
        "two cores for each of eight threads"
    );
    assert_eq!(shared(os("4"), os("8")), n(18), "never past the machine");
    assert_eq!(shared(os("3"), None), n(3), "no width is one thread");
    for bad in ["", "0", "x", "-1", " 8"] {
        assert_eq!(shared(os("3"), os(bad)), n(3), "{bad:?} is no width");
    }
    assert_eq!(
        bounded_for_binary(unknown(), os("2"), os("3")).expect("the grant answers"),
        n(6)
    );
    assert_eq!(
        shared(None, os("8")),
        n(18),
        "CONTROL: no grant is the machine"
    );
    assert_eq!(
        bounded(machine(18), os("2")).expect("a known machine"),
        n(2),
        "CONTROL: one thread's pool keeps its own share"
    );
}

/// The child half of [`the_process_reads_its_grant_from_its_environment`]: prints the cores
/// this process's pools may use.
#[test]
fn report_child() {
    if std::env::var_os(ASK).is_some() {
        let cores = available_parallelism().map_or(0, NonZeroUsize::get);
        println!("CORES={cores}");
    }
}

/// This test binary run as a child with `grant` as [`GRANT_ENV`] (`None`: removed): the cores
/// it reports.
fn child_reports(grant: Option<&str>) -> usize {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    cmd.args([
        "--exact",
        "tests::report_child",
        "--test-threads=1",
        "--nocapture",
        "--quiet",
    ])
    .env(ASK, "1");
    match grant {
        Some(grant) => cmd.env(GRANT_ENV, grant),
        None => cmd.env_remove(GRANT_ENV),
    };
    let out = aterm_uds::deadline::output_within(&mut cmd, MUST_HAPPEN).expect("the child ran");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the child passed: {text}");
    let at = text.find("CORES=").expect("the child's answer") + "CORES=".len();
    text[at..]
        .lines()
        .next()
        .and_then(|line| line.trim().parse().ok())
        .expect("a count")
}

/// A process reads its grant from its own environment: a child granted one core sizes its pools
/// to one, and one granted a malformed value to one. NEGATIVE CONTROL: a child with the variable
/// removed reports the machine's parallelism, which on a machine of more than one core differs.
#[test]
fn the_process_reads_its_grant_from_its_environment() {
    #[expect(
        clippy::disallowed_methods,
        reason = "the control compares the helper with std's own reading"
    )]
    let host = std::thread::available_parallelism().map_or(0, NonZeroUsize::get);
    assert_eq!(
        child_reports(None),
        host,
        "CONTROL: no grant is std's answer"
    );
    assert_eq!(child_reports(Some("1")), 1);
    assert_eq!(child_reports(Some("garbage")), 1);
    assert_eq!(
        child_reports(Some(&(host + 7).to_string())),
        host,
        "a grant above the machine is the machine"
    );
}
