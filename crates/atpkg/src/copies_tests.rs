// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_containment::consent::{Claimant, DrClass, Enumeration};

const DEV_ID: &str = "identifier \"com.aterm.aterm\" and anchor apple generic and \
                      certificate leaf[subject.OU] = \"A66A9P66Z7\"";

fn copy(path: &str, adhoc: bool, running: bool) -> Claimant {
    Claimant {
        path: PathBuf::from(path),
        dr: if adhoc {
            DrClass::Cdhash
        } else {
            DrClass::Identity
        },
        dr_text: if adhoc {
            format!("cdhash H\"{:040}\"", path.len())
        } else {
            DEV_ID.to_string()
        },
        signing: if adhoc { "adhoc" } else { "developer-id" },
        team: (!adhoc).then(|| "A66A9P66Z7".to_string()),
        running,
        links: Vec::new(),
    }
}

fn census_of(found: Vec<Claimant>, enumeration: Enumeration) -> Claimants {
    Claimants { found, enumeration }
}

fn owners_mac() -> Claimants {
    census_of(
        vec![
            copy("/Applications/aterm.app", false, true),
            copy("/Applications/aterm-prev-1788285383.app", true, false),
            copy("/Applications/aterm-local-1789361388.app", true, false),
            Claimant {
                dr_text: "identifier \"com.aterm.aterm\" and anchor apple generic".into(),
                ..copy("/Applications/aterm.app.rollback", false, false)
            },
        ],
        Enumeration::Complete,
    )
}

#[test]
fn the_prune_moves_only_the_census_offered_copies_and_names_the_rest() {
    let moved = std::cell::RefCell::new(Vec::new());
    let got = prune_with(
        "com.aterm.aterm",
        |_| owners_mac(),
        |_, _| true,
        |path| Some(path.ends_with("aterm-local-1789361388.app")),
        |path| {
            moved.borrow_mut().push(path.to_path_buf());
            Ok(())
        },
        true,
    );
    assert_eq!(
        moved.into_inner(),
        vec![PathBuf::from("/Applications/aterm-prev-1788285383.app")],
        "the copy in use stays; the Developer ID rollback is never moved"
    );
    let row = row(&got);
    assert_eq!(
        row.level,
        Level::Warn,
        "a copy in use that stays is a warning: {row:?}"
    );
}

#[test]
fn nothing_moves_without_a_trash_tool_or_beside_an_unstable_running_copy() {
    let never = |_: &Path| -> Result<(), String> { panic!("nothing may move") };
    let got = prune_with(
        "x",
        |_| owners_mac(),
        |_, _| true,
        |_| Some(false),
        never,
        false,
    );
    assert!(
        matches!(&got, Pruned::Done { moved, .. } if moved.is_empty()),
        "{got:?}"
    );
    let unstable = || {
        census_of(
            vec![
                copy("/Applications/aterm-local-1.app", true, true),
                copy("/Applications/aterm-prev-2.app", true, false),
            ],
            Enumeration::Complete,
        )
    };
    let got = prune_with(
        "x",
        |_| unstable(),
        |_, _| true,
        |_| Some(false),
        never,
        true,
    );
    assert!(
        matches!(&got, Pruned::Done { moved, kept } if moved.is_empty()
            && kept == &[(PathBuf::from("/Applications/aterm-prev-2.app"), "running-unstable")]),
        "{got:?}"
    );
}

#[test]
fn the_row_is_ok_alone_and_says_what_it_could_not_check() {
    let alone = prune_with(
        "x",
        |_| {
            census_of(
                vec![copy("/Applications/aterm.app", false, true)],
                Enumeration::Complete,
            )
        },
        |_, _| true,
        |_| Some(false),
        |_| Ok(()),
        true,
    );
    assert_eq!(row(&alone).level, Level::Ok);
    let partial = Pruned::Incomplete { moved: Vec::new() };
    assert_eq!(
        row(&partial).level,
        Level::Note,
        "an incomplete census is never ok"
    );
}

#[test]
fn a_test_binary_prunes_nothing() {
    for dry_run in [false, true] {
        let got = prune(dry_run);
        assert!(matches!(got, Pruned::NotLooked(_)), "{got:?}");
    }
    let look = look_with("x", |_| owners_mac(), true);
    let got = prune_after(&look);
    assert!(matches!(got, Pruned::NotLooked(_)), "{got:?}");
}

/// A prune right after a look takes one census, the fresh one its moves are checked against,
/// and moves what a prune from its own census moves. NEGATIVE CONTROL: [`prune_with`] after
/// the same look takes two (the planning census again, then the fresh one).
#[test]
fn a_prune_after_a_look_takes_only_the_fresh_census() {
    let censuses = std::cell::Cell::new(0);
    let census = |_: &str| {
        censuses.set(censuses.get() + 1);
        owners_mac()
    };
    let in_use = |path: &Path| Some(path.ends_with("aterm-local-1789361388.app"));
    let look = look_with("com.aterm.aterm", census, true);
    assert_eq!(censuses.get(), 1, "the look's census");
    let after = prune_after_with(
        &look,
        "com.aterm.aterm",
        census,
        |_, _| true,
        in_use,
        |_| Ok(()),
    )
    .expect("a look");
    assert_eq!(
        censuses.get(),
        2,
        "one census more: the fresh one before the moves"
    );
    let alone = prune_with(
        "com.aterm.aterm",
        census,
        |_, _| true,
        in_use,
        |_| Ok(()),
        true,
    );
    assert_eq!(
        censuses.get(),
        4,
        "a prune of its own plans and re-reads: two"
    );
    assert_eq!(after, alone, "the same moves and the same kept copies");
    assert!(
        matches!(&after, Pruned::Done { moved, .. }
            if moved.iter().filter(|(_, r)| *r == consent::Retired::Moved).count() == 1),
        "one copy moved, the one in use kept: {after:?}"
    );
    let nothing = Pruned::Found {
        offered: Vec::new(),
        kept: Vec::new(),
        complete: true,
    };
    let never = |_: &Path| -> Result<(), String> { panic!("nothing may move") };
    assert_eq!(
        prune_after_with(&nothing, "x", census, |_, _| true, in_use, never),
        Some(Pruned::Done {
            moved: Vec::new(),
            kept: Vec::new()
        })
    );
    assert_eq!(censuses.get(), 4, "nothing offered: no census at all");
    assert_eq!(
        prune_after_with(
            &Pruned::NotLooked("no".into()),
            "x",
            census,
            |_, _| true,
            in_use,
            never
        ),
        None,
        "no look: the caller prunes from its own census"
    );
}

#[test]
fn a_look_names_what_a_prune_would_move() {
    let got = look_with("com.aterm.aterm", |_| owners_mac(), true);
    assert_eq!(
        got,
        Pruned::Found {
            offered: vec![
                PathBuf::from("/Applications/aterm-prev-1788285383.app"),
                PathBuf::from("/Applications/aterm-local-1789361388.app"),
            ],
            kept: vec![(
                PathBuf::from("/Applications/aterm.app.rollback"),
                "signature"
            )],
            complete: true,
        }
    );
    assert_eq!(row(&got).level, Level::Warn);
}

#[test]
fn the_refusal_is_the_systems_own_words() {
    let stderr = "2026-09-23 22:29:30.491 trash[79297:53920104] # Error attempting to move \
                  /x/Nope.app to the trash folder, Error Domain=NSCocoaErrorDomain Code=4 \
                  \"The file “Nope.app” doesn’t exist.\" UserInfo={NSURL=file:///x/Nope.app}";
    assert_eq!(
        trash_refusal(stderr).as_deref(),
        Some("The file “Nope.app” doesn’t exist.")
    );
    assert_eq!(trash_refusal("no code here"), None);
    assert_eq!(trash_refusal("Code=4 \"\""), None);
}
