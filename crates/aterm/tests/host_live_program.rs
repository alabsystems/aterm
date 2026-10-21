// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the host's reading of a session run through atpkg's shim.
//! `claude` in an aterm shell runs the twin `…/agents/claude`, a `/bin/sh` script that
//! `exec`s the store binary, so until the exec the foreground leader's argv[0] is `/bin/sh`.
//! A twin that draws aterm-phase's 529 end of turn and never execs reads `program=claude`
//! and `agent=wall:overloaded` (`level=limited`; `await agent wall` latches). NEGATIVE
//! CONTROLS: the same script under another name, or named `claude` outside the managed
//! prefix (a user's own wrapper), reads `program=sh agent=-`.
//!
//! Isolation (`support/launch_isolation.rs`): scratch HOME/XDG roots, a private control
//! socket, every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::headless_boot;

use headless_boot::{Instance, boot_session, ctl, ctl_ok, field, status, type_line};

/// Boot one headless instance ([`headless_boot::boot`]: an environment refusal
/// is COULD NOT RUN; a product that cannot start fails the test). Wide enough that the
/// fixture's longest row (the 529 notice) is not wrapped by the terminal: Claude
/// Code lays out its own rows, so a terminal wrap to column 0 is not a shape it
/// draws.
fn boot(tag: &str) -> Instance {
    headless_boot::boot(
        &format!("athp{tag}"),
        &["--lines", "40", "--columns", "200"],
    )
}

/// How long a wait for something that must happen may take before it is read as a hang.
/// Every one is below the shim's life ([`write_shim`]'s `sleep`): a shim that ended first
/// would leave `program=sh` behind, which the negative controls would read as their pass.
const HANG: Duration = Duration::from_secs(60);
const HANG_MS: &str = "timeout=60000";

/// Poll `status` until `pred` holds, returning the matching record, or panic
/// with the last record after `within`.
fn status_until(
    inst: &Instance,
    sid: &str,
    within: Duration,
    what: &str,
    pred: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + within;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = status(inst, sid);
        if pred(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{what}: not within {within:?}; last status: {last}");
}

/// aterm-phase's 529 end of turn, its provenance line dropped.
fn screen_529() -> String {
    let text = include_str!("../../aterm-phase/src/fixtures/hand-built-529-end-of-turn.txt");
    text.split_once('\n')
        .map_or(text, |(_, rest)| rest)
        .to_string()
}

/// A shim-shaped script: `#!/bin/sh`, draw the 529 screen on the alternate
/// screen, then wait without ever `exec`-ing — for ten minutes, far past every
/// [`HANG`] wait on it (the instance's hangup ends it with the test).
fn write_shim(path: &Path, screen: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let body = format!(
        "#!/bin/sh\nprintf '\\033[?1049h\\033[2J\\033[H'\ncat '{}'\nsleep 600\nexit 0\n",
        screen.display()
    );
    std::fs::write(path, body).expect("write the shim");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Where a shim under test lives: the instance's managed atpkg prefix (its
/// scratch HOME's default, `atpkg::store::default_prefix`), so
/// `agents/claude` is the prefix's own twin path — the only agent-named
/// script the server reads as the agent.
fn shim_root(inst: &Instance) -> PathBuf {
    atpkg::store::default_prefix(&inst.tmp.join("home"))
}

/// Start `name` — under the managed prefix ([`shim_root`]) when `managed`,
/// else under a scratch directory of the user's own — in the boot session
/// and wait for the screen to show the 529.
fn run_shim(name: &str, managed: bool, tag: &str) -> (Instance, String) {
    let inst = boot(tag);
    let (_local, sid) = boot_session(&inst);
    let screen = inst.tmp.join("screen.txt");
    std::fs::write(&screen, screen_529()).expect("write the screen");
    let root = if managed {
        shim_root(&inst)
    } else {
        inst.tmp.join("home/bin")
    };
    let shim = root.join(name);
    std::fs::create_dir_all(shim.parent().expect("a parent")).expect("mkdir");
    write_shim(&shim, &screen);
    type_line(&inst, &sid, &format!("'{}'", shim.display()));
    let reply = ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "await",
            "match",
            "API.Error:.529",
            HANG_MS,
        ],
    );
    assert!(
        reply.starts_with("OK") && !reply.starts_with("OK timeout"),
        "the 529 screen never drew: {reply}"
    );
    (inst, sid)
}

#[test]
fn a_shim_before_its_exec_is_the_agent_and_its_529_is_a_wall() {
    let (inst, sid) = run_shim("agents/claude", true, "w");
    let rec = status_until(
        &inst,
        &sid,
        HANG,
        "program=claude agent=wall:overloaded",
        |s| field(s, "program") == Some("claude") && field(s, "agent") == Some("wall:overloaded"),
    );
    assert_eq!(field(&rec, "agent_detail"), Some("-"), "{rec}");
    assert_eq!(field(&rec, "level"), Some("limited"), "{rec}");
    // Latched: the verdict is already true, so each answers at once; one that did not latch
    // would time out (exit 124) and fail `ctl_ok`.
    for word in ["wall:overloaded", "wall", "busy,wall:overloaded"] {
        let reply = ctl_ok(
            &inst,
            &[&format!("@{sid}"), "await", "agent", word, HANG_MS],
        );
        assert!(
            reply.starts_with("OK agent wall:overloaded rev="),
            "{word}: {reply}"
        );
    }
    // NEGATIVE CONTROL for the await: a wall kind the screen is not.
    let reply = ctl(
        &inst,
        &[
            &format!("@{sid}"),
            "await",
            "agent",
            "wall:usage-session",
            "timeout=300",
        ],
    );
    let said = String::from_utf8_lossy(&reply.stdout);
    assert!(said.starts_with("OK timeout"), "{said}");
    // The roster says the same.
    let roster = ctl_ok(&inst, &["sessions"]);
    let row = roster.lines().find(|l| l.contains(&sid)).expect("row");
    assert_eq!(field(row, "program"), Some("claude"), "{row}");
    assert_eq!(field(row, "agent"), Some("wall:overloaded"), "{row}");
}

/// NEGATIVE CONTROL: the very same script, named `build.sh`, is a shell
/// running a script — `program=sh`, never an agent, whatever it draws.
#[test]
fn the_same_script_under_another_name_is_a_shell() {
    let (inst, sid) = run_shim("build.sh", true, "n");
    let rec = status_until(&inst, &sid, HANG, "program=sh", |s| {
        field(s, "program") == Some("sh")
    });
    assert_eq!(field(&rec, "agent"), Some("-"), "{rec}");
}

/// NEGATIVE CONTROL: a script named `claude` that is not atpkg's shim (a user's own
/// `~/bin/claude` wrapper) is a shell running a script too, so the in-GUI supervisor never
/// attaches to it.
#[test]
fn a_wrapper_named_claude_outside_the_prefix_is_a_shell() {
    let (inst, sid) = run_shim("claude", false, "u");
    let rec = status_until(&inst, &sid, HANG, "program=sh", |s| {
        field(s, "program") == Some("sh")
    });
    assert_eq!(field(&rec, "agent"), Some("-"), "{rec}");
}
