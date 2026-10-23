// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The job probe's Darwin layouts (`src/jobs/darwin.rs`), the process
//! records' (`src/kproc.rs`), the code-signing flags and categories
//! (`src/identity.rs`) and the holder scan's (`aterm_uds::holders`) held to
//! the SDK headers on both arches: every row of `jobs::darwin::SDK_ROWS`,
//! `kproc::SDK_ROWS`, `identity::SDK_ROWS` and `aterm_uds::holders::SDK_ROWS`
//! (a flavor, a flag, a struct's size, a field's offset) becomes one
//! `_Static_assert` that clang compiles against `sys/proc_info.h`,
//! `sys/sysctl.h`, `sys/event.h`, `mach/machine.h`, `vis.h`, `xlocale.h` and
//! `Kernel/kern/cs_blobs.h` for `arm64` and `x86_64`, through aterm-objc's
//! shared probe
//! (`crates/aterm-objc/tests/support/sdk_consts.rs`, mounted here). The
//! decoders read the answers at exactly these offsets, so a header that
//! moved a field fails here, not in a wrong verdict.
//!
//! `a_wrong_offset_fails_the_probe_on_both_arches` is the negative control:
//! the same probe with one row off by one must fail, or a green run proves
//! nothing.

#![cfg(target_os = "macos")]

#[path = "../../aterm-objc/tests/support/sdk_consts.rs"]
#[expect(
    dead_code,
    reason = "mounted for its compiler call and verdicts; the constant-line parser serves the constant censuses"
)]
mod sdk_consts;

use aterm_sysprobe::{identity, jobs, kproc};

/// Every table's rows.
fn rows() -> impl Iterator<Item = &'static (&'static str, i64)> {
    jobs::darwin::SDK_ROWS
        .iter()
        .chain(kproc::SDK_ROWS)
        .chain(identity::SDK_ROWS)
        .chain(aterm_uds::holders::SDK_ROWS)
}

const HEADERS: &str = "#include <stddef.h>\n\
                       #include <stdint.h>\n\
                       #include <sys/param.h>\n\
                       #include <sys/types.h>\n\
                       #include <sys/fcntl.h>\n\
                       #include <sys/stat.h>\n\
                       #include <sys/event.h>\n\
                       #include <sys/sysctl.h>\n\
                       #include <sys/proc.h>\n\
                       #include <sys/proc_info.h>\n\
                       #include <mach/machine.h>\n\
                       #include <libproc.h>\n\
                       #include <vis.h>\n\
                       #include <xlocale.h>\n\
                       #include <Kernel/kern/cs_blobs.h>\n";

/// The probe, with the row named `off` (if any) asserted one higher.
fn probe(off: Option<&str>) -> String {
    let mut s = String::from(HEADERS);
    for (expr, value) in rows() {
        let value = if off == Some(*expr) {
            value + 1
        } else {
            *value
        };
        s.push_str(&format!(
            "_Static_assert((long long)({expr}) == {value}LL, \"{expr}\");\n"
        ));
    }
    s.push_str("int main(void) { return 0; }\n");
    s
}

fn compile(arch: &str, body: &str, tag: &str) -> Result<(), String> {
    sdk_consts::compile(
        concat!(env!("CARGO_PKG_NAME"), "-proc-info"),
        arch,
        body,
        tag,
    )
}

#[test]
fn every_layout_row_matches_the_sdk_on_both_arches() {
    assert!(
        jobs::darwin::SDK_ROWS.len() >= 60
            && kproc::SDK_ROWS.len() >= 15
            && identity::SDK_ROWS.len() >= 14
            && aterm_uds::holders::SDK_ROWS.len() >= 23,
        "a table lost rows: {}, {}, {} and {}",
        jobs::darwin::SDK_ROWS.len(),
        kproc::SDK_ROWS.len(),
        identity::SDK_ROWS.len(),
        aterm_uds::holders::SDK_ROWS.len()
    );
    let body = probe(None);
    for arch in ["arm64", "x86_64"] {
        sdk_consts::green_verdict(
            arch,
            "a libproc layout row disagrees with the SDK",
            compile(arch, &body, "green"),
        );
    }
}

#[test]
fn a_wrong_offset_fails_the_probe_on_both_arches() {
    let row = "offsetof(struct pipe_fdinfo, pipeinfo.pipe_status)";
    assert!(rows().any(|(e, _)| *e == row), "{row} left the table");
    let body = probe(Some(row));
    for arch in ["arm64", "x86_64"] {
        sdk_consts::inverted_verdict(
            arch,
            row,
            compile(arch, &body, "inverted"),
            &format!(
                "{arch}: the probe accepted `{row}` one byte off, so the green run checks nothing"
            ),
        );
    }
}
