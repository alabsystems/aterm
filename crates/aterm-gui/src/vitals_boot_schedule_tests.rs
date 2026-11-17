// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Read { machine_only: bool, self_pid: u32 },
    Deadline(Option<Duration>),
}

struct CheckpointSource {
    model: aterm_vitals::Vitals,
    seen: std::sync::mpsc::Sender<Seen>,
    full: bool,
}

impl Source for CheckpointSource {
    fn reading(&mut self, request: &Request) -> Snapshot {
        if !request.machine_only {
            self.full = true;
        }
        let _ = self.seen.send(Seen::Read {
            machine_only: request.machine_only,
            self_pid: request.self_pid,
        });
        Snapshot {
            machine_only: request.machine_only,
            ..Snapshot::default()
        }
    }

    fn model(&mut self) -> &mut aterm_vitals::Vitals {
        &mut self.model
    }

    fn boot_after(&mut self) -> Option<Duration> {
        let after = (!self.full).then_some(Duration::ZERO);
        let _ = self.seen.send(Seen::Deadline(after));
        after
    }
}

/// Close the worker's only sender on an assertion failure as well as on the successful path.
struct Worker {
    sender: Option<SyncSender<Msg>>,
    ended: Receiver<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    fn send(&self, message: Msg) {
        self.sender.as_ref().unwrap().send(message).unwrap();
    }

    fn stop(&mut self) -> bool {
        drop(self.sender.take());
        let Some(handle) = self.handle.take() else {
            return true;
        };
        match self.ended.recv_timeout(aterm_spec::wait::MUST_HAPPEN) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => handle.join().is_ok(),
            Err(RecvTimeoutError::Timeout) => false,
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let stopped = self.stop();
        if !std::thread::panicking() {
            assert!(
                stopped,
                "the mock vitals worker ended within its hang detector"
            );
        }
    }
}

/// A zero boot deadline asks one full reading without turning the saved machine-only request
/// into a full one. A policy-triggered read reuses that saved request, so changing it in place
/// fails this control even if a later explicit Read would have replaced it again.
#[test]
fn a_boot_deadline_reads_full_once_and_preserves_the_workers_machine_only_request() {
    let (sender, receiver) = std::sync::mpsc::sync_channel(2);
    let (seen, observations) = std::sync::mpsc::channel();
    let (ended, done) = std::sync::mpsc::channel();
    let machine_request = || Request {
        self_pid: 73,
        machine_only: true,
        ..Request::default()
    };
    // Queue the first request before starting. A deadline queried while self_pid is unknown
    // would precede its first Read event and fail the first assertion below.
    sender.send(Msg::Read(Box::new(machine_request()))).unwrap();
    let handle = std::thread::spawn(move || {
        let latest = Mutex::new(Handover::default());
        run(
            &receiver,
            &latest,
            &|| true,
            CheckpointSource {
                model: aterm_vitals::Vitals::new(),
                seen,
                full: false,
            },
            None,
            None,
            &AtomicBool::new(false),
            PriorRun::default(),
        );
        let _ = ended.send(());
    });
    let mut worker = Worker {
        sender: Some(sender),
        ended: done,
        handle: Some(handle),
    };
    let next = || {
        observations
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the mock worker makes its next scheduling observation")
    };

    assert_eq!(
        next(),
        Seen::Read {
            machine_only: true,
            self_pid: 73
        }
    );
    assert_eq!(next(), Seen::Deadline(Some(Duration::ZERO)));
    assert_eq!(
        next(),
        Seen::Read {
            machine_only: false,
            self_pid: 73
        }
    );
    assert_eq!(
        next(),
        Seen::Deadline(None),
        "the resolved checkpoint leaves no repeat timer"
    );

    worker.send(Msg::CpuPolicy(aterm_vitals::sys::json::Jv::obj([(
        "fixture",
        aterm_vitals::sys::json::Jv::Bool(true),
    )])));
    assert_eq!(
        next(),
        Seen::Read {
            machine_only: true,
            self_pid: 73
        },
        "an ordinary read reuses the original request after the full checkpoint"
    );
    assert_eq!(next(), Seen::Deadline(None));

    worker.send(Msg::Read(Box::new(machine_request())));
    assert_eq!(
        next(),
        Seen::Read {
            machine_only: true,
            self_pid: 73
        }
    );
    assert_eq!(next(), Seen::Deadline(None));
    assert!(worker.stop(), "dropping the sender ends the worker");
    assert_eq!(
        observations.try_iter().collect::<Vec<_>>(),
        [],
        "no later checkpoint reading"
    );
}
