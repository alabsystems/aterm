// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real runtime processes share one custody, then transfer its transport while a
//! separately admitted peer keeps running. Only generated loopback state is used.

use super::{
    Network, Runtime, Visibility,
    auto_sync_tests::{Attachment, wait_network},
};
use crate::{accounts::proposals::Proposals, database::Change, network_state};
use aterm_net::lan::{Multicast, Scope as LinkScope};
use aterm_secrets::{client::Paths, commons::roster::Role, fsx};
use std::{
    io::{Read, Write},
    net::Ipv4Addr,
    os::unix::process::ExitStatusExt,
    path::Path,
    process::ExitStatus,
    sync::{Arc, atomic::AtomicUsize},
    thread,
    time::{Duration, Instant},
};

const HANG: Duration = Duration::from_secs(60);

#[test]
#[ignore = "child process fixture, invoked by separate_processes_transfer_live_repair"]
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
    let opens = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::start_on(
        Paths::for_home(Path::new(&home)),
        config.into(),
        |_| {},
        Network {
            isolated: Some(Attachment::new(port)),
            opens: Some(opens.clone()),
            ..Network::default()
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
    let opened = opens.load(std::sync::atomic::Ordering::Acquire);
    if owner.is_some() {
        assert_eq!(
            opened, 0,
            "a follower must relay without opening a transport"
        );
    } else {
        assert!(opened > 0, "an active owner must have opened its transport");
    }
    println!("commons-runtime-ready");
    std::io::stdout().flush().unwrap();
    if std::io::stdin().read(&mut [0; 1]).unwrap() == 1 {
        aterm_uds::process::kill_self_now();
    }
    runtime.stop();
}

struct Process(aterm_uds::spawnfd::Child);

impl Process {
    fn start(paths: &Paths, config: &Path, port: u16, owner: Option<u32>) -> Self {
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
            .env_remove("ATERM_TEST_COMMONS_OWNER")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::inherit());
        if let Some(owner) = owner {
            command.env("ATERM_TEST_COMMONS_OWNER", owner.to_string());
        }
        let mut process = Self(command.spawn().unwrap());
        aterm_uds::deadline::prints(&mut process.0, "commons-runtime-ready", HANG);
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

    fn wait(&mut self) -> ExitStatus {
        let until = Instant::now() + HANG;
        let mut exit = aterm_uds::deadline::ExitWait::new(&self.0);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < until, "runtime child did not exit");
            exit.wait(until.saturating_duration_since(Instant::now()));
        }
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

fn wait_owner(config: &Path, pid: u32) -> aterm_types::network_state::NetworkObservation {
    let until = Instant::now() + HANG;
    loop {
        let state = network_state::read_live_publication(config)
            .map(|publication| publication.observation().clone());
        if let Ok(state) = &state
            && state.via_local_process == Some(pid)
            && state.visibility == Visibility::Active
            && state.observer.is_some()
        {
            return state.clone();
        }
        assert!(
            Instant::now() < until,
            "process {pid} never owned repair: {state:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn separate_processes_transfer_live_repair() {
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
    let proposals = Proposals::at(lab.paths[0].clone());
    let table = proposals.initialize().unwrap();
    let write = |request, key: &str| {
        proposals
            .write(
                [request; 16],
                vec![Change {
                    collection: "accounts".into(),
                    key: key.into(),
                    value: Some(aterm_json::json!({"provider":"claude","method":"oauth"})),
                }],
            )
            .unwrap()
    };
    let first = write(61, "before-processes");
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
    progress.wait_for(table.digest(), 1, "peer discovered the initial table");

    let follower = Process::start(&lab.paths[0], &config, port, Some(owner.id()));
    owner.crash();
    assert_eq!(
        wait_owner(&config, follower.id()).observer,
        Some(identity.clone())
    );
    let after_crash = write(62, "after-owner-crash");
    progress.wait_for(
        table.digest(),
        2,
        "peer received the successor's fresh operation",
    );

    let restarted = Process::start(&lab.paths[0], &config, port, Some(follower.id()));
    follower.stop();
    assert_eq!(wait_owner(&config, restarted.id()).observer, Some(identity));
    let after_exit = write(63, "after-owner-exit");
    progress.wait_for(
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
