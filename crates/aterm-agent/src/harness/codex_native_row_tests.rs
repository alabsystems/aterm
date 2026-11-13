// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn occurrence_requires_real_writer_and_preserves_original_admission() {
    let world = aterm_tempfile::tempdir().unwrap();
    let home = world.path().join("home");
    let stage = world.path().join("stage");
    for path in [&home, &stage] {
        aterm_types::fs_restricted::ensure_private_dir(path).unwrap();
    }
    let store = accounts::Store::at_for(world.path(), accounts::Cli::Codex, &home);
    let roster = world.path().join("roster.toml");
    std::fs::write(
        &roster,
        "[[account]]\nlabel='work'\nemail='work@example.test'\nclass='dedicated'\n",
    )
    .unwrap();
    let request = super::super::codex_enrollment::Request {
        id: "a".repeat(32),
        caller: "b".repeat(64),
        label: "work".into(),
        revision: "c".repeat(64),
    };
    let now = 1_800_000_000;
    store
        .transact_logins(now, &mut None, |_, logins| {
            logins.enrollment.admit_native(&request)?;
            Ok(Vec::new())
        })
        .unwrap();
    let held = store.acquire_bounded().unwrap().unwrap();
    store
        .transact_pool_logins_held(&held, &roster, now, &mut None, |_, _, logins| {
            accounts::admit_workflow(logins, &request.label)?;
            Ok(Vec::new())
        })
        .unwrap();
    let foreign = accounts::Store::at_for(&world.path().join("other"), accounts::Cli::Codex, &home);
    assert!(
        foreign
            .native_row_snapshot_held(&held, &roster, now)
            .is_err()
    );
    let occurrence = NativeRowOccurrence::capture(
        store.native_row_snapshot_held(&held, &roster, now).unwrap(),
        &request,
    )
    .unwrap();
    let staging = super::super::codex_enrollment::Staging {
        native_row: Some(occurrence.fence().clone()),
        request: request.clone(),
        store: occurrence.store.clone(),
        directory: std::fs::canonicalize(&stage).unwrap(),
        store_identity: Some(occurrence.store_identity.clone()),
        directory_identity: Some(
            super::super::codex_enrollment::DirectoryIdentity::capture(&stage).unwrap(),
        ),
        parent: occurrence.parent,
        workflow: occurrence.workflow,
        policy: occurrence.policy().to_owned(),
        selection: occurrence.selection.clone(),
    };
    assert_eq!(
        occurrence.finish(staging.clone()).unwrap().staging(),
        &staging
    );
    let changed = NativeRowOccurrence::capture(
        store.native_row_snapshot_held(&held, &roster, now).unwrap(),
        &request,
    )
    .unwrap();
    let mut wrong = staging;
    wrong.policy = "d".repeat(64);
    assert!(changed.finish(wrong).is_err());
    let before = std::fs::read(store.logins_file()).unwrap();
    drop(held);
    assert_eq!(std::fs::read(store.logins_file()).unwrap(), before);
    assert!(!stage.join("auth.json").exists());
}
