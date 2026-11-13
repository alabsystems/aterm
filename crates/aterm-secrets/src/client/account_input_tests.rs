// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture() -> (aterm_tempfile::TempDir, Paths, Admin) {
    let temp = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&temp.path().join("home"));
    super::super::create(
        &paths,
        temp.path().join("test.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let admin = super::super::admin_open(&paths).unwrap();
    (temp, paths, admin)
}

#[test]
fn native_entry_publication_preserves_other_entries_and_compares_current_value() {
    let (_temp, paths, mut admin) = fixture();
    let name = format!("native-api-key-{}", "11".repeat(24));
    let before = admin.doc.entries();
    admin
        .doc
        .set_entry(&Audience::Everyone, "unpublished-draft", b"must-not-escape")
        .unwrap();
    admin
        .doc
        .set_entry(&Audience::Everyone, &name, b"unpublished-native-draft")
        .unwrap();
    assert_eq!(admin.native_entry_digest(&name).unwrap(), None);
    admin_set_entry_until(
        &paths,
        &admin,
        &name,
        None,
        b"first",
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let current = super::super::admin_open(&paths).unwrap();
    assert!(
        current
            .doc
            .entry(&Audience::Everyone, "unpublished-draft")
            .is_none()
    );
    for entry in before {
        assert_eq!(
            current.doc.entry(&entry.audience, &entry.name),
            Some(&entry)
        );
    }
    assert_eq!(
        current
            .doc
            .entry(&Audience::Everyone, &name)
            .unwrap()
            .value
            .as_slice(),
        b"first"
    );
    assert!(
        admin_set_entry_until(
            &paths,
            &current,
            &name,
            None,
            b"wrong",
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .is_err()
    );
    assert_eq!(
        super::super::admin_open(&paths)
            .unwrap()
            .doc
            .entry(&Audience::Everyone, &name)
            .unwrap()
            .value
            .as_slice(),
        b"first"
    );
}

#[test]
fn expired_native_publication_mutates_no_entry() {
    let (_temp, paths, admin) = fixture();
    let before = admin.doc.render();
    let name = format!("native-api-key-{}", "22".repeat(24));
    assert!(admin_set_entry_until(&paths, &admin, &name, None, b"never", Instant::now()).is_err());
    assert_eq!(
        super::super::admin_open(&paths).unwrap().doc.render(),
        before
    );
    assert!(!paths.home.join(".secrets").join(name).exists());
}

#[test]
fn native_publication_waits_on_its_existing_state_lock_within_one_budget() {
    let (_temp, paths, admin) = fixture();
    let held = std::sync::Arc::new(std::sync::Mutex::new(Some(
        super::super::StateLock::take(&paths).unwrap(),
    )));
    let release = held.clone();
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = observed.clone();
    let name = format!("native-api-key-{}", "33".repeat(24));
    aterm_uds::file_lock::with_lock_wait_observer(
        move |_| {
            seen.store(true, std::sync::atomic::Ordering::Relaxed);
            release.lock().unwrap().take();
        },
        || {
            admin_set_entry_until(
                &paths,
                &admin,
                &name,
                None,
                b"after contention",
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            )
            .unwrap();
        },
    );
    assert!(observed.load(std::sync::atomic::Ordering::Relaxed));
    assert!(held.lock().unwrap().is_none());
}
