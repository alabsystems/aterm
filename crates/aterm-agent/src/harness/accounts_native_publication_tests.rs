// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::harness::accounts::{self, Cli, Store};
use std::cell::Cell;

#[test]
fn derived_generation_hook_precedes_canonical_write_and_failure_preserves_old_binding() {
    let world = aterm_tempfile::tempdir().unwrap();
    let home = world.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let store = Store::at_for(world.path(), Cli::Codex, &home);
    let old_dir = world.path().join("old");
    let new_dir = world.path().join("new");
    let now = 1_800_000_000;
    store
        .transact_logins(now, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "work",
                None,
                old_dir.to_str().unwrap(),
                None,
                now,
            ))
        })
        .unwrap();
    let path = world.path().join("roster.toml");
    std::fs::write(
        &path,
        "[[account]]\nlabel = 'work'\nemail = 'work@example.test'\nclass = 'dedicated'\n",
    )
    .unwrap();
    let (before, _) = store.load_logins(now).unwrap();
    let old_generation = before.get("work").unwrap().last_success_generation;
    let old_image = std::fs::read(store.logins_file()).unwrap();
    let called = Cell::new(0);
    let attempt = |refuse: bool| {
        store.transact_roster_logins_native_within(
            &[],
            &path,
            now + 1,
            &mut None,
            |_, state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    None,
                    new_dir.to_str().unwrap(),
                    None,
                    now + 1,
                ))
            },
            |permit| {
                called.set(called.get() + 1);
                assert_eq!(
                    permit.after.get("work").unwrap().last_success_generation,
                    old_generation + 1
                );
                assert_eq!(
                    permit.before.get("work").unwrap().last_success_generation,
                    old_generation
                );
                let (on_disk, _) = store.load_logins(now + 1).unwrap();
                assert_eq!(
                    on_disk.get("work").unwrap().last_success_generation,
                    old_generation
                );
                if refuse {
                    Err("native owner baseline unavailable".into())
                } else {
                    Ok(None)
                }
            },
        )
    };
    assert!(attempt(true).is_err());
    assert_eq!(std::fs::read(store.logins_file()).unwrap(), old_image);
    attempt(false).unwrap();
    assert_eq!(called.get(), 2);
    let (published, _) = store.load_logins(now + 1).unwrap();
    assert_eq!(
        published.get("work").unwrap().last_success_generation,
        old_generation + 1
    );
    assert_eq!(
        published.get("work").unwrap().dir.as_deref(),
        new_dir.to_str()
    );
}
