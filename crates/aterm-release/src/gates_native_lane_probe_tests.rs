// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_verdict_reads_the_compilers_note() {
    assert!(
        verification_off_verdict(
            "note: compiled WITHOUT Trust verification — 0 obligations checked\n"
        )
        .is_ok(),
        "the compiler is the authority on its own flags"
    );
}

#[test]
fn a_switch_that_parsed_but_did_not_switch_is_refused() {
    let report = "=== Trust Verification Report (probe) ===\n\
                  note: Trust verification: 1 proved, 0 failed, 0 unknown, 0 timed out, \
                  0 runtime-checked out of 1 obligation(s)\n";
    verification_off_verdict(report).expect_err("a compiler that verified is not an off lane");
}
