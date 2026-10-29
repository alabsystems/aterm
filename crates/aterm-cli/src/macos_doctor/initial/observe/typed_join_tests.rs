// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Secrets vault row as the setup passes that feed it leave it, where nothing answered at the
//! ALab vault's address, and the GitHub repositories row beside it: a join password the person
//! typed waits only while its run holds it for a try, whether the try ran as the Secrets stage or
//! in its lane, and whichever aterm process holds it; a process reads what its own probe, Check
//! again's look or the run's last try heard only on the network that heard it, an address coming
//! and going there aside; and the repositories read on this Mac's own credentials where that
//! leaves the vault off this network, as their stage runs them. No test probes the ALab vault, so
//! this process's last probe stays unasked ([`scope::heard_at`]), as a relaunch's is; a test hands
//! [`scope::heard_from`] one instead.

use super::*;
use aterm_machines::init_secrets::SecretsOps;
use aterm_types::macos_setup::{AccessOutcome, BASE_PLAN};
use std::sync::{Arc, Mutex};

/// One Secrets try, ended as the live join's probe ends it here (`row`, [`unjoined_alab_row`]'s):
/// the join password left for the next try, or spent (`spends`). Each try says where it ran
/// (`ran`): in the lane beside the Workstation stage, or as the stage.
#[derive(Clone)]
struct Try {
    row: Row,
    spends: bool,
    ran: Arc<Mutex<Vec<&'static str>>>,
}

impl Try {
    fn new(status: &str, note: &str) -> Self {
        Self {
            row: Row {
                name: Stage::Secrets.name().into(),
                status: status.into(),
                detail: note.into(),
                elapsed_ms: None,
            },
            spends: false,
            ran: Arc::default(),
        }
    }

    fn run(
        &self,
        password: &mut Option<SecretBytes>,
        path: &'static str,
    ) -> Result<Vec<Row>, String> {
        self.ran.lock().unwrap().push(path);
        if self.spends {
            *password = None;
        }
        Ok(vec![self.row.clone()])
    }

    fn ran(&self) -> Vec<&'static str> {
        self.ran.lock().unwrap().clone()
    }
}

/// A network this Mac is on, by the key a kept look is held to (its gateway and IPv4 subnets) and
/// its routes (`None`: one it cannot read), read at a moment (unix seconds), with one build and no
/// vault of its own.
struct Network {
    key: Option<&'static str>,
    routes: Option<&'static str>,
    now: u64,
}

impl secrets_retry::Reads for Network {
    fn build(&self) -> String {
        "test-build".into()
    }

    fn now(&self) -> u64 {
        self.now
    }

    fn routes(&self) -> Option<String> {
        self.routes.map(str::to_owned)
    }

    fn network_key(&self) -> Option<String> {
        self.key.map(str::to_owned)
    }

    fn vault(&self) -> Option<Arc<dyn SecretsOps>> {
        None
    }
}

/// The network every try here runs on, and another the Mac is taken to.
const HOME: Network = Network {
    key: Some("home gateway"),
    routes: Some("home routes"),
    now: 1_000,
};
const OFFICE: Network = Network {
    key: Some("office gateway"),
    routes: Some("office routes"),
    now: 1_000,
};

/// [`HOME`] a day after its tries, past every retry pause ([`secrets_retry::LONGEST`]).
const HOME_LATER: Network = Network {
    now: 1_000 + 86_400,
    ..HOME
};

/// A network this Mac cannot read.
const UNREAD: Network = Network {
    key: None,
    routes: None,
    now: 1_000,
};

/// Every try's pace, read on [`HOME`]: a pass after a try lands inside its retry pause.
fn pace() -> Pace {
    secrets_retry::pace(&HOME)
}

/// The Secrets lane beside the Workstation stage.
struct Lane(Try);

impl SecretsLane for Lane {
    fn secrets(
        &mut self,
        _plan: &Plan,
        _replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String> {
        self.0.run(password, "lane")
    }

    fn secrets_pace(&mut self) -> Option<Pace> {
        Some(pace())
    }

    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
        Ok(None)
    }

    fn current_vault(&mut self, _plan: &Plan) -> Result<Vec<Row>, String> {
        unreachable!("this Mac is in no vault")
    }
}

/// A Mac whose every stage is in place but the Secrets try, which runs in the lane (`lane`) or as
/// its stage.
struct Host {
    attempt: Try,
    lane: bool,
}

fn in_place(stage: Stage) -> Vec<Row> {
    vec![readiness_row(stage.name(), true, "in place".into())]
}

impl InitialHost for Host {
    fn legacy(&mut self, _root: &Path) -> Result<Option<Report>, String> {
        unreachable!("a modern plan")
    }

    fn workstation(
        &mut self,
        root: &Path,
        _only: Option<Selection>,
    ) -> Result<Option<Report>, String> {
        Ok(Some(Report {
            action: Action::Apply,
            rows: in_place(Stage::Workstation),
            manual_steps: Vec::new(),
            backup: root.join("original-preferences.json"),
            restore: None,
            page: None,
        }))
    }

    fn spotlight(&mut self) -> Result<Vec<Row>, String> {
        Ok(in_place(Stage::Spotlight))
    }

    fn permissions(&mut self, _plan: &Plan, _revision: u64) -> Result<Vec<Row>, String> {
        Ok(in_place(Stage::Permissions))
    }

    fn secrets(
        &mut self,
        _plan: &Plan,
        _replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String> {
        self.attempt.run(password, "stage")
    }

    fn secrets_pace(&mut self) -> Option<Pace> {
        Some(pace())
    }

    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
        Ok(None)
    }

    fn current_vault(&mut self, _plan: &Plan) -> Result<Vec<Row>, String> {
        unreachable!("this Mac is in no vault")
    }

    fn repositories(&mut self, _vault: Option<&Plan>) -> Result<Vec<Row>, String> {
        unreachable!("not in the plan")
    }

    fn accounts(&mut self, _root: &Path, _on_screen: bool) -> Result<Option<Report>, String> {
        unreachable!("not in the plan")
    }

    fn withheld(&mut self, _root: &Path) -> Result<Option<Report>, String> {
        Ok(None)
    }

    fn secrets_lane(&mut self) -> Option<Box<dyn SecretsLane>> {
        self.lane
            .then(|| Box::new(Lane(self.attempt.clone())) as Box<dyn SecretsLane>)
    }
}

/// The base plan and the Dock: a Workstation stage for the Secrets lane to run beside.
fn plan() -> Plan {
    let mut selection = BASE_PLAN;
    selection.set(Setting::Dock, true);
    Plan::from(selection)
}

/// A state root with `plan` admitted, and its run's revision. A lane's run has heard macOS grant
/// Local Network already, so its lane tries at once.
fn admitted(lane: bool) -> (aterm_tempfile::TempDir, u64) {
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &plan()).unwrap().unwrap();
    if lane {
        let access = access_revision_at(root.path()).unwrap();
        access::record_step_isolated_at(root.path(), access, AccessOutcome::Granted);
    }
    (root, revision)
}

/// One pass of run `revision`, its worker holding `password`.
fn pass(host: &mut Host, root: &Path, revision: u64, password: &mut Option<SecretBytes>) {
    execute_initial_for_revision(host, root, revision, password, SetupHooks::NONE)
        .unwrap()
        .expect("the run is unfinished");
}

/// The vault row: its standing, value, wanted value and note.
type Reading = (Standing, String, String, Option<String>);

/// The vault row of [`plan`] on a Mac in no vault, as System State reads it from `root` on
/// `network`, with no LAN look heard.
fn row_on(root: &Path, network: &Network) -> Reading {
    row_heard(heard_for(&plan(), Some(root), network), root)
}

/// The vault row of [`plan`] on a Mac in no vault, with what aterm heard at the ALab vault's
/// address (`here`) and the run's records in `root`, with no LAN look heard.
fn row_heard(here: scope::VaultHere, root: &Path) -> Reading {
    let seen = vault_heard(
        &VaultRead::Unconfigured,
        plan().selection,
        here,
        None,
        Some(root),
    );
    (seen.standing, seen.now.unwrap(), seen.wanted, seen.note)
}

/// [`row_on`] [`HOME`], where the tries ran.
fn row(root: &Path) -> Reading {
    row_on(root, &HOME)
}

fn settled(note: &str) -> Reading {
    (
        Standing::InPlace,
        "Not joined".into(),
        "Not joined".into(),
        Some(note.into()),
    )
}

fn waiting(note: &str) -> Reading {
    (
        Standing::Waiting,
        "Not joined".into(),
        Setting::Secrets.wanted().into(),
        Some(note.into()),
    )
}

/// A row that heard nothing at the vault's address: the join is a change to make.
fn unheard() -> Reading {
    (
        Standing::Differs,
        "Not joined".into(),
        Setting::Secrets.wanted().into(),
        None,
    )
}

/// The state root of the pass [`watched_pass`] runs, and what the vault row read there each time
/// the Secrets stage ended while that pass still ran.
static WATCHED: Mutex<(Option<PathBuf>, Vec<Reading>)> = Mutex::new((None, Vec::new()));

/// Read the vault row as the Secrets stage ends, as System State reads that stage's rows again.
fn read_as_secrets_ends(event: StageEvent<'_>) {
    if event.running || !event.settings.contains(&Setting::Secrets) {
        return;
    }
    let mut watched = WATCHED.lock().unwrap();
    if let Some(root) = watched.0.clone() {
        watched.1.push(row(&root));
    }
}

/// [`pass`], and what the vault row read as its Secrets stage ended.
fn watched_pass(
    host: &mut Host,
    root: &Path,
    revision: u64,
    password: &mut Option<SecretBytes>,
) -> Vec<Reading> {
    *WATCHED.lock().unwrap() = (Some(root.to_path_buf()), Vec::new());
    let hooks = SetupHooks {
        stage: read_as_secrets_ends,
        ..SetupHooks::NONE
    };
    execute_initial_for_revision(host, root, revision, password, hooks)
        .unwrap()
        .expect("the run is unfinished");
    std::mem::take(&mut *WATCHED.lock().unwrap()).1
}

/// A join password the person typed waits for the vault while the run holds it for a try: as its
/// try ends, in the Secrets lane or as the stage, once the pass ends, through a pass inside the
/// retry pause that tries nothing, and in a newer run the person typed it for again. Once the
/// person stops the run, or a new run with no password replaces it, nothing will try that
/// password, and the row reads as for aterm's own join, as it does for a run with no password, at
/// its try's end too, or one whose try spent it.
#[test]
fn a_typed_join_waits_only_while_its_run_holds_the_password_for_a_try() {
    let secret = || Some(SecretBytes::from(b"typed join password".as_slice()));
    let unanswered = || Try::new(UNREACHED, scope::NOT_REACHED);
    for (lane, path) in [(false, "stage"), (true, "lane")] {
        let typed = |lane| {
            let (root, revision) = admitted(lane);
            let mut host = Host {
                attempt: unanswered(),
                lane,
            };
            let mut password = secret();
            let ended = watched_pass(&mut host, root.path(), revision, &mut password);
            assert_eq!(host.attempt.ran(), [path]);
            assert_eq!(ended, [waiting(scope::NOT_REACHED)], "{path}");
            assert!(password.is_some(), "{path}: the try handed it back");
            assert_eq!(row(root.path()), waiting(scope::NOT_REACHED), "{path}");
            (root, revision, host, password)
        };

        let (root, revision, mut host, mut password) = typed(lane);
        pass(&mut host, root.path(), revision, &mut password);
        assert_eq!(host.attempt.ran(), [path], "{path}: inside the pause");
        assert_eq!(row(root.path()), waiting(scope::NOT_REACHED), "{path}");
        request_stop_at(root.path(), revision).unwrap();
        assert_eq!(row(root.path()), settled(scope::NOT_REACHED), "{path}");

        // Set up now, say, admits a new run of the plan, whose worker holds no password.
        let (root, _, mut host, _) = typed(lane);
        let replaced = authorize_at(root.path(), &plan()).unwrap().unwrap();
        pass(&mut host, root.path(), replaced, &mut None);
        assert_eq!(
            host.attempt.ran(),
            [path, "stage"],
            "{path}: the new run tries"
        );
        assert_eq!(row(root.path()), settled(scope::NOT_REACHED), "{path}");

        // Join again, the password retyped, admits a new run whose worker holds it.
        let (root, _, mut host, _) = typed(lane);
        let retyped = authorize_at(root.path(), &plan()).unwrap().unwrap();
        let mut password = secret();
        pass(&mut host, root.path(), retyped, &mut password);
        assert_eq!(host.attempt.ran(), [path, "stage"], "{path}: retyped");
        assert_eq!(row(root.path()), waiting(scope::NOT_REACHED), "{path}");

        let (root, revision) = admitted(lane);
        let mut host = Host {
            attempt: unanswered(),
            lane,
        };
        let ended = watched_pass(&mut host, root.path(), revision, &mut None);
        assert_eq!(host.attempt.ran(), [path]);
        assert_eq!(ended, [settled(scope::NOT_REACHED)], "{path}: no password");
        assert_eq!(row(root.path()), settled(scope::NOT_REACHED), "{path}");

        let (root, revision) = admitted(lane);
        let mut host = Host {
            attempt: Try {
                spends: true,
                ..unanswered()
            },
            lane,
        };
        let mut password = secret();
        pass(&mut host, root.path(), revision, &mut password);
        assert_eq!(host.attempt.ran(), [path]);
        assert!(password.is_none(), "{path}");
        assert_eq!(row(root.path()), settled(scope::NOT_REACHED), "{path}");
    }
}

/// A process that asked the ALab vault nothing, as one relaunched inside the Secrets retry pause
/// is, whose pass tries nothing, reads what the run's last try heard: nobody answered, or a server
/// there holds no vault, settled for aterm's own join, with that note, and still once the pause
/// has ended, as a relaunch the next day reads it before a pass asks again. With no try kept, or
/// only a try of another run, it has heard nothing, and the join is a change to make: a run the
/// person admitted since keeps the older run's record until its own first try.
#[test]
fn a_process_that_asked_nothing_reads_what_the_runs_last_try_heard() {
    for (status, note) in [(UNREACHED, scope::NOT_REACHED), (WAITING, scope::NO_VAULT)] {
        let (root, revision) = admitted(false);
        let mut host = Host {
            attempt: Try::new(status, note),
            lane: false,
        };
        pass(&mut host, root.path(), revision, &mut None);
        pass(&mut host, root.path(), revision, &mut None);
        assert_eq!(host.attempt.ran(), ["stage"], "{note}: inside the pause");
        assert_eq!(row(root.path()), settled(note));
        assert_eq!(
            row_on(root.path(), &HOME_LATER),
            settled(note),
            "{note}: past the pause"
        );
    }
    let (root, revision) = admitted(false);
    assert_eq!(row(root.path()), unheard());
    let unanswered = || [Try::new(UNREACHED, scope::NOT_REACHED).row];
    secrets_retry::record(root.path(), revision + 1, &pace(), false, &mut unanswered()).unwrap();
    assert_eq!(row(root.path()), unheard(), "a newer run's try");
    secrets_retry::record(root.path(), revision, &pace(), false, &mut unanswered()).unwrap();
    assert_eq!(row(root.path()), settled(scope::NOT_REACHED));
    authorize_at(root.path(), &plan()).unwrap().unwrap();
    assert_eq!(row(root.path()), unheard(), "an older run's try");
}

/// What a try heard is of the network it ran on, so read on another network, or one this Mac
/// cannot read, it says nothing, and the join is a change to make: while the run goes on, until
/// it asks again, and once the person stopped it, which keeps the record and asks nothing again,
/// as a Mac stopped at home and reopened at the office reads it. On its own network it speaks
/// however long ago it ran, paused or not. A try that could not read its own network says nothing
/// of any.
#[test]
fn a_try_on_another_network_says_nothing_of_this_one() {
    let (root, revision) = admitted(false);
    let mut host = Host {
        attempt: Try::new(UNREACHED, scope::NOT_REACHED),
        lane: false,
    };
    pass(&mut host, root.path(), revision, &mut None);
    assert_eq!(row(root.path()), settled(scope::NOT_REACHED));
    assert_eq!(
        row_on(root.path(), &HOME_LATER),
        settled(scope::NOT_REACHED),
        "past the pause"
    );
    assert_eq!(row_on(root.path(), &OFFICE), unheard(), "the run goes on");
    request_stop_at(root.path(), revision).unwrap();
    assert_eq!(row(root.path()), settled(scope::NOT_REACHED), "stopped");
    assert_eq!(
        row_on(root.path(), &HOME_LATER),
        settled(scope::NOT_REACHED),
        "stopped, a day later"
    );
    assert_eq!(row_on(root.path(), &OFFICE), unheard(), "stopped");
    assert_eq!(row_on(root.path(), &UNREAD), unheard(), "unread");

    let (root, revision) = admitted(false);
    let unread = secrets_retry::pace(&UNREAD);
    let mut rows = [Try::new(UNREACHED, scope::NOT_REACHED).row];
    secrets_retry::record(root.path(), revision, &unread, false, &mut rows).unwrap();
    assert_eq!(
        row(root.path()),
        unheard(),
        "a try on a network it could not read"
    );
}

/// What this process's own probe heard is of the network it ran on, as a try's record is: a Mac
/// stopped where nobody answered and taken, with aterm still open, to a network where the vault
/// may answer has heard nothing there, and its join is a change to make, as a relaunch there reads
/// it. On its own network the probe speaks over the record; a probe that could not read its
/// network says nothing, and the record speaks.
#[test]
fn this_process_probe_speaks_only_for_the_network_it_ran_on() {
    use scope::VaultHere::{Absent, Unreached};
    let (root, revision) = admitted(false);
    let mut host = Host {
        attempt: Try::new(UNREACHED, scope::NOT_REACHED),
        lane: false,
    };
    pass(&mut host, root.path(), revision, &mut None);
    request_stop_at(root.path(), revision).unwrap();
    let probe = |here, on: &Network| scope::Probe {
        here,
        on: secrets_retry::network(on),
    };
    let heard = |probe, network: &Network| {
        row_heard(
            scope::heard_from(Some(probe), Some(root.path()), network),
            root.path(),
        )
    };
    assert_eq!(
        heard(probe(Unreached, &HOME), &HOME),
        settled(scope::NOT_REACHED)
    );
    assert_eq!(
        heard(probe(Unreached, &HOME), &OFFICE),
        unheard(),
        "stopped at home, open at the office"
    );
    assert_eq!(
        heard(probe(Absent, &HOME), &HOME),
        settled(scope::NO_VAULT),
        "over the record"
    );
    assert_eq!(
        heard(probe(Absent, &OFFICE), &OFFICE),
        settled(scope::NO_VAULT),
        "its own network"
    );
    assert_eq!(
        heard(probe(Absent, &UNREAD), &HOME),
        settled(scope::NOT_REACHED),
        "a probe on a network it could not read"
    );
    assert_eq!(
        heard(probe(Absent, &HOME), &UNREAD),
        unheard(),
        "a network this Mac cannot read now"
    );
}

/// What a try or this process's probe heard holds on its network while an address comes and goes
/// there (an IPv6 address, AirDrop's link, a sleep and a wake): that network is the key a kept look
/// is held to, its gateway and IPv4 subnets, not every address on the Mac. Another gateway is
/// another network, even one that leaves the Mac the same addresses. Where no key can be formed
/// (no gateway known), the routes tell the network, as they do for a try an older build recorded,
/// which kept only those.
#[test]
fn an_address_that_comes_and_goes_leaves_the_network_as_it_was() {
    use scope::VaultHere::Absent;
    const CHURNED: Network = Network {
        routes: Some("home routes and a new IPv6 address"),
        ..HOME
    };
    const SAME_ADDRESSES: Network = Network {
        key: Some("another gateway"),
        ..HOME
    };
    const NO_GATEWAY: Network = Network { key: None, ..HOME };
    const NO_GATEWAY_CHURNED: Network = Network {
        key: None,
        ..CHURNED
    };
    let (root, revision) = admitted(false);
    let mut host = Host {
        attempt: Try::new(UNREACHED, scope::NOT_REACHED),
        lane: false,
    };
    pass(&mut host, root.path(), revision, &mut None);
    request_stop_at(root.path(), revision).unwrap();
    assert_eq!(
        row_on(root.path(), &CHURNED),
        settled(scope::NOT_REACHED),
        "the try's record"
    );
    assert_eq!(row_on(root.path(), &SAME_ADDRESSES), unheard());
    let probe = |on: &Network| {
        Some(scope::Probe {
            here: Absent,
            on: secrets_retry::network(on),
        })
    };
    let heard = |probe, records: Option<&Path>, network: &Network| {
        row_heard(scope::heard_from(probe, records, network), root.path())
    };
    assert_eq!(
        heard(probe(&HOME), Some(root.path()), &CHURNED),
        settled(scope::NO_VAULT),
        "this process's probe"
    );
    assert_eq!(heard(probe(&HOME), None, &SAME_ADDRESSES), unheard());
    assert_eq!(
        heard(probe(&NO_GATEWAY), None, &NO_GATEWAY),
        settled(scope::NO_VAULT),
        "no gateway"
    );
    assert_eq!(
        heard(probe(&NO_GATEWAY), None, &NO_GATEWAY_CHURNED),
        unheard(),
        "no gateway, other routes"
    );

    let record = root.path().join(secrets_retry::RECORD);
    let mut kept: Value = aterm_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    kept.as_object_mut()
        .unwrap()
        .remove("network")
        .expect("this build keeps the try's network");
    write_json(&record, &kept).unwrap();
    assert_eq!(
        row(root.path()),
        settled(scope::NOT_REACHED),
        "an older build's try"
    );
    assert_eq!(
        row_on(root.path(), &CHURNED),
        unheard(),
        "an older build's try, other routes"
    );
}

/// Check again's look is what this process heard last ([`scope::look_again`]): a run stopped where
/// nobody answered, whose vault answers there later, reads what that look heard on the same
/// network, so its join is a change to make (**Set up now**), and settles again with the note of a
/// look that hears nothing.
#[test]
fn check_again_s_look_is_what_aterm_heard_last() {
    use scope::VaultHere::{Found, Unreached};
    let (root, revision) = admitted(false);
    let mut host = Host {
        attempt: Try::new(UNREACHED, scope::NOT_REACHED),
        lane: false,
    };
    pass(&mut host, root.path(), revision, &mut None);
    request_stop_at(root.path(), revision).unwrap();
    let kept = Mutex::new(Some(scope::Probe {
        here: Unreached,
        on: secrets_retry::network(&HOME),
    }));
    let read = || {
        let probe = kept.lock().unwrap().clone();
        row_heard(
            scope::heard_from(probe, Some(root.path()), &HOME),
            root.path(),
        )
    };
    assert_eq!(read(), settled(scope::NOT_REACHED));
    assert_eq!(scope::look_again_with(&kept, || Found, &HOME), Found);
    assert_eq!(read(), unheard(), "the vault answered");
    scope::look_again_with(&kept, || Unreached, &HOME);
    assert_eq!(read(), settled(scope::NOT_REACHED), "nobody answered again");
}

/// A join password another aterm process sharing the state root holds for the run in force (a
/// second copy of the app, say) waits for the vault there too, while that process runs: the mark
/// its pass leaves names it, and a pass of this process holding none, between that process's
/// passes, leaves it in place. A mark naming a process that no longer runs, as a quit or a crash
/// leaves it, waits for nothing, nor does one whose pid macOS has given a later process, and such
/// a pass takes it away.
#[test]
fn a_typed_join_another_aterm_holds_waits_while_that_process_runs() {
    use aterm_uds::process::{Birth, Identity, read_identity, read_lineage};
    let (root, revision) = admitted(false);
    let mut host = Host {
        attempt: Try::new(UNREACHED, scope::NOT_REACHED),
        lane: false,
    };
    pass(&mut host, root.path(), revision, &mut None);
    assert_eq!(row(root.path()), settled(scope::NOT_REACHED));
    let me = i32::try_from(std::process::id()).unwrap();
    let parent = read_lineage(me).expect("this process").parent;
    let other: Identity = read_identity(i32::try_from(parent).unwrap())
        .expect("the process that runs these tests, which outlives them");
    scope::keep_join_held(root.path(), revision, &other).unwrap();
    assert_eq!(
        row(root.path()),
        waiting(scope::NOT_REACHED),
        "another aterm holds it"
    );
    pass(&mut host, root.path(), revision, &mut None);
    assert_eq!(
        row(root.path()),
        waiting(scope::NOT_REACHED),
        "another aterm holds it, past a pass of this one holding none"
    );
    let later = Identity {
        birth: Birth(other.birth.0 + 1),
        ..other
    };
    scope::keep_join_held(root.path(), revision, &later).unwrap();
    assert_eq!(
        row(root.path()),
        settled(scope::NOT_REACHED),
        "its pid, another process"
    );
    let ended = Identity {
        pid: i32::MAX,
        ..other
    };
    scope::keep_join_held(root.path(), revision, &ended).unwrap();
    assert_eq!(
        row(root.path()),
        settled(scope::NOT_REACHED),
        "a process that no longer runs"
    );
    let mark = root.path().join(scope::JOIN_HELD);
    assert!(mark.exists());
    pass(&mut host, root.path(), revision, &mut None);
    assert!(!mark.exists(), "a pass takes an ended process's mark away");
}

/// A Mac outside ALab whose plan sets up GitHub repositories too: the base plan and Repositories.
fn with_repositories() -> Plan {
    let mut selection = BASE_PLAN;
    selection.set(Setting::Repositories, true);
    Plan::from(selection)
}

/// One console account, in no vault, at `home` on an inert machine whose GitHub answers with
/// nothing to clone, with the GitHub token this Mac signs in with there or not (`token`).
fn github(home: &Path, token: bool) -> (aterm_machines::port::Fake, aterm_machines::items::Ctx) {
    use aterm_machines::items::Ctx;
    use aterm_machines::model::{HostFacts, User};
    use aterm_machines::port::{Fake, GithubResponse, Ran, Stat};
    const TOKEN: &str = "inert_repository_token";
    let user = User {
        name: "tester".into(),
        uid: 501,
        gid: 20,
        home: home.to_string_lossy().into_owned(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let mut sys = Fake::root();
    sys.euid = user.uid;
    let stat = |dir, len| Stat {
        uid: user.uid,
        gid: user.gid,
        mode: if dir { 0o700 } else { 0o600 },
        dir,
        symlink: false,
        len,
    };
    sys.stat_of(&user.home, stat(true, 0));
    sys.file(aterm_machines::items::github::CLT_GIT, b"inert git");
    if token {
        let token = format!("{}/.secrets/gh_access_token", user.home);
        sys.file(&token, TOKEN.as_bytes())
            .stat_of(&token, stat(false, TOKEN.len() as u64));
    }
    for (endpoint, body) in [
        ("/user", aterm_json::json!({"login": "tester"})),
        (
            "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc&page=1",
            aterm_json::json!([]),
        ),
    ] {
        sys.on_github_api(
            &user,
            endpoint,
            Ok(GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: aterm_json::to_vec(&body).unwrap(),
            }),
        );
    }
    let helper = format!(
        "file:{}/.gitconfig\0credential.helper\ninert-helper\0",
        user.home
    );
    sys.simulate(move |_, command| {
        let command = command.display();
        if command.ends_with(" credential fill") {
            Some(Ran::ok(&format!(
                "protocol=https\nhost=github.com\nusername=tester\npassword={TOKEN}\n"
            )))
        } else if command.contains(" config --null --show-origin --get-regexp ") {
            Some(Ran::ok(&helper))
        } else {
            panic!("a reading changes nothing: {command}");
        }
    });
    let ctx = Ctx {
        facts: HostFacts::default(),
        users: vec![user.clone()],
        console: Some(user),
        host_name: "fixture".into(),
        cache_dir: home.join("cache"),
        disrupt: false,
    };
    (sys, ctx)
}

/// On a Mac outside ALab whose plan sets up GitHub repositories, a Secrets try that left the ALab
/// vault off this network (nobody answered, or a server there holds none) has the Repositories
/// stage run on this Mac's own GitHub credentials, and the row reads them so: in place with a
/// token of the Mac's own, read from GitHub, and waiting for the vault's token, as the stage does,
/// only with none. Where that try says nothing, on another network, the row waits for the vault's
/// token as the stage does there, asking GitHub nothing.
#[test]
fn outside_alab_the_repositories_read_as_their_stage_runs_them() {
    let plan = with_repositories();
    let syncing = Some("Waits for the vault to finish syncing this Mac".to_owned());
    let asked_github = |sys: &aterm_machines::port::Fake| {
        sys.calls()
            .iter()
            .any(|call| call.ends_with(" agh GET /user"))
    };
    for (status, note) in [(UNREACHED, scope::NOT_REACHED), (WAITING, scope::NO_VAULT)] {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut rows = [Try::new(status, note).row];
        secrets_retry::record(root.path(), revision, &pace(), false, &mut rows).unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        for token in [true, false] {
            for (network, heard) in [(&HOME, true), (&OFFICE, false)] {
                let (sys, ctx) = github(home.path(), token);
                let seen = repositories_heard(&sys, &ctx, &plan, || {
                    heard_for(&plan, Some(root.path()), network)
                })
                .unwrap();
                let case = format!("{note}, token {token}, heard {heard}");
                if heard && token {
                    assert_eq!(
                        (
                            seen.standing,
                            seen.now.as_deref(),
                            seen.wanted.as_str(),
                            seen.note
                        ),
                        (Standing::InPlace, Some("All 0"), "All 0", None),
                        "{case}"
                    );
                    assert!(asked_github(&sys), "{case}");
                } else {
                    assert_eq!(
                        (seen.standing, seen.note.clone()),
                        (Standing::Waiting, syncing.clone()),
                        "{case}"
                    );
                    assert!(!asked_github(&sys), "{case}");
                }
            }
        }
    }
}
