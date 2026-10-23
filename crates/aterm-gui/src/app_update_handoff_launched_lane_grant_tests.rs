// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::native_updater_service::ApplyMode;

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
            command: std::process::Command::new("/usr/bin/true"),
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
    assert_eq!(
        super::worker_pre_park_check(
            &mut job,
            |_| panic!("a fresh cached pass skips the full verification"),
            |_, _| None
        ),
        Ok(())
    );
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
    job.verify_staged_candidate = true;
    let verified = super::worker_pre_park_check(
        &mut job,
        |_| Err("codesign --verify (structural) failed".to_string()),
        |_, _| panic!("the full check reads the floor itself"),
    );
    assert!(
        matches!(
            &verified,
            Err(super::PreParkRefusal::Verification { outcome, detail })
                if *outcome == crate::UpdateHandoffOutcome::PreparationFailed
                    && detail.contains("codesign --verify (structural) failed")
        ),
        "the verifier's word is filed: {verified:?}"
    );
    app.update_handoff_prelaunch = None;
}

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
    let moment = format!(
        "lock: could not take the apply lock within 10s ({})",
        aterm_update::PASSING_REFUSAL_KEY
    );
    assert!(super::worker_pre_park_check(&mut job, |_| Err(moment.clone()), |_, _| None).is_err());
    assert!(kept().is_none(), "NEGATIVE: a moment is no verdict");
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
    assert!(
        entry
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains(&folder)),
        "the verifier's word is kept: {:?}",
        entry.reason
    );
    app.update_handoff_prelaunch = None;
}
