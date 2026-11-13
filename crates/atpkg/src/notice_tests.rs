// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::Mutex;

static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn heard(msg: &str) {
    HEARD.lock().unwrap().push(msg.to_owned());
}

#[test]
fn a_notice_is_new_once_per_text() {
    let said = Mutex::new(BTreeSet::new());
    assert!(first_time(&said, "[packages] channel is retired"));
    assert!(!first_time(&said, "[packages] channel is retired"));
    assert!(!first_time(&said, "[packages] channel is retired"));
    assert!(first_time(&said, "[packages] include is retired"));
}

#[test]
fn a_hosted_notice_is_a_log_record_and_a_verbs_is_a_stderr_line() {
    let mut stderr = Vec::new();
    route(
        "the table could not be read (at its value)",
        "the table could not be read (6B)",
        Sink::HostLog,
        &mut stderr,
        heard,
    );
    assert!(stderr.is_empty(), "a host's notice reached stderr");
    let host = HEARD.lock().unwrap().clone();
    assert!(
        host.iter().any(|m| m == "the table could not be read (6B)"),
        "the host's log heard its log form: {host:?}"
    );
    assert!(
        !host.iter().any(|m| m.contains("at its value")),
        "the host's log heard the said form: {host:?}"
    );

    let mut stderr = Vec::new();
    route(
        "typed (at its value)",
        "typed (6B)",
        Sink::Stderr,
        &mut stderr,
        heard,
    );
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        "atpkg: typed (at its value)\n",
        "a verb's notice prints what a person reads"
    );
    assert!(
        !HEARD.lock().unwrap().iter().any(|m| m.starts_with("typed")),
        "a verb's notice is not logged twice"
    );

    // A pass whose stderr the host copies into its aterm.log prints the log form there.
    let mut stderr = Vec::new();
    route(
        "copied (at its value)",
        "copied (6B)",
        Sink::HostReadStderr,
        &mut stderr,
        heard,
    );
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        "atpkg: copied (6B)\n",
        "a stderr the host's log reads hears the log form"
    );
    assert!(
        !HEARD
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.starts_with("copied")),
        "a copied stderr's notice is not logged in process as well"
    );
}
