// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live headless tests for `status typed_by=` attribution across caller contexts.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::{headless_boot, launch_isolation};

use headless_boot::{Instance, boot_session, ctl, ctl_ok, field, said, status, type_line};

/// A raw connection's reply not read by now is a hung instance.
const READ_HANG: Duration = Duration::from_secs(120);

/// A refused boot ends the test COULD NOT RUN.
fn boot() -> Instance {
    headless_boot::boot_with(
        "atyp",
        |_, cmd| {
            cmd.args(["--lines", "24", "--columns", "100"]);
        },
        |_| true,
    )
}

/// The sid a `spawn` minted.
fn spawn(inst: &Instance) -> String {
    let body = ctl_ok(inst, &["spawn"]);
    body.split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("a sid: {body}"))
        .to_string()
}

/// `sid`'s `typed_by=`, decoded.
fn typed_by(inst: &Instance, sid: &str) -> String {
    let record = status(inst, sid);
    let by = field(&record, "typed_by").unwrap_or_else(|| panic!("typed_by=: {record}"));
    aterm_control::wire::pct_decode(by)
}

/// `line` on a fresh connection that presents `token`, and its first reply line.
fn request(inst: &Instance, token: &str, line: &str) -> String {
    let stream = UnixStream::connect(&inst.sock).expect("connect to the instance");
    stream
        .set_read_timeout(Some(READ_HANG))
        .expect("bound the reply's read");
    let mut conn = BufReader::new(stream);
    conn.get_mut()
        .write_all(format!("TOKEN {token} {line}\n").as_bytes())
        .expect("send the request");
    let mut reply = String::new();
    conn.read_line(&mut reply).expect("read the reply");
    reply
}

#[test]
fn a_write_names_who_typed_it_and_a_refused_one_names_nobody() {
    let inst = boot();
    let (_, manager) = boot_session(&inst);
    let held = spawn(&inst);
    let worker = spawn(&inst);
    ctl_ok(
        &inst,
        &[&format!("@{manager}"), "meta", "set", "role", "lab-sweep"],
    );
    assert_eq!(typed_by(&inst, &worker), "-", "CONTROL: nobody typed yet");

    ctl_ok(&inst, &[&format!("@{worker}"), "send", "echo one"]);
    let outside = typed_by(&inst, &worker);
    assert!(outside.contains(" (pid "), "{outside}");

    let aterm = launch_isolation::aterm_executable();
    type_line(
        &inst,
        &manager,
        &format!(
            "'{}' ctl --sock '{}' @{worker} send 'echo two'",
            aterm.display(),
            inst.sock
        ),
    );
    until_within(MUST_HAPPEN, "the manager's shell credited", || {
        (typed_by(&inst, &worker) == "lab-sweep").then_some(())
    });

    let minted = ctl_ok(
        &inst,
        &[
            "connect",
            &format!("dst={worker}"),
            &format!("src={manager}"),
            "kind=push",
        ],
    );
    let token = field(minted.trim(), "write-input")
        .unwrap_or_else(|| panic!("a write-input token: {minted}"));
    let reply = request(&inst, token, &format!("@{worker} send echo three"));
    assert!(reply.starts_with("OK"), "{reply}");
    let edge = typed_by(&inst, &worker);
    assert!(edge.ends_with(" on lab-sweep's edge"), "{edge}");
    assert!(
        edge.contains(" (pid "),
        "the presenter by its process: {edge}"
    );

    ctl_ok(&inst, &["hold", &held, "on", "reason=demo"]);
    let refused = ctl(&inst, &[&format!("@{held}"), "send", "echo four"]);
    assert!(said(&refused).contains("ERR halted"), "{}", said(&refused));
    assert_eq!(typed_by(&inst, &held), "-", "a refusal credits nobody");
    ctl_ok(&inst, &["hold", &held, "off"]);
    ctl_ok(&inst, &[&format!("@{held}"), "send", "echo four"]);
    assert!(typed_by(&inst, &held).contains(" (pid "));
}
