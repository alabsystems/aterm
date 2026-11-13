// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// THE PARKED BIT AND THE EPHEMERAL MARK ROUND-TRIP, EACH ON ITS OWN, AND A RECORD WITH
/// NEITHER READS AS A PERSON'S SHOWN TAB: a person's tab writes neither key, so an older
/// build's record reads as one.
#[test]
fn the_parked_bit_and_the_mark_round_trip_and_an_old_record_reads_as_a_shown_tab() {
    let tab = |parked, ephemeral| RestoredTab {
        root: RestoredSplitTree::leaf(RestoredView::Placeholder(PlaceholderLeafRestore {
            restore_tag: "gone".to_string(),
            reason: "gone".to_string(),
            metadata: String::new(),
            unhanded: None,
        })),
        focused_path: Vec::new(),
        zoomed: false,
        parked,
        ephemeral,
        fleet: false,
    };
    let shown = aterm_toml::to_string(&tab(false, false)).expect("serializes");
    assert!(
        !shown.contains("parked") && !shown.contains("ephemeral"),
        "{shown}"
    );
    assert_eq!(
        aterm_toml::from_str::<RestoredTab>(&shown).expect("an old record reads"),
        tab(false, false)
    );
    for (parked, ephemeral) in [(true, true), (false, true), (true, false)] {
        let wrote = aterm_toml::to_string(&tab(parked, ephemeral)).expect("serializes");
        assert_eq!(wrote.contains("ephemeral"), ephemeral, "{wrote}");
        assert_eq!(
            aterm_toml::from_str::<RestoredTab>(&wrote).expect("reads"),
            tab(parked, ephemeral)
        );
    }
}

#[cfg(unix)]
#[test]
fn the_restore_lock_is_free_while_a_child_forked_under_it_has_yet_to_exec() {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("session.toml");
    let armed = crate::parked_fork::arm_at(1);
    with_restore_lock::<_, String>(&path, || Ok(())).expect("an uncontended restore lock");
    let child = armed.child();
    let lock_path = restore_lock_path(&path).expect("a lock path");
    let next = crate::native_document_host::HeldAdvisoryLock::try_lock(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("the lock file"),
    );
    assert!(
        next.is_ok(),
        "the restore lock stayed taken in the child forked under it: released by \
         the close alone"
    );
    drop(next);
    assert!(child.still_parked(), "…while the child had yet to exec");
    child.release();
}

#[cfg(unix)]
#[test]
fn the_cold_lanes_layout_write_waits_a_bounded_time_for_the_restore_lock() {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("session.toml");
    let budget = std::time::Duration::from_millis(100);
    let holder = crate::native_document_host::HeldAdvisoryLock::lock(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(restore_lock_path(&path).expect("a lock path"))
            .expect("the lock file"),
    )
    .expect("held");
    let (tx, rx) = std::sync::mpsc::channel();
    let writer = {
        let path = path.clone();
        std::thread::spawn(move || {
            let wrote = write_quit_to_within(&path, &sample(), Some("1-2"), budget);
            let took = unwrite_quit_to_within(&path, Some("1-2"), budget);
            let _ = tx.send((wrote, took));
        })
    };
    let answered = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    drop(holder);
    let (wrote, took) = answered.expect("THE WRITE ANSWERED WITHIN ITS BOUND");
    writer.join().expect("the writer");
    let fault = wrote.expect_err("refused while another aterm holds the lock");
    assert!(fault.passing(), "a passing refusal, retried: {fault}");
    let fault = took.expect_err("the take-back is bounded too");
    assert!(fault.passing(), "{fault}");
    assert!(!path.exists(), "nothing was written");
    write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("written");
    assert_eq!(
        take_from(&path).map(|manifest| manifest.windows.len()),
        Some(2)
    );
    write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("written");
    unwrite_quit_to_within(&path, Some("1-2"), budget).expect("taken back");
    assert!(!path.exists(), "its own desk, taken back");
}

#[cfg(unix)]
#[test]
fn the_restore_writes_name_their_flush() {
    use aterm_durable::seam::Recording;
    const O: Flush = Flush::Ordered;
    const D: Flush = Flush::Durable;
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("session.toml");
    let budget = std::time::Duration::from_secs(60);
    let recording = Recording::start();
    write_quit_to(&path, &sample(), Some("1-2")).expect("the quit's layout");
    assert_eq!(recording.take(), [D, D], "the quit's file and its entry");
    write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("cold lane");
    assert_eq!(
        recording.take(),
        [O, O],
        "the cold lane's file and its entry"
    );
    unwrite_quit_to_within(&path, Some("1-2"), budget).expect("taken back");
    assert_eq!(recording.take(), [O], "the take-back's removal");
    write_quit_to(&path, &sample(), Some("1-1")).expect("the other quit");
    write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("cold lane");
    let _ = recording.take();
    unwrite_quit_to_within(&path, Some("1-2"), budget).expect("taken back");
    assert_eq!(
        recording.take(),
        [O, O],
        "the take-back's rewrite and its entry"
    );
    assert!(path.exists(), "the other instance's desk stays");
    assert!(
        take_quit_from(&path).is_some(),
        "the single-use take takes it"
    );
    assert_eq!(recording.take(), [D, O], "the claim and the cleanup");
    write_quit_to(&path, &sample(), Some("1-1")).expect("the other quit");
    let _ = recording.take();
    let run = crate::crash_journal::JournalId::now(std::process::id());
    let (taken, held) = take_holding_from(&path, run, &no_evidence());
    assert!(taken.is_some(), "the launch takes it");
    assert_eq!(
        recording.take(),
        [D],
        "the claim alone: the take stays until settled"
    );
    held.settle();
    assert!(
        recording.take().is_empty(),
        "the settle's removal syncs nothing"
    );
    let sidecar = dir.path().join("seamless-1-2.layout.toml");
    write_wire_once_to(&sidecar, &sample().to_toml().expect("wire")).expect("sidecar");
    assert_eq!(
        recording.take(),
        [O, O],
        "the handoff's sidecar and its entry"
    );
    let metrics = dir.path().join("cell-metrics.toml");
    store_cell_metrics_to(&metrics, "font", 2.0, 13.0, 8, 16).expect("metrics");
    assert_eq!(recording.take(), [O, O], "the cell-metrics cache");
}

#[cfg(unix)]
fn no_evidence() -> Evidence<'static> {
    Evidence {
        log_dir: None,
        swept: &[],
    }
}

#[cfg(unix)]
fn evidence_in(logs: &Path) -> Evidence<'_> {
    Evidence {
        log_dir: Some(logs),
        swept: &[],
    }
}

#[cfg(unix)]
fn takes_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("the data dir")
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| parse_take_name(OsStr::new("session.toml"), name).is_some())
        .collect();
    names.sort();
    names
}

#[cfg(unix)]
fn take_of(kind: TakeKind, run: JournalId, n: usize) -> String {
    take_name(OsStr::new("session.toml"), kind, run, n)
}

#[cfg(unix)]
fn dead_run() -> JournalId {
    JournalId::now(crate::crash_journal::tests::reaped_pid())
}

#[cfg(unix)]
#[test]
fn a_launch_take_is_held_until_its_run_settles_and_never_taken_while_it_lives() {
    use crate::crash_journal::tests::{marker, scratch};
    let (dir, logs) = (scratch("rt-dir"), scratch("rt-logs"));
    let path = dir.path().join("session.toml");
    write_quit_to(&path, &sample(), Some("1-2")).expect("the quit's layout");
    let a = dead_run();
    let (taken, held) = take_holding_from(&path, a, &evidence_in(logs.path()));
    let taken = taken.expect("the quit's layout");
    assert_eq!(taken.manifest, sample());
    assert_eq!(taken.run.as_deref(), Some("1-2"));
    assert!(!path.exists(), "no longer under its public name");
    assert_eq!(takes_in(dir.path()), [take_of(TakeKind::Taken, a, 0)]);
    marker(logs.path(), a);
    let (beside, beside_held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
    assert!(beside.is_none(), "a live run's take is never taken");
    assert_eq!(takes_in(dir.path()), [take_of(TakeKind::Taken, a, 0)]);
    drop(beside_held);
    held.settle();
    assert!(takes_in(dir.path()).is_empty(), "settled, nothing is left");
    let (after, _) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
    assert!(after.is_none());
}

#[cfg(unix)]
#[test]
fn a_dead_runs_take_comes_back_once_when_its_run_ended_uncleanly() {
    use crate::crash_journal::tests::{cwds, layout, marker, scratch};
    let (dir, logs) = (scratch("rt-dir"), scratch("rt-logs"));
    let path = dir.path().join("session.toml");
    let lost = layout(&[("/work/lost", "zsh")]);
    let quit = layout(&[("/work/quit", "zsh")]);

    write_quit_to(&path, &lost, Some("1-2")).expect("the quit's layout");
    assert!(take_quit_from(&path).is_some(), "a single-use take");
    assert!(
        take_holding_from(&path, dead_run(), &evidence_in(logs.path()))
            .0
            .is_none(),
        "the single-use take left nothing behind"
    );

    write_quit_to(&path, &lost, Some("1-2")).expect("the quit's layout");
    let a = dead_run();
    let (_, held) = take_holding_from(&path, a, &evidence_in(logs.path()));
    marker(logs.path(), a);
    drop(held); // A dies before it settles: its lock goes with it.
    write_quit_to(&path, &quit, Some("7-8")).expect("another run's quit");
    let b = dead_run();
    let (taken, held) = take_holding_from(&path, b, &evidence_in(logs.path()));
    let taken = taken.expect("B takes both");
    assert_eq!(cwds(&taken.manifest), ["/work/quit", "/work/lost"]);
    assert_eq!(taken.run.as_deref(), Some("7-8"), "the quit's run leads");
    assert_eq!(taken.also, ["1-2"], "the run A's layout was the quit of");
    assert_eq!(
        takes_in(dir.path()),
        [
            take_of(TakeKind::Retaken, b, 0),
            take_of(TakeKind::Taken, b, 0)
        ]
    );

    marker(logs.path(), b);
    drop(held); // B dies before it settles too.
    let (taken, held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
    let taken = taken.expect("the quit layout B took first comes back");
    assert_eq!(
        cwds(&taken.manifest),
        ["/work/quit"],
        "and the layout B took again does not"
    );
    assert_eq!(
        takes_in(dir.path()).len(),
        1,
        "set aside: {:?}",
        takes_in(dir.path())
    );
    held.settle();
    assert!(takes_in(dir.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn a_dead_runs_take_goes_when_its_end_was_clean_or_its_desk_is_held_elsewhere() {
    use crate::crash_journal::tests::{cwds, layout, marker, scratch};
    let (dir, logs) = (scratch("rt-dir"), scratch("rt-logs"));
    let path = dir.path().join("session.toml");
    let lost = layout(&[("/work/lost", "zsh")]);
    let died_holding = |a: JournalId, marked: bool| {
        write_quit_to(&path, &lost, Some("1-2")).expect("the quit's layout");
        drop(take_holding_from(&path, a, &no_evidence()).1);
        if marked {
            marker(logs.path(), a);
        }
    };

    died_holding(dead_run(), false);
    let (taken, _) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
    assert!(taken.is_none(), "a clean end's take goes");
    assert!(takes_in(dir.path()).is_empty());

    let a = dead_run();
    died_holding(a, true);
    let own = layout(&[("/work/own", "zsh")]);
    write_quit_to(&path, &own, Some(&a.stem())).expect("A's own quit, cut short");
    let (taken, held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
    assert_eq!(cwds(&taken.expect("A's quit").manifest), ["/work/own"]);
    held.settle();
    assert!(takes_in(dir.path()).is_empty(), "A's take went");

    let a = dead_run();
    died_holding(a, true);
    fs::write(dir.path().join(a.file_name()), b"").expect("A's crash journal");
    let (taken, _) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
    assert!(taken.is_none(), "A's journal holds the desk");
    assert!(takes_in(dir.path()).is_empty());
    fs::remove_file(dir.path().join(a.file_name())).expect("A's crash journal");

    let a = dead_run();
    died_holding(a, true);
    let next_image = JournalId {
        pid: a.pid,
        nanos: a.nanos + 1,
    };
    let (taken, _) = take_holding_from(&path, next_image, &evidence_in(logs.path()));
    assert!(taken.is_none(), "this process's previous image's take goes");
    assert!(takes_in(dir.path()).is_empty());

    write_quit_to(&path, &lost, Some("1-2")).expect("the quit's layout");
    let y = dead_run();
    let (_, y_held) = take_holding_from(&path, y, &no_evidence());
    write_quit_to(&path, &own, Some(&y.stem())).expect("Y's quit");
    let a = dead_run();
    let (_, a_held) = take_holding_from(&path, a, &no_evidence());
    assert_eq!(takes_in(dir.path()).len(), 2, "both held");
    for (run, held) in [(y, y_held), (a, a_held)] {
        marker(logs.path(), run);
        drop(held);
    }
    let (taken, held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
    assert_eq!(
        cwds(&taken.expect("A's take").manifest),
        ["/work/own"],
        "Y's desk once, never what Y took beside it"
    );
    held.settle();
    assert!(takes_in(dir.path()).is_empty(), "Y's take went");

    died_holding(dead_run(), true);
    let (taken, held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
    assert_eq!(cwds(&taken.expect("comes back").manifest), ["/work/lost"]);
    held.settle();
}

#[cfg(unix)]
#[test]
fn a_take_whose_sync_fails_restores_nothing_and_holds_what_it_took() {
    use crate::crash_journal::tests::{marker, scratch};
    use std::os::unix::fs::PermissionsExt as _;
    let (dir, logs) = (scratch("rt-dir"), scratch("rt-logs"));
    let path = dir.path().join("session.toml");
    write_quit_to(&path, &sample(), Some("1-2")).expect("the quit's layout");
    let a = dead_run();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o300)).expect("chmod");
    let (taken, held) = take_holding_from(&path, a, &evidence_in(logs.path()));
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
    assert!(
        taken.is_none(),
        "nothing comes back past an indeterminate claim"
    );
    assert_eq!(takes_in(dir.path()), [take_of(TakeKind::Taken, a, 0)]);
    marker(logs.path(), a);
    let (beside, _) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
    assert!(beside.is_none(), "a live run's take is never taken");
    assert_eq!(takes_in(dir.path()), [take_of(TakeKind::Taken, a, 0)]);
    held.settle();
    assert!(takes_in(dir.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn a_restore_write_fault_says_whether_its_cause_passes() {
    for errno in [
        libc::ENOSPC,
        libc::EDQUOT,
        libc::EIO,
        libc::EINTR,
        libc::EAGAIN,
        libc::EMFILE,
        libc::ENFILE,
        libc::ENOMEM,
    ] {
        let error = std::io::Error::from_raw_os_error(errno);
        assert!(RestoreFault::io("write", &error).passing(), "{error}");
    }
    for errno in [
        libc::EACCES,
        libc::EPERM,
        libc::EROFS,
        libc::ENOTSUP,
        libc::ENOLCK,
        libc::ENOENT,
    ] {
        let error = std::io::Error::from_raw_os_error(errno);
        assert!(!RestoreFault::io("write", &error).passing(), "{error}");
    }
    assert!(!RestoreFault::from("serialize".to_string()).passing());
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let locked = dir.path().join("aterm");
        fs::create_dir(&locked).expect("the data folder");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).expect("read-only");
        let fault = write_quit_to_within(
            &locked.join("session.toml"),
            &sample(),
            Some("1-2"),
            std::time::Duration::from_millis(100),
        )
        .expect_err("refused");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).expect("restored");
        assert!(!fault.passing(), "settled: {fault}");
    }
}

fn terminal(id: Option<u64>, title: &str) -> RestoredSplitTree {
    RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
        sid: None,
        lineage: None,
        log_ep: None,
        cwd: Some("/tmp/work".to_string()),
        title: title.to_string(),
        profile: None,
        local_id: id,
        user_title: None,
        description: None,
        icon: None,
        role: None,
        attention: None,
        questions: None,
        autopilot: None,
        land_at: None,
        autopilot_by: None,
        theme: None,
        agent: None,
        supervised: None,
        lost: None,
        held: false,
        holds: Vec::new(),
        claim_ended: false,
        not_claim_worker: false,
        claim_worker: None,
        task: None,
    }))
}

/// The row a handoff carries for a window that held only an empty Workers list reads as a
/// layout and round-trips as `outgoing_layout_wire` requires, unlike a bare row with no tab.
#[cfg(unix)]
#[test]
fn a_lone_workers_list_row_is_a_layout_and_round_trips() {
    let lone = WindowLayout::lone_workers_list(24, 80);
    assert!(lone.is_lone_workers_list() && lone.holds_no_tab() && lone.holds_no_terminal());
    let desk = RestoreManifest::new(vec![lone.clone()]);
    assert!(!desk.is_empty(), "a lone list is something to place");
    assert!(desk.carries_lone_workers_list());
    let wire = desk.to_toml().expect("the row serializes");
    assert_eq!(
        RestoreManifest::from_toml(&wire).as_ref(),
        Some(&desk),
        "the parent's own read-back holds"
    );

    let bare = WindowLayout {
        workers_in_front: false,
        ..lone.clone()
    };
    assert!(!bare.is_lone_workers_list());
    let nothing = RestoreManifest::new(vec![bare.clone()]);
    assert!(nothing.is_empty(), "CONTROL: a window with nothing in it");
    assert!(!nothing.carries_lone_workers_list());

    let mut beside_a_tab = held_desk(None);
    let tabbed = beside_a_tab.windows[0].clone();
    assert!(!tabbed.is_lone_workers_list(), "CONTROL: it has a tab");
    beside_a_tab.windows.push(lone.clone());
    assert!(beside_a_tab.carries_lone_workers_list());

    for (row, keeps_lists, kept) in [
        (&lone, true, true),
        (&lone, false, false),
        (&bare, true, false),
        (&bare, false, false),
        (&tabbed, true, true),
        (&tabbed, false, true),
    ] {
        assert_eq!(
            row.kept_in_layout(keeps_lists),
            kept,
            "{row:?} with lone lists kept={keeps_lists}"
        );
    }
    assert!(
        !tabbed.holds_no_terminal(),
        "CONTROL: a window with terminals restores terminals"
    );
}

fn held_desk(exited: Option<u64>) -> RestoreManifest {
    let mut settings = NativeLeafRestore::settings("/text-fonts".to_string());
    settings.settings_drafts = vec![SettingsDraftRestore {
        key: "font_family".to_string(),
        text: "Carried Mono".to_string(),
    }];
    RestoreManifest::new(vec![WindowLayout {
        rows: 24,
        cols: 80,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: WindowShow::UNKNOWN,
        tabs: vec![PaneLayout::Split {
            dir: SplitKind::Vertical,
            ratio: 0.5,
            first: Box::new(PaneLayout::Leaf {
                cwd: Some("/tmp/work".to_string()),
                title: "zsh".to_string(),
                focused: false,
                local_id: Some(0),
            }),
            second: Box::new(PaneLayout::Leaf {
                cwd: Some("/tmp/work".to_string()),
                title: "build".to_string(),
                focused: true,
                local_id: exited,
            }),
        }],
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(1),
        workers_in_front: false,
        restored_tabs: vec![
            RestoredTab {
                root: RestoredSplitTree::Split {
                    axis: SplitKind::Vertical,
                    ratio: 0.5,
                    first: Box::new(terminal(Some(0), "zsh")),
                    second: Box::new(terminal(exited, "build")),
                },
                focused_path: vec![RestoreBranch::Second],
                zoomed: false,
                parked: false,
                ephemeral: false,
                fleet: false,
            },
            RestoredTab {
                root: RestoredSplitTree::leaf(RestoredView::Native(settings)),
                focused_path: Vec::new(),
                zoomed: false,
                parked: false,
                ephemeral: false,
                fleet: false,
            },
        ],
    }])
}

#[test]
fn the_pane_in_front_is_each_windows_active_tabs_focused_terminal() {
    let mut desk = held_desk(Some(7));
    assert_eq!(desk.windows[0].active_item, Some(1));
    assert!(desk.front_terminal_ids().is_empty());
    desk.windows[0].active_item = Some(0);
    assert_eq!(desk.front_terminal_ids(), vec![7]);
    desk.windows[0].restored_tabs[0].focused_path = vec![RestoreBranch::First];
    assert_eq!(desk.front_terminal_ids(), vec![0]);
    let mut other = held_desk(Some(9)).windows.remove(0);
    other.active_item = Some(0);
    desk.windows.push(other);
    assert_eq!(desk.front_terminal_ids(), vec![0, 9]);
    desk.windows[1].restored_tabs[0].root = RestoredSplitTree::Split {
        axis: SplitKind::Vertical,
        ratio: 0.5,
        first: Box::new(terminal(Some(8), "zsh")),
        second: Box::new(terminal(None, "build")),
    };
    desk.windows[0].active_item = None;
    assert!(desk.front_terminal_ids().is_empty());
}

#[test]
fn a_layout_naming_an_unhanded_pane_is_placed_with_that_pane_retired() {
    let wire = held_desk(Some(7))
        .to_toml()
        .expect("the outgoing capture serializes");
    let parsed = RestoreManifest::from_toml(&wire).expect("the successor parses it");
    assert!(
        !parsed.covers_exact_seamless_ids(&[0]),
        "CONTROL: the exact check refuses the layout for the exited pane"
    );
    let placed = parsed
        .clone()
        .placed_for_handed(&[0])
        .expect("placed, with the unhanded pane retired");
    assert_eq!(placed.seamless_terminal_ids(), Some(vec![0]));
    assert!(placed.covers_exact_seamless_ids(&[0]));
    let window = &placed.windows[0];
    assert!(
        window.shape_is_valid(),
        "the placed layout is one from_toml admits"
    );
    let tab = &window.restored_tabs[0];
    assert_eq!(
        tab.focused_path,
        vec![RestoreBranch::Second],
        "the focus is kept"
    );
    let RestoredSplitTree::Split { first, second, .. } = &tab.root else {
        panic!("the split is kept: {:?}", tab.root);
    };
    assert!(matches!(
        &**first,
        RestoredSplitTree::Leaf { view: RestoredView::Terminal(live) } if live.local_id == Some(0)
    ));
    let RestoredSplitTree::Leaf {
        view: RestoredView::Placeholder(retired),
    } = &**second
    else {
        panic!("the unhanded pane is a placeholder: {second:?}");
    };
    assert_eq!(retired.restore_tag, "terminal");
    assert_eq!(retired.reason, crate::update_words::PANE_NOT_CARRIED);
    assert!(retired.metadata.contains("build"), "{}", retired.metadata);
    assert_eq!(
        retired.unhanded_local_id(),
        Some(7),
        "the placeholder names the pane a held screen is matched by"
    );
    assert!(
        !placed
            .to_toml()
            .expect("the placed layout serializes")
            .contains("unhanded"),
        "and that id never reaches a wire"
    );
    assert_eq!(
        placed.carried_settings_drafts(),
        (
            vec![SettingsDraftRestore {
                key: "font_family".to_string(),
                text: "Carried Mono".to_string(),
            }],
            0
        ),
        "the Settings draft is placed with its tab"
    );
    let PaneLayout::Split { first, second, .. } = &window.tabs[0] else {
        panic!("the legacy mirror is kept");
    };
    assert_eq!(
        (first.local_id(), second.local_id()),
        (Some(0), None),
        "the legacy mirror names no session that was not handed"
    );
    assert_eq!(
        window.bootstrap_local_id(),
        Some(0),
        "the bootstrap pick is the handed shell"
    );

    let exact = RestoreManifest::from_toml(&held_desk(Some(7)).to_toml().unwrap()).unwrap();
    assert_eq!(exact.clone().placed_for_handed(&[0, 7]), Ok(exact));
}

/// A row whose shells were all retired at intake holds no terminal, though it keeps its
/// legacy mirror: the restore reads `restored_tabs`, and so does the first paint's guard.
#[test]
fn a_row_retired_at_intake_holds_no_terminal() {
    let parsed = RestoreManifest::from_toml(&held_desk(Some(7)).to_toml().unwrap()).unwrap();
    assert!(
        !parsed.windows[0].holds_no_terminal(),
        "CONTROL: two shells"
    );
    let retired = parsed
        .clone()
        .placed_for_handed(&[])
        .expect("a layout with no shell handed is placed");
    assert!(
        !retired.windows[0].tabs.is_empty(),
        "PRECONDITION: the mirror"
    );
    assert!(retired.windows[0].holds_no_terminal());
    let one = parsed.placed_for_handed(&[0]).expect("one shell handed");
    assert!(
        !one.windows[0].holds_no_terminal(),
        "CONTROL: one shell stays"
    );
}

#[test]
fn a_layout_missing_a_handed_session_stays_unplaced() {
    let parsed = RestoreManifest::from_toml(&held_desk(Some(7)).to_toml().unwrap()).unwrap();
    assert!(
        parsed.clone().placed_for_handed(&[0, 5]).is_err(),
        "5 is not named"
    );
    assert!(
        parsed.clone().placed_for_handed(&[0, 0]).is_err(),
        "0 twice"
    );
    let unnamed = RestoreManifest::from_toml(&held_desk(None).to_toml().unwrap()).unwrap();
    assert!(
        unnamed.placed_for_handed(&[0]).is_err(),
        "a leaf with no id"
    );
}

#[test]
fn a_layout_naming_a_session_in_two_windows_is_placed_whole() {
    let mut desk = held_desk(Some(7));
    desk.windows.push(shells_window(0..1));
    let parsed = RestoreManifest::from_toml(&desk.to_toml().unwrap())
        .expect("the successor parses a co-viewed layout");
    assert_eq!(parsed.seamless_terminal_ids(), Some(vec![0, 7]));
    let placed = parsed
        .placed_for_handed(&[0, 7])
        .expect("a co-viewed session does not unplace the layout");
    assert_eq!(placed.windows.len(), 2, "both windows are kept");
}

fn sample() -> RestoreManifest {
    RestoreManifest::new(vec![
        WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 1,
            outer_x: Some(120),
            outer_y: Some(64),
            maximized: Some(true),
            inner_px: None,
            presence_row: false,
            show: WindowShow::UNKNOWN,
            tabs: vec![
                PaneLayout::leaf(Some("/home/a".into()), "zsh".into(), true),
                PaneLayout::Split {
                    dir: SplitKind::Vertical,
                    ratio: 0.6,
                    first: Box::new(PaneLayout::leaf(Some("/tmp".into()), "vim".into(), true)),
                    second: Box::new(PaneLayout::leaf(None, "top".into(), false)),
                },
            ],
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: None,
            workers_in_front: false,
            restored_tabs: Vec::new(),
        },
        WindowLayout {
            rows: 40,
            cols: 132,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            inner_px: None,
            presence_row: false,
            show: WindowShow::UNKNOWN,
            tabs: vec![PaneLayout::leaf(Some("/".into()), "sh".into(), true)],
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: None,
            workers_in_front: false,
            restored_tabs: Vec::new(),
        },
    ])
}

fn mixed_sample(uri: &str) -> RestoreManifest {
    RestoreManifest::new(vec![WindowLayout {
        rows: 32,
        cols: 110,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: WindowShow::UNKNOWN,
        tabs: vec![PaneLayout::leaf(Some("/tmp".into()), "shell".into(), true)],
        native_tabs: vec![
            NativeTabRestore::Settings {
                route: "/about".to_string(),
            },
            NativeTabRestore::Editor {
                uri: uri.to_string(),
            },
        ],
        tab_order: vec![
            TabOrderEntry::Native { index: 0 },
            TabOrderEntry::Terminal { index: 0 },
            TabOrderEntry::Native { index: 1 },
        ],
        active_item: Some(2),
        workers_in_front: false,
        restored_tabs: Vec::new(),
    }])
}

fn draft(key: &str, text: &str) -> SettingsDraftRestore {
    SettingsDraftRestore {
        key: key.to_string(),
        text: text.to_string(),
    }
}

fn settings_handoff_layout(drafts: Vec<SettingsDraftRestore>) -> RestoreManifest {
    let mut leaf = NativeLeafRestore::settings("/text-fonts".to_string());
    leaf.settings_drafts = drafts;
    let mut window = shells_window(0..0);
    window.restored_tabs.push(RestoredTab {
        root: RestoredSplitTree::leaf(RestoredView::Native(leaf)),
        focused_path: Vec::new(),
        zoomed: false,
        parked: false,
        ephemeral: false,
        fleet: false,
    });
    RestoreManifest::new(vec![window])
}

#[test]
fn settings_drafts_ride_the_handoff_layout_as_a_fixed_point() {
    let layout = settings_handoff_layout(vec![
        draft("font_family", "Menlo \"Nerd\"\nFont"),
        draft("shell", "/bin/zsh -l \u{3042}"),
    ]);
    let wire = layout.to_toml().expect("serialize");
    let parsed = RestoreManifest::from_toml(&wire).expect("the layout parses");
    assert_eq!(parsed, layout, "the capture is a fixed point of the parse");
    assert_eq!(parsed.to_toml().unwrap(), wire);
    let RestoredSplitTree::Leaf {
        view: RestoredView::Native(native),
    } = &parsed.windows[0].restored_tabs[0].root
    else {
        panic!("the Settings leaf");
    };
    assert_eq!(native.settings_drafts.len(), 2);
    assert_eq!(native.settings_drafts_unreadable, 0);
    assert!(
        !settings_handoff_layout(Vec::new())
            .to_toml()
            .unwrap()
            .contains("settings_drafts"),
        "a layout with no draft is byte-identical to what an older consumer reads"
    );
}

#[test]
fn an_unreadable_settings_draft_costs_the_draft_not_the_layout() {
    let parse = |tag: &str, extra: &str| -> NativeLeafRestore {
        let route = if tag == "settings" {
            "route = \"/home\"\n"
        } else {
            "uri = \"file:///tmp/notes.md\"\n"
        };
        let wire = format!("kind = \"native\"\nrestore_tag = \"{tag}\"\n{route}{extra}");
        let mut view: RestoredView = aterm_toml::from_str(&wire)
            .unwrap_or_else(|error| panic!("a draft never fails the leaf: {error:?}\n{wire}"));
        view.sanitize();
        let RestoredView::Native(native) = view else {
            panic!("the leaf stays native");
        };
        assert!(view_is_bounded(&native), "{native:?}");
        native
    };
    let garbage = parse("settings", "settings_drafts = \"garbage\"\n");
    assert!(garbage.settings_drafts.is_empty());
    assert_eq!(garbage.settings_drafts_unreadable, 1);

    let long = "x".repeat(MAX_SETTINGS_DRAFT_TEXT_BYTES + 1);
    let mixed = parse(
        "settings",
        &format!(
            "settings_drafts = [{{ key = \"font_family\", text = \"Mono\" }}, {{ key = 7, \
             text = \"x\" }}, \"loose\", {{ key = \"font_family\", text = \"again\" }}, \
             {{ key = \"shell\", text = \"a\\u0000b\" }}, {{ key = \"padding\", text = \
             \"{long}\" }}, {{ key = \"\", text = \"keyless\" }}]\n"
        ),
    );
    assert_eq!(mixed.settings_drafts, vec![draft("font_family", "Mono")]);
    assert_eq!(
        mixed.settings_drafts_unreadable, 6,
        "a wrong type, a stray value, a repeat, a NUL, an overlong text and an empty key"
    );

    let editor = parse(
        "editor",
        "settings_drafts = [{ key = \"font_family\", text = \"Mono\" }]\n",
    );
    assert!(
        editor.settings_drafts.is_empty(),
        "only a Settings leaf carries drafts"
    );
    assert_eq!(editor.settings_drafts_unreadable, 1);

    let over = parse(
        "settings",
        &format!(
            "settings_drafts = [{}]\n",
            (0..=MAX_SETTINGS_DRAFTS_PER_VIEW)
                .map(|index| format!("{{ key = \"k{index}\", text = \"v\" }}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    assert_eq!(over.settings_drafts.len(), MAX_SETTINGS_DRAFTS_PER_VIEW);
    assert_eq!(over.settings_drafts_unreadable, 1);
}

fn view_is_bounded(native: &NativeLeafRestore) -> bool {
    RestoredView::Native(native.clone()).bounded()
}

#[test]
fn window_show_state_round_trips_and_unknown_emits_nothing() {
    let mut shown = sample();
    shown.windows[0].show = WindowShow {
        fullscreen: Some(true),
        minimized: Some(false),
        z_order: Some(0),
        key: Some(true),
    };
    shown.windows[1].show = WindowShow {
        fullscreen: Some(false),
        minimized: Some(true),
        z_order: Some(1),
        key: Some(false),
    };
    let wire = shown.to_toml().expect("serialize");
    let back = RestoreManifest::from_toml(&wire).expect("parse");
    assert_eq!(
        back, shown,
        "the show state survives the round trip:\n{wire}"
    );
    assert_eq!(
        back.to_toml().expect("reserialize"),
        wire,
        "a manifest carrying show state is a byte fixed point of the codec"
    );

    let mut unknown = sample();
    for window in &mut unknown.windows {
        window.maximized = None;
    }
    let wire = unknown.to_toml().expect("serialize");
    assert!(
        !wire.contains("show") && !wire.contains("z_order") && !wire.contains("maximized"),
        "an unknown window state must not reach the wire:\n{wire}"
    );
    let back = RestoreManifest::from_toml(&wire).expect("parse");
    assert!(back.windows.iter().all(|w| w.show.is_unknown()));
    assert!(back.windows.iter().all(|w| w.maximized.is_none()));
}

#[test]
fn window_show_state_is_compatible_with_older_and_newer_peers() {
    let mut shown = sample();
    shown.windows[0].show = WindowShow {
        fullscreen: Some(true),
        minimized: Some(false),
        z_order: Some(0),
        key: Some(true),
    };
    let wire = shown.to_toml().expect("serialize");
    assert!(wire.contains("[windows.show]"), "wire: {wire}");

    #[derive(Deserialize)]
    struct OlderWindow {
        rows: u16,
        cols: u16,
    }
    #[derive(Deserialize)]
    struct OlderManifest {
        schema: u32,
        windows: Vec<OlderWindow>,
    }
    let older: OlderManifest =
        aterm_toml::from_str(&wire).expect("an older reader skips the show table");
    assert_eq!(older.schema, SCHEMA);
    assert_eq!(
        older
            .windows
            .iter()
            .map(|w| (w.rows, w.cols))
            .collect::<Vec<_>>(),
        vec![(24, 80), (40, 132)]
    );

    let later = wire.replacen("[windows.show]\n", "[windows.show]\nspace_index = 3\n", 1);
    assert_ne!(later, wire, "the later key was spliced in:\n{later}");
    let back = RestoreManifest::from_toml(&later).expect("a later show key is skipped");
    assert_eq!(back.windows[0].show, shown.windows[0].show);

    let fixtures = aterm_tempfile::manifest_dir().join("tests/fixtures/handoff");
    let mut layouts = 0;
    let (mut without_show, mut with_show) = (0, 0);
    for release in fs::read_dir(&fixtures).expect("the handoff fixtures") {
        let release = release.expect("a release directory").path();
        if !release.is_dir() {
            continue;
        }
        for desk in fs::read_dir(&release).expect("a release's desks") {
            let desk = desk.expect("a desk").path();
            if !desk.is_dir() {
                continue;
            }
            for file in fs::read_dir(&desk).expect("a desk's files") {
                let path = file.expect("a fixture file").path();
                if !path.to_string_lossy().ends_with(".layout.toml") {
                    continue;
                }
                let wire = fs::read_to_string(&path).expect("a fixture layout");
                let parsed = RestoreManifest::from_toml(&wire)
                    .unwrap_or_else(|| panic!("{} parses", path.display()));
                let carried = wire.matches("[windows.show]\n").count();
                let known = parsed
                    .windows
                    .iter()
                    .filter(|w| !w.show.is_unknown())
                    .count();
                assert_eq!(
                    known,
                    carried,
                    "{}: a parent's show state is read as it carried it, and none is \
                     invented where it carried none",
                    path.display()
                );
                if carried == 0 {
                    without_show += 1;
                } else {
                    with_show += 1;
                }
                layouts += 1;
            }
        }
    }
    assert!(layouts >= 2, "the fixture walk read {layouts} layouts");
    assert!(
        without_show > 0,
        "the walk read a layout of a parent older than the show state"
    );
    assert!(with_show > 0, "and one of a parent that writes it");
}

#[test]
fn restore_parse_canonicalizes_hostile_user_chrome_metadata() {
    let mut manifest = sample();
    let RestoredSplitTree::Leaf {
        view: RestoredView::Terminal(terminal),
    } = &mut manifest.windows[0].restored_tabs[0].root
    else {
        panic!("sample first tab is a terminal leaf");
    };
    terminal.user_title = Some("  build\n\u{202e}agent  ".to_string());
    terminal.description = Some(format!("purpose\u{2029}{}", "x".repeat(1100)));
    terminal.icon = Some("\u{2066}👨‍👩‍👧‍👦\u{2069}".to_string());

    let wire = aterm_toml::to_string(&manifest).expect("serialize hostile fixture");
    let decoded = RestoreManifest::from_toml(&wire).expect("bounded restore parses");
    let RestoredSplitTree::Leaf {
        view: RestoredView::Terminal(terminal),
    } = &decoded.windows[0].restored_tabs[0].root
    else {
        panic!("decoded first tab is a terminal leaf");
    };
    assert_eq!(terminal.user_title.as_deref(), Some("buildagent"));
    assert_eq!(terminal.icon.as_deref(), Some("👨‍👩‍👧‍👦"));
    let description = terminal
        .description
        .as_deref()
        .expect("description retained");
    assert!(description.starts_with("purpose"));
    assert!(description.len() <= crate::session_timeline::META_DESCRIPTION_MAX);
    assert!(!crate::session_timeline::metadata_has_forbidden_formatting(
        description
    ));
    assert!(terminal.user_metadata_is_canonical());
}

#[test]
fn restore_parse_drops_the_retired_hook_badge_and_keeps_the_rest() {
    let parse = |attention: &str| {
        let mut manifest = sample();
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &mut manifest.windows[0].restored_tabs[0].root
        else {
            panic!("sample first tab is a terminal leaf");
        };
        terminal.user_title = Some("worker".to_string());
        terminal.attention = Some(attention.to_string());
        let wire = aterm_toml::to_string(&manifest).expect("serialize legacy fixture");
        assert!(wire.contains(attention), "the wire carries it: {wire}");
        let decoded = RestoreManifest::from_toml(&wire).expect("the manifest still parses");
        let again =
            RestoreManifest::from_toml(&decoded.to_toml().expect("serialize")).expect("reparse");
        assert_eq!(again, decoded, "the parse is a fixed point");
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &decoded.windows[0].restored_tabs[0].root
        else {
            panic!("decoded first tab is a terminal leaf");
        };
        assert_eq!(terminal.user_title.as_deref(), Some("worker"));
        assert!(terminal.user_metadata_is_canonical());
        terminal.attention.clone()
    };
    assert_eq!(
        parse("claude needs approval: Claude needs your permission - permission_prompt"),
        None
    );
    assert_eq!(
        parse("waiting on a human review").as_deref(),
        Some("waiting on a human review")
    );
}

#[test]
fn mixed_native_descriptors_round_trip_in_their_order() {
    let manifest = mixed_sample("file:///tmp/readme.md");
    let toml = manifest.to_toml().expect("serialize");
    let decoded = RestoreManifest::from_toml(&toml).expect("decode mixed manifest");
    assert_eq!(decoded, manifest);
    let window = &decoded.windows[0];
    let order = window.canonical_order().expect("validated order");
    assert_eq!(window.canonical_active(&order), 2);
    assert_eq!(
        window.native_tabs[1].document_uri(),
        Some("file:///tmp/readme.md")
    );
}

#[test]
fn legacy_terminal_only_manifest_migrates_to_identity_order() {
    let legacy = r#"
schema = 1

[[windows]]
rows = 24
cols = 80
active_tab = 0

[[windows.tabs]]
kind = "leaf"
cwd = "/tmp"
title = "shell"
focused = true
"#;
    let decoded = RestoreManifest::from_toml(legacy).expect("legacy manifest");
    assert!(decoded.windows[0].native_tabs.is_empty());
    assert_eq!(
        decoded.windows[0].canonical_order(),
        Some(vec![TabOrderEntry::Terminal { index: 0 }])
    );
    assert_eq!(
        decoded.windows[0]
            .canonical_active(&decoded.windows[0].canonical_order().expect("legacy order")),
        0
    );
    assert!(matches!(
        sample().windows[0].restored_tabs[1].root,
        RestoredSplitTree::Split {
            axis: SplitKind::Horizontal,
            ..
        }
    ));
}

#[test]
fn a_layout_past_the_caps_keeps_every_window_it_can() {
    use crate::crash_journal::fixtures::layout;
    let window = |cwd: String| layout(&[(cwd.as_str(), "zsh")]).windows.remove(0);
    let crowded: Vec<(String, String)> = (0..=MAX_TABS_PER_WINDOW)
        .map(|i| (format!("/t{i}"), "zsh".to_string()))
        .collect();
    let crowded: Vec<(&str, &str)> = crowded
        .iter()
        .map(|(cwd, title)| (cwd.as_str(), title.as_str()))
        .collect();
    let mut windows: Vec<WindowLayout> = (0..=MAX_WINDOWS)
        .map(|i| window(format!("/w{i}")))
        .collect();
    windows.insert(1, layout(&crowded).windows.remove(0));
    let manifest = RestoreManifest::new(windows);
    assert_eq!(manifest.windows.len(), MAX_WINDOWS + 2, "PRECONDITION");
    let admitted = AdmittedWindows::of(manifest.windows.clone());
    assert_eq!((admitted.past_cap, admitted.unbuildable), (1, 1));
    let read = RestoreManifest::from_toml(&manifest.to_toml().unwrap())
        .expect("the windows that fit come back");
    assert_eq!(read.windows.len(), MAX_WINDOWS);
    let firsts: Vec<Option<String>> = read.windows.iter().map(|w| w.bootstrap_cwd()).collect();
    let expected: Vec<Option<String>> = (0..MAX_WINDOWS).map(|i| Some(format!("/w{i}"))).collect();
    assert_eq!(firsts, expected, "in order, the crowded window left out");

    let only_crowded = RestoreManifest::new(layout(&crowded).windows);
    assert!(
        RestoreManifest::from_toml(&only_crowded.to_toml().unwrap()).is_none(),
        "nothing reads: refused, as before"
    );
}

fn shells_window(shells: std::ops::Range<u64>) -> WindowLayout {
    WindowLayout {
        rows: 30,
        cols: 100,
        active_tab: 0,
        outer_x: Some(40),
        outer_y: Some(60),
        maximized: None,
        inner_px: Some([1000, 640]),
        presence_row: true,
        show: WindowShow::UNKNOWN,
        tabs: shells
            .clone()
            .map(|shell| PaneLayout::Leaf {
                cwd: Some("/tmp/work".to_string()),
                title: "zsh".to_string(),
                focused: true,
                local_id: Some(shell),
            })
            .collect(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(0),
        workers_in_front: false,
        restored_tabs: shells
            .map(|shell| RestoredTab {
                root: terminal(Some(shell), "zsh"),
                focused_path: Vec::new(),
                zoomed: false,
                parked: false,
                ephemeral: false,
                fleet: false,
            })
            .collect(),
    }
}

fn shells_of(manifest: &RestoreManifest) -> Vec<Vec<u64>> {
    manifest
        .windows
        .iter()
        .map(|window| {
            window
                .restored_tabs
                .iter()
                .filter_map(|tab| tab.root.first_terminal_leaf()?.local_id)
                .collect()
        })
        .collect()
}

fn assert_folded_whole(folded: &RestoreManifest, shells: u64) {
    assert!(folded.admits_whole(), "within the caps");
    assert_eq!(
        RestoreManifest::from_toml(&folded.to_toml().unwrap()).as_ref(),
        Some(folded),
        "the producer's round trip holds"
    );
    let handed = (0..shells).collect::<Vec<_>>();
    assert!(
        folded.clone().placed_for_handed(&handed).is_ok(),
        "every handed shell is placed"
    );
}

#[test]
fn a_desk_within_the_caps_is_left_byte_identical() {
    let mut windows = vec![shells_window(0..256)];
    windows
        .extend((256..256 + MAX_WINDOWS as u64 - 1).map(|shell| shells_window(shell..shell + 1)));
    let before = RestoreManifest::new(windows);
    assert_eq!(
        before.windows.len(),
        MAX_WINDOWS,
        "PRECONDITION: at the window cap"
    );
    let mut folded = before.clone();
    folded.fold_into_caps();
    assert_eq!(folded, before);
    let wire = folded.to_toml().unwrap();
    assert_eq!(
        wire,
        before.to_toml().unwrap(),
        "byte-identical on the wire"
    );
    assert!(!wire.contains("fold"), "no fold table: {}", &wire[..200]);
    assert!(folded.admits_whole());
}

/// A lone list holds nothing, so it costs no window with tabs its slot, and one left out is
/// no fold. A list's window can still host a spill of shown tabs: it then holds tabs and no
/// list.
#[test]
fn a_lone_list_never_costs_a_window_its_slot() {
    let lone = || WindowLayout::lone_workers_list(30, 100);
    let tabbed = |count: u64| -> Vec<WindowLayout> {
        (0..count)
            .map(|shell| shells_window(shell..shell + 1))
            .collect()
    };
    let fold = |windows: Vec<WindowLayout>| {
        let mut desk = RestoreManifest::new(windows);
        desk.fold_into_caps();
        desk
    };
    let cap = MAX_WINDOWS as u64;

    let mut one_list_first = vec![lone()];
    one_list_first.extend(tabbed(cap));
    assert_eq!(
        fold(one_list_first),
        RestoreManifest::new(tabbed(cap)),
        "at the cap the list goes, every window keeps its own row and nothing folds"
    );

    let mut one_list_last = tabbed(cap);
    one_list_last.push(lone());
    assert_eq!(
        fold(one_list_last),
        RestoreManifest::new(tabbed(cap)),
        "the same past the end"
    );

    let mut past_the_cap = vec![lone()];
    past_the_cap.extend(tabbed(cap + 1));
    assert_eq!(
        fold(past_the_cap),
        fold(tabbed(cap + 1)),
        "a desk past the cap folds what it would have folded with no list"
    );

    let mut one_slot = vec![lone(), lone()];
    one_slot.extend(tabbed(cap - 1));
    let kept = fold(one_slot);
    assert_eq!(kept.windows.len(), MAX_WINDOWS);
    assert!(
        kept.windows[0].is_lone_workers_list(),
        "the earliest list stays"
    );
    assert!(
        !kept.windows[1].is_lone_workers_list(),
        "the other gave up its slot"
    );
    assert_eq!(kept.fold, LayoutFold::default());

    let lists = fold(vec![lone(); MAX_WINDOWS + 1]);
    assert_eq!(
        lists.windows.len(),
        MAX_WINDOWS,
        "lists alone keep every slot there is"
    );
    assert!(lists.windows.iter().all(WindowLayout::is_lone_workers_list));
    assert_eq!(lists.fold, LayoutFold::default());

    let mut room = tabbed(cap - 2);
    room.insert(1, lone());
    let before = RestoreManifest::new(room);
    let mut within = before.clone();
    within.fold_into_caps();
    assert_eq!(within, before, "CONTROL: within the caps nothing changes");

    let host = fold(vec![shells_window(0..257), lone()]);
    assert_eq!(host.windows[0].restored_tabs.len(), MAX_TABS_PER_WINDOW);
    assert_eq!(host.windows[1].restored_tabs.len(), 1);
    assert!(!host.windows[1].is_lone_workers_list());
    assert_eq!(host.fold.windows, 0);
    assert!(host.admits_whole(), "the spill's new home reads back");
}

#[test]
fn the_windows_past_the_cap_fold_into_the_last_window_that_fits() {
    let mut windows = (0..MAX_WINDOWS as u64 - 1)
        .map(|shell| shells_window(shell..shell + 1))
        .collect::<Vec<_>>();
    windows.push(shells_window(63..313)); // the last that fits: 250 tabs
    windows.push(shells_window(313..318)); // past the cap: 5 tabs…
    windows.push(shells_window(318..323)); // …and 5 more
    let before = RestoreManifest::new(windows);
    let mut folded = before.clone();
    folded.fold_into_caps();

    assert_eq!(
        folded.fold,
        LayoutFold {
            windows: 2,
            tabs: 4,
            front: (0..=63).chain([313, 318]).collect(),
        },
        "every captured window's front pane, before a tab moved"
    );
    let shells = shells_of(&folded);
    assert_eq!(shells.len(), MAX_WINDOWS);
    assert_eq!(
        shells[MAX_WINDOWS - 1],
        (63..319).collect::<Vec<_>>(),
        "its own 250, then the folded windows' tabs up to the cap"
    );
    assert_eq!(
        shells[0],
        vec![0, 319, 320, 321, 322],
        "the 4 past the last window's cap: into the next window with room"
    );
    assert!(
        folded.windows[MAX_WINDOWS - 1].tabs.is_empty(),
        "the last window lost tabs: no legacy mirror naming a moved shell"
    );
    assert_eq!(
        folded.windows[0].tabs, before.windows[0].tabs,
        "the first only received tabs: its mirror names its own, as captured"
    );
    for window in [0, MAX_WINDOWS - 1] {
        assert_eq!(
            folded.windows[window].active_item,
            Some(0),
            "its own front tab"
        );
    }
    for window in 1..MAX_WINDOWS - 1 {
        assert_eq!(
            folded.windows[window], before.windows[window],
            "window {window}, untouched, is as captured"
        );
    }
    assert_folded_whole(&folded, 323);

    assert!(!folded.windows_front_terminal_ids().contains(&313));
    assert_eq!(folded.front_terminal_ids(), folded.fold.front);
    let mut hostile = folded;
    hostile.fold.front = vec![7; MAX_FOLD_FRONT + 5];
    let read = RestoreManifest::from_toml(&hostile.to_toml().unwrap()).expect("reads");
    assert_eq!(read.fold.front.len(), MAX_FOLD_FRONT);
}

#[test]
fn a_window_past_the_tab_cap_spills_into_the_next_window_with_room() {
    let mut crowded = shells_window(0..300);
    crowded.active_item = Some(280);
    let before = RestoreManifest::new(vec![
        crowded,
        shells_window(300..556), // full
        shells_window(556..566),
    ]);
    let mut folded = before.clone();
    folded.fold_into_caps();
    assert_eq!(
        folded.fold,
        LayoutFold {
            windows: 0,
            tabs: 44,
            front: vec![280, 300, 556],
        }
    );
    let shells = shells_of(&folded);
    assert_eq!(shells[0], (0..256).collect::<Vec<_>>());
    assert_eq!(shells[1], (300..556).collect::<Vec<_>>(), "full: skipped");
    assert_eq!(
        shells[2],
        (556..566).chain(256..300).collect::<Vec<_>>(),
        "its own, then the spilled tabs in order"
    );
    assert_eq!(folded.windows[0].active_item, Some(255));
    assert_eq!(
        folded.windows[2].active_item,
        Some(0),
        "the host's own front tab"
    );
    assert!(folded.windows[0].tabs.is_empty(), "the donor: no mirror");
    assert_eq!(
        folded.windows[2].tabs, before.windows[2].tabs,
        "the host: as captured"
    );
    assert_eq!(folded.windows[1], before.windows[1]);
    assert_folded_whole(&folded, 566);
    assert!(folded.windows_front_terminal_ids().contains(&255));
    assert_eq!(folded.front_terminal_ids(), vec![280, 300, 556]);
}

#[test]
fn a_lone_window_past_the_tab_cap_spills_into_windows_made_for_it() {
    let mut crowded = shells_window(0..600);
    crowded.active_item = Some(300);
    let mut folded = RestoreManifest::new(vec![crowded]);
    folded.fold_into_caps();
    assert_eq!(
        folded.fold,
        LayoutFold {
            windows: 0,
            tabs: 344,
            front: vec![300],
        }
    );
    assert_eq!(
        shells_of(&folded),
        vec![
            (0..256).collect::<Vec<_>>(),
            (256..512).collect(),
            (512..600).collect()
        ]
    );
    assert_eq!(
        folded
            .windows
            .iter()
            .map(|window| window.active_item)
            .collect::<Vec<_>>(),
        vec![Some(255), Some(44), Some(0)],
        "the front tab (300) is the first made window's 45th"
    );
    for made in &folded.windows[1..] {
        assert_eq!(
            (made.rows, made.cols, made.inner_px, made.presence_row),
            (30, 100, Some([1000, 640]), true)
        );
        assert_eq!((made.outer_x, made.outer_y), (None, None));
    }
    assert_folded_whole(&folded, 600);
}

#[test]
fn a_desk_past_what_any_layout_holds_is_not_admitted_whole() {
    let mut windows = (0..MAX_WINDOWS as u64)
        .map(|window| {
            let first = window * MAX_TABS_PER_WINDOW as u64;
            shells_window(first..first + MAX_TABS_PER_WINDOW as u64)
        })
        .collect::<Vec<_>>();
    let spare = windows[1].restored_tabs[0].clone();
    windows[0].restored_tabs.push(spare);
    let mut full = RestoreManifest::new(windows);
    full.fold_into_caps();
    assert_eq!(
        full.windows[0].restored_tabs.len(),
        MAX_TABS_PER_WINDOW + 1,
        "nowhere to go"
    );
    assert!(!full.admits_whole());
    assert!(full.why_not_whole().is_some(), "the refusal says why");
    assert!(full.fold.is_none(), "nothing moved, nothing recorded");

    let mut root = terminal(Some(0), "zsh");
    for _ in 0..=MAX_SPLIT_DEPTH {
        root = RestoredSplitTree::Split {
            axis: SplitKind::Vertical,
            ratio: 0.5,
            first: Box::new(root),
            second: Box::new(terminal(None, "deep")),
        };
    }
    let mut deep = shells_window(0..1);
    deep.restored_tabs[0].root = root;
    deep.restored_tabs[0].focused_path = vec![RestoreBranch::Second];
    let mut deep = RestoreManifest::new(vec![deep]);
    let before = deep.clone();
    deep.fold_into_caps();
    assert_eq!(
        deep, before,
        "the fold moves tabs whole; it does not split a tab"
    );
    assert!(!deep.admits_whole());
    assert!(deep.why_not_whole().is_some(), "the refusal says why");
}

#[test]
fn system_state_restore_descriptors_round_trip_without_document_authority() {
    let native = NativeLeafRestore::system_state();
    assert_eq!(native.validation_error(), None);
    let bytes = aterm_json::to_vec(&native).unwrap();
    assert_eq!(
        aterm_json::from_slice::<NativeLeafRestore>(&bytes).unwrap(),
        native
    );
    let legacy = NativeTabRestore::SystemState;
    let bytes = aterm_json::to_vec(&legacy).unwrap();
    assert_eq!(
        aterm_json::from_slice::<NativeTabRestore>(&bytes).unwrap(),
        legacy
    );
    assert_eq!(
        RestoredTab::from_legacy_native(&legacy).unwrap().root,
        RestoredSplitTree::leaf(RestoredView::Native(native.clone()))
    );
    let mut invalid = native;
    invalid.uri = Some("file:///tmp/unrequested".into());
    assert!(invalid.validation_error().is_some());
}

#[test]
fn about_restore_descriptors_round_trip_without_document_or_settings_authority() {
    let native = NativeLeafRestore::about();
    assert_eq!(native.validation_error(), None);
    let bytes = aterm_json::to_vec(&native).unwrap();
    assert_eq!(
        aterm_json::from_slice::<NativeLeafRestore>(&bytes).unwrap(),
        native
    );
    let legacy = NativeTabRestore::About;
    assert!(legacy.shape_is_valid());
    assert_eq!(legacy.document_uri(), None);
    let bytes = aterm_json::to_vec(&legacy).unwrap();
    assert_eq!(
        aterm_json::from_slice::<NativeTabRestore>(&bytes).unwrap(),
        legacy
    );
    assert_eq!(
        RestoredTab::from_legacy_native(&legacy).unwrap().root,
        RestoredSplitTree::leaf(RestoredView::Native(native.clone()))
    );
    for (route, uri) in [
        (Some("/about".to_string()), None),
        (None, Some("file:///tmp/unrequested".to_string())),
    ] {
        let mut invalid = native.clone();
        invalid.route = route;
        invalid.uri = uri;
        assert!(invalid.validation_error().is_some());
    }
    let settings = NativeTabRestore::Settings {
        route: "/about".into(),
    };
    assert!(legacy.joins_mirror(std::slice::from_ref(&settings)));
    assert!(settings.joins_mirror(std::slice::from_ref(&legacy)));
    assert!(!legacy.joins_mirror(std::slice::from_ref(&legacy)));
}

#[test]
fn the_legacy_mirror_is_written_by_the_rule_the_parse_reads_it_by() {
    let settings = |route: &str| NativeTabRestore::Settings {
        route: route.to_string(),
    };
    let markdown = |uri: &str| NativeTabRestore::Markdown {
        uri: uri.to_string(),
    };
    let editor = |uri: &str| NativeTabRestore::Editor {
        uri: uri.to_string(),
    };
    let live = [
        settings("/about"),
        markdown("file:///tmp/notes.md"),
        settings("/updates"),
        markdown("file:///tmp/notes.md"),
        editor("file:///tmp/notes.md"),
        markdown("file:///tmp/other.md"),
    ];
    let mut mirror = Vec::new();
    for descriptor in live {
        if descriptor.joins_mirror(&mirror) {
            mirror.push(descriptor);
        }
    }
    assert_eq!(
        mirror,
        vec![
            settings("/about"),
            markdown("file:///tmp/notes.md"),
            editor("file:///tmp/notes.md"),
            markdown("file:///tmp/other.md"),
        ],
        "a second Settings and a second equal descriptor stay out; another view of the \
         same file does not"
    );

    let window_with = |native_tabs: Vec<NativeTabRestore>| {
        let mut window = shells_window(0..1);
        window.tab_order = std::iter::once(TabOrderEntry::Terminal { index: 0 })
            .chain((0..native_tabs.len()).map(|index| TabOrderEntry::Native { index }))
            .collect();
        window.native_tabs = native_tabs;
        window.restored_tabs.extend(
            std::iter::repeat_with(|| RestoredTab {
                root: RestoredSplitTree::leaf(RestoredView::Native(NativeLeafRestore::settings(
                    "/about".to_string(),
                ))),
                focused_path: Vec::new(),
                zoomed: false,
                parked: false,
                ephemeral: false,
                fleet: false,
            })
            .take(2),
        );
        RestoreManifest::new(vec![shells_window(1..2), window])
    };
    let written = window_with(mirror);
    assert!(
        written.admits_whole(),
        "two Settings leaves, one in the mirror"
    );
    assert_eq!(
        RestoreManifest::from_toml(&written.to_toml().unwrap()).as_ref(),
        Some(&written)
    );
    for doubled in [
        vec![settings("/about"), settings("/updates")],
        vec![
            markdown("file:///tmp/notes.md"),
            markdown("file:///tmp/notes.md"),
        ],
    ] {
        let refused = window_with(doubled);
        assert!(!refused.admits_whole(), "refused");
        let read = RestoreManifest::from_toml(&refused.to_toml().unwrap()).expect("one reads");
        assert_eq!(read.windows.len(), 1, "the parse leaves that window out");
    }
}

#[test]
fn malformed_native_metadata_and_aliasing_order_fail_closed() {
    let mut malformed_uri = mixed_sample("https://example.invalid/not-local.md");
    let toml = aterm_toml::to_string(&malformed_uri).unwrap();
    assert!(RestoreManifest::from_toml(&toml).is_none());

    malformed_uri.windows[0].native_tabs[1] = NativeTabRestore::Editor {
        uri: "file:///tmp/readme.md".to_string(),
    };
    malformed_uri.windows[0].tab_order = vec![
        TabOrderEntry::Native { index: 0 },
        TabOrderEntry::Terminal { index: 0 },
        TabOrderEntry::Native { index: 0 },
    ];
    let toml = aterm_toml::to_string(&malformed_uri).unwrap();
    assert!(
        RestoreManifest::from_toml(&toml).is_none(),
        "duplicate descriptor indices may not alias one live tab"
    );

    malformed_uri.windows[0].tab_order = vec![
        TabOrderEntry::Native { index: 1 },
        TabOrderEntry::Terminal { index: 0 },
        TabOrderEntry::Native { index: 0 },
    ];
    let permuted = RestoreManifest::from_toml(&aterm_toml::to_string(&malformed_uri).unwrap())
        .expect("non-monotonic bijection of legacy tab indices is valid");
    assert_eq!(permuted.windows[0].restored_tabs.len(), 3);

    let mut duplicate_settings = mixed_sample("file:///tmp/readme.md");
    duplicate_settings.windows[0].native_tabs[1] = NativeTabRestore::Settings {
        route: "/updates".to_string(),
    };
    assert!(
        RestoreManifest::from_toml(&duplicate_settings.to_toml().unwrap()).is_none(),
        "one window cannot restore two aliases of its Settings singleton"
    );

    let missing_uri = r#"
schema = 1
[[windows]]
rows = 24
cols = 80
active_tab = 0
[[windows.native_tabs]]
kind = "editor"
[[windows.tab_order]]
kind = "native"
index = 0
active_item = 0
"#;
    assert!(RestoreManifest::from_toml(missing_uri).is_none());
}

#[test]
fn unknown_native_metadata_is_cut_at_a_utf8_boundary() {
    let mut native = NativeLeafRestore::document("future.diagram", "file:///tmp/é".into());
    let prefix_len = native.copyable_metadata().len() + "\nmetadata=".len();
    native.metadata = format!("{}é", "x".repeat(MAX_VIEW_METADATA_BYTES - prefix_len - 1));
    let mut view = RestoredView::Native(native);
    view.sanitize();
    let RestoredView::Placeholder(placeholder) = view else {
        panic!("the unavailable native app must retain a placeholder");
    };
    assert_eq!(placeholder.metadata.len(), MAX_VIEW_METADATA_BYTES - 1);
    assert!(placeholder.metadata.contains("file:///tmp/é"));
}

#[test]
fn v2_unknown_and_corrupt_native_leaves_become_in_place_placeholders() {
    let input = r#"
schema = 2

[[windows]]
rows = 30
cols = 100
active_item = 0

[[windows.restored_tabs]]
focused_path = ["second"]

[windows.restored_tabs.root]
node = "split"
axis = "vertical"
ratio = 0.61

[windows.restored_tabs.root.first]
node = "leaf"
[windows.restored_tabs.root.first.view]
kind = "terminal"
cwd = "/tmp"
title = "healthy shell"

[windows.restored_tabs.root.second]
node = "leaf"
[windows.restored_tabs.root.second.view]
kind = "native"
restore_tag = "future.diagram"
metadata = "opaque=copy-me"
"#;
    let manifest = RestoreManifest::from_toml(input).expect("v2 manifest remains usable");
    let tab = &manifest.windows[0].restored_tabs[0];
    let RestoredSplitTree::Split { first, second, .. } = &tab.root else {
        panic!("split topology was not preserved");
    };
    assert!(matches!(
        &**first,
        RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(TerminalLeafRestore { title, .. })
        } if title == "healthy shell"
    ));
    assert!(matches!(
        &**second,
        RestoredSplitTree::Leaf {
            view: RestoredView::Placeholder(PlaceholderLeafRestore {
                restore_tag,
                metadata,
                ..
            })
        } if restore_tag == "future.diagram" && metadata.contains("copy-me")
    ));

    let corrupt_known = input.replace("future.diagram", "editor").replace(
        "metadata = \"opaque=copy-me\"",
        "uri = \"https://invalid.example\"",
    );
    let recovered = RestoreManifest::from_toml(&corrupt_known)
        .expect("one corrupt known leaf must not discard its sibling");
    let RestoredSplitTree::Split { second, .. } = &recovered.windows[0].restored_tabs[0].root
    else {
        unreachable!()
    };
    assert!(matches!(
        &**second,
        RestoredSplitTree::Leaf {
            view: RestoredView::Placeholder(_)
        }
    ));
}

#[test]
fn v2_sanitizes_geometry_but_rejects_resource_exhaustion_shapes() {
    let mut manifest = mixed_sample("file:///tmp/readme.md");
    let mut tab = RestoredTab::from_legacy_native(&NativeTabRestore::Editor {
        uri: "file:///tmp/readme.md".to_string(),
    })
    .unwrap();
    tab.root = RestoredSplitTree::Split {
        axis: SplitKind::Horizontal,
        ratio: f32::NAN,
        first: Box::new(tab.root.clone()),
        second: Box::new(terminal(None, "shell")),
    };
    tab.focused_path = vec![RestoreBranch::First];
    manifest.windows[0].restored_tabs = vec![tab];
    manifest.windows[0].active_item = Some(0);
    let encoded = aterm_toml::to_string(&manifest).unwrap();
    let decoded = RestoreManifest::from_toml(&encoded).expect("NaN ratio is recoverable");
    assert!(matches!(
        decoded.windows[0].restored_tabs[0].root,
        RestoredSplitTree::Split { ratio, .. } if ratio == 0.5
    ));

    let mut root = terminal(None, "");
    for _ in 0..=MAX_SPLIT_DEPTH {
        root = RestoredSplitTree::Split {
            axis: SplitKind::Horizontal,
            ratio: 0.5,
            first: Box::new(root),
            second: Box::new(terminal(None, "")),
        };
    }
    manifest.windows[0].restored_tabs = vec![RestoredTab {
        root,
        focused_path: Vec::new(),
        zoomed: false,
        parked: false,
        ephemeral: false,
        fleet: false,
    }];
    assert!(
        RestoreManifest::from_toml(&aterm_toml::to_string(&manifest).unwrap()).is_none(),
        "adversarial depth must fail before runtime allocation"
    );
}

#[test]
fn a_nul_cwd_leaf_round_trips_sanitized() {
    let hostile = || {
        let mut window = shells_window(7..8);
        let PaneLayout::Leaf { cwd, title, .. } = &mut window.tabs[0] else {
            unreachable!("a legacy leaf")
        };
        *cwd = Some("/tmp/a\0b".into());
        *title = "zsh\0 ~".into();
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(leaf),
        } = &mut window.restored_tabs[0].root
        else {
            unreachable!("a terminal leaf")
        };
        leaf.cwd = Some("/tmp/a\0b".into());
        leaf.title = "zsh\0 ~".into();
        window
    };
    let assert_clean = |manifest: &RestoreManifest, what: &str| {
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &manifest.windows[0].restored_tabs[0].root
        else {
            panic!("{what}: the tab is still a terminal leaf");
        };
        assert_eq!(terminal.cwd, None, "{what}: the NUL directory is dropped");
        assert_eq!(
            terminal.title, "zsh ~",
            "{what}: the title loses only its NUL"
        );
        assert_eq!(
            terminal.local_id,
            Some(7),
            "{what}: the adoption id survives"
        );
        let PaneLayout::Leaf { cwd, title, .. } = &manifest.windows[0].tabs[0] else {
            panic!("{what}: the legacy mirror is still a leaf");
        };
        assert_eq!(cwd, &None, "{what}: the legacy mirror drops it too");
        assert_eq!(title, "zsh ~", "{what}: and strips the title the same way");
    };

    let raw = RestoreManifest {
        schema: SCHEMA,
        fold: LayoutFold::default(),
        windows: vec![hostile()],
    };
    let wire = aterm_toml::to_string(&raw).expect("serialize the hostile fixture");
    let parsed = RestoreManifest::from_toml(&wire).expect("a NUL directory is recoverable");
    assert_clean(&parsed, "parse");

    let captured = RestoreManifest::new(vec![hostile()]);
    assert_clean(&captured, "capture");
    let round_trip = captured
        .to_toml()
        .ok()
        .and_then(|wire| RestoreManifest::from_toml(&wire));
    assert_eq!(
        round_trip.as_ref(),
        Some(&captured),
        "the worker's layout round trip holds on a sanitized capture"
    );
}

#[test]
fn a_leaf_names_its_logs_epoch_and_the_layout_says_the_largest() {
    let named = RestoreManifest::one_terminal_for_test(Some(1_792_000_123));
    let wire = named.to_toml().expect("serialize");
    assert!(wire.contains("log_ep = 1792000123"), "{wire}");
    let parsed = RestoreManifest::from_toml(&wire).expect("parse");
    assert_eq!(parsed, named, "the round trip holds");
    assert_eq!(parsed.max_log_ep(), Some(1_792_000_123));
    let older = wire.replace("log_ep = 1792000123\n", "");
    assert_ne!(older, wire, "PRECONDITION: the key is out");
    assert_eq!(
        RestoreManifest::from_toml(&older).map(|m| m.max_log_ep()),
        Some(None),
        "an older writer's leaf names none, and parses"
    );
    let plain = RestoreManifest::one_terminal_for_test(None)
        .to_toml()
        .expect("serialize");
    assert!(!plain.contains("log_ep"), "{plain}");
    let mut two = RestoreManifest::one_terminal_for_test(Some(7));
    two.windows
        .extend(RestoreManifest::one_terminal_for_test(Some(9)).windows);
    two.windows
        .extend(RestoreManifest::one_terminal_for_test(None).windows);
    assert_eq!(two.max_log_ep(), Some(9), "the largest of every window's");
    assert_eq!(
        RestoreManifest::one_terminal_for_test(None).max_log_ep(),
        None,
        "NEGATIVE CONTROL: no epoch, none said"
    );
}

#[test]
fn a_leaf_names_its_session_and_lineage_and_older_peers_read_past_them() {
    let window = |sid: Option<String>, lineage: Option<String>| WindowLayout {
        rows: 24,
        cols: 80,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: WindowShow::UNKNOWN,
        tabs: vec![PaneLayout::Leaf {
            cwd: None,
            title: "zsh".into(),
            focused: true,
            local_id: Some(7),
        }],
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: None,
        workers_in_front: false,
        restored_tabs: vec![RestoredTab {
            root: RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                sid,
                lineage,
                log_ep: None,
                cwd: None,
                title: "zsh".into(),
                profile: None,
                local_id: Some(7),
                user_title: None,
                description: None,
                icon: None,
                role: None,
                attention: None,
                theme: None,
                questions: None,
                autopilot: None,
                land_at: None,
                autopilot_by: None,
                agent: None,
                supervised: None,
                lost: None,
                held: false,
                holds: Vec::new(),
                claim_ended: false,
                not_claim_worker: false,
                claim_worker: None,
                task: None,
            })),
            focused_path: Vec::new(),
            zoomed: false,
            parked: false,
            ephemeral: false,
            fleet: false,
        }],
    };
    let leaf_of = |m: &RestoreManifest| match &m.windows[0].restored_tabs[0].root {
        RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(t),
        } => (t.sid.clone(), t.lineage.clone()),
        _ => panic!("a terminal leaf"),
    };
    let named = RestoreManifest::new(vec![window(
        Some("s-0123abcd".into()),
        Some("s-00ff".into()),
    )]);
    let wire = named.to_toml().expect("serialize");
    assert!(wire.contains("sid = \"s-0123abcd\""), "{wire}");
    assert!(wire.contains("lineage = \"s-00ff\""), "{wire}");
    let parsed = RestoreManifest::from_toml(&wire).expect("parse");
    assert_eq!(parsed, named, "the round trip holds");
    let plain = RestoreManifest::new(vec![window(None, None)])
        .to_toml()
        .expect("serialize");
    assert!(
        !plain.contains("sid =") && !plain.contains("lineage ="),
        "a leaf without them is written as before: {plain}"
    );
    let newer = wire.replace(
        "sid = \"s-0123abcd\"",
        "sid = \"s-0123abcd\"\nfuture_field = \"from a newer peer\"\nidentity = \"worker\"\ntold = \"1f\"",
    );
    assert_ne!(newer, wire, "PRECONDITION: the unknown field is in");
    assert_eq!(
        RestoreManifest::from_toml(&newer).map(|m| leaf_of(&m)),
        Some(leaf_of(&named)),
        "a field this build does not know is read past"
    );
    let rewritten = RestoreManifest::from_toml(&newer)
        .expect("read past unknown fields")
        .to_toml()
        .expect("serialize");
    assert!(!rewritten.contains("identity ="), "{rewritten}");
    assert!(
        !rewritten.contains("told ="),
        "an older aterm's told-once ids are read past: {rewritten}"
    );
    for (sid, lineage) in [
        ("s-ab\0cd".to_string(), format!("s-{}", "a".repeat(1 << 20))),
        (format!("s-{}", "0".repeat(48)), "s-xyz".to_string()),
        (String::new(), "-".to_string()),
    ] {
        let raw = RestoreManifest {
            schema: SCHEMA,
            fold: LayoutFold::default(),
            windows: vec![window(Some(sid.clone()), Some(lineage.clone()))],
        };
        let wire = aterm_toml::to_string(&raw).expect("serialize the hostile fixture");
        let parsed = RestoreManifest::from_toml(&wire).expect("a hostile sid is recoverable");
        assert_eq!(
            leaf_of(&parsed),
            (None, None),
            "{sid:.12?} / {lineage:.12?}"
        );
        let captured = RestoreManifest::new(vec![window(Some(sid), Some(lineage))]);
        assert_eq!(leaf_of(&captured), (None, None));
    }
}

#[test]
fn a_held_leaf_round_trips_and_its_folder_that_is_not_here_is_not_asked_for() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-restore-remote")
        .tempdir()
        .expect("scratch dir");
    let here = scratch
        .path()
        .to_str()
        .expect("utf-8 scratch path")
        .to_owned();
    let remote = format!("{here}/on-the-remote-host/proj");
    let window = |cwd: &str, held: bool| {
        let mut window = shells_window(1..2);
        window.tabs.clear();
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(leaf),
        } = &mut window.restored_tabs[0].root
        else {
            unreachable!("a terminal leaf");
        };
        leaf.cwd = Some(cwd.to_owned());
        leaf.held = held;
        RestoreManifest::new(vec![window])
    };
    let wire = |manifest: &RestoreManifest| manifest.to_toml().expect("serializes");

    let unheld = window(&remote, false);
    assert!(!wire(&unheld).contains("held ="), "{}", wire(&unheld));
    let held = window(&remote, true);
    assert!(wire(&held).contains("held = true"), "{}", wire(&held));
    let back = RestoreManifest::from_toml(&wire(&held)).expect("parses");
    assert_eq!(back, held, "held survives the wire");

    assert_eq!(
        back.first_leaf_cwd().as_deref(),
        (!cfg!(unix)).then_some(remote.as_str()),
        "the held pane's remote folder"
    );
    assert_eq!(
        unheld.first_leaf_cwd().as_deref(),
        Some(remote.as_str()),
        "NEGATIVE CONTROL: the shell's own folder"
    );
    assert_eq!(
        window(&here, true).first_leaf_cwd().as_deref(),
        Some(here.as_str()),
        "NEGATIVE CONTROL: a held pane's folder that is here"
    );
    let mut hosted = window(&remote, true);
    if let RestoredSplitTree::Leaf {
        view: RestoredView::Terminal(leaf),
    } = &mut hosted.windows[0].restored_tabs[0].root
    {
        leaf.agent = Some(Box::new(AgentRestore {
            claude_auth: None,
            word: String::new(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: None,
            cwd: remote.clone(),
            version: None,
            home: None,
            login: None,
            run: None,
            cause: None,
            tab: None,
        }));
    }
    assert_eq!(
        hosted.first_leaf_cwd().as_deref(),
        Some(remote.as_str()),
        "the agent aterm hosted ran on this machine: its gone folder is asked for, and \
         the spawn says it was not found"
    );
}

#[test]
fn parse_is_fail_safe() {
    assert!(RestoreManifest::from_toml("this is not toml {{{").is_none());
    assert!(RestoreManifest::from_toml("").is_none());
    let mut m = sample();
    m.schema = 999;
    let toml = aterm_toml::to_string(&m).unwrap();
    assert!(
        RestoreManifest::from_toml(&toml).is_none(),
        "stale schema is ignored"
    );
}

#[test]
fn a_one_shot_write_leaves_only_its_manifest() {
    let dir = std::env::temp_dir().join(format!("aterm-restore-once-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let once = dir.join("seamless-4242-abcd.layout.toml");
    let manifest = sample();
    write_once_to(&once, &manifest).unwrap();
    let names = |dir: &Path| {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    assert_eq!(names(&dir), ["seamless-4242-abcd.layout.toml"]);
    assert_eq!(
        std::fs::read_to_string(&once).unwrap(),
        manifest.to_toml().unwrap()
    );
    let shared = dir.join("session.toml");
    write_to(&shared, &manifest).unwrap();
    assert_eq!(
        names(&dir),
        [
            ".session.toml.aterm-restore.lock",
            "seamless-4242-abcd.layout.toml",
            "session.toml"
        ],
        "a shared name keeps its lock"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_dead_pids_restore_temporaries_go_under_the_lock() {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let elsewhere = aterm_tempfile::tempdir().expect("scratch dir");
    let target = elsewhere.path().join("layout.toml");
    std::fs::write(&target, "not ours").unwrap();
    let (dead, live) = (
        crate::crash_journal::tests::reaped_pid(),
        std::os::unix::process::parent_id(),
    );
    let swept = [
        format!(".session.toml.aterm-write-{dead}-1-0.tmp"),
        format!(".session.toml.aterm-claimed-{dead}-1-0.tmp"),
    ];
    let left = [
        format!(".session.toml.aterm-write-{live}-1-0.tmp"),
        format!(".cell-metrics.toml.aterm-write-{dead}-1-0.tmp"),
    ];
    for name in swept.iter().chain(&left) {
        std::fs::write(dir.path().join(name), "a layout").unwrap();
    }
    let link = dir
        .path()
        .join(format!(".session.toml.aterm-claimed-{dead}-2-0.tmp"));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let path = dir.path().join("session.toml");
    write_to(&path, &sample()).expect("write");
    for name in &swept {
        assert!(!dir.path().join(name).exists(), "{name} is swept");
    }
    for name in &left {
        assert!(dir.path().join(name).exists(), "{name} is left");
    }
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "not ours");
    assert!(
        std::fs::symlink_metadata(&link).is_ok(),
        "only regular files are swept, never a link"
    );
    assert_eq!(
        take_from(&path),
        Some(sample()),
        "the layout itself is whole"
    );
}

#[test]
fn unique_durable_write_replaces_existing_without_touching_fixed_temp_alias() {
    let dir =
        std::env::temp_dir().join(format!("aterm-restore-replace-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.toml");
    let fixed = path.with_extension("toml.tmp");
    std::fs::write(&fixed, "unrelated").unwrap();
    std::fs::write(&path, "stale").unwrap();

    let manifest = sample();
    write_to(&path, &manifest).unwrap();
    assert_eq!(
        RestoreManifest::from_toml(&std::fs::read_to_string(&path).unwrap()),
        Some(manifest)
    );
    assert_eq!(std::fs::read_to_string(&fixed).unwrap(), "unrelated");
    assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        !name.contains("aterm-write")
    }));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_takers_have_exactly_one_committed_consumer() {
    let dir =
        std::env::temp_dir().join(format!("aterm-restore-consume-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("session.toml");
    let manifest = sample();
    write_to(&path, &manifest).unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut takers = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let barrier = barrier.clone();
        takers.push(std::thread::spawn(move || {
            barrier.wait();
            take_from(&path)
        }));
    }
    barrier.wait();
    let results = takers
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_some()).count(), 1);
    assert!(results.into_iter().flatten().all(|value| value == manifest));
    assert!(!path.exists());
    assert!(take_from(&path).is_none());

    #[cfg(unix)]
    {
        write_to(&path, &manifest).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut takers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = barrier.clone();
            let run = dead_run();
            takers.push(std::thread::spawn(move || {
                barrier.wait();
                take_holding_from(&path, run, &no_evidence())
            }));
        }
        barrier.wait();
        let results = takers
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results.iter().filter(|(taken, _)| taken.is_some()).count(),
            1
        );
        assert!(
            results
                .iter()
                .filter_map(|(taken, _)| taken.as_ref())
                .all(|taken| taken.manifest == manifest)
        );
        assert!(!path.exists());
        assert_eq!(takes_in(&dir).len(), 1, "the winner's take, held");
        for (_, held) in results {
            held.settle();
        }
        assert!(takes_in(&dir).is_empty());
    }

    let model = aterm_models_native::restore_manifest_single_use_model();
    let locked = model.successors("LockTakeA", &model.init_state())[0].clone();
    let claimed = model.successors("ClaimA", &locked)[0].clone();
    assert!(model.successors("ReturnA", &claimed).is_empty());
    let synced = model.successors("SyncClaim", &claimed)[0].clone();
    let returned = model.successors("ReturnA", &synced)[0].clone();
    let loser_locked = model.successors("LockTakeB", &returned)[0].clone();
    let absent = model.successors("ObserveAbsentB", &loser_locked)[0].clone();
    assert_eq!(absent["returned"], 1);
    assert!(model.check_invariant("AtMostOneConsumer", &absent));
    assert!(model.check_invariant("ReturnOnlyAfterDurableClaim", &absent));

    write_to(&path, &manifest).unwrap();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let initial = buggy.init_state();
    let read_a = std::fs::read_to_string(&path).ok();
    let a_read = buggy.successors("HistoricalTakeA", &initial)[0].clone();
    assert!(path.exists(), "the name survives the read");
    assert_eq!(a_read["visible"], 1);
    assert_eq!(aterm_spec::interp::admits(&model, &initial, &a_read), None);
    assert!(!model.check_invariant("ClaimRemovesVisibleName", &a_read));
    let read_b = std::fs::read_to_string(&path).ok();
    let both_read = buggy.successors("HistoricalTakeB", &a_read)[0].clone();
    assert!(std::fs::remove_file(&path).is_ok());
    let a_returned = buggy.successors("HistoricalTakeA", &both_read)[0].clone();
    assert!(std::fs::remove_file(&path).is_err());
    let b_returned = buggy.successors("HistoricalTakeB", &a_returned)[0].clone();
    let consumed = [read_a, read_b]
        .into_iter()
        .filter_map(|text| RestoreManifest::from_toml(&text?))
        .collect::<Vec<_>>();
    assert_eq!(consumed, [manifest.clone(), manifest]);
    assert_eq!(b_returned["returned"], 2);
    assert!(!model.check_invariant("AtMostOneConsumer", &b_returned));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_manifest_journals_and_cell_metrics_follow_the_state_home_seam() {
    if !crate::test_alone::alone() {
        return;
    }
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-restore-state-home")
        .tempdir()
        .expect("scratch dir");
    let root = scratch.path().to_path_buf();
    crate::test_env::scoped("ATERM_STATE_HOME", &root, || {
        assert_eq!(manifest_path(), Some(root.join("session.toml")));
        assert_eq!(crate::crash_journal::journal_dir(), Some(root.clone()));
        assert_eq!(cell_metrics_path(), Some(root.join("cell-metrics.toml")));
        assert_eq!(
            aterm_types::dirs::logs_dir(),
            Some(root.join("logs")),
            "the crash evidence the claim reads sits under the same root"
        );
    });
    crate::test_env::scoped("ATERM_STATE_HOME", "relative/state", || {
        assert_eq!(manifest_path(), None, "a relative seam is refused");
        assert_eq!(crate::crash_journal::journal_dir(), None);
        assert_eq!(cell_metrics_path(), None);
        assert!(write(&RestoreManifest::new(Vec::new())).is_err());
    });
    crate::test_env::scoped_unset("ATERM_STATE_HOME", || {
        let data = aterm_types::dirs::aterm_data_dir();
        assert_eq!(
            data,
            aterm_types::dirs::test_root().map(|root| root.join("state")),
            "never the person's folder"
        );
        assert_eq!(
            manifest_path(),
            data.as_ref().map(|dir| dir.join("session.toml"))
        );
        assert_eq!(crate::crash_journal::journal_dir(), data.clone());
        assert_eq!(
            cell_metrics_path(),
            data.as_ref().map(|dir| dir.join("cell-metrics.toml"))
        );
        assert_ne!(manifest_path(), Some(root.join("session.toml")));
    });
}

#[test]
fn the_recorded_scale_is_the_sole_cached_scale_or_nothing() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-cell-metrics-recorded-scale-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cell-metrics.toml");
    assert_eq!(recorded_cell_metrics_scale_from(&path), None, "no cache");
    store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 2.0, 24.0, 14, 30).unwrap();
    assert_eq!(recorded_cell_metrics_scale_from(&path), Some(2.0));
    store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 7, 15).unwrap();
    assert_eq!(
        recorded_cell_metrics_scale_from(&path),
        None,
        "two recorded scales are not a prediction"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cell_metrics_round_trip_per_scale() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-cell-metrics-roundtrip-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cell-metrics.toml");

    store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 7, 15).unwrap();
    store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.5, 18.0, 11, 22).unwrap();
    assert_eq!(
        load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.0, 12.0),
        Some((7, 15))
    );
    assert_eq!(
        load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.5, 18.0),
        Some((11, 22))
    );
    assert_eq!(
        load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 2.0, 24.0),
        None
    );

    let before = std::fs::read_to_string(&path).unwrap();
    store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 7, 15).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 8, 16).unwrap();
    assert_eq!(
        load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.0, 12.0),
        Some((8, 16))
    );
    assert_eq!(
        load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.5, 18.0),
        Some((11, 22))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cell_metrics_staleness_gates_fail_closed() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-cell-metrics-staleness-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cell-metrics.toml");

    assert_eq!(load_cell_metrics_from(&path, "k", 1.0, 12.0), None);
    std::fs::write(&path, "not toml [").unwrap();
    assert_eq!(load_cell_metrics_from(&path, "k", 1.0, 12.0), None);
    let _ = std::fs::remove_file(&path);

    store_cell_metrics_to(&path, "Cascadia|lh=1", 1.0, 12.0, 7, 15).unwrap();
    assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 1.0, 12.0), None);
    store_cell_metrics_to(&path, "Fira|lh=1", 2.0, 24.0, 13, 29).unwrap();
    assert_eq!(
        load_cell_metrics_from(&path, "Cascadia|lh=1", 1.0, 12.0),
        None
    );
    assert_eq!(
        load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.0),
        Some((13, 29))
    );
    assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 25.0), None);
    assert_eq!(
        load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.2),
        Some((13, 29))
    );

    store_cell_metrics_to(&path, "Fira|lh=1", 3.0, 36.0, 0, 15).unwrap();
    assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 3.0, 36.0), None);
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("cell_w = 13", "cell_w = 99999");
    std::fs::write(&path, text).unwrap();
    assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.0), None);

    let _ = std::fs::remove_file(&path);
    store_cell_metrics_to(&path, "Fira|lh=1", 1.0, 12.0, 7, 15).unwrap();
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("schema = 1", "schema = 999");
    std::fs::write(&path, text).unwrap();
    assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 1.0, 12.0), None);
    let _ = std::fs::remove_dir_all(&dir);
}

fn numbered_sample() -> RestoreManifest {
    let mut m = sample();
    let mut next = 0;
    m.fill_terminals(&mut |t| {
        t.local_id = Some(next);
        next += 1;
    });
    m
}

#[test]
fn a_hosted_agent_rides_its_leaf_through_the_codec() {
    fn agents(m: &RestoreManifest) -> Vec<Option<AgentRestore>> {
        let mut out = Vec::new();
        for w in &m.windows {
            for t in &w.restored_tabs {
                t.root
                    .visit_terminals(&mut |t| out.push(t.agent.as_deref().cloned()));
            }
        }
        out
    }
    let claude = AgentRestore {
        claude_auth: None,
        word: String::new(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/Users//me/.local/bin/claude".into(),
        argv: vec![
            "/Users//me/.local/bin/claude".into(),
            "--dangerously-skip-permissions".into(),
        ],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/Users//me/work".into(),
        version: Some("2.1.283".into()),
        home: None,
        login: None,
        run: None,
        cause: None,
        tab: Some(OLD_TAB.into()),
    };
    let mut m = numbered_sample();
    assert!(agents(&m).len() >= 2, "the sample has leaves to fill");
    assert!(agents(&m).iter().all(Option::is_none), "never filled: none");
    m.fill_agents(&|id| (id == 1).then(|| claude.clone()));
    let filled = agents(&m);
    assert_eq!(filled.iter().filter(|a| a.is_some()).count(), 1);
    assert_eq!(filled[1].as_ref(), Some(&claude));
    let text = m.to_toml().expect("toml");
    let back = RestoreManifest::from_toml(&text).expect("parse");
    assert_eq!(agents(&back), filled, "through the codec unchanged");
    let older = text.replace(&format!("tab = \"{OLD_TAB}\"\n"), "");
    assert_ne!(older, text, "the tab is written");
    let older = RestoreManifest::from_toml(&older).expect("an older layout parses");
    let older = agents(&older);
    assert_eq!(older[1].as_ref().map(|a| a.tab.as_deref()), Some(None));
}

#[test]
fn a_restored_agent_names_the_tab_it_ran_in_only_for_another() {
    let snap = |tab: &str| aterm_agent::harness::relaunch::Snapshot {
        launch_env: None,
        auth_observed: true,
        tab: tab.to_string(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        shell: 4343,
        program: std::path::PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/tmp".into(),
        version: None,
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        profile: None,
    };
    let agent = AgentRestore::of(snap(OLD_TAB));
    assert_eq!(agent.tab.as_deref(), Some(OLD_TAB));
    assert_eq!(
        agent.ran_in_another("s-00000000000000000002").as_deref(),
        Some(OLD_TAB)
    );
    assert_eq!(agent.ran_in_another(OLD_TAB), None, "a kept sid");
    assert_eq!(AgentRestore::of(snap("")).tab, None, "none to write");
}

const OLD_TAB: &str = "s-00000000000000000001";

#[test]
fn a_tabs_holds_ride_its_leaf_and_its_agent_is_not_promised() {
    use crate::fabric::{CARRIED_UNREADABLE, Hold};
    let agent = AgentRestore {
        claude_auth: None,
        word: String::new(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/Users//me/.local/bin/claude".into(),
        argv: vec!["/Users//me/.local/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/Users//me/work".into(),
        version: Some("2.1.300".into()),
        home: None,
        login: None,
        run: None,
        cause: None,
        tab: None,
    };
    fn leaves(m: &RestoreManifest) -> Vec<TerminalLeafRestore> {
        let mut out = Vec::new();
        for w in &m.windows {
            for t in &w.restored_tabs {
                t.root.visit_terminals(&mut |t| out.push(t.clone()));
            }
        }
        out
    }
    let layout = |hold: Option<&str>| {
        let mut m = sample();
        if let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(leaf),
        } = &mut m.windows[0].restored_tabs[0].root
        {
            leaf.local_id = Some(7);
            leaf.agent = Some(Box::new(agent.clone()));
        }
        let row = hold.map(str::to_string);
        m.fill_holds(&|id| row.iter().filter(|_| id == 7).cloned().collect());
        let text = m.to_toml().expect("toml");
        let back = RestoreManifest::from_toml(&text).expect("parse");
        assert_eq!(leaves(&back), leaves(&m), "through the codec unchanged");
        (text, leaves(&back))
    };
    let (_, back) = layout(Some("local pause"));
    assert_eq!(back[0].holds, ["local pause"]);
    assert!(
        back[1..].iter().all(|leaf| leaf.holds.is_empty()),
        "{back:?}"
    );
    assert_eq!(
        back[0].carried_holds().of_origin("local").first(),
        Some(Hold {
            reason: "pause".to_string(),
            origin: "local".to_string(),
        })
    );
    assert_eq!(
        back[0].resumes_agent(),
        None,
        "the reopened row does not promise it"
    );
    let garbled = TerminalLeafRestore {
        holds: vec!["garbled row here".to_string()],
        ..back[0].clone()
    };
    assert_eq!(
        garbled.carried_holds().first(),
        Some(Hold {
            reason: CARRIED_UNREADABLE.to_string(),
            origin: "local".to_string(),
        })
    );
    assert_eq!(garbled.resumes_agent(), None, "a garbled row holds");
    let (_, back) = layout(Some("fleet fabric-lost"));
    assert!(
        back[0].carried_holds().has("fleet") && !back[0].carried_holds().has("local"),
        "{back:?}"
    );
    assert_eq!(back[0].resumes_agent(), None, "fleet");
    let (text, back) = layout(None);
    assert!(!text.contains("holds ="), "no field written: {text}");
    assert_eq!(
        (back[0].carried_holds().is_held(), back[0].resumes_agent()),
        (false, Some(&agent)),
        "older"
    );
}

#[test]
fn a_leaf_names_its_agent_and_an_unknown_one_is_relaunched_by_nothing() {
    use aterm_agent::harness::relaunch::{CodexRun, Snapshot};
    use aterm_agent::harness::upgrade::Agent;
    let claude = Snapshot {
        launch_env: None,
        auth_observed: true,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        profile: None,
        tab: "s-old".into(),
        pid: 5151,
        start: "Sat Sep 27 01:02:03 2026".into(),
        shell: 5150,
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some("0badf00d-1111-2222-3333-444455556666".into()),
        cwd: "/work/c".into(),
        version: Some("2.1.283".into()),
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    };
    let codex = Snapshot {
        program: "/opt/codex/bin/codex".into(),
        run: aterm_agent::harness::relaunch::AgentRun::Codex(CodexRun {
            home: "/Users//me/.codex".into(),
            home_env: None,
            self_claimed: None,
        }),
        ..claude.clone()
    };
    let wire = |snap: &Snapshot| aterm_toml::to_string(&AgentRestore::of(snap.clone())).unwrap();
    let read = |text: &str| aterm_toml::from_str::<AgentRestore>(text).expect("reads");
    assert!(!wire(&claude).contains("word"), "{}", wire(&claude));
    assert!(
        wire(&codex).contains("word = \"codex\""),
        "{}",
        wire(&codex)
    );
    assert_eq!(read(&wire(&claude)).agent(), Some(Agent::Claude));
    assert_eq!(read(&wire(&codex)).agent(), Some(Agent::Codex));
    let older = wire(&codex).replace("word = \"codex\"\n", "");
    assert!(!older.contains("word"), "{older}");
    assert_eq!(read(&older).agent(), Some(Agent::Codex));
    let no_run = read(wire(&codex).split("[codex]").next().unwrap());
    assert!(
        no_run.run.is_none() && no_run.word == "codex",
        "{:?}",
        no_run
    );
    assert_eq!(no_run.agent(), None, "codex without its run");
    let mut claude_with_run = read(&wire(&codex));
    claude_with_run.word = "claude".to_string();
    assert_eq!(claude_with_run.agent(), None, "claude beside a Codex run");
    assert!(
        aterm_toml::from_str::<AgentRestore>(&format!("word = \"opencode\"\n{}", wire(&claude)))
            .is_err(),
        "an unknown provider must not carry Claude authentication attribution"
    );
    let mut foreign = AgentRestore::of(claude.clone());
    foreign.claude_auth = None;
    let foreign_wire = aterm_toml::to_string(&foreign).unwrap();
    let newer = read(&format!("word = \"opencode\"\n{foreign_wire}"));
    assert_eq!(newer.agent(), None);
    assert!(
        newer.snapshot("s-new".into(), 6161).is_none(),
        "relaunched by nothing"
    );
    let muse_no_run = read(&format!("word = \"muse\"\n{foreign_wire}"));
    assert_eq!(muse_no_run.agent(), None, "muse without its run");
    assert!(muse_no_run.resume_line().is_none());
    assert!(muse_no_run.snapshot("s-new".into(), 6161).is_none());
    for word in ["agy", "gemini"] {
        let no_run = read(&format!("word = \"{word}\"\n{foreign_wire}"));
        assert_eq!(no_run.agent(), None, "{word} without its run");
        assert_eq!(no_run.name(), None, "{word}");
        assert!(no_run.resume_line().is_none(), "{word}");
        assert!(no_run.snapshot("s-new".into(), 6161).is_none(), "{word}");
        let mut beside_codex = read(&wire(&codex));
        beside_codex.word = word.to_string();
        assert_eq!(beside_codex.agent(), None, "{word} beside a Codex run");
    }
    assert!(
        read(&wire(&claude))
            .snapshot("s-new".into(), 6161)
            .is_some(),
        "NEGATIVE CONTROL: a Claude Code leaf comes back"
    );
}

fn first_agent_of(m: &RestoreManifest) -> Option<AgentRestore> {
    let mut found = None;
    for w in &m.windows {
        for t in &w.restored_tabs {
            t.root.all_terminals(&mut |term| {
                if let Some(agent) = term.agent.as_deref() {
                    found = Some(agent.clone());
                    return false;
                }
                true
            });
            if found.is_some() {
                return found;
            }
        }
    }
    found
}

#[test]
fn a_restored_agents_home_and_login_ride_its_leaf_into_its_relaunch_line() {
    use aterm_agent::harness::relaunch::{CodexRun, Snapshot, exit_line};
    use aterm_agent::harness::upgrade::Dialect;
    const SESSION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
    const HOME: &str = if cfg!(windows) {
        "C:/Users//me/lab home/.claude"
    } else {
        "/Users//me/lab home/.claude"
    };
    const LOGIN: &str = "/Users//me/logins/work-2";

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct AgentRestore0101 {
        pid: u32,
        start: String,
        program: String,
        argv: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        codex: Option<CodexRestore>,
    }

    let ran = |home: Option<&str>, login: Option<&str>| Snapshot {
        launch_env: None,
        auth_observed: true,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        profile: None,
        tab: "s-old".into(),
        pid: 4242,
        start: "Fri Oct  9 01:02:03 2026".into(),
        shell: 4241,
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some(SESSION.into()),
        cwd: "/work".into(),
        version: Some("2.1.295".into()),
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: login.map(str::to_string),
        home: home.map(str::to_string),
        claude_dir: None,
    };
    let through = |snap: Snapshot| -> Vec<(&str, Snapshot)> {
        let mut m = numbered_sample();
        let carried = AgentRestore::of(snap);
        m.fill_agents(&|id| (id == 0).then(|| carried.clone()));
        let layout = RestoreManifest::from_toml(&m.to_toml().expect("toml")).expect("parse");
        let header = crate::crash_journal::JournalHeader::now(7, false);
        let (_, journal) = crate::crash_journal::decode(
            &crate::crash_journal::encode(&m, &header).expect("the image"),
        )
        .expect("the journal reads");
        [("layout", layout), ("crash journal", journal)]
            .into_iter()
            .map(|(format, m)| {
                let agent = first_agent_of(&m).expect("the leaf carries its agent");
                let back = agent
                    .snapshot("s-new".into(), 6161)
                    .expect("an agent this build knows");
                let back = Snapshot {
                    dialect: Some(Dialect::Zsh),
                    ..back
                };
                (format, back)
            })
            .collect()
    };
    let program = format!("'/opt/claude/bin/claude' '--resume' '{SESSION}'");
    let plain = format!(" CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 {program}");
    for (format, back) in through(ran(Some(HOME), Some(LOGIN))) {
        assert_eq!(
            (back.home.as_deref(), back.login.as_deref()),
            (Some(HOME), Some(LOGIN)),
            "{format}"
        );
        assert_eq!(
            exit_line(std::path::Path::new("/Users//me"), &back),
            Some(format!(
                " CLAUDE_CONFIG_DIR='{HOME}' CLAUDE_SECURESTORAGE_CONFIG_DIR={LOGIN} \
                 CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 {program}"
            )),
            "{format}: the home, then the login, on the line"
        );
    }
    for (format, back) in through(ran(None, Some(""))) {
        assert_eq!(
            (back.home.as_deref(), back.login.as_deref()),
            (None, Some("")),
            "{format}"
        );
        assert_eq!(
            exit_line(std::path::Path::new("/Users//me"), &back),
            Some(format!(
                " CLAUDE_SECURESTORAGE_CONFIG_DIR='' CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 \
                 {program}"
            )),
            "{format}"
        );
    }
    for (format, back) in through(ran(None, None)) {
        assert_eq!((&back.home, &back.login), (&None, &None), "{format}");
        assert_eq!(
            exit_line(std::path::Path::new("/Users//me"), &back).as_deref(),
            Some(plain.as_str()),
            "{format}"
        );
    }
    let codex = Snapshot {
        run: AgentRun::Codex(CodexRun {
            home: "/Users//me/lab codex".into(),
            home_env: Some("/Users//me/lab codex/".into()),
            self_claimed: None,
        }),
        auth_observed: false,
        ..ran(None, None)
    };
    let ran_codex = codex.codex().cloned().expect("a Codex's run");
    let unchanged = |snap: &Snapshot| {
        for (format, back) in through(snap.clone()) {
            assert_eq!(
                Snapshot {
                    tab: snap.tab.clone(),
                    shell: snap.shell,
                    ..back
                },
                *snap,
                "{format}: everything but the new tab and shell, unchanged"
            );
        }
    };
    let vendor = |snap: &Snapshot| {
        let wire = aterm_toml::to_string(&AgentRestore::of(snap.clone())).expect("the leaf");
        let read: AgentRestore = aterm_toml::from_str(&wire).expect("reads");
        assert_eq!(read.name(), Some("Codex"), "{wire}");
        assert!(!read.relaunched() && !read.resumes(), "{wire}");
        assert_eq!(read.resume_line(), None, "{wire}");
        assert!(read.snapshot("s-new".into(), 6161).is_none(), "{wire}");
        wire
    };
    assert!(vendor(&codex).contains("home_env"));
    let unset = Snapshot {
        run: AgentRun::Codex(CodexRun {
            home_env: None,
            ..ran_codex.clone()
        }),
        ..codex.clone()
    };
    let wire = vendor(&unset);
    assert!(!wire.contains("home_env"), "{wire}");
    let line = aterm_agent::harness::self_claimed::Resume {
        argv: vec![
            "/Users//me/.aterm/agents/codex".into(),
            "--no-alt-screen".into(),
            "resume".into(),
            SESSION.into(),
        ],
        session: SESSION.into(),
        codex_home: Some("/Users//me/lab codex/".into()),
        codex_auth_path: Some("/Users//me/.aterm/accounts-codex/work/auth.json".into()),
        cwd: Some("/work".into()),
    };
    let alab = Snapshot {
        run: AgentRun::Codex(CodexRun {
            self_claimed: Some(line.clone()),
            ..ran_codex.clone()
        }),
        ..codex.clone()
    };
    unchanged(&alab);
    for (format, back) in through(alab.clone()) {
        assert_eq!(
            aterm_agent::harness::relaunch::self_claimed_line(&back),
            Some(&line),
            "{format}: the validated line, unchanged"
        );
    }
    let wire = aterm_toml::to_string(&AgentRestore::of(alab.clone())).expect("the leaf");
    assert!(wire.contains("[codex.alab]"), "{wire}");
    assert_eq!(
        aterm_toml::from_str::<AgentRestore>(&wire)
            .expect("reads")
            .resume_line(),
        Some(format!(
            "CODEX_HOME='/Users//me/lab codex/' \
             CODEX_AUTH_PATH=/Users//me/.aterm/accounts-codex/work/auth.json \
             /Users//me/.aterm/agents/codex --no-alt-screen resume {SESSION}"
        )),
        "the person's line is the validated one"
    );
    let with_profile = AgentRestore::of(Snapshot {
        argv: vec![
            "/Users//me/.aterm/agents/codex".into(),
            "-p".into(),
            "work".into(),
        ],
        ..alab.clone()
    });
    assert!(
        with_profile.resumes(),
        "Codex's -p <profile> is an interactive TUI flag, not Claude's -p one-shot"
    );
    let non_tui = AgentRestore::of(Snapshot {
        argv: vec![
            "/Users//me/.aterm/agents/codex".into(),
            "exec".into(),
            "hi".into(),
        ],
        ..alab
    });
    assert!(
        !non_tui.resumes(),
        "a non-TUI Codex subcommand never marks resumes"
    );
    let older = wire
        .split("[codex.alab]")
        .next()
        .expect("before the table")
        .to_string();
    assert!(
        aterm_toml::from_str::<AgentRestore>(&older)
            .expect("an older leaf reads")
            .snapshot("s-new".into(), 6161)
            .is_none(),
        "an older leaf with no line is relaunched by nobody"
    );
    for (label, tampered) in [
        (
            "empty argv",
            wire.replace(
                "argv = [\"/Users//me/.aterm/agents/codex\", \"--no-alt-screen\", \"resume\", \"0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c\"]",
                "argv = []",
            ),
        ),
        (
            "foreign argv flag",
            wire.replace("\"--no-alt-screen\"", "\"--evil-flag\""),
        ),
        (
            "relative codex_auth_path",
            wire.replace(
                "/Users//me/.aterm/accounts-codex/work/auth.json",
                "work/auth.json",
            ),
        ),
        (
            "unicode line separator in codex_home",
            wire.replace("/Users//me/lab codex/", "/Users//me/lab\u{2028}codex/"),
        ),
    ] {
        assert_ne!(tampered, wire, "{label} fixture must mutate wire");
        let parsed: AgentRestore = aterm_toml::from_str(&tampered).expect("reads");
        assert!(!parsed.relaunched(), "{label}: must not mark relaunched");
        assert!(!parsed.resumes(), "{label}: must not mark resumes");
        assert_eq!(parsed.resume_line(), None, "{label}: must emit no resume_line");
        assert!(
            parsed.snapshot("s-new".into(), 6161).is_none(),
            "{label}: must yield no relaunch snapshot"
        );
    }

    let old = AgentRestore0101 {
        pid: 4242,
        start: "Fri Oct  9 01:02:03 2026".into(),
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some(SESSION.into()),
        cwd: "/work".into(),
        version: Some("2.1.295".into()),
        codex: None,
    };
    let written = aterm_toml::to_string(&old).expect("0.101.0's record");
    let neither = AgentRestore {
        tab: None,
        ..AgentRestore::of(Snapshot {
            auth_observed: false,
            ..ran(None, None)
        })
    };
    assert_eq!(
        aterm_toml::to_string(&neither).expect("this build's"),
        written,
        "an agent with neither is written as 0.101.0 writes it"
    );
    let read: AgentRestore = aterm_toml::from_str(&written).expect("this build reads it");
    assert_eq!((&read.home, &read.login), (&None, &None));
    let back = Snapshot {
        dialect: Some(Dialect::Zsh),
        ..read
            .snapshot("s-new".into(), 6161)
            .expect("an agent this build knows")
    };
    assert_eq!(
        exit_line(std::path::Path::new("/Users//me"), &back).as_deref(),
        Some(plain.as_str())
    );
    let both = aterm_toml::to_string(&AgentRestore::of(ran(Some(HOME), Some(LOGIN))))
        .expect("this build's record");
    assert!(
        both.contains("home = ") && both.contains("login = "),
        "{both}"
    );
    assert_eq!(
        aterm_toml::from_str::<AgentRestore0101>(&both).expect("0.101.0 reads it"),
        old
    );
}

#[test]
fn a_handed_tab_program_names_no_shell_and_a_shells_job_its_shell() {
    use aterm_agent::harness::relaunch::NO_SHELL;
    let agent = AgentRestore {
        claude_auth: None,
        word: String::new(),
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".into(),
        program: "/Users//me/.local/bin/claude".into(),
        argv: vec!["/Users//me/.local/bin/claude".into()],
        session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
        cwd: "/Users//me/work".into(),
        version: Some("2.1.283".into()),
        home: None,
        login: None,
        run: None,
        cause: None,
        tab: None,
    };
    let own = agent
        .clone()
        .handed_snapshot("s-new".into(), 4242)
        .expect("a Claude Code leaf");
    assert_eq!(
        (own.tab.as_str(), own.pid, own.shell),
        ("s-new", 4242, NO_SHELL)
    );
    let under = agent
        .handed_snapshot("s-new".into(), 4241)
        .expect("a Claude Code leaf");
    assert_eq!((under.pid, under.shell), (4242, 4241), "CONTROL: its shell");
}

#[test]
fn the_agent_leaf_reads_both_ways_across_0_104_and_0_105() {
    use aterm_agent::harness::relaunch::{CodexRun, Snapshot};
    use aterm_agent::harness::upgrade::{Agent, is_session_id};
    const SESSION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
    const THREAD: &str = "01a0e4bd-2b37-75a0-9d52-7f1e2c3a4b5c";
    const MUSE: &str = "01a148f8-9301-7752-96db-fef456f88074";
    const AGY: &str = "eaeec4fb-4e43-47da-b46f-8a10c0fee186";

    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    struct CodexRestore0104 {
        home: String,
        embedded: bool,
        started_ms: u64,
    }
    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    struct AgentRestore0104 {
        pid: u32,
        start: String,
        program: String,
        argv: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        login: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        codex: Option<CodexRestore0104>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<String>,
    }
    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    struct CodexRestore0105 {
        home: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home_env: Option<String>,
        embedded: bool,
        started_ms: u64,
    }
    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    struct AgentRestore0105 {
        pid: u32,
        start: String,
        program: String,
        argv: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        login: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        codex: Option<CodexRestore0105>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        word: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<String>,
    }
    fn agent_0104(leaf: &AgentRestore0104) -> &'static str {
        if leaf.codex.is_some() {
            "codex"
        } else {
            "claude"
        }
    }
    fn resumes_0104(leaf: &AgentRestore0104) -> bool {
        leaf.session.as_deref().is_some_and(is_session_id)
            && !aterm_agent::harness::upgrade::one_shot(&leaf.argv)
    }
    fn agent_0105(leaf: &AgentRestore0105) -> Option<&'static str> {
        let agent = match leaf.word.as_str() {
            "" if leaf.codex.is_some() => "codex",
            "" | "claude" => "claude",
            "codex" => "codex",
            _ => return None,
        };
        (leaf.codex.is_some() == (agent == "codex")).then_some(agent)
    }

    let claude = Snapshot {
        launch_env: None,
        auth_observed: true,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        profile: None,
        tab: "s-old".into(),
        pid: 4242,
        start: "Fri Oct 16 01:02:03 2026".into(),
        shell: 4241,
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some(SESSION.into()),
        cwd: "/work".into(),
        version: Some("2.1.301".into()),
        run: AgentRun::Claude,
        dialect: None,
        login: Some(String::new()),
        home: Some("/Users//me/.claude-work".into()),
        claude_dir: None,
    };
    let codex = Snapshot {
        program: "/opt/codex/bin/codex".into(),
        argv: vec!["/opt/codex/bin/codex".into(), "--no-daemon".into()],
        session: Some(THREAD.into()),
        version: Some("0.163.0".into()),
        run: AgentRun::Codex(CodexRun {
            home: "/Users//me/.codex".into(),
            home_env: Some("/Users//me/.codex".into()),
            self_claimed: None,
        }),
        login: None,
        home: None,
        ..claude.clone()
    };
    let wire = |snap: &Snapshot| {
        aterm_toml::to_string(&AgentRestore::of(snap.clone())).expect("this build's leaf")
    };

    for (snap, agent) in [(&claude, "claude"), (&codex, "codex")] {
        let text = wire(snap);
        let old = aterm_toml::from_str::<AgentRestore0104>(&text).expect("0.104.0 reads it");
        assert_eq!(agent_0104(&old), agent, "{text}");
        assert!(resumes_0104(&old), "{text}");
        let older = aterm_toml::from_str::<AgentRestore0105>(&text).expect("0.105.0 reads it");
        assert_eq!(agent_0105(&older), Some(agent), "{text}");
        assert!(text.contains("tab = \"s-old\"\n"), "{text}");
        let mut legacy = AgentRestore::of(snap.clone());
        legacy.tab = None;
        legacy.claude_auth = None;
        let older_text = aterm_toml::to_string(&older).expect("0.105.0 writes it");
        assert_eq!(
            older_text,
            aterm_toml::to_string(&legacy).unwrap(),
            "0.105.0 preserves its known fields for {agent}"
        );
        assert!(
            aterm_toml::from_str::<AgentRestore>(&older_text)
                .unwrap()
                .claude_auth
                .is_none(),
            "older writers do not invent captured attribution"
        );
    }

    let codex_0104 = AgentRestore0104 {
        pid: 4343,
        start: "Fri Oct 16 01:02:03 2026".into(),
        program: "/opt/codex/bin/codex".into(),
        argv: vec!["/opt/codex/bin/codex".into(), "--no-daemon".into()],
        session: Some(THREAD.into()),
        cwd: "/work".into(),
        version: Some("0.157.1".into()),
        home: None,
        login: None,
        codex: Some(CodexRestore0104 {
            home: "/Users//me/.codex".into(),
            embedded: true,
            started_ms: 1_790_543_997_000,
        }),
        cause: None,
    };
    let claude_0104 = AgentRestore0104 {
        program: "/opt/claude/bin/claude".into(),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some(SESSION.into()),
        home: Some("/Users//me/.claude-work".into()),
        codex: None,
        ..codex_0104.clone()
    };
    let text = aterm_toml::to_string(&claude_0104).expect("0.104.0's leaf");
    let read: AgentRestore = aterm_toml::from_str(&text).expect("this build reads it");
    assert_eq!(read.agent(), Some(Agent::Claude), "{text}");
    let back = read.snapshot("s-new".into(), 6161).expect("relaunched");
    assert_eq!(back.agent(), Agent::Claude);
    assert_eq!(back.session, claude_0104.session);
    assert_eq!(back.home, claude_0104.home);
    assert!(back.codex().is_none(), "{text}");
    let text = aterm_toml::to_string(&codex_0104).expect("0.104.0's leaf");
    let read: AgentRestore = aterm_toml::from_str(&text).expect("this build reads it");
    assert_eq!(read.agent(), Some(Agent::Codex), "{text}");
    assert!(read.snapshot("s-new".into(), 6161).is_none(), "{text}");
    let codex_0105 = AgentRestore0105 {
        pid: 4343,
        start: "Fri Oct 16 01:02:03 2026".into(),
        program: "/opt/codex/bin/codex".into(),
        argv: vec!["/opt/codex/bin/codex".into()],
        session: Some(THREAD.into()),
        cwd: "/work".into(),
        version: Some("0.163.0".into()),
        home: None,
        login: None,
        codex: Some(CodexRestore0105 {
            home: "/Users//me/lab codex".into(),
            home_env: Some("/Users//me/lab codex/".into()),
            embedded: false,
            started_ms: 1_790_543_997_000,
        }),
        word: "codex".into(),
        cause: Some("asked".into()),
    };
    let text = aterm_toml::to_string(&codex_0105).expect("0.105.0's leaf");
    let read: AgentRestore = aterm_toml::from_str(&text).expect("this build reads it");
    assert_eq!(read.agent(), Some(Agent::Codex));
    assert_eq!(
        aterm_toml::to_string(&read).expect("written back"),
        text,
        "read and written back as 0.105.0 wrote it"
    );
    let Some(RunRestore::Codex(run)) = &read.run else {
        panic!("a Codex run: {read:?}");
    };
    assert_eq!(run.home_env.as_deref(), Some("/Users//me/lab codex/"));
    assert!(
        read.snapshot("s-new".into(), 6161).is_none(),
        "a vendor `codex`'s leaf is relaunched by nobody"
    );

    let muse = Snapshot {
        program: "/opt/pkg/store/muse/2001004010006427002/bin/muse".into(),
        argv: vec![
            "/opt/pkg/store/muse/2001004010006427002/bin/muse".into(),
            "--base-url".into(),
            "http://127.0.0.1:9".into(),
        ],
        session: Some(MUSE.into()),
        version: Some("1.4.10-R6427.1".into()),
        run: AgentRun::Muse(aterm_agent::harness::relaunch::MuseRun {
            data: "/Users//me/.local/share".into(),
            env: vec![("XDG_DATA_HOME".into(), "/Users//me/.local/share".into())],
            workspace: Some("/work/proj".into()),
            lock: Some("/Users//me/.local/share/muse/sessions/2026/10/17/x/.session.lock".into()),
            started_ms: 1_790_543_997_000,
            block: Some(7),
        }),
        login: None,
        home: None,
        ..claude.clone()
    };
    let lane = wire(&muse);
    assert!(
        lane.contains("word = \"muse\"\n") && lane.contains("[muse]\n"),
        "{lane}"
    );
    let (top, table) = lane.split_once("[muse]").expect("its table");
    assert!(
        table.contains(&format!("session = \"{MUSE}\"")) && !top.contains("session = "),
        "its session in its table alone: {lane}"
    );
    let old = aterm_toml::from_str::<AgentRestore0104>(&lane).expect("0.104.0 takes it");
    assert_eq!(old.session, None, "{lane}");
    assert_eq!(agent_0104(&old), "claude", "0.104.0 has no word");
    assert!(
        !resumes_0104(&old),
        "0.104.0 finds no conversation to relaunch it on"
    );
    let older = aterm_toml::from_str::<AgentRestore0105>(&lane).expect("0.105.0 takes it");
    assert_eq!(older.word, "muse");
    assert_eq!(agent_0105(&older), None, "0.105.0: no agent's");
    let here: AgentRestore = aterm_toml::from_str(&lane).expect("this build takes it");
    assert_eq!(here.agent(), Some(Agent::Muse), "read as Muse Code's");
    assert_eq!(here.name(), Some("Muse Code"));
    assert!(here.resumes(), "its own table's session resumes");
    assert_eq!(
        here.resume_line().as_deref(),
        Some(
            format!(
                "XDG_DATA_HOME=/Users//me/.local/share muse --contributor-mode off resume \
                 {MUSE}"
            )
            .as_str()
        ),
        "Muse Code's own resume line, behind its data home"
    );
    let back = here
        .clone()
        .snapshot("s-new".into(), 6161)
        .expect("relaunched by its lane");
    assert_eq!(back.agent(), Agent::Muse);
    assert_eq!(back.session.as_deref(), Some(MUSE));
    assert_eq!(
        back.muse().map(|run| (
            run.data.clone(),
            run.env.clone(),
            run.workspace.clone(),
            run.lock.clone(),
            run.block
        )),
        Some((
            PathBuf::from("/Users//me/.local/share"),
            vec![(
                "XDG_DATA_HOME".to_string(),
                "/Users//me/.local/share".to_string()
            )],
            Some("/work/proj".to_string()),
            None,
            None
        )),
        "its run back, its lock and block never carried"
    );
    assert_eq!(
        aterm_toml::to_string(&here).expect("written back"),
        lane,
        "read and written back as written"
    );
    let unproven = wire(&Snapshot {
        session: None,
        ..muse.clone()
    });
    let unproven: AgentRestore = aterm_toml::from_str(&unproven).expect("read");
    assert_eq!(unproven.agent(), Some(Agent::Muse));
    assert!(!unproven.resumes() && unproven.resume_line().is_none());
    for word in [None, Some(""), Some("claude"), Some("muse"), Some("codex")] {
        let word = word.map_or_else(String::new, |w| format!("word = {w:?}\n"));
        let both = format!(
            "{}\n[codex]\nhome = \"/Users//me/.codex\"\nembedded = true\nstarted_ms = 1\n",
            lane.replacen(
                "word = \"muse\"\n",
                &format!("{word}session = {SESSION:?}\n"),
                1,
            )
            .trim_end()
        );
        assert!(
            aterm_toml::from_str::<AgentRestore>(&both).is_err(),
            "AMBIGUOUS_AGENT_RUN_TABLES_MUST_NOT_RESUME_AS_CLAUDE: {both}"
        );
    }
    let in_session = format!("{}word = \"muse\"\n", wire(&claude));
    let old = aterm_toml::from_str::<AgentRestore0104>(&in_session).expect("0.104.0");
    assert_eq!((agent_0104(&old), resumes_0104(&old)), ("claude", true));

    let agy = Snapshot {
        program: "/opt/pkg/store/agy/1000001000004000000/bin/agy".into(),
        argv: vec![
            "/opt/pkg/store/agy/1000001000004000000/bin/agy".into(),
            "--effort=high".into(),
            "--gemini_dir=/Users//me/alt".into(),
        ],
        session: Some(AGY.into()),
        version: Some("1.4.0".into()),
        run: AgentRun::Agy(aterm_agent::harness::relaunch::AgyRun {
            config: "/Users//me/alt/antigravity-cli".into(),
            env: vec![("HOME".into(), "/Users//me/agy-home".into())],
            lock: Some(format!("/Users//me/alt/antigravity-cli/presence/{AGY}.lock").into()),
            crash: Some("/Users//me/alt/antigravity-cli/crashes/crash_7_x.log".into()),
            started_ms: 1_790_543_997_000,
            block: Some(7),
        }),
        login: None,
        home: None,
        ..claude.clone()
    };
    let lane = wire(&agy);
    assert!(
        lane.contains("word = \"agy\"\n") && lane.contains("[agy]\n"),
        "{lane}"
    );
    let (top, table) = lane.split_once("[agy]").expect("its table");
    assert!(
        table.contains(&format!("conversation = \"{AGY}\"")) && !top.contains("session = "),
        "its conversation in its table alone: {lane}"
    );
    let old = aterm_toml::from_str::<AgentRestore0104>(&lane).expect("0.104.0 takes it");
    assert_eq!(old.session, None, "{lane}");
    assert_eq!(agent_0104(&old), "claude", "0.104.0 has no word");
    assert!(
        !resumes_0104(&old),
        "0.104.0 finds no conversation to relaunch it on"
    );
    let older = aterm_toml::from_str::<AgentRestore0105>(&lane).expect("0.105.0 takes it");
    assert_eq!(older.word, "agy");
    assert_eq!(agent_0105(&older), None, "0.105.0: no agent's");
    let here: AgentRestore = aterm_toml::from_str(&lane).expect("this build takes it");
    assert_eq!(here.agent(), Some(Agent::Agy), "read as agy's");
    assert_eq!(here.name(), Some("agy"));
    assert!(here.resumes(), "its own table's conversation resumes");
    assert_eq!(
        here.resume_line().as_deref(),
        Some(
            format!("HOME=/Users//me/agy-home agy --gemini_dir=/Users//me/alt --conversation={AGY}")
                .as_str()
        ),
        "agy's own resume hint, after its config flag, behind its HOME"
    );
    let back = here
        .clone()
        .snapshot("s-new".into(), 6161)
        .expect("relaunched by its lane");
    assert_eq!(back.agent(), Agent::Agy);
    assert_eq!(back.session.as_deref(), Some(AGY));
    assert_eq!(
        back.agy().map(|run| (
            run.config.clone(),
            run.env.clone(),
            run.lock.clone(),
            run.crash.clone(),
            run.block
        )),
        Some((
            PathBuf::from("/Users//me/alt/antigravity-cli"),
            vec![("HOME".to_string(), "/Users//me/agy-home".to_string())],
            None,
            None,
            None
        )),
        "its run back, its lock, crash file and block never carried"
    );
    assert_eq!(
        aterm_toml::to_string(&here).expect("written back"),
        lane,
        "read and written back as written"
    );
    let unproven = wire(&Snapshot {
        session: None,
        ..agy.clone()
    });
    let unproven: AgentRestore = aterm_toml::from_str(&unproven).expect("read");
    assert_eq!(unproven.agent(), Some(Agent::Agy));
    assert!(!unproven.resumes() && unproven.resume_line().is_none());
    for word in [None, Some(""), Some("claude"), Some("agy"), Some("muse")] {
        let word = word.map_or_else(String::new, |w| format!("word = {w:?}\n"));
        let top = lane.replacen("word = \"agy\"\n", &word, 1);
        for other in [
            "[muse]\ndata = \"/Users//me/.local/share\"\n",
            "[codex]\nhome = \"/Users//me/.codex\"\nembedded = true\nstarted_ms = 1\n",
        ] {
            let both = format!("{}\n{other}", top.trim_end());
            assert!(
                aterm_toml::from_str::<AgentRestore>(&both).is_err(),
                "AMBIGUOUS_AGENT_RUN_TABLES_MUST_NOT_RESUME_AS_CLAUDE: {both}"
            );
        }
    }
    let in_session = format!("{}word = \"agy\"\n", wire(&claude));
    let old = aterm_toml::from_str::<AgentRestore0104>(&in_session).expect("0.104.0");
    assert_eq!((agent_0104(&old), resumes_0104(&old)), ("claude", true));

    // The retired Gemini CLI's leaf exactly as v0.134.0's producer wrote it
    // (tests/fixtures/handoff/v0.134.0/carried-state): read, relaunched by
    // nothing and never another agent's, its table dropped when written back.
    let gemini_table = "\n[gemini]\nsession = \"c2d9e4f1-7a3b-4c5d-8e6f-0a1b2c3d4e5f\"\n\
                        home = \"/home/dev/.gemini\"\n\
                        chats = \"/home/dev/.gemini/tmp/7f3a9c/chats\"\n\
                        env = [\"GEMINI_CLI_HOME=/home/dev\"]\n\
                        started_ms = 1792871730000\n";
    let top = "pid = 48503\nstart = \"Mon Oct 19 09:14:02 2026\"\n\
               program = \"/home/dev/.local/bin/gemini\"\nargv = [\"gemini\"]\n\
               cwd = \"/home/dev/src/aterm\"\nversion = \"1.4.0\"\n";
    for (what, word) in [("its word", "word = \"gemini\"\n"), ("no word", "")] {
        let lane = format!("{top}{word}{gemini_table}");
        let here: AgentRestore = aterm_toml::from_str(&lane)
            .unwrap_or_else(|e| panic!("{what}: this build reads the retired leaf: {e}"));
        assert!(here.retired(), "{what}");
        assert_eq!(
            here.agent(),
            None,
            "{what}: no agent's, never Claude Code's"
        );
        assert_eq!(here.name(), None, "{what}");
        assert!(!here.resumes(), "{what}");
        assert_eq!(here.resume_line(), None, "{what}");
        assert!(
            here.clone().snapshot("s-new".into(), 6161).is_none(),
            "{what}: relaunched by nothing"
        );
        let back = aterm_toml::to_string(&here).expect("written back");
        assert!(
            back.contains("word = \"gemini\"\n") && !back.contains("[gemini]"),
            "{what}: its word kept, its retired table dropped: {back}"
        );
        let again: AgentRestore = aterm_toml::from_str(&back).expect("read again");
        assert!(again.retired() && again.agent().is_none(), "{what}: {back}");
        for other in [
            "[agy]\nconfig = \"/Users//me/.gemini/antigravity-cli\"\n",
            "[codex]\nhome = \"/Users//me/.codex\"\nembedded = true\nstarted_ms = 1\n",
        ] {
            let both = format!("{}\n{other}", lane.trim_end());
            assert!(
                aterm_toml::from_str::<AgentRestore>(&both).is_err(),
                "AMBIGUOUS_AGENT_RUN_TABLES_MUST_NOT_RESUME_AS_CLAUDE: {both}"
            );
        }
    }
    assert!(
        aterm_toml::from_str::<AgentRestore>(&format!("{}{gemini_table}", wire(&claude))).is_err(),
        "a retired provider's table must not carry Claude authentication attribution"
    );
    let mut legacy_claude = AgentRestore::of(claude.clone());
    legacy_claude.claude_auth = None;
    let claude_word: AgentRestore = aterm_toml::from_str(&format!(
        "{}{gemini_table}",
        aterm_toml::to_string(&legacy_claude).unwrap()
    ))
    .expect("read");
    assert!(
        claude_word.retired() && claude_word.agent().is_none(),
        "a retired table beside Claude Code's word is no agent's"
    );
    let claude_leaf: AgentRestore = aterm_toml::from_str(&wire(&claude)).expect("read");
    assert!(
        !claude_leaf.retired() && claude_leaf.agent() == Some(Agent::Claude),
        "NEGATIVE CONTROL: Claude Code's leaf is no retired agent's"
    );
    let in_session = format!("{}word = \"gemini\"\n", wire(&claude));
    let old = aterm_toml::from_str::<AgentRestore0104>(&in_session).expect("0.104.0");
    assert_eq!((agent_0104(&old), resumes_0104(&old)), ("claude", true));
}

#[cfg(unix)]
#[test]
fn directories_made_under_an_open_umask_are_never_group_writable() {
    use std::os::unix::fs::PermissionsExt as _;
    if !crate::test_alone::alone() {
        return;
    }
    // SAFETY: `umask(2)` sets this isolated test child's file-creation mask and cannot fail.
    unsafe { libc::umask(0o002) };
    let scratch = aterm_tempfile::tempdir().expect("scratch dir");
    let root = scratch.path();
    let mode = |rel: &str| {
        let meta = fs::symlink_metadata(root.join(rel)).expect("made");
        format!("{:o}", meta.permissions().mode() & 0o7777)
    };
    write_wire_to(&root.join("wire/.local/share/aterm/session.toml"), "").expect("wire");
    store_cell_metrics_to(
        &root.join("metrics/.local/share/aterm/cell-metrics.toml"),
        "font",
        2.0,
        13.0,
        8,
        16,
    )
    .expect("metrics");
    crate::native_document_journal::DraftJournalHost::new(
        root.join("journal/.local/state/aterm/drafts"),
    )
    .expect("journal");
    crate::control_auth::ensure_private_dir(&root.join("control/.local/state/aterm/logs"))
        .expect("private dir");
    let made = [
        ("wire/.local/share/aterm", "755"),
        ("metrics/.local/share/aterm", "755"),
        ("journal/.local/state/aterm/drafts", "700"),
        ("control/.local/state/aterm/logs", "700"),
    ];
    let mut wrong = Vec::new();
    for (leaf, want) in made {
        let mut rel = String::new();
        for name in leaf.split('/') {
            if !rel.is_empty() {
                rel.push('/');
            }
            rel.push_str(name);
            let want = if rel == leaf { want } else { "755" };
            let got = mode(&rel);
            if got != want {
                wrong.push(format!("{rel} {got}, not {want}"));
            }
        }
    }
    assert!(wrong.is_empty(), "made under umask 002: {wrong:#?}");
}

#[test]
fn held_codex_resume_line_at_routes_off_recorded_limited_or_unenrolled_row() {
    use aterm_agent::harness::accounts::{self, Cli, Store};
    use aterm_agent::harness::accounts_pool::Seat;

    const SESSION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
    const NOW: i64 = 1_800_000_000;

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical tempdir");
    let cfg = root.join("cfg");
    let state_dir = root.join("state");
    let home = root.join("home");
    let default_codex = home.join(".codex");
    let work_dir = root.join("work-codex");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::create_dir_all(&default_codex).unwrap();
    std::fs::create_dir_all(&work_dir).unwrap();
    std::fs::write(default_codex.join("auth.json"), "{}\n").unwrap();
    let work_auth = work_dir.join("auth.json");
    std::fs::write(&work_auth, "{}\n").unwrap();
    let roster_path = cfg.join(Cli::Codex.accounts().roster_file);
    std::fs::write(
        &roster_path,
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {:?}\n",
            work_dir.display().to_string()
        ),
    )
    .unwrap();

    let seat = Seat {
        config_dir: &cfg,
        state: &state_dir,
        home: &home,
        now: NOW,
    };
    let work_auth_str = work_auth.display().to_string();
    let exe = aterm_agent::harness::codex_alab::lay_package(&root.join("package"), true);
    let exe = exe.to_str().unwrap();
    let make_restore = |auth_path: Option<&str>| {
        AgentRestore::of(aterm_agent::harness::relaunch::Snapshot {
            launch_env: None,
            auth_observed: true,
            tab: "s-held".into(),
            pid: 4242,
            start: "Wed Oct 28 08:00:00 2026".into(),
            shell: 4141,
            program: exe.into(),
            argv: vec![exe.into()],
            session: Some(SESSION.into()),
            cwd: "/work".into(),
            version: Some("0.1.0".into()),
            run: AgentRun::Codex(aterm_agent::harness::relaunch::CodexRun {
                home: default_codex.clone(),
                home_env: None,
                self_claimed: Some(aterm_agent::harness::self_claimed::Resume {
                    argv: vec![
                        exe.into(),
                        "--no-alt-screen".into(),
                        "resume".into(),
                        SESSION.into(),
                    ],
                    session: SESSION.into(),
                    codex_home: None,
                    codex_auth_path: auth_path.map(str::to_owned),
                    cwd: Some("/work".into()),
                }),
            }),
            dialect: None,
            claude_dir: None,
            login: None,
            home: None,
            route: aterm_agent::harness::accounts::AuthRoute::Outside,
            profile: None,
        })
    };

    let on_work = make_restore(Some(&work_auth_str));
    // Negative control: while `work` still serves, `resume_line_at` keeps `work`'s `CODEX_AUTH_PATH`.
    assert_eq!(
        on_work.resume_line_at(Some(&seat)),
        Some(format!(
            "CODEX_AUTH_PATH={} {} --no-alt-screen resume {SESSION}",
            AgentRestore::resume_word(&work_auth_str).expect("a typed word"),
            AgentRestore::resume_word(exe).expect("a typed word")
        ))
    );

    // Record a wall on `work`: the default row explicitly clears inherited selectors.
    let store = Store::at_for(&state_dir, Cli::Codex, &home);
    store
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |state| {
            Ok(accounts::note_wall(
                state,
                "work",
                "5h",
                Some(NOW + 3_600),
                NOW,
                NOW,
            ))
        })
        .expect("record wall");
    assert_eq!(
        on_work.resume_line_at(Some(&seat)),
        Some(format!(
            " '/usr/bin/env' -u 'CODEX_AUTH_PATH' -u 'CODEX_API_KEY' -u 'CODEX_ACCESS_TOKEN' \
             -u 'CODEX_HOME' '{exe}' '--no-alt-screen' 'resume' '{SESSION}'"
        )),
        "limited `work` routes onto serving default row `main`"
    );

    // Reverse direction: a held tab on `main` whose default `auth.json` is missing routes onto
    // `work` once `work`'s wall has expired.
    std::fs::remove_file(default_codex.join("auth.json")).unwrap();
    let on_main = make_restore(None);
    let later = Seat {
        now: NOW + 7_200,
        ..seat
    };
    assert_eq!(
        on_main.resume_line_at(Some(&later)),
        Some(format!(
            " '/usr/bin/env' -u 'CODEX_API_KEY' -u 'CODEX_ACCESS_TOKEN' -u 'CODEX_HOME' \
             'CODEX_AUTH_PATH={work_auth_str}' '{exe}' \
             '--no-alt-screen' 'resume' '{SESSION}'"
        )),
        "unenrolled default row `main` routes onto serving `work`"
    );
}
