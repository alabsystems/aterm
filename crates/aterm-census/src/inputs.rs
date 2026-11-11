// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Content dependencies of the deterministic census readers. A package is one input:
//! reading any of its files binds its complete tree, including additions and symlinks.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// `(operation, root-relative path, SHA-256)`; these are data, never filesystem capabilities.
pub type Input = (String, String, String);

struct Capture {
    root: PathBuf,
    found: Mutex<Found>,
}

/// What a capture's readers asked: each input's fingerprint as first seen, and whether every
/// read stayed inside the root.
struct Found {
    inputs: BTreeMap<(String, String), Option<String>>,
    bounded: bool,
}

impl Capture {
    fn unbounded(&self) {
        self.found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bounded = false;
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Arc<Capture>>> = const { RefCell::new(None) };
}

/// Puts `capture` on this thread until the value is dropped, then the capture it replaced.
struct Reset(Option<Arc<Capture>>);

impl Reset {
    fn install(capture: Option<Arc<Capture>>) -> Self {
        Self(ACTIVE.with(|active| active.replace(capture)))
    }
}

impl Drop for Reset {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = self.0.take());
    }
}

/// The capture this thread records into, for a thread it starts to record into too
/// ([`crate::memo::threads`]).
pub(crate) struct Carried(Option<Arc<Capture>>);

/// This thread's capture, if one is active.
pub(crate) fn carried() -> Carried {
    Carried(ACTIVE.with(|active| active.borrow().clone()))
}

impl Carried {
    /// Runs `f` with this capture active on the calling thread.
    pub(crate) fn enter<T>(self, f: impl FnOnce() -> T) -> T {
        let _reset = Reset::install(self.0);
        f()
    }
}

/// Run one census against a content snapshot and recheck every input before returning
/// evidence. The census's own threads, which it starts through [`crate::memo::threads`], record
/// into the same capture; no other thread does. An out-of-root read or failed snapshot leaves the
/// result usable only for this run, never for reuse.
pub fn capture<T>(root: &Path, run: impl FnOnce() -> T) -> (T, Option<Vec<Input>>) {
    let active = Arc::new(Capture {
        root: root.to_path_buf(),
        found: Mutex::new(Found {
            inputs: BTreeMap::new(),
            bounded: true,
        }),
    });
    let reset = Reset::install(Some(Arc::clone(&active)));
    let result = run();
    drop(reset);
    let found = std::mem::replace(
        &mut *active.found.lock().unwrap_or_else(PoisonError::into_inner),
        Found {
            inputs: BTreeMap::new(),
            bounded: false,
        },
    );
    let mut inputs = Vec::new();
    if !found.bounded {
        return (result, None);
    }
    for ((kind, path), digest) in found.inputs {
        let Some(digest) = digest else {
            return (result, None);
        };
        if fingerprint(root, &kind, &path).as_deref() != Some(&digest) {
            return (result, None);
        }
        inputs.push((kind, path, digest));
    }
    (result, Some(inputs))
}

/// Whether the same operations still see the same complete package inputs.
pub fn matches(root: &Path, inputs: &[Input]) -> bool {
    !inputs.is_empty()
        && inputs.iter().all(|(kind, path, digest)| {
            fingerprint(root, kind, path).as_deref() == Some(digest.as_str())
        })
}

/// Capture one explicit filesystem input with the same validation used by census snapshots.
pub fn file(root: &Path, relative: &str) -> Option<Input> {
    fingerprint(root, "file", relative).map(|digest| ("file".into(), relative.into(), digest))
}

/// A complete package tree, including non-Rust inputs, entry types and additions.
pub fn package(root: &Path, relative: &str) -> Option<Input> {
    fingerprint(root, "tree", relative).map(|digest| ("tree".into(), relative.into(), digest))
}

pub(crate) fn before(path: &Path, operation: &str) {
    let Some(capture) = ACTIVE.with(|active| active.borrow().clone()) else {
        return;
    };
    let Ok(relative) = path.strip_prefix(&capture.root) else {
        capture.unbounded();
        return;
    };
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        capture.unbounded();
        return;
    }
    let mut prefix = capture.root.clone();
    for component in relative.components() {
        prefix.push(component);
        if prefix
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            capture.unbounded();
            return;
        }
    }
    if path.canonicalize().is_ok_and(|p| {
        capture
            .root
            .canonicalize()
            .is_ok_and(|root| p != root.join(relative))
    }) {
        capture.unbounded();
        return;
    }
    let Some(relative) = relative.to_str() else {
        capture.unbounded();
        return;
    };
    let parts: Vec<_> = relative.split('/').collect();
    let package = match parts.as_slice() {
        ["crates" | "apps", name, ..] if !name.is_empty() => Some(parts[..2].join("/")),
        ["vendor", _, "crates", _, ..] => Some(parts[..4].join("/")),
        _ => None,
    };
    let key = package.map_or_else(
        || (operation.to_owned(), relative.to_owned()),
        |p| ("tree".into(), p),
    );
    if capture
        .found
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .inputs
        .contains_key(&key)
    {
        return;
    }
    // Fingerprinted without the lock, so the census's other threads read on meanwhile; two
    // threads that race here store the first, and `capture` rechecks it before it is evidence.
    let digest = fingerprint(&capture.root, &key.0, &key.1);
    capture
        .found
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .inputs
        .entry(key)
        .or_insert(digest);
}

fn part(hash: &mut aterm_digest::Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn fingerprint(root: &Path, kind: &str, relative: &str) -> Option<String> {
    let rel = Path::new(relative);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let path = root.join(rel);
    let mut hash = aterm_digest::Sha256::new();
    part(&mut hash, kind.as_bytes());
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            part(&mut hash, b"missing");
            return Some(hex(&hash.finalize()));
        }
        Err(_) => return None,
    };
    if kind == "tree" {
        tree(&mut hash, &path, &path, &mut (512 * 1024 * 1024))?;
    } else if kind == "dir" {
        for entry in std::fs::read_dir(&path).ok()? {
            let entry = entry.ok()?;
            part(&mut hash, entry.file_name().as_encoded_bytes());
            let is_dir = entry.file_type().ok()?.is_dir() || entry.path().is_dir();
            part(&mut hash, &[u8::from(is_dir)]);
        }
    } else if kind == "file" {
        if meta.is_file() {
            if meta.len() > crate::tree::MAX_FILE_BYTES {
                return None;
            }
            part(&mut hash, &read(&path)?);
        } else {
            return None;
        }
    } else {
        let value = match kind {
            "is-file" => path.is_file(),
            "is-dir" => path.is_dir(),
            "exists" => path.exists(),
            _ => return None,
        };
        part(&mut hash, &[u8::from(value)]);
    }
    Some(hex(&hash.finalize()))
}

fn tree(hash: &mut aterm_digest::Sha256, root: &Path, path: &Path, budget: &mut u64) -> Option<()> {
    part(
        hash,
        path.strip_prefix(root).ok()?.as_os_str().as_encoded_bytes(),
    );
    let meta = path.symlink_metadata().ok()?;
    if meta.file_type().is_symlink() {
        part(hash, b"link");
        part(
            hash,
            std::fs::read_link(path)
                .ok()?
                .as_os_str()
                .as_encoded_bytes(),
        );
    } else if meta.is_dir() {
        part(hash, b"directory");
        let mut children = std::fs::read_dir(path)
            .ok()?
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            tree(hash, root, &child.path(), budget)?;
        }
    } else if meta.is_file() {
        if meta.len() > crate::tree::MAX_FILE_BYTES {
            return None;
        }
        let bytes = read(path)?;
        *budget = budget.checked_sub(bytes.len() as u64)?;
        part(hash, b"file");
        part(hash, &bytes);
    } else {
        return None;
    }
    Some(())
}

fn read(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let (file, size) = crate::tree::open_regular_unrecorded(path).ok()?;
    let mut bytes = Vec::with_capacity(size.saturating_add(1));
    file.take(crate::tree::MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= crate::tree::MAX_FILE_BYTES).then_some(bytes)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                HEX[(b >> 4) as usize] as char,
                HEX[(b & 15) as usize] as char,
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn followed_links_cannot_hide_inputs_outside_the_package() {
        let root = aterm_tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("crates/a")).unwrap();
        let target = root.path().join("shared.rs");
        std::fs::write(&target, "shared").unwrap();
        let link = root.path().join("crates/a/link.rs");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (_, proof) = capture(root.path(), || crate::tree::read_to_string(&link).unwrap());
        assert!(proof.is_none());
        let (_, proof) = capture(root.path(), || {
            crate::tree::read_to_string(&target).unwrap()
        });
        assert!(proof.is_some());
        std::fs::remove_file(&target).unwrap();
        let (_, proof) = capture(root.path(), || crate::tree::exists(&link));
        assert!(
            proof.is_none(),
            "a dangling link can acquire an external target"
        );
    }

    /// A read on a thread the census started is one of its inputs: the lazy-init, lock-order and
    /// main-loop censuses read on worker threads.
    /// NEGATIVE CONTROL: a thread started outside `memo::threads` records nothing, so its
    /// package is missing from the evidence.
    #[test]
    fn a_census_threads_reads_are_its_inputs() {
        let root = aterm_tempfile::tempdir().unwrap();
        let file = root.path().join("crates/a/src/lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "one").unwrap();
        let packages = |inputs: Option<Vec<Input>>| -> Vec<String> {
            inputs
                .expect("bounded")
                .into_iter()
                .map(|(kind, path, _)| format!("{kind} {path}"))
                .collect()
        };
        let (_, carried) = capture(root.path(), || {
            crate::memo::threads(|s| s.spawn(|| crate::tree::read_to_string(&file)).join())
                .unwrap()
                .unwrap()
        });
        for own_worker in [false, true] {
            let read = || {
                crate::memo::threads(|scope| {
                    scope.spawn(|| crate::tree::read_to_string(&file)).join()
                })
                .unwrap()
                .unwrap()
            };
            let (text, serial) = capture(root.path(), || {
                if own_worker {
                    crate::on_one_worker(read).unwrap()
                } else {
                    crate::without_parallel_workers(read)
                }
            });
            assert_eq!(text, "one");
            assert_eq!(
                serial, carried,
                "serial work retains identical input evidence"
            );
        }
        assert_eq!(packages(carried), ["tree crates/a"]);
        let (_, raw) = capture(root.path(), || {
            std::thread::scope(|s| s.spawn(|| crate::tree::read_to_string(&file)).join())
                .unwrap()
                .unwrap()
        });
        assert!(packages(raw).is_empty());
    }

    #[test]
    fn package_changes_additions_and_mid_read_drift_invalidate_evidence() {
        let root = aterm_tempfile::tempdir().unwrap();
        let file = root.path().join("crates/a/src/lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "one").unwrap();
        let (_, proof) = capture(root.path(), || crate::tree::read_to_string(&file).unwrap());
        let proof = proof.unwrap();
        assert!(matches(root.path(), &proof));
        std::fs::create_dir_all(root.path().join("crates/b")).unwrap();
        std::fs::write(root.path().join("crates/b/unrelated.rs"), "unrelated").unwrap();
        assert!(
            matches(root.path(), &proof),
            "another package is not this reader's input"
        );
        std::fs::write(file.parent().unwrap().join("extra.rs"), "new").unwrap();
        assert!(!matches(root.path(), &proof));
        let (_, changed) = capture(root.path(), || {
            crate::tree::read_to_string(&file).unwrap();
            std::fs::write(&file, "changed during analysis").unwrap();
        });
        assert!(changed.is_none());
    }
}
