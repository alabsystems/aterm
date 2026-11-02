// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE SPAWN LOCK COSTS: a spawn's latency and a kept-out socket's creation latency while
//! other threads spawn without pause, with the socket made through `aterm_uds::kept_out` (the
//! lock) and made then marked (no lock).

#[cfg(target_vendor = "apple")]
mod bench {
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    /// How long each lane runs, and how often its creator makes a socket.
    const LANE: Duration = Duration::from_secs(8);
    const CREATE_EVERY: Duration = Duration::from_millis(2);

    /// p50, p95, p99 and the most of `samples`, in milliseconds.
    fn spread(mut samples: Vec<Duration>) -> String {
        if samples.is_empty() {
            return "none".into();
        }
        samples.sort_unstable();
        let at = |q: usize| samples[(samples.len() - 1) * q / 100].as_secs_f64() * 1e3;
        format!(
            "n={} p50 {:.3} ms p95 {:.3} ms p99 {:.3} ms max {:.3} ms",
            samples.len(),
            at(50),
            at(95),
            at(99),
            samples[samples.len() - 1].as_secs_f64() * 1e3
        )
    }

    fn create(guarded: bool) -> (UnixStream, UnixStream) {
        if guarded {
            aterm_uds::kept_out(UnixStream::pair).expect("a kept-out pair")
        } else {
            let pair = UnixStream::pair().expect("a pair");
            aterm_uds::keep_out_of_children(&pair.0).expect("mark");
            aterm_uds::keep_out_of_children(&pair.1).expect("mark");
            pair
        }
    }

    /// `spawners` threads spawning `/usr/bin/true` back to back until `stop`: each one's
    /// spawn calls' latencies.
    fn spawning(
        spawners: usize,
        stop: &Arc<AtomicBool>,
    ) -> Vec<std::thread::JoinHandle<Vec<Duration>>> {
        (0..spawners)
            .map(|_| {
                let stop = Arc::clone(stop);
                std::thread::spawn(move || {
                    let mut took = Vec::new();
                    while !stop.load(Ordering::Relaxed) {
                        let started = Instant::now();
                        let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
                            .spawn()
                            .expect("spawn");
                        took.push(started.elapsed());
                        child.wait().expect("wait");
                    }
                    took
                })
            })
            .collect()
    }

    /// `spawners` threads spawning `/usr/bin/true` back to back beside one creator, for
    /// [`LANE`]: each spawn call's latency, and each creation's.
    fn lane(spawners: usize, guarded: bool) -> (Vec<Duration>, Vec<Duration>) {
        let stop = Arc::new(AtomicBool::new(false));
        let threads = spawning(spawners, &stop);
        let mut creations = Vec::new();
        let ends = Instant::now() + LANE;
        while Instant::now() < ends {
            let started = Instant::now();
            let pair = create(guarded);
            creations.push(started.elapsed());
            drop(pair);
            std::thread::sleep(CREATE_EVERY);
        }
        stop.store(true, Ordering::Relaxed);
        let spawns = threads
            .into_iter()
            .flat_map(|t| t.join().expect("a spawner"))
            .collect();
        (spawns, creations)
    }

    /// `spawners` threads spawning `/usr/bin/true` back to back beside one that hands two
    /// kept-out lanes to its own `/usr/bin/true` (the fabric bridge's spawn, which holds the lock
    /// exclusively while the lanes are unmarked) every [`CREATE_EVERY`], for [`LANE`]: the plain
    /// spawns' latencies, and the handing spawns'.
    fn handing_lane(spawners: usize) -> (Vec<Duration>, Vec<Duration>) {
        let stop = Arc::new(AtomicBool::new(false));
        let threads = spawning(spawners, &stop);
        let mut handing = Vec::new();
        let ends = Instant::now() + LANE;
        while Instant::now() < ends {
            let (_near_verb, far_verb) = aterm_uds::kept_out(UnixStream::pair).expect("a lane");
            let (_near_push, far_push) = aterm_uds::kept_out(UnixStream::pair).expect("a lane");
            let mut cmd = aterm_uds::spawnfd::LaneCommand::new("/usr/bin/true");
            cmd.stdin(aterm_uds::spawnfd::ChildStdio::Null)
                .stdout(aterm_uds::spawnfd::ChildStdio::Null)
                .stderr(aterm_uds::spawnfd::ChildStdio::Null);
            let started = Instant::now();
            let mut child =
                aterm_uds::spawnfd::spawn_with_two_fds(cmd, far_verb.into(), far_push.into())
                    .expect("a handing spawn");
            handing.push(started.elapsed());
            child.wait().expect("wait");
            std::thread::sleep(CREATE_EVERY);
        }
        stop.store(true, Ordering::Relaxed);
        let spawns = threads
            .into_iter()
            .flat_map(|t| t.join().expect("a spawner"))
            .collect();
        (spawns, handing)
    }

    pub fn run() {
        const TAKES: u32 = 1_000_000;
        let started = Instant::now();
        for _ in 0..TAKES {
            drop(std::hint::black_box(aterm_uds::spawn_lock::spawning()));
        }
        println!(
            "an uncontended spawn guard: {:.1} ns",
            started.elapsed().as_secs_f64() * 1e9 / f64::from(TAKES)
        );
        for spawners in [0, 1, 4] {
            for guarded in [false, true] {
                let (spawns, creations) = lane(spawners, guarded);
                let rate = spawns.len() as f64 / LANE.as_secs_f64();
                println!(
                    "{spawners} spawner(s), creation {}: {rate:.0} spawns/s",
                    if guarded {
                        "kept_out (the lock)"
                    } else {
                        "made then marked (no lock)"
                    }
                );
                println!("  spawn():  {}", spread(spawns));
                println!("  creation: {}", spread(creations));
            }
        }
        let (spawns, handing) = handing_lane(4);
        let rate = spawns.len() as f64 / LANE.as_secs_f64();
        println!("4 spawner(s) beside a spawn handing kept-out lanes: {rate:.0} spawns/s");
        println!("  spawn():    {}", spread(spawns));
        println!("  handing():  {}", spread(handing));
    }
}

fn main() {
    #[cfg(target_vendor = "apple")]
    bench::run();
    #[cfg(not(target_vendor = "apple"))]
    println!("the spawn lock is Apple's: nothing to measure on this target");
}
