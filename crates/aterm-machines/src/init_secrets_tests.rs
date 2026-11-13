// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, user};
use crate::model::{ItemReport, Outcome};
use crate::port::{Cmd, Fake, Ran, RunFailed, Secret, Stat};
use aterm_secrets::age::Recipient;
use aterm_secrets::client::{Config, Probed};
use aterm_uds::{AcceptWait, AcceptedMode};
use std::collections::VecDeque;

/// The row's words without a LAN broker ([`Scripted::broker_absent`]): the broker's own,
/// not installed.
fn absent() -> String {
    lan_broker_absent(&aterm_uds::lan::Unavailable::NotInstalled.to_string())
}

/// A vault whose state answers from a script (the last one sticks), and which records
/// every look for the ALab vault, join and subscriber install.
#[derive(Default)]
pub(crate) struct Scripted {
    bootstrap: Mutex<Option<(aterm_secrets::client::NativeAdmission, Option<Status>)>>,
    prepared: Mutex<u32>,
    native_tries: Mutex<u32>,
    stop_at_native: Mutex<Option<Arc<AtomicBool>>>,
    off_at_native: Mutex<bool>,
    statuses: Mutex<VecDeque<aterm_secrets::Result<Status>>>,
    subscriber: Mutex<Option<aterm_secrets::Result<State>>>,
    terminal: Mutex<bool>,
    probed: Mutex<Option<aterm_secrets::Result<ProbeOutcome>>>,
    outcome: Mutex<Option<aterm_secrets::Result<JoinOutcome>>>,
    /// The join queues a request (calls `requested`) before it ends, as `client::join`
    /// does for a Mac not in yet.
    requests: Mutex<bool>,
    /// The exact caller-read configuration handed to the copy and join boundaries.
    replacement_reads: Mutex<Vec<Replaces>>,
    installed: Mutex<Option<aterm_secrets::Result<String>>>,
    probes: Mutex<u32>,
    joins: Mutex<Vec<(String, bool)>>,
    /// Whether each join was given a join password (never the password itself).
    passwords: Mutex<Vec<bool>>,
    /// What each `requested` call answered: the subscriber is in place.
    hooked: Mutex<Vec<bool>>,
    /// How many installs had happened when each join ended.
    installed_by_join: Mutex<Vec<u32>>,
    installs: Mutex<u32>,
    /// What `synced` answers until a sync makes it true.
    unwritten: Mutex<bool>,
    /// Exercise the live readiness and sync paths with isolated on-disk custody.
    local_home: Mutex<Option<PathBuf>>,
    syncs: Mutex<u32>,
    /// What `reachable` answers (`None`: the vault answers): nothing answered, or no
    /// vault there; and how many times it was asked.
    unreachable: Mutex<Option<Look>>,
    absent: Mutex<Option<String>>,
    reached: Mutex<u32>,
    /// What this Mac's vault state and subscriber become while `reachable` is asked:
    /// the subscriber, running beside the setup, applying a version or stopping.
    meanwhile: Mutex<Option<(Status, State)>>,
    /// Why `keep` fails (`None`: it keeps).
    unkept: Mutex<Option<String>>,
    /// A file the vault wrote is changed as the subscriber is installed.
    spoiled_by_install: Mutex<bool>,
    /// The subscriber publishes a source before its installer returns.
    published_by_install: Mutex<Option<Status>>,
    /// What `fleeting` answers: this aterm runs from a copy that goes away, and every
    /// install is refused as `aterm_secrets::service::service_program` refuses it.
    fleeting: Mutex<Option<PathBuf>>,
    /// The wait each join was given.
    waits: Mutex<Vec<Duration>>,
    /// Every change in order (`join <source>`, `install`, `sync`), with the lines a
    /// [`Lines`] journal records before them.
    log: Mutex<Vec<String>>,
    /// What `say` was given.
    said: Mutex<Vec<String>>,
    /// The person turned the subscriber off.
    turned_off: Mutex<bool>,
    /// Optional override for `[swarm] enabled` (`None`: `Ok(true)`).
    swarm: Mutex<Option<aterm_secrets::Result<bool>>>,
    /// Why a subscriber just installed does not keep running (`None`: it does).
    dies: Mutex<Option<String>>,
    /// Looks answered first, in order, before `probed` answers the rest.
    looks: Mutex<VecDeque<aterm_secrets::Result<ProbeOutcome>>>,
    /// What `subscriber` answers first, in order, before `subscriber` answers the rest: a
    /// subscriber that stops or starts between two reads.
    readings: Mutex<VecDeque<aterm_secrets::Result<State>>>,
    /// What each `check` answers, in order (`Pinned` once they run out), and the sources it
    /// was asked about.
    checked: Mutex<VecDeque<Result<Checked, String>>>,
    checks: Mutex<Vec<String>>,
    /// What this Mac's vault state becomes as a join runs, before its outcome: the client
    /// saving the vault before the request it then fails to queue.
    saved_by_join: Mutex<Option<Status>>,
    /// A join observes sharing withdrawn after saving its source; a later read is on again.
    withdraw_at_join: Mutex<bool>,
    /// How many times ALab's members were asked, what each ask answers (none heard it when
    /// unset), and whether a member already let this Mac in.
    member_asks: Mutex<u32>,
    members_hear: Mutex<Option<Result<usize, String>>>,
    let_in: Mutex<bool>,
    /// The LAN broker does not answer (`broker_unready`).
    broker_absent: Mutex<bool>,
    /// Where this Mac is for its join's words (`None`: on the ALab LAN).
    place: Mutex<Option<JoinPlace>>,
    /// What `dropped` answers (`None`: an act took this Mac out), and each seq it was asked
    /// with.
    drop_words: Mutex<Option<String>>,
    dropped_asks: Mutex<Vec<u64>>,
}

/// The scripted port's words for a Mac a rotation dropped: a stand-in, so a test holds the
/// routing, not the prose (`aterm_secrets::client::dropped_words`'s own test holds that).
pub(crate) const DROPPED: &str = "dropped: the scripted port's words";

impl Scripted {
    pub(crate) fn new(statuses: Vec<aterm_secrets::Result<Status>>) -> Arc<Self> {
        Arc::new(Self {
            statuses: Mutex::new(statuses.into()),
            terminal: Mutex::new(true),
            probed: Mutex::new(Some(Ok(ProbeOutcome::Absent))),
            outcome: Mutex::new(Some(Ok(JoinOutcome::Joined {
                seq: 7,
                names: vec!["gh_access_token".into()],
            }))),
            installed: Mutex::new(Some(Ok(
                "systems.alab.aterm-secrets: launchd user agent".into()
            ))),
            ..Self::default()
        })
    }

    /// Puts this Mac where `place` says for its join's words.
    pub(crate) fn placed(&self, place: JoinPlace) {
        *self.place.lock().unwrap() = Some(place);
    }

    /// Has `dropped` answer `words`: a rotation dropped this Mac with no `Remove`.
    pub(crate) fn drops(&self, words: &str) {
        *self.drop_words.lock().unwrap() = Some(words.to_owned());
    }

    /// Each seq `dropped` was asked with.
    pub(crate) fn dropped_asks(&self) -> Vec<u64> {
        self.dropped_asks.lock().unwrap().clone()
    }

    pub(crate) fn alab_answers(&self) {
        *self.probed.lock().unwrap() = Some(Ok(ProbeOutcome::Found(Probed {
            url: aterm_secrets::ALAB_DEFAULT_URL.into(),
            seq: 7,
            fingerprint: "0123-4567-89ab-cdef".into(),
            envelope: Vec::new(),
        })));
    }

    pub(crate) fn bootstraps_as(
        &self,
        admission: aterm_secrets::client::NativeAdmission,
        status: Option<Status>,
    ) {
        *self.bootstrap.lock().unwrap() = Some((admission, status));
    }

    pub(crate) fn stops_during_bootstrap(&self, stopped: Arc<AtomicBool>) {
        *self.stop_at_native.lock().unwrap() = Some(stopped);
    }

    pub(crate) fn turns_off_during_bootstrap(&self) {
        *self.off_at_native.lock().unwrap() = true;
    }

    fn subscribed(&self, state: aterm_secrets::Result<State>) {
        *self.subscriber.lock().unwrap() = Some(state);
    }

    pub(crate) fn probes(&self) -> u32 {
        *self.probes.lock().unwrap()
    }

    /// The LAN broker does not answer.
    pub(crate) fn broker_absent(&self) {
        *self.broker_absent.lock().unwrap() = true;
    }

    /// A join ends with `outcome`, this Mac's vault state becoming `saved` as it runs.
    pub(crate) fn joins_as(
        &self,
        saved: Option<Status>,
        outcome: aterm_secrets::Result<JoinOutcome>,
    ) {
        *self.saved_by_join.lock().unwrap() = saved;
        *self.outcome.lock().unwrap() = Some(outcome);
    }

    /// Whether each join was given a join password.
    pub(crate) fn passwords(&self) -> Vec<bool> {
        self.passwords.lock().unwrap().clone()
    }

    /// The person turns the subscriber off.
    pub(crate) fn turn_off(&self) {
        *self.turned_off.lock().unwrap() = true;
    }

    pub(crate) fn joins(&self) -> Vec<(String, bool)> {
        self.joins.lock().unwrap().clone()
    }

    pub(crate) fn installs(&self) -> u32 {
        *self.installs.lock().unwrap()
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn changes(&self) -> Vec<String> {
        self.log()
            .into_iter()
            .filter(|event| !event.starts_with("line "))
            .collect()
    }
}

/// A vault this machine is in, its subscriber installed and every file it wrote in place:
/// converged.
pub(crate) fn converged() -> Arc<Scripted> {
    let ops = Scripted::new(vec![Ok(configured(Membership::In {
        seq: 7,
        heard: 1_800_000_000,
    }))]);
    ops.subscribed(Ok(installed_state()));
    ops
}

fn installed_state() -> State {
    State::Installed {
        pid: Some(4242),
        unit: "/Users//alice/Library/LaunchAgents/systems.alab.aterm-secrets.plist".into(),
    }
}

impl SecretsOps for Scripted {
    fn prepare_bootstrap(&self) -> aterm_secrets::Result<()> {
        *self.prepared.lock().unwrap() += 1;
        Ok(())
    }

    fn native_bootstrap(
        &self,
        current: &(dyn Fn() -> aterm_secrets::Result<()> + Sync),
    ) -> aterm_secrets::Result<aterm_secrets::client::NativeAdmission> {
        current()?;
        *self.native_tries.lock().unwrap() += 1;
        if *self.off_at_native.lock().unwrap() {
            *self.turned_off.lock().unwrap() = true;
            current()?;
        }
        if let Some(stop) = self.stop_at_native.lock().unwrap().as_ref() {
            stop.store(true, Ordering::SeqCst);
            current()?;
        }
        let Some((admission, status)) = self.bootstrap.lock().unwrap().take() else {
            return Ok(aterm_secrets::client::NativeAdmission::None);
        };
        if let Some(status) = status {
            *self.statuses.lock().unwrap() = vec![Ok(status)].into();
        }
        Ok(admission)
    }
    fn status(&self) -> aterm_secrets::Result<Status> {
        let mut statuses = self.statuses.lock().unwrap();
        if statuses.len() > 1 {
            statuses.pop_front().unwrap()
        } else {
            statuses.front().cloned().expect("a scripted status")
        }
    }

    fn configured(&self) -> aterm_secrets::Result<Option<Config>> {
        let statuses = self.statuses.lock().unwrap();
        let status = statuses.front().cloned().expect("a scripted status");
        status.map(|status| status.config)
    }

    fn subscriber(&self) -> aterm_secrets::Result<State> {
        if let Some(state) = self.readings.lock().unwrap().pop_front() {
            return state;
        }
        if let Some(state) = self.subscriber.lock().unwrap().clone() {
            return state;
        }
        let installed = *self.installs.lock().unwrap() > 0
            && self.fleeting().is_none()
            && self
                .installed
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(Result::is_ok);
        Ok(if installed {
            installed_state()
        } else {
            State::Absent
        })
    }

    fn terminal(&self) -> bool {
        *self.terminal.lock().unwrap()
    }

    fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome> {
        *self.probes.lock().unwrap() += 1;
        if let Some(look) = self.looks.lock().unwrap().pop_front() {
            return look;
        }
        self.probed
            .lock()
            .unwrap()
            .clone()
            .expect("a scripted probe")
    }

    fn check(&self, source: &str) -> Result<Checked, String> {
        self.checks.lock().unwrap().push(source.to_owned());
        self.checked
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Checked::Pinned))
    }

    fn ask_members(
        &self,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<usize, String> {
        current()?;
        *self.member_asks.lock().unwrap() += 1;
        self.members_hear
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Err("no ALab member answers on this network".into()))
    }

    fn allowed(&self) -> bool {
        *self.let_in.lock().unwrap()
    }

    fn dropped(&self, seq: u64) -> Option<String> {
        self.dropped_asks.lock().unwrap().push(seq);
        self.drop_words.lock().unwrap().clone()
    }

    fn broker_unready(&self) -> Option<String> {
        self.broker_absent
            .lock()
            .unwrap()
            .then(|| aterm_uds::lan::Unavailable::NotInstalled.to_string())
    }

    fn join_place(&self) -> JoinPlace {
        self.place.lock().unwrap().unwrap_or(JoinPlace::Home)
    }

    fn swarm_enabled(&self) -> aterm_secrets::Result<bool> {
        self.swarm.lock().unwrap().clone().unwrap_or(Ok(true))
    }

    fn reachable(&self, _source: &Source) -> Look {
        *self.reached.lock().unwrap() += 1;
        if let Some((status, state)) = self.meanwhile.lock().unwrap().take() {
            *self.statuses.lock().unwrap() = vec![Ok(status)].into();
            *self.subscriber.lock().unwrap() = Some(Ok(state));
        }
        if let Some(look) = self.unreachable.lock().unwrap().clone() {
            return look;
        }
        match self.absent.lock().unwrap().clone() {
            Some(why) => Look::NoVault(why),
            None => Look::Answers,
        }
    }

    fn keep(&self, into: &Path, replaces: &Replaces) -> aterm_secrets::Result<()> {
        self.replacement_reads
            .lock()
            .unwrap()
            .push(replaces.clone());
        self.log
            .lock()
            .unwrap()
            .push(format!("keep {}", into.display()));
        match self.unkept.lock().unwrap().clone() {
            Some(why) => Err(aterm_secrets::Error::new(why)),
            None => Ok(()),
        }
    }

    fn join(
        &self,
        _sys: &dyn System,
        (source, replaces): (&str, &Replaces),
        password: Option<&SecretBytes>,
        interactive: bool,
        wait: Duration,
        requested: &dyn Fn() -> bool,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> aterm_secrets::Result<JoinOutcome> {
        current().map_err(aterm_secrets::Error::new)?;
        self.replacement_reads
            .lock()
            .unwrap()
            .push(replaces.clone());
        self.log.lock().unwrap().push(format!("join {source}"));
        self.waits.lock().unwrap().push(wait);
        self.joins
            .lock()
            .unwrap()
            .push((source.to_owned(), interactive));
        self.passwords.lock().unwrap().push(password.is_some());
        if *self.requests.lock().unwrap() {
            let answer = requested();
            self.hooked.lock().unwrap().push(answer);
        }
        self.installed_by_join.lock().unwrap().push(self.installs());
        if let Some(saved) = self.saved_by_join.lock().unwrap().take() {
            *self.statuses.lock().unwrap() = vec![Ok(saved)].into();
        }
        if *self.withdraw_at_join.lock().unwrap() {
            *self.swarm.lock().unwrap() = Some(Ok(false));
            let refused = current().expect_err("the join observes sharing off");
            *self.swarm.lock().unwrap() = Some(Ok(true));
            return Err(aterm_secrets::Error::new(refused));
        }
        self.outcome
            .lock()
            .unwrap()
            .clone()
            .expect("a scripted join")
    }

    fn install_subscriber(&self) -> aterm_secrets::Result<String> {
        self.log.lock().unwrap().push("install".into());
        *self.installs.lock().unwrap() += 1;
        if let Some(exe) = self.fleeting() {
            aterm_secrets::service::service_program(&exe, &|_| false)?;
        }
        if *self.spoiled_by_install.lock().unwrap() {
            *self.unwritten.lock().unwrap() = true;
        }
        let installed = self
            .installed
            .lock()
            .unwrap()
            .clone()
            .expect("a scripted install");
        if installed.is_ok() {
            *self.subscriber.lock().unwrap() = None;
            // As `aterm_secrets::service::install`: the subscriber installed is on
            // again, and the person's record that it was off goes.
            *self.turned_off.lock().unwrap() = false;
            if let Some(status) = self.published_by_install.lock().unwrap().take() {
                *self.statuses.lock().unwrap() = vec![Ok(status)].into();
            }
        }
        installed
    }

    fn turned_off(&self) -> bool {
        *self.turned_off.lock().unwrap()
    }

    fn started(&self) -> Result<(), String> {
        self.dies.lock().unwrap().clone().map_or(Ok(()), Err)
    }

    fn fleeting(&self) -> Option<PathBuf> {
        self.fleeting.lock().unwrap().clone()
    }

    fn synced(&self) -> aterm_secrets::Result<bool> {
        if let Some(home) = self.local_home.lock().unwrap().as_ref() {
            return LiveSecrets::new(home).synced();
        }
        Ok(!*self.unwritten.lock().unwrap())
    }

    fn sync(&self, current: &(dyn Fn() -> Result<(), String> + Sync)) -> aterm_secrets::Result<()> {
        current().map_err(aterm_secrets::Error::new)?;
        self.log.lock().unwrap().push("sync".into());
        *self.syncs.lock().unwrap() += 1;
        if let Some(home) = self.local_home.lock().unwrap().as_ref() {
            return LiveSecrets::new(home).sync(current);
        }
        *self.unwritten.lock().unwrap() = false;
        Ok(())
    }

    fn say(&self, line: &str) {
        self.said.lock().unwrap().push(line.to_owned());
    }
}

struct Lines {
    ops: Arc<Scripted>,
    refuse: bool,
}

impl crate::engine::Journal for Lines {
    fn before_change(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String> {
        if self.refuse {
            return Err("No space left on device (os error 28)".into());
        }
        self.ops.log.lock().unwrap().push(format!(
            "line {id}: {from} -> {to}; revert: {}",
            revert.unwrap_or("none")
        ));
        Ok(())
    }
}

const CODE: &str = "abcd-ef23-ghij-k234";

pub(crate) fn unconfigured() -> Status {
    Status {
        config: None,
        machine: None,
        membership: Membership::Unconfigured,
        names: Vec::new(),
    }
}

pub(crate) fn configured(membership: Membership) -> Status {
    Status {
        config: Some(Config {
            source: Source::Url(aterm_secrets::ALAB_DEFAULT_URL.into()),
            mirrors: Vec::new(),
            name: "alab".into(),
            signer: Some([3; 32]),
            followed: Vec::new(),
            host: "m22".into(),
        }),
        machine: Some((Recipient([7; 32]), CODE.into())),
        membership,
        names: vec![
            "gh_access_token".into(),
            "gh_access_token_alabsystems".into(),
            "~/.aterm/release-credentials.toml".into(),
        ],
    }
}

fn fake() -> Fake {
    let mut sys = Fake::root();
    sys.euid = 501;
    sys
}

#[test]
fn default_native_admission_precedes_discovery_and_requires_complete_readback() {
    use aterm_secrets::client::NativeAdmission;
    for (mode, incomplete) in [
        (Mode::Plan, false),
        (Mode::Apply, false),
        (Mode::Apply, true),
    ] {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.bootstrap.lock().unwrap() = Some((
            NativeAdmission::Admitted,
            Some(configured(Membership::In { seq: 7, heard: 0 })),
        ));
        *ops.unwritten.lock().unwrap() = incomplete;
        let rows = run(&sys, &ops, mode);
        let want = if mode == Mode::Plan {
            Outcome::NotApplicable
        } else if incomplete {
            Outcome::Failed
        } else {
            Outcome::Changed
        };
        assert_eq!(rows[0].outcome, want, "{mode:?}/{incomplete}: {rows:?}");
        assert_eq!(ops.probes(), 0, "native admission preceded vault discovery");
        assert!(ops.joins().is_empty());
        assert_eq!(*ops.reached.lock().unwrap(), 0, "no HTTP preflight");
        assert_eq!(
            *ops.native_tries.lock().unwrap(),
            u32::from(mode == Mode::Apply)
        );
        assert_eq!(
            *ops.prepared.lock().unwrap(),
            u32::from(mode == Mode::Apply)
        );
        assert_eq!(ops.installs(), u32::from(mode == Mode::Apply));
    }
}

#[test]
fn ambiguous_native_admission_starts_its_subscriber_without_another_enrollment() {
    use aterm_secrets::client::NativeAdmission;
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.bootstrap.lock().unwrap() = Some((NativeAdmission::Uncertain, None));
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert!(
        matches!(&rows[0].stood_aside, Some(StoodAside::Waiting(why)) if why.starts_with(NATIVE_ADMISSION_PENDING))
    );
    assert_eq!(ops.probes(), 0);
    assert_eq!(*ops.reached.lock().unwrap(), 0);
    assert!(ops.joins().is_empty());
    assert_eq!(ops.installs(), 1);
}

#[test]
fn a_late_stop_reaches_native_publication_and_prevents_fallback() {
    let sys = fake();
    let context = ctx(&[user("alice", 501)]);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let stopped = Arc::new(AtomicBool::new(false));
    *ops.stop_at_native.lock().unwrap() = Some(Arc::clone(&stopped));
    let rows = crate::init::run_initial_secrets_guarded(
        &sys,
        &context,
        ops.clone(),
        aterm_secrets::ALAB_DEFAULT_URL,
        Replaces::Read(None),
        &mut None,
        Duration::ZERO,
        &|| {
            if stopped.load(Ordering::SeqCst) {
                Err("setup stopped during native proof".into())
            } else {
                Ok(())
            }
        },
    )
    .unwrap();
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert!(
        matches!(&rows[0].stood_aside, Some(StoodAside::Waiting(why)) if why.contains("setup stopped"))
    );
    assert_eq!(*ops.native_tries.lock().unwrap(), 1);
    assert_eq!(ops.probes(), 0);
    assert_eq!(*ops.reached.lock().unwrap(), 0);
    assert!(ops.joins().is_empty());
    assert_eq!(ops.installs(), 0);
}

#[test]
fn a_source_chosen_during_native_lookup_is_never_replaced_by_default_fallback() {
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let mut chosen = configured(Membership::Pending);
    chosen.config.as_mut().unwrap().source = Source::Url("https://chosen.example/v1/alab".into());
    *ops.bootstrap.lock().unwrap() = Some((
        aterm_secrets::client::NativeAdmission::None,
        Some(chosen.clone()),
    ));
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    assert_eq!(ops.status().unwrap().config, chosen.config);
    assert_eq!(ops.probes(), 0);
    assert!(ops.joins().is_empty());
    assert_eq!(ops.installs(), 0);
}

fn run(sys: &Fake, ops: &Arc<Scripted>, mode: Mode) -> Vec<ItemReport> {
    run_with(sys, ops, mode, None)
}

/// [`run`], its journal lines in `ops`'s log ([`Lines`]).
fn run_journaled(sys: &Fake, ops: &Arc<Scripted>, mode: Mode) -> Vec<ItemReport> {
    let context = ctx(&[user("alice", 501)]);
    let port: Arc<dyn SecretsOps> = ops.clone();
    let step = items(sys, &context, port, mode, None).expect("items");
    let mut journal = Lines {
        ops: Arc::clone(ops),
        refuse: false,
    };
    engine::run(&step.items, mode, sys, &context, &mut journal)
}

/// [`run`] with `--vault` naming `vault`.
fn run_with(
    sys: &dyn System,
    ops: &Arc<Scripted>,
    mode: Mode,
    vault: Option<&str>,
) -> Vec<ItemReport> {
    let context = ctx(&[user("alice", 501)]);
    let port: Arc<dyn SecretsOps> = ops.clone();
    let step = items(sys, &context, port, mode, vault).expect("items");
    assert_eq!(step.items.len(), 1);
    engine::run(&step.items, mode, sys, &context, &mut NoJournal)
}

fn assert_report_matches_current(sys: &Fake, ops: &Arc<Scripted>, row: &ItemReport) {
    let current = run(sys, ops, Mode::Plan);
    assert_eq!(current[0].outcome, Outcome::Ok);
    assert_eq!(
        row.to, current[0].to,
        "the applied report names its read-back"
    );
}

fn asked(sys: &Fake) -> usize {
    sys.calls().iter().filter(|c| c.starts_with("ask ")).count()
}

/// A Fake whose person presses Ctrl-C at the prompt `stop`, or, with `sigint`, during any
/// wait ([`System::stopped`]). A Ctrl-C nothing catches ([`System::catch_stops`]) ends the
/// process, so a wait that reads one uncaught fails the test.
struct StopsAt {
    inner: Fake,
    stop: &'static str,
    sigint: bool,
}

impl System for StopsAt {
    fn ask(&self, prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
        if !self.stop.is_empty() && prompt.starts_with(self.stop) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.inner.ask(prompt, secret)
    }
    fn catch_stops(&self) -> Box<dyn std::any::Any> {
        self.inner.catch_stops()
    }
    fn stopped(&self) -> Option<i32> {
        let calls = self.inner.calls();
        let held = |call: &str| calls.iter().filter(|c| *c == call).count();
        assert!(
            !self.sigint || held("catch stops") > held("stops restored"),
            "an uncaught Ctrl-C ends the process"
        );
        self.sigint.then_some(libc::SIGINT)
    }
    fn read_owned_file(
        &self,
        path: &Path,
        uid: u32,
        max: usize,
        private: bool,
    ) -> io::Result<Vec<u8>> {
        self.inner.read_owned_file(path, uid, max, private)
    }
    fn run(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        self.inner.run(cmd)
    }
    fn run_tty(
        &self,
        cmd: &Cmd,
        before: &mut dyn FnMut(&mut Cmd) -> Result<(), RunFailed>,
    ) -> Result<Option<i32>, RunFailed> {
        self.inner.run_tty(cmd, before)
    }
    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.inner.read_file(path)
    }
    fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        self.inner.write_file(path, bytes, mode)
    }
    fn write_user_file(&self, user: &User, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        self.inner.write_user_file(user, path, bytes, mode)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        self.inner.remove(path)
    }
    fn create_dir_all(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.inner.create_dir_all(path, mode)
    }
    fn chown(&self, path: &Path, uid: u32, gid: u32) -> io::Result<()> {
        self.inner.chown(path, uid, gid)
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        self.inner.stat(path)
    }
    fn list_dir(&self, path: &Path) -> io::Result<Vec<String>> {
        self.inner.list_dir(path)
    }
    fn sha256(&self, path: &Path) -> io::Result<String> {
        self.inner.sha256(path)
    }
    fn copy_file(&self, from: &Path, to: &Path, mode: u32) -> io::Result<()> {
        self.inner.copy_file(from, to, mode)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn euid(&self) -> u32 {
        self.inner.euid()
    }
    fn now(&self) -> u64 {
        self.inner.now()
    }
}

#[test]
fn a_mac_already_in_the_vault_is_ok_without_a_prompt_or_a_join() {
    for mode in [Mode::Apply, Mode::Plan] {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(configured(Membership::In {
            seq: 7,
            heard: 1_800_000_000,
        }))]);
        ops.subscribed(Ok(installed_state()));
        let rows = run(&sys, &ops, mode);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        assert_eq!(rows[0].id, "init.secrets@alice");
        assert_eq!(rows[0].family, "secrets");
        assert_eq!(
            rows[0].to.as_deref(),
            Some(
                "in vault alab (seq 7): 2 secrets in ~/.secrets and 1 elsewhere in ~, kept in sync by the subscriber"
            )
        );
        assert_eq!(asked(&sys), 0);
        assert_eq!(ops.probes(), 0);
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);
    }
}

#[test]
fn a_mac_in_the_vault_whose_files_are_not_all_written_is_a_change_that_syncs() {
    for mode in [Mode::Plan, Mode::Apply] {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(configured(Membership::In {
            seq: 7,
            heard: 1_800_000_000,
        }))]);
        ops.subscribed(Ok(installed_state()));
        *ops.unwritten.lock().unwrap() = true;
        let rows = run(&sys, &ops, mode);
        let want = if mode == Mode::Plan {
            Outcome::Planned
        } else {
            Outcome::Changed
        };
        assert_eq!(rows[0].outcome, want, "{rows:?}");
        assert_eq!(
            rows[0].from.as_deref(),
            Some("in vault alab (seq 7), but a file it wrote failed, is missing or was changed")
        );
        if mode == Mode::Plan {
            assert_eq!(rows[0].to.as_deref(), Some(WRITTEN));
        } else {
            assert_report_matches_current(&sys, &ops, &rows[0]);
        }
        assert_eq!(*ops.syncs.lock().unwrap(), u32::from(mode == Mode::Apply));
        assert!(ops.joins().is_empty(), "nothing is joined again");
        assert_eq!(
            ops.installs(),
            0,
            "the subscriber that runs is left as it runs"
        );
    }
    // Its journal line undoes only the sync: no `agent off` of a subscriber it never touched.
    let sys = fake();
    let ops = Scripted::new(vec![Ok(configured(Membership::In {
        seq: 7,
        heard: 1_800_000_000,
    }))]);
    ops.subscribed(Ok(installed_state()));
    *ops.unwritten.lock().unwrap() = true;
    let rows = run_journaled(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(ops.changes(), ["sync"]);
    journaled_first(&ops.log());
    assert!(!ops.log()[0].contains("agent off"), "{:?}", ops.log());
}

fn configured_alab(membership: Membership) -> Status {
    let mut status = configured(membership);
    status.config.as_mut().unwrap().signer = Some(
        aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
            .unwrap()
            .try_into()
            .unwrap(),
    );
    status
}

#[test]
fn configured_settle_and_a_default_published_after_probe_retain_current_authority() {
    for published_late in [false, true] {
        for withdraw in [false, true] {
            let pending = configured_alab(Membership::Pending);
            let ops = Scripted::new(vec![Ok(if published_late {
                unconfigured()
            } else {
                pending.clone()
            })]);
            let context = ctx(&[user("alice", 501)]);
            let vault = selected(
                &context,
                ops.clone(),
                aterm_secrets::ALAB_DEFAULT_URL,
                Duration::ZERO,
                Replaces::Read(None),
            )
            .unwrap()
            .keeping_off();
            assert!(matches!(
                vault.probe(&fake(), &context),
                Probe::Change { .. }
            ));
            if published_late {
                *ops.statuses.lock().unwrap() = vec![Ok(pending)].into();
            }
            let current = || {
                if withdraw && ops.installs() > 0 {
                    Err("setup stopped during subscriber installation".into())
                } else {
                    Ok(())
                }
            };
            let result = vault.apply_guarded(&fake(), &context, &current);
            assert_eq!(
                result.is_err(),
                withdraw,
                "late={published_late}, withdraw={withdraw}: {result:?}"
            );
            assert!(
                ops.joins().is_empty(),
                "the already-configured vault is settled"
            );
            assert_eq!(ops.installs(), 1);
            assert_eq!(*ops.member_asks.lock().unwrap(), u32::from(!withdraw));
            assert_eq!(*ops.syncs.lock().unwrap(), 0);
        }
    }
}

#[test]
fn a_selected_join_never_recovers_its_first_observed_sharing_refusal() {
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    ops.joins_as(
        Some(configured(Membership::Pending)),
        Ok(JoinOutcome::Pending {
            code: "fixture-code".into(),
            host: "alice".into(),
        }),
    );
    *ops.withdraw_at_join.lock().unwrap() = true;
    let row = setup_pass(&ops, aterm_secrets::ALAB_DEFAULT_URL, &mut None, false);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert!(
        ops.swarm_enabled().unwrap(),
        "the later observation has recovered"
    );
    assert_eq!(ops.joins().len(), 1);
    assert_eq!(
        ops.installs(),
        0,
        "the refused attempt must not install its saved partial join"
    );
    assert_eq!(*ops.member_asks.lock().unwrap(), 0);
    assert_eq!(*ops.syncs.lock().unwrap(), 0);
}

#[test]
fn a_configured_automatic_repair_holds_a_sharing_off_since_its_probe() {
    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    let context = ctx(&[user("alice", 501)]);
    let vault = Vault::new(
        user("alice", 501),
        ops.clone(),
        aterm_secrets::ALAB_DEFAULT_URL.into(),
        Origin::Configured,
        None,
        Duration::ZERO,
    );
    assert!(matches!(
        vault.probe(&fake(), &context),
        Probe::Change { .. }
    ));
    *ops.swarm.lock().unwrap() = Some(Ok(false));
    assert!(vault.apply_guarded(&fake(), &context, &|| Ok(())).is_err());
    assert_eq!(ops.installs(), 0);
    assert_eq!(*ops.syncs.lock().unwrap(), 0);
    assert_eq!(*ops.member_asks.lock().unwrap(), 0);
    assert!(ops.joins().is_empty());
}

#[test]
fn settle_keeps_the_observed_source_but_accepts_its_trusted_pin_successor() {
    for configured_origin in [false, true] {
        for rotated in [false, true] {
            let ops = Scripted::new(vec![Ok(configured_alab(Membership::Pending))]);
            let vault = if configured_origin {
                Vault::new(
                    user("alice", 501),
                    ops.clone(),
                    aterm_secrets::ALAB_DEFAULT_URL.into(),
                    Origin::Configured,
                    None,
                    Duration::ZERO,
                )
            } else {
                selected(
                    &ctx(&[user("alice", 501)]),
                    ops.clone(),
                    aterm_secrets::ALAB_DEFAULT_URL,
                    Duration::ZERO,
                    Replaces::Any,
                )
                .unwrap()
            };
            let context = ctx(&[user("alice", 501)]);
            assert!(matches!(
                vault.probe(&fake(), &context),
                Probe::Change { .. }
            ));
            let changed = AtomicBool::new(false);
            let current = || {
                if ops.installs() > 0 && !changed.swap(true, Ordering::SeqCst) {
                    let mut statuses = ops.statuses.lock().unwrap();
                    let config = statuses
                        .front_mut()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .config
                        .as_mut()
                        .unwrap();
                    if rotated {
                        config.followed.push(config.signer.unwrap());
                        config.signer = Some([0x55; 32]);
                    } else {
                        config.source = Source::Url("https://chosen.example/v1/alab".into());
                    }
                }
                Ok(())
            };
            let result = vault.apply_guarded(&fake(), &context, &current);
            assert_eq!(
                result.is_ok(),
                rotated,
                "configured={configured_origin}, rotation={rotated}: {result:?}"
            );
            assert_eq!(*ops.member_asks.lock().unwrap(), u32::from(rotated));
            assert_eq!(*ops.syncs.lock().unwrap(), 0);
            assert!(ops.joins().is_empty());
        }
    }
}

#[test]
fn configured_settle_rechecks_after_the_real_http_response_before_materializing() {
    use std::io::{BufRead, Write};
    for withdraw in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("member"));
        let source = root.path().join("team.vault");
        let created =
            aterm_secrets::client::create(&paths, source.to_str().unwrap(), None, Some("member"))
                .unwrap();
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let mut document = admin.doc.clone();
        document
            .set_entry(
                &aterm_secrets::text::Audience::Everyone,
                "settle-fixture",
                b"fixture-value",
            )
            .unwrap();
        aterm_secrets::client::admin_publish(&paths, &admin, &document).unwrap();
        let cache_file = paths.dir.join("vault");
        let state_file = paths.dir.join("state.toml");
        let bytes = std::fs::read(&cache_file).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/team#{}",
            listener.local_addr().unwrap(),
            created.signer
        );
        let previous = aterm_secrets::client::config(&paths).unwrap();
        aterm_secrets::client::join_if_unchanged(
            &paths,
            &aterm_secrets::client::JoinOptions {
                source: url.clone(),
                host: Some("member".into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: aterm_secrets::client::JoinPassword::Given(None),
                fetched: Some(&bytes),
                requested: &|| {},
                say: &|_| {},
            },
            previous.as_ref(),
        )
        .unwrap();
        let target = paths.home.join(".secrets/settle-fixture");
        std::fs::write(&target, b"preserved-local-value").unwrap();
        let state = std::fs::read(&state_file).unwrap();
        let cache = std::fs::read(&cache_file).unwrap();
        let ops = Scripted::new(vec![Ok(aterm_secrets::client::status(&paths).unwrap())]);
        *ops.local_home.lock().unwrap() = Some(paths.home.clone());
        ops.subscribed(Ok(installed_state()));
        let context = ctx(&[user("alice", 501)]);
        let vault = selected(&context, ops.clone(), &url, Duration::ZERO, Replaces::Any).unwrap();
        assert!(matches!(
            vault.probe(&fake(), &context),
            Probe::Change { .. }
        ));
        let stopped = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let stopped = &stopped;
            scope.spawn(move || {
                let (stream, _) =
                    aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking)
                        .unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(stream);
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut stream = reader.into_inner();
                stopped.store(withdraw, Ordering::SeqCst);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                stream.write_all(&bytes).unwrap();
            });
            let current = || {
                if stopped.load(Ordering::SeqCst) {
                    Err("setup stopped while the vault response was in flight".into())
                } else {
                    Ok(())
                }
            };
            let result = vault.apply_guarded(&fake(), &context, &current);
            assert_eq!(result.is_err(), withdraw, "{result:?}");
        });
        assert_eq!(*ops.syncs.lock().unwrap(), 1);
        assert!(ops.joins().is_empty());
        if withdraw {
            assert_eq!(std::fs::read(&target).unwrap(), b"preserved-local-value");
            assert_eq!(std::fs::read(&state_file).unwrap(), state);
            assert_eq!(std::fs::read(&cache_file).unwrap(), cache);
        } else {
            assert_eq!(std::fs::read(&target).unwrap(), b"fixture-value");
            assert!(aterm_secrets::client::materialization_ready(&paths).unwrap());
        }
    }
}

fn legacy_lock_setup() -> (aterm_tempfile::TempDir, Paths, Arc<Scripted>) {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let source = root.path().join("team.vault");
    aterm_secrets::client::create(&paths, source.to_str().unwrap(), None, Some("member")).unwrap();
    assert!(aterm_secrets::client::materialization_ready(&paths).unwrap());
    let lock = paths.dir.join(".lock");
    std::fs::write(&lock, b"retained lock inode").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    let status = aterm_secrets::client::status(&paths).unwrap();
    assert!(matches!(status.membership, Membership::In { .. }));
    let ops = Scripted::new(vec![Ok(status)]);
    *ops.local_home.lock().unwrap() = Some(paths.home.clone());
    (root, paths, ops)
}

#[test]
fn an_owned_legacy_lock_and_absent_subscriber_are_repaired_only_by_apply() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let (_root, paths, ops) = legacy_lock_setup();
    let lock = paths.dir.join(".lock");
    let before = std::fs::metadata(&lock).unwrap();
    assert!(aterm_secrets::client::materialization_ready(&paths).is_err());
    assert_eq!(ops.subscriber().unwrap(), State::Absent);
    let planned = run(&fake(), &ops, Mode::Plan);
    assert_eq!(planned[0].outcome, Outcome::Planned, "{planned:?}");
    assert_eq!(ops.installs(), 0);
    assert_eq!(*ops.syncs.lock().unwrap(), 0);
    assert_eq!(std::fs::metadata(&lock).unwrap().mode() & 0o7777, 0o644);
    assert_eq!(std::fs::read(&lock).unwrap(), b"retained lock inode");

    let applied = run(&fake(), &ops, Mode::Apply);
    assert_eq!(applied[0].outcome, Outcome::Changed, "{applied:?}");
    assert_eq!(ops.installs(), 1);
    assert_eq!(*ops.syncs.lock().unwrap(), 1);
    let after = std::fs::metadata(&lock).unwrap();
    assert_eq!(after.ino(), before.ino());
    assert_eq!(after.permissions().mode() & 0o7777, 0o600);
    assert_eq!(std::fs::read(&lock).unwrap(), b"retained lock inode");
    assert!(aterm_secrets::client::materialization_ready(&paths).unwrap());
    assert_eq!(run(&fake(), &ops, Mode::Plan)[0].outcome, Outcome::Ok);
    assert!(ops.joins().is_empty());
}

/// A vault pass holding the state lock (the subscriber takes it for its pass over every long-poll
/// answer) says nothing of whether the vault's files are written: `init.secrets` waits it out and
/// reads the files as written, so neither its probe nor its apply syncs the vault again.
/// NEGATIVE CONTROL (2026-11-12): with `aterm_secrets::client::materialization_ready` answering a
/// held lock at once, the probe reads the files unwritten and plans a sync.
#[test]
fn a_vault_pass_holding_the_lock_leaves_the_files_written() {
    use aterm_secrets::client::{test_hold_state_lock, test_while_state_lock_held};
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let source = root.path().join("team.vault");
    aterm_secrets::client::create(&paths, source.to_str().unwrap(), None, Some("member")).unwrap();
    let status = aterm_secrets::client::status(&paths).unwrap();
    let ops = Scripted::new(vec![Ok(status)]);
    *ops.local_home.lock().unwrap() = Some(paths.home.clone());
    assert_eq!(run(&fake(), &ops, Mode::Apply)[0].outcome, Outcome::Changed);
    assert_eq!(ops.installs(), 1);
    for mode in [Mode::Plan, Mode::Apply] {
        let mut held = Some(test_hold_state_lock(&paths).unwrap());
        let (reports, tries) =
            test_while_state_lock_held(move || drop(held.take()), || run(&fake(), &ops, mode));
        assert_eq!(reports[0].outcome, Outcome::Ok, "{mode:?}: {reports:?}");
        assert!(
            tries >= 1,
            "{mode:?}: the read met the held pass and waited"
        );
    }
    assert_eq!(*ops.syncs.lock().unwrap(), 0);
}

#[test]
fn legacy_lock_setup_preserves_off_and_refuses_hostile_locks() {
    use std::os::unix::fs::PermissionsExt;
    let (_root, paths, ops) = legacy_lock_setup();
    let lock = paths.dir.join(".lock");
    ops.turn_off();
    let rows = run(&fake(), &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(ops.installs(), 0);
    assert_eq!(*ops.syncs.lock().unwrap(), 0);
    assert_eq!(
        std::fs::metadata(&lock).unwrap().permissions().mode() & 0o7777,
        0o644
    );

    for hostile in ["symlink", "hardlink", "read-only"] {
        let (_root, paths, ops) = legacy_lock_setup();
        let lock = paths.dir.join(".lock");
        let other = paths.dir.join("other-lock");
        match hostile {
            "symlink" => {
                std::fs::rename(&lock, &other).unwrap();
                std::os::unix::fs::symlink(&other, &lock).unwrap();
            }
            "hardlink" => std::fs::hard_link(&lock, &other).unwrap(),
            _ => std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444)).unwrap(),
        }
        let mode = std::fs::metadata(&lock).unwrap().permissions().mode();
        let rows = run(&fake(), &ops, Mode::Apply);
        assert!(rows[0].outcome.is_failure(), "{hostile}: {rows:?}");
        assert_eq!(ops.installs(), 0, "{hostile}");
        assert_eq!(*ops.syncs.lock().unwrap(), 0, "{hostile}");
        assert_eq!(std::fs::read(&lock).unwrap(), b"retained lock inode");
        assert_eq!(std::fs::metadata(&lock).unwrap().permissions().mode(), mode);
    }
}

#[test]
fn a_configured_mac_without_its_subscriber_is_a_change_that_only_installs_it() {
    for mode in [Mode::Plan, Mode::Apply] {
        let sys = fake();
        let mut status = configured(Membership::In { seq: 7, heard: 0 });
        status.names.truncate(1);
        let ops = Scripted::new(vec![Ok(status)]);
        let rows = run(&sys, &ops, mode);
        let want = if mode == Mode::Plan {
            Outcome::Planned
        } else {
            Outcome::Changed
        };
        assert_eq!(rows[0].outcome, want, "{rows:?}");
        assert_eq!(
            rows[0].from.as_deref(),
            Some("in vault alab (seq 7), subscriber not installed")
        );
        if mode == Mode::Plan {
            assert_eq!(rows[0].to.as_deref(), Some(SUBSCRIBED));
        } else {
            assert_report_matches_current(&sys, &ops, &rows[0]);
        }
        assert_eq!(asked(&sys), 0);
        assert_eq!(ops.probes(), 0);
        assert!(ops.joins().is_empty(), "nothing is joined again");
        assert_eq!(ops.installs(), u32::from(mode == Mode::Apply));
    }

    for mode in [Mode::Plan, Mode::Apply] {
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        let loaded = State::Installed {
            pid: None,
            unit: "/Users//alice/Library/LaunchAgents/systems.alab.aterm-secrets.plist".into(),
        };
        ops.subscribed(Ok(loaded));
        let sys = fake();
        let rows = run(&sys, &ops, mode);
        let want = if mode == Mode::Plan {
            Outcome::Planned
        } else {
            Outcome::Changed
        };
        assert_eq!(rows[0].outcome, want, "{rows:?}");
        assert_eq!(
            rows[0].from.as_deref(),
            Some("in vault alab (seq 7), subscriber installed but not running")
        );
        if mode == Mode::Plan {
            assert_eq!(rows[0].to.as_deref(), Some(SUBSCRIBED));
        } else {
            assert_report_matches_current(&sys, &ops, &rows[0]);
        }
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), u32::from(mode == Mode::Apply));
    }

    let sys = fake();
    let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    let why = rows[0].why.as_deref().unwrap();
    assert!(
        why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
        "{why}"
    );
    assert!(!why.contains("admin"), "{why}");
    assert!(ops.joins().is_empty());
    assert_eq!(ops.installs(), 1);

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    *ops.installed.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
        "/bin/launchctl bootstrap: exit 5",
    )));
    let rows = run(&fake(), &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    assert_eq!(
        rows[0].why.as_deref(),
        Some("the subscriber could not be installed: /bin/launchctl bootstrap: exit 5")
    );

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    ops.subscribed(Err(aterm_secrets::Error::new(
        "/bin/launchctl: hung for 30 s; killed",
    )));
    let rows = run(&fake(), &ops, Mode::Plan);
    assert_eq!(rows[0].outcome, Outcome::Unknown, "{rows:?}");
    assert!(
        rows[0]
            .why
            .as_deref()
            .unwrap()
            .starts_with("cannot read whether the subscriber runs")
    );
}

#[test]
fn off_the_alab_lan_init_asks_nothing_and_joins_nothing() {
    for probed in [
        Ok(ProbeOutcome::Absent),
        Ok(ProbeOutcome::NotThisVault(
            "envelope: signed by a key the pin does not endorse".into(),
        )),
        Err(aterm_secrets::Error::new(
            "fetching http://alab-secrets.local:7484/v1/alab: dns: no answer within 8000ms",
        )),
    ] {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let answered = matches!(
            &probed,
            Ok(ProbeOutcome::Absent | ProbeOutcome::NotThisVault(_))
        );
        *ops.probed.lock().unwrap() = Some(probed);
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(
            match &rows[0].stood_aside {
                Some(StoodAside::Absent(heard)) => answered && heard.starts_with(NO_VAULT_NOW),
                Some(StoodAside::Offline(heard)) => !answered && heard.starts_with(UNANSWERED),
                _ => false,
            },
            "{rows:?}"
        );
        assert_eq!(asked(&sys), 0, "{:?}", sys.calls());
        assert_eq!(ops.probes(), 1);
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 1);
        assert_eq!(*ops.prepared.lock().unwrap(), 1);
    }

    // Control: where the ALab vault answers, it is joined, and nothing is asked either.
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    run(&sys, &ops, Mode::Apply);
    assert_eq!(asked(&sys), 0, "{:?}", sys.calls());
    assert_eq!(ops.joins().len(), 1);
}

/// The step for `here`, run once in `mode` for alice.
fn run_at(sys: &Fake, ops: &Arc<Scripted>, mode: Mode, here: Here) -> Vec<ItemReport> {
    let context = ctx(&[user("alice", 501)]);
    let port: Arc<dyn SecretsOps> = ops.clone();
    let step = items_at(sys, &context, port, mode, None, here).expect("items");
    assert_eq!(step.items.len(), 1);
    engine::run(&step.items, mode, sys, &context, &mut NoJournal)
}

/// Design §7: "The vault step never asks for the password." Where the open ALab vault
/// answers, `aterm init` at the console joins it with no question; the join password stays
/// for `aterm secrets join --password-stdin` and System State's field.
#[test]
fn the_open_alab_vault_is_joined_at_the_console_with_no_question() {
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    run(&sys, &ops, Mode::Apply);
    assert_eq!(ops.joins().len(), 1, "the open ALab vault is joined");
    assert_eq!(*ops.passwords.lock().unwrap(), [false]);
    assert_eq!(
        asked(&sys),
        0,
        "no question at the console: {:?}",
        sys.calls()
    );
}

/// Design §7: off the console the vault's join still runs, as a background row that asks
/// nothing: in an agent's tab or with no terminal, and in a remote shell.
#[test]
fn off_the_console_the_open_alab_vault_is_joined_asking_nothing() {
    for here in [Here::Session, Here::Remote] {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let after = configured(Membership::In {
            seq: 8,
            heard: 1_800_000_000,
        });
        let outcome = JoinOutcome::Joined {
            seq: 8,
            names: after.names.clone(),
        };
        ops.joins_as(Some(after), Ok(outcome));
        *ops.terminal.lock().unwrap() = false;
        ops.alab_answers();
        let rows = run_at(&sys, &ops, Mode::Apply, here);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{here:?}: {rows:?}");
        assert_eq!(
            ops.joins(),
            [(aterm_secrets::ALAB_DEFAULT_URL.to_owned(), false)],
            "{here:?}: joined, and the join asks nothing"
        );
        assert_eq!(asked(&sys), 0, "{here:?}: {:?}", sys.calls());
    }

    // A plan off the console looks for nothing and joins nothing.
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    let rows = run_at(&sys, &ops, Mode::Plan, Here::Remote);
    assert_eq!(rows[0].why.as_deref(), Some(UNCONFIGURED), "{rows:?}");
    assert_eq!(ops.probes(), 0);
}

/// Every LAN act of aterm's on a Mac goes through the root LAN broker (or, before one is
/// installed, the allowed direct path, which no test has), so without either no look for the
/// ALab vault runs. The local bootstrap intent and its running subscriber retain the join
/// for the broker's arrival; with the broker there, the pass looks and joins immediately.
#[test]
fn without_the_lan_broker_no_look_runs_and_the_row_waits_for_it() {
    for here in [Here::Person, Here::Session, Here::Remote, Here::Unbidden] {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.terminal.lock().unwrap() = false;
        ops.broker_absent();
        ops.alab_answers();
        let rows = run_at(&sys, &ops, Mode::Apply, here);
        assert_eq!(
            *ops.prepared.lock().unwrap(),
            1,
            "{here:?}: bootstrap intent"
        );
        assert_eq!(ops.installs(), 1, "{here:?}: recovery owner");
        assert!(idle(&ops.subscriber().unwrap()).is_none(), "{here:?}");
        assert_eq!(ops.probes(), 0, "{here:?}: no look");
        assert_eq!(
            *ops.native_tries.lock().unwrap(),
            0,
            "{here:?}: no native ask"
        );
        assert_eq!(*ops.reached.lock().unwrap(), 0, "{here:?}: no HTTP");
        assert!(ops.joins().is_empty(), "{here:?}");
        assert_eq!(asked(&sys), 0, "{here:?}: {:?}", sys.calls());
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{here:?}: {rows:?}");
        assert!(waits_for_lan_broker(&rows[0]), "{here:?}");
        assert!(finishes_by_itself(&rows[0]), "{here:?}: {rows:?}");
    }

    // Neither a failed installation nor a failed start promises an autonomous finish.
    for fails_to_start in [false, true] {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.broker_absent();
        if fails_to_start {
            *ops.dies.lock().unwrap() = Some("subscriber ended during startup".into());
        } else {
            *ops.installed.lock().unwrap() =
                Some(Err(aterm_secrets::Error::new("subscriber install failed")));
        }
        let rows = run_at(&fake(), &ops, Mode::Apply, Here::Session);
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        assert!(!finishes_by_itself(&rows[0]), "{rows:?}");
    }

    // A source that cannot be kept, or an owner that ended after the probe, remains offline.
    for fleeting in [false, true] {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.broker_absent();
        if fleeting {
            *ops.fleeting.lock().unwrap() =
                Some("/Volumes/aterm/aterm.app/Contents/MacOS/aterm".into());
        } else {
            *ops.readings.lock().unwrap() = [Ok(installed_state()), Ok(State::Absent)].into();
        }
        let rows = run_at(&fake(), &ops, Mode::Apply, Here::Session);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        assert!(matches!(rows[0].stood_aside, Some(StoodAside::Offline(_))));
        assert!(!finishes_by_itself(&rows[0]), "{rows:?}");
    }

    // The installed owner can finish before its start is read back. Its admitted default
    // uses the full ordinary probe; a competing source never becomes this run's success.
    for same_default in [false, true] {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.broker_absent();
        let mut after = configured_alab(Membership::In { seq: 7, heard: 0 });
        if !same_default {
            let config = after.config.as_mut().unwrap();
            config.name = "other".into();
            config.source = Source::Url("https://other.example/v1/other".into());
        }
        *ops.published_by_install.lock().unwrap() = Some(after);
        let rows = run_at(&fake(), &ops, Mode::Apply, Here::Session);
        assert_eq!(
            rows[0].outcome,
            if same_default {
                Outcome::Changed
            } else {
                Outcome::Failed
            },
            "{rows:?}",
        );
        assert_eq!(ops.installs(), 1);
        assert_eq!(ops.probes(), 0);
        assert_eq!(*ops.native_tries.lock().unwrap(), 0);
        assert_eq!(*ops.reached.lock().unwrap(), 0);
        assert!(ops.joins().is_empty());
        assert!(!finishes_by_itself(&rows[0]), "{rows:?}");
    }

    let sys = fake();
    let context = ctx(&[user("alice", 501)]);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.broker_absent();
    let item = selected(
        &context,
        ops.clone(),
        aterm_secrets::ALAB_DEFAULT_URL,
        Duration::ZERO,
        Replaces::Read(None),
    )
    .unwrap();
    assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
    let stopped = item.apply_guarded(&sys, &context, &|| {
        if ops.installs() == 0 {
            Ok(())
        } else {
            Err("late Stop".into())
        }
    });
    assert_eq!(ops.installs(), 1);
    assert!(stopped.is_err());
    assert!(item.stood_aside().is_none());
    assert_eq!(*ops.native_tries.lock().unwrap(), 0);
    assert_eq!(*ops.reached.lock().unwrap(), 0);
    assert!(ops.joins().is_empty());

    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    let rows = run_at(&sys, &ops, Mode::Apply, Here::Session);
    assert!(!waits_for_lan_broker(&rows[0]), "{rows:?}");
    assert_eq!(ops.probes(), 1);
    assert_eq!(ops.joins().len(), 1);
}

/// The vault this Mac is already in may be on its LAN too (the ALab vault's `.local`
/// address): without the LAN broker a sync that would reach it at its URL does not, wherever
/// the run is, and the row waits for the broker as the look does. A check reads this Mac's
/// own records there, as anywhere, and says what a sync would change. NEGATIVE CONTROL: with
/// the broker there, every run syncs it; with no `holding_for_lan` on the configured vault,
/// the runs without it sync too.
#[test]
fn without_the_lan_broker_the_vault_this_mac_is_in_waits_for_it() {
    for here in [Here::Person, Here::Session, Here::Remote, Here::Unbidden] {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(configured(Membership::In {
            seq: 7,
            heard: 1_800_000_000,
        }))]);
        ops.subscribed(Ok(installed_state()));
        *ops.unwritten.lock().unwrap() = true;
        ops.broker_absent();
        let rows = run_at(&sys, &ops, Mode::Plan, here);
        assert_eq!(*ops.syncs.lock().unwrap(), 0, "{here:?}: {rows:?}");
        assert_ne!(rows[0].outcome, Outcome::Deferred, "{here:?}: {rows:?}");
        assert_ne!(rows[0].why.as_deref(), Some(absent().as_str()), "{here:?}");
        for broker in [false, true] {
            let sys = fake();
            let ops = Scripted::new(vec![Ok(configured(Membership::In {
                seq: 7,
                heard: 1_800_000_000,
            }))]);
            ops.subscribed(Ok(installed_state()));
            *ops.unwritten.lock().unwrap() = true;
            if !broker {
                ops.broker_absent();
            }
            let rows = run_at(&sys, &ops, Mode::Apply, here);
            assert_eq!(
                *ops.syncs.lock().unwrap(),
                u32::from(broker),
                "{here:?}: {rows:?}"
            );
            if broker {
                assert_eq!(rows[0].outcome, Outcome::Changed, "{here:?}: {rows:?}");
            } else {
                assert_eq!(rows[0].outcome, Outcome::Deferred, "{here:?}: {rows:?}");
                assert_eq!(rows[0].why.as_deref(), Some(absent().as_str()), "{here:?}");
                assert_eq!(*ops.reached.lock().unwrap(), 0, "{here:?}");
            }
        }
    }
}

/// Only a reach of a vault on the LAN waits for the LAN broker, never a read of this Mac's
/// own records: without the broker a vault at a path (a file this Mac reads, no network use)
/// or at a URL off the LAN is synced as anywhere, and a vault on the LAN already in sync, its
/// subscriber running, reads in place with nothing reached. NEGATIVE CONTROL: the same LAN
/// vault with a file to write again waits
/// ([`without_the_lan_broker_the_vault_this_mac_is_in_waits_for_it`]); with the gate on every
/// configured URL the public vault is deferred unsynced, and on every source the path vault
/// is too, and the vault in sync reads deferred.
#[test]
fn without_the_lan_broker_a_vault_off_the_lan_syncs_and_one_in_sync_reads_in_place() {
    for here in [Here::Person, Here::Session, Here::Remote, Here::Unbidden] {
        for source in [
            Source::Path("/Volumes/Keys/team.vault".into()),
            Source::Url("https://vault.example/v1/lab".into()),
        ] {
            let mut off_lan = configured(Membership::In {
                seq: 7,
                heard: 1_800_000_000,
            });
            if let Some(config) = off_lan.config.as_mut() {
                config.source = source.clone();
            }
            let sys = fake();
            let ops = Scripted::new(vec![Ok(off_lan)]);
            ops.subscribed(Ok(installed_state()));
            *ops.unwritten.lock().unwrap() = true;
            ops.broker_absent();
            let rows = run_at(&sys, &ops, Mode::Apply, here);
            assert_eq!(
                rows[0].outcome,
                Outcome::Changed,
                "{here:?} {source:?}: {rows:?}"
            );
            assert_eq!(*ops.syncs.lock().unwrap(), 1, "{here:?}: {rows:?}");
            assert!(!waits_for_lan_broker(&rows[0]), "{here:?}: {rows:?}");
        }

        let sys = fake();
        let ops = converged();
        ops.broker_absent();
        let rows = run_at(&sys, &ops, Mode::Apply, here);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{here:?}: {rows:?}");
        assert_ne!(rows[0].why.as_deref(), Some(absent().as_str()), "{here:?}");
        assert_eq!(*ops.syncs.lock().unwrap(), 0, "{here:?}");
        assert_eq!(*ops.reached.lock().unwrap(), 0, "{here:?}: nothing reached");
        assert_eq!(ops.installs(), 0, "{here:?}");
    }
}

/// A `--vault` reaches the LAN only where it names a URL there: without the LAN broker a
/// person's `--vault` at a path or at a public URL is read and joined as anywhere, and one at
/// a `.local` URL waits for the broker, read by nothing. NEGATIVE CONTROL: a gate on every
/// unconfigured apply defers the path and the public URL unread.
#[test]
fn without_the_lan_broker_only_a_named_vault_on_the_lan_waits_for_it() {
    for (typed, lan) in [
        ("/Volumes/Keys/team.vault", false),
        ("https://vault.example/v1/lab", false),
        ("http://lab-vault.local:7484/v1/lab", true),
    ] {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.broker_absent();
        let rows = run_with(&sys, &ops, Mode::Apply, Some(typed));
        assert_eq!(waits_for_lan_broker(&rows[0]), lan, "{typed}: {rows:?}");
        let read: Vec<String> = if lan {
            Vec::new()
        } else {
            vec![typed.to_owned()]
        };
        assert_eq!(*ops.checks.lock().unwrap(), read, "{typed}");
        assert_eq!(ops.joins().is_empty(), lan, "{typed}: {rows:?}");
        assert_eq!(ops.probes(), 0, "{typed}: no look for the ALab vault");
    }
}

/// A pairing code is said only where a person is at the console, in their own run or one
/// aterm decided: off it, a Mac waiting to be let into another vault reads that an admin Mac
/// approves it, without its code.
#[test]
fn off_the_console_a_waiting_row_names_no_code() {
    let url = "https://vault.example/v1/lab";
    let on_lab = || {
        let mut status = configured(Membership::Pending);
        if let Some(config) = status.config.as_mut() {
            config.source = Source::Url(url.into());
            config.name = "lab".into();
        }
        status
    };
    for (here, coded) in [
        (Here::Person, true),
        (Here::Unbidden, true),
        (Here::Session, false),
        (Here::Remote, false),
    ] {
        let sys = fake();
        let ops = Scripted::new(vec![Ok(on_lab())]);
        ops.subscribed(Ok(installed_state()));
        let rows = run_at(&sys, &ops, Mode::Apply, here);
        let why = rows[0].why.as_deref().expect("why it waits");
        assert_eq!(why.contains(CODE), coded, "{here:?}: {why}");
        assert!(why.contains("admin Mac"), "{here:?}: {why}");
    }
}

/// A look that reaches no vault (no route to the vault's address) stands aside after that
/// one look, on a Mac as on Linux: nothing waits for an answer and nothing is asked, and the
/// row says nothing answered, never that there is no vault here, and names no remedy. Another
/// vault answering there is a definite absence. Before, no answer read as no vault here.
#[test]
fn an_unreachable_look_is_unanswered_and_another_vault_is_none_here() {
    for linux in [false, true] {
        let mut sys = fake();
        sys.linux = linux;
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.probed.lock().unwrap() = Some(Err(aterm_secrets::Error::new(format!(
            "fetching http://alab-secrets.local:7484/v1/alab: {}",
            io::Error::from_raw_os_error(libc::EHOSTUNREACH)
        ))));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        let Some(StoodAside::Offline(why)) = &rows[0].stood_aside else {
            panic!("{rows:?}");
        };
        assert!(why.starts_with(UNANSWERED), "{why}");
        assert!(!why.contains("no secrets vault on this network"), "{why}");
        assert_eq!(ops.probes(), 1);
        assert_eq!(ops.installs(), 1, "the subscriber retains the offline join");
        assert_eq!(asked(&sys), 0);
    }
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.probed.lock().unwrap() = Some(Ok(ProbeOutcome::AnotherVault(
        "http://alab-secrets.local:7484/v1/other".into(),
    )));
    let rows = run(&sys, &ops, Mode::Apply);
    let Some(StoodAside::Absent(why)) = &rows[0].stood_aside else {
        panic!("{rows:?}");
    };
    assert!(why.starts_with(NOT_HERE), "{why}");
}

#[test]
fn a_look_kept_for_this_network_spares_the_next_and_a_failed_one_is_not_kept() {
    use aterm_secrets::heard::{self, Heard, Verdict};
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let here = || Some("n00112233445566aa".to_owned());
    let asked = Cell::new(0);
    let probe = |answer: aterm_secrets::Result<ProbeOutcome>| {
        let asked = &asked;
        move || {
            asked.set(asked.get() + 1);
            answer
        }
    };
    let other = || Ok(ProbeOutcome::AnotherVault("another vault".into()));
    let looked = look_here_with(root.path(), here(), 1_000, probe(other()));
    assert!(matches!(looked, Ok(ProbeOutcome::AnotherVault(_))));
    assert_eq!(asked.get(), 1);
    assert_eq!(
        heard::read(&paths),
        Some(Verdict {
            at: 1_000,
            heard: Heard::Absent,
            network: "n00112233445566aa".into(),
        })
    );
    // The next run on this network makes no request.
    let looked = look_here_with(root.path(), here(), 2_000, probe(other()));
    assert_eq!(looked.unwrap(), ProbeOutcome::Absent);
    assert_eq!(asked.get(), 1, "no look on a network a look found empty");
    // Another network is looked at again; a look the LAN broker could not make is not kept.
    let looked = look_here_with(
        root.path(),
        Some("n66778899aabbccdd".into()),
        2_000,
        probe(Err(aterm_secrets::Error::new(
            aterm_uds::lan::Unavailable::NotInstalled.to_string(),
        ))),
    );
    assert!(looked.is_err());
    assert_eq!(asked.get(), 2);
    assert_eq!(heard::read(&paths).unwrap().network, "n00112233445566aa");
    // Nor is a 404, an answer that is no vault, a timeout or a refused connect: none is an
    // answer about the network.
    let elsewhere = || Some("n0123456789abcdef".to_owned());
    for (n, failed) in [
        Ok(ProbeOutcome::Absent),
        Ok(ProbeOutcome::NotThisVault("503 Service Unavailable".into())),
        Err(aterm_secrets::Error::new("request deadline exceeded")),
        Err(aterm_secrets::Error::new(
            "connect: Connection refused (os error 61)",
        )),
    ]
    .into_iter()
    .enumerate()
    {
        let _ = look_here_with(root.path(), elsewhere(), 3_000 + n as u64, probe(failed));
        assert_eq!(
            heard::read(&paths).unwrap().network,
            "n00112233445566aa",
            "{n}"
        );
    }
    assert_eq!(asked.get(), 6, "each was asked, none kept");
}

#[test]
fn the_alab_vault_answering_is_joined_with_no_join_password_asked() {
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let after = configured(Membership::In {
        seq: 8,
        heard: 1_800_000_000,
    });
    let outcome = JoinOutcome::Joined {
        seq: 8,
        names: after.names.clone(),
    };
    ops.joins_as(Some(after), Ok(outcome));
    ops.alab_answers();
    *ops.requests.lock().unwrap() = true;
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_report_matches_current(&sys, &ops, &rows[0]);
    assert_eq!(
        ops.joins(),
        [(aterm_secrets::ALAB_DEFAULT_URL.to_owned(), true)]
    );
    assert_eq!(*ops.passwords.lock().unwrap(), [false]);
    assert_eq!(asked(&sys), 0, "nothing asked: {:?}", sys.calls());
    assert_eq!(
        ops.installs(),
        1,
        "installed once, when the request was queued"
    );
    assert_eq!(*ops.installed_by_join.lock().unwrap(), [1]);
    assert_eq!(*ops.hooked.lock().unwrap(), [true]);
}

/// A run aterm decided with a person at the console ([`Here::Unbidden`]) shows them the
/// codes, but its join of the open ALab vault may ask them nothing: no signer to confirm, whatever its source pins. The
/// person's own run, whose join may ask, is
/// `the_alab_vault_answering_is_joined_with_no_join_password_asked`.
/// NEGATIVE CONTROL: `items_at` building the vault with `.asking(says_codes)` makes this
/// join interactive, and the test fails.
#[test]
fn a_run_aterm_decided_joins_the_alab_vault_asking_nothing() {
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let after = configured(Membership::In {
        seq: 8,
        heard: 1_800_000_000,
    });
    let outcome = JoinOutcome::Joined {
        seq: 8,
        names: after.names.clone(),
    };
    ops.joins_as(Some(after), Ok(outcome));
    ops.alab_answers();
    let rows = run_at(&sys, &ops, Mode::Apply, Here::Unbidden);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(
        ops.joins(),
        [(aterm_secrets::ALAB_DEFAULT_URL.to_owned(), false)],
        "joined, and the join asks nothing"
    );
    assert_eq!(*ops.passwords.lock().unwrap(), [false]);
    assert_eq!(asked(&sys), 0, "nothing asked: {:?}", sys.calls());
}

#[test]
fn a_named_source_is_joined_as_typed_and_shown_without_credentials() {
    let sys = fake();
    let url = "http://vault.example:7484/v1/lab?token=private-query#private-pin";
    sys.answer(None);
    let ops = Scripted::new(vec![
        Ok(unconfigured()),
        Ok(unconfigured()),
        Ok(configured(Membership::In { seq: 1, heard: 0 })),
    ]);
    let rows = run_with(&sys, &ops, Mode::Apply, Some(url));
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(ops.joins(), [(url.to_owned(), true)]);
    assert_eq!(*ops.checks.lock().unwrap(), [url]);
    assert_eq!(
        ops.probes(),
        0,
        "a named vault needs no look for the ALab one"
    );
    let report = aterm_json::to_string(&rows).unwrap();
    assert!(!report.contains("private-query"), "{report}");
    assert!(!report.contains("private-pin"), "{report}");
    assert!(
        report.contains("http://vault.example:7484/v1/lab"),
        "{report}"
    );
}

#[test]
fn a_named_vault_that_holds_none_is_asked_for_again_and_never_joined() {
    const TYPO: &str = "http://vualt.example/v1/lab";
    let unanswered = || Err("fetching http://vualt.example/v1/lab: dns: no such host".to_owned());
    for typed in [Some("none"), Some("SKIP"), Some(" None "), None] {
        let sys = fake();
        sys.answer(typed);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        *ops.checked.lock().unwrap() = vec![unanswered()].into();
        let rows = run_with(&sys, &ops, Mode::Apply, Some(TYPO));
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert_eq!(rows[0].why.as_deref(), Some(SKIPPED));
        assert_eq!(sys.calls(), [format!("ask {PROMPT}")]);
        assert!(ops.joins().is_empty());
        assert_eq!(ops.installs(), 0);
        let said = ops.said.lock().unwrap().clone();
        assert!(said[0].contains("dns: no such host"), "{said:?}");
    }

    let retyped = "http://vault.example/v1/lab";
    let sys = fake();
    sys.answer(Some(retyped));
    sys.answer(None);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.checked.lock().unwrap() = vec![unanswered()].into();
    *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
        code: CODE.into(),
        host: "m22".into(),
    }));
    run_with(&sys, &ops, Mode::Apply, Some(TYPO));
    assert_eq!(*ops.checks.lock().unwrap(), [TYPO, retyped]);
    assert_eq!(ops.joins(), [(retyped.to_owned(), true)], "never the typo");
}

#[test]
fn an_unpinned_vault_is_trusted_at_the_prompt_and_joined_with_its_pin() {
    let url = "https://vault.example/v1/lab";
    let context = ctx(&[user("alice", 501)]);
    let unpinned = || {
        Ok(Checked::Unpinned {
            fingerprint: "0123-4567-89ab-cdef".into(),
            pin: "c2lnbmVy".into(),
        })
    };
    let sys = fake();
    sys.answer(Some("y"));
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.checked.lock().unwrap() = vec![unpinned()].into();
    *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
        code: CODE.into(),
        host: "m22".into(),
    }));
    let port: Arc<dyn SecretsOps> = ops.clone();
    let step = items(&sys, &context, port, Mode::Apply, Some(url)).unwrap();
    let prompts = sys.calls();
    assert!(
        prompts[0].starts_with("ask The vault is signed by 0123-4567-89ab-cdef"),
        "the signer is asked before anything runs: {prompts:?}"
    );
    engine::run(&step.items, Mode::Apply, &sys, &context, &mut NoJournal);
    assert_eq!(
        asked(&sys),
        1,
        "the signer is the one question: no join password, and nothing once the run makes \
         changes"
    );
    assert_eq!(ops.joins(), [(format!("{url}#c2lnbmVy"), true)]);

    let sys = fake();
    sys.answer(Some("n"));
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.checked.lock().unwrap() = vec![unpinned()].into();
    let rows = run_with(&sys, &ops, Mode::Apply, Some(url));
    assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
    assert!(ops.joins().is_empty());
}

#[test]
fn plan_and_check_never_prompt_probe_or_join_an_unconfigured_mac() {
    let sys = fake();
    sys.answer(Some(aterm_secrets::ALAB_DEFAULT_URL));
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    let rows = run(&sys, &ops, Mode::Plan);
    assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
    assert_eq!(rows[0].why.as_deref(), Some(UNCONFIGURED));
    assert_eq!(asked(&sys), 0);
    assert_eq!(ops.probes(), 0, "a plan never touches the network");
    assert!(ops.joins().is_empty());
    assert_eq!(ops.installs(), 0);
}

/// Without a terminal the open ALab vault, which needs nobody, is still joined, asking
/// nothing; a vault `--vault` names, whose questions need a person at the terminal, is left
/// as it is, unread.
#[test]
fn without_a_terminal_the_open_alab_vault_is_joined_and_a_named_vault_is_left() {
    let sys = fake();
    sys.answer(Some(aterm_secrets::ALAB_DEFAULT_URL));
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let after = configured(Membership::In {
        seq: 8,
        heard: 1_800_000_000,
    });
    let outcome = JoinOutcome::Joined {
        seq: 8,
        names: after.names.clone(),
    };
    ops.joins_as(Some(after), Ok(outcome));
    ops.alab_answers();
    *ops.terminal.lock().unwrap() = false;
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(asked(&sys), 0);
    assert_eq!(ops.passwords(), [false], "no join password");

    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.terminal.lock().unwrap() = false;
    let rows = run_with(
        &sys,
        &ops,
        Mode::Apply,
        Some("https://vault.example/v1/lab"),
    );
    assert_eq!(rows[0].why.as_deref(), Some(UNCONFIGURED));
    assert!(
        ops.checks.lock().unwrap().is_empty(),
        "a named vault is not read"
    );
    assert_eq!(ops.probes(), 0);
    assert!(ops.joins().is_empty());
}

/// A vault at a path has no join queue: a Mac waiting on it names the command that lets
/// its key in under its name, never a code no admin Mac can approve. One at a URL names the
/// Allow on any member, and no admin.
#[test]
fn a_mac_waiting_on_a_vault_at_a_path_names_its_key_not_its_code() {
    let path = "/Users//someone/home.vault";
    let mut status = configured(Membership::Pending);
    if let Some(config) = status.config.as_mut() {
        config.source = Source::Path(path.into());
    }
    let Probe::Unusable { why } = judge(&status, path, (None, true, false), &|| JoinPlace::Home)
    else {
        panic!("a watched pending join waits for an admin");
    };
    let key = Recipient([7; 32]).to_bech32();
    assert!(
        why.contains(&format!("aterm secrets approve {key} --name m22")),
        "{why}"
    );
    assert!(!why.contains(CODE), "{why}");
    let Probe::Unusable { why } = judge(
        &configured(Membership::Pending),
        aterm_secrets::ALAB_DEFAULT_URL,
        (None, true, false),
        &|| JoinPlace::Home,
    ) else {
        panic!("a watched pending join waits for an admin");
    };
    assert!(why.contains(LET_IN) && !why.contains("admin"), "{why}");
}

/// A Mac waiting on the ALab vault says what lets it in where it is (`aterm init`'s row and
/// System State's Secrets step alike): the automatic admission's words on the ALab LAN only,
/// and off it `aterm_secrets::alab_join::let_in`'s, never "joining ALab automatically", whether
/// or not a member heard it ask.
/// NEGATIVE CONTROL: `approval` giving `LET_IN` (and `quiet_first`) wherever this Mac is fails
/// at the first place off the LAN, "Proves: the words for where it is".
#[test]
fn an_alab_joins_waiting_words_follow_where_this_mac_is() {
    use aterm_secrets::alab_join::let_in;
    for at in [
        JoinPlace::Home,
        JoinPlace::Proves,
        JoinPlace::Away,
        JoinPlace::Unknown,
    ] {
        let Probe::Unusable { why } = judge(
            &configured(Membership::Pending),
            aterm_secrets::ALAB_DEFAULT_URL,
            (None, true, false),
            &|| at,
        ) else {
            panic!("a watched pending join waits");
        };
        assert!(
            why.contains(let_in(at)),
            "{at:?}: the words for where it is: {why}"
        );
        let asked = approval(
            None,
            (aterm_secrets::ALAB_DEFAULT_URL, true),
            false,
            (true, &|| at),
        );
        assert!(
            asked.contains(let_in(at)),
            "{at:?}: the words for where it is: {asked}"
        );
        assert_eq!(
            asked.contains(&quiet_first()),
            at == JoinPlace::Home,
            "{at:?}: automatic only on the ALab LAN: {asked}"
        );
    }
}

/// A Mac waiting on, or removed from, a vault other than ALab is told what lets it in there,
/// an admin Mac's approve of its code, never to press Allow on a Mac in ALab: that vault's
/// members show no Allow for it.
/// NEGATIVE CONTROL: with `alab_words` always true (the old words for every vault at a URL),
/// both rows send the person to ALab and the test fails.
#[test]
fn a_mac_on_another_vault_is_never_sent_to_alab() {
    let url = "https://vault.example/v1/lab";
    let on = |membership| {
        let mut status = configured(membership);
        if let Some(config) = status.config.as_mut() {
            config.source = Source::Url(url.into());
            config.name = "lab".into();
        }
        status
    };
    let Probe::Unusable { why } =
        judge(&on(Membership::Pending), url, (None, true, false), &|| {
            JoinPlace::Home
        })
    else {
        panic!("a watched pending join waits");
    };
    assert!(
        why.contains(&format!("aterm secrets approve {CODE}")),
        "{why}"
    );
    assert!(!why.contains("ALab") && !why.contains(LET_IN), "{why}");
    let Probe::Blocked { why } = judge(
        &on(Membership::Revoked { seq: 9 }),
        url,
        (None, true, false),
        &|| JoinPlace::Home,
    ) else {
        panic!("a removed Mac is blocked");
    };
    assert!(
        why.contains(&format!("aterm secrets approve {CODE}")),
        "{why}"
    );
    assert!(!why.contains("ALab"), "{why}");
}

/// `docs/DESIGN-fluid-swarm-2026-10-28.md` F10: the join System State and `aterm init` make
/// at the ALab vault asks every member it hears itself, as `aterm members join --no-wait`
/// does, so a row shows on a member before anyone runs a verb on this Mac.
#[test]
fn a_join_waiting_on_the_alab_vault_asks_every_member_itself() {
    let signer: [u8; 32] = aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
        .expect("the ALab signer")
        .try_into()
        .expect("32 bytes");
    let on_alab = |membership| {
        let mut status = configured(membership);
        status.config.as_mut().expect("a config").signer = Some(signer);
        status
    };
    let joining = |status: Status| {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        *ops.requests.lock().unwrap() = true;
        ops.joins_as(
            Some(status),
            Ok(JoinOutcome::Pending {
                code: CODE.into(),
                host: "m22".into(),
            }),
        );
        ops
    };
    let waiting = |ops: &Arc<Scripted>| {
        let sys = fake();
        sys.answer(None);
        let rows = run(&sys, ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        rows[0].why.clone().expect("why it waits")
    };
    let asked = format!("waiting to be let in: {}, {LET_IN}", quiet_first());

    // The join queues, then asks the members once; one heard it, so the row starts at the
    // Allow that member shows.
    let ops = joining(on_alab(Membership::Pending));
    *ops.members_hear.lock().unwrap() = Some(Ok(2));
    let why = waiting(&ops);
    assert_eq!(*ops.member_asks.lock().unwrap(), 1);
    assert!(why.starts_with(&asked), "{why}");
    assert!(!why.contains("admin"), "{why}");

    // No member heard it: the row starts at `aterm members join`, which asks them all.
    let ops = joining(on_alab(Membership::Pending));
    let why = waiting(&ops);
    assert_eq!(*ops.member_asks.lock().unwrap(), 1);
    assert!(
        why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
        "{why}"
    );

    // A later pass that finds the subscriber down starts it and asks again: the member's
    // row is one row, and one it lost (a restart, a day gone) shows again.
    let ops = Scripted::new(vec![Ok(on_alab(Membership::Pending))]);
    *ops.members_hear.lock().unwrap() = Some(Ok(1));
    let why = waiting(&ops);
    assert_eq!(*ops.member_asks.lock().unwrap(), 1);
    assert!(why.starts_with(&asked), "{why}");

    // A pass that finds the subscriber running only reads: nobody is asked, and the row
    // names `aterm members join`, which asks again.
    let ops = Scripted::new(vec![Ok(on_alab(Membership::Pending))]);
    ops.subscribed(Ok(installed_state()));
    *ops.members_hear.lock().unwrap() = Some(Ok(1));
    let why = waiting(&ops);
    assert_eq!(*ops.member_asks.lock().unwrap(), 0);
    assert!(
        why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
        "{why}"
    );

    // A member already let this Mac in: nobody is asked, and the row says what it waits on,
    // on the pass that starts the subscriber and on one that only reads.
    for running in [false, true] {
        let ops = Scripted::new(vec![Ok(on_alab(Membership::Pending))]);
        if running {
            ops.subscribed(Ok(installed_state()));
        }
        *ops.let_in.lock().unwrap() = true;
        let why = waiting(&ops);
        assert_eq!(*ops.member_asks.lock().unwrap(), 0);
        assert_eq!(why, format!("waiting to be let in: {ALLOWED}"), "{running}");
    }

    // Control: a vault that is not ALab's has no members to ask, and an Allow left from
    // ALab does not speak for it.
    let ops = joining(configured(Membership::Pending));
    *ops.members_hear.lock().unwrap() = Some(Ok(2));
    *ops.let_in.lock().unwrap() = true;
    let why = waiting(&ops);
    assert_eq!(*ops.member_asks.lock().unwrap(), 0);
    assert!(!why.starts_with(&asked), "{why}");
    assert!(!why.contains(ALLOWED), "{why}");
}

#[test]
fn a_join_that_waits_for_an_admin_stands_aside_naming_the_code() {
    let sys = fake();
    sys.answer(None);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    *ops.requests.lock().unwrap() = true;
    ops.joins_as(
        Some(configured(Membership::Pending)),
        Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }),
    );
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert!(matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))));
    let why = rows[0].why.as_deref().unwrap();
    assert!(
        why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
        "{why}"
    );
    assert!(!why.contains("admin"), "{why}");
    assert!(!why.contains("mistyped"), "no password was typed: {why}");
    assert_eq!(*ops.passwords.lock().unwrap(), [false]);
    assert_eq!(
        *ops.waits.lock().unwrap(),
        [Duration::ZERO],
        "the join is not awaited"
    );
    assert_eq!(*ops.installed_by_join.lock().unwrap(), [1]);
    assert_eq!(*ops.hooked.lock().unwrap(), [true]);
    assert_eq!(ops.installs(), 1, "the subscriber finishes the join");
}

#[test]
fn a_join_that_gave_up_waiting_on_a_mac_the_subscriber_let_in_is_done() {
    let sys = fake();
    sys.answer(None);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    *ops.requests.lock().unwrap() = true;
    ops.joins_as(
        Some(configured(Membership::In {
            seq: 8,
            heard: 1_800_000_000,
        })),
        Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }),
    );
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(rows[0].why, None);
    assert_eq!(ops.installs(), 1);
}

#[test]
fn github_waits_for_a_vault_join_that_waits_to_be_let_in() {
    const PROFILE: &[u8] =
        b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
    const INIT_TOML: &str = "/Users//alice/.config/aterm/init.toml";
    let configured_github = |sys: &Fake| {
        sys.file(INIT_TOML, PROFILE).stat_of(
            INIT_TOML,
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o644,
                dir: false,
                symlink: false,
                len: PROFILE.len() as u64,
            },
        );
    };
    let context = ctx(&[user("alice", 501)]);
    let github_after = |rows: &[ItemReport]| {
        let auth = rows
            .iter()
            .find(|row| row.family == "github" && !row.id.starts_with("init.github.keychain@"))
            .expect("the GitHub rows are still produced");
        assert!(
            auth.why
                .as_deref()
                .unwrap()
                .starts_with("waits for init.secrets@alice: waiting to be let in"),
            "{auth:?}"
        );
        assert!(matches!(auth.stood_aside, Some(StoodAside::Waiting(_))));
    };

    let sys = fake();
    configured_github(&sys);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    ops.joins_as(
        Some(configured(Membership::Pending)),
        Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }),
    );
    let port: Arc<dyn SecretsOps> = ops.clone();
    let step = items(&sys, &context, port, Mode::Apply, None).unwrap();
    let mut all = step.items;
    all.extend(crate::init_github::items(&sys, &context, step.waited_on.as_deref()).unwrap());
    let rows = engine::run(&all, Mode::Apply, &sys, &context, &mut NoJournal);
    assert!(
        matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))),
        "{rows:?}"
    );
    assert_eq!(asked(&sys), 0, "{:?}", sys.calls());
    github_after(&rows);

    // The next run, this Mac still waiting: the GitHub credential waits, not fails.
    let sys = fake();
    configured_github(&sys);
    let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
    ops.subscribed(Ok(installed_state()));
    let port: Arc<dyn SecretsOps> = ops.clone();
    let step = items(&sys, &context, port, Mode::Apply, None).unwrap();
    let mut all = step.items;
    all.extend(crate::init_github::items(&sys, &context, step.waited_on.as_deref()).unwrap());
    let rows = engine::run(&all, Mode::Apply, &sys, &context, &mut NoJournal);
    github_after(&rows);
}

/// alice's Mac with git and her own home, which the GitHub step reads beside the vault's,
/// and, where `github` gives one, her init.toml with that `[github]` table.
fn with_git(github: Option<&[u8]>) -> Fake {
    let sys = fake();
    sys.executable(crate::items::github::CLT_GIT, b"git")
        .stat_of(
            "/Users//alice",
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o700,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
    crate::git_credential::script::unconfigured(&sys, &user("alice", 501));
    if let Some(profile) = github {
        const INIT_TOML: &str = "/Users//alice/.config/aterm/init.toml";
        sys.file(INIT_TOML, profile).stat_of(
            INIT_TOML,
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o644,
                dir: false,
                symlink: false,
                len: profile.len() as u64,
            },
        );
        let alice = user("alice", 501);
        crate::git_credential::script::unconfigured(&sys, &alice);
    }
    sys
}

/// Without the LAN broker, a Mac waiting to be let into
/// a vault at a URL whose subscriber is down has that subscriber held (the vault's row
/// Deferred, the person's step), so nothing lands the join this run: with no init.toml
/// `[github]`, GitHub reads its own row, no token on this Mac yet, rather than one blocked on
/// the vault with no wait to name, and the apply exits 0. A running subscriber still finishes
/// the join, so GitHub waits for it, and the apply exits 0 too. An init.toml `[github]` that
/// names a `token_file` reads that file's own step, which says the file is not on this Mac
/// yet and fails (exit 1); one that names only an account reads the vault's token file, whose
/// absence blocks the step (exit 1), never a person's step.
/// NEGATIVE CONTROL: GitHub composed after the held vault whatever the subscriber (as it
/// was) reads Blocked on it with no wait of its own, and the apply exits 1.
#[test]
fn a_held_waiting_vault_with_its_subscriber_down_leaves_github_its_own_row() {
    let context = ctx(&[user("alice", 501)]);
    let applied = |subscriber: State, after: Option<Option<&str>>, github: Option<&[u8]>| {
        let sys = with_git(github);
        let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
        ops.subscribed(Ok(subscriber));
        ops.broker_absent();
        let port: Arc<dyn SecretsOps> = ops.clone();
        let step = items_at(&sys, &context, port, Mode::Apply, None, Here::Session).unwrap();
        let mut all = step.items;
        let after = after.unwrap_or(step.waited_on.as_deref());
        all.extend(crate::init_github::items(&sys, &context, after).unwrap());
        let rows = engine::run(&all, Mode::Apply, &sys, &context, &mut NoJournal);
        assert_eq!(ops.installs(), 0, "{rows:?}");
        rows
    };
    let github = |rows: &[ItemReport]| {
        rows.iter()
            .find(|row| row.family == "github" && !row.id.starts_with("init.github.keychain@"))
            .cloned()
            .expect("the GitHub row")
    };

    let rows = applied(State::Absent, None, None);
    assert_eq!(rows[0].outcome, Outcome::Deferred, "{rows:?}");
    assert_eq!(rows[0].why.as_deref(), Some(absent().as_str()), "{rows:?}");
    let own = github(&rows);
    assert_eq!(own.outcome, Outcome::NotApplicable, "{own:?}");
    assert!(
        own.why
            .as_deref()
            .is_some_and(|why| why.starts_with("no GitHub token on this Mac yet")),
        "{own:?}"
    );
    assert_eq!(crate::init::apply_status_for_test(&rows), 0, "{rows:?}");

    let rows = applied(
        State::Absent,
        None,
        Some(b"[github]\naccount='octocat'\ntoken_file='~/.secrets/github'\n"),
    );
    assert_eq!(rows[0].outcome, Outcome::Deferred, "{rows:?}");
    let file = github(&rows);
    assert_eq!(file.outcome, Outcome::Blocked, "{file:?}");
    assert_eq!(
        file.why.as_deref(),
        Some("the GitHub token file (/Users//alice/.secrets/github) is not on this Mac yet"),
        "{file:?}"
    );
    assert!(file.stood_aside.is_none(), "{file:?}");
    assert_eq!(crate::init::apply_status_for_test(&rows), 1, "{rows:?}");

    let rows = applied(State::Absent, None, Some(b"[github]\naccount='octocat'\n"));
    assert_eq!(rows[0].outcome, Outcome::Deferred, "{rows:?}");
    let account = github(&rows);
    assert_eq!(account.outcome, Outcome::Blocked, "{account:?}");
    assert!(account.stood_aside.is_none(), "{account:?}");
    assert_eq!(crate::init::apply_status_for_test(&rows), 1, "{rows:?}");

    let rows = applied(installed_state(), None, None);
    assert!(
        matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))),
        "{rows:?}"
    );
    let waits = github(&rows);
    assert!(
        waits.why.as_deref().is_some_and(
            |why| why.starts_with("waits for init.secrets@alice: waiting to be let in")
        ),
        "{waits:?}"
    );
    assert!(matches!(waits.stood_aside, Some(StoodAside::Waiting(_))));
    assert_eq!(crate::init::apply_status_for_test(&rows), 0, "{rows:?}");

    let rows = applied(State::Absent, Some(Some("init.secrets@alice")), None);
    let blocked = github(&rows);
    assert_eq!(blocked.outcome, Outcome::Blocked, "{blocked:?}");
    assert_eq!(
        blocked.why.as_deref(),
        Some("waits for init.secrets@alice"),
        "{blocked:?}"
    );
    assert!(blocked.stood_aside.is_none(), "{blocked:?}");
    assert_eq!(crate::init::apply_status_for_test(&rows), 1, "{rows:?}");
}

/// Held for the LAN broker, the vault's row and GitHub's wait judge by one reading
/// of the subscriber, taken as the step is composed, so a subscriber that stops or starts
/// before the vault's probe (launchd restarting its KeepAlive job) leaves the two agreeing,
/// whichever way it moved, and the apply exits 0: running at that reading, the vault's row
/// waits to be let in and GitHub waits for it; idle, the vault's row reads Deferred and
/// GitHub its own row. The later answer is never read.
/// NEGATIVE CONTROL (the mutant): the vault's probe reading `subscriber` again, in place of
/// the held reading, fails both orders: running then idle reads GitHub Blocked beside a
/// Deferred vault, with no wait of its own (exit 1), and idle then running a vault waiting to
/// be let in beside a GitHub that owes nothing.
#[test]
fn a_held_vaults_row_and_githubs_wait_judge_by_one_reading_of_the_subscriber() {
    let context = ctx(&[user("alice", 501)]);
    for (first, then) in [
        (installed_state(), State::Absent),
        (State::Absent, installed_state()),
    ] {
        let running = idle(&first).is_none();
        let sys = with_git(None);
        let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
        *ops.readings.lock().unwrap() = vec![Ok(first), Ok(then)].into();
        ops.broker_absent();
        let port: Arc<dyn SecretsOps> = ops.clone();
        let step = items_at(&sys, &context, port, Mode::Apply, None, Here::Session).unwrap();
        let mut all = step.items;
        all.extend(crate::init_github::items(&sys, &context, step.waited_on.as_deref()).unwrap());
        let rows = engine::run(&all, Mode::Apply, &sys, &context, &mut NoJournal);
        let github = rows
            .iter()
            .find(|row| row.family == "github" && !row.id.starts_with("init.github.keychain@"))
            .expect("the GitHub row");
        let waits = |row: &ItemReport| matches!(row.stood_aside, Some(StoodAside::Waiting(_)));
        assert_eq!(waits(&rows[0]), running, "running {running}: {rows:?}");
        assert_eq!(waits(github), running, "running {running}: {rows:?}");
        if !running {
            assert_eq!(rows[0].outcome, Outcome::Deferred, "{rows:?}");
            assert_eq!(github.outcome, Outcome::NotApplicable, "{rows:?}");
        }
        assert_eq!(
            crate::init::apply_status_for_test(&rows),
            0,
            "running {running}: {rows:?}"
        );
        assert_eq!(
            ops.readings.lock().unwrap().len(),
            1,
            "running {running}: the later answer is never read"
        );
        assert_eq!(ops.installs(), 0, "{rows:?}");
    }
}

#[test]
fn ctrl_c_at_a_prompt_stops_the_run_with_nothing_joined() {
    let context = ctx(&[user("alice", 501)]);
    let sys = StopsAt {
        inner: fake(),
        stop: PROMPT,
        sigint: false,
    };
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.checked.lock().unwrap() = vec![Err("dns: no such host".into())].into();
    let port: Arc<dyn SecretsOps> = ops.clone();
    let stopped = items(
        &sys,
        &context,
        port,
        Mode::Apply,
        Some("https://vault.example/v1/lab"),
    )
    .err();
    assert_eq!(
        stopped.as_deref(),
        Some("stopped at the secrets vault prompt; nothing was changed")
    );
    assert!(ops.joins().is_empty());

    // Ctrl-C at the question whether to trust a vault's key stops the run too, rather than
    // reading as a No and going on.
    let sys = StopsAt {
        inner: fake(),
        stop: "The vault is signed by ",
        sigint: false,
    };
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.checked.lock().unwrap() = vec![Ok(Checked::Unpinned {
        fingerprint: "0123-4567-89ab-cdef".into(),
        pin: "c2lnbmVy".into(),
    })]
    .into();
    let port: Arc<dyn SecretsOps> = ops.clone();
    let stopped = items(
        &sys,
        &context,
        port,
        Mode::Apply,
        Some("https://vault.example/v1/lab"),
    )
    .err();
    assert_eq!(stopped.as_deref(), Some(STOPPED_AT_TRUST));
    assert!(ops.joins().is_empty());

    let sys = fake();
    let ops = Scripted::new(vec![
        Ok(unconfigured()),
        Ok(unconfigured()),
        Ok(configured(Membership::Plain)),
    ]);
    let rows = run_with(&sys, &ops, Mode::Apply, Some("~/vault.txt"));
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(asked(&sys), 0, "a path has no join password");
    assert_eq!(*ops.passwords.lock().unwrap(), [false]);
}

#[test]
fn a_pending_mac_needs_an_admin_and_a_revoked_one_is_blocked() {
    let sys = fake();
    let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
    ops.subscribed(Ok(installed_state()));
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert!(
        matches!(rows[0].stood_aside, Some(StoodAside::Waiting(_))),
        "the subscriber finishes it once approved: {rows:?}"
    );
    let why = rows[0].why.as_deref().unwrap();
    assert!(why.contains(LET_IN), "{why}");
    assert!(why.contains("running aterm secrets join again"), "{why}");
    assert!(ops.joins().is_empty());
    assert_eq!(ops.installs(), 0);

    let mut elsewhere = configured(Membership::Pending);
    elsewhere.config.as_mut().unwrap().source = Source::Url("https://vault.example/v1/lab".into());
    let ops = Scripted::new(vec![Ok(elsewhere)]);
    ops.subscribed(Ok(installed_state()));
    let rows = run(&sys, &ops, Mode::Plan);
    let why = rows[0].why.as_deref().unwrap();
    assert!(
        why.contains("running aterm secrets join <vault-url> again"),
        "{why}"
    );

    let ops = Scripted::new(vec![Ok(configured(Membership::Revoked { seq: 9 }))]);
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert!(rows[0].why.as_deref().unwrap().contains("seq 9"));
    assert!(ops.joins().is_empty());
    assert_eq!(ops.installs(), 0);

    let mut plain = configured(Membership::Plain);
    plain.config.as_mut().unwrap().source = Source::Path("/Volumes/Keys/vault.txt".into());
    plain.names.truncate(1);
    let ops = Scripted::new(vec![Ok(plain)]);
    ops.subscribed(Ok(installed_state()));
    let rows = run(&sys, &ops, Mode::Plan);
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(
        rows[0].to.as_deref(),
        Some(
            "reading the plain text file /Volumes/Keys/vault.txt: 1 secret in ~/.secrets, kept in sync by the subscriber"
        )
    );
}

#[test]
fn failures_report_what_failed_and_never_what_was_typed() {
    let pasted = ["ghp", "_", &"Q7".repeat(18)].concat();
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let rows = run_with(&sys, &ops, Mode::Apply, Some(&pasted));
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    let report = aterm_json::to_string(&rows).unwrap();
    assert!(!report.contains(&pasted), "{report}");
    assert!(ops.joins().is_empty(), "nothing is joined with it");

    let sys = fake();
    sys.answer(None);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
        "vault alab: the signature does not verify",
    )));
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    assert_eq!(
        rows[0].why.as_deref(),
        Some(
            "joining the vault: vault alab: the signature does not verify; the subscriber retries setup automatically"
        )
    );
    assert_eq!(ops.installs(), 1);

    // A join that fails after saving the vault (its request refused) leaves this Mac set to
    // it and waiting: the subscriber that asks again goes in, and the row says so.
    let sys = fake();
    sys.answer(None);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    *ops.saved_by_join.lock().unwrap() = Some(configured(Membership::Pending));
    *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
        "asking to join: the join queue already holds 256 requests",
    )));
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    assert_eq!(
        rows[0].why.as_deref(),
        Some(
            format!(
                "joining the vault: asking to join: the join queue already holds 256 requests; this Mac is now set to {} and its subscriber asks to join again shortly",
                shown(aterm_secrets::ALAB_DEFAULT_URL)
            )
            .as_str()
        )
    );
    assert_eq!(ops.installs(), 1);

    // What another program saved meanwhile, a vault this join did not ask for, is not this
    // join's save: no subscriber goes in for it and the row says only what failed.
    let sys = fake();
    sys.answer(None);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    let mut elsewhere = configured(Membership::Pending);
    if let Some(config) = elsewhere.config.as_mut() {
        config.source = Source::Url("http://vault.example:7484/v1/other".into());
        config.name = "other".into();
    }
    *ops.saved_by_join.lock().unwrap() = Some(elsewhere);
    *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
        "asking to join: the join queue already holds 256 requests",
    )));
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    assert_eq!(
        rows[0].why.as_deref(),
        Some("joining the vault: asking to join: the join queue already holds 256 requests")
    );
    assert_eq!(ops.installs(), 0);

    for requests in [false, true] {
        let sys = fake();
        sys.answer(None);
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        *ops.requests.lock().unwrap() = requests;
        *ops.installed.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
            "no systemctl at /usr/bin/systemctl",
        )));
        let rows = run(&sys, &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        assert_eq!(
            rows[0].why.as_deref(),
            Some(
                "the vault answered, but the subscriber could not be installed: no systemctl at /usr/bin/systemctl"
            )
        );
        assert_eq!(ops.installs(), 1);
        let hooked: &[bool] = if requests { &[false] } else { &[] };
        assert_eq!(*ops.hooked.lock().unwrap(), hooked);
    }

    let sys = fake();
    let ops = Scripted::new(vec![Err(aterm_secrets::Error::new(
        "source.toml: mode 644 lets other users read it; run chmod 600 on it",
    ))]);
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Unknown, "{rows:?}");
    assert!(rows[0].outcome.is_failure());
    assert_eq!(asked(&sys), 0);
}

#[test]
fn a_signer_is_trusted_only_on_yes() {
    for (typed, trusted) in [
        (Some("y"), true),
        (Some(" YES "), true),
        (Some("n"), false),
        (Some("yess"), false),
        (None, false),
    ] {
        let sys = fake();
        sys.answer(typed);
        assert_eq!(
            confirm_signer(&sys, "0123-4567-89ab-cdef"),
            trusted,
            "{typed:?}"
        );
        assert_eq!(
            sys.calls(),
            [
                "ask The vault is signed by 0123-4567-89ab-cdef; compare it with aterm secrets status on an admin Mac. Trust it? [y/N] "
            ]
        );
    }
    assert!(!confirm_signer(&fake(), "0123-4567-89ab-cdef"));
}

#[test]
fn a_source_is_shown_without_its_pin_or_credentials() {
    assert_eq!(
        shown("http://alab-secrets.local:7484/v1/alab"),
        "http://alab-secrets.local:7484/v1/alab"
    );
    assert_eq!(shown("https://u:p@h/v1/x#pin"), "the vault URL");
    assert_eq!(shown("https://u@h"), "the vault URL");
    assert_eq!(shown("~/vault.txt"), "~/vault.txt");
    assert_eq!(shown("/srv/a#b.vault"), "/srv/a#b.vault");
    assert_eq!(shown("/srv/x://u@y"), "/srv/x://u@y");
    assert_eq!(shown("HTTP://h?q=private-query#private-pin"), "http://h/");
    assert_eq!(
        shown("vault.example"),
        "(the answer at the prompt, not shown)"
    );
    assert_eq!(shown("http://"), "(the answer at the prompt, not shown)");
    let private = "https://h/v1/lab?token=private-query#private-pin";
    assert_eq!(shown(private), "https://h/v1/lab");
    let hint = retype(private).expect("a URL");
    assert!(
        hint.contains("<vault-url>") && hint.contains("full configured URL"),
        "{hint}"
    );
    assert!(
        !hint.contains("private-query") && !hint.contains("private-pin"),
        "{hint}"
    );
}

#[test]
fn the_run_names_exactly_one_user() {
    let sys = fake();
    let ops: Arc<dyn SecretsOps> = Scripted::new(vec![Ok(unconfigured())]);
    let two = ctx(&[user("alice", 501), user("bob", 502)]);
    assert!(items(&sys, &two, ops, Mode::Plan, None).is_err());
    assert!(live(&two).is_err());
    assert!(unattended(&two).is_err());
}

const SETUP_WAIT: Duration = Duration::from_secs(20);

fn setup_pass(
    ops: &Arc<Scripted>,
    source: &str,
    password: &mut Option<SecretBytes>,
    refuse: bool,
) -> ItemReport {
    setup_pass_over(ops, source, Replaces::Any, password, refuse)
}

fn setup_pass_over(
    ops: &Arc<Scripted>,
    source: &str,
    replaces: Replaces,
    password: &mut Option<SecretBytes>,
    refuse: bool,
) -> ItemReport {
    let context = ctx(&[user("alice", 501)]);
    let port: Arc<dyn SecretsOps> = ops.clone();
    let mut journal = Lines {
        ops: Arc::clone(ops),
        refuse,
    };
    let mut rows = crate::init::run_initial_secrets_with(
        &fake(),
        &context,
        port,
        source,
        replaces,
        password,
        SETUP_WAIT,
        &mut journal,
    )
    .expect("the stage runs");
    assert_eq!(rows.len(), 1);
    rows.remove(0)
}

fn journaled_first(log: &[String]) {
    let (line, changes) = log.split_first().expect("a line and its changes");
    assert!(line.starts_with("line init.secrets@alice: "), "{log:?}");
    assert!(!changes.is_empty(), "{log:?}");
    assert!(
        changes.iter().all(|change| !change.starts_with("line ")),
        "one line per apply: {log:?}"
    );
    for change in changes {
        let undone = match change.as_str() {
            "install" => "aterm secrets agent off",
            "sync" => "~/.secrets/.replaced",
            join if join.starts_with("join ") => "source.toml",
            keep if keep.starts_with("keep ") => "~/.config/aterm/secrets/before-join-",
            other => panic!("an unknown change {other}"),
        };
        assert!(line.contains(undone), "{change} is not undone by {line}");
    }
}

fn password() -> SecretBytes {
    SecretBytes::from(["setup", "-", &"Wq".repeat(8)].concat().as_bytes())
}

#[test]
fn a_selected_vault_journals_each_change_before_it_is_made() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let ops = Scripted::new(vec![
        Ok(unconfigured()),
        Ok(unconfigured()),
        Ok(configured(Membership::In {
            seq: 7,
            heard: 1_800_000_000,
        })),
    ]);
    *ops.requests.lock().unwrap() = true;
    let mut held = Some(password());
    let row = setup_pass(&ops, source, &mut held, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(row.from.as_deref(), Some("no vault"));
    assert_eq!(ops.changes(), [format!("join {source}"), "install".into()]);
    journaled_first(&ops.log());
    assert_eq!(ops.joins(), [(source.to_owned(), false)], "unattended");
    assert_eq!(*ops.waits.lock().unwrap(), [SETUP_WAIT]);
    assert_eq!(*ops.passwords.lock().unwrap(), [true]);
    assert!(held.is_none(), "the proof is with the vault");
    assert_eq!(*ops.reached.lock().unwrap(), 1, "asked before the line");

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    *ops.unwritten.lock().unwrap() = true;
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(ops.changes(), ["install", "sync"]);
    journaled_first(&ops.log());
    assert!(ops.joins().is_empty());

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    ops.subscribed(Ok(installed_state()));
    *ops.unwritten.lock().unwrap() = true;
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(ops.changes(), ["sync"]);
    journaled_first(&ops.log());
    assert!(
        !ops.log()[0].contains("aterm secrets agent off"),
        "{:?}",
        ops.log()
    );

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    ops.subscribed(Ok(installed_state()));
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
    assert!(ops.log().is_empty());
    assert_eq!(*ops.reached.lock().unwrap(), 0, "no network");
    assert!(ops.said.lock().unwrap().is_empty());
}

#[test]
fn a_setup_from_a_disk_image_waits_to_be_opened_from_applications() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let image = PathBuf::from("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
    assert!(!fleeting_wait(&image).contains("--program"));

    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.joins_as(
        Some(configured(Membership::In { seq: 7, heard: 0 })),
        Ok(JoinOutcome::Joined {
            seq: 7,
            names: Vec::new(),
        }),
    );
    *ops.requests.lock().unwrap() = true;
    *ops.fleeting.lock().unwrap() = Some(image.clone());
    let row = setup_pass(&ops, source, &mut Some(password()), false);
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert_eq!(row.why.as_deref(), Some(fleeting_wait(&image).as_str()));
    assert_eq!(ops.changes(), [format!("join {source}"), "install".into()]);
    journaled_first(&ops.log());
    assert_eq!(*ops.hooked.lock().unwrap(), [false], "not in place");

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    *ops.fleeting.lock().unwrap() = Some(image.clone());
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert_eq!(row.why.as_deref(), Some(fleeting_wait(&image).as_str()));
    assert_eq!(ops.changes(), ["install"]);

    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    ops.joins_as(
        Some(configured(Membership::Pending)),
        Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }),
    );
    *ops.requests.lock().unwrap() = true;
    *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
        code: CODE.into(),
        host: "m22".into(),
    }));
    *ops.fleeting.lock().unwrap() = Some(image.clone());
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    let why = row.why.as_deref().unwrap();
    assert!(
        why.starts_with(&format!("waiting to be let in: {ASK_FIRST}, then {LET_IN}")),
        "{why}"
    );
    assert!(
        why.ends_with(&format!("; {}", fleeting_wait(&image))),
        "{why}"
    );

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    let refusal = aterm_secrets::service::service_program(&image, &|_| false)
        .expect_err("a disk image is refused")
        .to_string();
    *ops.installed.lock().unwrap() = Some(Err(aterm_secrets::Error::new(refusal.clone())));
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert_eq!(
        row.why,
        Some(format!("the subscriber could not be installed: {refusal}"))
    );

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    *ops.fleeting.lock().unwrap() = Some(image.clone());
    let rows = run(&fake(), &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    let why = rows[0].why.as_deref().unwrap();
    assert!(why.contains("--program"), "{why}");
}

#[test]
fn the_setup_stage_journals_into_the_accounts_journal() {
    let sys = fake();
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    ops.joins_as(
        Some(configured(Membership::In { seq: 7, heard: 0 })),
        Ok(JoinOutcome::Joined {
            seq: 7,
            names: Vec::new(),
        }),
    );
    *ops.requests.lock().unwrap() = true;
    let port: Arc<dyn SecretsOps> = ops.clone();
    let rows = crate::init::run_initial_secrets(
        &sys,
        &ctx(&[user("alice", 501)]),
        port,
        aterm_secrets::ALAB_DEFAULT_URL,
        Replaces::Any,
        &mut None,
        SETUP_WAIT,
    )
    .unwrap();
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    let journal = String::from_utf8(sys.written(Fake::JOURNAL).expect("journaled")).unwrap();
    let [line] = journal.lines().collect::<Vec<_>>()[..] else {
        panic!("one line: {journal}");
    };
    let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
    assert_eq!(line["id"].as_str(), Some("init.secrets@alice"));
    assert_eq!(line["from"].as_str(), Some("no vault"));
    let revert = line["revert"].as_str().unwrap();
    assert!(revert.contains("aterm secrets agent off"), "{revert}");
    assert!(
        revert.contains("~/.config/aterm/secrets/state.toml"),
        "{revert}"
    );
    assert!(revert.contains("~/.secrets/.replaced"), "{revert}");
}

#[test]
fn a_refused_journal_line_refuses_the_selected_vaults_change() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    for refuse in [true, false] {
        let ops = Scripted::new(vec![
            Ok(unconfigured()),
            Ok(unconfigured()),
            Ok(configured(Membership::In { seq: 7, heard: 0 })),
        ]);
        *ops.requests.lock().unwrap() = true;
        let mut held = Some(password());
        let row = setup_pass(&ops, source, &mut held, refuse);
        if refuse {
            assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
            assert!(
                row.why
                    .as_deref()
                    .unwrap()
                    .starts_with("not applied: the journal refused the record"),
                "{row:?}"
            );
            assert!(ops.log().is_empty(), "{:?}", ops.log());
            assert!(held.is_some(), "kept for the next try");
        } else {
            assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
            assert!(!ops.changes().is_empty());
        }
    }
    for refuse in [true, false] {
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.unwritten.lock().unwrap() = true;
        let row = setup_pass(&ops, source, &mut None, refuse);
        assert_eq!(ops.changes().is_empty(), refuse, "{row:?}");
        assert_eq!(*ops.syncs.lock().unwrap(), u32::from(!refuse));
        assert_eq!(ops.installs(), u32::from(!refuse));
    }
}

#[test]
fn an_unreachable_selected_vault_stands_aside_with_nothing_journaled_or_made() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.unreachable.lock().unwrap() = Some(Look::Unreached(
        "fetching http://alab-secrets.local:7484/v1/alab: No route to host (os error 65)".into(),
    ));
    let mut held = Some(password());
    for attempt in 1..=5 {
        let row = setup_pass(&ops, source, &mut held, false);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(
            matches!(row.stood_aside, Some(StoodAside::Offline(_))),
            "{row:?}"
        );
        assert_eq!(
            row.why.as_deref(),
            Some(
                "the vault does not answer, so nothing was changed: fetching http://alab-secrets.local:7484/v1/alab: No route to host (os error 65)"
            )
        );
        assert!(ops.log().is_empty(), "{:?}", ops.log());
        assert!(ops.joins().is_empty());
        assert_eq!(*ops.reached.lock().unwrap(), attempt);
        assert!(held.is_some());
    }
    let pending = Scripted::new(vec![Ok(configured(Membership::Pending))]);
    pending.subscribed(Ok(installed_state()));
    *pending.unreachable.lock().unwrap() =
        Some(Look::Unreached("No route to host (os error 65)".into()));
    let row = setup_pass(&pending, source, &mut None, false);
    assert!(
        row.outcome == Outcome::Blocked && matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
        "{row:?}"
    );
    assert_eq!(*pending.reached.lock().unwrap(), 0);
    assert!(pending.log().is_empty());

    *ops.unreachable.lock().unwrap() = None;
    *ops.statuses.lock().unwrap() = vec![
        Ok(unconfigured()),
        Ok(unconfigured()),
        Ok(configured(Membership::In { seq: 7, heard: 0 })),
    ]
    .into();
    let row = setup_pass(&ops, source, &mut held, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    journaled_first(&ops.log());
    assert!(held.is_none());
}

#[test]
fn a_pending_selected_vault_waits_and_a_new_password_asks_once_more() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
    ops.subscribed(Ok(installed_state()));
    let row = setup_pass(&ops, source, &mut None, false);
    assert!(
        row.outcome == Outcome::Blocked && matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
        "{row:?}"
    );
    assert!(ops.log().is_empty());

    let mut held = Some(password());
    *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
        code: CODE.into(),
        host: "m22".into(),
    }));
    let row = setup_pass(&ops, source, &mut held, false);
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert!(
        matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
        "{row:?}"
    );
    assert_eq!(
        ops.changes(),
        [
            "keep /Users//alice/.config/aterm/secrets/before-join-1800000000".to_owned(),
            format!("join {source}")
        ]
    );
    journaled_first(&ops.log());
    assert_eq!(*ops.passwords.lock().unwrap(), [true]);
    assert!(held.is_none(), "spent: its proof is queued");
    let row = setup_pass(&ops, source, &mut held, false);
    assert!(
        row.outcome == Outcome::Blocked && matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
        "{row:?}"
    );
    assert_eq!(ops.joins().len(), 1);

    let mut held = Some(password());
    *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
        "posting the join request: Connection reset by peer (os error 54)",
    )));
    let row = setup_pass(&ops, source, &mut held, false);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert!(held.is_some(), "kept for the next try");
}

#[test]
fn a_selected_join_waits_for_an_admin_only_with_the_join_password() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    for typed in [false, true] {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        ops.joins_as(
            Some(configured(Membership::Pending)),
            Ok(JoinOutcome::Pending {
                code: CODE.into(),
                host: "m22".into(),
            }),
        );
        let mut held = typed.then(password);
        let row = setup_pass(&ops, source, &mut held, false);
        let wait = if typed { SETUP_WAIT } else { Duration::ZERO };
        assert_eq!(*ops.waits.lock().unwrap(), [wait], "typed: {typed}");
        assert!(
            row.outcome == Outcome::Blocked
                && matches!(row.stood_aside, Some(StoodAside::Waiting(_)))
                && row.why.as_deref().is_some_and(|why| why.contains(LET_IN)),
            "typed: {typed}: {row:?}"
        );
    }
}

/// A Mac a rotation dropped from its vault with no `Remove` (one-mesh E2) is never joined
/// again, which would ask with the key every desk that read the rotation refuses: System
/// State's Secrets stage and `aterm init` read it blocked with its words (the way back as a new
/// Mac) and change nothing, and the port is asked only of a Mac its vault let go.
///
/// CONTROL: the same Mac with no dropped words (an act took it out) is planned a join, as
/// before. NEGATIVE CONTROL: a step that plans a join for every `Membership::Revoked` fails
/// the selected read; a probe without `Vault::dropped`'s return gives `judge`'s removal words
/// and fails the configured reads.
#[test]
fn a_dropped_mac_is_never_joined_again_and_says_how_it_comes_back() {
    let context = ctx(&[user("alice", 501)]);
    let alab = aterm_secrets::ALAB_DEFAULT_URL;
    let revoked = |name: &str| {
        let mut status = configured(Membership::Revoked { seq: 9 });
        if name != "alab" {
            let config = status.config.as_mut().unwrap();
            config.source = Source::Url(format!("https://vault.example/v1/{name}"));
            config.name = name.to_owned();
        }
        let ops = Scripted::new(vec![Ok(status)]);
        ops.subscribed(Ok(installed_state()));
        ops
    };

    // System State's Secrets stage: the selected vault.
    let ops = revoked("alab");
    ops.drops(DROPPED);
    let vault = selected(&context, ops.clone(), alab, SETUP_WAIT, Replaces::Any).unwrap();
    let probe = vault.probe(&fake(), &context);
    assert_eq!(
        probe,
        Probe::Blocked {
            why: format!("vault alab: {DROPPED}")
        }
    );
    assert_eq!(vault.planned(), Planned::default(), "nothing is planned");
    assert_eq!(ops.dropped_asks(), [9]);

    // `aterm init` on its configured vault, the ALab vault or another.
    for name in ["alab", "lab"] {
        let ops = revoked(name);
        ops.drops(DROPPED);
        let rows = run(&fake(), &ops, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
        let why = rows[0].why.as_deref().unwrap();
        assert_eq!(why, format!("vault {name}: {DROPPED}"));
        assert!(ops.changes().is_empty(), "{:?}", ops.changes());
        assert!(ops.joins().is_empty());
    }

    // Asked only of a Mac its vault let go.
    let ops = converged();
    ops.drops(DROPPED);
    let rows = run(&fake(), &ops, Mode::Plan);
    assert_ne!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert!(ops.dropped_asks().is_empty());

    // CONTROL: an act took it out, and the stage plans its join as before.
    let ops = revoked("alab");
    let vault = selected(&context, ops.clone(), alab, SETUP_WAIT, Replaces::Any).unwrap();
    assert!(
        matches!(vault.probe(&fake(), &context), Probe::Change { .. }) && vault.planned().join,
        "a removed Mac is planned a join"
    );
    assert_eq!(ops.dropped_asks(), [9]);
}

#[test]
fn a_selected_vault_joins_only_when_this_mac_is_not_in_it() {
    let context = ctx(&[user("alice", 501)]);
    let planned = |status: Status, source: &str| {
        let ops = Scripted::new(vec![Ok(status)]);
        ops.subscribed(Ok(installed_state()));
        let vault = selected(&context, ops, source, SETUP_WAIT, Replaces::Any).unwrap();
        match vault.probe(&fake(), &context) {
            Probe::Change { from, .. } => vault.planned().join.then_some(from),
            _ => None,
        }
    };
    let alab = aterm_secrets::ALAB_DEFAULT_URL;
    let pending = || configured(Membership::Pending);
    assert_eq!(planned(pending(), alab), None);
    let mut elsewhere = pending();
    elsewhere.config.as_mut().unwrap().source = Source::Url("https://vault.example/v1/alab".into());
    assert_eq!(
        planned(elsewhere, alab).as_deref(),
        Some("vault alab (https://vault.example/v1/alab), not the one System State names")
    );
    let mut keyless = pending();
    keyless.machine = None;
    assert_eq!(
        planned(keyless, alab).as_deref(),
        Some("vault alab, but this Mac's key is missing")
    );
    assert!(planned(configured(Membership::Revoked { seq: 9 }), alab).is_some());
    assert_eq!(planned(unconfigured(), alab).as_deref(), Some("no vault"));

    let team = "https://vault.example/v1/team";
    let mut rotated = pending();
    let config = rotated.config.as_mut().unwrap();
    config.source = Source::Url(team.into());
    config.name = "team".into();
    config.signer = Some([8; 32]);
    config.followed = vec![[7; 32]];
    let pinned = |pin: [u8; 32]| format!("{team}#{}", aterm_secrets::b64::encode_padded(&pin));
    assert_eq!(planned(rotated.clone(), &pinned([7; 32])), None);
    assert_eq!(planned(rotated.clone(), &pinned([8; 32])), None);
    assert!(planned(rotated, &pinned([9; 32])).is_some());

    let mut plain = configured(Membership::Plain);
    plain.config = Some(Config {
        source: Source::Path("/Volumes/Keys/team.vault".into()),
        mirrors: Vec::new(),
        name: "team".into(),
        signer: None,
        followed: Vec::new(),
        host: "m22".into(),
    });
    plain.machine = None;
    assert_eq!(planned(plain, "/Volumes/Keys/team.vault"), None);
}

#[test]
fn an_unreachable_vault_stands_aside_from_a_sync_with_nothing_journaled() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    ops.subscribed(Ok(installed_state()));
    *ops.unwritten.lock().unwrap() = true;
    *ops.unreachable.lock().unwrap() = Some(Look::Unreached(
        "fetching http://alab-secrets.local:7484/v1/alab: Connection refused (os error 61)".into(),
    ));
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert!(
        matches!(row.stood_aside, Some(StoodAside::Offline(_))),
        "{row:?}"
    );
    assert_eq!(
        row.why.as_deref(),
        Some(
            "the vault does not answer, so nothing was changed: fetching http://alab-secrets.local:7484/v1/alab: Connection refused (os error 61)"
        )
    );
    assert!(ops.log().is_empty(), "{:?}", ops.log());
    assert_eq!(*ops.reached.lock().unwrap(), 1);

    *ops.unreachable.lock().unwrap() = None;
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(ops.changes(), ["sync"]);
    journaled_first(&ops.log());
}

/// A member whose subscriber the person turned off (`aterm secrets agent off`) is no change
/// to make; with no record (the control) it is a change that installs it. A new join turns
/// sync on: its install clears the record.
#[test]
fn a_subscriber_turned_off_is_left_off_by_setup() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    for off in [true, false] {
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.turned_off.lock().unwrap() = off;
        let row = setup_pass(&ops, source, &mut None, false);
        if off {
            assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
            assert!(
                row.to.as_deref().is_some_and(|to| to.ends_with(SYNC_OFF)),
                "{row:?}"
            );
            assert!(ops.changes().is_empty(), "{:?}", ops.changes());
        } else {
            assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
            assert_eq!(ops.changes(), ["install"]);
        }
    }

    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    ops.joins_as(
        Some(configured(Membership::In { seq: 7, heard: 0 })),
        Ok(JoinOutcome::Joined {
            seq: 7,
            names: Vec::new(),
        }),
    );
    *ops.turned_off.lock().unwrap() = true;
    let _ = setup_pass(&ops, source, &mut None, false);
    assert_eq!(ops.joins().len(), 1, "the join is made");
    assert_eq!(ops.installs(), 1, "a new join turns sync on");
    assert!(!ops.turned_off(), "and the record that it was off goes");
}

/// The background converge join has no authority after a new subscriber Off. It neither
/// enrolls nor installs over that choice; the same join without Off remains eligible.
/// Explicit System State joins keep their separate person's On behavior above.
#[test]
fn the_converge_join_keeps_a_turn_off_recorded_since_its_read() {
    for off in [true, false] {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        ops.joins_as(
            Some(configured(Membership::In { seq: 7, heard: 0 })),
            Ok(JoinOutcome::Joined {
                seq: 7,
                names: Vec::new(),
            }),
        );
        *ops.turned_off.lock().unwrap() = off;
        let port: Arc<dyn SecretsOps> = ops.clone();
        let rows = crate::init::run_converge_join(
            &fake(),
            &ctx(&[user("alice", 501)]),
            port,
            aterm_secrets::ALAB_DEFAULT_URL,
            Replaces::Read(None),
            &mut crate::engine::NoJournal,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(ops.joins().len(), usize::from(!off), "{rows:?}");
        assert_eq!(ops.turned_off(), off, "off {off}: {rows:?}");
        assert_eq!(ops.installs(), u32::from(!off), "off {off}: {rows:?}");
        if off {
            assert_eq!(ops.probes(), 0);
            assert_eq!(rows[0].outcome, Outcome::NotApplicable);
        }
    }
}

#[test]
fn a_newer_sync_off_survives_an_already_probed_subscriber_repair() {
    struct OffBeforeChange {
        ops: Arc<Scripted>,
        off: bool,
        called: bool,
    }

    impl crate::engine::Journal for OffBeforeChange {
        fn before_change(
            &mut self,
            _id: &str,
            _from: &str,
            _to: &str,
            _revert: Option<&str>,
        ) -> Result<(), String> {
            self.called = true;
            *self.ops.turned_off.lock().unwrap() = self.off;
            Ok(())
        }
    }

    for off in [false, true] {
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        let context = ctx(&[user("alice", 501)]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let mut journal = OffBeforeChange {
            ops: Arc::clone(&ops),
            off,
            called: false,
        };
        let rows = crate::init::run_initial_secrets_with(
            &fake(),
            &context,
            port,
            aterm_secrets::ALAB_DEFAULT_URL,
            Replaces::Any,
            &mut None,
            SETUP_WAIT,
            &mut journal,
        )
        .unwrap();
        assert!(
            journal.called,
            "the real converge change boundary was reached"
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            ops.installs(),
            u32::from(!off),
            "an automatic repair must preserve a newer Sync off choice"
        );
        assert_eq!(ops.turned_off(), off);
        if off {
            assert!(
                rows[0]
                    .to
                    .as_deref()
                    .is_some_and(|to| to.ends_with(SYNC_OFF))
            );
        }
        assert!(ops.joins().is_empty(), "a member needs no new join");
    }
}

#[test]
fn a_newer_sync_off_cancels_an_already_planned_file_sync() {
    struct OffBeforeChange(Arc<Scripted>);
    impl crate::engine::Journal for OffBeforeChange {
        fn before_change(
            &mut self,
            _id: &str,
            _from: &str,
            _to: &str,
            _revert: Option<&str>,
        ) -> Result<(), String> {
            *self.0.turned_off.lock().unwrap() = true;
            self.0.subscribed(Ok(State::Absent));
            Ok(())
        }
    }
    for running in [false, true] {
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.unwritten.lock().unwrap() = true;
        if running {
            ops.subscribed(Ok(installed_state()));
        }
        let context = ctx(&[user("alice", 501)]);
        let port: Arc<dyn SecretsOps> = ops.clone();
        let rows = crate::init::run_initial_secrets_with(
            &fake(),
            &context,
            port,
            aterm_secrets::ALAB_DEFAULT_URL,
            Replaces::Any,
            &mut None,
            SETUP_WAIT,
            &mut OffBeforeChange(Arc::clone(&ops)),
        )
        .unwrap();
        assert_eq!(*ops.syncs.lock().unwrap(), 0, "a newer Off stops file work");
        assert_eq!(ops.installs(), 0);
        assert!(ops.turned_off());
        assert!(
            *ops.unwritten.lock().unwrap(),
            "last-synced files stay as they are"
        );
        assert_eq!(rows[0].outcome, Outcome::Changed);
        assert!(
            rows[0]
                .to
                .as_deref()
                .is_some_and(|to| to.ends_with(SYNC_OFF))
        );
    }
}

#[test]
fn sync_off_keeps_the_last_synced_files_without_installing() {
    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    *ops.unwritten.lock().unwrap() = true;
    *ops.turned_off.lock().unwrap() = true;
    let row = setup_pass(&ops, aterm_secrets::ALAB_DEFAULT_URL, &mut None, false);
    assert_eq!(row.outcome, Outcome::Ok);
    assert!(ops.changes().is_empty());
    assert!(*ops.unwritten.lock().unwrap());
    assert!(ops.turned_off());
}

/// A subscriber that never keeps running through launchd's restart window fails its step,
/// naming its log, and is never a wait; one that keeps running (the control) is changed.
#[test]
fn an_install_that_leaves_no_subscriber_running_is_a_failed_step_naming_its_log() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let log = "/Users//alice/.config/aterm/secrets/systems.alab.aterm-secrets.log";
    for dies in [true, false] {
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
        *ops.dies.lock().unwrap() = dies.then(|| {
            format!(
                "the secrets subscriber was installed but does not keep running; its log is {log}"
            )
        });
        let row = setup_pass(&ops, source, &mut None, false);
        assert_eq!(ops.changes(), ["install"]);
        if dies {
            assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
            assert!(
                row.why.as_deref().is_some_and(|why| why.contains(log)),
                "{row:?}"
            );
        } else {
            assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        }
    }
}

#[test]
fn no_vault_at_the_source_stands_aside_before_a_line_or_a_key() {
    let source = "/Volumes/Keys/team.vault";
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.absent.lock().unwrap() = Some("/Volumes/Keys/team.vault: no vault there".into());
    let mut held = Some(password());
    for attempt in 1..=3 {
        let row = setup_pass(&ops, source, &mut held, false);
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(
            matches!(row.stood_aside, Some(StoodAside::Absent(_))),
            "{row:?}"
        );
        assert_eq!(
            row.why.as_deref(),
            Some(
                "no vault is there now, so nothing was changed: /Volumes/Keys/team.vault: no vault there"
            )
        );
        assert!(ops.log().is_empty(), "{:?}", ops.log());
        assert_eq!(*ops.reached.lock().unwrap(), attempt);
        assert!(held.is_some());
    }
    *ops.absent.lock().unwrap() = None;
    let mut joined = configured(Membership::In { seq: 7, heard: 0 });
    joined.config = Some(Config {
        source: Source::Path(source.into()),
        mirrors: Vec::new(),
        name: "team".into(),
        signer: None,
        followed: Vec::new(),
        host: "m22".into(),
    });
    *ops.statuses.lock().unwrap() = vec![Ok(unconfigured()), Ok(unconfigured()), Ok(joined)].into();
    let row = setup_pass(&ops, source, &mut held, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(ops.joins(), [(source.to_owned(), false)]);
    journaled_first(&ops.log());
}

#[test]
fn a_join_from_another_vault_keeps_it_and_its_line_puts_this_mac_back() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let team = || {
        let mut team = configured(Membership::In { seq: 4, heard: 0 });
        let config = team.config.as_mut().unwrap();
        config.source = Source::Url("https://vault.example/v1/team".into());
        config.name = "team".into();
        team
    };
    let ops = Scripted::new(vec![Ok(team())]);
    ops.joins_as(
        Some(configured(Membership::In { seq: 7, heard: 0 })),
        Ok(JoinOutcome::Joined {
            seq: 7,
            names: vec!["gh_access_token".into()],
        }),
    );
    ops.subscribed(Ok(installed_state()));
    let row = setup_pass(&ops, source, &mut Some(password()), false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    let kept = "~/.config/aterm/secrets/before-join-1800000000";
    let log = ops.log();
    assert_eq!(
        log[1..],
        [
            "keep /Users//alice/.config/aterm/secrets/before-join-1800000000".to_owned(),
            format!("join {source}"),
        ],
        "{log:?}"
    );
    journaled_first(&log);
    let line = &log[0];
    assert!(
        line.contains(&format!(
            "cp {kept}/source.toml {kept}/state.toml ~/.config/aterm/secrets/ and run aterm secrets sync"
        )),
        "{line}"
    );
    assert!(
        line.contains("rm each file aterm secrets ls names"),
        "{line}"
    );
    assert!(
        !line.contains("rm ~/.config/aterm/secrets/source.toml"),
        "{line}"
    );
    assert!(!line.contains("identity"), "the key was there: {line}");
    assert!(!line.contains("agent off"), "the subscriber ran: {line}");

    let ops = Scripted::new(vec![Ok(team())]);
    ops.subscribed(Ok(installed_state()));
    *ops.unkept.lock().unwrap() = Some("Permission denied (os error 13)".into());
    let mut held = Some(password());
    let row = setup_pass(&ops, source, &mut held, false);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert!(ops.joins().is_empty());
    assert!(held.is_some());

    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    ops.joins_as(
        Some(configured(Membership::In { seq: 7, heard: 0 })),
        Ok(JoinOutcome::Joined {
            seq: 7,
            names: vec!["gh_access_token".into()],
        }),
    );
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(ops.changes(), [format!("join {source}"), "install".into()]);
    let line = &ops.log()[0];
    assert!(
        line.contains("rm ~/.config/aterm/secrets/source.toml ~/.config/aterm/secrets/state.toml"),
        "{line}"
    );
    assert!(
        line.contains("rm ~/.config/aterm/secrets/identity (the key the join made)"),
        "{line}"
    );
    assert!(!line.contains("before-join-"), "{line}");
}

#[test]
fn an_apply_makes_only_what_the_probe_planned_and_its_line_undoes() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    ops.subscribed(Ok(installed_state()));
    *ops.unwritten.lock().unwrap() = true;
    *ops.meanwhile.lock().unwrap() = Some((
        configured(Membership::In { seq: 7, heard: 0 }),
        State::Absent,
    ));
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(ops.changes(), ["sync"], "{row:?}");
    assert!(!ops.log()[0].contains("agent off"), "{:?}", ops.log());
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(ops.changes(), ["sync", "install"]);
    assert!(ops.log()[2].contains("agent off"), "{:?}", ops.log());

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    ops.subscribed(Ok(installed_state()));
    *ops.unwritten.lock().unwrap() = true;
    *ops.meanwhile.lock().unwrap() = Some((
        configured(Membership::Revoked { seq: 8 }),
        installed_state(),
    ));
    let row = setup_pass(&ops, source, &mut Some(password()), false);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert!(
        row.why
            .as_deref()
            .unwrap()
            .starts_with("this Mac's vault state changed after it was read"),
        "{row:?}"
    );
    assert!(ops.changes().is_empty(), "{:?}", ops.log());
    *ops.unwritten.lock().unwrap() = false;
    *ops.statuses.lock().unwrap() = vec![
        Ok(configured(Membership::Revoked { seq: 8 })),
        Ok(configured(Membership::Revoked { seq: 8 })),
        Ok(configured(Membership::In { seq: 9, heard: 0 })),
    ]
    .into();
    let row = setup_pass(&ops, source, &mut Some(password()), false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(ops.joins().len(), 1);
    assert!(
        ops.log()[1].contains("this Mac was removed from vault"),
        "{:?}",
        ops.log()
    );

    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 7, heard: 0 }))]);
    *ops.spoiled_by_install.lock().unwrap() = true;
    setup_pass(&ops, source, &mut None, false);
    assert_eq!(ops.changes(), ["install"], "{:?}", ops.log());
    assert!(!ops.log()[0].contains(".replaced"), "{:?}", ops.log());
    *ops.spoiled_by_install.lock().unwrap() = false;
    let row = setup_pass(&ops, source, &mut None, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(ops.changes(), ["install", "sync"]);
    assert!(ops.log()[2].contains(".replaced"), "{:?}", ops.log());
}

fn joined_elsewhere() -> Status {
    let mut status = configured(Membership::In { seq: 3, heard: 0 });
    let config = status.config.as_mut().unwrap();
    config.source = Source::Url("https://vault.example/v1/me?k=fixture-query".into());
    config.name = "me".into();
    status
}

#[test]
fn a_vault_joined_while_the_vault_is_asked_is_not_replaced() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.meanwhile.lock().unwrap() = Some((joined_elsewhere(), installed_state()));
    let mut held = Some(password());
    let row = setup_pass(&ops, source, &mut held, false);
    assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
    assert!(
        matches!(row.stood_aside, Some(StoodAside::Lock(_))),
        "{row:?}"
    );
    assert_eq!(
        row.why.as_deref(),
        Some(
            "this Mac's vault changed while the vault was asked (now vault me (https://vault.example/v1/me)), so nothing was changed; the next try reads it"
        )
    );
    assert!(ops.log().is_empty(), "{:?}", ops.log());
    assert!(ops.joins().is_empty());
    assert!(held.is_some(), "kept for a try that joins");
}

#[test]
fn an_apply_joins_only_over_the_vault_its_probe_read() {
    let context = ctx(&[user("alice", 501)]);
    let ops = Scripted::new(vec![Ok(unconfigured()), Ok(joined_elsewhere())]);
    let port: Arc<dyn SecretsOps> = ops.clone();
    let vault = selected(
        &context,
        port,
        aterm_secrets::ALAB_DEFAULT_URL,
        SETUP_WAIT,
        Replaces::Any,
    )
    .unwrap();
    let sys = fake();
    assert!(matches!(vault.probe(&sys, &context), Probe::Change { .. }));
    let refused = vault.apply(&sys, &context).unwrap_err();
    assert_eq!(
        refused,
        "this Mac's vault state changed after it was read (vault me (https://vault.example/v1/me), not the one System State names), so nothing was changed; the next try reads it"
    );
    assert!(ops.changes().is_empty(), "{:?}", ops.log());
}

#[test]
fn a_join_replaces_only_the_vault_its_caller_read() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    let ops = Scripted::new(vec![Ok(joined_elsewhere())]);
    let row = setup_pass_over(&ops, source, Replaces::Read(None), &mut None, false);
    assert_eq!(row.outcome, Outcome::Unknown, "{row:?}");
    assert_eq!(
        row.why.as_deref(),
        Some(
            "this Mac's vault changed after it was read (now vault me (https://vault.example/v1/me)), so nothing was changed; the next try reads it"
        )
    );
    assert!(ops.log().is_empty(), "{:?}", ops.log());
    assert_eq!(*ops.reached.lock().unwrap(), 0);

    let ops = Scripted::new(vec![
        Ok(joined_elsewhere()),
        Ok(joined_elsewhere()),
        Ok(configured(Membership::In { seq: 7, heard: 0 })),
    ]);
    ops.subscribed(Ok(installed_state()));
    let read = joined_elsewhere().config;
    let row = setup_pass_over(&ops, source, Replaces::Read(read), &mut None, false);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(
        ops.changes(),
        [
            "keep /Users//alice/.config/aterm/secrets/before-join-1800000000".to_owned(),
            format!("join {source}")
        ]
    );
    assert_eq!(
        *ops.replacement_reads.lock().unwrap(),
        vec![Replaces::Read(joined_elsewhere().config); 2],
        "the snapshot and the actual join receive the same admitted configuration"
    );

    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
        "injected conditional join refusal",
    )));
    let mut held = Some(password());
    let row = setup_pass_over(&ops, source, Replaces::Read(None), &mut held, false);
    assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
    assert!(held.is_some());
    assert_eq!(ops.changes(), [format!("join {source}")]);
    assert_eq!(
        *ops.replacement_reads.lock().unwrap(),
        [Replaces::Read(None)]
    );
}

fn caller_read_vault() -> (String, Status) {
    let source = "https://vault.example/v1/team";
    let mut status = configured(Membership::Pending);
    let config = status.config.as_mut().unwrap();
    config.source = Source::Url(source.into());
    config.name = "team".into();
    let pinned = format!(
        "{source}#{}",
        aterm_secrets::b64::encode_padded(&config.signer.unwrap())
    );
    (pinned, status)
}

fn assert_caller_read_vault_is_not_rebound(change: impl FnOnce(&mut Config)) {
    let (source, read) = caller_read_vault();
    let mut newer = read.clone();
    change(newer.config.as_mut().unwrap());
    let ops = Scripted::new(vec![Ok(newer)]);
    ops.subscribed(Ok(installed_state()));
    *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
        code: CODE.into(),
        host: "newer-name".into(),
    }));
    let mut held = Some(password());
    let row = setup_pass_over(&ops, &source, Replaces::Read(read.config), &mut held, false);
    assert!(
        ops.log().is_empty(),
        "a newer configuration must not authorize the older join: {:?}",
        ops.log()
    );
    assert_eq!(row.outcome, Outcome::Unknown, "{row:?}");
    assert!(ops.replacement_reads.lock().unwrap().is_empty());
    assert_eq!(*ops.reached.lock().unwrap(), 0);
    assert_eq!(ops.installs(), 0);
    assert_eq!(*ops.syncs.lock().unwrap(), 0);
    assert!(held.is_some(), "the refused join keeps its password");
}

#[test]
fn a_new_host_does_not_replace_the_caller_read_vault_snapshot() {
    assert_caller_read_vault_is_not_rebound(|config| config.host = "newer-name".into());
}

#[test]
fn a_new_signer_does_not_replace_the_caller_read_vault_snapshot() {
    assert_caller_read_vault_is_not_rebound(|config| config.signer = Some([8; 32]));
}

#[test]
fn a_caller_read_vault_snapshot_change_is_refused_at_later_join_boundaries() {
    let changes: [fn(&mut Config); 2] = [
        |config| config.host = "newer-name".into(),
        |config| config.signer = Some([8; 32]),
    ];
    for change in changes {
        let (source, read) = caller_read_vault();
        let mut newer = read.clone();
        change(newer.config.as_mut().unwrap());
        let ops = Scripted::new(vec![Ok(read.clone())]);
        ops.subscribed(Ok(installed_state()));
        *ops.meanwhile.lock().unwrap() = Some((newer.clone(), installed_state()));
        let mut held = Some(password());
        let row = setup_pass_over(
            &ops,
            &source,
            Replaces::Read(read.config.clone()),
            &mut held,
            false,
        );
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(matches!(row.stood_aside, Some(StoodAside::Lock(_))));
        assert_eq!(*ops.reached.lock().unwrap(), 1);
        assert!(ops.log().is_empty());
        assert!(ops.replacement_reads.lock().unwrap().is_empty());
        assert!(held.is_some());

        let context = ctx(&[user("alice", 501)]);
        let ops = Scripted::new(vec![Ok(read), Ok(newer)]);
        let vault = selected(&context, ops.clone(), &source, SETUP_WAIT, Replaces::Any).unwrap();
        vault.hold_password(Some(password()));
        assert!(matches!(
            vault.probe(&fake(), &context),
            Probe::Change { .. }
        ));
        assert!(vault.apply(&fake(), &context).is_err());
        assert!(ops.log().is_empty());
        assert!(ops.replacement_reads.lock().unwrap().is_empty());
        assert!(vault.unspent_password().is_some());
    }
}

#[test]
fn the_caller_read_vault_snapshot_can_still_join_with_its_verified_pin() {
    let (source, read) = caller_read_vault();
    for rotated in [false, true] {
        let mut current = read.clone();
        if rotated {
            let config = current.config.as_mut().unwrap();
            config.followed = vec![config.signer.unwrap()];
            config.signer = Some([8; 32]);
        }
        let expected = current.config.clone();
        let ops = Scripted::new(vec![Ok(current)]);
        ops.subscribed(Ok(installed_state()));
        *ops.outcome.lock().unwrap() = Some(Ok(JoinOutcome::Pending {
            code: CODE.into(),
            host: "m22".into(),
        }));
        let mut held = Some(password());
        let row = setup_pass_over(
            &ops,
            &source,
            Replaces::Read(expected.clone()),
            &mut held,
            false,
        );
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        assert!(matches!(row.stood_aside, Some(StoodAside::Waiting(_))));
        journaled_first(&ops.log());
        assert_eq!(ops.joins(), [(source.clone(), false)]);
        assert_eq!(
            *ops.replacement_reads.lock().unwrap(),
            vec![Replaces::Read(expected); 2]
        );
        assert!(held.is_none(), "the admitted request consumed its password");
    }

    let mut rotated = read.clone();
    let config = rotated.config.as_mut().unwrap();
    config.followed = vec![config.signer.unwrap()];
    config.signer = Some([8; 32]);
    let ops = Scripted::new(vec![Ok(rotated)]);
    ops.subscribed(Ok(installed_state()));
    let row = setup_pass_over(&ops, &source, Replaces::Read(read.config), &mut None, false);
    assert!(
        row.outcome == Outcome::Blocked && matches!(row.stood_aside, Some(StoodAside::Waiting(_))),
        "{row:?}"
    );
    assert!(ops.log().is_empty());
    assert_eq!(*ops.reached.lock().unwrap(), 0);
}

fn signed_file_vault(root: &Path, name: &str) -> String {
    let source = root.join(format!("{name}.vault")).display().to_string();
    let admin = Paths::for_home(&root.join(format!("{name}-admin")));
    let created = aterm_secrets::client::create(&admin, &source, None, Some("admin")).unwrap();
    format!("{source}#{}", created.signer)
}

fn pending_file_join(paths: &Paths, source: &str, host: &str) {
    let outcome = aterm_secrets::client::join(
        paths,
        &aterm_secrets::client::JoinOptions {
            source: source.into(),
            host: Some(host.into()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: aterm_secrets::client::JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();
    assert!(matches!(outcome, JoinOutcome::Pending { .. }));
}

fn live_file_join(live: &LiveSecrets, source: &str, replaces: &Replaces) -> JoinOutcome {
    let paths = live.paths();
    let held = password();
    live.with_join_options(
        &fake(),
        (source, replaces),
        Some(&held),
        false,
        &|| {},
        |options| {
            // Supply the platform default at the client boundary, so the real
            // signed-file join never asks scutil or reads this Mac's name.
            let options = aterm_secrets::client::JoinOptions {
                source: options.source.clone(),
                host: Some(
                    options
                        .host
                        .clone()
                        .unwrap_or_else(|| "inert-default".into()),
                ),
                interactive: options.interactive,
                wait: options.wait,
                confirm: options.confirm,
                password: options.password.clone(),
                fetched: options.fetched,
                requested: options.requested,
                say: options.say,
            };
            match replaces {
                Replaces::Any => aterm_secrets::client::join(&paths, &options),
                Replaces::Read(expected) => {
                    aterm_secrets::client::join_if_unchanged(&paths, &options, expected.as_ref())
                }
            }
        },
    )
    .unwrap()
}

#[test]
fn a_join_approved_before_the_wait_begins_ends_it_at_once() {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = signed_file_vault(root.path(), "team");
    let admin = Paths::for_home(&root.path().join("team-admin"));
    let reader = Paths::for_home(&root.path().join("reader"));
    pending_file_join(&reader, &source, "reader");
    let (recipient, _) = aterm_secrets::client::status(&reader)
        .unwrap()
        .machine
        .unwrap();
    let opened = aterm_secrets::client::admin_open(&admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.add_machine("reader", &recipient.to_bech32()).unwrap();
    aterm_secrets::client::admin_publish(&admin, &opened, &doc).unwrap();
    // The subscriber the join started applies the approval first.
    aterm_secrets::client::sync(&reader, None).unwrap();
    let said = Mutex::new(Vec::new());
    let outcome = wait_for_approval(
        &reader,
        &source,
        Duration::from_secs(60),
        true,
        None,
        &|line| {
            said.lock().unwrap().push(line.to_owned());
        },
        &|| Ok(()),
    )
    .unwrap();
    assert!(
        matches!(outcome, Some(JoinOutcome::Joined { .. })),
        "{outcome:?}"
    );
    assert!(said.lock().unwrap().is_empty(), "{said:?}");
}

/// `--vault`'s read against a live loopback vault: an unpinned vault hands back its
/// fingerprint and a pin the next read accepts, a pin the vault does not answer to and a
/// vault the server does not hold are refused there, before anything changes.
#[test]
fn a_named_vault_is_read_once_and_a_pin_it_does_not_answer_to_is_refused() {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let running = aterm_secrets::server::start(
        socket,
        &aterm_secrets::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .unwrap();
    let admin = Paths::for_home(&root.path().join("admin"));
    let url = format!("http://{}/v1/team", running.addr);
    let created = aterm_secrets::client::create(&admin, &url, None, Some("admin")).unwrap();
    let live = LiveSecrets::new(&root.path().join("joiner"));
    // This test's own server answers each check within CHECK_WAIT plus the one-minute hang
    // detector (aterm_spec::wait::MUST_HAPPEN) in place of the fetch margin's 5 s.
    let _answering = aterm_secrets::client::ServersAnswering::within(Duration::from_secs(60));

    let Ok(Checked::Unpinned { fingerprint, pin }) = live.check(&url) else {
        panic!("an unpinned vault is put to the person");
    };
    assert_eq!(fingerprint, created.fingerprint);
    assert_eq!(live.check(&format!("{url}#{pin}")), Ok(Checked::Pinned));

    let other = aterm_secrets::b64::encode_padded(&[7u8; 32]);
    let refused = live.check(&format!("{url}#{other}"));
    assert!(refused.is_err(), "{refused:?}");

    let missing = live.check(&format!("http://{}/v1/nobody", running.addr));
    assert!(missing.is_err(), "{missing:?}");
    running.stop();
    assert!(
        !root.path().join("joiner").exists(),
        "a read changes nothing"
    );
}

#[test]
fn a_url_join_says_nothing_itself_and_its_approval_lands_within_the_wait() {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let running = aterm_secrets::server::start(
        socket,
        &aterm_secrets::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .unwrap();
    let admin = Paths::for_home(&root.path().join("admin"));
    let url = format!("http://{}/v1/team", running.addr);
    let created =
        aterm_secrets::client::test_create_with_password(&admin, &url, None, Some("admin"))
            .unwrap();
    let source = format!("{url}#{}", created.signer);
    let held = aterm_secrets::client::admin_open(&admin)
        .unwrap()
        .doc
        .join_password()
        .unwrap();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let live = LiveSecrets {
        heard: Some(heard.clone()),
        ..LiveSecrets::new(&root.path().join("joiner"))
    };
    let outcome = live
        .with_join_options(
            &fake(),
            (&source, &Replaces::Any),
            Some(&SecretBytes::from(held.as_slice())),
            true,
            &|| {},
            |options| {
                aterm_secrets::client::join(
                    &live.paths(),
                    &aterm_secrets::client::JoinOptions {
                        host: Some("joiner".into()),
                        source: options.source.clone(),
                        password: options.password.clone(),
                        ..*options
                    },
                )
            },
        )
        .unwrap();
    assert!(
        matches!(outcome, JoinOutcome::Pending { .. }),
        "{outcome:?}"
    );
    assert!(heard.lock().unwrap().is_empty(), "{heard:?}");
    assert!(
        wait_for_approval(
            &live.paths(),
            &source,
            Duration::from_secs(60),
            false,
            None,
            &|line| live.say(line),
            &|| Err(aterm_secrets::Error::new("setup stopped"))
        )
        .is_err(),
        "a stopped wait ends at once"
    );
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let approver = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = aterm_secrets::client::auto_approve(&admin);
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };
    // A hang detector, not a budget: the approval lands within the approver's next pass.
    let inside = wait_for_approval(
        &live.paths(),
        &source,
        Duration::from_secs(60),
        false,
        None,
        &|line| live.say(line),
        &|| Ok(()),
    )
    .unwrap();
    stop.store(true, Ordering::SeqCst);
    approver.join().unwrap();
    running.stop();
    assert!(matches!(inside, Some(JoinOutcome::Joined { .. })));
    assert!(
        !heard.lock().unwrap().iter().any(|line| line == REFUSED),
        "{heard:?}"
    );
}

/// A join password an admin Mac's approver refuses ends the join window at once, said as
/// [`REFUSED`], so the wait ends at the verdict instead of the window's end.
#[test]
fn a_refused_join_password_ends_the_join_window_at_the_verdict() {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let running = aterm_secrets::server::start(
        socket,
        &aterm_secrets::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .unwrap();
    let admin = Paths::for_home(&root.path().join("admin"));
    let url = format!("http://{}/v1/team", running.addr);
    let created =
        aterm_secrets::client::test_create_with_password(&admin, &url, None, Some("admin"))
            .unwrap();
    let source = format!("{url}#{}", created.signer);
    let heard = Arc::new(Mutex::new(Vec::new()));
    let live = LiveSecrets {
        heard: Some(heard.clone()),
        ..LiveSecrets::new(&root.path().join("joiner"))
    };
    let outcome = live
        .with_join_options(
            &fake(),
            (&source, &Replaces::Any),
            Some(&SecretBytes::from("a-mistyped-join-password".as_bytes())),
            true,
            &|| {},
            |options| {
                aterm_secrets::client::join(
                    &live.paths(),
                    &aterm_secrets::client::JoinOptions {
                        host: Some("joiner".into()),
                        source: options.source.clone(),
                        password: options.password.clone(),
                        ..*options
                    },
                )
            },
        )
        .unwrap();
    assert!(
        matches!(outcome, JoinOutcome::Pending { .. }),
        "{outcome:?}"
    );
    let refused = {
        // This test's own server answers the approver's verdict post, which nothing
        // retries, within the one-minute hang detector (aterm_spec::wait::MUST_HAPPEN), not
        // the product's 10 s.
        let _answering = aterm_secrets::client::ServersAnswering::within(Duration::from_secs(60));
        aterm_secrets::client::auto_approve(&admin).unwrap()
    };
    assert_eq!(refused.refused.len(), 1, "{refused:?}");
    // A window far past the hang detector: only the verdict ends it within it.
    let (ended, waited) = std::sync::mpsc::channel();
    let joiner = root.path().join("joiner");
    let heard_by = heard.clone();
    std::thread::spawn(move || {
        let live = LiveSecrets {
            heard: Some(heard_by),
            ..LiveSecrets::new(&joiner)
        };
        let _ = ended.send(
            wait_for_approval(
                &live.paths(),
                &source,
                Duration::from_secs(600),
                false,
                None,
                &|line| live.say(line),
                &|| Ok(()),
            )
            .map(|outcome| outcome.is_some()),
        );
    });
    let inside = waited
        .recv_timeout(Duration::from_secs(60))
        .expect("CONTROL: a window that reads no verdict runs its 600 s");
    running.stop();
    assert!(!inside.unwrap());
    assert!(heard.lock().unwrap().iter().any(|line| line == REFUSED));
}

#[test]
fn live_join_keeps_the_name_of_the_pending_selected_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = signed_file_vault(root.path(), "team");
    let live = LiveSecrets::unattended(&root.path().join("reader"));
    pending_file_join(&live.paths(), &source, "custom-name");
    let expected = aterm_secrets::client::config(&live.paths())
        .unwrap()
        .unwrap();
    assert_eq!(expected.host, "custom-name");
    assert!(matches!(
        aterm_secrets::client::status(&live.paths())
            .unwrap()
            .membership,
        Membership::Pending
    ));
    let outcome = live_file_join(&live, &source, &Replaces::Read(Some(expected.clone())));
    let saved = aterm_secrets::client::config(&live.paths())
        .unwrap()
        .unwrap();
    assert_eq!(
        saved, expected,
        "a password retry keeps the configured name"
    );
    assert!(matches!(
        outcome,
        JoinOutcome::Pending { host, .. } if host == "custom-name"
    ));
}

#[test]
fn live_join_uses_the_default_name_for_a_fresh_or_different_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = signed_file_vault(root.path(), "team");
    let other = signed_file_vault(root.path(), "other");
    let live = LiveSecrets::unattended(&root.path().join("reader"));
    let outcome = live_file_join(&live, &source, &Replaces::Read(None));
    assert!(matches!(
        outcome,
        JoinOutcome::Pending { host, .. } if host == "inert-default"
    ));
    assert_eq!(
        aterm_secrets::client::config(&live.paths())
            .unwrap()
            .unwrap()
            .host,
        "inert-default"
    );
    pending_file_join(&live.paths(), &source, "custom-name");
    let before = aterm_secrets::client::config(&live.paths()).unwrap();
    let outcome = live_file_join(&live, &other, &Replaces::Read(before));
    assert!(matches!(
        outcome,
        JoinOutcome::Pending { host, .. } if host == "inert-default"
    ));
    let after = aterm_secrets::client::config(&live.paths())
        .unwrap()
        .unwrap();
    assert_eq!(after.host, "inert-default");
    assert_eq!(after.name, "other");
}

#[test]
fn a_source_that_is_no_vault_source_is_not_called_offline() {
    let sys = fake();
    sys.answer(None);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.unreachable.lock().unwrap() = Some(Look::Unreached("vault.example: no such host".into()));
    *ops.outcome.lock().unwrap() = Some(Err(aterm_secrets::Error::new(
        "that URL is not usable: it needs a host, and no user:password@, spaces or control characters",
    )));
    let rows = run_with(
        &sys,
        &ops,
        Mode::Apply,
        Some("https://who:typed@vault.example/v1/team"),
    );
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    assert!(rows[0].stood_aside.is_none(), "{rows:?}");
    assert_eq!(*ops.reached.lock().unwrap(), 0);
}

#[test]
fn an_offline_default_join_keeps_bootstrap_and_its_subscriber_for_retry() {
    let sys = fake();
    sys.answer(None);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.unreachable.lock().unwrap() =
        Some(Look::Unreached("alab-secrets.local: no such host".into()));
    let rows = run(&sys, &ops, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert_eq!(
        rows[0].stood_aside.as_ref().map(StoodAside::exit_status),
        Some(69)
    );
    assert!(ops.joins().is_empty());
    assert_eq!(*ops.prepared.lock().unwrap(), 1);
    assert_eq!(*ops.native_tries.lock().unwrap(), 1);
    assert_eq!(*ops.reached.lock().unwrap(), 0);
    assert_eq!(ops.changes(), ["install"]);
}

#[test]
fn an_offline_explicit_join_stands_aside_before_anything_is_made() {
    let sys = fake();
    sys.answer(None);
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    *ops.unreachable.lock().unwrap() = Some(Look::Unreached("vault.example: no such host".into()));
    let rows = run_with(
        &sys,
        &ops,
        Mode::Apply,
        Some("https://vault.example/v1/team"),
    );
    assert_eq!(rows[0].outcome, Outcome::Blocked, "{rows:?}");
    assert_eq!(
        rows[0].stood_aside.as_ref().map(StoodAside::exit_status),
        Some(69)
    );
    assert!(ops.joins().is_empty());
    assert_eq!(*ops.prepared.lock().unwrap(), 0);
    assert_eq!(*ops.native_tries.lock().unwrap(), 0);
    assert_eq!(*ops.reached.lock().unwrap(), 1);
    assert_eq!(ops.installs(), 0);
    assert!(ops.changes().is_empty());
}

/// A join waiting to be let in on a Mac with Sync off says so and is no step and nothing to
/// wait on (an approval lands only once sync is on), and setup installs nothing over the
/// record. With no record (the control) the subscriber is installed.
#[test]
fn a_pending_join_with_sync_off_says_so_and_installs_nothing() {
    let source = aterm_secrets::ALAB_DEFAULT_URL;
    for off in [false, true] {
        let ops = Scripted::new(vec![Ok(configured(Membership::Pending))]);
        ops.subscribed(Ok(State::Absent));
        *ops.turned_off.lock().unwrap() = off;
        let row = setup_pass(&ops, source, &mut None, false);
        if off {
            assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
            assert!(
                row.to.as_deref().is_some_and(
                    |to| to.contains(SYNC_OFF) && to.ends_with("the join lands once it is on")
                ),
                "{row:?}"
            );
            assert!(ops.changes().is_empty(), "{:?}", ops.changes());
        } else {
            assert!(
                ops.changes().iter().any(|change| change == "install"),
                "{:?} {row:?}",
                ops.changes()
            );
        }
    }
}

/// `aterm init` and System State keep Sync off for a member or plain source even with its
/// files unwritten: no path installs the subscriber, repairs the files or clears Off.
/// Without Off (the control) both install and repair as needed.
#[test]
fn aterm_init_keeps_the_persons_sync_off() {
    for membership in [Membership::In { seq: 7, heard: 0 }, Membership::Plain] {
        for selected in [false, true] {
            for off in [true, false] {
                for unwritten in [true, false] {
                    let ops =
                        Scripted::new((0..4).map(|_| Ok(configured(membership.clone()))).collect());
                    ops.subscribed(Ok(State::Absent));
                    *ops.turned_off.lock().unwrap() = off;
                    *ops.unwritten.lock().unwrap() = unwritten;
                    let row = if selected {
                        setup_pass(&ops, aterm_secrets::ALAB_DEFAULT_URL, &mut None, false)
                    } else {
                        run(&fake(), &ops, Mode::Apply).remove(0)
                    };
                    let case = format!(
                        "{membership:?} selected={selected} off={off} unwritten={unwritten}"
                    );
                    assert_eq!(
                        ops.installs() > 0,
                        !off,
                        "{case}: {:?} {row:?}",
                        ops.changes()
                    );
                    assert_eq!(
                        *ops.syncs.lock().unwrap(),
                        u32::from(!off && unwritten),
                        "{case}"
                    );
                    assert_eq!(ops.turned_off(), off, "{case}");
                    assert_eq!(*ops.unwritten.lock().unwrap(), off && unwritten, "{case}");
                    if off {
                        assert_eq!(row.outcome, Outcome::Ok, "{case}: {row:?}");
                        let to = row.to.as_deref().expect("Sync off value");
                        assert!(to.ends_with(SYNC_OFF), "{case}: {row:?}");
                        assert!(
                            !to.contains(" in ~/.secrets") && !to.contains("kept in sync"),
                            "{case}: no materialization claim: {row:?}"
                        );
                        assert!(ops.changes().is_empty(), "{case}: {:?}", ops.changes());
                    }
                }
            }
        }
    }
}

#[test]
fn swarm_enabled_false_marks_secrets_step_not_applicable_and_probes_nothing() {
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    *ops.swarm.lock().unwrap() = Some(Ok(false));
    for mode in [Mode::Plan, Mode::Apply] {
        let row = run(&fake(), &ops, mode).remove(0);
        assert_eq!(row.outcome, Outcome::NotApplicable, "{mode:?}: {row:?}");
        assert_eq!(
            row.why.as_deref(),
            Some(aterm_secrets::swarm::SWARM_OFF),
            "{mode:?}: {row:?}"
        );
    }
    assert_eq!(
        ops.probes(),
        0,
        "no LAN probe while [swarm] enabled = false"
    );
    assert!(ops.changes().is_empty());
    // Negative control: sharing enabled admits native bootstrap and the answering vault join.
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.joins_as(
        Some(configured(Membership::In { seq: 7, heard: 0 })),
        Ok(JoinOutcome::Joined {
            seq: 7,
            names: vec!["gh_access_token".into()],
        }),
    );
    ops.alab_answers();
    *ops.swarm.lock().unwrap() = Some(Ok(true));
    let sys = fake();
    sys.answer(None);
    let row = run(&sys, &ops, Mode::Apply).remove(0);
    assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
    assert_eq!(ops.probes(), 1);
    assert_eq!(*ops.prepared.lock().unwrap(), 1);
    assert_eq!(*ops.native_tries.lock().unwrap(), 1);
}
