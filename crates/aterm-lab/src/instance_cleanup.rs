// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One recorded stop attempt and retained evidence when private-world cleanup is unknown.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::Fail;

pub(super) struct StopRecord {
    result: Option<Result<(), String>>,
    world: PathBuf,
    failed: Arc<AtomicBool>,
    pid: i32,
    evidence: Option<PathBuf>,
}

impl StopRecord {
    pub(super) fn new(world: &Path, failed: Arc<AtomicBool>, pid: i32) -> Self {
        Self {
            result: None,
            world: world.to_owned(),
            failed,
            pid,
            evidence: None,
        }
    }

    pub(super) fn attempt(
        &mut self,
        run: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        if let Some(result) = &self.result {
            return result.clone();
        }
        let result = run();
        if let Err(error) = &result {
            self.failed.store(true, Ordering::Release);
            let saved = (|| -> Result<PathBuf, String> {
                let dir = aterm_tempfile::Builder::new()
                    .prefix("cleanup-unknown-")
                    .suffix(".noindex")
                    .tempdir_in(&self.world)
                    .map_err(|why| why.to_string())?;
                aterm_uds::private_file::write_private(
                    dir.path(),
                    "result.txt",
                    format!(
                        "aterm-lab-cleanup/1\npid={}\nstate=unknown\nerror={error}\n",
                        self.pid
                    )
                    .as_bytes(),
                )
                .map_err(|why| why.to_string())?;
                Ok(dir.keep())
            })();
            match saved {
                Ok(path) => {
                    eprintln!(
                        "aterm-lab: cleanup unknown for pid {}: {error}; evidence {}",
                        self.pid,
                        path.display()
                    );
                    self.evidence = Some(path);
                }
                Err(why) => eprintln!(
                    "aterm-lab: cleanup unknown for pid {}: {error}; cannot save evidence: {why}; keeping {}",
                    self.pid,
                    self.world.display()
                ),
            }
        }
        self.result = Some(result.clone());
        result
    }
}

/// Preserve the workload's exit/signal evidence while reporting cleanup failure as well.
pub(crate) fn after_stop<T>(work: Result<T, Fail>, cleanup: Result<(), String>) -> Result<T, Fail> {
    match (work, cleanup) {
        (work, Ok(())) => work,
        (Ok(_), Err(error)) => Err(Fail::die(format!("cleanup failed: {error}"))),
        (Err(mut primary), Err(error)) => {
            primary.msg = Some(match primary.msg {
                Some(message) => format!("{message}; cleanup also failed: {error}"),
                None => format!("cleanup also failed: {error}"),
            });
            Err(primary)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::{CONFIG_OFF, World};

    #[test]
    fn failed_stop_is_recorded_once_and_preserves_world_and_private_evidence() {
        let mut world = World::new(CONFIG_OFF).unwrap();
        let path = world.dir.clone();
        let mut failed = StopRecord::new(&path, world.cleanup_failed.clone(), 4242);
        assert!(
            failed
                .attempt(|| Err("registration unknown".into()))
                .is_err()
        );
        let evidence = failed.evidence.clone().unwrap();
        let result = aterm_uds::private_file::read_private(&evidence, "result.txt", 4096).unwrap();
        assert!(
            std::str::from_utf8(&result)
                .unwrap()
                .contains("state=unknown\nerror=registration unknown")
        );
        assert_eq!(
            failed.attempt(|| panic!("no repeated cleanup timeout")),
            Err("registration unknown".into())
        );
        let mut other = StopRecord::new(&path, world.cleanup_failed.clone(), 4343);
        other.attempt(|| Ok(())).unwrap();
        world.release();
        drop(world);
        assert!(
            path.is_dir(),
            "another successful stop must not discard failed evidence"
        );
        assert_eq!(
            aterm_uds::private_file::read_private(&evidence, "result.txt", 4096).unwrap(),
            result
        );
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn successful_stop_is_idempotent_and_allows_normal_world_removal() {
        let world = World::new(CONFIG_OFF).unwrap();
        let path = world.dir.clone();
        let mut stopped = StopRecord::new(&path, world.cleanup_failed.clone(), 4242);
        stopped.attempt(|| Ok(())).unwrap();
        stopped
            .attempt(|| panic!("completed stop is not rerun by Drop"))
            .unwrap();
        assert!(stopped.evidence.is_none());
        drop(world);
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_does_not_replace_primary_failure_or_signal() {
        let primary = Fail {
            msg: Some("work failed".into()),
            code: 143,
            stopped: Some(15),
        };
        let combined = after_stop::<()>(Err(primary), Err("cleanup unknown".into())).unwrap_err();
        assert_eq!(combined.code, 143);
        assert_eq!(combined.stopped, Some(15));
        let message = combined.msg.unwrap();
        assert!(message.contains("work failed") && message.contains("cleanup unknown"));
        assert!(after_stop(Ok(42), Err("cleanup unknown".into())).is_err());
        assert_eq!(after_stop(Ok(42), Ok(())).unwrap(), 42);
    }

    /// The role a stop test's re-run of this binary plays: its probe, or the probe's stand-in.
    const PROBE: &str = "ATERM_LAB_INSTANCE_STOP_PROBE";

    /// The stand-in's line once it has read its lifeline's end, which only the stop cuts.
    const CUT_SEEN: &str = "PROBE the lifeline was cut";

    /// Run this binary's `test` alone as `role`: its exit status and its output.
    fn rerun(test: &str, role: &str) -> (Option<i32>, String) {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        cmd.args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(PROBE, role);
        let out = aterm_uds::deadline::output_until(
            &mut cmd,
            std::time::Instant::now() + std::time::Duration::from_mins(1),
        )
        .expect("the probe ends within the one-minute hang detector");
        (
            out.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// The stand-in's part: it waits for its lifeline's end, as a headless aterm does, says so,
    /// then ends after a moment (`ends`) or stays until its probe is gone (`stays`).
    fn stand_in(role: &str) -> ! {
        let lifeline = aterm_uds::lifeline::adopt(aterm_uds::lifeline::STDIN).expect("lifeline");
        let _ = aterm_uds::lifeline::wait_for_cut(lifeline);
        println!("{CUT_SEEN}");
        let probe = std::os::unix::process::parent_id();
        let end = std::time::Instant::now() + std::time::Duration::from_mins(1);
        if role == "ends" {
            std::thread::sleep(std::time::Duration::from_millis(500));
        } else {
            while std::os::unix::process::parent_id() == probe && std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        std::process::exit(0);
    }

    /// An instance in `world` whose process is this binary's `test` as the stand-in `role`, on
    /// its lifeline in a session of its own, as [`super::super::Instance::start`] starts one; with
    /// the stand-in's stdout.
    fn instance_of(
        world: &World,
        test: &str,
        role: &str,
    ) -> (super::super::Instance, std::process::ChildStdout) {
        use aterm_uds::spawnfd::Stdio;
        let exe = std::env::current_exe().expect("test binary");
        let (lifeline, far) = aterm_uds::lifeline::Lifeline::open(&world.dir).expect("lifeline");
        let mut child = aterm_uds::spawnfd::command(&exe)
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(PROBE, role)
            .setsid(true)
            .stdin(Stdio::from(far))
            .stdout(Stdio::piped())
            .spawn()
            .expect("the stand-in starts");
        let said = child.stdout.take().expect("the stand-in's stdout");
        let pid = i32::try_from(child.id()).expect("pid");
        let instance = super::super::Instance {
            child: super::super::Proc::Child(child),
            lifeline: Some(lifeline),
            pid,
            kept: false,
            aterm: exe.clone(),
            client: exe,
            sock: world.sock.clone(),
            log: world.dir.join("aterm.log"),
            env: world.env(),
            _admission: None,
            cleanup: StopRecord::new(&world.dir, world.cleanup_failed.clone(), pid),
            gui: None,
        };
        (instance, said)
    }

    /// Raise a stop in this probe as a signal to it does, once it catches them.
    fn raise_stop() -> i32 {
        crate::sys::start_signal(libc::SIGTERM, false);
        crate::stop::install();
        let me = i32::try_from(std::process::id()).expect("pid");
        crate::sys::kill(me, libc::SIGTERM).expect("the stop's signal");
        assert_eq!(
            crate::stop::sleep(std::time::Duration::from_mins(1)),
            Err(crate::stop::Stopped(libc::SIGTERM)),
            "the stop is noted"
        );
        me
    }

    #[test]
    fn a_stop_waits_through_the_signal_that_asked_for_it_for_the_instances_end() {
        const TEST: &str = "instance::cleanup::tests::a_stop_waits_through_the_signal_that_asked_for_it_for_the_instances_end";
        match std::env::var(PROBE).as_deref() {
            Ok("probe") => {
                let world = World::new(CONFIG_OFF).unwrap();
                let dir = world.dir.clone();
                let (mut instance, _said) = instance_of(&world, TEST, "ends");
                raise_stop();
                let stopped = instance.stop();
                drop(instance);
                drop(world);
                let removed = !dir.exists();
                let _ = std::fs::remove_dir_all(&dir);
                println!("PROBE stop={stopped:?} world_removed={removed}");
                std::process::exit(0);
            }
            Ok(role) => stand_in(role),
            Err(_) => {}
        }
        let (code, out) = rerun(TEST, "probe");
        assert_eq!(code, Some(0), "{out}");
        assert!(
            out.contains("PROBE stop=Ok(()) world_removed=true"),
            "a stop's cleanup waits through the signal that asked for it, sees the instance end \
             and removes its world: {out}"
        );
    }

    #[test]
    fn a_second_signal_cuts_the_stops_wait_short_and_it_says_the_instance_still_runs() {
        const TEST: &str = "instance::cleanup::tests::a_second_signal_cuts_the_stops_wait_short_and_it_says_the_instance_still_runs";
        match std::env::var(PROBE).as_deref() {
            Ok("probe") => {
                use std::io::BufRead as _;
                let world = World::new(CONFIG_OFF).unwrap();
                let dir = world.dir.clone();
                let (mut instance, said) = instance_of(&world, TEST, "stays");
                let me = raise_stop();
                // The second signal once the stand-in read its lifeline's end: the stop is
                // under way then, waiting for an end that does not come.
                let second = std::thread::spawn(move || {
                    let seen = std::io::BufReader::new(said)
                        .lines()
                        .map_while(Result::ok)
                        .any(|line| line.ends_with(CUT_SEEN));
                    seen && crate::sys::kill(me, libc::SIGTERM).is_ok()
                });
                let stopped = instance.stop();
                let pid = instance.pid;
                let presence = aterm_uds::process::process_presence(pid);
                crate::sys::kill(pid, libc::SIGKILL).expect("end the stand-in the stop left");
                if let super::super::Proc::Child(child) = &mut instance.child {
                    child.wait().expect("reap the stand-in");
                }
                drop(instance);
                drop(world);
                let kept = dir.is_dir();
                let _ = std::fs::remove_dir_all(&dir);
                let second = second.join().expect("the second signal's thread");
                let expected = format!(
                    "signal {} cut the child grace short; the instance's pid {pid} still runs",
                    libc::SIGTERM
                );
                println!(
                    "PROBE second={second} truthful={} presence={presence:?} world_kept={kept} \
                     stop={stopped:?}",
                    stopped.as_ref().err() == Some(&expected)
                );
                std::process::exit(0);
            }
            Ok(role) => stand_in(role),
            Err(_) => {}
        }
        let (code, out) = rerun(TEST, "probe");
        assert_eq!(
            code,
            Some(0),
            "a second signal during a stop cuts its wait short, not the run: {out}"
        );
        assert!(
            out.contains("PROBE second=true truthful=true presence=Present world_kept=true"),
            "the cut stop says the instance's pid still runs and keeps its world: {out}"
        );
    }

    #[test]
    fn keeper_absence_does_not_close_a_live_unknown_or_unidentified_gui() {
        use aterm_uds::process::ProcessPresence::{Absent, Exited, Present, Unknown};
        assert!(!super::super::gui_closed(true, true, Present).unwrap());
        assert!(!super::super::gui_closed(true, true, Exited).unwrap());
        assert!(super::super::gui_closed(true, true, Unknown).is_err());
        assert!(super::super::gui_closed(true, false, Absent).is_err());
        assert!(super::super::gui_closed(true, true, Absent).unwrap());
        assert!(super::super::gui_closed(false, false, Unknown).unwrap());
    }
}
