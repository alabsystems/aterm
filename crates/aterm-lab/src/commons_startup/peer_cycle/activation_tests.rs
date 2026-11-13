// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn activation_keeps_the_owned_kernel_birth_parent_and_image() {
    let executable = Path::new("/owned/aterm.app/Contents/MacOS/aterm");
    let record = aterm_sysprobe::kproc::Record {
        pid: 42,
        ppid: 41,
        pgid: 0,
        tpgid: 0,
        tty: None,
        uid: crate::sys::getuid(),
        comm: "aterm".into(),
        start_s: 100,
        start_us: 3,
        zombie: false,
    };
    let admitted = process_sample(&record, Some(executable.into()), 42, 41, executable).unwrap();
    let mut replaced = record.clone();
    replaced.start_us += 1;
    assert_ne!(
        process_sample(&replaced, Some(executable.into()), 42, 41, executable).unwrap(),
        admitted
    );
    for index in 0..5 {
        let mut wrong = record.clone();
        match index {
            0 => wrong.pid += 1,
            1 => wrong.ppid += 1,
            2 => wrong.uid = wrong.uid.wrapping_add(1),
            3 => wrong.zombie = true,
            _ => wrong.start_s = 0,
        }
        assert!(process_sample(&wrong, Some(executable.into()), 42, 41, executable).is_none());
    }
    assert!(process_sample(&record, None, 42, 41, executable).is_none());
    assert!(process_sample(&record, Some("/other/aterm".into()), 42, 41, executable).is_none());
}

#[test]
fn activation_uses_the_unique_live_session_in_the_verified_logical_window() {
    let row = "0 s-ab12 - alive shell nonce=0123456789abcdef0123456789abcdef window=0 active=1 wfocus=0\n";
    let selected = session(row, 0).unwrap();
    assert_eq!(selected.sid, "s-ab12");
    assert_ne!(selected, session(&row.replace("0123", "4321"), 0).unwrap());
    for wrong in [
        row.replace("window=0", "window=42"),
        row.replace("active=1", "active=0"),
        row.replace("alive", "exited"),
        row.replace("s-ab12", "guessed-session"),
        row.replace("0123456789abcdef0123456789abcdef", "missing"),
        format!("{row}{row}"),
        row.replace("window=0", "window=0 window=42"),
    ] {
        assert!(session(&wrong, 0).is_err());
    }
}

#[test]
fn native_focus_requires_active_and_key_and_keeps_its_own_window_number() {
    let ready = "OK available=1 window=42 app_active=1 window_key=1 key=1 posted_keys=0\n";
    let focus = focus(ready).unwrap();
    assert!(focus.ready(None));
    assert!(focus.ready(Some(42)));
    assert!(
        !focus.ready(Some(0)),
        "logical WindowId 0 is not AppKit windowNumber 42"
    );
    assert!(!focus.ready(Some(43)));
    for pending in [
        "OK available=0 window=0 app_active=- window_key=- key=- posted_keys=0\n",
        "OK available=1 window=42 app_active=0 window_key=1 key=0 posted_keys=0\n",
        "OK available=1 window=42 app_active=1 window_key=0 key=0 posted_keys=0\n",
    ] {
        assert!(!super::focus(pending).unwrap().ready(None));
    }
    for invalid in [
        ready.replace("window=42", "window=0"),
        ready.replace("key=1 posted", "key=0 posted"),
        ready.replace("posted_keys=0", "posted_keys=invalid"),
        format!("{ready}{ready}"),
    ] {
        assert!(super::focus(&invalid).is_err());
    }
}
