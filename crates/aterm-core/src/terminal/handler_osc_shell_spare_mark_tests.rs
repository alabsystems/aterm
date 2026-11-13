// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::MAX_START_ROOTS;
use crate::terminal::{ShellIntegrationNonce, StartFlags, StartKind, StartMark, Terminal};

const KEY: [u8; 32] = [0x3Cu8; 32];

fn id() -> String {
    format!(";id={}", ShellIntegrationNonce(KEY).to_hex())
}

fn p(prop: &str, id: &str) -> Vec<u8> {
    format!("\x1b]633;P;{prop}{id}\x07").into_bytes()
}

fn gated() -> Terminal {
    let mut t = Terminal::new(4, 40);
    t.authorize_shell_integration(KEY);
    t.set_require_shell_integration_nonce(true);
    t
}

fn record(t: &mut Terminal, rc: &str, flags: &str, kind: &str) {
    let id = id();
    t.process(&p(&format!("AtermStartRc={rc}"), &id));
    t.process(&p(&format!("AtermStartFlags={flags}"), &id));
    t.process(&p(&format!("AtermStart={kind}"), &id));
}

#[test]
fn spare_marks_count_only_signed_and_are_taken_once() {
    let mut open = Terminal::new(4, 40);
    open.process(&p("AtermSpare=parked", ""));
    record(&mut open, "/h", "pre:0,s:0,j:0,r:0", "spare");
    assert!(!open.take_spare_parked());
    assert_eq!(open.take_start_mark(), None);

    let mut t = gated();
    t.process(&p("AtermSpare=parked", ""));
    t.process(&p("AtermSpare=parked", ";id=00"));
    t.process(&p("AtermStart=spare", ";id=00"));
    assert!(
        !t.take_spare_parked(),
        "an unsigned parked mark is no spare"
    );
    assert_eq!(t.take_start_mark(), None);

    let id = id();
    for bad in [
        "AtermSpare=warm",
        "AtermStart=hot",
        "AtermStartFlags=pre:0,s:0",
        "AtermStartFlags=pre:0,s:0,j:0,r:2",
        "AtermStartFlags=pre:x,s:0,j:0,r:0",
        "AtermStartFlags=pre:+0,s:0,j:0,r:0",
        "AtermStartFlags=pre:0,s:+0,j:0,r:0",
        "AtermStartFlags=pre:0,s:0,j:+0,r:0",
        "AtermStartFlags=pre:0,s:0,j:0,r:0,z:1",
        "AtermStartRc=",
    ] {
        t.process(&p(bad, &id));
    }
    assert_eq!(t.spare_marks_refused(), 10);
    assert!(!t.take_spare_parked());
    assert_eq!(t.take_start_mark(), None);

    t.process(&p("AtermSpare=parked", &id));
    assert!(t.take_spare_parked());
    assert!(!t.take_spare_parked(), "taken once");

    record(&mut t, "/Users//a\\x20b\\x3bc", "pre:2,s:1,j:-,r:1", "spare");
    let want = StartMark {
        kind: StartKind::Spare,
        rc_dir: Some("/Users//a b;c".into()),
        roots: None,
        flags: Some(StartFlags {
            pre: 2,
            sched: 1,
            jobs: None,
            periodic: true,
        }),
    };
    assert_eq!(t.take_start_mark(), Some(want));
    assert_eq!(t.take_start_mark(), None, "taken once");

    record(&mut t, "/other", "pre:0,s:0,j:0,r:0", "cold");
    assert_eq!(t.take_start_mark(), None);
    assert_eq!(t.spare_marks_refused(), 10, "a later record is not refused");
}

#[test]
fn a_start_rc_that_unescapes_to_a_nul_is_refused() {
    let id = id();
    let mut t = gated();
    t.process(&p("AtermStartRc=/tmp\\x00bad", &id));
    assert_eq!(t.spare_marks_refused(), 1);
    t.process(&p("AtermStart=cold", &id));
    assert_eq!(t.take_start_mark().map(|m| m.rc_dir), Some(None));

    let mut t = gated();
    record(&mut t, "/tmp\\x20ok", "pre:0,s:0,j:0,r:0", "cold");
    assert_eq!(t.spare_marks_refused(), 0);
    assert_eq!(
        t.take_start_mark().and_then(|m| m.rc_dir).as_deref(),
        Some("/tmp ok")
    );
}

#[test]
fn a_start_record_carries_the_startup_roots_it_names() {
    let id = id();
    let mut t = gated();
    let too_many = format!(
        "AtermStartRoots={}",
        ["/v"; MAX_START_ROOTS + 1].join("\\x00")
    );
    for bad in [
        "AtermStartRoots=",
        "AtermStartRoots=etc/fish",
        "AtermStartRoots=/etc/fish\\x00",
        "AtermStartRoots=/etc/fish\\x00\\x00/v",
        too_many.as_str(),
    ] {
        t.process(&p(bad, &id));
    }
    assert_eq!(t.spare_marks_refused(), 5);
    record(&mut t, "/c", "pre:0,s:0,j:0,r:0", "cold");
    assert_eq!(t.take_start_mark().map(|m| m.roots), Some(None));

    let mut t = gated();
    t.process(&p(
        "AtermStartRoots=/opt/homebrew/etc/fish\\x00/Users//a\\x20b/.local/share/fish/vendor_conf.d\\x00/x\\x3by\\\\z/fish/vendor_conf.d",
        &id,
    ));
    record(&mut t, "/c", "pre:0,s:0,j:0,r:0", "cold");
    assert_eq!(
        t.take_start_mark().and_then(|m| m.roots),
        Some(vec![
            "/opt/homebrew/etc/fish".to_owned(),
            "/Users//a b/.local/share/fish/vendor_conf.d".to_owned(),
            "/x;y\\z/fish/vendor_conf.d".to_owned(),
        ])
    );
    assert_eq!(t.spare_marks_refused(), 0);
}

#[test]
fn a_prompt_start_is_seen_only_once_signed() {
    let mut t = gated();
    assert!(!t.shell_prompt_started());
    t.process(b"\x1b]133;A\x07");
    assert!(!t.shell_prompt_started(), "an unsigned A is dropped");
    t.process(format!("\x1b]133;A{}\x07", id()).as_bytes());
    assert!(t.shell_prompt_started());
}
