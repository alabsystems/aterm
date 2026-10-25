// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Run numbers survive the progress record whose recovery retires it. Every
//! publication of progress reserves its number first, under the profile lock;
//! previews only read this floor, and an owned retry keeps its reserved number.

use super::*;

const RECORD: &str = "run-revision.json";

/// Legacy aterm allocated from one. A legacy recovery whose only run number
/// was lost enters a new era rather than reusing those approvals and caches.
/// Manually authored legacy upper-half run numbers are outside that migration.
const LEGACY_RECOVERY: u64 = 1 << 63;

fn read(root: &Path) -> Result<Option<u64>, String> {
    let path = root.join(RECORD);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value = aterm_json::from_slice(&bytes)
        .map_err(|error| format!("saved setup run number: {error}"))?;
    if value["profile"].as_str() != Some(PROFILE) || value["version"].as_u64() != Some(1) {
        return Err("unrecognized saved setup run number".into());
    }
    value["revision"]
        .as_u64()
        .map(Some)
        .ok_or_else(|| "invalid saved setup run number".into())
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
/// prevents reuse by a new admission and grants no authority to write one.
pub(super) fn reserve(root: &Path, revision: u64) -> Result<(), String> {
    match read(root)? {
        Some(current) if current >= revision => flush_record_with(&root.join(RECORD), durable_name),
        _ => write_json(
            &root.join(RECORD),
            &json!({"profile": PROFILE, "version": 1, "revision": revision}),
        ),
    }
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
    fn malformed_or_exhausted_allocations_never_restart_from_zero() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let path = root.join(RECORD);
        std::fs::write(&path, b"not json").unwrap();
        assert!(next(root, 0).is_err());
        assert!(reserve(root, 1).is_err());
        assert!(recover(root, None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
        std::fs::remove_file(&path).unwrap();
        reserve(root, u64::MAX).unwrap();
        assert!(next(root, 0).is_err());
        assert!(next(root, u64::MAX).is_err());
        assert!(reserved(root, u64::MAX).unwrap());
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
