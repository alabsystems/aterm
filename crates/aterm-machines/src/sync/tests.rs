// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge pass over inert ports: nothing here reads or changes the machine.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

mod repository_prerequisites;

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
    prerequisites: Option<Arc<repository_prerequisites::Packages>>,
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
    /// The trigger of each time a runner lowered its scheduling.
    backgrounds: Mutex<Vec<Trigger>>,
    build: Mutex<String>,
    routes: Mutex<Vec<String>>,
    now: AtomicU64,
    reads: AtomicU32,
    bases: Mutex<Vec<BaseLock>>,
    base_applied: Arc<Mutex<Vec<String>>>,
    released: Arc<AtomicU32>,
    applies: Mutex<Vec<&'static str>>,
    dispatches: Mutex<Vec<&'static str>>,
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
    release: Mutex<ReleaseRead>,
    github: Mutex<GithubRead>,
    /// What `github` reads once `sync_github` ran, in place of a current route.
    github_after: Mutex<Option<GithubRead>>,
    /// What `sync.clone` reads ([`Ports::owed_clone`]).
    owed: Mutex<crate::init::OwedClone>,
    /// What `sync.clone-tuning` reads ([`Ports::clone_tuning`]): every clone as it declares by
    /// default, so the other kinds' tests count none of it.
    tuning: Mutex<crate::clone_tuning::Reading>,
    /// What `sync.clone-tuning`'s apply answers; an `Ok` that stood nowhere aside makes the
    /// read current.
    tuned: Mutex<Result<crate::clone_tuning::Tuned, String>>,
    /// Set once `sync.clone-tuning` is read, where a test watches for it.
    tuning_read: Mutex<Option<Arc<AtomicBool>>>,
    fabric: Mutex<FabricRead>,
    fabric_needs_node: bool,
    /// `sync.awake`'s read: right by default, so the other kinds' tests count none of it.
    awake: Mutex<AwakeRead>,
    /// What `sync.awake`'s apply answers; an `Ok` makes the read right.
    awake_done: Mutex<Result<String, String>>,
    /// Whether the person turned aCaffeine's Enable at Startup off ([`Ports::awake_startup_off`]).
    awake_startup_off: AtomicBool,
    /// Old `aterm caffeine` login jobs no aterm will run again ([`Ports::awake_retired_jobs`]).
    awake_retired: Mutex<Vec<String>>,
    /// What retiring old `aterm caffeine` login jobs answers ([`Ports::retire_awake_old_jobs`]).
    retire_awake_done: Mutex<Result<String, String>>,
    /// What a renewal of the broker ends in ([`Ports::renew_broker`]).
    renewed: Mutex<Result<String, String>>,
    peers: Mutex<PeersRead>,
    /// What each `sync.peers` read was let do: look for the open vault.
    peer_looks: Mutex<Vec<bool>>,
    /// Whose network each `sync.steward.revoke`, `sync.steward.lease` and `sync.shared-data` read
    /// named.
    peers_done: Mutex<PeersDone>,
    /// Each aterm.log line the pass wrote ([`Ports::log`]).
    logged: Mutex<Vec<String>>,
    /// The installed release app's program elsewhere ([`Ports::installed_release_elsewhere`]).
    pub(crate) elsewhere: Option<PathBuf>,
    /// Each run handed to another program, and how it ends.
    pub(crate) ran_elsewhere: Mutex<Vec<(PathBuf, Vec<String>)>>,
    pub(crate) elsewhere_exit: Mutex<Result<Option<i32>, String>>,
    refreshed_subscribers: AtomicU32,
    /// Why System State holds the vault out ([`Ports::vault_held`]), and how often it was read.
    held: Mutex<Option<String>>,
    held_reads: AtomicU32,
    /// At each sync kind's read, whether another take of the pass's lock found it held.
    locked_at_reads: Mutex<Vec<bool>>,
    sleeps: Mutex<Vec<Duration>>,
    fixed_wait_ms: Option<u64>,
}

impl Inert {
    pub(crate) fn new(state: &Path) -> Self {
        Self {
            prerequisites: None,
            state: state.to_path_buf(),
            own: true,
            console: true,
            installed: true,
            window: false,
            swarm: Mutex::new(Ok(true)),
            contended: AtomicBool::new(false),
            fails: Arc::new(Mutex::new(Vec::new())),
            released_at_first_read: Mutex::new(None),
            backgrounds: Mutex::new(Vec::new()),
            refreshed_subscribers: AtomicU32::new(0),
            held: Mutex::new(None),
            held_reads: AtomicU32::new(0),
            build: Mutex::new("0.145.0+gabc".to_owned()),
            routes: Mutex::new(vec!["192.0.2.7".to_owned(), "-".to_owned()]),
            now: AtomicU64::new(1_000_000_000),
            reads: AtomicU32::new(0),
            bases: Mutex::new(Vec::new()),
            base_applied: Arc::new(Mutex::new(Vec::new())),
            released: Arc::new(AtomicU32::new(0)),
            applies: Mutex::new(Vec::new()),
            dispatches: Mutex::new(Vec::new()),
            vault: Mutex::new(VaultRead::Runs("runs (pid 7)".to_owned())),
            github: Mutex::new(GithubRead::NotApplicable("no vault helper".into())),
            github_after: Mutex::new(None),
            owed: Mutex::new(crate::init::OwedClone::Nothing(
                "aterm init owes no clone".into(),
            )),
            tuning: Mutex::new(crate::clone_tuning::Reading::Current(
                "1 vault clone on this Mac, each as it declares".into(),
            )),
            tuned: Mutex::new(Ok(crate::clone_tuning::Tuned {
                said: "tuned $HOME/trust (index format 2 -> 4)".into(),
                aside: None,
            })),
            tuning_read: Mutex::new(None),
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
            release: Mutex::new(ReleaseRead::NotApplicable("no release ring".into())),
            fabric: Mutex::new(FabricRead::Due(
                "the broker runs a replaced build".to_owned(),
            )),
            fabric_needs_node: true,
            awake: Mutex::new(AwakeRead::Current("the mug (pid 9) holds".to_owned())),
            awake_done: Mutex::new(Ok("launchd was asked to start the mug".to_owned())),
            awake_startup_off: AtomicBool::new(false),
            awake_retired: Mutex::new(Vec::new()),
            retire_awake_done: Mutex::new(Ok(
                "retired old `aterm caffeine` login item: com.aterm.aterm.caffeine".to_owned(),
            )),
            renewed: Mutex::new(Ok("renewed".to_owned())),
            peers: Mutex::new(PeersRead::Joined("in vault alab".to_owned())),
            peer_looks: Mutex::new(Vec::new()),
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
        self.dispatches.lock().unwrap().push(kind);
    }

    pub(crate) fn applies(&self) -> Vec<&'static str> {
        self.applies.lock().unwrap().clone()
    }

    pub(crate) fn clone_is(&self, owed: crate::init::OwedClone) {
        *self.owed.lock().unwrap() = owed;
    }

    /// The trigger of each time a runner lowered its scheduling.
    pub(crate) fn backgrounds(&self) -> Vec<Trigger> {
        self.backgrounds.lock().unwrap().clone()
    }

    pub(crate) fn base_applied(&self) -> Vec<String> {
        self.base_applied.lock().unwrap().clone()
    }

    /// The aterm.log lines the pass wrote.
    pub(crate) fn logged(&self) -> Vec<String> {
        self.logged.lock().unwrap().clone()
    }

    fn pass(&self, trigger: Trigger, mode: Mode) -> Result<Pass, Stop> {
        converge(
            self,
            trigger,
            mode,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |_| {},
        )
    }
}

impl Ports for Inert {
    fn background(&self, trigger: Trigger) {
        self.backgrounds.lock().unwrap().push(trigger);
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
        self.dispatches.lock().unwrap().push("sleep");
        self.sleeps.lock().unwrap().push(duration);
    }
    fn installed_release(&self) -> bool {
        self.installed
    }
    fn refresh_vault_subscriber(&self) -> Result<Option<String>, String> {
        self.refreshed_subscribers.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
    fn window_runs(&self) -> bool {
        self.window
    }
    fn vault_held(&self) -> Option<String> {
        self.held_reads.fetch_add(1, Ordering::SeqCst);
        self.held.lock().unwrap().clone()
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
    fn repository_prerequisites(&self) -> Option<Box<dyn Item + '_>> {
        self.prerequisites
            .as_ref()
            .map(|packages| repository_prerequisites::item(packages, &self.released))
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
    fn steward_revoke(&self) -> Result<StewardRevokeRead, String> {
        self.read(&self.steward_revoke)
    }
    fn apply_steward_revoke(&self) -> Result<String, String> {
        self.applied("steward-revoke");
        *self.steward_revoke.lock().unwrap() = StewardRevokeRead::Current(
            "every revoked Mac's roster and pool cascade is complete".to_owned(),
        );
        Ok("finished half-done revoke cascade".to_owned())
    }
    fn steward_lease(&self) -> Result<StewardLeaseRead, String> {
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
    fn shared_data(&self) -> Result<SharedRead, String> {
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
    fn release(&self) -> Result<ReleaseRead, String> {
        self.read(&self.release)
    }
    fn sync_release(&self) -> Result<String, String> {
        self.applied("release");
        *self.release.lock().unwrap() = ReleaseRead::Current("unattended signature proved".into());
        Ok("unattended signature proved".into())
    }
    fn github(&self) -> Result<GithubRead, String> {
        self.read(&self.github)
    }
    fn sync_github(&self, journaled: &str) -> Result<String, String> {
        self.applied("github");
        assert_eq!(journaled, "git config --add stand-in");
        *self.github.lock().unwrap() = self
            .github_after
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| GithubRead::Current("vault helper verified".into()));
        Ok("vault helper verified".into())
    }
    fn owed_clone(&self) -> Result<crate::init::OwedClone, String> {
        if self
            .prerequisites
            .as_ref()
            .is_some_and(|packages| !packages.git.load(Ordering::SeqCst))
        {
            return Ok(crate::init::OwedClone::Waiting("Git is missing".into()));
        }
        self.read(&self.owed)
    }
    fn clone_owed(
        &self,
        work: aterm_secrets::converge::RepositoryWork,
        _execution: Option<repository_refresh::Execution>,
    ) -> Result<String, String> {
        if matches!(
            *self.owed.lock().unwrap(),
            crate::init::OwedClone::Verified { .. }
        ) {
            assert_eq!(work, aterm_secrets::converge::RepositoryWork::Refresh);
            self.applied("refresh checkouts");
            return Ok("refreshed".into());
        }
        self.applied("clone");
        *self.owed.lock().unwrap() = crate::init::OwedClone::Verified {
            present: 1,
            total: 1,
        };
        Ok("cloned".into())
    }
    fn forget_owed_clone(&self) -> Result<(), String> {
        self.applied("forget clone");
        *self.owed.lock().unwrap() =
            crate::init::OwedClone::Nothing("aterm init owes no clone".into());
        Ok(())
    }
    fn clone_tuning(&self) -> Result<crate::clone_tuning::Reading, String> {
        if let Some(read) = self.tuning_read.lock().unwrap().as_ref() {
            read.store(true, Ordering::SeqCst);
        }
        self.read(&self.tuning)
    }
    fn tune_clones(&self) -> Result<crate::clone_tuning::Tuned, String> {
        self.applied("tune clones");
        let tuned = self.tuned.lock().unwrap().clone();
        if matches!(&tuned, Ok(tuned) if tuned.aside.is_none()) {
            *self.tuning.lock().unwrap() = crate::clone_tuning::Reading::Current(
                "1 vault clone on this Mac, each as it declares".into(),
            );
        }
        tuned
    }
    fn fabric_needs_node(&self) -> bool {
        self.fabric_needs_node
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
    fn awake(&self) -> Result<AwakeRead, String> {
        self.read(&self.awake)
    }
    fn awake_startup_off(&self) -> bool {
        self.awake_startup_off.load(Ordering::SeqCst)
    }
    fn awake_retired_jobs(&self) -> Vec<String> {
        self.read(&self.awake_retired).unwrap()
    }
    fn retire_awake_old_jobs(&self) -> Result<String, String> {
        self.applied("awake-retire");
        let done = self.retire_awake_done.lock().unwrap().clone();
        if done.is_ok() {
            self.awake_retired.lock().unwrap().clear();
        }
        done
    }
    fn keep_awake(&self) -> Result<String, String> {
        self.applied("awake");
        let done = self.awake_done.lock().unwrap().clone();
        if done.is_ok() {
            self.awake_retired.lock().unwrap().clear();
            *self.awake.lock().unwrap() = AwakeRead::Current("the mug (pid 9) holds".to_owned());
        }
        done
    }
    fn peers(&self, look: bool) -> Result<PeersRead, String> {
        self.dispatches.lock().unwrap().push("peers-read");
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
        repositories: None,
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

#[test]
fn signing_files_stay_pending_until_the_automatic_pass_proves_a_signature() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.release.lock().unwrap() =
        ReleaseRead::Due("authorized files present; signature proof pending".into());
    let planned = ran(ports.pass(Trigger::Init, Mode::Plan));
    assert_eq!(row(&planned.rows, RELEASE).outcome, Outcome::Planned);
    assert!(!ports.applies().contains(&"release"));
    let applied = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&applied.rows, RELEASE).outcome, Outcome::Changed);
    assert!(ports.applies().contains(&"release"));
    let checked = ran(ports.pass(Trigger::Init, Mode::Plan));
    assert_eq!(row(&checked.rows, RELEASE).outcome, Outcome::Ok);
}

/// The clone an `aterm init` owes waits for the vault's GitHub token, then runs in the
/// subscriber's pass once; later refresh work checks the acquired checkout without cloning it
/// again. A plan only plans it.
#[test]
fn the_subscribers_pass_runs_the_clone_init_owes_once_its_token_lands() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let clones = |ports: &Inert| {
        ports
            .applies()
            .iter()
            .filter(|kind| **kind == "clone")
            .count()
    };
    *ports.owed.lock().unwrap() = crate::init::OwedClone::Waiting("no token yet".into());
    let waiting = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&waiting.rows, CLONE).outcome, Outcome::Deferred);
    assert!(!waiting.complete());
    assert_eq!(read_record(scratch.path()).unwrap().converged_ms, None);
    *ports.owed.lock().unwrap() = crate::init::OwedClone::Due("the token landed".into());
    let planned = ran(ports.pass(Trigger::Init, Mode::Plan));
    assert_eq!(row(&planned.rows, CLONE).outcome, Outcome::Planned);
    assert_eq!(clones(&ports), 0);
    let applied = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&applied.rows, CLONE).outcome, Outcome::Changed);
    assert!(applied.complete());
    let again = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&again.rows, CLONE).outcome, Outcome::Ok);
    assert_eq!(clones(&ports), 1, "once");
    assert_eq!(
        ports
            .applies()
            .iter()
            .filter(|kind| **kind == "refresh checkouts")
            .count(),
        1
    );
}

/// Clones that differ from what their repositories declare are tuned by the pass through the
/// port, once: a plan only plans it, the apply runs it, and the next pass reads them as they
/// declare. A clone another git holds, as the row reads or as it applies, is left for the next
/// pass (the row stands aside, never failing a read-back), and a failed apply says the next
/// pass tries again.
#[test]
fn the_pass_tunes_differing_clones_once_and_a_plan_only_plans_it() {
    use crate::clone_tuning::{Reading, Tuned};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let tunes = |ports: &Inert| {
        ports
            .applies()
            .iter()
            .filter(|kind| **kind == "tune clones")
            .count()
    };
    *ports.tuning.lock().unwrap() = Reading::Deferred(
        "$HOME/trust: its index.lock is there (another git holds it, or one that stopped left it); \
         the next pass tunes it"
            .into(),
    );
    let held = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&held.rows, CLONE_TUNING).outcome, Outcome::Deferred);
    assert_eq!(tunes(&ports), 0);
    *ports.tuning.lock().unwrap() = Reading::Due {
        from: "$HOME/trust: core.untrackedCache and feature.manyFiles unset, index format 2".into(),
        to: "tuned as each declares in its scripts/git-clone.gitconfig".into(),
    };
    let planned = ran(ports.pass(Trigger::Init, Mode::Plan));
    let plan = row(&planned.rows, CLONE_TUNING);
    assert_eq!(plan.outcome, Outcome::Planned);
    assert!(
        plan.from
            .as_deref()
            .is_some_and(|from| from.contains("$HOME/trust")),
        "the row names the clone: {plan:?}"
    );
    assert_eq!(tunes(&ports), 0, "a plan writes nothing");
    *ports.tuned.lock().unwrap() = Err("$HOME/trust: core.untrackedCache: could not lock".into());
    let failed = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let failed = row(&failed.rows, CLONE_TUNING);
    assert_eq!(failed.outcome, Outcome::Failed, "{failed:?}");
    assert!(
        failed
            .why
            .as_deref()
            .is_some_and(|why| why.contains("could not lock; the next pass tries again")),
        "{failed:?}"
    );
    *ports.tuned.lock().unwrap() = Ok(Tuned {
        said: "nothing tuned".into(),
        aside: Some(
            "$HOME/trust: its config.lock is there (another git holds it, or one that stopped left \
             it); the next pass tunes it"
                .into(),
        ),
    });
    let raced = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let raced = row(&raced.rows, CLONE_TUNING);
    assert_eq!(raced.outcome, Outcome::Blocked, "{raced:?}");
    assert!(
        raced
            .why
            .as_deref()
            .is_some_and(|why| why.contains("its config.lock is there")),
        "{raced:?}"
    );
    *ports.tuned.lock().unwrap() = Ok(Tuned {
        said: "tuned $HOME/trust (index format 2 -> 4)".into(),
        aside: None,
    });
    let applied = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&applied.rows, CLONE_TUNING).outcome, Outcome::Changed);
    let again = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&again.rows, CLONE_TUNING).outcome, Outcome::Ok);
    assert_eq!(
        tunes(&ports),
        3,
        "the failed, the raced and the tuning apply, then none"
    );
}

/// The clones' tuning runs after the clone `aterm init` owes, over what that clone left.
#[test]
fn clone_tuning_runs_after_the_owed_clone() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.owed.lock().unwrap() = crate::init::OwedClone::Due("1 repository missing".into());
    *ports.tuning.lock().unwrap() = crate::clone_tuning::Reading::Due {
        from: "$HOME/trust: feature.manyFiles unset".into(),
        to: "tuned".into(),
    };
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let at = |id: &str| pass.rows.iter().position(|row| row.id == id).unwrap();
    assert!(at(CLONE) < at(CLONE_TUNING), "{:?}", pass.rows);
    let applied = ports.applies();
    let cloned = applied.iter().position(|kind| *kind == "clone").unwrap();
    let tuned = applied
        .iter()
        .position(|kind| *kind == "tune clones")
        .unwrap();
    assert!(cloned < tuned, "{applied:?}");
}

/// Missing Git and an older Git's missing capability both leave an admitted clone unfinished.
/// Local readiness alone admits its next pass, without a changed vault or a renewed setup run.
/// Explicit off and Stop controls settle without cloning or scheduling unfinished work.
#[test]
fn repository_readiness_waits_resume_and_explicit_holds_are_settled() {
    use crate::init::OwedClone;
    for waiting in [
        OwedClone::Waiting("Git is still installing".into()),
        OwedClone::Person("the installed Git lacks the required capability".into()),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        ports.clone_is(waiting);
        let pending = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
        assert!(!pending.complete(), "{pending:?}");
        assert!(!ports.applies().contains(&"clone"));
        assert_eq!(read_record(scratch.path()).unwrap().converged_ms, None);
        ports.clone_is(OwedClone::Due("managed Git is now ready".into()));
        let ready = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
        assert!(ready.complete(), "{ready:?}");
        assert!(ports.applies().contains(&"clone"));
        let record = read_record(scratch.path()).unwrap();
        assert_eq!(record.converged_ms, Some(record.attempted_ms));
    }
    for (swarm, hold, owed) in [
        (true, None, OwedClone::Off("repositories are off".into())),
        (
            true,
            None,
            OwedClone::Held("repositories left as is".into()),
        ),
        (
            true,
            Some("a Stop holds setup"),
            OwedClone::Waiting("Git is installing".into()),
        ),
        (false, None, OwedClone::Waiting("Git is installing".into())),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        *ports.swarm.lock().unwrap() = Ok(swarm);
        *ports.held.lock().unwrap() = hold.map(str::to_owned);
        ports.clone_is(owed);
        let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
        assert!(pass.complete(), "{pass:?}");
        assert_eq!(row(&pass.rows, CLONE).outcome, Outcome::NotApplicable);
        assert!(!ports.applies().contains(&"clone"));
    }
}

/// Fleet registration, the node listener and account setup finish before cloning starts:
/// a first inventory may contain many large repositories. Event order makes this independent
/// of clone speed or wall-clock timing.
#[test]
fn initial_fleet_and_account_setup_finishes_before_repository_cloning() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.owed.lock().unwrap() = crate::init::OwedClone::Due("53 repositories missing".into());
    *ports.github.lock().unwrap() = GithubRead::Due {
        from: "the GitHub helper is missing".into(),
        revert: "git config --add stand-in".into(),
    };
    *ports.node.lock().unwrap() = NodeRead::Due {
        from: "the node listener is missing".into(),
        to: "the node listener is running".into(),
    };
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    for id in [GITHUB, FLEET, NODE, ACCOUNTS, CLONE] {
        assert_eq!(row(&pass.rows, id).outcome, Outcome::Changed, "{id}");
    }
    let applied = ports.applies();
    let cloned = applied.iter().position(|kind| *kind == "clone").unwrap();
    for prerequisite in ["github", "fleet", "node", "accounts"] {
        assert!(
            applied
                .iter()
                .position(|kind| *kind == prerequisite)
                .unwrap()
                < cloned,
            "{prerequisite} was delayed by repository cloning: {applied:?}"
        );
    }
}

#[test]
fn subscriber_repairs_the_git_helper_and_a_swarm_off_never_does() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.github.lock().unwrap() = GithubRead::Due {
        from: "git's global settings name the login keychain's GitHub helper".into(),
        revert: "git config --add stand-in".into(),
    };
    let planned = ran(ports.pass(Trigger::Init, Mode::Plan));
    assert_eq!(row(&planned.rows, GITHUB).outcome, Outcome::Planned);
    assert_eq!(
        row(&planned.rows, GITHUB).revert.as_deref(),
        Some("git config --add stand-in"),
        "the row carries the block's revert, which the engine journals before the apply"
    );
    assert!(!ports.applies().contains(&"github"));
    *ports.swarm.lock().unwrap() = Ok(false);
    let off = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&off.rows, GITHUB).outcome, Outcome::NotApplicable);
    assert!(!ports.applies().contains(&"github"));
    *ports.swarm.lock().unwrap() = Ok(true);
    let applied = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&applied.rows, GITHUB).outcome, Outcome::Changed);
    assert!(ports.applies().contains(&"github"));
    assert_eq!(
        row(&ran(ports.pass(Trigger::Init, Mode::Plan)).rows, GITHUB).outcome,
        Outcome::Ok
    );
}

/// The block's replacement is what the row applied: a route that then waits on something else
/// (no token yet, a custom source) reads back as done, and the next pass reads that as it stands.
#[test]
fn a_replaced_block_reads_back_as_done_whatever_the_route_waits_on() {
    for after in [
        GithubRead::NotApplicable("a custom GitHub token source is left unchanged".into()),
        GithubRead::Deferred("the GitHub token file is not on this Mac yet".into()),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        *ports.github.lock().unwrap() = GithubRead::Due {
            from: "git's global settings name the login keychain's GitHub helper".into(),
            revert: "git config --add stand-in".into(),
        };
        *ports.github_after.lock().unwrap() = Some(after);
        let applied = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
        assert_eq!(row(&applied.rows, GITHUB).outcome, Outcome::Changed);
        assert_ne!(
            row(&ran(ports.pass(Trigger::Init, Mode::Plan)).rows, GITHUB).outcome,
            Outcome::Ok
        );
    }
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
    *ports.awake.lock().unwrap() =
        AwakeRead::NotApplicable("aCaffeine is off by your choice".to_owned());
    *ports.tuning.lock().unwrap() = crate::clone_tuning::Reading::NotApplicable(
        "off: init.toml's [github] enabled = false".to_owned(),
    );
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    for (id, says) in [
        (VAULT, "agent on"),
        (IDENTITY, "[swarm] enabled = false"),
        (FLEET, "fleet leave"),
        (NODE, "node on"),
        (SHARED_DATA, "[commons] enabled = false"),
        (ACCOUNTS, "[harness] accounts = \"off\""),
        (FABRIC, "the fabric is off"),
        (AWAKE, "off by your choice"),
        (CLONE_TUNING, "[github] enabled = false"),
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
            fabric_needs_node: false,
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
            fabric_needs_node: false,
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
        fabric_needs_node: false,
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
/// fails nothing; one the issuer did not answer fails, for the next pass to try again; and a check
/// that reads the fleet as left so (`FleetRead::Blocked`) plans no change and runs no apply.
///
/// NEGATIVE CONTROL: read `FleetRead::Blocked` as a change, and the plan and the pass report one
/// and run the fleet's apply.
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

    // A check that reads it so plans no change and runs no apply, in a plan or a pass alike.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.fleet.lock().unwrap() =
        FleetRead::Blocked("this Mac joined another fleet by hand".into());
    for mode in [Mode::Plan, Mode::Apply] {
        let pass = ran(ports.pass(Trigger::Subscriber, mode));
        let fleet = row(&pass.rows, FLEET);
        assert_eq!(fleet.outcome, Outcome::Blocked, "{mode:?}: {fleet:?}");
        assert!(
            fleet
                .why
                .as_deref()
                .is_some_and(|why| why.contains("by hand")),
            "{fleet:?}"
        );
        assert!(!fleet.outcome.is_failure());
    }
    assert!(!ports.applies().contains(&"fleet"), "{:?}", ports.applies());
}

/// Subscriber refresh runs from the installed apply path, including a headless subscriber
/// trigger; plans, held copies and swarm-off runs never reach it.
#[test]
fn only_installed_apply_passes_refresh_the_managed_subscriber() {
    for (installed, mode, enabled, refreshed) in [
        (true, Mode::Apply, true, 1),
        (true, Mode::Plan, true, 0),
        (false, Mode::Apply, true, 0),
        (true, Mode::Apply, false, 0),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert {
            installed,
            ..Inert::new(scratch.path())
        };
        *ports.swarm.lock().unwrap() = Ok(enabled);
        ran(ports.pass(Trigger::Subscriber, mode));
        assert_eq!(
            ports.refreshed_subscribers.load(Ordering::SeqCst),
            refreshed
        );
    }
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

/// Only the window's applying pass may join a vault; every other trigger defers it.
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

/// A pass's ledger line keeps naming who started it, the installed app's run of a subscriber
/// pass included, and a plan writes none.
#[test]
fn a_pass_keeps_its_trigger_in_its_ledger_line() {
    for (installed, trigger) in [
        (false, Trigger::Subscriber),
        (true, Trigger::Subscriber),
        (false, Trigger::Window),
        (true, Trigger::Window),
        (false, Trigger::Setup),
        (true, Trigger::Setup),
        (false, Trigger::Init),
        (true, Trigger::Init),
    ] {
        for mode in [Mode::Apply, Mode::Plan] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let ports = Inert {
                installed,
                ..Inert::new(scratch.path())
            };
            assert!(ports.elsewhere.is_none(), "no further app to hand to");
            ran(ports.pass(trigger, mode));
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

/// A plan (`--check`) from the window cannot admit a join. The applying pass alone receives
/// that authority; neither read discovers a vault, and the plan applies nothing.
/// NEGATIVE CONTROL: ignoring the pass's mode admits a join from the window's plan.
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
    *ports.peers_done.lock().unwrap() = PeersDone::Deferred(StoodAside::Waiting(
        "vault alab asks a join password".to_owned(),
    ));
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let peers = row(&pass.rows, PEERS);
    assert_eq!(peers.outcome, Outcome::Blocked, "{peers:?}");
    assert!(
        matches!(&peers.stood_aside, Some(StoodAside::Waiting(why)) if why.contains("password"))
    );
    assert!(!peers.outcome.is_failure());

    *ports.peers_done.lock().unwrap() = PeersDone::Deferred(StoodAside::Offline(
        "the pending join waits for this LAN".to_owned(),
    ));
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let peers = row(&pass.rows, PEERS);
    assert!(
        matches!(&peers.stood_aside, Some(StoodAside::Offline(_))),
        "{peers:?}"
    );
    assert_eq!(peers.outcome, Outcome::Blocked);
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

/// A founding that waits (on the LAN broker) reads deferred, and a control record this
/// Mac neither sees nor founds reads not applicable: neither founds.
#[test]
fn a_founding_that_waits_or_is_elsewhere_founds_nothing() {
    for (read, outcome) in [
        (
            SharedRead::Deferred("the LAN broker is not installed".into()),
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

/// The subscriber's pass, which runs with no window (after a restart, say), starts the mug when
/// none runs, and reads it back held; a mug that runs and asks
/// macOS again itself is left to it, a failed start fails the row for the next pass, and the
/// window's own passes leave it to the window's watch.
/// NEGATIVE CONTROL: with `Kind::Awake` out of `sync_items`, no `sync.awake` row is found.
#[test]
fn the_subscribers_pass_keeps_this_mac_awake() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.awake.lock().unwrap() = AwakeRead::Due("no aCaffeine mug runs".to_owned());
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let kept = row(&pass.rows, AWAKE);
    assert_eq!(kept.outcome, Outcome::Changed, "{kept:?}");
    assert!(ports.applies().contains(&"awake"));

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.awake.lock().unwrap() = AwakeRead::Due("no aCaffeine mug runs".to_owned());
    *ports.awake_done.lock().unwrap() = Err("launchctl bootstrap: 5".to_owned());
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&pass.rows, AWAKE).outcome, Outcome::Failed);

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.awake.lock().unwrap() =
        AwakeRead::Asking("macOS refused the keep-awake activity".to_owned());
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&pass.rows, AWAKE).outcome, Outcome::Deferred);
    assert!(!ports.applies().contains(&"awake"));

    for trigger in [Trigger::Window, Trigger::Setup] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        *ports.awake.lock().unwrap() = AwakeRead::Due("no aCaffeine mug runs".to_owned());
        let pass = ran(ports.pass(trigger, Mode::Apply));
        assert_eq!(row(&pass.rows, AWAKE).outcome, Outcome::NotApplicable);
        assert!(!ports.applies().contains(&"awake"), "{trigger:?}");
    }
}

/// A person who turned aCaffeine's Enable at Startup off chose no mug at login: the subscriber's
/// pass, which runs at login with no window, starts none, while a person's `aterm init
/// --converge` still does (and an aterm window's watch, which no pass is).
#[test]
fn the_subscribers_pass_keeps_a_persons_startup_off() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.awake.lock().unwrap() = AwakeRead::Due("no aCaffeine mug runs".to_owned());
    ports.awake_startup_off.store(true, Ordering::SeqCst);
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let left = row(&pass.rows, AWAKE);
    assert_eq!(left.outcome, Outcome::NotApplicable, "{left:?}");
    assert!(!ports.applies().contains(&"awake"));

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.awake.lock().unwrap() = AwakeRead::Due("no aCaffeine mug runs".to_owned());
    ports.awake_startup_off.store(true, Ordering::SeqCst);
    let pass = ran(ports.pass(Trigger::Init, Mode::Apply));
    assert_eq!(row(&pass.rows, AWAKE).outcome, Outcome::Changed);
    assert!(ports.applies().contains(&"awake"));
}

/// After a restart with no window open, `Enable at Startup` starts the installed app's mug before
/// the subscriber's pass runs (`AwakeRead::Current`), so an old `com.aterm.aterm.caffeine` login
/// item whose `aterm` is gone or 0.145.0 would otherwise keep failing every ten seconds until a
/// window opens (`ATERM-KI-1027-7`). `sync.awake` now retires those dead or post-rename jobs
/// without starting a mug, including over the person's keep-awake or Enable at Startup off, while
/// a plan (`--check`) writes nothing and a window's pass leaves retirement to the window's tidy.
#[test]
fn the_subscribers_pass_retires_old_caffeine_login_items_without_starting_a_mug() {
    // 1. Mug already running (`AwakeRead::Current`): plan reports `Planned` without retiring;
    //    apply retires without calling `keep_awake`, and the next pass reads `Ok`.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.awake_retired.lock().unwrap() = vec!["com.aterm.aterm.caffeine".to_owned()];
    let plan = ran(ports.pass(Trigger::Subscriber, Mode::Plan));
    assert_eq!(row(&plan.rows, AWAKE).outcome, Outcome::Planned);
    assert!(!ports.applies().contains(&"awake-retire"));
    assert!(!ports.applies().contains(&"awake"));

    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let retired = row(&pass.rows, AWAKE);
    assert_eq!(retired.outcome, Outcome::Changed, "{retired:?}");
    assert!(ports.applies().contains(&"awake-retire"));
    assert!(!ports.applies().contains(&"awake"), "mug already runs");
    ports.now.fetch_add(31 * 60 * 1000, Ordering::SeqCst);
    let next = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&next.rows, AWAKE).outcome, Outcome::Ok);

    // 2. Enable at Startup off or keep-awake off: retires old jobs without starting any mug, then
    //    returns to `NotApplicable` on the next pass.
    for startup_off in [true, false] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        ports.awake_startup_off.store(startup_off, Ordering::SeqCst);
        if !startup_off {
            *ports.awake.lock().unwrap() =
                AwakeRead::NotApplicable("aCaffeine is off by your choice".to_owned());
        }
        *ports.awake_retired.lock().unwrap() = vec!["com.aterm.aterm.caffeine".to_owned()];
        let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
        assert_eq!(
            row(&pass.rows, AWAKE).outcome,
            Outcome::Changed,
            "startup_off={startup_off}"
        );
        assert!(ports.applies().contains(&"awake-retire"));
        assert!(!ports.applies().contains(&"awake"));
        ports.now.fetch_add(31 * 60 * 1000, Ordering::SeqCst);
        let next = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
        assert_eq!(
            row(&next.rows, AWAKE).outcome,
            Outcome::NotApplicable,
            "startup_off={startup_off}"
        );
    }

    // 3. A retirement failure marks `sync.awake` `Failed` so the next pass retries.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.awake_retired.lock().unwrap() = vec!["com.aterm.aterm.caffeine".to_owned()];
    *ports.retire_awake_done.lock().unwrap() = Err("LaunchAgents: permission denied".to_owned());
    let failed = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&failed.rows, AWAKE).outcome, Outcome::Failed);

    // 4. NEGATIVE CONTROL: `Trigger::Window` leaves retirement to the window's own `TIDY`.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.awake_retired.lock().unwrap() = vec!["com.aterm.aterm.caffeine".to_owned()];
    let in_win = ran(ports.pass(Trigger::Window, Mode::Apply));
    assert_eq!(row(&in_win.rows, AWAKE).outcome, Outcome::NotApplicable);
    assert!(!ports.applies().contains(&"awake-retire"));
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
    let ports = Inert {
        fabric_needs_node: false,
        ..Inert::new(scratch.path())
    };
    *ports.peers.lock().unwrap() = PeersRead::Joinable("open vault alab".to_owned());
    *ports.shared.lock().unwrap() =
        SharedRead::Foundable("vault alab has no Commons control".to_owned());
    *ports.awake.lock().unwrap() = AwakeRead::Due("no aCaffeine mug runs".to_owned());
    *ports.tuning.lock().unwrap() = crate::clone_tuning::Reading::Due {
        from: "$HOME/trust: feature.manyFiles unset".to_owned(),
        to: "tuned".to_owned(),
    };
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
            AWAKE,
            CLONE_TUNING,
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
        AWAKE,
        CLONE_TUNING,
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
fn independent_fabric_runs_before_remote_work_and_node_hosting_waits_for_node() {
    for needs_node in [false, true] {
        for (before, effect) in [
            (FabricRead::Due("replaced broker".into()), "fabric"),
            (FabricRead::Unset("never turned off".into()), "fabric-on"),
        ] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let ports = Inert {
                fabric_needs_node: needs_node,
                window: true,
                ..Inert::new(scratch.path())
            };
            *ports.fabric.lock().unwrap() = before;
            *ports.node.lock().unwrap() = NodeRead::Due {
                from: "no node runs".into(),
                to: "node running".into(),
            };
            *ports.fleet_done.lock().unwrap() =
                Ok(FleetDone::AtCapacity("issuer at capacity".into()));
            let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
            assert_eq!(row(&pass.rows, FABRIC).outcome, Outcome::Changed);
            assert_eq!(row(&pass.rows, FLEET).outcome, Outcome::Failed);
            assert_eq!(row(&pass.rows, NODE).outcome, Outcome::Changed);
            let calls = ports.dispatches.lock().unwrap();
            let at = |kind| calls.iter().position(|call| *call == kind).unwrap();
            if needs_node {
                assert!(at("node") < at(effect), "{calls:?}");
                assert!(at("sleep") < at(effect), "{calls:?}");
            } else {
                assert!(at(effect) < at("peers-read"), "{calls:?}");
                assert!(at(effect) < at("fleet"), "{calls:?}");
                assert!(at(effect) < at("sleep"), "{calls:?}");
            }
        }
    }
}

#[test]
fn fleet_handshakes_start_immediately_and_retry_only_reported_capacity() {
    for trigger in [
        Trigger::Init,
        Trigger::Subscriber,
        Trigger::Window,
        Trigger::Setup,
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert {
            fixed_wait_ms: Some(45_000),
            ..Inert::new(scratch.path())
        };
        let pass = ran(ports.pass(trigger, Mode::Apply));
        assert_eq!(row(&pass.rows, FLEET).outcome, Outcome::Changed);
        assert!(
            ports.sleeps.lock().unwrap().is_empty(),
            "{trigger:?}: dial before waiting for a capacity response"
        );
    }

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
        "capacity retry sleep {total_slept:?} exceeded {FLEET_JOIN_RETRY_BUDGET:?}"
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

        fn run(retry_at_capacity: bool) -> (usize, usize, u64) {
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
                first_attempt_ms: Mutex<Option<u64>>,
            }

            impl Ports for SimSubscriber {
                fn background(&self, _trigger: Trigger) {}
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
                    self.first_attempt_ms.lock().unwrap().get_or_insert(now);
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
                fn steward_revoke(&self) -> Result<StewardRevokeRead, String> {
                    Ok(StewardRevokeRead::Idle("no admin key here".to_owned()))
                }
                fn apply_steward_revoke(&self) -> Result<String, String> {
                    Err("no steward here".to_owned())
                }
                fn shared_data(&self) -> Result<SharedRead, String> {
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
                fn awake(&self) -> Result<AwakeRead, String> {
                    Ok(AwakeRead::NotApplicable("not macOS".to_owned()))
                }
                fn keep_awake(&self) -> Result<String, String> {
                    Ok("kept".to_owned())
                }
                fn awake_startup_off(&self) -> bool {
                    false
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
                        first_attempt_ms: Mutex::new(None),
                    };
                    let pass = ran(converge(
                        &ports,
                        Trigger::Subscriber,
                        Mode::Apply,
                        aterm_secrets::converge::RepositoryWork::Refresh,
                        &mut |_| {},
                    ));
                    assert_eq!(
                        *ports.first_attempt_ms.lock().unwrap(),
                        Some(0),
                        "every first handshake precedes any capacity wait"
                    );
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
    let (no_retry_ok, no_retry_failed, _) = SimCluster::run(false);
    assert_eq!(no_retry_ok, 16, "without retry only 16 slots succeed");
    assert_eq!(
        no_retry_failed, 19,
        "without retry 19 of 35 subscribers fail at capacity"
    );

    // Worst-case simultaneous burst (all 35 hit at t = 0) WITH C18 backoff retry:
    // all 35 land within <= 10 min.
    let (burst_ok, burst_failed, burst_ms) = SimCluster::run(true);
    assert_eq!(burst_ok, SUBSCRIBERS);
    assert_eq!(burst_failed, 0);
    assert!(
        Duration::from_millis(burst_ms) <= FLEET_JOIN_RETRY_BUDGET,
        "simultaneous 35-join burst took {burst_ms} ms (> 10 min)"
    );
}
#[test]
fn steward_lease_and_standby_server_items_apply_and_respect_a_lan_broker_deferral() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.steward_lease.lock().unwrap() = StewardLeaseRead::Deferred(
        "no lease published; the LAN broker is not installed, so the steward's lease waits for it"
            .to_owned(),
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

/// Design §8.5 and principle 4: the Sync rows ride the Secrets switch, so while System State's
/// plan holds the vault out (the Secrets vault turned off, or a Stop holding setup) no pass acts
/// on the vault: every row that rides it reads Off with the plan's reason, read as the row
/// starts, and the subscriber's repair waits; the fabric, provisioned on this Mac alone, still
/// turns on. The same Mac with nothing holding the vault applies its due rows and repairs its
/// subscriber.
#[test]
fn no_sync_row_acts_on_the_vault_while_system_state_holds_it_out() {
    const HELD: &str = "System State's plan leaves the Secrets vault out, or a Stop holds it";
    for (trigger, mode) in [
        (Trigger::Window, Mode::Apply),
        (Trigger::Subscriber, Mode::Apply),
        (Trigger::Init, Mode::Apply),
        (Trigger::Init, Mode::Plan),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert {
            fabric_needs_node: false,
            ..Inert::new(scratch.path())
        };
        *ports.held.lock().unwrap() = Some(HELD.to_owned());
        *ports.peers.lock().unwrap() =
            PeersRead::Joinable("the open ALab vault answers".to_owned());
        *ports.identity.lock().unwrap() = IdentityRead::Due {
            from: "[ai] machine = unset".to_owned(),
            to: "label m112".to_owned(),
        };
        let pass = ran(ports.pass(trigger, mode));
        assert!(ports.held_reads.load(Ordering::SeqCst) > 0, "read");
        let rode_the_vault: Vec<&str> = ports
            .applies()
            .into_iter()
            .filter(|kind| !kind.starts_with("fabric"))
            .collect();
        assert!(
            rode_the_vault.is_empty(),
            "{trigger:?} {mode:?}: the vault held out, yet the pass applied {rode_the_vault:?}"
        );
        assert_eq!(ports.refreshed_subscribers.load(Ordering::SeqCst), 0);
        for id in [PEERS, VAULT, IDENTITY, FLEET, NODE, SHARED_DATA, ACCOUNTS] {
            let held = row(&pass.rows, id);
            assert_eq!(held.outcome, Outcome::NotApplicable, "{held:?}");
            assert_eq!(held.why.as_deref(), Some(HELD), "{held:?}");
        }
        assert_ne!(row(&pass.rows, FABRIC).why.as_deref(), Some(HELD));
        assert_ne!(row(&pass.rows, AWAKE).why.as_deref(), Some(HELD));
        if mode == Mode::Apply {
            assert!(ports.applies().contains(&"fabric"), "{:?}", ports.applies());
        }
    }
    // NEGATIVE CONTROL: nothing holds the vault, so the same pass joins, labels and syncs.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.peers.lock().unwrap() = PeersRead::Joinable("the open ALab vault answers".to_owned());
    ran(ports.pass(Trigger::Window, Mode::Apply));
    let applied = ports.applies();
    assert!(
        applied.contains(&"peers") && applied.contains(&"fleet"),
        "{applied:?}"
    );
    assert_eq!(ports.refreshed_subscribers.load(Ordering::SeqCst), 1);
}

/// `sync.clone` and System State's holds: a Stop or a turned-off step holds the pass from the row
/// it reaches on, and nothing loops on a step only a person can finish.
pub(crate) mod owed_clone {
    use super::*;
    use crate::init::OwedClone;

    /// Ports that run as [`Inert`] does, with `sync.clone`'s read, apply and forgetting and
    /// System State's hold on the vault each from a hook.
    pub(crate) struct Hooked<'a> {
        pub(crate) inner: Inert,
        pub(crate) held: &'a (dyn Fn() -> Option<String> + Sync),
        pub(crate) owed: &'a (dyn Fn() -> Result<OwedClone, String> + Sync),
        pub(crate) clone: &'a (dyn Fn() -> Result<String, String> + Sync),
        pub(crate) forget: &'a (dyn Fn() -> Result<(), String> + Sync),
    }

    impl Ports for Hooked<'_> {
        fn background(&self, trigger: Trigger) {
            self.inner.background(trigger);
        }
        fn own_account(&self) -> bool {
            self.inner.own_account()
        }
        fn at_console(&self) -> Result<bool, String> {
            self.inner.at_console()
        }
        fn state_dir(&self) -> Option<PathBuf> {
            self.inner.state_dir()
        }
        fn build(&self) -> String {
            self.inner.build()
        }
        fn routes(&self) -> Vec<String> {
            self.inner.routes()
        }
        fn now_ms(&self) -> u64 {
            self.inner.now_ms()
        }
        fn random_wait(&self, max: Duration) -> Duration {
            self.inner.random_wait(max)
        }
        fn sleep(&self, duration: Duration) {
            self.inner.sleep(duration);
        }
        fn installed_release(&self) -> bool {
            self.inner.installed_release()
        }
        fn refresh_vault_subscriber(&self) -> Result<Option<String>, String> {
            self.inner.refresh_vault_subscriber()
        }
        fn window_runs(&self) -> bool {
            self.inner.window_runs()
        }
        fn vault_held(&self) -> Option<String> {
            (self.held)()
        }
        fn swarm_enabled(&self) -> Result<bool, String> {
            self.inner.swarm_enabled()
        }
        fn base(&self, lock: BaseLock) -> Base {
            self.inner.base(lock)
        }
        fn vault(&self) -> Result<VaultRead, String> {
            self.inner.vault()
        }
        fn identity(&self) -> Result<IdentityRead, String> {
            self.inner.identity()
        }
        fn sync_identity(&self) -> Result<String, String> {
            self.inner.sync_identity()
        }
        fn fleet(&self) -> Result<FleetRead, String> {
            self.inner.fleet()
        }
        fn sync_fleet(&self) -> Result<FleetDone, String> {
            self.inner.sync_fleet()
        }
        fn node(&self) -> Result<NodeRead, String> {
            self.inner.node()
        }
        fn ensure_node(&self) -> Result<String, String> {
            self.inner.ensure_node()
        }
        fn shared_data(&self) -> Result<SharedRead, String> {
            self.inner.shared_data()
        }
        fn requests(&self) -> Result<RequestsRead, String> {
            self.inner.requests()
        }
        fn pass_requests(&self) -> Result<String, String> {
            self.inner.pass_requests()
        }
        fn found_commons(&self) -> Result<String, String> {
            self.inner.found_commons()
        }
        fn accounts(&self) -> Result<AccountsRead, String> {
            self.inner.accounts()
        }
        fn resync_accounts(&self) -> Result<String, String> {
            self.inner.resync_accounts()
        }
        fn owed_clone(&self) -> Result<OwedClone, String> {
            (self.owed)()
        }
        fn clone_owed(
            &self,
            _work: aterm_secrets::converge::RepositoryWork,
            _execution: Option<repository_refresh::Execution>,
        ) -> Result<String, String> {
            (self.clone)()
        }
        fn forget_owed_clone(&self) -> Result<(), String> {
            (self.forget)()
        }
        fn clone_tuning(&self) -> Result<crate::clone_tuning::Reading, String> {
            self.inner.clone_tuning()
        }
        fn tune_clones(&self) -> Result<crate::clone_tuning::Tuned, String> {
            self.inner.tune_clones()
        }
        fn fabric(&self) -> Result<FabricRead, String> {
            self.inner.fabric()
        }
        fn renew_broker(&self) -> Result<String, String> {
            self.inner.renew_broker()
        }
        fn turn_on_fabric(&self) -> Result<String, String> {
            self.inner.turn_on_fabric()
        }
        fn awake(&self) -> Result<AwakeRead, String> {
            self.inner.awake()
        }
        fn keep_awake(&self) -> Result<String, String> {
            self.inner.keep_awake()
        }
        fn awake_startup_off(&self) -> bool {
            self.inner.awake_startup_off()
        }
        fn awake_retired_jobs(&self) -> Vec<String> {
            self.inner.awake_retired_jobs()
        }
        fn retire_awake_old_jobs(&self) -> Result<String, String> {
            self.inner.retire_awake_old_jobs()
        }
        fn peers(&self, look: bool) -> Result<PeersRead, String> {
            self.inner.peers(look)
        }
        fn join_peers(&self) -> Result<PeersDone, String> {
            self.inner.join_peers()
        }
        fn log(&self, line: &str) {
            self.inner.log(line);
        }
        fn installed_release_elsewhere(&self) -> Option<PathBuf> {
            self.inner.installed_release_elsewhere()
        }
        fn run_elsewhere(&self, program: &Path, args: &[String]) -> Result<Option<i32>, String> {
            self.inner.run_elsewhere(program, args)
        }
    }

    /// No Sync row acts on the vault while System State holds it (design §8.5, principle 4): a
    /// person who presses Stop after fleet registration holds every row after it, each reading
    /// the hold as it starts, so the pass neither starts the node, resyncs the AI accounts nor
    /// begins cloning repositories from the vault.
    #[test]
    fn a_stop_pressed_mid_pass_holds_every_row_after_it() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let stopped = AtomicBool::new(false);
        let held = || {
            stopped
                .load(Ordering::SeqCst)
                .then(|| "a Stop holds it".to_owned())
        };
        let owed = || Ok(OwedClone::Due("the GitHub token is here".to_owned()));
        let clone = || panic!("a Stop before repository cloning must hold it");
        let ports = Hooked {
            inner: Inert::new(scratch.path()),
            held: &held,
            owed: &owed,
            clone: &clone,
            forget: &|| Ok(()),
        };
        let pass = ran(converge(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |row| {
                if row.id == FLEET {
                    stopped.store(true, Ordering::SeqCst);
                }
            },
        ));
        assert!(
            stopped.load(Ordering::SeqCst),
            "fleet registration completed: {:?}",
            pass.rows
        );
        assert_eq!(row(&pass.rows, FLEET).outcome, Outcome::Changed);
        for id in [NODE, ACCOUNTS, CLONE, CLONE_TUNING] {
            let held = row(&pass.rows, id);
            assert_eq!(
                (held.outcome, held.why.as_deref()),
                (Outcome::NotApplicable, Some("a Stop holds it")),
                "{held:?}"
            );
        }
        let after_the_stop: Vec<&str> = ports
            .inner
            .applies()
            .into_iter()
            .filter(|kind| !kind.starts_with("fabric") && *kind != "fleet")
            .collect();
        assert!(
            after_the_stop.is_empty(),
            "after the person's Stop the pass still applied {after_the_stop:?} on the vault"
        );
    }

    /// A Stop pressed after `sync.clone-tuning` read the clones and before it applies holds the
    /// tuning: the apply reads System State's hold on the vault again, as `sync.clone`'s does,
    /// and stands aside with no clone written.
    #[test]
    fn a_stop_pressed_after_the_tuning_row_read_holds_its_apply() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let read = Arc::new(AtomicBool::new(false));
        let held = || {
            read.load(Ordering::SeqCst)
                .then(|| "a Stop holds it".to_owned())
        };
        let ports = Hooked {
            inner: Inert::new(scratch.path()),
            held: &held,
            owed: &|| Ok(OwedClone::Nothing("aterm init owes no clone".into())),
            clone: &|| panic!("nothing is owed"),
            forget: &|| Ok(()),
        };
        *ports.inner.tuning_read.lock().unwrap() = Some(Arc::clone(&read));
        *ports.inner.tuning.lock().unwrap() = crate::clone_tuning::Reading::Due {
            from: "$HOME/trust: feature.manyFiles unset".into(),
            to: "tuned".into(),
        };
        let pass = ran(converge(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |_| {},
        ));
        let tuning = row(&pass.rows, CLONE_TUNING);
        assert_eq!(
            (tuning.outcome, tuning.why.as_deref()),
            (Outcome::Blocked, Some("a Stop holds it")),
            "{tuning:?}"
        );
        assert!(
            !ports.inner.applies().contains(&"tune clones"),
            "{:?}",
            ports.inner.applies()
        );
    }

    /// GitHub repositories the person left as they are in System State after `aterm init` owed
    /// their clone hold `sync.clone` as they hold `aterm init` (principle 4: a group turned off
    /// stays off on every path), with the Secrets vault on: once the vault's token lands, the
    /// pass clones nothing, says why, and forgets the clone `aterm init` owed, so turning the
    /// group on later clones nothing by itself. A plan only says so.
    #[test]
    fn sync_clone_honours_the_developer_switch() {
        const LEFT: &str = "left as you set them in System State: GitHub repositories";
        for mode in [Mode::Apply, Mode::Plan] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let ports = Inert::new(scratch.path());
            *ports.owed.lock().unwrap() = OwedClone::Held(LEFT.to_owned());
            let pass = ran(ports.pass(Trigger::Subscriber, mode));
            assert!(
                !ports.applies().contains(&"clone"),
                "{mode:?}: the pass cloned with GitHub repositories at Leave as is: {:?}",
                ports.applies()
            );
            let clone = row(&pass.rows, CLONE);
            assert_eq!(clone.outcome, Outcome::NotApplicable, "{clone:?}");
            assert!(
                clone
                    .why
                    .as_deref()
                    .is_some_and(|why| why.starts_with(LEFT)),
                "{clone:?}"
            );
            assert_eq!(
                ports.applies().contains(&"forget clone"),
                mode == Mode::Apply,
                "{mode:?}: {:?}",
                ports.applies()
            );
        }
    }

    /// alice's account on a Fake Mac whose console she owns, as `aterm init`'s context reads it.
    pub(crate) fn alice() -> (crate::port::Fake, crate::model::User) {
        alice_with(true)
    }

    /// [`alice`], with Apple's git (`git`) or none.
    fn alice_with(git: bool) -> (crate::port::Fake, crate::model::User) {
        let mut sys = crate::port::Fake::root();
        sys.euid = 501;
        let alice = crate::items::defaults::tests::user("alice", 501);
        sys.logged_in(&alice);
        sys.stat_of(
            "/dev/console",
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        sys.on(
            &crate::facts::hidutil_keyboards().display(),
            crate::port::Ran::ok("Services:\n\nDevices:\n"),
        );
        if git {
            sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        }
        sys.stat_of(
            &alice.home,
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o700,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        (sys, alice)
    }

    /// The record `aterm init` leaves when its GitHub step waits for the vault.
    fn owe_a_clone(state: &Path) {
        let dir = state.join(FileJournal::DIR);
        aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
        aterm_uds::private_file::write_private(&dir, "clone-owed", b"owed\n").unwrap();
    }

    /// Nothing loops: the vault's token landed, but GitHub refuses it, which only a person (a
    /// vault admin) can fix, and the native step's rule is that its row needs them and no pass
    /// retries it. The first pass runs the step once and records that it waits for a person;
    /// later passes say what to do and run nothing until the token or init.toml changes.
    #[test]
    fn a_clone_only_a_person_can_finish_is_not_rerun_every_pass() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let (sys, alice) = alice();
        // This independently configured profile still authenticates with GitHub. The vault
        // default instead reads account and inventory metadata locally.
        let profile = "/Users//alice/.config/aterm/init.toml";
        let profile_bytes = b"[github]\naccount='alice'\n";
        sys.file(profile, profile_bytes).stat_of(
            profile,
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: profile_bytes.len() as u64,
            },
        );
        let token = "/Users//alice/.secrets/gh_access_token";
        sys.file(token, b"fixture_token_only").stat_of(
            token,
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 18,
            },
        );
        sys.fail_github_api(&alice, "/user", crate::port::GithubFailure::Unauthenticated);
        owe_a_clone(scratch.path());
        let state = scratch.path().to_path_buf();
        let held = || None;
        let owed = || crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE);
        let clone = || {
            crate::init::clone_owed(
                &sys,
                &state,
                aterm_secrets::converge::RepositoryWork::Refresh,
            )
        };
        let ports = Hooked {
            inner: Inert::new(scratch.path()),
            held: &held,
            owed: &owed,
            clone: &clone,
            forget: &|| crate::init::forget_owed_clone(&state),
        };
        let mut said = Vec::new();
        for _ in 0..3 {
            let pass = ran(converge(
                &ports,
                Trigger::Subscriber,
                Mode::Apply,
                aterm_secrets::converge::RepositoryWork::Refresh,
                &mut |_| {},
            ));
            let clone_row = row(&pass.rows, CLONE);
            said.push((clone_row.outcome, clone_row.why.clone()));
        }
        let asked = sys
            .calls()
            .iter()
            .filter(|call| call.contains("agh GET /user"))
            .count();
        assert_eq!(
            asked, 1,
            "three passes each re-ran the GitHub step a person must finish: {said:?}"
        );
        let (outcome, why) = &said[2];
        assert_eq!(*outcome, Outcome::Person, "{said:?}");
        assert!(
            why.as_deref()
                .is_some_and(|why| why.contains("Only you can fix this")
                    && why.contains("aterm init --only github")),
            "{said:?}"
        );
        // A new token from the vault is a new chance: the next pass runs the step again.
        sys.file(token, b"fixture_token_next");
        ran(converge(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |_| {},
        ));
        let asked = sys
            .calls()
            .iter()
            .filter(|call| call.contains("agh GET /user"))
            .count();
        assert_eq!(asked, 2, "a new token runs the step again");
    }

    /// A turned-off step is no time bomb: init.toml's `[github] enabled = false` after `aterm
    /// init` owed a clone makes `sync.clone` read off and forget it, so the day the person
    /// removes that line the pass clones nothing by itself.
    #[test]
    fn an_owed_clone_turned_off_is_forgotten() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let (sys, _alice) = alice();
        let config = "/Users//alice/.config/aterm/init.toml";
        let off = b"[github]\nenabled = false\n";
        sys.file(config, off).stat_of(
            config,
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: off.len() as u64,
            },
        );
        owe_a_clone(scratch.path());
        let state = scratch.path().to_path_buf();
        let read = crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE).unwrap();
        assert!(matches!(read, OwedClone::Off(_)), "{read:?}");
        let held = || None;
        let owed = || crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE);
        let clone = || {
            crate::init::clone_owed(
                &sys,
                &state,
                aterm_secrets::converge::RepositoryWork::Refresh,
            )
        };
        let ports = Hooked {
            inner: Inert::new(scratch.path()),
            held: &held,
            owed: &owed,
            clone: &clone,
            forget: &|| crate::init::forget_owed_clone(&state),
        };
        ran(converge(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |_| {},
        ));
        assert!(
            !scratch
                .path()
                .join(FileJournal::DIR)
                .join("clone-owed")
                .exists(),
            "the owed clone outlives its off switch, and fires when the step comes back on"
        );
    }

    /// `aterm init --only github` names GitHub repositories over the person's Leave as is (the
    /// held line's way back, `init::naming`), and its step waits for the vault; init says the
    /// clone runs by itself once this Mac is let in (`init::OWED`). The vault's pass keeps that
    /// promise: the record is stamped with the hold the person's own command went past, so the
    /// pass clones once the token lands. A Stop pressed after that run began holds the clone and
    /// keeps it owed (Set up now goes on); the group left as is after a run that did not go past
    /// that hold is a later choice, and the pass forgets the clone, saying why.
    #[test]
    fn an_owed_clone_the_person_named_over_the_hold_is_kept() {
        use aterm_types::macos_setup::{Selection, Setting};
        let mut off = Selection::none();
        off.set(Setting::Repositories, true);
        let left = crate::init::Holds {
            off,
            ..crate::init::Holds::NONE
        };
        let stopped = crate::init::Holds {
            stopped: Some("you stopped setup in System State".to_owned()),
            ..left.clone()
        };
        // (the hold the run went past, the holds now, cloned, the record kept)
        for (start, now, cloned_now, kept) in [
            (left.clone(), left.clone(), true, true),
            (left.clone(), stopped, false, true),
            (crate::init::Holds::NONE, left.clone(), false, false),
        ] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let (sys, _alice) = alice();
            let token = "/Users//alice/.secrets/gh_access_token";
            sys.file(token, b"fixture_token_only").stat_of(
                token,
                crate::port::Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 18,
                },
            );
            crate::init::owe_clone(scratch.path(), &start).unwrap();
            let state = scratch.path().to_path_buf();
            let cloned = AtomicBool::new(false);
            let held = || None;
            let owed = || crate::init::owed_clone(&sys, &state, &now);
            let clone = || {
                cloned.store(true, Ordering::SeqCst);
                Ok("cloned".to_owned())
            };
            let ports = Hooked {
                inner: Inert::new(scratch.path()),
                held: &held,
                owed: &owed,
                clone: &clone,
                forget: &|| crate::init::forget_owed_clone(&state),
            };
            let pass = ran(converge(
                &ports,
                Trigger::Subscriber,
                Mode::Apply,
                aterm_secrets::converge::RepositoryWork::Refresh,
                &mut |_| {},
            ));
            let row = row(&pass.rows, CLONE);
            let record = scratch.path().join(FileJournal::DIR).join("clone-owed");
            assert_eq!(
                (cloned.load(Ordering::SeqCst), record.exists()),
                (cloned_now, kept),
                "held now as {now:?}: the pass cloned {} and kept the record {}: {row:?}",
                cloned.load(Ordering::SeqCst),
                record.exists()
            );
            if now.stopped.is_some() {
                assert!(
                    row.why.as_deref().is_some_and(
                        |why| why.contains("because you stopped setup in System State")
                    ),
                    "{row:?}"
                );
            }
            if !kept {
                assert!(
                    row.why.as_deref().is_some_and(|why| why
                        .starts_with("left as you set them in System State")
                        && why.ends_with("the clone aterm init owed is forgotten")),
                    "{row:?}"
                );
            }
        }
    }

    /// The token file the vault lands for alice, here now.
    fn token_landed(sys: &crate::port::Fake) {
        let token = "/Users//alice/.secrets/gh_access_token";
        sys.file(token, b"fixture_token_only").stat_of(
            token,
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 18,
            },
        );
    }

    /// Every clone runs Apple's git, which `base.clt` installs and this pass never does: a clone
    /// owed while the Command Line Tools are missing waits for them as for the token, naming
    /// the step that installs them; the pass after they arrive runs it.
    #[test]
    fn the_owed_clone_runs_once_the_command_line_tools_arrive() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().to_path_buf();
        crate::init::owe_clone(&state, &crate::init::Holds::NONE).unwrap();
        let (sys, alice) = alice_with(false);
        token_landed(&sys);
        let inventory = "/Users//alice/.secrets/github-repositories.json";
        let inventory_bytes = br#"{"schema":2,"account":"alice","repositories":[]}"#;
        sys.file(inventory, inventory_bytes).stat_of(
            inventory,
            crate::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: inventory_bytes.len() as u64,
            },
        );
        crate::git_credential::script::launcher(&sys, &alice);
        crate::git_credential::script::lookups_hold(
            &sys,
            &alice,
            crate::git_credential::GITHUB,
            "alice",
            "fixture_token_only",
        );
        crate::git_credential::script::alike(&sys, &alice);
        let held = || None;
        let owed = || crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE);
        let clone = || {
            crate::init::clone_owed(
                &sys,
                &state,
                aterm_secrets::converge::RepositoryWork::Refresh,
            )
        };
        let forget = || crate::init::forget_owed_clone(&state);
        let ports = Hooked {
            inner: Inert::new(&state),
            held: &held,
            owed: &owed,
            clone: &clone,
            forget: &forget,
        };
        let first = ran(converge(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |_| {},
        ));
        let waiting = row(&first.rows, CLONE);
        assert_eq!(waiting.outcome, Outcome::Deferred, "{waiting:?}");
        assert!(!first.complete());
        let asked = || {
            sys.calls()
                .iter()
                .filter(|call| call.contains("credential fill"))
                .count()
        };
        assert_eq!(asked(), 0, "no clone runs without git");
        // The tools arrive (their install); the token and init.toml are as they were.
        sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        let second = ran(converge(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |_| {},
        ));
        assert_eq!(
            row(&second.rows, CLONE).outcome,
            Outcome::Changed,
            "{second:?}"
        );
        assert!(
            asked() > 0,
            "the tools arrived, yet the owed clone never ran"
        );
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
    }

    /// A token file that is on this Mac but that aterm may not use (here readable by others)
    /// leaves the owed clone waiting with what is wrong with that file, never as if no token had
    /// landed. CONTROL: the same file readable by its owner alone is due.
    #[test]
    fn a_token_file_aterm_cannot_use_says_what_is_wrong() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().to_path_buf();
        crate::init::owe_clone(&state, &crate::init::Holds::NONE).unwrap();
        let (sys, _alice) = alice_with(true);
        let token = "/Users//alice/.secrets/gh_access_token";
        let stat = |mode| crate::port::Stat {
            uid: 501,
            gid: 20,
            mode,
            dir: false,
            symlink: false,
            len: 18,
        };
        sys.file(token, b"fixture_token_only")
            .stat_of(token, stat(0o644));
        let read = crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE).unwrap();
        assert!(
            matches!(&read, crate::init::OwedClone::Waiting(why)
                if why.contains(token) && why.contains("cannot be used")),
            "{read:?}"
        );
        sys.stat_of(token, stat(0o600));
        let read = crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE).unwrap();
        assert!(matches!(read, crate::init::OwedClone::Due(_)), "{read:?}");
    }

    /// A Stop pressed once `sync.clone` has read what is owed (its clone may take half an hour)
    /// holds the clone, as System State's own Repositories stage reads the person's choice
    /// before each clone: the apply reads the hold again as it begins.
    #[test]
    fn a_stop_pressed_as_sync_clone_starts_holds_its_clone() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().to_path_buf();
        let stopped = AtomicBool::new(false);
        let cloned = AtomicBool::new(false);
        let held = || {
            stopped
                .load(Ordering::SeqCst)
                .then(|| "you stopped setup in System State".to_owned())
        };
        let owed = || {
            // The person presses Stop while the row reads what is owed.
            stopped.store(true, Ordering::SeqCst);
            Ok(OwedClone::Due(
                "the vault's GitHub token is here now".to_owned(),
            ))
        };
        let clone = || {
            cloned.store(true, Ordering::SeqCst);
            Ok("cloned".to_owned())
        };
        let ports = Hooked {
            inner: Inert::new(&state),
            held: &held,
            owed: &owed,
            clone: &clone,
            forget: &|| Ok(()),
        };
        let pass = ran(converge(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Refresh,
            &mut |_| {},
        ));
        assert!(
            !cloned.load(Ordering::SeqCst),
            "a Stop pressed before sync.clone's clone began did not hold it"
        );
        let held_row = row(&pass.rows, CLONE);
        assert!(
            held_row.outcome != Outcome::Changed
                && held_row
                    .why
                    .as_deref()
                    .is_some_and(|why| why.contains("you stopped setup in System State")),
            "{held_row:?}"
        );
    }
}
