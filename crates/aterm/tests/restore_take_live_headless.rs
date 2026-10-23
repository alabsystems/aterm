// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live headless tests for two-phase `session.toml` restore take.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

use crate::live_ctl::Ctl;
use crate::{headless_boot, launch_isolation};

/// A wait for something that must happen is a hang detector (AGENTS.md).
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
/// How often a wait asks again.
const GAP: Duration = Duration::from_millis(25);

/// Seam giving a headless instance a window's desk (`aterm-gui`'s `desk::HEADLESS_RESTORE_SEAM`).
const DESK_SEAM: &str = "ATERM_DEBUG_HEADLESS_RESTORE";

/// The development seam that holds the boot inside `build_event_loop`.
const HOLD_SEAM: &str = "ATERM_DEBUG_LOOP_BUILD_HOLD";

/// One scratch world the instances share; removed on drop, after them.
struct World(aterm_tempfile::TempDir);

impl World {
    fn new(tag: &str) -> Self {
        let world = Self(launch_isolation::scratch_root(&format!("atrt-{tag}")));
        assert!(
            launch_isolation::sock_fits(&world.path().join("run/aterm/a.sock.token")),
            "the private root's socket fits sun_path"
        );
        world
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    /// The state root the desk seam's private-root check reads (`ATERM_STATE_HOME`).
    fn state(&self) -> PathBuf {
        self.path().join("astate")
    }

    /// The names in the state root that are launch takes of `session.toml`.
    fn takes(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.state())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| {
                        name.starts_with(".session.toml.aterm-taken-")
                            || name.starts_with(".session.toml.aterm-retaken-")
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Whether a crash journal of the run `pid` is on disk.
    fn journal_of(&self, pid: u32) -> bool {
        let prefix = format!("journal-{pid}-");
        std::fs::read_dir(self.state()).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with(&prefix) && name.ends_with(".toml")
            })
        })
    }

    fn quit_layout(&self) -> PathBuf {
        self.state().join("session.toml")
    }
}

impl Drop for World {
    fn drop(&mut self) {
        headless_boot::remove_world(self.path());
    }
}

/// One headless instance, SIGKILLed by its pid and reaped on drop.
struct Proc {
    child: Child,
    _lifeline: aterm_uds::lifeline::Lifeline,
    sock: String,
    tag: String,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Proc {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// SIGKILL it and reap it: the end that runs no handler at all.
    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Quit the instance via `invoke Quit`.
    fn quit(mut self) {
        let _ = Ctl::connect(&self.sock).line("invoke Quit");
        let ended = aterm_spec::wait::within(HANG, GAP, || self.child.try_wait().ok().flatten());
        assert!(ended.is_some(), "instance {} quits", self.tag);
    }
}

/// Boot one lab-desk instance named `tag` in `world`.
fn boot(world: &World, tag: &str, held: bool) -> Proc {
    let root = world.path();
    let sock = root.join(format!("run/aterm/{tag}.sock"));
    let log_path = root.join(format!("{tag}.log"));
    let (out, err) = match std::fs::File::create(&log_path).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot open the instance log ({e})")),
    };
    #[expect(
        clippy::disallowed_types,
        reason = "the launch helpers every headless live test shares (aterm_uds::lifeline, the ready FIFO) arm a std Command, whose child inherits their descriptors"
    )]
    let mut cmd = std::process::Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .arg("--control-sock")
        .arg(&sock)
        .env("ATERM_STATE_HOME", world.state())
        .env(DESK_SEAM, "1")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let hold = root.join(format!("hold-{tag}"));
    if held {
        cmd.env(HOLD_SEAM, &hold);
    }
    let lifeline = launch_isolation::lifeline(&mut cmd, root);
    let mut ready = (!held).then(|| headless_boot::arm_ready(&mut cmd, root));
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot launch aterm --headless ({e})")),
    };
    let mut proc = Proc {
        child,
        _lifeline: lifeline,
        sock: sock.to_string_lossy().into_owned(),
        tag: tag.to_string(),
    };
    match ready.as_mut() {
        Some(ready) => {
            headless_boot::await_ready(&mut proc, |p| &mut p.child, ready, &log_path, |_| true);
        }
        None => {
            let mut marker = hold.into_os_string();
            marker.push(".held");
            let marker = PathBuf::from(marker);
            let reached = aterm_spec::wait::within(HANG, GAP, || {
                if proc.child.try_wait().ok().flatten().is_some() {
                    panic!(
                        "instance {tag} ended before its event loop's build; log tail:\n{}",
                        headless_boot::log_tail(&log_path)
                    );
                }
                marker.exists().then_some(())
            });
            assert!(
                reached.is_some(),
                "instance {tag} reaches its held event-loop build"
            );
        }
    }
    proc
}

/// The titles of the instance's sessions (`sessions`, its fifth column), sorted.
fn session_titles(proc: &Proc) -> Vec<String> {
    let (_, rows) = Ctl::connect(&proc.sock).counted("sessions");
    let mut titles: Vec<String> = rows
        .iter()
        .filter_map(|row| row.split_whitespace().nth(4).map(str::to_string))
        .collect();
    titles.sort();
    titles
}

/// Seed `world` with a quit layout.
fn seed(world: &World) {
    let q = boot(world, "q", false);
    {
        let mut ctl = Ctl::connect(&q.sock);
        for title in TITLES {
            let reply = ctl.line("spawn");
            let sid = reply
                .strip_prefix("OK ")
                .unwrap_or_else(|| panic!("spawn: {reply}"))
                .trim()
                .to_string();
            let reply = ctl.line(&format!("@{sid} meta set title {title}"));
            assert!(reply.starts_with("OK"), "title {sid}: {reply}");
        }
    }
    q.quit();
    assert!(world.quit_layout().exists(), "the quit wrote its layout");
}

/// The seeded tabs' titles.
const TITLES: [&str; 2] = ["qone", "qtwo"];

/// Whether `titles` holds both seeded tabs.
fn holds_the_seeded_tabs(titles: &[String]) -> bool {
    TITLES.iter().all(|title| titles.iter().any(|t| t == title))
}

/// Whether `titles` holds either seeded tab.
fn holds_a_seeded_tab(titles: &[String]) -> bool {
    TITLES.iter().any(|title| titles.iter().any(|t| t == title))
}

/// Wait until `proc`'s crash journal is on disk.
fn restored_titles(world: &World, proc: &Proc) -> Vec<String> {
    let journaled =
        aterm_spec::wait::within(HANG, GAP, || world.journal_of(proc.pid()).then_some(()));
    assert!(
        journaled.is_some(),
        "instance {}'s crash journal holds its desk",
        proc.tag
    );
    session_titles(proc)
}

/// The module doc's first world.
#[test]
fn a_launch_killed_inside_its_event_loop_build_leaves_the_quit_layout_to_the_next() {
    let world = World::new("kill");
    seed(&world);

    let a = boot(&world, "a", true);
    assert!(
        !world.quit_layout().exists(),
        "A took the layout before its event loop's build"
    );
    let a_take = format!(".session.toml.aterm-taken-{}-", a.pid());
    assert!(
        matches!(world.takes().as_slice(), [name] if name.starts_with(&a_take)),
        "the layout is A's take while A lives: {:?}",
        world.takes()
    );

    let l = boot(&world, "l", false);
    let l_titles = restored_titles(&world, &l);
    assert!(
        l_titles.len() == 1 && !holds_a_seeded_tab(&l_titles),
        "L comes up with its own tab alone: {l_titles:?}"
    );
    assert!(
        matches!(world.takes().as_slice(), [name] if name.starts_with(&a_take)),
        "A's take still stands: {:?}",
        world.takes()
    );
    l.quit();

    a.kill();

    let b = boot(&world, "b", false);
    let b_titles = restored_titles(&world, &b);
    assert!(
        holds_the_seeded_tabs(&b_titles),
        "B brings back the tabs A took: {b_titles:?}"
    );
    assert_eq!(
        b_titles.len(),
        4,
        "A's three tabs, behind L's quit layout's one: {b_titles:?}"
    );
    let settled = aterm_spec::wait::within(HANG, GAP, || world.takes().is_empty().then_some(()));
    assert!(
        settled.is_some(),
        "once B keeps its desk no take is left: {:?}",
        world.takes()
    );
    assert!(!world.quit_layout().exists(), "B took L's layout too");
}

/// THE BRAKE (the module doc's second world).
#[test]
fn a_layout_taken_again_and_lost_again_is_not_taken_a_third_time() {
    let world = World::new("brake");
    seed(&world);

    let a = boot(&world, "a", true);
    a.kill();

    let b = boot(&world, "b", true);
    let b_take = format!(".session.toml.aterm-retaken-{}-", b.pid());
    assert!(
        matches!(world.takes().as_slice(), [name] if name.starts_with(&b_take)),
        "A's take is B's now: {:?}",
        world.takes()
    );
    b.kill();

    let c = boot(&world, "c", false);
    let c_titles = restored_titles(&world, &c);
    assert!(
        c_titles.len() == 1 && !holds_a_seeded_tab(&c_titles),
        "the twice-lost layout stays aside: C comes up with its own tab alone: {c_titles:?}"
    );
    assert!(
        world.takes().is_empty(),
        "C set the twice-lost take aside: {:?}",
        world.takes()
    );
}
