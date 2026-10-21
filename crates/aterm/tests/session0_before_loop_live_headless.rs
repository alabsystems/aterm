// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE: session 0's shell is forked before the event loop is built, and what it
//! writes before the loop exists reaches its screen whole (r5 sweep #4, LAG-11).
//!
//! The boot used to fork session 0 after the event loop's build, which on a macOS
//! window is `NSApplication` and its WindowServer connection (28.6 and 60.2 ms in
//! the owner's two logged launches). Now its shell starts first, and its reader
//! runs at once, posting into a sink that holds every wake until the loop exists
//! (`aterm-gui`'s `LaneSink`). On Linux the same holds where the build opens a
//! display that answered (LINUX-6): a headless boot with `DISPLAY` naming a private
//! Xvfb.
//!
//! Driven through the real binary with the build held by the dev seam
//! `ATERM_DEBUG_LOOP_BUILD_HOLD=<path>` (`main_entry` creates `<path>.held`, waits
//! for `<path>`, then creates `<path>.passed`). The boot shell is a stub that
//! writes its pid beside the scratch home, then a line naming that pid, then
//! becomes `/bin/sh`:
//!
//! * while the build is held, the stub has run: its pid file is there, the build
//!   has not passed, and the process is alive;
//! * once released, the boot session's screen shows the stub's line, once and
//!   whole, and the boot session's shell answers `$$` with that same pid.
//!
//! NEGATIVE CONTROL: with session 0 spawned after the build (the order before
//! LAG-11, `loop_before_session0` answering `true`), the stub cannot run while the
//! build is held: the wait for its pid file gives up ("no pid file from the boot
//! shell"), or the file appears only after `<path>.passed`, and the test fails
//! either way.
//!
//! Every wait is a one-minute hang detector, never a latency budget.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::headless_boot;

use headless_boot::{boot_session, boot_with, ctl_ok, standin, type_line};

/// The boot shell: its pid beside the scratch home (`<root>/home`, so the text
/// names no path and is shared, `standin`), then the line the test looks for,
/// then an interactive `/bin/sh` in the same process.
const FIRST_SHELL: &str = "#!/bin/sh\n\
[ \"$1\" = --version ] && exit 0\n\
printf '%s\\n' \"$$\" > \"$HOME/../shell.pid.part\" && mv \"$HOME/../shell.pid.part\" \"$HOME/../shell.pid\"\n\
printf 'first-bytes %s intact\\n' \"$$\"\n\
exec /bin/sh\n";

/// What the watcher saw while the build was held.
struct Held {
    pid: u32,
    /// `<path>.passed` existed when the pid file was first seen: the event loop
    /// was already being built.
    passed_first: bool,
}

/// Wait for the boot to reach the held build and for the stub's pid file, note
/// whether the build had passed by then, and release the hold whatever happened,
/// so the boot always goes on to its control socket.
fn watch_held_build(root: &Path) -> Result<Held, String> {
    let hold = root.join("hold");
    let (held, passed) = (root.join("hold.held"), root.join("hold.passed"));
    let pid_file = root.join("shell.pid");
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let seen = loop {
        if held.exists()
            && let Ok(text) = std::fs::read_to_string(&pid_file)
        {
            break Ok((text, passed.exists()));
        }
        if Instant::now() >= give_up {
            break Err(format!(
                "no pid file from the boot shell (build held: {}, passed: {})",
                held.exists(),
                passed.exists()
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let alive = |pid: u32| {
        i32::try_from(pid)
            // SAFETY: signal 0 delivers nothing; it only asks whether `pid` exists.
            .is_ok_and(|pid| unsafe { libc::kill(pid, 0) } == 0)
    };
    let held_view = seen.and_then(|(text, passed_first)| {
        let pid = text
            .trim()
            .parse::<u32>()
            .map_err(|e| format!("pid file {text:?}: {e}"))?;
        if !passed_first && !alive(pid) {
            return Err(format!(
                "the boot shell {pid} was gone while the build was held"
            ));
        }
        Ok(Held { pid, passed_first })
    });
    std::fs::write(&hold, b"").map_err(|e| format!("release the hold: {e}"))?;
    held_view
}

/// The boot session's screen once it shows `needle`.
fn screen_with(inst: &headless_boot::Instance, sid: &str, needle: &str) -> String {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        let text = ctl_ok(inst, &[&format!("@{sid}"), "text"]);
        if text.contains(needle) {
            return text;
        }
        assert!(
            Instant::now() < give_up,
            "the boot session never showed {needle:?}: {text}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Boot with the event loop's build held, `display` named (`DISPLAY`) or none, and
/// hold the boot to what the module doc says: the boot shell ran while the build was
/// held, its first line is on the boot session's screen once and whole, and its pid is
/// the boot session's shell.
fn forks_before_the_held_build(prefix: &str, display: Option<&str>) {
    let mut watcher = None;
    // The watcher's verdict is said before the boot's own: a boot that never forks
    // its shell under the hold reaches its control socket only once the hold ends.
    let booted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        boot_with(
            prefix,
            |root: &Path, cmd| {
                let shell = root.join("first-shell");
                standin(&shell, FIRST_SHELL);
                cmd.arg("--shell")
                    .arg(&shell)
                    .env("ATERM_DEBUG_LOOP_BUILD_HOLD", root.join("hold"))
                    .env_remove("WAYLAND_DISPLAY")
                    .env_remove("WAYLAND_SOCKET");
                // A headless instance with no display variable builds the
                // display-free loop, which cannot fail, so session 0 goes first
                // on every unix; with one, a free-unix build opens the display,
                // and goes first unless the display answered (`loop_before_session0`).
                match display {
                    Some(display) => cmd.env("DISPLAY", display),
                    None => cmd.env_remove("DISPLAY"),
                };
                let root: PathBuf = root.to_path_buf();
                watcher = Some(std::thread::spawn(move || watch_held_build(&root)));
            },
            |_| true,
        )
    }));
    let held = watcher
        .expect("the configure hook ran")
        .join()
        .expect("the watcher thread")
        .unwrap_or_else(|why| panic!("{why}"));
    assert!(
        !held.passed_first,
        "the boot shell {} started only once the event loop's build had begun",
        held.pid
    );
    let inst = booted.unwrap_or_else(|panic| std::panic::resume_unwind(panic));

    let (_, sid) = boot_session(&inst);
    let line = format!("first-bytes {} intact", held.pid);
    let screen = screen_with(&inst, &sid, &line);
    assert_eq!(
        screen.lines().filter(|l| l.trim_end() == line).count(),
        1,
        "the line the shell wrote before the loop existed, once and whole: {screen}"
    );

    // The pid that ran while the build was held is the boot session's shell.
    type_line(&inst, &sid, "echo s0-pid-$$");
    screen_with(&inst, &sid, &format!("s0-pid-{}", held.pid));
}

#[test]
fn session_zero_forks_before_the_event_loop_is_built_and_its_first_bytes_arrive_whole() {
    forks_before_the_held_build("aterm-s0early", None);
}

/// A private Xvfb, killed when dropped: it picks a free display and prints its number
/// (`-displayfd`). A Linux host with no `Xvfb` on PATH ends the test COULD NOT RUN.
#[cfg(target_os = "linux")]
struct Xvfb {
    child: aterm_uds::spawnfd::Child,
    /// `:<n>`, as `DISPLAY` names it.
    display: String,
}

#[cfg(target_os = "linux")]
impl Xvfb {
    fn start() -> Self {
        use aterm_uds::spawnfd::Stdio;
        let Some(xvfb) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("Xvfb"))
                .find(|p| p.is_file())
        }) else {
            aterm_spec::host::could_not_run(
                "no Xvfb on PATH, so no X server for the boot to open; install the \
                 distribution's Xvfb (xvfb) to run it",
            );
        };
        let mut child = aterm_uds::spawnfd::command(&xvfb)
            .args(["-displayfd", "1", "-nolisten", "tcp", "-noreset"])
            .args(["-screen", "0", "640x480x24"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start Xvfb");
        let stdout = child.stdout.take().expect("Xvfb's stdout");
        let (said, number) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut line);
            let _ = said.send(line);
        });
        let mut xvfb = Xvfb {
            child,
            display: String::new(),
        };
        let line = number
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("Xvfb printed no display number (a hang detector)");
        let n: u32 = line
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("Xvfb printed {line:?}, not a display number"));
        xvfb.display = format!(":{n}");
        xvfb
    }
}

#[cfg(target_os = "linux")]
impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// LINUX-6: where the event loop's build opens a display, session 0 forks first once a
/// display server has answered where winit will connect. Here a headless instance with
/// `DISPLAY` naming a private Xvfb, whose build opens it (a headless run with a display
/// keeps winit's real backend), forks its boot shell while the build is held, as a
/// launch with no display does.
///
/// NEGATIVE CONTROL: with the loop first wherever its build opens a display (the rule
/// before LINUX-6), the shell cannot start while the build is held: "no pid file from
/// the boot shell (build held: true, passed: false)".
#[cfg(target_os = "linux")]
#[test]
fn with_a_display_that_answers_session_zero_forks_before_the_event_loop_is_built() {
    let xvfb = Xvfb::start();
    forks_before_the_held_build("aterm-s0x11", Some(&xvfb.display));
}

/// The dev seam `ATERM_DEBUG_LOOP_FIRST` (WIN-1's windowed A/B of LAG-11): with it set,
/// the event loop is built first on every launch, the order before LAG-11, so session 0's
/// shell is forked only after the held build has passed. Read from the kernel, not from
/// what the shell wrote: the boot shell's start time (`aterm_pty::process_birth`, µs since
/// the epoch on macOS) is no earlier than `<hold>.passed`'s mtime.
///
/// The watcher releases the hold as soon as the build reaches it, waiting for no shell,
/// because with the seam none can start while the build is held.
///
/// NEGATIVE CONTROL: without the seam (or with `loop_before_session0` ignoring it), session
/// 0 is forked before the build begins, so its start time precedes `<hold>.held` and with it
/// `<hold>.passed`: the assertion fails, whatever the shell's own scheduling.
///
/// macOS only: Linux's start time counts clock ticks since boot, too coarse to order
/// against a file's mtime.
#[cfg(target_os = "macos")]
#[test]
fn with_the_loop_first_seam_session_zero_forks_only_after_the_event_loop_is_built() {
    let mut watcher = None;
    let inst = boot_with(
        "aterm-s0late",
        |root: &Path, cmd| {
            let shell = root.join("first-shell");
            standin(&shell, FIRST_SHELL);
            cmd.arg("--shell")
                .arg(&shell)
                .env("ATERM_DEBUG_LOOP_BUILD_HOLD", root.join("hold"))
                .env("ATERM_DEBUG_LOOP_FIRST", "1");
            let root: PathBuf = root.to_path_buf();
            watcher = Some(std::thread::spawn(move || release_once_held(&root)));
        },
        |_| true,
    );
    watcher
        .expect("the configure hook ran")
        .join()
        .expect("the watcher thread")
        .unwrap_or_else(|why| panic!("{why}"));

    let pid_file = inst.tmp.join("shell.pid");
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let pid = loop {
        if let Ok(pid) = std::fs::read_to_string(&pid_file)
            .map_err(|e| e.to_string())
            .and_then(|t| t.trim().parse::<i32>().map_err(|e| e.to_string()))
        {
            break pid;
        }
        assert!(Instant::now() < give_up, "no pid file from the boot shell");
        std::thread::sleep(Duration::from_millis(5));
    };
    let started = aterm_pty::process_birth(pid).expect("the boot shell is alive");
    let passed = mtime_us(&inst.tmp.join("hold.passed"));
    assert!(
        started >= passed,
        "the boot shell {pid} was forked {} µs before the held event loop's build passed",
        passed - started
    );

    let (_, sid) = boot_session(&inst);
    type_line(&inst, &sid, "echo s0-pid-$$");
    screen_with(&inst, &sid, &format!("s0-pid-{pid}"));
}

/// Release the held build once it is reached (`<root>/hold.held`), waiting for nothing else.
#[cfg(target_os = "macos")]
fn release_once_held(root: &Path) -> Result<(), String> {
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !root.join("hold.held").exists() {
        if Instant::now() >= give_up {
            return Err("the boot never reached the held event-loop build".to_string());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::write(root.join("hold"), b"").map_err(|e| format!("release the hold: {e}"))
}

/// A file's mtime in whole µs since the epoch (floored), the unit `process_birth` reads.
#[cfg(target_os = "macos")]
fn mtime_us(path: &Path) -> u64 {
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let since = modified
        .duration_since(std::time::UNIX_EPOCH)
        .expect("an mtime after the epoch");
    u64::try_from(since.as_micros()).expect("µs since the epoch fit u64")
}
