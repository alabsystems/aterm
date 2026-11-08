// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retirement of a release clone's legacy in-tree build directory and links.

use std::path::Path;

/// Retires the legacy physical `target.noindex`, then its two legacy links.
///
/// The return is the number of entries left in `target.noindex/debug/deps`. A successful
/// retirement therefore returns zero. A plain `target`, a `target` link with another target,
/// and a plain `target-gate` are not this migration's entries and stay untouched.
///
/// # Errors
///
/// The build cache cannot be safely removed, its state cannot be read, an admitted legacy link
/// cannot be read or removed, or `target.noindex` still stands after the removal attempt.
pub(crate) fn retire(clone: &Path) -> Result<usize, String> {
    let physical = clone.join("target.noindex");
    let stood = stands(&physical)?;
    let removed = atpkg::build_cache::remove_unbuilt(&physical).map_err(|error| {
        format!(
            "could not safely retire {}: {error}; {}",
            physical.display(),
            remaining_words(&physical)
        )
    })?;
    if stands(&physical)? || (stood && !removed) {
        return Err(format!(
            "could not safely retire {}; {}",
            physical.display(),
            remaining_words(&physical)
        ));
    }

    remove_link(&clone.join("target"), Some("target.noindex"))?;
    remove_link(&clone.join("target-gate"), None)?;

    remaining_deps_entries(&physical)
}

/// Whether `path` has any entry of its own, without following a link.
fn stands(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("could not inspect {}: {error}", path.display())),
    }
}

/// Removes `path` only when it is a symlink, and, where `target` is given, names exactly that
/// relative target with an optional leading `./`.
fn remove_link(path: &Path, target: Option<&str>) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not inspect {}: {error}", path.display())),
    };
    if !metadata.file_type().is_symlink() {
        return Ok(());
    }
    if let Some(target) = target {
        let actual = std::fs::read_link(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if actual != Path::new(target) && actual != Path::new(&format!("./{target}")) {
            return Ok(());
        }
    }
    std::fs::remove_file(path)
        .map_err(|error| format!("could not remove legacy link {}: {error}", path.display()))
}

/// The release clone's measured legacy population.
fn remaining_deps_entries(physical: &Path) -> Result<usize, String> {
    let deps = physical.join("debug/deps");
    let mut entries = match std::fs::read_dir(&deps) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(format!(
                "could not count entries in {}: {error}",
                deps.display()
            ));
        }
    };
    entries
        .try_fold(0usize, |count, entry| {
            entry.map(|_| count.saturating_add(1))
        })
        .map_err(|error| format!("could not count entries in {}: {error}", deps.display()))
}

fn remaining_words(physical: &Path) -> String {
    match remaining_deps_entries(physical) {
        Ok(entries) => format!("{entries} deps entries remain"),
        Err(error) => format!("remaining deps entry count is unknown: {error}"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::unix::fs::symlink;

    fn legacy(clone: &Path, target: &str) {
        let deps = clone.join("target.noindex/debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::write(deps.join("one"), b"one").unwrap();
        std::fs::write(deps.join("two"), b"two").unwrap();
        std::fs::write(clone.join("target.noindex/debug/.cargo-lock"), b"").unwrap();
        symlink(target, clone.join("target")).unwrap();
        symlink("retired-gate.noindex", clone.join("target-gate")).unwrap();
    }

    /// The physical cache goes first, both admitted spellings of its link go, and the retired
    /// gate link goes. Mutating the pass to unlink first strands the measured cache population.
    #[test]
    fn legacy_physical_target_and_links_retire_with_no_deps_entries() {
        for target in ["target.noindex", "./target.noindex"] {
            let clone = aterm_tempfile::tempdir().unwrap();
            legacy(clone.path(), target);

            assert_eq!(retire(clone.path()), Ok(0), "link spelling {target}");
            for name in ["target.noindex", "target", "target-gate"] {
                assert!(
                    std::fs::symlink_metadata(clone.path().join(name)).is_err(),
                    "{name} retired for link spelling {target}"
                );
            }
        }
    }

    /// NEGATIVE CONTROL: a held Cargo lock refuses physical retirement, and neither link is
    /// removed early. Once released, the same state retires in one pass.
    #[test]
    fn a_held_build_refuses_retirement_before_either_link_is_removed() {
        let clone = aterm_tempfile::tempdir().unwrap();
        legacy(clone.path(), "target.noindex");
        let lock_path = clone.path().join("target.noindex/debug/.cargo-lock");
        let lock = File::options()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        // The full release suite starts children in parallel. The fixture's lock belongs to
        // this test alone: a child must not inherit it and keep the negative-control state alive
        // after this image drops the guard.
        aterm_uds::keep_out_of_children(&lock).unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the negative control deliberately holds Cargo's lock until this fixture drops its file"
        )]
        lock.try_lock().unwrap();

        let said = retire(clone.path()).expect_err("the live build must refuse retirement");
        assert!(said.contains("could not safely retire"), "{said}");
        assert!(said.contains("2 deps entries remain"), "{said}");
        assert!(clone.path().join("target.noindex/debug/deps/one").is_file());
        assert_eq!(
            std::fs::read_link(clone.path().join("target")).unwrap(),
            Path::new("target.noindex")
        );
        assert!(
            std::fs::symlink_metadata(clone.path().join("target-gate"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        drop(lock);
        assert_eq!(retire(clone.path()), Ok(0));
    }

    /// NEGATIVE CONTROL: only the two legacy link shapes belong to this pass. Plain entries and
    /// a `target` symlink to somewhere else remain byte-for-byte available.
    #[test]
    fn plain_entries_and_an_unrelated_target_link_are_never_touched() {
        let plain = aterm_tempfile::tempdir().unwrap();
        for name in ["target", "target-gate"] {
            let dir = plain.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("keep"), name).unwrap();
        }
        assert_eq!(retire(plain.path()), Ok(0));
        for name in ["target", "target-gate"] {
            assert_eq!(
                std::fs::read_to_string(plain.path().join(name).join("keep")).unwrap(),
                name
            );
        }

        let unrelated = aterm_tempfile::tempdir().unwrap();
        symlink("other.noindex", unrelated.path().join("target")).unwrap();
        std::fs::write(unrelated.path().join("target-gate"), b"plain gate").unwrap();
        assert_eq!(retire(unrelated.path()), Ok(0));
        assert_eq!(
            std::fs::read_link(unrelated.path().join("target")).unwrap(),
            Path::new("other.noindex")
        );
        assert_eq!(
            std::fs::read(unrelated.path().join("target-gate")).unwrap(),
            b"plain gate"
        );
    }
}
