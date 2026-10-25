// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Run numbers survive the progress record whose recovery retires it. Every
//! publication of progress reserves its number first, under the profile lock;
//! previews only read this floor, and an owned retry keeps its reserved number.
//!
//! A counter that does not parse is rebuilt, never fatal ([`rebuilt`]): reads take the rebuilt
//! floor, and the next reservation keeps the damaged bytes beside the record and writes it
//! again. One a newer aterm wrote is [`NEWER_SETUP`].

use super::*;

const RECORD: &str = "run-revision.json";

/// Legacy aterm allocated from one. A legacy recovery whose only run number
/// was lost enters a new era rather than reusing those approvals and caches.
/// Manually authored legacy upper-half run numbers are outside that migration.
const LEGACY_RECOVERY: u64 = 1 << 63;

/// The counter as it stands.
enum Counter {
    Missing,
    Kept(u64),
    /// It does not parse: the floor rebuilt from the records ([`rebuilt`]).
    Damaged(u64),
}

fn counter(root: &Path) -> Result<Counter, String> {
    let path = root.join(RECORD);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Counter::Missing),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let Some(value) = aterm_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(|value| value["profile"].as_str() == Some(PROFILE))
    else {
        return Ok(Counter::Damaged(rebuilt(root)));
    };
    match known_version(&value["version"], "") {
        Err(why) if setup_is_newer(&why) => return Err(why),
        Err(_) => return Ok(Counter::Damaged(rebuilt(root))),
        Ok(()) => {}
    }
    Ok(value["revision"]
        .as_u64()
        .map_or_else(|| Counter::Damaged(rebuilt(root)), Counter::Kept))
}

fn read(root: &Path) -> Result<Option<u64>, String> {
    Ok(match counter(root)? {
        Counter::Missing => None,
        Counter::Kept(revision) | Counter::Damaged(revision) => Some(revision),
    })
}

/// The floor a counter that does not parse is rebuilt to: the highest run number any record in
/// `root` still names (a run's progress and the run it continues, a Stop asked for, a Restore's
/// run, a set-aside's checkpoint, an upgrade's or put-back's checkpoints) or the window's record
/// of access steps beside it names, in the recovery era ([`LEGACY_RECOVERY`]) at least, as a
/// recovery whose counter was lost enters it: no number a lost reservation took is used again.
fn rebuilt(root: &Path) -> u64 {
    fn named(value: &Value) -> u64 {
        match value {
            Value::Object(fields) => fields
                .iter()
                .map(|(key, field)| match (key.as_str(), field.as_u64()) {
                    ("revision" | "run" | "access_revision" | "continues_revision", Some(run)) => {
                        run
                    }
                    _ => named(field),
                })
                .max()
                .unwrap_or(0),
            Value::Array(items) => items.iter().map(named).max().unwrap_or(0),
            _ => 0,
        }
    }
    let records = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| std::fs::read(path).ok())
        .filter_map(|bytes| aterm_json::from_slice::<Value>(&bytes).ok())
        .map(|value| named(&value));
    let access = root
        .parent()
        .and_then(|dir| {
            std::fs::read_to_string(dir.join(aterm_types::macos_setup::access_record::FILE)).ok()
        })
        .into_iter()
        .flat_map(|text| {
            text.lines()
                .filter_map(|line| line.split_whitespace().next()?.parse::<u64>().ok())
                .collect::<Vec<_>>()
        });
    records
        .chain(access)
        .fold(LEGACY_RECOVERY, std::cmp::Ord::max)
}

/// The last reserved number; reading never allocates or creates state.
pub(super) fn floor(root: &Path) -> Result<u64, String> {
    Ok(read(root)?.unwrap_or(0))
}

/// Preview the next run after both a readable predecessor and the durable floor.
pub(super) fn next(root: &Path, observed: u64) -> Result<u64, String> {
    next_revision(floor(root)?.max(observed))
}

/// Whether an embedded run with a gap after its predecessor was reserved.
pub(super) fn reserved(root: &Path, revision: u64) -> Result<bool, String> {
    Ok(read(root)?.is_some_and(|floor| floor >= revision))
}

/// Reserve before publishing progress, including a checkpoint embedded in a
/// plan. A retry or put-back may still publish an earlier owned run: the floor
/// prevents reuse by a new admission and grants no authority to write one. A counter that does
/// not parse is kept beside itself first ([`set_aside`]) and written again, never below its
/// rebuilt floor.
pub(super) fn reserve(root: &Path, revision: u64) -> Result<(), String> {
    let revision = match counter(root)? {
        Counter::Kept(current) if current >= revision => {
            return flush_record_with(&root.join(RECORD), durable_name);
        }
        Counter::Damaged(rebuilt) => {
            set_aside::keep_aside(root, RECORD)?;
            revision.max(rebuilt)
        }
        Counter::Missing | Counter::Kept(_) => revision,
    };
    write_json(
        &root.join(RECORD),
        &json!({"profile": PROFILE, "version": 1, "revision": revision}),
    )
}

/// Under profile.lock: a counter that does not parse is kept beside itself and written again at
/// its rebuilt floor; whether it was.
pub(super) fn repair(root: &Path) -> Result<bool, String> {
    if !damaged(root)? {
        return Ok(false);
    }
    reserve(root, 0)?;
    Ok(true)
}

/// Whether the counter is there and does not parse.
pub(super) fn damaged(root: &Path) -> Result<bool, String> {
    Ok(matches!(counter(root)?, Counter::Damaged(_)))
}

/// Preserve the allocation namespace before a damaged setup loses progress.
/// An existing counter is never reset; only legacy recovery enters a new era.
pub(super) fn recover(root: &Path, previous: Option<u64>) -> Result<(), String> {
    let floor = read(root)?.unwrap_or(LEGACY_RECOVERY);
    reserve(root, floor.max(previous.unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_do_not_reserve_and_legacy_recovery_enters_an_era_once() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        assert_eq!(next(root, 8).unwrap(), 9);
        assert!(!root.join(RECORD).exists());
        recover(root, Some(8)).unwrap();
        assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 1);
        reserve(root, LEGACY_RECOVERY + 1).unwrap();
        recover(root, None).unwrap();
        assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 2);

        let existing = aterm_tempfile::tempdir().unwrap();
        reserve(existing.path(), 8).unwrap();
        recover(existing.path(), Some(12)).unwrap();
        assert_eq!(next(existing.path(), 0).unwrap(), 13);

        let readable = aterm_tempfile::tempdir().unwrap();
        recover(readable.path(), Some(LEGACY_RECOVERY + 9)).unwrap();
        assert_eq!(next(readable.path(), 0).unwrap(), LEGACY_RECOVERY + 10);
    }

    #[test]
    fn a_failed_progress_publication_keeps_its_reservation_and_owned_retry_target() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let _lock = HeldLock::take(root).unwrap();
        let plan = Plan::from(Selection::none());
        let proposed = Progress {
            revision: next(root, 0).unwrap(),
            ..Progress::for_plan(&plan)
        };
        let path = root.join("initial-setup.json");
        std::fs::create_dir(&path).unwrap();
        assert!(save_progress(root, proposed).is_err());
        assert_eq!(next(root, 0).unwrap(), proposed.revision + 1);
        std::fs::remove_dir(&path).unwrap();
        save_progress(root, proposed).unwrap();
        assert_eq!(progress_file(root).unwrap(), Some(proposed));
        assert_eq!(next(root, 0).unwrap(), proposed.revision + 1);
    }

    #[test]
    fn exhausted_allocations_never_restart_from_zero() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        reserve(root, u64::MAX).unwrap();
        assert!(next(root, 0).is_err());
        assert!(next(root, u64::MAX).is_err());
        assert!(reserved(root, u64::MAX).unwrap());
    }

    /// A counter that does not parse is rebuilt past every run number a record names, in the
    /// recovery era, and the next reservation keeps it beside itself and writes it again: never
    /// a wedge, never a number used again. One a newer aterm wrote is left as it is.
    #[test]
    fn a_damaged_run_counter_is_rebuilt_past_every_named_run_and_kept_aside() {
        for bytes in [
            &b"not json"[..],
            br#"{"profile":"other"}"#,
            br#"{"version":1}"#,
        ] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let doctor = scratch.path().join("doctor");
            std::fs::create_dir(&doctor).unwrap();
            let root = doctor.as_path();
            std::fs::write(root.join(RECORD), bytes).unwrap();
            assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 1, "an empty Mac");
            write_json(
                &root.join(STOP_REQUEST),
                &json!({"profile": PROFILE, "revision": LEGACY_RECOVERY + 7}),
            )
            .unwrap();
            std::fs::write(
                scratch
                    .path()
                    .join(aterm_types::macos_setup::access_record::FILE),
                format!("{} full_disk_access granted\n", LEGACY_RECOVERY + 9),
            )
            .unwrap();
            assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 10);
            assert!(damaged(root).unwrap());
            reserve(root, 3).unwrap();
            assert_eq!(
                std::fs::read(root.join("run-revision.json.unreadable")).unwrap(),
                bytes
            );
            assert!(!damaged(root).unwrap());
            assert_eq!(next(root, 0).unwrap(), LEGACY_RECOVERY + 10);
        }
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let newer =
            aterm_json::to_vec(&json!({"profile": PROFILE, "version": 2, "revision": 4})).unwrap();
        std::fs::write(root.join(RECORD), &newer).unwrap();
        assert!(next(root, 0).is_err_and(|why| setup_is_newer(&why)));
        assert!(reserve(root, 5).is_err());
        assert!(damaged(root).is_err());
        assert_eq!(std::fs::read(root.join(RECORD)).unwrap(), newer);
    }

    #[test]
    fn an_upgrade_bridge_keeps_its_reserved_target_across_a_gap() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let _lock = HeldLock::take(root).unwrap();
        let mut selected = Selection::none();
        selected.set(Setting::Permissions, true);
        let previous = Plan::from(selected);
        let prior = Progress {
            revision: 1,
            admitted: true,
            ..Progress::for_plan(&previous)
        };
        save_plan_as(root, &previous, true).unwrap();
        save_progress(root, prior).unwrap();
        reserve(root, 9).unwrap();
        let full = Plan::default();
        let pending = Progress {
            revision: next(root, prior.revision).unwrap(),
            ..Progress::for_plan(&full)
        };
        scope::UpgradeProgress::new(Some(previous), Some(prior), pending)
            .save(root, &full)
            .unwrap();
        assert_eq!(progress(root).unwrap(), Some(pending));
        let reserved = std::fs::read(root.join(RECORD)).unwrap();
        assert_eq!(next(root, prior.revision).unwrap(), pending.revision + 1);
        assert_eq!(std::fs::read(root.join(RECORD)).unwrap(), reserved);

        std::fs::remove_file(root.join(RECORD)).unwrap();
        assert!(progress(root).is_err(), "a gap needs its reservation");
        reserve(root, pending.revision).unwrap();
        assert_eq!(progress(root).unwrap(), Some(pending));
        save_progress(root, pending).unwrap();
        assert_eq!(progress_file(root).unwrap(), Some(pending));
        assert!(!pending.admitted);
    }
}
