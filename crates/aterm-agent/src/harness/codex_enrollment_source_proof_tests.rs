// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn distinct_backend_multiplicity_is_signed_without_duplicating_image_admission() {
    let world = NativeSourceProofWorld::new().unwrap();
    world.publish().unwrap();
    let image = candidate(world.directory(), "distinct");
    let app = account_work::Lifetime::default();
    let scope = app.enter().unwrap();
    let task = world.retained_distinct_task(&image).unwrap();
    let launch = SourceProofLaunch::admit(world.env(), &task, &app, &scope, "t00000pr00f").unwrap();
    assert!(launch.retained_frontend());
    assert_eq!(launch.retained_images().unwrap().len(), 1);
    let expected = launch.retained_backend_images().unwrap();
    assert_eq!(expected.len(), 2);
    assert_eq!(expected[0], expected[1]);
    assert!(!launch.resources_frontend());
    let resources = world.retained_resources_task(&image).unwrap();
    let launch =
        SourceProofLaunch::admit(world.env(), &resources, &app, &scope, "t00000pr00f").unwrap();
    assert!(launch.resources_frontend());
    assert_eq!(launch.retained_images().unwrap().len(), 1);
    assert_eq!(launch.retained_backend_images().unwrap().len(), 2);
    let ordinary = world.retained_task(&image).unwrap();
    let launch =
        SourceProofLaunch::admit(world.env(), &ordinary, &app, &scope, "t00000pr00f").unwrap();
    assert_eq!(launch.retained_backend_images().unwrap().len(), 1);
    assert!(!launch.resources_frontend());
}

fn candidate(root: &Path, name: &str) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    // Marker/file-identity fixture only. Nothing executes these bytes or
    // treats them as SDK readiness, Source ownership or provider permission.
    let path = codex_alab::lay_package(&root.join(name), true);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b"aterm machine credential slot v1\0aterm machine credential slot pending v1\0aterm session credential source v1\0aterm isolated source proof candidate v1\0").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
    path
}

#[test]
fn replacement_image_is_authenticated_and_leased_with_its_original_launch() {
    let world = NativeSourceProofWorld::new().unwrap();
    world.publish().unwrap();
    let before = candidate(world.directory(), "before");
    let after = candidate(world.directory(), "after");
    let third = candidate(world.directory(), "third");
    let app = account_work::Lifetime::default();
    let scope = app.enter().unwrap();
    let task = world.retained_upgrade_task(&before, &after).unwrap();
    let encoded = task.strip_prefix(SOURCE_PROOF_TASK).unwrap();
    let bytes = aterm_codec::hex::decode_vec(encoded.as_bytes()).unwrap();
    let mut envelope: Envelope = aterm_json::from_slice(&bytes).unwrap();
    envelope.body.replacement.as_mut().unwrap().path = third.clone();
    let tampered = format!(
        "{SOURCE_PROOF_TASK}{}",
        aterm_codec::hex::encode_lower(&aterm_json::to_vec(&envelope).unwrap())
    );
    assert_eq!(
        SourceProofLaunch::admit(world.env(), &tampered, &app, &scope, "t00000pr00f")
            .err()
            .as_deref(),
        Some("source proof ticket authentication failed")
    );
    std::fs::rename(&third, &after).unwrap();
    assert_eq!(
        SourceProofLaunch::admit(world.env(), &task, &app, &scope, "t00000pr00f")
            .err()
            .as_deref(),
        Some("source proof candidate changed")
    );
    let task = world.retained_upgrade_task(&before, &after).unwrap();
    let launch = SourceProofLaunch::admit(world.env(), &task, &app, &scope, "t00000pr00f").unwrap();
    assert_eq!(launch.retained_images().unwrap().len(), 2);
    let canonical_after = std::fs::canonicalize(&after).unwrap();
    assert_eq!(
        launch.replacement_candidate(),
        Some(canonical_after.as_path())
    );
    let changed = candidate(world.directory(), "changed");
    std::fs::rename(changed, after).unwrap();
    assert!(
        launch.current().is_err(),
        "replacing either actual image retires the admitted launch"
    );
}

#[test]
fn foreign_and_paused_app_scopes_refuse_before_ticket_authentication() {
    let world = NativeSourceProofWorld::new().unwrap();
    let app = account_work::Lifetime::default();
    let foreign = account_work::Lifetime::default();
    let foreign_scope = foreign.enter().unwrap();
    let frame = format!("{SOURCE_PROOF_TASK}not-a-ticket");
    assert_eq!(
        SourceProofLaunch::admit(world.env(), &frame, &app, &foreign_scope, "t00000pr00f")
            .err()
            .as_deref(),
        Some("source proof admission unavailable")
    );
    let scope = app.enter().unwrap();
    let _pause = app.pause().unwrap();
    assert_eq!(
        SourceProofLaunch::admit(world.env(), &frame, &app, &scope, "t00000pr00f")
            .err()
            .as_deref(),
        Some("source proof admission unavailable")
    );
    assert!(!world.directory().read_dir().unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".claimed")
    }));
}
