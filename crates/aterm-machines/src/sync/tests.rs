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

/// Why a Mac is no ALab steward, as the inert ports read every Mac but the steward.
const NO_STEWARD: &str = "this Mac holds no admin key of the ALab vault";

/// Ports that read from fields and record every call: each read kind reads due until its apply,
/// then right. `aterm init --converge`'s tests run the runner over them.
pub(crate) struct Inert {
    state: PathBuf,
    own: bool,
    console: bool,
    installed: bool,
    /// Whether an aterm window runs ([`Ports::window_runs`]).
    window: bool,
    /// Whether `[swarm] enabled` leaves swarm participation on ([`Ports::swarm_enabled`]).
    swarm: Mutex<Result<bool, String>>,
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
    identity: Mutex<IdentityRead>,
    fleet: Mutex<FleetRead>,
    pub(crate) fleet_done: Mutex<Result<FleetDone, String>>,
    node: Mutex<NodeRead>,
    steward_revoke: Mutex<StewardRevokeRead>,
    steward_lease: Mutex<StewardLeaseRead>,
    steward_standby_server: Mutex<StewardStandbyServerRead>,
    shared: Mutex<SharedRead>,
    requests: Mutex<RequestsRead>,
    /// What a founding of the vault's Commons ends in ([`Ports::found_commons`]).
    founded: Mutex<Result<String, String>>,
    accounts: Mutex<AccountsRead>,
    fabric: Mutex<FabricRead>,
    /// What a renewal of the broker ends in ([`Ports::renew_broker`]).
    renewed: Mutex<Result<String, String>>,
    peers: Mutex<PeersRead>,
    /// What each `sync.peers` read was let do: look for the open vault.
    peer_looks: Mutex<Vec<bool>>,
    /// Whose network each `sync.steward.revoke`, `sync.steward.lease` and `sync.shared-data` read
    /// named.
    publishers: Mutex<Vec<Publisher>>,
    peers_done: Mutex<PeersDone>,
    /// Each aterm.log line the pass wrote ([`Ports::log`]).
    logged: Mutex<Vec<String>>,
    /// The installed release app's program elsewhere ([`Ports::installed_release_elsewhere`]).
    pub(crate) elsewhere: Option<PathBuf>,
    /// Each run handed to another program, and how it ends.
    pub(crate) ran_elsewhere: Mutex<Vec<(PathBuf, Vec<String>)>>,
    pub(crate) elsewhere_exit: Mutex<Result<Option<i32>, String>>,
    /// At each sync kind's read, whether another take of the pass's lock found it held.
    locked_at_reads: Mutex<Vec<bool>>,
    sleeps: Mutex<Vec<Duration>>,
    fixed_wait_ms: Option<u64>,
}

impl Inert {
    pub(crate) fn new(state: &Path) -> Self {
        Self {
            state: state.to_path_buf(),
            own: true,
            console: true,
            installed: true,
            window: false,
            swarm: Mutex::new(Ok(true)),
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
            identity: Mutex::new(IdentityRead::Current(
                "label m15 in [ai] and [fleet] machine".to_owned(),
            )),
            fleet: Mutex::new(FleetRead::Due("this Mac is in no fleet".to_owned())),
            fleet_done: Mutex::new(Ok(FleetDone::Synced("joined".to_owned()))),
            node: Mutex::new(NodeRead::Current(
                "systems.alab.aterm-node runs (pid 9)".to_owned(),
            )),
            steward_revoke: Mutex::new(StewardRevokeRead::Idle(NO_STEWARD.to_owned())),
            steward_lease: Mutex::new(StewardLeaseRead::Idle(NO_STEWARD.to_owned())),
            steward_standby_server: Mutex::new(StewardStandbyServerRead::Idle(
                NO_STEWARD.to_owned(),
            )),
            shared: Mutex::new(SharedRead::Visible("control record here".to_owned())),
            requests: Mutex::new(RequestsRead::Idle(
                "this Mac is not a Commons Writer, and holds no needs".to_owned(),
            )),
            founded: Mutex::new(Ok("founded, approving m2".to_owned())),
            accounts: Mutex::new(AccountsRead::Due("the roster's sources changed".to_owned())),
            fabric: Mutex::new(FabricRead::Due(
                "the broker runs a replaced build".to_owned(),
            )),
            renewed: Mutex::new(Ok("renewed".to_owned())),
            peers: Mutex::new(PeersRead::Joined("in vault alab".to_owned())),
            peer_looks: Mutex::new(Vec::new()),
            publishers: Mutex::new(Vec::new()),
            peers_done: Mutex::new(PeersDone::Joined("joined".to_owned())),
            logged: Mutex::new(Vec::new()),
            elsewhere: None,
            ran_elsewhere: Mutex::new(Vec::new()),
            elsewhere_exit: Mutex::new(Ok(Some(0))),
            locked_at_reads: Mutex::new(Vec::new()),
            sleeps: Mutex::new(Vec::new()),
            fixed_wait_ms: Some(0),
        }
    }

    fn read<T: Clone>(&self, field: &Mutex<T>) -> Result<T, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        // Another take of the pass's lock, as a second runner's would be: `None` while held. Only
        // where a pass made its lock file: a plan makes none, and this take must not make one.
        if self.state.join(FileJournal::DIR).join(LOCK).exists() {
            let held = try_lock(&self.state).is_ok_and(|lock| lock.is_none());
            self.locked_at_reads.lock().unwrap().push(held);
        }
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

    /// The aterm.log lines the pass wrote.
    pub(crate) fn logged(&self) -> Vec<String> {
        self.logged.lock().unwrap().clone()
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
    fn random_wait(&self, max: Duration) -> Duration {
        match self.fixed_wait_ms {
            Some(ms) => Duration::from_millis(ms).min(max),
            None => random_duration_up_to(max, self.now_ms()),
        }
    }
    fn sleep(&self, duration: Duration) {
        self.sleeps.lock().unwrap().push(duration);
    }
    fn installed_release(&self) -> bool {
        self.installed
    }
    fn window_runs(&self) -> bool {
        self.window
    }
    fn swarm_enabled(&self) -> Result<bool, String> {
        self.swarm.lock().unwrap().clone()
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
    fn identity(&self) -> Result<IdentityRead, String> {
        self.read(&self.identity)
    }
    fn sync_identity(&self) -> Result<String, String> {
        self.applied("identity");
        *self.identity.lock().unwrap() =
            IdentityRead::Current("label m112 in [ai] and [fleet] machine".to_owned());
        Ok("wrote m112 into [ai] machine and [fleet] machine".to_owned())
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
    fn node(&self) -> Result<NodeRead, String> {
        self.read(&self.node)
    }
    fn ensure_node(&self) -> Result<String, String> {
        self.applied("node");
        *self.node.lock().unwrap() =
            NodeRead::Current("systems.alab.aterm-node runs (pid 9)".to_owned());
        Ok("installed and started systems.alab.aterm-node".to_owned())
    }
    fn steward_revoke(&self, by: Publisher) -> Result<StewardRevokeRead, String> {
        self.publishers.lock().unwrap().push(by);
        self.read(&self.steward_revoke)
    }
    fn apply_steward_revoke(&self) -> Result<String, String> {
        self.applied("steward-revoke");
        *self.steward_revoke.lock().unwrap() = StewardRevokeRead::Current(
            "every revoked Mac's roster and pool cascade is complete".to_owned(),
        );
        Ok("finished half-done revoke cascade".to_owned())
    }
    fn steward_lease(&self, by: Publisher) -> Result<StewardLeaseRead, String> {
        self.publishers.lock().unwrap().push(by);
        self.read(&self.steward_lease)
    }
    fn apply_steward_lease(&self) -> Result<String, String> {
        self.applied("steward-lease");
        *self.steward_lease.lock().unwrap() =
            StewardLeaseRead::Current("lease epoch 1 held by m3 until 99999".to_owned());
        Ok("published lease epoch 1 as m3".to_owned())
    }
    fn steward_standby_server(&self) -> Result<StewardStandbyServerRead, String> {
        self.read(&self.steward_standby_server)
    }
    fn apply_steward_standby_server(&self) -> Result<String, String> {
        self.applied("steward-server-standby");
        *self.steward_standby_server.lock().unwrap() = StewardStandbyServerRead::Current(
            "systems.alab.aterm-secrets-server running for m29 (pid 88)".to_owned(),
        );
        Ok("started systems.alab.aterm-secrets-server advertising m29.local".to_owned())
    }
    fn shared_data(&self, by: Publisher) -> Result<SharedRead, String> {
        self.publishers.lock().unwrap().push(by);
        self.read(&self.shared)
    }
    fn requests(&self) -> Result<RequestsRead, String> {
        self.read(&self.requests)
    }
    fn pass_requests(&self) -> Result<String, String> {
        self.applied("requests");
        Ok("0 posted".to_owned())
    }
    fn found_commons(&self) -> Result<String, String> {
        self.applied("commons");
        let founded = self.founded.lock().unwrap().clone();
        if founded.is_ok() {
            *self.shared.lock().unwrap() = SharedRead::Visible("control record here".to_owned());
        }
        founded
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
        let renewed = self.renewed.lock().unwrap().clone();
        if renewed.is_ok() {
            *self.fabric.lock().unwrap() = FabricRead::Current("the broker runs this build".into());
        }
        renewed
    }
    fn turn_on_fabric(&self) -> Result<String, String> {
        self.applied("fabric-on");
        *self.fabric.lock().unwrap() = FabricRead::Current("the broker runs this build".into());
        Ok("turned on".to_owned())
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
    fn log(&self, line: &str) {
        self.logged.lock().unwrap().push(line.to_owned());
    }
    fn installed_release_elsewhere(&self) -> Option<PathBuf> {
        self.elsewhere.clone()
    }
    fn run_elsewhere(&self, program: &Path, args: &[String]) -> Result<Option<i32>, String> {
        self.ran_elsewhere
            .lock()
            .unwrap()
            .push((program.to_path_buf(), args.to_vec()));
        self.elsewhere_exit.lock().unwrap().clone()
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

/// `sync.requests` runs its pass only where this Mac reaches the lab's table, and a plan
/// (`--check`) posts and writes nothing.
#[test]
fn the_requests_pass_runs_only_where_the_table_is_reached_and_never_in_a_plan() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let idle = ran(ports.pass(Trigger::Window, Mode::Apply));
    assert_eq!(row(&idle.rows, REQUESTS).outcome, Outcome::NotApplicable);
    assert!(!ports.applies().contains(&"requests"));
    *ports.requests.lock().unwrap() = RequestsRead::Due("needs: 1 waiting".to_owned());
    let planned = ran(ports.pass(Trigger::Window, Mode::Plan));
    assert_eq!(row(&planned.rows, REQUESTS).outcome, Outcome::Planned);
    assert!(
        !ports.applies().contains(&"requests"),
        "a plan posts nothing"
    );
    let applied = ran(ports.pass(Trigger::Window, Mode::Apply));
    assert!(ports.applies().contains(&"requests"));
    assert_eq!(row(&applied.rows, REQUESTS).outcome, Outcome::Changed);
    assert_eq!(
        row(&applied.rows, REQUESTS).detail.as_deref(),
        Some("0 posted")
    );
    assert_eq!(
        applied
            .rows
            .iter()
            .filter(|r| r.outcome.is_failure())
            .count(),
        0
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
    *ports.identity.lock().unwrap() =
        IdentityRead::NotApplicable("off: [swarm] enabled = false".to_owned());
    *ports.fleet.lock().unwrap() =
        FleetRead::Idle("fleet sync is off after aterm fleet leave".to_owned());
    *ports.node.lock().unwrap() = NodeRead::NotApplicable(
        "off on this Mac by your choice: `aterm node on` turns it on".into(),
    );
    *ports.shared.lock().unwrap() = SharedRead::Off;
    *ports.accounts.lock().unwrap() = AccountsRead::Off;
    *ports.fabric.lock().unwrap() = FabricRead::NotApplicable("the fabric is off".to_owned());
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    for (id, says) in [
        (VAULT, "agent on"),
        (IDENTITY, "[swarm] enabled = false"),
        (FLEET, "fleet leave"),
        (NODE, "node on"),
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

/// A fabric nobody turned off is turned on by a pass an aterm window runs beside (the window's
/// lane, the Secrets stage's, or a person's with a window running), never by the vault
/// subscriber's, whose program can be a held copy, and never with no window; a person's off
/// (`aterm fabric off`'s record) is never turned on by any pass.
/// NEGATIVE CONTROL: a `turns_fabric_on` that ignores the trigger turns the fabric on from the
/// subscriber's pass.
#[test]
fn the_fabric_is_turned_on_beside_a_window_and_a_persons_off_stands() {
    let unset = || FabricRead::Unset("off: never turned off here".to_owned());
    for (trigger, window, turns) in [
        (Trigger::Window, true, true),
        (Trigger::Setup, true, true),
        (Trigger::Init, true, true),
        (Trigger::Subscriber, true, false),
        (Trigger::Window, false, false),
        (Trigger::Init, false, false),
        (Trigger::Subscriber, false, false),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert {
            window,
            ..Inert::new(scratch.path())
        };
        *ports.fabric.lock().unwrap() = unset();
        let pass = ran(ports.pass(trigger, Mode::Apply));
        let fabric = row(&pass.rows, FABRIC);
        let case = format!("{trigger:?} window={window}: {fabric:?}");
        assert_eq!(
            ports.applies().contains(&"fabric-on"),
            turns,
            "{case} {:?}",
            ports.applies()
        );
        if turns {
            assert_eq!(fabric.outcome, Outcome::Changed, "{case}");
            assert_eq!(fabric.detail.as_deref(), Some("turned on"), "{case}");
        } else {
            assert_eq!(fabric.outcome, Outcome::NotApplicable, "{case}");
            assert!(
                fabric
                    .why
                    .as_deref()
                    .is_some_and(|why| why.contains("an aterm window starts turns it on")),
                "{case}"
            );
        }
        // A plan names the same and turns nothing on.
        let scratch = aterm_tempfile::tempdir().unwrap();
        let checked = Inert {
            window,
            ..Inert::new(scratch.path())
        };
        *checked.fabric.lock().unwrap() = unset();
        let plan = ran(checked.pass(trigger, Mode::Plan));
        let want = if turns {
            Outcome::Planned
        } else {
            Outcome::NotApplicable
        };
        assert_eq!(row(&plan.rows, FABRIC).outcome, want, "{case}");
        assert!(!checked.applies().contains(&"fabric-on"), "{case}");
    }
    // The person's off reads not applicable beside a window, and nothing turns it on.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert {
        window: true,
        ..Inert::new(scratch.path())
    };
    *ports.fabric.lock().unwrap() =
        FabricRead::NotApplicable("off by your choice: `aterm fabric off` recorded it".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    assert_eq!(row(&pass.rows, FABRIC).outcome, Outcome::NotApplicable);
    assert!(
        !ports
            .applies()
            .iter()
            .any(|kind| kind.starts_with("fabric"))
    );
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

/// One pass at a time: the pass holds its lock from before the first sync kind is read until
/// its record is written, so a second runner's take finds it held at every read (a window's
/// pass and a subscriber's never apply side by side), and it is let go once the pass ends.
/// NEGATIVE CONTROL: a pass that lets its lock go as soon as it took it (`let _ = ...`) reads
/// the lock free at its sync kinds.
#[test]
fn the_pass_holds_its_lock_from_its_first_read_to_its_record() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    ran(ports.pass(Trigger::Window, Mode::Apply));
    let seen = ports.locked_at_reads.lock().unwrap().clone();
    assert!(
        seen.len() >= 6,
        "each sync kind read once at least: {seen:?}"
    );
    assert!(
        seen.iter().all(|held| *held),
        "held at every read: {seen:?}"
    );
    assert!(
        try_lock(scratch.path()).unwrap().is_some(),
        "let go once the pass ended"
    );
}

/// A refused pass writes one aterm.log line naming its build, trigger and why (a runner's
/// stderr goes nowhere), and a plan that is refused writes none; a pass that ran writes its one
/// ledger line.
/// NEGATIVE CONTROL: a refusal that returns without logging leaves no line.
#[test]
fn a_refused_pass_writes_its_own_log_line() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert {
        own: false,
        ..Inert::new(scratch.path())
    };
    let Err(Stop::Refused(why)) = ports.pass(Trigger::Subscriber, Mode::Apply) else {
        panic!("refused")
    };
    assert_eq!(
        ports.logged(),
        [format!(
            "converge build=0.145.0+gabc trigger=subscriber refused: {why}"
        )]
    );
    assert!(matches!(
        ports.pass(Trigger::Init, Mode::Plan),
        Err(Stop::Refused(_))
    ));
    assert_eq!(ports.logged().len(), 1, "a plan writes nothing");

    let ran_here = Inert::new(scratch.path());
    ran(ran_here.pass(Trigger::Window, Mode::Apply));
    let logged = ran_here.logged();
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert!(logged[0].starts_with("converge build=0.145.0+gabc trigger=window/new-build"));
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
/// a person's resyncs it either way; a window's pass and the first-launch Secrets stage's leave
/// it to the window's upkeep.
/// NEGATIVE CONTROLS: a subscriber's pass that does not ask whether a window runs resyncs beside
/// the window's upkeep; an `in_window` that leaves `Setup` out resyncs from the Secrets stage.
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
            // Written out, not read from the code's own predicate: the window's two triggers
            // never resync, the subscriber only with no window, a person always.
            let leaves = match trigger {
                Trigger::Window | Trigger::Setup => true,
                Trigger::Subscriber => window,
                Trigger::Init => false,
            };
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

/// Publications use the runner's network while the trigger keeps naming who started the pass.
/// A subscriber handed to the installed app uses aterm's Local Network answer; a held copy
/// with no app to hand to keeps the subscriber job's network. Every other trigger uses aterm's.
/// NEGATIVE CONTROL: choosing from the trigger alone bypasses aterm's answer after the handoff.
#[test]
fn a_publication_uses_the_runners_network_and_keeps_its_trigger() {
    for (installed, trigger, want) in [
        (false, Trigger::Subscriber, Publisher::Subscriber),
        (true, Trigger::Subscriber, Publisher::Aterm),
        (false, Trigger::Window, Publisher::Aterm),
        (true, Trigger::Window, Publisher::Aterm),
        (false, Trigger::Setup, Publisher::Aterm),
        (true, Trigger::Setup, Publisher::Aterm),
        (false, Trigger::Init, Publisher::Aterm),
        (true, Trigger::Init, Publisher::Aterm),
    ] {
        for mode in [Mode::Apply, Mode::Plan] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let ports = Inert {
                installed,
                ..Inert::new(scratch.path())
            };
            assert!(ports.elsewhere.is_none(), "no further app to hand to");
            ran(ports.pass(trigger, mode));
            let by = ports.publishers.lock().unwrap().clone();
            assert_eq!(
                by,
                [want, want, want],
                "installed={installed} {trigger:?} {mode:?}: revoke, lease and Commons use this runner's network"
            );
            if mode == Mode::Apply && trigger == Trigger::Subscriber {
                let logged = ports.logged();
                assert_eq!(logged.len(), 1, "one pass ledger line");
                assert!(
                    logged[0].contains("trigger=subscriber/"),
                    "the handoff keeps its subscriber trigger: {logged:?}"
                );
            }
            if mode == Mode::Plan {
                assert!(ports.logged().is_empty(), "a plan writes no ledger line");
                assert!(ports.applies().is_empty(), "a plan applies nothing");
            }
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

/// An open vault's only admin founds its Commons in the pass that reads it foundable, once,
/// whichever runs it; a plan (`--check`) plans it and never founds; a failed founding fails the
/// row for the next pass.
/// NEGATIVE CONTROL: an apply arm that founds nothing (report only, as before) fails the row,
/// which still reads foundable.
#[test]
fn an_admin_founds_commons_in_an_applying_pass_alone() {
    for trigger in [
        Trigger::Window,
        Trigger::Subscriber,
        Trigger::Init,
        Trigger::Setup,
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        *ports.shared.lock().unwrap() = SharedRead::Foundable("vault alab has none".to_owned());
        let plan = ran(ports.pass(trigger, Mode::Plan));
        assert_eq!(row(&plan.rows, SHARED_DATA).outcome, Outcome::Planned);
        assert!(!ports.applies().contains(&"commons"), "{trigger:?}");
        let pass = ran(ports.pass(trigger, Mode::Apply));
        let shared = row(&pass.rows, SHARED_DATA);
        assert_eq!(shared.outcome, Outcome::Changed, "{trigger:?}: {shared:?}");
        assert_eq!(shared.detail.as_deref(), Some("founded, approving m2"));
        ports.now.fetch_add(31 * 60 * 1000, Ordering::SeqCst);
        let again = ran(ports.pass(trigger, Mode::Apply));
        assert_eq!(row(&again.rows, SHARED_DATA).outcome, Outcome::Ok);
        let founded = ports
            .applies()
            .iter()
            .filter(|kind| **kind == "commons")
            .count();
        assert_eq!(founded, 1, "{trigger:?}");
    }

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.shared.lock().unwrap() = SharedRead::Foundable("vault alab has none".to_owned());
    *ports.founded.lock().unwrap() = Err("the vault did not take the publication".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let shared = row(&pass.rows, SHARED_DATA);
    assert_eq!(shared.outcome, Outcome::Failed, "{shared:?}");
    assert!(
        shared
            .why
            .as_deref()
            .is_some_and(|why| why.contains("the vault did not take the publication")),
        "the row names why: {shared:?}"
    );
    // The next pass tries again, and founds once the vault takes the publication.
    *ports.founded.lock().unwrap() = Ok("founded, approving m2".to_owned());
    ports.now.fetch_add(31 * 60 * 1000, Ordering::SeqCst);
    let next = ran(ports.pass(Trigger::Window, Mode::Apply));
    assert_eq!(row(&next.rows, SHARED_DATA).outcome, Outcome::Changed);
    let tries = ports
        .applies()
        .iter()
        .filter(|kind| **kind == "commons")
        .count();
    assert_eq!(tries, 2, "the failed founding and the next pass's");
}

/// A founding that waits on a person (Local Network) reads deferred, and a control record this
/// Mac neither sees nor founds reads not applicable: neither founds.
#[test]
fn a_founding_that_waits_or_is_elsewhere_founds_nothing() {
    for (read, outcome) in [
        (
            SharedRead::Deferred("macOS has not asked aterm about the local network".into()),
            Outcome::Deferred,
        ),
        (
            SharedRead::Unavailable("vault alab names 2 admins".into()),
            Outcome::NotApplicable,
        ),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        *ports.shared.lock().unwrap() = read.clone();
        let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
        assert_eq!(row(&pass.rows, SHARED_DATA).outcome, outcome, "{read:?}");
        assert!(
            !pass.rows.iter().any(|row| row.outcome.is_failure()),
            "{read:?}"
        );
        assert!(!ports.applies().contains(&"commons"), "{read:?}");
    }
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

/// The 0.144 → 0.145 update measured on m22: the broker renewal failed, and the log line cut its
/// why at 160 chars, inside the step's first line, so the launchctl error that failed it was lost.
/// A failed row's why now keeps its head and its tail, and the pass's journal records the failure
/// after the change's line.
/// NEGATIVE CONTROLS: a ledger that keeps only a why's head drops the error; an engine that
/// records no failure leaves the journal without it.
#[test]
fn a_failed_renewal_keeps_its_error_in_the_ledger_line_and_the_journal() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let error = "launchctl bootstrap gui/501 /Users//x/Library/LaunchAgents/systems.alab.astream-\
                 broker.n-0123456789abcdef.plist exited 5: Bootstrap failed: 5: Input/output error";
    let why = format!(
        "plist already /Users//x/Library/LaunchAgents/systems.alab.astream-broker.n-0123456789abcdef\
         .plist; broker FAILED /Users//x/Library/LaunchAgents/systems.alab.astream-broker.\
         n-0123456789abcdef.plist is installed but launchd has not loaded it, so nothing keeps the \
         broker until `aterm fabric on` (or the next converge pass) loads it: {error}"
    );
    *ports.renewed.lock().unwrap() = Err(why.clone());
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let fabric = row(&pass.rows, FABRIC);
    assert_eq!(fabric.outcome, Outcome::Failed, "{fabric:?}");
    assert!(why.chars().count() > LEDGER_FAILED_WHY, "the why is cut");
    let line = ledger(
        "0.145.0+gabc",
        Trigger::Subscriber,
        pass.due,
        &pass.rows,
        None,
    );
    assert!(line.contains(" failed=1 "), "{line}");
    assert!(
        line.contains("sync.fabric: plist already /Users//x/") && line.contains(" … "),
        "its head, then the cut: {line}"
    );
    assert!(line.contains(error), "its error, whole: {line}");
    let kept = clipped(&why, LEDGER_FAILED_WHY);
    assert_eq!(kept.chars().count(), LEDGER_FAILED_WHY, "{kept}");
    assert_eq!(clipped("short", LEDGER_FAILED_WHY), "short");

    let journal = aterm_converge::FileJournal::under(scratch.path()).path();
    let lines: Vec<aterm_json::Value> = std::fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|l| aterm_json::from_str(l).unwrap())
        .collect();
    let fabric: Vec<&aterm_json::Value> = lines
        .iter()
        .filter(|l| l["id"].as_str() == Some(FABRIC))
        .collect();
    assert_eq!(fabric.len(), 2, "the change, then its failure: {lines:?}");
    assert!(fabric[0]["from"].as_str().is_some(), "{lines:?}");
    assert_eq!(
        fabric[1]["failed"].as_str(),
        Some(why.as_str()),
        "{lines:?}"
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
    // The words the subscriber, the window's lane and the Secrets stage start the runner with.
    #[cfg(unix)]
    for (word, trigger) in [
        (aterm_secrets::converge::SUBSCRIBER, Trigger::Subscriber),
        (aterm_secrets::converge::WINDOW, Trigger::Window),
        (aterm_secrets::converge::SETUP, Trigger::Setup),
    ] {
        assert_eq!(Trigger::parse(word), Some(trigger), "{word}");
    }
}

/// `sync.identity` applies before `sync.fleet` and `sync.accounts` so both read the roster label
/// in the same pass, then reads `Ok` on the next pass; a personal vault with no swarm roster reads
/// `NotApplicable` and writes nothing.
/// NEGATIVE CONTROL: ordering `Kind::Identity` after `Kind::Fleet`/`Kind::Accounts` applies
/// `"identity"` after `"fleet"` and `"accounts"`.
#[test]
fn sync_identity_applies_before_fleet_and_accounts_and_skips_personal_vaults() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.identity.lock().unwrap() = IdentityRead::Due {
        from: "[ai] machine unset, [fleet] machine unset".to_owned(),
        to: "label m112 in [ai] and [fleet] machine".to_owned(),
    };
    let first = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(
        ports.applies(),
        ["identity", "fleet", "accounts", "fabric"],
        "sync.identity runs before fleet and accounts"
    );
    let id_row = row(&first.rows, IDENTITY);
    assert_eq!(id_row.outcome, Outcome::Changed);
    assert_eq!(
        id_row.detail.as_deref(),
        Some("wrote m112 into [ai] machine and [fleet] machine")
    );

    ports.now.fetch_add(60_000, Ordering::SeqCst);
    let second = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&second.rows, IDENTITY).outcome, Outcome::Ok);

    let personal = aterm_tempfile::tempdir().unwrap();
    let personal_ports = Inert::new(personal.path());
    *personal_ports.identity.lock().unwrap() =
        IdentityRead::NotApplicable("no swarm label applies on this Mac".to_owned());
    let pass = ran(personal_ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&pass.rows, IDENTITY).outcome, Outcome::NotApplicable);
    assert!(!personal_ports.applies().contains(&"identity"));
}

/// `[swarm] enabled = false` turns every converge sync item `NotApplicable` across every trigger,
/// reading none of the individual sync ports and applying nothing; an invalid `[swarm]` section
/// fails every sync row rather than defaulting on.
/// NEGATIVE CONTROL: on the same ports with `[swarm] enabled = true`, the due sync items read
/// their ports and apply changes.
#[test]
fn swarm_enabled_false_marks_every_sync_item_not_applicable_and_reads_nothing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.peers.lock().unwrap() = PeersRead::Joinable("open vault alab".to_owned());
    *ports.shared.lock().unwrap() =
        SharedRead::Foundable("vault alab has no Commons control".to_owned());
    *ports.swarm.lock().unwrap() = Ok(false);

    for trigger in [
        Trigger::Subscriber,
        Trigger::Window,
        Trigger::Setup,
        Trigger::Init,
    ] {
        let pass = ran(ports.pass(trigger, Mode::Apply));
        for id in [
            PEERS,
            VAULT,
            IDENTITY,
            FLEET,
            NODE,
            SHARED_DATA,
            ACCOUNTS,
            FABRIC,
        ] {
            let left = row(&pass.rows, id);
            assert_eq!(
                left.outcome,
                Outcome::NotApplicable,
                "{trigger:?} {id}: {left:?}"
            );
            assert_eq!(
                left.why.as_deref(),
                Some(SWARM_OFF),
                "{trigger:?} {id}: {left:?}"
            );
        }
    }
    assert_eq!(
        ports.reads.load(Ordering::SeqCst),
        0,
        "no individual sync port read while [swarm] enabled = false"
    );
    assert!(ports.applies().is_empty(), "{:?}", ports.applies());

    *ports.swarm.lock().unwrap() = Err("aterm.toml: swarm enabled setting must be boolean".into());
    let broken = ran(ports.pass(Trigger::Init, Mode::Plan));
    for id in [
        PEERS,
        VAULT,
        IDENTITY,
        FLEET,
        NODE,
        SHARED_DATA,
        ACCOUNTS,
        FABRIC,
    ] {
        let failed = row(&broken.rows, id);
        assert_eq!(failed.outcome, Outcome::Unknown, "{id}: {failed:?}");
        assert!(failed.outcome.is_failure(), "{id}: {failed:?}");
    }

    // Negative control: flipping `[swarm] enabled` back on over the same ports reads and applies.
    *ports.swarm.lock().unwrap() = Ok(true);
    let active = ran(ports.pass(Trigger::Window, Mode::Apply));
    assert!(ports.reads.load(Ordering::SeqCst) > 0);
    assert!(!ports.applies().is_empty(), "{active:?}");
}
#[test]
fn init_skips_join_jitter_and_non_capacity_failures_do_not_retry() {
    // 1. Trigger::Init skips initial 0..=120 s join jitter, while Trigger::Subscriber waits it.
    let scratch_init = aterm_tempfile::tempdir().unwrap();
    let init_ports = Inert {
        fixed_wait_ms: Some(45_000),
        ..Inert::new(scratch_init.path())
    };
    let init_pass = ran(init_ports.pass(Trigger::Init, Mode::Apply));
    assert_eq!(row(&init_pass.rows, FLEET).outcome, Outcome::Changed);
    assert!(
        init_ports.sleeps.lock().unwrap().is_empty(),
        "Trigger::Init must not wait initial join jitter"
    );

    let scratch_sub = aterm_tempfile::tempdir().unwrap();
    let sub_ports = Inert {
        fixed_wait_ms: Some(45_000),
        ..Inert::new(scratch_sub.path())
    };
    let sub_pass = ran(sub_ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&sub_pass.rows, FLEET).outcome, Outcome::Changed);
    assert_eq!(
        *sub_ports.sleeps.lock().unwrap(),
        vec![Duration::from_secs(45)],
        "Trigger::Subscriber must wait initial join jitter before dialing"
    );

    // 2. Non-capacity error and Blocked stand-aside do NOT retry at capacity backoff.
    let scratch_err = aterm_tempfile::tempdir().unwrap();
    let err_ports = Inert::new(scratch_err.path());
    *err_ports.fleet_done.lock().unwrap() = Err("the issuer did not answer".to_owned());
    let err_pass = ran(err_ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&err_pass.rows, FLEET).outcome, Outcome::Failed);
    assert_eq!(
        err_ports
            .applies()
            .iter()
            .filter(|&&k| k == "fleet")
            .count(),
        1,
        "non-capacity failure must not retry"
    );

    // 3. Permanently wedged capacity stops within FLEET_JOIN_RETRY_BUDGET (<= 10 min).
    let scratch_cap = aterm_tempfile::tempdir().unwrap();
    let cap_ports = Inert {
        fixed_wait_ms: Some(120_000),
        ..Inert::new(scratch_cap.path())
    };
    *cap_ports.fleet_done.lock().unwrap() = Ok(FleetDone::AtCapacity(
        "fleet listener at 10.0.0.1:7100 closed during handshake".to_owned(),
    ));
    let cap_pass = ran(cap_ports.pass(Trigger::Subscriber, Mode::Apply));
    let fleet_row = row(&cap_pass.rows, FLEET);
    assert_eq!(fleet_row.outcome, Outcome::Failed);
    assert_eq!(
        fleet_row.why.as_deref(),
        Some("fleet listener at 10.0.0.1:7100 closed during handshake; the next pass tries again")
    );
    let total_slept: Duration = cap_ports.sleeps.lock().unwrap().iter().copied().sum();
    assert!(
        total_slept <= FLEET_JOIN_RETRY_BUDGET,
        "total jitter + retry sleep {total_slept:?} exceeded {FLEET_JOIN_RETRY_BUDGET:?}"
    );
    assert!(
        cap_ports
            .applies()
            .iter()
            .filter(|&&k| k == "fleet")
            .count()
            > 1,
        "at-capacity refusal must retry with backoff"
    );
}

#[test]
fn thirty_five_joins_against_sixteen_connections_all_land_within_ten_minutes() {
    use std::sync::Condvar;

    const SUBSCRIBERS: usize = 35;
    const MAX_CONNECTIONS: usize = 16;
    const CONN_HOLD_MS: u64 = 5_000;

    struct SimState {
        now_ms: u64,
        active_runners: usize,
        sleeping: Vec<u64>,
        slot_free_at_ms: Vec<u64>,
    }

    struct SimCluster {
        state: Mutex<SimState>,
        cvar: Condvar,
        retry_at_capacity: bool,
        simultaneous_burst: bool,
    }

    impl SimCluster {
        fn advance_if_all_parked(state: &mut SimState, cvar: &Condvar) {
            if state.active_runners == state.sleeping.len()
                && let Some(&next_ms) = state.sleeping.iter().min()
            {
                state.now_ms = state.now_ms.max(next_ms);
                cvar.notify_all();
            }
        }

        fn run(retry_at_capacity: bool, simultaneous_burst: bool) -> (usize, usize, u64) {
            let root = aterm_tempfile::tempdir().unwrap();
            let cluster = Arc::new(Self {
                state: Mutex::new(SimState {
                    now_ms: 0,
                    active_runners: SUBSCRIBERS,
                    sleeping: Vec::new(),
                    slot_free_at_ms: Vec::new(),
                }),
                cvar: Condvar::new(),
                retry_at_capacity,
                simultaneous_burst,
            });

            struct RunnerGuard(Arc<SimCluster>);
            impl Drop for RunnerGuard {
                fn drop(&mut self) {
                    let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
                    state.active_runners -= 1;
                    SimCluster::advance_if_all_parked(&mut state, &self.0.cvar);
                }
            }

            struct SimSubscriber {
                idx: usize,
                dir: PathBuf,
                cluster: Arc<SimCluster>,
                rng: Mutex<u64>,
                joined: AtomicBool,
                initial_jitter: Mutex<Option<Duration>>,
            }

            impl Ports for SimSubscriber {
                fn background(&self) {}
                fn own_account(&self) -> bool {
                    true
                }
                fn at_console(&self) -> Result<bool, String> {
                    Ok(true)
                }
                fn state_dir(&self) -> Option<PathBuf> {
                    Some(self.dir.clone())
                }
                fn build(&self) -> String {
                    "0.146.0+gsim".to_owned()
                }
                fn routes(&self) -> Vec<String> {
                    vec!["10.0.0.10".to_owned()]
                }
                fn now_ms(&self) -> u64 {
                    1_700_000_000_000 + self.cluster.state.lock().unwrap().now_ms
                }
                fn random_wait(&self, max: Duration) -> Duration {
                    let max_ms = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
                    if max_ms == 0 {
                        return Duration::ZERO;
                    }
                    let mut first = self.initial_jitter.lock().unwrap();
                    if first.is_none() && max == FLEET_JOIN_JITTER_MAX {
                        let pick = if self.cluster.simultaneous_burst {
                            // Synchronize all 35 subscribers onto t = 1 ms to force 19 collisions!
                            Duration::from_millis(1)
                        } else {
                            let mut s = self.rng.lock().unwrap();
                            *s ^= *s << 13;
                            *s ^= *s >> 7;
                            *s ^= *s << 17;
                            Duration::from_millis(*s % (max_ms + 1))
                        };
                        *first = Some(pick);
                        return pick;
                    }
                    let mut s = self.rng.lock().unwrap();
                    *s ^= *s << 13;
                    *s ^= *s >> 7;
                    *s ^= *s << 17;
                    Duration::from_millis(*s % (max_ms + 1))
                }
                fn sleep(&self, duration: Duration) {
                    let delta_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
                    if delta_ms == 0 {
                        return;
                    }
                    let mut state = self.cluster.state.lock().unwrap();
                    let wake_at = state.now_ms + delta_ms;
                    state.sleeping.push(wake_at);
                    while state.now_ms < wake_at {
                        SimCluster::advance_if_all_parked(&mut state, &self.cluster.cvar);
                        if state.now_ms >= wake_at {
                            break;
                        }
                        state = self.cluster.cvar.wait(state).unwrap();
                    }
                    if let Some(pos) = state.sleeping.iter().position(|&t| t == wake_at) {
                        state.sleeping.swap_remove(pos);
                    }
                }
                fn installed_release(&self) -> bool {
                    false
                }
                fn window_runs(&self) -> bool {
                    false
                }
                fn swarm_enabled(&self) -> Result<bool, String> {
                    Ok(true)
                }
                fn base(&self, _lock: BaseLock) -> Base {
                    Base::none()
                }
                fn vault(&self) -> Result<VaultRead, String> {
                    Ok(VaultRead::Runs("runs".to_owned()))
                }
                fn identity(&self) -> Result<IdentityRead, String> {
                    Ok(IdentityRead::Current("label current".to_owned()))
                }
                fn sync_identity(&self) -> Result<String, String> {
                    Ok("wrote label".to_owned())
                }
                fn fleet(&self) -> Result<FleetRead, String> {
                    if self.joined.load(Ordering::SeqCst) {
                        Ok(FleetRead::Current(
                            "in the vault's default fleet".to_owned(),
                        ))
                    } else {
                        Ok(FleetRead::Due("this Mac is in no fleet".to_owned()))
                    }
                }
                fn sync_fleet(&self) -> Result<FleetDone, String> {
                    let mut state = self.cluster.state.lock().unwrap();
                    let now = state.now_ms;
                    state.slot_free_at_ms.retain(|&free_at| free_at > now);
                    if state.slot_free_at_ms.len() >= MAX_CONNECTIONS {
                        let msg =
                            "fleet listener at 10.0.0.1:7100 closed during handshake".to_owned();
                        return if self.cluster.retry_at_capacity {
                            Ok(FleetDone::AtCapacity(msg))
                        } else {
                            Err(msg)
                        };
                    }
                    state.slot_free_at_ms.push(now + CONN_HOLD_MS);
                    drop(state);
                    self.joined.store(true, Ordering::SeqCst);
                    Ok(FleetDone::Synced(format!("m{} joined", self.idx + 1)))
                }
                fn steward_revoke(&self, _by: Publisher) -> Result<StewardRevokeRead, String> {
                    Ok(StewardRevokeRead::Idle("no admin key here".to_owned()))
                }
                fn apply_steward_revoke(&self) -> Result<String, String> {
                    Err("no steward here".to_owned())
                }
                fn shared_data(&self, _by: Publisher) -> Result<SharedRead, String> {
                    Ok(SharedRead::Visible("control record here".to_owned()))
                }
                fn requests(&self) -> Result<RequestsRead, String> {
                    Ok(RequestsRead::Idle("off".to_owned()))
                }
                fn pass_requests(&self) -> Result<String, String> {
                    Ok("0 posted".to_owned())
                }
                fn found_commons(&self) -> Result<String, String> {
                    Ok("founded".to_owned())
                }
                fn accounts(&self) -> Result<AccountsRead, String> {
                    Ok(AccountsRead::Current("in step".to_owned()))
                }
                fn resync_accounts(&self) -> Result<String, String> {
                    Ok("synced".to_owned())
                }
                fn fabric(&self) -> Result<FabricRead, String> {
                    Ok(FabricRead::NotApplicable("off".to_owned()))
                }
                fn renew_broker(&self) -> Result<String, String> {
                    Ok("current".to_owned())
                }
                fn turn_on_fabric(&self) -> Result<String, String> {
                    Err("the fabric is off here".to_owned())
                }
                fn peers(&self, _look: bool) -> Result<PeersRead, String> {
                    Ok(PeersRead::Joined("in vault alab".to_owned()))
                }
                fn join_peers(&self) -> Result<PeersDone, String> {
                    Ok(PeersDone::Joined("joined".to_owned()))
                }
                fn log(&self, _line: &str) {}
                fn installed_release_elsewhere(&self) -> Option<PathBuf> {
                    None
                }
                fn run_elsewhere(
                    &self,
                    _program: &Path,
                    _args: &[String],
                ) -> Result<Option<i32>, String> {
                    unreachable!("the capacity simulation has no installed app")
                }
            }

            let mut handles = Vec::with_capacity(SUBSCRIBERS);
            for idx in 0..SUBSCRIBERS {
                let dir = root.path().join(format!("m{}", idx + 1));
                std::fs::create_dir_all(&dir).unwrap();
                let cluster = Arc::clone(&cluster);
                handles.push(std::thread::spawn(move || {
                    let _guard = RunnerGuard(Arc::clone(&cluster));
                    let seed = ((idx as u64) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    let ports = SimSubscriber {
                        idx,
                        dir,
                        cluster,
                        rng: Mutex::new(seed),
                        joined: AtomicBool::new(false),
                        initial_jitter: Mutex::new(None),
                    };
                    let pass = ran(converge(
                        &ports,
                        Trigger::Subscriber,
                        Mode::Apply,
                        &mut |_| {},
                    ));
                    if !ports.cluster.simultaneous_burst {
                        assert!(
                            ports.initial_jitter.lock().unwrap().is_some(),
                            "the subscriber's pass draws its join jitter"
                        );
                    }
                    row(&pass.rows, FLEET).outcome
                }));
            }

            let mut changed = 0;
            let mut failed = 0;
            for handle in handles {
                match handle.join().unwrap() {
                    Outcome::Changed => changed += 1,
                    Outcome::Failed => failed += 1,
                    other => panic!("unexpected sync.fleet outcome: {other:?}"),
                }
            }
            let elapsed_ms = cluster.state.lock().unwrap().now_ms;
            (changed, failed, elapsed_ms)
        }
    }

    // NEGATIVE CONTROL (§7.1, §12.3 C18): Without retry at capacity, a simultaneous 35-subscriber
    // burst against 16 listener slots lands 16 and leaves 19 failed until the next 30-min pass.
    let (no_retry_ok, no_retry_failed, _) = SimCluster::run(false, true);
    assert_eq!(no_retry_ok, 16, "without retry only 16 slots succeed");
    assert_eq!(
        no_retry_failed, 19,
        "without retry 19 of 35 subscribers fail at capacity"
    );

    // Worst-case simultaneous burst (all 35 hit at t = 1 ms) WITH C18 backoff retry:
    // all 35 land within <= 10 min.
    let (burst_ok, burst_failed, burst_ms) = SimCluster::run(true, true);
    assert_eq!(burst_ok, SUBSCRIBERS);
    assert_eq!(burst_failed, 0);
    assert!(
        Duration::from_millis(burst_ms) <= FLEET_JOIN_RETRY_BUDGET,
        "simultaneous 35-join burst took {burst_ms} ms (> 10 min)"
    );

    // Full C18 (0..=120 s join jitter + backoff retry): all 35 land within <= 10 min.
    let (jitter_ok, jitter_failed, jitter_ms) = SimCluster::run(true, false);
    assert_eq!(jitter_ok, SUBSCRIBERS);
    assert_eq!(jitter_failed, 0);
    assert!(
        Duration::from_millis(jitter_ms) <= FLEET_JOIN_RETRY_BUDGET,
        "jittered 35-join pass took {jitter_ms} ms (> 10 min)"
    );
}
#[test]
fn steward_lease_and_standby_server_items_apply_and_respect_local_network_deferral() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.steward_lease.lock().unwrap() = StewardLeaseRead::Deferred(
        "no lease published; macOS has not asked aterm about the local network yet".to_owned(),
    );
    *ports.steward_standby_server.lock().unwrap() = StewardStandbyServerRead::Due {
        found: "no vault server job is running on always-on steward m29".to_owned(),
        makes: "start systems.alab.aterm-secrets-server advertising m29.local".to_owned(),
    };
    let deferred_pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    assert_eq!(
        row(&deferred_pass.rows, STEWARD_LEASE).outcome,
        Outcome::Deferred
    );
    assert!(!ports.applies().contains(&"steward-lease"));
    assert_eq!(
        row(&deferred_pass.rows, STEWARD_SERVER_STANDBY).outcome,
        Outcome::Changed
    );
    assert!(ports.applies().contains(&"steward-server-standby"));

    // Once on subscriber's network (or allowed LAN), `sync.steward.lease` publishes.
    *ports.steward_lease.lock().unwrap() = StewardLeaseRead::Due {
        found: "no aterm-swarm-lease is published yet".to_owned(),
        makes: "claim lease epoch 1 as m3".to_owned(),
    };
    let active_pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(
        row(&active_pass.rows, STEWARD_LEASE).outcome,
        Outcome::Changed
    );
    assert!(ports.applies().contains(&"steward-lease"));
    assert_eq!(
        row(&active_pass.rows, STEWARD_SERVER_STANDBY).outcome,
        Outcome::Ok
    );
}

/// `sync.node` runs right after `sync.fleet` so a Mac that joins a fleet installs and starts
/// `systems.alab.aterm-node` in the same converge pass; `NotApplicable` (`aterm node off` or no
/// fleet) applies nothing.
/// NEGATIVE CONTROL: when `node` reads `NotApplicable`, `ensure_node` is never called.
#[test]
fn sync_node_runs_after_fleet_and_respects_not_applicable() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.node.lock().unwrap() = NodeRead::Due {
        from: "systems.alab.aterm-node is not installed".to_owned(),
        to: "systems.alab.aterm-node running /Applications/aterm.app/Contents/MacOS/aterm"
            .to_owned(),
    };
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&pass.rows, NODE).outcome, Outcome::Changed);
    let order: Vec<&str> = ports
        .applies()
        .into_iter()
        .filter(|&k| k == "fleet" || k == "node")
        .collect();
    assert_eq!(
        order,
        ["fleet", "node"],
        "sync.node must run after sync.fleet"
    );

    // Negative control: NotApplicable (`aterm node off`) applies nothing.
    let scratch_off = aterm_tempfile::tempdir().unwrap();
    let off_ports = Inert::new(scratch_off.path());
    *off_ports.node.lock().unwrap() =
        NodeRead::NotApplicable("off on this Mac by your choice".to_owned());
    let off_pass = ran(off_ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&off_pass.rows, NODE).outcome, Outcome::NotApplicable);
    assert!(!off_ports.applies().contains(&"node"));
}
