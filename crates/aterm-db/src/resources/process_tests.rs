// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Abrupt process death at the dispatch boundary, without Rust unwinding.

use super::*;
use aterm_secrets::client::Paths;
use std::{
    io::{Read, Write},
    os::unix::process::ExitStatusExt,
    path::Path,
    time::{Duration, Instant},
};

const HANG: Duration = Duration::from_secs(60);
const REQUEST: [u8; 16] = [80; 16];
const OPERATION: Digest = [81; 32];

fn observed(outcome: Outcome) -> Observation {
    Observation::new(outcome, [82; 32]).unwrap()
}

fn checkpoint() -> ! {
    println!("resource-dispatch-checkpoint");
    std::io::stdout().flush().unwrap();
    assert_eq!(std::io::stdin().read(&mut [0; 1]).unwrap(), 1);
    aterm_uds::process::kill_self_now();
}

#[test]
#[ignore = "child process fixture, invoked by sigkill_retains_dispatch_and_offline_liability"]
fn child_dispatch() {
    let home = std::env::var_os("ATERM_TEST_RESOURCE_HOME").unwrap();
    let paths = Paths::for_home(Path::new(&home));
    let encoded = std::env::var("ATERM_TEST_RESOURCE_ALLOCATION").unwrap();
    let bytes = hex::decode_vec(encoded.as_bytes()).unwrap();
    let proposal = Proposal::decode(&bytes).unwrap();
    let bootstrap = Bootstrap::required(&paths).unwrap();
    let allocation = Allocation::verify(&bytes, proposal.fingerprint(), bootstrap.trust()).unwrap();
    let phase: u8 = std::env::var("ATERM_TEST_RESOURCE_PHASE")
        .unwrap()
        .parse()
        .unwrap();
    assert!(phase < 3);
    let db = Resources::at(paths.clone(), allocation);
    let attempt = db
        .dispatch_with(
            REQUEST,
            1,
            OPERATION,
            || {
                if phase == 0 {
                    checkpoint();
                }
            },
            || {
                // A durable stand-in for an externally visible adapter effect.
                assert!(
                    fsx::write_new(&paths.home.join("adapter-entered"), b"entered", 0o600).unwrap()
                );
                if phase == 1 {
                    checkpoint();
                }
                Some(observed(Outcome::Completed))
            },
        )
        .unwrap();
    assert!(attempt.invoked);
    assert_eq!(
        attempt.observation,
        ObservationState::Recorded(observed(Outcome::Completed))
    );
    checkpoint();
}

struct Process(aterm_uds::spawnfd::Child);

impl Process {
    fn start(paths: &Paths, allocation: &Allocation, phase: u8) -> Self {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "resources::tests::process::child_dispatch",
                "--ignored",
                "--nocapture",
            ])
            .env("ATERM_TEST_RESOURCE_HOME", &paths.home)
            .env(
                "ATERM_TEST_RESOURCE_ALLOCATION",
                hex::encode_lower(allocation.bytes()),
            )
            .env("ATERM_TEST_RESOURCE_PHASE", phase.to_string())
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::inherit());
        let mut process = Self(command.spawn().unwrap());
        aterm_uds::deadline::prints(&mut process.0, "resource-dispatch-checkpoint", HANG);
        process
    }

    fn crash(mut self) {
        self.0.stdin.as_mut().unwrap().write_all(b"x").unwrap();
        let until = Instant::now() + HANG;
        let mut exit = aterm_uds::deadline::ExitWait::new(&self.0);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert_eq!(status.signal(), Some(9), "child must die without unwinding");
                break;
            }
            assert!(Instant::now() < until, "dispatch child did not exit");
            exit.wait(until.saturating_duration_since(Instant::now()));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        aterm_uds::deadline::kill_within(&mut self.0, HANG);
    }
}

#[test]
fn sigkill_retains_dispatch_and_offline_liability() {
    for machines in [1, 2] {
        for phase in 0..3 {
            let lab = Lab::new(&vec![Role::Writer; machines]);
            let allocation = allocation(&lab, &vec![1; machines]);
            let db = resources(&lab, 0, &allocation);
            db.initialize().unwrap();
            let child = Process::start(&lab.paths[0], &allocation, phase);
            let expected = if phase == 2 {
                ObservationState::Recorded(observed(Outcome::Completed))
            } else {
                ObservationState::Unknown
            };

            // A different process can observe and retry while the adapter is
            // stopped at the boundary: persistent locks cannot cross entry.
            let pending = db.attempt(REQUEST).unwrap().unwrap();
            assert_eq!(pending.observation, expected);
            assert!(!pending.invoked);
            let retry = || {
                let retried = db
                    .dispatch_once(REQUEST, 1, OPERATION, || {
                        panic!("process reentered adapter")
                    })
                    .unwrap();
                assert_eq!(retried, pending);
                assert!(db.reserve([64; 16], 1).is_err());
                assert_eq!(db.snapshot().unwrap().used.values().sum::<u64>(), 1);
            };
            retry();
            assert_eq!(
                lab.paths[0].home.join("adapter-entered").exists(),
                phase > 0
            );
            child.crash();
            retry();

            if machines == 2 {
                let peer = resources(&lab, 1, &allocation);
                peer.initialize().unwrap();
                let reservation = peer.reserve([65; 16], 1).unwrap();
                assert!(reservation.fresh);
                assert!(peer.reserve([66; 16], 1).is_err());
                rounds(&lab);
                assert_eq!(db.snapshot().unwrap(), peer.snapshot().unwrap());
                assert_eq!(db.snapshot().unwrap().used.values().sum::<u64>(), 2);
                assert_eq!(db.attempt(REQUEST).unwrap().unwrap(), pending);
                assert!(
                    !db.dispatch_once(REQUEST, 1, OPERATION, || panic!("repair reentered adapter"))
                        .unwrap()
                        .invoked
                );
                assert_eq!(
                    peer.reserve([65; 16], 1).unwrap().receipt,
                    reservation.receipt
                );
            }
        }
    }
}
