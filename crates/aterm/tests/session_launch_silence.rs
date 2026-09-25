// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A SESSION LAUNCH PRINTS NOTHING ABOUT UPDATES (Phase 2 of
//! `docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`, 2026-09-22): nothing prints
//! into a shell the user did not ask to update.
//!
//! The real binary as a person launches it: `aterm --session` with a REAL terminal on
//! stdin (the interactive lane — the one that spawns the background work) over a scratch
//! `HOME`, `[packages]` ENABLED, and stderr captured on a pipe of its own. (The session's
//! own greeting asks for a terminal on stderr too, so a pipe there keeps it out of the way;
//! the assertion is on every byte, not on one line.) Three machines:
//!
//! * a store that has never completed a pass — until 2026-09-22 every launch printed
//!   `atpkg: no update check has run yet on this machine — …`;
//! * a MALFORMED `[packages]` table — the launch read `aterm.toml` once per consumer and
//!   printed a five-line "ignoring malformed aterm.toml" block per read, 25 lines before the
//!   prompt (review, 2026-09-22);
//! * the reroute ENGAGED over a store whose `reroute/` cannot be made, with a retired
//!   `tracked_install` key and a configured prefix this user cannot write — every one of
//!   which printed on stderr, the lay failure from a thread beside the
//!   live shell.
//!
//! Nothing here reaches the machine: every root is private ([`launch_isolation`]), no
//! one-shot `aterm pkg update` is spawned (the lane's own pass slot is claimed in the
//! scratch prefix first, [`hold_the_pass`] — the lane is interactive and packages are
//! ENABLED, so without the claim a real, networked pass would start), the `[machine]`
//! settings are the fixture's `leave`, and the app updater is off (`[update] enabled =
//! false`) as in every launch test.

#![cfg(unix)]

use std::io::{Read as _, Write as _};
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

/// The session gets this long to start the shell, run `exit` and leave.
const DEADLINE: Duration = Duration::from_secs(60);

/// The `[machine]` table every fixture carries: nothing on the host is touched.
const MACHINE: &str = "[machine]\nspotlight_noindex = false\nuniversal_control = \"leave\"\n";

/// The `[update]` table every fixture carries: the app updater off, as in every launch
/// test (`launch_isolation::CONFIG_OFF`).
const UPDATE: &str = "[update]\nenabled = false\nauto_apply = false\n";

/// A private root with `packages` as the fixture's `[packages]` table.
fn fixture(label: &str, packages: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "aterm-session-launch-silence-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    launch_isolation::prepare(&root).expect("prepare private session state");
    std::fs::write(
        root.join("cfg/aterm/aterm.toml"),
        format!("agents_auto_prime = false\n{UPDATE}{packages}{MACHINE}"),
    )
    .expect("write the fixture config");
    root
}

/// The default store prefix under the fixture's `HOME`.
fn prefix(root: &Path) -> PathBuf {
    atpkg::store::default_prefix(&root.join("home"))
}

/// Make the default prefix the way atpkg makes it (every component 0700).
fn make_prefix(root: &Path) -> PathBuf {
    let home = root.join("home");
    let prefix = prefix(root);
    let mut dir = home.clone();
    for part in prefix
        .strip_prefix(&home)
        .expect("the prefix is under HOME")
    {
        dir.push(part);
        std::fs::create_dir_all(&dir).expect("make the prefix");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .expect("0700 like atpkg's own");
    }
    prefix
}

/// Claim the session lane's pass slot NOW in the scratch prefix, so a launch with
/// packages enabled spawns no pass: the machine-wide rule the lane runs on
/// (`pkg_check::claim` on `session-pass.stamp`) says a sibling just started one. The
/// environment kill switch this used to take is gone (2026-09-23).
fn hold_the_pass(root: &Path) {
    let layout = atpkg::store::Layout {
        prefix: make_prefix(root),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    std::fs::write(layout.session_pass_stamp(), format!("{now}\n")).expect("claim the slot");
}

/// Launch `aterm --session` over `root` on a real pty, type `exit 0`, and answer
/// `(stderr, screen)`. `reroute` leaves the reroute engaged; without it the launch
/// carries `--no-reroute`.
fn launch(root: &Path, reroute: bool) -> (String, String) {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: `openpty` writes the two fds through its out-params; the trailing three are
    // NULL, which the API defines as "default termios and window size".
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    // SAFETY: both fds come from the successful `openpty` above and are owned here.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, root);
    cmd.arg("--session");
    if !reroute {
        cmd.arg(launch_isolation::NO_REROUTE);
    }
    cmd.stdin(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stdout(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn the aterm binary");
    // The Command keeps its Stdio (two slave fds) until dropped, and the parent's own
    // slave must go too, or the master never sees EOF and the drain below never ends.
    drop(cmd);
    drop(slave);

    let mut stderr = child.stderr.take().expect("piped stderr");
    let stderr_drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let mut pty = std::fs::File::from(master);
    let mut input = pty.try_clone().expect("dup pty master");
    let pty_drain = std::thread::spawn(move || {
        let (mut out, mut chunk) = (Vec::new(), [0u8; 4096]);
        loop {
            match pty.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
            }
        }
        out
    });
    input.write_all(b"exit 0\n").expect("type `exit 0`");

    let start = Instant::now();
    let status_code = loop {
        match child.try_wait().expect("wait on aterm") {
            Some(status) => break status,
            None if start.elapsed() >= DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`aterm --session` did not exit within {DEADLINE:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    drop(input);
    // Bounded joins: a descendant holding a pipe or the slave past the session's exit
    // must fail this test, never wedge the suite.
    let join = |drain: std::thread::JoinHandle<Vec<u8>>, what: &str| -> String {
        let grace = Instant::now() + Duration::from_secs(10);
        while !drain.is_finished() {
            assert!(
                Instant::now() < grace,
                "the {what} stayed open past the session's exit"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        String::from_utf8_lossy(&drain.join().expect("drain")).into_owned()
    };
    let stderr = join(stderr_drain, "stderr pipe");
    let screen = join(pty_drain, "terminal");
    assert!(
        status_code.success(),
        "the session exits with the shell's status: {status_code:?}; screen={screen:?}; \
         stderr={stderr:?}"
    );
    (stderr, screen)
}

/// Nothing on stderr, and nothing of the package manager or the reroute on the terminal.
fn assert_silent(what: &str, stderr: &str, screen: &str) {
    assert_eq!(
        stderr, "",
        "{what}: the launch printed on stderr; screen={screen:?}"
    );
    for word in ["atpkg:", "no update check", "reroute", "aterm.toml"] {
        assert!(
            !screen.contains(word),
            "{what}: {word:?} reached the terminal: {screen:?}"
        );
    }
}

#[test]
fn an_interactive_session_launch_over_a_never_checked_store_writes_nothing_to_stderr() {
    // PACKAGES ENABLED — the gate the never-checked line was printed under — with the
    // pass slot held, so an interactive launch spawns no network work.
    let root = fixture("never-checked", "[packages]\nenabled = true\n");
    hold_the_pass(&root);
    let status = prefix(&root).join("status.toml");
    assert!(
        aterm_update_core::pkg_check::never_checked(&status),
        "precondition: no pass has completed in this HOME"
    );
    let (stderr, screen) = launch(&root, false);
    assert_silent("a never-checked store", &stderr, &screen);
    let _ = std::fs::remove_dir_all(&root);
}

/// A table atpkg cannot read is reported by a verb the person runs (`aterm pkg doctor`
/// loads the same file and says so on its own stderr), never by a session launch: here it
/// is one record in the session's log, and the launch reads the file once.
#[test]
fn a_malformed_packages_table_is_not_reported_into_the_new_shell() {
    // One typo: `enabled` wants a boolean. An unreadable table still lets what is
    // installed update (config.rs's module doc), so the pass gate would spawn a real,
    // networked pass: the slot is held. The table is read ahead of every gate.
    let root = fixture("malformed", "[packages]\nenabled = \"yes\"\n");
    hold_the_pass(&root);
    let (stderr, screen) = launch(&root, false);
    assert_silent("a malformed [packages] table", &stderr, &screen);
    let _ = std::fs::remove_dir_all(&root);
}

/// The reroute ENGAGED over a `reroute/` that cannot be made (a regular file stands
/// there), so both the directory and the background lay fail — the lay on a thread beside
/// the live shell — plus a retired `tracked_install` key and, where the test is not root, a
/// configured prefix this user cannot write. Each is a log record now.
#[test]
fn an_engaged_reroute_that_cannot_be_laid_and_a_misconfigured_table_say_nothing() {
    // `/usr/…`: a root-owned chain the vet admits and this user cannot write, so the
    // prefix falls back to the default, loudly — never tried as root, who could write it.
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let root_user = unsafe { libc::geteuid() } == 0;
    let foreign_prefix = if root_user {
        String::new()
    } else {
        "prefix = \"/usr/aterm-session-launch-silence\"\n".to_owned()
    };
    let root = fixture(
        "reroute",
        &format!("[packages]\nenabled = true\ntracked_install = \"maybe\"\n{foreign_prefix}"),
    );
    // The default prefix, made the way atpkg makes it (every component 0700), its pass
    // slot held, and a regular file where `reroute/` goes.
    let prefix = make_prefix(&root);
    hold_the_pass(&root);
    std::fs::write(prefix.join(atpkg::reroute::DIR_NAME), "not a directory\n")
        .expect("block reroute/");
    let (stderr, screen) = launch(&root, true);
    assert_silent("an unlaid reroute", &stderr, &screen);
    assert!(
        prefix.join(atpkg::reroute::DIR_NAME).is_file(),
        "precondition held: nothing could lay the reroute"
    );
    let _ = std::fs::remove_dir_all(&root);
}
