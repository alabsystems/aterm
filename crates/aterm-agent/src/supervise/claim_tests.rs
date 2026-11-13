// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn reply(code: i32, stderr: &str) -> CtlReply {
    CtlReply {
        code,
        stdout: if code == 0 {
            "OK\n".to_string()
        } else {
            String::new()
        },
        stderr: stderr.to_string(),
    }
}

#[test]
fn a_busy_reply_names_the_other_holder() {
    let r = reply(1, "aterm-ctl: ERR busy supervisor=gui%3A42\n");
    assert_eq!(busy_holder(&r).as_deref(), Some("gui:42"));
    assert_eq!(busy_holder(&reply(1, "aterm-ctl: ERR busy sink\n")), None);
    assert_eq!(busy_holder(&reply(1, "aterm-ctl: ERR denied\n")), None);
}

#[test]
fn only_writes_are_refused_to_a_loop_that_watches() {
    for w in [
        &["@s-1", "key", "if=x", "1"][..],
        &["turn", "idle=600", "hi"],
        &["meta", "set", "attention", "owner=supervisor", "x"],
        &["meta", "unset", "attention", "owner=supervisor"],
        &["post", "to=@s-2", "kind=ask", "x"],
        &["task", "mark", "done", "why=marker"],
    ] {
        assert!(is_write(w), "{w:?}");
    }
    for r in [
        &["@s-1", "text", "--json"][..],
        &["status"],
        &["meta"],
        &["task"],
        &["await", "gone", "x", "timeout", "20000"],
        &["meta", "set", "supervisor", "x", "ttl=90000"],
    ] {
        assert!(!is_write(r), "{r:?}");
    }
    assert!(is_input(&["@s-1", "key", "if=x", "1"]));
    assert!(is_input(&["send", "if-gen=1.2", "--", "keep going"]));
    assert!(!is_input(&[
        "meta",
        "set",
        "attention",
        "owner=supervisor",
        "x"
    ]));
    assert!(!is_input(&["post", "to=@s-2", "kind=ask", "x"]));
    assert!(!is_input(&["task", "mark", "done", "why=marker"]));
}
