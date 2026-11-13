// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Ran};

fn call(fake: &Fake, original: Option<&str>) -> (ExitCode, String) {
    let mut out = Vec::new();
    let code = ssh(
        fake,
        &Layout { root: "/A".into() },
        original,
        &mut std::io::empty(),
        &mut out,
    );
    (code, String::from_utf8_lossy(&out).into_owned())
}

#[test]
fn only_the_five_requests_pass_and_everything_else_is_refused_with_status_two() {
    let mut fake = Fake::root();
    fake.euid = 501;
    for bad in [
        None,
        Some(""),
        Some("bash"),
        Some("hello extra"),
        Some("hello  "),
        Some("apply; rm -rf /"),
        Some("put"),
        Some("put x y"),
        Some("attach"),
        Some("install --admin admin"),
        Some("apply\nhello"),
        Some("/bin/sh -c id"),
        Some("scp -t /tmp"),
        Some("internal-sftp"),
        Some("screen 5901"),
        Some("screen "),
        Some("screen\t"),
    ] {
        let (code, out) = call(&fake, bad);
        assert_eq!(code, ExitCode::from(super::super::REFUSED), "{bad:?}");
        assert!(out.starts_with("refused: "), "{bad:?}: {out}");
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

#[test]
fn apply_becomes_exactly_the_sudo_the_rule_names() {
    let mut fake = Fake::root();
    fake.euid = 501;
    fake.on(
        "tty /usr/bin/sudo -n /Library/Management/aterm/bin/aterm machines agent apply",
        Ran::exit(1, ""),
    );
    let (code, _) = call(&fake, Some("apply"));
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(
        fake.calls(),
        ["tty /usr/bin/sudo -n /Library/Management/aterm/bin/aterm machines agent apply"]
    );
}

#[test]
fn a_bad_digest_or_run_id_is_refused_before_anything_touches_a_path() {
    let mut fake = Fake::root();
    fake.euid = 501;
    let (code, out) = call(&fake, Some("put ../../.ssh/authorized_keys"));
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert!(out.contains("SHA-256"), "{out}");
    let (code, _) = call(&fake, Some("attach ../../../etc"));
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}
