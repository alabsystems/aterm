// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE: session 0's shell is forked before the event loop is built, and what it
//! writes before the loop exists reaches its screen whole (r5 sweep #4, LAG-11).
//!
//! The boot used to fork session 0 after the event loop's build, which on a macOS
//! window is `NSApplication` and its WindowServer connection (28.6 and 60.2 ms in
//! the owner's two logged launches). Now its shell starts first, and its reader
//! runs at once, posting into a sink that holds every wake until the loop exists
//! (`aterm-gui`'s `LaneSink`).
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

#[test]
fn session_zero_forks_before_the_event_loop_is_built_and_its_first_bytes_arrive_whole() {
    let mut watcher = None;
    // The watcher's verdict is said before the boot's own: a boot that never forks
    // its shell under the hold reaches its control socket only once the hold ends.
    let booted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        boot_with(
            "aterm-s0early",
            |root: &Path, cmd| {
                let shell = root.join("first-shell");
                standin(&shell, FIRST_SHELL);
                cmd.arg("--shell")
                    .arg(&shell)
                    .env("ATERM_DEBUG_LOOP_BUILD_HOLD", root.join("hold"))
                    // A headless instance with no display variable builds the
                    // display-free loop, which cannot fail, so session 0 goes first
                    // on every unix; with one, a free-unix build opens the display
                    // and goes first (`loop_before_session0`).
                    .env_remove("DISPLAY")
                    .env_remove("WAYLAND_DISPLAY")
                    .env_remove("WAYLAND_SOCKET");
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
