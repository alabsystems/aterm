// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn attempt(store: &Store, signin: bool) -> io::Result<Option<Lock>> {
    if signin {
        store.signin_now()
    } else {
        store.try_acquire()
    }
}

fn fixture(signin: bool) -> (aterm_tempfile::TempDir, Store, PathBuf) {
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-account-lock-path-")
        .tempdir()
        .unwrap();
    let store = Store::at(root.path());
    store.ensure().unwrap();
    let path = store
        .dir
        .join(if signin { SIGNIN_LOCK_FILE } else { LOCK_FILE });
    (root, store, path)
}

#[test]
fn existing_regular_lock_files_keep_their_contents_and_identity() {
    for signin in [false, true] {
        let (_root, store, path) = fixture(signin);
        let bytes = b"preexisting lock-file contents\n";
        std::fs::write(&path, bytes).unwrap();
        #[cfg(unix)]
        let before = {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = std::fs::metadata(&path).unwrap();
            (metadata.dev(), metadata.ino(), metadata.mode())
        };
        let held = attempt(&store, signin).unwrap().unwrap();
        drop(held);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = std::fs::metadata(&path).unwrap();
            assert_eq!(before, (metadata.dev(), metadata.ino(), metadata.mode()));
        }
    }
}

#[test]
fn directories_at_lock_paths_are_preserved_and_refused() {
    for signin in [false, true] {
        let (_root, store, path) = fixture(signin);
        std::fs::create_dir(&path).unwrap();
        let child = path.join("preserve");
        std::fs::write(&child, b"untouched").unwrap();
        assert!(attempt(&store, signin).is_err());
        assert!(path.is_dir());
        assert_eq!(std::fs::read(&child).unwrap(), b"untouched");
    }
}

#[cfg(unix)]
#[test]
fn lock_symlinks_neither_open_existing_targets_nor_create_missing_ones() {
    for signin in [false, true] {
        for existing in [false, true] {
            let (root, store, path) = fixture(signin);
            let target = root.path().join("outside-the-store");
            if existing {
                std::fs::write(&target, b"untouched").unwrap();
            }
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(attempt(&store, signin).is_err());
            assert_eq!(std::fs::read_link(&path).unwrap(), target);
            if existing {
                assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
            } else {
                assert!(
                    !target.exists(),
                    "a dangling lock must not create its target"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn lock_fifos_are_refused_with_or_without_a_reader() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};

    for signin in [false, true] {
        for with_reader in [false, true] {
            let (_root, store, path) = fixture(signin);
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: one NUL-terminated scratch path; mkfifo retains no pointer.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let _reader = with_reader.then(|| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&path)
                    .unwrap()
            });
            let (tx, rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                tx.send(attempt(&store, signin).map(|guard| guard.is_some()))
                    .unwrap();
            });
            let result = rx
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("lock open hung on a FIFO before the bounded lock wait");
            worker.join().unwrap();
            assert!(result.is_err(), "a FIFO must not become a lock: {result:?}");
            assert!(
                std::fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_fifo()
            );
        }
    }
}
