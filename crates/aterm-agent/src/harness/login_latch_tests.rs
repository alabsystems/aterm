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
    assert_eq!(
        login_key(&env(&[
            "CLAUDE_CONFIG_DIR=/work/first",
            "CLAUDE_CONFIG_DIR=/work/second"
        ])),
        Some("dir:/work/second".to_string())
    );
    assert_eq!(
        login_key(&env(&[
            "CLAUDE_CONFIG_DIR=/work/first",
            "CLAUDE_CONFIG_DIR="
        ])),
        None
    );
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

/// The rows carried have a generation that a traced read of them records: a carry of a row, a
/// login back, a closed carrier and an ended worker each move it once and ring the bell once on
/// the thread that made it, so a trace that read the rows before stops standing, while a tab
/// that only waits, a carry that names no row and a refused carry move nothing.
///
/// NEGATIVE CONTROL (2026-11-15): `LoginLatches::gone` locking the table itself instead of
/// through `change` (no bump) failed with "closed carrier: the rows read before it still stood",
/// left `Ok(())`, right `Err(Memory("login_latch/carried_rows"))`. Restored.
#[test]
fn the_rows_carried_move_their_generation_only_when_another_set_is_carried() {
    use aterm_uds::trace::{self, Moved, Now};
    let l = LoginLatches::default();
    let owner = Arc::new(AtomicBool::new(false));
    let read = || trace::scope(|| l.carried_rows()).1;
    let stands = |trace: &trace::Trace| trace.stands(Now::at(0), std::time::Instant::now());
    let moved = Err(Moved::Memory("login_latch/carried_rows"));
    let rung = trace::rung_here;

    let none = read();
    assert_eq!(
        none.memory().collect::<Vec<_>>(),
        [("login_latch/carried_rows", 0)]
    );
    let before = rung();
    assert_eq!(l.carry_for("default", "@1", &owner, None), Ok(()));
    assert_eq!(
        l.shared("default", "@2"),
        Some(LoginShared::Carried { by: "@1".into() })
    );
    assert_eq!(
        l.carry_for("default", "@3", &owner, None),
        Err("@1".to_string())
    );
    assert_eq!(
        stands(&none),
        Ok(()),
        "a carry that named no row moved the rows read"
    );
    assert_eq!(rung(), before, "a carry that named no row rang");

    for (what, change) in [
        (
            "a row carried",
            Box::new(|| {
                assert_eq!(l.carry_for("dir:/a", "@4", &owner, Some("work-a")), Ok(()));
            }) as Box<dyn Fn()>,
        ),
        ("login back", Box::new(|| l.back("dir:/a", "@4"))),
        (
            "a row carried again",
            Box::new(|| {
                assert_eq!(l.carry_for("dir:/a", "@4", &owner, Some("work-a")), Ok(()));
            }),
        ),
        ("closed carrier", Box::new(|| l.gone("@4"))),
        (
            "a row carried once more",
            Box::new(|| {
                assert_eq!(l.carry_for("dir:/a", "@4", &owner, Some("work-a")), Ok(()));
            }),
        ),
        (
            "its worker ended",
            Box::new(|| assert!(l.abandon_for("@4", &owner))),
        ),
    ] {
        let seen = read();
        let _waiting = l.shared("dir:/a", "@5");
        assert_eq!(
            stands(&seen),
            Ok(()),
            "{what}: a waiting tab moved the rows read before it"
        );
        let before = rung();
        change();
        assert_eq!(
            stands(&seen),
            moved,
            "{what}: the rows read before it still stood"
        );
        assert_eq!(rung() - before, 1, "{what}: rang {} times", rung() - before);
    }
    assert!(l.carried_rows().is_empty());
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

    assert_eq!(l.carry_for("dir:/a", "@1", &owner, Some("work-a")), Ok(()));
    assert_eq!(l.carry_for("dir:/b", "@1", &owner, None), Ok(()));
    assert!(
        !l.lock().contains_key("dir:/a"),
        "idle prior latch is pruned when a tab switches login"
    );
    assert!(!l.abandon_for("@1", &owner));
}

#[test]
fn a_token_session_takes_no_sign_in_latch() {
    use crate::harness::accounts::{AuthRoute, TokenEntry};
    let entry = TokenEntry::parse("aterm-claude-token-ac-00000000000000a1").expect("an entry");
    let at = |route| ProcessLogin {
        login: Some("/work/b".to_owned()),
        default_home: true,
        route,
    };
    let none = env(&[]);
    assert_eq!(
        login_key_of(at(AuthRoute::Stored), &none),
        Some("dir:/work/b".to_owned())
    );
    for route in [AuthRoute::VaultToken { entry }, AuthRoute::Outside] {
        assert_eq!(login_key_of(at(route), &none), None, "{route:?}");
    }
}
