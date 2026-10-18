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
    assert!(app.cold_lane_layout().is_err(), "nothing would reopen them");
    // Control: a headless instance has no windows to leave.
    let mut headless = windowed_with_settings();
    headless.headless = true;
    assert_eq!(headless.cold_lane_layout(), Ok(None));
}

/// A failed layout write is this machine's condition, never a `Failed` charged to the
/// artifact. A passing cause (full disk, I/O error, another aterm holding the restore lock past
/// the main thread's bound) refuses and is retried; a settled one (unwritable data folder,
/// read-only volume, no data dir) lets the lane go on without the layout and say why.
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

    // Published, then failed (its directory's sync): `Written` whatever the cause. An `exec`
    // that does not happen takes back this run's desk alone; another instance's stays.
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

    // Control: a write that lands, and no layout at all.
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
/// ROUND EIGHT, U48's REVIEW: ONCE PER TARGET BUILD AND CAUSE. A record is
/// never folded, so an `exec` that fails and is retried must not write it
/// again.
#[test]
fn a_layout_not_kept_is_said_on_record_once_the_switch_is_granted() {
    const CAUSE: &str = "open restore lock: Permission denied (os error 13)";
    let records = |app: &App, needle: &str| {
        app.messages
            .log()
            .records()
            .filter(|record| record.detail.iter().any(|line| line.contains(needle)))
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
    // Controls: another cause, or another build, is news.
    app.say_cold_lane_tabs_not_kept(7, "open restore lock: Read-only file system");
    assert_eq!(records(&app, "Read-only file system"), 1);
    app.say_cold_lane_tabs_not_kept(8, CAUSE);
    assert_eq!(records(&app, "Permission denied"), 2);
}

/// A refused take-back of the cold lane's layout (the restore lock held past the main thread's
/// bound) is owed, never dropped: a desk left behind reopens as another instance's. The event
/// loop retries it, spaced and bounded; a switch that writes no newer desk of this run tries it
/// first and refuses while its cause passes; a quit tries it once more; a newer write of this
/// run's desk replaces it.
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
    // A seamless park tries now and refuses while the cause passes, the terminal untouched.
    assert!(
        app.settle_owed_layout_take_back_before_switch_with(t0, unwrite)
            .is_err(),
        "a passing refusal refuses the park"
    );
    assert_eq!(runs(), ["2-2", "1-1"], "nothing changed while it is held");
    // The holder lets go: the next try lands, this run's desk alone.
    drop(holder);
    let due = app.owed_layout_take_back_at().expect("owed");
    app.retry_owed_layout_take_back_with(due, unwrite);
    assert_eq!(app.owed_layout_take_back_at(), None, "landed: nothing owed");
    assert_eq!(runs(), ["1-1"], "THIS RUN'S DESK TAKEN BACK, W'S KEPT");
    // Control: with nothing owed the park tries nothing.
    assert_eq!(
        app.settle_owed_layout_take_back_before_switch_with(t0, |_| unreachable!("owed none")),
        Ok(())
    );

    // A newer write of this run's desk (the next cold attempt's, or the quit's) replaces it.
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

    // A settled cause goes on past the switch (logged): no retry changes it.
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
