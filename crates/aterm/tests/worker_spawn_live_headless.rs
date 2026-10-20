// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WORKER STARTED THROUGH THE ONE `aterm` BINARY against a real `aterm
//! --headless` (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.1):
//! `aterm ctl @self spawn agent=claude raise=false -`, the task on stdin, opens
//! a tab BEHIND the manager's running the ordinary login shell, marks the
//! worker's task record `working from=<the manager>` before it answers, and at
//! the shell's first prompt types the one line that starts `claude` with the
//! task file's text as its argument — byte for byte, through every character
//! a shell could read into it, at 60 KiB. The stand-in `claude` (the only one
//! on the instance's PATH) writes its argv NUL-separated and exits, and the
//! pane's shell draws its next prompt. Under zsh and under bash. A worker
//! whose shell never draws a prompt is `stalled why=shell`, nothing typed.
//!
//! THE HARNESS IS ON, as a worker needs (`ERR unsupervised:` otherwise; the
//! last test boots the isolation config's harness-off instance to prove it),
//! with `relaunch = false` and `upgrade = false`: the stand-in is a `#!/bin/sh`
//! script the window reads as `sh`, not Claude Code, and it exits at once, so
//! nothing would be relaunched or upgraded; the two switches keep it so.
//!
//! Every wait that must end is a one-minute hang detector, never a latency
//! budget. A machine that refuses the headless instance is COULD NOT RUN,
//! never a pass ([`headless_boot::boot_with`]). ISOLATION: `support/launch_isolation.rs` and
//! `support/headless_boot.rs`, plus the state seam (`ATERM_STATE_HOME`) and a
//! PATH of the stand-in's folder and the system's alone, so no real `claude`
//! can answer — in the tab's login shell too, after macOS's `path_helper`: the
//! boot pins it and ends COULD NOT RUN unless that shell names `claude` by the
//! stand-in's path ([`headless_boot::boot_with`]).

#![cfg(unix)]

use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use crate::{child_output, headless_boot, launch_isolation};

use headless_boot::{Instance, boot_session, client_command, ctl, ctl_ok, field};

/// The stand-in `claude`: `--version` answers at once (the helper runs every
/// fake once that way, `headless_boot::standin`); any other call writes its
/// argv NUL-separated to `$HOME/argv.bin`, whole (written aside, then
/// renamed), and exits 0.
const STANDIN: &str = "#!/bin/sh\n\
    case \"$1\" in --version) echo '2.1.0 (Claude Code)'; exit 0 ;; esac\n\
    for a in \"$@\"; do printf '%s\\000' \"$a\"; done > \"$HOME/argv.part\" && \
    mv \"$HOME/argv.part\" \"$HOME/argv.bin\"\n";

/// A wait for something that MUST happen (AGENTS.md: a hang detector).
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// One headless instance whose shell is `shell` and whose harness supervises
/// with `harness` as its `[harness]` table (`None`: the isolation config's,
/// which switches it off), the stand-in first on its PATH, and `zshrc` the
/// person's `~/.zshrc` when given.
#[track_caller]
fn boot(tag: &str, shell: &str, harness: Option<&str>, zshrc: Option<&str>) -> Instance {
    headless_boot::boot_with(
        &format!("atws{tag}"),
        |tmp, cmd| {
            if let Some(rc) = zshrc {
                std::fs::write(tmp.join("home/.zshrc"), rc).expect("write the zshrc");
            }
            if let Some(harness) = harness {
                let cfg = tmp.join("cfg/aterm/aterm.toml");
                let base = std::fs::read_to_string(&cfg).expect("the isolation config");
                assert!(base.contains(launch_isolation::HARNESS_OFF), "{base}");
                let base = base.replace(launch_isolation::HARNESS_OFF, "");
                std::fs::write(&cfg, format!("{base}[harness]\n{harness}"))
                    .expect("write the config");
            }
            let bin = tmp.join("bin");
            headless_boot::standin(&bin.join("claude"), STANDIN);
            std::fs::create_dir_all(tmp.join("work")).expect("the worker's folder");
            cmd.env("SHELL", shell)
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
                )
                // A development seam (a debug build): the state root, so the
                // task file's path is known.
                .env("ATERM_STATE_HOME", tmp.join("state"))
                // A development seam too: a stalled worker's reason is the
                // diagnosis a failed wait prints.
                .env("ATERM_LOG", "info")
                .args(["--lines", "24", "--columns", "100"]);
        },
        |_| true,
    )
}

/// `aterm ctl @self <args…>` as the session `manager` runs it (its
/// `ATERM_PARENT_SESSION_ID`), from `cwd`, `task` on stdin.
fn spawn_as(
    inst: &Instance,
    manager: &str,
    cwd: &Path,
    args: &[&str],
    task: &[u8],
) -> (bool, String, String) {
    let mut cmd = client_command(inst, args);
    cmd.env("ATERM_PARENT_SESSION_ID", manager)
        .current_dir(cwd)
        .stdin(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn aterm ctl");
    let mut stdin = child.stdin.take().expect("its stdin");
    stdin.write_all(task).expect("write the task");
    drop(stdin);
    let out =
        child_output::wait_and_read(child, Instant::now() + headless_boot::CLIENT_EXIT_DEADLINE)
            .expect("aterm ctl exits");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The task under test: a leading `-`, lines, both quotes, a `$(…)` and
/// backticks a shell would run, a backslash — padded to 60 KiB, its last
/// line unended.
fn hostile_task() -> String {
    let mut task = String::from(
        "-starts with a dash, as an option would\n\
         'single' and \"double\" quotes, a \\ backslash, $HOME unexpanded\n\
         $(touch \"$HOME/ran\") and `touch \"$HOME/ran\"` stay text\n",
    );
    while task.len() < 60 * 1024 {
        task.push_str("padding with 'quotes' \"too\" and $(no) `run` \\\n");
    }
    task.push_str("the last line, unended");
    task
}

/// Poll `probe` until it holds, for at most [`HANG`]; a failure shows the
/// worker as the instance sees it (its screen, its blocks, its record) and
/// the instance log's tail.
fn until(inst: &Instance, worker: &str, what: &str, mut probe: impl FnMut() -> bool) {
    let give_up = Instant::now() + HANG;
    while !probe() {
        if Instant::now() >= give_up {
            let ask = |args: &[&str]| {
                let out = ctl(inst, args);
                String::from_utf8_lossy(&out.stdout).into_owned()
            };
            panic!(
                "{what}: not within {HANG:?}\nscreen:\n{}\nblocks:\n{}\ntask: {}\nlog:\n{}",
                ask(&[&format!("@{worker}"), "text", "trim"]),
                ask(&[&format!("@{worker}"), "blocks"]),
                ask(&[&format!("@{worker}"), "task"]),
                headless_boot::log_tail(&inst.log),
            );
        }
        std::thread::sleep(headless_boot::POLL_GAP);
    }
}

/// The `[harness]` table of an instance whose workers run the stand-in.
const SUPERVISED: &str = "relaunch = false\nupgrade = false\n";

/// The worker's sid out of a spawn's `OK <sid>`.
fn sid_of(stdout: &str) -> String {
    stdout
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("a sid: {stdout}"))
        .to_string()
}

fn run_worker(tag: &str, shell: &str, model: Option<&str>) {
    let inst = boot(tag, shell, Some(SUPERVISED), None);
    let (_, manager) = boot_session(&inst);
    let work = inst.tmp.join("work");
    let task = hostile_task();
    let model_arg = model.map(|m| format!("model={m}"));
    let mut args = vec!["@self", "spawn", "agent=claude", "raise=false"];
    args.extend(model_arg.as_deref());
    args.push("-");
    let (ok, stdout, stderr) = spawn_as(&inst, &manager, &work, &args, task.as_bytes());
    assert!(ok, "spawn agent=claude: {stdout} {stderr}");
    let worker = sid_of(&stdout);

    // The record stands before anything was waited for.
    let record = ctl_ok(&inst, &[&format!("@{worker}"), "task"]);
    assert!(
        record.starts_with(&format!("OK task rev=1 state=working from={manager} ")),
        "{record}"
    );
    // A background tab: the manager's stays its window's active one.
    let roster = ctl_ok(&inst, &["sessions"]);
    let active = |sid: &str| {
        roster
            .lines()
            .find(|l| l.split_whitespace().nth(1) == Some(sid))
            .and_then(|row| field(row, "active").map(str::to_string))
            .unwrap_or_else(|| panic!("{sid}'s row: {roster}"))
    };
    assert_eq!(
        (active(&manager), active(&worker)),
        ("1".into(), "0".into()),
        "{roster}"
    );
    // The folder it was started in is trusted for it alone.
    let meta = ctl_ok(&inst, &[&format!("@{worker}"), "meta"]);
    let trusted = std::fs::canonicalize(&work).expect("canonical work folder");
    assert_eq!(
        field(&meta, "trusted_cwd").map(pct_decode),
        Some(trusted.to_string_lossy().into_owned()),
        "{meta}"
    );
    assert_eq!(
        field(
            &ctl_ok(&inst, &[&format!("@{manager}"), "meta"]),
            "trusted_cwd"
        ),
        Some("-")
    );

    // The task file: 0600 in a 0700 folder, the task, a blank line, one line
    // of fact naming the manager.
    let tasks = inst.tmp.join("state/tasks");
    let file = tasks.join(format!("{worker}-1.txt"));
    let home = inst.tmp.join("home");
    let argv_file = home.join("argv.bin");
    until(&inst, &worker, "the stand-in ran", || argv_file.exists());
    let text = std::fs::read(&file).expect("the task file");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&tasks), mode(&file)), (0o700, 0o600));
    }
    let head = format!("{task}\n\n");
    let fact = text
        .strip_prefix(head.as_bytes())
        .unwrap_or_else(|| panic!("the file opens with the task and a blank line"));
    let fact = std::str::from_utf8(fact).expect("utf-8 fact");
    assert_eq!(fact.matches('\n').count(), 1, "one line of fact: {fact:?}");
    assert!(fact.ends_with('\n') && fact.contains(&manager), "{fact:?}");

    // The argv: the model, the end of options, and the file's text whole.
    let argv = std::fs::read(&argv_file).expect("the argv");
    let argv: Vec<&[u8]> = argv.split(|b| *b == 0).collect();
    let whole = text.strip_suffix(b"\n").expect("the file ends its line");
    let mut want: Vec<&[u8]> = Vec::new();
    if let Some(m) = model {
        want.extend([b"--model".as_slice(), m.as_bytes()]);
    }
    want.extend([b"--".as_slice(), whole, b"".as_slice()]);
    assert!(
        argv == want,
        "argv: {} parts, the last {:?}",
        argv.len(),
        argv.last().map(|a| a.len())
    );
    assert!(!home.join("ran").exists(), "the shell ran the task's text");

    // The pane outlives the agent: its line completed, and the shell drew its
    // next prompt.
    until(&inst, &worker, "the shell's next prompt", || {
        let blocks = ctl_ok(&inst, &[&format!("@{worker}"), "blocks"]);
        let rows: Vec<&str> = blocks.lines().filter(|l| l.starts_with("block ")).collect();
        rows.iter().any(|r| r.contains(" complete exit=0 "))
            && rows
                .last()
                .is_some_and(|r| matches!(r.split_whitespace().nth(2), Some("prompt" | "entering")))
    });
    assert!(
        ctl_ok(&inst, &["sessions"]).contains(&format!(" {worker} ")),
        "the worker's session stands"
    );
}

#[test]
fn a_worker_runs_its_task_as_claudes_argv_in_a_background_tab_under_zsh() {
    run_worker("z", "/bin/zsh", Some("claude-test.1_a"));
}

#[test]
fn a_worker_runs_its_task_as_claudes_argv_in_a_background_tab_under_bash() {
    run_worker("b", "/bin/bash", None);
}

/// With the window harness off nothing would finish a worker's task: `ERR
/// unsupervised:` naming the switch, and nothing is spawned (the other
/// switches that refuse it: `harness_host::worker_unsupervised`'s test).
/// `agent=codex` is reserved, and an empty task never leaves the client.
#[test]
fn a_worker_is_refused_unsupervised_and_spawns_nothing() {
    let inst = boot("u", "/bin/zsh", None, None);
    let (_, manager) = boot_session(&inst);
    let work = inst.tmp.join("work");
    let rows = || ctl_ok(&inst, &["sessions"]).lines().count();
    let before = rows();
    let (ok, _, stderr) = spawn_as(
        &inst,
        &manager,
        &work,
        &["@self", "spawn", "agent=claude", "-"],
        b"a task",
    );
    assert!(
        !ok && stderr.contains("ERR unsupervised: [harness] enabled = false"),
        "{stderr}"
    );
    let (ok, _, stderr) = spawn_as(
        &inst,
        &manager,
        &work,
        &["spawn", "agent=codex", "-"],
        b"a task",
    );
    assert!(
        !ok && stderr.contains("ERR agent codex: not yet"),
        "{stderr}"
    );
    let (ok, _, stderr) = spawn_as(&inst, &manager, &work, &["spawn", "agent=claude", "-"], b"");
    assert!(
        !ok && stderr.contains("the task on stdin is empty"),
        "{stderr}"
    );
    assert_eq!(rows(), before, "nothing was spawned");
    // A worker's spawn with no `-` is refused before anything is sent.
    let raw = ctl(&inst, &["spawn", "agent=claude"]);
    assert!(
        String::from_utf8_lossy(&raw.stderr).contains("end the request with -"),
        "{raw:?}"
    );
}

/// A worker whose shell ends before its first prompt — the person's rc file
/// ends it (a worker's shell alone: the one `ATERM_TASK_FILE` names a task to)
/// — has its task `stalled why=shell`, and nothing was typed: the stand-in
/// never ran. Aimed at another session than its manager's, its `from=` is
/// still the manager, the session that started it.
#[test]
fn a_worker_whose_shell_draws_no_prompt_is_stalled_and_nothing_is_typed() {
    let inst = boot(
        "s",
        "/bin/zsh",
        Some(SUPERVISED),
        Some("[ -n \"$ATERM_TASK_FILE\" ] && exit 3\n"),
    );
    let (_, manager) = boot_session(&inst);
    let other = sid_of(&ctl_ok(&inst, &["spawn", "raise=false"]));
    let work = inst.tmp.join("work");
    let (ok, stdout, stderr) = spawn_as(
        &inst,
        &manager,
        &work,
        &[&format!("@{other}"), "spawn", "agent=claude", "-"],
        b"a task its shell never reaches",
    );
    assert!(ok, "spawn agent=claude: {stdout} {stderr}");
    let worker = sid_of(&stdout);
    // `working` from the reply on, and soon `stalled`: from= the manager
    // either way.
    let record = ctl_ok(&inst, &[&format!("@{worker}"), "task"]);
    assert!(
        record.contains(&format!(" from={manager} ")),
        "from= the manager, not the aim {other}: {record}"
    );
    until(&inst, &worker, "the task stalled", || {
        let record = ctl_ok(&inst, &[&format!("@{worker}"), "task"]);
        record.contains(" state=stalled ") && record.contains(" why=shell")
    });
    assert!(
        !inst.tmp.join("home/argv.bin").exists(),
        "nothing was typed: the stand-in never ran"
    );
}

/// Undo the wire's pct-encoding.
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
