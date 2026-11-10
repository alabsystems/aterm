// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The stream flags and callback context agree with the SDK; a mutant must be refused.

#![cfg(target_os = "macos")]

#[path = "../../aterm-objc/tests/support/sdk_consts.rs"]
#[expect(dead_code, reason = "the shared SDK compiler serves this layout probe")]
mod sdk_consts;

fn probe(mutated: bool) -> String {
    let mut body = String::from("#include <stddef.h>\n#include <CoreServices/CoreServices.h>\n");
    for (i, (expression, value)) in aterm_sysprobe::file_events::SDK_ROWS.iter().enumerate() {
        let value = value + usize::from(mutated && i == 0);
        body.push_str(&format!(
            "_Static_assert(({expression}) == {value}, \"{expression}\");\n"
        ));
    }
    body
}

#[test]
fn file_event_flags_and_context_match_the_sdk_and_a_mutant_is_rejected() {
    for arch in ["arm64", "x86_64"] {
        sdk_consts::green_verdict(
            arch,
            "FSEvents SDK mismatch",
            sdk_consts::compile("file-events", arch, &probe(false), "green"),
        );
        sdk_consts::inverted_verdict(
            arch,
            "kFSEventStreamCreateFlagFileEvents",
            sdk_consts::compile("file-events", arch, &probe(true), "mutant"),
            "a wrong FSEvents flag must fail",
        );
    }
}
