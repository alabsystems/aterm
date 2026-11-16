// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Cmd, Live, RunFailed, System};
use crate::repository_progress::{Phase, Progress};

/// A progress record for a test transfer, of a checkout that does not exist.
fn progress() -> Progress {
    Progress::new(
        "law",
        std::path::PathBuf::from("/nonexistent-aterm-law/checkout"),
    )
}

/// A TRANSFER THAT KEEPS REPORTING PROGRESS OUTLIVES ITS BUDGET AND ITS STALL WINDOW: a child
/// that writes Git's progress meter once a second, as Git's own meter moves while objects
/// trickle in, for eight seconds or more (eight `sleep 1`s, which never return early), finishes
/// under a one-second budget and a five-second window, and its meter is read to its last line.
/// The window is the ticks' hang detector: a tick whose `sleep` a loaded Mac starts up to four
/// seconds late still lands inside it. NEGATIVE CONTROL: the same child with its budget as a
/// wall clock ([`System::run_complete_stdout`]) is ended at that budget.
#[test]
fn a_clone_that_keeps_reporting_progress_outlives_its_budget() {
    const METER: &str = r"i=1
while [ $i -le 8 ]; do
  printf 'Receiving objects: %3d%% (%d/8)\r' $((i * 100 / 8)) $i >&2
  /bin/sleep 1
  i=$((i + 1))
done
printf 'Receiving objects: 100%% (8/8), done.\n' >&2";
    let budget = Duration::from_secs(1);
    let clone = Cmd::new("/bin/sh").args(["-c", METER]).budget(budget);
    let live = Live::stalling_after(Duration::from_secs(5));
    let mut progress = progress();
    let started = Instant::now();
    let ran = live.run_repository(&clone, &mut progress).unwrap();
    assert_eq!(ran.status, Some(0), "{ran:?}");
    assert!(
        started.elapsed() >= Duration::from_secs(8),
        "eight one-second sleeps ran"
    );
    assert_eq!(progress.phase, Phase::Downloading);
    assert_eq!(progress.percent, Some(100));
    assert!(matches!(
        live.run_complete_stdout(&clone),
        Err(RunFailed::TimedOut(at)) if at == budget
    ));
}

/// A TRANSFER SILENT PAST ITS STALL WINDOW IS STOPPED, AS A STALL: a child that writes nothing
/// and does no work (`sleep`) under a one-second window ends with [`RunFailed::Stalled`] of that
/// window, its process group killed. The minute the child would sleep is the hang detector: a
/// transfer the window did not stop ends then, with exit 0, and fails the assertion. NEGATIVE
/// CONTROL: silence shorter than its window (a second's sleep under a minute's window) finishes.
#[test]
fn a_transfer_silent_past_its_stall_window_is_stopped() {
    let window = Duration::from_secs(1);
    assert_eq!(
        Live::stalling_after(window)
            .run_repository(&Cmd::new("/bin/sleep").arg("60"), &mut progress()),
        Err(RunFailed::Stalled(window))
    );
    assert_eq!(
        Live::stalling_after(Duration::from_secs(60))
            .run_repository(&Cmd::new("/bin/sleep").arg("1"), &mut progress())
            .map(|ran| ran.status),
        Ok(Some(0))
    );
}

/// At which look over `window` a transfer that writes nothing stalls, its tree read as
/// `reading` gives at each look (the `n`th, from 1), within `looks` looks; `None` where it
/// never does.
fn stalls_at(window: Duration, looks: u32, reading: impl Fn(u32) -> Option<Work>) -> Option<u32> {
    let start = Instant::now();
    let mut judged = Window::new(window, start);
    let look = judged.look();
    (1..=looks).find(|&n| judged.stalled(start + look * n, 0, || reading(n)))
}

/// WORK WHERE GIT PRINTS NOTHING COUNTS AS PROGRESS, AND TIMER WAKEUPS DO NOT: over a minute's
/// window (a look every five seconds), a silent transfer whose tree spends a twentieth of a core
/// (an index-pack, a checkout) or only writes the disk never stalls over five minutes; one that
/// spends what Git's once-a-second progress timer costs (ten microseconds a second) and touches
/// no disk stalls at its window, the twelfth look; and one that works for a minute, then waits,
/// stalls a window after its last work. A tree that cannot be read is no evidence either way: one
/// never readable stalls at its window, the twelfth look, as an idle one does, so a transfer
/// whose tree cannot be read holds the repository lease no longer than one that waits; and a
/// working tree whose every other reading fails never stalls, each reading judged against the
/// last one that could be made.
/// NEGATIVE CONTROL: the first (working) tree's readings with its CPU time held still stall at
/// the window as the timer's do.
#[test]
fn work_where_git_prints_nothing_keeps_a_transfer_alive_and_timer_wakeups_do_not() {
    let window = Duration::from_secs(60);
    let look_ns = 5_000_000_000_u64;
    let spending = |share: u64| {
        move |n: u32| {
            Some(Work {
                cpu_ns: look_ns / share * u64::from(n),
                io_bytes: Some(0),
            })
        }
    };
    assert_eq!(stalls_at(window, 60, spending(20)), None, "a working tree");
    assert_eq!(
        stalls_at(window, 60, |n| Some(Work {
            cpu_ns: 0,
            io_bytes: Some(4096 * u64::from(n)),
        })),
        None,
        "a tree writing the disk"
    );
    assert_eq!(
        stalls_at(window, 60, |_| None),
        Some(12),
        "a tree never read stalls at its window"
    );
    assert_eq!(
        stalls_at(window, 60, |n| (n % 2 == 0)
            .then(|| spending(20)(n))
            .flatten()),
        None,
        "a working tree whose every other reading fails"
    );
    assert_eq!(
        stalls_at(window, 60, |n| Some(Work {
            cpu_ns: 50_000 * u64::from(n),
            io_bytes: Some(0),
        })),
        Some(12),
        "a tree its progress timer wakes"
    );
    assert_eq!(
        stalls_at(window, 60, |n| Some(Work {
            cpu_ns: look_ns / 20 * u64::from(n.min(12)),
            io_bytes: Some(0),
        })),
        Some(24),
        "a tree that stopped working after its twelfth look"
    );
    assert_eq!(
        stalls_at(window, 60, |_| Some(Work {
            cpu_ns: look_ns,
            io_bytes: Some(0),
        })),
        Some(12),
        "NEGATIVE CONTROL: the working tree with its CPU time held still"
    );
}

/// A WRITE RESTARTS THE WINDOW: a transfer idle for most of its window that then writes a byte
/// is judged from that write, and stalls a whole window after it, not before. NEGATIVE CONTROL:
/// the same idle tree with no write stalls at the window.
#[test]
fn a_write_restarts_the_window() {
    let window = Duration::from_secs(60);
    let start = Instant::now();
    let idle = |_: u32| {
        Some(Work {
            cpu_ns: 0,
            io_bytes: Some(0),
        })
    };
    let mut judged = Window::new(window, start);
    let look = judged.look();
    for n in 1..=11 {
        assert!(!judged.stalled(start + look * n, 0, || idle(n)));
    }
    let wrote = start + look * 11 + look / 2;
    assert!(!judged.stalled(wrote, 1, || idle(0)));
    assert!(
        !judged.stalled(start + window, 1, || idle(0)),
        "the window is judged from the write"
    );
    assert!(!judged.stalled(wrote + window - look, 1, || idle(0)));
    assert!(judged.stalled(wrote + window, 1, || idle(0)));
    assert_eq!(
        stalls_at(window, 60, idle),
        Some(12),
        "NEGATIVE CONTROL: no write"
    );
}
