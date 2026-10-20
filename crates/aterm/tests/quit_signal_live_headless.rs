// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A HEADLESS INSTANCE'S `SIGTERM` IS A QUIT, against a real `aterm
//! --headless`. The 2026-10-16 real-login run stopped its two instances with
//! `kill -TERM <pid>` (how a harness or a person stops one) and found no quit
//! path ran: the workers' task files stayed under `<state>/tasks/` (the spawn
//! help says a quit removes them), the crash markers stayed, and the
//! `subscribe @* events,sessions` watchers got no `EVENT * bye quit` and
//! waited out their 30 s successor window. Now the signal runs the quit the
//! Quit verb runs (`owned_endpoint::arm_orderly_quit`), and the process still
//! dies of the signal, so its parent reads the status it always did.
//!
//! NEGATIVE CONTROL: the same world ended by `SIGKILL`, which no process can
//! act on, leaves the task file and the crash marker where they were — so the
//! assertions about their removal are about the quit, not about files that
//! were never there.
//!
//! Every wait is a one-minute hang detector. A machine that refuses the
//! headless instance is COULD NOT RUN ([`headless_boot::boot_with`]).
//! ISOLATION: `support/launch_isolation.rs` and `support/headless_boot.rs`,
//! the state seam (`ATERM_STATE_HOME`), the window harness on with the
//! stand-in `claude` (the only one on the instance's PATH, its login shell's
//! included: [`headless_boot::boot_with`] proves it) as its worker.

#![cfg(unix)]

use std::io::Write as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::{child_output, headless_boot, launch_isolation};

use headless_boot::{Instance, boot_session, client_command, ctl_ok, field};

/// A wait for something that MUST happen (AGENTS.md: a hang detector).
const HANG: Duration = Duration::from_secs(60);

/// The stand-in `claude`: `--version` answers at once; a task run exits 0,
/// and the worker's pane goes back to its shell (its session, and so its task
/// file, stays).
const STANDIN: &str = "#!/bin/sh\n\
    case \"$1\" in --version) echo '2.1.0 (Claude Code)'; exit 0 ;; esac\n\
    exit 0\n";

/// One headless instance with the harness on (a worker needs it), the
/// stand-in first on its PATH and its state at `<tmp>/state`.
#[track_caller]
fn boot(tag: &str) -> Instance {
    headless_boot::boot_with(
        &format!("atqs{tag}"),
        |tmp, cmd| {
            let cfg = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&cfg).expect("the isolation config");
            assert!(base.contains(launch_isolation::HARNESS_OFF), "{base}");
            let base = base.replace(launch_isolation::HARNESS_OFF, "");
            std::fs::write(
                &cfg,
                format!("{base}[harness]\nrelaunch = false\nupgrade = false\n"),
            )
            .expect("write the config");
            let bin = tmp.join("bin");
            headless_boot::standin(&bin.join("claude"), STANDIN);
            std::fs::create_dir_all(tmp.join("work")).expect("the worker's folder");
            cmd.env("SHELL", "/bin/zsh")
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
                )
                // Development seams (a debug build): the state root, so the
                // task file and the crash marker are where this test looks.
                .env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_LOG", "info");
        },
        |_| true,
    )
}

/// Start a worker as the boot session (`manager`) would: its task file.
fn spawn_worker(inst: &Instance, manager: &str) -> PathBuf {
    let mut cmd = client_command(
        inst,
        &["@self", "spawn", "agent=claude", "raise=false", "-"],
    );
    cmd.env("ATERM_PARENT_SESSION_ID", manager)
        .current_dir(inst.tmp.join("work"))
        .stdin(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn aterm ctl");
    child
        .stdin
        .take()
        .expect("its stdin")
        .write_all(b"say hello")
        .expect("write the task");
    let out = child_output::wait_and_read(child, Instant::now() + HANG).expect("aterm ctl exits");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "spawn: {stdout} {:?}", out.stderr);
    let worker = stdout
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("a sid: {stdout}"));
    let file = inst.tmp.join(format!("state/tasks/{worker}-1.txt"));
    assert!(file.exists(), "the task file is there before the signal");
    file
}

/// This instance's crash markers (`<state>/logs/crash-marker-<pid>-…`).
fn markers(inst: &Instance) -> Vec<PathBuf> {
    let prefix = format!("crash-marker-{}-", inst.child.id());
    std::fs::read_dir(inst.tmp.join("state/logs"))
        .map(|dir| {
            dir.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(&prefix))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Send `sig` to the instance by pid and reap it, within the hang detector.
fn signal_and_reap(inst: &mut Instance, sig: libc::c_int) -> std::process::ExitStatus {
    let pid = libc::pid_t::try_from(inst.child.id()).expect("a pid");
    // SAFETY: a plain signal to the child this test spawned and still owns.
    assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
    let give_up = Instant::now() + HANG;
    loop {
        if let Some(status) = inst.child.try_wait().expect("poll the instance") {
            return status;
        }
        assert!(
            Instant::now() < give_up,
            "the instance outlived signal {sig}; log:\n{}",
            headless_boot::log_tail(&inst.log)
        );
        std::thread::sleep(headless_boot::POLL_GAP);
    }
}

/// Wait until session `sid` has `n` watchers (`who`), within the hang detector.
fn until_watched(inst: &Instance, sid: &str, n: usize) {
    let give_up = Instant::now() + HANG;
    loop {
        let who = ctl_ok(inst, &["who"]);
        let watched = who
            .lines()
            .find(|l| l.split_whitespace().nth(1) == Some(sid))
            .and_then(|row| field(row, "watchers"))
            .and_then(|w| w.parse::<usize>().ok());
        if watched.is_some_and(|w| w >= n) {
            return;
        }
        assert!(Instant::now() < give_up, "never watched: {who}");
        std::thread::sleep(headless_boot::POLL_GAP);
    }
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

#[test]
fn sigterm_quits_a_headless_instance_as_the_quit_verb_does() {
    let mut inst = boot("t");
    let (_, manager) = boot_session(&inst);
    let task_file = spawn_worker(&inst, &manager);
    assert!(!markers(&inst).is_empty(), "the crash marker is armed");
    // A watcher as the run's: the client's own `subscribe`, its pushes on
    // stdout, attached before the signal.
    let watcher = client_command(&inst, &["subscribe", "@*", "events,sessions"])
        .spawn()
        .expect("spawn the watcher");
    until_watched(&inst, &manager, 1);

    let status = signal_and_reap(&mut inst, libc::SIGTERM);
    assert_eq!(
        status.signal(),
        Some(libc::SIGTERM),
        "it dies of the signal"
    );
    assert!(
        !exists(&task_file),
        "the quit removed the worker's task file"
    );
    assert_eq!(
        markers(&inst),
        Vec::<PathBuf>::new(),
        "and the crash marker"
    );
    assert!(!exists(Path::new(&inst.sock)), "and its socket");

    let out = child_output::wait_and_read(watcher, Instant::now() + HANG)
        .expect("the watcher ends once the instance has quit");
    let pushed = String::from_utf8_lossy(&out.stdout);
    assert!(
        pushed.lines().any(|l| l == "EVENT * bye quit"),
        "the watcher was told the instance quit:\n{pushed}"
    );
    assert_eq!(out.status.code(), Some(75), "a quit has no successor");
}

#[test]
fn sigkill_leaves_what_a_quit_would_have_removed() {
    let mut inst = boot("k");
    let (_, manager) = boot_session(&inst);
    let task_file = spawn_worker(&inst, &manager);
    assert!(!markers(&inst).is_empty(), "the crash marker is armed");
    let status = signal_and_reap(&mut inst, libc::SIGKILL);
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(exists(&task_file), "no quit ran: the task file stays");
    assert!(!markers(&inst).is_empty(), "and so does the crash marker");
}
