// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SDK header layout checks for `iflist::SDK_ROWS`.

#![cfg(target_os = "macos")]

#[path = "../../aterm-objc/tests/support/sdk_consts.rs"]
#[expect(
    dead_code,
    reason = "mounted for its compiler call and verdicts; the constant-line parser serves the constant censuses"
)]
mod sdk_consts;

use aterm_sysprobe::iflist;

const HEADERS: &str = "#include <stddef.h>\n\
                       #include <stdint.h>\n\
                       #include <sys/types.h>\n\
                       #include <sys/socket.h>\n\
                       #include <sys/socketvar.h>\n\
                       #include <sys/sockio.h>\n\
                       #include <sys/sysctl.h>\n\
                       #include <net/if.h>\n\
                       #include <net/if_var.h>\n\
                       #include <net/if_dl.h>\n\
                       #include <net/route.h>\n\
                       #include <netinet/in_pcb.h>\n\
                       #include <netinet/tcp_fsm.h>\n\
                       #include <netinet/tcp_var.h>\n";

/// The probe, with the row named `off` (if any) asserted one higher.
fn probe(off: Option<&str>) -> String {
    let mut s = String::from(HEADERS);
    for (expr, value) in iflist::SDK_ROWS {
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
    sdk_consts::compile(concat!(env!("CARGO_PKG_NAME"), "-iflist"), arch, body, tag)
}

#[test]
fn every_layout_row_matches_the_sdk_on_both_arches() {
    assert!(
        iflist::SDK_ROWS.len() >= 40,
        "the table lost rows: {}",
        iflist::SDK_ROWS.len()
    );
    let body = probe(None);
    for arch in ["arm64", "x86_64"] {
        sdk_consts::green_verdict(
            arch,
            "an interface-list layout row disagrees with the SDK",
            compile(arch, &body, "green"),
        );
    }
}

#[test]
fn a_wrong_offset_fails_the_probe_on_both_arches() {
    let row = "offsetof(struct if_msghdr2, ifm_data.ifi_ibytes)";
    assert!(
        iflist::SDK_ROWS.iter().any(|(e, _)| *e == row),
        "{row} left the table"
    );
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
