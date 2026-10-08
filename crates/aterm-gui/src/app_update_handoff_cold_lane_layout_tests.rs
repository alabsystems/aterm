// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;
use crate::native_settings::SettingsRoute;

fn windowed_with_settings() -> App {
    let mut app = App::headless_for_test();
    app.headless = false;
    assert!(app.open_settings_tab(SettingsRoute::TextFonts));
    app
}

#[test]
fn the_cold_lane_leaves_a_layout_that_reopens_the_native_tabs() {
    let app = windowed_with_settings();
    let layout = app
        .cold_lane_layout()
        .expect("restore is on by default")
        .expect("a windowed desk leaves its layout");
    assert!(
        layout
            .windows
            .iter()
            .any(|window| !window.native_tabs.is_empty()),
        "the Settings tab is in the layout: {layout:?}"
    );
    assert_eq!(
        layout,
        app.capture_restore_manifest(),
        "the same layout a quit writes"
    );
}

#[test]
fn the_cold_lane_refuses_what_it_could_not_reopen() {
    let mut app = windowed_with_settings();
    app.config.restore_session = Some(false);
    let refusal = app
        .cold_lane_layout()
        .expect_err("nothing would reopen them");
    assert!(refusal.contains("session restore is off"), "{refusal}");
    // CONTROL: a headless instance has no windows to leave.
    let mut headless = windowed_with_settings();
    headless.headless = true;
    assert_eq!(headless.cold_lane_layout(), Ok(None));
}

/// A layout write that fails is this machine's condition, never a `Failed`
/// charged to the artifact. A PASSING cause (a full disk, an I/O error,
/// another aterm holding the restore lock past the main thread's bound) is
/// a refusal the lane retries.
///
/// ROUND EIGHT, U48: a SETTLED cause (a data folder this user cannot write,
/// a read-only volume, no data dir) was refused too, every minute with no
/// end, while the posture said the update installs by itself. The lane now
/// goes on without the layout and says why. And a write that failed AFTER it
/// published (its directory's sync) is the layout it wrote, which an
/// `exec` that does not happen takes back, where the refusal left it on
/// disk for a later launch to open beside this window.
///
/// RED before the fix: the settled cause refused (and so did the published
/// one, leaving its layout).
#[test]
fn a_layout_the_disk_will_not_take_refuses_only_while_its_cause_passes() {
    use crate::restore::RestoreFault;
    let app = windowed_with_settings();
    let layout = app
        .cold_lane_layout()
        .expect("restore is on")
        .expect("layout");
    let errno = std::io::Error::from_raw_os_error;
    let left = |fault: RestoreFault| {
        super::leave_cold_lane_layout(Some(&layout), |_| Err(fault))
            .map_err(|error| (error.is_refused(), error.to_string()))
    };
    let error = left(RestoreFault::io("write session.toml", &errno(libc::ENOSPC)))
        .expect_err("a full disk passes: refused, retried");
    assert!(error.0, "a refusal, not a failure: {error:?}");
    assert!(error.1.contains("No space left on device"), "{error:?}");
    for settled in [libc::EACCES, libc::EROFS, libc::ENOTSUP] {
        let fault = RestoreFault::io("open restore lock", &errno(settled));
        let cause = fault.to_string();
        assert_eq!(
            left(fault),
            Ok(super::LayoutLeft::NotKept(cause)),
            "SETTLED: THE LANE GOES ON WITHOUT ITS LAYOUT, never a refusal with no end"
        );
    }
    assert_eq!(
        left(RestoreFault::settled("no data dir for session restore")),
        Ok(super::LayoutLeft::NotKept(
            "no data dir for session restore".to_string()
        ))
    );

    // PUBLISHED, THEN FAILED (its directory's sync): the layout a reader
    // sees, `Written` whatever the cause — the successor reads it, and an
    // `exec` that does not happen takes it back, this run's desk alone;
    // another instance's quit layout the write kept stays.
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("session.toml");
    let wait = super::COLD_LANE_LAYOUT_LOCK_WAIT;
    let other = crate::restore::RestoreManifest::new(layout.windows.clone());
    crate::restore::write_quit_to(&path, &other, Some("1-1")).expect("W's quit");
    for errno_after in [libc::EIO, libc::ENOTSUP] {
        let left = super::leave_cold_lane_layout(Some(&layout), |layout| {
            crate::restore::write_quit_to_within(&path, layout, Some("2-2"), wait)?;
            Err(RestoreFault::published_but(
                "session.toml was published but directory durability is indeterminate",
                &errno(errno_after),
            ))
        })
        .map_err(|error| error.to_string());
        assert_eq!(left, Ok(super::LayoutLeft::Written), "{errno_after}");
        let written = std::fs::read_to_string(&path).expect("both desks");
        assert_eq!(
            crate::restore::quit_run_of(&written),
            Some("2-2".to_string()),
            "THE SUCCESSOR READS THIS RUN'S LAYOUT"
        );
        // The `exec` did not happen: the cold arm's take-back.
        crate::restore::unwrite_quit_to_within(&path, Some("2-2"), wait).expect("taken back");
        let waiting = std::fs::read_to_string(&path).expect("W's desk waits");
        assert_eq!(
            (
                crate::restore::quit_run_of(&waiting),
                crate::restore::quit_also_of(&waiting)
            ),
            (Some("1-1".to_string()), Vec::new()),
            "this run's desk taken back, W's kept: {waiting}"
        );
    }

    // CONTROL: a write that lands, and no layout at all.
    let mut wrote = false;
    assert_eq!(
        super::leave_cold_lane_layout(Some(&layout), |_| {
            wrote = true;
            Ok(())
        })
        .map_err(|error| error.to_string()),
        Ok(super::LayoutLeft::Written)
    );
    assert!(wrote);
    assert_eq!(
        super::leave_cold_lane_layout(None, |_| unreachable!()).map_err(|error| error.to_string()),
        Ok(super::LayoutLeft::Nothing)
    );
}

/// The record a settled layout write leaves: why the new version opens
/// without those tabs, in Messages, written before the switch.
///
/// ROUND EIGHT, U48's REVIEW: ON RECORD ONCE THE SWITCH IS GRANTED, AND
/// ONCE PER TARGET BUILD AND CAUSE. It was written before the operator's
/// quiesce, so every attempt a busy operator turned back (retried on the
/// activity cadence) said the new version "starts without them" while the
/// old one ran on, and a record is never folded: one more per retry. It
/// is written after the quiesce now, just before the steps whose
/// message-log flush carries it, and an `exec` that then fails and is
/// retried does not write it again.
///
/// RED before the fix: the record came before the quiesce, and a retry
/// wrote a second.
#[test]
fn a_layout_not_kept_is_said_on_record_once_the_switch_is_granted() {
    const CAUSE: &str = "open restore lock: Permission denied (os error 13)";
    let records = |app: &App, needle: &str| {
        app.messages
            .log()
            .records()
            .filter(|record| {
                record.title == "Open tabs not kept for the update"
                    && record.detail.iter().any(|line| line.contains(needle))
            })
            .count()
    };
    let mut app = windowed_with_settings();
    app.say_cold_lane_tabs_not_kept(7, CAUSE);
    assert_eq!(
        records(&app, "Permission denied"),
        1,
        "the record names it, with its cause"
    );
    app.say_cold_lane_tabs_not_kept(7, CAUSE);
    assert_eq!(
        records(&app, "Permission denied"),
        1,
        "A RETRY OF THE SAME SWITCH DOES NOT RECORD IT AGAIN"
    );
    // CONTROLS: another cause, or another build, is news.
    app.say_cold_lane_tabs_not_kept(7, "open restore lock: Read-only file system");
    assert_eq!(records(&app, "Read-only file system"), 1);
    app.say_cold_lane_tabs_not_kept(8, CAUSE);
    assert_eq!(records(&app, "Permission denied"), 2);

    // THE WIRING: in the cold arm, after the operator's quiesce is granted
    // and before the steps that flush the message log; nowhere else.
    let (src, start) = attempt_entry_source();
    let quiesce = start
        .find(".map(crate::operator_host::ControlHandle::try_begin_update_quiesce)")
        .expect("the operator's quiesce");
    let said = start
        .find("self.say_cold_lane_tabs_not_kept(target_build, cause);")
        .expect("the record");
    let prepared = start
        .find("self.prepare_cold_exec(")
        .expect("the steps before the exec");
    assert!(
        quiesce < said && said < prepared,
        "said once the switch is granted, before the flush that carries it"
    );
    let shipped = &src[..src
        .find("mod cold_lane_layout_tests;")
        .expect("this module")];
    assert_eq!(
        shipped.matches("self.say_cold_lane_tabs_not_kept(").count(),
        1,
        "one caller outside the tests"
    );
}

/// This file's source, and the body of the attempt's entry,
/// `start_unix_update_handoff`, in it.
pub(super) fn attempt_entry_source() -> (&'static str, &'static str) {
    let src = include_str!("app_update_handoff.rs");
    let start = &src[src
        .find("fn start_unix_update_handoff(")
        .expect("the attempt's entry")..];
    let end = start[1..]
        .find("\n    fn ")
        .into_iter()
        .chain(start[1..].find("\n    pub(crate) fn "))
        .min()
        .expect("the entry ends");
    (src, &start[..end])
}

/// ROUND EIGHT, U49's REVIEW: A REFUSED TAKE-BACK IS OWED, NEVER DROPPED.
/// The cold lane's layout written for an `exec` that did not happen is
/// taken back waiting at most the main thread's bound for the restore
/// lock; refused there (another aterm holding it just then), this run's
/// desk stayed in `session.toml` while the process ran on. A later crash's
/// claim then set the journal's newer desk aside by this run's name and
/// reopened the stale one, and after a seamless update the desk read as
/// another instance's, which every later quit kept and the next cold
/// launch reopened beside the windows it stood for. The take-back is now
/// owed: the event loop tries it again, spaced and bounded, until it lands;
/// a switch that writes no newer desk of this run (a seamless park, a cold
/// `exec` with no layout to leave) tries it first and refuses while its
/// cause passes; a quit that writes no desk of this run tries it once
/// more; and a newer write of this run's desk (the next cold attempt's,
/// the quit's) replaces it.
///
/// RED before the fix: the refused take-back was logged and dropped, so
/// nothing was owed and the desk stayed after the lock let go.
#[test]
fn a_refused_take_back_is_owed_until_it_lands_or_a_newer_write_replaces_it() {
    use crate::native_document_host::HeldAdvisoryLock;
    use std::time::{Duration, Instant};
    const RUN: &str = "2-2";
    let budget = Duration::from_millis(50);
    let app_layout = windowed_with_settings()
        .cold_lane_layout()
        .expect("restore is on")
        .expect("layout");
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("session.toml");
    let other = crate::restore::RestoreManifest::new(app_layout.windows.clone());
    crate::restore::write_quit_to(&path, &other, Some("1-1")).expect("W's quit");
    let hold = || {
        HeldAdvisoryLock::lock(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.path().join(".session.toml.aterm-restore.lock"))
                .expect("the lock file"),
        )
        .expect("held")
    };
    let unwrite = |run: Option<&str>| crate::restore::unwrite_quit_to_within(&path, run, budget);
    let runs = || {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut runs: Vec<String> = crate::restore::quit_run_of(&text).into_iter().collect();
        runs.extend(crate::restore::quit_also_of(&text));
        runs
    };
    let write_this_runs_desk = || {
        crate::restore::write_quit_to_within(&path, &app_layout, Some(RUN), budget)
            .expect("this run's desk");
    };

    let mut app = windowed_with_settings();
    write_this_runs_desk();
    assert_eq!(runs(), ["2-2", "1-1"], "PRECONDITION: both desks");
    // The `exec` did not happen; another aterm holds the lock just then.
    let t0 = Instant::now();
    let holder = hold();
    assert!(
        app.take_back_cold_lane_layout_with(Some(RUN.to_string()), t0, unwrite)
            .is_err_and(|fault| fault.passing()),
        "refused at the bound, a passing refusal"
    );
    let first = app
        .owed_layout_take_back_at()
        .expect("THE REFUSED TAKE-BACK IS OWED");
    assert_eq!(first, t0 + super::OWED_TAKE_BACK_RETRY_FIRST);
    // Not due yet: nothing is tried.
    app.retry_owed_layout_take_back_with(first - Duration::from_millis(1), |_| {
        unreachable!("not due")
    });
    // Due, and still held: refused again, spaced further.
    app.retry_owed_layout_take_back_with(first, unwrite);
    let second = app.owed_layout_take_back_at().expect("still owed");
    assert_eq!(second, first + super::OwedTakeBack::spacing(2));
    assert!(super::OwedTakeBack::spacing(2) > super::OWED_TAKE_BACK_RETRY_FIRST);
    assert_eq!(
        super::OwedTakeBack::spacing(40),
        super::OWED_TAKE_BACK_RETRY_CAP,
        "spaced at most a minute"
    );
    // A seamless park would end this run: it tries now and refuses while
    // the cause passes, the terminal untouched.
    let refusal = app
        .settle_owed_layout_take_back_before_switch_with(t0, unwrite)
        .expect_err("a passing refusal refuses the park");
    assert!(refusal.contains("could not be taken back"), "{refusal}");
    assert_eq!(runs(), ["2-2", "1-1"], "nothing changed while it is held");
    // The holder lets go: the next try lands, this run's desk alone.
    drop(holder);
    let due = app.owed_layout_take_back_at().expect("owed");
    app.retry_owed_layout_take_back_with(due, unwrite);
    assert_eq!(app.owed_layout_take_back_at(), None, "landed: nothing owed");
    assert_eq!(runs(), ["1-1"], "THIS RUN'S DESK TAKEN BACK, W'S KEPT");
    // CONTROL: with nothing owed the park tries nothing.
    assert_eq!(
        app.settle_owed_layout_take_back_before_switch_with(t0, |_| unreachable!("owed none")),
        Ok(())
    );

    // A NEWER WRITE OF THIS RUN'S DESK REPLACES IT: the next cold attempt's
    // write, or the quit's.
    write_this_runs_desk();
    let holder = hold();
    let _ = app.take_back_cold_lane_layout_with(Some(RUN.to_string()), t0, unwrite);
    assert!(app.owed_layout_take_back_at().is_some(), "owed");
    app.cold_lane_layout_rewritten();
    assert_eq!(
        app.owed_layout_take_back_at(),
        None,
        "replaced by the rewrite"
    );
    let _ = app.take_back_cold_lane_layout_with(Some(RUN.to_string()), t0, unwrite);
    app.settle_owed_layout_take_back_at_quit_with(true, |_| unreachable!("the quit wrote"));
    assert_eq!(
        app.owed_layout_take_back_at(),
        None,
        "replaced by the quit's desk"
    );
    // …and a quit that writes no desk of this run takes it back.
    let _ = app.take_back_cold_lane_layout_with(Some(RUN.to_string()), t0, unwrite);
    drop(holder);
    app.settle_owed_layout_take_back_at_quit_with(false, unwrite);
    assert_eq!(app.owed_layout_take_back_at(), None);
    assert_eq!(runs(), ["1-1"], "the quit took this run's desk back");

    // THE WIRING: a cold `exec` that writes no newer desk of this run tries
    // it before the operator's quiesce, and the seamless lane before the
    // park changes anything; a write of this run's desk replaces it.
    let (_, start) = attempt_entry_source();
    let switches = start
        .match_indices("self.settle_owed_layout_take_back_before_switch(")
        .map(|(at, _)| at)
        .collect::<Vec<_>>();
    assert_eq!(switches.len(), 2, "the cold exec and the seamless park");
    let at = |needle: &str| start.find(needle).expect(needle);
    assert!(
        at("self.cold_lane_layout_rewritten();") < switches[0]
            && switches[0]
                < at(".map(crate::operator_host::ControlHandle::try_begin_update_quiesce)")
    );
    assert!(
        at("crate::seamless::handoff_commit_stamp(&target_commit)") < switches[1]
            && switches[1] < at("self.begin_update_installing(")
    );

    // A SETTLED cause goes on past the switch (logged): no retry changes
    // it, and an update held on it had no end (U48).
    let _ = app.take_back_cold_lane_layout_with(Some(RUN.to_string()), t0, |_| {
        Err(crate::restore::RestoreFault::settled(
            "no data dir for session restore",
        ))
    });
    assert_eq!(
        app.settle_owed_layout_take_back_before_switch_with(t0, |_| {
            Err(crate::restore::RestoreFault::settled(
                "no data dir for session restore",
            ))
        }),
        Ok(())
    );
}

/// Round eight, #70, the cold lanes: a resident operator BUSY at the
/// replacement — a proposal in flight at the quiesce, or its gate held for a
/// moment at the `exec`'s permit — defers the attempt as activity, which the
/// synchronous arm retries and charges nothing, exactly as the seamless
/// Commit files the same answer. It was `Failed`, which that arm charges to
/// the physical budget. A refusal that is no work in flight (shutting down)
/// stays the failure it was.
///
/// RED before the fix: every operator refusal was minted `failed`, so the
/// first assertion fails (measured by making `operator_refused` answer
/// `failed` again).
#[test]
fn a_busy_resident_operator_defers_a_cold_replacement_and_charges_nothing() {
    use crate::native_updater_service::ApplyMode;
    let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(8);
    let operator =
        crate::operator_host::ControlHandle::new("test-cold-operator-busy".to_string(), notify_tx);
    let proposal = operator.begin_action_activity().expect("a proposal starts");
    let busy = operator
        .try_begin_update_quiesce()
        .err()
        .expect("a proposal is in flight");
    for what in ["could not quiesce for replacement", "revoked replacement"] {
        let error = crate::UpdateHandoffStartError::operator_refused(busy, what);
        assert!(error.is_activity_deferred(), "activity: {error:?}");
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            assert!(error.stops_routinely(mode), "routine: {error}");
        }
        assert!(
            error.to_string().contains("transaction is in flight"),
            "{error}"
        );
    }
    drop(proposal);

    // NEGATIVE CONTROL: shutting down is no work in flight — a failure.
    operator.begin_shutdown();
    let shut = operator
        .try_begin_update_quiesce()
        .err()
        .expect("a shutting-down operator refuses");
    let error = crate::UpdateHandoffStartError::operator_refused(shut, "revoked replacement");
    assert!(
        !error.is_activity_deferred() && !error.is_refused(),
        "a failure, as before: {error:?}"
    );
}
