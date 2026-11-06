// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Attachment activation belongs to the exact accepted Caps setting, beyond stage completion.

use super::*;

fn accepted(records: &Records) -> Option<u64> {
    let (plan, _) = records.plan.as_ref()?;
    let held = holds(records);
    if !plan.selection.contains(Setting::CapsLock)
        || held.holds(Setting::CapsLock)
        || matches!(records.decision, Decision::Stopped(_))
    {
        return None;
    }
    let Some(progress) = records.progress else {
        // The older selection-only writer kept no progress. Its applied decision already
        // proves the workstation pass ran (the same evidence a later choice carries with
        // older_run). Keep that run's existing revision zero without admitting another pass.
        // With no physical run to disambiguate a Stop, any request withholds this authority.
        if records.stop_request.is_some() {
            return None;
        }
        let run = older_run(records.plan.as_ref(), records.decision)?;
        return (run.done & Stage::Workstation.bit() != 0).then_some(run.revision);
    };
    (progress.belongs_to(plan)
        && progress.admitted
        && !progress.stopped
        && !progress.named_by(records.stop_request)
        && progress.done & Stage::Workstation.bit() != 0)
        .then_some(progress.revision)
}

fn revision_at(root: &Path) -> Result<Option<u64>, String> {
    Ok(accepted(
        &observation_at(root).map_err(|why| why.to_string())?.records,
    ))
}

/// The exact accepted Caps-setting revision, after its workstation stage has applied. No
/// profile is inferred or widened; Off, Stop, Restore and unreadable records grant nothing.
/// A selection-only older setup's applied workstation pass retains its original revision zero.
///
/// # Errors
/// This account's scope or its setup records cannot be read.
pub fn input_activation_revision() -> Result<Option<u64>, String> {
    ensure_person_scope()?;
    revision_at(&state_root()?)
}

/// Activate only newly attached services under their original accepted Caps setting. The
/// profile lock orders the operation against choices and Restore, while both the durable and
/// this process's immediate Stop are checked before each whole item.
///
/// # Errors
/// Setup authority, console identity, input facts or live activation cannot be read safely.
pub fn activate_input_attachments(
    expected: u64,
    registries: &[u64],
    stopped: impl Fn(u64) -> bool,
) -> Result<Option<bool>, String> {
    ensure_person_scope()?;
    let root = state_root()?;
    activate_at(&root, expected, &stopped, |kept, continues| {
        aterm_machines::init::activate_keyboard_attachments(
            &aterm_machines::port::Live::new(),
            registries,
            &|item| kept.keeps(item),
            continues,
        )
    })
}

fn activate_at(
    root: &Path,
    expected: u64,
    stopped: &dyn Fn(u64) -> bool,
    work: impl FnOnce(&kept::Kept, &dyn Fn() -> bool) -> Result<bool, String>,
) -> Result<Option<bool>, String> {
    let Some(_lock) = HeldLock::try_take(root)? else {
        return Ok(None);
    };
    let continues =
        || !stopped(expected) && revision_at(root).is_ok_and(|now| now == Some(expected));
    if !continues() {
        return Ok(None);
    }
    let kept = kept::Kept::load(root)?;
    work(&kept, &continues).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(root: &Path) -> (Plan, Progress) {
        let mut selected = Selection::none();
        selected.set(Setting::CapsLock, true);
        let plan = Plan::from(selected);
        save_plan(root, &plan).unwrap();
        save_workstation_authorization(root, &plan).unwrap();
        let mut progress = Progress::for_plan(&plan);
        progress.revision = 7;
        progress.admitted = true;
        progress.done = Stage::Workstation.bit();
        progress.completed = true;
        save_progress(root, progress).unwrap();
        (plan, progress)
    }

    fn older(root: &Path, selection: Selection, state: &str, verified: bool) {
        save_automatic_plan(root, selection).unwrap();
        write_json(
            &root.join("first-run.json"),
            &json!({"profile":PROFILE,"state":state,"accounts_verified":verified}),
        )
        .unwrap();
    }

    #[test]
    fn completed_selection_only_setup_keeps_caps_authority_without_restarting_its_pass() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let selected = Selection::all().workstation();
        older(root, selected, "applied", true);
        bring_forward_restored_at(root).unwrap();
        let before = observation_at(root).unwrap();
        assert!(finished_at(root).unwrap());
        assert!(before.records.progress.is_none());
        assert!(scope::upgrade_of(&before.records, || true).is_none());
        assert_eq!(revision_at(root).unwrap(), Some(0));
        assert_eq!(
            activate_at(root, 0, &|_| false, |_, go| {
                assert!(HeldLock::try_take(root)?.is_none());
                Ok(go())
            })
            .unwrap(),
            Some(true)
        );
        assert_eq!(observation_at(root).unwrap(), before);
        assert!(finished_at(root).unwrap());
        assert!(progress_file(root).unwrap().is_none());

        // A subsequent modern choice is staged under a different revision: its saved plan
        // alone never lets the old worker borrow the new setting's authority.
        let (plan, mut progress) = ready(root);
        assert_ne!(revision_at(root).unwrap(), Some(0));
        assert_eq!(
            activate_at(root, 0, &|_| false, |_, _| panic!(
                "superseded legacy setting"
            ))
            .unwrap(),
            None
        );
        progress.admitted = false;
        save_progress(root, progress).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        std::fs::remove_file(root.join("initial-setup.json")).unwrap();
        save_workstation_decision(root, true).unwrap();
        assert!(saved_plan_record(root).unwrap().unwrap().1);
        assert!(plan.selection.contains(Setting::CapsLock));
        assert_eq!(revision_at(root).unwrap(), None);
    }

    #[test]
    fn selection_only_attachment_authority_requires_an_applied_caps_choice_without_holds() {
        for state in ["pending", "cancelled", "restored"] {
            let dir = aterm_tempfile::tempdir().unwrap();
            older(dir.path(), Selection::all().workstation(), state, true);
            assert_eq!(revision_at(dir.path()).unwrap(), None, "{state}");
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut selected = Selection::all().workstation();
        selected.set(Setting::CapsLock, false);
        older(root, selected, "applied", true);
        assert_eq!(revision_at(root).unwrap(), None);

        // Unverified sign-outs do not revoke a workstation pass that already applied.
        selected.set(Setting::CapsLock, true);
        older(root, selected, "applied", false);
        assert_eq!(revision_at(root).unwrap(), Some(0));
        assert_eq!(
            activate_at(root, 0, &|_| true, |_, _| panic!("immediate legacy Stop")).unwrap(),
            None
        );
        request_stop_at(root, 0).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        request_stop_at(root, 9).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        std::fs::write(root.join(STOP_REQUEST), b"unreadable").unwrap();
        assert!(revision_at(root).is_err() || revision_at(root).unwrap().is_none());
    }

    #[test]
    fn attachment_authority_survives_completion_but_not_stop_off_or_unadmitted_work() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let (mut plan, mut progress) = ready(root);
        assert_eq!(revision_at(root).unwrap(), Some(7));
        assert_eq!(
            activate_at(root, 7, &|_| false, |_, go| Ok(go())).unwrap(),
            Some(true)
        );
        assert_eq!(
            activate_at(root, 8, &|_| false, |_, _| panic!("stale revision")).unwrap(),
            None
        );
        assert_eq!(
            activate_at(root, 7, &|_| true, |_, _| panic!("immediate Stop")).unwrap(),
            None
        );
        progress.admitted = false;
        save_progress(root, progress).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        progress.admitted = true;
        progress.done = 0;
        save_progress(root, progress).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        progress.done = Stage::Workstation.bit();
        plan.selection.set(Setting::CapsLock, false);
        progress.bind(&plan);
        save_plan(root, &plan).unwrap();
        save_progress(root, progress).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        ready(root);
        request_stop_at(root, 7).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        assert_eq!(
            activate_at(root, 7, &|_| false, |_, _| panic!("durable Stop")).unwrap(),
            None
        );
        let restored = aterm_tempfile::tempdir().unwrap();
        ready(restored.path());
        save_restore(restored.path()).unwrap();
        assert_eq!(revision_at(restored.path()).unwrap(), None);
        let unread = aterm_tempfile::tempdir().unwrap();
        ready(unread.path());
        std::fs::write(unread.path().join(STOP_REQUEST), b"unreadable").unwrap();
        assert!(
            revision_at(unread.path()).is_err() || revision_at(unread.path()).unwrap().is_none()
        );
    }

    #[test]
    fn the_effect_boundary_rechecks_a_stop_and_keeps_the_profile_lock() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        ready(root);
        assert_eq!(
            activate_at(root, 7, &|_| false, |_, go| {
                assert!(HeldLock::try_take(root)?.is_none());
                assert!(go());
                request_stop_at(root, 7)?;
                Ok(go())
            })
            .unwrap(),
            Some(false)
        );
        assert!(HeldLock::try_take(root).unwrap().is_some());
    }
}
