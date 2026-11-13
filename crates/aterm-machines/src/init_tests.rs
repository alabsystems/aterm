// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::init_clt::TAB_CLEAR;
use crate::items::defaults::tests::{ctx, export, user};
use crate::model::{Effect, Probe};
use crate::port::{Fake, Ran, Stat};
use aterm_converge::{FileJournal, NoJournal};
use std::sync::Mutex;

/// A machines item whose probes answer from a list (the last one repeats).
struct Listed {
    id: String,
    family: &'static str,
    probes: Mutex<Vec<Probe>>,
}

impl Listed {
    fn boxed(id: impl Into<String>, probes: Vec<Probe>) -> Box<dyn Item> {
        Self::of("test", id, probes)
    }

    /// [`Self::boxed`], of `family`.
    fn of(family: &'static str, id: impl Into<String>, probes: Vec<Probe>) -> Box<dyn Item> {
        Box::new(Self {
            id: id.into(),
            family,
            probes: Mutex::new(probes),
        })
    }
}

impl Item for Listed {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn family(&self) -> &'static str {
        self.family
    }
    fn title(&self) -> String {
        self.id.clone()
    }
    fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
        let mut probes = self.probes.lock().unwrap();
        if probes.len() > 1 {
            probes.remove(0)
        } else {
            probes[0].clone()
        }
    }
    fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
        Ok(())
    }
    fn revert(&self) -> Option<String> {
        Some(format!("undo {}", self.id))
    }
}

fn ok() -> Probe {
    Probe::Ok { now: "b".into() }
}

fn change() -> Probe {
    Probe::Change {
        from: "a".into(),
        to: "b".into(),
    }
}

fn verified_accounts(ctx: &Ctx) -> Vec<Box<dyn Item>> {
    crate::init_accounts::items(ctx)
        .unwrap()
        .into_iter()
        .map(|item| Listed::boxed(item.id(), vec![ok()]))
        .collect()
}

/// [`prepare_groups_with`] for an apply with no flag, no `--only` and no grant, on an ALab
/// Mac ([`alab_standing`]), whose vault look (a scripted one: a test never reads the
/// network) finds no vault.
fn prepared(sys: &dyn System, ctx: &Ctx) -> Vec<Group> {
    let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
        crate::init_secrets::tests::unconfigured(),
    )]);
    prepare_groups_with(
        sys,
        ctx,
        Mode::Apply,
        None,
        &reading(&[]),
        &alab_standing(),
        no_grants,
        move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
    )
    .unwrap()
    .0
}

/// A run with no flag and the `--only` words `only`.
fn reading(only: &[String]) -> Reading<'_> {
    static KEYBOARDS: KeyboardRead = KeyboardRead::EMPTY;
    Reading {
        keyboards: &KEYBOARDS,
        context_scope: Selection::all(),
        only,
        picks: Picks::default(),
    }
}

/// An ALab Mac with a person at its screen, nothing held in System State.
fn alab_standing() -> Standing<'static> {
    Standing {
        decided: Decider::Person,
        alab: &|| true,
        present: &|| true,
        holds: Holds::default(),
        workstation_observation: None,
    }
}

/// The coordinator's read-back of `verified`, read `ago` seconds before [`Fake::root`]'s
/// clock (and undated when it verified none, as `setup status` publishes it).
fn read_back(verified: Selection, ago: u64) -> ReadBack {
    ReadBack {
        rows: verified,
        read_at: (!verified.is_empty()).then_some(1_800_000_000 - ago),
        ..ReadBack::default()
    }
}

/// The coordinator's completion with `verified`, read a minute before [`Fake::root`]'s clock.
fn completed(verified: Selection) -> WorkstationObservation {
    WorkstationObservation::Complete(read_back(verified, 60))
}

/// A Mac that is not ALab's, with a person at its screen, nothing held in System State.
fn personal_standing() -> Standing<'static> {
    Standing {
        decided: Decider::Person,
        alab: &|| false,
        present: &|| true,
        holds: Holds::default(),
        workstation_observation: None,
    }
}

/// What `aterm init` answers on standard output fails the run only when the answer is lost:
/// a reader gone ends the answer and keeps the run's own status, while a full disk under a
/// redirect (or EIO) is said on stderr, once, and exits 1, so `aterm init --json > file`
/// never reads a short report as a success; a stderr that fails too (`2>&1` onto the same
/// full disk) still returns exit 1, never unwinding, so the walk after the summary is still
/// handed over. CONTROL: an answer written in full is the run's, and says nothing on stderr.
#[test]
fn a_lost_answer_fails_the_run_and_a_gone_reader_does_not() {
    use std::io::ErrorKind;
    struct Refusing(ErrorKind);
    impl std::io::Write for Refusing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let said = std::cell::RefCell::new(Vec::<String>::new());
    let say = |line: &str| {
        said.borrow_mut().push(line.to_owned());
        Ok(())
    };
    let mut written = Vec::new();
    assert_eq!(answer_to(&mut written, "{}\n", say), Ok(()));
    assert_eq!(written, b"{}\n");
    assert_eq!(
        answer_to(&mut Refusing(ErrorKind::BrokenPipe), "{}\n", say),
        Ok(())
    );
    assert!(said.borrow().is_empty(), "{:?}", said.borrow());
    for kind in [ErrorKind::StorageFull, ErrorKind::Other] {
        said.borrow_mut().clear();
        assert_eq!(
            answer_to(&mut Refusing(kind), "{}\n", say),
            Err(ExitCode::FAILURE),
            "{kind:?}"
        );
        assert_eq!(said.borrow().len(), 1, "{kind:?}: {:?}", said.borrow());
    }
    assert_eq!(
        answer_to(&mut Refusing(ErrorKind::StorageFull), "{}\n", |_| {
            Err(ErrorKind::StorageFull.into())
        }),
        Err(ExitCode::FAILURE),
        "stderr lost too"
    );
}

#[test]
fn an_empty_selection_prepares_and_runs_without_any_host_probe() {
    let sys = Fake::root();
    let pass =
        prepare_workstation_selected(&sys, WorkstationGroup::All, Selection::none()).unwrap();
    let rows = pass
        .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
        .unwrap()
        .rows;
    assert!(rows.is_empty());
    assert!(
        sys.calls().is_empty(),
        "even account and input facts remain unread"
    );
}

/// The widget, desktop and Dock items of one pass share its one relaunch: one
/// `DesktopActivation` across the whole selection, as one
/// `init_preferences::selected_tagged_items` call builds it.
#[test]
fn the_desktop_items_of_one_pass_share_one_activation() {
    let who = user("alice", 501);
    let context = ctx(&[who]);
    let items = workstation_items_selected(
        &context,
        WorkstationGroup::All,
        Selection::all(),
        &crate::facts::KeyboardCohorts::EMPTY,
    )
    .unwrap();
    let mut order: Vec<Setting> = items.iter().map(|(setting, _)| *setting).collect();
    order.dedup();
    assert_eq!(
        order,
        [
            Setting::CapsLock,
            Setting::Widgets,
            Setting::Desktop,
            Setting::Dock,
            Setting::NaturalScrolling,
            Setting::TrackpadScrolling,
            Setting::Messages,
            Setting::FaceTime,
        ]
    );
    assert!(
        items
            .iter()
            .filter(|(setting, _)| *setting == Setting::CapsLock)
            .all(|(_, item)| item.family() == "keyboard"),
    );
    let (settings, activations): (Vec<Setting>, Vec<_>) = items
        .iter()
        .filter_map(|(setting, item)| Some((*setting, item.activation()?)))
        .unzip();
    assert_eq!(
        settings,
        [
            Setting::Widgets,
            Setting::Widgets,
            Setting::Widgets,
            Setting::Desktop,
            Setting::Dock
        ]
    );
    let ids: Vec<String> = activations.iter().map(|a| a.id()).collect();
    assert!(
        activations
            .windows(2)
            .all(|w| std::sync::Arc::ptr_eq(&w[0], &w[1])),
        "one DesktopActivation per pass, not one per setting (ids {ids:?})"
    );
}

/// The row a preference key belongs to is read off the keys the items write: every key an
/// item of a full-profile pass would change names the row that item reports to, so a
/// Restore can say what it put back in System State's row names.
#[test]
fn every_key_an_item_changes_names_the_row_it_reports_to() {
    let who = user("alice", 501);
    let mut context = ctx(&[who]);
    context.facts.full_disk_access = Some(true);
    context.facts.arch = "arm64".into();
    let sys = Fake::root();
    crate::init_preferences::tests::installed_dock_apps(&sys, &context.users[0]);
    sys.simulate(|_, cmd| {
        let line = cmd.display();
        (line.contains("/usr/bin/defaults") && line.contains(" export ")).then(|| export("<dict/>"))
    });
    let items = workstation_items_selected(
        &context,
        WorkstationGroup::Preferences,
        Selection::all(),
        &crate::facts::KeyboardCohorts::EMPTY,
    )
    .unwrap();
    let mut named = Vec::new();
    for (setting, item) in &items {
        for (target, _) in item.preference_changes(&sys, &context).unwrap() {
            assert_eq!(
                crate::init_preferences::setting_of_preference(&target.domain, &target.key),
                Some(*setting),
                "{}",
                target.describe()
            );
            named.push(*setting);
        }
    }
    for setting in [
        Setting::CapsLock,
        Setting::NaturalScrolling,
        Setting::TrackpadScrolling,
        Setting::Desktop,
        Setting::Widgets,
        Setting::Dock,
    ] {
        assert!(named.contains(&setting), "no key of {setting:?} was read");
    }
    assert_eq!(
        crate::init_preferences::setting_of_preference("com.apple.dock", "no-such-key"),
        None
    );
}

#[test]
fn selected_widgets_do_not_read_keyboard_dock_trackpad_or_accounts() {
    let who = user("alice", 501);
    for (full_disk_access, keys) in [(Some(true), 3), (Some(false), 2), (None, 2)] {
        let mut context = ctx(std::slice::from_ref(&who));
        context.facts.full_disk_access = full_disk_access;
        let sys = Fake::root();
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.WindowManager -",
            export("<dict/>"),
        );
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.notificationcenterui -",
            export("<dict/>"),
        );
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        let items = workstation_items_selected(
            &context,
            WorkstationGroup::All,
            selected,
            &crate::facts::KeyboardCohorts::EMPTY,
        )
        .unwrap();
        let ids: Vec<String> = items.iter().map(|(_, item)| item.id()).collect();
        assert_eq!(
            ids,
            [
                "consumer.widgets.desktop@alice",
                "consumer.widgets.stage-manager@alice",
                "consumer.widgets.notification-center@alice"
            ]
        );
        assert!(
            items
                .iter()
                .all(|(setting, _)| *setting == Setting::Widgets)
        );
        let mut snapshots = 0;
        for (_, item) in &items {
            snapshots += item.preference_changes(&sys, &context).unwrap().len();
        }
        assert_eq!(snapshots, keys, "{full_disk_access:?}");
        for (_, item) in &items[..2] {
            assert!(matches!(
                item.probe(&sys, &context),
                Probe::Change { .. } | Probe::Deferred { .. }
            ));
        }
        let calls = sys.calls();
        for call in &calls {
            assert!(
                call.contains("com.apple.WindowManager")
                    || (full_disk_access == Some(true)
                        && call.contains("com.apple.notificationcenterui"))
                    || call.contains("/init-activation/desktopwidgets.pending")
                    || call.contains("/init-activation/widgets.pending"),
                "{full_disk_access:?}: unchecked setting queried: {call}"
            );
        }
        assert_eq!(
            calls
                .iter()
                .any(|call| call.contains("com.apple.notificationcenterui")),
            full_disk_access == Some(true),
            "{calls:?}"
        );
    }
}

#[test]
fn the_runners_full_disk_access_reaches_the_widgets_row_through_the_port() {
    for full_disk_access in [None, Some(false), Some(true)] {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.full_disk_access = full_disk_access;
        sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.WindowManager -",
            export("<dict><key>StandardHideWidgets</key><true/><key>StageManagerHideWidgets</key><true/></dict>"),
        );
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.notificationcenterui -",
            export("<dict><key>widgets</key><dict><key>instances</key><array/></dict></dict>"),
        );
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        let pass = prepare_workstation_selected(&sys, WorkstationGroup::All, selected).unwrap();
        let rows = pass
            .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter().all(|row| row.outcome == Outcome::Ok),
            "{rows:?}"
        );
        let granted = full_disk_access == Some(true);
        assert_eq!(
            rows[2].title.contains("Full Disk Access"),
            !granted,
            "{}",
            rows[2].title
        );
        assert_eq!(
            sys.calls()
                .iter()
                .any(|call| call.contains("com.apple.notificationcenterui")),
            granted,
            "{:?}",
            sys.calls()
        );
    }
}

#[test]
fn the_withheld_group_reaches_only_the_keys_a_pass_without_the_grant_left_alone() {
    assert_eq!(
        WorkstationGroup::Withheld.selected(Selection::all()),
        WorkstationGroup::Preferences.selected(Selection::all())
    );
    let mut selected = Selection::none();
    selected.set(Setting::Widgets, true);
    selected.set(Setting::Dock, true);
    let console = |full_disk_access| {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.full_disk_access = full_disk_access;
        sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys.on(
            "alice$ /usr/bin/defaults export com.apple.notificationcenterui -",
            export("<dict><key>widgets</key><dict><key>instances</key><array><data>Kg==</data></array></dict></dict>"),
        );
        sys
    };
    let sys = console(Some(true));
    let mut follow_up =
        prepare_workstation_selected(&sys, WorkstationGroup::Withheld, selected).unwrap();
    let snapshot = follow_up.preference_changes(&sys).unwrap();
    assert_eq!(snapshot.len(), 1, "{snapshot:?}");
    assert_eq!(snapshot[0].0.domain, "com.apple.notificationcenterui");
    let run = follow_up
        .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
        .unwrap();
    let rows: Vec<(&str, &str, Outcome)> = run
        .rows
        .iter()
        .map(|row| (row.id.as_str(), row.title.as_str(), row.outcome))
        .collect();
    assert_eq!(
        rows,
        [(
            "consumer.widgets.notification-center@alice",
            "No Notification Center widgets",
            Outcome::Planned
        )]
    );
    assert!(run.withheld.is_empty());
    for call in sys.calls() {
        assert!(
            !call.contains("com.apple.WindowManager") && !call.contains("com.apple.dock"),
            "{call}"
        );
    }

    let sys = console(None);
    sys.on(
        "alice$ /usr/bin/defaults export com.apple.WindowManager -",
        export("<dict/>"),
    );
    let mut widgets = Selection::none();
    widgets.set(Setting::Widgets, true);
    let pass = prepare_workstation_selected(&sys, WorkstationGroup::Preferences, widgets).unwrap();
    let run = pass
        .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
        .unwrap();
    assert_eq!(run.withheld, ["consumer.widgets.notification-center@alice"]);
    let sys = console(Some(true));
    sys.on(
        "alice$ /usr/bin/defaults export com.apple.WindowManager -",
        export("<dict/>"),
    );
    let mut selected = Selection::none();
    selected.set(Setting::Widgets, true);
    let mut all =
        prepare_workstation_selected(&sys, WorkstationGroup::Preferences, selected).unwrap();
    assert_eq!(all.preference_changes(&sys).unwrap().len(), 3);
    assert!(
        sys.calls()
            .iter()
            .any(|call| call.contains("com.apple.WindowManager"))
    );
}

#[test]
fn a_single_selected_account_needs_no_keyboard_inventory_or_other_service_read() {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
    sys.stat_of(
        "/dev/console",
        Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    sys.on(
        "alice$ /usr/bin/defaults export com.apple.imservice.ids.iMessage -",
        export("<dict/>"),
    );
    let mut selected = Selection::none();
    selected.set(Setting::Messages, true);
    let pass = prepare_workstation_selected(&sys, WorkstationGroup::All, selected).unwrap();
    let rows = pass
        .run_recording_with(&sys, Mode::Plan, &mut NoJournal)
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, Outcome::Ok, "{:?}", rows[0].why);
    for call in sys.calls() {
        assert!(
            !call.contains("hidutil")
                && !call.contains("FaceTime")
                && !call.contains("osascript")
                && !call.contains("scroll-direction"),
            "unchecked setting queried: {call}"
        );
    }
    let inventory = crate::facts::hidutil_keyboards().display();
    sys.on(
        &inventory,
        Ran::ok("Services:\nVendorID ProductID Product  Built-In\n0x5ac    0x1       Keyboard 1\n"),
    );
    let account_only = initial_setup_context(&sys).unwrap();
    assert!(account_only.facts.keyboards.is_empty());
    assert!(!sys.calls().contains(&inventory));
}

fn saved_off(sys: &Fake) {
    sys.on(
        "alice$ /usr/bin/defaults export -g -",
        export("<dict><key>com.apple.swipescrolldirection</key><false/></dict>"),
    );
}

fn at_console(alice: crate::model::User) -> Ctx {
    Ctx {
        console: Some(alice.clone()),
        ..ctx(&[alice])
    }
}

/// A base-layer item (no console needed) whose probes answer from a list.
struct BaseItem {
    probes: Mutex<Vec<Probe>>,
    applied: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl aterm_converge::Item for BaseItem {
    fn id(&self) -> String {
        "base.pkg.shims".into()
    }
    fn family(&self) -> &'static str {
        "pkg"
    }
    fn layer(&self) -> Layer {
        Layer::Base
    }
    fn class(&self) -> Class {
        Class::Relay
    }
    fn title(&self) -> String {
        "the installed programs' commands".into()
    }
    fn probe(&self) -> Probe {
        let mut probes = self.probes.lock().unwrap();
        if probes.len() > 1 {
            probes.remove(0)
        } else {
            probes[0].clone()
        }
    }
    fn apply(&self) -> Result<(), String> {
        self.applied
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn explain(&self, probe: &Probe) -> aterm_converge::Explain {
        aterm_converge::Explain {
            why: format!("{probe:?}"),
            remedy: Remedy::Repair,
        }
    }
}

fn place(class: Class) -> Place {
    Place {
        layer: Layer::Workstation,
        class,
        requires: Requires::Console,
        remedy: Remedy::Command("aterm init".into()),
    }
}

fn journaled(journal: &FileJournal) -> Vec<aterm_json::Value> {
    match std::fs::read_to_string(journal.path()) {
        Ok(text) => text
            .lines()
            .map(|line| aterm_json::from_str(line).unwrap())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("{}: {e}", journal.path().display()),
    }
}

#[test]
fn a_workstation_pass_journals_in_the_journal_of_the_system_it_runs_on() {
    let sys = Fake::root();
    let pass = Workstation {
        items: vec![
            Listed::boxed("init.test.changes", vec![change(), ok()]),
            Listed::boxed("init.test.right", vec![ok()]),
        ],
        ctx: ctx(&[user("alice", 501)]),
        snapshots: None,
        settings: Vec::new(),
        held: Vec::new(),
        preparation: None,
    };
    let rows = pass.run_recording(&sys, Mode::Apply).unwrap().rows;
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    let text = String::from_utf8(sys.written(Fake::JOURNAL).expect("the fake's journal")).unwrap();
    let ids: Vec<String> = text
        .lines()
        .map(|line| {
            let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
            line["id"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(ids, ["init.test.changes"]);
}

struct Counted {
    id: &'static str,
    probes: Mutex<Vec<Probe>>,
    applies: Arc<std::sync::atomic::AtomicU32>,
    refreshes: Arc<std::sync::atomic::AtomicU32>,
    aside: Option<aterm_converge::StoodAside>,
    /// The preference keys it would change, or why it cannot say
    /// ([`Item::preference_changes`]), one answer per call.
    keys: Mutex<Vec<Result<PreferenceSnapshot, String>>>,
}

impl Counted {
    fn boxed(
        id: &'static str,
        probes: Vec<Probe>,
    ) -> (Box<dyn Item>, [Arc<std::sync::atomic::AtomicU32>; 2]) {
        Self::aside(id, probes, None)
    }

    fn aside(
        id: &'static str,
        probes: Vec<Probe>,
        aside: Option<aterm_converge::StoodAside>,
    ) -> (Box<dyn Item>, [Arc<std::sync::atomic::AtomicU32>; 2]) {
        let counts = [Arc::default(), Arc::default()];
        let item = Box::new(Self {
            id,
            probes: Mutex::new(probes),
            applies: Arc::clone(&counts[0]),
            refreshes: Arc::clone(&counts[1]),
            aside,
            keys: Mutex::new(vec![Ok(Vec::new())]),
        });
        (item, counts)
    }
}

impl Item for Counted {
    fn id(&self) -> String {
        self.id.into()
    }
    fn family(&self) -> &'static str {
        "test"
    }
    fn title(&self) -> String {
        self.id.into()
    }
    fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
        let mut probes = self.probes.lock().unwrap();
        if probes.len() > 1 {
            probes.remove(0)
        } else {
            probes[0].clone()
        }
    }
    fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
        self.applies
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn refresh_guarded(
        &self,
        _: &dyn System,
        _: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Option<Result<(), String>> {
        Some(current().map(|()| {
            self.refreshes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }))
    }
    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.aside.clone()
    }
    fn preference_changes(
        &self,
        _: &dyn System,
        _: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        let mut keys = self.keys.lock().unwrap();
        if keys.len() > 1 {
            keys.remove(0)
        } else {
            keys[0].clone()
        }
    }
}

#[test]
fn a_held_item_has_no_before_images() {
    let alice = user("alice", 501);
    let item = |id: &'static str, domain: &str| {
        Box::new(Counted {
            id,
            probes: Mutex::new(vec![change()]),
            applies: Arc::default(),
            refreshes: Arc::default(),
            aside: None,
            keys: Mutex::new(vec![Ok(vec![(
                Target::user(domain, "k", &alice),
                Read::Absent,
            )])]),
        }) as Box<dyn Item>
    };
    let mut pass = Workstation {
        items: vec![
            item("init.test.dock", "com.apple.dock"),
            item("init.test.widgets", "com.apple.WindowManager"),
        ],
        ctx: ctx(std::slice::from_ref(&alice)),
        snapshots: None,
        settings: Vec::new(),
        held: Vec::new(),
        preparation: None,
    };
    pass.hold(&[("init.test.dock".into(), Hold::Later("held".into()))]);
    let before = pass.preference_changes(&Fake::root()).unwrap();
    let domains: Vec<&str> = before.iter().map(|(t, _)| t.domain.as_str()).collect();
    assert_eq!(domains, ["com.apple.WindowManager"]);
    assert_eq!(
        pass.snapshots.as_ref().map(Vec::len),
        Some(2),
        "one snapshot per item, in item order"
    );
}

#[test]
fn an_item_without_before_images_changes_nothing_and_the_others_run() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let alice = user("alice", 501);
    let item = |id: &'static str, keys| {
        let applies = Arc::new(AtomicU32::new(0));
        let item = Box::new(Counted {
            id,
            probes: Mutex::new(vec![change(), ok()]),
            applies: Arc::clone(&applies),
            refreshes: Arc::default(),
            aside: None,
            keys: Mutex::new(keys),
        }) as Box<dyn Item>;
        (item, applies)
    };
    let target = |domain: &str| Target::user(domain, "k", &alice);
    let (cleared, cleared_applies) = item(
        "init.test.cleared",
        vec![
            Err("plutil timed out; left untouched".into()),
            Ok(vec![(target("com.apple.dock"), Read::Absent)]),
        ],
    );
    let (unusable, unusable_applies) = item(
        "init.test.unusable",
        vec![Ok(vec![(
            target("com.apple.finder"),
            Read::Unusable("cfprefsd timed out".into()),
        )])],
    );
    let (widgets, widgets_applies) = item(
        "init.test.widgets",
        vec![Ok(vec![(target("com.apple.WindowManager"), Read::Absent)])],
    );
    let mut pass = Workstation {
        items: vec![cleared, unusable, widgets],
        ctx: ctx(std::slice::from_ref(&alice)),
        settings: Vec::new(),
        snapshots: None,
        held: Vec::new(),
        preparation: None,
    };
    let sys = Fake::root();
    let before = pass.preference_changes(&sys).unwrap();
    let domains: Vec<&str> = before.iter().map(|(t, _)| t.domain.as_str()).collect();
    assert_eq!(domains, ["com.apple.WindowManager"]);
    let run = pass
        .run_recording_with(&sys, Mode::Apply, &mut NoJournal)
        .unwrap();
    let outcome = |id: &str| {
        let row = run.rows.iter().find(|row| row.id == id).unwrap();
        (row.outcome, row.why.clone().unwrap_or_default())
    };
    let (cleared_outcome, cleared_why) = outcome("init.test.cleared");
    assert_eq!(cleared_outcome, Outcome::Failed);
    assert!(cleared_why.contains("plutil timed out"), "{cleared_why}");
    let (unusable_outcome, unusable_why) = outcome("init.test.unusable");
    assert_eq!(unusable_outcome, Outcome::Failed);
    assert!(unusable_why.contains("read for recovery"), "{unusable_why}");
    assert_eq!(outcome("init.test.widgets").0, Outcome::Changed);
    assert_eq!(
        cleared_applies.load(Ordering::SeqCst),
        0,
        "no unbacked write"
    );
    assert_eq!(
        unusable_applies.load(Ordering::SeqCst),
        0,
        "no unbacked write"
    );
    assert_eq!(widgets_applies.load(Ordering::SeqCst), 1);
    let attempts: Vec<&str> = run.attempts.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(attempts, ["init.test.widgets"]);
}

/// The ids of the fake's journal lines, in order.
fn journaled_ids(sys: &Fake) -> Vec<String> {
    let text = String::from_utf8(sys.written(Fake::JOURNAL).expect("the fake's journal")).unwrap();
    text.lines()
        .map(|line| {
            let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
            line["id"].as_str().unwrap().to_owned()
        })
        .collect()
}

#[test]
fn a_held_item_changes_nothing_and_only_reached_items_are_attempts() {
    use std::sync::atomic::Ordering;
    let sys = Fake::root();
    let (held, held_counts) = Counted::boxed("init.test.held", vec![change()]);
    let (held_ok, held_ok_counts) = Counted::boxed("init.test.held-ok", vec![ok()]);
    let (as_is, as_is_counts) = Counted::boxed("init.test.as-is", vec![change()]);
    let (free, free_counts) = Counted::boxed("init.test.free", vec![change(), ok()]);
    let (fails, fails_counts) = Counted::boxed("init.test.fails", vec![change()]);
    let mut pass = Workstation {
        items: vec![
            held,
            held_ok,
            as_is,
            free,
            fails,
            Listed::boxed("init.test.right", vec![ok()]),
        ],
        ctx: ctx(&[user("alice", 501)]),
        snapshots: None,
        settings: Vec::new(),
        held: Vec::new(),
        preparation: None,
    };
    pass.hold(&[
        (
            "init.test.held".into(),
            Hold::Later("held by the test".into()),
        ),
        (
            "init.test.held-ok".into(),
            Hold::Later("held by the test".into()),
        ),
        ("init.test.as-is".into(), Hold::AsIs("changed since".into())),
    ]);
    let run = pass.run_recording(&sys, Mode::Apply).unwrap();
    let outcome = |id: &str| {
        let row = run.rows.iter().find(|row| row.id == id).unwrap();
        (row.outcome, row.why.clone())
    };
    assert_eq!(
        outcome("init.test.held"),
        (
            Outcome::Deferred,
            Some("a; not changed yet: held by the test".into())
        )
    );
    assert_eq!(outcome("init.test.held-ok").0, Outcome::Ok);
    assert_eq!(
        outcome("init.test.as-is"),
        (
            Outcome::NotApplicable,
            Some("a; left as it is: changed since".into())
        )
    );
    assert_eq!(outcome("init.test.free").0, Outcome::Changed);
    assert_eq!(outcome("init.test.fails").0, Outcome::Failed);
    let counts = |counts: &[Arc<std::sync::atomic::AtomicU32>; 2]| {
        counts.each_ref().map(|count| count.load(Ordering::SeqCst))
    };
    assert_eq!(counts(&held_counts), [0, 0], "a held change never runs");
    assert_eq!(counts(&held_ok_counts), [0, 0], "nor a held refresh");
    assert_eq!(counts(&as_is_counts), [0, 0], "nor one left as it is");
    assert_eq!(counts(&free_counts), [1, 0]);
    assert_eq!(counts(&fails_counts), [1, 0]);
    assert_eq!(journaled_ids(&sys), ["init.test.free", "init.test.fails"]);
    let attempts: Vec<(&str, bool)> = run
        .attempts
        .iter()
        .map(|attempt| (attempt.id.as_str(), attempt.verified))
        .collect();
    assert_eq!(
        attempts,
        [("init.test.free", true), ("init.test.fails", false)],
        "an Ok item with no refresh, and a held one, reached nothing"
    );
}

/// An item whose change to a reading that starts `new ` is only to targets its read-back did
/// not name ([`Item::new_targets`]), counting its applies.
struct Grows {
    probes: Mutex<Vec<Probe>>,
    applies: Arc<std::sync::atomic::AtomicU32>,
}

impl Item for Grows {
    fn id(&self) -> String {
        "init.test.grows".into()
    }
    fn family(&self) -> &'static str {
        "test"
    }
    fn title(&self) -> String {
        self.id()
    }
    fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
        let mut probes = self.probes.lock().unwrap();
        if probes.len() > 1 {
            probes.remove(0)
        } else {
            probes[0].clone()
        }
    }
    fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
        self.applies
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn new_targets(&self, _held: &str, from: &str) -> bool {
        from.starts_with("new ")
    }
}

/// An item held as read back is made as any change is until aterm set it again; after that
/// only a change from what it read back (aterm's want moved) or to targets its read-back did
/// not name is made, and any other is left as it is, with nothing applied. Before, a held
/// item was either made or left, whatever it read and whoever set it last.
#[test]
fn an_item_set_again_is_left_as_it_is_unless_its_want_moved_or_it_grew_targets() {
    use std::sync::atomic::Ordering;
    let sys = Fake::root();
    let (once, once_counts) = Counted::boxed("init.test.once", vec![change(), ok()]);
    let (changed, changed_counts) = Counted::boxed("init.test.changed", vec![change()]);
    let (moved, moved_counts) = Counted::boxed("init.test.moved", vec![change(), change(), ok()]);
    let (unread, unread_counts) = Counted::boxed("init.test.unread", vec![change()]);
    let grows_applies = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let new_c = || Probe::Change {
        from: "new c".into(),
        to: "b".into(),
    };
    let grows = Box::new(Grows {
        probes: Mutex::new(vec![new_c(), new_c(), ok()]),
        applies: Arc::clone(&grows_applies),
    });
    let mut pass = Workstation {
        items: vec![once, changed, moved, unread, grows],
        ctx: ctx(&[user("alice", 501)]),
        snapshots: None,
        settings: Vec::new(),
        held: Vec::new(),
        preparation: None,
    };
    let read_back = |held: Option<&str>, again| Hold::ReadBack {
        held: held.map(str::to_owned),
        again,
        why: "changed again".into(),
    };
    pass.hold(&[
        ("init.test.once".into(), read_back(Some("b"), false)),
        ("init.test.changed".into(), read_back(Some("b"), true)),
        ("init.test.moved".into(), read_back(Some("a"), true)),
        ("init.test.unread".into(), read_back(None, true)),
        ("init.test.grows".into(), read_back(Some("b"), true)),
    ]);
    let run = pass.run_recording(&sys, Mode::Apply).unwrap();
    let outcome = |id: &str| {
        let row = run.rows.iter().find(|row| row.id == id).unwrap();
        (row.outcome, row.why.clone())
    };
    let left = (
        Outcome::NotApplicable,
        Some("a; left as it is: changed again".to_owned()),
    );
    assert_eq!(outcome("init.test.once").0, Outcome::Changed);
    assert_eq!(outcome("init.test.changed"), left);
    assert_eq!(outcome("init.test.moved").0, Outcome::Changed);
    assert_eq!(outcome("init.test.unread"), left);
    assert_eq!(outcome("init.test.grows").0, Outcome::Changed);
    let applies =
        |counts: &[Arc<std::sync::atomic::AtomicU32>; 2]| counts[0].load(Ordering::SeqCst);
    assert_eq!(applies(&once_counts), 1);
    assert_eq!(applies(&changed_counts), 0, "changed again: never set");
    assert_eq!(applies(&moved_counts), 1);
    assert_eq!(applies(&unread_counts), 0);
    assert_eq!(grows_applies.load(Ordering::SeqCst), 1);
    assert_eq!(run.new_targets, ["init.test.grows"]);
}

#[test]
fn an_apply_that_stood_aside_is_no_attempt() {
    let sys = Fake::root();
    let (aside, counts) = Counted::aside(
        "init.test.aside",
        vec![change()],
        Some(aterm_converge::StoodAside::Permission(
            "Accessibility is not granted".into(),
        )),
    );
    let (fails, _) = Counted::boxed("init.test.fails", vec![change()]);
    let pass = Workstation {
        items: vec![aside, fails],
        ctx: ctx(&[user("alice", 501)]),
        snapshots: None,
        settings: Vec::new(),
        held: Vec::new(),
        preparation: None,
    };
    let run = pass.run_recording(&sys, Mode::Apply).unwrap();
    let row = run
        .rows
        .iter()
        .find(|row| row.id == "init.test.aside")
        .unwrap();
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert!(row.stood_aside.is_some(), "{row:?}");
    assert_eq!(counts[0].load(std::sync::atomic::Ordering::SeqCst), 1);
    let attempts: Vec<&str> = run.attempts.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(attempts, ["init.test.fails"], "{:?}", run.attempts);
}

#[test]
fn init_on_a_fake_system_journals_one_line_per_changed_item() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    let alice = at_console(user("alice", 501));
    let sys = Fake::root();
    sys.scroll_state(true);
    saved_off(&sys);
    let groups = || {
        vec![
            (
                vec![
                    Listed::boxed("init.test.changes", vec![change(), ok()]),
                    Listed::boxed("init.test.right", vec![ok()]),
                ],
                place(Class::Preference),
            ),
            (
                crate::init_trackpad::items(&alice).unwrap(),
                place(Class::Preference),
            ),
        ]
    };
    let before = Fake::root();
    before.scroll_state(true);
    saved_off(&before);
    let planned = run(
        &before,
        &alice,
        Base::none(),
        &groups(),
        Mode::Plan,
        &mut NoJournal,
    )
    .unwrap();
    let rows = run(
        &sys,
        &alice,
        Base::none(),
        &groups(),
        Mode::Apply,
        &mut journal,
    )
    .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.outcome).collect::<Vec<_>>(),
        [Outcome::Changed, Outcome::Ok, Outcome::Changed]
    );
    let lines = journaled(&journal);
    assert_eq!(
        lines
            .iter()
            .map(|l| l["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["init.test.changes", "init.trackpad.scroll-direction@alice"]
    );
    assert_eq!(
        planned
            .iter()
            .filter(|r| r.outcome == Outcome::Planned)
            .count(),
        lines.len()
    );
    for ((line, requested), row) in lines
        .iter()
        .zip(planned.iter().filter(|r| r.outcome == Outcome::Planned))
        .zip(rows.iter().filter(|r| r.outcome == Outcome::Changed))
    {
        assert_eq!(line["id"].as_str(), Some(requested.id.as_str()));
        assert_eq!(requested.id, row.id);
        assert_eq!(line["from"].as_str(), row.from.as_deref());
        assert_eq!(line["from"].as_str(), requested.from.as_deref());
        assert_eq!(line["to"].as_str(), requested.to.as_deref());
        assert_eq!(line["revert"].as_str(), row.revert.as_deref());
    }
    assert!(rows.iter().all(|r| r.layer == Some(Layer::Workstation)));
}

#[test]
fn what_the_base_layer_holds_is_released_before_an_item_that_may_take_it_is_read() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    struct Held(Arc<AtomicBool>);
    impl Drop for Held {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    struct Sees {
        released: Arc<AtomicBool>,
        saw: Arc<Mutex<Vec<bool>>>,
    }
    impl Item for Sees {
        fn id(&self) -> String {
            "init.test.sees".into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            self.id()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            self.saw
                .lock()
                .unwrap()
                .push(self.released.load(Ordering::SeqCst));
            ok()
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            Ok(())
        }
    }
    let released = Arc::new(AtomicBool::new(false));
    let saw = Arc::new(Mutex::new(Vec::new()));
    let applied = Arc::new(AtomicU32::new(0));
    let base = Base {
        items: vec![Box::new(BaseItem {
            probes: Mutex::new(vec![change(), ok()]),
            applied: Arc::clone(&applied),
        })],
        held: Some(Box::new(Held(Arc::clone(&released)))),
        clt: None,
        toolset: None,
        targo: None,
    };
    let groups = vec![(
        vec![Box::new(Sees {
            released: Arc::clone(&released),
            saw: Arc::clone(&saw),
        }) as Box<dyn Item>],
        place(Class::Install),
    )];
    let alice = at_console(user("alice", 501));
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    let rows = run(
        &Fake::root(),
        &alice,
        base,
        &groups,
        Mode::Apply,
        &mut journal,
    )
    .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.as_str(), row.outcome))
            .collect::<Vec<_>>(),
        [
            ("base.pkg.shims", Outcome::Changed),
            ("init.test.sees", Outcome::Ok)
        ]
    );
    assert_eq!(applied.load(Ordering::SeqCst), 1);
    let saw = saw.lock().unwrap();
    assert!(
        !saw.is_empty() && saw.iter().all(|&released| released),
        "{saw:?}"
    );
}

#[test]
fn init_journals_nothing_when_every_item_is_already_right() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    let alice = at_console(user("alice", 501));
    let sys = Fake::root();
    sys.scroll_state(false);
    saved_off(&sys);
    let groups = vec![
        (
            vec![Listed::boxed("init.test.right", vec![ok()])],
            place(Class::Preference),
        ),
        (
            crate::init_trackpad::items(&alice).unwrap(),
            place(Class::Preference),
        ),
    ];
    let rows = run(
        &sys,
        &alice,
        Base::none(),
        &groups,
        Mode::Apply,
        &mut journal,
    )
    .unwrap();
    assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:?}");
    assert!(sys.calls().contains(&"scroll-direction set false".into()));
    assert!(journaled(&journal).is_empty());
    assert!(!scratch.path().join(FileJournal::DIR).exists());
}

#[test]
fn init_places_every_item_at_the_workstation_layer_run_at_the_console() {
    let alice = ctx(&[user("alice", 501)]);
    let mut sys = Fake::root();
    sys.euid = 501;
    let profile = b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
    let path = "/Users//alice/.config/aterm/init.toml";
    sys.file(path, profile).stat_of(
        path,
        Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: profile.len() as u64,
        },
    );
    let groups = groups(
        &alice,
        Presence::AtConsole,
        crate::init_spotlight::Projects::Noted,
        crate::init_secrets::Secrets {
            items: Vec::new(),
            waited_on: None,
        },
        crate::init_github::items(&sys, &alice, None).unwrap(),
        &KeyboardRead::EMPTY,
        Selection::all(),
        &[],
        true,
        None,
    )
    .0;
    let bound = crate::engine::bind(
        groups.iter().flat_map(|(items, place)| {
            items.iter().map(move |item| (item.as_ref(), place.clone()))
        }),
        &sys,
        &alice,
    );
    let github: Vec<String> = bound
        .iter()
        .filter(|b| b.class() == Class::Install)
        .map(|b| b.id())
        .collect();
    assert_eq!(
        github.len(),
        3,
        "the keychain block, the GitHub sign-in and checkouts: {github:?}"
    );
    assert!(github.iter().all(|id| id.contains("github")), "{github:?}");
    let asked: Vec<String> = bound
        .iter()
        .filter(|b| b.class() == Class::Asked)
        .map(|b| b.id())
        .collect();
    assert_eq!(
        asked,
        [
            "init.spotlight.rustup-profile@alice",
            "init.spotlight.rust-docs@alice",
            "init.spotlight.build-noindex@alice"
        ]
    );
    for b in &bound {
        assert_eq!(b.layer(), Layer::Workstation, "{}", b.id());
        assert_eq!(b.requires(), Requires::Console, "{}", b.id());
        assert!(
            matches!(
                b.class(),
                Class::Preference | Class::Install | Class::Asked | Class::Elevate
            ),
            "{}",
            b.id()
        );
    }
}

#[test]
fn a_snapshotted_pass_refuses_new_or_retyped_preferences_before_mutation() {
    use crate::items::defaults::tests::{ctx, export, user};
    const READ: &str = "alice$ /usr/bin/defaults export -g -";
    for (original, observed, checked, natural) in [
        ("<false/>", "<true/>", "<true/>", true),
        (
            "<true/>",
            "<integer>1</integer>",
            "<integer>1</integer>",
            true,
        ),
        ("<false/>", "<false/>", "<true/>", false),
    ] {
        let sys = Fake::root();
        sys.scroll_state(natural);
        let pref = |value| {
            export(&format!(
                "<dict><key>com.apple.swipescrolldirection</key>{value}</dict>"
            ))
        };
        sys.on(READ, pref(original))
            .on(READ, pref(observed))
            .on(READ, pref(checked));
        let ctx = ctx(&[user("alice", 501)]);
        let mut pass = Workstation {
            items: crate::init_trackpad::items(&ctx).unwrap(),
            ctx,
            settings: Vec::new(),
            snapshots: None,
            held: Vec::new(),
            preparation: None,
        };
        pass.preference_changes(&sys).unwrap();
        let recording = pass
            .run_recording_with(&sys, Mode::Apply, &mut NoJournal)
            .unwrap();
        assert!(recording.attempts.is_empty());
        let rows = recording.rows;
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(rows[0].why.as_ref().unwrap().contains("recovery snapshot"));
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains(" write ") || call.starts_with("scroll-direction set"))
        );
    }
}

#[test]
fn an_unchanged_snapshot_runs_the_shared_setting_and_verifies_its_write() {
    use crate::items::defaults::tests::{ctx, export, user};
    const READ: &str = "alice$ /usr/bin/defaults export -g -";
    const WRITE: &str =
        "alice$ /usr/bin/defaults write -g com.apple.swipescrolldirection -bool false";
    let sys = Fake::root();
    sys.scroll_state(true);
    for value in [true, true, true, true, false, false] {
        sys.on(
            READ,
            export(&format!(
                "<dict><key>com.apple.swipescrolldirection</key><{}/></dict>",
                if value { "true" } else { "false" }
            )),
        );
    }
    sys.on(WRITE, Ran::ok(""));
    let ctx = ctx(&[user("alice", 501)]);
    let mut pass = Workstation {
        items: crate::init_trackpad::items(&ctx).unwrap(),
        ctx,
        settings: Vec::new(),
        snapshots: None,
        held: Vec::new(),
        preparation: None,
    };
    assert_eq!(pass.preference_changes(&sys).unwrap().len(), 1);
    let recording = pass
        .run_recording_with(&sys, Mode::Apply, &mut NoJournal)
        .unwrap();
    assert_eq!(recording.attempts.len(), 1);
    assert!(recording.attempts[0].verified);
    assert_eq!(recording.attempts[0].preferences.len(), 1);
    let rows = recording.rows;
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.as_str() == WRITE)
            .count(),
        1
    );
    assert!(!sys.natural_scrolling().unwrap());
}

#[test]
fn overlapping_shared_items_cannot_publish_an_ambiguous_snapshot() {
    use crate::items::defaults::tests::{ctx, export, user};
    let sys = Fake::root();
    for _ in 0..2 {
        sys.on(
            "alice$ /usr/bin/defaults export -g -",
            export("<dict><key>com.apple.swipescrolldirection</key><true/></dict>"),
        );
    }
    let ctx = ctx(&[user("alice", 501)]);
    let mut items = crate::init_trackpad::items(&ctx).unwrap();
    items.extend(crate::init_trackpad::items(&ctx).unwrap());
    let mut pass = Workstation {
        ctx,
        items,
        settings: Vec::new(),
        snapshots: None,
        held: Vec::new(),
        preparation: None,
    };
    assert!(
        pass.preference_changes(&sys)
            .unwrap_err()
            .contains("overlap")
    );
    assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
}

/// Setup records `--converge` must never hold.
struct NoSetup;

impl Records for NoSetup {
    fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
        panic!("--converge held the setup records")
    }
}

fn grants_unread(_sign_out: bool) -> Vec<(Access, Grant)> {
    panic!("--converge read the grants")
}

/// `aterm init <args>` over a Host whose every setup port panics and `ports`, its exit
/// status as `ExitCode`'s `Debug` spells it.
fn converge_run(args: &[&str], ports: &dyn crate::sync::Ports) -> String {
    let host = Host {
        decided: Decider::Person,
        base: &|_| panic!("--converge composed init's base layer"),
        full_disk_access: &|| panic!("--converge measured Full Disk Access"),
        records: &NoSetup,
        grants: grants_unread,
        workstation_check: &|_| panic!("--converge read workstation observations"),
        walk: &|_| panic!("--converge walked the grants"),
        converge: ports,
        alab: &|| panic!("--converge read the ALab scope"),
        present: || panic!("--converge read who is at the screen"),
    };
    format!(
        "{:?}",
        main_entry(args.iter().map(OsString::from).collect(), &host)
    )
}

fn exit(code: u8) -> String {
    format!("{:?}", ExitCode::from(code))
}

/// `aterm init --converge` runs the converge pass alone, at background QoS: no setup port
/// (init's base layer, Full Disk Access, the records, the grants, the walk) is touched, the
/// pass records itself, and `--check` leaves the state directory as it found it.
/// NEGATIVE CONTROL: routing `--converge` into the whole of `aterm init` composes init's
/// base layer, whose port panics.
#[test]
fn converge_runs_the_pass_alone_and_check_writes_nothing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = crate::sync::tests::Inert::new(scratch.path());
    assert_eq!(converge_run(&["--converge"], &ports), exit(0));
    assert_eq!(
        ports.backgrounds(),
        [crate::sync::Trigger::Init],
        "the runner lowers its QoS as it starts, as its trigger's pass"
    );
    assert!(crate::sync::read_record(scratch.path()).is_some());
    assert!(
        !ports.applies().is_empty(),
        "the pass applied its due kinds"
    );

    let checked = aterm_tempfile::tempdir().unwrap();
    let ports = crate::sync::tests::Inert::new(checked.path());
    assert_eq!(
        converge_run(&["--converge", "--check", "--json"], &ports),
        exit(0)
    );
    assert_eq!(ports.applies(), Vec::<&str>::new());
    assert_eq!(ports.base_applied(), Vec::<String>::new());
    let left: Vec<_> = std::fs::read_dir(checked.path()).unwrap().collect();
    assert!(left.is_empty(), "--check wrote {left:?}");
}

/// Pending admitted work has its own non-failure transport and an incomplete JSON report.
/// A locally ready Git resumes the same clone; an explicit group hold is settled instead.
#[test]
fn converge_transports_unfinished_repository_work_without_claiming_completion() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = crate::sync::tests::Inert::new(scratch.path());
    ports.clone_is(OwedClone::Waiting("Git is installing".into()));
    assert_eq!(
        converge_run(&["--converge", "--trigger", "subscriber", "--json"], &ports),
        exit(aterm_secrets::converge::UNFINISHED)
    );
    let pass = crate::sync::converge(
        &ports,
        crate::sync::Trigger::Subscriber,
        Mode::Plan,
        aterm_secrets::converge::RepositoryWork::Refresh,
        &mut |_| {},
    )
    .unwrap();
    let json = report_json(Action::Check, pass.complete(), pass.rows, &Laps::start()).unwrap();
    let report: aterm_json::Value = aterm_json::from_str(&json).unwrap();
    assert_eq!(report["complete"], aterm_json::Value::Bool(false));
    assert!(!ports.applies().contains(&"clone"));
    ports.clone_is(OwedClone::Due("managed Git is now ready".into()));
    assert_eq!(converge_run(&["--converge"], &ports), exit(0));
    assert!(ports.applies().contains(&"clone"));
    ports.clone_is(OwedClone::Held("repositories left as is".into()));
    assert_eq!(converge_run(&["--converge"], &ports), exit(0));
}

/// A subscriber's pass from a held copy is the installed app's: it runs `init --converge
/// --trigger subscriber` there with this run's flags, ends with that run's exit, and runs no
/// pass here. A window's or a person's pass never hands off, and a hand-off that cannot be
/// started, or that an app with no such pass refuses as usage, leaves this copy to run the
/// pass, saying so in aterm.log.
/// NEGATIVE CONTROL: a runner that never asks `installed_release_elsewhere` runs the
/// subscriber's pass here, from the held copy.
#[test]
fn a_held_copys_subscriber_pass_runs_from_the_installed_app() {
    let app = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut ports = crate::sync::tests::Inert::new(scratch.path());
    ports.elsewhere = Some(app.clone());
    *ports.elsewhere_exit.lock().unwrap() = Ok(Some(75));
    assert_eq!(
        converge_run(&["--converge", "--trigger", "subscriber", "--json"], &ports),
        exit(75),
        "the installed app's exit"
    );
    let words = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        *ports.ran_elsewhere.lock().unwrap(),
        [(app.clone(), {
            let mut args = words(&[
                "init",
                "--converge",
                "--trigger",
                "subscriber",
                "--repository-request",
            ]);
            args.push(
                aterm_json::to_string(&aterm_secrets::converge::RepositoryRequest::Refresh(
                    aterm_secrets::converge::RefreshRequest::after(None, Default::default()),
                ))
                .unwrap(),
            );
            args.push("--json".into());
            args
        })]
    );
    assert!(ports.applies().is_empty(), "no pass here");
    assert!(crate::sync::read_record(scratch.path()).is_none());

    for args in [&["--converge", "--trigger", "window"][..], &["--converge"]] {
        assert_eq!(converge_run(args, &ports), exit(0), "{args:?}");
    }
    assert_eq!(
        ports.ran_elsewhere.lock().unwrap().len(),
        1,
        "never handed off"
    );
    assert_eq!(
        ports.backgrounds(),
        [
            crate::sync::Trigger::Subscriber,
            crate::sync::Trigger::Window,
            crate::sync::Trigger::Init
        ],
        "each runner lowers itself as its own trigger's pass, the hand-off's first"
    );

    for (answer, said) in [
        (Err("No such file or directory".to_owned()), "did not start"),
        (Ok(Some(2)), "does not take the pass"),
    ] {
        let unstarted = aterm_tempfile::tempdir().unwrap();
        let mut ports = crate::sync::tests::Inert::new(unstarted.path());
        ports.elsewhere = Some(app.clone());
        *ports.elsewhere_exit.lock().unwrap() = answer;
        assert_eq!(
            converge_run(&["--converge", "--trigger", "subscriber"], &ports),
            exit(0)
        );
        assert!(!ports.applies().is_empty(), "this copy ran the pass");
        assert!(
            ports.logged().iter().any(|line| line.contains(said)),
            "{:?}",
            ports.logged()
        );
    }
}

/// Exit 75 while another pass holds the lock (applying nothing), 1 for a failed row, 2 for
/// a flag `--converge` does not take, before any port is read.
#[test]
fn converge_exits_75_on_a_held_lock_1_on_a_failed_row_and_2_on_usage() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = crate::sync::tests::Inert::new(scratch.path());
    let held = crate::sync::try_lock(scratch.path()).unwrap().unwrap();
    assert_eq!(converge_run(&["--converge"], &ports), exit(75));
    assert_eq!(ports.applies(), Vec::<&str>::new());
    drop(held);
    *ports.fleet_done.lock().unwrap() = Err("the issuer did not answer".to_owned());
    assert_eq!(converge_run(&["--converge"], &ports), exit(1));

    let untouched = crate::sync::tests::Inert::new(scratch.path());
    for args in [
        &["--converge", "--plan"][..],
        &["--converge", "--only", "base"],
        &["--converge", "--no-wait"],
        &["--converge", "--profile"],
        &["--converge", "--develop"],
        &["--converge", "--vault", "/tmp/vault"],
        &["--converge", "--trigger", "nightly"],
        &["--converge", "--trigger"],
        &["--trigger", "window"],
    ] {
        assert_eq!(converge_run(args, &untouched), exit(2), "{args:?}");
    }
    assert_eq!(untouched.backgrounds(), [], "a usage error reads nothing");
    assert_eq!(
        options(&[
            "--converge".into(),
            "--trigger=window".into(),
            "--check".into()
        ])
        .unwrap(),
        Options {
            action: Action::Check,
            converge: true,
            trigger: Some(crate::sync::Trigger::Window),
            ..Options::default()
        }
    );
}

#[test]
fn modes_are_exclusive_and_invalid_arguments_fail_before_probes() {
    assert!(options(&["--plan".into(), "--check".into()]).is_err());
    assert!(options(&["--unknown".into(), "--help".into()]).is_err());
    assert_eq!(
        options(&["--check".into(), "--json".into()]).unwrap(),
        Options {
            action: Action::Check,
            json: true,
            ..Options::default()
        }
    );
    let vault = |args: &[&str]| {
        options(&args.iter().map(OsString::from).collect::<Vec<_>>()).map(|o| o.vault)
    };
    assert_eq!(
        vault(&["--vault", "https://vault.example/v1/lab"])
            .unwrap()
            .as_deref(),
        Some("https://vault.example/v1/lab")
    );
    assert_eq!(
        vault(&["--vault=~/vault.txt"]).unwrap().as_deref(),
        Some("~/vault.txt")
    );
    assert!(vault(&["--vault"]).is_err());
    assert!(vault(&["--vault="]).is_err());
    assert!(vault(&["--vault", "--plan"]).is_err(), "a flag is no vault");
    assert!(
        vault_reaches(
            &options(&["--vault=~/vault.txt".into(), "--only".into(), "base".into()]).unwrap()
        )
        .is_err(),
        "the vault joins in the workstation layer"
    );
    assert_eq!(
        options(&[
            "--only".into(),
            "base.clt".into(),
            "--only=shell,pkg".into()
        ])
        .unwrap()
        .only,
        ["base.clt", "shell", "pkg"]
    );
    assert!(options(&["--only".into()]).is_err());
    assert!(options(&["--only=".into()]).is_err());
}

/// Every `aterm init` command this crate prints is one init takes: each [`Remedy::Init`]
/// it builds ([`crate::init_clt::place`], [`crate::init_spotlight::ID`], the base layer)
/// rendered by [`Remedy::command`], the one spelling every surface prints, and each one
/// its texts name. A word that names nothing, a value left off and a word without `--only`
/// are the negative controls.
#[test]
fn every_init_remedy_this_crate_prints_is_a_line_init_takes() {
    let base = Base {
        items: Vec::new(),
        held: None,
        clt: Some(crate::init_clt::CommandLineTools::new(
            Box::new(|_: &dyn System| crate::init_clt::Found::Present("cc".into())),
            PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
        )),
        toolset: None,
        targo: None,
    };
    let alice = crate::items::defaults::tests::user("alice", 501);
    let station = crate::init_spotlight::items(
        &crate::items::defaults::tests::ctx(&[alice]),
        crate::init_spotlight::Projects::Noted,
    )
    .unwrap();
    let line = |only: &str| Remedy::Init { only: only.into() }.command().unwrap();
    let mut lines = vec![
        crate::init_clt::place().remedy.command().unwrap(),
        line(crate::init_spotlight::ID),
        line(Layer::Base.name()),
        line(Layer::Workstation.name()),
        Remedy::Develop.command().unwrap(),
    ];
    let named = init_lines(crate::init_github::NEEDS_GIT);
    assert!(!named.is_empty());
    lines.extend(named);
    for line in &lines {
        assert_eq!(refusal(line, &base, &station), None, "{line}");
    }
    for refused in [
        line("init.spotlite"),
        line("base.cl"),
        "aterm init --only".to_owned(),
        "aterm init base.clt".to_owned(),
        "aterm pkg repair".to_owned(),
    ] {
        assert!(refusal(&refused, &base, &station).is_some(), "{refused}");
    }
    assert_eq!(
        init_lines("run `aterm init --only base.clt` in a terminal; or (aterm init --develop)"),
        ["aterm init --only base.clt", "aterm init --develop"]
    );
}

#[test]
fn only_narrows_the_base_layer_and_names_the_words_it_does_not_know() {
    let base = || Base {
        items: vec![Box::new(BaseItem {
            probes: Mutex::new(vec![ok()]),
            applied: std::sync::Arc::default(),
        }) as Box<dyn aterm_converge::Item>],
        held: None,
        clt: Some(crate::init_clt::CommandLineTools::new(
            Box::new(|_: &dyn System| crate::init_clt::Found::Present("cc".into())),
            PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
        )),
        toolset: None,
        targo: None,
    };
    assert_eq!(
        base().unnamed(&["base.clt".into(), "pkg".into(), "base".into()]),
        Vec::<String>::new()
    );
    assert_eq!(
        base().unnamed(&["workstation".into(), "base.cl".into()]),
        ["workstation", "base.cl"]
    );
    let clt = base().narrowed(&["base.clt".into()]);
    assert!(clt.items.is_empty() && clt.clt.is_some());
    let pkg = base().narrowed(&["pkg".into()]);
    assert_eq!(pkg.items.len(), 1);
    assert!(pkg.clt.is_none());
    let all = base().narrowed(&[]);
    assert!(all.items.len() == 1 && all.clt.is_some());
}

/// `base.clt` reads only once the base layer let its hold go, and off the console (no one
/// logged in at its screen, no terminal) its install waits for a person there: Deferred,
/// naming the console-presence reason, with nothing run.
#[test]
fn base_clt_runs_after_the_base_hold_is_released_and_waits_for_a_person_off_the_console() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Held(Arc<AtomicBool>);
    impl Drop for Held {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let released = Arc::new(AtomicBool::new(false));
    let saw = Arc::new(Mutex::new(Vec::new()));
    let base = Base {
        items: Vec::new(),
        held: Some(Box::new(Held(Arc::clone(&released)))),
        clt: Some(crate::init_clt::CommandLineTools::new(
            {
                let (released, saw) = (Arc::clone(&released), Arc::clone(&saw));
                Box::new(move |_: &dyn System| {
                    saw.lock().unwrap().push(released.load(Ordering::SeqCst));
                    crate::init_clt::Found::Missing("no developer directory".into())
                })
            },
            PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
        )),
        toolset: None,
        targo: None,
    };
    let alice = user("alice", 501);
    let ctx = Ctx {
        console: None,
        ..ctx(&[alice])
    };
    let sys = Fake::default();
    sys.running(&[]);
    let rows = run(&sys, &ctx, base, &[], Mode::Apply, &mut NoJournal).unwrap();
    // The tools' row, and the read-only row of the root steps their password does.
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1].id, crate::init_clt::ROOT_STEPS_ID);
    let row = &rows[0];
    assert_eq!(row.id, crate::init_clt::ID);
    assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
    assert_eq!(row.layer, Some(Layer::Base));
    assert_eq!(row.class, Some(Class::Elevate));
    let presence = Presence::NoConsole.needs_person().unwrap();
    assert!(
        row.why
            .as_deref()
            .is_some_and(|why| why.ends_with(&presence)),
        "{row:?}"
    );
    let saw = saw.lock().unwrap();
    assert!(
        !saw.is_empty() && saw.iter().all(|&released| released),
        "{saw:?}"
    );
    assert!(
        sys.calls()
            .iter()
            .all(|call| matches!(call.as_str(), "defer stops" | "stops restored")),
        "no machine command runs: {:?}",
        sys.calls()
    );

    // Ordinary explicit init aligns names only after the selected root row really
    // succeeded. The existing released-store premise remains in force at this boundary.
    let mut ready = row.clone();
    ready.outcome = Outcome::Ok;
    ready.why = None;
    let explicit = Options {
        only: vec![crate::init_clt::ID.into()],
        ..Options::default()
    };
    let publishes = std::cell::Cell::new(0);
    let publish = |current: &dyn Fn() -> Result<(), String>| {
        assert!(released.load(Ordering::SeqCst));
        current()?;
        publishes.set(publishes.get() + 1);
        Ok(Some("macOS names match m85 Dog / m85-dog".into()))
    };
    let aligned = finish_init_name(
        &explicit,
        Decider::Person,
        &sys,
        &NoRecords,
        &[ready.clone()],
        &publish,
    );
    assert!(
        aligned.is_some(),
        "successful explicit base.clt must finish its fresh naming projection"
    );
    assert_eq!(publishes.get(), 1);
    for (decided, action, only, root) in [
        (Decider::Aterm, Action::Apply, vec![], ready.clone()),
        (Decider::Person, Action::Check, vec![], ready.clone()),
        (Decider::Person, Action::Plan, vec![], ready.clone()),
        (
            Decider::Person,
            Action::Apply,
            vec!["keyboard".into()],
            ready.clone(),
        ),
        (
            Decider::Person,
            Action::Apply,
            vec!["github".into()],
            ready.clone(),
        ),
        (Decider::Person, Action::Apply, vec![], row.clone()),
    ] {
        let selected = Options {
            action,
            only,
            ..Options::default()
        };
        assert!(
            finish_init_name(&selected, decided, &sys, &NoRecords, &[root], &publish).is_none()
        );
    }
    assert_eq!(
        publishes.get(),
        1,
        "unasked, query, unrelated and deferred root lanes do not publish names"
    );
    let mut independent = ready.clone();
    independent.id = "github.repositories@alice".into();
    independent.class = Some(Class::Preference);
    independent.outcome = Outcome::Failed;
    assert!(
        finish_init_name(
            &explicit,
            Decider::Person,
            &sys,
            &NoRecords,
            &[ready.clone(), independent],
            &publish
        )
        .is_some(),
        "an independent repository failure does not suppress completed naming work"
    );
    let mut root_failed = ready.clone();
    root_failed.outcome = Outcome::Failed;
    assert!(
        finish_init_name(
            &explicit,
            Decider::Person,
            &sys,
            &NoRecords,
            &[ready.clone(), root_failed],
            &publish
        )
        .is_none()
    );
    assert_eq!(
        publishes.get(),
        2,
        "failed root work is never treated as a successful root return"
    );
    let mut deferred = ready.clone();
    deferred.id = crate::init_fseventsd::ID.into();
    deferred.outcome = Outcome::Deferred;
    assert!(
        finish_init_name(
            &explicit,
            Decider::Person,
            &sys,
            &NoRecords,
            &[ready, deferred],
            &publish
        )
        .is_some(),
        "an unrelated elevation-class row that never attempted root does not suppress names"
    );
    assert_eq!(publishes.get(), 3);
}

#[test]
fn repository_setup_does_not_wait_for_the_apple_tools_install() {
    use crate::init_toolset::{Found, Toolset};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;
    let wait = Duration::from_secs(60);
    let released = Arc::new(AtomicBool::new(false));
    let package_held = Arc::new(AtomicBool::new(false));
    let packages_ready = Arc::new(AtomicBool::new(false));
    let root_started = Arc::new(AtomicBool::new(false));
    let root_done = Arc::new(AtomicBool::new(false));
    let root_during_package = Arc::new(AtomicBool::new(false));
    let root_after_base = Arc::new(AtomicBool::new(false));
    let repositories_during_root = Arc::new(AtomicBool::new(false));
    let repositories_done = Arc::new(AtomicBool::new(false));
    let develop_done = Arc::new(AtomicBool::new(false));
    let signouts_done = Arc::new(AtomicBool::new(false));
    let (package_started, package_entered) = mpsc::sync_channel(1);
    let (root_entered, root_arrived) = mpsc::sync_channel(1);
    let (repositories_entered, repositories_arrived) = mpsc::sync_channel(1);
    struct Held(Arc<AtomicBool>);
    impl Drop for Held {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    struct Repositories {
        packages_ready: Arc<AtomicBool>,
        root_started: Arc<AtomicBool>,
        root_done: Arc<AtomicBool>,
        during_root: Arc<AtomicBool>,
        done: Arc<AtomicBool>,
        entered: mpsc::SyncSender<()>,
    }
    impl Item for Repositories {
        fn id(&self) -> String {
            "init.github.native@alice".into()
        }
        fn family(&self) -> &'static str {
            "github"
        }
        fn title(&self) -> String {
            "Repositories".into()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            self.during_root.store(
                self.packages_ready.load(Ordering::SeqCst)
                    && self.root_started.load(Ordering::SeqCst)
                    && !self.root_done.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            if !self.done.swap(true, Ordering::SeqCst) {
                let _ = self.entered.send(());
            }
            ok()
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            panic!("the repositories are already present")
        }
    }
    struct Finished {
        id: &'static str,
        root: Arc<AtomicBool>,
        repositories: Arc<AtomicBool>,
        develop: Option<Arc<AtomicBool>>,
        done: Arc<AtomicBool>,
    }
    impl Item for Finished {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            self.id()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            self.done.store(
                self.root.load(Ordering::SeqCst)
                    && self.repositories.load(Ordering::SeqCst)
                    && self
                        .develop
                        .as_ref()
                        .is_none_or(|done| done.load(Ordering::SeqCst)),
                Ordering::SeqCst,
            );
            ok()
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            panic!("the completion sentinel changes nothing")
        }
        fn after(&self) -> Vec<String> {
            if self.id.starts_with("develop.") {
                crate::init_develop::TOOLS_AFTER
                    .iter()
                    .map(|&id| id.into())
                    .collect()
            } else {
                Vec::new()
            }
        }
    }
    let mut sys = Fake::default();
    sys.euid = 501;
    sys.tty = true;
    sys.terminal = true;
    sys.graphic_session = Some(true);
    sys.admin = Some(true);
    sys.stat_of(
        "/dev/console",
        Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    )
    .running(&[]);
    sys.tty_model({
        let (released, package_held, root_started, root_done, during, after_base) = (
            Arc::clone(&released),
            Arc::clone(&package_held),
            Arc::clone(&root_started),
            Arc::clone(&root_done),
            Arc::clone(&root_during_package),
            Arc::clone(&root_after_base),
        );
        let package_entered = Mutex::new(package_entered);
        let repositories_arrived = Mutex::new(repositories_arrived);
        move |_, cmd| {
            assert_eq!(cmd.program, Path::new("/usr/bin/sudo"));
            assert!(cmd.args.iter().any(|word| word == "--plain"));
            let package_started = package_entered.lock().unwrap().recv_timeout(wait).is_ok();
            after_base.store(released.load(Ordering::SeqCst), Ordering::SeqCst);
            during.store(
                package_started && package_held.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            root_started.store(true, Ordering::SeqCst);
            let _ = root_entered.send(());
            let repositories_started = repositories_arrived
                .lock()
                .unwrap()
                .recv_timeout(wait)
                .is_ok();
            root_done.store(true, Ordering::SeqCst);
            Some(if repositories_started {
                Ran::ok("")
            } else {
                Ran::exit(
                    1,
                    "repositories did not progress while the root command was held",
                )
            })
        }
    });
    let base = Base {
        items: Vec::new(),
        held: Some(Box::new(Held(Arc::clone(&released)))),
        toolset: Some(Toolset::new(
            {
                let ready = Arc::clone(&packages_ready);
                Box::new(move || {
                    if ready.load(Ordering::SeqCst) {
                        Found::Complete("Git installed".into())
                    } else {
                        Found::Missing("Git is not installed".into())
                    }
                })
            },
            {
                let held = Arc::clone(&package_held);
                let ready = Arc::clone(&packages_ready);
                let root_arrived = Mutex::new(root_arrived);
                Box::new(move |args, _, _| {
                    if args[0] == "seed" {
                        held.store(true, Ordering::SeqCst);
                        let _ = package_started.send(());
                        let root_started = root_arrived.lock().unwrap().recv_timeout(wait).is_ok();
                        held.store(false, Ordering::SeqCst);
                        if !root_started {
                            return Err(crate::port::RunFailed::Refused(
                                "root setup could not start while the package pass was held".into(),
                            ));
                        }
                    } else {
                        assert_eq!(args[0], "update");
                        ready.store(true, Ordering::SeqCst);
                    }
                    Ok(Ran::ok(""))
                })
            },
            None,
            crate::init_toolset::tests::ATPKG,
        )),
        clt: Some(
            crate::init_clt::CommandLineTools::new(
                {
                    let ready = Arc::clone(&root_done);
                    Box::new(move |_| {
                        if ready.load(Ordering::SeqCst) {
                            crate::init_clt::Found::Present("compiler installed".into())
                        } else {
                            crate::init_clt::Found::Missing("compiler not installed".into())
                        }
                    })
                },
                PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
            )
            .saying(Arc::new(|_| {})),
        ),
        targo: None,
    };
    let groups: Vec<Group> = vec![
        (
            vec![Box::new(Repositories {
                packages_ready: Arc::clone(&packages_ready),
                root_started: Arc::clone(&root_started),
                root_done: Arc::clone(&root_done),
                during_root: Arc::clone(&repositories_during_root),
                done: Arc::clone(&repositories_done),
                entered: repositories_entered,
            })],
            place(Class::Install),
        ),
        (
            vec![Box::new(Finished {
                id: ACCOUNTS_GROUP,
                root: Arc::clone(&root_done),
                repositories: Arc::clone(&repositories_done),
                develop: Some(Arc::clone(&develop_done)),
                done: Arc::clone(&signouts_done),
            })],
            place(Class::Preference),
        ),
    ];
    let develop: Vec<(Box<dyn Item>, Place)> = vec![(
        Box::new(Finished {
            id: "develop.workspace-tools@~/aterm",
            root: Arc::clone(&root_done),
            repositories: Arc::clone(&repositories_done),
            develop: None,
            done: Arc::clone(&develop_done),
        }),
        crate::init_develop::place(Class::Install),
    )];
    let result = run_layers(
        &sys,
        &at_console(user("alice", 501)),
        base,
        &groups,
        &develop,
        &Narrowing {
            only: &[],
            record: false,
            relayed: false,
        },
        Mode::Apply,
        &mut NoJournal,
        &mut Laps::start(),
        &Hooks::QUIET,
    );
    // The ordinary run has joined every lane before a failed causal assertion unwinds.
    let rows = result.unwrap();
    assert!(
        root_during_package.load(Ordering::SeqCst),
        "root setup waited for the held package pass: {rows:?}"
    );
    assert!(root_after_base.load(Ordering::SeqCst));
    assert!(repositories_during_root.load(Ordering::SeqCst));
    assert!(develop_done.load(Ordering::SeqCst));
    assert!(signouts_done.load(Ordering::SeqCst));
}

/// `base.pkg.toolset` reads only once the base layer lets the store lock go (its passes
/// take it), beside `base.clt` and before the develop layer, whose tools run after both
/// whatever it ends as: once it installed the set, and while it installs nothing
/// (`auto_install = false`), since their own probe reads the `targo` they need. CONTROL:
/// with the toolset complete it runs no pass.
#[test]
fn the_toolset_installs_once_the_lock_is_let_go_and_the_develop_tools_follow_it() {
    use crate::init_toolset::{Found, Toolset, WAIT_LOCK};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    struct Held(Arc<AtomicBool>);
    impl Drop for Held {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    struct Tools {
        applied: Arc<AtomicU32>,
        after_toolset: bool,
    }
    impl Item for Tools {
        fn id(&self) -> String {
            "develop.workspace-tools@~/aterm".into()
        }
        fn family(&self) -> &'static str {
            "tooling"
        }
        fn title(&self) -> String {
            self.id()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            if self.applied.load(Ordering::SeqCst) == 0 {
                change()
            } else {
                ok()
            }
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            self.applied.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn after(&self) -> Vec<String> {
            let mut after: Vec<_> = crate::init_develop::TOOLS_AFTER
                .iter()
                .map(|&id| id.to_owned())
                .collect();
            if self.after_toolset {
                after.push(crate::init_toolset::ID.into());
            }
            after
        }
    }
    let missing = Found::Missing("none installed".into());
    let complete = Found::Complete("7 active".into());
    let off = Found::Off("auto_install off".into());
    for (reads, outcome, passes) in [
        (vec![missing, complete.clone()], Outcome::Changed, 2),
        (vec![off], Outcome::NotApplicable, 0),
        (vec![complete], Outcome::Ok, 0),
    ] {
        let released = Arc::new(AtomicBool::new(false));
        let saw = Arc::new(Mutex::new(Vec::new()));
        let ran = Arc::new(Mutex::new(Vec::new()));
        let toolset = Toolset::new(
            {
                let (released, saw) = (Arc::clone(&released), Arc::clone(&saw));
                let reads = Mutex::new(reads);
                Box::new(move || {
                    saw.lock().unwrap().push(released.load(Ordering::SeqCst));
                    let mut reads = reads.lock().unwrap();
                    if reads.len() > 1 {
                        reads.remove(0)
                    } else {
                        reads[0].clone()
                    }
                })
            },
            {
                let ran = Arc::clone(&ran);
                Box::new(move |args: &[String], _, _: &dyn Fn() -> bool| {
                    ran.lock().unwrap().push(args.join(" "));
                    Ok(Ran::ok(""))
                })
            },
            Some(WAIT_LOCK),
            crate::init_toolset::tests::ATPKG,
        );
        let base = Base {
            items: Vec::new(),
            held: Some(Box::new(Held(Arc::clone(&released)))),
            clt: Some(crate::init_clt::CommandLineTools::new(
                Box::new(|_: &dyn System| crate::init_clt::Found::Present("cc".into())),
                PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
            )),
            toolset: Some(toolset),
            targo: None,
        };
        let applied = Arc::new(AtomicU32::new(0));
        let develop: Vec<(Box<dyn Item>, Place)> = vec![(
            Box::new(Tools {
                applied: Arc::clone(&applied),
                after_toolset: false,
            }),
            crate::init_develop::place(Class::Install),
        )];
        let ctx = Ctx {
            console: None,
            ..ctx(&[user("alice", 501)])
        };
        let sys = Fake::default();
        let rows = run_layers(
            &sys,
            &ctx,
            base,
            &[],
            &develop,
            &Narrowing {
                only: &[],
                record: false,
                relayed: false,
            },
            Mode::Apply,
            &mut NoJournal,
            &mut Laps::start(),
            &Hooks::QUIET,
        )
        .unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                crate::init_clt::ID,
                crate::init_clt::ROOT_STEPS_ID,
                crate::init_toolset::ID,
                "develop.workspace-tools@~/aterm"
            ]
        );
        assert_eq!(rows[2].outcome, outcome, "{rows:?}");
        assert_eq!(rows[2].layer, Some(Layer::Base));
        assert_eq!(rows[2].class, Some(Class::Install));
        assert_eq!(rows[3].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(applied.load(Ordering::SeqCst), 1);
        let saw = saw.lock().unwrap();
        assert!(
            !saw.is_empty() && saw.iter().all(|&released| released),
            "read only once the lock was let go: {saw:?}"
        );
        let ran = ran.lock().unwrap();
        assert_eq!(ran.len(), passes, "{ran:?}");
        if passes > 0 {
            assert_eq!(
                *ran,
                [
                    "seed --wait-lock 1800",
                    "update --defer-busy-flip --wait-lock 1800"
                ]
            );
        }
        assert!(
            sys.calls()
                .iter()
                .all(|call| matches!(call.as_str(), "defer stops" | "stops restored")),
            "no other machine effect runs: {:?}",
            sys.calls()
        );
    }

    // Moving develop to its own stage must preserve actual failed predecessor outcomes
    // and their typed standing, without replaying an original probe or callback. Plan
    // mode still probes the dependent instead of suppressing it for an apply-only edge.
    for (mode, clt_refuses, toolset_locked) in [
        (Mode::Apply, true, false),
        (Mode::Plan, true, false),
        (Mode::Apply, false, true),
        (Mode::Plan, false, true),
    ] {
        let reads = Arc::new(AtomicU32::new(0));
        let base = Base {
            items: Vec::new(),
            held: None,
            clt: Some(crate::init_clt::CommandLineTools::new(
                {
                    let reads = Arc::clone(&reads);
                    Box::new(move |_| {
                        reads.fetch_add(1, Ordering::SeqCst);
                        if clt_refuses {
                            crate::init_clt::Found::Refuses("compiler refused".into())
                        } else {
                            crate::init_clt::Found::Present("cc".into())
                        }
                    })
                },
                "/Applications/aterm.app/Contents/MacOS/aterm".into(),
            )),
            toolset: toolset_locked.then(|| {
                Toolset::new(
                    Box::new(|| Found::Missing("tools not installed".into())),
                    Box::new(|_, _, _| Ok(Ran::exit(crate::init_toolset::CONTENDED, "store held"))),
                    None,
                    crate::init_toolset::tests::ATPKG,
                )
            }),
            targo: None,
        };
        let applied = Arc::new(AtomicU32::new(0));
        let develop: Vec<(Box<dyn Item>, Place)> = vec![(
            Box::new(Tools {
                applied: Arc::clone(&applied),
                after_toolset: toolset_locked,
            }),
            crate::init_develop::place(Class::Install),
        )];
        let heard = Mutex::new(Vec::new());
        let on_row = |row: &ItemReport| heard.lock().unwrap().push(row.id.clone());
        let rows = run_layers(
            &Fake::default(),
            &ctx(&[]),
            base,
            &[],
            &develop,
            &Narrowing {
                only: &[],
                record: false,
                relayed: false,
            },
            mode,
            &mut NoJournal,
            &mut Laps::start(),
            &Hooks {
                on_row: &on_row,
                ..Hooks::QUIET
            },
        )
        .unwrap();
        let dependent = rows
            .iter()
            .find(|row| row.id.starts_with("develop."))
            .unwrap();
        assert_eq!(
            dependent.outcome,
            if mode == Mode::Plan {
                Outcome::Planned
            } else {
                Outcome::Blocked
            }
        );
        assert_eq!(applied.load(Ordering::SeqCst), 0);
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "a completed root probe is not replayed"
        );
        let heard = heard.lock().unwrap();
        assert_eq!(heard.len(), rows.len());
        assert_eq!(
            heard
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            rows.len()
        );
        if mode == Mode::Apply && toolset_locked {
            let source = rows
                .iter()
                .find(|row| row.id == crate::init_toolset::ID)
                .unwrap();
            assert!(source.stood_aside.is_some());
            assert_eq!(dependent.stood_aside, source.stood_aside);
        }
    }
}

/// A run under another home or a private state root is `base.pkg.toolset`'s alone: never
/// a bare run, another base row (`base` takes the keychain's accounts and `base.clt`), the
/// workstation or the develop layer.
#[test]
fn only_the_toolset_row_goes_under_another_home() {
    let alone = |args: &[&str]| {
        options(&args.iter().map(OsString::from).collect::<Vec<_>>())
            .unwrap()
            .toolset_alone()
    };
    for args in [
        &["--only", "base.pkg.toolset"][..],
        &["--only", "base.pkg.toolset", "--check"],
        &["--only", "base.pkg.toolset,base.pkg.toolset", "--plan"],
    ] {
        assert!(alone(args), "{args:?}");
    }
    for args in [
        &[][..],
        &["--check"],
        &["--only", "base"],
        &["--only", "pkg"],
        &["--only", "base.pkg.toolset,base.clt"],
        &["--only", "workstation"],
        &["--only", "develop"],
        &["--only", "base.pkg.toolset", "--develop"],
    ] {
        assert!(!alone(args), "{args:?}");
    }
}

#[test]
fn cached_completion_of_held_scope_does_not_verify_an_explicit_profile_check() {
    let mut sys = alice_at(501);
    sys.terminal = false;
    sys.agent = Some("Codex");
    let (ctx, _) = local_context(&sys).unwrap();
    let options =
        options(&["--profile", "--check", "--no-sign-out", "--no-clone"].map(OsString::from))
            .unwrap();
    let mut partial = Selection::none();
    partial.set(Setting::CapsLock, true);
    for (verified, ready) in [
        (Selection::none(), false),
        (partial, false),
        (Selection::all().workstation(), true),
    ] {
        let mut standing = personal_standing();
        for setting in Setting::ALL
            .into_iter()
            .filter(|setting| setting.is_workstation())
        {
            standing.holds.off.set(setting, !verified.contains(setting));
        }
        // A coordinator whose groups are all left as is legitimately reports completion,
        // but has no observations proving the profile this command explicitly names.
        standing.workstation_observation = Some(completed(verified));
        let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
            crate::init_secrets::tests::unconfigured(),
        )]);
        let (groups, picked) = prepare_groups_with(
            &sys,
            &ctx,
            Mode::Plan,
            None,
            &Reading {
                keyboards: &KeyboardRead::EMPTY,
                context_scope: Selection::all(),
                only: &[],
                picks: options.picks,
            },
            &standing,
            no_grants,
            move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
        )
        .unwrap();
        assert!(
            picked.runs.contains(Setting::Dock),
            "the explicit profile overrides its earlier hold"
        );
        let away = groups
            .into_iter()
            .filter(|(items, _)| items.iter().any(|item| item.id() == AWAY))
            .collect::<Vec<_>>();
        let before = sys.calls();
        let rows = run(&sys, &ctx, Base::none(), &away, Mode::Plan, &mut NoJournal).unwrap();
        assert_eq!(
            complete(&rows, Action::Check),
            ready,
            "another observation scope cannot verify these preferences"
        );
        assert_eq!(
            sys.calls(),
            before,
            "the check must not re-probe private preferences"
        );
    }
}

#[test]
fn root_is_refused_without_running_any_command() {
    let sys = Fake::root();
    assert!(local_context(&sys).unwrap_err().contains("without sudo"));
    assert!(sys.calls().is_empty());
}

/// Alice's own shell, at a terminal in the console's graphic session, with the console
/// owned by `console_uid`: what `local_context` reads to resolve her.
fn alice_at(console_uid: u32) -> Fake {
    alice_with_keyboard_census(console_uid, "Services:\n\nDevices:\n")
}

fn alice_with_keyboard_census(console_uid: u32, census: &str) -> Fake {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
    sys.stat_of(
        "/dev/console",
        Stat {
            uid: console_uid,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    sys.on(
        &crate::facts::hidutil_keyboards().display(),
        Ran::ok(census),
    );
    sys
}

#[test]
fn attachment_activation_does_not_settle_an_unseen_requested_service() {
    let sys = alice_at(501);
    let invalidated = std::cell::Cell::new(0);
    assert!(
        matches!(
            activate_keyboard_attachments(&sys, &[41], &|_| false, &|| true, &|| invalidated
                .set(invalidated.get() + 1))
            .unwrap(),
            KeyboardAttachmentActivation::Unfinished
        ),
        "a native service missing from the separate model census remains retryable"
    );
    assert_eq!(
        invalidated.get(),
        1,
        "uncertain coverage retires an older reading once"
    );
    assert!(sys.keyboard_changes().is_empty());
}

#[test]
fn saved_caps_preferences_do_not_verify_an_unseen_native_keyboard() {
    use aterm_types::macos_setup::{Observation, Standing as SettingStanding, Subject};

    let mut sys = alice_at(501);
    sys.keyboard_services = Some(Ok(vec![41]));
    let mapping = crate::items::keyboard::Model {
        vendor: 0,
        product: 0,
    }
    .key();
    let saved = crate::plist::document(&PValue::Dict(vec![(
        mapping,
        PValue::Array(vec![crate::items::keyboard::caps_to_control()]),
    )]));
    sys.on(
        "alice$ /usr/bin/defaults -currentHost export -g -",
        Ran::ok(&saved),
    );
    sys.on(
        "/usr/bin/defaults export /Library/Preferences/com.apple.keyboardtype -",
        export("<dict/>"),
    );
    assert_eq!(sys.keyboard_service_ids().unwrap(), [41]);
    assert!(
        !matches!(
            activate_keyboard_attachments(&sys, &[41], &|_| false, &|| true, &|| {}),
            Ok(KeyboardAttachmentActivation::Verified(_))
        ),
        "attachment activation still owes the service absent from the model census"
    );

    let mut selected = Selection::none();
    selected.set(Setting::CapsLock, true);
    let observed = prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
        .and_then(|pass| pass.run_tagged(&sys, Mode::Plan))
        .map_or_else(
            |_| Observation::unknown(Subject::Setting(Setting::CapsLock)),
            |rows| {
                crate::init_observe::observation(
                    Setting::CapsLock,
                    &rows.iter().map(|(_, row)| row).collect::<Vec<_>>(),
                )
            },
        );
    assert_ne!(
        observed.standing,
        SettingStanding::InPlace,
        "saved preferences cannot confirm a keyboard its live census did not observe"
    );
    assert_ne!(observed.now.as_deref(), Some("Control"));
    assert!(sys.keyboard_changes().is_empty());
    assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
}

#[test]
fn a_partial_live_mapping_read_cannot_verify_the_whole_keyboard_model() {
    use crate::items::keyboard::{CAPS_LOCK, LEFT_CONTROL, Model};
    use crate::port::Cmd;
    use aterm_types::macos_setup::Standing as SettingStanding;

    let census = format!(
        "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n{:<12}{:<12}{:<16}{:<24}0\n{:<12}{:<12}{:<16}{:<24}0\n\nDevices:\n",
        "VendorID",
        "ProductID",
        "RegistryID",
        "Product",
        "0xfffe",
        "0xa9",
        "0x29",
        "Fixture keyboard",
        "0xfffe",
        "0xa9",
        "0x2a",
        "Fixture keyboard"
    );
    let mut sys = alice_with_keyboard_census(501, &census);
    sys.keyboard_services = Some(Ok(vec![41, 42]));
    let who = crate::items::defaults::tests::user("alice", 501);
    let models = [
        Model {
            vendor: 0,
            product: 0,
        },
        Model {
            vendor: 65534,
            product: 169,
        },
    ];
    let saved = crate::plist::document(&PValue::Dict(
        models
            .into_iter()
            .map(|model| {
                (
                    model.key(),
                    PValue::Array(vec![crate::items::keyboard::caps_to_control()]),
                )
            })
            .collect(),
    ));
    sys.on(
        "alice$ /usr/bin/defaults -currentHost export -g -",
        Ran::ok(&saved),
    );
    sys.on(
        "/usr/bin/defaults export /Library/Preferences/com.apple.keyboardtype -",
        export("<dict/>"),
    );
    let property = Cmd::new(aterm_uds::os_path!("hidutil"))
        .args([
            "property",
            "--matching",
            &crate::port::keyboard_usage::matching_model(65534, 169),
            "--get",
            "UserKeyMapping",
        ])
        .as_user(&who);
    sys.on(&property.display(), Ran::ok("RegistryID Key Value\n2a UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n"));
    sys.on(&property.display(), Ran::ok("RegistryID Key Value\n29 UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n2a UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n"));
    let dump = Cmd::new(aterm_uds::os_path!("hidutil"))
        .args(["dump", "services", "-f", "xml"])
        .as_user(&who);
    sys.on(
        &dump.display(),
        Ran::ok(&crate::init_preferences::tests::hid_dump(&[(
            42,
            &[],
            &[(CAPS_LOCK, LEFT_CONTROL)],
        )])),
    );
    sys.on(
        &dump.display(),
        Ran::ok(&crate::init_preferences::tests::hid_dump(&[
            (41, &[], &[(CAPS_LOCK, LEFT_CONTROL)]),
            (42, &[], &[(CAPS_LOCK, LEFT_CONTROL)]),
        ])),
    );
    let mut selected = Selection::none();
    selected.set(Setting::CapsLock, true);
    let rows = prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
        .unwrap()
        .run_tagged(&sys, Mode::Plan)
        .unwrap();
    let observed = crate::init_observe::observation(
        Setting::CapsLock,
        &rows.iter().map(|(_, row)| row).collect::<Vec<_>>(),
    );
    assert_ne!(
        observed.standing,
        SettingStanding::InPlace,
        "one mapped service cannot verify its model's other unread native service"
    );
    assert_ne!(observed.now.as_deref(), Some("Control"));
    let recovered = prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
        .unwrap()
        .run_tagged(&sys, Mode::Plan)
        .unwrap();
    let recovered = crate::init_observe::observation(
        Setting::CapsLock,
        &recovered.iter().map(|(_, row)| row).collect::<Vec<_>>(),
    );
    assert_eq!(recovered.standing, SettingStanding::InPlace);
    assert_eq!(recovered.now.as_deref(), Some("Control"));
    assert!(sys.keyboard_changes().is_empty());
}

#[test]
fn an_unconfirmed_keyboard_leaves_other_selected_settings_readable() {
    use aterm_types::macos_setup::Standing as SettingStanding;

    for native in [
        Ok(vec![41]),
        Err("native census is unread".into()),
        Ok(Vec::new()),
    ] {
        let mut sys = alice_at(501);
        sys.keyboard_services = Some(native.clone());
        saved_off(&sys);
        sys.scroll_state(false);
        let mapping = crate::items::keyboard::Model {
            vendor: 0,
            product: 0,
        }
        .key();
        sys.on(
            "alice$ /usr/bin/defaults -currentHost export -g -",
            Ran::ok(&crate::plist::document(&PValue::Dict(vec![(
                mapping,
                PValue::Array(vec![crate::items::keyboard::caps_to_control()]),
            )]))),
        );
        sys.on(
            "/usr/bin/defaults export /Library/Preferences/com.apple.keyboardtype -",
            export("<dict/>"),
        );
        let mut selected = Selection::none();
        selected.set(Setting::CapsLock, true);
        selected.set(Setting::NaturalScrolling, true);
        let rows = prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
            .unwrap()
            .run_tagged(&sys, Mode::Plan)
            .unwrap();
        for setting in [Setting::CapsLock, Setting::NaturalScrolling] {
            let observed = crate::init_observe::observation(
                setting,
                &rows
                    .iter()
                    .filter(|(tag, _)| *tag == setting)
                    .map(|(_, row)| row)
                    .collect::<Vec<_>>(),
            );
            let confirmed = setting != Setting::CapsLock || native == Ok(Vec::new());
            assert_eq!(
                observed.standing,
                if confirmed {
                    SettingStanding::InPlace
                } else {
                    SettingStanding::Unknown
                },
                "{setting:?}, {native:?}"
            );
        }
        assert!(sys.keyboard_changes().is_empty());
        assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
    }

    let mut sys = alice_at(501);
    sys.keyboard_services = Some(Err("Caps is not selected; must not read".into()));
    saved_off(&sys);
    sys.scroll_state(false);
    let mut selected = Selection::none();
    selected.set(Setting::NaturalScrolling, true);
    prepare_workstation_selected(&sys, WorkstationGroup::All, selected)
        .unwrap()
        .run_tagged(&sys, Mode::Plan)
        .unwrap();
    assert!(
        sys.calls()
            .iter()
            .all(|call| { !call.contains("hidutil") && call != "keyboard-service-ids" })
    );
}

#[test]
fn held_attachments_cover_only_matching_services_without_probing_preferences() {
    let census = format!(
        "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n{:<12}{:<12}{:<16}{:<24}0\n{:<12}{:<12}{:<16}{:<24}0\n\nDevices:\n",
        "VendorID",
        "ProductID",
        "RegistryID",
        "Product",
        "0xfffe",
        "0xa9",
        "0x29",
        "Fixture keyboard",
        "0xfffe",
        "0xa9",
        "0x2a",
        "Fixture keyboard"
    );
    let mut sys = alice_with_keyboard_census(501, &census);
    sys.keyboard_services = Some(Ok(vec![41, 42, 43]));
    let who = crate::items::defaults::tests::user("alice", 501);
    let get = crate::port::Cmd::new(aterm_uds::os_path!("hidutil"))
        .args([
            "property",
            "--matching",
            &crate::port::keyboard_usage::matching_model(65534, 169),
            "--get",
            "UserKeyMapping",
        ])
        .as_user(&who);
    sys.on(
        &get.display(),
        Ran::ok("RegistryID Key Value\n29 UserKeyMapping (null)\n"),
    );
    let invalidated = std::cell::Cell::new(0);
    assert!(matches!(
        activate_keyboard_attachments(&sys, &[41], &|_| true, &|| true, &|| invalidated
            .set(invalidated.get() + 1))
        .unwrap(),
        KeyboardAttachmentActivation::Verified(_)
    ));
    assert_eq!(
        invalidated.get(),
        1,
        "kept requested coverage is no whole-Caps proof"
    );
    invalidated.set(0);
    assert!(
        matches!(
            activate_keyboard_attachments(&sys, &[41, 42], &|_| true, &|| true, &|| {
                invalidated.set(invalidated.get() + 1)
            })
            .unwrap(),
            KeyboardAttachmentActivation::Unfinished
        ),
        "an observed held model cannot cover an unseen requested service"
    );
    assert_eq!(invalidated.get(), 1, "kept and uncertain coverage coalesce");
    assert!(sys.keyboard_changes().is_empty());
    assert!(
        sys.calls()
            .iter()
            .all(|call| { !call.contains("defaults") && !call.contains("dump services") })
    );
}

#[test]
fn a_noninteractive_check_relays_background_evidence_without_probing_preferences() {
    let mut sys = alice_at(501);
    sys.terminal = false;
    sys.agent = None;
    let (ctx, _) = local_context(&sys).unwrap();
    let mut standing = personal_standing();
    standing.workstation_observation = Some(completed(Selection::all().workstation()));
    let before = sys.calls();
    for (mode, outcome) in [(Mode::Plan, Outcome::Ok), (Mode::Apply, Outcome::Deferred)] {
        let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
            crate::init_secrets::tests::unconfigured(),
        )]);
        let (mut groups, _) = prepare_groups_with(
            &sys,
            &ctx,
            mode,
            None,
            &reading(&[]),
            &standing,
            no_grants,
            move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
        )
        .unwrap();
        groups.retain(|(items, _)| items.iter().any(|item| item.id() == AWAY));
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let rows = run(&sys, &ctx, Base::none(), &groups, mode, &mut journal).unwrap();
        let row = rows.iter().find(|row| row.id == AWAY).unwrap();
        assert_eq!(row.outcome, outcome);
        if mode == Mode::Plan {
            assert_eq!(status(&rows, Action::Check), 0);
        }
    }
    assert_eq!(
        sys.calls(),
        before,
        "neither path reads or changes other apps"
    );
    for action in [Action::Apply, Action::Plan] {
        assert!(
            observed_workstation(&sys, action, Selection::all(), &|_| panic!(
                "a non-check queried observations"
            ))
            .is_none()
        );
    }
    assert_eq!(
        observed_workstation(&sys, Action::Check, Selection::all(), &|_| {
            completed(Selection::all().workstation())
        }),
        Some(completed(Selection::all().workstation()))
    );
    let other = alice_at(502);
    assert!(
        observed_workstation(&other, Action::Check, Selection::all(), &|_| panic!(
            "another login queried observations"
        ))
        .is_none()
    );
}

/// A relayed completion is a reading taken some time ago, and the check says how long
/// (design §3.5): a minute before the fake's clock is told as 60 s, never as now.
#[test]
fn a_relayed_completion_says_how_old_its_reading_is() {
    let sys = alice_at(501);
    let (ctx, _) = local_context(&sys).unwrap();
    let groups = vec![away_from_console(
        Presence::AgentTab,
        Some(completed(Selection::all().workstation())),
    )];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    let rows = run(&sys, &ctx, Base::none(), &groups, Mode::Plan, &mut journal).unwrap();
    assert_eq!(status(&rows, Action::Check), 0);
    let said = format!("{:?} {:?}", rows[0].to, rows[0].why);
    assert!(said.contains(&age_words(60)), "{said}");
    assert!(
        !said.contains("last read"),
        "a standing reading is one of now: {said}"
    );
}

/// A finished Mac nobody watches: its rows were read back as wanted two hours ago and
/// nothing has read them since (no person at System State). The check passes on that
/// reading, says it is the last one and how old, and never says the rows are in place now
/// (design §3.5); before this it exited 77 with them unread, forever. What the read-back
/// names of the rows goes with it: one set for the next login, and one changed since aterm
/// set it, which is never said read as wanted, nor said changed by the person (H19: nothing
/// says who changed it).
#[test]
fn a_finished_mac_read_back_long_ago_passes_as_set_up_when_last_read() {
    let sys = alice_at(501);
    let (ctx, _) = local_context(&sys).unwrap();
    let mut trackpad = Selection::none();
    trackpad.set(Setting::TrackpadScrolling, true);
    let mut dock = Selection::none();
    dock.set(Setting::Dock, true);
    let read_back = ReadBack {
        at_login: trackpad,
        changed: dock,
        ..read_back(Selection::all().workstation(), 2 * 3600)
    };
    let groups = vec![away_from_console(
        Presence::AgentTab,
        Some(WorkstationObservation::Complete(read_back.clone())),
    )];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    let rows = run(&sys, &ctx, Base::none(), &groups, Mode::Plan, &mut journal).unwrap();
    assert_eq!(status(&rows, Action::Check), 0);
    let said = format!("{:?} {:?}", rows[0].to, rows[0].why);
    assert!(
        said.contains(&format!(
            "last read the requested preference rows other than Dock, {} ago",
            age_words(7200)
        )),
        "{said}"
    );
    assert!(said.contains(READ_THEM_NOW), "who reads them again: {said}");
    assert!(
        said.contains("Trackpad scrolling takes full effect at your next login")
            && said.contains("the requested preference rows other than Dock, ")
            && said.contains("Dock changed since aterm set it, which setup leaves as it is")
            && !said.contains("you changed"),
        "{said}"
    );
    // A reading dated after the clock (it moved back) has no age to tell.
    assert!(
        read_back
            .said("rows", Some(1_800_000_000 - 3 * 3600))
            .contains("at a time this Mac's clock cannot date")
    );
}

/// A relayed read-back with a row changed since aterm set it reads as left as it is (`not
/// applicable`), as at the console a check reads a key left as it is, never as `ok`; where
/// the changed row is every row read back, it is never said read as wanted. Before, the row
/// read `ok` with the Dock changed. NEGATIVE CONTROL: the same read-back with nothing changed
/// reads `ok`.
#[test]
fn a_relayed_read_back_with_a_changed_row_is_left_as_it_is_never_ok() {
    let sys = alice_at(501);
    let (ctx, _) = local_context(&sys).unwrap();
    let mut dock = Selection::none();
    dock.set(Setting::Dock, true);
    for (changed, outcome) in [
        (dock, Outcome::NotApplicable),
        (Selection::none(), Outcome::Ok),
    ] {
        let read_back = ReadBack {
            changed,
            ..read_back(Selection::all().workstation(), 60)
        };
        let groups = vec![away_from_console(
            Presence::AgentTab,
            Some(WorkstationObservation::Complete(read_back)),
        )];
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let rows = run(&sys, &ctx, Base::none(), &groups, Mode::Plan, &mut journal).unwrap();
        assert_eq!(rows[0].outcome, outcome, "{:?}", rows[0]);
        assert_eq!(status(&rows, Action::Check), 0);
    }
    let only_dock = ReadBack {
        changed: dock,
        ..read_back(dock, 60)
    };
    assert_eq!(
        only_dock.said("the requested preference rows", Some(1_800_000_000)),
        format!(
            "System State read Dock {} ago; Dock changed since aterm set it, which setup \
             leaves as it is",
            age_words(60)
        )
    );
    let said = only_dock.said("rows", Some(1_800_000_000 + 2 * 3600));
    assert!(
        said.starts_with("System State last read Dock, ") && !said.contains("as wanted"),
        "{said}"
    );
}

/// A read-back dated after this Mac's clock (the clock moved back since it was read) has no
/// age to tell, so the check never passes on it: it is unread here (77), in those words,
/// never ok (the review found it exited 0). NEGATIVE CONTROL: the same read-back dated at
/// the clock passes.
#[test]
fn a_read_back_the_clock_cannot_date_is_unread_never_a_pass() {
    let sys = alice_at(501);
    let (ctx, _) = local_context(&sys).unwrap();
    for (at, code) in [(1_800_000_000 + 3600, 77), (1_800_000_000, 0)] {
        let read_back = ReadBack {
            rows: Selection::all().workstation(),
            read_at: Some(at),
            ..ReadBack::default()
        };
        let groups = vec![away_from_console(
            Presence::AgentTab,
            Some(WorkstationObservation::Complete(read_back)),
        )];
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let rows = run(&sys, &ctx, Base::none(), &groups, Mode::Plan, &mut journal).unwrap();
        assert_eq!(status(&rows, Action::Check), code, "read at {at}");
        if code == 77 {
            let said = format!("{:?} {:?}", rows[0].to, rows[0].why);
            assert!(
                said.contains("at a time this Mac's clock cannot date"),
                "{said}"
            );
        }
    }
}

/// A relayed check is scoped to the rows it asks about: a row it does not ask about,
/// unread or not, says nothing of it, so a check of verified rows alone passes on them; one
/// that asks about an unread row is told just the rows it asked about.
#[test]
fn an_unread_relay_names_only_the_rows_the_check_asks_about() {
    let dock = {
        let mut dock = Selection::none();
        dock.set(Setting::Dock, true);
        dock
    };
    let unread = |rows: &str, verified: Selection| WorkstationObservation::Unread {
        rows: rows.into(),
        read_back: read_back(verified, 60),
    };
    assert_eq!(
        unread("capslock,spotlight", dock).for_scope(dock),
        completed(dock),
        "the check's own rows are verified"
    );
    assert!(
        matches!(
            unread("spotlight", Selection::none()).for_scope(dock),
            WorkstationObservation::Waiting(_)
        ),
        "nothing verified the Dock: still a wait"
    );
    assert_eq!(
        unread("dock,spotlight", Selection::none()).for_scope(dock),
        unread("dock", Selection::none()),
        "it is told only the rows it asked about"
    );
    let all = Selection::all().workstation();
    assert_eq!(
        unread("capslock,dock", Selection::none()).for_scope(all),
        unread("capslock,dock", Selection::none())
    );
}

/// A reading's age is told rounded up, never as newer than it is and never a whole unit
/// older, in the unit its size picks.
#[test]
fn a_reading_age_is_never_told_as_newer() {
    for secs in (0..20_000).chain([u64::MAX / 2, u64::MAX]) {
        let (count, unit) = age_told(secs);
        assert!(
            count.saturating_mul(unit.secs()) >= secs,
            "{secs}: never newer"
        );
        assert!(
            count == 0 || (count - 1) * unit.secs() < secs,
            "{secs}: rounded up by less than one {unit:?}"
        );
        let want = if secs < 120 {
            AgeUnit::Secs
        } else if secs < 7200 {
            AgeUnit::Mins
        } else {
            AgeUnit::Hours
        };
        assert_eq!(unit, want, "{secs}");
    }
}

#[test]
fn background_check_distinguishes_waiting_permission_and_missing_evidence() {
    let sys = alice_at(501);
    let (ctx, _) = local_context(&sys).unwrap();
    let before = sys.calls();
    for (observation, code) in [
        (WorkstationObservation::Waiting("work continues".into()), 75),
        (
            WorkstationObservation::Permission("a real grant is off".into()),
            77,
        ),
        (
            WorkstationObservation::Unavailable("not observed".into()),
            1,
        ),
        // Rows nothing read are neither done nor a wait: a person's console reads them.
        (
            WorkstationObservation::Unread {
                rows: "capslock,dock".into(),
                read_back: ReadBack::default(),
            },
            77,
        ),
    ] {
        let unread = matches!(observation, WorkstationObservation::Unread { .. });
        let groups = vec![away_from_console(Presence::AgentTab, Some(observation))];
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let rows = run(&sys, &ctx, Base::none(), &groups, Mode::Plan, &mut journal).unwrap();
        assert_eq!(status(&rows, Action::Check), code);
        assert_eq!(rows[0].class, Some(Class::Relay));
        if unread {
            assert_eq!(rows[0].outcome, Outcome::Deferred);
            let why = rows[0].why.as_deref().unwrap_or_default();
            assert!(why.contains("capslock,dock"), "the rows are named: {why}");
        }
    }
    assert_eq!(sys.calls(), before);
}

#[test]
fn under_another_console_owner_a_base_item_applies_and_a_workstation_item_waits() {
    let refused = prepare_workstation(&alice_at(502), WorkstationGroup::Preferences)
        .err()
        .expect("refused away from the console");
    assert!(refused.contains("at this Mac's console"), "{refused}");
    for (console_uid, at_console) in [(502, false), (501, true)] {
        let sys = alice_at(console_uid);
        let (ctx, _) = local_context(&sys).unwrap();
        assert_eq!(ctx.console.is_some(), at_console);
        let applied = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let base = Base {
            items: vec![Box::new(BaseItem {
                probes: Mutex::new(vec![change(), ok()]),
                applied: std::sync::Arc::clone(&applied),
            })],
            held: None,
            clt: None,
            toolset: None,
            targo: None,
        };
        let groups = if at_console {
            vec![(
                vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                place(Class::Preference),
            )]
        } else {
            let before = sys.calls();
            let groups = prepared(&sys, &ctx);
            assert_eq!(
                sys.calls(),
                before,
                "no workstation preparation off the console"
            );
            assert_eq!(groups[0].0[0].id(), "init.workstation");
            groups
        };
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mut journal = FileJournal::under(scratch.path());
        let rows = run(&sys, &ctx, base, &groups, Mode::Apply, &mut journal).unwrap();
        assert_eq!(rows[0].id, "base.pkg.shims");
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(applied.load(std::sync::atomic::Ordering::SeqCst), 1);
        if at_console {
            assert_eq!(rows[1].outcome, Outcome::Changed, "{rows:?}");
        } else {
            // Another login's workstation waits for a person at its console: the base
            // layer's run is done, and only `--check` reads the deferred row as unmet.
            assert_eq!(rows[1].outcome, Outcome::Deferred, "{rows:?}");
            assert_eq!(rows[1].why, Presence::OtherLogin.needs_person());
            assert!(!sys.calls().iter().any(|call| call.contains("defaults")
                || call.contains("hidutil")
                || call.contains("accounts.data")));
            assert_eq!(status(&rows, Action::Apply), 0);
            assert_eq!(status(&rows, Action::Plan), 0);
            assert_eq!(status(&rows, Action::Check), 77);
        }
    }
    assert!(
        local_context(&Fake::root())
            .unwrap_err()
            .contains("without sudo")
    );
}

/// An SSH session of the console's own login is not the person at it: nothing of the
/// console session is read, and the workstation waits for them as under another login.
#[test]
fn over_ssh_the_console_owners_workstation_waits_for_a_person_at_it() {
    for (remote_login, graphic, at_console) in [
        (true, Some(true), false),
        (false, Some(false), false),
        (false, Some(true), true),
    ] {
        let mut sys = alice_at(501);
        sys.remote_login = remote_login;
        sys.graphic_session = graphic;
        let (ctx, _) = local_context(&sys).unwrap();
        assert_eq!(
            ctx.console.is_some(),
            at_console,
            "{remote_login} {graphic:?}"
        );
        if at_console {
            continue;
        }
        // `aterm doctor`'s and System State's preparation says why, not to log in.
        let refused = prepare_workstation(&sys, WorkstationGroup::Preferences)
            .err()
            .expect("refused outside the console's session");
        assert!(
            refused.contains(Presence::Remote.why().unwrap()),
            "{refused}"
        );
        let groups = prepared(&sys, &ctx);
        let rows = run(
            &sys,
            &ctx,
            Base::none(),
            &groups,
            Mode::Apply,
            &mut NoJournal,
        )
        .unwrap();
        let away = rows
            .iter()
            .find(|row| row.id == AWAY)
            .expect("the stand-in");
        assert_eq!(away.outcome, Outcome::Deferred);
        assert_eq!(away.why, Presence::Remote.needs_person());
        // Beside it run the background rows an agent's tab runs (design §7): Rust in
        // Spotlight, the vault's join (here the vault's address holds none) and the GitHub
        // repositories,
        // and nothing of the console session.
        let background = ["init.spotlight.", "init.secrets@", "init.github."];
        assert!(
            rows.iter()
                .all(|row| row.id == AWAY
                    || background.iter().any(|lane| row.id.starts_with(lane))),
            "{rows:?}"
        );
        for lane in background {
            assert!(
                rows.iter().any(|row| row.id.starts_with(lane)),
                "{lane}: {rows:?}"
            );
        }
        let secrets = rows
            .iter()
            .find(|row| row.id.starts_with("init.secrets@"))
            .expect("the background vault join");
        assert_eq!(secrets.outcome, Outcome::Blocked, "{rows:?}");
        assert!(matches!(
            secrets.stood_aside,
            Some(aterm_converge::StoodAside::Absent(_))
        ));
        assert_eq!(status(&rows, Action::Apply), 69, "{rows:?}");
        assert!(
            !sys.calls().iter().any(|call| {
                [
                    "hidutil",
                    "defaults",
                    "killall",
                    "osascript",
                    "accounts.data",
                ]
                .iter()
                .any(|console| call.contains(console))
            }),
            "{:?}",
            sys.calls()
        );
    }
}

type BoundFixture = (
    Vec<(String, Class)>,
    Vec<Group>,
    Ctx,
    Arc<crate::init_secrets::tests::Scripted>,
);

/// The rows a run of `sys` binds, standing as `standing` says, with an unconfigured vault
/// whose look answers as ALab's open vault when `alab`; the groups, context and vault port
/// to read back.
fn bound_for(sys: &Fake, standing: &Standing<'_>, alab: bool) -> BoundFixture {
    let (ctx, _) = local_context(sys).unwrap();
    let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
        crate::init_secrets::tests::unconfigured(),
    )]);
    if alab {
        vault.alab_answers();
    }
    let port = Arc::clone(&vault);
    let groups = prepare_groups_with(
        sys,
        &ctx,
        Mode::Apply,
        None,
        &reading(&[]),
        standing,
        no_grants,
        move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
    )
    .unwrap()
    .0;
    let bound = crate::engine::bind(
        groups.iter().flat_map(|(items, place)| {
            items.iter().map(move |item| (item.as_ref(), place.clone()))
        }),
        sys,
        &ctx,
    )
    .iter()
    .map(|b| (b.id(), b.class()))
    .collect::<Vec<_>>();
    (bound, groups, ctx, vault)
}

/// With no terminal (an agent's tool call, a launch agent, a script), in the console's own
/// session, init runs the base layer and looks for the vault, and signs out of Messages and
/// FaceTime, which nothing undoes, only for a person at the screen now (input within the
/// last minute, the screen unlocked); with nobody there they wait. An agent's local setup
/// runs the selected profile. Rust in Spotlight runs for each; no preference
/// runs off the console: no `killall`, no `defaults write`. Control: a person at the console
/// gets every row.
#[test]
fn with_no_terminal_the_sign_outs_wait_for_a_person_at_the_screen() {
    let personal = [Class::Preference];
    for (terminal, agent, presence, present) in [
        (false, None, Presence::NoTerminal, false),
        (false, None, Presence::NoTerminal, true),
        (true, Some("Claude Code"), Presence::AgentTab, true),
        (true, None, Presence::AtConsole, false),
    ] {
        let mut sys = alice_at(501);
        sys.terminal = terminal;
        sys.agent = agent;
        assert_eq!(crate::console::presence(&sys), presence);
        let standing = Standing {
            decided: Decider::Person,
            alab: &|| true,
            present: if present { &|| true } else { &|| false },
            holds: Holds::default(),
            workstation_observation: None,
        };
        let (bound, groups, ctx, vault) = bound_for(&sys, &standing, false);
        assert!(ctx.console.is_some(), "the console's own session");
        assert_eq!(
            vault.probes(),
            0,
            "{presence:?} prepares without looking for the ALab vault"
        );
        assert!(
            bound
                .iter()
                .any(|(id, _)| id == "init.spotlight.rustup-profile@alice"),
            "{presence:?}: {bound:?}"
        );
        if init_presence(&sys) == Presence::AtConsole {
            assert!(
                bound.iter().any(|(_, class)| *class == Class::Preference),
                "{bound:?}"
            );
            continue;
        }
        let personal_rows = bound
            .iter()
            .filter(|(_, class)| personal.contains(class))
            .collect::<Vec<_>>();
        if presence == Presence::NoTerminal && present {
            assert_eq!(personal_rows.len(), 2, "{bound:?}");
            assert!(
                personal_rows
                    .iter()
                    .all(|(id, _)| crate::init_accounts::signs_out(id)),
                "{bound:?}"
            );
        } else {
            assert!(personal_rows.is_empty(), "{presence:?}: {bound:?}");
        }
        let rows = run(
            &sys,
            &ctx,
            Base::none(),
            &groups,
            Mode::Apply,
            &mut NoJournal,
        )
        .unwrap();
        assert_eq!(vault.probes(), 1, "{presence:?} looks during apply");
        let away = rows
            .iter()
            .find(|row| row.id == AWAY)
            .expect("the stand-in");
        assert_eq!(away.outcome, Outcome::Deferred);
        assert_eq!(away.why, presence.needs_person());
        let calls = sys.calls();
        for change in ["killall", "defaults write"] {
            assert!(
                !calls.iter().any(|call| call.contains(change)),
                "{change}: {calls:?}"
            );
        }
        if !present || presence == Presence::AgentTab {
            assert!(
                !calls.iter().any(|call| call.contains("osascript")),
                "{calls:?}"
            );
        }
    }
}

/// On a Mac that is not ALab's, `aterm init` at the console with no flag runs the base
/// plan (Permissions, the secrets, Spotlight) and the Sync rows; it changes no preference
/// of the ALab profile and signs out of nothing, which take `--profile` and `--sign-out`.
#[test]
fn a_personal_macs_console_run_changes_no_profile_and_signs_out_of_nothing() {
    let mut sys = alice_at(501);
    sys.terminal = true;
    sys.agent = None;
    assert_eq!(init_presence(&sys), Presence::AtConsole);
    let (bound, _, _, vault) = bound_for(&sys, &personal_standing(), false);
    assert_eq!(vault.probes(), 0, "preparation leaves discovery to apply");
    assert!(
        bound
            .iter()
            .any(|(id, _)| id == "init.spotlight.rustup-profile@alice"),
        "the base plan's Spotlight runs: {bound:?}"
    );
    let profile: Vec<_> = bound
        .iter()
        .filter(|(id, class)| *class == Class::Preference || crate::init_accounts::signs_out(id))
        .collect();
    assert!(profile.is_empty(), "{profile:?}");
    let (bound, _, _, _) = bound_for(&sys, &alab_standing(), false);
    assert!(
        bound
            .iter()
            .any(|(id, _)| crate::init_accounts::signs_out(id))
            && bound.iter().any(|(_, class)| *class == Class::Preference),
        "an ALab Mac's run takes the profile: {bound:?}"
    );
}

/// Off the console the vault join is a background row: an agent's tab whose look finds
/// ALab's open vault asks it to let this Mac in, with no join password and no code shown.
#[test]
fn an_agent_tab_joins_the_open_alab_vault() {
    let mut sys = alice_at(501);
    sys.terminal = true;
    sys.agent = Some("Claude Code");
    assert_eq!(init_presence(&sys), Presence::AtConsole);
    let (_, groups, ctx, vault) = bound_for(&sys, &alab_standing(), true);
    let _ = run(
        &sys,
        &ctx,
        Base::none(),
        &groups,
        Mode::Apply,
        &mut NoJournal,
    )
    .unwrap();
    assert_eq!(vault.joins().len(), 1, "probes {}", vault.probes());
    assert_eq!(vault.passwords(), [false], "no join password");
}

/// A run aterm decided at this Mac's console (the follow-up of a person's package install,
/// [`Decider::Aterm`]) puts no question of macOS's on the screen: Rust in Spotlight asks
/// Spotlight for none of the person's build folders. Without the LAN broker the vault's look
/// never runs, whoever decided the run, and its subscriber keeps the join waiting for the
/// broker (the run exits 0). NEGATIVE CONTROL: the person's own run searches for them.
#[test]
fn a_run_aterm_decided_raises_no_question_of_macos() {
    const MDFIND: &str =
        "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"CACHEDIR.TAG\"'";
    for (decided, asks) in [(Decider::Aterm, false), (Decider::Person, true)] {
        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.agent = None;
        assert_eq!(init_presence(&sys), Presence::AtConsole);
        sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
            .unwrap();
        sys.file("/usr/bin/mdfind", b"").on(MDFIND, Ran::ok(""));
        let (ctx, _) = local_context(&sys).unwrap();
        let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
            crate::init_secrets::tests::unconfigured(),
        )]);
        vault.broker_absent();
        let port = Arc::clone(&vault);
        let standing = Standing {
            decided,
            ..personal_standing()
        };
        let groups = prepare_groups_with(
            &sys,
            &ctx,
            Mode::Apply,
            None,
            &reading(&[]),
            &standing,
            no_grants,
            move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
        )
        .unwrap()
        .0;
        let rows = run(
            &sys,
            &ctx,
            Base::none(),
            &groups,
            Mode::Apply,
            &mut NoJournal,
        )
        .unwrap();
        assert_eq!(vault.probes(), 0, "{decided:?}: no look");
        assert_eq!(
            sys.calls().iter().any(|call| call == MDFIND),
            asks,
            "{decided:?}: {:?}",
            sys.calls()
        );
        let row = rows
            .iter()
            .find(|row| row.id.starts_with("init.secrets@"))
            .expect("the vault row");
        assert!(
            crate::init_secrets::waits_for_lan_broker(row),
            "{decided:?}: {row:?}"
        );
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(crate::init_secrets::finishes_by_itself(row), "{row:?}");
        assert_eq!(status(&rows, Action::Apply), 0, "{rows:?}");
    }
}

#[test]
fn a_scoped_agent_check_reaches_its_advertised_workstation_selector() {
    let unread = "selected preference has no current cached reading";
    for (selector, console, terminal, agent, held, dock_verified, waiting, expected) in [
        (
            "github",
            501,
            true,
            Some("Codex"),
            false,
            false,
            false,
            Some(0),
        ),
        (
            "widgets",
            501,
            true,
            Some("Codex"),
            false,
            false,
            false,
            Some(75),
        ),
        (
            "keyboard",
            501,
            true,
            Some("Codex"),
            false,
            false,
            false,
            Some(0),
        ),
        (
            "keyboard",
            501,
            true,
            Some("Codex"),
            false,
            false,
            true,
            Some(75),
        ),
        (
            "keyboard.caps-lock.seen@alice",
            501,
            false,
            Some("Codex"),
            false,
            false,
            false,
            Some(0),
        ),
        (
            "keyboard,dock",
            501,
            true,
            Some("Codex"),
            false,
            false,
            false,
            Some(75),
        ),
        (
            "keyboard,dock",
            501,
            true,
            Some("Codex"),
            false,
            true,
            false,
            Some(0),
        ),
        (
            "keybord",
            501,
            true,
            Some("Codex"),
            false,
            false,
            false,
            None,
        ),
        ("keyboard", 501, false, None, true, false, false, None),
        (
            "keyboard",
            502,
            true,
            Some("Codex"),
            false,
            false,
            false,
            Some(77),
        ),
    ] {
        let mut sys = alice_at(console);
        sys.terminal = terminal;
        sys.agent = agent;
        let mut requested = options(&["--check", "--only", selector].map(OsString::from)).unwrap();
        requested.station = unnamed(&Base::none(), &requested.only);
        let mut verified = Selection::none();
        verified.set(Setting::CapsLock, true);
        verified.set(Setting::Dock, dock_verified);
        let mut standing = alab_standing();
        standing.holds.off.set(Setting::CapsLock, held);
        let fda_reads = std::cell::Cell::new(0);
        let full_disk_access = || {
            fda_reads.set(fda_reads.get() + 1);
            Some(true)
        };
        let prepare = |sys: &dyn System, ctx: &Ctx, mode: Mode, reading: &Reading<'_>| {
            let standing = Standing {
                workstation_observation: observed_workstation(
                    sys,
                    Action::Check,
                    reading.context_scope,
                    &|required| {
                        assert_ne!(
                            selector, "github",
                            "GitHub-only needs no window's cached preference observation"
                        );
                        if selector.starts_with("keyboard") {
                            assert!(required.contains(Setting::CapsLock));
                            assert_eq!(required.contains(Setting::Dock), selector.contains("dock"));
                            assert!(!required.contains(Setting::Repositories));
                        }
                        if waiting {
                            WorkstationObservation::Waiting(unread.into())
                        } else {
                            completed(verified)
                        }
                    },
                ),
                holds: standing.holds.clone(),
                ..standing
            };
            prepare_groups_with(sys, ctx, mode, None, reading, &standing, no_grants, |_| {
                Ok(crate::init_secrets::tests::Scripted::new(vec![Ok(
                    crate::init_secrets::tests::unconfigured(),
                )])
                    as Arc<dyn crate::init_secrets::SecretsOps>)
            })
            .inspect(|(_, picked)| {
                if held {
                    assert!(picked.kept.contains(Setting::CapsLock));
                    assert!(!picked.runs.contains(Setting::CapsLock));
                }
            })
        };
        let result = converge(
            &sys,
            &requested,
            &Front {
                base: &|_| Base::none(),
                groups: &prepare,
                full_disk_access: &full_disk_access,
                targo: None,
                cwd: None,
                records: &NoSetup,
                on_row: &|_| {},
                present: &|| {
                    assert_ne!(
                        selector, "github",
                        "GitHub-only needs no WindowServer presence read"
                    );
                    true
                },
                on_held: &|_| {},
            },
            &mut NoJournal,
            &mut Laps::start(),
        );
        match expected {
            Some(code) => {
                let rows = result.expect("a real selector reaches the coordinator's observation");
                assert_eq!(status(&rows, Action::Check), code, "{selector}: {rows:?}");
                if selector == "github" {
                    assert_eq!(fda_reads.get(), 0, "GitHub-only needs no FDA reading");
                    assert_eq!(rows.len(), 2);
                    assert!(rows.iter().all(|row| row.family == "github"));
                    assert!(
                        sys.calls()
                            .iter()
                            .all(|call| !call.contains("hidutil")
                                && call != "keyboard-service-ids"),
                        "GitHub-only needs no keyboard census: {:?}",
                        sys.calls()
                    );
                } else {
                    assert_eq!(
                        rows.len(),
                        1,
                        "{selector}: only the selected aggregate runs"
                    );
                    assert_eq!(rows[0].id, AWAY);
                    if waiting {
                        assert_eq!(rows[0].why.as_deref(), Some(unread));
                    }
                    if console == 501 {
                        assert!(
                            rows[0]
                                .explain
                                .as_ref()
                                .is_none_or(|explain| explain.remedy == Remedy::None),
                            "a read-only cached observation starts no setup"
                        );
                    }
                    assert_eq!(
                        rows[0].class,
                        Some(if console == 501 {
                            Class::Relay
                        } else {
                            Class::Person
                        }),
                        "another login cannot relay this account's cached evidence"
                    );
                }
            }
            None => {
                let Refused::Usage(why) = result.expect_err("a typo or held group is refused")
                else {
                    panic!("selector refusal must remain a usage error");
                };
                assert!(why.contains(selector), "{why}");
            }
        }
        assert!(
            sys.calls().iter().all(|call| {
                !call.contains("defaults")
                    && !call.contains("dump services")
                    && !call.contains("mdfind")
                    && !call.contains("osascript")
            }),
            "{selector}: {:?}",
            sys.calls()
        );
        assert!(sys.keyboard_changes().is_empty());
        if selector == "widgets" {
            assert_eq!(
                fda_reads.get(),
                1,
                "selected contained keys retain the FDA reading"
            );
        }
        if selector.starts_with("keyboard") && console == 501 {
            assert!(
                sys.calls().iter().any(|call| call.contains("hidutil list")),
                "keyboard selection still discovers its actual services"
            );
        }
    }
}

#[test]
fn only_a_persons_console_run_searches_spotlight_project_folders() {
    const MDFIND: &str =
        "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"CACHEDIR.TAG\"'";
    const TARGET: &str = "/Users//alice/Documents/demo/target";
    for (terminal, agent, remote, decided, searches) in [
        (true, None, false, Decider::Person, true),
        (true, None, false, Decider::Aterm, false),
        (true, Some("Codex"), false, Decider::Person, false),
        (false, Some("Codex"), false, Decider::Person, false),
        (false, None, false, Decider::Person, false),
        (true, None, true, Decider::Person, false),
    ] {
        for mode in [Mode::Plan, Mode::Apply] {
            let mut sys = alice_at(501);
            sys.terminal = terminal;
            sys.agent = agent;
            sys.remote_login = remote;
            for cache in [".ubd/cargo-build.noindex", "tooling.noindex"] {
                sys.create_dir_all(
                    &Path::new("/Users//alice/Library/Caches/aterm").join(cache),
                    0o755,
                )
                .unwrap();
            }
            sys.create_dir_all(Path::new(&format!("{TARGET}/debug")), 0o755)
                .unwrap();
            sys.file(
                &format!("{TARGET}/CACHEDIR.TAG"),
                b"Signature: 8a477f597d28d172789f06886806bc55\n# created by cargo\n",
            );
            sys.file("/usr/bin/mdfind", b"")
                .on(MDFIND, Ran::ok(&format!("{TARGET}/CACHEDIR.TAG\0")));
            let (ctx, _) = local_context(&sys).unwrap();
            let standing = Standing {
                decided,
                ..personal_standing()
            };
            let only = ["spotlight".to_owned()];
            let (groups, _) = prepare_groups_with(
                &sys,
                &ctx,
                mode,
                None,
                &reading(&only),
                &standing,
                no_grants,
                |_| panic!("a Spotlight-only run needs no vault"),
            )
            .unwrap();
            let builds = groups
                .iter()
                .flat_map(|(items, _)| items)
                .find(|item| item.id() == "init.spotlight.build-noindex@alice")
                .expect("the real build-cache item");
            let probe = builds.probe(&sys, &ctx);
            assert!(matches!(probe, Probe::Ok { .. }), "{probe:?}");
            assert_eq!(
                sys.calls().iter().any(|call| call == MDFIND),
                searches,
                "{terminal:?}/{agent:?}/{remote:?}/{decided:?}/{mode:?}: {probe:?}"
            );
            assert_eq!(
                matches!(probe, Probe::Ok { now } if now.contains(TARGET)),
                searches,
                "only the person's run reads and notes the protected cache tag"
            );
        }
    }
}

/// The package follow-up's words ([`PACKAGE_FOLLOW_UP`]) are one run init accepts: an apply
/// whose base items stand aside for a pass holding the store, that still joins the vault
/// and, on an ALab Mac, clones the repositories, and that signs out of nothing and changes
/// no workstation preference on any Mac. NEGATIVE CONTROL: without `--no-sign-out` an ALab
/// Mac's run signs out.
#[test]
fn the_package_follow_up_signs_out_of_nothing_and_changes_no_preference() {
    let words = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
    let follow_up = options(&words(&PACKAGE_FOLLOW_UP)).unwrap();
    assert_eq!(follow_up.action, Action::Apply);
    assert_eq!(follow_up.base_lock(true), BaseLock::Try);
    let alab = follow_up.picks.choice(true);
    assert_eq!(
        alab,
        Choice {
            profile: false,
            sign_out: false,
            clone: true,
        }
    );
    assert_eq!(
        follow_up.picks.choice(false),
        Choice {
            clone: true,
            ..Choice::NONE
        }
    );
    let runs = alab.selection();
    assert!(runs.contains(Setting::Repositories), "{runs:?}");
    for left in [
        Setting::Messages,
        Setting::FaceTime,
        Setting::CapsLock,
        Setting::NaturalScrolling,
        Setting::TrackpadScrolling,
        Setting::Desktop,
        Setting::Widgets,
        Setting::Dock,
    ] {
        assert!(!runs.contains(left), "{left:?}: {runs:?}");
    }
    let signs_out = options(&words(&["--no-wait", "--no-profile"])).unwrap();
    assert!(signs_out.picks.choice(true).sign_out);
}

/// Agent setup starts the vault look at once.
#[test]
fn an_agent_tab_starts_the_vault_look_without_waiting_for_a_second_command() {
    let mut sys = alice_at(501);
    sys.terminal = true;
    sys.agent = Some("Claude Code");
    let (ctx, _) = local_context(&sys).unwrap();
    let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
        crate::init_secrets::tests::unconfigured(),
    )]);
    vault.alab_answers();
    let port = Arc::clone(&vault);
    let groups = prepare_groups_with(
        &sys,
        &ctx,
        Mode::Apply,
        None,
        &reading(&[]),
        &alab_standing(),
        no_grants,
        move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
    )
    .unwrap()
    .0;
    let rows = run(
        &sys,
        &ctx,
        Base::none(),
        &groups,
        Mode::Apply,
        &mut NoJournal,
    )
    .unwrap();
    assert_eq!(vault.probes(), 1);
    assert_eq!(vault.joins().len(), 1);
    assert!(rows.iter().any(|row| row.id.starts_with("init.secrets@")));
}

#[test]
fn local_agent_tools_can_run_explicit_setup_flags_without_a_terminal() {
    for flag in ["--clone", "--profile", "--sign-out"] {
        for terminal in [false, true] {
            let mut sys = alice_with_checkout(501, None);
            sys.terminal = terminal;
            sys.agent = Some("Codex");
            let options = options(&[OsString::from(flag)]).unwrap();
            let groups = |_: &Ctx| Vec::new();
            assert!(
                converge_with(&sys, &options, &groups, &NoRecords).is_ok(),
                "{flag}, terminal={terminal}"
            );
        }
    }
}

/// An apply whose GitHub step waits for this Mac to be let into the vault records the clone
/// it owes; the converge pass waits for the vault's GitHub token, then runs that step once
/// and forgets it, so nobody runs `aterm init` again. A plan, or a run that owes nothing,
/// records nothing.
#[test]
fn the_clone_init_owes_runs_once_the_token_lands_with_no_second_init() {
    let state = aterm_tempfile::tempdir().unwrap();
    let waiting = behind_the_vault("init.github.native@alice");
    let sys = alice_at(501);
    let nothing = || OwedClone::Nothing("aterm init owes no clone".into());
    note_owed(
        std::slice::from_ref(&waiting),
        Action::Plan,
        Some(state.path()),
        &Holds::NONE,
    )
    .unwrap();
    note_owed(
        &[report("init.test", Outcome::Changed)],
        Action::Apply,
        Some(state.path()),
        &Holds::NONE,
    )
    .unwrap();
    assert_eq!(
        owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap(),
        nothing()
    );
    note_owed(&[waiting], Action::Apply, Some(state.path()), &Holds::NONE).unwrap();
    let owed = owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap();
    assert!(matches!(owed, OwedClone::Waiting(_)), "{owed:?}");
    crate::init_github::scripted::converged(&sys, &user("alice", 501), "notes");
    let owed = owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap();
    assert!(matches!(owed, OwedClone::Due(_)), "{owed:?}");
    clone_owed(
        &sys,
        state.path(),
        aterm_secrets::converge::RepositoryWork::Refresh,
    )
    .unwrap();
    assert_eq!(
        owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Verified {
            present: 1,
            total: 1
        },
        "the next pass verifies the inventory instead of cloning again"
    );
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains("hidutil") && call != "keyboard-service-ids"),
        "repository retry needs no workstation census: {:?}",
        sys.calls()
    );
}

/// The keychain row's own trouble is `sync.github`'s to retry: with the repositories done, the
/// owed step is done, so a settings read that fails does not rerun it every pass.
#[test]
fn the_keychain_rows_trouble_does_not_hold_the_owed_clone() {
    let state = aterm_tempfile::tempdir().unwrap();
    let waiting = behind_the_vault("init.github.native@alice");
    let sys = alice_at(501);
    let who = user("alice", 501);
    note_owed(&[waiting], Action::Apply, Some(state.path()), &Holds::NONE).unwrap();
    sys.on(
        &crate::git_credential::script::helpers(&who),
        crate::port::Ran::exit(128, "fatal: bad config line 1"),
    );
    crate::init_github::scripted::converged(&sys, &who, "notes");
    clone_owed(
        &sys,
        state.path(),
        aterm_secrets::converge::RepositoryWork::Refresh,
    )
    .unwrap();
    assert_eq!(
        owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Verified {
            present: 1,
            total: 1
        },
    );
}

/// Fleet materialization alone starts repository reconciliation: no earlier init or
/// clone-owed file is needed. A complete local inventory settles; a new entry wakes it.
#[test]
fn fleet_inventory_clones_without_an_earlier_init_and_respects_holds() {
    let state = aterm_tempfile::tempdir().unwrap();
    let sys = alice_at(501);
    let who = user("alice", 501);
    crate::init_github::scripted::converged(&sys, &who, "notes");
    assert_eq!(
        owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Verified {
            present: 1,
            total: 1
        },
    );
    let path = Path::new(&who.home).join(".secrets/github-repositories.json");
    let bytes = aterm_json::to_vec(&aterm_json::json!({
        "schema":2,"account":"alice","repositories":[{
            "id":2,"name":"new-code","owner":{"login":"alice"},
            "full_name":"alice/new-code","private":true,
            "clone_url":"https://github.com/alice/new-code.git","default_branch":"main"
        }]
    }))
    .unwrap();
    sys.file(path.to_str().unwrap(), &bytes).stat_of(
        path.to_str().unwrap(),
        Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: bytes.len() as u64,
        },
    );
    assert!(matches!(
        owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Due(_)
    ));
    let mut held = Holds::NONE;
    held.off.set(Setting::Repositories, true);
    assert!(matches!(
        owed_clone(&sys, state.path(), &held, None).unwrap(),
        OwedClone::Held(_)
    ));
    let path = Path::new(&who.home).join(".config/aterm/init.toml");
    let profile = b"[github]\nenabled=false\n";
    sys.file(path.to_str().unwrap(), profile).stat_of(
        path.to_str().unwrap(),
        Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: profile.len() as u64,
        },
    );
    assert!(matches!(
        owed_clone(&sys, state.path(), &Holds::NONE, None).unwrap(),
        OwedClone::Off(_)
    ));
}

/// GitHub waiting for the vault to let this Mac in is owed, not blocked: its row reads
/// deferred, saying when it runs, and init exits 0, not 75.
#[test]
fn github_waiting_on_the_vaults_approval_exits_0() {
    use aterm_converge::StoodAside;
    let row = |id: &str| ItemReport {
        id: id.into(),
        family: if crate::init_secrets::is_vault_row(id) {
            "secrets"
        } else {
            "github"
        }
        .into(),
        title: id.into(),
        outcome: Outcome::Blocked,
        from: None,
        to: None,
        why: Some("waits for init.secrets@alice: waiting to be let in".into()),
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: Some(StoodAside::Waiting("waiting to be let in".into())),
        waits_on: (!crate::init_secrets::is_vault_row(id)).then(|| "init.secrets@alice".into()),
        essential: false,
        elapsed_ms: None,
    };
    let rows: Vec<ItemReport> = [row("init.secrets@alice"), row("init.github.native@alice")]
        .into_iter()
        .map(owe)
        .collect();
    assert_eq!(status(&rows, Action::Apply), 0);
    assert_eq!(
        rows[0].outcome,
        Outcome::Blocked,
        "the join finishes by itself"
    );
    assert!(
        rows[1].outcome == Outcome::Deferred && rows[1].why.as_deref() == Some(OWED),
        "{rows:?}"
    );
    assert!(tally(&rows, Action::Apply).contains("to run once this Mac is let in"));
}

/// On Linux init runs the base layer, the vault and GitHub in any shell of the account (an
/// SSH session's included), and a machine where each is converged exits 0. Control: each
/// macOS group is one row reading "macOS only", not applicable, never a failure, and a step
/// still to make is no exit 0.
#[test]
fn on_linux_init_runs_the_base_the_vault_and_github_and_the_mac_rows_are_not_applicable() {
    for converged in [true, false] {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.linux = true;
        sys.terminal = true;
        let alice = user("alice", 501);
        sys.logged_in(&alice);
        crate::init_github::scripted::converged(&sys, &alice, "notes");
        let (ctx, _) = local_context(&sys).unwrap();
        assert!(ctx.console.is_none(), "nobody's console session: {ctx:?}");
        let vault = crate::init_secrets::tests::converged();
        let port = Arc::clone(&vault);
        let (groups, picked) = prepare_groups_with(
            &sys,
            &ctx,
            Mode::Apply,
            None,
            &reading(&[]),
            &alab_standing(),
            no_grants,
            move |_| Ok(port as Arc<dyn crate::init_secrets::SecretsOps>),
        )
        .unwrap();
        assert_eq!(picked, Picked::ALL);
        let applied = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let base = Base {
            items: vec![Box::new(BaseItem {
                probes: Mutex::new(if converged {
                    vec![ok()]
                } else {
                    vec![Probe::Unusable {
                        why: "the shims are not writable".into(),
                    }]
                }),
                applied: Arc::clone(&applied),
            })],
            held: None,
            clt: None,
            toolset: None,
            targo: None,
        };
        let rows = run(&sys, &ctx, base, &groups, Mode::Apply, &mut NoJournal).unwrap();
        let outcome = |id: &str| {
            rows.iter()
                .find(|row| row.id == id)
                .unwrap_or_else(|| panic!("no {id} row: {rows:#?}"))
                .outcome
        };
        assert_eq!(
            outcome("base.pkg.shims"),
            if converged {
                Outcome::Ok
            } else {
                Outcome::Unknown
            }
        );
        assert_eq!(outcome("init.secrets@alice"), Outcome::Ok, "{rows:#?}");
        assert_eq!(
            outcome("init.github.native@alice"),
            Outcome::Ok,
            "{rows:#?}"
        );
        for mac in [
            "init.preferences",
            crate::init_spotlight::ID,
            ACCOUNTS_GROUP,
            "init.permissions",
        ] {
            let row = rows.iter().find(|row| row.id == mac).expect(mac);
            assert_eq!(row.outcome, Outcome::NotApplicable, "{row:?}");
            assert_ne!(row.outcome, Outcome::Failed);
            assert_eq!(row.why.as_deref(), Some("macOS only"));
        }
        assert_eq!(
            status(&rows, Action::Apply),
            if converged { 0 } else { 1 },
            "{rows:#?}"
        );
        assert_eq!(applied.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            vault.joins().len(),
            0,
            "a machine in the vault joins nothing"
        );
        let calls = sys.calls();
        for mac in ["killall", "osascript", "defaults", "tccutil", "launchctl"] {
            assert!(
                !calls.iter().any(|call| call.contains(mac)),
                "{mac}: {calls:?}"
            );
        }
        assert!(
            calls
                .iter()
                .filter(|call| call.contains(" git ") || call.contains("/git "))
                .all(|call| call.contains(crate::items::github::SYSTEM_GIT)),
            "the distribution's git: {calls:?}"
        );
    }
}

/// The ordinary `init --only github` producer shares the existing alignment
/// lease too; it cannot slip past a subscriber's admitted repository wave.
#[test]
fn plain_github_apply_respects_the_existing_repository_pass_lease() {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.linux = true;
    sys.terminal = true;
    let alice = user("alice", 501);
    sys.logged_in(&alice);
    crate::init_github::scripted::converged(&sys, &alice, "notes");
    let mut picked = options(&["--only".into(), "github".into()]).unwrap();
    picked.station = vec!["github".into()];
    let groups = |sys: &dyn System, ctx: &Ctx, mode: Mode, reading: &Reading<'_>| {
        prepare_groups_with(
            sys,
            ctx,
            mode,
            None,
            reading,
            &alab_standing(),
            no_grants,
            |_| panic!("GitHub-only init does not open a Secrets stage"),
        )
    };
    let run = || {
        converge(
            &sys,
            &picked,
            &Front {
                base: &|_| Base::none(),
                groups: &groups,
                full_disk_access: &|| panic!("repository init reads no workstation permission"),
                targo: None,
                cwd: None,
                records: &NoRecords,
                on_row: &|_| {},
                present: &|| false,
                on_held: &|_| {},
            },
            &mut NoJournal,
            &mut Laps::start(),
        )
        .unwrap()
    };
    let held = sys.repository_pass().unwrap().pass.unwrap();
    let rows = run();
    assert!(
        rows.iter()
            .any(|row| row.id == "init.github.native@alice" && row.outcome == Outcome::Failed),
        "{rows:?}"
    );
    assert!(
        sys.calls()
            .iter()
            .all(|call| !call.contains("--symref") && !call.contains(" fetch "))
    );
    drop(held);
    let rows = run();
    assert!(
        rows.iter()
            .any(|row| row.id == "init.github.native@alice" && row.outcome == Outcome::Ok),
        "{rows:?}"
    );
    let record = sys.repository_pass().unwrap().pass.unwrap().record();
    assert!(record.converged_ms.is_none());
    assert_eq!(
        crate::sync::due(Some(&record), "current", &[], 0),
        crate::sync::Due::NewBuild
    );
}

#[test]
fn a_run_left_only_by_stood_aside_rows_exits_75_77_or_69() {
    use aterm_converge::StoodAside;
    let row = |outcome, aside: Option<StoodAside>| ItemReport {
        id: "test".into(),
        family: "test".into(),
        title: "test".into(),
        outcome,
        from: None,
        to: None,
        why: None,
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: aside,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    };
    let lock = || Some(StoodAside::Lock("held".into()));
    let offline = || Some(StoodAside::Offline("down".into()));
    let permission = || Some(StoodAside::Permission("not granted".into()));
    let waiting = || Some(StoodAside::Waiting("waiting to be let in".into()));
    let done = row(Outcome::Changed, None);
    assert_eq!(status(std::slice::from_ref(&done), Action::Apply), 0);
    let vault = || ItemReport {
        id: "init.secrets@alice".into(),
        ..row(Outcome::Blocked, waiting())
    };
    // A vault join an admin has yet to approve is delegated for apply and plan:
    // the subscriber finishes it. A check still reports that approval is missing.
    for action in [Action::Apply, Action::Plan] {
        assert_eq!(status(&[done.clone(), vault()], action), 0);
    }
    assert_eq!(status(&[done.clone(), vault()], Action::Check), 77);
    // A GitHub row waiting for that join is owed, and runs once this Mac is let in: no
    // second run is needed for it. Any other wait is this run's own, which a later pass
    // retries.
    let behind = owe(behind_the_vault("init.github.native@alice"));
    assert_eq!(status(&[vault(), behind], Action::Apply), 0);
    assert_eq!(
        status(
            &[vault(), owe(row(Outcome::Blocked, waiting()))],
            Action::Apply
        ),
        75
    );
    assert_eq!(
        status(&[vault(), row(Outcome::Blocked, offline())], Action::Apply),
        69
    );
    assert_eq!(
        status(
            &[done.clone(), row(Outcome::Blocked, lock())],
            Action::Apply
        ),
        75
    );
    assert_eq!(
        status(
            &[done.clone(), row(Outcome::Blocked, offline())],
            Action::Apply
        ),
        69
    );
    assert_eq!(
        status(
            &[
                row(Outcome::Blocked, offline()),
                row(Outcome::Blocked, lock())
            ],
            Action::Apply
        ),
        75
    );
    assert_eq!(
        status(
            &[
                row(Outcome::Blocked, offline()),
                row(Outcome::Blocked, permission())
            ],
            Action::Apply
        ),
        77
    );
    assert_eq!(
        status(
            &[
                row(Outcome::Blocked, permission()),
                row(Outcome::Blocked, lock())
            ],
            Action::Apply
        ),
        75
    );
    assert_eq!(
        status(
            &[row(Outcome::Blocked, lock()), row(Outcome::Failed, None)],
            Action::Apply
        ),
        1
    );
}

#[test]
/// A keyboard list macOS answers in words this build does not read leaves only the Caps
/// Lock rows unread: one row says why, and every other preference is still built and run.
/// System State's own reading still refuses it.
fn an_unreadable_keyboard_list_leaves_only_the_caps_lock_rows_unread() {
    let current_user = || {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys
    };
    let command = crate::facts::hidutil_keyboards();
    for invalid in [
        "Services:\nUnknownHeader\nnonempty unparsed row\n",
        "Devices:\n",
    ] {
        let sys = current_user();
        sys.on(&command.display(), Ran::ok(invalid));
        let (ctx, unread) = local_context(&sys).unwrap();
        let why = unread
            .unread
            .as_ref()
            .expect("the list is not read")
            .clone();
        assert!(ctx.console.is_some() && ctx.facts.keyboards.is_empty());
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains("defaults") || call.contains("accounts.data"))
        );
        let groups = groups(
            &ctx,
            Presence::AtConsole,
            crate::init_spotlight::Projects::Noted,
            crate::init_secrets::Secrets {
                items: Vec::new(),
                waited_on: None,
            },
            Vec::new(),
            &unread,
            Selection::all(),
            &[],
            true,
            None,
        )
        .0;
        let preferences: Vec<&dyn Item> = groups
            .iter()
            .filter(|(_, place)| place.class == Class::Preference)
            .flat_map(|(items, _)| items.iter().map(AsRef::as_ref))
            .collect();
        let keyboard: Vec<&&dyn Item> = preferences
            .iter()
            .filter(|item| item.family() == "keyboard")
            .collect();
        assert_eq!(keyboard.len(), 1, "one Caps Lock row");
        assert_eq!(
            keyboard[0].probe(&sys, &ctx),
            Probe::Unusable { why: why.clone() }
        );
        assert!(
            preferences.len() > 1 && preferences.iter().any(|item| item.family() == "trackpad"),
            "the other preferences are built"
        );
        assert!(context_for_selected(&sys, WorkstationGroup::All, Selection::all()).is_err());
    }
    let sys = current_user();
    sys.on(&command.display(), Ran::ok("Services:\n\nDevices:\n"));
    let (ctx, unread) = local_context(&sys).unwrap();
    assert!(unread.unread.is_none() && ctx.facts.keyboards.is_empty());
    let sys = current_user();
    sys.on(&command.display(), Ran::ok("Services:\n"));
    sys.refuse_complete_stdout(&command.display(), "incomplete captured service list");
    assert!(local_context(&sys).unwrap().1.unread.is_some());
}

#[test]
fn pending_vault_approval_is_not_a_complete_read_only_check() {
    use crate::init_secrets::SecretsOps as _;
    use crate::init_secrets::tests::{Scripted, configured, converged};
    use aterm_secrets::client::Membership;

    let mut sys = Fake::root();
    sys.euid = 501;
    let context = ctx(&[user("alice", 501)]);
    for pending in [true, false] {
        let vault = if pending {
            let vault = Scripted::new(vec![Ok(configured(Membership::Pending))]);
            // Arrange the subscriber that will finish this request after approval.
            // The scripted service manager writes no real unit or state.
            vault.install_subscriber().unwrap();
            vault
        } else {
            converged()
        };
        let installs = vault.installs();
        for action in [Action::Check, Action::Apply, Action::Plan] {
            let mode = if action == Action::Apply {
                Mode::Apply
            } else {
                Mode::Plan
            };
            let port = Arc::clone(&vault) as Arc<dyn crate::init_secrets::SecretsOps>;
            let step = crate::init_secrets::items(&sys, &context, port, mode, None).unwrap();
            let rows = crate::engine::run(&step.items, mode, &sys, &context, &mut NoJournal);
            assert_eq!(rows.len(), 1);
            if pending {
                assert!(waits_by_itself(&rows[0]), "{rows:?}");
                assert_eq!(rows[0].outcome, Outcome::Blocked);
            } else {
                assert_eq!(rows[0].outcome, Outcome::Ok);
            }
            let ready = !pending || action != Action::Check;
            assert_eq!(complete(&rows, action), ready, "{action:?}: {rows:?}");
            assert_eq!(status(&rows, action), if ready { 0 } else { 77 });
            let json = report_json(action, complete(&rows, action), rows, &Laps::start()).unwrap();
            let json: aterm_json::Value = aterm_json::from_str(&json).unwrap();
            assert_eq!(json["complete"], aterm_json::Value::Bool(ready));
            assert!(vault.joins().is_empty());
            assert_eq!(vault.installs(), installs);
            assert_eq!(vault.probes(), 0, "a configured vault needs no discovery");
        }
    }
}

#[test]
fn pending_permission_or_unverified_changes_never_count_as_complete() {
    let mut row = ItemReport {
        id: "test".into(),
        family: "test".into(),
        title: "test".into(),
        outcome: Outcome::Ok,
        from: None,
        to: None,
        why: None,
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    };
    assert!(!complete(&[], Action::Apply));
    for outcome in [
        Outcome::Person,
        Outcome::Blocked,
        Outcome::Deferred,
        Outcome::Unknown,
        Outcome::Failed,
    ] {
        row.outcome = outcome;
        for action in [Action::Apply, Action::Plan, Action::Check] {
            assert!(!complete(&[row.clone()], action));
        }
    }
    row.outcome = Outcome::Planned;
    assert!(complete(&[row.clone()], Action::Plan));
    assert!(!complete(&[row.clone()], Action::Apply));
    assert!(!complete(&[row], Action::Check));
}

fn alice_with_checkout(console_uid: u32, profile: Option<&str>) -> Fake {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.logged_in(&crate::items::defaults::tests::user("alice", 501));
    let owned = |uid: u32, dir: bool| Stat {
        uid,
        gid: 20,
        mode: if dir { 0o755 } else { 0o600 },
        dir,
        symlink: false,
        len: 0,
    };
    sys.stat_of("/dev/console", owned(console_uid, false));
    sys.stat_of("/Users//alice", owned(501, true));
    sys.stat_of("/Users//alice/aterm", owned(501, true));
    sys.on(
        &crate::facts::hidutil_keyboards().display(),
        Ran::ok("Services:\n\nDevices:\n"),
    );
    let alice = user("alice", 501);
    sys.write_user_file(
        &alice,
        Path::new("/Users//alice/aterm/Cargo.toml"),
        crate::checkout::tests::manifest(Path::new("/srv/origin.git")).as_bytes(),
        0o644,
    )
    .unwrap();
    if let Some(profile) = profile {
        sys.write_user_file(
            &alice,
            Path::new("/Users//alice/.config/aterm/init.toml"),
            profile.as_bytes(),
            0o600,
        )
        .unwrap();
    }
    sys
}

struct Composed {
    bases: std::sync::atomic::AtomicU32,
    workstations: std::sync::atomic::AtomicU32,
    base_item: bool,
}

impl Composed {
    fn new() -> Self {
        Self {
            bases: std::sync::atomic::AtomicU32::new(0),
            workstations: std::sync::atomic::AtomicU32::new(0),
            base_item: false,
        }
    }

    fn with_base() -> Self {
        Self {
            base_item: true,
            ..Self::new()
        }
    }

    fn converge(&self, sys: &dyn System, options: &Options) -> Result<Vec<ItemReport>, Refused> {
        self.converge_timed(sys, options, &mut Laps::start())
    }

    fn converge_timed(
        &self,
        sys: &dyn System,
        options: &Options,
        laps: &mut Laps,
    ) -> Result<Vec<ItemReport>, Refused> {
        use std::sync::atomic::Ordering;
        let base = |_: bool| {
            self.bases.fetch_add(1, Ordering::SeqCst);
            let mut base = Base::none();
            if self.base_item {
                base.items.push(Box::new(BaseItem {
                    probes: Mutex::new(vec![ok()]),
                    applied: Arc::new(std::sync::atomic::AtomicU32::new(0)),
                }));
            }
            base
        };
        let groups = |_: &dyn System, _: &Ctx, _: Mode, _: &Reading<'_>| {
            self.workstations.fetch_add(1, Ordering::SeqCst);
            Ok((
                vec![(
                    vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                    place(Class::Preference),
                )],
                Picked::ALL,
            ))
        };
        converge(
            sys,
            options,
            &Front {
                base: &base,
                groups: &groups,
                full_disk_access: &|| sys.full_disk_access(),
                targo: None,
                cwd: Some(Path::new("/Users//alice/aterm")),
                records: &NoRecords,
                on_row: &|_| {},
                present: &|| true,
                on_held: &|_| {},
            },
            &mut NoJournal,
            laps,
        )
    }

    fn counts(&self) -> (u32, u32) {
        use std::sync::atomic::Ordering;
        (
            self.bases.load(Ordering::SeqCst),
            self.workstations.load(Ordering::SeqCst),
        )
    }
}

/// `--json` TIMES THE RUN: its `phases` name this login's context, the questions, the
/// base layer and the workstation layer, and every row, the base item's and the
/// workstation item's alike, carries its time (presence only, never a budget).
#[test]
fn a_json_report_carries_each_phase_and_every_row_its_time() {
    let sys = alice_with_checkout(501, None);
    let mut laps = Laps::start();
    let rows = Composed::with_base()
        .converge_timed(&sys, &Options::default(), &mut laps)
        .unwrap();
    let complete = complete(&rows, Action::Apply);
    let text = report_json(Action::Apply, complete, rows, &laps).unwrap();
    let report: Report = aterm_json::from_str(&text).unwrap();
    assert_eq!(
        report.phases.keys().map(String::as_str).collect::<Vec<_>>(),
        ["base", "context", "questions", "workstation"],
        "{text}"
    );
    for layer in [Layer::Base, Layer::Workstation] {
        assert!(
            report.items.iter().any(|row| row.layer == Some(layer)),
            "{layer:?}: {text}"
        );
    }
    assert!(
        report.items.iter().all(|row| row.elapsed_ms.is_some()),
        "{text}"
    );
}

#[test]
fn develop_on_a_fake_system_applies_no_preference_item() {
    let develop = Options {
        develop: Some(true),
        ..Options::default()
    };
    let sys = alice_with_checkout(501, None);
    let front = Composed::new();
    let rows = front.converge(&sys, &develop).unwrap();
    assert_eq!(front.counts(), (1, 0), "the workstation is never composed");
    assert!(
        rows.iter().all(
            |row| row.class != Some(Class::Preference) && row.layer != Some(Layer::Workstation)
        ),
        "{rows:?}"
    );
    assert_eq!(rows[0].id, crate::init_develop::RECORD);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert!(rows.iter().all(|row| row.layer == Some(Layer::Develop)));
    assert!(!sys.calls().iter().any(|call| call.contains("defaults")
        || call.contains("hidutil")
        || call.contains("accounts")));

    let sys = alice_with_checkout(501, None);
    let front = Composed::new();
    let rows = front.converge(&sys, &Options::default()).unwrap();
    assert_eq!(front.counts(), (1, 1));
    let dock = rows.iter().find(|row| row.id == "init.test.dock").unwrap();
    assert_eq!(dock.class, Some(Class::Preference));
    assert_eq!(dock.outcome, Outcome::Changed, "{rows:?}");
    assert!(
        sys.calls().iter().any(|call| call.contains("hidutil")),
        "the workstation inventories its keyboard obligations: {:?}",
        sys.calls()
    );
    assert!(
        !rows.iter().any(|row| row.layer == Some(Layer::Develop)),
        "{rows:?}"
    );
}

#[test]
fn a_fleet_style_run_as_root_is_refused_before_any_develop_item() {
    let develop = Options {
        develop: Some(true),
        ..Options::default()
    };
    let mut root = alice_with_checkout(501, None);
    root.euid = 0;
    let before = root.calls();
    let front = Composed::new();
    let refused = front.converge(&root, &develop).unwrap_err();
    assert_eq!(
        refused,
        Refused::Failed("run as the logged-in user, without sudo".into())
    );
    assert_eq!(front.counts(), (0, 0));
    assert_eq!(root.calls(), before, "nothing ran");

    let sys = alice_with_checkout(501, None);
    let front = Composed::new();
    let rows = front.converge(&sys, &develop).unwrap();
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "develop.record",
            "develop.checkout@~/aterm",
            "develop.submodules@~/aterm",
            "develop.workspace-tools@~/aterm"
        ]
    );
    let calls = sys.calls();
    assert!(
        calls
            .iter()
            .any(|call| call == "alice$ write /Users//alice/.config/aterm/init.toml 600"),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|call| call.contains("git -C /Users//alice/aterm")),
        "{calls:?}"
    );
}

#[test]
fn only_takes_workstation_and_develop_words_and_refuses_a_checkout_nobody_records() {
    assert!(options(&["--develop".into(), "--no-develop".into()]).is_err());
    for flag in ["--develop", "--no-develop"] {
        for args in [
            vec![flag, "--only", "workstation"],
            vec!["--only=workstation", flag],
            vec![flag, "--only=base,workstation"],
            vec!["--only", "base", flag, "--only", "workstation"],
        ] {
            let sys = alice_with_checkout(501, None);
            let before = sys.calls();
            let front = Composed::new();
            let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
            let result = options(&args)
                .map_err(Refused::Usage)
                .and_then(|options| front.converge(&sys, &options));
            assert!(
                matches!(&result, Err(Refused::Usage(_))),
                "{args:?}: {result:?}"
            );
            assert_eq!(front.counts(), (0, 0), "{args:?}");
            assert_eq!(sys.calls(), before, "{args:?}");
        }
    }
    assert_eq!(
        options(&["--no-develop".into(), "--no-develop".into()])
            .unwrap()
            .develop,
        Some(false)
    );
    assert_eq!(
        unnamed(
            &Base::none(),
            &[
                "workstation".into(),
                "develop".into(),
                "develop.checkout@~/aterm".into(),
                "tooling".into(),
                "keyboard".into(),
            ]
        ),
        ["keyboard"]
    );
    let recorded = "[develop]\ncheckouts = [\"~/aterm\"]\n";
    let only = |word: &str| Options {
        only: vec![word.to_owned()],
        ..Options::default()
    };
    let sys = alice_with_checkout(501, Some(recorded));
    let front = Composed::new();
    let refused = front
        .converge(&sys, &only("develop.checkout@~/elsewhere"))
        .unwrap_err();
    assert!(
        matches!(&refused, Refused::Usage(why) if why.contains("its checkouts: develop.checkout@~/aterm")),
        "{refused:?}"
    );
    assert!(
        !sys.calls().iter().any(|call| call.contains("git")),
        "{:?}",
        sys.calls()
    );
    let rows = front
        .converge(&sys, &only("develop.checkout@~/aterm"))
        .unwrap();
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["develop.checkout@~/aterm"]);
    let workstation = options(&["--only".into(), "workstation".into()]).unwrap();
    let rows = front.converge(&sys, &workstation).unwrap();
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["init.test.dock"]);
    assert_eq!(rows[0].class, Some(Class::Preference));
    assert_eq!(rows[0].outcome, Outcome::Changed);
    let sys = alice_with_checkout(501, None);
    let rows = Composed::new().converge(&sys, &only("develop")).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].outcome, Outcome::Ok);
    assert!(
        rows[0]
            .to
            .as_deref()
            .is_some_and(|now| now.contains("`aterm init --develop` records one")),
        "{rows:?}"
    );
}

#[test]
fn a_profile_that_cannot_be_read_never_stops_the_base_layer() {
    for (profile, mode, why) in [
        ("[develop\n", 0o600, "invalid ~/.config/aterm/init.toml"),
        (
            "[unknown]\nx = 1\n",
            0o600,
            "invalid ~/.config/aterm/init.toml",
        ),
        (
            "[develop]\ncheckouts = [\"aterm\"]\n",
            0o600,
            "must be ~/<path> or absolute",
        ),
        (
            "[develop]\ncheckouts = [\"~/aterm\"]\n",
            0o664,
            "cannot safely read ~/.config/aterm/init.toml",
        ),
    ] {
        for (only, console) in [
            (Vec::new(), 501),
            (Vec::new(), 0),
            (vec!["base".to_owned()], 0),
            (vec!["base.pkg.shims".to_owned()], 501),
        ] {
            let sys = alice_with_checkout(console, None);
            let alice = user("alice", 501);
            sys.write_user_file(
                &alice,
                Path::new("/Users//alice/.config/aterm/init.toml"),
                profile.as_bytes(),
                mode,
            )
            .unwrap();
            let front = Composed::with_base();
            let options = Options {
                only: only.clone(),
                ..Options::default()
            };
            let rows = front
                .converge(&sys, &options)
                .unwrap_or_else(|refused| panic!("{profile:?} {only:?} at {console}: {refused:?}"));
            assert_eq!(front.counts().0, 1);
            let base = rows.iter().find(|row| row.id == "base.pkg.shims");
            assert_eq!(base.map(|row| row.outcome), Some(Outcome::Ok), "{rows:?}");
            let record: Vec<&ItemReport> = rows
                .iter()
                .filter(|row| row.layer == Some(Layer::Develop))
                .collect();
            if only.is_empty() {
                assert_eq!(record.len(), 1, "{profile:?}: {rows:?}");
                assert_eq!(record[0].id, crate::init_develop::RECORD);
                assert_eq!(record[0].outcome, Outcome::Unknown);
                assert!(
                    record[0]
                        .why
                        .as_deref()
                        .is_some_and(|said| said.contains(why)),
                    "{profile:?}: {:?}",
                    record[0].why
                );
                assert_eq!(status(&rows, Action::Apply), 1);
            } else {
                assert!(record.is_empty(), "{only:?}: {rows:?}");
                assert_eq!(status(&rows, Action::Apply), 0);
            }
            let (ctx, _) = local_context(&sys).unwrap();
            let composed = crate::init_develop::compose(
                &sys,
                &ctx,
                &crate::init_develop::Inputs {
                    want: None,
                    asked: true,
                    cwd: None,
                    targo: None,
                },
            );
            assert!(
                composed.is_err_and(|said| said.contains(why)),
                "{profile:?}"
            );
        }
    }
}

#[test]
fn an_offline_develop_machine_exits_69_not_1() {
    use aterm_converge::StoodAside;
    for (stderr, code) in [
        (
            "fatal: unable to access 'https://github.com/alabsystems/aterm/': Could not \
             resolve host: github.com\n",
            69,
        ),
        (
            "git@github.com: Permission denied (publickey).\nfatal: Could not read from \
             remote repository.\n",
            1,
        ),
    ] {
        for action in [Action::Apply, Action::Check] {
            let sys = alice_with_checkout(501, Some("[develop]\ncheckouts = [\"~/aterm\"]\n"));
            let alice = user("alice", 501);
            crate::init_develop::tests::script_local(
                &sys,
                &alice,
                crate::init_develop::tests::ORIGIN,
                true,
                "",
            );
            sys.on(
                &crate::checkout::network(&sys, &alice, Some(Path::new("/Users//alice/aterm")))
                    .args(["ls-remote", "--symref", "origin", "HEAD"])
                    .display(),
                Ran::exit(128, stderr),
            );
            let options = Options {
                action,
                only: vec!["develop.checkout@~/aterm".into()],
                ..Options::default()
            };
            let rows = Composed::new().converge(&sys, &options).unwrap();
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(status(&rows, action), code, "{action:?}: {rows:?}");
            if code == 69 {
                assert_eq!(rows[0].outcome, Outcome::Blocked);
                assert!(matches!(rows[0].stood_aside, Some(StoodAside::Offline(_))));
                assert_eq!(
                    rows[0].explain.as_ref().map(|explain| &explain.remedy),
                    Some(&Remedy::Network)
                );
            } else {
                assert_eq!(rows[0].outcome, Outcome::Unknown, "{rows:?}");
                assert_eq!(rows[0].stood_aside, None);
            }
            assert!(
                sys.calls().iter().all(|call| !call.contains(" fetch ")),
                "{:?}",
                sys.calls()
            );
        }
    }
}

#[test]
fn an_only_word_with_its_path_names_that_checkout_alone() {
    let profile = "[develop]\ncheckouts = [\"~/aterm\", \"~/aterm.old\"]\n";
    let ids = |word: &str| -> Vec<String> {
        let sys = alice_with_checkout(501, Some(profile));
        let options = Options {
            only: vec![word.to_owned()],
            ..Options::default()
        };
        Composed::new()
            .converge(&sys, &options)
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect()
    };
    assert_eq!(
        ids("develop.checkout@~/aterm"),
        ["develop.checkout@~/aterm"]
    );
    assert_eq!(
        ids("develop.checkout"),
        ["develop.checkout@~/aterm", "develop.checkout@~/aterm.old"]
    );
}

#[test]
fn develop_records_whatever_only_says() {
    let ids = |develop: Option<bool>, profile: Option<&str>| -> Vec<String> {
        let sys = alice_with_checkout(501, profile);
        let options = Options {
            develop,
            only: vec!["tooling".into()],
            ..Options::default()
        };
        Composed::new()
            .converge(&sys, &options)
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect()
    };
    assert_eq!(
        ids(Some(true), None),
        ["develop.record", "develop.workspace-tools@~/aterm"]
    );
    assert_eq!(
        ids(None, Some("[develop]\ncheckouts = [\"~/aterm\"]\n")),
        ["develop.workspace-tools@~/aterm"]
    );
}

/// alice's Mac as doctor's develop layer sees it: euid 501, her home, and her `init.toml`
/// when given; the account's context cannot be built (no account is scripted, as for an
/// account the local directory node does not hold).
fn develop_doctor(profile: Option<&str>, home: bool) -> (Doctor<Fake>, usize) {
    let mut sys = Fake::root();
    let alice = user("alice", 501);
    sys.euid = alice.uid;
    if let Some(profile) = profile {
        sys.write_user_file(
            &alice,
            &Path::new(&alice.home).join(".config/aterm/init.toml"),
            profile.as_bytes(),
            0o600,
        )
        .unwrap();
    }
    let setup = sys.calls().len();
    let home = home.then(|| PathBuf::from(&alice.home));
    (Doctor::over(sys, home, None), setup)
}

/// With no `init.toml`, or one with no `[develop]` table, doctor composes and reads no
/// develop row and never builds the account's context (no account reading). A profile
/// that records a checkout, an `--only` develop word, or no home from the account record
/// (the controls) build it, and its failure here is the one `develop.record` row.
#[test]
fn doctor_runs_nothing_for_the_develop_layer_of_a_mac_that_records_none() {
    let id = "identity".to_owned();
    for profile in [None, Some("[github]\naccount = \"alice\"\n")] {
        let (doctor, setup) = develop_doctor(profile, true);
        let composed = doctor.compose(false);
        assert!(
            composed.items.is_empty() && !composed.unreadable,
            "{profile:?}"
        );
        assert_eq!(doctor.rows(&composed, &[]).unwrap(), Vec::new());
        assert_eq!(
            doctor.sys.calls().split_off(setup),
            Vec::<String>::new(),
            "{profile:?}"
        );
    }

    for (profile, asked, home) in [
        (Some("[develop]\ncheckouts = [\"~/aterm\"]\n"), false, true),
        (None, true, true),
        (None, false, false),
    ] {
        let (doctor, setup) = develop_doctor(profile, home);
        let composed = doctor.compose(asked);
        assert!(composed.unreadable, "{profile:?} {asked} {home}");
        let rows = doctor.rows(&composed, &[]).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id, crate::init_develop::RECORD);
        assert_eq!(rows[0].outcome, Outcome::Unknown);
        assert!(
            rows[0]
                .why
                .as_deref()
                .is_some_and(|why| why.starts_with("cannot tell whose develop layer this is")),
            "{rows:?}"
        );
        assert!(
            doctor.sys.calls().split_off(setup).contains(&id),
            "{profile:?} {asked} {home}"
        );
    }
}

/// DOCTOR'S RUST-DOCS REPAIR is the workstation item `init.spotlight.rust-docs@alice` bound
/// as a relay-class repair of the account's own: nothing is read before the call, and bare
/// doctor's scope applies it and reads it back done. NEGATIVE CONTROL: outside the account's
/// own shell the same item is blocked and its apply never runs.
#[test]
fn doctor_s_rust_docs_repair_is_a_relay_item_of_the_account_s_own() {
    const RUSTUP: &str = "alice$ CARGO_HOME=/Users//alice/.cargo \
                          RUSTUP_HOME=/Users//alice/.rustup /Users//alice/.cargo/bin/rustup";
    const STABLE: &str = "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin";
    let remove =
        format!("{RUSTUP} component remove rust-docs --toolchain stable-aarch64-apple-darwin");
    let mac = || {
        let sys = alice_with_checkout(501, None);
        let record = format!("{STABLE}/lib/rustlib/components");
        let html = format!("{STABLE}/share/doc/rust/html");
        sys.create_dir_all(Path::new(&format!("{html}/std")), 0o755)
            .unwrap();
        sys.file(&format!("{html}/std/index.html"), b"<html>")
            .file("/Users//alice/.cargo/bin/rustup", b"")
            .on(
                &format!("{RUSTUP} toolchain list"),
                Ran::ok("stable-aarch64-apple-darwin (active, default)\n"),
            )
            .file(
                &record,
                b"cargo-aarch64-apple-darwin\nrust-docs-aarch64-apple-darwin\n",
            )
            .on(&remove, Ran::ok(""))
            .on_write(&remove, Path::new(&record), b"cargo-aarch64-apple-darwin\n");
        let setup = sys.calls().len();
        (
            Doctor::over(sys, Some(PathBuf::from("/Users//alice")), None),
            setup,
        )
    };
    let run = |doctor: &Doctor<Fake>, scope: aterm_converge::Scope| {
        let items = doctor.rust_docs().expect("alice's context reads");
        aterm_converge::run_scoped(
            &items,
            aterm_converge::Mode::Apply,
            scope,
            &mut aterm_converge::NoJournal,
            &mut |_| {},
        )
        .unwrap()
    };

    let (doctor, setup) = mac();
    assert_eq!(
        doctor.sys.calls().len(),
        setup,
        "nothing read before the call"
    );
    let rows = run(&doctor, aterm_converge::Scope::REPAIR);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].id, "init.spotlight.rust-docs@alice");
    assert_eq!(rows[0].class, Some(Class::Relay));
    assert_eq!(rows[0].layer, Some(Layer::Workstation));
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");

    let (doctor, _) = mac();
    let rows = run(
        &doctor,
        aterm_converge::Scope {
            not_own_account: Some("it runs as root"),
            ..aterm_converge::Scope::REPAIR
        },
    );
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert!(
        !doctor
            .sys
            .calls()
            .iter()
            .any(|call| call == &remove || call.starts_with("rename ")),
        "{:?}",
        doctor.sys.calls()
    );
}

/// DOCTOR'S CLONE TUNING is the workstation item `init.github.tuning@alice` bound as a
/// relay-class repair of the account's own: nothing is read before the call, and under System
/// State's hold of GitHub repositories bare doctor's scope reads it not applicable, naming
/// the hold, and runs no git. NEGATIVE CONTROL: with no hold the same Mac's row names what it
/// read instead.
#[test]
fn doctor_s_clone_tuning_is_a_relay_item_of_the_account_s_own() {
    let mac = || {
        let sys = alice_with_checkout(501, None);
        let setup = sys.calls().len();
        (
            Doctor::over(sys, Some(PathBuf::from("/Users//alice")), None),
            setup,
        )
    };
    let run = |doctor: &Doctor<Fake>, holds: Holds| {
        let items = doctor
            .clone_tuning(Path::new("/state"), Box::new(move || holds.clone()))
            .expect("alice's context reads");
        aterm_converge::run_scoped(
            &items,
            aterm_converge::Mode::Apply,
            aterm_converge::Scope::REPAIR,
            &mut aterm_converge::NoJournal,
            &mut |_| {},
        )
        .unwrap()
    };
    let mut off = Selection::none();
    off.set(Setting::Repositories, true);
    let held = Holds { off, ..Holds::NONE };
    let (doctor, setup) = mac();
    assert_eq!(
        doctor.sys.calls().len(),
        setup,
        "nothing read before the call"
    );
    let rows = run(&doctor, held);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].id, "init.github.tuning@alice");
    assert_eq!(rows[0].class, Some(Class::Relay));
    assert_eq!(rows[0].layer, Some(Layer::Workstation));
    assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
    assert_eq!(
        rows[0].why.as_deref(),
        Some("left as you set it in System State")
    );
    assert!(
        !doctor
            .sys
            .calls()
            .iter()
            .any(|call| call.contains("rev-parse") || call.contains("config")),
        "{:?}",
        doctor.sys.calls()
    );

    let (doctor, _) = mac();
    let rows = run(&doctor, Holds::NONE);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_ne!(
        rows[0].why.as_deref(),
        Some("left as you set it in System State"),
        "{rows:?}"
    );
}

/// Lanes meeting in their probes: [`Meet::arrive`] returns once `count` lanes stand in it
/// at the same time, and fails after a minute (a hang detector): lanes run one after
/// another never all arrive.
struct Meet {
    here: Mutex<u32>,
    all: std::sync::Condvar,
    count: u32,
}

impl Meet {
    fn new(count: u32) -> Arc<Self> {
        Arc::new(Self {
            here: Mutex::new(0),
            all: std::sync::Condvar::new(),
            count,
        })
    }

    fn arrive(&self) {
        let mut here = self.here.lock().unwrap();
        *here += 1;
        self.all.notify_all();
        let (here, waited) = self
            .all
            .wait_timeout_while(here, std::time::Duration::from_secs(60), |here| {
                *here < self.count
            })
            .unwrap();
        assert!(
            !waited.timed_out(),
            "only {} of {} lanes ran at once",
            *here,
            self.count
        );
    }
}

/// A base item that meets the other lanes in its probe.
struct MeetsBase(Arc<Meet>);

impl aterm_converge::Item for MeetsBase {
    fn id(&self) -> String {
        "base.pkg.shims".into()
    }
    fn family(&self) -> &'static str {
        "pkg"
    }
    fn layer(&self) -> Layer {
        Layer::Base
    }
    fn class(&self) -> Class {
        Class::Relay
    }
    fn title(&self) -> String {
        self.id()
    }
    fn probe(&self) -> Probe {
        self.0.arrive();
        ok()
    }
    fn apply(&self) -> Result<(), String> {
        Ok(())
    }
    fn explain(&self, probe: &Probe) -> aterm_converge::Explain {
        aterm_converge::Explain {
            why: format!("{probe:?}"),
            remedy: Remedy::Repair,
        }
    }
}

/// A workstation item whose probe runs `then` before it reads right.
struct Then {
    id: &'static str,
    family: &'static str,
    then: Box<dyn Fn() + Send + Sync>,
}

impl Item for Then {
    fn id(&self) -> String {
        self.id.into()
    }
    fn family(&self) -> &'static str {
        self.family
    }
    fn title(&self) -> String {
        self.id.into()
    }
    fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
        (self.then)();
        ok()
    }
    fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
        Ok(())
    }
}

fn then(
    id: &'static str,
    family: &'static str,
    then: impl Fn() + Send + Sync + 'static,
) -> Box<dyn Item> {
    Box::new(Then {
        id,
        family,
        then: Box::new(then),
    })
}

/// The base layer, the vault's join and the preferences run at once: each lane's first
/// item meets the other two, which a serial run never lets happen.
#[test]
fn the_base_vault_and_preference_lanes_run_at_once() {
    let meet = Meet::new(3);
    let base = Base {
        items: vec![Box::new(MeetsBase(Arc::clone(&meet)))],
        held: None,
        clt: None,
        toolset: None,
        targo: None,
    };
    let (dock, vault) = (Arc::clone(&meet), Arc::clone(&meet));
    let groups = vec![
        (
            vec![then("init.test.dock", "dock", move || dock.arrive())],
            place(Class::Preference),
        ),
        (
            vec![then(
                "init.test.vault",
                crate::init_secrets::FAMILY,
                move || vault.arrive(),
            )],
            place(Class::Install),
        ),
    ];
    let alice = at_console(user("alice", 501));
    let rows = run(
        &Fake::root(),
        &alice,
        base,
        &groups,
        Mode::Apply,
        &mut NoJournal,
    )
    .unwrap();
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["base.pkg.shims", "init.test.dock", "init.test.vault"]
    );
}

/// The Messages and FaceTime sign-outs, which nothing undoes, run after every other row,
/// another lane's included: the vault's join here holds its lane for a second watching for
/// a sign-out to start, and none starts until it ends.
#[test]
fn the_sign_outs_run_after_every_other_row() {
    type Log = Arc<(Mutex<Vec<&'static str>>, std::sync::Condvar)>;
    let log: Log = Arc::default();
    let note = |log: &Log, what: &'static str| {
        log.0.lock().unwrap().push(what);
        log.1.notify_all();
    };
    let (dock, sign_out, vault) = (Arc::clone(&log), Arc::clone(&log), Arc::clone(&log));
    let groups = vec![
        (
            vec![
                then("init.test.dock", "dock", move || note(&dock, "dock")),
                then("init.messages.signed-out@alice", "init", move || {
                    note(&sign_out, "sign-out");
                }),
            ],
            place(Class::Preference),
        ),
        (
            vec![then(
                "init.test.vault",
                crate::init_secrets::FAMILY,
                move || {
                    let (seen, started) = &*vault;
                    drop(
                        started
                            .wait_timeout_while(
                                seen.lock().unwrap(),
                                std::time::Duration::from_secs(1),
                                |seen| !seen.contains(&"sign-out"),
                            )
                            .unwrap(),
                    );
                    note(&vault, "vault");
                },
            )],
            place(Class::Install),
        ),
    ];
    let alice = at_console(user("alice", 501));
    run(
        &Fake::root(),
        &alice,
        Base::none(),
        &groups,
        Mode::Apply,
        &mut NoJournal,
    )
    .unwrap();
    let seen = log.0.lock().unwrap().clone();
    let at = |what| seen.iter().position(|seen| *seen == what).unwrap();
    assert!(at("vault") < at("sign-out"), "{seen:?}");
}

/// Each row reaches `on_row` as its item finishes, while the run goes on: the preference
/// waits for the vault's row to have been printed. The rows come back in composition
/// order all the same.
#[test]
fn each_row_streams_as_it_finishes_and_the_rows_return_in_composition_order() {
    let streamed: Arc<(Mutex<Vec<String>>, std::sync::Condvar)> = Arc::default();
    let seen = Arc::clone(&streamed);
    let groups = vec![
        (
            vec![then("init.test.dock", "dock", move || {
                let (rows, more) = &*seen;
                let (rows, waited) = more
                    .wait_timeout_while(
                        rows.lock().unwrap(),
                        std::time::Duration::from_secs(60),
                        |rows| !rows.iter().any(|id| id == "init.test.vault"),
                    )
                    .unwrap();
                assert!(
                    !waited.timed_out(),
                    "the vault's row never streamed: {rows:?}"
                );
            })],
            place(Class::Preference),
        ),
        (
            vec![then("init.test.vault", crate::init_secrets::FAMILY, || {})],
            place(Class::Install),
        ),
    ];
    let alice = at_console(user("alice", 501));
    let on_row = |row: &ItemReport| {
        let (rows, more) = &*streamed;
        rows.lock().unwrap().push(row.id.clone());
        more.notify_all();
    };
    let rows = run_layers(
        &Fake::root(),
        &alice,
        Base::none(),
        &groups,
        &[],
        &Narrowing {
            only: &[],
            record: false,
            relayed: false,
        },
        Mode::Apply,
        &mut NoJournal,
        &mut Laps::start(),
        &Hooks {
            on_row: &on_row,
            desktop: &|stages| stages.plain(),
            held: &|_| None,
        },
    )
    .unwrap();
    assert_eq!(
        *streamed.0.lock().unwrap(),
        ["init.test.vault", "init.test.dock"]
    );
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["init.test.dock", "init.test.vault"]
    );
}

/// The grants an apply hands to the window's walk are the Permissions rows left to the
/// person, in row order; a grant that is on, a row of another phase and an unknown word are
/// not. NEGATIVE CONTROL: every needs-you row, read by outcome alone, takes the vault's too.
#[test]
fn the_walk_takes_the_grants_the_permissions_phase_left_to_the_person() {
    let rows = [
        report("init.permissions.full-disk-access", Outcome::Person),
        report("init.permissions.developer-tools", Outcome::Ok),
        report("init.permissions.debugger", Outcome::Person),
        report("init.permissions.accessibility", Outcome::Person),
        report("init.permissions.camera", Outcome::Person),
        report("init.secrets.vault", Outcome::Person),
    ];
    assert_eq!(
        walk_grants(&rows),
        [
            Access::FullDiskAccess,
            Access::Debugger,
            Access::Accessibility
        ]
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.outcome == Outcome::Person)
            .count(),
        5,
        "the outcome alone takes rows that are no grant"
    );
}

/// A GitHub row the vault's own wait for an admin's approval held back ([`owed`]).
fn behind_the_vault(id: &str) -> ItemReport {
    ItemReport {
        family: "github".into(),
        stood_aside: Some(aterm_converge::StoodAside::Waiting(
            "waiting to be let in".into(),
        )),
        waits_on: Some("init.secrets@alice".into()),
        ..report(id, Outcome::Blocked)
    }
}

fn report(id: &str, outcome: Outcome) -> ItemReport {
    ItemReport {
        id: id.into(),
        family: "test".into(),
        title: id.into(),
        outcome,
        from: None,
        to: None,
        why: None,
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    }
}

/// What the default output prints for `rows`: each shown row, then the tally.
fn printed(rows: &[ItemReport], action: Action) -> String {
    let mut text: String = rows
        .iter()
        .filter(|row| shown(row))
        .map(|row| rendered(row, false))
        .collect();
    text.push_str(&completion_summary(rows, action, true));
    text.push('\n');
    text
}

/// A run with every row right prints one line; a changed row adds its row and what
/// changed, and a row only the person can finish is named in the tally.
#[test]
fn a_run_with_every_row_right_prints_one_line() {
    let mut rows: Vec<ItemReport> = (0..45)
        .map(|n| report(&format!("init.test.{n}"), Outcome::Ok))
        .collect();
    let quiet = printed(&rows, Action::Apply);
    assert_eq!(
        quiet,
        "aterm init: 45 ok — Successfully completed. No further action is needed.\n"
    );
    rows[3] = ItemReport {
        from: Some("on".into()),
        to: Some("off".into()),
        ..report("init.test.changed", Outcome::Changed)
    };
    let changed = printed(&rows, Action::Apply);
    assert_eq!(changed.lines().count(), 3, "{changed}");
    assert!(
        changed.ends_with("aterm init: 44 ok \u{b7} 1 changed — Successfully completed. No further action is needed.\n"),
        "{changed}"
    );
    rows[5] = ItemReport {
        title: "Full Disk Access".into(),
        why: Some("switch it on".into()),
        ..report("init.permissions.full-disk", Outcome::Person)
    };
    let person = printed(&rows, Action::Apply);
    assert_eq!(person.lines().count(), 5, "{person}");
    assert!(
        person.ends_with("43 ok \u{b7} 1 changed \u{b7} 1 needs you: Full Disk Access\n"),
        "{person}"
    );
}

#[test]
fn unfinished_or_planned_setup_never_claims_successful_completion() {
    for outcome in [
        Outcome::Failed,
        Outcome::Unknown,
        Outcome::Blocked,
        Outcome::Person,
        Outcome::Planned,
    ] {
        let rows = [report("base.clt", outcome)];
        assert!(!completion_summary(&rows, Action::Apply, true).contains("Successfully completed"));
    }
    assert!(!completion_summary(&[], Action::Apply, true).contains("Successfully completed"));
    assert!(
        !completion_summary(&[report("base.clt", Outcome::Ok)], Action::Apply, false)
            .contains("Successfully completed")
    );
    assert!(
        completion_summary(&[report("base.clt", Outcome::Ok)], Action::Plan, true)
            .ends_with("(nothing changed)")
    );
}

/// A row that finishes while another lane's question or attached program (a join
/// password, `sudo`) owns the terminal is held back, and printed with the next row or once
/// the run is over; an ok row is never printed by default.
#[test]
fn a_row_waits_while_a_prompt_owns_the_terminal() {
    let printer = Printer::new(false, false);
    let changed = ItemReport {
        from: Some("on".into()),
        to: Some("off".into()),
        ..report("init.test.changed", Outcome::Changed)
    };
    let mut during = Vec::new();
    crate::port::owning_terminal(|| printer.row_to(&changed, &mut during));
    assert!(during.is_empty(), "{}", String::from_utf8_lossy(&during));
    let mut after = Vec::new();
    printer.row_to(&report("init.test.ok", Outcome::Ok), &mut after);
    printer.flush_to(&mut after);
    assert_eq!(String::from_utf8(after).unwrap(), rendered(&changed, false));
}

/// `aterm init --help` and `aterm help init` read one page: every System State setting by
/// its label, with the same automatic first-launch behavior.
#[test]
fn the_usage_names_every_setting_by_its_label() {
    let usage = usage();
    for setting in Setting::ALL {
        assert!(usage.contains(setting.label()), "{setting:?}: {usage}");
    }
}

/// First-run setup is on by default: a person at an ALab Mac's
/// console with no flag gets every group, the preferences and both sign-outs included, and
/// is asked nothing; `--no-profile` and `--no-clone` leave theirs out. On another Mac a flag
/// adds its group to the base plan.
#[test]
fn a_person_at_the_console_gets_every_group_and_is_asked_nothing() {
    let prepared_with = |picks: Picks, standing: Standing<'static>| {
        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.agent = None;
        assert_eq!(crate::console::presence(&sys), Presence::AtConsole);
        let (ctx, _) = local_context(&sys).unwrap();
        let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
            crate::init_secrets::tests::unconfigured(),
        )]);
        let (groups, picked) = prepare_groups_with(
            &sys,
            &ctx,
            Mode::Apply,
            None,
            &Reading {
                keyboards: &KeyboardRead::EMPTY,
                context_scope: Selection::all(),
                only: &[],
                picks,
            },
            &standing,
            no_grants,
            move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
        )
        .unwrap();
        let preferences: Vec<String> = groups
            .iter()
            .filter(|(_, place)| place.class == Class::Preference)
            .flat_map(|(items, _)| items.iter().map(|item| item.id()))
            .collect();
        let asked: Vec<String> = sys
            .calls()
            .into_iter()
            .filter(|call| call.starts_with("ask"))
            .collect();
        (preferences, picked, asked)
    };
    let signs_out = |id: &String| crate::init_accounts::signs_out(id);
    let (preferences, picked, asked) = prepared_with(Picks::default(), alab_standing());
    assert_eq!((picked.runs, picked.held), (Picked::ALL.runs, None));
    assert!(asked.is_empty(), "nothing is asked: {asked:?}");
    assert!(
        preferences.iter().filter(|id| signs_out(id)).count() == 2
            && preferences.iter().any(|id| !signs_out(id)),
        "{preferences:?}"
    );
    let sign_out = Choice {
        sign_out: true,
        ..Choice::NONE
    };
    for (picks, standing) in [
        (
            Picks {
                named: sign_out,
                left: Choice::NONE,
            },
            personal_standing(),
        ),
        (
            Picks {
                named: Choice::NONE,
                left: Choice {
                    profile: true,
                    clone: true,
                    ..Choice::NONE
                },
            },
            alab_standing(),
        ),
    ] {
        let (preferences, picked, asked) = prepared_with(picks, standing);
        let mut expected = sign_out.selection();
        expected.set(Setting::Repositories, !picks.left.clone);
        assert_eq!(picked.runs, expected);
        assert!(asked.is_empty(), "{asked:?}");
        assert!(
            preferences.len() == 2 && preferences.iter().all(signs_out),
            "{preferences:?}"
        );
    }
}

/// A group the person turned off in System State, or every group while a Stop there holds,
/// is left as it is by a plain `aterm init`, which says so and how to set it up anyway; a
/// flag that names the group runs it.
#[test]
fn init_keeps_a_group_the_person_turned_off_and_names_the_way_to_run_it() {
    let mut off = Selection::all();
    off.set(Setting::Messages, false);
    off.set(Setting::FaceTime, false);
    let mut kept = Selection::none();
    kept.set(Setting::Messages, true);
    kept.set(Setting::FaceTime, true);
    let turned_off = Holds {
        off: kept,
        ..Holds::NONE
    };
    let plain = picked(Picks::default(), &[], &|| true, &turned_off, true);
    assert_eq!(plain.runs, off, "{plain:?}");
    assert!(!plain.signs_out());
    let line = plain.held.expect("the held line");
    assert!(
        line.contains("left as you set them in System State")
            && line
                .contains("aterm init --only with each one's word (init.messages, init.facetime)"),
        "{line}"
    );
    let named = Picks {
        named: Choice {
            sign_out: true,
            ..Choice::NONE
        },
        left: Choice::NONE,
    };
    let flagged = picked(named, &[], &|| true, &turned_off, true);
    assert!(flagged.signs_out() && flagged.held.is_none(), "{flagged:?}");
    let stopped = Holds {
        stopped: Some("you stopped setup in System State".into()),
        stopped_revision: Some(1),
        ..Holds::NONE
    };
    let plain = picked(Picks::default(), &[], &|| true, &stopped, true);
    assert!(plain.runs.is_empty(), "{plain:?}");
    let line = plain.held.expect("the held line");
    assert!(
        line.starts_with("left as they are because you stopped setup in System State"),
        "{line}"
    );
    let only = picked(
        Picks::default(),
        &["init.dock".into()],
        &|| true,
        &stopped,
        true,
    );
    assert_eq!(
        (only.runs, only.held, only.kept),
        (Picked::ALL.runs, None, Selection::none()),
        "--only runs what it names"
    );
    assert_eq!(only.start, stopped, "over the holds on record as it began");
}

/// The setup records a first launch shares: a pass that changes the preferences holds
/// them, `whole` for every preference and both sign-outs, and records only a whole pass
/// it verified.
#[derive(Default)]
struct Recorded {
    holds: Mutex<Vec<bool>>,
    verified: Arc<std::sync::atomic::AtomicU32>,
}

impl Records for Recorded {
    fn hold(&self, pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
        self.holds.lock().unwrap().push(pass.whole());
        Ok(Box::new(Verifies(Arc::clone(&self.verified))))
    }
}

struct Verifies(Arc<std::sync::atomic::AtomicU32>);

impl Held for Verifies {
    fn verified(self: Box<Self>) -> Result<(), String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// One converge of alice's Mac at her console with a terminal, the groups `groups` builds
/// and `records` for the setup records.
fn converge_with(
    sys: &Fake,
    options: &Options,
    groups: &dyn Fn(&Ctx) -> Vec<Group>,
    records: &dyn Records,
) -> Result<Vec<ItemReport>, Refused> {
    let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
        Ok((
            groups(ctx),
            picked(
                reading.picks,
                reading.only,
                &|| true,
                &Holds::default(),
                true,
            ),
        ))
    };
    converge(
        sys,
        options,
        &Front {
            base: &|_| Base::none(),
            groups: &prepare,
            full_disk_access: &|| sys.full_disk_access(),
            targo: None,
            cwd: None,
            records,
            on_row: &|_| {},
            present: &|| true,
            on_held: &|_| {},
        },
        &mut NoJournal,
        &mut Laps::start(),
    )
}

/// Only the Desktop lane waits behind a first-launch pass that holds the setup records: the
/// base layer and the vault's join run meanwhile. Here the hold returns only once the base
/// item and the vault's join stand in it too (a hang detector), which a hold taken before
/// the run never lets happen.
#[test]
fn only_the_desktop_lane_waits_behind_the_setup_records() {
    struct Behind(Arc<Meet>, Mutex<Vec<String>>);
    impl Records for Behind {
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            self.1.lock().unwrap().push("held".into());
            self.0.arrive();
            Ok(Box::new(NoRecords))
        }
    }
    let meet = Meet::new(3);
    let records = Behind(Arc::clone(&meet), Mutex::default());
    let base_meet = Arc::clone(&meet);
    let base = move |_: bool| Base {
        items: vec![Box::new(MeetsBase(Arc::clone(&base_meet)))],
        held: None,
        clt: None,
        toolset: None,
        targo: None,
    };
    let vault = Arc::clone(&meet);
    let groups = move |_: &Ctx| {
        let vault = Arc::clone(&vault);
        vec![
            (
                vec![Listed::boxed("init.test.dock", vec![ok()])],
                place(Class::Preference),
            ),
            (
                vec![then(
                    "init.test.vault",
                    crate::init_secrets::FAMILY,
                    move || vault.arrive(),
                )],
                place(Class::Install),
            ),
        ]
    };
    let prepare =
        |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| Ok((groups(ctx), Picked::ALL));
    let sys = alice_with_checkout(501, None);
    let rows = converge(
        &sys,
        &Options::default(),
        &Front {
            base: &base,
            groups: &prepare,
            full_disk_access: &|| sys.full_disk_access(),
            targo: None,
            cwd: None,
            records: &records,
            on_row: &|_| {},
            present: &|| true,
            on_held: &|_| {},
        },
        &mut NoJournal,
        &mut Laps::start(),
    )
    .unwrap();
    assert_eq!(*records.1.lock().unwrap(), ["held"]);
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["base.pkg.shims", "init.test.dock", "init.test.vault"]
    );
}

#[test]
fn a_whole_profile_pass_records_what_it_verified_and_a_part_records_nothing() {
    let dock = |ctx: &Ctx| {
        let mut items = verified_accounts(ctx);
        items.push(Listed::boxed("init.test.dock", vec![change(), ok()]));
        vec![(items, place(Class::Preference))]
    };
    let unverified = |ctx: &Ctx| {
        let mut items = verified_accounts(ctx);
        items.push(Listed::boxed("init.test.dock", vec![change(), change()]));
        vec![(items, place(Class::Preference))]
    };
    // On an ALab Mac `--no-sign-out` runs the profile without the sign-outs: a part.
    let part = Options {
        picks: Picks {
            named: Choice::NONE,
            left: Choice {
                sign_out: true,
                ..Choice::NONE
            },
        },
        ..Options::default()
    };
    for (options, groups, holds, verified) in [
        (
            &Options::default(),
            &dock as &dyn Fn(&Ctx) -> Vec<Group>,
            [true],
            1,
        ),
        (&Options::default(), &unverified, [true], 0),
        (&part, &dock, [false], 0),
    ] {
        let sys = alice_with_checkout(501, None);
        let records = Recorded::default();
        converge_with(&sys, options, groups, &records).unwrap();
        assert_eq!(*records.holds.lock().unwrap(), holds, "{options:?}");
        assert_eq!(
            records.verified.load(std::sync::atomic::Ordering::SeqCst),
            verified,
            "{options:?}"
        );
    }
}

struct WithheldRecords {
    events: Arc<Mutex<Vec<String>>>,
    refuses: bool,
}

impl Records for WithheldRecords {
    fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
        Ok(Box::new(Self {
            events: Arc::clone(&self.events),
            refuses: self.refuses,
        }))
    }
}

impl Held for WithheldRecords {
    fn withheld(&self, ids: &[String]) -> Result<(), String> {
        self.events.lock().unwrap().extend(ids.iter().cloned());
        if self.refuses {
            Err("withheld record is unwritable".into())
        } else {
            Ok(())
        }
    }

    fn verified(self: Box<Self>) -> Result<(), String> {
        self.events.lock().unwrap().push("verified".into());
        Ok(())
    }
}

fn init_withheld_pass(
    only: Vec<String>,
    refuses: bool,
) -> (Result<Vec<ItemReport>, Refused>, Vec<String>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let records = WithheldRecords {
        events: Arc::clone(&events),
        refuses,
    };
    let lane = Arc::clone(&events);
    let groups = move |ctx: &Ctx| {
        let mut widgets = Selection::none();
        widgets.set(Setting::Widgets, true);
        let mut items: Vec<_> = crate::init_preferences::selected_items(ctx, widgets)
            .unwrap()
            .into_iter()
            .filter(|item| item.withheld().is_some())
            .collect();
        assert_eq!(
            items.len(),
            1,
            "the real Notification Center item waits for FDA"
        );
        let lane = Arc::clone(&lane);
        items.push(then("init.test.after-withheld", "test", move || {
            lane.lock().unwrap().push("desktop".into());
        }));
        items.extend(verified_accounts(ctx));
        vec![(items, place(Class::Preference))]
    };
    let result = converge_with(
        &alice_with_checkout(501, None),
        &Options {
            station: unnamed(&Base::none(), &only),
            only,
            ..Options::default()
        },
        &groups,
        &records,
    );
    let events = events.lock().unwrap().clone();
    (result, events)
}

#[test]
fn init_withheld_items_are_retained_before_the_desktop_lane_verifies() {
    let (rows, events) = init_withheld_pass(Vec::new(), false);
    let rows = rows.unwrap();
    assert_eq!(rows[0].id, "consumer.widgets.notification-center@alice");
    assert_eq!(rows[0].outcome, Outcome::Ok);
    assert_eq!(
        events,
        [
            "consumer.widgets.notification-center@alice",
            "desktop",
            "verified"
        ]
    );
}

#[test]
fn init_withheld_record_failure_prevents_the_desktop_lane() {
    let (result, events) = init_withheld_pass(Vec::new(), true);
    assert!(
        result.is_err(),
        "an unrecorded permission follow-up cannot be verified"
    );
    assert_eq!(events, ["consumer.widgets.notification-center@alice"]);
}

#[test]
fn init_withheld_items_outside_the_narrowed_pass_are_not_recorded() {
    let (result, events) = init_withheld_pass(vec!["init.test.after-withheld".into()], false);
    let rows = result.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "init.test.after-withheld");
    assert_eq!(events, ["desktop"]);
}

/// `--only init.spotlight` names Rust in Spotlight's rows alone: no base item, no other
/// workstation row, no look for a vault and no hold on the setup records.
#[test]
fn only_init_spotlight_runs_the_spotlight_rows_alone() {
    let parsed = options(&["--only".into(), "init.spotlight".into()]).unwrap();
    assert_eq!(parsed.only, ["init.spotlight"]);
    let options = Options {
        station: unnamed(&Base::none(), &parsed.only),
        ..parsed
    };
    assert!(options.workstation());
    let sys = alice_with_checkout(501, None);
    let groups = |ctx: &Ctx| {
        vec![
            (
                vec![Listed::boxed("init.test.dock", vec![change(), ok()])],
                place(Class::Preference),
            ),
            (
                crate::init_spotlight::items(ctx, crate::init_spotlight::Projects::Noted).unwrap(),
                place(Class::Asked),
            ),
        ]
    };
    let records = Recorded::default();
    let rows = converge_with(&sys, &options, &groups, &records).unwrap();
    assert!(
        !rows.is_empty() && rows.iter().all(|row| row.id.starts_with("init.spotlight.")),
        "{rows:?}"
    );
    assert!(records.holds.lock().unwrap().is_empty());
    let unknown = Options {
        only: vec!["init.spotlite".into()],
        station: vec!["init.spotlite".into()],
        ..Options::default()
    };
    assert!(matches!(
        converge_with(&sys, &unknown, &groups, &records),
        Err(Refused::Usage(_))
    ));
}

/// `--only init.fseventsd`, the line a press of the memory alert types, names the item, so
/// it restarts fseventsd; in a plain `aterm init` the same fseventsd asks for no password
/// and says how to restart it now.
#[test]
fn only_init_fseventsd_restarts_it() {
    use aterm_uds::services::{Look, Service};
    let alice = ctx(&[user("alice", 501)]);
    let mut sys = Fake::root();
    sys.euid = 501;
    sys.tty = true;
    for (only, outcome) in [
        (Vec::new(), Outcome::Deferred),
        (vec![crate::init_fseventsd::ID.to_owned()], Outcome::Planned),
    ] {
        sys.services(vec![Look::Running(
            Service {
                pid: 537,
                start_s: 1_790_000_000,
                live: 1,
                footprint: 20 << 30,
                ..Service::default()
            },
            128 << 30,
        )]);
        let groups = groups(
            &alice,
            Presence::AtConsole,
            crate::init_spotlight::Projects::Noted,
            crate::init_secrets::Secrets {
                items: Vec::new(),
                waited_on: None,
            },
            Vec::new(),
            &KeyboardRead::EMPTY,
            Selection::all(),
            &only,
            true,
            None,
        )
        .0;
        let bound = crate::engine::bind(
            groups.iter().flat_map(|(items, place)| {
                items
                    .iter()
                    .filter(|item| item.id() == crate::init_fseventsd::ID)
                    .map(move |item| (item.as_ref(), place.clone()))
            }),
            &sys,
            &alice,
        );
        let rows = aterm_converge::run_scoped(
            &bound,
            Mode::Plan,
            aterm_converge::Scope::EVERY,
            &mut aterm_converge::NoJournal,
            &mut |_| {},
        )
        .unwrap();
        let ran: Vec<(&str, Outcome)> = rows.iter().map(|r| (r.id.as_str(), r.outcome)).collect();
        assert_eq!(ran, [(crate::init_fseventsd::ID, outcome)], "{only:?}");
    }
    assert!(
        !sys.calls().iter().any(|c| c.starts_with("tty ")),
        "{:?}",
        sys.calls()
    );
}

/// Explicit setup also runs from an agent's tool call in the console owner's session.
#[test]
fn sign_out_runs_without_a_terminal_including_from_an_agent_tab() {
    let options = Options {
        picks: Picks {
            named: Choice {
                sign_out: true,
                ..Choice::NONE
            },
            left: Choice::NONE,
        },
        ..Options::default()
    };
    let groups = |_: &Ctx| {
        vec![(
            vec![Listed::boxed("init.test.dock", vec![ok()])],
            place(Class::Preference),
        )]
    };
    for (terminal, agent, allowed) in [
        (false, None, true),
        (false, Some("Claude Code"), true),
        (false, Some("Codex"), true),
        (true, Some("Claude Code"), true),
        (true, None, true),
    ] {
        let mut sys = alice_with_checkout(501, None);
        sys.terminal = terminal;
        sys.agent = agent;
        let ran = converge_with(&sys, &options, &groups, &NoRecords);
        if allowed {
            assert!(ran.is_ok(), "{ran:?}");
        } else {
            assert!(matches!(ran, Err(Refused::Usage(_))), "{ran:?}");
            assert!(!sys.calls().iter().any(|call| call.contains("/usr/bin/id")));
        }
    }
    let ctx = ctx(&[user("alice", 501)]);
    let accounts = self::groups(
        &ctx,
        Presence::NoTerminal,
        crate::init_spotlight::Projects::Unread,
        crate::init_secrets::Secrets {
            items: Vec::new(),
            waited_on: None,
        },
        Vec::new(),
        &KeyboardRead::EMPTY,
        Choice {
            sign_out: true,
            ..Choice::NONE
        }
        .selection(),
        &[],
        true,
        None,
    )
    .0;
    let ids: Vec<String> = accounts
        .iter()
        .flat_map(|(items, _)| items.iter().map(|item| item.id()))
        .collect();
    assert_eq!(
        ids,
        [
            AWAY,
            "init.spotlight.rustup-profile@alice",
            "init.spotlight.rust-docs@alice",
            "init.spotlight.build-noindex@alice",
            "init.messages.signed-out@alice",
            "init.facetime.signed-out@alice"
        ],
        "the preferences wait for a person at the console, Rust in Spotlight runs as a \
         background row, and the sign-outs run last"
    );
    let preferences = Options {
        picks: Picks {
            named: Choice {
                profile: true,
                ..Choice::NONE
            },
            left: Choice::NONE,
        },
        ..Options::default()
    };
    let mut sys = alice_with_checkout(501, None);
    sys.terminal = false;
    assert!(matches!(
        converge_with(&sys, &preferences, &groups, &NoRecords),
        Err(Refused::Usage(_))
    ));
}

#[test]
fn an_invalid_init_toml_leaves_one_unread_row_and_the_rest_run() {
    let sys = alice_with_checkout(501, Some("[github\n"));
    let (ctx, _) = local_context(&sys).unwrap();
    let read = crate::init_github::items(&sys, &ctx, None);
    assert!(
        read.as_ref().is_err_and(|why| why.contains("init.toml")),
        "the group cannot be built"
    );
    let github = built("init.github", "init", Setting::Repositories.label(), read);
    let groups = groups(
        &ctx,
        Presence::AtConsole,
        crate::init_spotlight::Projects::Noted,
        crate::init_secrets::Secrets {
            items: Vec::new(),
            waited_on: None,
        },
        github,
        &KeyboardRead::EMPTY,
        Selection::all(),
        &[],
        true,
        None,
    )
    .0;
    let applied = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let base = Base {
        items: vec![Box::new(BaseItem {
            probes: Mutex::new(vec![ok()]),
            applied,
        })],
        held: None,
        clt: None,
        toolset: None,
        targo: None,
    };
    let rows = run(&sys, &ctx, base, &groups, Mode::Plan, &mut NoJournal).unwrap();
    assert_eq!(rows[0].id, "base.pkg.shims");
    let github: Vec<&ItemReport> = rows.iter().filter(|row| row.id == "init.github").collect();
    assert_eq!(github.len(), 1, "{rows:?}");
    assert_eq!(github[0].outcome, Outcome::Unknown);
    assert!(
        github[0]
            .why
            .as_deref()
            .is_some_and(|why| why.contains("init.toml"))
    );
    assert!(
        rows.iter().any(|row| row.class == Some(Class::Preference)),
        "{rows:?}"
    );
}

/// The Permissions phase: a row for each grant aterm needs, Accessibility only for a run
/// that signs out; a grant only the person switches on names how, and an apply is done
/// with it while `--check` is not. A grant this process cannot read (an aterm outside
/// aterm.app reads none) fails nothing: the run exits 0.
#[test]
fn each_missing_grant_is_one_row_naming_how_and_only_a_check_waits_on_it() {
    fn read(sign_out: bool) -> Vec<(Access, Grant)> {
        assert!(!sign_out, "this run does not sign out");
        vec![
            (
                Access::FullDiskAccess,
                Grant::NeedsYou("switch on aterm".into()),
            ),
            (Access::DeveloperTools, Grant::On),
            (
                Access::Debugger,
                Grant::Unread("tccd did not answer".into()),
            ),
        ]
    }
    let rows = permissions(read, false);
    assert_eq!(
        rows.iter().map(|row| row.id()).collect::<Vec<_>>(),
        [
            "init.permissions.full-disk-access",
            "init.permissions.developer-tools",
            "init.permissions.debugger"
        ]
    );
    let sys = Fake::root();
    let alice = at_console(user("alice", 501));
    assert!(matches!(
        rows[0].probe(&sys, &alice),
        Probe::Person { how, .. } if how == "switch on aterm"
    ));
    assert!(matches!(rows[1].probe(&sys, &alice), Probe::Ok { .. }));
    assert!(matches!(
        rows[2].probe(&sys, &alice),
        Probe::NotApplicable { why } if why.contains("tccd did not answer")
    ));
    assert_eq!(permissions(no_grants, true).len(), 4);
    let reported = run(
        &sys,
        &alice,
        Base::none(),
        &[(rows, workstation_place(Class::Person))],
        Mode::Apply,
        &mut NoJournal,
    )
    .unwrap();
    let person = reported
        .iter()
        .find(|row| row.outcome == Outcome::Person)
        .expect("the grant only the person switches on");
    assert!(counts(person, Action::Apply) && !counts(person, Action::Check));
    assert!(tally(&reported, Action::Apply).ends_with("needs you: Full Disk Access"));
    assert_eq!(status(&reported, Action::Apply), 0, "{reported:?}");
    // `--check` reads the grant as not in place, and as a permission, not a failure.
    assert_eq!(status(&reported, Action::Check), 77, "{reported:?}");
    // CONTROL: the same rows with one that failed read as a failure under `--check`.
    let mut failed = reported.clone();
    failed.push(ItemReport {
        id: "base.failed".into(),
        outcome: Outcome::Failed,
        ..person.clone()
    });
    assert_eq!(status(&failed, Action::Check), 1, "{failed:?}");
}

#[test]
fn tally_names_swarm_enabled_false_in_init_check_summary() {
    let swarm_off_row = ItemReport {
        id: "init.secrets@alice".into(),
        family: "secrets".into(),
        layer: Some(Layer::Workstation),
        class: None,
        title: "Secrets vault (aterm help secrets)".into(),
        outcome: Outcome::NotApplicable,
        from: None,
        to: None,
        why: Some(aterm_secrets::swarm::SWARM_OFF.into()),
        revert: None,
        effect: aterm_converge::Effect::Now,
        detail: None,
        explain: None,
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    };
    let line = tally(&[swarm_off_row], Action::Plan);
    assert_eq!(
        line,
        "aterm init: 1 ok \u{b7} [swarm] enabled = false (nothing changed)"
    );
}

/// What one run of the Mac Setup tab's program did: its exit code, everything it
/// wrote to the tab, and how many times it converged.
struct TabRun {
    code: u8,
    said: Vec<String>,
    converged: u32,
}

const TAB_ATERM: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

const TAB_CC: &str = "/Library/Developer/CommandLineTools/usr/bin/cc";

/// The `sudo` argv on `alice_with_checkout`, in the tab and outside it: `-k`, the plain prompt
/// and the root half's `--plain`, which reads no file.
fn tab_sudo() -> String {
    format!("tty /usr/bin/sudo -k -p 'Mac password: ' {TAB_ATERM} machines here apply --plain")
}

/// [`tab`] over `sys` (`live`: in the person's own login), composing `base.clt` alone as
/// [`tab_entry`] does, its probe finding [`TAB_CC`] or nothing.
fn tab_run(sys: &Fake, live: bool) -> TabRun {
    let said = Arc::new(Mutex::new(Vec::new()));
    let out = crate::init_clt::Tab::new({
        let said = Arc::clone(&said);
        Arc::new(move |text: &str| said.lock().unwrap().push(text.to_owned()))
    });
    let converged = std::sync::atomic::AtomicU32::new(0);
    let only = options(&["--only".into(), crate::init_clt::ID.into()]).unwrap();
    let code = tab(sys, &out, &|| live, &|tab| {
        converged.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let base = |_: bool| Base {
            clt: Some(
                crate::init_clt::CommandLineTools::new(
                    Box::new(|sys: &dyn System| {
                        if sys.read_file(Path::new(TAB_CC)).is_ok() {
                            crate::init_clt::Found::Present(TAB_CC.into())
                        } else {
                            crate::init_clt::Found::Missing("no developer directory".into())
                        }
                    }),
                    PathBuf::from(TAB_ATERM),
                )
                .in_tab(tab.clone()),
            ),
            ..Base::none()
        };
        let groups =
            |_: &dyn System, _: &Ctx, _: Mode, _: &Reading<'_>| Ok((Vec::new(), Picked::NONE));
        converge(
            sys,
            &only,
            &Front {
                base: &base,
                groups: &groups,
                full_disk_access: &|| sys.full_disk_access(),
                targo: None,
                cwd: Some(Path::new("/Users//alice/aterm")),
                records: &NoRecords,
                on_row: &|_| {},
                present: &|| true,
                on_held: &|_| {},
            },
            &mut NoJournal,
            &mut Laps::start(),
        )
    });
    let said = said.lock().unwrap().clone();
    TabRun {
        code,
        said,
        converged: converged.load(std::sync::atomic::Ordering::SeqCst),
    }
}

/// The tab's run is `--only base.clt` alone under the tab's variable; without the variable
/// (or with it empty), or with any other word or flag, `aterm init` speaks as it always did.
#[test]
fn only_base_clt_under_the_tab_variable_is_the_tabs_program() {
    let tab_var = Some(std::ffi::OsStr::new("1"));
    let parse =
        |args: &[&str]| options(&args.iter().map(OsString::from).collect::<Vec<_>>()).unwrap();
    assert!(in_tab(tab_var, &parse(&["--only", "base.clt"])));
    assert!(in_tab(tab_var, &parse(&["--only=base.clt"])));
    assert!(!in_tab(None, &parse(&["--only", "base.clt"])));
    assert!(!in_tab(
        Some(std::ffi::OsStr::new("")),
        &parse(&["--only", "base.clt"])
    ));
    for other in [
        &["--only", "base.clt,shell"][..],
        &["--only", "base"],
        &["--only", "base.clt", "--json"],
        &["--plan", "--only", "base.clt"],
        &[],
    ] {
        assert!(!in_tab(tab_var, &parse(other)), "{other:?}");
    }
}

/// `aterm init --only base.clt` under the tab's variable is the tab's own run, whose code is
/// the run's exit: none of `aterm init`'s ports (its base layer, Full Disk Access, the
/// records, the grants, the walk), which here panic, is touched first.
/// NEGATIVE CONTROL: an entry that never hands the tab its run composes init's base layer,
/// whose port panics, and never exits 7.
#[test]
fn the_tabs_variable_hands_the_run_to_the_tab() {
    struct NoRecords;
    impl Records for NoRecords {
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            panic!("the tab's run held the setup records first")
        }
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = crate::sync::tests::Inert::new(scratch.path());
    let host = Host {
        decided: Decider::Person,
        base: &|_| panic!("the tab's run composed init's base layer first"),
        full_disk_access: &|| panic!("the tab's run measured Full Disk Access"),
        records: &NoRecords,
        grants: |_| panic!("the tab's run read the grants"),
        workstation_check: &|_| panic!("the tab's run read workstation observations"),
        walk: &|_| panic!("the tab's run walked the grants"),
        converge: &ports,
        alab: &|| panic!("the tab's run read the ALab scope first"),
        present: || panic!("the tab's run read who is at the screen first"),
    };
    let ran = std::cell::Cell::new(0);
    let code = entry(
        vec!["--only".into(), "base.clt".into()],
        &host,
        Some(std::ffi::OsStr::new("1")),
        &|options, _| {
            ran.set(ran.get() + 1);
            assert_eq!(options.only, [crate::init_clt::ID]);
            7
        },
    );
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(7)));
    assert_eq!(ran.get(), 1);

    // Exercise the ordinary parser/entry as well as the earlier direct completion
    // boundary. All host effects are Fake; only its selected base.clt row runs.
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Released(std::sync::Arc<AtomicBool>);
    impl Drop for Released {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let released = std::sync::Arc::new(AtomicBool::new(false));
    let published = AtomicUsize::new(0);
    let ordinary = Host {
        decided: Decider::Person,
        base: &|_| Base {
            items: Vec::new(),
            held: Some(Box::new(Released(std::sync::Arc::clone(&released)))),
            clt: Some(crate::init_clt::CommandLineTools::new(
                Box::new(|_| crate::init_clt::Found::Present("already installed".into())),
                PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
            )),
            toolset: None,
            targo: None,
        },
        full_disk_access: &|| None,
        records: &crate::init::NoRecords,
        grants: |_| Vec::new(),
        workstation_check: &|_| panic!("base.clt reads no workstation"),
        walk: &|_| None,
        converge: &ports,
        alab: &|| false,
        present: || true,
    };
    let answer = entry_with_system(
        vec!["--only".into(), "base.clt".into(), "--json".into()],
        &ordinary,
        None,
        &|_, _| panic!("ordinary entry is not the tab worker"),
        &|| true,
        &|| {
            let sys = alice_with_checkout(501, None);
            sys.file(
                crate::root_steps::ADMITTED_NAME_RULE,
                crate::root_steps::name_rule("alice").unwrap().as_bytes(),
            );
            sys.stat_of(
                crate::root_steps::ADMITTED_NAME_RULE,
                crate::port::Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o440,
                    dir: false,
                    symlink: false,
                    len: crate::root_steps::name_rule("alice").unwrap().len() as u64,
                },
            );
            assert_eq!(crate::root_steps::name_rule_current(&sys), Ok(true));
            Box::new(sys)
        },
        &|current| {
            assert!(
                released.load(Ordering::SeqCst),
                "publication follows the base release"
            );
            current()?;
            published.fetch_add(1, Ordering::SeqCst);
            Ok(Some("macOS names match the freshly saved choice".into()))
        },
    );
    assert_eq!(format!("{answer:?}"), exit(0));
    assert_eq!(
        published.load(Ordering::SeqCst),
        1,
        "actual ordinary entry must call its completion port"
    );
}

/// The tab's program starts nothing and says nothing until the window types its go line
/// (read with echo off, keeping what was typed before the read), and ends on 2, so a tab
/// whose go line never came is never taken for an install that ran.
#[test]
fn the_tab_starts_nothing_and_says_nothing_until_its_go_line() {
    for answer in [
        Some(Some("installx")),
        Some(Some("Install")),
        Some(Some("")),
        Some(None),
        None,
    ] {
        let mut sys = alice_with_checkout(501, None);
        sys.tty = true;
        if let Some(answer) = answer {
            sys.answer(answer);
        }
        let before = sys.calls();
        let run = tab_run(&sys, true);
        assert_eq!(run.code, TAB_NOT_STARTED, "{answer:?}");
        assert!(run.said.is_empty(), "{answer:?}: {:?}", run.said);
        assert_eq!(run.converged, 0, "{answer:?}");
        let calls = sys.calls();
        assert_eq!(
            calls[before.len()..],
            ["ask kept ".to_owned()],
            "{answer:?}: one read with echo off, then nothing"
        );
    }
}

/// Past its go line, the tab's program clears the screen, says its one intro line right
/// before `sudo` asks for the Mac password with the plain prompt, runs the root half
/// `--plain`, and ends on its one installed line and 0. Tools already there end on the same
/// line with nothing asked.
#[test]
fn the_tab_installs_and_ends_on_its_one_installed_line() {
    let mut sys = alice_with_checkout(501, None);
    sys.running(&[]);
    sys.tty = true;
    sys.answer(Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO));
    let sudo = tab_sudo();
    sys.on(&sudo, Ran::ok(""))
        .on_write(&sudo, Path::new(TAB_CC), b"");
    let run = tab_run(&sys, true);
    assert_eq!(
        run.said,
        [
            TAB_CLEAR.to_owned(),
            format!("{}\n", crate::init_clt::TAB_INTRO),
            format!("{}\n", crate::init_clt::TAB_INSTALLED),
        ],
        "{:?}",
        sys.calls()
    );
    assert_eq!(run.code, 0);
    assert_eq!(run.converged, 1);
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.starts_with("tty "))
            .collect::<Vec<_>>(),
        [&sudo]
    );

    let mut there = alice_with_checkout(501, None);
    there.tty = true;
    there
        .answer(Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO))
        .file(TAB_CC, b"");
    let run = tab_run(&there, true);
    assert_eq!(
        run.said,
        [
            TAB_CLEAR.to_owned(),
            format!("{}\n", crate::init_clt::TAB_INSTALLED),
        ]
    );
    assert_eq!(run.code, 0);
    assert!(!there.calls().iter().any(|call| call.starts_with("tty ")));
}

/// Every way the tab's install falls short, early ones included, ends on its one plain line
/// `Mac Setup did not finish: <reason>.` and
/// its class's exit (1, or a stop's and Apple's window closed's own): `sudo`'s own refusal, each
/// class the root half exits with, a signal, a `sudo` that never ran, a run outside the
/// person's own login, and a refused converge.
#[test]
fn the_tab_ends_on_one_line_saying_why_the_install_did_not_finish() {
    use crate::init_clt::Unfinished;
    let go = aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO;
    let ends_on = |run: &TabRun, why: Unfinished, intro: bool| {
        let mut said = vec![TAB_CLEAR.to_owned()];
        if intro {
            said.push(format!("{}\n", crate::init_clt::TAB_INTRO));
        }
        said.push(format!("{}\n", why.line()));
        assert_eq!(run.said, said, "{why:?}");
        assert_eq!(
            run.code,
            if !intro && why == Unfinished::NotOwnLogin {
                7 // The tab refused its own login before entering the root command.
            } else {
                why.tab_exit()
            },
            "{why:?}"
        );
    };
    let sudo = tab_sudo();
    for (ran, why) in [
        (Some(Ran::exit(1, "")), Unfinished::Password),
        (Some(Ran::exit(80, "")), Unfinished::Catalog),
        (Some(Ran::exit(81, "")), Unfinished::NoneOffered),
        (Some(Ran::exit(82, "")), Unfinished::Download),
        (Some(Ran::exit(83, "")), Unfinished::Installer),
        (Some(Ran::exit(84, "")), Unfinished::Busy),
        (Some(Ran::exit(85, "")), Unfinished::Stopped),
        (Some(Ran::exit(86, "")), Unfinished::AppleWindow),
        (Some(Ran::exit(87, "")), Unfinished::Other),
        (Some(Ran::exit(88, "")), Unfinished::WindowClosed),
        (Some(Ran::exit(127, "")), Unfinished::Other),
        (
            Some(Ran {
                status: None,
                ..Ran::default()
            }),
            Unfinished::Other,
        ),
        // Exit 0 with nothing installed after it.
        (Some(Ran::ok("")), Unfinished::Other),
        // `sudo` never ran.
        (None, Unfinished::Other),
    ] {
        let mut sys = alice_with_checkout(501, None);
        sys.running(&[]);
        sys.tty = true;
        sys.answer(Some(go));
        if let Some(ran) = ran {
            sys.on(&sudo, ran);
        }
        ends_on(&tab_run(&sys, true), why, true);
    }

    // Outside the person's own login nothing is converged.
    let mut sys = alice_with_checkout(501, None);
    sys.tty = true;
    sys.answer(Some(go));
    let run = tab_run(&sys, false);
    ends_on(&run, Unfinished::NotOwnLogin, false);
    assert_eq!(run.converged, 0);

    // A converge refused before the item (run as root) is the other class.
    let mut root = alice_with_checkout(501, None);
    root.tty = true;
    root.euid = 0;
    root.answer(Some(go));
    let run = tab_run(&root, true);
    ends_on(&run, Unfinished::Other, false);
    assert_eq!(run.converged, 1);

    // So is a probe with no terminal to ask in.
    let sys = alice_with_checkout(501, None);
    sys.running(&[]);
    sys.answer(Some(go));
    ends_on(&tab_run(&sys, true), Unfinished::Other, false);
}

/// In Low Data Mode the tab asks for no password and ends on its own line, naming Low Data
/// Mode and how to go on, with its own exit, which the window reads to wait for the mode to
/// end without spending its one automatic opening. An account that is no administrator of
/// the Mac, which `sudo` would refuse, ends on its own line and exit too.
#[test]
fn low_data_mode_and_a_non_administrator_each_end_the_tab_on_their_own_line_and_exit() {
    use crate::init_clt::Unfinished;
    for (why, code, line) in [
        (
            Unfinished::LowData,
            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA,
            crate::init_clt::TAB_LOW_DATA,
        ),
        (
            Unfinished::NotAdmin,
            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN,
            crate::init_clt::TAB_NOT_ADMIN,
        ),
    ] {
        let mut sys = alice_with_checkout(501, None);
        sys.running(&[]);
        sys.tty = true;
        match why {
            Unfinished::LowData => sys.low_data = Some(true),
            _ => sys.admin = Some(false),
        }
        sys.answer(Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO));
        let run = tab_run(&sys, true);
        assert_eq!(run.code, code, "{why:?}");
        assert_eq!(why.tab_exit(), code, "{why:?}");
        assert_eq!(
            run.said,
            [TAB_CLEAR.to_owned(), format!("{line}\n")],
            "{why:?}"
        );
        assert!(
            !sys.calls().iter().any(|call| call.starts_with("tty ")),
            "{why:?}: no password asked: {:?}",
            sys.calls()
        );
    }
}

/// The tab's program (`--only base.clt`) composes init's base layer as a read: Apple's
/// Command Line Tools touch no store, so the tab never waits for an install pass that holds
/// it before its password prompt. CONTROL: any other run that applies waits for the lock,
/// and `--no-wait` only tries it.
#[test]
fn the_tab_never_asks_for_the_store_lock() {
    let parse =
        |args: &[&str]| options(&args.iter().map(OsString::from).collect::<Vec<_>>()).unwrap();
    assert_eq!(
        parse(&["--only", "base.clt"]).base_lock(true),
        BaseLock::Read
    );
    assert_eq!(parse(&["--only=base.clt"]).base_lock(true), BaseLock::Read);
    assert_eq!(parse(&[]).base_lock(true), BaseLock::Wait);
    assert_eq!(parse(&["--only", "base"]).base_lock(true), BaseLock::Wait);
    assert_eq!(
        parse(&["--only", "base.clt,base.pkg.toolset"]).base_lock(true),
        BaseLock::Wait
    );
    assert_eq!(parse(&["--no-wait"]).base_lock(true), BaseLock::Try);
    assert_eq!(parse(&[]).base_lock(false), BaseLock::Read);
}

/// `base.clt` with the tools missing, run over `sys` in its login's own context.
fn clt_rows(sys: &Fake) -> Vec<ItemReport> {
    let (ctx, _) = local_context(sys).unwrap();
    let base = Base {
        clt: Some(
            crate::init_clt::CommandLineTools::new(
                Box::new(|_: &dyn System| {
                    crate::init_clt::Found::Missing("no developer directory".into())
                }),
                PathBuf::from(TAB_ATERM),
            )
            .saying(Arc::new(|_: &str| {})),
        ),
        ..Base::none()
    };
    run(sys, &ctx, base, &[], Mode::Apply, &mut NoJournal).unwrap()
}

/// Who may be asked for the Mac password: installing Apple's Command Line Tools is a
/// person's step at this Mac's console (design §6.3, §7). In an agent's tab, or with no
/// terminal at all (an agent's tool call, a script), nothing asks for it, the row waits for
/// a person there, and the run exits 0. CONTROL: a person at the console is asked once.
#[test]
fn off_the_console_init_asks_no_password_for_the_command_line_tools_and_exits_0() {
    let sudo = tab_sudo();
    for (terminal, agent) in [
        (true, Some("Claude Code")),
        (false, Some("Claude Code")),
        (false, None),
    ] {
        let mut sys = alice_at(501);
        sys.running(&[]);
        sys.terminal = terminal;
        sys.tty = terminal;
        sys.agent = agent;
        assert_ne!(crate::console::presence(&sys), Presence::AtConsole);
        let rows = clt_rows(&sys);
        let row = rows
            .iter()
            .find(|row| row.id == crate::init_clt::ID)
            .unwrap();
        assert!(
            !sys.calls().iter().any(|call| call.starts_with("tty ")),
            "{terminal} {agent:?}: asked for the Mac password: {:?}",
            sys.calls()
        );
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        assert_eq!(status(&rows, Action::Apply), 0, "{row:?}");
        assert_eq!(status(&rows, Action::Check), 77, "{row:?}");
    }

    let mut sys = alice_at(501);
    sys.terminal = true;
    sys.tty = true;
    sys.agent = None;
    assert_eq!(init_presence(&sys), Presence::AtConsole);
    sys.running(&[]);
    sys.on(&sudo, Ran::exit(1, ""));
    let rows = clt_rows(&sys);
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.starts_with("tty "))
            .collect::<Vec<_>>(),
        [&sudo]
    );
    assert_ne!(status(&rows, Action::Apply), 0);
}

/// `aterm init` at the console while an install of the tools runs (System State's tab, or
/// another account's) asks for no password and fails nothing: the row waits for that
/// install, and the run exits 0. CONTROL: a placeholder nothing holds is no install.
#[test]
fn during_a_running_clt_install_init_asks_no_password_and_fails_nothing() {
    let console = || {
        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.tty = true;
        sys.agent = None;
        sys.file(crate::items::developer::PLACEHOLDER, b"");
        sys
    };
    let sys = console();
    sys.running(&["/usr/sbin/softwareupdate"]);
    let rows = clt_rows(&sys);
    let row = rows
        .iter()
        .find(|row| row.id == crate::init_clt::ID)
        .unwrap();
    assert!(
        !sys.calls().iter().any(|call| call.starts_with("tty ")),
        "an install is running: no password asked: {:?}",
        sys.calls()
    );
    assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
    assert!(!row.outcome.is_failure(), "{row:?}");
    assert_eq!(status(&rows, Action::Apply), 0, "{row:?}");

    let stale = console();
    stale.running(&["/tmp/installer"]);
    let _ = clt_rows(&stale);
    assert!(
        stale.calls().iter().any(|call| call.starts_with("tty ")),
        "{:?}",
        stale.calls()
    );
}

/// One converge of `sys` with `prepare`'s groups and `records`, a person at the screen as
/// `present` says: its rows (or why it was refused), and the lines of what the person's
/// choices held ([`Front::on_held`]).
fn converge_held(
    sys: &Fake,
    options: &Options,
    prepare: &Prepare<'_>,
    records: &dyn Records,
    present: &(dyn Fn() -> bool + Sync),
) -> (Result<Vec<ItemReport>, Refused>, Vec<ItemReport>) {
    let held = Mutex::new(Vec::new());
    let rows = converge(
        sys,
        options,
        &Front {
            base: &|_| Base::none(),
            groups: prepare,
            full_disk_access: &|| sys.full_disk_access(),
            targo: None,
            cwd: None,
            records,
            on_row: &|_| {},
            present,
            on_held: &|row| held.lock().unwrap().push(row),
        },
        &mut NoJournal,
        &mut Laps::start(),
    );
    (rows, held.into_inner().unwrap())
}

/// [`desktop_rows`]'s pair: the items, and each one's tag.
type DesktopRows = (Vec<Box<dyn Item>>, Vec<(String, Setting)>);

/// The sign-outs for `ctx`'s one user and a Dock row, each a change, tagged with the System
/// State row each is built for ([`Picked::rows`]).
fn desktop_rows(ctx: &Ctx) -> DesktopRows {
    let mut items: Vec<Box<dyn Item>> = crate::init_accounts::items(ctx)
        .unwrap()
        .into_iter()
        .map(|item| Listed::boxed(item.id(), vec![change(), ok()]))
        .collect();
    items.push(Listed::boxed("init.test.dock", vec![change(), ok()]));
    let tags = items
        .iter()
        .map(|item| {
            let id = item.id();
            let setting = if id.starts_with("init.messages.") {
                Setting::Messages
            } else if id.starts_with("init.facetime.") {
                Setting::FaceTime
            } else {
                Setting::Dock
            };
            (id, setting)
        })
        .collect();
    (items, tags)
}

/// System State's stopped revision before and after init waits for the window's pass.
struct StoppedWhileWaiting {
    waited: std::sync::atomic::AtomicBool,
    before: Option<u64>,
    after: u64,
}

impl Records for StoppedWhileWaiting {
    fn hold(&self, _pass: Pass, wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
        if let Some(wait) = wait {
            wait("waiting while System State changes this Mac's settings");
        }
        self.waited.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(NoRecords))
    }
    fn holds(&self) -> Holds {
        let stopped_revision = if self.waited.load(std::sync::atomic::Ordering::SeqCst) {
            Some(self.after)
        } else {
            self.before
        };
        Holds {
            stopped: stopped_revision.map(|_| "you stopped setup in System State".to_owned()),
            stopped_revision,
            ..Holds::NONE
        }
    }
}

/// A Stop the person presses in System State while `aterm init` waits for the records holds
/// init's preferences and sign-outs, as a Stop pressed before init started does (principle
/// 4): the holds are read again once the records are held, and the run says what it left.
#[test]
fn a_stop_pressed_while_init_waits_for_the_records_holds_its_desktop_lane() {
    let records = StoppedWhileWaiting {
        waited: std::sync::atomic::AtomicBool::new(false),
        before: None,
        after: 1,
    };
    let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
        let (items, tags) = desktop_rows(ctx);
        let mut picked = picked(
            reading.picks,
            reading.only,
            &|| true,
            &records.holds(),
            true,
        );
        picked.rows = tags;
        Ok((vec![(items, place(Class::Preference))], picked))
    };
    let sys = alice_with_checkout(501, None);
    let (rows, held) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
    let rows = rows.unwrap();
    let changed: Vec<&str> = rows
        .iter()
        .filter(|row| row.outcome == Outcome::Changed)
        .map(|row| row.id.as_str())
        .collect();
    assert!(
        changed.is_empty(),
        "the person's Stop landed before init changed anything, yet init changed {changed:?}"
    );
    let line = held
        .iter()
        .find(|row| row.id == HELD)
        .and_then(|row| row.why.clone())
        .expect("the held line");
    assert!(
        line.starts_with("left as they are because you stopped setup in System State")
            && line.contains("Dock")
            && line.contains("Messages"),
        "{line}"
    );
}

/// System State's records, stopped once `stop` is set (a Stop pressed in System State).
struct StoppedLater(Arc<std::sync::atomic::AtomicBool>);

impl Records for StoppedLater {
    fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
        Ok(Box::new(NoRecords))
    }
    fn holds(&self) -> Holds {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            Holds {
                stopped: Some("you stopped setup in System State".to_owned()),
                stopped_revision: Some(1),
                ..Holds::NONE
            }
        } else {
            Holds::NONE
        }
    }
}

/// A GitHub batch the person stops after a clone ran its hook installer: its row says their
/// words and what the installer did, both printed live and returned, and is not deferred as
/// a clone owed until this Mac is let into the vault, which it is not waiting for.
/// NEGATIVE CONTROL: deferring every waiting row as owed answers `OWED` here, the person's
/// words and the installer's line gone.
#[test]
fn a_late_stop_keeps_the_persons_words_and_the_installer_line_on_the_github_row() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Batch(Arc<AtomicBool>);
    impl Item for Batch {
        fn id(&self) -> String {
            "init.github.batch@alice".into()
        }
        fn family(&self) -> &'static str {
            "github"
        }
        fn title(&self) -> String {
            self.id()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            change()
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            panic!("the wrapper forwards its hold into the batch")
        }
        fn apply_guarded(
            &self,
            _: &dyn System,
            _: &Ctx,
            current: &(dyn Fn() -> Result<(), String> + Sync),
        ) -> Result<(), String> {
            current()?;
            // The clone is published and its installer ran; the person presses Stop.
            self.0.store(true, Ordering::SeqCst);
            current().map_err(|why| {
                format!("{why}; $HOME/trust: ran its scripts/install-git-hooks.sh: installed")
            })
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let records = StoppedLater(Arc::clone(&stop));
    let batch = Arc::clone(&stop);
    let prepare = move |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
        let picked = picked(reading.picks, reading.only, &|| true, &Holds::NONE, true);
        let items: Vec<Box<dyn Item>> = vec![Box::new(Batch(Arc::clone(&batch)))];
        let _ = ctx;
        Ok((vec![(items, anywhere(Class::Install))], picked))
    };
    let sys = alice_with_checkout(501, None);
    let printed = Mutex::new(Vec::new());
    let rows = converge(
        &sys,
        &Options::default(),
        &Front {
            base: &|_| Base::none(),
            groups: &prepare,
            full_disk_access: &|| sys.full_disk_access(),
            targo: None,
            cwd: None,
            records: &records,
            on_row: &|row| printed.lock().unwrap().push(row.clone()),
            present: &|| true,
            on_held: &|_| {},
        },
        &mut NoJournal,
        &mut Laps::start(),
    )
    .unwrap();
    assert!(stop.load(Ordering::SeqCst), "the batch ran");
    let printed = printed.into_inner().unwrap();
    for rows in [&rows, &printed] {
        let row = rows
            .iter()
            .find(|row| row.id == "init.github.batch@alice")
            .expect("the batch's row");
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert_eq!(
            row.why.as_deref(),
            Some(
                "left as it is because you stopped setup in System State; $HOME/trust: ran its \
                 scripts/install-git-hooks.sh: installed"
            ),
            "{row:?}"
        );
    }
}

/// System State's records as init holds them: `taken` once held, `held` while held, as the
/// profile lock is.
struct Watched {
    taken: Arc<std::sync::atomic::AtomicBool>,
    held: Arc<std::sync::atomic::AtomicBool>,
}

impl Records for Watched {
    fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
        self.held.store(true, std::sync::atomic::Ordering::SeqCst);
        self.taken.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(Releases(Arc::clone(&self.held))))
    }
}

/// The hold [`Watched`] hands out: it lets the records go when released or dropped.
struct Releases(Arc<std::sync::atomic::AtomicBool>);

impl Held for Releases {
    fn verified(self: Box<Self>) -> Result<(), String> {
        Ok(())
    }
    fn release(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    fn again(&mut self, _wait: Option<&dyn Fn(&str)>) -> Result<(), Unheld> {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for Releases {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// init holds System State's records (profile.lock) only over its Desktop lane's own
/// changes: the preferences, then the sign-outs, which run last. While its clones (or the
/// Command Line Tools) run in the Network lane, the window's Stop, a group turned off,
/// Restore and Set up now never wait behind it. Here the clone waits (a one-minute hang
/// detector) for the records to be let go after the Dock changed.
#[test]
fn init_lets_system_states_records_go_while_its_clones_run() {
    use std::sync::atomic::Ordering;
    let (taken, held) = (
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let saw = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let records = Watched {
        taken: Arc::clone(&taken),
        held: Arc::clone(&held),
    };
    let clone = {
        let (taken, held, saw) = (Arc::clone(&taken), Arc::clone(&held), Arc::clone(&saw));
        move || {
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_secs(60) {
                if taken.load(Ordering::SeqCst) && !held.load(Ordering::SeqCst) {
                    saw.store(true, Ordering::SeqCst);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    };
    let prepare = move |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
        let (items, _) = desktop_rows(ctx);
        Ok((
            vec![
                (items, place(Class::Preference)),
                (
                    vec![then("init.test.clone", "github", clone.clone())],
                    place(Class::Install),
                ),
            ],
            Picked::ALL,
        ))
    };
    let sys = alice_with_checkout(501, None);
    let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
    let rows = rows.unwrap();
    assert!(
        saw.load(Ordering::SeqCst),
        "init kept System State's records held for the whole minute its clone ran"
    );
    assert!(
        rows.iter()
            .filter(|row| crate::init_accounts::signs_out(&row.id))
            .all(|row| row.outcome == Outcome::Changed),
        "the sign-outs ran, under a fresh hold: {rows:?}"
    );
    assert!(
        !held.load(Ordering::SeqCst),
        "the records are let go at the end"
    );
}

/// With no terminal, presence is read once, as the apply is prepared: a person at the screen
/// then admits the Messages and FaceTime sign-outs, which run last, after every other lane
/// (the clones included), whether or not that person is still there by then; nothing reads
/// presence again before the sign-outs or their clicks (decided 2026-11-05). Control: a
/// person still there gets them too.
#[test]
fn with_no_terminal_presence_is_read_once_as_the_run_is_prepared() {
    use std::sync::atomic::Ordering;
    for walks_away in [true, false] {
        let here = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut sys = alice_at(501);
        sys.terminal = false;
        sys.agent = None;
        assert_eq!(crate::console::presence(&sys), Presence::NoTerminal);
        let gone = Arc::clone(&here);
        let prepare = move |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
            let gone = Arc::clone(&gone);
            let items: Vec<Box<dyn Item>> = crate::init_accounts::items(ctx)
                .unwrap()
                .into_iter()
                .map(|item| Listed::boxed(item.id(), vec![change(), ok()]))
                .collect();
            // A clone that takes a while; the person walks away meanwhile.
            let clone = then("init.test.clone", "github", move || {
                if walks_away {
                    gone.store(false, Ordering::SeqCst);
                }
            });
            Ok((
                vec![
                    (items, place(Class::Preference)),
                    (vec![clone], anywhere(Class::Install)),
                ],
                Picked::ALL,
            ))
        };
        let present = || here.load(Ordering::SeqCst);
        let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &NoRecords, &present);
        let rows = rows.unwrap();
        let sign_outs: Vec<&ItemReport> = rows
            .iter()
            .filter(|row| crate::init_accounts::signs_out(&row.id))
            .collect();
        assert_eq!(sign_outs.len(), 2, "{rows:?}");
        assert!(
            sign_outs.iter().all(|row| row.outcome == Outcome::Changed),
            "walks_away={walks_away}: {sign_outs:#?}"
        );
    }
}

/// The held line names a working command for both a console terminal and an agent tool.
#[test]
fn the_held_line_off_the_console_names_a_way_back_that_runs_there() {
    for (agent, flags) in [(Some("Claude Code"), true), (None, true)] {
        let mut sys = alice_at(501);
        sys.terminal = true;
        sys.agent = agent;
        let (ctx, _) = local_context(&sys).unwrap();
        let mut off = Selection::none();
        off.set(Setting::Dock, true);
        let standing = Standing {
            decided: Decider::Person,
            alab: &|| true,
            present: &|| true,
            holds: Holds { off, ..Holds::NONE },
            workstation_observation: None,
        };
        let vault = crate::init_secrets::tests::Scripted::new(vec![Ok(
            crate::init_secrets::tests::unconfigured(),
        )]);
        let (_, picked) = prepare_groups_with(
            &sys,
            &ctx,
            Mode::Apply,
            None,
            &Reading {
                keyboards: &KeyboardRead::EMPTY,
                context_scope: Selection::all(),
                only: &[],
                picks: Picks::default(),
            },
            &standing,
            no_grants,
            move |_| Ok(vault as Arc<dyn crate::init_secrets::SecretsOps>),
        )
        .unwrap();
        let line = picked.held.expect("the held line");
        assert!(line.contains("Turn one on with its switch there"), "{line}");
        assert_eq!(
            line.contains(naming(Setting::Dock)),
            flags,
            "{agent:?}: {line}"
        );
        // The way back itself: it sets the Dock at the console, and in an agent's tab names
        // no item the run takes, saying the person left it as is.
        let words: Vec<OsString> = naming(Setting::Dock)
            .split_whitespace()
            .skip(2)
            .map(OsString::from)
            .collect();
        let mut way = options(&words).unwrap();
        way.station = way.only.clone();
        let holds = standing.holds.clone();
        let prepare = |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
            let picked = super::picked(reading.picks, reading.only, &|| true, &holds, flags);
            let items: Vec<Box<dyn Item>> = if picked.runs.contains(Setting::Dock) {
                vec![Listed::of("dock", "init.dock@alice", vec![change(), ok()])]
            } else {
                Vec::new()
            };
            Ok((vec![(items, place(Class::Preference))], picked))
        };
        let (ran, _) = converge_held(&sys, &way, &prepare, &NoRecords, &|| true);
        if flags {
            let ran = ran.unwrap();
            assert!(
                ran.iter()
                    .any(|row| row.id == "init.dock@alice" && row.outcome == Outcome::Changed),
                "{ran:?}"
            );
        } else {
            assert!(
                matches!(&ran, Err(Refused::Usage(why))
                    if why.contains("left as you set them in System State: Dock")),
                "{agent:?}: {ran:?}"
            );
        }
        let named = Options {
            picks: Picks {
                named: Choice {
                    profile: true,
                    ..Choice::NONE
                },
                left: Choice::NONE,
            },
            ..Options::default()
        };
        let none =
            |_: &dyn System, _: &Ctx, _: Mode, _: &Reading<'_>| Ok((Vec::new(), Picked::NONE));
        let (typed, _) = converge_held(&sys, &named, &none, &NoRecords, &|| true);
        assert_eq!(
            matches!(typed, Err(Refused::Usage(_))),
            !flags,
            "{agent:?}: {typed:?}"
        );
    }
}

/// The way back init names for the GitHub repositories ([`naming`], also the owed clone's
/// words when its record cannot be written) runs the repositories alone: on an ALab Mac
/// `--clone` adds to the scope's groups, which would also reset the Dock and sign out of
/// Messages and FaceTime.
#[test]
fn the_narrowest_way_back_for_the_repositories_runs_only_them() {
    let words: Vec<OsString> = naming(Setting::Repositories)
        .split_whitespace()
        .skip(2)
        .map(OsString::from)
        .collect();
    let mut options = options(&words).unwrap();
    // As `entry` reads it: a word no base item names is the workstation's.
    options.station = options.only.clone();
    let groups = |ctx: &Ctx| {
        let (items, _) = desktop_rows(ctx);
        vec![
            (items, place(Class::Preference)),
            (
                vec![then("init.github.native@alice", "github", || {})],
                place(Class::Install),
            ),
        ]
    };
    let sys = alice_with_checkout(501, None);
    let rows = converge_with(&sys, &options, &groups, &NoRecords).unwrap();
    let ran: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ran,
        ["init.github.native@alice"],
        "`{}` ran more than the repositories",
        naming(Setting::Repositories)
    );
}

/// Records another setup holds past init's bounded wait.
struct NeverLetGo;

impl Records for NeverLetGo {
    fn hold(&self, _pass: Pass, wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
        if let Some(wait) = wait {
            wait("waiting while System State changes this Mac's settings");
        }
        Err(Unheld::Busy(
            "left as it is: System State was changing this Mac's settings for 10 minutes".into(),
        ))
    }
}

/// When init's 10-minute wait for the records ends, the run still reports what its other
/// lanes did (the vault's join, the clones, the base layer) and exits 75, "a lock", as its
/// usage says: the Desktop lane's rows stand aside for that lock in plain words, nothing of
/// them changed. It does not end as a bare failure with every row lost.
#[test]
fn inits_bounded_wait_reports_the_other_lanes_and_exits_75() {
    let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
        let (items, _) = desktop_rows(ctx);
        Ok((
            vec![
                (items, place(Class::Preference)),
                (
                    vec![then("init.test.vault", crate::init_secrets::FAMILY, || {})],
                    place(Class::Install),
                ),
            ],
            Picked::ALL,
        ))
    };
    let sys = alice_with_checkout(501, None);
    let (result, _) = converge_held(&sys, &Options::default(), &prepare, &NeverLetGo, &|| true);
    let rows = match result {
        Ok(rows) => rows,
        Err(refused) => panic!("the wait's end discards the run's report: {refused:?}"),
    };
    assert!(
        rows.iter().any(|row| row.id == "init.test.vault"),
        "{rows:?}"
    );
    let desk: Vec<&ItemReport> = rows
        .iter()
        .filter(|row| {
            row.id.starts_with("init.test.dock") || crate::init_accounts::signs_out(&row.id)
        })
        .collect();
    assert_eq!(desk.len(), 3, "{rows:?}");
    assert!(
        desk.iter().all(|row| row.outcome != Outcome::Changed
            && row
                .stood_aside
                .as_ref()
                .is_some_and(|aside| aside.why().contains("10 minutes"))),
        "{desk:#?}"
    );
    assert_eq!(status(&rows, Action::Apply), 75, "{rows:?}");
}

/// A key that changed after aterm set it (design §6.6) is left as it is: `aterm init` leaves
/// it, says so without naming whose change it is, and names the ways that set it again; a
/// key not kept is changed as ever. With the record of those keys unreadable, it sets none.
#[test]
fn init_leaves_a_key_changed_since_aterm_set_it_and_names_the_way_to_set_it_again() {
    struct Kept(Result<Vec<String>, String>);
    impl Records for Kept {
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            Ok(Box::new(NoRecords))
        }
        fn kept(&self) -> Result<Vec<String>, String> {
            self.0.clone()
        }
    }
    let prepare = |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
        let mut picked = picked(
            reading.picks,
            reading.only,
            &|| true,
            &Holds::default(),
            true,
        );
        picked.rows = vec![
            ("init.test.dock".to_owned(), Setting::Dock),
            ("init.test.widgets".to_owned(), Setting::Widgets),
        ];
        Ok((
            vec![(
                vec![
                    Listed::boxed("init.test.dock", vec![change(), ok()]),
                    Listed::boxed("init.test.widgets", vec![change(), ok()]),
                ],
                place(Class::Preference),
            )],
            picked,
        ))
    };
    let sys = alice_with_checkout(501, None);
    let outcome =
        |rows: &[ItemReport], id: &str| rows.iter().find(|row| row.id == id).map(|row| row.outcome);
    let kept = Kept(Ok(vec!["init.test.dock".to_owned()]));
    let (rows, held) = converge_held(&sys, &Options::default(), &prepare, &kept, &|| true);
    let rows = rows.unwrap();
    assert_eq!(
        outcome(&rows, "init.test.dock"),
        Some(Outcome::NotApplicable)
    );
    assert_eq!(outcome(&rows, "init.test.widgets"), Some(Outcome::Changed));
    let line = held
        .iter()
        .find(|row| row.id == CHANGED)
        .and_then(|row| row.why.clone())
        .expect("the line of the keys changed since aterm set them");
    assert!(
        line.contains("Dock")
            && !line.contains("Widgets")
            && line.contains("changed since aterm set them")
            && !line.contains("you changed")
            && line.contains("Set up now in System State, or aterm doctor --apply"),
        "{line}"
    );
    let unread = Kept(Err("setup-kept.json: not JSON".to_owned()));
    let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &unread, &|| true);
    let rows = rows.unwrap();
    assert!(
        rows.iter().all(|row| row.outcome == Outcome::NotApplicable),
        "{rows:?}"
    );
}

/// A run with no terminal builds no preference (they wait for a person at the console), so
/// its Desktop lane holds the records for its sign-outs alone: a part of the profile, which
/// saves no plan and clears none of the window's finished stages ([`Pass::Part`]).
#[test]
fn a_run_that_sets_no_preference_takes_no_whole_hold() {
    struct Passes(Mutex<Vec<Pass>>);
    impl Records for Passes {
        fn hold(&self, pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            self.0.lock().unwrap().push(pass);
            Ok(Box::new(NoRecords))
        }
    }
    let mut sys = alice_at(501);
    sys.terminal = false;
    sys.agent = None;
    assert_eq!(crate::console::presence(&sys), Presence::NoTerminal);
    let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
        let (mut items, mut tags) = desktop_rows(ctx);
        // No preference: only the sign-outs.
        items.retain(|item| crate::init_accounts::signs_out(&item.id()));
        tags.retain(|(id, _)| crate::init_accounts::signs_out(id));
        let mut picked = picked(
            reading.picks,
            reading.only,
            &|| true,
            &Holds::default(),
            false,
        );
        picked.rows = tags;
        Ok((vec![(items, place(Class::Preference))], picked))
    };
    let records = Passes(Mutex::new(Vec::new()));
    let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
    let rows = rows.unwrap();
    assert!(
        rows.iter()
            .filter(|row| crate::init_accounts::signs_out(&row.id))
            .all(|row| row.outcome == Outcome::Changed),
        "{rows:?}"
    );
    assert_eq!(
        records.0.into_inner().unwrap(),
        [Pass::Part],
        "a run that set no preference held the records for a whole-profile pass"
    );
}

/// The person typed `aterm init --sign-out`, then pressed Stop in System State while init
/// waited for the records: the Stop is their later choice, and holds the sign-outs, which
/// nothing undoes, though a flag named them. A flag goes past only a hold on record when the
/// run began: an unchanged Stop, never another stopped revision with the same words.
#[test]
fn a_stop_pressed_after_init_started_holds_a_named_sign_out() {
    for (before, after, blocked) in [(None, 1, true), (Some(1), 1, false), (Some(1), 2, true)] {
        let records = StoppedWhileWaiting {
            waited: std::sync::atomic::AtomicBool::new(false),
            before,
            after,
        };
        let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
            let (items, tags) = desktop_rows(ctx);
            let mut picked = picked(
                reading.picks,
                reading.only,
                &|| true,
                &records.holds(),
                true,
            );
            picked.rows = tags;
            Ok((vec![(items, place(Class::Preference))], picked))
        };
        let sys = alice_with_checkout(501, None);
        assert_eq!(crate::console::presence(&sys), Presence::AtConsole);
        let named = Options {
            picks: Picks {
                named: Choice {
                    sign_out: true,
                    ..Choice::NONE
                },
                left: Choice::NONE,
            },
            ..Options::default()
        };
        let (rows, held) = converge_held(&sys, &named, &prepare, &records, &|| true);
        let rows = rows.unwrap();
        let signed_out: Vec<&str> = rows
            .iter()
            .filter(|row| {
                crate::init_accounts::signs_out(&row.id) && row.outcome == Outcome::Changed
            })
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(
            signed_out.len(),
            if blocked { 0 } else { 2 },
            "stopped revision {before:?} -> {after}: {signed_out:?}"
        );
        assert_eq!(
            held.iter().any(|row| row.id == HELD
                && row
                    .why
                    .as_deref()
                    .is_some_and(|why| why.contains("Messages, FaceTime"))),
            blocked,
            "{held:?}"
        );
    }
}

/// With the record of the keys changed since aterm set them unreadable, init sets no
/// preference ([`KEPT_UNREAD`]), so it records no Mac setting verified for the first launch:
/// the window's run then sets them.
#[test]
fn a_pass_that_could_not_read_the_kept_record_records_no_verified_settings() {
    type Log = Arc<Mutex<Vec<(bool, bool)>>>;
    struct Logged(Log);
    impl Held for Logged {
        fn checkpoint(&self, settings: Selection) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .push((settings.contains(Setting::Dock), false));
            Ok(())
        }
        fn verified(self: Box<Self>) -> Result<(), String> {
            self.0.lock().unwrap().push((true, true));
            Ok(())
        }
    }
    struct Unread(Log);
    impl Records for Unread {
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            Ok(Box::new(Logged(Arc::clone(&self.0))))
        }
        fn kept(&self) -> Result<Vec<String>, String> {
            Err("setup-kept.json: not JSON".to_owned())
        }
    }
    let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
        let (items, tags) = desktop_rows(ctx);
        let mut picked = picked(
            reading.picks,
            reading.only,
            &|| true,
            &Holds::default(),
            true,
        );
        picked.rows = tags;
        Ok((vec![(items, place(Class::Preference))], picked))
    };
    let sys = alice_with_checkout(501, None);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let records = Unread(Arc::clone(&log));
    let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
    let rows = rows.unwrap();
    assert!(
        rows.iter().all(|row| row.outcome != Outcome::Changed),
        "{rows:?}"
    );
    let log = log.lock().unwrap().clone();
    assert!(
        log.iter().all(|(dock, verified)| !dock && !verified),
        "init set no preference, yet recorded the Dock or the whole profile verified: \
         {log:?}"
    );
}

/// A sign-out the person undid after aterm made it (they signed back in) is in the record
/// of the keys changed since aterm set them (design §6.6), and the page reads "Changed since
/// aterm set it" on its row: `aterm init` leaves it as it is, as it leaves such a preference.
#[test]
fn init_leaves_a_sign_out_the_person_undid_as_they_set_it() {
    struct KeptSignOut(Vec<String>);
    impl Records for KeptSignOut {
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            Ok(Box::new(NoRecords))
        }
        fn kept(&self) -> Result<Vec<String>, String> {
            Ok(self.0.clone())
        }
    }
    let sys = alice_with_checkout(501, None);
    let (ctx, _) = local_context(&sys).unwrap();
    let (_, tags) = desktop_rows(&ctx);
    let messages = tags
        .iter()
        .find(|(_, setting)| *setting == Setting::Messages)
        .map(|(id, _)| id.clone())
        .expect("a Messages sign-out");
    let prepare = |_: &dyn System, ctx: &Ctx, _: Mode, reading: &Reading<'_>| {
        let (items, tags) = desktop_rows(ctx);
        let mut picked = picked(
            reading.picks,
            reading.only,
            &|| true,
            &Holds::default(),
            true,
        );
        picked.rows = tags;
        Ok((vec![(items, place(Class::Preference))], picked))
    };
    let records = KeptSignOut(vec![messages.clone()]);
    let (rows, held) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
    let rows = rows.unwrap();
    let outcome = |id: &str| rows.iter().find(|row| row.id == id).map(|row| row.outcome);
    assert_eq!(outcome(&messages), Some(Outcome::NotApplicable), "{rows:?}");
    let facetime = tags
        .iter()
        .find(|(_, setting)| *setting == Setting::FaceTime)
        .map(|(id, _)| id.as_str())
        .expect("a FaceTime sign-out");
    assert_eq!(outcome(facetime), Some(Outcome::Changed), "{rows:?}");
    assert!(
        held.iter().any(|row| row.id == CHANGED
            && row
                .why
                .as_deref()
                .is_some_and(|why| why.contains("Messages"))),
        "{held:?}"
    );
}

/// `aterm init --help`: "A group off in System State (all after a Stop) stays as is unless a
/// flag names it; one turned off mid-run stays as is". A Stop pressed while a clone runs, which may take half an
/// hour, holds the clones that have not started; one the run named over a hold already on
/// record when it began still runs.
#[test]
fn a_stop_pressed_mid_run_holds_the_clones_not_yet_started() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct StopLater(Arc<AtomicBool>, Holds);
    impl Records for StopLater {
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            Ok(Box::new(NoRecords))
        }
        fn holds(&self) -> Holds {
            Holds {
                stopped: self
                    .0
                    .load(Ordering::SeqCst)
                    .then(|| STOPPED.to_owned())
                    .or_else(|| self.1.stopped.clone()),
                stopped_revision: if self.0.load(Ordering::SeqCst) {
                    Some(2)
                } else {
                    self.1.stopped_revision
                },
                ..self.1.clone()
            }
        }
    }
    const STOPPED: &str = "you stopped setup in System State";
    let mut left = Selection::none();
    left.set(Setting::Repositories, true);
    for (start, named) in [
        (Holds::NONE, false),
        (
            Holds {
                off: left,
                ..Holds::NONE
            },
            true,
        ),
        (
            Holds {
                stopped: Some(STOPPED.to_owned()),
                stopped_revision: Some(1),
                ..Holds::NONE
            },
            true,
        ),
    ] {
        let stop = Arc::new(AtomicBool::new(false));
        let records = StopLater(Arc::clone(&stop), start.clone());
        let pressed = Arc::clone(&stop);
        let prepare = move |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
            let pressed = Arc::clone(&pressed);
            let picked = picked(reading.picks, reading.only, &|| true, &start, true);
            Ok((
                vec![(
                    vec![
                        // The first repository: the person presses Stop while it clones.
                        then("init.github.first@alice", "github", move || {
                            pressed.store(true, Ordering::SeqCst);
                        }),
                        Listed::of("github", "init.github.second@alice", vec![change(), ok()]),
                    ],
                    place(Class::Install),
                )],
                picked,
            ))
        };
        let sys = alice_with_checkout(501, None);
        let options = if named {
            Options {
                picks: Picks {
                    named: Choice {
                        clone: true,
                        ..Choice::NONE
                    },
                    left: Choice::NONE,
                },
                ..Options::default()
            }
        } else {
            Options::default()
        };
        let (rows, held) = converge_held(&sys, &options, &prepare, &records, &|| true);
        let rows = rows.unwrap();
        assert!(stop.load(Ordering::SeqCst), "the Stop was pressed mid-run");
        let second = rows
            .iter()
            .find(|row| row.id == "init.github.second@alice")
            .expect("the second clone's row");
        assert_eq!(second.outcome, Outcome::NotApplicable, "{second:?}");
        assert!(
            second
                .why
                .as_deref()
                .is_some_and(|why| why.contains(STOPPED)),
            "{second:?}"
        );
        assert!(
            held.iter().any(|row| row.id == HELD
                && row
                    .why
                    .as_deref()
                    .is_some_and(|why| why.contains("GitHub repositories"))),
            "{held:?}"
        );
    }
    // A hold on record when the run began, which `--clone` names, holds nothing it runs.
    for start in [
        Holds {
            off: left,
            ..Holds::NONE
        },
        Holds {
            stopped: Some(STOPPED.to_owned()),
            stopped_revision: Some(1),
            ..Holds::NONE
        },
    ] {
        let records = StopLater(Arc::new(AtomicBool::new(false)), start);
        let start = records.holds();
        let prepare = move |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
            let picked = picked(reading.picks, reading.only, &|| true, &start, true);
            Ok((
                vec![(
                    vec![Listed::of(
                        "github",
                        "init.github.second@alice",
                        vec![change(), ok()],
                    )],
                    place(Class::Install),
                )],
                picked,
            ))
        };
        let named = Options {
            picks: Picks {
                named: Choice {
                    clone: true,
                    ..Choice::NONE
                },
                left: Choice::NONE,
            },
            ..Options::default()
        };
        let sys = alice_with_checkout(501, None);
        let (rows, _) = converge_held(&sys, &named, &prepare, &records, &|| true);
        let rows = rows.unwrap();
        assert!(
            rows.iter()
                .any(|row| row.id == "init.github.second@alice" && row.outcome == Outcome::Changed),
            "{rows:?}"
        );
    }
}

#[test]
fn held_late_forwards_new_choices_to_each_effect_of_one_admitted_batch() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Batch {
        effects: AtomicUsize,
        stopped: AtomicBool,
        stop_late: bool,
    }
    impl Item for Batch {
        fn id(&self) -> String {
            "init.github.batch@alice".into()
        }
        fn family(&self) -> &'static str {
            "github"
        }
        fn title(&self) -> String {
            self.id()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            if self.effects.load(Ordering::SeqCst) == 2 {
                ok()
            } else {
                change()
            }
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            panic!("the wrapper must forward current authorization into the batch")
        }
        fn apply_guarded(
            &self,
            _: &dyn System,
            _: &Ctx,
            current: &(dyn Fn() -> Result<(), String> + Sync),
        ) -> Result<(), String> {
            for at in 0..2 {
                // What its first effect did, which only its own answer can say.
                current().map_err(|why| {
                    format!("{why}; ~/batch: ran its scripts/install-git-hooks.sh")
                })?;
                self.effects.fetch_add(1, Ordering::SeqCst);
                if at == 0 && self.stop_late {
                    self.stopped.store(true, Ordering::SeqCst);
                }
            }
            Ok(())
        }
    }
    for stop_late in [false, true] {
        let batch = Batch {
            effects: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
            stop_late,
        };
        let policy = |_: &str| {
            batch
                .stopped
                .load(Ordering::SeqCst)
                .then(|| "late Stop".to_owned())
        };
        let wrapper = HeldLate {
            inner: &batch,
            held: &policy,
            decided: std::sync::OnceLock::new(),
            late: std::sync::OnceLock::new(),
            said: std::sync::OnceLock::new(),
        };
        let sys = crate::port::Fake::root();
        let ctx = crate::items::defaults::tests::ctx(&[]);
        assert!(matches!(wrapper.probe(&sys, &ctx), Probe::Change { .. }));
        assert!(
            wrapper.apply(&sys, &ctx).is_ok(),
            "a held batch is left rather than failed"
        );
        assert_eq!(
            batch.effects.load(Ordering::SeqCst),
            if stop_late { 1 } else { 2 }
        );
        // NEGATIVE CONTROL: a wrapper that answers the hold's words alone loses the line.
        assert_eq!(
            wrapper.stood_aside().map(|aside| aside.why().to_owned()),
            stop_late
                .then(|| "late Stop; ~/batch: ran its scripts/install-git-hooks.sh".to_owned())
        );
        if stop_late {
            assert!(matches!(
                wrapper.probe(&sys, &ctx),
                Probe::NotApplicable { .. }
            ));
        }
    }
}

/// With a terminal at the console the person who typed `aterm init` may walk away (screen
/// locked or idle) during a long clone: the sign-outs read no presence, as they start or
/// before their clicks, and run after it all the same.
#[test]
fn a_terminal_runs_admitted_sign_outs_without_a_presence_gate() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let here = Arc::new(AtomicBool::new(true));
    let mut sys = alice_at(501);
    sys.terminal = true;
    sys.agent = None;
    assert_eq!(crate::console::presence(&sys), Presence::AtConsole);
    let gone = Arc::clone(&here);
    let prepare = move |_: &dyn System, ctx: &Ctx, _: Mode, _: &Reading<'_>| {
        let gone = Arc::clone(&gone);
        let items: Vec<Box<dyn Item>> = crate::init_accounts::items(ctx)
            .unwrap()
            .into_iter()
            .map(|item| Listed::boxed(item.id(), vec![change(), ok()]))
            .collect();
        // A clone that takes half an hour; the person walks away meanwhile.
        let clone = then("init.test.clone", "github", move || {
            gone.store(false, Ordering::SeqCst);
        });
        Ok((
            vec![
                (items, place(Class::Preference)),
                (vec![clone], anywhere(Class::Install)),
            ],
            Picked::ALL,
        ))
    };
    let present = || here.load(Ordering::SeqCst);
    let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &NoRecords, &present);
    let rows = rows.unwrap();
    let sign_outs: Vec<&ItemReport> = rows
        .iter()
        .filter(|row| crate::init_accounts::signs_out(&row.id))
        .collect();
    assert!(!sign_outs.is_empty());
    assert!(
        sign_outs.iter().all(|row| row.outcome == Outcome::Changed),
        "admitted work continues after a long clone: {sign_outs:#?}"
    );
}

/// [`naming`] promises the narrowest run that sets one group up and nothing else, and the
/// held line offers it to turn one group back on: on an ALab Mac the Dock's way back sets
/// the Dock alone, not the Widgets the person also left as is (`--profile` set both).
#[test]
fn the_way_back_for_one_group_left_as_is_runs_only_that_group() {
    let mut off = Selection::none();
    off.set(Setting::Dock, true);
    off.set(Setting::Widgets, true);
    let holds = Holds { off, ..Holds::NONE };
    let words: Vec<OsString> = naming(Setting::Dock)
        .split_whitespace()
        .skip(2)
        .map(OsString::from)
        .collect();
    let mut options = options(&words).unwrap();
    options.station = options.only.clone();
    let prepare = |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
        let picked = picked(reading.picks, reading.only, &|| true, &holds, true);
        let mut items: Vec<Box<dyn Item>> = Vec::new();
        if picked.runs.contains(Setting::Dock) {
            items.push(Listed::of("dock", "init.dock@alice", vec![change(), ok()]));
        }
        if picked.runs.contains(Setting::Widgets) {
            items.push(Listed::of(
                "widgets",
                "consumer.widgets.test@alice",
                vec![change(), ok()],
            ));
        }
        Ok((vec![(items, place(Class::Preference))], picked))
    };
    let sys = alice_with_checkout(501, None);
    let (rows, _) = converge_held(&sys, &options, &prepare, &NoRecords, &|| true);
    let changed: Vec<String> = rows
        .unwrap()
        .iter()
        .filter(|row| row.outcome == Outcome::Changed)
        .map(|row| row.id.clone())
        .collect();
    assert_eq!(
        changed,
        ["init.dock@alice"],
        "`{}`, the Dock's way back, set more than the Dock",
        naming(Setting::Dock)
    );
}

/// Each Mac setting's way back ([`naming`]) names the items of its own System State row
/// and no other, as this build builds them: the preferences, the trackpad's two rows and
/// each sign-out.
#[test]
fn the_way_back_for_each_group_names_only_that_groups_items() {
    let sys = alice_with_checkout(501, None);
    let (ctx, keyboards) = local_context(&sys).unwrap();
    let mut items: Vec<(Setting, Box<dyn Item>)> =
        crate::init_preferences::selected_tagged_items(&ctx, Selection::all(), &keyboards.cohorts)
            .unwrap();
    items.extend(
        crate::init_trackpad::items(&ctx)
            .unwrap()
            .into_iter()
            .map(|item| (Setting::NaturalScrolling, item)),
    );
    items.extend(
        crate::init_trackpad::behavior_items(&ctx)
            .unwrap()
            .into_iter()
            .map(|item| (Setting::TrackpadScrolling, item)),
    );
    items.extend(crate::init_accounts::selected_tagged_items(&ctx, Selection::all()).unwrap());
    for setting in Setting::ALL.into_iter().filter(|s| s.is_workstation()) {
        let word = only_word(setting);
        assert_eq!(naming(setting), format!("aterm init --only {word}"));
        let named: Vec<(Setting, String)> = items
            .iter()
            .filter(|(_, item)| aterm_converge::selects(word, &item.id(), item.family(), None))
            .map(|(each, item)| (*each, item.id()))
            .collect();
        let own = items.iter().filter(|(each, _)| *each == setting).count();
        assert!(
            named.len() == own && named.iter().all(|(each, _)| *each == setting),
            "--only {word} names {named:?} for {setting:?}"
        );
    }
}

/// The line of the keys changed since aterm set them names each by the short name System
/// State's row shows ([`Setting::name`]), as the held line does.
#[test]
fn the_changed_since_line_names_rows_as_the_page_does() {
    let rows = vec![
        held_row("init.test.dock", "Dock", format!("a; {KEPT_CHANGED}")),
        held_row("init.test.caps", "Caps", format!("a; {KEPT_CHANGED}")),
    ];
    let tags = vec![
        ("init.test.dock".to_owned(), Setting::Dock),
        ("init.test.caps".to_owned(), Setting::CapsLock),
    ];
    let lines = kept_lines(&rows, &tags);
    let line = lines.first().expect("the changed-since line");
    assert!(
        line.contains("Caps Lock key, Dock.") && !line.contains("only aterm"),
        "{line}"
    );
}

/// The line of what a person does that the owed clone's record keeps stays on one line and
/// within what its reader reads whole, however long the row's words.
#[test]
fn the_owed_clones_person_line_stays_within_what_its_reader_reads() {
    let long = format!("{}\nsecond line", "é".repeat(4000));
    let line = person_line(&long);
    assert!(line.len() <= PERSON_LINE_MAX, "{}", line.len());
    assert!(!line.contains('\n') && line.ends_with('\u{2026}'));
    assert_eq!(person_line("short"), "short");
}

/// A person who left GitHub repositories as is and then names them (`aterm init --clone`,
/// principle 4: naming a group is their choice too) is told the clone their run owes "runs by
/// itself once this Mac is let into the vault". The record their run leaves is stamped with
/// the hold they went past, so the vault's sync clones once the token lands, though System
/// State still holds the group.
#[test]
fn a_clone_the_person_named_is_not_forgotten_by_the_vaults_sync() {
    use crate::sync::tests::owed_clone::{Hooked, alice};
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut left = Selection::none();
    left.set(Setting::Repositories, true);
    let holds = Holds {
        off: left,
        ..Holds::NONE
    };
    let named = Picks {
        named: Choice {
            clone: true,
            ..Choice::NONE
        },
        left: Choice::NONE,
    };
    let picked = picked(named, &[], &|| true, &holds, true);
    assert!(picked.runs.contains(Setting::Repositories), "{picked:?}");
    // The run's GitHub step waits for this Mac to be let into the vault: owed.
    let waiting = ItemReport {
        stood_aside: Some(aterm_converge::StoodAside::Waiting(
            "waiting to be let in".into(),
        )),
        ..held_row(
            "init.github.native@alice",
            "GitHub repositories",
            "waits for init.secrets@alice: waiting to be let in".into(),
        )
    };
    let waiting = ItemReport {
        family: "github".into(),
        outcome: Outcome::Blocked,
        waits_on: Some("init.secrets@alice".into()),
        ..waiting
    };
    assert_eq!(owe(waiting.clone()).why.as_deref(), Some(OWED));
    let scratch = aterm_tempfile::tempdir().unwrap();
    let state = scratch.path().to_path_buf();
    note_owed(&[waiting], Action::Apply, Some(&state), &picked.start).unwrap();
    // The vault's token lands; System State still holds the group, as it did.
    let (sys, _) = alice();
    let token = "/Users//alice/.secrets/gh_access_token";
    sys.file(token, b"fixture_token_only").stat_of(
        token,
        Stat {
            uid: 501,
            gid: 20,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 18,
        },
    );
    let cloned = AtomicBool::new(false);
    let owed = |readback: Option<&crate::init_github::RepositoryReadback>| {
        owed_clone(&sys, &state, &holds, readback)
    };
    let clone = || {
        cloned.store(true, Ordering::SeqCst);
        Ok(CloneOwedOutcome {
            said: "cloned".to_owned(),
            readback: None,
        })
    };
    let ports = Hooked {
        inner: crate::sync::tests::Inert::new(&state),
        held: &|| None,
        owed: &owed,
        clone: &clone,
        control: None,
        forget: &|| forget_owed_clone(&state),
    };
    assert!(
        matches!(owed(None), Ok(OwedClone::Due(_))),
        "{:?}",
        owed(None)
    );
    let _ = crate::sync::converge(
        &ports,
        crate::sync::Trigger::Subscriber,
        Mode::Apply,
        aterm_secrets::converge::RepositoryWork::Refresh,
        &mut |_| {},
    );
    assert!(
        cloned.load(Ordering::SeqCst),
        "the clone the person's own `aterm init --clone` owed was forgotten: {:?}",
        owed(None)
    );
}

/// The relayed background row waits on the window's own setup, not on the vault: it keeps
/// the coordinator's words, is owed to no vault join, and its tally says so. Before, it read
/// "runs by itself once this Mac is let into the vault" beside a vault row that read synced.
#[test]
fn a_relayed_background_wait_keeps_its_words_and_is_not_owed() {
    let words = "background setup is waiting: capslock,dock".to_owned();
    let row = ItemReport {
        outcome: Outcome::Blocked,
        stood_aside: Some(aterm_converge::StoodAside::Waiting(words.clone())),
        ..held_row(
            AWAY,
            "Workstation setup observed in the background",
            words.clone(),
        )
    };
    let after = owe_unless_held(row.clone(), None);
    assert_eq!(after.why.as_deref(), Some(words.as_str()), "{after:?}");
    assert_eq!(after.outcome, Outcome::Blocked);
    assert!(!owed(&row));
    assert!(waits_on_background_setup(&after) && !held_late(&after));
    let tally = tally(std::slice::from_ref(&after), Action::Check);
    assert!(!tally.contains("let in"), "{tally}");
    assert_eq!(status(&[after], Action::Check), 75);
}

/// A sign-out that stood aside for another macOS dialog clicked nothing: its own words stay
/// (nothing clicked, and aterm looks again by itself), an apply is not complete with it, and
/// it writes no clone owed to the vault's join.
#[test]
fn a_sign_out_waiting_for_the_dialog_slot_is_neither_done_nor_owed() {
    let row = ItemReport {
        outcome: Outcome::Blocked,
        stood_aside: Some(aterm_converge::StoodAside::Waiting(
            crate::init_accounts::DIALOG_UP.into(),
        )),
        ..held_row(
            "init.messages.signed-out@alice",
            "Messages signed out",
            crate::init_accounts::DIALOG_UP.into(),
        )
    };
    let after = owe_unless_held(row.clone(), None);
    assert_eq!(
        after.why.as_deref(),
        Some(crate::init_accounts::DIALOG_UP),
        "{after:?}"
    );
    assert!(!counts(&after, Action::Apply), "{after:?}");
    assert_eq!(status(std::slice::from_ref(&after), Action::Apply), 75);
    let scratch = aterm_tempfile::tempdir().unwrap();
    note_owed(&[row], Action::Apply, Some(scratch.path()), &Holds::NONE).unwrap();
    assert!(
        !converge_state(scratch.path()).join(CLONE_OWED).exists(),
        "a sign-out wrote a clone owed to the vault's join"
    );
}

/// A GitHub batch a later hold of the person's stopped (their Stop, or GitHub repositories
/// left as they are, pressed while `aterm init` ran it) is their choice, as a hold read when
/// the run began is: its row says their words and what the batch did, owes nothing to the
/// vault, and the apply exits 0. Its step stays owed under the start's holds, so `sync.clone`
/// keeps a Stop's clone owed until Set up now, and forgets the clone of a group the person
/// left as is since (the control).
#[test]
fn a_later_hold_over_a_github_batch_is_the_persons_choice_and_keeps_its_step_owed() {
    use std::sync::atomic::{AtomicBool, Ordering};
    const STOPPED: &str = "you stopped setup in System State";
    struct LaterHold(Arc<AtomicBool>, Holds);
    impl Records for LaterHold {
        fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
            Ok(Box::new(NoRecords))
        }
        fn holds(&self) -> Holds {
            if self.0.load(Ordering::SeqCst) {
                self.1.clone()
            } else {
                Holds::NONE
            }
        }
    }
    /// A batch of clones the hold lands on between its two effects.
    struct Batch(Arc<AtomicBool>);
    impl Item for Batch {
        fn id(&self) -> String {
            "init.github.batch@alice".into()
        }
        fn family(&self) -> &'static str {
            "github"
        }
        fn title(&self) -> String {
            self.id()
        }
        fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
            change()
        }
        fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
            panic!("the wrapper forwards the person's current choices into the batch")
        }
        fn apply_guarded(
            &self,
            _: &dyn System,
            _: &Ctx,
            current: &(dyn Fn() -> Result<(), String> + Sync),
        ) -> Result<(), String> {
            current()?;
            self.0.store(true, Ordering::SeqCst);
            current().map_err(|why| format!("{why}; ~/first: cloned"))
        }
    }
    let mut left = Selection::none();
    left.set(Setting::Repositories, true);
    for (later, kept) in [
        (
            Holds {
                stopped: Some(STOPPED.to_owned()),
                stopped_revision: Some(2),
                ..Holds::NONE
            },
            true,
        ),
        (
            Holds {
                off: left,
                ..Holds::NONE
            },
            false,
        ),
    ] {
        let pressed = Arc::new(AtomicBool::new(false));
        let records = LaterHold(Arc::clone(&pressed), later.clone());
        let batch = Arc::clone(&pressed);
        let prepare = move |_: &dyn System, _: &Ctx, _: Mode, reading: &Reading<'_>| {
            let picked = picked(reading.picks, reading.only, &|| true, &Holds::NONE, true);
            let item: Box<dyn Item> = Box::new(Batch(Arc::clone(&batch)));
            Ok((vec![(vec![item], place(Class::Install))], picked))
        };
        let sys = alice_with_checkout(501, None);
        let (rows, _) = converge_held(&sys, &Options::default(), &prepare, &records, &|| true);
        let rows = rows.unwrap();
        assert!(pressed.load(Ordering::SeqCst), "the hold came mid-batch");
        let row = rows
            .iter()
            .find(|row| row.id == "init.github.batch@alice")
            .expect("the batch's row");
        assert_eq!(row.outcome, Outcome::Blocked, "{kept}: {row:?}");
        assert!(held_late(row) && !owed(row), "{kept}: {row:?}");
        assert_eq!(status(&rows, Action::Apply), 0, "{kept}: {rows:?}");
        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().to_path_buf();
        note_owed(&rows, Action::Apply, Some(&state), &Holds::NONE).unwrap();
        assert!(
            converge_state(&state).join(CLONE_OWED).exists(),
            "{kept}: the step the hold stopped is owed"
        );
        let (sys, _) = crate::sync::tests::owed_clone::alice();
        let owed = owed_clone(&sys, &state, &later, None);
        assert_eq!(
            matches!(owed, Ok(OwedClone::Waiting(_))),
            kept,
            "{kept}: {owed:?}"
        );
        assert_eq!(
            matches!(owed, Ok(OwedClone::Held(_))),
            !kept,
            "{kept}: {owed:?}"
        );
    }
}

/// A sign-out that stands aside as SignOut does while another macOS dialog is up: it probes a
/// change, clicks nothing, and leaves a Waiting stand-aside.
struct StoodAsideSignOut(String);

impl Item for StoodAsideSignOut {
    fn id(&self) -> String {
        self.0.clone()
    }
    fn family(&self) -> &'static str {
        "init.messages"
    }
    fn title(&self) -> String {
        self.0.clone()
    }
    fn probe(&self, _: &dyn System, _: &Ctx) -> Probe {
        change()
    }
    fn apply(&self, _: &dyn System, _: &Ctx) -> Result<(), String> {
        Ok(())
    }
    fn revert(&self) -> Option<String> {
        None
    }
    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        Some(aterm_converge::StoodAside::Waiting(
            crate::init_accounts::DIALOG_UP.into(),
        ))
    }
}

/// What a whole pass recorded of the window's run: each checkpoint's settings, and how many
/// times it recorded the whole profile verified.
#[derive(Default)]
struct Checkpoints {
    log: Arc<Mutex<Vec<Selection>>>,
    verified: Arc<std::sync::atomic::AtomicU32>,
}

struct CheckpointsHeld {
    log: Arc<Mutex<Vec<Selection>>>,
    verified: Arc<std::sync::atomic::AtomicU32>,
}

impl Records for Checkpoints {
    fn hold(&self, _pass: Pass, _wait: Option<&dyn Fn(&str)>) -> Result<Box<dyn Held>, Unheld> {
        Ok(Box::new(CheckpointsHeld {
            log: Arc::clone(&self.log),
            verified: Arc::clone(&self.verified),
        }))
    }
}

impl Held for CheckpointsHeld {
    fn checkpoint(&self, settings: Selection) -> Result<(), String> {
        self.log.lock().unwrap().push(settings);
        Ok(())
    }
    fn verified(self: Box<Self>) -> Result<(), String> {
        self.verified
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// Completion evidence is a read-back: a sign-out that only stood aside (another macOS
/// dialog was up, so it clicked nothing) records neither its Accounts settings nor the whole
/// profile verified for the window's run, which then signs out itself. NEGATIVE CONTROL:
/// sign-outs that read signed out record both.
#[test]
fn a_stood_aside_sign_out_supplies_no_completion_evidence() {
    for stood_aside in [true, false] {
        let groups = |ctx: &Ctx| {
            let mut items: Vec<Box<dyn Item>> = if stood_aside {
                crate::init_accounts::items(ctx)
                    .unwrap()
                    .into_iter()
                    .map(|item| Box::new(StoodAsideSignOut(item.id())) as Box<dyn Item>)
                    .collect()
            } else {
                verified_accounts(ctx)
            };
            items.push(Listed::boxed("init.test.dock", vec![change(), ok()]));
            vec![(items, place(Class::Preference))]
        };
        let sys = alice_with_checkout(501, None);
        let records = Checkpoints::default();
        converge_with(&sys, &Options::default(), &groups, &records).unwrap();
        let log = records.log.lock().unwrap().clone();
        let signs_out = |selection: &Selection| {
            selection.contains(Setting::Messages) || selection.contains(Setting::FaceTime)
        };
        assert_eq!(
            log.iter().any(signs_out),
            !stood_aside,
            "{stood_aside}: {log:?}"
        );
        assert_eq!(
            records.verified.load(std::sync::atomic::Ordering::SeqCst),
            u32::from(!stood_aside),
            "{stood_aside}"
        );
    }
}
