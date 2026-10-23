// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn env(vars: &[&str]) -> Vec<String> {
    vars.iter().map(|v| (*v).to_string()).collect()
}

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
    assert_eq!(l.carry("default", "@3"), Ok(()));
    assert_eq!(l.shared("default", "@3"), None);

    let l = LoginLatches::default();
    let old = Arc::new(AtomicBool::new(false));
    let next = Arc::new(AtomicBool::new(false));
    assert_eq!(l.carry_for("default", "@1", &old, None), Ok(()));
    assert!(l.shared("default", "@2").is_some());
    l.abandon_for("@1", &next);
    assert_eq!(l.carry("default", "@2"), Err("@1".to_string()));
    l.abandon_for("@1", &old);
    assert_eq!(l.shared("default", "@2"), None, "no success was reported");
    assert_eq!(l.carry_for("default", "@1", &next, None), Ok(()));
    l.abandon_for("@1", &old);
    assert_eq!(l.carry("default", "@2"), Err("@1".to_string()));
    l.abandon_for("@1", &next);
    assert_eq!(l.carry("default", "@2"), Ok(()));
}

#[test]
fn the_carrier_reads_the_login_back_where_it_came_back_elsewhere() {
    let back_in = |by: &str| Some(LoginShared::Back { by: by.to_string() });
    let l = LoginLatches::default();
    assert_eq!(l.carry("default", "@1"), Ok(()));
    assert!(l.shared("default", "@2").is_some(), "@2 waits on @1");
    l.back("default", "@3");
    assert_eq!(l.shared("default", "@1"), back_in("@3"), "the carrier");
    assert_eq!(l.shared("default", "@2"), back_in("@3"), "the waiter");
    assert_eq!(l.shared("default", "@3"), None, "back where it came back");
    l.back("default", "@1");
    assert_eq!(l.shared("default", "@1"), None, "its own loss over");
    assert_eq!(l.shared("default", "@2"), back_in("@3"));
    let l = LoginLatches::default();
    assert_eq!(l.carry("default", "@1"), Ok(()));
    l.back("default", "@1");
    assert_eq!(l.shared("default", "@1"), None);
    assert_eq!(l.carry("default", "@2"), Ok(()), "nobody's sign-in");
}

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
    assert_eq!(l.carry("default", "@2"), Err("@1".to_string()));
    assert_eq!(l.shared("dir:/work/b", "@3"), None);
    assert_eq!(l.carry("dir:/work/b", "@3"), Ok(()));
    assert!(l.shared("dir:/work/b", "@4").is_some());
    l.back("dir:/work/c", "@3");
    assert_eq!(l.shared("dir:/work/b", "@4"), None);
    assert_eq!(l.carry("dir:/work/b", "@4"), Ok(()));
}

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

#[test]
fn the_rows_carried_are_those_a_tab_signs_in_now() {
    let l = LoginLatches::default();
    let owner = Arc::new(AtomicBool::new(false));
    assert_eq!(l.carry_for("dir:/a", "@1", &owner, Some("work-a")), Ok(()));
    assert_eq!(l.carry_for("default", "@2", &owner, None), Ok(()));
    assert_eq!(
        l.carried_rows(),
        BTreeSet::from(["work-a".to_string()]),
        "a carry with no row names none"
    );
    assert_eq!(
        l.carry_for("dir:/a", "@3", &owner, Some("work-a")),
        Err("@1".to_string())
    );
    l.back("dir:/a", "@1");
    assert!(l.carried_rows().is_empty(), "the login back");
    assert_eq!(l.carry_for("dir:/a", "@1", &owner, Some("work-a")), Ok(()));
    l.gone("@1");
    assert!(l.carried_rows().is_empty(), "the carrier gone");
    assert_eq!(l.carry_for("dir:/a", "@1", &owner, Some("work-a")), Ok(()));
    l.abandon_for("@1", &owner);
    assert!(l.carried_rows().is_empty(), "its worker ended");
}

#[test]
fn a_release_says_whether_a_rows_carry_ended() {
    let l = LoginLatches::default();
    let (owner, older) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(l.carry_for("dir:/a", "@1", &owner, None), Ok(()));
    assert!(!l.abandon_for("@1", &owner), "a carry that named no row");
    assert_eq!(l.carry_for("dir:/a", "@1", &owner, Some("work-a")), Ok(()));
    assert!(!l.abandon_for("@1", &older), "another worker's");
    assert_eq!(l.carried_rows(), BTreeSet::from(["work-a".to_string()]));
    assert!(l.abandon_for("@1", &owner), "its own, a row's");
    assert!(l.carried_rows().is_empty());
    assert!(!l.abandon_for("@1", &owner), "ended already");
}
