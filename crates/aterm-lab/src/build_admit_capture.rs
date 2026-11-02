// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Opt-in stacks of an owned, unreaped build front. Sampling uses the readiness deadline;
//! killing and reaping the sampler is separate safety cleanup and can finish after it.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Command, Stdio, command};

const POLL: Duration = Duration::from_millis(20);
static CAPTURE: Mutex<()> = Mutex::new(());

/// Hold before a hard exit releases the front barriers: a sampler still owns their pids.
pub(super) fn lock() -> MutexGuard<'static, ()> {
    CAPTURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A sampler always has a private group. No caller reaps its leader before this guard ends it.
struct Sampler {
    child: Option<Child>,
}

impl Sampler {
    fn finish(&mut self) -> Result<ExitStatus, String> {
        let child = self.child.as_mut().ok_or("sampler was already reaped")?;
        let signalled = super::sweep_group(child.id());
        // Also end the spawned leader if the group sweep could not deliver a signal.
        let _ = child.kill();
        // Safety cleanup deliberately does not stop at the sampling/readiness deadline.
        // After SIGKILL the kernel may still have to finish an exec assessment or a syscall.
        let status = child.wait().map_err(|e| format!("reap sampler: {e}"));
        // A wait was attempted: never signal this group again after a possibly reaping wait.
        self.child = None;
        if let Err(error) = signalled {
            return Err(format!("end sampler group: {error}; wait: {status:?}"));
        }
        status
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.finish();
        }
    }
}

fn sidecar(output: &Path, suffix: &str) -> PathBuf {
    let mut path = output.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn sampler(pid: u32, output: &Path) -> Command {
    let mut cmd = command("/usr/bin/sample");
    cmd.arg(pid.to_string())
        .args(["1", "10", "-mayDie", "-fullPaths", "-file"])
        .arg(output);
    cmd
}

/// Capture one stack receipt. Failure is diagnostic only; this never reaps or signals target.
pub(super) fn capture(target: &mut Child, output: &Path, ready_by: Instant) -> String {
    capture_using(
        target,
        output,
        ready_by,
        &|pid, path| Ok(sampler(pid, path)),
        &|| crate::stop::check().is_err(),
    )
}

fn capture_using(
    target: &mut Child,
    output: &Path,
    ready_by: Instant,
    make_sampler: &dyn Fn(u32, &Path) -> io::Result<Command>,
    interrupted: &dyn Fn() -> bool,
) -> String {
    // This guard outlives the sampler guard, including during panic unwinding. The hard-exit
    // callback can release target barriers only after the sampler is ended and reaped.
    let _capture = lock();
    let pid = target.id();
    let unavailable = |why: &str| format!("sample pid={pid}: unavailable ({why})");
    match target.exited() {
        Ok(false) => {}
        Ok(true) => return unavailable("owned target exited; it was not sampled or reaped"),
        Err(error) => return unavailable(&format!("target exit query: {error}")),
    }
    let image = i32::try_from(pid)
        .ok()
        .and_then(aterm_uds::process::read_process_path);
    let observed = format!("sample pid={pid} observed_image={image:?} (may exec-change)");
    let unavailable = |why: &str| format!("{observed}: unavailable ({why})");
    if interrupted() {
        return unavailable("interrupted before sampling");
    }
    if Instant::now() >= ready_by {
        return unavailable("readiness deadline elapsed before sampling");
    }
    let stdout = sidecar(output, ".stdout");
    let stderr = sidecar(output, ".stderr");
    let spawn = || -> io::Result<Child> {
        // Truncate stale output before the attempt, so a successful empty capture cannot
        // accidentally inherit evidence from a previous invocation.
        drop(File::create(output)?);
        let mut cmd = make_sampler(pid, output)?;
        cmd.stdin(Stdio::null())
            .stdout(File::create(&stdout)?)
            .stderr(File::create(&stderr)?)
            .process_group(0)
            .setsid(true);
        if interrupted() || Instant::now() >= ready_by {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "sampling cancelled before spawn",
            ));
        }
        cmd.spawn()
    };
    let child = match spawn() {
        Ok(child) => child,
        Err(error) => return unavailable(&format!("sampler unavailable: {error}")),
    };
    let mut running = Sampler { child: Some(child) };
    let mut exit =
        aterm_uds::deadline::ExitWait::new(running.child.as_ref().expect("owned sampler"));
    let outcome = loop {
        if interrupted() {
            break Err("interrupted during sampling".to_owned());
        }
        let left = ready_by.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break Err("readiness deadline elapsed during sampling".to_owned());
        }
        match running.child.as_mut().expect("owned sampler").exited() {
            Ok(true) => break Ok(()),
            Ok(false) => exit.wait(POLL.min(left)),
            Err(error) => break Err(format!("sampler exit query: {error}")),
        }
    };
    let status = running.finish();
    match (outcome, status) {
        (Err(why), cleanup) => unavailable(&format!("{why}; cleanup={cleanup:?}")),
        (Ok(()), Err(why)) => unavailable(&why),
        (Ok(()), Ok(status)) if !status.success() => {
            unavailable(&format!("sampler {status}; stderr={}", stderr.display()))
        }
        (Ok(()), Ok(_)) => match fs::metadata(output) {
            Ok(metadata) if metadata.is_file() && metadata.len() > 0 => {
                format!("{observed}: captured {}", output.display())
            }
            Ok(_) => unavailable("sampler produced no nonempty output file"),
            Err(error) => unavailable(&format!("sampler output unavailable: {error}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLE: &str = "ATERM_BUILD_ADMIT_CAPTURE_ROLE";
    const ROOT: &str = "ATERM_BUILD_ADMIT_CAPTURE_ROOT";
    const PID: &str = "ATERM_BUILD_ADMIT_CAPTURE_PID";
    const HANG: Duration = Duration::from_secs(60);

    #[test]
    fn standin() {
        let Ok(role) = std::env::var(ROLE) else {
            return;
        };
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        fs::write(
            root.join(format!("{role}.ready")),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        match role.as_str() {
            "success" => fs::write(root.join("sample"), std::env::var(PID).unwrap()).unwrap(),
            "empty" => {}
            "refused" => std::process::exit(13),
            "target" => loop {
                std::thread::sleep(POLL);
            },
            "blocked" => {
                while !root.join("sampler.release").exists() {
                    std::thread::sleep(POLL);
                }
            }
            _ => panic!("unknown stand-in role"),
        }
    }

    fn standin_command(root: &Path, role: &str) -> Command {
        let mut cmd = command(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "build_admit::capture::tests::standin",
            "--test-threads=1",
        ])
        .env_clear()
        .env(ROLE, role)
        .env(ROOT, root);
        cmd
    }

    fn target(root: &Path) -> Sampler {
        let mut cmd = standin_command(root, "target");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .setsid(true);
        Sampler {
            child: Some(cmd.spawn().unwrap()),
        }
    }

    #[test]
    fn sampler_receives_only_the_owned_targets_pid_and_never_reaps_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut target = target(root.path());
        let child = target.child.as_mut().unwrap();
        let pid = child.id();
        let receipt = capture_using(
            child,
            &root.path().join("sample"),
            Instant::now() + HANG,
            &|selected, _| {
                let mut cmd = standin_command(root.path(), "success");
                cmd.env(PID, selected.to_string());
                Ok(cmd)
            },
            &|| false,
        );
        let alive = !child.exited().unwrap();
        let selected = fs::read_to_string(root.path().join("sample")).unwrap();
        target.finish().unwrap();
        assert!(receipt.contains(": captured "), "{receipt}");
        assert_eq!(selected, pid.to_string());
        assert!(alive, "capture ended or reaped its target");
    }

    #[test]
    fn refusal_and_empty_output_are_diagnostic_only() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut target = target(root.path());
        for role in ["empty", "refused", "permission"] {
            let receipt = capture_using(
                target.child.as_mut().unwrap(),
                &root.path().join("sample"),
                Instant::now() + HANG,
                &|_, _| {
                    if role == "permission" {
                        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                    }
                    Ok(standin_command(root.path(), role))
                },
                &|| false,
            );
            assert!(receipt.contains(": unavailable ("), "{receipt}");
        }
        let receipt = capture_using(
            target.child.as_mut().unwrap(),
            &root.path().join("sample"),
            Instant::now(),
            &|_, _| panic!("expired readiness must not start a sampler"),
            &|| false,
        );
        assert!(
            receipt.contains("readiness deadline elapsed before sampling"),
            "{receipt}"
        );
        let child = target.child.as_mut().unwrap();
        child.kill().unwrap();
        let started = Instant::now();
        while !child.exited().unwrap() {
            assert!(started.elapsed() < HANG, "hang detector: target exit");
            std::thread::sleep(POLL);
        }
        let receipt = capture_using(
            child,
            &root.path().join("sample"),
            Instant::now() + HANG,
            &|_, _| panic!("an exited target must not launch a sampler"),
            &|| false,
        );
        target.finish().unwrap();
        assert!(receipt.contains("owned target exited"));
    }

    #[test]
    fn interruption_and_panic_reap_the_sampler_while_target_remains_owned() {
        for panic in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut target = target(root.path());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                capture_using(
                    target.child.as_mut().unwrap(),
                    &root.path().join("sample"),
                    Instant::now() + HANG,
                    &|_, _| Ok(standin_command(root.path(), "blocked")),
                    &|| {
                        let ready = fs::read_to_string(root.path().join("blocked.ready"))
                            .ok()
                            .and_then(|text| {
                                text.strip_suffix('\n')
                                    .and_then(|pid| pid.parse::<i32>().ok())
                            });
                        if ready.is_some() {
                            assert!(!panic, "stand-in for capture unwinding");
                            true
                        } else {
                            false
                        }
                    },
                )
            }));
            let sampler_pid = fs::read_to_string(root.path().join("blocked.ready"))
                .unwrap()
                .trim()
                .parse::<i32>()
                .unwrap();
            let sampler_gone = aterm_uds::process::process_presence(sampler_pid)
                == aterm_uds::process::ProcessPresence::Absent;
            let alive = !target.child.as_mut().unwrap().exited().unwrap();
            // A negative control with sampler Drop removed must release and finish its
            // stand-in before an assertion reports the leaked sampler.
            fs::write(root.path().join("sampler.release"), "go\n").unwrap();
            let by = Instant::now() + HANG;
            while aterm_uds::process::process_presence(sampler_pid)
                != aterm_uds::process::ProcessPresence::Absent
                && Instant::now() < by
            {
                std::thread::sleep(POLL);
            }
            target.finish().unwrap();
            assert_eq!(
                aterm_uds::process::process_presence(sampler_pid),
                aterm_uds::process::ProcessPresence::Absent,
                "fallback did not finish the sampler",
            );
            assert_eq!(result.is_err(), panic);
            assert!(sampler_gone && alive, "sampler leaked or target was reaped");
            if let Ok(receipt) = result {
                assert!(receipt.contains("interrupted during sampling"), "{receipt}");
            }
        }
    }
}
