// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drive the actual poll/read retry loop with explicit elapsed time. No sleeping, process,
//! descriptor, or timing budget is involved; an extra retry exhausts the script immediately.

use super::*;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const MASTER: i32 = 10;
const WAKE: i32 = 11;
const EXIT: i32 = 12;
const PROC: i32 = 13;
const POKE: i32 = 14;

struct PollStep {
    ms: u64,
    outcome: Result<Vec<(i32, libc::c_short)>, io::ErrorKind>,
}

fn ready(ms: u64, fd: i32, events: libc::c_short) -> PollStep {
    PollStep {
        ms,
        outcome: Ok(vec![(fd, events)]),
    }
}

fn interrupted(ms: u64) -> PollStep {
    PollStep {
        ms,
        outcome: Err(io::ErrorKind::Interrupted),
    }
}

struct Script {
    origin: Instant,
    elapsed: Duration,
    polls: VecDeque<PollStep>,
    reads: VecDeque<(u64, Result<usize, io::ErrorKind>)>,
    timeouts: Vec<i32>,
    watched: Vec<Vec<i32>>,
    read_count: usize,
}

impl Script {
    fn new(polls: Vec<PollStep>, reads: Vec<(u64, Result<usize, io::ErrorKind>)>) -> Self {
        Self {
            origin: Instant::now(),
            elapsed: Duration::ZERO,
            polls: polls.into(),
            reads: reads.into(),
            timeouts: Vec::new(),
            watched: Vec::new(),
            read_count: 0,
        }
    }
}

impl ReadWait for Script {
    fn now(&mut self) -> Instant {
        self.origin + self.elapsed
    }

    fn poll(&mut self, fds: &mut [libc::pollfd], timeout: i32) -> io::Result<usize> {
        self.timeouts.push(timeout);
        self.watched
            .push(fds.iter().map(|entry| entry.fd).collect());
        let step = self
            .polls
            .pop_front()
            .expect("no poll after the original deadline");
        self.elapsed += Duration::from_millis(step.ms);
        for entry in fds.iter_mut() {
            entry.revents = 0;
        }
        let events = step.outcome.map_err(io::Error::from)?;
        for &(fd, revents) in &events {
            fds.iter_mut()
                .find(|entry| entry.fd == fd)
                .expect("only a still-watched descriptor can fire")
                .revents = revents;
        }
        Ok(events.len())
    }

    fn read(&mut self, master: i32, buf: &mut [u8]) -> io::Result<usize> {
        assert_eq!(master, MASTER, "watch bytes are never consumed");
        self.read_count += 1;
        let (ms, outcome) = self.reads.pop_front().expect("only a ready master is read");
        self.elapsed += Duration::from_millis(ms);
        let count = outcome.map_err(io::Error::from)?;
        buf[..count].fill(b'x');
        Ok(count)
    }
}

fn fds(wake: i32) -> ReadFds {
    ReadFds {
        master: MASTER,
        wake,
        exit: EXIT,
        process: PROC,
        poke: POKE,
    }
}

#[test]
fn one_original_deadline_survives_every_actual_retry_branch() {
    let mut script = Script::new(
        vec![
            interrupted(20),
            ready(20, MASTER, libc::POLLIN),
            ready(15, MASTER, libc::POLLIN),
            ready(10, EXIT, libc::POLLERR),
            ready(10, PROC, libc::POLLHUP),
            interrupted(15),
        ],
        vec![
            (5, Err(io::ErrorKind::WouldBlock)),
            (5, Err(io::ErrorKind::Interrupted)),
        ],
    );
    assert!(matches!(
        read_or_poke_with(fds(WAKE), &mut [0; 8], 100, &mut script),
        ReadOutcome::Idle
    ));
    assert_eq!(script.timeouts, [100, 80, 55, 35, 25, 15]);
    assert_eq!(script.elapsed, Duration::from_millis(100));
    assert_eq!(script.read_count, 2);
    assert_eq!(
        script.watched[4][2], -1,
        "nonterminal exit readiness is disabled"
    );
    assert_eq!(
        script.watched[5][3], -1,
        "nonterminal process readiness is disabled"
    );
    assert!(script.polls.is_empty() && script.reads.is_empty());
}

#[test]
fn zero_bound_reads_immediately_ready_data_once_and_never_retries() {
    for wake in [WAKE, -1] {
        let mut script = Script::new(vec![ready(0, MASTER, libc::POLLIN)], vec![(0, Ok(1))]);
        let mut buf = [0; 8];
        assert!(matches!(
            read_or_poke_with(fds(wake), &mut buf, 0, &mut script),
            ReadOutcome::Data(1)
        ));
        assert_eq!(buf[0], b'x');
        assert_eq!(script.timeouts, [0]);
        let mut script = Script::new(vec![interrupted(0)], vec![]);
        assert!(matches!(
            read_or_poke_with(fds(wake), &mut buf, 0, &mut script),
            ReadOutcome::Idle
        ));
        assert_eq!(script.timeouts, [0]);
    }
}

#[test]
fn negative_bound_keeps_legacy_unbounded_retries_until_ready_data() {
    let mut script = Script::new(
        vec![
            interrupted(1_000_000),
            ready(1_000_000, MASTER, libc::POLLIN),
            ready(1_000_000, MASTER, libc::POLLIN),
        ],
        vec![(0, Err(io::ErrorKind::WouldBlock)), (0, Ok(2))],
    );
    assert!(matches!(
        read_or_poke_with(fds(WAKE), &mut [0; 8], -9, &mut script),
        ReadOutcome::Data(2)
    ));
    assert_eq!(script.timeouts, [-1, -1, -1]);
    assert_eq!(script.read_count, 2);
}

#[test]
fn wakeless_cap_and_ready_watch_priority_remain_unchanged() {
    for (within, expected) in [(-1, 250), (1_000, 250), (10, 10)] {
        let mut script = Script::new(
            vec![PollStep {
                ms: expected as u64,
                outcome: Ok(vec![]),
            }],
            vec![],
        );
        assert!(matches!(
            read_or_poke_with(fds(-1), &mut [0; 8], within, &mut script),
            ReadOutcome::Idle
        ));
        assert_eq!(script.timeouts, [expected]);
        assert_eq!(script.read_count, 0);
    }
    for (fd, expected) in [(WAKE, 0), (POKE, 1), (EXIT, 2), (PROC, 3)] {
        let mut script = Script::new(vec![ready(0, fd, libc::POLLIN)], vec![]);
        let outcome = read_or_poke_with(fds(WAKE), &mut [0; 8], 0, &mut script);
        assert!(matches!(
            (expected, outcome),
            (0, ReadOutcome::Wake)
                | (1, ReadOutcome::Poke)
                | (2, ReadOutcome::Exited)
                | (3, ReadOutcome::Proc)
        ));
        assert_eq!(script.read_count, 0);
    }
}
