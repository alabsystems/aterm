// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The tools are held by one of Apple's command-line installers (as the caller's exact
/// comparison names them: a stand-in here) beside the placeholder, whatever else runs, else
/// by Apple's own installer app, by absolute path, with the placeholder beside it or not: a
/// placeholder beside another program, even one named like an installer, or beside a process
/// the kernel names no executable for, is held by nothing, and a command-line installer with
/// no placeholder installs something else.
/// NEGATIVE CONTROL: the same processes with the installer's or the window's row taken out
/// hold nothing, placeholder or not.
#[test]
fn the_tools_are_held_only_by_their_own_installers() {
    let cli = |exe: &std::path::Path| exe == std::path::Path::new("/stand-in/installs");
    let apple = "/System/Library/CoreServices/Install Command Line Developer Tools.app/\
                 Contents/MacOS/Install Command Line Developer Tools";
    let held = |placeholder: bool, exes: &[Option<&str>]| {
        tools_held_by(placeholder, exes.iter().copied(), cli)
    };
    let (line, window) = (
        Some(ToolsInstaller::CommandLine),
        Some(ToolsInstaller::AppleWindow),
    );
    assert_eq!(held(true, &[Some("/stand-in/installs")]), line);
    assert_eq!(held(true, &[Some("/bin/zsh"), Some(apple)]), window);
    assert_eq!(
        held(true, &[Some(apple), Some("/stand-in/installs")]),
        line,
        "an install under way outranks the window beside it"
    );
    assert_eq!(held(true, &[None, Some(apple)]), window, "{apple}");
    assert_eq!(held(true, &[None, Some("/stand-in/installs")]), line);
    assert_eq!(
        held(true, &[None, Some("/bin/zsh")]),
        None,
        "an unnamed row"
    );
    assert_eq!(held(true, &[Some("/bin/zsh")]), None, "the control");
    assert_eq!(held(true, &[]), None);
    assert_eq!(
        held(false, &[Some(apple)]),
        window,
        "the window with no placeholder beside it"
    );
    assert_eq!(
        held(false, &[Some("/stand-in/installs"), Some(apple)]),
        window,
        "a command-line installer counts only beside the placeholder"
    );
    assert_eq!(held(false, &[None, Some(apple)]), window);
    assert_eq!(held(false, &[Some("/stand-in/installs")]), None);
    assert_eq!(held(false, &[Some("/bin/zsh"), None]), None, "the control");
    assert_eq!(
        tools_installer(std::path::Path::new("/stand-in/installs"), cli),
        line
    );
    assert_eq!(tools_installer(std::path::Path::new(apple), cli), window);
    for other in [
        "/tmp/installs",
        "stand-in/installs",
        "/System/Library/CoreServices/Software Update.app/Contents/Resources/softwareupdated",
        "Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools",
    ] {
        assert_eq!(held(true, &[Some(other)]), None, "{other}");
        assert_eq!(
            tools_installer(std::path::Path::new(other), cli),
            None,
            "{other}"
        );
    }
}

/// A placeholder is a live run's only when its stamp names a process alive from the start
/// the stamp names: a gone process, a pid another process took since, a start of `0` (which
/// matches nothing, whatever runs under the pid) and any other contents are no run's.
#[test]
fn a_placeholder_is_a_live_runs_only_from_the_start_its_stamp_names() {
    let alive = |pid: u32| (pid == 4242).then_some(1000);
    let stamp = placeholder_stamp(4242, 1000);
    assert_eq!(stamp, "aterm 4242 1000\n");
    assert!(placeholder_held_by_a_live_run(stamp.as_bytes(), alive));
    for other in [
        placeholder_stamp(4242, 999),
        placeholder_stamp(4243, 1000),
        placeholder_stamp(4242, 0),
        "aterm 4242\n".to_owned(),
        "aterm 4242 1000 extra\n".to_owned(),
        "apple 4242 1000\n".to_owned(),
        String::new(),
    ] {
        assert!(
            !placeholder_held_by_a_live_run(other.as_bytes(), alive),
            "{other:?}"
        );
    }
    assert!(!placeholder_held_by_a_live_run(
        placeholder_stamp(4242, 0).as_bytes(),
        |_| Some(0)
    ));
    assert!(stamp.len() <= PLACEHOLDER_STAMP_MAX);
    assert!(placeholder_stamp(u32::MAX, u64::MAX).len() <= PLACEHOLDER_STAMP_MAX);
}

/// A `Subject::Sync` row rides `Setting::Secrets` (its group, setting and wanted all follow
/// it, and it uses the vault source) and is not enumerated by `all()`, so no Sync row is
/// built in shipped code yet. Its six kinds are distinct and have distinct names. The
/// negative control is `all()`'s zero Sync subjects: were the stub enumerated, this would
/// fail.
#[test]
fn a_sync_row_is_a_stub_that_rides_secrets_and_stays_off_the_page() {
    for kind in SyncKind::ALL {
        let row = Subject::Sync(kind);
        assert_eq!(row.group(), Setting::Secrets.group(), "{kind:?}");
        assert_eq!(row.setting(), Setting::Secrets, "{kind:?}");
        assert_eq!(row.wanted(), Setting::Secrets.wanted(), "{kind:?}");
        assert_eq!(row.name(), kind.name(), "{kind:?}");
        assert!(row.uses_vault_source(), "{kind:?}");
    }
    // Not enumerated: no Sync row is built in shipped code.
    assert_eq!(
        Subject::all()
            .into_iter()
            .filter(|subject| matches!(subject, Subject::Sync(_)))
            .count(),
        0
    );
    // The six kinds are distinct, and so are their names.
    for (i, a) in SyncKind::ALL.iter().enumerate() {
        for b in &SyncKind::ALL[i + 1..] {
            assert_ne!(a, b);
            assert_ne!(a.name(), b.name(), "{a:?} vs {b:?}");
        }
    }
}

/// A Restore's outcome holds one entry per row, its worst key speaking for it, in page
/// order with the keys of no row last.
#[test]
fn a_restore_outcome_is_one_entry_per_row_in_page_order_worst_first() {
    let outcome = RestoreOutcome::of([
        (None, PutBack::Failed),
        (Some(Setting::Dock), PutBack::Back),
        (Some(Setting::NaturalScrolling), PutBack::Back),
        (Some(Setting::NaturalScrolling), PutBack::AtLogin),
        (Some(Setting::Dock), PutBack::Waits),
        (Some(Setting::Dock), PutBack::Back),
        (Some(Setting::CapsLock), PutBack::AtLogin),
    ]);
    assert_eq!(
        outcome.rows,
        [
            (Some(Setting::CapsLock), PutBack::AtLogin),
            (Some(Setting::NaturalScrolling), PutBack::AtLogin),
            (Some(Setting::Dock), PutBack::Waits),
            (None, PutBack::Failed),
        ]
    );
    assert!(!outcome.journal_refused);
    assert_eq!(until_login("Control"), "Control · until login");
    assert_eq!(live_until_login(&until_login("Control")), Some("Control"));
    assert_eq!(live_until_login("Control"), None);
}

/// A pass's outcome names each row once per way its change did not finish, in page order,
/// and a later part of the pass adds to it: its rollback joins the earlier one row by row.
#[test]
fn a_pass_outcome_names_each_row_once_and_takes_in_its_later_parts() {
    let mut outcome = PassOutcome::default();
    outcome.unfinished(Setting::Dock, Unfinished::Failed);
    outcome.unfinished(Setting::CapsLock, Unfinished::Unconfirmed);
    outcome.unfinished(Setting::Dock, Unfinished::Failed);
    outcome.unfinished(Setting::CapsLock, Unfinished::Failed);
    assert_eq!(
        outcome.unfinished,
        [
            (Setting::CapsLock, Unfinished::Unconfirmed),
            (Setting::CapsLock, Unfinished::Failed),
            (Setting::Dock, Unfinished::Failed),
        ]
    );
    outcome.rolled_back = Some(RestoreOutcome::of([(Some(Setting::Dock), PutBack::Back)]));
    let mut later = RestoreOutcome::of([(Some(Setting::Dock), PutBack::AtLogin)]);
    later.journal_refused = true;
    outcome.merge(PassOutcome {
        rolled_back: Some(later),
        unfinished: vec![(Setting::Widgets, Unfinished::Failed)],
    });
    let rolled_back = outcome.rolled_back.unwrap();
    assert_eq!(rolled_back.rows, [(Some(Setting::Dock), PutBack::AtLogin)]);
    assert!(rolled_back.journal_refused);
    assert_eq!(outcome.unfinished.len(), 4);
}

#[test]
fn every_row_defaults_on_and_each_checkbox_is_independent() {
    assert_eq!(Selection::default(), Selection::all());
    for row in Setting::ALL {
        let mut selected = Selection::all();
        selected.set(row, false);
        for other in Setting::ALL {
            assert_eq!(selected.contains(other), other != row);
        }
        selected.set(row, true);
        assert_eq!(selected, Selection::all());
    }
}

#[test]
fn plans_round_trip_and_unknown_groups_are_refused() {
    for bits in 0..=Selection::all().bits() {
        let selected = Selection::from_bits(bits).unwrap();
        assert_eq!(selected.bits(), bits);
        assert_eq!(selected.is_empty(), bits == 0);
    }
    assert_eq!(Selection::from_bits(1 << Setting::ALL.len()), None);
    assert_eq!(Selection::from_bits(u16::MAX), None);
}

#[test]
fn every_setting_sits_in_exactly_the_group_that_lists_it() {
    for setting in Setting::ALL {
        let owners: Vec<_> = Group::ALL
            .into_iter()
            .filter(|group| group.settings().contains(&setting))
            .collect();
        assert_eq!(owners, vec![setting.group()], "{setting:?}");
    }
    let subjects = Subject::all();
    assert_eq!(subjects.len(), Setting::ALL.len() - 1 + Access::ALL.len());
    for subject in &subjects {
        assert_eq!(subject.group(), subject.setting().group());
        assert!(!subject.name().is_empty() && !subject.wanted().is_empty());
    }
    assert!(
        subjects
            .iter()
            .all(|subject| *subject != Subject::Setting(Setting::Permissions))
    );
}

#[test]
fn accessibility_is_needed_only_while_a_sign_out_is_managed() {
    let mut selection = Selection::none();
    assert!(!Access::Accessibility.needed_for(selection));
    assert!(Access::FullDiskAccess.needed_for(selection));
    selection.set(Setting::FaceTime, true);
    assert!(Access::Accessibility.needed_for(selection));
}

#[test]
fn only_settled_rows_count_and_only_people_and_failures_draw_attention() {
    for standing in [
        Standing::InPlace,
        Standing::Differs,
        Standing::NeedsYou,
        Standing::Waiting,
        Standing::AtLogin,
        Standing::Failed,
        Standing::Unknown,
        Standing::NotNeeded,
    ] {
        assert_eq!(
            standing.settled(),
            matches!(
                standing,
                Standing::InPlace | Standing::AtLogin | Standing::NotNeeded
            )
        );
        assert_eq!(
            standing.attention(),
            matches!(standing, Standing::NeedsYou | Standing::Failed)
        );
    }
    let unread = Observation::unknown(Subject::Access(Access::Debugger));
    assert_eq!(unread.now, None);
    assert_eq!(unread.wanted, "On");
}

/// A read the observer ran carries when it started; a withheld one stays unread whatever
/// stamps it, so no age and no failure is ever said of a row nothing read (design §3.5).
#[test]
fn a_stamp_dates_a_read_and_never_a_withheld_one() {
    let dock = Subject::Setting(Setting::Dock);
    let failed = Observation {
        detail: Some("the read failed".into()),
        ..Observation::unknown(dock)
    };
    assert_eq!(failed.read_at(), None);
    let failed = failed.stamped(1_700);
    assert_eq!(
        (failed.read, failed.read_at()),
        (Read::At(1_700), Some(1_700))
    );
    assert!(!failed.is_withheld());
    assert_eq!(failed.stamped(1_800).read_at(), Some(1_800));
    let withheld = Observation::withheld(dock);
    assert!(withheld.is_withheld());
    assert_eq!(withheld.standing, Standing::Unknown);
    assert_eq!(
        (withheld.now, withheld.note, withheld.detail),
        (None, None, None)
    );
    let withheld = Observation::withheld(dock).stamped(1_700);
    assert_eq!((withheld.read, withheld.read_at()), (Read::Withheld, None));
}

/// A reading stands for its row for [`READING_STANDS_SECS`] after its read started and no
/// longer; one with no stamp, a withheld one, or one dated after the clock never does, and
/// only a dated one is owed a read again (design §3.5).
#[test]
fn a_reading_stands_for_its_row_only_while_it_is_young() {
    let dock = Subject::Setting(Setting::Dock);
    let at = 1_700_000_000;
    let read = Observation::unknown(dock).stamped(at);
    assert!(read.current_at(at));
    assert!(read.current_at(at + READING_STANDS_SECS));
    assert!(!read.stale_at(at + READING_STANDS_SECS));
    assert!(!read.current_at(at + READING_STANDS_SECS + 1));
    assert!(read.stale_at(at + READING_STANDS_SECS + 1));
    assert!(
        !read.current_at(at - 1),
        "dated after the clock: age unknown"
    );
    assert!(read.stale_at(at - 1));
    let clockless = Observation::unknown(dock).stamped(0);
    assert!(
        !clockless.current_at(0) && clockless.stale_at(0),
        "read with no clock"
    );
    for undated in [Observation::unknown(dock), Observation::withheld(dock)] {
        assert!(
            !undated.current_at(at) && !undated.stale_at(at),
            "{undated:?}"
        );
    }
    assert!(dock.reads_privately());
    assert!(Subject::Setting(Setting::Spotlight).reads_privately());
    assert!(!Subject::Setting(Setting::Repositories).reads_privately());
    assert!(!Subject::Access(Access::FullDiskAccess).reads_privately());
    for setting in Setting::ALL {
        assert_eq!(
            Selection::all().private().contains(setting),
            Subject::Setting(setting).reads_privately(),
            "{setting:?}"
        );
    }
}

/// A grant on offer (nobody asked for it, or developer mode left to the person) is neither a
/// row read settled nor a person's step; a row read settled is in place, not needed, set for
/// the next login or changed since aterm set it, and only with a clock's date. NEGATIVE
/// CONTROLS: a grant needing a person, a row that differs with no change since aterm set it
/// on record, an undated or clockless reading.
#[test]
fn a_grant_on_offer_is_no_reading_as_wanted_and_a_dated_settled_row_is() {
    let at = 1_700_000_000;
    let reading = |subject, standing, note: Option<&str>| Observation {
        standing,
        note: note.map(str::to_owned),
        ..Observation::unknown(subject).stamped(at)
    };
    let fda = Subject::Access(Access::FullDiskAccess);
    let debugger = Subject::Access(Access::Debugger);
    let dock = Subject::Setting(Setting::Dock);
    for offered in [
        reading(fda, Standing::NotNeeded, Some(ACCESS_OFFERED)),
        reading(debugger, Standing::NotNeeded, Some(DEBUGGER_NOT_ASKED)),
    ] {
        assert!(offered.on_offer(), "{offered:?}");
        assert!(!offered.read_back_settled(), "{offered:?}");
    }
    for not_offered in [
        reading(debugger, Standing::NeedsYou, Some(DEBUGGER_NOT_ASKED)),
        reading(fda, Standing::NotNeeded, None),
        reading(dock, Standing::NotNeeded, Some(ACCESS_OFFERED)),
    ] {
        assert!(!not_offered.on_offer(), "{not_offered:?}");
    }
    for wanted in [
        reading(dock, Standing::InPlace, None),
        reading(dock, Standing::NotNeeded, None),
        reading(dock, Standing::AtLogin, None),
        reading(dock, Standing::Differs, Some(CHANGED_SINCE)),
    ] {
        assert!(wanted.read_back_settled(), "{wanted:?}");
        assert!(
            !Observation {
                read: Read::At(0),
                ..wanted.clone()
            }
            .read_back_settled(),
            "clockless: {wanted:?}"
        );
        assert!(
            !Observation {
                read: Read::Unstamped,
                ..wanted.clone()
            }
            .read_back_settled(),
            "undated: {wanted:?}"
        );
    }
    assert!(reading(dock, Standing::Differs, Some(CHANGED_SINCE)).changed_since());
    for short in [
        reading(dock, Standing::Differs, None),
        reading(dock, Standing::Unknown, None),
        reading(dock, Standing::Waiting, None),
    ] {
        assert!(!short.read_back_settled(), "{short:?}");
        assert!(!short.changed_since(), "{short:?}");
    }
}

/// Each row's and each standing's word names it back, and no two share one.
#[test]
fn setup_words_and_standing_words_name_their_row_back() {
    let subjects: Vec<Subject> = Subject::all()
        .into_iter()
        .chain(SyncKind::ALL.into_iter().map(Subject::Sync))
        .collect();
    for subject in &subjects {
        assert_eq!(
            Subject::from_setup_word(subject.setup_word()),
            Some(*subject)
        );
    }
    assert_eq!(Subject::from_setup_word("dock,fda"), None);
    assert_eq!(Subject::from_setup_word(""), None);
    for standing in Standing::ALL {
        assert_eq!(Standing::from_word(standing.word()), Some(standing));
    }
    assert_eq!(Standing::from_word("in place"), None);
}

/// Every failed step's status names its kind, a status that names none is a retry's, and
/// no status is ever a person's: a reading says that (design §3.6).
#[test]
fn a_step_status_names_its_failure_kind_and_never_a_persons() {
    assert_eq!(
        FailureKind::of_status("error"),
        Some(FailureKind::Transient)
    );
    assert_eq!(
        FailureKind::of_status("failed"),
        Some(FailureKind::Transient)
    );
    for calm in ["ok", "applied", "waiting", "blocked", "unverified", "noted"] {
        assert_eq!(FailureKind::of_status(calm), None, "{calm}");
    }
    assert!(FailureKind::Transient.retried());
    assert!(!FailureKind::NeedsPerson.retried());
}

#[test]
fn the_original_workstation_bits_remain_separate_from_new_setup_stages() {
    let original = Selection::all().workstation();
    assert_eq!(original.bits(), 0xff);
    for setting in Setting::ALL {
        assert_eq!(original.contains(setting), setting.is_workstation());
    }
    assert_eq!(Plan::default().selection, Selection::all());
    assert_eq!(Plan::from(Selection::none()).selection, Selection::none());
}

#[test]
fn the_access_record_keeps_each_revision_apart_and_skips_foreign_lines() {
    let dir = crate::test_scratch_dir("access");
    let root = dir.path();
    assert!(access_record::read(root, 7).is_empty());
    access_record::append(root, 7, Access::FullDiskAccess, AccessOutcome::Handed).unwrap();
    access_record::append(root, 8, Access::Debugger, AccessOutcome::Declined).unwrap();
    access_record::append(root, 7, Access::FullDiskAccess, AccessOutcome::Granted).unwrap();
    std::fs::write(
        root.join(access_record::FILE),
        std::fs::read_to_string(root.join(access_record::FILE)).unwrap()
            + "7 telepathy handed\nnot a line\n7 debugger handed extra\n",
    )
    .unwrap();
    assert_eq!(
        access_record::read(root, 7),
        vec![
            (Access::FullDiskAccess, AccessOutcome::Handed),
            (Access::FullDiskAccess, AccessOutcome::Granted),
        ]
    );
    assert_eq!(
        access_record::latest(root, 7, Access::FullDiskAccess),
        Some(AccessOutcome::Granted)
    );
    assert_eq!(access_record::latest(root, 7, Access::Debugger), None);
    assert_eq!(
        access_record::latest(root, 8, Access::Debugger),
        Some(AccessOutcome::Declined)
    );
    let lines = || {
        std::fs::read_to_string(root.join(access_record::FILE))
            .unwrap()
            .lines()
            .count()
    };
    for n in 0..100 {
        let outcome = [AccessOutcome::Handed, AccessOutcome::Granted][n % 2];
        access_record::append(root, 9, Access::Accessibility, outcome).unwrap();
        assert!(lines() <= 64, "{n}");
    }
    assert_eq!(lines(), 64, "the file is kept to its last lines");
}

/// A step judged again in each new process (a Not now that still holds) repeats its row's
/// latest outcome: the record keeps it once, so it never pushes the revision's earlier steps,
/// Accessibility's first, out of the kept lines. A changed outcome is written, a repeat of an
/// earlier one included, and so is the same outcome in another revision.
#[test]
fn the_access_record_does_not_repeat_a_rows_latest_outcome() {
    let dir = crate::test_scratch_dir("access");
    let root = dir.path();
    let lines = || {
        std::fs::read_to_string(root.join(access_record::FILE))
            .unwrap()
            .lines()
            .count()
    };
    access_record::append(root, 9, Access::Accessibility, AccessOutcome::Handed).unwrap();
    for _ in 0..100 {
        access_record::append(root, 9, Access::FullDiskAccess, AccessOutcome::Declined).unwrap();
    }
    assert_eq!(lines(), 2);
    assert_eq!(
        access_record::latest(root, 9, Access::Accessibility),
        Some(AccessOutcome::Handed)
    );
    access_record::append(root, 9, Access::FullDiskAccess, AccessOutcome::Handed).unwrap();
    access_record::append(root, 9, Access::FullDiskAccess, AccessOutcome::Declined).unwrap();
    access_record::append(root, 10, Access::FullDiskAccess, AccessOutcome::Declined).unwrap();
    assert_eq!(lines(), 5);
    assert_eq!(
        access_record::read(root, 9),
        vec![
            (Access::Accessibility, AccessOutcome::Handed),
            (Access::FullDiskAccess, AccessOutcome::Declined),
            (Access::FullDiskAccess, AccessOutcome::Handed),
            (Access::FullDiskAccess, AccessOutcome::Declined),
        ]
    );
}

/// The watcher reads the line back from disk; a failed write tells it nothing.
#[test]
fn a_written_access_step_tells_its_watcher_after_the_line_is_on_disk() {
    use std::sync::Mutex;
    /// A line the watcher was told of, and what the record held for it then.
    type Seen = (u64, Access, AccessOutcome, Option<AccessOutcome>);
    static SEEN: Mutex<Vec<Seen>> = Mutex::new(Vec::new());
    static DIR: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
    fn appended(revision: u64, access: Access, outcome: AccessOutcome) {
        // Only this test's lines: other tests append to their own directories.
        if !(4_201..=4_202).contains(&revision) {
            return;
        }
        let dir = DIR.lock().unwrap().clone().unwrap();
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(".system-state-access.lock"))
            .unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the watcher must run after the publication has released its file lock"
        )]
        probe.try_lock().unwrap();
        probe.unlock().unwrap();
        let on_disk = access_record::latest(&dir, revision, access);
        SEEN.lock()
            .unwrap()
            .push((revision, access, outcome, on_disk));
        access_record::append(&dir, 4_203, Access::Debugger, AccessOutcome::Granted).unwrap();
    }
    access_record::on_append(appended);
    let dir = crate::test_scratch_dir("access");
    *DIR.lock().unwrap() = Some(dir.path().to_path_buf());
    access_record::append(
        dir.path(),
        4_201,
        Access::FullDiskAccess,
        AccessOutcome::Granted,
    )
    .unwrap();
    let unwritable = dir.path().join(access_record::FILE).join("no");
    assert!(
        access_record::append(&unwritable, 4_202, Access::Debugger, AccessOutcome::Granted)
            .is_err()
    );
    assert_eq!(
        *SEEN.lock().unwrap(),
        [(
            4_201,
            Access::FullDiskAccess,
            AccessOutcome::Granted,
            Some(AccessOutcome::Granted)
        )]
    );
    assert_eq!(
        access_record::latest(dir.path(), 4_203, Access::Debugger),
        Some(AccessOutcome::Granted),
        "the watcher can append to the record it just observed"
    );
}

#[test]
fn a_macos_dialog_counts_as_up_only_while_its_guard_lives() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static READY: AtomicUsize = AtomicUsize::new(0);
    fn ready() {
        READY.fetch_add(1, Ordering::SeqCst);
    }
    on_macos_dialog_waiter_ready(ready);
    assert!(!macos_dialog_up());
    let first = MacosDialog::up();
    let second = MacosDialog::up();
    drop(first);
    assert!(macos_dialog_up());
    assert!(MacosDialog::when_none_up(std::time::Duration::ZERO).is_none());
    drop(second);
    assert!(!macos_dialog_up());
    let waited = MacosDialog::when_none_up(std::time::Duration::ZERO);
    assert!(waited.is_some() && macos_dialog_up());
    // Taken from none up in one step: a second caller sees it up.
    assert!(MacosDialog::when_none_up(std::time::Duration::ZERO).is_none());
    drop(waited);
    assert!(!macos_dialog_up());
    let max_waited = MacosDialog::when_none_up(std::time::Duration::MAX);
    assert!(max_waited.is_some() && macos_dialog_up());
    drop(max_waited);
    assert!(!macos_dialog_up());
    assert_eq!(READY.load(Ordering::SeqCst), 0);

    let first = MacosDialog::up();
    let last = MacosDialog::up();
    assert!(MacosDialog::when_none_up(std::time::Duration::from_nanos(1)).is_none());
    drop(first);
    assert_eq!(READY.load(Ordering::SeqCst), 0);
    drop(last);
    assert_eq!(READY.load(Ordering::SeqCst), 1);
    drop(MacosDialog::up());
    assert_eq!(READY.load(Ordering::SeqCst), 1, "the waiter was consumed");

    // The last dialog can end after the timed-out caller saw contention
    // but before it arms the wake. Its post-arm check still notifies once.
    WORKER_WAKE.arm();
    assert_eq!(READY.load(Ordering::SeqCst), 2);
    WORKER_WAKE.notify();
    assert_eq!(READY.load(Ordering::SeqCst), 2);

    // The window's wake is its own: a window gesture that probed the slot without waiting
    // and found another dialog up (the Mac Setup tab behind the Commons runtime's Local
    // Network question) is woken by the last one's end, and the worker, which did not wait,
    // is not. NEGATIVE CONTROL: a drop that wakes only the worker leaves `WINDOW` at 0 here,
    // which is how E3's first launch lost its one Mac password (2026-11-05).
    static WINDOW: AtomicUsize = AtomicUsize::new(0);
    fn window() {
        WINDOW.fetch_add(1, Ordering::SeqCst);
    }
    on_no_macos_dialog_for_window(window);
    let commons = MacosDialog::up();
    wake_window_when_no_macos_dialog();
    assert_eq!(WINDOW.load(Ordering::SeqCst), 0, "not while it is up");
    drop(commons);
    assert_eq!(WINDOW.load(Ordering::SeqCst), 1, "its end wakes the window");
    assert_eq!(READY.load(Ordering::SeqCst), 2, "and not the worker");
    drop(MacosDialog::up());
    assert_eq!(WINDOW.load(Ordering::SeqCst), 1, "the wake was spent");
    wake_window_when_no_macos_dialog();
    assert_eq!(
        WINDOW.load(Ordering::SeqCst),
        2,
        "armed with none up, at once"
    );
    // A worker's timed-out wait wakes the worker alone.
    let up = MacosDialog::up();
    assert!(MacosDialog::when_none_up(std::time::Duration::from_nanos(1)).is_none());
    drop(up);
    assert_eq!(READY.load(Ordering::SeqCst), 3);
    assert_eq!(WINDOW.load(Ordering::SeqCst), 2);
    assert!(!macos_dialog_up());
    // A look that does not wait (the unattended sign-outs) takes the slot only where none is
    // up, never holding its thread, and is woken once the one up ends, as a timed-out wait
    // is. NEGATIVE CONTROL: with none up it takes the slot at once and arms nothing.
    let taken = MacosDialog::if_none_up();
    assert!(taken.is_some() && macos_dialog_up());
    assert!(MacosDialog::if_none_up().is_none(), "one step from none up");
    drop(taken);
    assert_eq!(
        READY.load(Ordering::SeqCst),
        4,
        "the look's own end wakes it"
    );
    drop(MacosDialog::if_none_up());
    assert_eq!(READY.load(Ordering::SeqCst), 4, "none up: nothing armed");
    assert!(!macos_dialog_up());
}

#[test]
fn system_state_coming_to_the_front_calls_its_watcher_once_per_arrival() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SHOWN: AtomicUsize = AtomicUsize::new(0);
    fn shown() {
        SHOWN.fetch_add(1, Ordering::SeqCst);
    }
    on_system_state_front(shown);
    assert!(!system_state_front());
    set_system_state_front(true);
    set_system_state_front(true);
    assert!(system_state_front());
    assert_eq!(SHOWN.load(Ordering::SeqCst), 1);
    set_system_state_front(false);
    assert!(!system_state_front());
    set_system_state_front(true);
    assert_eq!(SHOWN.load(Ordering::SeqCst), 2);
    set_system_state_front(false);
}

#[test]
fn an_opt_out_is_kept_per_grant_until_the_window_clears_it() {
    use opt_out::OptOut;
    let off = OptOut::WhileOff("You turned it off.");
    assert_eq!(opt_out::get(Access::Debugger), None);
    opt_out::set(Access::Debugger, Some(off));
    assert_eq!(opt_out::get(Access::Debugger), Some(off));
    for other in Access::ALL {
        if other != Access::Debugger {
            assert_eq!(opt_out::get(other), None, "{other:?}");
        }
    }
    opt_out::set(Access::Debugger, None);
    assert_eq!(opt_out::get(Access::Debugger), None);
}

#[test]
fn a_turned_off_opt_out_holds_only_of_an_off_reading() {
    use opt_out::OptOut;
    assert_eq!(OptOut::WhileOff("off").holds(true), Some("off"));
    assert_eq!(OptOut::WhileOff("off").holds(false), None);
    for reads_off in [true, false] {
        assert_eq!(OptOut::Always("switch").holds(reads_off), Some("switch"));
    }
}
