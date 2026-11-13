// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::refuse_stray_args;

#[test]
fn a_zero_argument_verb_refuses_its_tail_and_passes_bare() {
    for verb in ["version", "sessions", "who", "whoami"] {
        assert_eq!(
            refuse_stray_args(verb, " --json").as_deref(),
            Some(format!("ERR usage: {verb}\n").as_str()),
            "`{verb} --json` must be a usage error, not a silent drop"
        );
        assert_eq!(refuse_stray_args(verb, ""), None, "bare `{verb}` proceeds");
        assert_eq!(
            refuse_stray_args(verb, "   "),
            None,
            "whitespace tail is bare"
        );
    }
}
