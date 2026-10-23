// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session journal live headless test across real aterm processes sharing one scratch world.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

use crate::live_ctl::Ctl;
use crate::{headless_boot, launch_isolation};

/// A wait for something that must happen is a hang detector (AGENTS.md).
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The planted word: a command's second word, which the journal never keeps.
const SECRET: &str = "QZP6SECRET1";

/// The development seam that gives a headless instance a window's desk
/// (`aterm-gui`'s `desk::HEADLESS_RESTORE_SEAM`).
const DESK_SEAM: &str = "ATERM_DEBUG_HEADLESS_RESTORE";

/// One scratch world the instances share; removed on drop, after them.
struct World(aterm_tempfile::TempDir);

impl World {
    fn path(&self) -> &Path {
        self.0.path()
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
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A prepared world named `tag`, its config with `extra` appended.
fn scratch_world(tag: &str, extra: &str) -> World {
    let world = launch_isolation::scratch_root(&format!("atsj-{tag}"));
    let root = world.path();
    assert!(
        launch_isolation::sock_fits(&root.join("run/aterm/a.sock.token")),
        "the private root's socket fits sun_path"
    );
    let config = root.join("cfg/aterm/aterm.toml");
    let mut text = std::fs::read_to_string(&config).expect("the scratch config");
    assert!(
        !text.contains("[session_log]"),
        "the default world has no [session_log] table"
    );
    text.push_str(extra);
    std::fs::write(&config, text).expect("write the scratch config");
    World(world)
}

/// The state root the seam's private-root check reads (`ATERM_STATE_HOME`).
fn state_home(root: &Path) -> PathBuf {
    root.join("astate")
}

/// The journal folder: `aterm_types::dirs::logs_dir` under the state root.
fn journal_dir(root: &Path) -> PathBuf {
    state_home(root).join("logs/session-log")
}

/// Boot one headless instance named `tag` in `root`, under the lab's desk
/// seam when `lab`.
fn boot(root: &Path, tag: &str, lab: bool) -> Proc {
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
        .env("ATERM_STATE_HOME", state_home(root))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    if lab {
        cmd.env(DESK_SEAM, "1");
    }
    let lifeline = launch_isolation::lifeline(&mut cmd, root);
    let mut ready = headless_boot::arm_ready(&mut cmd, root);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot launch aterm --headless ({e})")),
    };
    let mut proc = Proc {
        child,
        _lifeline: lifeline,
        sock: sock.to_string_lossy().into_owned(),
    };
    headless_boot::await_ready(&mut proc, |p| &mut p.child, &mut ready, &log_path, |_| true);
    proc
}

/// The boot session's sid (`sessions`: `<local> <sid> …`).
fn boot_sid(ctl: &mut Ctl) -> String {
    let (_, rows) = ctl.counted("sessions");
    rows.iter()
        .find_map(|row| row.split_whitespace().find(|t| t.starts_with("s-")))
        .unwrap_or_else(|| panic!("a session row: {rows:?}"))
        .to_string()
}

/// Quit `proc` as a person does (`invoke Quit`, which a headless instance
/// takes without a confirm) and wait for it to end.
fn quit(proc: &mut Proc) {
    let _ = Ctl::connect(&proc.sock).line("invoke Quit");
    let ended = aterm_spec::wait::within(HANG, Duration::from_millis(25), || {
        proc.child.try_wait().ok().flatten()
    });
    assert!(ended.is_some(), "the instance quits");
}

/// A script that writes one whole shell-integration block in one write, A to
/// D;0, its command line `echo <SECRET> 42` (OSC 633;E) and its output
/// `ran-42` (a match the typed line cannot make), each mark signed with the
/// tab's nonce when the shell was handed one.
fn marked_command(root: &Path) -> PathBuf {
    let script = format!(
        "#!/bin/sh\n\
         n=\"${{ATERM_SHELL_NONCE:+;id=$ATERM_SHELL_NONCE}}\"\n\
         printf '\\033]133;A%s\\007$ \\033]133;B%s\\007\\033]633;E;echo {SECRET} 42%s\\007\\r\\n\\033]133;C%s\\007ran-%s\\r\\n\\033]133;D;0%s\\007' \
         \"$n\" \"$n\" \"$n\" \"$n\" \"$((40+2))\" \"$n\"\n"
    );
    let path = root.join("marked.sh");
    std::fs::write(&path, script).expect("write the marked command");
    path
}

/// The run of one world: A runs the secret's command, waits until its log
/// holds the run, and quits; then B starts on the same roots.
fn a_then_b(root: &Path) -> (String, String, Vec<String>, Proc) {
    let script = marked_command(root);
    let mut a = boot(root, "a", true);
    let sid = {
        let mut ctl = Ctl::connect(&a.sock);
        let sid = boot_sid(&mut ctl);
        ctl.type_line(&format!("sh {}", script.display()));
        ctl.expect_on_screen("ran-42");
        sid
    };
    let req = format!("@{sid} log");
    let logged = aterm_spec::wait::within(HANG, Duration::from_millis(50), || {
        let (_, rows) = Ctl::connect(&a.sock).counted(&req);
        rows.iter()
            .any(|l| l.contains(" cmd ") && l.contains(" detail="))
            .then_some(())
    });
    assert!(logged.is_some(), "A's log holds the command's run");
    quit(&mut a);
    drop(a);
    let b = boot(root, "b", true);
    let header = Ctl::connect(&b.sock).line(&req);
    let lines = if header.starts_with("OK") {
        Ctl::connect(&b.sock).counted(&req).1
    } else {
        Vec::new()
    };
    (sid, header, lines, b)
}

/// B answers `log @<A's sid>` from A's journal; a plain headless instance and
/// `[session_log] journal = false` keep no journal.
#[test]
fn the_journal_is_on_by_default_and_answers_for_an_earlier_process() {
    use std::os::unix::fs::PermissionsExt as _;
    let world = scratch_world("on", "");
    let (sid, header, lines, b) = a_then_b(world.path());
    let journal = journal_dir(world.path());
    let mode = |p: &Path| {
        std::fs::metadata(p)
            .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&journal), 0o700, "the journal folder is private");
    let b_prefix = format!("p{}-", b.child.id());
    let names = aterm_spec::wait::within(HANG, Duration::from_millis(25), || {
        let names: Vec<String> = std::fs::read_dir(&journal)
            .ok()?
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<Result<_, _>>()
            .ok()?;
        let firsts = names
            .iter()
            .filter(|name| name.ends_with(".1.slog"))
            .count();
        let b_started = names.iter().any(|name| {
            name.starts_with(&b_prefix)
                && name.ends_with(".1.slog")
                && std::fs::metadata(journal.join(name)).is_ok_and(|md| md.len() > 0)
        });
        (firsts >= 2 && b_started).then_some(names)
    })
    .expect("B's writer creates its first journal segment");
    let firsts: Vec<&String> = names.iter().filter(|n| n.ends_with(".1.slog")).collect();
    assert!(firsts.len() >= 2, "A's and B's first segments: {names:?}");
    let mut runs = 0;
    for name in names.iter().filter(|n| n.ends_with(".slog")) {
        let path = journal.join(name);
        assert_eq!(mode(&path), 0o600, "{name} is the owner's alone");
        let bytes = std::fs::read(&path).expect("a segment");
        let text = String::from_utf8_lossy(&bytes);
        runs += text
            .lines()
            .filter(|l| l.contains(" cmd ") && l.contains(" detail=echo "))
            .count();
        assert!(
            !text.contains(SECRET),
            "{name} keeps no later word of a command: {text}"
        );
    }
    assert!(
        runs >= 1,
        "a positive control: the journal holds the command's run, by its program: {names:?}"
    );
    assert!(header.starts_with("OK"), "`log @{sid}` answers: {header}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains(" cmd ") && l.contains(" detail=echo ")),
        "A's run, from A's journal: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(" close ") && l.contains("out=app-quit")),
        "A's own clean quit, from A's journal: {lines:?}"
    );
    drop(b);
    let plain = scratch_world("plain", "");
    let mut c = boot(plain.path(), "c", false);
    quit(&mut c);
    drop(c);
    assert!(
        !journal_dir(plain.path()).exists(),
        "a headless instance keeps no journal"
    );
    let off = scratch_world("off", "[session_log]\njournal = false\n");
    let (sid, header, _, b) = a_then_b(off.path());
    assert!(
        !journal_dir(off.path()).exists(),
        "`journal = false` creates nothing"
    );
    assert!(
        header.starts_with("ERR"),
        "without a journal `log @{sid}` of an earlier process is no session: {header}"
    );
    drop(b);
}
