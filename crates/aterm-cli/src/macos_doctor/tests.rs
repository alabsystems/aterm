// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_machines::port::Ran;

mod automatic_withheld_tests;
mod init_stage_outcomes_tests;
mod kept_by_person_tests;
mod restore_activation_tests;
mod withheld_revision_tests;

/// Fake doctor host backed by in-memory [`wanted`] keys.
#[derive(Default)]
struct Fake {
    selected: Option<Selection>,
    /// This Mac is not ALab's ([`Host::alab`]); the default is an ALab Mac's scope.
    outside_alab: bool,
    preference_passes: usize,
    values: BTreeMap<Target, PValue>,
    writes: Vec<Target>,
    /// The first item applied reports `error` after its write, once.
    fail_once: bool,
    /// Items whose read-back never verifies.
    failing: Vec<Target>,
    /// Items whose consumer writes the value before the change back over it.
    written_back: Vec<Target>,
    /// Items that stand blocked (a grant not given, a read that does not answer).
    blocked: Vec<Target>,
    accounts_blocked: bool,
    /// The account sign-out's apply runs and its read-back never verifies.
    accounts_fail: bool,
    /// A second sign-out row, FaceTime's, stands blocked beside the first.
    facetime_blocked: bool,
    /// The account sign-out stands aside on what passes by itself (a locked screen), as
    /// `aterm_machines::init_accounts::looks_again` reads it: no try runs.
    accounts_wait: bool,
    /// [`Host::waits`].
    waits: Vec<String>,
    /// [`Host::looked`].
    looked: Vec<String>,
    fail_restart_once: bool,
    /// The journal refuses every line of the doctor's own (`doctor.…`).
    journal_refuses: bool,
    /// An apply pass of the preference group panics, as a process that dies there.
    dies_in_preferences: bool,
    /// An apply pass of the preference group panics once its items wrote.
    dies_after_item_writes: bool,
    /// The doctor's own writes (a rollback, a Restore) panic.
    dies_in_doctor_writes: bool,
    restarts: Vec<String>,
    account_passes: usize,
    attempts: Vec<Attempt>,
    settled: Vec<String>,
    held: Vec<(String, Hold)>,
    /// [`Host::drifted`], [`Host::readings`] and [`Host::changed`] since the last take.
    drifted: Vec<String>,
    readings: Vec<(String, String)>,
    changed: Vec<(String, String)>,
    /// Every journal line, write and relaunch, in order.
    events: Vec<String>,
    /// Each journal line's id, from, to and revert.
    lines: Vec<(String, String, String, Option<String>)>,
    runtime_changed: bool,
    /// What the pass measured of its Full Disk Access.
    full_disk_access: Option<bool>,
    /// Whether the widgets row also clears Notification Center's widget list.
    notification_center: bool,
    /// The Window Manager's widget key belongs to the widgets item too, so one item leaves
    /// one key for want of the grant and reaches another; the doctor counts its tries
    /// whichever pass makes them.
    bundles: bool,
    withheld: Vec<String>,
    /// The doctor's own writes of these keys (a rollback, a Restore) fail.
    refuses: Vec<Target>,
    /// Every key whose live value `restore_runtime` was handed, in order.
    runtime_given: Vec<Target>,
}

impl Fake {
    fn item(&self, target: &Target) -> String {
        if contained(target) || (self.bundles && target.domain == "com.apple.WindowManager") {
            WIDGETS.into()
        } else {
            target.describe()
        }
    }

    fn items(&self) -> Vec<(String, Vec<(Target, PValue)>)> {
        let mut items: Vec<(String, Vec<(Target, PValue)>)> = Vec::new();
        for (target, value) in self.wants() {
            let item = self.item(&target);
            match items.iter_mut().find(|(id, _)| *id == item) {
                Some((_, keys)) => keys.push((target, value)),
                None => items.push((item, vec![(target, value)])),
            }
        }
        items
    }

    fn setting(target: &Target) -> Setting {
        if target.domain == "com.apple.dock" {
            Setting::Dock
        } else {
            Setting::Widgets
        }
    }

    fn relaunches(&self, process: &str) -> usize {
        let event = format!("relaunch {process}");
        self.events.iter().filter(|e| **e == event).count()
    }

    fn wants(&self) -> Vec<(Target, PValue)> {
        let mut keys = wanted();
        if self.notification_center {
            keys.push((nc_widgets(), cleared()));
        }
        keys
    }

    fn reaches(&self, target: &Target) -> bool {
        !contained(target) || self.full_disk_access == Some(true)
    }

    fn changes(&self) -> usize {
        self.events
            .iter()
            .filter(|event| event.starts_with("write ") || event.starts_with("restart "))
            .count()
    }

    fn put(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String> {
        self.writes.push(target.clone());
        self.events.push(format!("write {}", target.describe()));
        match value {
            Some(value) => {
                self.values.insert(target.clone(), value.clone());
            }
            None => {
                self.values.remove(target);
            }
        }
        Ok(())
    }

    /// How this pass leaves `item`, as the native host's resolves a hold as read back
    /// (`aterm_machines::init::Workstation`): set again unless aterm set it again already, then
    /// as it is. No key of this host's grows targets.
    fn hold_of(&self, item: &str) -> Option<Hold> {
        match self
            .held
            .iter()
            .find(|(held, _)| held == item)
            .map(|(_, hold)| hold.clone())
        {
            Some(Hold::ReadBack {
                again: true, why, ..
            }) => Some(Hold::AsIs(why)),
            Some(Hold::ReadBack { .. }) => None,
            hold => hold,
        }
    }
}

pub(super) fn restore_without_effects(root: &Path) -> Result<Report, String> {
    let mut host = Fake {
        runtime_changed: true,
        ..Fake::default()
    };
    let result = run_mutation(&mut host, root, Action::Restore);
    assert!(host.writes.is_empty());
    assert!(host.restarts.is_empty());
    assert!(host.runtime_changed, "Restore reached runtime activation");
    result
}

/// Test harness where the Dock change never verifies.
pub(super) struct NeverVerifies {
    host: Fake,
    now: std::time::SystemTime,
}

impl NeverVerifies {
    pub(super) fn dock() -> Self {
        Self {
            host: Fake {
                failing: vec![wanted()[0].0.clone()],
                ..Fake::default()
            },
            now: std::time::SystemTime::now(),
        }
    }

    pub(super) fn accounts() -> Self {
        Self {
            host: Fake {
                accounts_fail: true,
                ..Fake::default()
            },
            now: std::time::SystemTime::now(),
        }
    }

    /// What `apply_on_first_run_at` runs once the stage is due.
    pub(super) fn pass(&mut self, root: &Path) -> Result<Option<Report>, String> {
        self.now += std::time::Duration::from_secs(61);
        apply_on_first_run_with(
            root,
            &Inert::default(),
            &mut self.host,
            "1.0.0+gtest",
            self.now,
        )
    }

    pub(super) fn pass_at(
        &mut self,
        root: &Path,
        selected: Selection,
        seconds: u64,
    ) -> Result<Report, String> {
        self.now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        self.host.select(selected);
        automatic_pass(&mut self.host, root, &aterm_stamp::build_id(), self.now)
    }

    /// Preference writes and relaunches so far.
    pub(super) fn changes(&self) -> usize {
        self.host.changes()
    }

    /// Relaunches so far.
    pub(super) fn relaunches(&self) -> usize {
        self.host.restarts.len()
    }
}

/// Test harness for Notification Center widget withholding and follow-up.
pub(super) struct Withholding {
    host: Fake,
    now: std::time::SystemTime,
    build: &'static str,
}

impl Withholding {
    pub(super) fn widgets() -> Self {
        Self {
            host: Fake {
                notification_center: true,
                values: BTreeMap::from([(nc_widgets(), populated())]),
                ..Fake::default()
            },
            now: std::time::SystemTime::now(),
            build: "1.0.0+gtest",
        }
    }

    /// [`Withholding::widgets`] with the Dock selected too, its change never verifying (the
    /// Dock wrote its own tiles back over it), so the Workstation stage stays pending.
    pub(super) fn widgets_and_a_dock_that_never_verifies() -> Self {
        let mut doctor = Self::widgets();
        doctor.host.failing = vec![wanted()[0].0.clone()];
        doctor
    }

    /// [`Withholding::widgets`] whose first change does not verify.
    pub(super) fn widgets_whose_first_try_does_not_verify() -> Self {
        let mut doctor = Self::widgets();
        doctor.host.bundles = true;
        doctor.host.fail_once = true;
        doctor
    }

    pub(super) fn pass(&mut self, root: &Path) -> Result<Option<Report>, String> {
        self.now += std::time::Duration::from_secs(61);
        apply_on_first_run_with(
            root,
            &Inert::default(),
            &mut self.host,
            self.build,
            self.now,
        )
    }

    pub(super) fn follow_up(&mut self, root: &Path) -> Result<Option<Report>, String> {
        follow_up_at(root, &mut self.host, self.build, self.now)
    }

    /// What a follow-up would change now ([`owed_with_full_disk_access`]).
    pub(super) fn owed(&self, root: &Path) -> Vec<Setting> {
        owed_with_full_disk_access(root, || self.host.full_disk_access)
    }

    pub(super) fn restore(&mut self, root: &Path) -> Result<Report, String> {
        execute(&mut self.host, root, Action::Restore)
    }

    pub(super) fn explicit_apply(&mut self, root: &Path) -> Result<Report, String> {
        run_mutation(&mut self.host, root, Action::Apply)
    }

    pub(super) fn with_build(mut self, build: &'static str) -> Self {
        self.build = build;
        self
    }

    pub(super) fn wait(&mut self, by: std::time::Duration) {
        self.now += by;
    }

    /// The time of the last pass, which its follow-up shares.
    pub(super) fn now(&self) -> std::time::SystemTime {
        self.now
    }

    /// macOS gives aterm Full Disk Access.
    pub(super) fn grant(&mut self) {
        self.host.full_disk_access = Some(true);
    }

    /// aterm's Full Disk Access reads off again (an update, say).
    pub(super) fn revoke(&mut self) {
        self.host.full_disk_access = Some(false);
    }

    /// Notification Center never keeps a clear of its widget list (it is not up again in time).
    pub(super) fn notification_center_never_keeps(&mut self) {
        self.host.failing.push(nc_widgets());
    }

    pub(super) fn refuse_notification_center_restore(&mut self) {
        self.host.refuses.push(nc_widgets());
    }

    pub(super) fn fail_next_relaunch(&mut self) {
        self.host.fail_restart_once = true;
    }

    /// The person shows their desktop widgets again.
    pub(super) fn show_desktop_widgets(&mut self) {
        self.host
            .values
            .insert(wanted()[1].0.clone(), PValue::Bool(false));
    }

    /// Whether the desktop widgets show, as the person set them.
    pub(super) fn desktop_widgets_shown(&self) -> bool {
        self.host
            .values
            .get(&wanted()[1].0)
            .is_some_and(|now| now.same(&PValue::Bool(false)))
    }

    /// Writes to the Window Manager's widget key so far.
    pub(super) fn window_manager_writes(&self) -> usize {
        self.host
            .writes
            .iter()
            .filter(|target| **target == wanted()[1].0)
            .count()
    }

    /// Relaunches of the Dock so far.
    pub(super) fn dock_relaunches(&self) -> usize {
        self.host.relaunches("Dock") + self.host.restarts.iter().filter(|p| *p == "Dock").count()
    }

    /// Relaunches of Notification Center so far.
    pub(super) fn notification_center_relaunches(&self) -> usize {
        self.host.relaunches("NotificationCenter")
            + self
                .host
                .restarts
                .iter()
                .filter(|p| *p == "NotificationCenter")
                .count()
    }

    /// What the record of withheld items names.
    pub(super) fn withheld(root: &Path) -> Vec<String> {
        load_withheld(root).unwrap().items
    }

    /// Writes to Notification Center's widget list so far.
    pub(super) fn notification_center_writes(&self) -> usize {
        self.host
            .writes
            .iter()
            .filter(|target| contained(target))
            .count()
    }

    /// Whether Notification Center's widget list holds no widget.
    pub(super) fn notification_center_cleared(&self) -> bool {
        self.host
            .values
            .get(&nc_widgets())
            .is_some_and(|now| now.same(&cleared()))
    }
}

/// A [`System`] that runs nothing ([`NativeHost`]).
#[derive(Default)]
pub(super) struct Inert {
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// What it answers for this process's Full Disk Access.
    full_disk_access: Option<bool>,
    /// What it answers for the commands a test models, and which pids it says are alive.
    model: Option<std::sync::Arc<dyn Model>>,
}

/// The commands and processes a test models over an [`Inert`] system.
pub(super) trait Model: Send + Sync {
    /// Its answer to `cmd`; `None` refuses it, as the inert system does.
    fn answer(&self, cmd: &Cmd) -> Option<Ran>;
    /// Whether `pid` is alive.
    fn alive(&self, pid: u32) -> bool;
    /// A wait of the code under test ([`System::sleep`]), which passes no time: what moves
    /// the model on.
    fn sleep(&self) {}
}

struct InertJournal(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl aterm_converge::Journal for InertJournal {
    fn before_change(
        &mut self,
        id: &str,
        _from: &str,
        _to: &str,
        _revert: Option<&str>,
    ) -> Result<(), String> {
        self.0.lock().unwrap().push(id.into());
        Ok(())
    }
}

mod inert {
    use super::Inert;
    use aterm_machines::port::{Cmd, Ran, RunFailed, Stat, System, User};
    use std::io;
    use std::path::Path;

    fn unsupported<T>() -> io::Result<T> {
        Err(io::ErrorKind::Unsupported.into())
    }

    impl System for Inert {
        fn run(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
            self.calls.lock().unwrap().push(cmd.display());
            if let Some(ran) = self.model.as_ref().and_then(|model| model.answer(cmd)) {
                return Ok(ran);
            }
            Err(RunFailed::Refused(
                "an inert test system runs nothing".into(),
            ))
        }
        fn run_complete_stdout(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
            self.run(cmd)
        }
        fn pid_alive(&self, pid: u32) -> bool {
            self.model.as_ref().is_some_and(|model| model.alive(pid))
        }
        fn sleep(&self, period: std::time::Duration) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("sleep {}ms", period.as_millis()));
            if let Some(model) = &self.model {
                model.sleep();
            }
        }
        fn run_tty(
            &self,
            cmd: &Cmd,
            before: &mut dyn FnMut(&mut Cmd) -> Result<(), RunFailed>,
        ) -> Result<Option<i32>, RunFailed> {
            let mut cmd = cmd.clone();
            before(&mut cmd)?;
            self.run(&cmd).map(|ran| ran.status)
        }
        fn read_file(&self, _: &Path) -> io::Result<Vec<u8>> {
            unsupported()
        }
        fn write_file(&self, _: &Path, _: &[u8], _: u32) -> io::Result<()> {
            unsupported()
        }
        fn write_user_file(&self, _: &User, _: &Path, _: &[u8], _: u32) -> io::Result<()> {
            unsupported()
        }
        fn remove(&self, _: &Path) -> io::Result<()> {
            unsupported()
        }
        fn create_dir_all(&self, _: &Path, _: u32) -> io::Result<()> {
            unsupported()
        }
        fn chown(&self, _: &Path, _: u32, _: u32) -> io::Result<()> {
            unsupported()
        }
        fn stat(&self, _: &Path) -> io::Result<Stat> {
            unsupported()
        }
        fn list_dir(&self, _: &Path) -> io::Result<Vec<String>> {
            unsupported()
        }
        fn sha256(&self, _: &Path) -> io::Result<String> {
            unsupported()
        }
        fn copy_file(&self, _: &Path, _: &Path, _: u32) -> io::Result<()> {
            unsupported()
        }
        fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
            unsupported()
        }
        fn euid(&self) -> u32 {
            501
        }
        fn now(&self) -> u64 {
            0
        }
        fn full_disk_access(&self) -> Option<bool> {
            self.full_disk_access
        }
        fn journal(&self) -> Result<Box<dyn aterm_converge::Journal + Send + '_>, String> {
            Ok(Box::new(super::InertJournal(std::sync::Arc::clone(
                &self.lines,
            ))))
        }
    }
}

const NC: &str = "com.apple.notificationcenterui";

/// The shared widgets item's id.
pub(super) const WIDGETS: &str = "consumer.widgets.notification-center@alice";

fn touches_notification_center(call: &str) -> bool {
    call.contains(NC) || call.contains("NotificationCenter")
}

fn inert_host(
    full_disk_access: Option<bool>,
    items: fn() -> Vec<Box<dyn aterm_machines::items::Item>>,
) -> (NativeHost, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let inert = Inert {
        full_disk_access,
        ..Inert::default()
    };
    let calls = std::sync::Arc::clone(&inert.calls);
    let host = NativeHost::over(
        Box::new(inert),
        Box::new(move |_, group, _| {
            Ok(Workstation::of(if group == WorkstationGroup::Preferences {
                items()
            } else {
                Vec::new()
            }))
        }),
        initial::complete_explicit_workstation_plan_for_test,
    );
    (host, calls)
}

#[test]
fn the_doctors_host_neither_reads_nor_writes_a_container_without_the_grant() {
    let widgets = Target {
        domain: NC.into(),
        key: "widgets".into(),
        host: false,
    };
    for unknown in [None, Some(false)] {
        let (mut host, calls) = inert_host(unknown, Vec::new);
        let read = host.read(&widgets).unwrap_err();
        let write = host.write(&widgets, None).unwrap_err();
        assert!(read.contains("Full Disk Access"), "{read}");
        assert!(write.contains("Full Disk Access"), "{write}");
        assert!(calls.lock().unwrap().is_empty(), "{:?}", calls.lock());
    }
    let (mut host, calls) = inert_host(Some(true), Vec::new);
    assert!(
        host.read(&widgets).is_err(),
        "the inert system runs nothing"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        [format!("{DEFAULTS} export {NC} -")]
    );
}

/// The Caps Lock mapping, and a scroll direction the Mac had never set, load only at the
/// next login once Restore puts them back. That is a note to log out, not a setting
/// Restore failed to put back: each row reads [`LOGIN`] under its System State row's name,
/// never `blocked` (a failure) nor `ok` (which says nothing to do), and `aterm doctor`
/// still prints the advice to log out.
#[test]
fn a_setting_that_loads_at_the_next_login_is_restored_not_blocked() {
    let saved = [
        Saved {
            target: Target {
                domain: "-g".into(),
                key: "com.apple.keyboard.modifiermapping.1452-610-0".into(),
                host: true,
            },
            value: None,
            granted: true,
        },
        Saved {
            target: Target::new("-g", "com.apple.swipescrolldirection"),
            value: None,
            granted: true,
        },
        Saved {
            target: Target::new("com.apple.AppleMultitouchTrackpad", "TrackpadScroll"),
            value: None,
            granted: true,
        },
    ];
    let (mut host, calls) = inert_host(Some(true), Vec::new);
    let runtime = host.restore_runtime(&saved);
    assert_eq!(
        runtime
            .iter()
            .map(|runtime| runtime.put_back)
            .collect::<Vec<_>>(),
        [
            Some((Setting::NaturalScrolling, PutBack::AtLogin)),
            Some((Setting::CapsLock, PutBack::AtLogin)),
            Some((Setting::TrackpadScrolling, PutBack::AtLogin)),
        ]
    );
    let rows: Vec<Row> = runtime.into_iter().map(|runtime| runtime.row).collect();
    let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names,
        [
            Setting::NaturalScrolling.name(),
            Setting::CapsLock.name(),
            Setting::TrackpadScrolling.name()
        ]
    );
    for row in &rows {
        assert_eq!(row.status, "login", "{}: {}", row.name, row.detail);
        assert!(
            row.detail
                .to_ascii_lowercase()
                .contains("log out and back in"),
            "{}",
            row.detail
        );
    }
    let report = Report {
        action: Action::Restore,
        rows,
        manual_steps: Vec::new(),
        backup: PathBuf::from("original-preferences.json"),
        restore: None,
        page: None,
    };
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert!(calls.lock().unwrap().is_empty(), "no runtime value was set");
    let text = report.to_text();
    for row in &report.rows {
        assert!(
            text.contains(&format!("  {}: login — {}\n", row.name, row.detail)),
            "the terminal still says to log out: {text}"
        );
    }
}

/// Only a key Restore put back, and that stayed back, has a live value to put back or loads
/// at the next login. A Restore whose reads and writes of the Caps Lock mapping and of a
/// scroll direction the Mac had never set do not land (the inert system runs nothing) says
/// each of them failed, and nothing of the next login. Its positive control is
/// `a_setting_that_loads_at_the_next_login_is_restored_not_blocked`, where the same two keys
/// reach the login rows.
#[test]
fn a_key_restore_could_not_put_back_has_no_login_row() {
    let saved = [
        Saved {
            target: Target {
                domain: "-g".into(),
                key: "com.apple.keyboard.modifiermapping.1452-610-0".into(),
                host: true,
            },
            value: None,
            granted: true,
        },
        Saved {
            target: Target::new("-g", "com.apple.swipescrolldirection"),
            value: None,
            granted: true,
        },
    ];
    let root = aterm_tempfile::tempdir().expect("scratch");
    save_backup(&root.path().join("original-preferences.json"), &saved).unwrap();
    let (mut host, _) = inert_host(Some(true), Vec::new);
    let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
    assert!(
        report.rows.iter().all(|row| row.status != LOGIN),
        "{}",
        report.to_text()
    );
    let restore = report
        .restore
        .expect("a Restore says what it did to each row");
    assert_eq!(
        restore.outcome.rows,
        [
            (Some(Setting::CapsLock), PutBack::Failed),
            (Some(Setting::NaturalScrolling), PutBack::Failed)
        ]
    );
}

/// A rollback hands the live values only of what it put back and its reader kept: a key whose
/// rollback write fails has no live value put back and no login row. Positive control: the
/// same pass with the write allowed hands it over.
#[test]
fn a_rollback_hands_over_only_what_it_put_back() {
    let dock = wanted()[0].0.clone();
    for refused in [false, true] {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake {
            fail_once: true,
            refuses: if refused {
                vec![dock.clone()]
            } else {
                Vec::new()
            },
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).expect("report");
        assert!(report.needs_attention());
        let expected = if refused {
            Vec::new()
        } else {
            vec![dock.clone()]
        };
        assert_eq!(host.runtime_given, expected, "{}", report.to_text());
    }
}

/// A rollback key whose saved value already reads the original (its reader wrote the change
/// back over) is put back as much as one the rollback wrote, as Restore counts it: its live
/// value is handed to `restore_runtime` too, so a live value the failed step changed is put
/// back. Its pair is `a_rollback_with_nothing_to_put_back_relaunches_nothing`: no write, no
/// relaunch.
#[test]
fn a_rollback_hands_over_a_key_already_back_as_restore_does() {
    let dock = wanted()[0].0.clone();
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake {
        written_back: vec![dock.clone()],
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).expect("report");
    assert!(report.needs_attention(), "{}", report.to_text());
    assert_eq!(host.runtime_given, vec![dock], "{}", report.to_text());
}

/// A pass whose change failed reaches System State by row: the change that did not take,
/// and what its rollback put back, in the page's row names, where `aterm doctor`'s rows name
/// each key (design §3.4, §3.6). The page's own rows name no key.
#[test]
fn a_failed_change_and_its_rollback_reach_the_page_by_row() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake {
        fail_once: true,
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).expect("report");
    assert!(report.needs_attention(), "{}", report.to_text());
    assert!(
        report.to_text().contains("com.apple.dock"),
        "the terminal keeps each key: {}",
        report.to_text()
    );
    let page = report
        .page
        .as_ref()
        .expect("a pass says what it did, by row");
    assert_eq!(
        page.outcome,
        PassOutcome {
            rolled_back: Some(RestoreOutcome {
                rows: vec![(Some(Setting::Dock), PutBack::Back)],
                journal_refused: false,
            }),
            unfinished: vec![(Setting::Dock, Unfinished::Failed)],
        },
        "{}",
        report.to_text()
    );
    for row in &page.rows {
        assert!(
            !row.name.contains("com.apple") && !row.detail.contains("com.apple"),
            "{row:?}"
        );
    }
}

/// A Restore of what an Apply set reports each System State row it put back, never a key
/// (`aterm doctor`'s text keeps those), and leaves those groups at Leave as is in the saved
/// plan (design §3.4).
#[test]
fn a_restore_reports_each_row_it_put_back_and_leaves_their_groups_as_is() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    authorize_automatic_setup_at(root.path(), Selection::all()).unwrap();
    let mut host = Fake::default();
    execute(&mut host, root.path(), Action::Apply).expect("apply");
    let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
    assert!(!report.needs_attention(), "{}", report.to_text());
    let restore = report.restore.as_ref().expect("a Restore says what it did");
    assert_eq!(
        restore.outcome,
        RestoreOutcome {
            rows: vec![
                (Some(Setting::Widgets), PutBack::Back),
                (Some(Setting::Dock), PutBack::Back),
            ],
            journal_refused: false,
        }
    );
    assert!(
        report.to_text().contains("com.apple.dock show-recents"),
        "{}",
        report.to_text()
    );
    let plan =
        initial::initial_setup_snapshot_at(root.path(), aterm_types::macos_setup::Plan::default())
            .unwrap()
            .plan;
    let restored = initial::preferences(Selection::all());
    for setting in Setting::ALL {
        assert_eq!(
            plan.selection.contains(setting),
            !restored.contains(setting),
            "{setting:?}"
        );
    }
}

/// A Restore whose relaunch fails after the preferences were written leaves the rows that
/// process reads to the next login: back as saved, in use from then, never failed, and the
/// page's outcome names no process (`aterm doctor --restore` keeps its `Reload` line).
#[test]
fn a_restore_whose_relaunch_fails_brings_its_rows_back_at_the_next_login() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    authorize_automatic_setup_at(root.path(), Selection::all()).unwrap();
    let mut host = Fake::default();
    execute(&mut host, root.path(), Action::Apply).expect("apply");
    host.fail_restart_once = true;
    let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
    let restore = report.restore.as_ref().expect("a Restore says what it did");
    assert_eq!(
        restore.outcome,
        RestoreOutcome {
            rows: vec![
                (Some(Setting::Widgets), PutBack::AtLogin),
                (Some(Setting::Dock), PutBack::AtLogin),
            ],
            journal_refused: false,
        }
    );
    assert!(
        report.rows.iter().any(|row| row.name == "Reload Dock"),
        "{}",
        report.to_text()
    );
}

/// EVERY PROFILE ROW CARRIES ITS TIME in `--json`: a shared item's, a relaunch's, a
/// restore's and a note's, on Apply, Check, a relaunch that fails and Restore. A row its
/// producer timed keeps that time; a lap gives the rest the time since the last one.
/// Presence only, never a budget.
#[test]
fn every_profile_row_carries_its_time_in_the_json() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    authorize_automatic_setup_at(root.path(), Selection::all()).unwrap();
    let mut host = Fake::default();
    let applied = execute(&mut host, root.path(), Action::Apply).expect("apply");
    save_pending_refresh(root.path(), &[wanted()[0].0.clone()]).expect("owed activation");
    let checked = execute(&mut host, root.path(), Action::Check).expect("check");
    assert!(
        checked
            .rows
            .iter()
            .any(|row| row.name == "Preference activation"),
        "{}",
        checked.to_text()
    );
    host.fail_restart_once = true;
    let restored = execute(&mut host, root.path(), Action::Restore).expect("restore");
    assert!(
        restored.rows.iter().any(|row| row.name == "Reload Dock")
            && restored.rows.iter().any(|row| row.status == "restored"),
        "{}",
        restored.to_text()
    );
    for report in [applied, checked, restored] {
        let json: Value = aterm_json::from_str(&report.to_json()).unwrap();
        let rows = json["settings"].as_array().unwrap();
        assert!(!rows.is_empty(), "{json}");
        assert!(
            rows.iter().all(|row| row["elapsed_ms"].as_u64().is_some()),
            "{json}"
        );
    }

    let row = |elapsed_ms| Row {
        name: "row".into(),
        status: "ok".into(),
        detail: String::new(),
        elapsed_ms,
    };
    let mut rows = vec![row(Some(7)), row(None)];
    RowClock::start().lap(&mut rows);
    assert_eq!(rows[0].elapsed_ms, Some(7), "its producer's time stays");
    assert!(rows[1].elapsed_ms.is_some());
}

#[test]
fn restoring_a_login_time_preference_reads_the_original_value_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut trackpad = Selection::none();
    trackpad.set(Setting::TrackpadScrolling, true);
    let note = root.path().join("at-login.json");
    write_json(
        &note,
        &json!({"profile":PROFILE,"session":"1.0:7","settings":trackpad.bits()}),
    )
    .unwrap();
    let pending = std::fs::read(&note).unwrap();
    let _lock = HeldLock::try_take(root.path()).unwrap().unwrap();
    let target = Target::new("com.apple.AppleMultitouchTrackpad", "TrackpadScroll");
    let mut host = Fake::default();
    host.values.insert(target.clone(), PValue::Bool(true));
    save_backup(
        &root.path().join("original-preferences.json"),
        &[Saved {
            target: target.clone(),
            value: Some(PValue::Bool(false)),
            granted: true,
        }],
    )
    .unwrap();

    let report = run_mutation(&mut host, root.path(), Action::Restore).unwrap();
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert_eq!(host.read(&target).unwrap(), Some(PValue::Bool(false)));
    assert_eq!(host.writes, [target]);
    assert_eq!(std::fs::read(note).unwrap(), pending);
}

/// Restore previous settings meets the profile lock another holder has for one short write
/// (the ALab watch widening a plan, a Stop's fold): it waits for it as a choice does and
/// runs, never refused at once (design §3.4). Past the wait it is refused in the person's
/// words, which System State queues.
#[test]
fn restore_waits_for_a_short_lock_as_a_choice_does() {
    use std::cell::Cell;
    let root = aterm_tempfile::tempdir().unwrap();
    let target = Target::new("com.apple.AppleMultitouchTrackpad", "TrackpadScroll");
    save_backup(
        &root.path().join("original-preferences.json"),
        &[Saved {
            target: target.clone(),
            value: Some(PValue::Bool(false)),
            granted: true,
        }],
    )
    .unwrap();
    let mut holder = Some(HeldLock::take(root.path()).unwrap());
    let mut host = Fake::default();
    host.values.insert(target.clone(), PValue::Bool(true));
    let mut asked = 0;
    let observed = mutation_observed(
        root.path(),
        None,
        Action::Restore,
        || {
            asked += 1;
            // The other holder's write ends.
            holder = None;
            true
        },
        |root| run_mutation(&mut host, root, Action::Restore),
    );
    let report = observed
        .result
        .expect("a Restore pressed during a short write runs");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert_eq!(asked, 1, "the Restore waited once and took the lock again");
    assert_eq!(host.read(&target).unwrap(), Some(PValue::Bool(false)));

    let _holder = HeldLock::take(root.path()).unwrap();
    let refused = mutation_observed(
        root.path(),
        None,
        Action::Restore,
        || false,
        |_| panic!("a refused Restore constructs no mutation host"),
    );
    assert_eq!(refused.result.unwrap_err(), SETUP_BUSY);

    // The actual native-host factory must capture the grant after a queued CLI or page
    // Restore wins its lease. A grant worker can finish its empty look in between.
    for on_page in [false, true] {
        let queued = aterm_tempfile::tempdir().unwrap();
        let mut native = InitialSetupWithheldStopTest::unfinished(queued.path()).unwrap();
        let (cleared, _) = native.follow_up(|| Some(()), |_| false);
        cleared.result.unwrap();
        assert!(!native.owed().unwrap());
        let writes = native.notification_center_writes();
        let grant = Cell::new(false);
        let constructed = Cell::new(None);
        let factory = || {
            assert!(HeldLock::try_take(queued.path()).unwrap().is_none());
            constructed.set(Some(grant.get()));
            native.host(grant.get())
        };
        let mut holder = Some(HeldLock::take(queued.path()).unwrap());
        let wait = || {
            grant.set(true);
            holder.take();
            let WithheldFollowUp::Finished(finished) = follow_up_attempt_at(
                queued.path(),
                || Some(()),
                || Ok(native.host(grant.get())),
                BUILD,
                std::time::SystemTime::now,
                |_| false,
            ) else {
                panic!("the grant worker must win the empty profile lease");
            };
            assert!(finished.observed.result.as_ref().unwrap().is_none());
            true
        };
        let operation = |root: &Path| run_mutation(&mut factory(), root, Action::Restore);
        let observed = if on_page {
            let observed = restore_at(queued.path(), None, wait, || {}, || true, operation);
            InitialSetupObserved::new(
                observed
                    .result
                    .map(|report| report.expect("the queued page Restore runs")),
                observed.observation,
            )
        } else {
            mutation_observed(queued.path(), None, Action::Restore, wait, operation)
        };
        let report = observed.result.unwrap();
        assert!(
            !report.needs_attention(),
            "the queued Restore must use the grant now held: {}",
            report.to_text()
        );
        assert_eq!(constructed.get(), Some(true));
        assert!(!native.owed().unwrap());
        assert_eq!(native.notification_center_writes(), writes + 1);
    }
}

/// System State's Restore says the moment its own attempt finds the profile lock another
/// setup holds, so the page shows it waiting from then, not only once a later look finds
/// the records held; and once it holds the lock, before its first write, it asks whether a
/// group press overtook it while it waited: one that did lets it go having changed nothing
/// (design §3.4), and otherwise it puts the saved settings back.
#[test]
fn restore_says_when_it_waits_and_asks_before_its_first_write() {
    let root = aterm_tempfile::tempdir().unwrap();
    let target = Target::new("com.apple.AppleMultitouchTrackpad", "TrackpadScroll");
    save_backup(
        &root.path().join("original-preferences.json"),
        &[Saved {
            target: target.clone(),
            value: Some(PValue::Bool(false)),
            granted: true,
        }],
    )
    .unwrap();
    let mut host = Fake::default();
    host.values.insert(target.clone(), PValue::Bool(true));
    let holder = HeldLock::take(root.path()).unwrap();
    let mut waited = 0;
    let refused = restore_at(
        root.path(),
        None,
        || false,
        || waited += 1,
        || panic!("it never held the records"),
        |_| panic!("a refused Restore constructs no mutation host"),
    );
    assert_eq!(refused.result.unwrap_err(), SETUP_BUSY);
    assert_eq!(waited, 1, "said once, as it found the lock held");
    drop(holder);
    let overtaken = restore_at(
        root.path(),
        None,
        || false,
        || panic!("the lock was free"),
        || false,
        |_| panic!("an overtaken Restore constructs no mutation host"),
    );
    assert!(overtaken.result.unwrap().is_none(), "let go");
    assert_eq!(
        host.read(&target).unwrap(),
        Some(PValue::Bool(true)),
        "it changed nothing"
    );
    let restored = restore_at(
        root.path(),
        None,
        || false,
        || panic!("the lock was free"),
        || true,
        |root| run_mutation(&mut host, root, Action::Restore),
    );
    assert!(restored.result.unwrap().is_some());
    assert_eq!(host.read(&target).unwrap(), Some(PValue::Bool(false)));
}

#[test]
fn restore_leaves_a_container_before_image_alone_without_the_grant() {
    let saved = [
        Saved {
            target: Target {
                domain: NC.into(),
                key: "widgets".into(),
                host: false,
            },
            value: Some(PValue::Dict(vec![(
                "instances".into(),
                PValue::Array(vec![PValue::Data(vec![42])]),
            )])),
            granted: true,
        },
        Saved {
            target: Target::new("com.apple.WindowManager", "StandardHideWidgets"),
            value: Some(PValue::Bool(false)),
            granted: true,
        },
    ];
    for full_disk_access in [None, Some(false), Some(true)] {
        let root = aterm_tempfile::tempdir().expect("scratch");
        save_backup(&root.path().join("original-preferences.json"), &saved).unwrap();
        let (mut host, calls) = inert_host(full_disk_access, Vec::new);
        let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
        let calls = calls.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("export com.apple.WindowManager")),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|call| call.ends_with(" Dock")),
            "{calls:?}"
        );
        let row = report
            .rows
            .iter()
            .find(|row| row.name == format!("{NC} widgets"))
            .unwrap();
        if full_disk_access == Some(true) {
            assert_eq!(row.status, "error", "the inert system runs nothing");
            assert!(
                calls.contains(&format!("{DEFAULTS} export {NC} -")),
                "{calls:?}"
            );
            assert!(
                calls
                    .iter()
                    .any(|call| call.ends_with(" NotificationCenter")),
                "{calls:?}"
            );
        } else {
            assert_eq!(
                (row.status.as_str(), row.detail.as_str()),
                ("blocked", restore_waits(&saved[0].target).as_str()),
                "{}",
                report.to_text()
            );
            assert!(
                !calls.iter().any(|call| touches_notification_center(call)),
                "{calls:?}"
            );
            assert_eq!(
                load_backup(&root.path().join("original-preferences.json"))
                    .unwrap()
                    .len(),
                2,
                "the backup keeps it"
            );
        }
    }
}

fn setup_records(root: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(root)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .filter(|(name, _)| {
            !["original-preferences.json", WITHHELD_RECORD].contains(&name.as_str())
        })
        .collect()
}

/// A Dock under management: once relaunched it takes a second to start, then shows the
/// pins it is given whatever was written. `defaults` reads and writes its one domain, and
/// `pgrep`, `launchctl print` and `killall` act on it, and each wait moves it on.
#[derive(Default)]
struct ManagedDock(std::sync::Mutex<ManagedState>);

#[derive(Default)]
struct ManagedState {
    prefs: Vec<(String, PValue)>,
    pid: u32,
    /// The sleeps left until a relaunched Dock is past its start.
    starting: Option<u32>,
    kills: usize,
    writes: usize,
    interrupt_before_restart: bool,
    keep_original: bool,
}

/// One pin of `label`, with the GUID a Dock gives a tile it loaded.
fn loaded_pin(label: &str, guid: i64) -> PValue {
    PValue::Dict(vec![
        ("GUID".into(), PValue::Int(guid)),
        (
            "tile-data".into(),
            PValue::Dict(vec![("file-label".into(), PValue::Str(label.into()))]),
        ),
    ])
}

/// The pins management gives the Dock.
fn managed_pins() -> PValue {
    PValue::Array(vec![loaded_pin("Managed", 7)])
}

impl Model for ManagedDock {
    fn answer(&self, cmd: &Cmd) -> Option<Ran> {
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut dock = self.0.lock().unwrap();
        match (cmd.program.to_str()?, args.as_slice()) {
            (DEFAULTS, ["export", "com.apple.dock", "-"]) => {
                let entries = dock.prefs.clone();
                Some(Ran::ok(&plist::document(&PValue::Dict(entries))))
            }
            (DEFAULTS, ["write", "com.apple.dock", key, fragment]) => {
                let value = plist::parse(fragment.as_bytes()).ok()?;
                dock.prefs.retain(|(k, _)| k != key);
                dock.prefs.push(((*key).into(), value));
                dock.writes += 1;
                Some(Ran::ok(""))
            }
            ("/usr/bin/pgrep", ["-u", _, "-x", "Dock"]) => {
                Some(Ran::ok(&format!("{}\n", dock.pid)))
            }
            ("/bin/launchctl", ["print", _]) => Some(Ran::ok("")),
            ("/usr/bin/killall", ["-u", _, "Dock"]) => {
                if dock.interrupt_before_restart {
                    dock.interrupt_before_restart = false;
                    drop(dock);
                    panic!("simulated exit before the reader was relaunched");
                }
                dock.pid += 1;
                dock.starting = Some(1);
                dock.kills += 1;
                Some(Ran::ok(""))
            }
            _ => None,
        }
    }

    fn alive(&self, pid: u32) -> bool {
        self.0.lock().unwrap().pid == pid
    }

    fn sleep(&self) {
        let mut dock = self.0.lock().unwrap();
        if let Some(left) = dock.starting.as_mut() {
            *left = left.saturating_sub(1);
            if *left == 0 {
                dock.starting = None;
                if !dock.keep_original {
                    dock.prefs.retain(|(k, _)| k != "persistent-apps");
                    dock.prefs.push(("persistent-apps".into(), managed_pins()));
                }
            }
        }
    }
}

/// The relaunch after a value is put back waits, as the shared items' own does, until the
/// reader runs again and its reads settle, so a managed Dock that shows its own pins a
/// second into its start is reported as not keeping the original. Negative control: a read
/// right after a bare kill races the Dock's start and reads the original back.
#[test]
fn a_restores_read_back_waits_for_the_relaunched_reader_to_settle() {
    let pins = Target::new("com.apple.dock", "persistent-apps");
    let original = PValue::Array(vec![loaded_pin("Mail", 1)]);
    let dock = std::sync::Arc::new(ManagedDock::default());
    {
        let mut state = dock.0.lock().unwrap();
        state.pid = 100;
        state.prefs.push((
            "persistent-apps".into(),
            PValue::Array(vec![loaded_pin("aterm", 2)]),
        ));
    }
    let root = aterm_tempfile::tempdir().expect("scratch");
    save_backup(
        &root.path().join("original-preferences.json"),
        &[Saved {
            target: pins.clone(),
            value: Some(original.clone()),
            granted: true,
        }],
    )
    .unwrap();
    let inert = Inert {
        full_disk_access: Some(true),
        model: Some(std::sync::Arc::clone(&dock) as std::sync::Arc<dyn Model>),
        ..Inert::default()
    };
    let mut host = NativeHost::over(
        Box::new(inert),
        Box::new(|_, _, _| Ok(Workstation::of(Vec::new()))),
        initial::complete_explicit_workstation_plan_for_test,
    );
    let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
    let row = report
        .rows
        .iter()
        .find(|row| row.name == pins.describe())
        .unwrap();
    assert_eq!(
        (row.status.as_str(), row.detail.as_str()),
        ("error", NOT_RETAINED),
        "{}",
        report.to_text()
    );
    assert!(
        !report.rows.iter().any(|row| row.name == "Reload Dock"),
        "{}",
        report.to_text()
    );
    assert_eq!(dock.0.lock().unwrap().kills, 1);

    let racing = ManagedDock::default();
    racing.0.lock().unwrap().pid = 100;
    let write = defaults_cmd(&pins, Some(&original));
    racing.answer(&write).unwrap();
    let kill = Cmd::new("/usr/bin/killall").args(["-u", "501", "Dock"]);
    racing.answer(&kill).unwrap();
    let export = Cmd::new(DEFAULTS).args(["export", "com.apple.dock", "-"]);
    let read = plist::parse_domain(&racing.answer(&export).unwrap().stdout).unwrap();
    assert_eq!(read, [("persistent-apps".to_owned(), Ok(original))]);
}

#[test]
fn an_interrupted_restore_rechecks_an_already_original_value_after_relaunch() {
    for keep_original in [true, false] {
        let pins = Target::new("com.apple.dock", "persistent-apps");
        let original = PValue::Array(vec![loaded_pin("Mail", 1)]);
        let dock = std::sync::Arc::new(ManagedDock(std::sync::Mutex::new(ManagedState {
            prefs: vec![(
                "persistent-apps".into(),
                PValue::Array(vec![loaded_pin("aterm", 2)]),
            )],
            pid: 100,
            interrupt_before_restart: true,
            keep_original,
            ..ManagedState::default()
        })));
        let root = aterm_tempfile::tempdir().expect("scratch");
        let backup = root.path().join("original-preferences.json");
        save_backup(
            &backup,
            &[Saved {
                target: pins.clone(),
                value: Some(original.clone()),
                granted: true,
            }],
        )
        .unwrap();
        let before = std::fs::read(&backup).unwrap();
        let new_host = || {
            NativeHost::over(
                Box::new(Inert {
                    full_disk_access: Some(true),
                    model: Some(std::sync::Arc::clone(&dock) as std::sync::Arc<dyn Model>),
                    ..Inert::default()
                }),
                Box::new(|_, _, _| Ok(Workstation::of(Vec::new()))),
                initial::complete_explicit_workstation_plan_for_test,
            )
        };
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute(&mut new_host(), root.path(), Action::Restore)
        }));
        assert!(interrupted.is_err());
        assert_eq!(
            pending_refresh(root.path()).unwrap(),
            std::slice::from_ref(&pins)
        );
        {
            let state = dock.0.lock().unwrap();
            assert_eq!(state.prefs, [("persistent-apps".into(), original.clone())]);
            assert_eq!((state.writes, state.kills), (1, 0));
        }

        let report = execute(&mut new_host(), root.path(), Action::Restore).unwrap();
        let row = report
            .rows
            .iter()
            .find(|row| row.name == pins.describe())
            .unwrap();
        let state = dock.0.lock().unwrap();
        assert_eq!((state.writes, state.kills), (1, 1));
        assert_eq!(
            state.prefs,
            [(
                "persistent-apps".into(),
                if keep_original {
                    original
                } else {
                    managed_pins()
                },
            )]
        );
        assert!(pending_refresh(root.path()).unwrap().is_empty());
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        assert_eq!(
            report.needs_attention(),
            !keep_original,
            "{}",
            report.to_text()
        );
        assert_eq!(
            row.status,
            if keep_original { "restored" } else { "error" },
            "{}",
            report.to_text()
        );
    }
}

/// A shared item that pins the Dock and never verifies, whose before-image of the
/// Dock's pins, `.0`, the doctor's rollback puts back.
struct Repinned(PValue);

impl aterm_machines::items::Item for Repinned {
    fn id(&self) -> String {
        "init.test.repinned".into()
    }
    fn family(&self) -> &'static str {
        "test"
    }
    fn title(&self) -> String {
        "init.test.repinned".into()
    }
    fn probe(
        &self,
        _: &dyn System,
        _: &aterm_machines::items::Ctx,
    ) -> aterm_machines::model::Probe {
        aterm_machines::model::Probe::Change {
            from: "a".into(),
            to: "b".into(),
        }
    }
    fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
        Ok(())
    }
    fn preference_changes(
        &self,
        _: &dyn System,
        _: &aterm_machines::items::Ctx,
    ) -> Result<
        Vec<(
            aterm_machines::items::defaults::Target,
            aterm_machines::model::Read<PValue>,
        )>,
        String,
    > {
        let alice = aterm_machines::model::User {
            name: "alice".into(),
            uid: 501,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        Ok(vec![(
            aterm_machines::items::defaults::Target::user(
                "com.apple.dock",
                "persistent-apps",
                &alice,
            ),
            aterm_machines::model::Read::Value(self.0.clone()),
        )])
    }
}

/// A rollback's read-back runs once each reader is up again, so pins a managed Dock did not
/// keep are reported in one `error` row and not also as back. Negative control: without the
/// read-back the report says the pins are back.
#[test]
fn a_rollback_its_relaunched_reader_did_not_keep_is_reported_once_as_not_kept() {
    let pins = Target::new("com.apple.dock", "persistent-apps");
    let original = PValue::Array(vec![loaded_pin("Mail", 1)]);
    let dock = std::sync::Arc::new(ManagedDock::default());
    {
        let mut state = dock.0.lock().unwrap();
        state.pid = 100;
        state.prefs.push((
            "persistent-apps".into(),
            PValue::Array(vec![loaded_pin("aterm", 2)]),
        ));
    }
    let root = aterm_tempfile::tempdir().expect("scratch");
    let inert = Inert {
        full_disk_access: Some(true),
        model: Some(std::sync::Arc::clone(&dock) as std::sync::Arc<dyn Model>),
        ..Inert::default()
    };
    let before = original.clone();
    let mut host = NativeHost::over(
        Box::new(inert),
        Box::new(move |_, group, _| {
            Ok(Workstation::of(if group == WorkstationGroup::Preferences {
                vec![Box::new(Repinned(before.clone())) as Box<dyn aterm_machines::items::Item>]
            } else {
                Vec::new()
            }))
        }),
        initial::complete_explicit_workstation_plan_for_test,
    );
    let report = execute(&mut host, root.path(), Action::Apply).expect("apply");
    let rollback: Vec<(&str, &str)> = report
        .rows
        .iter()
        .filter(|row| row.name == "Preference rollback")
        .map(|row| (row.status.as_str(), row.detail.as_str()))
        .collect();
    let lost = format!("{NOT_RETAINED}: {}", pins.describe());
    assert_eq!(rollback, [("error", lost.as_str())], "{}", report.to_text());
    assert_eq!(dock.0.lock().unwrap().kills, 1, "{}", report.to_text());
}

#[test]
fn notification_center_waits_for_the_grant_and_is_cleared_once_silently_when_it_arrives() {
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);

    let root = aterm_tempfile::tempdir().unwrap();
    authorize_automatic_setup_at(root.path(), widgets).unwrap();
    let mut doctor = Withholding::widgets();
    let report = doctor
        .pass(root.path())
        .unwrap()
        .expect("the setup pass runs");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert_eq!(doctor.notification_center_writes(), 0);
    assert!(!doctor.notification_center_cleared());
    assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
    assert!(
        Automatic::load(root.path(), initial::revision_at(root.path()).unwrap())
            .unwrap()
            .settled
            .contains(&WIDGETS.to_owned()),
        "settled for the keys it reached"
    );
    doctor.show_desktop_widgets();
    let docks = doctor.dock_relaunches();
    for unknown in [None, Some(false), None] {
        doctor.host.full_disk_access = unknown;
        assert!(doctor.follow_up(root.path()).unwrap().is_none());
    }
    assert_eq!(doctor.notification_center_writes(), 0);
    let records = setup_records(root.path());

    doctor.grant();
    let follow_up = doctor
        .follow_up(root.path())
        .unwrap()
        .expect("the grant is held");
    assert!(!follow_up.needs_attention(), "{}", follow_up.to_text());
    assert!(doctor.notification_center_cleared());
    assert_eq!(doctor.notification_center_writes(), 1);
    assert_eq!(doctor.notification_center_relaunches(), 1);
    assert_eq!(doctor.dock_relaunches(), docks);
    assert!(
        doctor.desktop_widgets_shown(),
        "the person's change stays theirs"
    );
    assert_eq!(setup_records(root.path()), records);
    assert!(Withholding::withheld(root.path()).is_empty());
    let backup = load_backup(&root.path().join("original-preferences.json")).unwrap();
    let saved = backup.iter().find(|s| s.target == nc_widgets()).unwrap();
    assert!(saved.granted && saved.value.as_ref().is_some_and(|v| v.same(&populated())));
    assert!(doctor.follow_up(root.path()).unwrap().is_none(), "once");
    assert_eq!(doctor.notification_center_writes(), 1);

    let root = aterm_tempfile::tempdir().unwrap();
    authorize_automatic_setup_at(root.path(), widgets).unwrap();
    let mut doctor = Withholding::widgets();
    doctor.grant();
    doctor
        .pass(root.path())
        .unwrap()
        .expect("the setup pass runs");
    assert!(doctor.notification_center_cleared());
    assert_eq!(doctor.notification_center_writes(), 1);
    assert!(Withholding::withheld(root.path()).is_empty());
    assert!(doctor.follow_up(root.path()).unwrap().is_none());
}

/// The one widgets item aterm 0.118.0 and older recorded, `init.widgets@<user>`, reads as
/// Notification Center's row for the follow-up, which clears that list once, and as each of
/// the three widget rows when settled. Negative control: reading the follow-up record's ids
/// as written runs no follow-up.
#[test]
fn a_follow_up_an_older_aterm_recorded_still_clears_notification_center() {
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_automatic_setup_at(root.path(), widgets).unwrap();
    record_withheld(
        root.path(),
        &["init.widgets@alice".to_owned()],
        &[],
        &[],
        (BUILD, 0),
    )
    .unwrap();
    let mut doctor = Withholding::widgets();
    doctor.grant();
    let follow_up = doctor
        .follow_up(root.path())
        .unwrap()
        .expect("the grant is held");
    assert!(!follow_up.needs_attention(), "{}", follow_up.to_text());
    assert!(doctor.notification_center_cleared());
    assert_eq!(doctor.notification_center_writes(), 1);
    assert!(Withholding::withheld(root.path()).is_empty());
    let revision = initial::revision_at(root.path()).unwrap();
    write_json(
        &root.path().join(HELD_RECORD),
        &json!({
            "profile": PROFILE, "revision": revision,
            "settled": ["init.widgets@alice"], "failed": [],
        }),
    )
    .unwrap();
    assert_eq!(
        Automatic::load(root.path(), revision).unwrap().settled,
        [
            "consumer.widgets.desktop@alice",
            "consumer.widgets.stage-manager@alice",
            WIDGETS,
        ]
    );
}

#[test]
fn the_follow_up_record_keeps_an_item_that_ran_but_left_a_key_alone_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    let ids =
        |names: &[&str]| -> Vec<String> { names.iter().map(|name| (*name).to_owned()).collect() };
    let turn = (BUILD, 60);
    record_withheld(root.path(), &ids(&["a", "b"]), &[], &[], turn).unwrap();
    record_withheld(
        root.path(),
        &ids(&["a", "c"]),
        &ids(&["a", "b"]),
        &ids(&["a"]),
        turn,
    )
    .unwrap();
    let record = load_withheld(root.path()).unwrap();
    assert_eq!(record.items, ["a", "c"]);
    assert!(record.failed.is_empty(), "{record:?}");
    record_withheld(root.path(), &[], &[], &ids(&["c", "d"]), turn).unwrap();
    let record = load_withheld(root.path()).unwrap();
    assert_eq!(record.items, ["a", "c"]);
    assert_eq!(
        put_off(&record.failed, BUILD, 61),
        [("c".to_owned(), Hold::Later(LATER.into()))]
    );
    record_withheld(root.path(), &[], &ids(&["a", "c"]), &[], turn).unwrap();
    assert!(!root.path().join(WITHHELD_RECORD).exists());
}

#[test]
fn a_retry_with_the_grant_leaves_the_settled_row_and_the_follow_up_reaches_what_it_left() {
    let mut selected = Selection::none();
    selected.set(Setting::Widgets, true);
    selected.set(Setting::Dock, true);
    for granted in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_automatic_setup_at(root.path(), selected).unwrap();
        let mut doctor = Withholding::widgets_and_a_dock_that_never_verifies();
        doctor
            .pass(root.path())
            .unwrap()
            .expect("the setup pass runs");
        assert_eq!(doctor.notification_center_writes(), 0);
        if granted {
            doctor.grant();
        }
        doctor.pass(root.path()).unwrap().expect("the retry runs");
        assert_eq!(doctor.notification_center_writes(), 0);
        assert_eq!(doctor.window_manager_writes(), 1);
        assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
        assert_eq!(doctor.follow_up(root.path()).unwrap().is_some(), granted);
        assert_eq!(doctor.notification_center_cleared(), granted);
        assert_eq!(doctor.notification_center_writes(), usize::from(granted));
        assert_eq!(doctor.window_manager_writes(), 1);
        assert_eq!(Withholding::withheld(root.path()).is_empty(), granted);
    }
}

/// A follow-up whose change does not verify stays owed and is tried again by itself on its
/// pauses ([`RETRY`]), each longer than the last, and never held for a build.
#[test]
fn a_follow_up_that_does_not_verify_stays_recorded_and_is_tried_again_less_often() {
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_automatic_setup_at(root.path(), widgets).unwrap();
    let mut doctor = Withholding::widgets();
    doctor
        .pass(root.path())
        .unwrap()
        .expect("the setup pass runs");
    assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
    doctor.grant();
    doctor.host.failing = vec![nc_widgets()];
    let tried = |doctor: &mut Withholding, root: &Path| {
        let report = doctor.follow_up(root).unwrap().expect("the follow-up runs");
        assert!(report.needs_attention(), "{}", report.to_text());
        assert!(!doctor.notification_center_cleared(), "rolled back");
        assert_eq!(Withholding::withheld(root), [WIDGETS]);
    };
    let held = |doctor: &mut Withholding, root: &Path| {
        let events = doctor.host.events.len();
        let report = doctor.follow_up(root).unwrap().expect("the follow-up runs");
        let row = report.rows.iter().find(|row| row.name == WIDGETS).unwrap();
        assert_eq!(row.status, DEFERRED, "{}", report.to_text());
        assert!(
            row.detail.ends_with(&format!("not changed yet: {LATER}")),
            "{}",
            row.detail
        );
        assert!(
            doctor.host.events[events..].is_empty(),
            "nothing written, relaunched or journaled: {:?}",
            &doctor.host.events[events..]
        );
        assert_eq!(Withholding::withheld(root), [WIDGETS]);
    };
    tried(&mut doctor, root.path());
    assert_eq!(
        doctor.notification_center_writes(),
        2,
        "the try and its rollback"
    );
    doctor.wait(std::time::Duration::from_secs(5 * 60));
    held(&mut doctor, root.path());
    doctor.wait(std::time::Duration::from_secs(5 * 60));
    tried(&mut doctor, root.path());
    assert_eq!(doctor.notification_center_writes(), 4);
    let second = std::time::Duration::from_secs(1);
    for (tries, writes) in [(2, 6), (3, 8)] {
        doctor.wait(RETRY.after(tries) - second);
        held(&mut doctor, root.path());
        doctor.wait(second);
        tried(&mut doctor, root.path());
        assert_eq!(
            doctor.notification_center_writes(),
            writes,
            "try {} once its pause ends",
            tries + 1
        );
    }
    doctor.build = "1.0.1+gtest";
    doctor.host.failing.clear();
    let report = doctor
        .follow_up(root.path())
        .unwrap()
        .expect("the follow-up runs");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert!(doctor.notification_center_cleared());
    assert!(Withholding::withheld(root.path()).is_empty());
    assert!(doctor.follow_up(root.path()).unwrap().is_none());
}

#[test]
fn a_finished_setups_follow_up_holds_the_busy_slot_while_it_may_hold_the_profile_lock() {
    use std::cell::{Cell, RefCell};
    struct Slot<'a> {
        root: &'a Path,
        released: &'a RefCell<Vec<(bool, bool)>>,
    }
    impl Drop for Slot<'_> {
        fn drop(&mut self) {
            let free = HeldLock::try_take(self.root).is_ok_and(|lock| lock.is_some());
            let ran = load_withheld(self.root).is_ok_and(|record| record.items.is_empty());
            self.released.borrow_mut().push((free, ran));
        }
    }
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_automatic_setup_at(root.path(), widgets).unwrap();
    let taken = RefCell::new(Vec::new());
    let released = RefCell::new(Vec::new());
    let reserve = || {
        taken
            .borrow_mut()
            .push(HeldLock::try_take(root.path()).is_ok_and(|lock| lock.is_some()));
        Slot {
            root: root.path(),
            released: &released,
        }
    };
    let built = Cell::new(0);
    let probed = RefCell::new(Vec::new());
    let host = |full_disk_access| {
        built.set(built.get() + 1);
        probed.borrow_mut().push((
            taken.borrow().len(),
            HeldLock::try_take(root.path()).is_ok_and(|lock| lock.is_some()),
        ));
        Ok(Fake {
            notification_center: true,
            values: BTreeMap::from([(nc_widgets(), populated())]),
            full_disk_access,
            ..Fake::default()
        })
    };
    let now = std::time::SystemTime::now();
    assert!(
        follow_up_finished_at(root.path(), reserve, || host(Some(true)), BUILD, now)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        (taken.borrow().len(), built.get()),
        (0, 0),
        "nothing owed: no slot, no grant measured"
    );
    record_withheld(root.path(), &[WIDGETS.to_owned()], &[], &[], (BUILD, 0)).unwrap();
    for full_disk_access in [None, Some(false)] {
        assert!(
            follow_up_finished_at(root.path(), reserve, || host(full_disk_access), BUILD, now)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(taken.borrow().len(), 2, "each grant read holds both leases");
    let report = follow_up_finished_at(root.path(), reserve, || host(Some(true)), BUILD, now)
        .unwrap()
        .expect("the follow-up runs");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert_eq!(*taken.borrow(), [true; 3], "the slot, before the lock");
    assert_eq!(
        *probed.borrow(),
        [(1, false), (2, false), (3, false)],
        "each grant is measured after acquiring the slot and the lock"
    );
    assert_eq!(
        *released.borrow(),
        [(true, false), (true, false), (true, true)],
        "the slot, after the follow-up and its lock"
    );
    record_withheld(root.path(), &[WIDGETS.to_owned()], &[], &[], (BUILD, 0)).unwrap();
    write_legacy_cancel_at(root.path());
    let unbuilt = || -> Result<Fake, String> { panic!("a stopped setup measures nothing") };
    assert!(
        follow_up_finished_at(root.path(), reserve, unbuilt, BUILD, now)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        taken.borrow().len(),
        3,
        "a stopped setup takes no further slot"
    );
    assert_eq!(
        Withholding::withheld(root.path()),
        [WIDGETS],
        "the record stays"
    );
    let done = aterm_tempfile::tempdir().unwrap();
    write_json(
        &done.path().join("first-run.json"),
        &json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
    )
    .unwrap();
    assert!(first_run_complete(done.path()).unwrap());
    record_withheld(done.path(), &[WIDGETS.to_owned()], &[], &[], (BUILD, 0)).unwrap();
    let report = follow_up_finished_at(done.path(), || (), || host(Some(true)), BUILD, now)
        .unwrap()
        .expect("a completed setup's follow-up runs");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert!(Withholding::withheld(done.path()).is_empty());
}

#[test]
fn nothing_is_followed_up_after_setup_was_restored_or_cancelled() {
    let mut selected = Selection::none();
    selected.set(Setting::Widgets, true);
    selected.set(Setting::Dock, true);
    let mut dock = Selection::none();
    dock.set(Setting::Dock, true);
    for stop in ["restore", "cancel", "unchecked", "none"] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_automatic_setup_at(root.path(), selected).unwrap();
        let mut doctor = Withholding::widgets();
        doctor.host.failing = vec![wanted()[0].0.clone()];
        doctor
            .pass(root.path())
            .unwrap()
            .expect("the setup pass runs");
        assert!(!first_run_complete(root.path()).unwrap());
        match stop {
            "restore" => {
                execute(&mut doctor.host, root.path(), Action::Restore).unwrap();
                assert!(Withholding::withheld(root.path()).is_empty());
            }
            "cancel" => write_legacy_cancel_at(root.path()),
            "unchecked" => save_automatic_plan(root.path(), dock).unwrap(),
            _ => {}
        }
        doctor.grant();
        let follow_up = doctor.follow_up(root.path()).unwrap();
        assert_eq!(follow_up.is_some(), stop == "none", "{stop}");
        assert_eq!(
            doctor.notification_center_cleared(),
            stop == "none",
            "{stop}"
        );
    }
}

#[test]
fn backup_original_value_missing_refuses_restore_without_effects() {
    let root = aterm_tempfile::tempdir().unwrap();
    let _lock = HeldLock::take(root.path()).unwrap();
    let (dock, applied) = wanted().remove(0);
    let original = PValue::Bool(true);
    let mut selected = Selection::none();
    selected.set(Setting::Dock, true);
    let mut host = Fake {
        selected: Some(selected),
        values: BTreeMap::from([(dock.clone(), original.clone())]),
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).unwrap();
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert_eq!(host.values.get(&dock), Some(&applied));

    let backup = root.path().join("original-preferences.json");
    let mut record: Value = aterm_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
    let Value::Array(keys) = record.as_object_mut().unwrap().get_mut("keys").unwrap() else {
        panic!("Apply's backup has keys");
    };
    assert_eq!(keys.len(), 1);
    let fields = keys[0].as_object_mut().unwrap();
    assert_eq!(
        fields.remove("xml"),
        Some(Value::from(plist::document(&original)))
    );
    write_json(&backup, &record).unwrap();
    let damaged = std::fs::read(&backup).unwrap();
    let (writes, restarts, lines) = (host.writes.len(), host.restarts.len(), host.lines.len());

    let restored = run_mutation(&mut host, root.path(), Action::Restore);
    assert_eq!(
        host.values.get(&dock),
        Some(&applied),
        "a missing before-image must not be treated as an instruction to delete"
    );
    assert_eq!(host.writes.len(), writes);
    assert_eq!(host.restarts.len(), restarts);
    assert_eq!(host.lines.len(), lines);
    assert!(restored.is_err());
    assert_eq!(std::fs::read(&backup).unwrap(), damaged);
    assert!(
        initial::finished_at(root.path()).unwrap(),
        "Restore still stops setup"
    );
}

#[test]
fn backup_original_value_explicit_null_still_restores_absence() {
    let root = aterm_tempfile::tempdir().unwrap();
    let _lock = HeldLock::take(root.path()).unwrap();
    let (dock, applied) = wanted().remove(0);
    let mut selected = Selection::none();
    selected.set(Setting::Dock, true);
    let mut host = Fake {
        selected: Some(selected),
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).unwrap();
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert_eq!(host.values.get(&dock), Some(&applied));
    let backup = root.path().join("original-preferences.json");
    let bytes = std::fs::read(&backup).unwrap();
    let record: Value = aterm_json::from_slice(&bytes).unwrap();
    let keys = record["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].get("xml"), Some(&Value::Null));
    let writes = host.writes.len();
    let restored = run_mutation(&mut host, root.path(), Action::Restore).unwrap();
    assert!(!restored.needs_attention(), "{}", restored.to_text());
    assert!(!host.values.contains_key(&dock), "the original was absent");
    assert_eq!(host.writes.len(), writes + 1);
    assert_eq!(std::fs::read(&backup).unwrap(), bytes);
}

#[test]
fn a_restore_never_deletes_what_a_read_without_the_grant_saved_as_absent() {
    let backup_of = |widgets: Value| {
        json!({ "profile": PROFILE, "keys": [
            widgets,
            { "domain": "com.apple.WindowManager", "key": "StandardHideWidgets",
              "host": false, "xml": plist::document(&PValue::Bool(false)) },
        ] })
    };
    let legacy = |xml: Value| json!({ "domain": NC, "key": "widgets", "host": false, "xml": xml });
    let root = aterm_tempfile::tempdir().unwrap();
    let backup = root.path().join("original-preferences.json");
    write_json(
        &backup,
        &backup_of(legacy(json!(plist::document(&populated())))),
    )
    .unwrap();
    assert!(
        load_backup(&backup)
            .unwrap()
            .iter()
            .all(|saved| saved.granted)
    );
    for (granted, full_disk_access) in [
        (false, None),
        (false, Some(false)),
        (false, Some(true)),
        (true, Some(true)),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let backup = root.path().join("original-preferences.json");
        let widgets = if granted {
            json!({ "domain": NC, "key": "widgets", "host": false, "xml": null, "granted": true })
        } else {
            legacy(Value::Null)
        };
        write_json(&backup, &backup_of(widgets)).unwrap();
        let saved = load_backup(&backup).unwrap();
        assert_eq!(saved[0].granted, granted);
        assert!(saved[1].granted);
        let (mut host, calls) = inert_host(full_disk_access, Vec::new);
        let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
        let calls = calls.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .any(|call| call.contains("export com.apple.WindowManager")),
            "{calls:?}"
        );
        let row = report
            .rows
            .iter()
            .find(|row| row.name == format!("{NC} widgets"))
            .unwrap();
        if granted {
            assert_eq!(row.status, "error", "the inert system runs nothing");
            assert!(
                calls.iter().any(|call| touches_notification_center(call)),
                "{calls:?}"
            );
        } else {
            assert_eq!(
                (row.status.as_str(), Some(row.detail.clone())),
                ("blocked", unknown_original(&saved[0])),
                "{full_disk_access:?}"
            );
            assert!(
                !calls.iter().any(|call| touches_notification_center(call)),
                "{calls:?}"
            );
        }
    }
}

#[test]
fn the_first_read_with_the_grant_replaces_a_before_image_the_denial_wrote() {
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let other = PValue::Dict(vec![("vers".into(), PValue::Int(2))]);
    for (old, original) in [
        (
            Saved {
                target: nc_widgets(),
                value: None,
                granted: false,
            },
            populated(),
        ),
        (
            Saved {
                target: nc_widgets(),
                value: Some(other.clone()),
                granted: true,
            },
            other.clone(),
        ),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let backup = root.path().join("original-preferences.json");
        save_backup(&backup, &[old]).unwrap();
        let mut host = Fake {
            selected: Some(widgets),
            notification_center: true,
            full_disk_access: Some(true),
            values: BTreeMap::from([(nc_widgets(), populated())]),
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).unwrap();
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert!(
            host.values
                .get(&nc_widgets())
                .is_some_and(|now| now.same(&cleared()))
        );
        let saved = load_backup(&backup).unwrap();
        let nc = saved.iter().find(|s| s.target == nc_widgets()).unwrap();
        assert!(nc.granted);
        assert!(nc.value.as_ref().is_some_and(|v| v.same(&original)));
        execute(&mut host, root.path(), Action::Restore).unwrap();
        assert!(
            host.values
                .get(&nc_widgets())
                .is_some_and(|now| now.same(&original))
        );
    }
}

#[test]
fn a_granted_absence_is_kept_by_the_backup_and_restored_by_deleting_the_key() {
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let root = aterm_tempfile::tempdir().unwrap();
    let backup = root.path().join("original-preferences.json");
    save_backup(
        &backup,
        &[Saved {
            target: nc_widgets(),
            value: None,
            granted: true,
        }],
    )
    .unwrap();
    let mut host = Fake {
        selected: Some(widgets),
        notification_center: true,
        full_disk_access: Some(true),
        values: BTreeMap::from([(nc_widgets(), populated())]),
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).unwrap();
    assert!(!report.needs_attention(), "{}", report.to_text());
    let saved = load_backup(&backup).unwrap();
    let nc = saved.iter().find(|s| s.target == nc_widgets()).unwrap();
    assert!(
        nc.granted && nc.value.is_none(),
        "the recorded absence stays"
    );
    let report = execute(&mut host, root.path(), Action::Restore).unwrap();
    let row = report
        .rows
        .iter()
        .find(|row| row.name == nc_widgets().describe())
        .unwrap();
    assert_eq!(row.status, "restored", "{}", report.to_text());
    assert!(!host.values.contains_key(&nc_widgets()), "deleted");
}

#[test]
fn a_notification_center_relaunch_owed_waits_for_the_grant_instead_of_being_dropped() {
    let activation_rows = |report: &Report| -> Vec<String> {
        report
            .rows
            .iter()
            .filter(|row| row.name == "Preference activation")
            .map(|row| row.status.clone())
            .collect()
    };
    for unknown in [None, Some(false)] {
        let root = aterm_tempfile::tempdir().unwrap();
        save_pending_refresh(root.path(), &[nc_widgets()]).unwrap();
        let mut host = Fake {
            full_disk_access: unknown,
            ..Fake::default()
        };
        let check = execute(&mut host, root.path(), Action::Check).unwrap();
        assert_eq!(activation_rows(&check), ["blocked"], "{}", check.to_text());
        execute(&mut host, root.path(), Action::Apply).unwrap();
        execute(&mut host, root.path(), Action::Restore).unwrap();
        assert!(
            !host.restarts.iter().any(|p| p == "NotificationCenter"),
            "{:?}",
            host.restarts
        );
        assert_eq!(pending_refresh(root.path()).unwrap(), [nc_widgets()]);

        host.full_disk_access = Some(true);
        let check = execute(&mut host, root.path(), Action::Check).unwrap();
        assert_eq!(activation_rows(&check), ["drift"], "{}", check.to_text());
        execute(&mut host, root.path(), Action::Apply).unwrap();
        assert_eq!(
            host.restarts
                .iter()
                .filter(|p| *p == "NotificationCenter")
                .count(),
            1,
            "{:?}",
            host.restarts
        );
        assert!(pending_refresh(root.path()).unwrap().is_empty());
    }
}

struct Contained;

impl aterm_machines::items::Item for Contained {
    fn id(&self) -> String {
        "init.test.contained".into()
    }
    fn family(&self) -> &'static str {
        "test"
    }
    fn title(&self) -> String {
        "init.test.contained".into()
    }
    fn probe(
        &self,
        _: &dyn System,
        _: &aterm_machines::items::Ctx,
    ) -> aterm_machines::model::Probe {
        aterm_machines::model::Probe::Change {
            from: "a".into(),
            to: "b".into(),
        }
    }
    fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
        Ok(())
    }
    fn preference_changes(
        &self,
        _: &dyn System,
        _: &aterm_machines::items::Ctx,
    ) -> Result<
        Vec<(
            aterm_machines::items::defaults::Target,
            aterm_machines::model::Read<PValue>,
        )>,
        String,
    > {
        let alice = aterm_machines::model::User {
            name: "alice".into(),
            uid: 501,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        Ok(vec![(
            aterm_machines::items::defaults::Target::user(NC, "widgets", &alice),
            aterm_machines::model::Read::Absent,
        )])
    }
}

struct LeftAlone(bool);

impl aterm_machines::items::Item for LeftAlone {
    fn id(&self) -> String {
        "init.test.left-alone".into()
    }
    fn family(&self) -> &'static str {
        "test"
    }
    fn title(&self) -> String {
        "init.test.left-alone".into()
    }
    fn probe(
        &self,
        _: &dyn System,
        _: &aterm_machines::items::Ctx,
    ) -> aterm_machines::model::Probe {
        aterm_machines::model::Probe::Ok { now: "b".into() }
    }
    fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
        Ok(())
    }
    fn withheld(&self) -> Option<String> {
        self.0.then(|| "waits for Full Disk Access".into())
    }
}

#[test]
fn a_row_that_left_a_key_alone_is_settled_and_recorded_for_the_follow_up() {
    let item = "init.test.left-alone".to_owned();
    for left_alone in [true, false] {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let items: fn() -> Vec<Box<dyn aterm_machines::items::Item>> = if left_alone {
            || vec![Box::new(LeftAlone(true))]
        } else {
            || vec![Box::new(LeftAlone(false))]
        };
        let (mut host, _) = inert_host(None, items);
        let pass = execute_pass(&mut host, root.path(), Action::Apply, BUILD, 0).expect("apply");
        let row = pass
            .report
            .rows
            .iter()
            .find(|row| row.name == item)
            .unwrap();
        assert_eq!(row.status, "ok", "{}", pass.report.to_text());
        assert!(pass.settled.contains(&item));
        assert_eq!(
            load_withheld(root.path()).unwrap().items.contains(&item),
            left_alone
        );
    }
}

/// Fake widgets item with separate Window Manager and Notification Center state.
#[derive(Clone, Default)]
struct Bundled {
    hidden: std::sync::Arc<std::sync::atomic::AtomicBool>,
    applies: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    granted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cleared: std::sync::Arc<std::sync::atomic::AtomicBool>,
    contained_only: bool,
}

impl Bundled {
    fn on(flag: &std::sync::atomic::AtomicBool) -> bool {
        flag.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl aterm_machines::items::Item for Bundled {
    fn id(&self) -> String {
        WIDGETS.into()
    }
    fn family(&self) -> &'static str {
        "widgets"
    }
    fn title(&self) -> String {
        WIDGETS.into()
    }
    fn probe(
        &self,
        _: &dyn System,
        _: &aterm_machines::items::Ctx,
    ) -> aterm_machines::model::Probe {
        use aterm_machines::model::Probe;
        let mut from = Vec::new();
        if !self.contained_only && !Self::on(&self.hidden) {
            from.push("com.apple.WindowManager StandardHideWidgets".to_owned());
        }
        if Self::on(&self.granted) && !Self::on(&self.cleared) {
            from.push(format!("{NC} widgets"));
        }
        if from.is_empty() {
            Probe::Ok {
                now: "hidden".into(),
            }
        } else {
            Probe::Change {
                from: from.join(", "),
                to: "hidden".into(),
            }
        }
    }
    fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
        if !self.contained_only {
            self.applies
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.hidden.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if Self::on(&self.granted) {
            self.cleared
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }
    fn withheld(&self) -> Option<String> {
        (!Self::on(&self.granted)).then(|| "Notification Center's wait for Full Disk Access".into())
    }
}

/// A withheld row whose key changed after aterm set it is set once more, as any key, while
/// the follow-up still owes its other app's part; changed again after that, every later pass
/// leaves it as it is, and the follow-up still owes it.
#[test]
fn a_withheld_row_changed_after_aterm_set_it_is_set_again_once_then_left() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut selected = Selection::none();
    selected.set(Setting::Widgets, true);
    selected.set(Setting::Dock, true);
    authorize_automatic_setup_at(root.path(), selected).unwrap();
    let widgets = Bundled::default();
    let never = Scripted::new("init.test.never", &[false]);
    let items = (widgets.clone(), never.clone());
    let mut host = NativeHost::over(
        Box::new(Inert::default()),
        Box::new(move |_, group, _| {
            Ok(Workstation::of(if group == WorkstationGroup::Preferences {
                vec![
                    Box::new(items.0.clone()) as Box<dyn aterm_machines::items::Item>,
                    Box::new(items.1.clone()),
                ]
            } else {
                Vec::new()
            }))
        }),
        initial::complete_explicit_workstation_plan_for_test,
    );
    let tick = |host: &mut NativeHost, minute: u64| -> Report {
        apply_on_first_run_with(root.path(), &Inert::default(), host, BUILD, at(minute))
            .expect("tick")
            .expect("setup still runs")
    };
    tick(&mut host, 0);
    assert_eq!(widgets.applies.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(load_withheld(root.path()).unwrap().items, [WIDGETS]);
    widgets
        .hidden
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let again = tick(&mut host, 1);
    let row = again.rows.iter().find(|row| row.name == WIDGETS).unwrap();
    assert_eq!(row.status, "applied", "{}", again.to_text());
    assert_eq!(widgets.applies.load(std::sync::atomic::Ordering::SeqCst), 2);
    widgets
        .hidden
        .store(false, std::sync::atomic::Ordering::SeqCst);
    for minute in 2..30 {
        let report = tick(&mut host, minute);
        let row = report.rows.iter().find(|row| row.name == WIDGETS).unwrap();
        assert_eq!(row.status, CHANGED, "minute {minute}: {}", report.to_text());
        assert!(row.detail.ends_with(initial::kept::KEPT), "{}", row.detail);
    }
    assert_eq!(
        widgets.applies.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "changed again after aterm set it again: left as it is"
    );
    assert!(!widgets.hidden.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        never.applies(),
        2,
        "the Dock's two tries, and setup still retries"
    );
    assert!(!first_run_complete(root.path()).unwrap());
    assert_eq!(load_withheld(root.path()).unwrap().items, [WIDGETS]);
}

/// A narrowed pass leaves the other group's settings as the person left them.
#[test]
fn a_narrowed_preferences_pass_selects_only_the_groups_it_names() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    authorize_automatic_setup_at(root.path(), Selection::all()).unwrap();
    let mut only = Selection::none();
    for setting in aterm_types::macos_setup::Group::Desktop.settings() {
        only.set(*setting, true);
    }
    // The second pass a minute on, past the automatic gate's pause.
    let pass = |only, minute| {
        let mut host = Fake::default();
        apply_part_with(
            root.path(),
            &Inert::default(),
            &mut host,
            BUILD,
            at(minute),
            Part::Preferences { only },
        )
        .expect("pass");
        host.selected
    };
    assert_eq!(pass(Some(only), 0), Some(only));
    assert_eq!(
        pass(None, 1),
        Some(initial::preferences(Selection::all()).workstation())
    );
}

/// After a Restore, a choice turned Desktop & Dock to Set up: its Workstation pass (the
/// real one, scoped to that group) marks the first-run decision pending without
/// dropping what the Restore keeps, so Keyboard & Trackpad, which it never set, still
/// stand restored and the profile does not read applied; the pass takes out only what
/// it set. NEGATIVE CONTROL: the same pass with no Restore reads applied.
#[test]
fn a_narrowed_pass_after_a_restore_leaves_the_other_group_restored() {
    use aterm_types::macos_setup::Group;
    let group = |group: Group| {
        let mut selection = Selection::none();
        for setting in group.settings() {
            selection.set(*setting, true);
        }
        selection
    };
    let (desktop, input) = (group(Group::Desktop), group(Group::Input));
    let record = |root: &Path| -> Value {
        aterm_json::from_slice(&std::fs::read(root.join("first-run.json")).unwrap()).unwrap()
    };
    let pass = |root: &Path| {
        let mut host = Fake::default();
        let report = apply_part_with(
            root,
            &Inert::default(),
            &mut host,
            BUILD,
            at(0),
            Part::Preferences {
                only: Some(desktop),
            },
        )
        .expect("pass")
        .expect("the pass ran");
        assert!(!report.needs_attention(), "{:?}", report.rows);
        assert_eq!(host.selected, Some(desktop));
    };
    let root = aterm_tempfile::tempdir().expect("scratch");
    authorize_automatic_setup_at(root.path(), Selection::all()).unwrap();
    write_json(
        &root.path().join("first-run.json"),
        &json!({
            "profile": PROFILE,
            "state": "pending",
            "restored": initial::preferences(Selection::all()).bits(),
            "queued": desktop.bits(),
            "selection": Selection::all().bits(),
        }),
    )
    .unwrap();
    pass(root.path());
    let after = record(root.path());
    assert_eq!(after["state"].as_str(), Some("pending"), "{after}");
    assert_eq!(
        after["restored"].as_u64(),
        Some(u64::from(input.bits())),
        "Keyboard & Trackpad were never set again: {after}"
    );
    assert_eq!(after["queued"].as_u64(), Some(0), "{after}");
    assert!(!first_run_complete(root.path()).unwrap());

    let root = aterm_tempfile::tempdir().expect("scratch");
    authorize_automatic_setup_at(root.path(), Selection::all()).unwrap();
    pass(root.path());
    assert!(
        first_run_complete(root.path()).unwrap(),
        "{}",
        record(root.path())
    );
}

/// A Workstation pass a choice narrowed to nothing (a group turned to Set up, then back
/// before its pass landed) reports the stage done, never a blocked retry on every pass.
#[test]
fn an_empty_workstation_scope_is_a_finished_stage() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    authorize_automatic_setup_at(root.path(), Selection::all()).unwrap();
    let mut host = Fake::default();
    let pass = apply_part_with(
        root.path(),
        &Inert::default(),
        &mut host,
        BUILD,
        at(0),
        Part::Preferences {
            only: Some(Selection::none()),
        },
    )
    .expect("pass");
    let rows = pass.expect("an empty scope is a report, not a retry").rows;
    assert!(
        rows.iter().all(|row| row.status == "ok") && !rows.is_empty(),
        "{rows:?}"
    );
    assert!(host.selected.is_none(), "nothing was set");
}

#[test]
fn the_grant_after_a_withheld_row_settled_leaves_it_owed_to_the_follow_up_and_says_so() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut selected = Selection::none();
    selected.set(Setting::Widgets, true);
    selected.set(Setting::Dock, true);
    authorize_automatic_setup_at(root.path(), selected).unwrap();
    let widgets = Bundled::default();
    let never = Scripted::new("init.test.never", &[false]);
    let host = |full_disk_access: Option<bool>| {
        let items = (widgets.clone(), never.clone());
        NativeHost::over(
            Box::new(Inert {
                full_disk_access,
                ..Inert::default()
            }),
            Box::new(move |_, group, _| {
                Ok(Workstation::of(match group {
                    WorkstationGroup::Preferences => vec![
                        Box::new(items.0.clone()) as Box<dyn aterm_machines::items::Item>,
                        Box::new(items.1.clone()),
                    ],
                    WorkstationGroup::Withheld => vec![Box::new(Bundled {
                        contained_only: true,
                        ..items.0.clone()
                    })],
                    _ => Vec::new(),
                }))
            }),
            initial::complete_explicit_workstation_plan_for_test,
        )
    };
    let row = |full_disk_access: Option<bool>, minute: u64| -> (String, String) {
        let report = apply_on_first_run_with(
            root.path(),
            &Inert::default(),
            &mut host(full_disk_access),
            BUILD,
            at(minute),
        )
        .expect("tick")
        .expect("setup still runs");
        let row = report.rows.iter().find(|row| row.name == WIDGETS).unwrap();
        (row.status.clone(), row.detail.clone())
    };
    let applies = || widgets.applies.load(std::sync::atomic::Ordering::SeqCst);
    let window_manager = "com.apple.WindowManager StandardHideWidgets";
    let notification_center = format!("{NC} widgets");

    row(None, 0);
    assert_eq!(applies(), 1);
    assert_eq!(load_withheld(root.path()).unwrap().items, [WIDGETS]);
    widgets
        .granted
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        row(Some(true), 1),
        (
            "ok".into(),
            format!("{notification_center}; left as it is: {SETTLED_OWED}")
        )
    );
    assert_eq!(
        load_withheld(root.path()).unwrap().items,
        [WIDGETS],
        "a held row is not reached: the follow-up still owes Notification Center"
    );
    widgets
        .hidden
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        row(Some(true), 2),
        (
            "ok".into(),
            format!("{window_manager}, {notification_center}; left as it is: {SETTLED_OWED}")
        )
    );
    assert_eq!(applies(), 1, "the person's change stays theirs");

    let follow_up = follow_up_at(root.path(), &mut host(Some(true)), BUILD, at(3))
        .unwrap()
        .expect("the grant is held");
    assert!(!follow_up.needs_attention(), "{}", follow_up.to_text());
    assert!(Bundled::on(&widgets.cleared));
    assert!(!Bundled::on(&widgets.hidden));
    assert!(load_withheld(root.path()).unwrap().items.is_empty());
    assert!(
        !initial::kept::Kept::load(root.path())
            .unwrap()
            .keeps(WIDGETS),
        "a change while setup owed the widgets' step is no change after aterm set them"
    );
    // Nothing owed now: the change since the read-back is set once more, since nothing says
    // who made it.
    assert_eq!(row(Some(true), 4).0, "applied");
    assert_eq!(applies(), 2);
    assert!(Bundled::on(&widgets.hidden));
    widgets
        .hidden
        .store(false, std::sync::atomic::Ordering::SeqCst);
    for minute in 5..7 {
        assert_eq!(
            row(Some(true), minute),
            (
                CHANGED.into(),
                format!("{window_manager}; left as it is: {}", initial::kept::KEPT)
            ),
            "minute {minute}"
        );
    }
    assert_eq!(applies(), 2, "changed again after aterm set it again: left");
    assert!(!Bundled::on(&widgets.hidden));
    assert_eq!(
        never.applies(),
        1,
        "the Dock's first try; its second is not due"
    );
}

#[test]
fn a_pass_without_the_grant_never_records_again_a_row_it_held() {
    let mut selected = Selection::none();
    selected.set(Setting::Widgets, true);
    selected.set(Setting::Dock, true);
    for cleared_by in ["a pass with the grant", "the follow-up"] {
        let root = aterm_tempfile::tempdir().expect("scratch");
        authorize_automatic_setup_at(root.path(), selected).unwrap();
        let widgets = Bundled::default();
        let never = Scripted::new("init.test.never", &[false]);
        let host = |full_disk_access: Option<bool>| {
            widgets.granted.store(
                full_disk_access == Some(true),
                std::sync::atomic::Ordering::SeqCst,
            );
            let items = (widgets.clone(), never.clone());
            NativeHost::over(
                Box::new(Inert {
                    full_disk_access,
                    ..Inert::default()
                }),
                Box::new(move |_, group, _| {
                    Ok(Workstation::of(match group {
                        WorkstationGroup::Preferences => vec![
                            Box::new(items.0.clone()) as Box<dyn aterm_machines::items::Item>,
                            Box::new(items.1.clone()),
                        ],
                        WorkstationGroup::Withheld => vec![Box::new(Bundled {
                            contained_only: true,
                            ..items.0.clone()
                        })],
                        _ => Vec::new(),
                    }))
                }),
                initial::complete_explicit_workstation_plan_for_test,
            )
        };
        let tick = |full_disk_access: Option<bool>, build: &str, minute: u64| {
            apply_on_first_run_with(
                root.path(),
                &Inert::default(),
                &mut host(full_disk_access),
                build,
                at(minute),
            )
            .expect("tick")
            .expect("setup still runs")
        };
        if cleared_by == "the follow-up" {
            tick(None, BUILD, 0);
            assert_eq!(load_withheld(root.path()).unwrap().items, [WIDGETS]);
            follow_up_at(root.path(), &mut host(Some(true)), BUILD, at(1))
                .unwrap()
                .expect("the grant is held");
        } else {
            tick(Some(true), BUILD, 0);
        }
        assert!(Bundled::on(&widgets.cleared), "{cleared_by}");
        assert!(load_withheld(root.path()).unwrap().items.is_empty());
        widgets
            .cleared
            .store(false, std::sync::atomic::Ordering::SeqCst);

        let report = tick(None, "1.0.1+gtest", 2);
        let row = report.rows.iter().find(|row| row.name == WIDGETS).unwrap();
        assert_eq!(row.status, "ok", "{cleared_by}: {}", report.to_text());
        assert!(
            load_withheld(root.path()).unwrap().items.is_empty(),
            "{cleared_by}: a held row is not recorded again"
        );
        assert!(
            follow_up_at(root.path(), &mut host(Some(true)), BUILD, at(3))
                .unwrap()
                .is_none(),
            "{cleared_by}"
        );
        // The widget list changed after it read back: set once more, then left as it is
        // once it changes again.
        tick(Some(true), BUILD, 4);
        assert!(Bundled::on(&widgets.cleared), "{cleared_by}: set once more");
        widgets
            .cleared
            .store(false, std::sync::atomic::Ordering::SeqCst);
        tick(Some(true), BUILD, 5);
        assert!(
            !Bundled::on(&widgets.cleared),
            "{cleared_by}: changed again after aterm set it again, it stays"
        );
        assert!(
            initial::kept::Kept::load(root.path())
                .unwrap()
                .keeps(WIDGETS),
            "{cleared_by}"
        );
        assert_eq!(widgets.applies.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}

#[test]
fn the_rollback_leaves_a_container_before_image_alone_without_the_grant() {
    for full_disk_access in [None, Some(false), Some(true)] {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let (mut host, calls) = inert_host(full_disk_access, || vec![Box::new(Contained)]);
        let report = execute(&mut host, root.path(), Action::Apply).expect("apply");
        let item = report
            .rows
            .iter()
            .find(|row| row.name == "init.test.contained")
            .unwrap();
        assert_eq!(item.status, "error", "{}", report.to_text());
        let calls = calls.lock().unwrap().clone();
        if full_disk_access == Some(true) {
            assert!(
                calls.contains(&format!("{DEFAULTS} export {NC} -")),
                "{calls:?}"
            );
        } else {
            assert!(
                report
                    .rows
                    .iter()
                    .any(|row| row.name == "Preference rollback"
                        && row.status == "blocked"
                        && row.detail.contains("Full Disk Access")),
                "{}",
                report.to_text()
            );
            assert!(
                !calls.iter().any(|call| touches_notification_center(call)),
                "{calls:?}"
            );
        }
    }
}

/// A shared item whose probes answer from a list (the last one repeats), counting its applies.
#[derive(Clone)]
struct Scripted {
    id: &'static str,
    probes: std::sync::Arc<std::sync::Mutex<Vec<aterm_machines::model::Probe>>>,
    applies: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Scripted {
    fn new(id: &'static str, wanted: &[bool]) -> Self {
        use aterm_machines::model::Probe;
        let probes = wanted
            .iter()
            .map(|wanted| {
                if *wanted {
                    Probe::Ok { now: "b".into() }
                } else {
                    Probe::Change {
                        from: "a".into(),
                        to: "b".into(),
                    }
                }
            })
            .collect();
        Self {
            id,
            probes: std::sync::Arc::new(std::sync::Mutex::new(probes)),
            applies: std::sync::Arc::default(),
        }
    }

    fn applies(&self) -> usize {
        self.applies.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl aterm_machines::items::Item for Scripted {
    fn id(&self) -> String {
        self.id.into()
    }
    fn family(&self) -> &'static str {
        "test"
    }
    fn title(&self) -> String {
        self.id.into()
    }
    fn probe(
        &self,
        _: &dyn System,
        _: &aterm_machines::items::Ctx,
    ) -> aterm_machines::model::Probe {
        let mut probes = self.probes.lock().unwrap();
        if probes.len() > 1 {
            probes.remove(0)
        } else {
            probes[0].clone()
        }
    }
    fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
        self.applies
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// The worker tries a standing failure again less often, through the doctor's own host, and
/// sets once more a key that changed after it read back, since nothing says who changed it;
/// one that changes again after that is left as it is, in [`initial::kept::KEPT`]'s words.
#[test]
fn the_worker_tries_again_less_often_through_the_doctors_own_host() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut selected = Selection::none();
    selected.set(Setting::Dock, true);
    authorize_automatic_setup_at(root.path(), selected).unwrap();
    let never = Scripted::new("init.test.never", &[false]);
    // Set, read back, changed, set again and read back, then changed for good.
    let drifts = Scripted::new("init.test.drifts", &[false, true, false, true, false]);
    let applies = || (never.applies(), drifts.applies());
    let inert = Inert::default();
    let (calls, lines) = (
        std::sync::Arc::clone(&inert.calls),
        std::sync::Arc::clone(&inert.lines),
    );
    let items = (never.clone(), drifts.clone());
    let mut host = NativeHost::over(
        Box::new(inert),
        Box::new(move |_, group, _| {
            Ok(Workstation::of(if group == WorkstationGroup::Preferences {
                vec![
                    Box::new(items.0.clone()) as Box<dyn aterm_machines::items::Item>,
                    Box::new(items.1.clone()),
                ]
            } else {
                Vec::new()
            }))
        }),
        initial::complete_explicit_workstation_plan_for_test,
    );
    let tick = |host: &mut NativeHost, minute: u64, build: &str| -> Report {
        apply_on_first_run_with(root.path(), &Inert::default(), host, build, at(minute))
            .expect("tick")
            .expect("setup still runs")
    };
    let row = |report: &Report, name: &str| {
        let row = report.rows.iter().find(|row| row.name == name).unwrap();
        (row.status.clone(), row.detail.clone())
    };
    let later = || (DEFERRED.to_owned(), format!("a; not changed yet: {LATER}"));
    let first = tick(&mut host, 0, BUILD);
    assert_eq!(
        row(&first, "init.test.never").0,
        "error",
        "{}",
        first.to_text()
    );
    assert_eq!(row(&first, "init.test.drifts").0, "applied");
    assert_eq!(applies(), (1, 1));
    let again = tick(&mut host, 1, BUILD);
    assert_eq!(row(&again, "init.test.never"), later());
    assert_eq!(
        row(&again, "init.test.drifts").0,
        "applied",
        "set once more: {}",
        again.to_text()
    );
    assert_eq!(applies(), (1, 2));
    assert!(
        !initial::kept::Kept::load(root.path())
            .unwrap()
            .keeps("init.test.drifts")
    );
    let quiet = |host: &mut NativeHost, minutes: std::ops::Range<u64>| {
        for minute in minutes {
            let report = tick(host, minute, BUILD);
            assert_eq!(row(&report, "init.test.never"), later(), "minute {minute}");
            assert_eq!(
                row(&report, "init.test.drifts"),
                (
                    CHANGED.into(),
                    format!("a; left as it is: {}", initial::kept::KEPT)
                ),
                "minute {minute}"
            );
        }
    };
    quiet(&mut host, 2..10);
    assert_eq!(applies(), (1, 2));
    assert!(
        initial::kept::Kept::load(root.path())
            .unwrap()
            .keeps("init.test.drifts")
    );
    tick(&mut host, 10, BUILD);
    assert_eq!(applies(), (2, 2), "the second try, ten minutes on");
    quiet(&mut host, 11..30);
    assert_eq!(applies(), (2, 2));
    tick(&mut host, 30, BUILD);
    assert_eq!(applies(), (3, 2), "the third try, twenty minutes on");
    tick(&mut host, 31, "1.0.1+gbbbb");
    assert_eq!(
        applies(),
        (4, 2),
        "a new build's try, and the setting left as it is"
    );
    assert!(calls.lock().unwrap().is_empty(), "{:?}", calls.lock());
    assert_eq!(
        *lines.lock().unwrap(),
        [
            "init.test.never",
            "init.test.drifts",
            "init.test.drifts",
            "init.test.never",
            "init.test.never",
            "init.test.never"
        ]
    );
}

/// Notification Center's widget list, a key of another app's container.
pub(super) fn nc_widgets() -> Target {
    Target::new(NC, "widgets")
}

fn populated() -> PValue {
    PValue::Dict(vec![
        ("vers".into(), PValue::Int(1)),
        (
            "instances".into(),
            PValue::Array(vec![PValue::Data(vec![42])]),
        ),
    ])
}

fn cleared() -> PValue {
    PValue::Dict(vec![
        ("vers".into(), PValue::Int(1)),
        ("instances".into(), PValue::Array(Vec::new())),
    ])
}

fn wanted() -> Vec<(Target, PValue)> {
    vec![
        (
            Target::new("com.apple.dock", "show-recents"),
            PValue::Bool(false),
        ),
        (
            Target::new("com.apple.WindowManager", "StandardHideWidgets"),
            PValue::Bool(true),
        ),
    ]
}

impl Host for Fake {
    fn alab(&self) -> bool {
        !self.outside_alab
    }
    fn complete_explicit(&self) -> initial::CompleteExplicit {
        initial::complete_explicit_workstation_plan_for_test
    }
    fn selection(&self) -> Selection {
        self.selected.unwrap_or_default()
    }
    fn full_disk_access(&self) -> Option<bool> {
        self.full_disk_access
    }
    fn select(&mut self, selected: Selection) {
        self.selected = Some(selected.workstation());
    }
    fn attempts(&mut self) -> Vec<Attempt> {
        std::mem::take(&mut self.attempts)
    }
    fn settled(&mut self) -> Vec<String> {
        std::mem::take(&mut self.settled)
    }
    fn waits(&mut self) -> Vec<String> {
        std::mem::take(&mut self.waits)
    }
    fn looked(&mut self) -> Vec<String> {
        std::mem::take(&mut self.looked)
    }
    fn withheld(&mut self) -> Vec<String> {
        std::mem::take(&mut self.withheld)
    }
    fn hold(&mut self, items: &[(String, Hold)]) {
        self.held = items.to_vec();
    }
    fn held(&self) -> Vec<String> {
        self.held.iter().map(|(item, _)| item.clone()).collect()
    }
    fn drifted(&mut self) -> Vec<String> {
        std::mem::take(&mut self.drifted)
    }
    fn readings(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.readings)
    }
    fn changed(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.changed)
    }
    fn item_setting(&self, item: &str) -> Option<Setting> {
        self.items()
            .into_iter()
            .find(|(id, _)| id == item)
            .and_then(|(_, keys)| keys.first().map(|(target, _)| Self::setting(target)))
    }
    fn journal(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String> {
        if self.journal_refuses && id.starts_with("doctor.") {
            self.events.push(format!("refused {id}"));
            return Err("the journal is full".into());
        }
        self.events.push(format!("journal {id}"));
        self.lines
            .push((id.into(), from.into(), to.into(), revert.map(str::to_owned)));
        Ok(())
    }
    fn restore_runtime(&mut self, saved: &[Saved]) -> Vec<RuntimeRow> {
        self.runtime_changed = false;
        self.runtime_given
            .extend(saved.iter().map(|saved| saved.target.clone()));
        Vec::new()
    }
    fn read(&mut self, target: &Target) -> Result<Option<PValue>, String> {
        Ok(self.values.get(target).cloned())
    }
    fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String> {
        assert!(
            !self.dies_in_doctor_writes,
            "the process ends among the doctor's own writes"
        );
        if self.refuses.contains(target) {
            return Err("injected write refusal".into());
        }
        self.put(target, value)
    }
    fn preference_changes(&mut self, group: WorkstationGroup) -> Result<Vec<Saved>, String> {
        self.preference_passes += 1;
        self.attempts.clear();
        self.settled.clear();
        self.withheld.clear();
        self.drifted.clear();
        self.readings.clear();
        self.changed.clear();
        Ok(self
            .wants()
            .into_iter()
            .filter(|(target, _)| group != WorkstationGroup::Withheld || contained(target))
            .filter(|(target, _)| self.reaches(target))
            .filter(|(target, _)| self.selection().contains(Self::setting(target)))
            .filter(|(target, _)| self.hold_of(&self.item(target)).is_none())
            .filter(|(t, v)| !self.values.get(t).is_some_and(|now| now.same(v)))
            .map(|(target, _)| Saved {
                value: self.values.get(&target).cloned(),
                granted: self.reaches(&target),
                target,
            })
            .collect())
    }
    fn restart(&mut self, name: &str, _reads: &[&Target]) -> Result<(), String> {
        self.restarts.push(name.into());
        self.events.push(format!("restart {name}"));
        if self.fail_restart_once {
            self.fail_restart_once = false;
            return Err("injected restart failure".into());
        }
        Ok(())
    }
    fn workstation(
        &mut self,
        group: WorkstationGroup,
        apply: bool,
    ) -> Result<Vec<(Option<Setting>, Row)>, String> {
        let mut rows = Vec::new();
        assert!(
            !(apply && group == WorkstationGroup::Preferences && self.dies_in_preferences),
            "the process ends in its preference pass"
        );
        if group != WorkstationGroup::Accounts {
            for (item, keys) in self.items() {
                let keys: Vec<(Target, PValue)> = keys
                    .into_iter()
                    .filter(|(target, _)| {
                        self.selection().contains(Self::setting(target))
                            && (group != WorkstationGroup::Withheld || contained(target))
                    })
                    .collect();
                let Some(setting) = keys.first().map(|(target, _)| Self::setting(target)) else {
                    continue;
                };
                let left_alone = keys.iter().any(|(target, _)| !self.reaches(target));
                if apply && left_alone {
                    self.withheld.push(item.clone());
                }
                let targets: Vec<Target> = keys.iter().map(|(target, _)| target.clone()).collect();
                let unmet: Vec<(Target, PValue)> = keys
                    .into_iter()
                    .filter(|(target, value)| {
                        self.reaches(target)
                            && !self.values.get(target).is_some_and(|now| now.same(value))
                    })
                    .collect();
                let mut status = if unmet.is_empty() { "ok" } else { "drift" };
                let mut detail = if left_alone {
                    "Notification Center's wait for Full Disk Access".to_owned()
                } else {
                    "injected shared item".to_owned()
                };
                // As the native host's held items read at their probe
                // (`aterm_converge::left_as_is`): a check too reads one left as it is,
                // changed since aterm set it.
                if !apply
                    && !unmet.is_empty()
                    && let Some(Hold::AsIs(why)) = self.hold_of(&item)
                {
                    status = if changed(&why) { CHANGED } else { "ok" };
                    let from: Vec<String> =
                        unmet.iter().map(|(target, _)| target.describe()).collect();
                    detail = format!("{}; left as it is: {why}", from.join(", "));
                }
                if apply && !unmet.is_empty() {
                    match self.hold_of(&item) {
                        Some(Hold::Later(why)) => {
                            status = DEFERRED;
                            let from: Vec<String> =
                                unmet.iter().map(|(target, _)| target.describe()).collect();
                            detail = format!("{}; not changed yet: {why}", from.join(", "));
                        }
                        Some(Hold::AsIs(why)) => {
                            status = if changed(&why) { CHANGED } else { "ok" };
                            let from: Vec<String> =
                                unmet.iter().map(|(target, _)| target.describe()).collect();
                            detail = format!("{}; left as it is: {why}", from.join(", "));
                            self.drifted.push(item.clone());
                        }
                        // `hold_of` resolves a hold as read back as the native pass does.
                        Some(Hold::ReadBack { .. }) | None
                            if unmet
                                .iter()
                                .any(|(target, _)| self.blocked.contains(target)) =>
                        {
                            status = "blocked";
                        }
                        Some(Hold::ReadBack { .. }) | None => {
                            self.events.push(format!("journal {item}"));
                            let mut failed = std::mem::take(&mut self.fail_once);
                            let mut preferences = Vec::new();
                            let reads_before: Vec<String> = targets
                                .iter()
                                .map(|target| format!("{:?}", self.values.get(target)))
                                .collect();
                            for (target, value) in &unmet {
                                let before = self.values.get(target).cloned();
                                self.put(target, Some(value))?;
                                if self.written_back.contains(target) {
                                    failed = true;
                                    match &before {
                                        Some(before) => {
                                            self.values.insert(target.clone(), before.clone());
                                        }
                                        None => {
                                            self.values.remove(target);
                                        }
                                    }
                                }
                                failed |= self.failing.contains(target);
                                preferences.push(Saved {
                                    target: target.clone(),
                                    value: before,
                                    granted: true,
                                });
                            }
                            for process in aterm_machines::init_activation::RELAUNCHED {
                                if unmet.iter().any(|(target, _)| {
                                    aterm_machines::init_activation::readers(&target.domain)
                                        .contains(&process)
                                }) {
                                    self.events.push(format!("relaunch {process}"));
                                }
                            }
                            self.runtime_changed = true;
                            status = if failed { "error" } else { "applied" };
                            if !failed {
                                self.changed.push((item.clone(), reads_before.join(", ")));
                            }
                            self.attempts.push(Attempt {
                                item: item.clone(),
                                verified: !failed,
                                preferences,
                            });
                        }
                    }
                }
                if apply && matches!(status, "ok" | "applied") {
                    self.settled.push(item.clone());
                    let reads: Vec<String> = targets
                        .iter()
                        .map(|target| format!("{:?}", self.values.get(target)))
                        .collect();
                    self.readings.push((item.clone(), reads.join(", ")));
                }
                rows.push((
                    Some(setting),
                    Row {
                        name: item,
                        status: status.into(),
                        detail,
                        elapsed_ms: None,
                    },
                ));
            }
            assert!(
                !(apply && group == WorkstationGroup::Preferences && self.dies_after_item_writes),
                "the process ends after its items' writes, before they relaunch their readers"
            );
        }
        if matches!(group, WorkstationGroup::Accounts | WorkstationGroup::All)
            && !WorkstationGroup::Accounts
                .selected(self.selection())
                .is_empty()
        {
            self.account_passes += 1;
            let item = "Account sign-out".to_owned();
            if apply {
                self.looked.push(item.clone());
            }
            let mut detail = "injected account result".to_owned();
            let status = if self.accounts_blocked {
                "blocked"
            } else if self.accounts_wait && apply {
                self.waits.push(item.clone());
                detail = "this Mac's screen is locked; aterm looks again by itself".into();
                initial::WAITING
            } else if !self.accounts_fail {
                "ok"
            } else if !apply {
                // As the native host's check reads a held sign-out at its probe
                // (`aterm_converge::left_as_is`): left as it is, changed since aterm did it.
                match self.hold_of(&item) {
                    Some(Hold::AsIs(why)) => {
                        let status = if changed(&why) { CHANGED } else { "ok" };
                        detail = format!("account remains registered; left as it is: {why}");
                        status
                    }
                    _ => "drift",
                }
            } else {
                match self.hold_of(&item) {
                    Some(Hold::Later(why)) => {
                        detail = why;
                        DEFERRED
                    }
                    Some(Hold::AsIs(why)) => {
                        let status = if changed(&why) { CHANGED } else { "ok" };
                        detail = why;
                        status
                    }
                    Some(Hold::ReadBack { .. }) | None => {
                        self.events.push(format!("journal {item}"));
                        self.attempts.push(Attempt {
                            item: item.clone(),
                            verified: false,
                            preferences: Vec::new(),
                        });
                        "error"
                    }
                }
            };
            if apply && status == "ok" {
                self.settled.push(item.clone());
            }
            rows.push((
                Some(Setting::Messages),
                Row {
                    name: item,
                    status: status.into(),
                    detail,
                    elapsed_ms: None,
                },
            ));
            if self.facetime_blocked {
                rows.push((
                    Some(Setting::FaceTime),
                    Row {
                        name: "FaceTime sign-out".into(),
                        status: "blocked".into(),
                        detail: "injected blocked sign-out".into(),
                        elapsed_ms: None,
                    },
                ));
            }
        }
        Ok(rows)
    }
}

#[test]
fn selected_preferences_leave_unchecked_accounts_and_recovery_targets_untouched() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut selected = Selection::none();
    selected.set(Setting::Widgets, true);
    authorize_automatic_setup_at(root.path(), selected).unwrap();
    assert_eq!(authorized_selection(root.path()).unwrap(), Some(selected));
    let dock = wanted()[0].0.clone();
    save_pending_refresh(root.path(), std::slice::from_ref(&dock)).unwrap();
    let old_activation = std::fs::read(root.path().join("pending-refresh.json")).unwrap();
    let mut host = Fake {
        selected: Some(selected),
        accounts_blocked: true,
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).unwrap();
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert_eq!(
        host.account_passes, 0,
        "unchecked account operations never run"
    );
    assert_eq!(host.writes, [wanted()[1].0.clone()]);
    assert!(!host.values.contains_key(&dock));
    assert!(
        host.restarts.is_empty(),
        "do not activate an unchecked old Dock obligation"
    );
    assert_eq!(
        std::fs::read(root.path().join("pending-refresh.json")).unwrap(),
        old_activation
    );
    let saved = load_backup(&report.backup).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].target, wanted()[1].0);
}

#[test]
fn account_only_and_empty_selections_never_probe_preferences() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut selected = Selection::none();
    selected.set(Setting::Messages, true);
    let mut host = Fake {
        selected: Some(selected),
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).unwrap();
    assert!(!report.needs_attention());
    assert_eq!(host.preference_passes, 0);
    assert_eq!(host.account_passes, 1);
    assert!(host.writes.is_empty());
    assert!(!report.backup.exists());

    let empty_root = aterm_tempfile::tempdir().unwrap();
    authorize_automatic_setup_at(empty_root.path(), Selection::none()).unwrap();
    assert!(first_run_complete(empty_root.path()).unwrap());
    assert_eq!(
        authorized_selection(empty_root.path()).unwrap(),
        Some(Selection::none())
    );
    let mut host = Fake {
        selected: Some(Selection::none()),
        ..Fake::default()
    };
    let report = execute(&mut host, empty_root.path(), Action::Apply).unwrap();
    assert!(!report.needs_attention());
    assert_eq!(host.preference_passes, 0);
    assert_eq!(host.account_passes, 0);
    assert!(host.writes.is_empty());
    assert!(host.restarts.is_empty());
    assert!(!report.backup.exists());
}

#[test]
fn authorization_survives_reopen_and_missing_or_unknown_plans_are_refused() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert_eq!(authorized_selection(root.path()).unwrap(), None);
    let mut selected = Selection::all();
    selected.set(Setting::Messages, false);
    selected.set(Setting::FaceTime, false);
    authorize_automatic_setup_at(root.path(), selected).unwrap();
    assert_eq!(authorized_selection(root.path()).unwrap(), Some(selected));
    assert!(!first_run_complete(root.path()).unwrap());
    save_automatic_plan(root.path(), Selection::all()).unwrap();
    assert_eq!(
        authorized_selection(root.path()).unwrap(),
        Some(Selection::all())
    );
    write_json(
        &root.path().join("automatic-plan.json"),
        &json!({"profile": PROFILE, "selection": 65535}),
    )
    .unwrap();
    assert!(authorized_selection(root.path()).is_err());
}

#[test]
fn stale_automatic_authorization_preserves_every_completed_decision() {
    let root = aterm_tempfile::tempdir().unwrap();
    let marker_path = root.path().join("first-run.json");
    let mut original = Selection::none();
    original.set(Setting::Widgets, true);
    save_automatic_plan(root.path(), original).unwrap();
    let plan = std::fs::read(root.path().join("automatic-plan.json")).unwrap();
    for marker in [
        json!({"profile": PROFILE, "state": "cancelled"}),
        json!({"profile": PROFILE, "state": "restored"}),
        json!({"profile": PROFILE, "state": "applied", "accounts_verified": true}),
    ] {
        write_json(&marker_path, &marker).unwrap();
        let before = std::fs::read(&marker_path).unwrap();
        let _lock = HeldLock::take(root.path()).unwrap();
        authorize_automatic_setup_at(root.path(), Selection::all()).unwrap();
        assert_eq!(std::fs::read(&marker_path).unwrap(), before);
        assert_eq!(
            std::fs::read(root.path().join("automatic-plan.json")).unwrap(),
            plan
        );
    }
}

#[test]
fn visible_finish_and_authorization_markers_must_discharge_failed_namespace_flushes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let marker = root.path().join("first-run.json");
    for value in [
        json!({"profile": PROFILE, "state": "cancelled"}),
        json!({"profile": PROFILE, "state": "restored"}),
        json!({"profile": PROFILE, "state": "applied", "accounts_verified": true}),
    ] {
        assert!(
            write_json_with_flush(&marker, &value, |_, _| Err(std::io::Error::other(
                "failed directory flush"
            )))
            .is_err()
        );
        assert!(marker.exists(), "the rename is visible even on failure");
        assert!(
            first_run_complete_with_flush(root.path(), |_, _| Err(std::io::Error::other(
                "still not durable"
            )))
            .is_err()
        );
        let mut reached_root = false;
        assert!(
            first_run_complete_with_flush(root.path(), |directory, _| {
                reached_root |= directory.parent().is_none();
                Ok(())
            })
            .unwrap()
        );
        assert!(
            reached_root,
            "the retry flushes the complete namespace chain"
        );
    }
    let plan = root.path().join("automatic-plan.json");
    assert!(
        write_json_with_flush(
            &plan,
            &json!({"profile":PROFILE,"selection":Selection::all().bits()}),
            |_, _| Err(std::io::Error::other("failed plan flush"))
        )
        .is_err()
    );
    assert!(
        authorized_selection_with_flush(root.path(), |_, _| Err(std::io::Error::other(
            "still not durable"
        )))
        .is_err()
    );
    assert_eq!(
        authorized_selection_with_flush(root.path(), |_, _| Ok(())).unwrap(),
        Some(Selection::all())
    );
}

#[test]
fn shared_apply_is_idempotent_and_restore_preserves_unowned_keys() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake::default();
    let unowned = Target::new("com.apple.dock", "orientation");
    host.values
        .insert(unowned.clone(), PValue::Str("left".into()));
    let first = execute(&mut host, root.path(), Action::Apply).expect("apply");
    assert!(!first.needs_attention(), "{}", first.to_text());
    assert!(first_run_complete(root.path()).expect("complete marker"));
    let count = host.writes.len();
    let backup = std::fs::read(&first.backup).expect("backup");
    assert!(
        !execute(&mut host, root.path(), Action::Apply)
            .expect("reapply")
            .needs_attention()
    );
    assert_eq!(host.writes.len(), count);
    assert_eq!(
        backup,
        std::fs::read(&first.backup).expect("same originals")
    );
    host.values
        .insert(unowned.clone(), PValue::Str("right".into()));
    assert!(
        !execute(&mut host, root.path(), Action::Restore)
            .expect("restore")
            .needs_attention()
    );
    assert_eq!(host.values.len(), 1);
    assert_eq!(host.values[&unowned], PValue::Str("right".into()));
    // Restore ends the preference work; what it leaves for the offer is aterm's own
    // permissions alone, which no run held here.
    let staged =
        initial::initial_setup_snapshot_at(root.path(), aterm_types::macos_setup::Plan::default())
            .expect("snapshot")
            .plan
            .selection;
    let mut permissions = Selection::none();
    permissions.set(aterm_types::macos_setup::Setting::Permissions, true);
    assert_eq!(staged, permissions, "restore disables automatic setup");
}

#[test]
fn an_older_cancel_is_durable_until_explicit_apply_and_check_leaves_it_alone() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    assert!(!first_run_complete(root.path()).expect("new profile"));
    write_legacy_cancel_at(root.path());
    assert!(first_run_complete(root.path()).expect("cancelled profile"));
    let marker_path = root.path().join("first-run.json");
    let cancelled = std::fs::read(&marker_path).expect("durable cancellation");
    assert_eq!(
        aterm_json::from_slice::<Value>(&cancelled).unwrap()["state"].as_str(),
        Some("cancelled")
    );
    let mut host = Fake {
        accounts_blocked: true,
        ..Fake::default()
    };
    execute(&mut host, root.path(), Action::Check).expect("check cancellation");
    assert!(host.writes.is_empty());
    assert_eq!(std::fs::read(&marker_path).unwrap(), cancelled);
    assert!(!root.path().join("original-preferences.json").exists());

    let report = execute(&mut host, root.path(), Action::Apply).expect("explicit apply");
    assert!(report.needs_attention(), "blocked account remains visible");
    assert!(
        !host.writes.is_empty(),
        "explicit apply supersedes cancellation"
    );
    assert!(!first_run_complete(root.path()).expect("pending explicit apply"));
    assert_eq!(
        aterm_json::from_slice::<Value>(&std::fs::read(marker_path).unwrap()).unwrap()["state"]
            .as_str(),
        Some("pending")
    );
}

#[test]
fn a_revived_legacy_profile_finishes_only_after_successful_explicit_apply() {
    for stopped in ["cancelled", "restored"] {
        for blocked in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":stopped}),
            )
            .unwrap();
            let _profile = HeldLock::take(root.path()).unwrap();
            let explicit = initial::save_explicit_workstation_plan(
                root.path(),
                Selection::all().workstation(),
            )
            .unwrap();
            let revision = initial::revision_at(root.path()).unwrap();
            assert_ne!(revision, 0);
            let mut host = Fake {
                accounts_blocked: blocked,
                ..Fake::default()
            };
            let report = execute(&mut host, root.path(), Action::Apply).unwrap();
            assert_eq!(report.needs_attention(), blocked);
            if !report.needs_attention() {
                initial::complete_explicit_workstation_plan_for_test(root.path(), explicit)
                    .unwrap();
            }
            // With no record but the older aterm's, the plan is aterm's own base plan with
            // the profile: Rust in Spotlight and the vault are stages the window's run sets
            // up, so the run is not finished and its worker can still claim it. A Restore
            // withdrew no permission, so the plan keeps aterm's permissions too; a Cancel's
            // plan has none. The profile's own rows read applied only after an explicit
            // apply that succeeded.
            assert!(!initial::finished_at(root.path()).unwrap());
            assert_eq!(initial::revision_at(root.path()).unwrap(), revision);
            assert!(claim_automatic_setup_at(root.path()).unwrap().is_some());
            if !blocked {
                let snapshot =
                    initial::initial_setup_snapshot_at(root.path(), Default::default()).unwrap();
                let left: Vec<&str> = snapshot
                    .rows
                    .iter()
                    .filter(|row| row.status != "applied")
                    .map(|row| row.name.as_str())
                    .collect();
                let mut expected = vec![
                    "Spotlight and Rust documentation",
                    "Secrets vault",
                    "GitHub repositories",
                ];
                if stopped == "restored" {
                    expected.insert(1, "aterm permissions");
                }
                assert_eq!(left, expected, "{stopped}");
            }
        }
    }
}

#[test]
fn automatic_worker_claim_is_exclusive_and_does_not_hold_the_profile_lock() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let claim = claim_automatic_setup_at(root.path())
        .expect("claim worker")
        .expect("first worker");
    assert!(claim_automatic_setup_at(root.path()).unwrap().is_none());
    let profile = HeldLock::take(root.path()).expect("explicit apply remains available");
    drop(profile);
    // An older aterm's Cancel, saved while this worker held its claim.
    write_legacy_cancel_at(root.path());
    drop(claim);
    assert!(
        claim_automatic_setup_at(root.path()).unwrap().is_none(),
        "a cancelled setup is never offered again"
    );

    write_json(
        &root.path().join("first-run.json"),
        &json!({ "profile": PROFILE, "state": "pending" }),
    )
    .expect("explicit retry pending");
    let claim = claim_automatic_setup_at(root.path())
        .unwrap()
        .expect("retry worker");
    drop(claim);
    let replacement = claim_automatic_setup_at(root.path())
        .unwrap()
        .expect("released claim can be acquired again");
    let mut host = Fake::default();
    execute(&mut host, root.path(), Action::Apply).expect("explicit apply during an offer");
    drop(replacement);
    assert!(
        claim_automatic_setup_at(root.path()).unwrap().is_none(),
        "a completed explicit apply ends automatic setup too"
    );
}

#[test]
fn failed_shared_preference_apply_rolls_back_partial_mutations() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let (dock, widgets) = (wanted()[0].0.clone(), wanted()[1].0.clone());
    let mut host = Fake {
        fail_once: true,
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).expect("failure report");
    assert!(report.needs_attention());
    assert!(
        !host.values.contains_key(&dock),
        "even a failed write may already have mutated a preference"
    );
    assert!(
        host.values.contains_key(&widgets),
        "the item that verified keeps its change"
    );
    assert!(
        !host.runtime_changed,
        "rollback must recover active settings as well as saved preferences"
    );
    let restored: Vec<&Row> = report
        .rows
        .iter()
        .filter(|row| row.status == "restored")
        .collect();
    assert_eq!(restored.len(), 1, "{}", report.to_text());
    assert!(
        restored[0].detail.contains(&dock.describe())
            && !restored[0].detail.contains(&widgets.describe()),
        "recovery claims only what it put back: {}",
        report.to_text()
    );
    assert_eq!(
        report
            .rows
            .iter()
            .find(|row| row.name == widgets.describe())
            .map(|row| row.status.as_str()),
        Some("applied"),
        "a verified change is not relabelled"
    );
    assert_eq!(host.restarts, ["Dock"]);
    assert_eq!(host.account_passes, 1);
    assert!(!first_run_complete(root.path()).expect("failed pass remains pending"));
    assert!(report.backup.exists());
}

fn at(minutes: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000 + minutes * 60)
}

const BUILD: &str = "1.0.0+gaaaa";

#[test]
fn mac_settings_deadlines_name_every_current_run_and_build_attempt() {
    let root = aterm_tempfile::tempdir().unwrap();
    initial::save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let revision = initial::revision_at(root.path()).unwrap();
    let failed = |item: &str, build: &str, tries, at| Failed {
        item: item.into(),
        build: build.into(),
        tries,
        at,
    };
    Automatic {
        settled: vec!["settled".into()],
        failed: vec![
            failed("older", BUILD, 1, 0),
            failed("later", BUILD, 1, 600),
            failed("settled", BUILD, 1, 1),
            failed("spent", BUILD, 2, 2),
            failed("untried", BUILD, 0, 3),
            failed("another-build", "other", 1, 4),
        ],
        waits: Vec::new(),
    }
    .save(root.path(), revision)
    .unwrap();
    let due =
        |revision, build, now| mac_settings_retry_due(root.path(), revision, build, now).unwrap();
    assert_eq!(
        due(revision, BUILD, 599),
        Some(599),
        "a try recorded after the clock now (set back since) is due at once"
    );
    assert_eq!(due(revision, BUILD, 600), Some(600));
    assert_eq!(due(revision, BUILD, 1_199), Some(600));
    assert_eq!(due(revision, BUILD, 1_200), Some(1_200));
    assert_eq!(
        due(revision, BUILD, 1_202),
        Some(1_202),
        "a second try's pause is twice as long, and it still comes due"
    );
    assert_eq!(due(revision + 1, BUILD, 2_000), None, "another run");
    assert_eq!(due(revision, "absent-build", 2_000), None, "another build");
}

/// A sign-out behind a locked screen (or the person's keys, another login, an Automation
/// question nobody answered) is no failed try: it reads waiting, never deferred or failed,
/// counts no try, and the park gets a timer of its own to look again by itself, a minute
/// after the first wait and twice as long after each later one in a row ([`LOOK_AGAIN`]),
/// kept on record across relaunches; the wait ends with the first pass that finds the
/// session open. Before, nothing timed a waiting sign-out's next look.
#[test]
fn a_sign_out_behind_a_lock_looks_again_by_itself_and_counts_no_try() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let revision = initial::revision_at(root.path()).unwrap();
    let mut host = Fake {
        accounts_wait: true,
        ..Fake::default()
    };
    let start = epoch_secs(at(0));
    let ahead =
        |root: &Path, now: u64| mac_settings_retry_ahead(root, revision, BUILD, now).unwrap();
    let first = automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    let row = first
        .rows
        .iter()
        .find(|row| row.name == "Account sign-out")
        .unwrap();
    assert_eq!(row.status, initial::WAITING, "{}", row.detail);
    assert!(host.attempts.is_empty(), "no try ran");
    assert_eq!(
        ahead(root.path(), start),
        Some(start + 60),
        "a timer of its own"
    );
    assert_eq!(
        mac_settings_retry_due(root.path(), revision, BUILD, start + 60).unwrap(),
        Some(start + 60)
    );
    automatic_pass(&mut host, root.path(), BUILD, at(1)).expect("second look");
    assert_eq!(
        ahead(root.path(), start + 60),
        Some(start + 60 + 120),
        "twice as long"
    );
    let mut record = Automatic::load(root.path(), revision).unwrap();
    assert!(record.failed.is_empty(), "no failed try: {record:?}");
    assert_eq!(record.waits.len(), 1, "{record:?}");
    // Another stage's pass, which looks at no sign-out, leaves the wait in a row as it was.
    let elsewhere = Pass::from(Report {
        action: Action::Apply,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup: PathBuf::new(),
        restore: None,
        page: None,
    });
    let waits = record.waits.clone();
    assert!(!record.add(BUILD, start + 90, &elsewhere), "{record:?}");
    assert_eq!(record.waits, waits);
    // The session opens: the sign-out runs and the wait ends.
    host.accounts_wait = false;
    let done = automatic_pass(&mut host, root.path(), BUILD, at(3)).expect("open");
    assert!(done.complete(), "{done:?}");
    let record = Automatic::load(root.path(), revision).unwrap();
    assert!(record.waits.is_empty(), "{record:?}");
    assert_eq!(ahead(root.path(), start + 180), None);
}

/// A STANDING FAILURE IS NEVER GIVEN UP ON: the automatic worker tries it again by itself,
/// quietly, less often after each try ([`RETRY`]); what it puts off reads as deferred, never
/// as a failure that needs a person.
#[test]
fn a_standing_failure_is_tried_again_less_often_by_the_automatic_worker() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let (dock, widgets) = (wanted()[0].0.clone(), wanted()[1].0.clone());
    let mut host = Fake {
        failing: vec![dock.clone()],
        accounts_fail: true,
        ..Fake::default()
    };
    let first = automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    assert!(first.needs_attention());
    assert!(
        !host.values.contains_key(&dock),
        "the failed change is undone"
    );
    assert!(host.values.contains_key(&widgets), "the verified one stays");
    assert_eq!(host.restarts, ["Dock"], "{:?}", host.events);
    let sign_outs = |host: &Fake| {
        host.events
            .iter()
            .filter(|event| *event == "journal Account sign-out")
            .count()
    };
    assert_eq!(sign_outs(&host), 1);
    let quiet = |host: &mut Fake, minutes: std::ops::Range<u64>| {
        let events = host.events.len();
        for minute in minutes {
            let report = automatic_pass(host, root.path(), BUILD, at(minute)).expect("retry pass");
            assert!(!report.complete(), "minute {minute}");
            for name in [dock.describe(), "Account sign-out".into()] {
                let row = report.rows.iter().find(|row| row.name == name).unwrap();
                assert_eq!(row.status, DEFERRED, "minute {minute}: {}", row.detail);
                assert!(
                    row.detail.ends_with(LATER),
                    "minute {minute}: {}",
                    row.detail
                );
            }
        }
        assert!(
            host.events[events..].is_empty(),
            "nothing changed, relaunched or journaled: {:?}",
            &host.events[events..]
        );
    };
    quiet(&mut host, 1..10);
    automatic_pass(&mut host, root.path(), BUILD, at(10)).expect("second try");
    assert_eq!(host.restarts, ["Dock", "Dock"]);
    assert_eq!(sign_outs(&host), 2);
    quiet(&mut host, 11..30);
    automatic_pass(&mut host, root.path(), BUILD, at(30)).expect("third try");
    assert_eq!(host.restarts.len(), 3);
    assert_eq!(sign_outs(&host), 3);
    quiet(&mut host, 31..41);
    assert!(host.values.contains_key(&widgets));
    assert!(!first_run_complete(root.path()).unwrap());
    automatic_pass(&mut host, root.path(), "1.0.1+gbbbb", at(41)).expect("new build");
    assert_eq!(host.restarts.len(), 4);
    assert_eq!(sign_outs(&host), 4);
    let record = Automatic::load(root.path(), 0).unwrap();
    assert_eq!(record.settled, [widgets.describe()], "{record:?}");
    assert_eq!(
        record.failed.len(),
        4,
        "two items in two builds: {record:?}"
    );
    assert_eq!(
        Automatic::load(root.path(), 1).unwrap(),
        Automatic::default()
    );
    host.held.clear();
    execute(&mut host, root.path(), Action::Apply).expect("explicit apply");
    assert_eq!(host.restarts.len(), 5);
}

/// A FAILURE THAT STOPS FAILING IS TRIED AGAIN BY ITSELF: a Mac setting whose change did not
/// take under load is tried again on its backoff, in the same build, with no count of tries
/// that turns into a hold: once the load is gone, a later try makes the change.
#[test]
fn a_failure_that_stops_failing_is_tried_again_by_itself_in_the_same_build() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake {
        failing: vec![dock.clone()],
        ..Fake::default()
    };
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first try");
    automatic_pass(&mut host, root.path(), BUILD, at(10)).expect("second try");
    assert_eq!(host.restarts, ["Dock", "Dock"]);
    host.failing.clear();
    let revision = initial::revision_at(root.path()).unwrap();
    let due = mac_settings_retry_ahead(root.path(), revision, BUILD, epoch_secs(at(11)))
        .unwrap()
        .expect("a third try is ahead in the same build");
    let report = automatic_pass(
        &mut host,
        root.path(),
        BUILD,
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(due),
    )
    .expect("third try");
    assert!(host.values.contains_key(&dock), "{}", report.to_text());
    assert!(
        Automatic::load(root.path(), revision)
            .unwrap()
            .settled
            .contains(&dock.describe())
    );
}

#[test]
fn two_builds_taking_turns_each_keep_their_own_pauses() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake {
        failing: vec![wanted()[0].0.clone()],
        ..Fake::default()
    };
    let builds = ["1.0.0+gaaaa", "1.0.0+gbbbb"];
    let turns = |host: &mut Fake, minutes: std::ops::Range<u64>| {
        for (turn, minute) in minutes.enumerate() {
            automatic_pass(host, root.path(), builds[turn % 2], at(minute)).expect("pass");
        }
    };
    turns(&mut host, 0..8);
    assert_eq!(host.restarts, ["Dock", "Dock"], "one try each");
    turns(&mut host, 20..40);
    assert_eq!(
        host.restarts.len(),
        4,
        "a second try each, the next twenty minutes on"
    );
    turns(&mut host, 40..42);
    assert_eq!(host.restarts.len(), 6, "a third try each");
}

#[test]
fn the_record_keeps_the_builds_that_tried_last() {
    let failed = || Pass {
        report: Report {
            action: Action::Apply,
            rows: Vec::new(),
            manual_steps: Vec::new(),
            backup: PathBuf::new(),
            restore: None,
            page: None,
        },
        unverified: vec!["dock".to_owned()],
        settled: Vec::new(),
        waits: Vec::new(),
        looked: Vec::new(),
        drifted: Vec::new(),
        readings: Vec::new(),
        changed: Vec::new(),
        new_targets: Vec::new(),
    };
    let build = |n: usize| format!("1.0.{n}+g0000");
    let mut record = Automatic::default();
    let mut now = 0;
    for n in 0..HELD_BUILDS {
        now += 1;
        assert!(record.add(&build(n), now, &failed()));
    }
    now += 1;
    record.add(&build(0), now, &failed());
    now += 1;
    record.add(&build(HELD_BUILDS), now, &failed());
    let builds: Vec<&str> = record.failed.iter().map(|f| f.build.as_str()).collect();
    assert_eq!(builds.len(), HELD_BUILDS, "{builds:?}");
    assert!(!builds.contains(&build(1).as_str()), "{builds:?}");
    assert_eq!(
        record.holds(&build(0), now, &Withheld::default(), false),
        [("dock".to_owned(), Hold::Later(LATER.into()))],
        "{builds:?}"
    );
    assert!(
        record
            .holds(&build(1), now, &Withheld::default(), false)
            .is_empty()
    );
}

/// Of the items this run settled, only the one the follow-up owes is held as it is, in its
/// words: any other is the kept record's to hold, which sets it again once should it read
/// otherwise. Only under a record a newer aterm wrote, which notes nothing, is each held as it
/// is.
#[test]
fn only_the_settled_item_the_follow_up_owes_is_held_with_its_words() {
    let record = Automatic {
        settled: vec!["dock".to_owned(), WIDGETS.to_owned()],
        failed: Vec::new(),
        waits: Vec::new(),
    };
    let owed = Withheld {
        items: vec![WIDGETS.to_owned()],
        failed: Vec::new(),
    };
    assert_eq!(
        record.holds(BUILD, 0, &owed, false),
        [(WIDGETS.to_owned(), Hold::AsIs(SETTLED_OWED.into()))]
    );
    assert!(
        record
            .holds(BUILD, 0, &Withheld::default(), false)
            .is_empty()
    );
    assert_eq!(
        record.holds(BUILD, 0, &owed, true),
        [
            ("dock".to_owned(), Hold::AsIs(SETTLED.into())),
            (WIDGETS.to_owned(), Hold::AsIs(SETTLED_OWED.into())),
        ]
    );
    assert_eq!(
        record.holds(BUILD, 0, &Withheld::default(), true),
        [
            ("dock".to_owned(), Hold::AsIs(SETTLED.into())),
            (WIDGETS.to_owned(), Hold::AsIs(SETTLED.into())),
        ]
    );
}

/// The follow-up's own tries put off an automatic pass that reaches the same keys, and the
/// automatic pass's tries put off what it owes; each item's pause is its own ([`RETRY`]), and
/// a long run of tries is only a longer pause.
#[test]
fn the_follow_ups_pauses_put_off_an_automatic_pass_that_reaches_its_keys() {
    let tried = |item: &str, tries: u32, at: u64| Failed {
        item: item.to_owned(),
        build: BUILD.to_owned(),
        tries,
        at,
    };
    let later = |item: &str| (item.to_owned(), Hold::Later(LATER.into()));
    let now = RETRY.after(2).as_secs();
    let recent = now - 60;
    let record = Automatic {
        settled: vec!["settled".to_owned()],
        failed: vec![
            tried("paused here, due there", 1, recent),
            tried("tried often here", 5, 0),
            tried("due here, paused there", 2, 0),
            tried("due here, owed nothing", 1, 0),
        ],
        waits: Vec::new(),
    };
    let owed = Withheld {
        items: [
            "settled",
            "paused here, due there",
            "due here, paused there",
            "paused there",
            "due here, owed nothing",
        ]
        .map(str::to_owned)
        .to_vec(),
        failed: vec![
            tried("paused here, due there", 2, 0),
            tried("due here, paused there", 1, recent),
            tried("paused there", 1, recent),
            tried("settled", 2, recent),
            tried("no longer named", 1, recent),
        ],
    };
    assert_eq!(
        record.holds(BUILD, now, &owed, false),
        [
            ("settled".to_owned(), Hold::AsIs(SETTLED_OWED.into())),
            later("paused here, due there"),
            later("tried often here"),
            later("due here, paused there"),
            later("paused there"),
        ]
    );
    assert_eq!(
        record.holds(BUILD, now, &Withheld::default(), false),
        [later("paused here, due there"), later("tried often here")]
    );
    let often = RETRY.after(5).as_secs();
    assert!(
        put_off(&record.failed, BUILD, often)
            .iter()
            .all(|(item, _)| item != "tried often here"),
        "five tries are a longer pause, never a hold"
    );
}

/// A setting that changed after setup made it is set once more by the next pass, since
/// nothing says who changed it; changed again after that, it is left as it is by every later
/// pass, a new build's included, and its row says it changed. Before, the first change was
/// left for good.
#[test]
fn a_setting_changed_after_setup_made_it_is_set_again_once_then_left() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let widgets = wanted()[1].0.clone();
    let mut host = Fake {
        failing: vec![wanted()[0].0.clone()],
        ..Fake::default()
    };
    let writes = |host: &Fake| {
        host.writes
            .iter()
            .filter(|target| **target == widgets)
            .count()
    };
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    assert!(host.values.contains_key(&widgets));
    host.values.remove(&widgets);
    let again = automatic_pass(&mut host, root.path(), BUILD, at(1)).expect("set again");
    let row = again
        .rows
        .iter()
        .find(|row| row.name == widgets.describe())
        .unwrap();
    assert_eq!(row.status, "applied", "{}", again.to_text());
    assert_eq!(writes(&host), 2, "{:?}", host.events);
    assert!(host.values.contains_key(&widgets));
    host.values.remove(&widgets);
    for minute in 2..30 {
        let report = automatic_pass(&mut host, root.path(), BUILD, at(minute)).expect("retry");
        let row = report
            .rows
            .iter()
            .find(|row| row.name == widgets.describe())
            .unwrap();
        assert_eq!(row.status, CHANGED, "minute {minute}: {}", report.to_text());
        assert!(row.detail.ends_with(initial::kept::KEPT), "{}", row.detail);
        assert_eq!(
            report.changed_since(),
            [widgets.describe().as_str()],
            "the fact for later names the row that changed"
        );
    }
    assert_eq!(writes(&host), 2, "{:?}", host.events);
    assert!(!host.values.contains_key(&widgets));
    host.failing.clear();
    let report = automatic_pass(&mut host, root.path(), "1.0.1+gbbbb", at(31)).expect("done");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert!(first_run_complete(root.path()).unwrap());
    assert!(!host.values.contains_key(&widgets));
}

#[test]
fn a_failure_that_clears_gets_its_second_try_and_setup_completes() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake {
        fail_once: true,
        ..Fake::default()
    };
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    assert!(!first_run_complete(root.path()).unwrap());
    let changes = host.changes();
    for minute in 1..10 {
        automatic_pass(&mut host, root.path(), BUILD, at(minute)).expect("retry");
    }
    assert_eq!(host.changes(), changes, "{:?}", host.events);
    let report = automatic_pass(&mut host, root.path(), BUILD, at(10)).expect("second try");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert!(first_run_complete(root.path()).unwrap());
}

#[test]
fn a_held_retry_that_dies_part_way_leaves_nothing_to_relaunch() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake {
        failing: vec![wanted()[0].0.clone()],
        ..Fake::default()
    };
    automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
    assert_eq!(host.restarts, ["Dock"]);
    host.dies_in_preferences = true;
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        automatic_pass(&mut host, root.path(), BUILD, at(1))
    }));
    assert!(died.is_err(), "the pass died in its preference group");
    assert!(
        pending_refresh(root.path()).unwrap().is_empty(),
        "{:?}",
        pending_refresh(root.path())
    );
    host.dies_in_preferences = false;
    automatic_pass(&mut host, root.path(), BUILD, at(2)).expect("the next process");
    assert_eq!(host.restarts, ["Dock"], "{:?}", host.events);
}

#[test]
fn a_pass_that_ends_after_its_items_wrote_leaves_their_relaunch_to_them() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake {
        dies_after_item_writes: true,
        ..Fake::default()
    };
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(&mut host, root.path(), Action::Apply)
    }));
    assert!(died.is_err(), "the pass died after its items' writes");
    assert!(
        pending_refresh(root.path()).unwrap().is_empty(),
        "{:?}",
        pending_refresh(root.path())
    );
    host.dies_after_item_writes = false;
    let report = execute(&mut host, root.path(), Action::Apply).expect("the next process");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert!(host.restarts.is_empty(), "{:?}", host.events);
}

#[test]
fn a_pass_that_ends_in_its_rollback_relaunches_their_readers_on_the_next() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake {
        failing: vec![dock.clone()],
        dies_in_doctor_writes: true,
        ..Fake::default()
    };
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(&mut host, root.path(), Action::Apply)
    }));
    assert!(died.is_err(), "the pass died in its rollback");
    assert_eq!(pending_refresh(root.path()).unwrap(), [dock]);
    assert!(host.restarts.is_empty());
    host.dies_in_doctor_writes = false;
    host.failing.clear();
    let report = execute(&mut host, root.path(), Action::Apply).expect("the next process");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert_eq!(host.restarts, ["Dock"], "{:?}", host.events);
    assert!(pending_refresh(root.path()).unwrap().is_empty());
}

#[test]
fn a_recorded_key_an_item_rewrites_and_verifies_is_not_relaunched_for_again() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    save_pending_refresh(root.path(), std::slice::from_ref(&dock)).unwrap();
    let mut host = Fake::default();
    let report = execute(&mut host, root.path(), Action::Apply).expect("apply");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert!(host.writes.contains(&dock));
    assert!(host.restarts.is_empty(), "{:?}", host.events);
    assert!(pending_refresh(root.path()).unwrap().is_empty());
}

#[test]
fn a_refused_journal_line_never_keeps_an_original_from_coming_back() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake {
        failing: vec![dock.clone()],
        journal_refuses: true,
        ..Fake::default()
    };
    let report = automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("failed pass");
    assert!(!host.values.contains_key(&dock), "rolled back anyway");
    assert_eq!(host.restarts, ["Dock"], "relaunched anyway");
    let refused = |report: &Report| {
        report
            .rows
            .iter()
            .filter(|row| row.name == "Change journal" && row.status == "error")
            .map(|row| row.detail.clone())
            .collect::<Vec<_>>()
            .join("; ")
    };
    let said = refused(&report);
    assert!(
        said.contains(&restore_id(&dock)) && said.contains("doctor.relaunch.Dock"),
        "{}",
        report.to_text()
    );
    let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
    assert!(
        host.values.is_empty(),
        "every original is back: {:?}",
        host.values
    );
    assert_eq!(host.restarts, ["Dock", "Dock"]);
    assert!(
        refused(&report).contains("doctor.restore.com.apple.WindowManager"),
        "{}",
        report.to_text()
    );
}

#[test]
fn a_rollback_with_nothing_to_put_back_relaunches_nothing() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let dock = wanted()[0].0.clone();
    let mut host = Fake {
        written_back: vec![dock.clone()],
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).expect("failed pass");
    assert!(report.needs_attention());
    assert!(!host.values.contains_key(&dock));
    assert!(host.restarts.is_empty(), "{:?}", host.events);
    assert_eq!(
        host.writes.iter().filter(|target| **target == dock).count(),
        1,
        "the apply's write, and no rollback write"
    );
}

#[test]
fn one_failing_item_leaves_the_applied_ones_applied_and_the_next_pass_changes_nothing() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let (dock, widgets) = (wanted()[0].0.clone(), wanted()[1].0.clone());
    let mut host = Fake {
        blocked: vec![dock.clone()],
        ..Fake::default()
    };
    let first = execute(&mut host, root.path(), Action::Apply).expect("first pass");
    assert!(
        first.needs_attention(),
        "the blocked row still needs the person"
    );
    assert_eq!(
        host.writes,
        std::slice::from_ref(&widgets),
        "{:?}",
        host.events
    );
    assert!(host.values.contains_key(&widgets));
    assert!(
        host.restarts.is_empty(),
        "nothing was rolled back to reload"
    );
    let changes = host.changes();
    for _ in 0..3 {
        let report = execute(&mut host, root.path(), Action::Apply).expect("next pass");
        assert_eq!(
            report
                .rows
                .iter()
                .find(|row| row.name == widgets.describe())
                .map(|row| row.status.as_str()),
            Some("ok")
        );
    }
    assert_eq!(host.changes(), changes, "{:?}", host.events);
    assert!(host.values.contains_key(&widgets));
    host.blocked.clear();
    let report = execute(&mut host, root.path(), Action::Apply).expect("unblocked");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert_eq!(&host.writes[1..], [dock]);
    assert!(first_run_complete(root.path()).unwrap());
}

#[test]
fn every_rollback_restore_write_and_relaunch_is_journaled_before_it_is_made() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake {
        fail_once: true,
        ..Fake::default()
    };
    execute(&mut host, root.path(), Action::Apply).expect("failed pass");
    execute(&mut host, root.path(), Action::Apply).expect("retry");
    execute(&mut host, root.path(), Action::Restore).expect("restore");
    assert!(host.restarts.len() >= 2, "{:?}", host.events);
    assert!(host.writes.len() >= 4, "{:?}", host.events);
    for (at, event) in host.events.iter().enumerate() {
        let wanted = if let Some(target) = event.strip_prefix("write ") {
            let target = wanted()
                .into_iter()
                .map(|(target, _)| target)
                .find(|t| t.describe() == target)
                .unwrap();
            vec![host.item(&target), restore_id(&target)]
        } else if let Some(process) = event.strip_prefix("restart ") {
            vec![format!("doctor.relaunch.{process}")]
        } else {
            continue;
        };
        let line = at
            .checked_sub(1)
            .and_then(|before| host.events[before].strip_prefix("journal "));
        assert!(
            line.is_some_and(|id| wanted.iter().any(|want| want == id)),
            "{event} at {at} has no journal line before it: {:?}",
            host.events
        );
    }
    let restores: Vec<_> = host
        .lines
        .iter()
        .filter(|(id, ..)| id.starts_with("doctor.restore."))
        .collect();
    assert!(restores.len() >= 2, "{:?}", host.lines);
    for (id, from, to, revert) in restores {
        let revert = revert.as_deref().unwrap_or_default();
        assert!(
            revert.starts_with("/usr/bin/defaults write ")
                || revert.starts_with("/usr/bin/defaults delete "),
            "{id}: {revert}"
        );
        assert!(
            from.ends_with(" = true") || from.ends_with(" = false"),
            "{id}: {from}"
        );
        assert!(to.ends_with(" = absent"), "{id}: {to}");
    }
}

#[test]
fn blocked_accounts_keep_preference_progress_but_not_completion() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake {
        accounts_blocked: true,
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).expect("blocked");
    assert!(report.needs_attention());
    assert!(!first_run_complete(root.path()).expect("blocked accounts remain pending"));
    assert!(!host.values.is_empty());
    let writes = host.writes.len();
    host.accounts_blocked = false;
    assert!(
        !execute(&mut host, root.path(), Action::Apply)
            .expect("permission retry")
            .needs_attention()
    );
    assert_eq!(host.writes.len(), writes);
    assert!(first_run_complete(root.path()).expect("fully verified"));
}

#[test]
fn failed_activation_is_reported_and_retried_without_preference_writes() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake::default();
    execute(&mut host, root.path(), Action::Apply).expect("initial apply");
    save_pending_refresh(root.path(), &[wanted()[0].0.clone()]).expect("interrupted activation");
    assert!(
        execute(&mut host, root.path(), Action::Check)
            .expect("pending check")
            .needs_attention()
    );
    let writes = host.writes.len();
    host.fail_restart_once = true;
    assert!(
        execute(&mut host, root.path(), Action::Apply)
            .expect("failed activation")
            .needs_attention()
    );
    assert!(root.path().join("pending-refresh.json").exists());
    assert!(
        !execute(&mut host, root.path(), Action::Apply)
            .expect("retry activation")
            .needs_attention()
    );
    assert_eq!(host.writes.len(), writes);
    assert!(!root.path().join("pending-refresh.json").exists());
}

/// Clearing a record (the pending activation, the waiting restore, the withheld
/// preferences) flushes its directory once the record is gone, as writing one does, so a
/// crash cannot bring it back. NEGATIVE CONTROL: clearing a record already gone flushes
/// nothing.
#[test]
fn a_cleared_record_s_removal_is_flushed() {
    type Clear = fn(&Path) -> Result<(), String>;
    let clears: [(&str, Clear); 3] = [
        ("pending-refresh.json", clear_pending_refresh),
        (RESTORE_WAITING, |root| save_restore_waiting(root, &[])),
        (WITHHELD_RECORD, |root| {
            save_withheld_reading_revision(root, &Withheld::default(), || {
                unreachable!("an empty record is removed without its revision")
            })
        }),
    ];
    for (name, clear) in clears {
        let root = aterm_tempfile::tempdir().expect("scratch");
        std::fs::write(root.path().join(name), b"{}").expect("record");
        let recording = aterm_durable::seam::Recording::start();
        clear(root.path()).expect("cleared");
        assert!(!root.path().join(name).exists(), "{name}");
        assert_eq!(
            recording.take(),
            [aterm_durable::Flush::Durable],
            "{name}: its directory is flushed once it is gone"
        );
        clear(root.path()).expect("already clear");
        assert_eq!(recording.take(), [], "{name}: nothing to flush");
    }
}

#[test]
fn read_only_and_bad_recovery_records_never_write_preferences() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut host = Fake::default();
    let report = execute(&mut host, root.path(), Action::Check).expect("check");
    assert!(report.needs_attention());
    assert!(host.writes.is_empty());
    assert!(!report.backup.exists());
    std::fs::write(&report.backup, b"broken").expect("bad backup");
    assert!(execute(&mut host, root.path(), Action::Apply).is_err());
    assert!(host.writes.is_empty());
    std::fs::write(root.path().join("pending-refresh.json"), b"broken").expect("bad pending");
    assert!(execute(&mut host, root.path(), Action::Check).is_err());
    assert!(execute(&mut host, root.path(), Action::Restore).is_err());
    assert!(host.writes.is_empty());

    let record = root
        .path()
        .join("new-state")
        .join("doctor")
        .join("backup.json");
    let value = json!({"profile": PROFILE, "keys": []});
    let mut attempted = Vec::new();
    let refused = write_json_with_flush(&record, &value, |directory, _| {
        attempted.push(directory.to_path_buf());
        if directory == root.path() {
            Err(std::io::Error::other("injected ancestor flush refusal"))
        } else {
            Ok(())
        }
    });
    assert!(refused.is_err(), "a visible rename is not a durable backup");
    assert_eq!(attempted.last().map(PathBuf::as_path), Some(root.path()));
    assert_eq!(
        aterm_json::from_slice::<Value>(&std::fs::read(&record).unwrap()).unwrap(),
        value,
        "failure after publication is retained as failure"
    );
    let mut reached_root = false;
    write_json_with_flush(&record, &value, |directory, _| {
        reached_root |= directory.parent().is_none();
        Ok(())
    })
    .expect("a retry discharges the complete namespace chain");
    assert!(
        reached_root,
        "existing ancestor entries also need publication"
    );
}

#[test]
fn a_published_name_is_flushed_again_only_once_it_or_its_directory_is_another() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let doctor = root.path().join("doctor");
    let record = doctor.join("first-run.json");
    std::fs::create_dir(&doctor).expect("doctor dir");
    std::fs::write(&record, b"{}").expect("record");
    let flushes = aterm_durable::seam::Recording::start();
    flush_record_with(&record, durable_name).expect("first");
    assert!(
        flushes.take().len() >= 3,
        "the record's, the doctor dir's and the scratch dir's names are new"
    );
    flush_record_with(&record, durable_name).expect("again");
    assert_eq!(flushes.take(), [], "every name on the way is as published");
    std::fs::write(doctor.join("next"), b"{}").expect("next");
    std::fs::rename(doctor.join("next"), &record).expect("rewrite");
    flush_record_with(&record, durable_name).expect("rewritten");
    assert_eq!(
        flushes.take().len(),
        1,
        "a record written again: its directory"
    );
    std::fs::rename(&doctor, root.path().join("old")).expect("move away");
    std::fs::create_dir(&doctor).expect("new doctor dir");
    std::fs::rename(root.path().join("old").join("first-run.json"), &record)
        .expect("the same record, in the new directory");
    flush_record_with(&record, durable_name).expect("replaced");
    assert_eq!(
        flushes.take().len(),
        2,
        "a directory put in place of another: it, and the scratch dir naming it"
    );
}

#[test]
fn recovery_restores_plist_types_and_old_marker_does_not_skip_accounts() {
    let mut host = Fake::default();
    let target = wanted()[0].0.clone();
    host.values.insert(target.clone(), PValue::Bool(true));
    restore_one(&mut host, &target, Some(&PValue::Int(1)), &mut Vec::new())
        .expect("typed original");
    assert_eq!(host.values[&target], PValue::Int(1));
    let root = aterm_tempfile::tempdir().expect("scratch");
    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile": PROFILE, "state": "applied"}),
    )
    .expect("old marker");
    assert!(!first_run_complete(root.path()).expect("not verified"));
}

#[test]
fn automatic_permission_retries_do_not_repeat_on_every_launch() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let then = std::time::SystemTime::now();
    assert!(automatic_retry_due(root.path(), Some(false), then).expect("first attempt"));
    write_json(
        &root.path().join("automatic-attempt.json"),
        &json!({"profile": PROFILE, "accessibility_ready": false, "at": epoch_secs(then)}),
    )
    .expect("attempt");
    let now = then + std::time::Duration::from_secs(1);
    assert!(!automatic_retry_due(root.path(), Some(false), now).expect("same permission"));
    assert!(automatic_retry_due(root.path(), Some(true), now).expect("new permission"));
    let later = now + std::time::Duration::from_secs(61);
    assert!(automatic_retry_due(root.path(), Some(false), later).expect("after cooldown"));
    let back = then - std::time::Duration::from_secs(3_600);
    assert!(automatic_retry_due(root.path(), Some(false), back).expect("clock set back"));
}

/// A sign-out's first look again, a minute after a pass dated `S` at a fraction past a
/// second, comes due at `S + 60` and passes the automatic gate at that very second, as the
/// park's timer wakes it: the gate and the Automatic record time from the same second. Before,
/// the gate timed from the attempt file's write, later by that fraction, so the look was held
/// back (`Ok(None)`) and its row read blocked. NEGATIVE CONTROL: a second earlier, the gate
/// holds the pass.
#[test]
fn a_first_look_again_passes_the_automatic_gate_at_its_due_second() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let mut selection = Selection::none();
    selection.set(Setting::Messages, true);
    authorize_automatic_setup_at(root.path(), selection).unwrap();
    let revision = initial::revision_at(root.path()).unwrap();
    let start = at(0) + std::time::Duration::from_millis(500);
    let pass = |now: std::time::SystemTime| {
        let mut host = Fake {
            accounts_wait: true,
            ..Fake::default()
        };
        apply_part_with(
            root.path(),
            &Inert::default(),
            &mut host,
            BUILD,
            now,
            Part::Accounts,
        )
        .expect("pass")
    };
    assert!(pass(start).is_some(), "the first pass runs");
    // The attempt file as the live clock writes it: just after the pass's own second began.
    std::fs::File::options()
        .write(true)
        .open(root.path().join("automatic-attempt.json"))
        .and_then(|file| file.set_modified(start))
        .expect("the attempt file's time");
    let due = mac_settings_retry_ahead(root.path(), revision, BUILD, epoch_secs(start))
        .unwrap()
        .expect("the sign-out's look has a timer");
    assert_eq!(due, epoch_secs(start) + 60);
    let at_second = |secs: u64| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    assert!(
        pass(at_second(due - 1)).is_none(),
        "NEGATIVE CONTROL: a second early"
    );
    let looked = pass(at_second(due)).expect("the look runs at its due second");
    let row = looked
        .rows
        .iter()
        .find(|row| row.name == "Account sign-out")
        .expect("the sign-out's row");
    assert_eq!(row.status, initial::WAITING, "{}", row.detail);
}

#[test]
fn a_real_home_may_change_the_profile_and_a_scratch_home_never_may() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let account = root.path().join("account");
    let scratch = root.path().join("scratch");
    let link = root.path().join("link");
    std::fs::create_dir(&account).expect("account home");
    std::fs::create_dir(&scratch).expect("scratch home");
    std::os::unix::fs::symlink(&account, &link).expect("a link to the account home");
    let own = || -> Result<PathBuf, String> { Ok(account.clone()) };
    for home in [&account, &link] {
        assert_eq!(
            scope_of(false, 501, Some(home), own),
            PersonScope::Person,
            "{}",
            home.display()
        );
        assert_eq!(scope_of(false, 501, Some(home), own).lasting(), None);
    }
    for scope in [
        scope_of(false, 501, Some(&scratch), own),
        scope_of(true, 501, Some(&account), own),
        scope_of(false, 0, Some(&account), own),
    ] {
        assert!(matches!(scope, PersonScope::Refused(_)), "{scope:?}");
        assert!(scope.lasting().is_some());
    }
    let unread = || -> Result<PathBuf, String> { Err("getpwuid_r: 5".into()) };
    for scope in [
        scope_of(false, 501, None, own),
        scope_of(false, 501, Some(&account), unread),
        scope_of(false, 501, Some(&root.path().join("missing")), own),
    ] {
        assert!(matches!(scope, PersonScope::Unresolved(_)), "{scope:?}");
        assert_eq!(scope.lasting(), None);
    }
}

fn deferred_widgets_restore() -> (aterm_tempfile::TempDir, Fake) {
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let root = aterm_tempfile::tempdir().unwrap();
    let mut host = Fake {
        selected: Some(widgets),
        notification_center: true,
        full_disk_access: Some(true),
        values: BTreeMap::from([(nc_widgets(), populated())]),
        ..Fake::default()
    };
    let report = execute(&mut host, root.path(), Action::Apply).unwrap();
    assert!(!report.needs_attention(), "{}", report.to_text());
    let shows = |host: &Fake, value: &PValue| {
        host.values
            .get(&nc_widgets())
            .is_some_and(|now| now.same(value))
    };
    assert!(shows(&host, &cleared()));
    host.full_disk_access = Some(false);
    let report = execute(&mut host, root.path(), Action::Restore).unwrap();
    assert!(
        report
            .restore
            .as_ref()
            .unwrap()
            .outcome
            .rows
            .contains(&(Some(Setting::Widgets), PutBack::Waits)),
        "{}",
        report.to_text()
    );
    assert_eq!(restore_waiting(root.path()).unwrap(), [nc_widgets()]);
    assert!(shows(&host, &cleared()));
    (root, host)
}

#[test]
fn init_withheld_widgets_are_applied_once_after_full_disk_access() {
    use aterm_machines::init::Held as _;

    let root = aterm_tempfile::tempdir().unwrap();
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let mut host = Fake {
        selected: Some(widgets),
        notification_center: true,
        full_disk_access: Some(false),
        values: BTreeMap::from([(nc_widgets(), populated())]),
        ..Fake::default()
    };
    let hold = init_hold_at(
        root.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        false,
        Some(&|_| panic!("uncontended hold")),
        initial::complete_explicit_workstation_plan_for_test,
    )
    .unwrap();
    hold.withheld(&[WIDGETS.into()]).unwrap();
    Box::new(hold).verified().unwrap();
    let _lock = HeldLock::take(root.path()).unwrap();
    let now = std::time::SystemTime::now();
    assert!(
        follow_up_at(root.path(), &mut host, BUILD, now)
            .unwrap()
            .is_none()
    );
    assert!(host.writes.is_empty());
    assert!(host.values[&nc_widgets()].same(&populated()));
    host.full_disk_access = Some(true);
    let report = follow_up_at(root.path(), &mut host, BUILD, now)
        .unwrap()
        .expect("the whole init retained its withheld Notification Center item");
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert!(host.values[&nc_widgets()].same(&cleared()));
    assert_eq!(host.writes, [nc_widgets()]);
    assert_eq!(
        host.relaunches("NotificationCenter")
            + host
                .restarts
                .iter()
                .filter(|name| *name == "NotificationCenter")
                .count(),
        1
    );
    assert!(load_withheld(root.path()).unwrap().items.is_empty());
    let effects = (host.writes.len(), host.restarts.len(), host.lines.len());
    assert!(
        follow_up_at(root.path(), &mut host, BUILD, now)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        (host.writes.len(), host.restarts.len(), host.lines.len()),
        effects
    );
}

#[test]
fn init_withheld_record_failure_leaves_the_admission_unverified() {
    use aterm_machines::init::Held as _;

    let root = aterm_tempfile::tempdir().unwrap();
    let hold = init_hold_at(
        root.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        false,
        Some(&|_| panic!("uncontended hold")),
        initial::complete_explicit_workstation_plan_for_test,
    )
    .unwrap();
    let before = initial::observation_at(root.path()).unwrap();
    std::fs::create_dir(root.path().join(WITHHELD_RECORD)).unwrap();
    let result = hold.withheld(&[WIDGETS.into()]);
    assert_eq!(initial::observation_at(root.path()).unwrap(), before);
    assert!(
        result.is_err(),
        "the desktop lane cannot start without recording its withheld item"
    );
    drop(hold);
    assert!(!first_run_complete(root.path()).unwrap());
}

#[test]
fn init_withheld_partial_pass_preserves_existing_debt_and_restore() {
    use aterm_machines::init::Held as _;

    for older_debt in [false, true] {
        let (root, mut host) = deferred_widgets_restore();
        let old = Withheld {
            items: vec![WIDGETS.into()],
            failed: vec![Failed {
                item: WIDGETS.into(),
                build: BUILD.into(),
                tries: 1,
                at: 1,
            }],
        };
        if older_debt {
            save_withheld(root.path(), &old).unwrap();
        }
        let withheld = std::fs::read(root.path().join(WITHHELD_RECORD)).ok();
        let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
        let before = initial::observation_at(root.path()).unwrap();
        let hold = init_hold_at(
            root.path(),
            aterm_machines::init::Pass::Part,
            false,
            Some(&|_| panic!("uncontended hold")),
            initial::complete_explicit_workstation_plan_for_test,
        )
        .unwrap();
        hold.withheld(&[WIDGETS.into()]).unwrap();
        Box::new(hold).verified().unwrap();
        assert_eq!(
            std::fs::read(root.path().join(WITHHELD_RECORD)).ok(),
            withheld
        );
        assert_eq!(
            std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
            waiting
        );
        assert_eq!(initial::observation_at(root.path()).unwrap(), before);
        host.full_disk_access = Some(true);
        let writes = host.writes.len();
        let report = follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now())
            .unwrap()
            .expect("the earlier Restore remains owed");
        assert_eq!(
            report.restore.unwrap().outcome.rows,
            [(Some(Setting::Widgets), PutBack::Back)]
        );
        assert!(host.values[&nc_widgets()].same(&populated()));
        assert_eq!(host.writes.len(), writes + 1);
        assert!(restore_waiting(root.path()).unwrap().is_empty());
    }
}

/// A Restore that could not put Notification Center's widget list back for want of Full
/// Disk Access puts it back by itself once aterm has the grant (design §3.4), with no
/// press: nothing before the grant, once after it, its reader relaunched, and a calm
/// outcome for the page.
#[test]
fn a_restore_waiting_for_full_disk_access_puts_back_once_granted() {
    let (root, mut host) = deferred_widgets_restore();
    let now = std::time::SystemTime::now();
    let writes = host.writes.len();
    assert!(owed_with_full_disk_access(root.path(), || Some(false)).is_empty());
    assert!(
        follow_up_at(root.path(), &mut host, "1.0.0+gtest", now)
            .unwrap()
            .is_none()
    );
    assert_eq!(host.writes.len(), writes, "nothing before the grant");
    host.full_disk_access = Some(true);
    assert_eq!(
        owed_with_full_disk_access(root.path(), || Some(true)),
        [Setting::Widgets]
    );
    let relaunches = host.relaunches("NotificationCenter")
        + host
            .restarts
            .iter()
            .filter(|p| *p == "NotificationCenter")
            .count();
    let report = follow_up_at(root.path(), &mut host, "1.0.0+gtest", now)
        .unwrap()
        .expect("the waiting put-back runs once granted");
    assert_eq!(
        report.restore.unwrap().outcome.rows,
        [(Some(Setting::Widgets), PutBack::Back)]
    );
    assert!(host.values[&nc_widgets()].same(&populated()));
    assert_eq!(
        host.relaunches("NotificationCenter")
            + host
                .restarts
                .iter()
                .filter(|p| *p == "NotificationCenter")
                .count(),
        relaunches + 1
    );
    assert!(restore_waiting(root.path()).unwrap().is_empty());
    let writes = host.writes.len();
    assert!(
        follow_up_at(root.path(), &mut host, "1.0.0+gtest", now)
            .unwrap()
            .is_none()
    );
    assert_eq!(host.writes.len(), writes, "once");
}

fn missing_backup_keeps_restore_waiting(explicit: bool) {
    let (root, mut host) = deferred_widgets_restore();
    let _lock = HeldLock::take(root.path()).unwrap();
    let backup = root.path().join("original-preferences.json");
    let held_backup = root.path().join("original-preferences.saved");
    let originals = std::fs::read(&backup).unwrap();
    let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
    std::fs::rename(&backup, &held_backup).unwrap();
    host.full_disk_access = Some(true);
    let effects = (host.writes.len(), host.restarts.len(), host.lines.len());
    let now = std::time::SystemTime::now();
    let result = if explicit {
        run_mutation(&mut host, root.path(), Action::Restore).map(|_| ())
    } else {
        follow_up_at(root.path(), &mut host, BUILD, now).map(|_| ())
    };
    assert_eq!(
        (host.writes.len(), host.restarts.len(), host.lines.len()),
        effects,
        "unavailable originals permit no preference write, relaunch or journal entry"
    );
    assert!(host.values[&nc_widgets()].same(&cleared()));
    assert!(
        result.is_err(),
        "an owed original cannot be treated as absent"
    );
    assert_eq!(restore_waiting(root.path()).unwrap(), [nc_widgets()]);
    assert_eq!(
        std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
        waiting,
        "the missing backup must leave the original put-back owed"
    );
    assert_eq!(std::fs::read(&held_backup).unwrap(), originals);
    assert!(!backup.exists());

    std::fs::rename(&held_backup, &backup).unwrap();
    let restored = follow_up_at(root.path(), &mut host, BUILD, now)
        .unwrap()
        .expect("repairing the backup permits the original put-back");
    assert_eq!(
        restored.restore.unwrap().outcome.rows,
        [(Some(Setting::Widgets), PutBack::Back)]
    );
    assert!(host.values[&nc_widgets()].same(&populated()));
    assert_eq!(host.writes.len(), effects.0 + 1);
    assert_eq!(host.restarts.len(), effects.1 + 1);
    assert!(restore_waiting(root.path()).unwrap().is_empty());
    assert_eq!(std::fs::read(&backup).unwrap(), originals);
    let effects = (host.writes.len(), host.restarts.len(), host.lines.len());
    assert!(
        follow_up_at(root.path(), &mut host, BUILD, now)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        (host.writes.len(), host.restarts.len(), host.lines.len()),
        effects,
        "the repaired put-back runs once"
    );
}

#[test]
fn restore_backup_debt_missing_backup_keeps_the_follow_up_owed() {
    missing_backup_keeps_restore_waiting(false);
}

#[test]
fn restore_backup_debt_missing_backup_keeps_a_repeated_restore_owed() {
    missing_backup_keeps_restore_waiting(true);
}

#[test]
fn restore_backup_debt_no_backup_and_no_prior_changes_is_valid() {
    let root = aterm_tempfile::tempdir().unwrap();
    let _lock = HeldLock::take(root.path()).unwrap();
    let unowned = Target::new("com.apple.dock", "orientation");
    let mut host = Fake {
        full_disk_access: Some(true),
        values: BTreeMap::from([(unowned.clone(), PValue::Str("left".into()))]),
        ..Fake::default()
    };
    let report = run_mutation(&mut host, root.path(), Action::Restore).unwrap();
    assert!(!report.needs_attention(), "{}", report.to_text());
    assert!(!report.backup.exists());
    assert!(host.writes.is_empty());
    assert!(host.restarts.is_empty());
    assert!(host.lines.is_empty());
    assert_eq!(host.values.len(), 1);
    assert_eq!(host.values[&unowned], PValue::Str("left".into()));
    assert!(restore_waiting(root.path()).unwrap().is_empty());
    assert!(
        follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now())
            .unwrap()
            .is_none()
    );
}

fn init_admission_preserves_pending_original(missing: bool) {
    let (root, _host) = deferred_widgets_restore();
    let backup = root.path().join("original-preferences.json");
    let originals = std::fs::read(&backup).unwrap();
    let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
    if missing {
        std::fs::rename(&backup, root.path().join("original-preferences.saved")).unwrap();
    } else {
        let mut saved = load_backup(&backup).unwrap();
        saved.retain(|saved| saved.target != nc_widgets());
        assert!(!saved.is_empty(), "the other valid originals remain");
        save_backup(&backup, &saved).unwrap();
    }
    let damaged = std::fs::read(&backup).ok();
    let before = initial::observation_at(root.path()).unwrap();
    let admitted = init_hold_at(
        root.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        false,
        Some(&|line| panic!("no other pass holds these records: {line}")),
        initial::complete_explicit_workstation_plan_for_test,
    );
    let refused = admitted.is_err();
    drop(admitted);
    assert!(
        refused,
        "init must validate its pending original before admission"
    );
    assert_eq!(initial::observation_at(root.path()).unwrap(), before);
    assert_eq!(
        std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
        waiting
    );
    assert_eq!(std::fs::read(&backup).ok(), damaged);
    assert_eq!(
        initial::restore_owed(root.path(), Some(Setting::Widgets)),
        initial::RestoreOwed::PutBack
    );
    assert!(
        HeldLock::try_take(root.path()).unwrap().is_some(),
        "refusal releases the profile lock"
    );

    std::fs::write(&backup, &originals).unwrap();
    let held = init_hold_at(
        root.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        false,
        Some(&|line| panic!("{line}")),
        initial::complete_explicit_workstation_plan_for_test,
    )
    .expect("repairing the original permits the real init admission");
    assert_ne!(initial::observation_at(root.path()).unwrap(), before);
    assert_eq!(std::fs::read(&backup).unwrap(), originals);
    drop(held);
}

#[test]
fn init_admission_missing_original_preserves_restore_debt() {
    init_admission_preserves_pending_original(true);
}

#[test]
fn init_admission_incomplete_original_preserves_restore_debt() {
    init_admission_preserves_pending_original(false);
}

#[test]
fn init_admission_fresh_and_partial_passes_need_no_pending_original() {
    let fresh = aterm_tempfile::tempdir().unwrap();
    let held = init_hold_at(
        fresh.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        false,
        Some(&|line| panic!("{line}")),
        initial::complete_explicit_workstation_plan_for_test,
    )
    .expect("a fresh profile has no original it still owes");
    assert!(!fresh.path().join("original-preferences.json").exists());
    drop(held);

    let (root, _host) = deferred_widgets_restore();
    std::fs::rename(
        root.path().join("original-preferences.json"),
        root.path().join("original-preferences.saved"),
    )
    .unwrap();
    let before = initial::observation_at(root.path()).unwrap();
    let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
    let held = init_hold_at(
        root.path(),
        aterm_machines::init::Pass::Part,
        false,
        Some(&|line| panic!("{line}")),
        initial::complete_explicit_workstation_plan_for_test,
    )
    .expect("a partial pass does not replace the Restore admission");
    assert_eq!(initial::observation_at(root.path()).unwrap(), before);
    assert_eq!(
        std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
        waiting
    );
    drop(held);
}

#[test]
fn restore_backup_debt_incomplete_originals_refuse_before_any_put_back() {
    for unknown in [false, true] {
        for explicit in [false, true] {
            let (root, mut host) = deferred_widgets_restore();
            let _lock = HeldLock::take(root.path()).unwrap();
            let backup = root.path().join("original-preferences.json");
            let mut saved = load_backup(&backup).unwrap();
            let mut owed =
                saved.remove(saved.iter().position(|s| s.target == nc_widgets()).unwrap());
            if unknown {
                owed.value = None;
                owed.granted = false;
                saved.insert(0, owed);
            }
            // This valid original would be put back first by a reverse walk. Validate
            // the missing or unknown owed original before even that independent write.
            let valid = saved.last().unwrap();
            assert_ne!(valid.target, nc_widgets());
            host.values.insert(valid.target.clone(), PValue::Bool(true));
            save_backup(&backup, &saved).unwrap();
            let damaged = std::fs::read(&backup).unwrap();
            let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
            let effects = (host.writes.len(), host.restarts.len(), host.lines.len());
            let values = host.values.clone();
            host.full_disk_access = Some(true);
            let result = if explicit {
                run_mutation(&mut host, root.path(), Action::Restore).map(|_| ())
            } else {
                follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now())
                    .map(|_| ())
            };
            assert!(result.is_err(), "unknown={unknown}, explicit={explicit}");
            assert_eq!(
                (host.writes.len(), host.restarts.len(), host.lines.len()),
                effects
            );
            assert_eq!(host.values, values);
            assert_eq!(std::fs::read(&backup).unwrap(), damaged);
            assert_eq!(
                std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
                waiting
            );
        }
    }
}

#[test]
fn restore_backup_debt_apply_cannot_recapture_a_changed_value_as_the_original() {
    let (root, mut host) = deferred_widgets_restore();
    let _lock = HeldLock::take(root.path()).unwrap();
    let backup = root.path().join("original-preferences.json");
    let held_backup = root.path().join("original-preferences.saved");
    let originals = std::fs::read(&backup).unwrap();
    let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
    std::fs::rename(&backup, &held_backup).unwrap();
    host.full_disk_access = Some(true);
    let external = PValue::Dict(vec![
        ("vers".into(), PValue::Int(1)),
        (
            "instances".into(),
            PValue::Array(vec![PValue::Data(vec![99])]),
        ),
    ]);
    host.values.insert(nc_widgets(), external.clone());
    let effects = (host.writes.len(), host.restarts.len(), host.lines.len());
    let captures = host.preference_passes;
    let before = initial::observation_at(root.path()).unwrap();
    assert!(run_mutation(&mut host, root.path(), Action::Apply).is_err());
    assert_eq!(initial::observation_at(root.path()).unwrap(), before);
    assert_eq!(
        host.preference_passes, captures,
        "validate before capturing replacements"
    );
    assert_eq!(
        (host.writes.len(), host.restarts.len(), host.lines.len()),
        effects
    );
    assert_eq!(host.values[&nc_widgets()], external);
    assert!(
        !backup.exists(),
        "Apply must not invent a replacement original"
    );
    assert_eq!(std::fs::read(&held_backup).unwrap(), originals);
    assert_eq!(
        std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
        waiting
    );

    std::fs::rename(&held_backup, &backup).unwrap();
    let applied = run_mutation(&mut host, root.path(), Action::Apply).unwrap();
    assert!(!applied.needs_attention(), "{}", applied.to_text());
    assert_eq!(std::fs::read(&backup).unwrap(), originals);
    let restored = run_mutation(&mut host, root.path(), Action::Restore).unwrap();
    assert!(!restored.needs_attention(), "{}", restored.to_text());
    assert!(host.values[&nc_widgets()].same(&populated()));
}

#[test]
fn restore_backup_debt_a_queued_choice_still_needs_its_original() {
    let (root, mut host) = deferred_widgets_restore();
    let choice = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
    assert!(choice.choose_group(&[Setting::Widgets]).unwrap().is_some());
    let _lock = HeldLock::take(root.path()).unwrap();
    assert_eq!(
        initial::restore_owed(root.path(), Some(Setting::Widgets)),
        initial::RestoreOwed::Wait
    );
    let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
    std::fs::rename(
        root.path().join("original-preferences.json"),
        root.path().join("original-preferences.saved"),
    )
    .unwrap();
    host.full_disk_access = Some(true);
    let effects = (host.writes.len(), host.restarts.len(), host.lines.len());
    assert!(follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now()).is_err());
    assert_eq!(
        (host.writes.len(), host.restarts.len(), host.lines.len()),
        effects
    );
    assert_eq!(
        std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
        waiting
    );
}

#[test]
fn restore_backup_debt_a_completed_new_choice_retires_the_old_original() {
    for explicit in [false, true] {
        let (root, mut host) = deferred_widgets_restore();
        // The Restore left Widgets as is; the person turns it on again on the page.
        let choice = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
        assert!(choice.choose_group(&[Setting::Widgets]).unwrap().is_some());
        let _lock = HeldLock::take(root.path()).unwrap();
        host.full_disk_access = Some(true);
        let applied = run_mutation(&mut host, root.path(), Action::Apply).unwrap();
        assert!(!applied.needs_attention(), "{}", applied.to_text());
        assert_eq!(
            initial::restore_owed(root.path(), Some(Setting::Widgets)),
            initial::RestoreOwed::LetGo
        );
        assert_eq!(restore_waiting(root.path()).unwrap(), [nc_widgets()]);
        std::fs::rename(
            root.path().join("original-preferences.json"),
            root.path().join("original-preferences.saved"),
        )
        .unwrap();
        let effects = (host.writes.len(), host.restarts.len(), host.lines.len());
        if explicit {
            let restored = run_mutation(&mut host, root.path(), Action::Restore).unwrap();
            assert!(!restored.needs_attention(), "{}", restored.to_text());
        } else {
            assert!(
                follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now())
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            (host.writes.len(), host.restarts.len(), host.lines.len()),
            effects
        );
        assert!(restore_waiting(root.path()).unwrap().is_empty());
        assert!(host.values[&nc_widgets()].same(&cleared()));
    }
}

#[test]
fn restore_backup_debt_observes_the_result_before_releasing_its_reservation() {
    use std::cell::Cell;

    struct Slot<'a> {
        root: &'a Path,
        held: &'a Cell<bool>,
    }
    impl Drop for Slot<'_> {
        fn drop(&mut self) {
            let _lock = HeldLock::take(self.root).expect("the profile lock goes before the slot");
            initial::save_explicit_workstation_plan(self.root, Selection::all().workstation())
                .unwrap();
            self.held.set(false);
        }
    }

    for missing in [false, true] {
        let (root, mut host) = deferred_widgets_restore();
        host.full_disk_access = Some(true);
        let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
        if missing {
            std::fs::rename(
                root.path().join("original-preferences.json"),
                root.path().join("original-preferences.saved"),
            )
            .unwrap();
        }
        let held = Cell::new(false);
        let (observed, slot) = follow_up_finished_observed_at(
            root.path(),
            || {
                held.set(true);
                Slot {
                    root: root.path(),
                    held: &held,
                }
            },
            || Ok(host),
            BUILD,
            std::time::SystemTime::now(),
        );
        assert!(slot.is_some() && held.get());
        assert!(HeldLock::try_take(root.path()).unwrap().is_some());
        assert_eq!(
            observed.observation,
            Some(initial::observation_at(root.path()).unwrap())
        );
        if missing {
            assert!(
                observed
                    .result
                    .as_ref()
                    .unwrap_err()
                    .starts_with(BACKUP_UNREAD)
            );
            assert_eq!(
                std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
                waiting
            );
        } else {
            assert_eq!(
                observed
                    .result
                    .unwrap()
                    .unwrap()
                    .restore
                    .unwrap()
                    .outcome
                    .rows,
                [(Some(Setting::Widgets), PutBack::Back)]
            );
            assert!(restore_waiting(root.path()).unwrap().is_empty());
        }
        let binding = observed.observation.unwrap();
        if missing {
            // The slot's later explicit admission must meet the same
            // original-preservation preflight as the operation it follows.
            std::fs::rename(
                root.path().join("original-preferences.saved"),
                root.path().join("original-preferences.json"),
            )
            .unwrap();
        }
        drop(slot);
        assert!(!held.get());
        assert_ne!(
            initial::observation_at(root.path()).unwrap(),
            binding,
            "a later choice cannot become this result's binding"
        );
    }
}

fn a_waiting_restore_survives_a_replacement_that_never_wrote(interrupted: bool) {
    let (root, mut host) = deferred_widgets_restore();
    let _lock = HeldLock::take(root.path()).unwrap();
    let waiting = std::fs::read(root.path().join(RESTORE_WAITING)).unwrap();
    let backup = std::fs::read(root.path().join("original-preferences.json")).unwrap();
    let effects = (host.writes.len(), host.restarts.len());
    host.full_disk_access = Some(true);
    if interrupted {
        host.dies_in_doctor_writes = true;
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_mutation(&mut host, root.path(), Action::Restore)
        }));
        assert!(
            stopped.is_err(),
            "the replacement ends before its first write"
        );
        host.dies_in_doctor_writes = false;
    } else {
        std::fs::write(root.path().join("pending-refresh.json"), b"broken").unwrap();
        assert!(run_mutation(&mut host, root.path(), Action::Restore).is_err());
        std::fs::remove_file(root.path().join("pending-refresh.json")).unwrap();
    }
    assert_eq!((host.writes.len(), host.restarts.len()), effects);
    assert!(host.values[&nc_widgets()].same(&cleared()));
    assert_eq!(
        std::fs::read(root.path().join("original-preferences.json")).unwrap(),
        backup
    );
    assert_eq!(
        restore_waiting(root.path()).unwrap(),
        [nc_widgets()],
        "a replacement that never wrote must keep the earlier put-back owed"
    );
    assert_eq!(
        std::fs::read(root.path().join(RESTORE_WAITING)).unwrap(),
        waiting
    );
    let now = std::time::SystemTime::now();
    let report = follow_up_at(root.path(), &mut host, BUILD, now)
        .unwrap()
        .expect("the original waiting Restore still follows the grant");
    assert_eq!(
        report.restore.unwrap().outcome.rows,
        [(Some(Setting::Widgets), PutBack::Back)]
    );
    assert!(host.values[&nc_widgets()].same(&populated()));
    assert_eq!(host.writes.len(), effects.0 + 1);
    assert_eq!(host.restarts.len(), effects.1 + 1);
    assert!(restore_waiting(root.path()).unwrap().is_empty());
    assert!(
        follow_up_at(root.path(), &mut host, BUILD, now)
            .unwrap()
            .is_none()
    );
    assert_eq!(host.writes.len(), effects.0 + 1, "the follow-up runs once");
}

#[test]
fn restore_waiting_survives_interruption_before_its_first_write() {
    a_waiting_restore_survives_a_replacement_that_never_wrote(true);
}

#[test]
fn restore_waiting_survives_bad_activation_preflight() {
    a_waiting_restore_survives_a_replacement_that_never_wrote(false);
}

#[test]
fn restore_waiting_reported_write_failure_still_needs_manual_retry() {
    let (root, mut host) = deferred_widgets_restore();
    let _lock = HeldLock::take(root.path()).unwrap();
    host.full_disk_access = Some(true);
    host.refuses.push(nc_widgets());
    let report = run_mutation(&mut host, root.path(), Action::Restore).unwrap();
    assert!(
        report
            .restore
            .unwrap()
            .outcome
            .rows
            .contains(&(Some(Setting::Widgets), PutBack::Failed))
    );
    assert!(restore_waiting(root.path()).unwrap().is_empty());
    host.refuses.clear();
    assert!(
        follow_up_at(root.path(), &mut host, BUILD, std::time::SystemTime::now())
            .unwrap()
            .is_none()
    );
    assert!(host.values[&nc_widgets()].same(&cleared()));
}

/// The follow-up the window starts as it records Full Disk Access granted
/// (`try_finish_withheld_preferences_unless_stopped`): after a Restore with nothing
/// waiting it measures nothing and takes no slot; after one that left the widget list waiting
/// for the grant it puts that back once, holding the slot, and the next grant's follow-up
/// finds nothing owed.
#[test]
fn a_grants_follow_up_puts_back_what_waits_once_and_starts_nothing_when_nothing_waits() {
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let now = std::time::SystemTime::now();
    let slots = std::cell::Cell::new(0_u8);
    let reserve = || slots.set(slots.get() + 1);
    let unbuilt = || -> Result<Fake, String> { panic!("nothing owed measures no grant") };
    for waits in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut host = Fake {
            selected: Some(widgets),
            notification_center: true,
            full_disk_access: Some(true),
            values: BTreeMap::from([(nc_widgets(), populated())]),
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).unwrap();
        assert!(!report.needs_attention(), "{}", report.to_text());
        host.full_disk_access = Some(!waits);
        execute(&mut host, root.path(), Action::Restore).unwrap();
        assert_eq!(restore_waiting(root.path()).unwrap().is_empty(), !waits);
        slots.set(0);
        if waits {
            host.full_disk_access = Some(true);
            let report = follow_up_finished_at(
                root.path(),
                reserve,
                || Ok(std::mem::take(&mut host)),
                BUILD,
                now,
            )
            .unwrap()
            .expect("the waiting put-back runs once granted");
            assert_eq!(
                report.restore.unwrap().outcome.rows,
                [(Some(Setting::Widgets), PutBack::Back)]
            );
            assert!(restore_waiting(root.path()).unwrap().is_empty());
        }
        assert!(
            follow_up_finished_at(root.path(), reserve, unbuilt, BUILD, now)
                .unwrap()
                .is_none(),
            "waits={waits}"
        );
        assert_eq!(slots.get(), u8::from(waits), "waits={waits}");
    }
}

/// A busy lease leaves recovery explicitly held, without constructing a host or capturing
/// its grant/time. A later attempt after the old ten-minute budget takes fresh facts and
/// puts back the same waiting Restore in this launch.
#[test]
fn a_grants_follow_up_waits_for_the_profile_lock_and_puts_back_in_this_launch() {
    use std::cell::Cell;
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let now = std::time::SystemTime::now();
    let waiting = |root: &Path| {
        let mut host = Fake {
            selected: Some(widgets),
            notification_center: true,
            full_disk_access: Some(true),
            values: BTreeMap::from([(nc_widgets(), populated())]),
            ..Fake::default()
        };
        execute(&mut host, root, Action::Apply).unwrap();
        host.full_disk_access = Some(false);
        execute(&mut host, root, Action::Restore).unwrap();
        assert!(!restore_waiting(root).unwrap().is_empty());
        host.full_disk_access = Some(true);
        host
    };
    struct Slot<'a>(&'a Cell<bool>);
    impl Drop for Slot<'_> {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let host = waiting(root.path());
    let held = Cell::new(false);
    let slots = Cell::new(0);
    let reserve = || {
        held.set(true);
        slots.set(slots.get() + 1);
        Some(Slot(&held))
    };
    let unbuilt = || -> Result<Fake, String> { panic!("held work constructs no host") };
    let uncaptured = || panic!("held work captures no effect time");
    assert!(matches!(
        follow_up_attempt_at(
            root.path(),
            || None::<Slot<'_>>,
            unbuilt,
            BUILD,
            uncaptured,
            |_| false
        ),
        WithheldFollowUp::Held
    ));
    let holder = HeldLock::take(root.path()).unwrap();
    for _ in 0..3 {
        assert!(matches!(
            follow_up_attempt_at(root.path(), reserve, unbuilt, BUILD, uncaptured, |_| false,),
            WithheldFollowUp::Held
        ));
        assert!(!held.get());
        assert!(!restore_waiting(root.path()).unwrap().is_empty());
    }
    drop(holder);
    let after_budget = now + std::time::Duration::from_secs(20 * 60);
    let WithheldFollowUp::Finished(finished) = follow_up_attempt_at(
        root.path(),
        reserve,
        || {
            assert!(held.get());
            assert!(HeldLock::try_take(root.path()).unwrap().is_none());
            Ok(host)
        },
        BUILD,
        || {
            assert!(held.get());
            assert!(HeldLock::try_take(root.path()).unwrap().is_none());
            after_budget
        },
        |_| false,
    ) else {
        panic!("the unlocked finite attempt must run");
    };
    let FollowedUp {
        observed,
        reservation: slot,
        ..
    } = *finished;
    let report = observed.result.unwrap().expect("put back in this launch");
    assert_eq!(
        report.restore.unwrap().outcome.rows,
        [(Some(Setting::Widgets), PutBack::Back)]
    );
    assert!(restore_waiting(root.path()).unwrap().is_empty());
    assert_eq!(slots.get(), 4);
    assert!(held.get(), "publication retains its slot");
    drop(slot);
    assert!(!held.get());
}

fn report_of(rows: &[(&str, &str, &str)]) -> Report {
    Report {
        action: Action::Apply,
        rows: rows
            .iter()
            .map(|(name, status, detail)| Row {
                name: (*name).into(),
                status: (*status).into(),
                detail: (*detail).into(),
                elapsed_ms: None,
            })
            .collect(),
        manual_steps: Vec::new(),
        backup: PathBuf::from("original-preferences.json"),
        restore: None,
        page: None,
    }
}

/// The log's line for a pass names the row that blocks however many rows are in place, and
/// no row in place; `--check` prints the rows not in place, and `--verbose` every row.
#[test]
fn a_pass_line_names_only_the_rows_not_in_place() {
    let in_place = "x".repeat(200);
    let mut rows: Vec<(String, &str, String)> = (1..=11)
        .map(|n| (format!("Setting {n}"), "ok", in_place.clone()))
        .collect();
    rows.push((
        "Developer Tools".into(),
        "blocked",
        "Needs an administrator".into(),
    ));
    let borrowed: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(name, status, detail)| (name.as_str(), *status, detail.as_str()))
        .collect();
    let report = report_of(&borrowed);
    let line = report.attention_line();
    assert!(line.len() <= ATTENTION_LINE, "{} bytes: {line}", line.len());
    assert!(
        line.contains("1 of 12 rows need attention")
            && line.contains("Developer Tools: blocked — Needs an administrator"),
        "{line}"
    );
    assert!(!line.contains("Setting 1"), "a row in place: {line}");
    let text = report.to_text();
    assert!(text.contains("Developer Tools: blocked"), "{text}");
    assert!(!text.contains("Setting 1"), "{text}");
    let verbose = report.to_text_verbose();
    assert!(
        (1..=11).all(|n| verbose.contains(&format!("Setting {n}: ok"))),
        "{verbose}"
    );

    let long = "\u{e9}".repeat(1500);
    let report = report_of(&[
        ("Secrets", "waiting", &long),
        ("Repositories", "unreached", "after Secrets"),
    ]);
    let line = report.attention_line();
    assert!(line.len() <= ATTENTION_LINE, "{} bytes", line.len());
    assert!(line.contains("Secrets: waiting"), "{line}");
    assert!(line.ends_with("; and 1 more"), "{line}");
}

/// A change put off to a later pass waits on nobody, so nothing needs attention, yet the
/// pass is not complete; a row that waits is a not-yet, never a failure.
#[test]
fn a_deferred_change_waits_on_nobody_and_leaves_the_pass_incomplete() {
    let deferred = report_of(&[("Dock", "ok", ""), ("Spotlight", DEFERRED, "next pass")]);
    assert!(!deferred.needs_attention() && !deferred.failed());
    assert!(!deferred.complete());
    assert!(report_of(&[("Dock", "ok", "")]).complete());
    let waiting = report_of(&[("Secrets", "waiting", "for the vault")]);
    assert!(waiting.needs_attention() && !waiting.failed());
    assert!(report_of(&[("Dock", "error", "write refused")]).failed());
}

/// A vault the Secrets stage parked on, one nobody answered ([`initial::UNREACHED`]) or one
/// whose address answered that none is there ([`initial::ABSENT`]), is a not-yet: it needs
/// attention, so no pass that parked on it is logged as needing none or reads complete, and
/// it is no failure of aterm's own step. Before, an answered absence read `blocked`, which
/// needed attention; its own status, new with the typed stand-aside, did not.
#[test]
fn a_vault_parked_on_as_unreached_or_absent_leaves_the_pass_incomplete() {
    for status in [initial::UNREACHED, initial::ABSENT] {
        let report = report_of(&[("Dock", "ok", ""), ("Secrets", status, "no vault here")]);
        assert!(report.needs_attention(), "{status}");
        assert!(!report.failed(), "{status}");
        assert!(!report.complete(), "{status}");
        assert!(
            report
                .attention_line()
                .contains(&format!("Secrets: {status}")),
            "{status}: {}",
            report.attention_line()
        );
    }
}

/// A grant left to someone else ([`OFFERED`]) or not read ([`UNREAD`]) is no attention and
/// no failure, and the pass is complete on it, yet the report never hides it as in place:
/// the terminal's text names each such row, and its verdict counts them apart from the rows
/// in place. NEGATIVE CONTROL: an `ok` row is left out of the text.
#[test]
fn an_offered_grant_is_complete_yet_named_as_not_in_place() {
    let report = report_of(&[
        ("Dock", "ok", "set"),
        ("Full Disk Access", OFFERED, "Not confirmed; offered"),
        ("Debugger access", UNREAD, "could not be read here"),
    ]);
    assert!(!report.needs_attention() && !report.failed());
    assert!(report.complete());
    let text = report.to_text();
    for name in ["Full Disk Access", "Debugger access"] {
        assert!(text.contains(name), "{name}: {text}");
    }
    assert!(!text.contains("Dock"), "CONTROL: {text}");
    let verdict = text.lines().next().unwrap();
    let in_place = report_of(&[
        ("Dock", "ok", "set"),
        ("Full Disk Access", "ok", "on"),
        ("Debugger access", "ok", "on"),
    ])
    .to_text();
    assert_ne!(
        verdict,
        in_place.lines().next().unwrap(),
        "the verdict counts them apart"
    );
}

/// The profile lock is System State's setup's too (its Set up now, a group's switch, the
/// ALab watch): an `aterm doctor --apply` typed meanwhile is refused saying so, never
/// naming a doctor pass nobody ran.
#[test]
fn apply_refused_by_the_windows_setup_names_setup() {
    let root = aterm_tempfile::tempdir().unwrap();
    let _window_setup = HeldLock::take(root.path()).unwrap();
    let refused = mutation_observed(
        root.path(),
        None,
        Action::Apply,
        || false,
        |_| panic!("a refused Apply constructs no mutation host"),
    );
    let why = refused.result.unwrap_err();
    assert_eq!(why, SETTING_UP);
    assert!(why.contains("System State"), "{why}");
}

/// A group the person turned off in System State stays off through `aterm doctor --apply`
/// (System State design principle 4): the pass leaves it as it is, the plan it saves keeps
/// it off, and one row says so, naming the narrowest way to turn it on. While the person's
/// Stop of setup stands, the pass changes nothing and says why.
#[test]
fn apply_keeps_a_group_the_person_turned_off() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut chosen = Selection::all();
    chosen.set(Setting::Messages, false);
    chosen.set(Setting::FaceTime, false);
    authorize_automatic_setup_at(root.path(), chosen).unwrap();
    let mut host = Fake::default();
    let report = {
        let _lock = HeldLock::take(root.path()).unwrap();
        run_mutation(&mut host, root.path(), Action::Apply).unwrap()
    };
    let ran = host.selection();
    assert!(
        !ran.contains(Setting::Messages) && !ran.contains(Setting::FaceTime),
        "the pass signed out: {ran:?}"
    );
    assert!(ran.contains(Setting::Dock), "{ran:?}");
    let plan = authorized_selection(root.path()).unwrap().unwrap();
    assert!(
        !plan.contains(Setting::Messages) && !plan.contains(Setting::FaceTime),
        "the plan turned the sign-outs back on: {plan:?}"
    );
    let off = report
        .rows
        .iter()
        .find(|row| row.status == OFF)
        .expect("the row for what the pass left");
    assert!(
        off.detail.contains("left as you set them in System State")
            && off
                .detail
                .contains("aterm init --only with each one's word (init.messages, init.facetime)")
            && !off.detail.contains("doctor --apply"),
        "{}",
        off.detail
    );
    assert!(!report.needs_attention(), "{}", report.to_text());

    let root = aterm_tempfile::tempdir().unwrap();
    initial::save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let revision = initial::revision_at(root.path()).unwrap();
    initial::request_stop_at(root.path(), revision).unwrap();
    let mut host = Fake::default();
    let report = {
        let _lock = HeldLock::take(root.path()).unwrap();
        run_mutation(&mut host, root.path(), Action::Apply).unwrap()
    };
    assert!(
        host.writes.is_empty() && host.restarts.is_empty(),
        "a Stop holds every group"
    );
    assert!(
        report
            .rows
            .iter()
            .any(|row| row.status == OFF && row.detail.contains(initial::STOPPED_IN_SYSTEM_STATE)),
        "{}",
        report.to_text()
    );
}

/// A group row's remedy names the narrowest action that changes that row, its own group's
/// switch, never a whole-profile pass such as `aterm doctor --apply`, which would also
/// sign out or set up groups the person left as they are.
#[test]
fn a_group_rows_remedy_names_its_own_switch_not_a_whole_profile_pass() {
    for remedy in [LATER, SETTLED, SETTLED_OWED] {
        assert!(
            remedy.contains("turn its group off and on in System State")
                && !remedy.contains("doctor --apply")
                && !remedy.contains("Set up now"),
            "{remedy}"
        );
    }
}

/// `aterm doctor --check` judges only what this Mac's setup selects: a group the person
/// turned off reads off, and the ALab profile on a Mac outside ALab with no plan of the
/// person's reads not in scope; neither needs attention.
#[test]
fn check_reads_a_group_turned_off_or_out_of_scope_as_no_failure() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut chosen = Selection::all();
    chosen.set(Setting::Dock, false);
    authorize_automatic_setup_at(root.path(), chosen).unwrap();
    let (judged, rows) = check_scope(root.path(), || true, true);
    assert!(!judged.contains(Setting::Dock) && judged.contains(Setting::Messages));
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].status, OFF);
    assert!(
        rows[0].detail.contains("aterm init --only dock"),
        "{}",
        rows[0].detail
    );
    assert!(!rows[0].needs_attention());

    let root = aterm_tempfile::tempdir().unwrap();
    let (judged, rows) = check_scope(root.path(), || false, true);
    assert!(judged.is_empty(), "{judged:?}");
    assert!(
        rows.iter()
            .all(|row| row.status == NOT_IN_SCOPE && !row.needs_attention()),
        "{rows:?}"
    );
    let (judged, rows) = check_scope(root.path(), || true, true);
    assert_eq!(judged, Selection::all().workstation());
    assert!(rows.is_empty(), "{rows:?}");
}

/// `aterm init` waits for another setup's hold on the records only so long, then ends with
/// words that say what holds them, having changed nothing; `--no-wait` (no `wait`) ends so at
/// once, in its own words, however long the limit.
#[test]
fn init_waits_for_another_setup_only_so_long_and_says_what_holds_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let _window_setup = HeldLock::take(root.path()).unwrap();
    let heard = std::sync::Mutex::new(Vec::new());
    let waited = init_hold_within(
        root.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        false,
        Some(&|line| heard.lock().unwrap().push(line.to_owned())),
        std::time::Duration::from_millis(250),
        initial::complete_explicit_workstation_plan_for_test,
    );
    assert_eq!(
        waited.err(),
        Some(aterm_machines::init::Unheld::Busy(INIT_WAITED.into()))
    );
    assert_eq!(*heard.lock().unwrap(), [INIT_WAITS]);
    assert!(INIT_WAITS.contains("System State") && INIT_WAITED.contains("System State"));
    assert!(
        !root.path().join("automatic-plan.json").exists(),
        "nothing was recorded"
    );
    let at_once = init_hold_within(
        root.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        false,
        None,
        std::time::Duration::from_secs(10 * 60),
        initial::complete_explicit_workstation_plan_for_test,
    );
    assert_eq!(
        at_once.err(),
        Some(aterm_machines::init::Unheld::Busy(INIT_NO_WAIT.into()))
    );
    assert!(!root.path().join("automatic-plan.json").exists());
}

/// A whole-profile `aterm init` at the console of a Mac whose window never ran (no setup
/// records) saves aterm's own plan, the base plan and what it ran: the next `aterm init`
/// reads nothing of it as the person's Leave as is, so the Secrets vault, Rust in Spotlight
/// and (on an ALab Mac) the GitHub repositories keep running, and the vault stays in sync.
#[test]
fn a_whole_init_on_a_mac_with_no_records_keeps_the_base_plan() {
    for alab in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        drop(
            init_hold_within(
                root.path(),
                aterm_machines::init::Pass::Whole { clones: true },
                alab,
                Some(&|_| {}),
                std::time::Duration::from_secs(5),
                initial::complete_explicit_workstation_plan_for_test,
            )
            .unwrap(),
        );
        let holds = initial::holds_at(root.path());
        let in_scope = initial::plan_in_scope(root.path(), || alab);
        for setting in [Setting::Secrets, Setting::Spotlight, Setting::Repositories] {
            assert!(!holds.holds(setting), "{alab} {setting:?}: {holds:?}");
            assert!(
                in_scope.contains(setting),
                "{alab} {setting:?}: {in_scope:?}"
            );
        }
        assert!(initial::keeps_vault_sync_at(root.path()).unwrap(), "{alab}");
    }
}

/// On an ALab Mac whose saved plan is still aterm's own provisional base plan (the window
/// widens it at its next look), `doctor --check` judges the ALab profile there, judging scope
/// from the ALab check as `aterm init` does, rather than calling it out of scope.
#[test]
fn check_on_an_alab_mac_with_aterms_base_plan_does_not_call_it_out_of_scope() {
    let root = aterm_tempfile::tempdir().unwrap();
    write_json(
        &root.path().join("automatic-plan.json"),
        &initial::stamped(json!({
            "profile": PROFILE,
            "selection": aterm_types::macos_setup::BASE_PLAN.bits(),
            "vault_url": aterm_types::macos_setup::DEFAULT_VAULT_URL,
            "provisional": true,
        })),
    )
    .unwrap();
    let (judged, rows) = check_scope(root.path(), || true, true);
    assert!(
        judged.contains(Setting::Dock),
        "an ALab Mac's Dock reads: {rows:?}"
    );
    // NEGATIVE CONTROL: elsewhere the ALab profile is out of scope.
    let (judged, rows) = check_scope(root.path(), || false, true);
    assert!(!judged.contains(Setting::Dock), "{judged:?}");
    assert!(
        rows.iter().any(|row| row.status == NOT_IN_SCOPE),
        "{rows:?}"
    );
}

/// `aterm doctor --apply` on a Mac with no setup records keeps the base plan too: nothing
/// reads it afterwards as the person's Leave as is.
#[test]
fn doctor_apply_on_a_mac_with_no_records_keeps_the_base_plan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut host = Fake::default();
    {
        let _lock = HeldLock::take(root.path()).unwrap();
        run_mutation(&mut host, root.path(), Action::Apply).unwrap();
    }
    let holds = initial::holds_at(root.path());
    for setting in [Setting::Secrets, Setting::Spotlight, Setting::Permissions] {
        assert!(!holds.holds(setting), "{setting:?}: {holds:?}");
    }
}

/// The ALab profile's sign-outs, which nothing undoes, stay scoped to ALab's Macs on `aterm
/// doctor --apply` as on `aterm init` and `--check`: a Mac outside ALab whose records hold
/// the base plan aterm chose by itself is not signed out of Messages and FaceTime, and each
/// group out of scope reads so. Control: an ALab Mac's apply takes them.
#[test]
fn doctor_apply_on_a_mac_outside_alab_does_not_sign_out() {
    for outside_alab in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        write_json(
            &root.path().join("automatic-plan.json"),
            &initial::stamped(json!({
                "profile": PROFILE,
                "selection": aterm_types::macos_setup::BASE_PLAN.bits(),
                "vault_url": aterm_types::macos_setup::DEFAULT_VAULT_URL,
                "provisional": true,
            })),
        )
        .unwrap();
        let mut host = Fake {
            outside_alab,
            ..Fake::default()
        };
        let report = {
            let _lock = HeldLock::take(root.path()).unwrap();
            run_mutation(&mut host, root.path(), Action::Apply).unwrap()
        };
        let ran = host.selection();
        assert_eq!(
            ran.contains(Setting::Messages) || ran.contains(Setting::FaceTime),
            !outside_alab,
            "{outside_alab}: {ran:?}"
        );
        assert_eq!(
            report.rows.iter().any(|row| row.status == NOT_IN_SCOPE),
            outside_alab,
            "{:?}",
            report.rows
        );
    }
}

/// The keys kept as they changed after aterm set them are what `aterm init` leaves as they
/// are ([`InitRecords`]' `kept`); a record that cannot be read says so, and init then sets
/// none.
#[test]
fn init_reads_the_keys_kept_as_they_changed() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert_eq!(kept_items_at(root.path()).unwrap(), Vec::<String>::new());
    let mut kept = initial::kept::Kept::default();
    kept.kept.push(initial::kept::Key {
        item: "init.dock@alice".to_owned(),
        setting: Some(Setting::Dock),
        held: None,
        again: true,
    });
    kept.save(root.path()).unwrap();
    assert_eq!(kept_items_at(root.path()).unwrap(), ["init.dock@alice"]);
    std::fs::write(root.path().join(initial::kept::KEPT_RECORD), b"{").unwrap();
    assert!(kept_items_at(root.path()).is_err());
}

/// While the person's Stop holds setup, `aterm doctor --apply` leaves every group as it is,
/// and `--check` reads each such group `off` in the same words, never as drift: one Mac is
/// never "held" to `--apply` and "failing" to `--check` (init design §7, round 23).
#[test]
fn check_reads_every_group_a_stop_holds_as_off() {
    let root = aterm_tempfile::tempdir().unwrap();
    initial::save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let revision = initial::revision_at(root.path()).unwrap();
    initial::request_stop_at(root.path(), revision).unwrap();
    let holds = initial::holds_at(root.path());
    assert!(
        holds.stopped.is_some() && holds.holds(Setting::Dock),
        "{holds:?}"
    );
    let (judged, rows) = check_scope(root.path(), || true, true);
    assert!(
        judged.is_empty()
            && rows.len() == 8
            && rows
                .iter()
                .all(|row| row.status == OFF && !row.needs_attention()),
        "under the person's Stop `doctor --check` judges {judged:?} (rows {rows:?})"
    );
}

/// A plan an older aterm saved (no `vault_url`: it predates the Permissions group) presents
/// aterm's own permissions on, since nobody turned them off; a whole `aterm init` or `aterm
/// doctor --apply` starts its explicit plan from that presented plan, so it never turns them
/// to Leave as is, which nobody chose.
#[test]
fn an_explicit_pass_keeps_aterms_permissions_in_an_older_plan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut legacy = Selection::all();
    legacy.set(Setting::Permissions, false);
    authorize_automatic_setup_at(root.path(), legacy).unwrap();
    assert!(
        !initial::holds_at(root.path())
            .off
            .contains(Setting::Permissions),
        "the older plan presents aterm's permissions on"
    );
    initial::save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let holds = initial::holds_at(root.path());
    assert!(
        !holds.off.contains(Setting::Permissions),
        "the explicit pass turned aterm's permissions to Leave as is: {holds:?}"
    );
}

/// A Cancel an older aterm's review recorded (with the plan it showed) holds every group.
/// `aterm doctor --apply` and a whole `aterm init` honour it: they set nothing, and they save
/// no plan and no decision, so the Cancel still stands and the next launch runs none of the
/// groups it holds.
#[test]
fn a_whole_pass_under_a_cancel_leaves_the_cancel_standing() {
    let cancelled = || {
        let root = aterm_tempfile::tempdir().unwrap();
        let shown = json!({
            "profile": PROFILE,
            "selection": Selection::all().bits(),
            "vault_url": aterm_types::macos_setup::DEFAULT_VAULT_URL,
        });
        write_json(
            &root.path().join("automatic-plan.json"),
            &initial::stamped(shown.clone()),
        )
        .unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({ "profile": PROFILE, "state": "cancelled", "reviewed": shown }),
        )
        .unwrap();
        let before = initial::holds_at(root.path());
        assert!(
            before.off.contains(Setting::Dock)
                && before.off.contains(Setting::Secrets)
                && before.off.contains(Setting::Repositories),
            "the Cancel holds every group: {before:?}"
        );
        root
    };
    let stands = |root: &Path, pass: &str| {
        let after = initial::holds_at(root);
        assert!(
            after.off.contains(Setting::Secrets)
                && after.off.contains(Setting::Spotlight)
                && after.off.contains(Setting::Repositories),
            "{pass} lifted the Cancel: the records now hold only {after:?}"
        );
        assert!(
            initial::cancelled_at(root).unwrap(),
            "{pass} replaced the Cancel's decision"
        );
    };
    let root = cancelled();
    let mut host = Fake::default();
    let report = {
        let _lock = HeldLock::take(root.path()).unwrap();
        run_mutation(&mut host, root.path(), Action::Apply).unwrap()
    };
    assert!(
        host.writes.is_empty() && host.restarts.is_empty(),
        "{}",
        report.to_text()
    );
    stands(root.path(), "doctor --apply");

    let root = cancelled();
    let held = init_hold_at(
        root.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        true,
        Some(&|line| panic!("{line}")),
        initial::complete_explicit_workstation_plan_for_test,
    )
    .unwrap();
    drop(held);
    stands(root.path(), "a whole aterm init");
}
