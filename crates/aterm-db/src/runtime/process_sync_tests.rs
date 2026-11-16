// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real runtime processes enroll, repair external writes and transfer custody.
//! Only generated identities and isolated loopback state are used.
//!
//! What each wait for the children's repair waits for is their rounds, so its hang detector's
//! minute is theirs while they work on one: a round they end, or their CPU or I/O while a round of
//! theirs is in flight, starts it again ([`Minute::worked`]), and a loaded Mac's stretched wall
//! fails no round still at work. What they do at rest (timers, beacons, polls) brings no record,
//! so a write that waits for an audit is named a minute into their rest, with their rounds' log.
//! A wait on one child's own work (its start, its ownership, its exit) is that child's minute
//! ([`aterm_spec::wait::within_while_working`]).

use super::{
    Network, Runtime, Visibility,
    auto_sync_tests::{Attachment, Minute, Progress, Rounds, Seams, Watch, wait_network},
};
use crate::{
    accounts::proposals::Proposals,
    catalog::{Descriptor, Directory},
    database::Change,
    network_state,
    table::{Scope, Session, store::Store},
};
use aterm_net::lan::{Multicast, Scope as LinkScope};
use aterm_secrets::{
    client::Paths,
    commons::{bootstrap::Bootstrap, roster::Role},
    fsx,
};
use aterm_spec::wait::{MUST_HAPPEN, WORKING_GUARD};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::Ipv4Addr,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// The deadline of one read or one attempt the child runtimes may hold up, and the hang detector
/// of a kill. No wait for what the children do ends at it.
const HANG: Duration = MUST_HAPPEN;

/// The pause between two looks at what a wait waits for: it only keeps the poll from spinning a
/// core.
const POLL: Duration = Duration::from_millis(10);

#[test]
#[ignore = "child process fixture for the separate_processes tests"]
fn child_runtime() {
    let home = std::env::var_os("ATERM_TEST_COMMONS_HOME").expect("fixture home");
    let config = std::env::var_os("ATERM_TEST_COMMONS_CONFIG").expect("fixture config");
    let port = std::env::var("ATERM_TEST_COMMONS_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let owner = std::env::var("ATERM_TEST_COMMONS_OWNER")
        .ok()
        .map(|pid| pid.parse::<u32>().unwrap());
    let match_table = std::env::var("ATERM_TEST_COMMONS_MATCH_TABLE").ok();
    let matched_file = std::env::var_os("ATERM_TEST_COMMONS_MATCHED_FILE");
    let rounds = PathBuf::from(
        std::env::var_os("ATERM_TEST_COMMONS_ROUNDS_FILE").expect("fixture round log"),
    );
    let reported = AtomicBool::new(false);
    let opens = Arc::new(AtomicUsize::new(0));
    // The round log tells the parent's waits when a round is in flight as well as each round's
    // end; both come from the serving loop's thread, so the lines keep their order.
    let flights = rounds.clone();
    let runtime = Runtime::start_on_with_reports(
        Paths::for_home(Path::new(&home)),
        config.into(),
        |_| {},
        Network {
            isolated: Some(
                Attachment::new(port)
                    .telling_flight(move |in_flight| append_flight(&flights, in_flight)),
            ),
            opens: Some(opens.clone()),
            ..Network::default()
        },
        move |round| {
            append_round(&rounds, &round);
            if let (Some(table), Some(file), Some(witness)) =
                (&match_table, &matched_file, round.matching_inventory())
                && aterm_codec::hex::encode_lower(&witness.scope().digest()) == *table
                && !reported.swap(true, Ordering::AcqRel)
            {
                // Retain this event independently of later rounds, including ones
                // for other tables. The parent only observes this test-owned file.
                let proof = format!("{} {table}", std::process::id());
                assert!(fsx::write_new(Path::new(file), proof.as_bytes(), 0o600).unwrap());
            }
        },
    )
    .unwrap();
    wait_network(
        &runtime.network_observation(),
        "child runtime ready",
        |state| {
            state.visibility == Visibility::Active
                && state.observer.is_some()
                && state.via_local_process == owner
        },
    );
    let opened = opens.load(Ordering::Acquire);
    if owner.is_some() {
        assert_eq!(
            opened, 0,
            "a follower must relay without opening a transport"
        );
    } else {
        assert!(opened > 0, "an active owner must have opened its transport");
    }
    // Keep the readiness proof on its own line even after libtest's single-thread prefix.
    println!("\ncommons-runtime-ready");
    std::io::stdout().flush().unwrap();
    if std::io::stdin().read(&mut [0; 1]).unwrap() == 1 {
        aterm_uds::process::kill_self_now();
    }
    runtime.stop();
}

struct Process(aterm_uds::spawnfd::Child);

impl Process {
    fn start(paths: &Paths, config: &Path, port: u16, owner: Option<u32>) -> Self {
        Self::start_with_match(paths, config, port, owner, None)
    }

    fn start_with_match(
        paths: &Paths,
        config: &Path,
        port: u16,
        owner: Option<u32>,
        table: Option<Scope>,
    ) -> Self {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "runtime::process_sync_tests::child_runtime",
                "--ignored",
                "--nocapture",
            ])
            .env("ATERM_TEST_COMMONS_HOME", &paths.home)
            .env("ATERM_TEST_COMMONS_CONFIG", config)
            .env("ATERM_TEST_COMMONS_PORT", port.to_string())
            .env("ATERM_TEST_COMMONS_ROUNDS_FILE", round_log(config))
            .env_remove("ATERM_TEST_COMMONS_OWNER")
            .env_remove("ATERM_TEST_COMMONS_MATCH_TABLE")
            .env_remove("ATERM_TEST_COMMONS_MATCHED_FILE")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::inherit());
        if let Some(owner) = owner {
            command.env("ATERM_TEST_COMMONS_OWNER", owner.to_string());
        }
        if let Some(table) = table {
            let file = config.with_extension("matched");
            assert!(!file.exists());
            command
                .env(
                    "ATERM_TEST_COMMONS_MATCH_TABLE",
                    aterm_codec::hex::encode_lower(&table.digest()),
                )
                .env("ATERM_TEST_COMMONS_MATCHED_FILE", file);
        }
        let mut process = Self(command.spawn().unwrap());
        process.ready();
        assert_ne!(process.0.id(), std::process::id());
        println!(
            "commons-process ready pid={} relays={owner:?}",
            process.id()
        );
        process
    }

    fn id(&self) -> u32 {
        self.0.id()
    }

    /// Wait for the child's readiness line for as long as it works: starting its runtime and
    /// seeing it active are the child's own work.
    fn ready(&mut self) {
        use std::io::BufRead as _;
        use std::sync::mpsc::TryRecvError;
        let pid = self.id();
        let stdout = self.0.stdout.take().expect("the child's stdout is piped");
        let (tx, said) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut stdout = std::io::BufReader::new(stdout);
            let mut line = Vec::new();
            while stdout
                .read_until(b'\n', &mut line)
                .is_ok_and(|read| read != 0)
            {
                if line.trim_ascii_end() == b"commons-runtime-ready" {
                    let _ = tx.send(());
                    // Drain the rest, so the child never blocks on a full pipe.
                    let _ = std::io::copy(&mut stdout, &mut std::io::sink());
                    return;
                }
                line.clear();
            }
        });
        let ready =
            aterm_spec::wait::within_while_working(pid, MUST_HAPPEN, WORKING_GUARD, POLL, || {
                match said.try_recv() {
                    Ok(()) => Some(true),
                    Err(TryRecvError::Empty) => None,
                    // Its output ended first.
                    Err(TryRecvError::Disconnected) => Some(false),
                }
            });
        match ready {
            Ok(true) => {}
            Ok(false) => panic!("runtime child {pid} ended its output before it was ready"),
            Err(why) => panic!("runtime child {pid} was not ready ({why:?})"),
        }
    }

    /// Wait, for as long as the children work on their rounds ([`Minute::worked`]), for this
    /// child's record that a round between them matched `table`'s inventory. `children` are the
    /// configurations the children started with, whose round logs say what they did.
    #[track_caller]
    fn matched(&self, config: &Path, table: Scope, children: &[PathBuf]) {
        let expected = format!(
            "{} {}",
            self.id(),
            aterm_codec::hex::encode_lower(&table.digest())
        );
        let mut used = aterm_uds::deadline::descendants_work();
        let mut minute = Minute::new(Instant::now());
        loop {
            if let Some(proof) = fsx::read_private(&config.with_extension("matched"), 256).unwrap()
            {
                assert_eq!(proof.as_slice(), expected.as_bytes());
                return;
            }
            let rounds = logged(children);
            let now = Instant::now();
            minute.worked(rounds, used(), now);
            if let Err(unmet) = minute.remaining(now) {
                panic!(
                    "no round between the children matched the inventory {expected}: the wait \
                     ended {unmet:?} after {:?}; the children's last rounds:\n{}",
                    minute.ran(now),
                    log_tails(children),
                );
            }
            thread::sleep(POLL);
        }
    }

    /// The child's exit status, waited for as long as it works: stopping its runtime is its own.
    fn wait(&mut self) -> ExitStatus {
        let pid = self.id();
        let child = &mut self.0;
        aterm_spec::wait::within_while_working(pid, MUST_HAPPEN, WORKING_GUARD, POLL, || {
            child.try_wait().unwrap()
        })
        .unwrap_or_else(|why| panic!("runtime child {pid} did not exit ({why:?})"))
    }

    fn crash(mut self) {
        let pid = self.id();
        self.0.stdin.as_mut().unwrap().write_all(b"x").unwrap();
        // kill_self_now uses SIGKILL (9) on the supported Unix hosts.
        let signal = self.wait().signal();
        assert_eq!(signal, Some(9));
        println!("commons-process exited pid={pid} signal={signal:?}");
    }

    fn stop(mut self) {
        let pid = self.id();
        drop(self.0.stdin.take());
        assert!(self.wait().success());
        println!("commons-process exited pid={pid} success=true");
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        aterm_uds::deadline::kill_within(&mut self.0, HANG);
    }
}

/// The live publication at `config` once process `pid` owns its repair, waited for as long as
/// that process works: taking the ownership is its own.
fn wait_owner(config: &Path, pid: u32) -> aterm_types::network_state::NetworkObservation {
    let mut last = None;
    let owned =
        aterm_spec::wait::within_while_working(pid, MUST_HAPPEN, WORKING_GUARD, POLL, || {
            let state = network_state::read_live_publication(config)
                .map(|publication| publication.observation().clone());
            let owned = state
                .as_ref()
                .ok()
                .filter(|state| {
                    state.via_local_process == Some(pid)
                        && state.visibility == Visibility::Active
                        && state.observer.is_some()
                })
                .cloned();
            last = Some(state);
            owned
        });
    owned.unwrap_or_else(|why| panic!("process {pid} never owned repair ({why:?}): {last:?}"))
}

/// What `attempt` returns once it answers more than a busy refusal or a deadline it ran out at,
/// each attempt given a deadline of its own. The child runtimes hold this Mac's vault, custody
/// and journal for their rounds, so such a refusal learned nothing while their `rounds` work: the
/// attempts go on while the children end rounds or work on one in flight ([`Minute::worked`]).
///
/// # Panics
/// Where `attempt` is refused otherwise; and where every attempt was refused through a minute in
/// which the children ended no round and worked on none (a lock held across no round), or
/// through the guard. The panic names the last refusal.
#[track_caller]
fn while_children_work<T>(
    what: &str,
    rounds: &mut dyn FnMut() -> Rounds,
    mut attempt: impl FnMut(Instant) -> crate::Result<T>,
) -> T {
    let mut used = aterm_uds::deadline::descendants_work();
    let mut seams = Seams {
        now: &mut Instant::now,
        used: &mut used,
        pause: POLL,
    };
    match await_attempts(rounds, &mut attempt, &mut seams) {
        Ok(value) => value,
        Err(why) => panic!("{what}: {why}"),
    }
}

/// [`while_children_work`]'s attempts, over their `seams`: the value, or why they ended without
/// one.
fn await_attempts<T>(
    rounds: &mut dyn FnMut() -> Rounds,
    attempt: &mut dyn FnMut(Instant) -> crate::Result<T>,
    seams: &mut Seams<'_>,
) -> Result<T, String> {
    let mut minute = Minute::new((seams.now)());
    loop {
        let refusal = match attempt((seams.now)() + HANG) {
            Ok(value) => return Ok(value),
            Err(error) if error.is_busy() || error.is_out_of_time() => error,
            Err(error) => return Err(format!("{error:?}")),
        };
        let rounds = rounds();
        let used = (seams.used)();
        let now = (seams.now)();
        minute.worked(rounds, used, now);
        if let Err(unmet) = minute.remaining(now) {
            return Err(format!(
                "every attempt was refused, the last {refusal:?}; the wait ended {unmet:?} after \
                 {:?}",
                minute.ran(now),
            ));
        }
        thread::sleep(seams.pause);
    }
}

#[test]
fn separate_processes_transfer_live_repair() {
    transfer_live_repair(false);
}

#[test]
fn separate_configurations_share_live_repair_and_transfer_its_owner() {
    transfer_live_repair(true);
}

fn transfer_live_repair(separate_configs: bool) {
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-proc-")
        .tempdir_in("/tmp")
        .unwrap();
    let local = observers.path().join("local");
    let peer = observers.path().join("peer");
    for directory in [&local, &peer] {
        fsx::ensure_private_dir(directory).unwrap();
    }
    let config = local.join("aterm.toml");
    let follower_config = if separate_configs {
        let directory = observers.path().join("follower");
        fsx::ensure_private_dir(&directory).unwrap();
        directory.join("aterm.toml")
    } else {
        config.clone()
    };
    let restart_config = if separate_configs {
        let directory = observers.path().join("restart");
        fsx::ensure_private_dir(&directory).unwrap();
        directory.join("aterm.toml")
    } else {
        config.clone()
    };
    let proposals = Proposals::at(lab.paths[0].clone());
    let table = proposals.initialize().unwrap();
    // A write's request makes a repeat of it return its first receipt, so an attempt a child's
    // round kept busy is made again whole. The children's rounds are with the receiver's runtime
    // here, so its rounds say what they did; before it starts, no child runs.
    let write = |request, key: &str, rounds: &mut dyn FnMut() -> Rounds| {
        while_children_work(&format!("the proposal {key}"), rounds, |until| {
            Proposals::at_until(lab.paths[0].clone(), until).write(
                [request; 16],
                vec![Change {
                    collection: "accounts".into(),
                    key: key.into(),
                    value: Some(aterm_json::json!({"provider":"claude","method":"oauth"})),
                }],
            )
        })
    };
    let first = write(61, "before-processes", &mut Rounds::default);
    assert!(!lab.paths[1].dir.join("commons/tables").exists());
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    let owner = Process::start(&lab.paths[0], &config, port, None);
    let identity = wait_owner(&config, owner.id()).observer.unwrap();
    let progress = Attachment::new(port);
    let receiver = Runtime::start_on(
        lab.paths[1].clone(),
        peer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(progress.clone()),
            ..Network::default()
        },
    )
    .unwrap();
    progress.wait_for_while_children_work(table.digest(), 1, "peer discovered the initial table");

    let follower = Process::start(&lab.paths[0], &follower_config, port, Some(owner.id()));
    if separate_configs {
        assert!(
            !network_state::read_live_observation(&follower_config)
                .is_ok_and(|state| state.visibility == Visibility::Active),
            "a configuration relay cannot recursively forward the active transport owner"
        );
    }
    assert_eq!(
        super::read_live_publication(&lab.paths[0], &follower_config)
            .unwrap()
            .observation()
            .via_local_process,
        Some(owner.id()),
        "the CLI must read the actual owner directly, without a recursive relay"
    );
    owner.crash();
    assert_eq!(
        wait_owner(&follower_config, follower.id()).observer,
        Some(identity.clone())
    );
    let after_crash = write(62, "after-owner-crash", &mut || progress.rounds());
    progress.wait_for_while_children_work(
        table.digest(),
        2,
        "peer received the successor's fresh operation",
    );

    let restarted = Process::start(&lab.paths[0], &restart_config, port, Some(follower.id()));
    follower.stop();
    assert_eq!(
        wait_owner(&restart_config, restarted.id()).observer,
        Some(identity)
    );
    let after_exit = write(63, "after-owner-exit", &mut || progress.rounds());
    progress.wait_for_while_children_work(
        table.digest(),
        3,
        "peer received the next owner's fresh operation",
    );
    restarted.stop();
    receiver.stop();

    let expected = proposals.snapshot().unwrap();
    let actual = Proposals::at(lab.paths[1].clone()).snapshot().unwrap();
    assert_eq!(expected.operations.len(), 3);
    assert_eq!(expected.cells.len(), 3);
    assert_eq!(actual.cells, expected.cells);
    let ids = |snapshot: &crate::accounts::proposals::Snapshot| {
        snapshot
            .operations
            .iter()
            .map(|op| op.header.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&actual), ids(&expected));
    for receipt in [first, after_crash, after_exit] {
        assert!(
            actual
                .operations
                .iter()
                .any(|op| op.header.id == receipt.id)
        );
    }
    println!("commons-process verified operations=3 values=3");
}

type Records = BTreeMap<[u8; 32], Vec<u8>>;

fn records(paths: &Paths, table: Scope, until: Instant) -> crate::Result<Records> {
    let bootstrap = Bootstrap::required_until(paths, until).map_err(crate::table::failure)?;
    let session = Session::observe_until(&bootstrap, until)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let Some(store) = directory.observe_table_if_exists(&session, table)? else {
        return Ok(Records::new());
    };
    Ok(store
        .catalog(&session)?
        .into_iter()
        .map(|record| (record.header().id, record.payload().to_vec()))
        .collect())
}

/// The file the child runtime started with `config` appends its rounds to ([`append_round`]).
fn round_log(config: &Path) -> PathBuf {
    config.with_extension("rounds")
}

/// Append to `file` one line for `round`, which a child runtime completed: when (Unix
/// milliseconds), whether it exchanged a table (`application`) or not (`control`), whether this
/// side dialled or served it, the table, what it sent, received and added, whether it was the
/// scan's regular turn, and why its control or table exchange failed. [`wait_records`] counts
/// the application rounds and reports the last ones.
fn append_round(file: &Path, round: &super::RepairRound) {
    let failed = |failure: Option<&crate::control::Failure>| {
        failure.map_or_else(|| "-".to_owned(), |failure| format!("{:?}", failure.cause))
    };
    let side = if round.target.is_some() {
        "dialled"
    } else {
        "served"
    };
    let line = match &round.application {
        Some(app) => format!(
            "{} application {side} table={} sent={} received={} added={} regular={} error={}\n",
            round.completed_at_ms,
            app.table.map_or_else(
                || "-".to_owned(),
                |table| aterm_codec::hex::encode_lower(&table[..4])
            ),
            app.sent + app.conflicts_sent,
            app.received + app.conflicts_received,
            app.added,
            app.regular,
            failed(round.application_error.as_ref()),
        ),
        None => format!(
            "{} control {side} control={} error={}\n",
            round.completed_at_ms,
            failed(round.control.as_ref().err()),
            failed(round.application_error.as_ref()),
        ),
    };
    append_line(file, &line);
}

/// Append to `file` one line telling whether a round of the child runtime's is now `in_flight`,
/// as its transport's turn found it (`Attachment::telling_flight`): when (Unix milliseconds),
/// `flight`, and `true` or `false`. [`logged`] reads the last.
fn append_flight(file: &Path, in_flight: bool) {
    let line = format!(
        "{} flight {in_flight}\n",
        aterm_time::unix_millis().unwrap_or_default()
    );
    append_line(file, &line);
}

fn append_line(file: &Path, line: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut log| log.write_all(line.as_bytes()))
        .unwrap();
}

/// The lines the child runtime started with `config` has logged ([`append_round`],
/// [`append_flight`]).
fn logged_lines(config: &Path) -> Vec<String> {
    std::fs::read_to_string(round_log(config))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// What the child runtimes started with the configurations `children` logged of their rounds:
/// how many they ended ([`append_round`]), and whether one of theirs is in flight, as each one's
/// last flight line says ([`append_flight`]).
fn logged(children: &[PathBuf]) -> Rounds {
    let mut rounds = Rounds::default();
    for config in children {
        let mut in_flight = false;
        for line in logged_lines(config) {
            match line.split(' ').nth(1) {
                Some("flight") => in_flight = line.ends_with(" true"),
                Some("application" | "control") => rounds.ended += 1,
                _ => {}
            }
        }
        rounds.in_flight |= in_flight;
    }
    rounds
}

/// Each child's last 24 logged lines, after the time now (Unix milliseconds), for a failure.
fn log_tails(children: &[PathBuf]) -> String {
    let tails = children
        .iter()
        .map(|config| {
            let lines = logged_lines(config);
            format!(
                "{}:\n{}",
                round_log(config).display(),
                lines[lines.len().saturating_sub(24)..].join("\n"),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "(now {} ms)\n{tails}",
        aterm_time::unix_millis().unwrap_or_default()
    )
}

/// Wait until `paths` holds exactly `expected` in `table`, the `phase` this is. The child
/// runtimes started with the configurations `children` do the repair and log their rounds
/// ([`append_round`], [`append_flight`]). The wait ends as [`Watch`] does: after a minute in which
/// the count of `expected` that a read finds does not rise, the children end no round and do no
/// work on a round in flight ([`Minute::worked`]), or at its guard. Each read has a deadline of
/// its own: one that ran out of time learned nothing, and one a live enrollment or witness
/// publication refused holds nothing, so neither fails the wait before its detector does. A wait
/// that ends unmet fails naming the phase, the table, why it ended, the children's rounds and
/// the time it ran, what the last read that finished held or why it was refused, how many reads
/// ran out of time, and the children's last rounds.
#[track_caller]
fn wait_records(
    paths: &Paths,
    table: Scope,
    expected: &Records,
    phase: &str,
    children: &[PathBuf],
) {
    let mut used = aterm_uds::deadline::descendants_work();
    let mut seams = Seams {
        now: &mut Instant::now,
        used: &mut used,
        pause: POLL,
    };
    if let Err(why) = await_records(
        &mut |until| records(paths, table, until),
        table.digest(),
        expected,
        children,
        &mut seams,
    ) {
        panic!("{phase}: {why}");
    }
}

/// [`wait_records`]' wait, over its seams: `read` reads the table whose digest is `digest` by
/// the deadline it is given, and `seams` are the clock, the children's work and the pause between
/// two reads. Where it ends unmet, what its failure says after the phase.
fn await_records(
    read: &mut dyn FnMut(Instant) -> crate::Result<Records>,
    digest: [u8; 32],
    expected: &Records,
    children: &[PathBuf],
    seams: &mut Seams<'_>,
) -> Result<(), String> {
    let progress = |finished: Option<&crate::Result<Records>>, rounds: Rounds| Progress {
        retained: BTreeMap::from([(
            digest,
            finished
                .and_then(|read| read.as_ref().ok())
                .map_or(0, |actual| {
                    expected
                        .iter()
                        .filter(|(id, payload)| actual.get(*id) == Some(*payload))
                        .count()
                }),
        )]),
        rounds: rounds.ended,
        in_flight: rounds.in_flight,
    };
    let mut watch = Watch::new(digest, &progress(None, logged(children)), (seams.now)());
    let mut finished = None;
    let mut out_of_time = 0_u64;
    loop {
        match read((seams.now)() + HANG) {
            Ok(actual) if actual == *expected => return Ok(()),
            Err(error) if error.is_out_of_time() => out_of_time += 1,
            other => finished = Some(other),
        }
        let rounds = logged(children);
        let used = (seams.used)();
        let now = (seams.now)();
        watch.worked(rounds, used, now);
        let progress = progress(finished.as_ref(), rounds);
        let Err(unmet) = watch.remaining(&progress, now) else {
            thread::sleep(seams.pause);
            continue;
        };
        let (rounds, elapsed) = watch.ran(&progress, now);
        let read = match &finished {
            None => "no read finished".to_owned(),
            Some(Ok(actual)) => format!(
                "the last read that finished held {} records, {} of them expected",
                actual.len(),
                progress.held(&digest),
            ),
            Some(Err(error)) => format!("the last read that finished was refused: {error:?}"),
        };
        return Err(format!(
            "table {} did not repair to its {} records; the wait ended {unmet:?} after {rounds} \
             rounds of the children in {elapsed:?}; {read}; {out_of_time} reads ran out of time \
             and learned nothing; the children's last rounds {}",
            aterm_codec::hex::encode_lower(&digest[..4]),
            expected.len(),
            log_tails(children),
        ));
    }
}

fn append(
    paths: &Paths,
    descriptor: &Descriptor,
    declare: bool,
    payload: &[u8],
    children: &[PathBuf],
) -> [u8; 32] {
    let (bootstrap, journal) = while_children_work(
        "this Mac's journal, to append with",
        &mut || logged(children),
        |until| Bootstrap::required_with_journal_until(paths, until).map_err(crate::table::failure),
    );
    let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    if declare {
        directory.publish(&session, descriptor).unwrap();
    }
    Store::open(
        &session,
        &directory.path(descriptor.scope).unwrap(),
        descriptor.scope,
    )
    .unwrap()
    .commit_catalog(&session, payload)
    .unwrap()
    .id
}

#[test]
fn separate_processes_enroll_then_repair_external_writes_and_later_tables() {
    let lab = crate::control::enrollment::tests::Lab::new(true);
    assert!(lab.bootstrap(1).witness().unwrap().is_none());
    assert!(!lab.paths[1].dir.join("commons/tables").exists());
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-warm-proc-")
        .tempdir_in("/tmp")
        .unwrap();
    let configs = [0, 1].map(|n| {
        let directory = observers.path().join(n.to_string());
        fsx::ensure_private_dir(&directory).unwrap();
        directory.join("aterm.toml")
    });
    // Read before the children start, which then hold the vault for their rounds.
    let trust = lab.bootstrap(0).trust();
    let table = Descriptor::application(trust, "warm-processes", "items", 1).unwrap();
    let first = br#"{"key":"before-processes","quantity":1}"#;
    let first_id = append(&lab.paths[0], &table, true, first, &configs);
    let mut expected = Records::from([(first_id, first.to_vec())]);
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let source = Process::start_with_match(
        &lab.paths[0],
        &configs[0],
        reservation.port(),
        None,
        Some(table.scope),
    );
    let recipient = Process::start_with_match(
        &lab.paths[1],
        &configs[1],
        reservation.port(),
        None,
        Some(table.scope),
    );
    let identities = [
        wait_owner(&configs[0], source.id()).observer,
        wait_owner(&configs[1], recipient.id()).observer,
    ];
    source.matched(&configs[0], table.scope, &configs);
    recipient.matched(&configs[1], table.scope, &configs);
    let witness = while_children_work(
        "the recipient's witness",
        &mut || logged(&configs),
        |until| {
            Bootstrap::required_until(&lab.paths[1], until)
                .and_then(|bootstrap| bootstrap.witness())
                .map_err(crate::table::failure)
        },
    );
    assert!(witness.is_some());
    wait_records(
        &lab.paths[1],
        table.scope,
        &expected,
        "initial enrollment",
        &configs,
    );

    // Both independent processes have completed matching scans. These writes
    // originate outside them; the test neither restarts them nor clears their caches.
    let forward = br#"{"key":"source-after-warmup","quantity":2}"#;
    let forward_id = append(&lab.paths[0], &table, false, forward, &configs);
    expected.insert(forward_id, forward.to_vec());
    wait_records(
        &lab.paths[1],
        table.scope,
        &expected,
        "external forward write",
        &configs,
    );
    let reverse = br#"{"key":"recipient-after-warmup","quantity":3}"#;
    let reverse_id = append(&lab.paths[1], &table, false, reverse, &configs);
    expected.insert(reverse_id, reverse.to_vec());
    wait_records(
        &lab.paths[0],
        table.scope,
        &expected,
        "external reverse write",
        &configs,
    );

    let later = Descriptor::application(trust, "warm-processes", "later", 1).unwrap();
    let later_payload = br#"{"key":"later-table","quantity":4}"#;
    let later_id = append(&lab.paths[0], &later, true, later_payload, &configs);
    let later_expected = Records::from([(later_id, later_payload.to_vec())]);
    wait_records(
        &lab.paths[1],
        later.scope,
        &later_expected,
        "later declared table",
        &configs,
    );
    for (config, (pid, identity)) in configs.iter().zip([
        (source.id(), &identities[0]),
        (recipient.id(), &identities[1]),
    ]) {
        assert_eq!(&wait_owner(config, pid).observer, identity);
    }
    recipient.stop();
    source.stop();

    // Reopen only after both transport owners stop, then compare every exact
    // retained ID and decrypted value. A contact or receipt count is not enough.
    for paths in &lab.paths {
        let until = Instant::now() + HANG;
        assert_eq!(records(paths, table.scope, until).unwrap(), expected);
        assert_eq!(records(paths, later.scope, until).unwrap(), later_expected);
    }
    println!("commons-process verified enrollment, warm bidirectional writes and later table");
}

/// `wait_records`' wait, with the clock and the children's work in the test's hands and their
/// rounds in a log the test writes as the child fixture does: each look advances the clock ten
/// seconds and finds the children's work count risen, and the records come three minutes in.
/// Where the child's last flight line says a round of its is in flight, or a round ends every
/// thirty seconds, the wait lasts until they come; where neither, the children's work is their
/// rest (timers, beacons, polls), and the wait ends a minute in, its failure naming why.
#[test]
fn a_records_wait_counts_the_childrens_work_on_rounds_alone() {
    let hang = MUST_HAPPEN;
    let step = Duration::from_secs(10);
    let logs = aterm_tempfile::tempdir().unwrap();
    let expected = Records::from([([1; 32], b"record".to_vec())]);
    for case in ["in flight", "ending", "resting"] {
        let children = [logs.path().join(format!("{}.toml", case.replace(' ', "-")))];
        let log = round_log(&children[0]);
        append_flight(&log, true);
        if case != "in flight" {
            append_flight(&log, false);
        }
        let began = Instant::now();
        let at = std::cell::Cell::new(began);
        let mut count = 0_u64;
        let mut used = || {
            at.set(at.get() + step);
            count += 1;
            if case == "ending" && count % 3 == 0 {
                append_round(
                    &log,
                    &super::RepairRound::completed_with(crate::replica::Report::default()),
                );
            }
            count
        };
        let ended = await_records(
            &mut |_| {
                Ok(if at.get() >= began + hang * 3 {
                    expected.clone()
                } else {
                    Records::new()
                })
            },
            [1; 32],
            &expected,
            &children,
            &mut Seams {
                now: &mut || at.get(),
                used: &mut used,
                pause: Duration::ZERO,
            },
        );
        if case == "resting" {
            let why = ended.unwrap_err();
            assert!(why.contains("Hung"), "{case}: {why}");
            assert!(at.get() < began + hang + step * 2, "{case}: {why}");
        } else {
            assert_eq!(ended, Ok(()), "{case}");
            assert!(at.get() >= began + hang * 3, "{case}");
        }
    }
}

/// `while_children_work`'s attempts, with the clock and the children's work in the test's hands:
/// every attempt is refused busy (a child's round holds this Mac's journal) and advances the
/// clock ten seconds. While a round of the children's is in flight, the attempts go on until one
/// is answered three minutes in; where none is in flight and none ends (a lock held across no
/// round), they end a minute in, naming the last refusal. Any other refusal ends them at once.
#[test]
fn refused_attempts_last_while_the_children_work_on_rounds_and_name_the_last_refusal() {
    let hang = MUST_HAPPEN;
    let step = Duration::from_secs(10);
    for in_flight in [true, false] {
        let began = Instant::now();
        let at = std::cell::Cell::new(began);
        let mut used = 0_u64;
        let got = await_attempts(
            &mut || Rounds {
                ended: 0,
                in_flight,
            },
            &mut |_| {
                at.set(at.get() + step);
                if at.get() >= began + hang * 3 {
                    Ok(7)
                } else {
                    Err(crate::Error::busy())
                }
            },
            &mut Seams {
                now: &mut || at.get(),
                used: &mut || {
                    used += 1;
                    used
                },
                pause: Duration::ZERO,
            },
        );
        if in_flight {
            assert_eq!(got, Ok(7), "a round in flight");
        } else {
            let why = got.unwrap_err();
            let busy = format!("{:?}", crate::Error::busy());
            assert!(why.contains("Hung") && why.contains(&busy), "{why}");
            assert!(at.get() < began + hang + step * 2, "{why}");
        }
    }
    let refused = await_attempts(
        &mut Rounds::default,
        &mut |_| Err::<(), _>(crate::Error::new("refused otherwise")),
        &mut Seams {
            now: &mut Instant::now,
            used: &mut || 0,
            pause: Duration::ZERO,
        },
    );
    assert!(refused.unwrap_err().contains("refused otherwise"));
}
