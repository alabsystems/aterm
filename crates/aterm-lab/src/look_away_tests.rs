// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_verdict_wants_the_done_unseen_away_and_seen_back() {
    let v = |away, faked, back, present| {
        verdict(Reads {
            away,
            faked,
            back,
            present,
        })
        .0
    };
    assert!(v("1", "1", "0", "0"), "the rule as built");
    assert!(!v("0", "0", "0", "0"), "main's rule: seen while away");
    assert!(
        !v("1", "0", "0", "1"),
        "the look read the controller's focus flag"
    );
    assert!(!v("1", "1", "1", "0"), "their return saw nothing");
    assert!(!v("1", "1", "0", "1"), "a done while aterm is active stood");
    assert!(!v("?", "1", "0", "0"), "no reading");
}

#[test]
fn the_reads_parse_what_aterm_and_launch_services_print() {
    assert_eq!(
        pid_of_info(
            "[ NULL ]  [ NULL ]  \n    bundleID=[ NULL ] \n    pid = 44422 !cgsConnection !signalled type=[ NULL ]\n"
        ),
        Some(44422)
    );
    assert_eq!(pid_of_info("[ NULL ]\n"), None);
    assert_eq!(
        front_sid(
            "OK 2\n0 0 s-aaaa - alive aterm meta=0 window=0 active=0 wfocus=1\n\
             0 1 s-bbbb - alive aterm meta=0 window=0 active=1 wfocus=1\n"
        )
        .as_deref(),
        Some("s-bbbb")
    );
    assert_eq!(front_sid("OK 0\n"), None);
    assert_eq!(
        unseen_of("OK schema=1 sid=7 end=- end_seen=- end_cheered=- done_unseen=1 goal=-\n"),
        Some("1"),
        "a done nobody looked at"
    );
    assert_eq!(
        unseen_of("OK schema=1 sid=7 done_unseen=0 goal=-\n"),
        Some("0"),
        "seen"
    );
    assert_eq!(
        unseen_of("OK schema=1 sid=7 end=- goal=-\n"),
        None,
        "a build with no such field"
    );
    assert_eq!(unseen_of("ERR no session s-aaaa\n"), None, "no status");
}

#[test]
fn the_verb_reads_its_words() {
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert!(matches!(parse_args(&v(&[])), Ok(Some((None, false)))));
    assert!(matches!(
        parse_args(&v(&["--aterm", "/x/aterm", "--keep"])),
        Ok(Some((Some(ref a), true))) if a == "/x/aterm"
    ));
    assert!(parse_args(&v(&["--harness", "claude"])).is_err());
    assert!(matches!(parse_args(&v(&["--help"])), Ok(None)));
}
