// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A FAKE WORKER WITH ONE BOX, and `aterm drive` run against a headless
//! instance's scratch world: what the harness-tape tests record a real
//! supervisor loop over (`supervise_tape_live_headless.rs`,
//! `tape_replay_changed_world.rs`).
//!
//! The worker is a POSIX `sh` script in raw mode, run as `claude` (`exec -a`)
//! in the instance's boot session. It draws Claude Code 2.1.280's measured
//! Bash box for `touch x` and waits. Every byte it reads is appended to its
//! KEY LOG; a digit is a PRESS — appended to its PRESS LOG, one per line — and
//! answers the box: the worker draws a finished turn and sits at its composer.
//! It reads nothing else and draws nothing else, so what the loop typed is
//! exactly what the logs hold.

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Instant;

use aterm_agent::supervise::prompt::fixtures::{BOX_BASH_TOUCH, composer, screen};

use crate::headless_boot::{CLIENT_EXIT_DEADLINE, Instance, ctl, type_line};
use crate::{child_output, launch_isolation};

/// The script ([module docs](self)): `$1` the screens' directory, `$2` the key
/// log, `$3` the press log.
const WORKER: &str = r#"#!/bin/sh
dir="$1"; keys="$2"; presses="$3"
stty raw -echo
: > "$keys"; : > "$presses"
printf '\033[H\033[2J'; cat "$dir/box.scr"
answered=0
while :; do
  got=$(dd bs=64 count=1 2>/dev/null)
  [ -z "$got" ] && continue
  printf '%s' "$got" >> "$keys"
  [ "$answered" = 1 ] && continue
  digit=$(printf '%s' "$got" | tr -cd '0-9' | cut -c1)
  if [ -n "$digit" ]; then
    printf '%s\n' "$digit" >> "$presses"
    answered=1
    printf '\033[H\033[2J'; cat "$dir/done.scr"
  fi
done
"#;

/// The worker's logs.
pub struct BoxWorker {
    /// Every byte it read.
    pub keys: PathBuf,
    /// Every digit that answered its box, one per line.
    pub presses: PathBuf,
}

impl BoxWorker {
    /// The presses it took, in order.
    pub fn presses(&self) -> Vec<String> {
        std::fs::read_to_string(&self.presses)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// Rows as the worker draws them: `\r\n` between, the cursor then placed at
/// (`row`, `col`), both 0-based.
fn render(rows: &[String], cursor: (usize, usize)) -> String {
    let mut s = rows.join("\r\n");
    s.push_str(&format!("\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1));
    s
}

/// A finished turn at Claude Code's composer.
fn done() -> Vec<String> {
    let mut rows = vec![
        "⏺ Created x.".to_string(),
        String::new(),
        "✻ Worked for 3m 2s · done 4:24 PM".to_string(),
        String::new(),
    ];
    rows.extend(composer("  ? for shortcuts"));
    rows
}

/// Start the worker as `claude` in session `sid` of `inst`, and wait for its
/// box on the screen.
pub fn start(inst: &Instance, sid: &str) -> BoxWorker {
    let dir = inst.tmp.join("box-screens");
    std::fs::create_dir_all(&dir).expect("the screens' dir");
    std::fs::write(dir.join("box.scr"), render(&screen(BOX_BASH_TOUCH), (0, 0))).expect("box");
    let done = done();
    let caret = done
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("a caret row");
    std::fs::write(dir.join("done.scr"), render(&done, (caret, 2))).expect("done");
    let script = inst.tmp.join("box-worker.sh");
    std::fs::write(&script, WORKER).expect("the worker");
    let worker = BoxWorker {
        keys: inst.tmp.join("box-keys.log"),
        presses: inst.tmp.join("box-presses.log"),
    };
    type_line(
        inst,
        sid,
        &format!(
            "/bin/bash -c 'exec -a claude /bin/sh {} {} {} {}'",
            script.display(),
            dir.display(),
            worker.keys.display(),
            worker.presses.display(),
        ),
    );
    let out = ctl(
        inst,
        &[
            &format!("@{sid}"),
            "await",
            "match",
            "Do.you.want.to.proceed",
            "timeout=60000",
        ],
    );
    let reply = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && reply.starts_with("OK") && !reply.starts_with("OK timeout"),
        "the box never reached the screen: {reply:?} {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    worker
}

/// `aterm drive <args…>` in `root`'s scratch world (its HOME, XDG roots and
/// `$TMPDIR`, no inherited aterm variable), `extra` set on top, run to its
/// exit under the client's hang detector.
pub fn drive_in(root: &Path, args: &[&str], extra: &[(&str, &str)], cwd: Option<&Path>) -> Output {
    let aterm = launch_isolation::aterm_executable();
    // The support files' own launch, as `headless_boot::client_command`'s:
    // `launch_isolation::apply` takes a std `Command` and makes it inherit no
    // descriptor of this process.
    #[expect(
        clippy::disallowed_types,
        reason = "launch_isolation::apply configures a std Command and strips its inheritance"
    )]
    let mut cmd = std::process::Command::new(&aterm);
    cmd.arg("drive")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launch_isolation::apply(&mut cmd, root);
    for (key, value) in extra {
        cmd.env(key, value);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let child = cmd.spawn().expect("spawn aterm drive");
    child_output::wait_and_read(child, Instant::now() + CLIENT_EXIT_DEADLINE)
        .unwrap_or_else(|| panic!("aterm drive {args:?} did not exit in time"))
}

/// `aterm drive --socket <inst> <args…>` in `inst`'s scratch world.
pub fn drive(inst: &Instance, args: &[&str]) -> Output {
    let mut all = vec!["--socket", inst.sock.as_str()];
    all.extend_from_slice(args);
    drive_in(&inst.tmp, &all, &[], None)
}

/// What `out` said, for a failure's message.
pub fn said(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
