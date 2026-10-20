// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn env(vars: &[&str]) -> Vec<String> {
    vars.iter().map(|v| (*v).to_string()).collect()
}

/// ONE SIGN-IN, THE FIRST TAB'S: a second tab at the same login's wall finds
/// it taken and waits on it; once the login is back in the first tab, the
/// second reads it back there, and only until its own loss ends.
#[test]
fn the_first_tab_carries_the_sign_in_and_the_others_read_it_back() {
    let l = LoginLatches::default();
    assert_eq!(l.shared("default", "@1"), None, "nothing shared yet");
    assert_eq!(l.carry("default", "@1"), Ok(()));
    assert_eq!(l.carry("default", "@1"), Ok(()), "its own, again");
    assert_eq!(l.shared("default", "@1"), None, "its own sign-in");
    assert_eq!(
        l.shared("default", "@2"),
        Some(LoginShared::Carried {
            by: "@1".to_string()
        })
    );
    assert_eq!(l.carry("default", "@3"), Err("@1".to_string()));
    l.back("default", "@1");
    for sid in ["@2", "@3"] {
        assert_eq!(
            l.shared("default", sid),
            Some(LoginShared::Back {
                by: "@1".to_string()
            }),
            "{sid}"
        );
    }
    assert_eq!(
        l.shared("default", "@1"),
        None,
        "back where it was signed in"
    );
    l.back("default", "@2");
    assert_eq!(l.shared("default", "@2"), None, "its own loss over");
    // A tab whose turn met the wall again takes a sign-in of its own.
    assert_eq!(l.carry("default", "@3"), Ok(()));
    assert_eq!(l.shared("default", "@3"), None);

    let l = LoginLatches::default();
    let old = Arc::new(AtomicBool::new(false));
    let next = Arc::new(AtomicBool::new(false));
    assert_eq!(l.carry_for("default", "@1", &old), Ok(()));
    assert!(l.shared("default", "@2").is_some());
    l.abandon_for("@1", &next);
    assert_eq!(l.carry("default", "@2"), Err("@1".to_string()));
    l.abandon_for("@1", &old);
    assert_eq!(l.shared("default", "@2"), None, "no success was reported");
    assert_eq!(l.carry_for("default", "@1", &next), Ok(()));
    l.abandon_for("@1", &old);
    assert_eq!(l.carry("default", "@2"), Err("@1".to_string()));
    l.abandon_for("@1", &next);
    assert_eq!(l.carry("default", "@2"), Ok(()));
}

/// ANOTHER LOGIN IS ANOTHER SIGN-IN: a tab on its own login carries its own,
/// and a tab whose process moved onto another login leaves the one it carried.
#[test]
fn each_login_has_its_own_sign_in() {
    let l = LoginLatches::default();
    assert_eq!(l.carry("default", "@1"), Ok(()));
    assert_eq!(l.carry("dir:/work/b", "@2"), Ok(()));
    assert_eq!(
        l.shared("dir:/work/b", "@3"),
        Some(LoginShared::Carried {
            by: "@2".to_string()
        })
    );
    assert_eq!(l.carry("dir:/work/b", "@3"), Err("@2".to_string()));
    // @2's process signs in with the default login now: b's sign-in is
    // nobody's, and @2 waits on @1's.
    assert_eq!(l.carry("default", "@2"), Err("@1".to_string()));
    assert_eq!(l.shared("dir:/work/b", "@3"), None);
    assert_eq!(l.carry("dir:/work/b", "@3"), Ok(()));
    // @3's process relaunched onto another login, back there: b's sign-in
    // is nobody's, and the tab that waited on it takes its own.
    assert!(l.shared("dir:/work/b", "@4").is_some());
    l.back("dir:/work/c", "@3");
    assert_eq!(l.shared("dir:/work/b", "@4"), None);
    assert_eq!(l.carry("dir:/work/b", "@4"), Ok(()));
}

/// A CLOSED TAB'S SIGN-IN IS THE NEXT TAB'S: nobody waits on a tab that is
/// gone, and nothing reads a login back that never came back.
#[test]
fn a_closed_tabs_sign_in_is_the_next_tabs() {
    let l = LoginLatches::default();
    assert_eq!(l.carry("default", "@1"), Ok(()));
    assert!(l.shared("default", "@2").is_some());
    l.gone("@1");
    assert_eq!(l.shared("default", "@2"), None);
    assert_eq!(l.carry("default", "@2"), Ok(()));
    l.gone("@2");
    assert_eq!(l.shared("default", "@3"), None, "nothing left of it");
}

/// THE KEY IS THE KEYCHAIN ITEM: the login variable's directory where it is
/// set, else the home's, else the default item; none for a process that
/// signs in otherwise, or one whose environment was not read.
#[test]
fn the_key_is_the_stored_login_the_process_reads() {
    assert_eq!(login_key(&env(&["PATH=/bin"])), Some("default".to_string()));
    assert_eq!(
        login_key(&env(&["CLAUDE_CONFIG_DIR=/work/b"])),
        Some("dir:/work/b".to_string())
    );
    assert_eq!(
        login_key(&env(&[
            "CLAUDE_CONFIG_DIR=/work/b",
            "CLAUDE_SECURESTORAGE_CONFIG_DIR=/work/c"
        ])),
        Some("dir:/work/c".to_string())
    );
    assert_eq!(
        login_key(&env(&["CLAUDE_SECURESTORAGE_CONFIG_DIR=/work/b"])),
        login_key(&env(&["CLAUDE_CONFIG_DIR=/work/b"])),
        "one directory names one item"
    );
    assert_eq!(login_key(&env(&["ANTHROPIC_API_KEY=k", "PATH=/bin"])), None);
    assert_eq!(login_key(&[]), None);
}
