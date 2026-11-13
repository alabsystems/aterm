// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn transport_requires_canonical_parent_and_callbacks_cannot_select_other_verbs() {
    let args = words(&["42", "repo", "refs", "a/b"]);
    assert!(
        matches!(invocation(&args), Some(Invocation::Transport(42, tail)) if tail == &args[1..])
    );
    for op in ["get", "store", "erase", "capability"] {
        let args = words(&["credential", "auth", "git-credential", op]);
        assert!(
            matches!(invocation(&args), Some(Invocation::Credential(tail)) if tail == &args[1..])
        );
    }
    for args in [
        words(&[]),
        words(&["42"]),
        words(&["0", "help"]),
        words(&["1", "help"]),
        words(&["-2", "help"]),
        words(&["+2", "help"]),
        words(&["02", "help"]),
        words(&["4294967296", "help"]),
        words(&["credential", "help"]),
        words(&["credential", "auth", "git-credential", "get", "extra"]),
        words(&["credential", "api", "repos/a/b", "GET"]),
    ] {
        assert!(invocation(&args).is_none(), "{args:?}");
    }
}
