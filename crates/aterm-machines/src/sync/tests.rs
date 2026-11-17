// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The converge pass over inert ports, with private native Git read-back controls.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

mod fleet_retry;
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
    /// The member vault server an earlier aterm installed ([`Ports::stale_vault_server`]), which
    /// its removal takes away, and how often it was read (apart from `reads`, the swarm's ports).
    stale_server: Mutex<Option<String>>,
    stale_reads: AtomicU32,
    shared: Mutex<SharedRead>,
    requests: Mutex<RequestsRead>,
    /// Observe the actual need ledger when the publisher runs, where a test supplies one.
    pass_requests: Option<Box<dyn Fn() -> Result<String, String> + Sync>>,
    /// What a founding of the vault's Commons ends in ([`Ports::found_commons`]).
    founded: Mutex<Result<SharedDone, String>>,
    /// What `sync.shared-data` reads once a founding is done.
    shared_after: Mutex<SharedRead>,
    /// Why `sync.shared-data`'s read fails, where it does.
    shared_fails: Mutex<Option<String>>,
    accounts: Mutex<AccountsRead>,
    release: Mutex<ReleaseRead>,
    github: Mutex<GithubRead>,
    /// What `github` reads once `sync_github` ran, in place of a current route.
    github_after: Mutex<Option<GithubRead>>,
    /// What `sync.clone` reads ([`Ports::owed_clone`]).
    owed: Mutex<crate::init::OwedClone>,
    /// What `sync.refresh`'s apply answers ([`Ports::refresh_repositories`]).
    refreshed: Mutex<Result<crate::init::RepositoryRefreshed, String>>,
    /// What `sync.clone-tuning` reads ([`Ports::clone_tuning`]): every clone as it declares by
    /// default, so the other kinds' tests count none of it.
    tuning: Mutex<crate::clone_tuning::Reading>,
    /// What `sync.clone-tuning`'s apply answers; an `Ok` that stood nowhere aside makes the
    /// read current.
    tuned: Mutex<Result<crate::clone_tuning::Tuned, String>>,
    /// Set once `sync.clone-tuning` is read, where a test watches for it.
    tuning_read: Mutex<Option<Arc<AtomicBool>>>,
    /// What each read of `sync.clone-tuning` was asked to keep, in order.
    tuning_keeps: Mutex<Vec<crate::clone_tuning::Keep>>,
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
    /// Each run handed to another program, and how it ends: an `Err` is a run that could not
    /// start ([`Ports::start_elsewhere`]).
    pub(crate) ran_elsewhere: Mutex<Vec<(PathBuf, Vec<String>)>>,
    pub(crate) elsewhere_exit: Mutex<Result<Option<i32>, String>>,
    /// Where set, the next run handed elsewhere goes on until this gets a message or its sender
    /// is dropped, as an installed app's pass waiting for the lock does; unset, it ends at once.
    pub(crate) elsewhere_release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    /// Where set, the next run handed elsewhere starts and goes on as above, and then the wait
    /// for it fails with this instead of its exit ([`ElsewhereRun`]).
    pub(crate) elsewhere_unwaited: Mutex<Option<String>>,
    refreshed_subscribers: AtomicU32,
    /// Why System State holds the vault out ([`Ports::vault_held`]), and how often it was read.
    held: Mutex<Option<String>>,
    held_reads: AtomicU32,
    /// At each sync kind's read, whether another take of the pass's lock found it held.
    locked_at_reads: Mutex<Vec<bool>>,
    /// At each [`Ports::new_pass`], whether another take of the pass's lock found it held.
    new_passes: Mutex<Vec<bool>>,
    /// The note this runner's starter gave it ([`Ports::start_note`]).
    start_note: Option<String>,
    /// Whether this runner's starter has ended already ([`Ports::watch_starter`]).
    pub(crate) starter_ended: AtomicBool,
    /// Where set, this runner's starter ends the moment a [`Ports::watch_starter`] begins
    /// watching it: the watch is [`StarterWatch::Watching`], and what it was handed has run.
    pub(crate) starter_ends_once_watched: AtomicBool,
    /// What a [`Ports::watch_starter`] was handed to call once the starter ends, while nothing
    /// called it ([`Inert::end_starter`]).
    starter_watch: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// How often the console was read ([`Ports::at_console`]).
    console_reads: AtomicU32,
    /// What each [`Ports::spent`] reads, in order (nothing once they run out), and how often
    /// it was read.
    spent: Mutex<std::collections::VecDeque<Spent>>,
    spent_reads: AtomicU32,
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
            refreshed: Mutex::new(Ok(crate::init::RepositoryRefreshed::Ran(
                "refreshed".into(),
            ))),
            tuning: Mutex::new(crate::clone_tuning::Reading::Current(
                "1 vault clone on this Mac, each as it declares".into(),
            )),
            tuned: Mutex::new(Ok(crate::clone_tuning::Tuned {
                said: "tuned $HOME/trust (index format 2 -> 4)".into(),
                aside: None,
            })),
            tuning_read: Mutex::new(None),
            tuning_keeps: Mutex::new(Vec::new()),
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
            stale_server: Mutex::new(None),
            stale_reads: AtomicU32::new(0),
            shared: Mutex::new(SharedRead::Visible("control record here".to_owned())),
            requests: Mutex::new(RequestsRead::Idle(
                "this Mac is not a Commons Writer, and holds no needs".to_owned(),
            )),
            pass_requests: None,
            founded: Mutex::new(Ok(SharedDone::Done("founded, approving m2".to_owned()))),
            shared_after: Mutex::new(SharedRead::Visible("control record here".to_owned())),
            shared_fails: Mutex::new(None),
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
            elsewhere_release: Mutex::new(None),
            elsewhere_unwaited: Mutex::new(None),
            locked_at_reads: Mutex::new(Vec::new()),
            new_passes: Mutex::new(Vec::new()),
            start_note: None,
            starter_ended: AtomicBool::new(false),
            starter_ends_once_watched: AtomicBool::new(false),
            starter_watch: Mutex::new(None),
            console_reads: AtomicU32::new(0),
            spent: Mutex::new(std::collections::VecDeque::new()),
            spent_reads: AtomicU32::new(0),
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

    /// End this runner's starter as its watch sees it: call what the pass's
    /// [`Ports::watch_starter`] was handed. Whether a pass was watching it.
    pub(crate) fn end_starter(&self) -> bool {
        let Some(ended) = self.starter_watch.lock().unwrap().take() else {
            return false;
        };
        ended();
        true
    }

    /// Whether a pass watches its starter now, as one does while it waits for the lock.
    pub(crate) fn watching_starter(&self) -> bool {
        self.starter_watch.lock().unwrap().is_some()
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
    fn new_pass(&self) {
        // Another take of the pass's lock, only where a pass made its lock file (as `read`).
        let held = self.state.join(FileJournal::DIR).join(LOCK).exists()
            && try_lock(&self.state).is_ok_and(|lock| lock.is_none());
        self.new_passes.lock().unwrap().push(held);
    }
    fn own_account(&self) -> bool {
        self.own
    }
    fn at_console(&self) -> Result<bool, String> {
        self.console_reads.fetch_add(1, Ordering::SeqCst);
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
    fn stale_vault_server(&self) -> Result<Option<String>, String> {
        self.stale_reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.stale_server.lock().unwrap().clone())
    }
    fn retire_stale_vault_server(&self) -> Result<Option<String>, String> {
        self.applied("retire-stale-server");
        Ok(self
            .stale_server
            .lock()
            .unwrap()
            .take()
            .map(|_| "removed systems.alab.aterm-secrets-server.service".to_owned()))
    }
    fn shared_data(&self) -> Result<SharedRead, String> {
        if let Some(why) = self.shared_fails.lock().unwrap().clone() {
            return Err(why);
        }
        self.read(&self.shared)
    }
    fn requests(&self) -> Result<RequestsRead, String> {
        self.read(&self.requests)
    }
    fn pass_requests(&self) -> Result<String, String> {
        self.applied("requests");
        self.pass_requests
            .as_ref()
            .map_or_else(|| Ok("0 posted".to_owned()), |pass| pass())
    }
    fn found_commons(&self) -> Result<SharedDone, String> {
        self.applied("commons");
        let founded = self.founded.lock().unwrap().clone();
        if matches!(founded, Ok(SharedDone::Done(_))) {
            *self.shared.lock().unwrap() = self.shared_after.lock().unwrap().clone();
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
    fn owed_clone(
        &self,
        _readback: Option<&crate::init_github::RepositoryReadback>,
        _work: aterm_secrets::converge::RepositoryWork,
    ) -> Result<crate::init::OwedClone, String> {
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
        _execution: Option<repository_refresh::Execution>,
    ) -> Result<crate::init::CloneOwedOutcome, String> {
        self.applied("clone");
        *self.owed.lock().unwrap() =
            crate::init::OwedClone::Verified(crate::init_observe::RepositoryCounts {
                ready: 1,
                ..crate::init_observe::RepositoryCounts::default()
            });
        Ok(crate::init::CloneOwedOutcome {
            said: "cloned".into(),
            readback: None,
        })
    }
    fn refresh_repositories(
        &self,
        _execution: Option<repository_refresh::Execution>,
    ) -> Result<crate::init::RepositoryRefreshed, String> {
        self.applied("refresh checkouts");
        self.refreshed.lock().unwrap().clone()
    }
    fn forget_owed_clone(&self) -> Result<(), String> {
        self.applied("forget clone");
        *self.owed.lock().unwrap() =
            crate::init::OwedClone::Nothing("aterm init owes no clone".into());
        Ok(())
    }
    fn clone_tuning(
        &self,
        keep: crate::clone_tuning::Keep,
    ) -> Result<crate::clone_tuning::Reading, String> {
        if let Some(read) = self.tuning_read.lock().unwrap().as_ref() {
            read.store(true, Ordering::SeqCst);
        }
        self.tuning_keeps.lock().unwrap().push(keep);
        self.read(&self.tuning)
    }
    fn tune_clones(
        &self,
        _execution: Option<repository_refresh::Execution>,
    ) -> Result<crate::clone_tuning::Tuned, String> {
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
    fn start_note(&self) -> Option<String> {
        self.start_note.clone()
    }
    fn watch_starter(&self, ended: Box<dyn FnOnce() + Send>) -> StarterWatch {
        if self.starter_ended.load(Ordering::SeqCst) {
            return StarterWatch::Ended;
        }
        if self.starter_ends_once_watched.load(Ordering::SeqCst) {
            ended();
            return StarterWatch::Watching(Box::new(()));
        }
        *self.starter_watch.lock().unwrap() = Some(ended);
        StarterWatch::Watching(Box::new(()))
    }
    fn spent(&self) -> Spent {
        self.spent_reads.fetch_add(1, Ordering::SeqCst);
        self.spent.lock().unwrap().pop_front().unwrap_or_default()
    }
    fn installed_release_elsewhere(&self) -> Option<PathBuf> {
        self.elsewhere.clone()
    }
    fn start_elsewhere(&self, program: &Path, args: &[String]) -> Result<ElsewhereRun, String> {
        self.ran_elsewhere
            .lock()
            .unwrap()
            .push((program.to_path_buf(), args.to_vec()));
        let exit = self.elsewhere_exit.lock().unwrap().clone()?;
        let release = self.elsewhere_release.lock().unwrap().take();
        let unwaited = self.elsewhere_unwaited.lock().unwrap().take();
        Ok(Box::new(move || -> Result<Option<i32>, String> {
            if let Some(release) = release {
                let _ = release.recv();
            }
            unwaited.map_or(Ok(exit), Err)
        }))
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

    #[cfg(unix)]
    {
        use aterm_secrets::{client::Paths, need};
        use owed_clone::Hooked;

        // A later local producer may raise its first need while still failing its own work.
        // The one publisher must read that durable row in this pass, without claiming that
        // observing it posts to a real Commons table or completes repository acquisition.
        for (mode, swarm, held) in [
            (Mode::Plan, true, None),
            (Mode::Apply, false, None),
            (Mode::Apply, true, Some("setup is held")),
            (Mode::Apply, true, None),
        ] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(scratch.path());
            let raised = Mutex::new(None);
            let observed = Arc::new(Mutex::new(Vec::new()));
            let clone = || {
                let row = need::raise(
                    &paths,
                    need::Want::Gh(need::GhProfile::Dev),
                    need::Purpose::AghDev,
                )
                .map_err(|why| why.to_string())?;
                assert!(!row.state.ended(), "a cold need remains unfinished");
                *raised.lock().unwrap() = Some(row.id);
                Err("repository acquisition still waits for its credential".into())
            };
            let mut inner = Inert::new(scratch.path());
            *inner.swarm.lock().unwrap() = Ok(swarm);
            *inner.requests.lock().unwrap() =
                RequestsRead::Due("read the local need ledger".into());
            let reading = paths.clone();
            let capture = Arc::clone(&observed);
            inner.pass_requests = Some(Box::new(move || {
                let rows = need::peek(&reading).map_err(|why| why.to_string())?;
                capture
                    .lock()
                    .unwrap()
                    .extend(rows.iter().map(|row| row.id));
                Ok(format!("{} local needs observed", rows.len()))
            }));
            let ports = Hooked {
                inner,
                held: &|| held.map(str::to_owned),
                owed: &|_| {
                    Ok(crate::init::OwedClone::Due(
                        "repository credential needed".into(),
                    ))
                },
                clone: &clone,
                control: None,
                forget: &|| Ok(()),
            };
            let pass = ran(converge(
                &ports,
                Trigger::Subscriber,
                mode,
                aterm_secrets::converge::RepositoryWork::Repair,
                &mut |_| {},
            ));
            if mode == Mode::Apply && swarm && held.is_none() {
                let id = raised.lock().unwrap().expect("the later producer ran");
                assert_eq!(need::peek(&paths).unwrap()[0].id, id, "the need is durable");
                assert_eq!(row(&pass.rows, CLONE).outcome, Outcome::Failed);
                assert!(
                    !pass.complete(),
                    "the publisher cannot complete the producer"
                );
                assert_eq!(
                    *observed.lock().unwrap(),
                    [id],
                    "the same pass's publisher missed the newly raised need"
                );
                assert_eq!(row(&pass.rows, REQUESTS).outcome, Outcome::Changed);
            } else {
                assert!(
                    raised.lock().unwrap().is_none(),
                    "the producer must not run"
                );
                assert!(
                    need::peek(&paths).unwrap().is_empty(),
                    "no durable need was raised"
                );
                assert!(observed.lock().unwrap().is_empty());
                assert!(!ports.inner.applies().contains(&"requests"));
            }
        }
    }
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
/// subscriber's pass once; `sync.refresh` refreshes what is here in each applying pass after it,
/// and the waiting pass refreshes nothing. A plan only plans it.
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
    assert_eq!(row(&again.rows, REFRESH).outcome, Outcome::Ok);
    assert_eq!(clones(&ports), 1, "once");
    assert_eq!(
        ports
            .applies()
            .iter()
            .filter(|kind| matches!(**kind, "clone" | "refresh checkouts"))
            .copied()
            .collect::<Vec<_>>(),
        ["clone", "refresh checkouts", "refresh checkouts"],
        "the clone first, then a refresh in each applying pass"
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

/// Only an applying pass's read of the clones keeps what it read of their declarations
/// ([`crate::clone_tuning::Keep::Declarations`]): a plan's read keeps nothing, so `--check`
/// writes nothing of clone tuning's. NEGATIVE CONTROL: the same pass applying asks its read to
/// keep them.
#[test]
fn only_an_applying_pass_keeps_what_clone_tuning_read() {
    use crate::clone_tuning::Keep;
    for (mode, keep) in [
        (Mode::Plan, Keep::Nothing),
        (Mode::Apply, Keep::Declarations),
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        let pass = ran(ports.pass(Trigger::Subscriber, mode));
        let asked = ports.tuning_keeps.lock().unwrap().clone();
        assert!(
            !asked.is_empty(),
            "{mode:?}: the row reads the clones: {:?}",
            row(&pass.rows, CLONE_TUNING)
        );
        assert!(
            asked.iter().all(|asked| *asked == keep),
            "{mode:?}: {asked:?}"
        );
    }
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

/// A person's pass meets another pass's lock and stops: exit 75's case, with nothing read,
/// applied or recorded, and no starter watched.
#[test]
fn a_held_lock_skips_a_persons_pass() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let held = try_lock(scratch.path()).unwrap().expect("free");
    assert!(
        try_lock(scratch.path()).unwrap().is_none(),
        "a second take waits on none"
    );
    let stopped = ports.pass(Trigger::Init, Mode::Apply);
    assert!(matches!(stopped, Err(Stop::Held)), "{stopped:?}");
    assert_eq!(ports.reads.load(Ordering::SeqCst), 0, "nothing read");
    assert_eq!(ports.console_reads.load(Ordering::SeqCst), 0);
    assert!(
        !ports.watching_starter(),
        "a person's pass watches no starter"
    );
    assert!(ports.bases.lock().unwrap().is_empty());
    assert!(ports.applies().is_empty());
    assert_eq!(read_record(scratch.path()), None, "nothing recorded");
    drop(held);
    ran(ports.pass(Trigger::Init, Mode::Apply));
    assert!(read_record(scratch.path()).is_some(), "once it is let go");
}

/// A pass begins its record of checkout facts once, once it holds its lock
/// ([`Ports::new_pass`]): its rows (`sync.clone`'s stage, then `sync.refresh`'s) share that one
/// record, and a pass that meets another's lock empties nothing under the pass that holds it.
/// NEGATIVE CONTROL: the person's pass that found the lock held began no record at all.
#[test]
fn a_pass_begins_its_record_of_checkout_facts_once_it_holds_its_lock() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let held = try_lock(scratch.path()).unwrap().expect("free");
    let stopped = ports.pass(Trigger::Init, Mode::Apply);
    assert!(matches!(stopped, Err(Stop::Held)), "{stopped:?}");
    assert!(
        ports.new_passes.lock().unwrap().is_empty(),
        "a pass that found the lock held begins no record"
    );
    drop(held);
    ran(ports.pass(Trigger::Init, Mode::Apply));
    assert_eq!(
        *ports.new_passes.lock().unwrap(),
        [true],
        "one record, begun under the pass's lock"
    );
}

/// Until the pass `waiting` runs watches its starter, as it does once it found the lock held and
/// before it waits for it, or has ended: a hang detector of a minute, not a budget.
fn until_waiting<T>(ports: &Inert, waiting: &std::thread::ScopedJoinHandle<'_, T>) {
    const HANG: Duration = Duration::from_secs(60);
    let began = std::time::Instant::now();
    while !(ports.watching_starter() || waiting.is_finished()) {
        assert!(
            began.elapsed() < HANG,
            "hang detector: the pass never waited for the lock"
        );
        aterm_uds::deadline::poll_yield();
    }
}

/// What the scoped thread `running` returned once it ended: a hang detector of a minute, not a
/// budget. Where it has not ended by then, `let_go` first frees what it may still wait for (the
/// lock, an installed app's pass), so the scope's own join of it ends and the test fails rather
/// than hangs.
pub(crate) fn joined<T>(running: std::thread::ScopedJoinHandle<'_, T>, let_go: impl FnOnce()) -> T {
    const HANG: Duration = Duration::from_secs(60);
    let began = std::time::Instant::now();
    while !running.is_finished() {
        if began.elapsed() >= HANG {
            let_go();
            panic!("hang detector: the thread never ended");
        }
        aterm_uds::deadline::poll_yield();
    }
    running.join().unwrap()
}

/// A STARTED RUNNER WAITS FOR A HELD LOCK AND RUNS ONCE IT IS LET GO: a pass a starter started
/// (the subscriber's, the window's, setup's) that finds the lock another process's pass holds
/// reads nothing, not even the console, and records nothing while it is held, with its starter
/// watched; the moment the holder lets it go the same pass takes it and runs to its record, with
/// no `converge held` line and no second start. NEGATIVE CONTROL: a person's pass (`--trigger
/// init`) meeting the same lock stops at once, `Stop::Held`, as every trigger's did before.
#[test]
fn a_started_runner_waits_for_a_held_lock_and_runs_once_it_is_let_go() {
    for trigger in [Trigger::Subscriber, Trigger::Window, Trigger::Setup] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        let dir = scratch.path().join(FileJournal::DIR);
        aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
        let holder =
            aterm_uds::private_file::PrivateLock::take(&dir, LOCK, Duration::ZERO, "the test")
                .expect("the lock is free");
        let pass = std::thread::scope(|scope| {
            let waiting = scope.spawn(|| ports.pass(trigger, Mode::Apply));
            until_waiting(&ports, &waiting);
            assert!(
                !waiting.is_finished(),
                "{trigger:?}'s pass ended on a held lock"
            );
            assert_eq!(
                ports.reads.load(Ordering::SeqCst),
                0,
                "nothing read while held"
            );
            assert_eq!(ports.console_reads.load(Ordering::SeqCst), 0);
            assert_eq!(ports.spent_reads.load(Ordering::SeqCst), 0);
            assert_eq!(
                read_record(scratch.path()),
                None,
                "nothing recorded while held"
            );
            assert!(ports.logged().is_empty(), "{:?}", ports.logged());
            drop(holder);
            waiting.join().unwrap()
        });
        let pass = ran(pass);
        assert!(!pass.rows.is_empty());
        assert!(ports.reads.load(Ordering::SeqCst) > 0);
        assert!(
            read_record(scratch.path()).is_some(),
            "it ran to its record"
        );
        let logged = ports.logged();
        assert!(
            logged.iter().all(|line| !line.starts_with("converge held")),
            "{logged:?}"
        );
        assert!(
            logged
                .iter()
                .any(|line| line.starts_with("converge build=")),
            "{logged:?}"
        );
    }

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    let lock = try_lock(scratch.path()).unwrap().expect("the lock is free");
    assert!(matches!(
        ports.pass(Trigger::Init, Mode::Apply),
        Err(Stop::Held)
    ));
    assert!(!ports.watching_starter());
    drop(lock);
}

/// A WAITING RUNNER WHOSE STARTER ENDED RUNS NOTHING: a started pass waiting for a held lock
/// whose starter ends (a restarted subscriber, a quit window) stops, `Stop::Abandoned`, with
/// nothing read or recorded and one `converge abandoned` line, so it outlives no cancelled lane;
/// a starter gone before the wait begins is not waited on at all. Once let go, a pass whose
/// starter lives runs. NEGATIVE CONTROL: the first test's passes, whose starter does not end,
/// run once the lock is let go.
#[test]
fn a_waiting_runner_whose_starter_ended_runs_nothing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert {
        start_note: Some("c=recheck".to_owned()),
        ..Inert::new(scratch.path())
    };
    let mut lock = Some(try_lock(scratch.path()).unwrap().expect("the lock is free"));
    let stopped = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| ports.pass(Trigger::Subscriber, Mode::Apply));
        until_waiting(&ports, &waiting);
        assert!(ports.end_starter(), "the pass watched its starter");
        // A pass that kept waiting once its starter ended fails here within the minute: the
        // lock is let go, and it runs instead of stopping.
        joined(waiting, || drop(lock.take()))
    });
    assert!(matches!(stopped, Err(Stop::Abandoned)), "{stopped:?}");
    assert_eq!(ports.reads.load(Ordering::SeqCst), 0, "nothing read");
    assert_eq!(ports.console_reads.load(Ordering::SeqCst), 0);
    assert_eq!(read_record(scratch.path()), None, "nothing recorded");
    assert_eq!(
        ports.logged(),
        ["converge abandoned build=0.145.0+gabc trigger=subscriber note=c=recheck"]
    );

    let gone = Inert::new(scratch.path());
    gone.starter_ended.store(true, Ordering::SeqCst);
    assert!(matches!(
        gone.pass(Trigger::Window, Mode::Apply),
        Err(Stop::Abandoned)
    ));
    assert_eq!(
        gone.logged(),
        ["converge abandoned build=0.145.0+gabc trigger=window note=-"]
    );
    drop(lock);
    gone.starter_ended.store(false, Ordering::SeqCst);
    ran(gone.pass(Trigger::Window, Mode::Apply));
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
/// stderr goes nowhere), and a plan that is refused writes none; a pass that ran writes its
/// ledger line and its cost line.
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
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert!(logged[0].starts_with("converge build=0.145.0+gabc trigger=window/new-build"));
    assert!(logged[1].starts_with("converge cost build=0.145.0+gabc trigger=window/new-build"));
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
                assert_eq!(logged.len(), 2, "one pass ledger line and its cost line");
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
    *ports.founded.lock().unwrap() = Ok(SharedDone::Done("founded, approving m2".to_owned()));
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

/// A founding that meets the same answer at the next pass until a person changes something (the
/// vault server refused it) waits for that person, naming why and `aterm secrets commons init`,
/// and promises no retry; a founding that may pass by itself fails the row and says the next
/// pass tries again, naming the same command.
/// NEGATIVE CONTROL: an apply that reads every failure as passing promises the refused founding
/// a retry; a row that names no remedy leaves the person with none.
#[test]
fn a_refused_founding_waits_for_a_person_and_promises_no_retry() {
    let init = Some(Remedy::Command("aterm secrets commons init".to_owned()));
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.shared.lock().unwrap() = SharedRead::Foundable("vault alab has none".to_owned());
    *ports.founded.lock().unwrap() = Ok(SharedDone::Waiting(
        "publishing answered 403: the vault server refused it".to_owned(),
    ));
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let shared = row(&pass.rows, SHARED_DATA);
    assert_eq!(shared.outcome, Outcome::Blocked, "{shared:?}");
    assert_eq!(
        shared.stood_aside,
        Some(StoodAside::Waiting(
            "publishing answered 403: the vault server refused it".to_owned()
        )),
        "{shared:?}"
    );
    let why = shared.why.as_deref().unwrap_or_default();
    assert!(
        why.contains("answered 403") && !why.contains("the next pass tries again"),
        "{shared:?}"
    );
    assert_eq!(
        shared
            .explain
            .as_ref()
            .map(|explain| explain.remedy.clone()),
        init,
        "{shared:?}"
    );

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.shared.lock().unwrap() = SharedRead::Foundable("vault alab has none".to_owned());
    *ports.founded.lock().unwrap() = Err("another publish landed first".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let shared = row(&pass.rows, SHARED_DATA);
    assert_eq!(shared.outcome, Outcome::Failed, "{shared:?}");
    assert_eq!(
        shared.why.as_deref(),
        Some("another publish landed first; the next pass tries again"),
        "{shared:?}"
    );
    assert_eq!(
        shared
            .explain
            .as_ref()
            .map(|explain| explain.remedy.clone()),
        init,
        "{shared:?}"
    );
}

/// A founding read back while the vault's state stays busy is no failure: the row keeps the
/// apply's report, unconfirmed, and the next pass reads the settled state. A busy first read
/// founds nothing and fails nothing.
/// NEGATIVE CONTROL: a busy read-back read as not applicable fails the founding that landed.
#[test]
fn a_busy_read_back_leaves_the_founding_unconfirmed_not_failed() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.shared.lock().unwrap() = SharedRead::Foundable("vault alab has none".to_owned());
    *ports.shared_after.lock().unwrap() =
        SharedRead::Busy("the vault's state is changing".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let shared = row(&pass.rows, SHARED_DATA);
    assert_eq!(shared.outcome, Outcome::Changed, "{shared:?}");
    assert_eq!(
        shared.to.as_deref(),
        Some("founded, approving m2; unconfirmed: the vault's state is changing"),
        "{shared:?}"
    );

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.shared.lock().unwrap() = SharedRead::Busy("the vault's state is changing".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let shared = row(&pass.rows, SHARED_DATA);
    assert_eq!(shared.outcome, Outcome::NotApplicable, "{shared:?}");
    assert!(!pass.rows.iter().any(|row| row.outcome.is_failure()));
    assert!(!ports.applies().contains(&"commons"));
}

/// A `sync.shared-data` read that failed, which a member's can as well as an admin's (its
/// `[commons]` config unreadable, say), names no command, since `aterm secrets commons init`
/// fails on a member; a founding that waits, which only the vault's only admin reads, names it.
/// NEGATIVE CONTROL: a failed read that names `commons init` points a member at a command that
/// fails for it.
#[test]
fn a_failed_commons_read_names_no_founding_command() {
    let remedy = |shared: &ItemReport| {
        shared
            .explain
            .as_ref()
            .map(|explain| explain.remedy.clone())
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.shared_fails.lock().unwrap() = Some("aterm.toml: [commons] is not a table".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let shared = row(&pass.rows, SHARED_DATA);
    assert_eq!(shared.outcome, Outcome::Unknown, "{shared:?}");
    assert_eq!(
        shared.why.as_deref(),
        Some("aterm.toml: [commons] is not a table"),
        "{shared:?}"
    );
    assert_eq!(remedy(shared), Some(Remedy::None), "{shared:?}");
    assert!(!ports.applies().contains(&"commons"));

    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.shared.lock().unwrap() =
        SharedRead::Deferred("the LAN broker is not installed".to_owned());
    let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
    let shared = row(&pass.rows, SHARED_DATA);
    assert_eq!(shared.outcome, Outcome::Deferred, "{shared:?}");
    assert_eq!(
        remedy(shared),
        Some(Remedy::Command("aterm secrets commons init".to_owned())),
        "{shared:?}"
    );
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

/// The lane's own ids ([`own_item`]) are its kinds' rows and no others: every item a pass
/// composes of a [`Kind`] carries one, and the base layer's stand-in row, the repository
/// prerequisite's row beside them, the family and an id of the same spelling do not.
/// NEGATIVE CONTROL: with `own_item` passing any id that starts `sync.`, the prerequisite's
/// row and `sync.dock` are passed.
#[test]
fn the_lanes_own_ids_are_its_kinds_rows_and_no_others() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut ports = Inert::new(scratch.path());
    let packages = repository_prerequisites::Packages::default();
    ports.prerequisites = Some(Arc::new(packages));
    let items = sync_items(
        &ports,
        Trigger::Subscriber,
        Mode::Plan,
        aterm_secrets::converge::RepositoryWork::Refresh,
        None,
    );
    let ids: Vec<String> = items
        .local
        .iter()
        .chain(&items.repositories)
        .chain(&items.publication)
        .map(|item| item.id())
        .collect();
    let (own, others): (Vec<&String>, Vec<&String>) = ids.iter().partition(|id| own_item(id));
    assert_eq!(own.len(), Kind::ALL.len(), "{ids:?}");
    assert_eq!(others, ["sync.repository-prerequisites"], "{ids:?}");
    for id in [
        BASE,
        FAMILY,
        "sync.",
        "sync.dock",
        "sync.requests@alice",
        "",
    ] {
        assert!(!own_item(id), "{id:?}");
    }
}

/// The subscriber's pass, which runs with no window (after a restart, say), starts the mug when
/// none runs, and reads it back held; a mug that runs and asks
/// macOS again itself is left to it, a failed start fails the row for the next pass, and a Mac
/// window's own passes leave it to the window's watch. Elsewhere no window keeps a mug, so its
/// passes say what the port says (aCaffeine is macOS only), not that the window keeps one.
/// NEGATIVE CONTROL: with `Kind::Awake` out of `sync_items`, no `sync.awake` row is found; with
/// the window's arm on every OS, a window's pass off a Mac says the window keeps it awake.
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
        let off_a_mac = "aCaffeine is macOS only";
        *ports.awake.lock().unwrap() = if cfg!(target_os = "macos") {
            AwakeRead::Due("no aCaffeine mug runs".to_owned())
        } else {
            AwakeRead::NotApplicable(off_a_mac.to_owned())
        };
        let pass = ran(ports.pass(trigger, Mode::Apply));
        let read = row(&pass.rows, AWAKE);
        assert_eq!(read.outcome, Outcome::NotApplicable);
        if !cfg!(target_os = "macos") {
            assert_eq!(read.why.as_deref(), Some(off_a_mac), "{trigger:?}");
        }
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
/// a plan (`--check`) writes nothing and a Mac window's pass leaves retirement to the window's
/// tidy (off a Mac no window tidies one, and no such login item is there to retire).
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

    // 4. NEGATIVE CONTROL: on a Mac, `Trigger::Window` leaves retirement to the window's own
    //    `TIDY`.
    if cfg!(target_os = "macos") {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        *ports.awake_retired.lock().unwrap() = vec!["com.aterm.aterm.caffeine".to_owned()];
        let in_win = ran(ports.pass(Trigger::Window, Mode::Apply));
        assert_eq!(row(&in_win.rows, AWAKE).outcome, Outcome::NotApplicable);
        assert!(!ports.applies().contains(&"awake-retire"));
    }
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

/// A row as the engine reports it, for [`cost::line`]'s own laws: `id` ended `outcome` after
/// `elapsed_ms`.
fn timed_row(id: &str, outcome: Outcome, elapsed_ms: u64, why: Option<&str>) -> ItemReport {
    ItemReport {
        id: id.to_owned(),
        family: "test".to_owned(),
        title: id.to_owned(),
        outcome,
        from: None,
        to: None,
        why: why.map(str::to_owned),
        revert: None,
        effect: aterm_converge::Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        waits_on: None,
        essential: false,
        elapsed_ms: Some(elapsed_ms),
    }
}

#[test]
fn a_standing_row_parks_only_when_every_incomplete_row_has_a_standing_cause() {
    let mut source = timed_row("source", Outcome::Person, 0, Some("checkout preserved"));
    source.class = Some(aterm_converge::Class::Install);
    let mut inherited = timed_row("dependent", Outcome::Blocked, 0, Some("waiting"));
    inherited.class = source.class;
    inherited.waits_on = Some(source.id.clone());
    let mut pass = Pass {
        due: Due::NotDue,
        rows: vec![source, inherited],
        unrecorded: None,
    };
    assert!(!pass.complete());
    assert!(
        pass.waits_for_input(),
        "inherited waits use their original cause"
    );
    let mut transient = timed_row("transient", Outcome::Failed, 0, Some("unclassified"));
    transient.class = Some(aterm_converge::Class::Install);
    pass.rows.push(transient);
    assert!(
        !pass.waits_for_input(),
        "one transient failure keeps the retry"
    );
    pass.rows.pop();
    pass.unrecorded = Some("record write failed".into());
    assert!(
        !pass.waits_for_input(),
        "an unwritten record still needs a retry"
    );
    pass.unrecorded = None;
    pass.rows[1].waits_on = Some("missing source".into());
    assert!(!pass.waits_for_input(), "an unknown cause is not standing");
    pass.rows.clear();
    assert!(
        !pass.waits_for_input(),
        "no work does not invent a standing cause"
    );
}

/// A count of gits by subcommand, for [`cost::line`]'s own laws: each `(word, n)` of
/// [`crate::port::GIT_WORDS`] counted `n` times.
fn git_by(counts: &[(&str, u64)]) -> crate::port::GitBy {
    let mut by = crate::port::GitBy::default();
    for (word, count) in counts {
        let at = crate::port::GIT_WORDS
            .iter()
            .position(|known| known == word)
            .unwrap();
        by.0[at] = *count;
    }
    by
}

/// THE COST LINE SAYS WHAT THE PASS SPENT, THE NOTE OF ITS START AND EVERY CHANGED ROW: under a
/// head of its own (`converge cost build=… trigger=…/…`) it carries `ms=`, `spawns=`, `git=`,
/// `child_cpu_ms=`, `cpu_ms=`, `note=`, `changed_rows=` (every Changed row's id, in run order),
/// `slow=` (each row the engine spent at least [`cost::SLOW_MS`] on, longest first, then the
/// rest summed) and `git_by=` (the gits by subcommand, the most first, the order of
/// [`crate::port::GIT_WORDS`] between equals), and nothing a row read: a row id that is not
/// plain is written as its index, and no why reaches the line.
/// NEGATIVE CONTROL: a `changed_rows` that names only the first Changed row reads `sync.fleet`
/// alone.
#[test]
fn the_cost_line_says_what_the_pass_spent_its_note_and_every_changed_row() {
    let rows = [
        timed_row("sync.fleet", Outcome::Changed, 400, None),
        timed_row("sync.vault", Outcome::Ok, 100, None),
        timed_row(
            "sync.identity",
            Outcome::Failed,
            2000,
            Some("x ms=9 spawns=9 [y]"),
        ),
        timed_row("sync.clone", Outcome::Changed, 1840, None),
        timed_row("sync.requests", Outcome::Changed, 20, None),
        timed_row("keyboard.caps@Alice Smith", Outcome::Changed, 250, None),
    ];
    let spent = Spent {
        mono_ms: 1200,
        spawns: 5,
        gits: 4,
        git_by: git_by(&[("fetch", 1), ("rev-parse", 1), ("status", 2)]),
        child_cpu_ms: 240,
        cpu_ms: 25,
    };
    let line = cost::line(
        "0.145.0+gabc",
        Trigger::Subscriber,
        Due::Recheck,
        spent,
        Some("c=recheck+retry-failed"),
        &rows,
    );
    assert_eq!(
        line,
        "converge cost build=0.145.0+gabc trigger=subscriber/recheck ms=1200 spawns=5 git=4 \
         child_cpu_ms=240 cpu_ms=25 note=c=recheck+retry-failed \
         changed_rows=sync.fleet,sync.clone,sync.requests,#5 \
         slow=sync.identity:2000,sync.clone:1840,sync.fleet:400,#5:250,rest:120 \
         git_by=status:2,rev-parse:1,fetch:1"
    );
    assert!(!line.contains("Alice"), "no name reaches the line: {line}");
    assert!(
        !line.contains("converge build="),
        "a tally of pass lines does not count it"
    );

    // More slow rows than the line names: the eight longest, run order between equals, and the
    // rest summed; a pass with nothing changed or slow, and a start with no note, write `-`.
    let many: Vec<ItemReport> = (0..10)
        .map(|n| timed_row(&format!("sync.r{n}"), Outcome::Ok, 300, None))
        .collect();
    let line = cost::line(
        "b",
        Trigger::Window,
        Due::Recheck,
        Spent::default(),
        None,
        &many,
    );
    assert!(
        line.ends_with(
            " note=- changed_rows=- slow=sync.r0:300,sync.r1:300,sync.r2:300,sync.r3:300,\
             sync.r4:300,sync.r5:300,sync.r6:300,sync.r7:300,rest:600 git_by=-"
        ),
        "{line}"
    );
    let quiet = cost::line(
        "b",
        Trigger::Window,
        Due::Recheck,
        Spent::default(),
        None,
        &[timed_row("sync.vault", Outcome::Ok, 0, None)],
    );
    assert_eq!(
        quiet,
        "converge cost build=b trigger=window/recheck ms=0 spawns=0 git=0 child_cpu_ms=0 \
         cpu_ms=0 note=- changed_rows=- slow=- git_by=-"
    );
}

/// A COST LINE IS ONE LOG RECORD, WHOLE: aterm.log keeps at most [`aterm_log::MAX_RECORD_BYTES`]
/// of an INFO record, so where the whole line would be longer, `git_by` first names the most
/// counted subcommands that fit and then `rest:<n>` for the others; where it names none,
/// `changed_rows` names the first Changed rows that fit and then `+<n>` for the rest, the most
/// that fit and in run order; where naming none still does not fit, `slow` gives up its shortest
/// rows to `rest`. The head, every number and the note stay whole, and at their largest are 438
/// bytes beside the build's name.
/// NEGATIVE CONTROL: a line whose lists are never cut is 1202 bytes, which the log would cut
/// inside `changed_rows`.
#[test]
fn a_cost_line_fits_one_log_record_cutting_its_lists_from_their_ends() {
    const CAP: usize = aterm_log::MAX_RECORD_BYTES;
    let note = format!("c={}", "a".repeat(238));
    let largest = Spent {
        mono_ms: u64::MAX,
        spawns: u64::MAX,
        gits: u64::MAX,
        git_by: crate::port::GitBy::default(),
        child_cpu_ms: u64::MAX,
        cpu_ms: u64::MAX,
    };
    let bare = cost::line(
        "",
        Trigger::Subscriber,
        Due::NetworkMoved,
        largest,
        Some(&note),
        &[],
    );
    assert_eq!(
        bare.len() - " changed_rows=- slow=- git_by=-".len(),
        438,
        "the head, the numbers and the note at their largest: {bare}"
    );

    // Gits of many subcommands: `git_by` gives up its least counted first, their sum `rest`.
    let mut many = largest;
    many.git_by = git_by(&[
        ("rev-parse", 50),
        ("config", 40),
        ("for-each-ref", 30),
        ("status", 20),
        ("fetch", 10),
        ("merge", 5),
    ]);
    let cut = cost::line(
        "",
        Trigger::Subscriber,
        Due::NetworkMoved,
        many,
        Some(&note),
        &[],
    );
    assert_eq!(
        cut,
        format!(
            "{} git_by=rev-parse:50,config:40,rest:65",
            bare.strip_suffix(" git_by=-").unwrap()
        )
    );
    assert!(cut.len() <= CAP, "{} bytes: {cut}", cut.len());
    let one_more = cut.replacen(",rest:65", ",for-each-ref:30,rest:35", 1);
    assert!(one_more.len() > CAP, "the most that fit: {one_more}");

    // Forty Changed rows, each slow: the ids are cut from the end, the slow rows kept.
    let rows: Vec<ItemReport> = (0..40)
        .map(|n| {
            timed_row(
                &format!("sync.changed-row-{n:02}"),
                Outcome::Changed,
                300 + n,
                None,
            )
        })
        .collect();
    let spent = Spent {
        mono_ms: 61_000,
        spawns: 120,
        gits: 97,
        git_by: crate::port::GitBy::default(),
        child_cpu_ms: 9_000,
        cpu_ms: 800,
    };
    let note = "c=input-repair+retry-unfinished,m=git-dir+index,n=1";
    let line = cost::line(
        "0.201.0+g9c554c6",
        Trigger::Window,
        Due::NotDue,
        spent,
        Some(note),
        &rows,
    );
    assert!(line.len() <= CAP, "{} bytes: {line}", line.len());
    let head = format!(
        "converge cost build=0.201.0+g9c554c6 trigger=window/not-due ms=61000 spawns=120 git=97 \
         child_cpu_ms=9000 cpu_ms=800 note={note} changed_rows="
    );
    assert!(line.starts_with(&head), "{line}");
    let (changed, slow) = line[head.len()..].split_once(" slow=").unwrap();
    let slow = slow.strip_suffix(" git_by=-").unwrap();
    let changed: Vec<&str> = changed.split(',').collect();
    let (left, named) = changed.split_last().unwrap();
    let left: usize = left.strip_prefix('+').unwrap().parse().unwrap();
    assert!(!named.is_empty(), "{line}");
    assert_eq!(named.len() + left, rows.len());
    for (word, row) in named.iter().zip(&rows) {
        assert_eq!(*word, row.id, "the first rows, in run order");
    }
    // The most that fit: one more id (and one fewer left out) passes the cap.
    let one_more = if left > 1 {
        format!(",{},+{}", rows[named.len()].id, left - 1)
    } else {
        format!(",{}", rows[named.len()].id)
    };
    let more = line.replacen(&format!(",+{left}"), &one_more, 1);
    assert!(more.len() > CAP, "{} bytes: {more}", more.len());
    let slowest: Vec<String> = (32..40)
        .rev()
        .map(|n| format!("{}:{}", rows[n].id, 300 + n))
        .collect();
    let rest: u64 = (0..32).map(|n| 300 + n).sum();
    assert_eq!(slow, format!("{},rest:{rest}", slowest.join(",")));

    // Longer ids: naming none of the Changed rows still does not fit, so `slow` gives up its
    // shortest rows to `rest` until the line does.
    let long: Vec<ItemReport> = (0..40)
        .map(|n| {
            timed_row(
                &format!("sync.a-changed-row-with-a-long-id-{n:02}"),
                Outcome::Changed,
                300 + n,
                None,
            )
        })
        .collect();
    let note = format!("c={}", "a".repeat(238));
    let line = cost::line(
        "0.201.0+g9c554c6",
        Trigger::Subscriber,
        Due::NetworkMoved,
        spent,
        Some(&note),
        &long,
    );
    assert!(line.len() <= CAP, "{} bytes: {line}", line.len());
    let (before, slow) = line.split_once(" slow=").unwrap();
    let slow = slow.strip_suffix(" git_by=-").unwrap();
    assert!(
        before.ends_with(&format!(" note={note} changed_rows=+40")),
        "{line}"
    );
    let slow: Vec<&str> = slow.split(',').collect();
    let (rest, kept) = slow.split_last().unwrap();
    assert!(!kept.is_empty() && kept.len() < cost::SLOW_ROWS, "{line}");
    for (word, n) in kept.iter().zip((32..40).rev()) {
        assert_eq!(*word, format!("{}:{}", long[n].id, 300 + n), "the longest");
    }
    let named: u64 = (0..kept.len()).map(|k| 300 + 39 - k as u64).sum();
    let total: u64 = (0..40).map(|n| 300 + n).sum();
    assert_eq!(*rest, format!("rest:{}", total - named));
}

/// A PASS'S LEDGER LINE IS THE BYTES IT WAS, AND ITS COST HAS A LINE OF ITS OWN THAT SAYS WHAT
/// WAS SPENT BETWEEN ITS TWO READINGS: a pass that ran logs its ledger line ([`ledger`]: the
/// head, the tally and the whys, nothing more), then its cost line ([`cost::line`]), which
/// carries the difference of the [`Ports::spent`] read once it holds the lock and once as it
/// ends, and the rows it reported Changed. A pass that finds the lock held, and a plan, read
/// nothing and log no ledger or cost line.
/// NEGATIVE CONTROLS: the cost fields written on the ledger line, before its whys (as they
/// first were), make the first line `… failed=1 ms=1200 spawns=3 … slow=rest:7 [base.person: …`
/// where [`ledger`]'s has no field between `failed=1` and the bracket; a pass that logs its
/// second reading whole reads `spawns=4`.
#[test]
fn a_pass_logs_its_ledger_line_as_it_was_and_what_it_spent_in_a_line_of_its_own() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert::new(scratch.path());
    *ports.fleet_done.lock().unwrap() = Err("the issuer did not answer".to_owned());
    *ports.spent.lock().unwrap() = [
        Spent {
            mono_ms: 100,
            spawns: 1,
            gits: 1,
            git_by: crate::port::GitBy::default(),
            child_cpu_ms: 10,
            cpu_ms: 5,
        },
        Spent {
            mono_ms: 1300,
            spawns: 4,
            gits: 3,
            git_by: crate::port::GitBy::default(),
            child_cpu_ms: 250,
            cpu_ms: 30,
        },
    ]
    .into();
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(ports.spent_reads.load(Ordering::SeqCst), 2);
    let logged = ports.logged();
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(
        logged[0],
        ledger(
            "0.145.0+gabc",
            Trigger::Subscriber,
            pass.due,
            &pass.rows,
            None
        )
    );
    assert!(
        logged[0].ends_with(" | sync.fleet: the issuer did not answer; the next pass tries again]"),
        "the whys end the ledger line: {logged:?}"
    );
    let changed: Vec<&str> = pass
        .rows
        .iter()
        .filter(|row| row.outcome == Outcome::Changed)
        .map(|row| row.id.as_str())
        .collect();
    assert!(changed.len() > 1, "more than one row changed: {changed:?}");
    assert!(
        logged[1].starts_with(&format!(
            "converge cost build=0.145.0+gabc trigger=subscriber/{} ms=1200 spawns=3 git=2 \
             child_cpu_ms=240 cpu_ms=25 note=- changed_rows={} slow=",
            pass.due.word(),
            changed.join(",")
        )),
        "{logged:?}"
    );

    let held = Inert::new(scratch.path());
    let lock = try_lock(scratch.path()).unwrap().expect("the lock is free");
    assert!(matches!(
        held.pass(Trigger::Init, Mode::Apply),
        Err(Stop::Held)
    ));
    drop(lock);
    ran(held.pass(Trigger::Window, Mode::Plan));
    assert_eq!(
        held.spent_reads.load(Ordering::SeqCst),
        0,
        "a held pass and a plan read nothing"
    );
    assert_eq!(held.logged().len(), 1, "the held start's own line alone");
}

/// EACH START SAYS WHAT CAUSED IT, AND A PERSON'S PASS THAT FOUND THE LOCK HELD IS NOT
/// COUNTED AS A PASS: a pass that ran carries its starter's note as its cost line's `note=`
/// (`-` with none); a person's pass that found the lock held writes one line of its own, whose
/// head is `converge held` and never a pass line's `converge build=`, with its note; a plan
/// writes none. NEGATIVE CONTROL: a held pass logged under the pass line's head (`converge
/// build=… held`) is counted by the tally of pass lines below: 3 where 1 is wanted.
#[test]
fn a_start_says_its_note_and_a_held_pass_has_a_line_of_its_own() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = Inert {
        start_note: Some("c=recheck+input-repair,m=head,n=2".to_owned()),
        ..Inert::new(scratch.path())
    };
    let lock = try_lock(scratch.path()).unwrap().expect("the lock is free");
    for _ in 0..2 {
        assert!(matches!(
            ports.pass(Trigger::Init, Mode::Apply),
            Err(Stop::Held)
        ));
    }
    assert!(ports.pass(Trigger::Subscriber, Mode::Plan).is_ok());
    drop(lock);
    ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    let logged = ports.logged();
    let held =
        "converge held build=0.145.0+gabc trigger=init note=c=recheck+input-repair,m=head,n=2";
    let passes = logged
        .iter()
        .filter(|line| line.contains("converge build="))
        .count();
    assert_eq!(passes, 1, "a tally of pass lines counts no held pass");
    assert_eq!(logged.len(), 4, "{logged:?}");
    assert_eq!(logged[..2], [held, held]);
    assert!(logged[2].starts_with("converge build="), "{logged:?}");
    assert!(
        logged[3].contains(" note=c=recheck+input-repair,m=head,n=2 changed_rows="),
        "{logged:?}"
    );

    let unnoted = Inert::new(scratch.path());
    let lock = try_lock(scratch.path()).unwrap().expect("the lock is free");
    assert!(matches!(
        unnoted.pass(Trigger::Init, Mode::Apply),
        Err(Stop::Held)
    ));
    drop(lock);
    ran(unnoted.pass(Trigger::Window, Mode::Apply));
    let logged = unnoted.logged();
    assert_eq!(
        logged[0],
        "converge held build=0.145.0+gabc trigger=init note=-"
    );
    assert!(logged[2].contains(" note=- changed_rows="), "{logged:?}");
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
/// reading none of the individual sync ports (aterm's own stale vault server's read apart,
/// [`Ports::stale_vault_server`]) and applying nothing; an invalid `[swarm]` section fails every
/// sync row rather than defaulting on.
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
            *ports.fleet_done.lock().unwrap() = Err("issuer at capacity".into());
            let pass = ran(ports.pass(Trigger::Window, Mode::Apply));
            assert_eq!(row(&pass.rows, FABRIC).outcome, Outcome::Changed);
            assert_eq!(row(&pass.rows, FLEET).outcome, Outcome::Failed);
            assert_eq!(row(&pass.rows, NODE).outcome, Outcome::Changed);
            let calls = ports.dispatches.lock().unwrap();
            let at = |kind| calls.iter().position(|call| *call == kind).unwrap();
            if needs_node {
                assert!(at("node") < at(effect), "{calls:?}");
                assert!(at("fleet") < at(effect), "{calls:?}");
            } else {
                assert!(at(effect) < at("peers-read"), "{calls:?}");
                assert!(at(effect) < at("fleet"), "{calls:?}");
            }
        }
    }
}

/// A fleet join is tried at once: a join that did not land
/// (an unreachable or busy issuer included) ends its row. A pass with no long repository work
/// finishes for its starter's retry; repository acquisition is still allowed to progress.
#[test]
fn a_fleet_join_starts_at_once_and_its_failure_does_not_withhold_cloning() {
    for trigger in [
        Trigger::Init,
        Trigger::Subscriber,
        Trigger::Window,
        Trigger::Setup,
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        let pass = ran(ports.pass(trigger, Mode::Apply));
        assert_eq!(row(&pass.rows, FLEET).outcome, Outcome::Changed);
    }
    for why in [
        "fleet listener at 10.0.0.1:7100 closed during handshake",
        "the issuer did not answer",
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let ports = Inert::new(scratch.path());
        *ports.fleet_done.lock().unwrap() = Err(why.to_owned());
        let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
        let fleet = row(&pass.rows, FLEET);
        assert_eq!(fleet.outcome, Outcome::Failed);
        assert_eq!(
            fleet.why.as_deref(),
            Some(format!("{why}; the next pass tries again").as_str())
        );
        // The rows after it still run: a fleet that did not land holds no clone back.
        assert_ne!(row(&pass.rows, CLONE).outcome, Outcome::Unknown);
    }
}

/// C18 (§12.3): a simultaneous burst of 35 subscribers
/// against an issuer with 16 connection slots all join within ten minutes. The retry that lands
/// the rest is the starters' (`aterm_secrets::converge::RunnerRetry`, the pacing every starter
/// shares) after a pass whose join failed, never a wait inside the pass: each subscriber here runs
/// whole passes on a virtual clock, and starts the next one when its runner retry comes due.
#[test]
fn thirty_five_joins_against_sixteen_connections_all_land_within_ten_minutes() {
    const SUBSCRIBERS: usize = 35;
    const MAX_CONNECTIONS: usize = 16;
    const CONN_HOLD_MS: u64 = 5_000;
    const BUDGET_MS: u64 = 10 * 60 * 1000;

    struct Issuer {
        now_ms: AtomicU64,
        slot_free_at_ms: Mutex<Vec<u64>>,
    }

    /// One subscriber's runner: an [`Inert`] Mac whose fleet join dials the shared issuer.
    struct Subscriber {
        inner: Inert,
        issuer: Arc<Issuer>,
        joined: AtomicBool,
    }

    impl Subscriber {
        fn join(&self) -> Result<FleetDone, String> {
            let now = self.issuer.now_ms.load(Ordering::SeqCst);
            let mut slots = self.issuer.slot_free_at_ms.lock().unwrap();
            slots.retain(|&free_at| free_at > now);
            if slots.len() >= MAX_CONNECTIONS {
                return Err("fleet listener at 10.0.0.1:7100 closed during handshake".to_owned());
            }
            slots.push(now + CONN_HOLD_MS);
            self.joined.store(true, Ordering::SeqCst);
            Ok(FleetDone::Synced("joined".to_owned()))
        }

        fn pass(&self) -> Outcome {
            // Only a Mac in no fleet is started here, so its pass dials the issuer once.
            *self.inner.fleet_done.lock().unwrap() = self.join();
            row(
                &ran(self.inner.pass(Trigger::Subscriber, Mode::Apply)).rows,
                FLEET,
            )
            .outcome
        }
    }

    /// Run the burst: every subscriber's first pass at t = 0, then, with `retry`, each pass the
    /// starter's runner retry starts. Returns how many joined and when the last of them did.
    fn burst(retry: bool) -> (usize, u64) {
        let issuer = Arc::new(Issuer {
            now_ms: AtomicU64::new(0),
            slot_free_at_ms: Mutex::new(Vec::new()),
        });
        let scratch = aterm_tempfile::tempdir().unwrap();
        let subscribers: Vec<_> = (0..SUBSCRIBERS)
            .map(|idx| {
                let dir = scratch.path().join(format!("m{}", idx + 1));
                std::fs::create_dir_all(&dir).unwrap();
                let inner = Inert::new(&dir);
                *inner.fleet.lock().unwrap() = FleetRead::Due("this Mac is in no fleet".into());
                Subscriber {
                    inner,
                    issuer: Arc::clone(&issuer),
                    joined: AtomicBool::new(false),
                }
            })
            .collect();
        let mut retries: Vec<_> = (0..SUBSCRIBERS)
            .map(|_| aterm_secrets::converge::RunnerRetry::default())
            .collect();
        let mut owed: Vec<bool> = vec![true; SUBSCRIBERS];
        let mut last_join = 0;
        loop {
            let now = issuer.now_ms.load(Ordering::SeqCst);
            for (idx, subscriber) in subscribers.iter().enumerate() {
                if !owed[idx] {
                    continue;
                }
                retries[idx].started();
                let outcome = subscriber.pass();
                let last = if outcome == Outcome::Failed {
                    aterm_secrets::converge::Last::Failed
                } else {
                    assert_eq!(outcome, Outcome::Changed);
                    last_join = now;
                    aterm_secrets::converge::Last::Ended
                };
                retries[idx].reaped(last, now);
                owed[idx] = false;
            }
            if !retry {
                break;
            }
            let Some(next) = retries.iter().filter_map(|retry| retry.remaining(0)).min() else {
                break;
            };
            let next = u64::try_from(next.as_millis()).unwrap();
            assert!(next > now, "a runner retry is never due at once");
            issuer.now_ms.store(next, Ordering::SeqCst);
            for (idx, retry) in retries.iter().enumerate() {
                owed[idx] = retry.due(next);
            }
        }
        let joined = subscribers
            .iter()
            .filter(|subscriber| subscriber.joined.load(Ordering::SeqCst))
            .count();
        (joined, last_join)
    }

    // NEGATIVE CONTROL: one pass each, with no retry after it, lands only the 16 the issuer's
    // slots take; the other 19 would wait for the next 30-minute recheck.
    assert_eq!(burst(false).0, MAX_CONNECTIONS);

    let (joined, last_ms) = burst(true);
    assert_eq!(joined, SUBSCRIBERS, "the starters' retries land every join");
    assert!(
        last_ms <= BUDGET_MS,
        "the 35-join burst took {last_ms} ms (> 10 min)"
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

/// The member vault server an earlier aterm installed where nothing finds or feeds it (a Linux
/// member's) goes through `sync.steward.server.standby`, swarm on or off, and its read-back,
/// which then reads the standby step idle, says it is gone: the row is `Changed`, not `Failed`
/// for a removal that worked. A hold on the vault keeps it, and a plan only plans it.
/// NEGATIVE CONTROL: with no stale server the row reads the swarm switch and the standby step.
#[test]
fn a_member_server_an_earlier_aterm_installed_goes_with_the_swarm_on_or_off() {
    const STALE: &str = "systems.alab.aterm-secrets-server runs the member vault server an \
                         earlier aterm installed";
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ports = |swarm: bool, held: Option<&str>| {
        let ports = Inert::new(&scratch.path().join(format!("{swarm}-{held:?}")));
        *ports.swarm.lock().unwrap() = Ok(swarm);
        *ports.held.lock().unwrap() = held.map(str::to_owned);
        *ports.stale_server.lock().unwrap() = Some(STALE.to_owned());
        // Off macOS the standby step serves nothing, so its read idles once the job is gone.
        *ports.steward_standby_server.lock().unwrap() =
            StewardStandbyServerRead::Idle("Bonjour is macOS only".to_owned());
        ports
    };

    let server_applies = |ports: &Inert| -> Vec<&'static str> {
        ports
            .applies()
            .into_iter()
            .filter(|kind| kind.contains("server"))
            .collect()
    };
    for swarm in [true, false] {
        let ports = ports(swarm, None);
        let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
        let removed = row(&pass.rows, STEWARD_SERVER_STANDBY);
        assert_eq!(
            removed.outcome,
            Outcome::Changed,
            "swarm {swarm}: {removed:?}"
        );
        assert_eq!(removed.from.as_deref(), Some(STALE), "swarm {swarm}");
        assert_eq!(
            server_applies(&ports),
            ["retire-stale-server"],
            "swarm {swarm}"
        );
        assert_eq!(*ports.stale_server.lock().unwrap(), None, "swarm {swarm}");
        if !swarm {
            assert_eq!(
                ports.reads.load(Ordering::SeqCst),
                0,
                "no swarm port read with the swarm off"
            );
        }
        // The next pass finds none left and reads the row as it reads any other.
        let next = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
        let after = row(&next.rows, STEWARD_SERVER_STANDBY);
        assert_eq!(
            after.outcome,
            Outcome::NotApplicable,
            "swarm {swarm}: {after:?}"
        );
        assert_eq!(
            server_applies(&ports),
            ["retire-stale-server"],
            "swarm {swarm}"
        );
    }

    let held = ports(true, Some("a Stop holds it"));
    let pass = ran(held.pass(Trigger::Subscriber, Mode::Apply));
    let kept = row(&pass.rows, STEWARD_SERVER_STANDBY);
    assert_eq!(kept.outcome, Outcome::NotApplicable, "{kept:?}");
    assert_eq!(kept.why.as_deref(), Some("a Stop holds it"));
    assert!(server_applies(&held).is_empty(), "{:?}", held.applies());
    assert!(held.stale_server.lock().unwrap().is_some());

    let planned = ports(false, None);
    let plan = ran(planned.pass(Trigger::Init, Mode::Plan));
    assert_eq!(
        row(&plan.rows, STEWARD_SERVER_STANDBY).outcome,
        Outcome::Planned
    );
    assert!(
        server_applies(&planned).is_empty(),
        "{:?}",
        planned.applies()
    );

    let none = ports(false, None);
    *none.stale_server.lock().unwrap() = None;
    let pass = ran(none.pass(Trigger::Subscriber, Mode::Apply));
    let off = row(&pass.rows, STEWARD_SERVER_STANDBY);
    assert_eq!(off.outcome, Outcome::NotApplicable, "{off:?}");
    assert_eq!(off.why.as_deref(), Some(SWARM_OFF));
    assert_eq!(none.stale_reads.load(Ordering::SeqCst), 1);
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
        pub(crate) owed: &'a (
                dyn Fn(Option<&crate::init_github::RepositoryReadback>) -> Result<OwedClone, String>
                    + Sync
            ),
        pub(crate) clone: &'a (dyn Fn() -> Result<crate::init::CloneOwedOutcome, String> + Sync),
        pub(crate) control: Option<&'a (dyn Fn() -> Result<Option<String>, String> + Sync)>,
        pub(crate) forget: &'a (dyn Fn() -> Result<(), String> + Sync),
    }

    impl Ports for Hooked<'_> {
        fn background(&self, trigger: Trigger) {
            self.inner.background(trigger);
        }
        fn new_pass(&self) {
            self.inner.new_pass();
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
        fn found_commons(&self) -> Result<SharedDone, String> {
            self.inner.found_commons()
        }
        fn accounts(&self) -> Result<AccountsRead, String> {
            self.inner.accounts()
        }
        fn resync_accounts(&self) -> Result<String, String> {
            self.inner.resync_accounts()
        }
        fn owed_clone(
            &self,
            readback: Option<&crate::init_github::RepositoryReadback>,
            _work: aterm_secrets::converge::RepositoryWork,
        ) -> Result<OwedClone, String> {
            (self.owed)(readback)
        }
        fn clone_owed(
            &self,
            _execution: Option<repository_refresh::Execution>,
        ) -> Result<crate::init::CloneOwedOutcome, String> {
            (self.clone)()
        }
        fn refresh_repositories(
            &self,
            execution: Option<repository_refresh::Execution>,
        ) -> Result<crate::init::RepositoryRefreshed, String> {
            self.inner.refresh_repositories(execution)
        }
        fn owed_clone_control(&self) -> Result<Option<String>, String> {
            match self.control {
                Some(control) => control(),
                None => self
                    .owed_clone(None, aterm_secrets::converge::RepositoryWork::Repair)
                    .map(|owed| match owed {
                        OwedClone::Off(why) | OwedClone::Held(why) => Some(why),
                        _ => None,
                    }),
            }
        }
        fn forget_owed_clone(&self) -> Result<(), String> {
            (self.forget)()
        }
        fn clone_tuning(
            &self,
            keep: crate::clone_tuning::Keep,
        ) -> Result<crate::clone_tuning::Reading, String> {
            self.inner.clone_tuning(keep)
        }
        fn tune_clones(
            &self,
            execution: Option<repository_refresh::Execution>,
        ) -> Result<crate::clone_tuning::Tuned, String> {
            self.inner.tune_clones(execution)
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
        fn start_elsewhere(&self, program: &Path, args: &[String]) -> Result<ElsewhereRun, String> {
            self.inner.start_elsewhere(program, args)
        }
    }

    /// No Sync row acts on the vault while System State holds it (design §8.5, principle 4): a
    /// person who presses Stop after fleet registration holds every row after it, each reading
    /// the hold as it starts, so the pass neither starts the node, resyncs the AI accounts nor
    /// begins cloning or refreshing repositories from the vault.
    #[test]
    fn a_stop_pressed_mid_pass_holds_every_row_after_it() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let stopped = AtomicBool::new(false);
        let held = || {
            stopped
                .load(Ordering::SeqCst)
                .then(|| "a Stop holds it".to_owned())
        };
        let owed = |_: Option<&crate::init_github::RepositoryReadback>| {
            Ok(OwedClone::Due("the GitHub token is here".to_owned()))
        };
        let clone = || panic!("a Stop before repository cloning must hold it");
        let ports = Hooked {
            inner: Inert::new(scratch.path()),
            held: &held,
            owed: &owed,
            clone: &clone,
            control: None,
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
        for id in [NODE, ACCOUNTS, CLONE, CLONE_TUNING, REFRESH] {
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
            owed: &|_| Ok(OwedClone::Nothing("aterm init owes no clone".into())),
            clone: &|| panic!("nothing is owed"),
            control: None,
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
            sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
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

    /// alice's init.toml names `account`, and the vault's token file holds a token GitHub's
    /// `GET /user` answers with `user`.
    fn profiled(
        account: &str,
        user: Result<crate::port::GithubResponse, crate::port::GithubFailure>,
    ) -> crate::port::Fake {
        let (sys, alice) = alice();
        // This independently configured profile still authenticates with GitHub. The vault
        // default instead reads account and inventory metadata locally.
        let profile = "/Users//alice/.config/aterm/init.toml";
        let profile_bytes = format!("[github]\naccount='{account}'\n").into_bytes();
        sys.file(profile, &profile_bytes).stat_of(
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
        sys.vault_set(
            "/Users//alice",
            crate::init_github::TOKEN_ENTRY,
            b"fixture_token_only",
        );
        match user {
            Ok(answer) => sys.on_github_api(&alice, "/user", Ok(answer)),
            Err(failure) => sys.fail_github_api(&alice, "/user", failure),
        };
        sys
    }

    /// `n` subscriber passes over an owed clone: each one's clone row, and how many times the
    /// GitHub step asked `GET /user` in all.
    fn owed_passes(
        sys: &crate::port::Fake,
        state: &Path,
        n: usize,
    ) -> (Vec<(Outcome, Option<String>)>, usize) {
        let held = || None;
        let owed = |readback: Option<&crate::init_github::RepositoryReadback>| {
            crate::init::owed_clone_in_pass(
                sys,
                state,
                &crate::init::Holds::NONE,
                readback,
                aterm_secrets::converge::RepositoryWork::Refresh,
            )
        };
        let clone = || crate::init::clone_owed(sys, state);
        let ports = Hooked {
            inner: Inert::new(state),
            held: &held,
            owed: &owed,
            clone: &clone,
            control: None,
            forget: &|| crate::init::forget_owed_clone(state),
        };
        let mut said = Vec::new();
        for _ in 0..n {
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
        (said, asked)
    }

    /// Nothing loops: the vault's token landed, but it signs in as another account than
    /// init.toml names, which only a person's settings fix, and the native step's rule is that
    /// its row needs them and no pass retries it. The first pass runs the step once and records
    /// that it waits for a person; later passes say what to do and run nothing until the token
    /// or init.toml changes.
    #[test]
    fn a_clone_only_a_person_can_finish_is_not_rerun_every_pass() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let sys = profiled(
            "bob",
            Ok(crate::port::GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: br#"{"login":"alice"}"#.to_vec(),
            }),
        );
        owe_a_clone(scratch.path());
        let (said, asked) = owed_passes(&sys, scratch.path(), 3);
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
        sys.vault_set(
            "/Users//alice",
            crate::init_github::TOKEN_ENTRY,
            b"fixture_token_next",
        );
        let (_, asked) = owed_passes(&sys, scratch.path(), 1);
        assert_eq!(asked, 2, "a new token runs the step again");
    }

    /// A token GitHub refuses is no person's step and no loop: the first pass runs the step
    /// once, and later passes wait, asking nobody and running nothing, until the vault's token
    /// changes, which runs the step again.
    #[test]
    fn a_refused_token_waits_without_a_person_and_is_not_rerun_every_pass() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let sys = profiled("alice", Err(crate::port::GithubFailure::Unauthenticated));
        owe_a_clone(scratch.path());
        let (said, asked) = owed_passes(&sys, scratch.path(), 3);
        assert_eq!(
            asked, 1,
            "three passes each re-ran a refused token: {said:?}"
        );
        assert!(
            said.iter().all(|(outcome, _)| *outcome != Outcome::Person),
            "{said:?}"
        );
        let (outcome, why) = &said[2];
        assert_eq!(*outcome, Outcome::Deferred, "{said:?}");
        assert!(
            why.as_deref()
                .is_some_and(|why| !why.contains("Only you can fix this")),
            "{said:?}"
        );
        sys.vault_set(
            "/Users//alice",
            crate::init_github::TOKEN_ENTRY,
            b"fixture_token_next",
        );
        let (_, asked) = owed_passes(&sys, scratch.path(), 1);
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
        let read = crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE, None).unwrap();
        assert!(matches!(read, OwedClone::Off(_)), "{read:?}");
        assert!(
            crate::init::owed_clone_control(&sys, &state, &crate::init::Holds::NONE)
                .unwrap()
                .is_some()
        );
        let held = || None;
        let owed = |readback: Option<&crate::init_github::RepositoryReadback>| {
            crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE, readback)
        };
        let clone = || crate::init::clone_owed(&sys, &state);
        let ports = Hooked {
            inner: Inert::new(scratch.path()),
            held: &held,
            owed: &owed,
            clone: &clone,
            control: None,
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
            sys.vault_set(
                "/Users//alice",
                crate::init_github::TOKEN_ENTRY,
                b"fixture_token_only",
            );
            crate::init::owe_clone(scratch.path(), &start).unwrap();
            let state = scratch.path().to_path_buf();
            let calls = sys.calls().len();
            let control = || crate::init::owed_clone_control(&sys, &state, &now);
            assert_eq!(control().unwrap().is_none(), cloned_now);
            // Fake also records the fresh in-process account reading in this log.
            assert!(
                sys.calls()
                    .iter()
                    .skip(calls)
                    .all(|call| call == "identity"),
                "control policy starts no commands: {:?}",
                &sys.calls()[calls..]
            );
            // This is the production policy used by Main's Live adapter, not the
            // successful fake clone callback below.
            let intent = crate::init::OwedCloneIntent::capture(&sys, &state, &now);
            assert_eq!(
                intent.is_ok(),
                cloned_now,
                "the actual Live apply policy must preserve a named old-hold override: {intent:?}"
            );
            if let Ok(intent) = intent {
                assert!(intent.current(&sys, &state, &now).unwrap());
                for later in [
                    crate::init::Holds {
                        stopped: Some("later Stop".into()),
                        ..now.clone()
                    },
                    crate::init::Holds {
                        newer: true,
                        ..now.clone()
                    },
                ] {
                    assert!(!intent.current(&sys, &state, &later).unwrap());
                }
            }
            let cloned = AtomicBool::new(false);
            let held = || None;
            let owed = |readback: Option<&crate::init_github::RepositoryReadback>| {
                crate::init::owed_clone(&sys, &state, &now, readback)
            };
            let clone = || {
                cloned.store(true, Ordering::SeqCst);
                Ok(crate::init::CloneOwedOutcome {
                    said: "cloned".to_owned(),
                    readback: None,
                })
            };
            let ports = Hooked {
                inner: Inert::new(scratch.path()),
                held: &held,
                owed: &owed,
                clone: &clone,
                control: Some(&control),
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
        let scratch = aterm_tempfile::tempdir().unwrap();
        let (sys, _) = alice();
        assert!(
            crate::init::owed_clone_control(&sys, scratch.path(), &left)
                .unwrap()
                .is_some(),
            "automatic work with no owed record never borrows a named hold override"
        );
        assert!(
            sys.calls().iter().all(|call| call == "identity"),
            "automatic hold starts no commands: {:?}",
            sys.calls()
        );
        assert!(crate::init::OwedCloneIntent::capture(&sys, scratch.path(), &left).is_err());

        // A later named request cannot grant an apply that captured no request,
        // and a replacement cannot revive the capture of an earlier request.
        use std::os::unix::fs::MetadataExt as _;
        let unheld = crate::init::Holds::NONE;
        let automatic =
            crate::init::OwedCloneIntent::capture(&sys, scratch.path(), &unheld).unwrap();
        assert!(automatic.current(&sys, scratch.path(), &unheld).unwrap());
        let other = aterm_tempfile::tempdir().unwrap();
        assert!(!automatic.current(&sys, other.path(), &unheld).unwrap());
        crate::init::owe_clone(scratch.path(), &unheld).unwrap();
        assert!(!automatic.current(&sys, scratch.path(), &unheld).unwrap());
        let intent = crate::init::OwedCloneIntent::capture(&sys, scratch.path(), &unheld).unwrap();
        assert!(!intent.current(&sys, scratch.path(), &left).unwrap());
        let dir = scratch.path().join(FileJournal::DIR);
        let (original, before) = aterm_uds::private_file::open_private(&dir, "clone-owed").unwrap();
        let bytes = aterm_uds::private_file::read_private(&dir, "clone-owed", 4096).unwrap();
        aterm_uds::private_file::write_private(&dir, "clone-owed", &bytes).unwrap();
        let after = std::fs::metadata(dir.join("clone-owed")).unwrap();
        assert_ne!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(original.metadata().unwrap().ino(), before.ino());
        assert!(
            !intent.current(&sys, scratch.path(), &unheld).unwrap(),
            "identical bytes in a new debt record cannot grant the older captured apply"
        );
        let intent = crate::init::OwedCloneIntent::capture(&sys, scratch.path(), &unheld).unwrap();
        let mut changed = bytes;
        changed.push(b'\n');
        std::fs::write(dir.join("clone-owed"), &changed).unwrap();
        let rewritten = std::fs::metadata(dir.join("clone-owed")).unwrap();
        assert_eq!(
            (after.dev(), after.ino()),
            (rewritten.dev(), rewritten.ino())
        );
        assert!(
            !intent.current(&sys, scratch.path(), &unheld).unwrap(),
            "an in-place debt edit also retires the captured apply"
        );
        let intent = crate::init::OwedCloneIntent::capture(&sys, scratch.path(), &unheld).unwrap();
        crate::init::forget_owed_clone(scratch.path()).unwrap();
        assert!(!intent.current(&sys, scratch.path(), &unheld).unwrap());
    }

    /// `work` (`~/unborn`), a checkout without a commit of `empty`, an empty bare repository,
    /// under `origin`, the GitHub name the vault's scripted inventory selects. `sys` runs each
    /// git command in `work` or naming `origin` for real, reading `empty` wherever a remote
    /// read or fetch names `origin`; `advertisements` counts the remote reads.
    pub(crate) struct EmptyRemote<'a> {
        pub(crate) repos: crate::checkout::tests::Repos<'a>,
        pub(crate) sys: crate::port::Fake,
        pub(crate) ctx: crate::items::Ctx,
        pub(crate) state: PathBuf,
        pub(crate) work: PathBuf,
        pub(crate) empty: PathBuf,
        pub(crate) origin: String,
        pub(crate) advertisements: Arc<AtomicU32>,
        pub(crate) validations: Arc<AtomicU32>,
    }

    /// [`EmptyRemote`] in a scratch home `label` names.
    pub(crate) fn empty_remote<'a>(live: &'a crate::port::Live, label: &str) -> EmptyRemote<'a> {
        use crate::git_credential::script;
        use crate::port::{Live, System};
        let repos = crate::checkout::tests::Repos::new(live, label);
        let home = Path::new(&repos.who.home);
        let empty = home.join("empty.git");
        repos.init(&empty, true);
        let work = home.join("unborn");
        repos.clone_into(&empty, &work);
        let origin = format!("https://github.com/{}/unborn.git", repos.who.name);
        repos.g(&work, &["remote", "set-url", "origin", &origin]);
        let mut sys = crate::port::Fake::root();
        sys.euid = repos.who.uid;
        sys.linux = !live.macos();
        sys.logged_in(&repos.who)
            .stat_of(&repos.who.home, live.stat(home).unwrap())
            .executable(
                crate::items::github::need_native_git(),
                b"selected fixture git",
            );
        for path in [work.clone(), work.join(".git"), work.join(".git/objects")] {
            sys.stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
            sys.file_id_of(&path, live.file_id(&path).unwrap());
        }
        script::launcher(&sys, &repos.who);
        crate::init_github::scripted::owned_source(&sys, &repos.who);
        crate::init_github::scripted::metadata(
            &sys,
            &repos.who,
            aterm_json::json!([crate::init_github::scripted::repository(
                &repos.who.name,
                "unborn",
                1
            )]),
        );
        script::lookups_hold(
            &sys,
            &repos.who,
            "github.com",
            &repos.who.name,
            "fixture_development_token_only",
        );
        script::alike(&sys, &repos.who);
        let advertisements = Arc::new(AtomicU32::new(0));
        let observed = Arc::clone(&advertisements);
        let validations = Arc::new(AtomicU32::new(0));
        let checked = Arc::clone(&validations);
        let (at, bare, advertised) = (
            work.clone(),
            empty.clone(),
            std::ffi::OsString::from(origin.clone()),
        );
        sys.simulate(move |_, command| {
            let in_work = command
                .args
                .windows(2)
                .any(|pair| pair[0] == "-C" && pair[1] == at.as_os_str());
            if !in_work && !command.args.iter().any(|arg| arg == &advertised) {
                return None;
            }
            if in_work
                && command
                    .args
                    .windows(2)
                    .any(|pair| pair[0] == "rev-parse" && pair[1] == "--is-inside-work-tree")
            {
                checked.fetch_add(1, Ordering::SeqCst);
            }
            let remote = command.args.iter().any(|arg| arg == "ls-remote")
                && !command.args.iter().any(|arg| arg == "--get-url");
            let fetch = command.args.iter().any(|arg| arg == "fetch");
            let mut command = command.clone();
            if remote {
                observed.fetch_add(1, Ordering::SeqCst);
            }
            if remote || fetch {
                for arg in &mut command.args {
                    if arg == "origin" || arg == &advertised {
                        *arg = bare.as_os_str().to_owned();
                    }
                }
            }
            Some(Live::new().run_complete_stdout(&command).unwrap())
        });
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&repos.who));
        let state = home.join("sync-state");
        EmptyRemote {
            repos,
            sys,
            ctx,
            state,
            work,
            empty,
            origin,
            advertisements,
            validations,
        }
    }

    /// System State's GitHub repositories row and `sync.clone` count one partition alike: for
    /// the same inventory and checkouts, the stage's acquisition reads the counts the private
    /// inventory's count reads, its row names their summary, and `sync.clone`'s row names that
    /// same summary. CONTROL: with another of them already here, all three read the new count.
    #[test]
    fn the_native_row_and_sync_clone_count_one_partition_alike() {
        use crate::init_observe::{Reading, RepositoryCounts};
        let listed = ["here", "new-a", "new-b"];
        for present in [&["here"][..], &["here", "new-a"][..]] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let state = scratch.path().to_path_buf();
            let (sys, alice) = crate::init_github::scripted::inventory(&listed, present);
            let expected = RepositoryCounts {
                ready: present.len(),
                missing: listed.len() - present.len(),
                ..RepositoryCounts::default()
            };
            let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&alice));
            let rows =
                crate::init_github::run_initial_repositories(&sys, &ctx, Mode::Plan).unwrap();
            let stage = rows
                .iter()
                .find(|row| row.id.starts_with("init.github.native@"))
                .expect("the acquisition's row");
            assert_eq!(
                Reading::of(stage),
                Some(Reading::Repositories {
                    counts: expected,
                    signed_in: true,
                }),
                "{present:?}: {rows:?}"
            );
            assert_eq!(stage.from.as_deref(), Some(expected.summary().as_str()));
            assert_eq!(
                crate::init_github::automatic_inventory_counts(&sys, &ctx),
                Ok(Some(expected))
            );
            let owed = |readback: Option<&crate::init_github::RepositoryReadback>| {
                crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE, readback)
            };
            let ports = Hooked {
                inner: Inert::new(&state),
                held: &|| None::<String>,
                owed: &owed,
                clone: &|| panic!("a plan clones nothing"),
                control: None,
                forget: &|| Ok(()),
            };
            let pass = ran(converge(
                &ports,
                Trigger::Subscriber,
                Mode::Plan,
                aterm_secrets::converge::RepositoryWork::Repair,
                &mut |_| {},
            ));
            let clone = row(&pass.rows, CLONE);
            assert_eq!(
                clone.from,
                Some(format!("vault repositories: {}", expected.summary())),
                "{present:?}: {clone:?}"
            );
        }
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
        crate::init_github::scripted::converged(&sys, &alice, "ready");
        crate::port::System::remove(&sys, Path::new(crate::items::github::CLT_GIT)).unwrap();
        let held = || None;
        let owed = |readback: Option<&crate::init_github::RepositoryReadback>| {
            crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE, readback)
        };
        let clone = || crate::init::clone_owed(&sys, &state);
        let forget = || crate::init::forget_owed_clone(&state);
        let ports = Hooked {
            inner: Inert::new(&state),
            held: &held,
            owed: &owed,
            clone: &clone,
            control: None,
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
        sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
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

        // An unborn checkout is not offline proof of an empty remote. Only this item's
        // actual native admission may carry that fact into its fresh outer read-back.
        use crate::port::{Live, System};
        let live = Live::new();
        let EmptyRemote {
            repos,
            sys,
            ctx,
            state,
            work,
            empty,
            origin,
            advertisements,
            validations,
        } = empty_remote(&live, "sync-empty-readback");
        let home = Path::new(&repos.who.home);
        let read = |readback: Option<&crate::init_github::RepositoryReadback>,
                    now: &crate::init::Holds| {
            crate::init::owed_clone_captured(
                &sys,
                &state,
                now,
                crate::init_github::InitialRepositoryInputs::capture(&sys, &ctx)?,
                || Ok(true),
                readback,
                aterm_secrets::converge::RepositoryWork::Repair,
            )
        };
        assert_eq!(
            read(None, &crate::init::Holds::NONE).unwrap(),
            OwedClone::Unobserved(crate::init_observe::RepositoryCounts {
                ready: 0,
                unborn: 1,
                ..crate::init_observe::RepositoryCounts::default()
            })
        );
        assert_eq!(
            advertisements.load(Ordering::SeqCst),
            0,
            "offline debt asks no remote"
        );
        let cloned = AtomicU32::new(0);
        let after_probe = AtomicU32::new(u32::MAX);
        let guard_validations = AtomicU32::new(u32::MAX);
        let clone = || {
            if cloned.load(Ordering::SeqCst) == 0 {
                guard_validations.store(
                    validations
                        .load(Ordering::SeqCst)
                        .saturating_sub(after_probe.load(Ordering::SeqCst)),
                    Ordering::SeqCst,
                );
            }
            let outcome = crate::init::clone_owed_captured(
                &sys,
                &state,
                crate::init_github::InitialRepositoryInputs::capture(&sys, &ctx)?,
                || Ok(true),
            )?;
            let proof = outcome
                .readback
                .as_ref()
                .expect("the actual empty admission returned its read-back");
            assert_eq!(
                read(Some(proof), &crate::init::Holds::NONE)?,
                OwedClone::Verified(crate::init_observe::RepositoryCounts {
                    ready: 1,
                    ..crate::init_observe::RepositoryCounts::default()
                })
            );
            assert!(
                matches!(
                    read(None, &crate::init::Holds::NONE)?,
                    OwedClone::Unobserved(_)
                ),
                "no durable completion substitutes for the item's proof"
            );
            if cloned.fetch_add(1, Ordering::SeqCst) == 0 {
                let mut left = crate::init::Holds::NONE;
                left.off
                    .set(aterm_types::macos_setup::Setting::Repositories, true);
                assert!(matches!(read(Some(proof), &left)?, OwedClone::Held(_)));
                let stopped = crate::init::Holds {
                    stopped: Some("fixture Stop".into()),
                    ..crate::init::Holds::NONE
                };
                assert!(!matches!(
                    read(Some(proof), &stopped),
                    Ok(OwedClone::Verified(_))
                ));
                sys.vault_remove(home, crate::init_github::TOKEN_ENTRY);
                assert!(
                    !matches!(
                        read(Some(proof), &crate::init::Holds::NONE),
                        Ok(OwedClone::Verified(_))
                    ),
                    "an empty-remote proof cannot supply missing source authority"
                );
                crate::init_github::scripted::owned_source(&sys, &repos.who);
                repos.g(
                    &work,
                    &[
                        "remote",
                        "set-url",
                        "origin",
                        "https://github.com/elsewhere/unborn.git",
                    ],
                );
                assert!(
                    !matches!(
                        read(Some(proof), &crate::init::Holds::NONE),
                        Ok(OwedClone::Verified(_))
                    ),
                    "a retargeted checkout cannot borrow the old proof"
                );
                repos.g(&work, &["remote", "set-url", "origin", &origin]);
                // The raw origin and checkout identity stay unchanged, but Git's actual
                // local endpoint can change without any vault input changing.
                let redirected = home.join("unadmitted-empty.git");
                let rewritten_url = format!("file://{}", redirected.display());
                let rewrite_key = format!("url.{rewritten_url}.insteadOf");
                repos.g(&work, &["config", "--local", &rewrite_key, &origin]);
                assert_eq!(
                    repos.g(&work, &["config", "--local", "--get", "remote.origin.url"]),
                    origin
                );
                assert_eq!(
                    repos.g(&work, &["ls-remote", "--get-url", "origin"]),
                    rewritten_url
                );
                let before_rewrite_read = advertisements.load(Ordering::SeqCst);
                assert!(
                    !matches!(
                        read(Some(proof), &crate::init::Holds::NONE),
                        Ok(OwedClone::Verified(_))
                    ),
                    "a locally rewritten endpoint cannot borrow the raw origin's proof"
                );
                assert_eq!(advertisements.load(Ordering::SeqCst), before_rewrite_read);
                assert!(!redirected.exists(), "read-back performs no acquisition");
                repos.g(&work, &["config", "--local", "--unset-all", &rewrite_key]);
                assert_eq!(
                    read(Some(proof), &crate::init::Holds::NONE)?,
                    OwedClone::Verified(crate::init_observe::RepositoryCounts {
                        ready: 1,
                        ..crate::init_observe::RepositoryCounts::default()
                    })
                );
                let held_work = home.join("held-unborn");
                std::fs::rename(&work, &held_work).unwrap();
                repos.clone_into(&empty, &work);
                repos.g(&work, &["remote", "set-url", "origin", &origin]);
                for path in [work.clone(), work.join(".git"), work.join(".git/objects")] {
                    sys.stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
                    sys.file_id_of(&path, live.file_id(&path).unwrap());
                }
                assert!(
                    !matches!(
                        read(Some(proof), &crate::init::Holds::NONE),
                        Ok(OwedClone::Verified(_))
                    ),
                    "a same-path replacement cannot borrow the old proof"
                );
                std::fs::remove_dir_all(&work).unwrap();
                std::fs::rename(&held_work, &work).unwrap();
                for path in [work.clone(), work.join(".git"), work.join(".git/objects")] {
                    sys.stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
                    sys.file_id_of(&path, live.file_id(&path).unwrap());
                }
                assert_eq!(
                    read(Some(proof), &crate::init::Holds::NONE)?,
                    OwedClone::Verified(crate::init_observe::RepositoryCounts {
                        ready: 1,
                        ..crate::init_observe::RepositoryCounts::default()
                    })
                );
            } else {
                // The second native batch succeeds, then a different current source lands
                // before Synced's outer probe. The old receipt must not finish that item.
                crate::init_github::scripted::metadata(
                    &sys,
                    &repos.who,
                    aterm_json::json!([crate::init_github::scripted::repository(
                        &repos.who.name,
                        "unborn",
                        2
                    )]),
                );
                assert!(!matches!(
                    read(Some(proof), &crate::init::Holds::NONE),
                    Ok(OwedClone::Verified(_))
                ));
            }
            Ok(outcome)
        };
        let held = || None;
        let owed = |readback: Option<&crate::init_github::RepositoryReadback>| {
            let value = read(readback, &crate::init::Holds::NONE);
            let _ = after_probe.compare_exchange(
                u32::MAX,
                validations.load(Ordering::SeqCst),
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            value
        };
        let control = || crate::init::owed_clone_control(&sys, &state, &crate::init::Holds::NONE);
        let ports = Hooked {
            inner: Inert::new(&state),
            held: &held,
            owed: &owed,
            clone: &clone,
            control: Some(&control),
            forget: &|| crate::init::forget_owed_clone(&state),
        };
        let complete = ran(converge(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Repair,
            &mut |_| {},
        ));
        assert_eq!(
            row(&complete.rows, CLONE).outcome,
            Outcome::Changed,
            "{complete:?}"
        );
        assert!(
            complete.complete(),
            "the actual empty native admission must settle the whole pass: {complete:?}"
        );
        assert!(
            after_probe.load(Ordering::SeqCst) > 0
                && after_probe.load(Ordering::SeqCst) != u32::MAX,
            "the actual row probe verified the existing checkout through Git"
        );
        assert_eq!(
            guard_validations.load(Ordering::SeqCst),
            0,
            "the fresh pre-apply hold check must not repeat repository validation"
        );
        assert_eq!(cloned.load(Ordering::SeqCst), 1);
        assert_eq!(
            advertisements.load(Ordering::SeqCst),
            1,
            "all read-backs reuse the current item's admission"
        );
        assert!(
            matches!(
                read(None, &crate::init::Holds::NONE).unwrap(),
                OwedClone::Unobserved(_)
            ),
            "a later item needs its own current admission"
        );
        let changed = ran(converge(
            &ports,
            Trigger::Subscriber,
            Mode::Apply,
            aterm_secrets::converge::RepositoryWork::Repair,
            &mut |_| {},
        ));
        assert!(!changed.complete(), "{changed:?}");
        assert_ne!(
            row(&changed.rows, CLONE).outcome,
            Outcome::Changed,
            "{changed:?}"
        );
        assert_eq!(cloned.load(Ordering::SeqCst), 2);
        assert_eq!(
            advertisements.load(Ordering::SeqCst),
            2,
            "the changed source is not silently retried within the pass"
        );
    }

    /// A checkout without a commit is acquisition's, never the refresh's: each Refresh pass's
    /// `sync.clone` reads that remote once (a commit there would be acquired, never assumed
    /// away) and its read-back verifies the empty one, and `sync.refresh` reads it no second
    /// time. An acquisition that proves nothing fails the row instead of reading in step.
    /// CONTROL: a plan plans the read as a change and reads no remote.
    #[test]
    fn an_empty_remote_checked_out_is_read_once_by_acquisition_in_every_refresh_pass() {
        let live = crate::port::Live::new();
        // `repos` holds the scratch home: dropping it removes the checkout.
        let EmptyRemote {
            repos: _home,
            sys,
            ctx,
            state,
            advertisements,
            ..
        } = empty_remote(&live, "sync-empty-refresh");
        let refresh = aterm_secrets::converge::RepositoryWork::Refresh;
        let owed = |readback: Option<&crate::init_github::RepositoryReadback>| {
            crate::init::owed_clone_captured(
                &sys,
                &state,
                &crate::init::Holds::NONE,
                crate::init_github::InitialRepositoryInputs::capture(&sys, &ctx)?,
                || Ok(true),
                readback,
                refresh,
            )
        };
        let proves = AtomicBool::new(true);
        let clone = || {
            let outcome = crate::init::clone_owed_captured(
                &sys,
                &state,
                crate::init_github::InitialRepositoryInputs::capture(&sys, &ctx)?,
                || Ok(true),
            )?;
            Ok(if proves.load(Ordering::SeqCst) {
                outcome
            } else {
                crate::init::CloneOwedOutcome {
                    readback: None,
                    ..outcome
                }
            })
        };
        let held = || None;
        let ports = Hooked {
            inner: Inert::new(&state),
            held: &held,
            owed: &owed,
            clone: &clone,
            control: None,
            forget: &|| crate::init::forget_owed_clone(&state),
        };
        let pass = |mode| {
            ran(converge(
                &ports,
                Trigger::Subscriber,
                mode,
                refresh,
                &mut |_| {},
            ))
        };
        let planned = pass(Mode::Plan);
        assert_eq!(
            row(&planned.rows, CLONE).outcome,
            Outcome::Planned,
            "{planned:?}"
        );
        assert_eq!(
            advertisements.load(Ordering::SeqCst),
            0,
            "a plan reads no remote"
        );
        for reads in 1..=2 {
            let applied = pass(Mode::Apply);
            let clone_row = row(&applied.rows, CLONE);
            assert_eq!(clone_row.outcome, Outcome::Changed, "{applied:?}");
            assert_eq!(
                clone_row.to.as_deref(),
                Some("vault repositories: 1 of 1 ready, verified on this machine"),
                "{clone_row:?}"
            );
            assert!(applied.complete(), "{applied:?}");
            assert_eq!(
                advertisements.load(Ordering::SeqCst),
                reads,
                "each pass reads the remote once"
            );
        }
        proves.store(false, Ordering::SeqCst);
        let unproved = pass(Mode::Apply);
        let clone_row = row(&unproved.rows, CLONE);
        assert_eq!(clone_row.outcome, Outcome::Failed, "{unproved:?}");
        assert!(
            clone_row
                .why
                .as_deref()
                .is_some_and(|why| why.contains("applied, but it still reads")
                    && why.contains("without a commit")),
            "{clone_row:?}"
        );
    }

    /// A store whose local state cannot be read leaves the owed clone waiting with what is
    /// wrong, never as if no token had landed. CONTROL: the same store read is due.
    #[test]
    fn a_vault_aterm_cannot_read_says_what_is_wrong() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().to_path_buf();
        crate::init::owe_clone(&state, &crate::init::Holds::NONE).unwrap();
        let (sys, _alice) = alice_with(true);
        sys.vault_set(
            "/Users//alice",
            crate::init_github::TOKEN_ENTRY,
            b"fixture_token_only",
        );
        let read = crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE, None).unwrap();
        assert!(matches!(read, crate::init::OwedClone::Due(_)), "{read:?}");
        sys.vault_unreadable(std::io::ErrorKind::PermissionDenied);
        let read = crate::init::owed_clone(&sys, &state, &crate::init::Holds::NONE, None).unwrap();
        assert!(
            matches!(&read, crate::init::OwedClone::Waiting(why)
                if why.contains("gh_access_token") && why.contains("cannot be read")),
            "{read:?}"
        );
    }

    /// A Stop pressed once `sync.clone` has read what is owed (its clone may take half an hour)
    /// holds the clone, as System State's own Repositories stage reads the person's choice
    /// before each clone: the apply reads the hold again as it begins. A clone owed, or a
    /// checkout without a commit, is a clone held; with every repository here, nothing is
    /// cloned and the row reads them as they are. Either way `sync.refresh`, after it, reads the
    /// Stop and refreshes nothing.
    #[test]
    fn a_stop_pressed_as_sync_clone_starts_holds_its_clone() {
        let readings: [(fn() -> OwedClone, bool); 3] = [
            (
                || OwedClone::Due("the vault's GitHub token is here now".to_owned()),
                true,
            ),
            (
                || {
                    OwedClone::Unobserved(crate::init_observe::RepositoryCounts {
                        unborn: 1,
                        ..crate::init_observe::RepositoryCounts::default()
                    })
                },
                true,
            ),
            (
                || {
                    OwedClone::Verified(crate::init_observe::RepositoryCounts {
                        ready: 1,
                        ..crate::init_observe::RepositoryCounts::default()
                    })
                },
                false,
            ),
        ];
        for (reading, owes) in readings {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let state = scratch.path().to_path_buf();
            let stopped = AtomicBool::new(false);
            let cloned = AtomicBool::new(false);
            let held = || {
                stopped
                    .load(Ordering::SeqCst)
                    .then(|| "you stopped setup in System State".to_owned())
            };
            let owed = |_: Option<&crate::init_github::RepositoryReadback>| {
                // The person presses Stop while the row reads what is owed.
                stopped.store(true, Ordering::SeqCst);
                Ok(reading())
            };
            let clone = || {
                cloned.store(true, Ordering::SeqCst);
                Ok(crate::init::CloneOwedOutcome {
                    said: "cloned".to_owned(),
                    readback: None,
                })
            };
            let ports = Hooked {
                inner: Inert::new(&state),
                held: &held,
                owed: &owed,
                clone: &clone,
                control: None,
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
                "a Stop pressed before sync.clone's clone began did not hold it: {:?}",
                reading()
            );
            let stopped_by = |row: &ItemReport| {
                row.why
                    .as_deref()
                    .is_some_and(|why| why.contains("you stopped setup in System State"))
            };
            let clone_row = row(&pass.rows, CLONE);
            if owes {
                assert!(
                    clone_row.outcome == Outcome::Blocked && stopped_by(clone_row),
                    "{:?}: {clone_row:?}",
                    reading()
                );
            } else {
                assert_eq!(clone_row.outcome, Outcome::Ok, "{clone_row:?}");
            }
            let refresh_row = row(&pass.rows, REFRESH);
            assert!(stopped_by(refresh_row), "{:?}: {refresh_row:?}", reading());
            assert!(
                !ports.inner.applies().contains(&"refresh checkouts"),
                "{:?}",
                reading()
            );
        }
    }
}
