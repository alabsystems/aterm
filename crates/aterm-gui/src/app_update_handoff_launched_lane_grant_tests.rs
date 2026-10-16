// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::native_updater_service::ApplyMode;

/// A fresh cached pass declaring the chunked grant lets the lane admit more
/// sessions than one descriptor message carries and lets the worker skip its own
/// verification (`verify_staged_candidate: false`). The worker's job must carry
/// the same fact, or the launch omits `ATERM_HANDOFF_GRANT_CAPS`, the successor
/// claims `ATRZ1C`, and `transfer` refuses the desk.
#[test]
fn a_worker_that_skips_verification_offers_the_grant_the_lane_admitted() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for grant_chunks in [true, false] {
        let mut app = crate::App::headless_for_test();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            crate::running_build_number().saturating_add(1),
            "",
            &"ab".repeat(32),
        );
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<super::HandoffWorkerJob>(1);
        let (reconcile_worker, _reconcile_rx) = std::sync::mpsc::sync_channel(1);
        let reconcile_ticket = app
            .mint_native_update_reconcile_ticket()
            .expect("a reconcile ticket");
        app.prelaunch_out_of_band_handoff(super::PrelaunchArgs {
            attempt_id: 31,
            build: crate::running_build_number(),
            mode: ApplyMode::Automatic,
            apply_attempt: Some(ticket.clone()),
            same_image: None,
            target_build: ticket.target_build(),
            target_commit: String::new(),
            // std on purpose: the shipped `PrelaunchArgs::command` is a std one.
            command: std::process::Command::new("/usr/bin/true"),
            // A fresh cached pass: the worker does not verify again.
            verify_staged_candidate: false,
            grant_chunks,
            installed_activation: false,
            bundle: None,
            cancel,
            cancelled,
            job_tx,
            reconcile_worker,
            reconcile_ticket,
        })
        .expect("the launched lane records its attempt");
        let job = job_rx.try_recv().expect("the worker's job was sent");
        assert!(!job.verify_staged_candidate, "PRECONDITION");
        assert_eq!(
            job.successor_grant_chunks, grant_chunks,
            "the successor is offered exactly the grant the lane admitted"
        );
        app.update_handoff_prelaunch = None;
    }
}

/// A fresh cached pass does not outlive a raised operator floor: the worker that skips its
/// full verification still asks the floor for the target build and refuses before any reader
/// parks.
#[test]
fn a_worker_that_skips_verification_still_refuses_a_raised_floor() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        crate::running_build_number().saturating_add(1),
        "",
        &"ab".repeat(32),
    );
    let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<super::HandoffWorkerJob>(1);
    let (reconcile_worker, _reconcile_rx) = std::sync::mpsc::sync_channel(1);
    let reconcile_ticket = app
        .mint_native_update_reconcile_ticket()
        .expect("a reconcile ticket");
    app.prelaunch_out_of_band_handoff(super::PrelaunchArgs {
        attempt_id: 32,
        build: crate::running_build_number(),
        mode: ApplyMode::Automatic,
        apply_attempt: Some(ticket.clone()),
        same_image: None,
        target_build: ticket.target_build(),
        target_commit: String::new(),
        command: std::process::Command::new("/usr/bin/true"),
        // A fresh cached pass: the worker does not verify again.
        verify_staged_candidate: false,
        grant_chunks: false,
        installed_activation: false,
        bundle: None,
        cancel,
        cancelled,
        job_tx,
        reconcile_worker,
        reconcile_ticket,
    })
    .expect("the launched lane records its attempt");
    let mut job = job_rx.try_recv().expect("the worker's job was sent");
    assert!(!job.verify_staged_candidate, "PRECONDITION");
    let floor = job.target_build.saturating_add(1);
    let asked = std::cell::Cell::new(None);
    // The worker's own check, as `run_handoff_worker` calls it: the full verification is
    // skipped and the floor still refuses.
    let refusal = super::worker_pre_park_check(
        &mut job,
        |_| panic!("a fresh cached pass skips the full verification"),
        |build, staged_commit| {
            assert_eq!(
                staged_commit,
                Some(""),
                "a staged candidate names its commit"
            );
            asked.set(Some(build));
            (build < floor).then(|| format!("below the operator apply floor {floor}"))
        },
    );
    assert_eq!(
        asked.get(),
        Some(job.target_build),
        "the floor is asked of the target"
    );
    assert!(
        matches!(
            &refusal,
            Err(super::PreParkRefusal::Verification { outcome, detail })
                if *outcome == crate::UpdateHandoffOutcome::PreparationFailed
                    && detail.contains("floor")
        ),
        "a raised floor refuses before anything is launched: {refusal:?}"
    );
    // At the floor, nothing is refused.
    assert_eq!(
        super::worker_pre_park_check(
            &mut job,
            |_| panic!("a fresh cached pass skips the full verification"),
            |_, _| None
        ),
        Ok(())
    );
    // A stage the check retired is refused: the staged candidate is asked under its commit.
    // The stage's state is a moment, as the full check files it, so the refusal is transient:
    // nothing is launched or booked against the build. An activation has no stage to ask about.
    let gone = super::worker_pre_park_check(
        &mut job,
        |_| panic!("a fresh cached pass skips the full verification"),
        |build, staged_commit| {
            staged_commit.map(|_| {
                format!(
                    "staged build {build} is no longer staged; not handing off to it ({})",
                    aterm_update::PASSING_REFUSAL_KEY
                )
            })
        },
    );
    assert!(
        matches!(
            &gone,
            Err(super::PreParkRefusal::Verification { outcome, detail })
                if *outcome == crate::UpdateHandoffOutcome::ProducerFailed
                    && detail.contains("no longer staged")
        ),
        "{gone:?}"
    );
    job.installed_activation = true;
    assert_eq!(
        super::worker_pre_park_check(
            &mut job,
            |_| panic!("a fresh cached pass skips the full verification"),
            |_, staged_commit| {
                assert_eq!(
                    staged_commit, None,
                    "an activation is asked the floor alone"
                );
                None
            }
        ),
        Ok(())
    );
    job.installed_activation = false;
    // Negative control: a worker that verifies never asks the floor separately (the full check
    // reads it), and files the verifier's word.
    job.verify_staged_candidate = true;
    assert_eq!(
        super::worker_pre_park_check(
            &mut job,
            |_| Err("codesign --verify (structural) failed".to_string()),
            |_, _| panic!("the full check reads the floor itself"),
        ),
        Err(super::PreParkRefusal::Verification {
            outcome: crate::UpdateHandoffOutcome::PreparationFailed,
            detail: "staged update failed pre-park verification: codesign --verify \
                     (structural) failed"
                .to_string(),
        })
    );
    app.update_handoff_prelaunch = None;
}

/// A worker that verifies (the attempt outran the arm-time read) publishes a refusal that is a
/// verdict to the attempt's cache entry, so the attempt is booked by the verifier's word (an
/// unwritable folder is the person's, never a structural failure of the build). Negative
/// control: a passing moment (the apply lock held) is no verdict and is not kept.
#[test]
fn a_worker_that_verifies_keeps_its_verdict_for_the_booking() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        crate::running_build_number().saturating_add(1),
        "",
        &"ab".repeat(32),
    );
    let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<super::HandoffWorkerJob>(1);
    let (reconcile_worker, _reconcile_rx) = std::sync::mpsc::sync_channel(1);
    let reconcile_ticket = app
        .mint_native_update_reconcile_ticket()
        .expect("a reconcile ticket");
    app.prelaunch_out_of_band_handoff(super::PrelaunchArgs {
        attempt_id: 33,
        build: crate::running_build_number(),
        mode: ApplyMode::Automatic,
        apply_attempt: Some(ticket.clone()),
        same_image: None,
        target_build: ticket.target_build(),
        target_commit: String::new(),
        command: std::process::Command::new("/usr/bin/true"),
        // No verdict cached: the worker verifies.
        verify_staged_candidate: true,
        grant_chunks: false,
        installed_activation: false,
        bundle: None,
        cancel,
        cancelled,
        job_tx,
        reconcile_worker,
        reconcile_ticket,
    })
    .expect("the launched lane records its attempt");
    let mut job = job_rx.try_recv().expect("the worker's job was sent");
    let kept = || {
        app.handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    };
    // A passing moment: refused, transient, and nothing kept.
    let moment = format!(
        "lock: could not take the apply lock within 10s ({})",
        aterm_update::PASSING_REFUSAL_KEY
    );
    assert!(super::worker_pre_park_check(&mut job, |_| Err(moment.clone()), |_, _| None).is_err());
    assert!(kept().is_none(), "NEGATIVE: a moment is no verdict");
    // The folder's refusal: a verdict, kept for this artifact.
    let folder = format!(
        "{}: /Applications/aterm.app; this install cannot update itself",
        aterm_update::INSTALL_LOCATION_REFUSAL_KEY
    );
    let refusal = super::worker_pre_park_check(&mut job, |_| Err(folder.clone()), |_, _| None);
    assert!(
        matches!(
            &refusal,
            Err(super::PreParkRefusal::Verification { outcome, .. })
                if *outcome == crate::UpdateHandoffOutcome::PreparationFailed
        ),
        "{refusal:?}"
    );
    let entry = kept().expect("the verdict is kept");
    assert!(!entry.passed);
    assert_eq!(entry.build, ticket.target_build());
    assert_eq!(entry.artifact, ticket.target_dmg_sha256());
    assert_eq!(
        entry.reason.as_deref(),
        Some(format!("staged update failed pre-park verification: {folder}").as_str()),
        "in the arm-time read's own words"
    );
    app.update_handoff_prelaunch = None;
}
