// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge pass over inert ports: nothing here reads or changes the machine.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// A base item of `class` that plans one change, reads right once applied, and counts each
/// apply under its id; one whose id `fails` names fails its apply.
struct Fake {
    id: &'static str,
    class: Class,
    done: AtomicBool,
    applied: Arc<Mutex<Vec<String>>>,
    fails: Arc<Mutex<Vec<&'static str>>>,
}

impl Item for Fake {
    fn id(&self) -> String {
        self.id.to_owned()
    }
    fn family(&self) -> &'static str {
        "test"
    }
    fn layer(&self) -> Layer {
        Layer::Base
    }
    fn class(&self) -> Class {
        self.class
    }
    fn title(&self) -> String {
        self.id.to_owned()
    }
    fn probe(&self) -> Probe {
        if self.done.load(Ordering::SeqCst) {
            Probe::Ok {
                now: "laid".to_owned(),
            }
        } else {
            Probe::Change {
                from: "absent".to_owned(),
                to: "laid".to_owned(),
            }
        }
    }
    fn apply(&self) -> Result<(), String> {
        if self.fails.lock().unwrap().contains(&self.id) {
            return Err(format!("{} could not be laid", self.id));
        }
        self.applied.lock().unwrap().push(self.id.to_owned());
        self.done.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn explain(&self, _probe: &Probe) -> Explain {
        Explain {
            why: self.id.to_owned(),
            remedy: Remedy::None,
        }
    }
}

/// The store lock a base layer holds, counted as it is let go.
struct Released(Arc<AtomicU32>);

impl Drop for Released {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// The base layer every test composes: one item of each class (two of Relay), the store's own
/// builds (the Install class, whose apply only an install makes) among them.
const BASE_ITEMS: [(&str, Class); 7] = [
    ("base.shell.links", Class::Relay),
    ("base.shell.rc", Class::Relay),
    ("base.pkg.store", Class::Install),
    ("base.person", Class::Person),
    ("base.preference", Class::Preference),
    ("base.asked", Class::Asked),
    ("base.elevate", Class::Elevate),
];

/// The Relay base items, the ones a pass applies, in run order.
const RELAY: [&str; 2] = ["base.shell.links", "base.shell.rc"];

/// Ports that read from fields and record every call: each read kind reads due until its apply,
/// then right. `aterm init --converge`'s tests run the runner over them.
pub(crate) struct Inert {
    state: PathBuf,
    own: bool,
    console: bool,
    installed: bool,
    /// Whether an aterm window runs ([`Ports::window_runs`]).
    window: bool,
    /// Whether an install pass holds the store: each base item stands aside.
    contended: AtomicBool,
    /// The base items whose apply fails.
    fails: Arc<Mutex<Vec<&'static str>>>,
    /// How many times the store lock had been let go when a sync kind was first read.
    released_at_first_read: Mutex<Option<u32>>,
    backgrounds: AtomicU32,
    build: Mutex<String>,
    routes: Mutex<Vec<String>>,
    now: AtomicU64,
    reads: AtomicU32,
    bases: Mutex<Vec<BaseLock>>,
    base_applied: Arc<Mutex<Vec<String>>>,
    released: Arc<AtomicU32>,
    applies: Mutex<Vec<&'static str>>,
    vault: Mutex<VaultRead>,
    fleet: Mutex<FleetRead>,
    pub(crate) fleet_done: Mutex<Result<FleetDone, String>>,
    shared: Mutex<SharedRead>,
    accounts: Mutex<AccountsRead>,
    fabric: Mutex<FabricRead>,
    peers: Mutex<PeersRead>,
    /// What each `sync.peers` read was let do: look for the open vault.
    peer_looks: Mutex<Vec<bool>>,
    peers_done: Mutex<PeersDone>,
}

impl Inert {
    pub(crate) fn new(state: &Path) -> Self {
        Self {
            state: state.to_path_buf(),
            own: true,
            console: true,
            installed: true,
            window: false,
            contended: AtomicBool::new(false),
            fails: Arc::new(Mutex::new(Vec::new())),
            released_at_first_read: Mutex::new(None),
            backgrounds: AtomicU32::new(0),
            build: Mutex::new("0.145.0+gabc".to_owned()),
            routes: Mutex::new(vec!["192.0.2.7".to_owned(), "-".to_owned()]),
            now: AtomicU64::new(1_000_000_000),
            reads: AtomicU32::new(0),
            bases: Mutex::new(Vec::new()),
            base_applied: Arc::new(Mutex::new(Vec::new())),
            released: Arc::new(AtomicU32::new(0)),
            applies: Mutex::new(Vec::new()),
            vault: Mutex::new(VaultRead::Runs("runs (pid 7)".to_owned())),
            fleet: Mutex::new(FleetRead::Due("this Mac is in no fleet".to_owned())),
            fleet_done: Mutex::new(Ok(FleetDone::Synced("joined".to_owned()))),
            shared: Mutex::new(SharedRead::Visible("control record here".to_owned())),
            accounts: Mutex::new(AccountsRead::Due("the roster's sources changed".to_owned())),
            fabric: Mutex::new(FabricRead::Replaced(
                "the broker runs a replaced build".to_owned(),
            )),
            peers: Mutex::new(PeersRead::Joined("in vault alab".to_owned())),
            peer_looks: Mutex::new(Vec::new()),
            peers_done: Mutex::new(PeersDone::Joined("joined".to_owned())),
        }
    }

    fn read<T: Clone>(&self, field: &Mutex<T>) -> Result<T, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.released_at_first_read
            .lock()
            .unwrap()
            .get_or_insert_with(|| self.released.load(Ordering::SeqCst));
        Ok(field.lock().unwrap().clone())
    }

    fn applied(&self, kind: &'static str) {
        self.applies.lock().unwrap().push(kind);
    }

    pub(crate) fn applies(&self) -> Vec<&'static str> {
        self.applies.lock().unwrap().clone()
    }

    /// How many times a runner lowered its scheduling.
    pub(crate) fn backgrounds(&self) -> u32 {
        self.backgrounds.load(Ordering::SeqCst)
    }

    pub(crate) fn base_applied(&self) -> Vec<String> {
        self.base_applied.lock().unwrap().clone()
    }

    fn pass(&self, trigger: Trigger, mode: Mode) -> Result<Pass, Stop> {
        converge(self, trigger, mode, &mut |_| {})
    }
}

impl Ports for Inert {
    fn background(&self) {
        self.backgrounds.fetch_add(1, Ordering::SeqCst);
    }
    fn own_account(&self) -> bool {
        self.own
    }
    fn at_console(&self) -> Result<bool, String> {
        Ok(self.console)
    }
    fn state_dir(&self) -> Option<PathBuf> {
        Some(self.state.clone())
    }
    fn build(&self) -> String {
        self.build.lock().unwrap().clone()
    }
    fn routes(&self) -> Vec<String> {
        self.routes.lock().unwrap().clone()
    }
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
    fn installed_release(&self) -> bool {
        self.installed
    }
    fn window_runs(&self) -> bool {
        self.window
    }
    fn base(&self, lock: BaseLock) -> Base {
        self.bases.lock().unwrap().push(lock);
        let items: Vec<Box<dyn Item>> = BASE_ITEMS
            .into_iter()
            .map(|(id, class)| {
                Box::new(Fake {
                    id,
                    class,
                    done: AtomicBool::new(false),
                    applied: Arc::clone(&self.base_applied),
                    fails: Arc::clone(&self.fails),
                }) as Box<dyn Item>
            })
            .collect();
        if self.contended.load(Ordering::SeqCst) {
            // As `aterm doctor`'s base stands beside an install pass it never waits for.
            let aside = StoodAside::Lock("an install pass holds the store".to_owned());
            return Base {
                items: aterm_converge::standing_aside(items, &aside),
                ..Base::none()
            };
        }
        Base {
            items,
            held: Some(Box::new(Released(Arc::clone(&self.released)))),
            ..Base::none()
        }
    }
    fn vault(&self) -> Result<VaultRead, String> {
        self.read(&self.vault)
    }
    fn fleet(&self) -> Result<FleetRead, String> {
        self.read(&self.fleet)
    }
    fn sync_fleet(&self) -> Result<FleetDone, String> {
        self.applied("fleet");
        let done = self.fleet_done.lock().unwrap().clone();
        if matches!(done, Ok(FleetDone::Synced(_))) {
            *self.fleet.lock().unwrap() = FleetRead::Current("in the fleet".to_owned());
        }
        done
    }
    fn shared_data(&self) -> Result<SharedRead, String> {
        self.read(&self.shared)
    }
    fn accounts(&self) -> Result<AccountsRead, String> {
        self.read(&self.accounts)
    }
    fn resync_accounts(&self) -> Result<String, String> {
        self.applied("accounts");
        *self.accounts.lock().unwrap() = AccountsRead::Current("2 accounts synced".to_owned());
        Ok("synced".to_owned())
    }
    fn fabric(&self) -> Result<FabricRead, String> {
        self.read(&self.fabric)
    }
    fn renew_broker(&self) -> Result<String, String> {
        self.applied("fabric");
        *self.fabric.lock().unwrap() = FabricRead::Current("the broker runs this build".into());
        Ok("renewed".to_owned())
    }
    fn peers(&self, look: bool) -> Result<PeersRead, String> {
        self.peer_looks.lock().unwrap().push(look);
        self.read(&self.peers)
    }
    fn join_peers(&self) -> Result<PeersDone, String> {
        self.applied("peers");
        let done = self.peers_done.lock().unwrap().clone();
        if matches!(done, PeersDone::Joined(_)) {
            *self.peers.lock().unwrap() = PeersRead::Joined("in vault alab".to_owned());
        }
        Ok(done)
    }
}

fn row<'a>(rows: &'a [ItemReport], id: &str) -> &'a ItemReport {
    rows.iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("no {id} row in {rows:?}"))
}

fn ran(result: Result<Pass, Stop>) -> Pass {
    result.unwrap_or_else(|stop| panic!("{stop:?}"))
}

fn record(build: &str, routes: &[&str], attempted_ms: u64) -> Record {
    Record {
        build: build.to_owned(),
        spec: SPEC,
        builds: vec![build.to_owned()],
        attempted_ms,
        converged_ms: Some(attempted_ms),
        routes: routes.iter().map(|route| (*route).to_owned()).collect(),
        rows: Vec::new(),
        base: None,
    }
}

/// One row of [`due`]'s table: the record, the build, the routes, now, and what is due.
type Case<'a> = (Option<Record>, &'a str, &'a [String], u64, Due);

#[test]
fn a_pass_is_due_on_a_new_build_a_moved_network_or_half_an_hour() {
    let routes = ["192.0.2.7".to_owned(), "-".to_owned()];
    let at = 5_000_000;
    let half_hour = 30 * 60 * 1000;
    let same = record("b1", &["192.0.2.7", "-"], at);
    let table: [Case<'_>; 8] = [
        (None, "b1", &routes, at, Due::NewBuild),
        (Some(same.clone()), "b1", &routes, at + 60_000, Due::NotDue),
        (
            Some(same.clone()),
            "b2",
            &routes,
            at + 60_000,
            Due::NewBuild,
        ),
        (
            Some(Record {
                spec: SPEC + 1,
                ..same.clone()
            }),
            "b1",
            &routes,
            at + 60_000,
            Due::NewBuild,
        ),
        (
            Some(same.clone()),
            "b1",
            &["198.51.100.4".to_owned(), "-".to_owned()],
            at + 60_000,
            Due::NetworkMoved,
        ),
        (
            Some(same.clone()),
            "b1",
            &routes,
            at + half_hour - 1,
            Due::NotDue,
        ),
        (
            Some(same.clone()),
            "b1",
            &routes,
            at + half_hour,
            Due::Recheck,
        ),
        // A clock behind the attempt leaves the record's age unknown.
        (Some(same), "b1", &routes, at - 1, Due::Recheck),
    ];
    for (record, build, routes, now, want) in table {
        assert_eq!(
            due(record.as_ref(), build, routes, now),
            want,
            "{record:?} {build} {routes:?} {now}"
        );
    }
    // A new build outranks a moved network, and a moved network a recheck.
    let old = record("b1", &["192.0.2.7"], 0);
    assert_eq!(
        due(Some(&old), "b2", &["10.0.0.1".to_owned()], u64::MAX),
        Due::NewBuild
    );
    assert_eq!(
        due(Some(&old), "b1", &["10.0.0.1".to_owned()], u64::MAX),
        Due::NetworkMoved
    );
}

/// A pass applies the Relay and Install classes alone: an item of any other class (a person's,
/// a preference, an asked repair, root) is probed and planned, never applied, and nothing asks.
/// NEGATIVE CONTROL: the pass's scope set to `Scope::EVERY` applies each of them.
#[test]
fn a_person_preference_asked_or_root_item_is_probed_and_never_applied() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(pass.due, Due::NewBuild);
    assert_eq!(
        ports.base_applied(),
        RELAY,
        "the Relay items alone are applied: {:?}",
        pass.rows
    );
    for id in [
        "base.person",
        "base.preference",
        "base.asked",
        "base.elevate",
    ] {
        let planned = row(&pass.rows, id);
        assert_eq!(planned.outcome, Outcome::Planned, "{planned:?}");
    }
    assert_eq!(
        row(&pass.rows, "base.shell.links").outcome,
        Outcome::Changed
    );
}

/// A pass after one that converged applies nothing: every kind reads right, and the base layer
/// is re-laid once per build.
#[test]
fn a_second_pass_applies_nothing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let first = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(ports.applies(), ["fleet", "accounts", "fabric"]);
    for id in [FLEET, ACCOUNTS, FABRIC] {
        assert_eq!(row(&first.rows, id).outcome, Outcome::Changed, "{id}");
    }
    assert_eq!(row(&first.rows, FLEET).detail.as_deref(), Some("joined"));
    let base_applied = ports.base_applied().len();
    ports.now.fetch_add(60_000, Ordering::SeqCst);
    let second = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(second.due, Due::NotDue);
    assert_eq!(ports.applies().len(), 3, "nothing more applied");
    assert_eq!(ports.base_applied().len(), base_applied);
    assert_eq!(ports.bases.lock().unwrap().len(), 1, "no base layer read");
    assert_eq!(row(&second.rows, BASE).outcome, Outcome::NotApplicable);
    assert!(
        second
            .rows
            .iter()
            .all(|row| matches!(row.outcome, Outcome::Ok | Outcome::NotApplicable)),
        "{:?}",
        second.rows
    );
}

/// A person's off switch leaves its kind as it is: the row reads `NotApplicable`, naming it, and
/// nothing of that kind is applied.
#[test]
fn each_off_switch_reads_not_applicable_and_applies_nothing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.vault.lock().unwrap() = VaultRead::Off;
    *ports.fleet.lock().unwrap() =
        FleetRead::Idle("fleet sync is off after aterm fleet leave".to_owned());
    *ports.shared.lock().unwrap() = SharedRead::Off;
    *ports.accounts.lock().unwrap() = AccountsRead::Off;
    *ports.fabric.lock().unwrap() = FabricRead::NotApplicable("the fabric is off".to_owned());
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    for (id, says) in [
        (VAULT, "agent on"),
        (FLEET, "fleet leave"),
        (SHARED_DATA, "[commons] enabled = false"),
        (ACCOUNTS, "[harness] accounts = \"off\""),
        (FABRIC, "the fabric is off"),
    ] {
        let left = row(&pass.rows, id);
        assert_eq!(left.outcome, Outcome::NotApplicable, "{left:?}");
        assert!(
            left.why.as_deref().is_some_and(|why| why.contains(says)),
            "{left:?}"
        );
    }
    assert!(ports.applies().is_empty(), "{:?}", ports.applies());
}

/// A pass meets another pass's lock and stops: exit 75's case, with nothing read, applied or
/// recorded.
#[test]
fn a_held_lock_skips_the_pass() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let held = try_lock(scratch.path()).unwrap().expect("free");
    assert!(
        try_lock(scratch.path()).unwrap().is_none(),
        "a second take waits on none"
    );
    let stopped = ports.pass(Trigger::Window, Mode::Apply);
    assert!(matches!(stopped, Err(Stop::Held)), "{stopped:?}");
    assert_eq!(ports.reads.load(Ordering::SeqCst), 0, "nothing read");
    assert!(ports.bases.lock().unwrap().is_empty());
    assert!(ports.applies().is_empty());
    assert_eq!(read_record(scratch.path()), None, "nothing recorded");
    drop(held);
    ran(ports.pass(Trigger::Window, Mode::Apply));
    assert!(read_record(scratch.path()).is_some(), "once it is let go");
}

/// The base layer is laid on a build the installed release app laid none on, from that app
/// alone, under the store lock it alone holds (let go before the sync kinds read anything), and
/// never the store's own builds; otherwise one row says why it is left.
/// NEGATIVE CONTROL: the store lock kept through the sync kinds (`drop(held)` taken out) reads
/// it held at the first sync read.
#[test]
fn the_base_layer_runs_on_a_new_build_from_the_installed_app_and_never_the_store() {
    for (laid, installed) in [(false, true), (false, false), (true, true), (true, false)] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert {
            installed,
            ..Inert::new(scratch.path())
        };
        if laid {
            let mut seen = record(&ports.build(), &[], ports.now_ms());
            seen.routes = ports.routes();
            seen.base = Some(BaseRecord {
                build: ports.build(),
                pending: Vec::new(),
            });
            let dir = scratch.path().join(FileJournal::DIR);
            aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
            write_record(&dir, &seen).unwrap();
        }
        let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
        let ids: Vec<&str> = pass.rows.iter().map(|row| row.id.as_str()).collect();
        assert!(!ids.contains(&"base.pkg.store"), "{ids:?}");
        if !laid && installed {
            assert_eq!(*ports.bases.lock().unwrap(), [BaseLock::Try]);
            assert_eq!(ports.base_applied(), RELAY);
            assert!(!ids.contains(&BASE), "{ids:?}");
            assert_eq!(
                *ports.released_at_first_read.lock().unwrap(),
                Some(1),
                "the store lock let go before the sync kinds read"
            );
            let recorded = read_record(scratch.path()).and_then(|record| record.base);
            assert_eq!(
                recorded,
                Some(BaseRecord {
                    build: ports.build(),
                    pending: Vec::new()
                })
            );
            continue;
        }
        assert!(ports.bases.lock().unwrap().is_empty(), "no base layer read");
        let left = row(&pass.rows, BASE);
        assert_eq!(left.outcome, Outcome::NotApplicable, "{left:?}");
        let why = if installed {
            BASE_ONCE
        } else {
            BASE_NOT_INSTALLED
        };
        assert_eq!(left.why.as_deref(), Some(why));
    }
}

/// A pass that did not lay the base layer leaves it owed to the installed app's next pass on
/// that build: a held copy's pass (or a build's) records nothing laid; a pass beside an install
/// pass that holds the store records each item that stood aside, and one whose item failed
/// records that item. The next pass lays what was left and leaves an item laid on that build as
/// it is, even when it reads changed again (a block the person took out of their shell rc).
/// NEGATIVE CONTROL: a record that marks the build's base layer laid on every pass leaves the
/// installed app's pass after a held copy's at "re-laid once per new build".
#[test]
fn a_base_layer_a_pass_did_not_lay_is_laid_by_the_installed_apps_next_pass() {
    // A held copy's pass first, then the installed app's on the same build.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let held = Inert {
        installed: false,
        ..Inert::new(scratch.path())
    };
    let first = ran(held.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(first.due, Due::NewBuild);
    assert_eq!(
        row(&first.rows, BASE).why.as_deref(),
        Some(BASE_NOT_INSTALLED)
    );
    let app = Inert::new(scratch.path());
    app.now.fetch_add(60_000, Ordering::SeqCst);
    let second = ran(app.pass(Trigger::Window, Mode::Apply));
    assert_eq!(second.due, Due::NotDue);
    assert_eq!(*app.bases.lock().unwrap(), [BaseLock::Try]);
    assert_eq!(app.base_applied(), RELAY, "{:?}", second.rows);

    // Beside an install pass that holds the store, then once it is free.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let app = Inert::new(scratch.path());
    app.contended.store(true, Ordering::SeqCst);
    let aside = ran(app.pass(Trigger::Window, Mode::Apply));
    for id in RELAY {
        let stood = row(&aside.rows, id);
        assert_eq!(stood.outcome, Outcome::Blocked, "{stood:?}");
        assert!(
            matches!(stood.stood_aside, Some(StoodAside::Lock(_))),
            "{stood:?}"
        );
    }
    assert!(app.base_applied().is_empty());
    let pending = read_record(scratch.path()).and_then(|record| record.base);
    assert_eq!(
        pending.map(|base| base.pending),
        Some(RELAY.map(str::to_owned).to_vec())
    );
    app.contended.store(false, Ordering::SeqCst);
    app.now.fetch_add(60_000, Ordering::SeqCst);
    ran(app.pass(Trigger::Window, Mode::Apply));
    assert_eq!(app.base_applied(), RELAY, "laid once the store is free");
    app.now.fetch_add(60_000, Ordering::SeqCst);
    let after = ran(app.pass(Trigger::Window, Mode::Apply));
    assert_eq!(app.bases.lock().unwrap().len(), 2, "then never read again");
    assert_eq!(row(&after.rows, BASE).why.as_deref(), Some(BASE_ONCE));

    // One item failed: the next pass lays it alone.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let app = Inert::new(scratch.path());
    app.fails.lock().unwrap().push("base.shell.links");
    let failed = ran(app.pass(Trigger::Window, Mode::Apply));
    assert_eq!(
        row(&failed.rows, "base.shell.links").outcome,
        Outcome::Failed
    );
    assert_eq!(app.base_applied(), ["base.shell.rc"]);
    app.fails.lock().unwrap().clear();
    app.now.fetch_add(60_000, Ordering::SeqCst);
    let retried = ran(app.pass(Trigger::Window, Mode::Apply));
    assert_eq!(
        app.base_applied(),
        ["base.shell.rc", "base.shell.links"],
        "the rc block laid on this build is not put back: {:?}",
        retried.rows
    );
    let rc = row(&retried.rows, "base.shell.rc");
    assert_eq!(rc.outcome, Outcome::NotApplicable, "{rc:?}");
    assert!(
        rc.why.as_deref().is_some_and(|why| why.contains(BASE_LAID)),
        "{rc:?}"
    );
    let laid = read_record(scratch.path()).and_then(|record| record.base);
    assert_eq!(laid.map(|base| base.pending), Some(Vec::new()));
}

/// Two builds that take turns over one record (the installed app, a subscriber's held copy of
/// another build) make neither one's later pass due as new, and the installed app lays the base
/// layer once for its build, however many of the held copy's passes come between.
/// NEGATIVE CONTROL: a pass due as new whenever the record's last build differs reads every
/// pass as new.
#[test]
fn two_builds_that_take_turns_neither_make_each_other_new_nor_relay_the_base() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let app = Inert::new(scratch.path());
    let held = Inert {
        installed: false,
        build: Mutex::new("0.144.0+gold".to_owned()),
        ..Inert::new(scratch.path())
    };
    for round in 0..3_u64 {
        for runner in [&app, &held] {
            runner
                .now
                .store(1_000_000_000 + round * 120_000, Ordering::SeqCst);
        }
        let by_app = ran(app.pass(Trigger::Window, Mode::Apply));
        held.now.fetch_add(60_000, Ordering::SeqCst);
        let by_held = ran(held.pass(Trigger::Subscriber, Mode::Apply));
        let want = if round == 0 {
            Due::NewBuild
        } else {
            Due::NotDue
        };
        assert_eq!((by_app.due, by_held.due), (want, want), "round {round}");
    }
    assert_eq!(app.bases.lock().unwrap().len(), 1, "the app laid it once");
    assert_eq!(app.base_applied(), RELAY);
    assert!(held.bases.lock().unwrap().is_empty());
    let recorded = read_record(scratch.path()).expect("recorded");
    assert_eq!(recorded.build, "0.144.0+gold");
    assert_eq!(recorded.builds, ["0.145.0+gabc", "0.144.0+gold"]);
    assert_eq!(
        recorded.base.map(|base| base.build).as_deref(),
        Some("0.145.0+gabc"),
        "the held copy's passes carry the app's base record"
    );
}

/// A record remembers each build that ran a pass once, where it last ran, and no more than
/// [`BUILDS_KEPT`] of them; a new [`SPEC`] starts it again.
#[test]
fn the_record_remembers_the_last_builds_each_once() {
    let mut record = None;
    for build in ["b1", "b2", "b1"] {
        record = Some(Record {
            builds: builds_after(record.as_ref(), build),
            ..self::record(build, &[], 0)
        });
    }
    assert_eq!(record.as_ref().unwrap().builds, ["b2", "b1"]);
    for n in 0..20 {
        let build = format!("n{n}");
        record = Some(Record {
            builds: builds_after(record.as_ref(), &build),
            ..self::record(&build, &[], 0)
        });
    }
    let builds = &record.as_ref().unwrap().builds;
    assert_eq!(builds.len(), BUILDS_KEPT);
    assert_eq!(builds.last().map(String::as_str), Some("n19"));
    let old_spec = Record {
        spec: SPEC + 1,
        ..record.unwrap()
    };
    assert_eq!(builds_after(Some(&old_spec), "c1"), ["c1"]);
}

/// The subscriber's pass resyncs the roster with no sign-in only while no aterm window runs, and
/// a person's resyncs it either way; a window's pass leaves it to the window's upkeep.
/// NEGATIVE CONTROL: a subscriber's pass that does not ask whether a window runs resyncs beside
/// the window's upkeep.
#[test]
fn the_accounts_resync_from_the_subscriber_or_a_person_and_never_from_a_window() {
    for trigger in [
        Trigger::Subscriber,
        Trigger::Init,
        Trigger::Window,
        Trigger::Setup,
    ] {
        for window in [false, true] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let ports = Inert {
                window,
                ..Inert::new(scratch.path())
            };
            let pass = ran(ports.pass(trigger, Mode::Apply));
            let accounts = row(&pass.rows, ACCOUNTS);
            let resynced = ports.applies().contains(&"accounts");
            let leaves = trigger.in_window() || (trigger == Trigger::Subscriber && window);
            if leaves {
                assert_eq!(accounts.outcome, Outcome::NotApplicable, "{accounts:?}");
                assert!(!resynced, "{trigger:?} window={window}");
            } else {
                assert_eq!(accounts.outcome, Outcome::Changed, "{accounts:?}");
                assert!(resynced, "{trigger:?} window={window}");
            }
        }
    }
}

/// A sync that leaves a fleet a person joined by hand stands aside, waiting on that person, and
/// fails nothing; one the issuer did not answer fails, for the next pass to try again.
#[test]
fn a_fleet_left_as_a_person_joined_it_waits_and_an_unanswered_join_fails() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.fleet.lock().unwrap() = FleetRead::Due("this Mac holds another fleet's key".into());
    *ports.fleet_done.lock().unwrap() = Ok(FleetDone::Blocked(
        "this Mac joined another fleet by hand".to_owned(),
    ));
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let fleet = row(&pass.rows, FLEET);
    assert_eq!(fleet.outcome, Outcome::Blocked, "{fleet:?}");
    assert!(
        matches!(&fleet.stood_aside, Some(StoodAside::Waiting(why)) if why.contains("by hand"))
    );
    assert!(!fleet.outcome.is_failure());

    *ports.fleet_done.lock().unwrap() = Err("the issuer at m22:7100 did not answer".to_owned());
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let fleet = row(&pass.rows, FLEET);
    assert_eq!(fleet.outcome, Outcome::Failed, "{fleet:?}");
    assert_eq!(
        fleet.why.as_deref(),
        Some("the issuer at m22:7100 did not answer; the next pass tries again")
    );
}

/// A plan reads every kind and writes nothing: no lock file, record, journal or apply.
#[test]
fn a_plan_writes_nothing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let pass = ran(ports.pass(Trigger::Init, Mode::Plan));
    assert_eq!(row(&pass.rows, FLEET).outcome, Outcome::Planned);
    assert_eq!(*ports.bases.lock().unwrap(), [BaseLock::Read]);
    assert!(ports.applies().is_empty());
    assert!(ports.base_applied().is_empty());
    assert_eq!(
        std::fs::read_dir(scratch.path()).unwrap().count(),
        0,
        "the state directory stays empty"
    );
}

/// The record says the build, the routes and each row; a pass with a failed row keeps the time
/// the last converged pass ended.
#[test]
fn the_record_keeps_the_last_converged_time_across_a_failed_pass() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let first = read_record(scratch.path()).expect("recorded");
    assert_eq!(first.build, "0.145.0+gabc");
    assert_eq!(first.spec, SPEC);
    assert_eq!(first.routes, ["192.0.2.7", "-"]);
    assert_eq!(first.converged_ms, Some(first.attempted_ms));
    assert!(
        first
            .rows
            .iter()
            .any(|row| row.id == FLEET && row.outcome == Outcome::Changed)
    );
    ports.now.fetch_add(31 * 60 * 1000, Ordering::SeqCst);
    *ports.fleet.lock().unwrap() = FleetRead::Due("this Mac is in no fleet".to_owned());
    *ports.fleet_done.lock().unwrap() = Err("the issuer did not answer".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    assert_eq!(pass.due, Due::Recheck);
    let second = read_record(scratch.path()).expect("recorded");
    assert_eq!(second.attempted_ms, first.attempted_ms + 31 * 60 * 1000);
    assert_eq!(second.converged_ms, first.converged_ms, "{second:?}");
}

/// Only the window's pass looks for and joins an open vault; any other says where the join
/// happens.
#[test]
fn an_open_vault_is_joined_from_the_window_alone() {
    for trigger in [
        Trigger::Window,
        Trigger::Subscriber,
        Trigger::Init,
        Trigger::Setup,
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        *ports.peers.lock().unwrap() = PeersRead::Joinable("vault alab answered".to_owned());
        let pass = ran(ports.pass(trigger, Mode::Apply));
        let peers = row(&pass.rows, PEERS);
        let joined = ports.applies().contains(&"peers");
        let looks = ports.peer_looks.lock().unwrap().clone();
        assert!(
            !looks.is_empty()
                && looks
                    .iter()
                    .all(|&look| look == (trigger == Trigger::Window)),
            "{trigger:?}: {looks:?}"
        );
        if trigger == Trigger::Window {
            assert_eq!(peers.outcome, Outcome::Changed, "{peers:?}");
            assert!(joined);
        } else {
            assert_eq!(peers.outcome, Outcome::Deferred, "{peers:?}");
            assert!(!joined, "{trigger:?}");
        }
    }
}

/// A plan (`--check`) from the window does not look for the open vault, as its applying pass
/// does: the look keeps what it heard (the network's verdict, the Local Network answer), and a
/// plan writes nothing.
/// NEGATIVE CONTROL: a look that ignores the pass's mode looks from the window's plan.
#[test]
fn a_check_from_the_window_never_looks_for_the_open_vault() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.peers.lock().unwrap() = PeersRead::Joinable("vault alab answered".to_owned());
    ran(ports.pass(Trigger::Window, Mode::Plan));
    let looks = ports.peer_looks.lock().unwrap().clone();
    assert!(
        !looks.is_empty() && looks.iter().all(|&look| !look),
        "{looks:?}"
    );
    assert!(ports.applies().is_empty());
}

/// A join the vault keeps waiting (it asks a join password) stands aside, waiting on a person,
/// and fails nothing; a Mac with no vault to join reads not applicable.
#[test]
fn a_join_the_vault_keeps_waiting_stands_aside() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.peers.lock().unwrap() = PeersRead::Joinable("vault alab answered".to_owned());
    *ports.peers_done.lock().unwrap() =
        PeersDone::Waiting("vault alab asks a join password".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let peers = row(&pass.rows, PEERS);
    assert_eq!(peers.outcome, Outcome::Blocked, "{peers:?}");
    assert!(
        matches!(&peers.stood_aside, Some(StoodAside::Waiting(why)) if why.contains("password"))
    );
    assert!(!peers.outcome.is_failure());

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.peers.lock().unwrap() = PeersRead::Off("no ALab vault on this network".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    assert_eq!(row(&pass.rows, PEERS).outcome, Outcome::NotApplicable);
}

/// Outside the account's own login a pass refuses before it reads anything.
#[test]
fn outside_the_accounts_own_login_a_pass_refuses_and_reads_nothing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert {
        own: false,
        ..Inert::new(scratch.path())
    };
    let stopped = ports.pass(Trigger::Init, Mode::Apply);
    assert!(matches!(stopped, Err(Stop::Refused(_))), "{stopped:?}");
    assert_eq!(ports.reads.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}

#[test]
fn the_ledger_line_names_the_build_the_trigger_a_tally_and_what_is_left() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.fleet_done.lock().unwrap() = Err("the issuer did not answer".to_owned());
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let line = ledger(
        "0.145.0+gabc",
        Trigger::Subscriber,
        pass.due,
        &pass.rows,
        None,
    );
    assert!(
        line.starts_with("converge build=0.145.0+gabc trigger=subscriber/new-build ok="),
        "{line}"
    );
    assert!(line.contains(" changed=4 "), "{line}");
    assert!(line.contains(" failed=1 "), "{line}");
    assert!(
        line.contains("sync.fleet: the issuer did not answer; the next pass tries again"),
        "{line}"
    );
    assert!(
        line.contains("base.person: "),
        "a planned row is named: {line}"
    );
}

#[test]
fn a_trigger_reads_back_its_own_word() {
    for trigger in [
        Trigger::Subscriber,
        Trigger::Window,
        Trigger::Setup,
        Trigger::Init,
    ] {
        assert_eq!(Trigger::parse(trigger.word()), Some(trigger));
    }
    assert_eq!(Trigger::parse("cron"), None);
}
