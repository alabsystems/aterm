// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture() -> (aterm_tempfile::TempDir, Store, Logins) {
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-binding-witness-")
        .tempdir()
        .unwrap();
    let store = Store::at(root.path());
    let mut bindings = Logins::default();
    note_renewed(
        &mut State::default(),
        &mut bindings,
        "main",
        None,
        &store.logins_dir().join("main+1").to_string_lossy(),
        None,
        100,
    );
    (root, store, bindings)
}

#[test]
fn lost_bindings_refuse_selection_and_edits_until_explicit_restore_for_each_pool() {
    let (root, _, bindings) = fixture();
    let original = Roster::parse("[[account]]\nlabel='main'\nclass='dedicated'\n").unwrap();
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let store = Store::at_for(root.path(), cli, &root.path().join("home"));
        assert_eq!(store.bound(&original, 100).unwrap(), original);
        assert!(!store.dir.exists(), "first read creates nothing");
        store.save_logins(&bindings).unwrap();
        assert_eq!(store.logins_witness_file(), store.dir.join("logins.seen"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(store.logins_witness_file())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let backup = store.dir.join("retained-bindings.json");
        std::fs::rename(store.logins_file(), &backup).unwrap();
        let before = std::fs::read(&backup).unwrap();
        assert!(
            store
                .bound(&original, 100)
                .unwrap_err()
                .contains("logins.seen")
        );
        assert!(store.check_signin_state(100).is_err());
        let mut edited = false;
        assert!(
            store
                .transact_logins(100, &mut None, |_, _| {
                    edited = true;
                    Ok(Vec::new())
                })
                .is_err()
        );
        assert!(!edited);
        for path in [
            store.logins_file(),
            store.state_file(),
            store.ledger_path(),
            store.wake_path(),
        ] {
            assert!(!path.exists());
        }
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        store.save_logins(&bindings).unwrap();
        assert_eq!(store.load_logins(100), Ok((bindings.clone(), None)));
        assert_eq!(std::fs::read(&backup).unwrap(), before);
    }
}

#[test]
fn legacy_bindings_and_state_only_stores_remain_readable_without_markers() {
    let (_root, store, bindings) = fixture();
    store.save(&State::default()).unwrap();
    assert_eq!(store.load_logins(100), Ok((Logins::default(), None)));
    assert!(!store.logins_witness_file().exists());
    std::fs::write(store.logins_file(), bindings.to_json()).unwrap();
    assert_eq!(store.load_logins(100), Ok((bindings, None)));
    assert!(
        !store.logins_witness_file().exists(),
        "reading never migrates"
    );
    store.save_logins(&Logins::default()).unwrap();
    assert!(
        store.logins_witness_file().is_file(),
        "even an empty published snapshot is retained"
    );
    assert_eq!(store.load_logins(100), Ok((Logins::default(), None)));
}

#[test]
fn a_directory_witness_refuses_replacement_and_preserves_prior_bindings() {
    let (_root, store, bindings) = fixture();
    store.ensure().unwrap();
    std::fs::write(store.logins_file(), bindings.to_json()).unwrap();
    let before = std::fs::read(store.logins_file()).unwrap();
    let witness = store.logins_witness_file();
    std::fs::create_dir(&witness).unwrap();
    std::fs::write(witness.join("keep"), b"untouched").unwrap();
    assert!(store.save_logins(&Logins::default()).is_err());
    assert_eq!(std::fs::read(store.logins_file()).unwrap(), before);
    assert_eq!(std::fs::read(witness.join("keep")).unwrap(), b"untouched");
    std::fs::rename(store.logins_file(), store.dir.join("kept.json")).unwrap();
    assert!(store.load_logins(100).unwrap_err().contains("logins.seen"));
}

#[cfg(unix)]
#[test]
fn binding_witness_symlinks_refuse_without_touching_their_targets() {
    for existing in [false, true] {
        let (root, store, bindings) = fixture();
        store.ensure().unwrap();
        let target = root.path().join("keep");
        if existing {
            std::fs::write(&target, b"untouched").unwrap();
        }
        let witness = store.logins_witness_file();
        std::os::unix::fs::symlink(&target, &witness).unwrap();
        assert!(
            store.load_logins(100).is_err(),
            "dangling evidence is present"
        );
        assert!(store.save_logins(&bindings).is_err());
        assert!(!store.logins_file().exists());
        assert_eq!(std::fs::read_link(&witness).unwrap(), target);
        if existing {
            assert_eq!(std::fs::read(target).unwrap(), b"untouched");
        } else {
            assert!(!target.exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn publication_witness_fifos_refuse_with_or_without_a_reader() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};

    for binding in [false, true] {
        for reader in [false, true] {
            let (_root, store, bindings) = fixture();
            store.ensure().unwrap();
            let witness = if binding {
                store.logins_witness_file()
            } else {
                store.state_witness_file()
            };
            let name = std::ffi::CString::new(witness.as_os_str().as_bytes()).unwrap();
            // SAFETY: a live NUL-terminated scratch path; mkfifo retains no pointer.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let _reader = reader.then(|| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&witness)
                    .unwrap()
            });
            let (tx, rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                tx.send(if binding {
                    store.save_logins(&bindings)
                } else {
                    store.save(&State::default())
                })
                .unwrap();
            });
            assert!(
                rx.recv_timeout(std::time::Duration::from_secs(60))
                    .expect("witness open hung on a FIFO")
                    .is_err()
            );
            worker.join().unwrap();
            assert!(
                std::fs::symlink_metadata(witness)
                    .unwrap()
                    .file_type()
                    .is_fifo()
            );
        }
    }
}
