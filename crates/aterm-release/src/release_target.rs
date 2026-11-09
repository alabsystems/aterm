// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retirement of a release clone's legacy in-tree build directory and links.

use std::path::Path;

/// Stages the legacy physical `target.noindex`, removes its two legacy links and starts a
/// background drain. A later call resumes any aside an interrupted drain left.
///
/// The return is the number of entries left in `target.noindex/debug/deps`. A successful
/// retirement therefore returns zero. A plain `target`, a `target` link with another target,
/// and a plain `target-gate` are not this migration's entries and stay untouched.
///
/// # Errors
///
/// The build cache cannot be safely removed, its state cannot be read, an admitted legacy link
/// cannot be read or removed, `target.noindex` still stands after staging, or the drain
/// worker cannot be started.
pub(crate) fn retire(clone: &Path) -> Result<usize, String> {
    retire_using(
        clone,
        atpkg::build_cache::retirement::stage,
        &mut background,
    )
}

fn retire_using(
    clone: &Path,
    stage: impl FnOnce(&Path) -> std::io::Result<Vec<std::path::PathBuf>>,
    drain: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<usize, String> {
    let physical = clone.join("target.noindex");
    let pending = stage(&physical).map_err(|error| {
        format!(
            "could not safely retire {}: {error}; {}",
            physical.display(),
            remaining_words(&physical)
        )
    })?;
    if stands(&physical)? {
        return Err(format!(
            "could not safely retire {}; {}",
            physical.display(),
            remaining_words(&physical)
        ));
    }

    remove_link(&clone.join("target"), Some("target.noindex"))?;
    remove_link(&clone.join("target-gate"), None)?;

    if !pending.is_empty() {
        drain(&physical)?;
    }

    remaining_deps_entries(&physical)
}

const DRAIN: &str = "--finish-target-retirement";

/// A separate cutter image survives the release's exec or exit. It owns no release lease,
/// uses no inherited terminal streams and re-certifies each aside before deleting it.
fn background(physical: &Path) -> Result<(), String> {
    let image = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = aterm_uds::spawnfd::command(image);
    command
        .arg(DRAIN)
        .arg(physical)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let mut child = command
        .spawn_in_dedicated_process_group()
        .map_err(|error| error.to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Private worker entry, handled before release initialization or resource adoption.
pub(crate) fn finish_if_asked(args: &[std::ffi::OsString]) -> Option<std::process::ExitCode> {
    if args.first().is_none_or(|arg| arg != DRAIN) {
        return None;
    }
    let result = (|| {
        if args.len() != 2 {
            return Err(std::io::Error::other("retirement requires one target path"));
        }
        #[cfg(unix)]
        {
            // SAFETY: this changes only this worker's scheduling priority; no pointers.
            if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 19) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        atpkg::build_cache::retirement::finish(Path::new(&args[1]))
    })();
    Some(match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("target retirement drain: {error}");
            std::process::ExitCode::from(2)
        }
    })
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
    use std::os::unix::fs::symlink;

    fn retire_with(
        clone: &Path,
        drain: &mut dyn FnMut(&Path) -> Result<(), String>,
    ) -> Result<usize, String> {
        super::retire_using(
            clone,
            |physical| {
                atpkg::build_cache::retirement::stage_with_processes(physical, &|_, _| {
                    Ok(Default::default())
                })
            },
            drain,
        )
    }

    fn legacy(clone: &Path, target: &str) {
        let deps = clone.join("target.noindex/debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::write(deps.join("one"), b"one").unwrap();
        std::fs::write(deps.join("two"), b"two").unwrap();
        std::fs::write(clone.join("target.noindex/debug/.cargo-lock"), b"").unwrap();
        symlink(target, clone.join("target")).unwrap();
        symlink("retired-gate.noindex", clone.join("target-gate")).unwrap();
    }

    fn retire(clone: &Path) -> Result<usize, String> {
        retire_with(clone, &mut |physical| {
            atpkg::build_cache::retirement::finish_with_processes(physical, &|_, _| {
                Ok(Default::default())
            })
            .map_err(|e| e.to_string())
        })
    }

    /// NEGATIVE CONTROL: synchronous removal would erase the payload before the drain is
    /// scheduled. A second call must schedule the interrupted aside with the original absent.
    #[test]
    fn links_retire_before_bulk_deletion_and_a_later_call_resumes() {
        let clone = aterm_tempfile::tempdir().unwrap();
        legacy(clone.path(), "target.noindex");
        let mut calls = 0;
        let mut deferred = |_: &Path| {
            calls += 1;
            Ok(())
        };
        assert_eq!(retire_with(clone.path(), &mut deferred), Ok(0));
        let aside = clone.path().join(".ubd/target.noindex.removing");
        assert!(aside.join("debug/deps/one").is_file());
        assert!(!stands(&clone.path().join("target")).unwrap());
        assert!(!stands(&clone.path().join("target-gate")).unwrap());
        assert_eq!(retire_with(clone.path(), &mut deferred), Ok(0));
        assert_eq!(calls, 2);
        assert_eq!(retire(clone.path()), Ok(0));
        assert!(!aside.exists());
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
        // The suite forks children in parallel. Closing this descriptor alone may leave a
        // child's copy holding the lock on kernels without effective close-on-fork. The
        // standard guard explicitly unlocks before closing, including while unwinding.
        let lock = aterm_uds::file_lock::LockFile::take_existing(
            &lock_path,
            std::time::Instant::now(),
            "the fixture's Cargo lock was unexpectedly held",
        )
        .unwrap();

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
