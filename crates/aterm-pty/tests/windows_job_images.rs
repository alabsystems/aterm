// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The programs a Windows tab's commands run, as `job_images` lists them over a real ConPTY.
//!
//! STOCK EXCEPTION: Windows only, and Trust has no Windows std, so only stock Rust builds this.

#![cfg(windows)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// A hang detector, never a latency budget: every wait below ends on bytes the shell writes.
const DEADLINE: Duration = Duration::from_secs(60);

/// What the shell is asked to run: a program that writes at once and then outlives the listing.
const TYPED: &[u8] = b"ping -n 60 127.0.0.1\r";

/// What `ping` writes with its first reply in every display language (`Reply from 127.0.0.1:
/// bytes=32 time<1ms TTL=128`, `Antwort von 127.0.0.1: Bytes=32 Zeit<1ms TTL=128`); its banner is
/// localized, and the typed line the console echoes does not hold this.
const RUNNING: &[u8] = b"TTL=";

fn caps() -> (
    aterm_cap::Cap<aterm_cap::effects::Spawn>,
    aterm_cap::Cap<aterm_sandbox::Sandbox>,
) {
    // SAFETY: test-process mint; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    (
        authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted),
        authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted),
    )
}

/// Whether `image` is `ping`'s: `C:\Windows\System32\PING.EXE`, in any case.
fn is_ping(image: &Path) -> bool {
    image
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("ping.exe"))
}

/// Both listings name `ping`.
fn assert_ping_listed(listed: &[Vec<(u32, PathBuf)>; 2]) {
    let [at_pace, at_mark] = listed;
    assert!(
        at_pace.iter().any(|(_, image)| is_ping(image)),
        "at the pace: {at_pace:?}"
    );
    assert!(
        at_mark.iter().any(|(_, image)| is_ping(image)),
        "at a block's start or end: {at_mark:?}"
    );
}

/// Run `ping` at `shell`'s prompt and return what the session lists once ping has written: at
/// the pace first (`job_images_at_pace`, which never walks to find out where a shell runs its
/// programs, so it names a packaged shell's only because the session knew at its start), then
/// at a block's start or end (`job_images`).
fn listed_while_ping_runs(shell: Vec<OsString>) -> [Vec<(u32, PathBuf)>; 2] {
    let (tx, rx) = mpsc::channel();
    let name = format!("{shell:?}");
    std::thread::Builder::new()
        .name("job-images".into())
        .spawn(move || {
            let (spawn_cap, sandbox_cap) = caps();
            let spawned = aterm_pty::spawn_shell_with_pid(
                24,
                80,
                &spawn_cap,
                &sandbox_cap,
                &[],
                None,
                None,
                None,
                Some(&shell),
                None,
                None,
                aterm_sandbox::Limits::inherit(),
            )
            .expect("the shell spawns under a ConPTY");
            let (master, pid) = (spawned.master, spawned.pid);
            aterm_pty::write_all(master, TYPED);
            let mut out = Vec::new();
            let mut buf = [0u8; 4096];
            let ran = loop {
                let n = aterm_pty::read(master, &mut buf);
                if n <= 0 {
                    break false;
                }
                out.extend_from_slice(&buf[..n as usize]);
                if out.windows(RUNNING.len()).any(|w| w == RUNNING) {
                    break true;
                }
            };
            let listed = [
                aterm_pty::job_images_at_pace(pid),
                aterm_pty::job_images(pid),
            ];
            aterm_pty::hangup(pid);
            while aterm_pty::read(master, &mut buf) > 0 {}
            aterm_pty::reap(pid);
            aterm_pty::close_master(master);
            let _ = tx.send((ran, String::from_utf8_lossy(&out).into_owned(), listed));
        })
        .expect("the worker thread spawns");
    let (ran, out, listed) = rx
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|e| panic!("{name}: no answer within {DEADLINE:?}: {e}"));
    assert!(ran, "{name}: ping never wrote: {out:?}");
    listed
}

/// A shell on `PATH` as aterm's own spawn finds it, or `None` on a machine without it.
fn on_path(name: &str) -> Option<OsString> {
    match aterm_pty::classify_shell_name(name.as_ref()) {
        aterm_pty::ShellResolution::Resolved(path) => Some(path),
        _ => None,
    }
}

#[test]
fn a_program_run_at_windows_powershells_prompt_is_listed() {
    let listed = listed_while_ping_runs(vec![
        "powershell.exe".into(),
        "-NoLogo".into(),
        "-NoProfile".into(),
    ]);
    assert_ping_listed(&listed);
}

#[test]
fn a_program_run_at_cmds_prompt_is_listed() {
    let listed = listed_while_ping_runs(vec!["cmd.exe".into()]);
    assert_ping_listed(&listed);
}

/// PowerShell 7 as this machine has it: a Store install is a packaged app, whose programs
/// Windows starts outside the tab's job.
#[test]
fn a_program_run_at_pwshs_prompt_is_listed() {
    let Some(pwsh) = on_path("pwsh") else {
        eprintln!("no pwsh on PATH: nothing to list");
        return;
    };
    eprintln!("pwsh is {}", Path::new(&pwsh).display());
    let listed = listed_while_ping_runs(vec![pwsh, "-NoLogo".into(), "-NoProfile".into()]);
    assert_ping_listed(&listed);
}

/// The shell's own process and the console host are no command's program.
#[test]
fn the_shell_and_its_console_host_are_never_listed() {
    let listed = listed_while_ping_runs(vec!["cmd.exe".into()]);
    for (_, image) in listed.iter().flatten() {
        let name = image.file_name().unwrap_or_default().to_ascii_lowercase();
        assert!(
            name != "cmd.exe" && name != "openconsole.exe" && name != "conhost.exe",
            "{listed:?}"
        );
    }
}
