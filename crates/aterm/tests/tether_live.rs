// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A helper a window tethers ends when the window is killed outright; one started plainly is
//! adopted and runs on. The window is a stand-in: this test binary, re-run as [`PARENT`], which
//! spawns both helpers through `aterm_uds::spawnfd` with the front door as its tether executable,
//! as a window's helpers run through the window's own.

use std::os::fd::{AsRawFd as _, OwnedFd};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_spec::wait::MUST_HAPPEN;
use aterm_uds::spawnfd::{Stdio, Tether as _};

use crate::{launch_isolation, standin};

const ENTRY: &str = "tether_live::a_tethered_helper_ends_with_its_killed_parent";

/// The stand-in window's [`standin::ENV`] word.
const PARENT: &str = "tether-parent";

/// The helper: it says it ran, by its pid, then sleeps ten minutes (a bound on a leftover).
const HELPER: [&str; 2] = ["-c", "echo ran $$; exec /bin/sleep 600"];

/// Play the stand-in window when asked, or return to the test.
fn play_parent_if_asked() {
    let Some(spec) = standin::spec() else { return };
    if spec != PARENT {
        standin::put(format!("unknown stand-in: {spec}\n").as_bytes());
        standin::exit(2);
    }
    aterm_uds::tether::through(launch_isolation::aterm_executable());
    let spawn = |tethered: bool| {
        let mut helper = aterm_uds::spawnfd::command("/bin/sh");
        helper
            .args(HELPER)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if tethered {
            helper.tether();
        }
        helper.spawn().unwrap_or_else(|e| {
            standin::put(format!("spawn: {e}\n").as_bytes());
            standin::exit(3)
        })
    };
    let tethered = spawn(true);
    let plain = spawn(false);
    standin::put(format!("tethered {}\nplain {}\n", tethered.id(), plain.id()).as_bytes());
    loop {
        std::thread::park();
    }
}

/// Every process of the stand-in's group, which it leads: `SIGKILL`ed on drop, so a failing run
/// leaves nothing it started.
struct Group(i32);

impl Group {
    /// Whether any process is left in the group.
    fn holds_any(&self) -> bool {
        // SAFETY: signal zero to this test's own group only asks whether a member exists.
        unsafe { libc::kill(-self.0, 0) == 0 }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        // SAFETY: the group the stand-in made for itself; every member is this test's.
        unsafe { libc::kill(-self.0, libc::SIGKILL) };
    }
}

/// Whether `exit` (an [`aterm_uds::exitwatch::exit_fd`]) reports its process's exit within `hang`.
fn exits_within(exit: &OwnedFd, hang: Duration) -> bool {
    let mut fd = libc::pollfd {
        fd: exit.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let deadline = Instant::now() + hang;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one live `pollfd`, which `poll` reads and writes and does not retain.
        match unsafe { libc::poll(&mut fd, 1, ms) } {
            1.. => return true,
            0 => return false,
            _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => {}
            _ => panic!("poll: {}", std::io::Error::last_os_error()),
        }
    }
}

#[test]
fn a_tethered_helper_ends_with_its_killed_parent() {
    play_parent_if_asked();
    let aterm = launch_isolation::aterm_executable();
    aterm_spec::wait::first_exec(&aterm, &["--help"]);
    let mut parent = aterm_uds::spawnfd::command(standin::exe())
        .args(standin::argv(ENTRY))
        .env(standin::ENV, PARENT)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .expect("start the stand-in window");
    let group = Group(i32::try_from(parent.id()).expect("a pid"));
    let out = parent.stdout.take().expect("its piped stdout");
    let (lines, said) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let (mut tethered, mut plain, mut ran) = (None, None, Vec::new());
    let deadline = Instant::now() + aterm_spec::wait::FIRST_EXEC;
    while tethered.is_none_or(|pid| !ran.contains(&pid))
        || plain.is_none_or(|pid| !ran.contains(&pid))
    {
        let left = deadline.saturating_duration_since(Instant::now());
        let line: String = said
            .recv_timeout(left)
            .expect("both helpers said they ran, by pid");
        let mut words = line.split_whitespace();
        let (Some(word), Some(pid)) = (
            words.next(),
            words.next().and_then(|p| p.parse::<u32>().ok()),
        ) else {
            continue;
        };
        match word {
            "tethered" => tethered = Some(pid),
            "plain" => plain = Some(pid),
            "ran" => ran.push(pid),
            _ => {}
        }
    }
    let (tethered, plain) = (tethered.expect("its pid"), plain.expect("its pid"));
    let tethered_exit = aterm_uds::exitwatch::exit_fd(tethered).expect("watch the tethered helper");
    let plain_exit = aterm_uds::exitwatch::exit_fd(plain).expect("watch the plain helper");

    let killed = Instant::now();
    parent.kill().expect("SIGKILL the stand-in window");
    parent.wait().expect("reap it");
    assert!(
        exits_within(&tethered_exit, MUST_HAPPEN),
        "the tethered helper (pid {tethered}) outlived its killed parent by {MUST_HAPPEN:?}"
    );
    eprintln!(
        "the tethered helper (pid {tethered}) ended {:?} after its parent's SIGKILL",
        killed.elapsed()
    );
    assert_eq!(
        aterm_uds::process::process_presence(i32::try_from(plain).expect("a pid")),
        aterm_uds::process::ProcessPresence::Present,
        "the plain helper (pid {plain}), the negative control, was adopted and runs on"
    );

    // SAFETY: the plain helper is this test's own, alive as just read.
    unsafe { libc::kill(i32::try_from(plain).expect("a pid"), libc::SIGKILL) };
    assert!(
        exits_within(&plain_exit, MUST_HAPPEN),
        "the plain helper ended"
    );
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "nothing the stand-in started is left, the tether's watcher included",
        || (!group.holds_any()).then_some(()),
    );
}

/// A shell forks the tether and tells it a pid that is not its parent. Its arguments refuse `1`;
/// this test's own pid is alive and passes them, so only the tether's parent check keeps the
/// program, which would print `ran`, from running.
#[test]
fn a_tether_whose_parent_is_not_its_own_starts_nothing() {
    let aterm = launch_isolation::aterm_executable();
    aterm_spec::wait::first_exec(&aterm, &["--help"]);
    for stranger in ["1".to_string(), std::process::id().to_string()] {
        let out = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command("/bin/sh")
                .args(["-c", "\"$@\"; exit $?", "sh"])
                .arg(&aterm)
                .args([aterm_uds::tether::VERB, stranger.as_str()])
                .args(["/bin/sh", "/bin/sh", "-c", "echo ran"]),
            Instant::now() + MUST_HAPPEN,
        )
        .expect("run the front door's tether verb from a shell");
        assert_eq!(
            out.status.code(),
            Some(i32::from(aterm_uds::tether::UNTETHERED)),
            "told pid {stranger}: {out:?}"
        );
        assert!(
            out.stdout.is_empty(),
            "told pid {stranger}, the program never ran: {out:?}"
        );
    }
}
