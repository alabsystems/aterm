// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn every_error_shape_yields_its_name() {
    let cases: [(&str, &[&str]); 9] = [
        (
            "error[E0425]: cannot find value `AF_INET` in crate `libc`",
            &["AF_INET"],
        ),
        (
            "error[E0412]: cannot find type `stat` in this scope",
            &["stat"],
        ),
        (
            "error[E0425]: cannot find function, tuple struct or tuple variant `f` in crate `libc`",
            &["f"],
        ),
        (
            "error[E0433]: cannot find struct, variant or union type `kevent` in crate `libc`",
            &["kevent"],
        ),
        (
            "error[E0432]: unresolved import `libc::getpid`",
            &["getpid"],
        ),
        ("error[E0432]: unresolved import `libc::Foo::bar`", &[]),
        ("error[E0425]: no `SIGWINCH` in the root", &["SIGWINCH"]),
        ("error: no `X` here", &[]),
        (
            "   = note: `libc::sysctl` and `c::sysctlbyname` are not found",
            &["sysctl", "sysctlbyname"],
        ),
    ];
    for (line, want) in cases {
        let want: BTreeSet<&str> = want.iter().copied().collect();
        assert_eq!(missing_names(line), want, "{line}");
    }
}

#[test]
fn a_line_is_attributed_to_its_crate() {
    assert_eq!(
        crate_of(
            "/h/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rustix-1.1.4/src/fs.rs:3:5: error"
        ),
        "rustix-1.1.4"
    );
    assert_eq!(
        crate_of("crates/aterm-gui/src/app.rs:1:1: error[E0425]"),
        "FIRST-PARTY aterm-gui"
    );
    assert_eq!(
        crate_of("/w/vendor/astream/src/x.rs:1:1: error"),
        "FIRST-PARTY astream"
    );
    assert_eq!(crate_of("error[E0425]: cannot find value"), "error[E0425]");
}

/// Only names the reference exports (or a primitive) count; the rest are
/// unattributed, so a loose match never becomes an obligation.
#[test]
fn the_reference_filters_what_a_log_names() {
    let log = "\
crates/a/src/x.rs:1:1: error[E0425]: cannot find value `AF_INET` in crate `libc`
crates/a/src/x.rs:2:1: error[E0412]: cannot find type `c_int` in crate `libc`
crates/b/src/y.rs:1:1: error[E0425]: cannot find value `NOT_IN_LIBC` in crate `libc`
crates/b/src/y.rs:1:1: warning: unused import `libc::AF_UNIX`
";
    let harvest = Harvest::scan(log, &|name| name == "AF_INET");
    let names: Vec<String> = harvest.names().into_iter().collect();
    assert_eq!(names, ["AF_INET", "c_int"]);
    assert_eq!(
        harvest.by_crate.keys().collect::<Vec<_>>(),
        ["FIRST-PARTY a"]
    );
    assert_eq!(
        harvest.unattributed.len(),
        1,
        "NOT_IN_LIBC; a warning is no error line"
    );
    assert!(
        harvest
            .report("mac")
            .contains("DISTINCT MISSING (mac): 2: AF_INET c_int")
    );
}
