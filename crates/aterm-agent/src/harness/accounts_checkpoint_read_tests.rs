// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn checked_completion_reads_keep_unavailable_io_distinct_from_invalid_data() {
    let world = aterm_tempfile::TempDir::new().unwrap();
    let staging = world.path().join("owner+1");
    std::fs::create_dir(&staging).unwrap();
    let actual = std::fs::canonicalize(&staging).unwrap();
    let now = 1_800_000_000;
    write_renewal_checkpoint_for(
        Cli::Codex,
        None,
        "owner",
        "ann@example.com",
        staging.to_str().unwrap(),
        None,
        Some(Proof::Status),
        now - 1,
        &Logins::default(),
    )
    .unwrap();
    let path = staging.join(RENEWAL_CHECKPOINT_FILE);
    let original = std::fs::read(&path).unwrap();
    assert!(
        read_renewal_checkpoint_checked_for(Cli::Codex, &staging, &actual, now)
            .unwrap()
            .is_some()
    );
    let unavailable = read_renewal_checkpoint_with(
        Cli::Codex,
        &staging,
        &actual,
        now,
        |_| Err(io::ErrorKind::PermissionDenied.into()),
        |path| std::fs::canonicalize(path),
        native_google_digest_checked,
    );
    assert_eq!(
        unavailable.unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    let failed_read = read_renewal_checkpoint_with(
        Cli::Codex,
        &staging,
        &actual,
        now,
        |path| {
            let file = std::fs::OpenOptions::new().write(true).open(path)?;
            let metadata = file.metadata()?;
            Ok((file, metadata))
        },
        |path| std::fs::canonicalize(path),
        native_google_digest_checked,
    );
    assert!(
        failed_read.is_err(),
        "reading the actual write-only descriptor fails as I/O"
    );
    let unavailable_path = read_renewal_checkpoint_with(
        Cli::Codex,
        &staging,
        &actual,
        now,
        aterm_uds::regular_file::open,
        |_| Err(io::ErrorKind::PermissionDenied.into()),
        native_google_digest_checked,
    );
    assert_eq!(
        unavailable_path.unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(
        read_renewal_checkpoint_checked_for(
            Cli::Codex,
            &world.path().join("missing"),
            &actual,
            now
        )
        .unwrap()
        .is_none()
    );
    assert!(
        read_renewal_checkpoint_checked_for(Cli::Codex, &staging, &actual, now - 2)
            .unwrap()
            .is_none(),
        "a future check is not accepted"
    );
    assert!(
        read_renewal_checkpoint_checked_for(Cli::Codex, &staging, &actual, now + 10_000_000)
            .unwrap()
            .is_some(),
        "no expiration window is invented"
    );
    for invalid in [
        b"{not-json".to_vec(),
        vec![b'x'; RENEWAL_CHECKPOINT_MAX as usize + 1],
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert!(
            read_renewal_checkpoint_checked_for(Cli::Codex, &staging, &actual, now)
                .unwrap()
                .is_none()
        );
    }
    std::fs::write(&path, &original).unwrap();
    let mut future: Value = aterm_json::from_str(std::str::from_utf8(&original).unwrap()).unwrap();
    let fields = future.as_object_mut().unwrap();
    fields.insert("program".into(), Value::from(Cli::Agy.id()));
    fields.insert("schema".into(), Value::from(5));
    fields.insert("checked_at".into(), Value::from(now + 1));
    for field in [
        "google_principal",
        "google_grant_digest",
        "google_binding_scope",
    ] {
        fields.insert(field.into(), Value::from("1".repeat(64)));
    }
    let future = future.to_string();
    assert!(decode_renewal_checkpoint(Cli::Agy, future.as_bytes()).is_some());
    std::fs::write(&path, future).unwrap();
    assert!(
        read_renewal_checkpoint_with(
            Cli::Agy,
            &staging,
            &actual,
            now,
            aterm_uds::regular_file::open,
            |_| panic!("future metadata needs no path resolution"),
            |_| panic!("future metadata needs no native grant read")
        )
        .unwrap()
        .is_none()
    );
    std::fs::write(&path, &original).unwrap();
    let other = world.path().join("different");
    std::fs::create_dir(&other).unwrap();
    assert!(
        read_renewal_checkpoint_checked_for(Cli::Codex, &staging, &other, now)
            .unwrap()
            .is_none(),
        "a known directory mismatch is invalid"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(
        read_renewal_checkpoint_checked_for(Cli::Codex, &staging, &actual, now)
            .unwrap()
            .is_some()
    );
}
