// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_verb_with_no_help_of_its_own_answers_help_with_its_usage() {
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let refused = Fail::usage("instance: unexpected '--help'");
    let usage = help_instead("instance", &words(&["--help"]), &refused).expect("its usage");
    assert!(usage.starts_with("  instance "), "{usage}");
    assert!(!usage.contains("abrew-admission"), "{usage}");
    assert!(help_instead("instance", &words(&["-h", "--keep"]), &refused).is_some());
    for (rest, fail) in [
        (words(&["--", "/bin/sh", "--help"]), Fail::usage("x")),
        (words(&["--keep"]), Fail::usage("x")),
        (words(&["--help"]), Fail::die("x")),
    ] {
        assert_eq!(help_instead("instance", &rest, &fail), None, "{rest:?}");
    }
    let send = verb_usage("send").expect("turn|send|enter|key");
    assert!(send.starts_with("  turn|send|enter|key"), "{send}");
    assert_eq!(verb_usage("no-such-verb"), None);
}
