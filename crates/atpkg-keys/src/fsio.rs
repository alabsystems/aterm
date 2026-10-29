// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! File-I/O helpers for `atpkg-keys`.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-process counter that makes each staged temporary's name unique (see [`stage_sibling_temp`]).
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Maximum bytes read by [`read_bytes`].
pub const READ_CAP: u64 = 1024 * 1024;

/// Build a message by concatenation — deliberately not `format!`.
#[must_use]
pub fn concat(parts: &[&str]) -> String {
    let mut s = String::new();
    for p in parts {
        s.push_str(p);
    }
    s
}

/// Name a non-regular file's actual type for the [`read_bytes`] refusal message.
fn file_type_name(t: std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt as _;
    if t.is_dir() {
        "a directory"
    } else if t.is_char_device() {
        "a character device"
    } else if t.is_block_device() {
        "a block device"
    } else if t.is_fifo() {
        "a FIFO"
    } else if t.is_socket() {
        "a socket"
    } else {
        "not a regular file"
    }
}

/// Read a file's exact bytes, the one shared file-read site.
pub fn read_bytes(path: &str) -> std::io::Result<Vec<u8>> {
    read_bytes_up_to(path, READ_CAP)
}

/// [`read_bytes`] with a caller's bound, for a release's `SHA256SUMS`.
pub fn read_bytes_up_to(path: &str, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let f = std::fs::File::open(path)?;
    let ft = f.metadata()?.file_type();
    if !ft.is_file() {
        return Err(std::io::Error::other(concat(&[
            path,
            " is ",
            file_type_name(ft),
            ", not a regular file",
        ])));
    }
    let mut bytes = Vec::new();
    f.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        let mib = 1024 * 1024;
        let limit = if cap >= mib && cap.is_multiple_of(mib) {
            concat(&[&(cap / mib).to_string(), " MiB"])
        } else {
            concat(&[&cap.to_string(), "-byte"])
        };
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            concat(&[path, " exceeds the ", &limit, " read cap"]),
        ));
    }
    Ok(bytes)
}

/// Write `bytes` to `path`, the one plain-file-write site.
pub fn write_bytes(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::File::create(path)?.write_all(bytes)
}

/// Replace `path` atomically via a synced sibling temporary ([`stage_sibling_temp_with_mode`]).
pub fn write_bytes_atomic(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    write_bytes_atomic_with_sync(path, bytes, sync_parent).map_err(|e| e.error)
}

/// An atomic replacement's error and whether its rename already succeeded.
#[derive(Debug)]
pub(crate) struct AtomicWriteError {
    pub error: std::io::Error,
    pub renamed: bool,
}

/// The shared replacement with its parent-directory sync operation supplied by the caller.
pub(crate) fn write_bytes_atomic_with_sync(
    path: &str,
    bytes: &[u8],
    sync: impl FnOnce(&str) -> std::io::Result<()>,
) -> Result<(), AtomicWriteError> {
    let before_rename = |error| AtomicWriteError {
        error,
        renamed: false,
    };
    let tmp = stage_sibling_temp(path, bytes).map_err(before_rename)?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(before_rename(error));
    }
    sync(path).map_err(|error| AtomicWriteError {
        error,
        renamed: true,
    })
}

/// The staging half of [`write_bytes_atomic`].
fn stage_sibling_temp(path: &str, bytes: &[u8]) -> std::io::Result<String> {
    stage_sibling_temp_with_mode(path, bytes, 0o666)
}

/// [`stage_sibling_temp`] with an explicit creation mode ([`write_owner_file_create_new`]).
fn stage_sibling_temp_with_mode(path: &str, bytes: &[u8], mode: u32) -> std::io::Result<String> {
    use std::io::Write as _;
    let (tmp, mut f) = loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp = concat(&[
            path,
            ".atpkg-keys.",
            &std::process::id().to_string(),
            ".",
            &sequence.to_string(),
            ".tmp",
        ]);
        #[cfg(test)]
        window::reached("temp name drawn", &tmp);
        match std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
        {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    };
    let written = f.write_all(bytes).and_then(|()| aterm_durable::durable(&f));
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(tmp)
}

/// Install a new owner-only (`0600`) file atomically, never replacing an existing path.
pub fn write_owner_file_create_new(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = stage_sibling_temp_with_mode(path, bytes, 0o600)?;
    if let Err(e) = std::fs::hard_link(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = sync_parent(path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::remove_file(&tmp)?;
    sync_parent(path)
}

/// Flush the directory entry containing `path`.
pub fn sync_parent(path: &str) -> std::io::Result<()> {
    let parent = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    aterm_durable::durable(&std::fs::File::open(parent)?)
}

/// [`sync_parent`], only ORDERED ([`aterm_durable::ordered`]).
///
/// # Errors
/// The parent could not be opened, or the flush's own error.
pub fn sync_parent_ordered(path: &str) -> std::io::Result<()> {
    let parent = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    aterm_durable::ordered(&std::fs::File::open(parent)?)
}

/// Create every missing directory above `path`.
pub fn ensure_parent_dir(path: &str) -> std::io::Result<()> {
    let Some(parent) = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    let mut missing = Vec::new();
    let mut cursor = parent;
    while !cursor.as_os_str().is_empty() && std::fs::symlink_metadata(cursor).is_err() {
        missing.push(cursor.to_path_buf());
        let Some(next) = cursor.parent() else {
            break;
        };
        cursor = next;
    }
    std::fs::create_dir_all(parent)?;
    for directory in missing.iter().rev() {
        let directory = directory.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "new directory path is not UTF-8",
            )
        })?;
        sync_parent(directory)?;
    }
    Ok(())
}

/// The test seam for what another process does inside a window between a check and its
/// use: litter at the temporary name just drawn, a lock file swapped after it was opened, a
/// write that lands between ours and its read-back. A test arms one act for one named
/// window on its own thread ([`with`](window::with)); the code under test marks the window
/// with [`reached`](window::reached). Test builds only: a release build has no window.
#[cfg(test)]
pub(crate) mod window {
    use std::cell::RefCell;

    type Act = Box<dyn FnOnce(&str)>;

    thread_local! {
        static ARMED: RefCell<Option<(&'static str, Act)>> = const { RefCell::new(None) };
    }

    /// Run `f` with `act` armed for the window `name` on this thread. It fires once, with
    /// the path the window names. Panics if `f` never reached the window, so a test cannot
    /// pass without its act having run. Disarmed after, unwinding included.
    pub(crate) fn with<R>(
        name: &'static str,
        act: impl FnOnce(&str) + 'static,
        f: impl FnOnce() -> R,
    ) -> R {
        struct Disarm;
        impl Drop for Disarm {
            fn drop(&mut self) {
                ARMED.with(|armed| *armed.borrow_mut() = None);
            }
        }
        ARMED.with(|armed| *armed.borrow_mut() = Some((name, Box::new(act))));
        let disarm = Disarm;
        let out = f();
        let unreached = ARMED.with(|armed| armed.borrow().is_some());
        drop(disarm);
        assert!(!unreached, "the window `{name}` was never reached");
        out
    }

    /// The window `name`, at `path`: run the act armed for it, if any.
    pub(crate) fn reached(name: &'static str, path: &str) {
        let act = ARMED.with(|armed| {
            let mut armed = armed.borrow_mut();
            match armed.as_ref() {
                Some((window, _)) if *window == name => armed.take().map(|(_, act)| act),
                _ => None,
            }
        });
        if let Some(act) = act {
            act(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-keys-fsio-{name}-"))
    }

    /// The ordinary case: the file is replaced, and no temporary is left beside it. Both
    /// the staged bytes and the renamed entry are made durable.
    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_nothing_behind() {
        let dir = scratch("replace");
        let path = dir.join("pins.rs").to_str().unwrap().to_string();
        std::fs::write(&path, b"old").unwrap();

        let recording = aterm_durable::seam::Recording::start();
        write_bytes_atomic(&path, b"new content").expect("the write succeeds");
        assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
        assert_eq!(std::fs::read(&path).unwrap(), b"new content");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec!["pins.rs".to_string()],
            "no temporary survives"
        );

        let fresh = dir.join("new.rs").to_str().unwrap().to_string();
        write_bytes_atomic(&fresh, b"hello").unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"hello");
    }

    /// A parent obstructed before staging fails without changing the original, even as root.
    #[test]
    fn a_failed_atomic_write_leaves_the_original_intact() {
        let root = scratch("failed");
        let dir = root.join("anchor");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("pins.rs").to_str().unwrap().to_string();
        std::fs::write(&path, b"the trust anchor").unwrap();

        let saved = root.join("anchor-before-staging");
        let (move_from, move_to) = (dir.clone(), saved.clone());
        let result = window::with(
            "temp name drawn",
            move |_| {
                std::fs::rename(&move_from, &move_to).unwrap();
                std::fs::write(&move_from, b"a parent must be a directory").unwrap();
            },
            || write_bytes_atomic(&path, b"a replacement that cannot land"),
        );
        std::fs::remove_file(&dir).unwrap();
        std::fs::rename(&saved, &dir).unwrap();

        assert!(result.is_err(), "the write must fail");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"the trust anchor",
            "the original is untouched — not truncated, not partial"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["pins.rs".to_string()], "no litter");

        let squatted = dir.join("roster.toml");
        std::fs::create_dir(&squatted).unwrap();
        assert!(write_bytes_atomic(squatted.to_str().unwrap(), b"body").is_err());
        let mut leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        leftovers.sort();
        assert_eq!(leftovers, ["pins.rs", "roster.toml"], "no litter");
    }

    /// A read is bounded at the cap, not one byte short of it, and refuses a file that is
    /// not regular before reading a byte.
    #[test]
    fn a_read_is_bounded_at_its_cap_and_refuses_non_regular_files() {
        let dir = scratch("read-cap");
        let path = dir.join("doc").to_str().unwrap().to_string();
        std::fs::write(&path, b"12345678").unwrap();
        assert_eq!(read_bytes_up_to(&path, 8).unwrap(), b"12345678");
        std::fs::write(&path, b"123456789").unwrap();
        let err = read_bytes_up_to(&path, 8).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("8-byte read cap"), "{err}");
        assert_eq!(read_bytes_up_to(&path, u64::MAX).unwrap(), b"123456789");
        std::fs::write(&path, vec![b'x'; (READ_CAP as usize) + 1]).unwrap();
        let mib_err = read_bytes(&path).unwrap_err();
        assert!(mib_err.to_string().contains("1 MiB read cap"), "{mib_err}");
        assert!(read_bytes(dir.to_str().unwrap()).is_err(), "a directory");
    }

    /// Mutation check: with a fixed temporary name the write below fails with EEXIST.
    #[test]
    fn a_crash_left_temp_never_wedges_the_next_atomic_write() {
        let dir = scratch("stale-temp");
        let path = dir.join("roster.toml").to_str().unwrap().to_string();
        let stale = concat(&[&path, ".atpkg-keys.tmp"]);
        std::fs::write(&stale, b"bytes from a dead process").unwrap();

        write_bytes_atomic(&path, b"current").expect("a unique temp bypasses stale litter");
        assert_eq!(std::fs::read(&path).unwrap(), b"current");
        assert_eq!(std::fs::read(&stale).unwrap(), b"bytes from a dead process");

        let corpse = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let found = std::rc::Rc::clone(&corpse);
        window::with(
            "temp name drawn",
            move |tmp| {
                std::fs::write(tmp, b"a reused pid's corpse").unwrap();
                *found.borrow_mut() = tmp.to_string();
            },
            || write_bytes_atomic(&path, b"later").expect("the next name is free"),
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"later");
        assert_eq!(
            std::fs::read(corpse.borrow().as_str()).unwrap(),
            b"a reused pid's corpse"
        );
    }

    /// `create_new` + `write_all` would expose a zero-length key under its final name and
    /// `write_bytes_atomic` would clobber; link-after-fsync is whole, private and never replaces.
    #[test]
    fn an_owner_file_is_complete_private_and_never_replaced() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("owner-create-new");
        let path = dir.join("machine.toml").to_str().unwrap().to_string();

        let recording = aterm_durable::seam::Recording::start();
        write_owner_file_create_new(&path, b"first").unwrap();
        assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 3]);
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        assert!(write_owner_file_create_new(&path, b"second").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec!["machine.toml".to_string()],
            "the refused write leaves no staged temporary behind"
        );
    }

    #[test]
    fn ensure_parent_dir_creates_a_missing_chain_and_is_idempotent() {
        let dir = scratch("parents");
        let key = dir
            .join("home/.aterm/machine.key")
            .to_str()
            .unwrap()
            .to_string();
        assert!(!dir.join("home/.aterm").exists());
        let recording = aterm_durable::seam::Recording::start();
        ensure_parent_dir(&key).expect("creates the chain");
        assert!(dir.join("home/.aterm").is_dir());
        assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
        ensure_parent_dir(&key).expect("a second call is a no-op");
        assert_eq!(recording.take(), []);
        ensure_parent_dir("machine.key").expect("no parent, no problem");
        sync_parent("machine.key").expect("a bare name syncs the current directory");
    }
}
