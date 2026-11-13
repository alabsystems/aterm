// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_live_snapshot_copy_requires_its_current_proven_claim() {
    use crate::session_timeline::{claim_supervisor_as, release_supervisor};
    let handle = crate::session_store::test_handle_on(7593, -1);
    let ctx = handle.ctx.as_ref();
    let sid = ctx.self_id.as_str();
    let now = crate::metrics::now_us();
    let proven = Proven {
        pid: 4242,
        start: "current primary".to_string(),
    };
    let own = copy_of(
        proven.pid,
        &proven.start,
        present(Some("0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77")),
    );
    set_copy_for_test(sid, Some(own.clone()));
    assert!(
        live_copy(ctx).is_none(),
        "a retained copy alone is no current claim"
    );
    claim_supervisor_as(
        ctx,
        "codex-alab@4242",
        Some(1),
        None,
        Some(proven.clone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the proven claim stands");
    assert_eq!(live_copy(ctx), Some(own.clone()));
    for other in [
        copy_of(4243, &proven.start, own.seen.clone()),
        copy_of(4242, "old process", own.seen.clone()),
    ] {
        set_copy_for_test(sid, Some(other));
        assert!(
            live_copy(ctx).is_none(),
            "a current proof cannot borrow another identity's copy"
        );
    }
    set_copy_for_test(sid, Some(own));
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    assert!(
        copy_of_tab(sid).is_some(),
        "a post-release crash copy may remain"
    );
    assert!(
        live_copy(ctx).is_none(),
        "the released claim cannot disambiguate a live root"
    );
    forget(sid);
}

/// A copy of `pid`'s record, read under `start`, naming `session`.
fn copy_of(pid: u32, start: &str, seen: self_claimed::Seen) -> Copy {
    Copy {
        pid,
        lifetime: None,
        start: start.to_string(),
        home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
        seen,
        resume: Err("no-resume-line"),
    }
}

fn present(session: Option<&str>) -> self_claimed::Seen {
    self_claimed::Seen::Present(self_claimed::Record {
        pid: 4242,
        session_id: session.map(str::to_string),
        resume_argv: None,
        env: Vec::new(),
        status: Some("busy".to_string()),
        exit: None,
        cwd: None,
        draft_empty: None,
        frame: None,
        goal: None,
        background_terminals: None,
        initializing: false,
    })
}

/// THE ROLLOUT TAP'S CLAIM GATE: only a
/// proven claim's own copy, standing and naming a thread id, names a
/// rollout to tap — its thread, under the home the record was read
/// under.
#[test]
fn only_a_proven_claims_own_copy_names_a_rollout_target() {
    const THREAD: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";
    let start = "Mon Oct 20 16:37:58 2026";
    let proven = Proven {
        pid: 4242,
        start: start.to_string(),
    };
    let own = copy_of(4242, start, present(Some(THREAD)));
    assert_eq!(
        target_of(Some(&proven), 7, Some(&own)),
        Some(RolloutTarget {
            revision: 7,
            pid: 4242,
            start: start.to_string(),
            thread: THREAD.to_string(),
            home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
        })
    );
    assert_eq!(
        target_of(None, 7, Some(&own)),
        None,
        "a vendor Codex: no proof"
    );
    assert_eq!(target_of(Some(&proven), 7, None), None, "no copy read yet");
    for other in [
        copy_of(4243, start, present(Some(THREAD))),
        copy_of(4242, "Mon Oct 20 09:00:00 2026", present(Some(THREAD))),
        copy_of(4242, start, self_claimed::Seen::Removed),
        copy_of(4242, start, self_claimed::Seen::Never),
        copy_of(4242, start, present(None)),
        copy_of(4242, start, present(Some("not-a-thread"))),
    ] {
        assert_eq!(target_of(Some(&proven), 7, Some(&other)), None, "{other:?}");
    }
}

/// THE PASSIVE RELEASE KEEPS ITS COPY TO READ ([`keep_to_read`]): a proven claim released
/// while its process runs and its record shows no end keeps the copy READ ONLY. No act path
/// sees it ([`copy_of_tab`], [`live_copy`], [`rollout_target`]) and its resume line is gone
/// even from the kept entry; the rollout tap's target stands on it ([`tap_target`]: the same
/// process and home, at the release's revision) while that process leads the front and is
/// alive as its claim proved it, and the sweep keeps asking (`Ask::Read`, never `Stop`). A
/// claim granted again names exactly the claim's target again, at its own revision.
/// NEGATIVE CONTROLS: another process leading the front, a process not alive as that start
/// (dead, or another start under the pid), the real kernel's word on an identity no process
/// has, and a release whose record shows the end coming (the claim's copy kept for the exit)
/// name no read-only target; a copy of another process goes at the release.
#[test]
fn a_passive_release_keeps_its_copy_to_read_and_no_act_path_sees_it() {
    use crate::codex_rollout_tap::{Ask, RECHECK, due};
    use crate::session_timeline::{claim_supervisor_as, release_supervisor};
    const THREAD: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";
    let handle = crate::session_store::test_handle_on(7594, -1);
    let ctx = handle.ctx.as_ref();
    let sid = ctx.self_id.as_str();
    let proven = Proven {
        pid: 4242,
        start: "Mon Oct 20 16:37:58 2026".to_string(),
    };
    let now = crate::metrics::now_us();
    let claim = |conn| {
        claim_supervisor_as(
            ctx,
            "codex-alab@4242",
            Some(conn),
            None,
            Some(proven.clone()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("the proven claim stands")
        .revision
    };
    let lined = || Copy {
        resume: Ok(self_claimed::Resume {
            argv: vec!["/opt/alab/bin/codex".into(), "resume".into(), THREAD.into()],
            session: THREAD.to_string(),
            codex_home: None,
            codex_auth_path: None,
            cwd: None,
        }),
        ..copy_of(4242, &proven.start, present(Some(THREAD)))
    };
    let revision = ctx.meta.lock().unwrap().supervisor_revision;
    let release = |copy: Copy, rings_delta: u64| {
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
        let revision = ctx.meta.lock().unwrap().supervisor_revision;
        let rings = crate::harness_host::rings_here();
        assert!(finish_release(ctx, &proven, revision, true, Some(copy)));
        assert_eq!(crate::harness_host::rings_here(), rings + rings_delta);
        revision
    };
    let alive_as_proven = |pid: u32, start: &str| pid == 4242 && start == proven.start;
    forget(sid);
    let first = claim(1);
    assert!(first > revision);
    let rings = crate::harness_host::rings_here();
    assert!(refresh_by(ctx, &proven, first, || Some(lined())));
    assert_eq!(crate::harness_host::rings_here(), rings + 1);
    let claimed = rollout_target(ctx).expect("the claim's own target");
    assert_eq!(
        tap_target_by(ctx, 4242, alive_as_proven),
        Some(TapTarget::Claimed(claimed.clone())),
        "under the claim the tap's target is the claim's, exactly"
    );

    let released = release(lined(), 1);
    assert_eq!(copy_of_tab(sid), None, "no act path reads a passive copy");
    assert_eq!(live_copy(ctx), None);
    assert_eq!(
        rollout_target(ctx),
        None,
        "a landing or `result` finds none"
    );
    assert_eq!(
        COPIES
            .lock()
            .unwrap()
            .get(sid)
            .map(|kept| kept.copy.resume.clone()),
        Some(Err("released")),
        "the kept entry itself carries no line"
    );
    let watched = TapTarget::Watched(Watched {
        revision: released,
        pid: 4242,
        start: proven.start.clone(),
        home: std::path::PathBuf::from("/private/tmp/x/codex-home"),
    });
    assert_eq!(
        tap_target_by(ctx, 4242, alive_as_proven),
        Some(watched.clone())
    );
    let mut asked = std::collections::HashMap::new();
    let t0 = std::time::Instant::now();
    for at in [t0, t0 + RECHECK] {
        assert_eq!(
            due(
                &mut asked,
                7594,
                tap_target_by(ctx, 4242, alive_as_proven),
                None,
                (at, None)
            ),
            Ask::Read(watched.clone()),
            "the sweep keeps asking, never stops"
        );
    }
    assert_eq!(
        tap_target_by(ctx, 4243, alive_as_proven),
        None,
        "another process leads the front"
    );
    assert_eq!(
        tap_target_by(ctx, 4242, |_, _| false),
        None,
        "the process is not alive as its claim proved it"
    );
    assert_eq!(
        tap_target(ctx, 4242),
        None,
        "the kernel knows no process 4242 started then"
    );
    assert_eq!(
        due(
            &mut asked,
            7594,
            tap_target(ctx, 4242),
            None,
            (t0 + RECHECK * 2, None)
        ),
        Ask::Stop
    );

    let again = claim(2);
    assert!(again > released, "the claim moves the revision");
    assert_eq!(
        tap_target_by(ctx, 4242, alive_as_proven),
        None,
        "a claim no copy was read for names nothing yet, as before the release kept one"
    );
    let rings = crate::harness_host::rings_here();
    assert!(refresh_by(ctx, &proven, again, || Some(lined())));
    assert_eq!(crate::harness_host::rings_here(), rings + 1);
    assert_eq!(
        tap_target_by(ctx, 4242, alive_as_proven),
        Some(TapTarget::Claimed(RolloutTarget {
            revision: again,
            ..claimed
        }))
    );
    assert_eq!(
        copy_of_tab(sid),
        Some(lined()),
        "the claim's copy acts again"
    );

    let ending = Copy {
        seen: self_claimed::Seen::Present(self_claimed::Record {
            status: Some("exited".to_string()),
            ..match present(Some(THREAD)) {
                self_claimed::Seen::Present(record) => record,
                _ => unreachable!("present"),
            }
        }),
        ..lined()
    };
    let _ = release(ending.clone(), 0);
    assert_eq!(copy_of_tab(sid), Some(ending), "kept for the exit");
    assert_eq!(tap_target_by(ctx, 4242, |_, _| true), None);

    let third = claim(3);
    assert!(refresh_by(ctx, &proven, third, || Some(lined())));
    let _ = release(copy_of(4242, &proven.start, self_claimed::Seen::Never), 1);
    assert_eq!(
        copy_of_tab(sid).map(|copy| (copy.seen, copy.resume)),
        Some((self_claimed::Seen::Removed, Err("no-record"))),
        "an orderly exit that removed its record withdraws its resume line and rings the host"
    );

    let _ = claim(4);
    set_copy_for_test(
        sid,
        Some(copy_of(4243, &proven.start, present(Some(THREAD)))),
    );
    let _ = release(copy_of(4243, &proven.start, present(Some(THREAD))), 0);
    assert!(
        COPIES.lock().unwrap().get(sid).is_none(),
        "another process's copy goes"
    );
    forget(sid);
}

/// Only a usage wall, only with the policy in effect and `resume_limits`
/// on, is the self-claim's own to wait out; everything else stays a stop.
#[test]
fn a_self_claim_answers_only_a_usage_wall_under_resume_limits() {
    use aterm_agent::supervise::SupervisorConfig;
    use aterm_messages::presence::WallAnswer;
    use aterm_phase::WallKind;
    let on = SupervisorConfig::default();
    assert!(on.resume_limits, "the default resumes limits");
    let off = SupervisorConfig {
        resume_limits: false,
        ..SupervisorConfig::default()
    };
    for kind in [WallKind::UsageSession, WallKind::UsageWeekly] {
        assert_eq!(wall_answer(kind, true, &on, true), WallAnswer::Harness);
        assert_eq!(wall_answer(kind, false, &on, true), WallAnswer::Harness);
        assert_eq!(wall_answer(kind, true, &off, true), WallAnswer::Nobody);
        assert_eq!(wall_answer(kind, true, &on, false), WallAnswer::Nobody);
    }
    for kind in [
        WallKind::Spend,
        WallKind::Context,
        WallKind::Auth,
        WallKind::Memory,
        WallKind::ModelBucket { consent: false },
    ] {
        assert_eq!(
            wall_answer(kind, true, &on, true),
            WallAnswer::Nobody,
            "{kind:?}"
        );
    }
}

/// A self-claim speaks for a Codex in front, by either name it runs
/// under.
#[test]
fn a_self_claim_speaks_only_for_a_codex_in_front() {
    for claimant in [
        "codex",
        "CODEX",
        "/opt/alab/bin/codex",
        "codex-alab",
        "Codex-Alab",
    ] {
        assert!(fronts_claimant(Some(claimant)), "{claimant}");
    }
    for other in [
        None,
        Some("claude"),
        Some("node"),
        Some("zsh"),
        Some("codexx"),
        Some("codex-alab-helper"),
        Some(""),
    ] {
        assert!(!fronts_claimant(other), "{other:?}");
    }
}

#[test]
fn a_holder_names_its_pid_in_one_spelling() {
    assert_eq!(holder_pid("codex-alab@4242"), Some(4242));
    for other in [
        "aterm-harness@4242",
        "codex-alab@",
        "codex-alab@04242",
        "codex-alab@+4242",
        "codex-alab@1",
        "codex-alab@0",
        "codex-alab@42x",
        "codex-alab-4242",
    ] {
        assert_eq!(holder_pid(other), None, "{other}");
    }
}

#[test]
fn only_the_peer_that_leads_the_foreground_proves_its_claim() {
    let facts = |peer, foreground| Facts {
        peer,
        foreground,
        alab_image: true,
    };
    assert_eq!(proves("codex-alab@500", facts(Some(500), 500)), Some(500));
    assert_eq!(
        proves(
            "codex-alab@500",
            Facts {
                alab_image: false,
                ..facts(Some(500), 500)
            }
        ),
        None
    );
    assert_eq!(proves("codex-alab@500", facts(Some(501), 500)), None);
    assert_eq!(proves("codex-alab@500", facts(Some(500), 499)), None);
    assert_eq!(proves("codex-alab@500", facts(None, 500)), None);
    assert_eq!(proves("codex-alab@500", facts(Some(500), -1)), None);
    assert_eq!(proves("watcher@500", facts(Some(500), 500)), None);
}

#[test]
fn record_reads_cannot_publish_after_handback_or_over_a_new_claim() {
    use crate::session_timeline::{claim_supervisor_as, release_supervisor};
    let handle = crate::session_store::test_handle_on(7511, -1);
    let ctx = handle.ctx.as_ref();
    let sid = ctx.self_id.as_str();
    let proven = Proven {
        pid: 4242,
        start: "a test identity".to_string(),
    };
    let holder = "codex-alab@4242";
    let now = crate::metrics::now_us();
    let copy = |seen| Copy {
        pid: proven.pid,
        lifetime: None,
        start: proven.start.clone(),
        home: "/test/.codex".into(),
        seen,
        resume: Err("test-line"),
    };
    let standing = || {
        copy(self_claimed::Seen::Present(self_claimed::Record {
            pid: proven.pid,
            session_id: None,
            resume_argv: None,
            env: Vec::new(),
            status: Some("busy".to_string()),
            exit: None,
            cwd: None,
            draft_empty: None,
            frame: None,
            goal: None,
            background_terminals: None,
            initializing: false,
        }))
    };
    forget(sid);
    let first = claim_supervisor_as(
        ctx,
        holder,
        Some(1),
        None,
        Some(proven.clone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("a claim");
    assert!(refresh_by(
        ctx,
        &proven,
        first.revision,
        || Some(standing())
    ));
    assert!(copy_of_tab(sid).is_some(), "the standing claim publishes");

    assert!(!refresh_by(ctx, &proven, first.revision, || {
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
        let revision = ctx.meta.lock().unwrap().supervisor_revision;
        assert!(finish_release(
            ctx,
            &proven,
            revision,
            true,
            Some(standing())
        ));
        assert!(
            copy_of_tab(sid).is_none(),
            "passive handback keeps no copy an act path reads"
        );
        Some(standing())
    }));
    assert!(copy_of_tab(sid).is_none(), "the old read stayed invalid");

    let released_revision = ctx.meta.lock().unwrap().supervisor_revision;
    let next = claim_supervisor_as(
        ctx,
        holder,
        Some(2),
        None,
        Some(proven.clone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the same identity claims again");
    assert!(refresh_by(ctx, &proven, next.revision, || Some(copy(
        self_claimed::Seen::Never
    ))));
    assert_eq!(
        copy_of_tab(sid).unwrap().seen,
        self_claimed::Seen::Removed,
        "a passive read-only copy preserves that this identity's record was already seen"
    );
    assert!(refresh_by(ctx, &proven, next.revision, || Some(standing())));
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    let passive_rev = ctx.meta.lock().unwrap().supervisor_revision;
    assert!(finish_release(
        ctx,
        &proven,
        passive_rev,
        true,
        Some(standing())
    ));
    let next = claim_supervisor_as(
        ctx,
        holder,
        Some(2),
        None,
        Some(proven.clone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the same identity claims again after passive release");
    let initializing = || {
        let mut c = standing();
        if let self_claimed::Seen::Present(ref mut record) = c.seen {
            record.initializing = true;
        }
        c
    };
    assert!(refresh_by(ctx, &proven, next.revision, || Some(
        initializing()
    )));
    assert_eq!(
        copy_of_tab(sid).map(|c| (c.seen, c.resume)),
        Some((
            self_claimed::Seen::Unreadable("record-regressed"),
            Err("record-regressed")
        )),
        "a passive read-only copy prevents a re-claim from regressing to initializing"
    );
    assert!(refresh_by(ctx, &proven, next.revision, || Some(standing())));
    assert!(!finish_release(
        ctx,
        &proven,
        released_revision,
        true,
        Some(copy(self_claimed::Seen::Removed))
    ));
    assert!(matches!(
        copy_of_tab(sid).unwrap().seen,
        self_claimed::Seen::Present(_)
    ));

    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    let revision = ctx.meta.lock().unwrap().supervisor_revision;
    assert!(finish_release(
        ctx,
        &proven,
        revision,
        true,
        Some(copy(self_claimed::Seen::Removed))
    ));
    assert!(!refresh_by(
        ctx,
        &proven,
        next.revision,
        || Some(standing())
    ));
    assert!(matches!(
        copy_of_tab(sid).unwrap().seen,
        self_claimed::Seen::Removed
    ));

    let next = claim_supervisor_as(
        ctx,
        holder,
        Some(3),
        None,
        Some(proven.clone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("a next claim");
    assert!(refresh_by(ctx, &proven, next.revision, || Some(standing())));
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    let revision = ctx.meta.lock().unwrap().supervisor_revision;
    let alive = std::cell::Cell::new(true);
    assert_eq!(
        released_by(
            ctx,
            &proven,
            revision,
            || alive.get(),
            || {
                alive.set(false);
                None
            }
        ),
        Some(false)
    );
    assert!(matches!(
        copy_of_tab(sid).unwrap().seen,
        self_claimed::Seen::Present(_)
    ));

    let next = claim_supervisor_as(
        ctx,
        holder,
        Some(4),
        None,
        Some(proven.clone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("a next claim");
    assert!(refresh_by(ctx, &proven, next.revision, || Some(copy(
        self_claimed::Seen::Never
    ))));
    assert_eq!(copy_of_tab(sid).unwrap().seen, self_claimed::Seen::Removed);
    forget(sid);
    assert!(refresh_by(ctx, &proven, next.revision, || Some(copy(
        self_claimed::Seen::Never
    ))));
    assert_eq!(copy_of_tab(sid).unwrap().seen, self_claimed::Seen::Never);
    forget(sid);
}

/// THE WATCH-ONLY WORKER'S IDLE POINT, live ([`idle_now`]): a real
/// stand-in process (the kept signed `sleep`, its `CODEX_HOME` the
/// test's directory) holding the session's PROVEN claim, with a PLANTED
/// record saying `idle` and `draftEmpty: true`, is at its idle point; the
/// record saying `busy` is not.
#[cfg(target_vendor = "apple")]
#[test]
fn the_idle_point_is_the_proven_processs_own_record_read_live() {
    use crate::session_timeline::{claim_supervisor_as, release_supervisor};
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let home = std::fs::canonicalize(dir.path()).expect("canonical");
    let sleep = std::fs::canonicalize(aterm_uds::deadline::stand_in(std::path::Path::new(
        "/bin/sleep",
    )))
    .expect("the kept stand-in");
    let sleep_s = sleep.to_string_lossy().into_owned();
    let mut child = aterm_uds::spawnfd::command(&sleep)
        .arg("60")
        .env("CODEX_HOME", &home)
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !atpkg::caller_shell::process_args(pid).is_some_and(|a| a.argv.first() == Some(&sleep_s))
    {
        assert!(std::time::Instant::now() < deadline, "sleep never ran");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let start = restart_stamp::kernel_start(pid).expect("its start");
    let path = self_claimed::record_path(&home, pid);
    std::fs::create_dir_all(path.parent().expect("dir")).expect("sessions dir");
    let plant = |status: &str| {
        std::fs::write(
            &path,
            format!(r#"{{"v":1,"pid":{pid},"status":"{status}","draftEmpty":true,"exit":null}}"#),
        )
        .expect("the record");
    };
    let handle = crate::session_store::test_handle_on(7512, -1);
    let ctx = handle.ctx.as_ref();
    let proven = Proven {
        pid,
        start: start.clone(),
    };
    let holder = format!("{SELF_CLAIM_PREFIX}{pid}");
    let now = crate::metrics::now_us();
    claim_supervisor_as(
        ctx,
        &holder,
        Some(1),
        None,
        Some(proven),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("a claim");
    plant("idle");
    assert!(idle_now(ctx, pid, &start), "its own record says it rests");
    plant("busy");
    assert!(!idle_now(ctx, pid, &start), "busy");
    plant("idle");
    assert!(!idle_now(ctx, pid + 1, &start), "another process");
    assert!(
        !idle_now(ctx, pid, "Thu Jan  1 00:00:00 1970"),
        "another identity"
    );
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    assert!(!idle_now(ctx, pid, &start), "no proof stands");
    claim_supervisor_as(
        ctx,
        &holder,
        Some(2),
        None,
        Some(Proven {
            pid,
            start: start.clone(),
        }),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("claimed again");
    assert!(idle_now(ctx, pid, &start), "positive control, again");
    let _ = child.kill();
    let _ = child.wait();
    assert!(!idle_now(ctx, pid, &start), "a dead identity rests nowhere");
}

/// THE READ-ONLY COPY FOLLOWS ITS PROCESS'S OWN RECORD, live: a real stand-in process
/// ([`stand_in::Sleeper`], its `CODEX_HOME` the test's directory) whose proven claim is
/// released through the real path keeps its copy read only, and its target taps the thread
/// the record names, read again at each ask ([`tapped`] over [`self_claimed::read_seen`]): a
/// `/new` written into the record moves the tapped thread at once and the old thread is held
/// no more ([`hold_rollout`]); a record naming no thread, or one that says it is another
/// process's, taps nothing while the target still stands, so the next ask reads it again and
/// taps the thread it names then. FAIL CLOSED: a record that shows the end coming (`exited`),
/// or is gone after it was seen (folded to removed), ends the target at that ask, and only a
/// claim granted and given back again names one. NEGATIVE CONTROLS: another group in front,
/// and the process ended, name no target; an ended process's record is not read, and its copy
/// goes ([`discard_dead`]).
#[cfg(target_vendor = "apple")]
#[test]
fn a_read_only_copy_follows_its_live_processs_own_record() {
    use crate::session_timeline::{claim_supervisor_as, release_supervisor};
    const THREAD: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";
    const NEXT: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b78";
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let home = std::fs::canonicalize(dir.path()).expect("canonical");
    let mut sleeper = stand_in::Sleeper::under(&home);
    let proven = sleeper.proven.clone();
    let pid = proven.pid;
    let group = i32::try_from(pid).expect("a pid");
    sleeper.plant(Some(THREAD));
    let handle = crate::session_store::test_handle_on(7595, -1);
    let ctx = handle.ctx.as_ref();
    let sid = ctx.self_id.as_str();
    let now = crate::metrics::now_us();
    let claim_and_release = || {
        claim_supervisor_as(
            ctx,
            &format!("{SELF_CLAIM_PREFIX}{pid}"),
            Some(1),
            None,
            Some(proven.clone()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("a claim");
        meta_changed(ctx);
        let claimed = rollout_target(ctx).expect("the claim's copy names its thread");
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
        assert_eq!(copy_of_tab(sid), None, "no act path is handed the copy");
        let Some(TapTarget::Watched(watched)) = tap_target(ctx, group) else {
            panic!("a read-only target for the released claim's live process");
        };
        (claimed, watched)
    };
    let (claimed, watched) = claim_and_release();
    assert_eq!(
        (claimed.thread.as_str(), claimed.home.as_path()),
        (THREAD, home.as_path())
    );
    assert_eq!(
        (watched.pid, watched.start.as_str(), watched.home.as_path()),
        (pid, proven.start.as_str(), home.as_path())
    );
    let target = TapTarget::Watched(watched);
    let thread = |target: &TapTarget| {
        tapped(ctx, target, self_claimed::read_seen).map(|tapped| tapped.thread)
    };
    let old = tapped(ctx, &target, self_claimed::read_seen).expect("its thread");
    assert_eq!(old.thread, THREAD);
    assert!(hold_rollout(ctx, &old).is_some());
    sleeper.plant(Some(NEXT));
    assert_eq!(
        thread(&target),
        Ok(NEXT.to_string()),
        "a /new under Off taps the new thread"
    );
    assert!(hold_rollout(ctx, &old).is_none(), "never the old one");
    sleeper.plant(None);
    assert_eq!(thread(&target), Err(Untapped::Unnamed));
    assert_eq!(
        tap_target(ctx, group),
        Some(target.clone()),
        "the target stands, so the next ask reads the record again"
    );
    sleeper.plant(Some(NEXT));
    assert_eq!(thread(&target), Ok(NEXT.to_string()));
    sleeper.plant_as(pid + 1, Some(THREAD), "busy");
    assert_eq!(
        thread(&target),
        Err(Untapped::Unnamed),
        "a record that says it is another process's names no thread of this one"
    );
    assert_eq!(tap_target(ctx, group), Some(target.clone()));
    sleeper.plant(Some(NEXT));
    assert_eq!(thread(&target), Ok(NEXT.to_string()));

    sleeper.unplant();
    assert_eq!(
        thread(&target),
        Err(Untapped::Moved),
        "a record gone after it was seen reads removed: the end"
    );
    assert_eq!(tap_target(ctx, group), None);
    sleeper.plant(Some(NEXT));
    assert_eq!(
        thread(&target),
        Err(Untapped::Moved),
        "a record written again names no target until a claim reads it"
    );
    let (_, watched) = claim_and_release();
    let target = TapTarget::Watched(watched);
    assert_eq!(thread(&target), Ok(NEXT.to_string()));
    sleeper.plant_as(pid, Some(NEXT), "exited");
    assert_eq!(
        thread(&target),
        Err(Untapped::Moved),
        "a record that shows the end coming ends the target"
    );
    assert_eq!(tap_target(ctx, group), None);

    sleeper.plant(Some(NEXT));
    let (_, watched) = claim_and_release();
    let target = TapTarget::Watched(watched);
    assert_eq!(thread(&target), Ok(NEXT.to_string()));
    assert_eq!(tap_target(ctx, group + 1), None, "another group in front");
    sleeper.end();
    assert_eq!(tap_target(ctx, group), None, "the process ended");
    assert_eq!(
        thread(&target),
        Err(Untapped::Moved),
        "an ended process's record is not read"
    );
    discard_dead(sid);
    assert!(
        COPIES.lock().unwrap().get(sid).is_none(),
        "a dead process's copy goes"
    );
    forget(sid);
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_proof_keeps_the_identity_of_the_process_it_named() {
    let me = std::process::id();
    let holder = format!("{SELF_CLAIM_PREFIX}{me}");
    let facts = Facts {
        peer: Some(me),
        foreground: i32::try_from(me).expect("a pid"),
        alab_image: true,
    };
    assert!(!runs_alab_build(me));
    let proven = prove(&holder, facts).expect("this process proves its own claim");
    assert_eq!(proven.pid, me);
    assert!(proven.alive(), "this process runs");
    let reused = Proven {
        start: "Thu Jan  1 00:00:00 1970".to_string(),
        ..proven
    };
    assert!(!reused.alive(), "a reused pid is not the proof's process");
}
